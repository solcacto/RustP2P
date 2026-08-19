use anyhow::{anyhow, bail, Result};
use host::{
    avatar_state::AvatarState,
    hybrid_connection::{HybridConfig, HybridConnection},
    net,
};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const SIGNAL_SERVER: &str = "127.0.0.1:9001";
/// An unused port — the WebRTC signaling attempt against it fails fast, so the
/// hybrid transport genuinely exercises its direct-UDP fallback.
const DEAD_SIGNAL_SERVER: &str = "127.0.0.1:9";

/// Which transport scenario to run.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Mode {
    /// Both peers connect over WebRTC (offer/answer/ICE on loopback).
    Webrtc,
    /// The WebRTC attempt fails (signaling unreachable); peers fall back to
    /// direct UDP.
    Fallback,
}

fn config(mode: Mode) -> HybridConfig {
    let webrtc_signal_addr = match mode {
        Mode::Webrtc => None,
        Mode::Fallback => Some(DEAD_SIGNAL_SERVER.to_string()),
    };
    HybridConfig {
        webrtc_timeout: Duration::from_secs(10),
        webrtc_signal_addr,
        ..Default::default()
    }
}

fn peer_a(mode: Mode) -> Result<()> {
    let conn = HybridConnection::establish(
        "NAT-A",
        "NAT-B",
        SIGNAL_SERVER,
        "127.0.0.1:0",
        &config(mode),
    )?;
    let transport = conn.stats().connection_type;
    println!("[A] connected to NAT-B via {transport}");
    if mode == Mode::Webrtc && transport != "WebRTC (direct)" {
        bail!("[A] expected WebRTC transport, got {transport}");
    }
    if mode == Mode::Fallback && transport != "Direct UDP" {
        bail!("[A] expected direct-UDP fallback, got {transport}");
    }

    // Batched + delta-compressed pose traffic over the transport.
    let mut last: Option<AvatarState> = None;
    let mut cur = AvatarState::default();
    for _ in 0..5 {
        cur.x += 0.05;
        cur.rot_y += 0.01;
        let payload = net::encode_pose(last.as_ref(), &cur);
        conn.batch_send(&payload);
        last = Some(cur);
    }
    conn.batch_send(&42u32.to_le_bytes());
    conn.flush_batches();

    // Continuously poll (echoing pings) and ping until B's batched reply
    // (pose + score) arrives and at least one RTT has been measured (pongs are
    // consumed and timed inside `poll_received`, surfacing via `stats().rtt_ms`).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut received: Vec<Vec<u8>> = Vec::new();
    loop {
        conn.ping();
        conn.flush_batches();
        received.extend(conn.poll_received());
        let rtt = conn.stats().rtt_ms;
        if received.len() >= 2 && rtt > 0.0 {
            println!("[A] RTT over {transport}: {rtt:.2}ms");
            break;
        }
        if Instant::now() >= deadline {
            bail!(
                "[A] never received NAT-B's batched reply (got {}, rtt {:.2}ms)",
                received.len(),
                conn.stats().rtt_ms
            );
        }
        thread::sleep(Duration::from_millis(20));
    }

    // Verify B's score arrived (the other payload is B's pose).
    let scores: Vec<u32> = received
        .iter()
        .filter(|p| p.len() == 4)
        .map(|p| u32::from_le_bytes(p[0..4].try_into().unwrap()))
        .collect();
    if !scores.contains(&7) {
        bail!("[A] expected NAT-B's score 7, got {scores:?}");
    }
    println!("[A] ✓ received NAT-B's batched reply (pose + score 7)");

    let stats = conn.stats();
    println!(
        "[A] stats: {transport} | avg batch {:.2} msgs/frame | {:.2} KiB/s | {} frames sent",
        stats.avg_batch_size, stats.bandwidth_kbps, stats.packets_sent
    );
    if mode == Mode::Webrtc && stats.avg_batch_size < 2.0 {
        bail!(
            "[A] batching lost over WebRTC (avg {:.2} msgs/frame)",
            stats.avg_batch_size
        );
    }
    conn.close()?;
    Ok(())
}

