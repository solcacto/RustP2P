//! The lightweight game registry: the platform's only centralized component.
//!
//! A registry is just a `games.json` list of available games — name, CID,
//! description, author, and mode. It is intentionally trivial: anyone can host
//! a mirror, serve a static `games.json`, or put the file on IPFS. Clients
//! fetch the list over HTTP, display it, and download/play games by CID (see
//! `docs/` and the host's `list_games` / `play_game --cid`).
//!
//! `GameListing` is the shared schema; `serve` runs the tiny HTTP server and
//! `fetch_games` / `register_game` are the client helpers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Default bind address of the registry server.
pub const REGISTRY_ADDR: &str = "127.0.0.1:9002";

/// One entry in the game registry (`games.json`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameListing {
    /// Human-readable game title.
    pub name: String,
    /// IPFS CID of the published game bundle.
    pub cid: String,
    /// Short description shown in the game browser.
    pub description: String,
    /// Creator identity (peer id or `ed25519:` public key).
    pub author: String,
    /// Session mode, e.g. `"session"`.
    pub mode: String,
}

/// Fetches the game list from a registry base URL (e.g.
/// `http://127.0.0.1:9002`).
pub fn fetch_games(base_url: &str) -> Result<Vec<GameListing>> {
    let url = format!("{}/games.json", base_url.trim_end_matches('/'));
    let response = ureq::get(&url)
        .call()
        .with_context(|| format!("failed fetching game list from {url}"))?;
    let games: Vec<GameListing> = serde_json::from_str(&response.into_string()?)
        .context("registry returned an invalid games.json")?;
    Ok(games)
}

/// Registers (or updates) a game in the registry.
pub fn register_game(base_url: &str, listing: &GameListing) -> Result<()> {
    let url = format!("{}/games", base_url.trim_end_matches('/'));
    let body = serde_json::to_vec(listing)?;
    let response = ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_bytes(&body)
        .with_context(|| format!("failed registering game at {url}"))?;
    if !(200..300).contains(&response.status()) {
        anyhow::bail!("registry returned status {}", response.status());
    }
    Ok(())
}

/// Runs the registry server until the process exits. Blocks forever.
///
/// - `GET /games.json` returns the current registry.
/// - `POST /games` appends (or replaces, by CID) a game and persists the
///   registry to `registry_path`.
pub fn serve(bind_addr: &str, registry_path: &std::path::Path) -> Result<()> {
    let server = tiny_http::Server::http(bind_addr)
        .map_err(|e| anyhow::anyhow!("failed to bind registry server: {e}"))?;
    println!("Registry server listening on http://{bind_addr}");

    let mut games: Vec<GameListing> = std::fs::read_to_string(registry_path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    println!("Loaded {} game(s) from {}", games.len(), registry_path.display());

    for mut request in server.incoming_requests() {
        match (request.method(), request.url()) {
            (&tiny_http::Method::Get, "/games.json") => {
                let body = serde_json::to_string_pretty(&games)?;
                let response = tiny_http::Response::from_string(body)
                    .with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                            .expect("valid header"),
                    )
                    .with_status_code(200);
                let _ = request.respond(response);
            }
            (&tiny_http::Method::Post, "/games") => {
                let mut body = Vec::new();
                use std::io::Read;
                let _ = request.as_reader().take(1024 * 1024).read_to_end(&mut body);
                match serde_json::from_slice::<GameListing>(&body) {
                    Ok(listing) => {
                        games.retain(|g| g.cid != listing.cid);
                        games.push(listing);
                        persist(&games, registry_path)?;
                        let response = tiny_http::Response::from_string("registered\n")
                            .with_status_code(200);
                        let _ = request.respond(response);
                    }
                    Err(e) => {
                        eprintln!("invalid registration: {e}");
                        let response =
                            tiny_http::Response::from_string(format!("invalid body: {e}\n"))
                                .with_status_code(400);
                        let _ = request.respond(response);
                    }
                }
            }
            _ => {
                let response = tiny_http::Response::from_string("not found\n")
                    .with_status_code(404);
                let _ = request.respond(response);
            }
        }
    }
    Ok(())
}

fn persist(games: &[GameListing], registry_path: &std::path::Path) -> Result<()> {
    let json = serde_json::to_string_pretty(games)?;
    std::fs::write(registry_path, json)
        .with_context(|| format!("cannot persist registry to {}", registry_path.display()))?;
    Ok(())
}
