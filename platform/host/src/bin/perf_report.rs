use anyhow::{bail, Result};
use host::{
    avatar_state::{AvatarPose, AvatarState},
    host_functions,
    host_state::HostState,
    manifest::GameManifest,
    peer_connection::PeerConnection,
    profiling::{SharedFrameStats, TimingSummary},
    renderer,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use sysinfo::{Pid, System};

const PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Profiling command: runs the game for N frames collecting per-phase timing,
/// measures memory, and prints a summary report.
///
///   cargo run -p host --bin perf_report [--frames 1000]
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let frames: u32 = args
        .iter()
        .position(|a| a == "--frames")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);

    // Load + verify the guest package.
    let manifest = GameManifest::from_path(PathBuf::from(PACKAGE_DIR).join("game_manifest.json"))?;
    let wasm = std::fs::read(PathBuf::from(PACKAGE_DIR).join(&manifest.wasm_entry))?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm)?;

    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let stats = SharedFrameStats::default();

    // Instantiate the guest.
    let engine = wasmtime::Engine::default();
    let mut linker = wasmtime::Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wasm_size = wasm.len();
    let module = wasmtime::Module::new(&engine, wasm)?;
    let mut store = wasmtime::Store::new(&engine, HostState::new("perf"));
    store.data_mut().set_avatar_state(Some(avatar_state.clone()));
    store.data_mut().set_remote_avatars(remote_avatars.clone());
    let instance = linker.instantiate(&mut store, &module)?;
    let tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick")?;

    // Build the app with our stats collector, run N frames, then exit.
    let mut app = renderer::build_app(avatar_state.clone(), remote_avatars.clone());
    app.insert_resource(stats.clone());
    let store_handle = Arc::new(Mutex::new(store));
    app.insert_resource(renderer::WasmRuntime {
        store: store_handle.clone(),
        render_tick: tick,
    });
    renderer::add_exit_after(&mut app, frames);

    let mem_before = process_rss_mb();
    let exit = app.run();
    if !exit.is_success() {
        bail!("renderer exited with an error: {exit:?}");
    }
    let mem_after = process_rss_mb();

    let summary = stats.0.lock().unwrap().summary();
    let network = measure_network();
    print_report(&summary, mem_before, mem_after, wasm_size, frames, &network);
    Ok(())
}

/// Network performance measured over a loopback batched+compressed session
/// between two `udp_only` connections.
#[derive(Debug, Clone)]
struct NetworkReport {
    /// Round-trip time (ms), microsecond timing.
    rtt_ms: f64,
    /// Estimated packet loss (batch sequence gaps).
    packet_loss_percent: f64,
    /// Bandwidth (KiB/s) during the batched exchange.
    bandwidth_kbps: f64,
    /// Average inner messages per sent UDP packet.
    avg_batch_size: f64,
    /// Bandwidth reduction vs sending full 16-byte poses.
    compression_ratio_pct: f64,
}