fn peer_b(mode: Mode) -> Result<()> {
    let conn = HybridConnection::establish(
        "NAT-B",
        "NAT-A",
        SIGNAL_SERVER,
        "127.0.0.1:0",
        &config(mode),
    )?;
    let transport = conn.stats().connection_type;
    println!("[B] connected to NAT-A via {transport}");
    if mode == Mode::Webrtc && transport != "WebRTC (direct)" {
        bail!("[B] expected WebRTC transport, got {transport}");
    }
    if mode == Mode::Fallback && transport != "Direct UDP" {
        bail!("[B] expected direct-UDP fallback, got {transport}");
    }

    // Poll continuously (echoing A's pings); as soon as A's batched poses and
    // score arrive, reply with one batched frame (pose + score 7) and keep
    // polling so A can measure RTT.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_pose: Option<host::avatar_state::AvatarPose> = None;
    let mut poses_decoded = 0usize;
    let mut scores: Vec<u32> = Vec::new();
    while poses_decoded < 5 || scores.is_empty() {
        conn.ping();
        conn.flush_batches();
        for p in conn.poll_received() {
            match p.len() {
                8 | 16 => {
                    if let Some(pose) = net::decode_pose(last_pose.as_ref(), &p) {
                        last_pose = Some(pose);
                        poses_decoded += 1;
                    }
                }
                4 => scores.push(u32::from_le_bytes(p[0..4].try_into().unwrap())),
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "[B] never received NAT-A's batched poses (poses {poses_decoded}/5, scores {scores:?})"
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    if !scores.contains(&42) {
        bail!("[B] expected NAT-A's score 42, got {scores:?}");
    }
    let mut cur = AvatarState::default();
    cur.z += 0.03;
    cur.rot_y += 0.02;
    conn.batch_send(&net::encode_pose(None, &cur));
    conn.batch_send(&7u32.to_le_bytes());
    conn.flush_batches();
    println!(
        "[B] ✓ received NAT-A's batched+compressed poses ({poses_decoded}) and score 42; replied"
    );

    // Keep polling (echoing A's pings) until A closes the link or we time out.
    let reply_deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < reply_deadline {
        conn.ping();
        conn.flush_batches();
        let _ = conn.poll_received();
        thread::sleep(Duration::from_millis(20));
    }

    conn.close()?;
    Ok(())
}

fn main() -> Result<()> {
    let mode = if std::env::args().any(|a| a == "--fallback") {
        Mode::Fallback
    } else {
        Mode::Webrtc
    };
    println!("=== NAT Traversal Test ({mode:?}) ===");

    let (tx, rx) = mpsc::channel();
    let tx2 = tx.clone();
    let a = thread::spawn(move || {
        let r = peer_a(mode);
        let _ = tx.send(("A", r));
    });
    let b = thread::spawn(move || {
        let r = peer_b(mode);
        let _ = tx2.send(("B", r));
    });

    let mut results = Vec::new();
    for _ in 0..2 {
        results.push(rx.recv().map_err(|_| anyhow!("peer thread panicked"))?);
    }
    let _ = a.join();
    let _ = b.join();

    for (name, r) in &results {
        match r {
            Ok(()) => {}
            Err(e) => bail!("peer {name} failed: {e:#}"),
        }
    }

    match mode {
        Mode::Webrtc => {
            println!("✓ WebRTC SDP offer/answer exchanged via signaling server");
            println!("✓ ICE candidates exchanged; direct P2P data channel opened");
            println!("✓ Batched + delta-compressed game traffic over WebRTC data channel");
        }
        Mode::Fallback => {
            println!("✓ WebRTC attempt failed -> fell back to direct UDP");
            println!("✓ Game traffic over the direct-UDP fallback");
        }
    }
    println!("✓ NAT traversal validated");
    Ok(())
}
