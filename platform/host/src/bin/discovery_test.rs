use anyhow::{bail, Result};
use host::{ipfs, manifest::GameManifest};
use registry_server::fetch_games;

const REGISTRY_URL: &str = "http://127.0.0.1:9002";

/// Commit 18 regression: the host fetches the game list from the registry,
/// finds a game, downloads its bundle from IPFS by CID, and verifies it.
/// Requires the registry server (9002) and a Kubo daemon (5001) running.
fn main() -> Result<()> {
    let games = fetch_games(REGISTRY_URL)?;
    if games.is_empty() {
        bail!("✗ registry returned an empty game list");
    }
    let listing = games
        .iter()
        .find(|g| g.name == "Chase Tag" || !g.cid.is_empty())
        .ok_or_else(|| anyhow::anyhow!("✗ no Chase Tag in the registry"))?;
    println!(
        "✓ fetched game list: '{}' cid={} mode={} author={}",
        listing.name, listing.cid, listing.mode, listing.author
    );

    let tar_bytes = ipfs::cat(&listing.cid)?;
    if tar_bytes.is_empty() {
        bail!("✗ empty bundle downloaded for '{}'", listing.name);
    }
    println!("✓ downloaded {} bytes for '{}' by CID", tar_bytes.len(), listing.name);

    let tmp = std::env::temp_dir().join(format!("rustp2p_discovery_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    ipfs::extract(&tar_bytes, &tmp)?;
    let manifest = GameManifest::from_path(tmp.join("game_manifest.json"))?;
    let wasm = std::fs::read(tmp.join(&manifest.wasm_entry))?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm)?;
    println!(
        "✓ downloaded game verifies: {} v{} (hash OK)",
        manifest.name, manifest.version
    );

    let _ = std::fs::remove_dir_all(&tmp);
    println!("✓ all discovery checks passed (list -> download -> verify)");
    Ok(())
}
