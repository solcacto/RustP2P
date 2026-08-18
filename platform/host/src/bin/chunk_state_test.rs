use anyhow::{bail, Context, Result};
use host::chunk::{
    ChunkCoord, ChunkDht, ChunkEdit, ChunkState, ChunkStatePointer, ChunkStore, STATE_TAG,
};
use host::{host_functions, host_state::HostState};
use std::net::SocketAddr;
use wasmtime::{Engine, Linker, Module, Store};

/// Commit 24 regression: chunk state persistence — the owner saves edits
/// locally, publishes the state to IPFS (keyed by chunk + owner key) on
/// shutdown, and other peers can load the cached "ruins" while the owner is
/// offline.
///
/// Modes:
///   `chunk_state_test`                                    # units + publish + remote load
///   `chunk_state_test --offline CID --ipfs API`            # load a published state
///                                                          # from the swarm (owner offline)
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(cid) = args.iter().position(|a| a == "--offline").and_then(|i| args.get(i + 1)) {
        return offline_load(cid, &offline_ipfs(&args));
    }

    // 1. ChunkEdit / ChunkState serialize round-trip.
    let state = ChunkState {
        chunk: ChunkCoord { x: 1, z: 0 },
        owner: "PeerA".into(),
        owner_pubkey: "ed25519:abc".into(),
        modified_at: 1234,
        edits: vec![
            ChunkEdit { x: 150.0, z: 30.0, kind: "ruin".into(), value: 3.0 },
            ChunkEdit { x: 160.0, z: 35.0, kind: "ruin".into(), value: 5.0 },
        ],
    };
    let json = serde_json::to_vec(&state)?;
    let round: ChunkState = serde_json::from_slice(&json)?;
    assert_eq!(round, state);
    assert_eq!(round.edits.len(), 2);
    println!("✓ chunk state serializes ({} edits)", round.edits.len());

    // 2. Local persistence: the owner saves its chunk modifications to JSON.
    let store_path = std::env::temp_dir().join(format!("chunk_store_{}", std::process::id()));
    let store = ChunkStore::new("test-owner");
    store.save(&state)?;
    let loaded = store.load_all()?;
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].edits.len(), 2, "modifications persisted to disk");
    println!("✓ owner saves chunk state locally (JSON)");

    // 3. State pointer: key = chunk coordinate + owner key; wire round-trip.
    let pointer = ChunkStatePointer {
        peer_id: "PeerA".into(),
        owner_pubkey: state.owner_pubkey.clone(),
        chunk_x: state.chunk.x,
        chunk_z: state.chunk.z,
        state_cid: "QmStateExample".into(),
    };
    assert_eq!(pointer.key(), "chunk_1_0_ed25519:abc");
    let wire = pointer.wire_bytes();
    assert_eq!(wire[0], STATE_TAG);
    let parsed = ChunkStatePointer::from_wire(&wire).ok_or_else(|| anyhow::anyhow!("pointer parse"))?;
    assert_eq!(parsed, pointer);
    println!("✓ state pointer keyed by chunk coordinate + owner pubkey");

    // 4. DHT records the published state CID.
    let mut dht = ChunkDht::new();
    let addr: SocketAddr = "127.0.0.1:52001".parse()?;
    dht.record_state(state.chunk, "QmStateExample", "ed25519:abc", "PeerA", addr);
    let entry = dht.get(state.chunk).expect("recorded");
    assert_eq!(entry.state_cid.as_deref(), Some("QmStateExample"));
    assert_eq!(entry.owner_pubkey.as_deref(), Some("ed25519:abc"));
    println!("✓ chunk DHT maps chunk -> (owner key, state CID)");

    // 5. Host flow: an owner records edits and publishes to IPFS; an observer
    //    ingests the pointer and loads the state back.
    println!("publishing chunk state to IPFS (default node)...");
    let mut owner = HostState::new("owner-a");
    owner.set_owner_pubkey("ed25519:dev");
    owner.record_chunk_edit(ChunkEdit { x: 150.0, z: 30.0, kind: "ruin".into(), value: 3.0 })?;
    owner.record_chunk_edit(ChunkEdit { x: 160.0, z: 35.0, kind: "ruin".into(), value: 5.0 })?;
    let published = owner.publish_chunk_states()?;
    assert_eq!(published, 1);
    let entry = owner
        .chunk_registry()
        .lock()
        .unwrap()
        .get(ChunkCoord { x: 1, z: 0 })
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("published state not in the owner's DHT"))?;
    let cid = entry.state_cid.expect("state CID recorded");
    println!("✓ published chunk state to IPFS: CID = {cid}");

    // The observer ingests the pointer (as poll_network would on the wire).
    let pointer = ChunkStatePointer {
        peer_id: "owner-a".into(),
        owner_pubkey: "ed25519:dev".into(),
        chunk_x: 1,
        chunk_z: 0,
        state_cid: cid.clone(),
    };
    let mut observer = HostState::new("observer-b");
    observer.ingest_state_pointer(&pointer, addr);
    let ruins = observer
        .load_remote_chunk_state(ChunkCoord { x: 1, z: 0 })?
        .ok_or_else(|| anyhow::anyhow!("observer could not load the ruins"))?;
    assert_eq!(ruins.owner, "owner-a");
    assert_eq!(ruins.edits.len(), 2, "ruins carry the owner's modifications");
    assert_eq!(ruins.edits[0].kind, "ruin");
    println!("✓ observer loaded the cached ruins ({} edits) from IPFS", ruins.edits.len());

    // 6. Host functions: save_chunk_state + publish_chunk_states via a guest.
    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/chunk_state_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new("guest-owner"));
    store.data_mut().set_owner_pubkey("ed25519:dev");
    let instance = linker.instantiate(&mut store, &module)?;
    let save = instance.get_typed_func::<(), i32>(&mut store, "save_state")?;
    let r = save.call(&mut store, ())?;
    if r != 1 {
        bail!("✗ save_chunk_state host function failed (got {r})");
    }
    let states = store.data().chunk_states();
    assert_eq!(states.len(), 1, "guest edit recorded in host state");
    assert_eq!(states.values().next().unwrap().edits[0].kind, "ruin");
    println!("✓ save_chunk_state host function persisted a guest edit");

    let _ = std::fs::remove_dir_all(store_path);
    println!("all chunk-state persistence checks passed (offline CID: {cid})");
    Ok(())
}

fn offline_ipfs(args: &[String]) -> String {
    args.iter()
        .position(|a| a == "--ipfs")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| host::ipfs::IPFS_API.to_string())
}

/// Loads a published chunk state from the swarm and verifies it — the "ruins
/// while the owner is offline" check. Pins it so this node becomes a seeder.
fn offline_load(cid: &str, api: &str) -> Result<()> {
    let ipfs = host::ipfs::IpfsClient::new(api);
    let bytes = ipfs.cat(cid)?;
    let state: ChunkState = serde_json::from_slice(&bytes)
        .context("fetched object is not a chunk state")?;
    println!(
        "✓ loaded ruins for chunk ({},{}) owned by '{}' ({}) — {} edits",
        state.chunk.x,
        state.chunk.z,
        state.owner,
        state.owner_pubkey,
        state.edits.len()
    );
    ipfs.pin(cid)?;
    println!("✓ pinned '{cid}' — this node is now a seeder for the ruins");
    Ok(())
}