use anyhow::{Context, Result};

use registry_server::{fetch_games, GameListing};
use std::path::PathBuf;

const REGISTRY_URL: &str = "http://127.0.0.1:9002";
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/games");

/// Commit 18: the game browser client. Fetches the registry's `games.json`,
/// displays the available games, and can download a game by CID from IPFS.
///
///   `list_games`                       fetch + display the registry
///   `list_games --download NAME`       download a game by its registry entry
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let download = args
        .iter()
        .position(|a| a == "--download")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let ipfs_api = args
        .iter()
        .position(|a| a == "--ipfs")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| host::ipfs::IPFS_API.to_string());
    let ipfs = host::ipfs::IpfsClient::new(&ipfs_api);

    println!("fetching game list from {REGISTRY_URL}/games.json...");
    let games = fetch_games(REGISTRY_URL)?;
    if games.is_empty() {
        println!("no games registered yet");
        return Ok(());
    }

    display(&games);

    if let Some(needle) = download {
        let listing = games
            .iter()
            .find(|g| g.name == needle || g.cid == needle)
            .with_context(|| format!("no registered game matching '{needle}'"))?;
        let dir = download_game(&ipfs, listing)?;
        println!(
            "downloaded '{}' to {}\nlaunch it with: cargo run -p host --bin play_game -- --role A --cid {}",
            listing.name,
            dir.display(),
            listing.cid
        );
    }
    Ok(())
}

fn display(games: &[GameListing]) {
    println!();
    println!("{:<20} {:<48} {:<30} {:<10}", "NAME", "CID", "AUTHOR", "MODE");
    println!("{}", "-".repeat(110));
    for g in games {
        println!("{:<20} {:<48} {:<30} {:<10}", g.name, g.cid, g.author, g.mode);
        println!("    {}", g.description);
    }
    println!();
}

/// Downloads a game's bundle from IPFS by its registry CID, extracts it, and
/// pins it (becoming a seeder for the swarm). Returns the extracted package
/// directory.
fn download_game(ipfs: &host::ipfs::IpfsClient, listing: &GameListing) -> Result<PathBuf> {
    let games_dir = PathBuf::from(GAMES_DIR);
    let game_dir = games_dir.join(&listing.cid);
    if game_dir.join("game_manifest.json").exists() {
        println!("already downloaded '{}'", listing.cid);
        return Ok(game_dir);
    }
    println!("downloading '{}' from IPFS ({})...", listing.name, listing.cid);
    let tar_bytes = ipfs.cat(&listing.cid)?;
    let _ = std::fs::remove_dir_all(&game_dir);
    host::ipfs::extract(&tar_bytes, &game_dir)?;
    match ipfs.pin(&listing.cid) {
        Ok(()) => println!("pinned '{}' — this node is now a seeder", listing.cid),
        Err(e) => println!("warning: could not pin '{}': {e}", listing.cid),
    }
    Ok(game_dir)
}
