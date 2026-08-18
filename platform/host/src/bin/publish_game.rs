use anyhow::{Context, Result};

use registry_server::{register_game, GameListing};
use std::path::{Path, PathBuf};

const PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/games");
const REGISTRY_URL: &str = "http://127.0.0.1:9002";

/// Commit 17/18: publishes the Chase Tag game to the local IPFS node and,
/// with `--register`, lists it in the game registry.
///
/// Bundles `guest.wasm + game_manifest.json + assets` into a `.tar`, adds it
/// to IPFS, and prints the resulting CID — the game's permanent address.
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let register = args.iter().any(|a| a == "--register");
    let ipfs_api = args
        .iter()
        .position(|a| a == "--ipfs")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| host::ipfs::IPFS_API.to_string());
    let description = args
        .iter()
        .position(|a| a == "--description")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "A simple tag game".to_string());

    let package_dir = PathBuf::from(PACKAGE_DIR);
    let assets_dir = PathBuf::from(ASSETS_DIR);
    let ipfs = host::ipfs::IpfsClient::new(&ipfs_api);

    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(package_dir.join("game_manifest.json"))?,
    )?;

    println!("bundling game package from {}...", package_dir.display());
    let tar_bytes = host::ipfs::bundle(&package_dir, &assets_dir)?;
    println!("bundle: {} bytes", tar_bytes.len());

    let cid = ipfs.add_bytes(&tar_bytes).context("publish failed")?;
    println!("✓ published game to IPFS: CID = {cid}");

    // Keep a local copy so the bundle can be inspected or re-pinned.
    let games_dir = Path::new(GAMES_DIR);
    std::fs::create_dir_all(games_dir)?;
    let tar_path = games_dir.join(format!("{cid}.tar"));
    std::fs::write(&tar_path, &tar_bytes)?;
    println!("local bundle saved to {}", tar_path.display());

    if register {
        let listing = GameListing {
            name: manifest["name"].as_str().unwrap_or("Unnamed").to_string(),
            cid: cid.clone(),
            description,
            author: manifest["author"].as_str().unwrap_or("unknown").to_string(),
            mode: manifest["mode"].as_str().unwrap_or("session").to_string(),
        };
        match register_game(REGISTRY_URL, &listing) {
            Ok(()) => println!("✓ registered '{name}' in the registry at {REGISTRY_URL}", name = listing.name),
            Err(e) => println!("⚠ registry registration skipped ({e:#}) — start it with: cargo run -p registry_server"),
        }
    }

    println!("share the CID with any player: cargo run -p host --bin play_game -- --cid {cid}");
    Ok(())
}

