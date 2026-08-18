use anyhow::{bail, Result};
use host::{ipfs::IpfsClient, manifest::GameManifest};

/// Commit 19: verifies a node can fetch a game bundle from the swarm, pin it
/// (becoming a seeder), and that the bundle verifies end to end.
///
/// Usage: `cache_test <CID> <api-url>` (e.g. `cache_test QmX... http://127.0.0.1:5002/api/v0`).
///
/// The swarm scenario (driven by the test harness):
///   1. Machine A publishes the game (its node has the content).
///   2. Machine B runs `cache_test <CID> <B-api>`  → fetches from A, pins.
///   3. Machine A goes offline.
///   4. Machine C runs `cache_test <CID> <C-api>`  → fetches from B.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        bail!("usage: cache_test <CID> <api-url>");
    }
    let cid = args[1].clone();
    let api = args[2].clone();
    let ipfs = IpfsClient::new(&api);

    println!("[{api}] fetching '{cid}' from the IPFS swarm...");
    let tar_bytes = ipfs.cat(&cid)?;
    println!("[{api}] fetched {} bytes", tar_bytes.len());
    if tar_bytes.is_empty() {
        bail!("[{api}] ✗ empty bundle");
    }

    // Pin so this node seeds the game for other players.
    ipfs.pin(&cid)?;
    println!("[{api}] pinned '{cid}' — this node is now a seeder");

    // Verify the bundle is a valid game package.
    let tmp = std::env::temp_dir().join(format!("rustp2p_cache_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    host::ipfs::extract(&tar_bytes, &tmp)?;
    let manifest = GameManifest::from_path(tmp.join("game_manifest.json"))?;
    let wasm = std::fs::read(tmp.join(&manifest.wasm_entry))?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm)?;
    println!(
        "[{api}] ✓ fetched '{}' v{} by CID (hash OK, now seeding)",
        manifest.name, manifest.version
    );
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}
