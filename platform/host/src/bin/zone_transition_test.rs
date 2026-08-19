use anyhow::Result;
use host::chunk::{
    ChunkClaim, ChunkCoord, ChunkEdit, ZoneJoinRequest, ZoneState, ZONE_JOIN_TAG, ZONE_STATE_TAG,
};
use host::host_state::HostState;
use host::net_link::NetLink;
use host::peer_connection::PeerConnection;
use std::net::SocketAddr;
use wasmtime::{Engine, Store};

/// Commit 25 regression: seamless zone transitions. As a player walks from a
/// chunk owned by Peer 1 into a chunk owned by Peer 2, the host disconnects
/// from Peer 1's state stream, queries the DHT for chunk B's owner, connects
/// to Peer 2, and downloads the live state — the "server" changes
/// transparently.
fn main() -> Result<()> {
    let engine = Engine::default();

    // 1. Zone messages wire round-trip.
    let req = ZoneJoinRequest {
        peer_id: "player".into(),
        chunk_x: 1,
        chunk_z: 0,
    };
    let req_wire = req.wire_bytes();
    assert_eq!(req_wire[0], ZONE_JOIN_TAG);
    assert_eq!(
        ZoneJoinRequest::from_wire(&req_wire).ok_or(anyhow::anyhow!("req parse"))?,
        req
    );

    let zone = ZoneState {
        owner: "peer2".into(),
        chunk: ChunkCoord { x: 1, z: 0 },
        state: host::chunk::ChunkState {
            chunk: ChunkCoord { x: 1, z: 0 },
            owner: "peer2".into(),
            owner_pubkey: "ed25519:p2".into(),
            modified_at: 1,
            edits: vec![ChunkEdit {
                x: 150.0,
                z: 30.0,
                kind: "ruin".into(),
                value: 9.0,
            }],
        },
    };
    let zone_wire = zone.wire_bytes();
    assert_eq!(zone_wire[0], ZONE_STATE_TAG);
    assert_eq!(
        host::chunk::ZoneState::from_wire(&zone_wire).ok_or(anyhow::anyhow!("zone parse"))?,
        zone
    );
    println!("✓ zone join request + zone state wire formats");

    // 2. Live handoff over real UDP (no signaling server): two udp-only
    //    peers, a player X and the zone owner P2.
    let mut player = Store::new(&engine, HostState::new("player"));
    let mut owner = Store::new(&engine, HostState::new("peer2"));
    player
        .data_mut()
        .set_peer_connection(Some(NetLink::Udp(Box::new(PeerConnection::udp_only(
            "player",
            "127.0.0.1:0",
        )?))));
    owner
        .data_mut()
        .set_peer_connection(Some(NetLink::Udp(Box::new(PeerConnection::udp_only(
            "peer2",
            "127.0.0.1:0",
        )?))));

    let player_addr = player.data().peer_connection().unwrap().local_addr()?;
    let owner_addr = owner.data().peer_connection().unwrap().local_addr()?;

    // P2 owns chunk (1,0) and has live state for it. The claim reaches X's DHT.
    owner
        .data_mut()
        .claim_chunk_region(ChunkCoord { x: 1, z: 0 }, (1, 1))?;
    owner.data_mut().record_chunk_edit(ChunkEdit {
        x: 150.0,
        z: 30.0,
        kind: "ruin".into(),
        value: 9.0,
    })?;
    {
        let mut dht = player.data().chunk_registry().lock().unwrap();
        dht.apply_claim(
            &ChunkClaim::region("peer2", ChunkCoord { x: 1, z: 0 }, (1, 1)),
            owner_addr,
        );
    }

    // The player walks from chunk (0,0) into chunk (1,0).
    println!("player walks: (0,0) -> (1,0)");
    player
        .data_mut()
        .transition_zone(ChunkCoord { x: 1, z: 0 })?;

    // The host queried the DHT and connected directly to the zone owner.
    let zone_info = player
        .data()
        .current_zone()
        .expect("entered a zone")
        .clone();
    assert_eq!(zone_info.owner, "peer2", "DHT resolved chunk (1,0) owner");
    assert_eq!(
        zone_info.owner_addr, owner_addr,
        "DHT resolved the owner's address"
    );
    assert_eq!(
        player.data().peer_connection().unwrap().peer_addr("peer2"),
        Some(&owner_addr),
        "connected directly to the zone owner"
    );
    println!("✓ queried the DHT and connected to chunk (1,0) owner peer2");

    // Let the join request (sent by transition_zone) land on the owner's
    // socket, then the owner replies with live state, which the player reads.
    std::thread::sleep(std::time::Duration::from_millis(30));
    host::renderer::poll_network(&mut owner);
    std::thread::sleep(std::time::Duration::from_millis(30));
    // Player receives the owner's live state.
    host::renderer::poll_network(&mut player);
    let content = player
        .data()
        .zone_content()
        .cloned()
        .expect("downloaded the live state");
    assert_eq!(content.owner, "peer2");
    assert_eq!(
        content.edits.len(),
        1,
        "live state carries the owner's edits"
    );
    assert_eq!(content.edits[0].kind, "ruin");
    println!(
        "✓ downloaded peer2's live state ({} edits) over the wire",
        content.edits.len()
    );

    // 3. Transparent handoff: walking to a chunk owned by peer1 disconnects
    //    peer2's state stream and switches zones.
    let mut other_owner = Store::new(&engine, HostState::new("peer1"));
    other_owner
        .data_mut()
        .set_peer_connection(Some(NetLink::Udp(Box::new(PeerConnection::udp_only(
            "peer1",
            "127.0.0.1:0",
        )?))));
    let other_addr: SocketAddr = other_owner.data().peer_connection().unwrap().local_addr()?;
    {
        let mut dht = player.data().chunk_registry().lock().unwrap();
        dht.apply_claim(
            &ChunkClaim::region("peer1", ChunkCoord { x: 0, z: 0 }, (1, 1)),
            other_addr,
        );
    }
    player
        .data_mut()
        .transition_zone(ChunkCoord { x: 0, z: 0 })?;
    assert_eq!(player.data().current_zone().unwrap().owner, "peer1");
    assert_eq!(
        player.data().zone_content().map(|s| s.owner.clone()),
        None,
        "peer2's state stream was disconnected"
    );
    println!("✓ handoff to peer1: peer2's state stream disconnected, zone switched");

    // 4. A zone owner answers a join request with its live state.
    let mut responder = Store::new(&engine, HostState::new("peer2"));
    responder
        .data_mut()
        .set_peer_connection(Some(NetLink::Udp(Box::new(PeerConnection::udp_only(
            "peer2",
            "127.0.0.1:0",
        )?))));
    responder
        .data_mut()
        .claim_chunk_region(ChunkCoord { x: 5, z: 5 }, (1, 1))?;
    responder.data_mut().record_chunk_edit(ChunkEdit {
        x: 550.0,
        z: 550.0,
        kind: "ruin".into(),
        value: 2.0,
    })?;
    let join = ZoneJoinRequest {
        peer_id: "visitor".into(),
        chunk_x: 5,
        chunk_z: 5,
    };
    responder
        .data_mut()
        .handle_zone_join_request(&join, player_addr)?;
    // The visitor's socket now holds the reply.
    let mut buf = [0u8; 2048];
    player
        .data()
        .peer_connection()
        .unwrap()
        .recv_udp(&mut buf)?;
    println!("✓ zone owner answered a join request with its live state");

    println!("✓ all seamless zone transition checks passed");
    Ok(())
}
