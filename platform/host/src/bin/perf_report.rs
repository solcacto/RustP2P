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
    let loopback_rtt = measure_loopback_rtt();
    print_report(&summary, mem_before, mem_after, wasm_size, frames, loopback_rtt);
    Ok(())
}

fn print_report(
    s: &TimingSummary,
    mem_before: f64,
    mem_after: f64,
    wasm_size: usize,
    frames: u32,
    loopback_rtt: Option<f64>,
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
    match loopback_rtt {
        Some(rtt) => println!("    Loopback RTT: {:.3}ms", rtt),
        None => println!("    Loopback RTT: n/a"),
    }
    println!("    (per-peer RTT is logged by play_game's `measure_rtt`)");
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

/// Measures loopback UDP round-trip time with two `udp_only` connections.
fn measure_loopback_rtt() -> Option<f64> {
    let peer = PeerConnection::udp_only("rtt-a", "127.0.0.1:0").ok()?;
    let target = PeerConnection::udp_only("rtt-b", "127.0.0.1:0").ok()?;
    let target_addr = target.local_addr().ok()?;
    let mut buf = [0u8; 64];
    let mut samples = Vec::new();
    for _ in 0..8 {
        let start = std::time::Instant::now();
        peer.send_udp(target_addr, b"probe").ok()?;
        let (_, _) = target.recv_udp(&mut buf).ok()?;
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Some(samples.iter().sum::<f64>() / samples.len() as f64)
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
