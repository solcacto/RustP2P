use anyhow::{Context, Result};
use host::ipfs;
use std::path::{Path, PathBuf};

const PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/games");

/// Commit 17: publishes the Chase Tag game to the local IPFS node.
///
/// Bundles `guest.wasm + game_manifest.json + assets` into a `.tar`, adds it
/// to IPFS, and prints the resulting CID — the game's permanent address.
fn main() -> Result<()> {
    let package_dir = PathBuf::from(PACKAGE_DIR);
    let assets_dir = PathBuf::from(ASSETS_DIR);

    println!("bundling game package from {}...", package_dir.display());
    let tar_bytes = ipfs::bundle(&package_dir, &assets_dir)?;
    println!("bundle: {} bytes", tar_bytes.len());

    let cid = ipfs::add_bytes(&tar_bytes).context("publish failed")?;
    println!("✓ published game to IPFS: CID = {cid}");

    // Keep a local copy so the bundle can be inspected or re-pinned.
    let games_dir = Path::new(GAMES_DIR);
    std::fs::create_dir_all(games_dir)?;
    let tar_path = games_dir.join(format!("{cid}.tar"));
    std::fs::write(&tar_path, &tar_bytes)?;
    println!("local bundle saved to {}", tar_path.display());
    println!("share the CID with any player: cargo run -p host --bin play_game -- --cid {cid}");
    Ok(())
}