fn measure_network() -> NetworkReport {
    use host::avatar_state::AvatarState;
    use host::net;

    let mut a = PeerConnection::udp_only("net-a", "127.0.0.1:0").unwrap();
    let mut b = PeerConnection::udp_only("net-b", "127.0.0.1:0").unwrap();
    let b_addr = b.local_addr().unwrap();
    let a_addr = a.local_addr().unwrap();
    a.connect_direct("peer-b", b_addr);
    b.connect_direct("peer-a", a_addr);

    // RTT with microsecond timing: tight non-blocking poll (no sleep artifact).
    let mut rtts = Vec::new();
    let mut buf = [0u8; 64];
    for _ in 0..20 {
        let start = std::time::Instant::now();
        a.send_udp(b_addr, b"probe").unwrap();
        loop {
            if b.try_recv_udp(&mut buf).is_some() {
                break;
            }
            if start.elapsed() > std::time::Duration::from_millis(100) {
                break;
            }
        }
        rtts.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    let rtt = rtts.iter().sum::<f64>() / rtts.len() as f64;

    // Batched + delta-compressed pose traffic: 5 pose messages + 1 score per
    // batch, paced at one batch per 16 ms frame (game cadence) so bandwidth is
    // the sustained rate.
    let start = std::time::Instant::now();
    let mut last: Option<AvatarState> = None;
    let mut full_poses = 0usize;
    let mut delta_poses = 0usize;
    for _ in 0..40 {
        let mut cur = last.unwrap_or_default();
        cur.x += 0.05;
        cur.rot_y += 0.01;
        for _ in 0..5 {
            let payload = net::encode_pose(last.as_ref(), &cur);
            if payload.len() == net::POSE_FULL_LEN {
                full_poses += 1;
            } else {
                delta_poses += 1;
            }
            a.batch_send_to_all(&payload);
            last = Some(cur);
        }
        a.batch_send_to_all(&3u32.to_le_bytes());
        a.flush_batches();
        b.poll_incoming_from();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    let elapsed = start.elapsed();

    let stats = a.stats();
    let total_poses = (full_poses + delta_poses) as f64;
    let avg_pose_bytes = (full_poses as f64 * 16.0 + delta_poses as f64 * 8.0) / total_poses;
    let compression_ratio = ((16.0 - avg_pose_bytes) / 16.0 * 100.0).max(0.0);
    let bandwidth = (stats.bytes_sent as f64 / 1024.0) / elapsed.as_secs_f64().max(0.001);

    NetworkReport {
        rtt_ms: rtt,
        packet_loss_percent: stats.packet_loss_percent,
        bandwidth_kbps: bandwidth,
        avg_batch_size: stats.avg_batch_size,
        compression_ratio_pct: compression_ratio,
    }
}

fn print_report(
    s: &TimingSummary,
    mem_before: f64,
    mem_after: f64,
    wasm_size: usize,
    frames: u32,
    network: &NetworkReport,
) {
    let fps = if s.avg_frame_ms > 0.0 { 1000.0 / s.avg_frame_ms } else { 0.0 };
    println!("\n=== Performance Report ({frames} frames) ===");
    println!("  Average frame time: {:.2}ms ({:.1} FPS)", s.avg_frame_ms, fps);
    println!("  Frame time percentiles: p50 {:.2}ms | p95 {:.2}ms | p99 {:.2}ms", s.p50_frame_ms, s.p95_frame_ms, s.p99_frame_ms);
    println!();
    println!("  Breakdown (per frame):");
    let total = s.avg_frame_ms.max(0.001);
    let other = (s.avg_frame_ms - s.avg_input_ms - s.avg_network_ms - s.avg_wasm_ms - s.avg_bevy_ms).max(0.0);
    for (label, ms) in [
        ("Input polling", s.avg_input_ms),
        ("Network polling", s.avg_network_ms),
        ("Wasm execution", s.avg_wasm_ms),
        ("Bevy rendering (incl. other systems)", s.avg_bevy_ms),
        ("Other", other),
    ] {
        println!("    {label:<40} {ms:6.2}ms ({:4.1}%)", ms / total * 100.0);
    }
    println!();
    println!("  Network stats:");
    println!("    RTT: {:.2}ms", network.rtt_ms);
    println!("    Packet loss: {:.2}%", network.packet_loss_percent);
    println!("    Bandwidth: {:.2} KiB/s", network.bandwidth_kbps);
    println!("    Avg batch size: {:.2} messages/packet", network.avg_batch_size);
    println!("    Compression ratio (delta vs full pose): {:.0}%", network.compression_ratio_pct);
    println!();
    println!("  Memory:");
    println!("    Process RSS before: {:.1} MB | after: {:.1} MB (delta {:.1} MB)", mem_before, mem_after, mem_after - mem_before);
    println!("    Wasm module: {:.2} MB", wasm_size as f64 / 1024.0 / 1024.0);
    println!("    Host assets dir: {:.2} MB", dir_size_mb(PathBuf::from(ASSETS_DIR)));
    println!();
    println!("  Bottlenecks:");
    let mut phases: Vec<(&str, f64)> = vec![
        ("Bevy rendering", s.avg_bevy_ms),
        ("Wasm execution", s.avg_wasm_ms),
        ("Network polling", s.avg_network_ms),
        ("Input polling", s.avg_input_ms),
    ];
    phases.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    for (i, (name, ms)) in phases.iter().take(3).enumerate() {
        println!("    {}. {name} ({ms:.2}ms, {:.1}% of frame)", i + 1, ms / total * 100.0);
    }
}

fn process_rss_mb() -> f64 {
    let mut sys = System::new();
    sys.refresh_processes();
    sys.process(Pid::from_u32(std::process::id()))
        .map(|p| p.memory() as f64 / 1024.0 / 1024.0)
        .unwrap_or(0.0)
}

fn dir_size_mb(dir: PathBuf) -> f64 {
    fn walk(d: &std::path::Path, total: &mut u64) {
        if let Ok(rd) = std::fs::read_dir(d) {
            for e in rd.flatten() {
                if e.path().is_dir() {
                    walk(&e.path(), total);
                } else if let Ok(md) = e.metadata() {
                    *total += md.len();
                }
            }
        }
    }
    let mut total = 0u64;
    walk(&dir, &mut total);
    total as f64 / 1024.0 / 1024.0
}
