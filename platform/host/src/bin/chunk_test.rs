use anyhow::{bail, Result};
use host::chunk::{ChunkClaim, ChunkCoord, ChunkDht, CHUNK_SIZE};
use host::{host_functions, host_state::HostState};
use std::net::SocketAddr;
use std::time::Duration;
use wasmtime::{Engine, Linker, Module, Store};

/// Commit 23 regression: the spatial chunk system — coordinate math, claims,
/// the kad-style chunk DHT, and the `broadcast_chunk_claim` / `get_chunk_owner`
/// host functions.
fn main() -> Result<()> {
    // 1. World → chunk coordinate math.
    let c = ChunkCoord::at(150.0, 30.0);
    assert_eq!((c.x, c.z), (1, 0), "position 150,30 lands in chunk 1,0");
    let c = ChunkCoord::at(-50.0, 120.0);
    assert_eq!((c.x, c.z), (-1, 1), "negative positions floor correctly");
    assert_eq!(CHUNK_SIZE, 100.0, "chunk size is 100 units");
    println!("✓ chunk coordinate math (100-unit grid)");

    // 2. Claim region covers (0,0) and (1,0) — the classic X,Y + X+1,Y claim.
    let claim = ChunkClaim::region("PeerA", ChunkCoord { x: 0, z: 0 }, (2, 1));
    assert!(claim.covers(ChunkCoord { x: 0, z: 0 }));
    assert!(claim.covers(ChunkCoord { x: 1, z: 0 }));
    assert!(!claim.covers(ChunkCoord { x: 2, z: 0 }));
    assert_eq!(claim.coords().len(), 2);
    println!("✓ chunk claim covers 'X,Y and X+1,Y'");

    // 3. Claim wire format round-trips through the datagram encoding.
    let wire = claim.wire_bytes();
    assert_eq!(wire[0], host::chunk::CLAIM_TAG);
    let parsed = ChunkClaim::from_wire(&wire).ok_or_else(|| anyhow::anyhow!("claim parse failed"))?;
    assert_eq!(parsed, claim);
    assert!(ChunkClaim::from_wire(&[0u8; 16]).is_none(), "pose-sized datagram is not a claim");
    println!("✓ claim wire encoding round-trips");

    // 4. Kad-style DHT: chunk → (peer, address), XOR nearest routing.
    let addr_b: SocketAddr = "127.0.0.1:51002".parse()?;
    let addr_c: SocketAddr = "127.0.0.1:51003".parse()?;
    let mut dht = ChunkDht::new();
    dht.apply_claim(&ChunkClaim::region("PeerB", ChunkCoord { x: 0, z: 0 }, (1, 1)), addr_b);
    dht.apply_claim(&ChunkClaim::region("PeerC", ChunkCoord { x: 5, z: 5 }, (1, 1)), addr_c);

    let owner = dht
        .get(ChunkCoord { x: 0, z: 0 })
        .ok_or_else(|| anyhow::anyhow!("chunk (0,0) has no owner"))?;
    assert_eq!(owner.peer_id, "PeerB");
    assert_eq!(owner.address, addr_b);
    println!("✓ DHT maps chunk (0,0) -> PeerB @ {addr_b}");

    // XOR nearest: target (0,0) is closest to itself, then (5,5).
    let nearest = dht.nearest(ChunkCoord { x: 0, z: 0 }, 2);
    assert_eq!(nearest.len(), 2);
    assert_eq!(nearest[0].0, ChunkCoord { x: 0, z: 0 }, "nearest is the target chunk itself");
    assert!(nearest[0].1 < nearest[1].1, "XOR distance ordering");

    dht.remove_peer("PeerB");
    assert!(dht.get(ChunkCoord { x: 0, z: 0 }).is_none(), "peer removal clears its chunks");
    println!("✓ kad-style DHT lookup, XOR nearest routing, peer eviction");

    // 5. Host functions: a guest claims chunks and queries the owner.
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/chunk_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new("chunk-test"));
    let instance = linker.instantiate(&mut store, &module)?;
    let claim_test = instance.get_typed_func::<(), i32>(&mut store, "claim_test")?;
    let owner_test = instance.get_typed_func::<(), i32>(&mut store, "owner_test")?;

    let r = claim_test.call(&mut store, ())?;
    if r != 0 {
        bail!("✗ broadcast_chunk_claim failed (got {r})");
    }
    // The local peer owns the claimed chunk region in the DHT.
    let registry = store.data().chunk_registry().lock().unwrap();
    let mine = registry
        .get(ChunkCoord { x: 0, z: 0 })
        .ok_or_else(|| anyhow::anyhow!("local claim not recorded in the DHT"))?;
    assert_eq!(mine.peer_id, "chunk-test");
    assert_eq!(store.data().owned_chunks().len(), 2, "owns two chunks");
    drop(registry);
    println!("✓ broadcast_chunk_claim recorded local ownership in the DHT");

    // Simulate a remote claim landing in the DHT (as poll_network would).
    {
        let mut dht = store.data_mut().chunk_registry().lock().unwrap();
        dht.apply_claim(&ChunkClaim::region("PeerB", ChunkCoord { x: 3, z: 0 }, (1, 1)), addr_b);
    }

    let r = owner_test.call(&mut store, ())?;
    if r != 1 {
        bail!("✗ get_chunk_owner did not find the chunk owner (got {r})");
    }
    let mem = instance.get_memory(&mut store, "memory").expect("memory export");
    let owner_bytes = mem.data(&store)[64..69].to_vec();
    let owner_str = String::from_utf8_lossy(&owner_bytes).into_owned();
    assert_eq!(owner_str, "PeerB", "get_chunk_owner returned the hosting peer id");
    println!("✓ get_chunk_owner resolved chunk (3,0) -> {owner_str}");

    // 6. Stale entries expire via last_seen (used by the renderer).
    let mut stale_dht = ChunkDht::new();
    stale_dht.apply_claim(&ChunkClaim::region("P", ChunkCoord { x: 1, z: 1 }, (1, 1)), addr_b);
    let fresh = stale_dht
        .get(ChunkCoord { x: 1, z: 1 })
        .unwrap()
        .last_seen
        .elapsed()
        < Duration::from_secs(1);
    assert!(fresh, "entries are timestamped for staleness checks");
    println!("✓ chunk entries are timestamped");

    println!("✓ all spatial chunk checks passed");
    Ok(())
}