//! Zero-cost deployment configuration and the public game registry client.
//!
//! Reads `config.toml` (deployment URLs for signaling, STUN/TURN, the GitHub
//! Pages registry, and a public IPFS gateway) and talks to the live
//! infrastructure without any of it being self-hosted:
//!
//! - The **registry** is a static `games.json` on GitHub Pages: each entry
//!   carries a game's IPFS CID.
//! - **Bundles** are downloaded straight from a public IPFS gateway
//!   (`/ipfs/<CID>`), so players never need a local Kubo node.
//!
//! HTTP fetches are async so a slow registry/gateway never blocks the game
//! thread — the request runs on a blocking worker (the crate's existing `ureq`
//! HTTPS stack) and is awaited from the caller.

use anyhow::{Context, Result};
use registry_server::GameListing;
use serde::Deserialize;
use std::io::Read;
use std::path::Path;

/// Free-tier default: Google's public STUN server.
pub const DEFAULT_STUN: &str = "stun:stun.l.google.com:19302";
/// Public IPFS gateway used when the config doesn't specify one.
pub const DEFAULT_IPFS_GATEWAY: &str = "https://cloudflare-ipfs.com";

/// Root of `config.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct DeploymentConfig {
    pub network: NetworkConfig,
}

/// The `[network]` section of `config.toml`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct NetworkConfig {
    /// WebSocket signaling URL (Cloudflare Worker) for WebRTC SDP/ICE.
    pub signaling_url: String,
    /// STUN servers for NAT discovery.
    pub stun_servers: Vec<String>,
    /// TURN relay servers (empty during beta).
    pub turn_servers: Vec<String>,
    pub turn_username: String,
    pub turn_password: String,
    /// URL of the `games.json` registry hosted on GitHub Pages.
    pub registry_url: String,
    /// Public IPFS gateway base URL used to download game bundles.
    #[serde(default = "default_ipfs_gateway")]
    pub ipfs_gateway: String,
}

fn default_ipfs_gateway() -> String {
    DEFAULT_IPFS_GATEWAY.to_string()
}

impl DeploymentConfig {
    /// Loads and parses `config.toml` from `path`.
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read deployment config at {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("invalid config.toml at {}", path.display()))
    }
}

/// Fetches the game registry (`games.json`) from the full `registry_url` and
/// parses it into a list of games. Non-blocking: the HTTP round-trip runs on a
/// blocking worker task.
pub async fn fetch_registry(registry_url: &str) -> Result<Vec<GameListing>> {
    let url = registry_url.to_string();
    let games = tokio::task::spawn_blocking(move || -> Result<Vec<GameListing>> {
        let response = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(15))
            .call()
            .with_context(|| format!("failed fetching registry from {url}"))?;
        let body = response
            .into_string()
            .context("registry returned invalid body")?;
        serde_json::from_str(&body).context("registry returned an invalid games.json")
    })
    .await
    .context("registry fetch task panicked")??;
    Ok(games)
}

/// Downloads a game bundle from a public IPFS gateway by CID. The returned
/// bytes are the `.tar`/`.tar.gz` bundle (`host::ipfs::extract` decompresses
/// gzip automatically).
pub async fn download_from_gateway(gateway_base: &str, cid: &str) -> Result<Vec<u8>> {
    let url = format!("{}/ipfs/{cid}", gateway_base.trim_end_matches('/'));
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let response = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(60))
            .call()
            .with_context(|| format!("failed downloading bundle from {url}"))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(256 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed reading bundle from {url}"))?;
        Ok(bytes)
    })
    .await
    .context("gateway download task panicked")??;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_the_shipped_config_toml() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml");
        let cfg = DeploymentConfig::load(&path).expect("config.toml must parse");
        assert_eq!(
            cfg.network.signaling_url,
            "wss://p2p-signal.solcacto-p2p.workers.dev"
        );
        assert_eq!(
            cfg.network.registry_url,
            "https://solcacto.github.io/my-platform-registry/games.json"
        );
        assert_eq!(cfg.network.stun_servers, vec![DEFAULT_STUN.to_string()]);
        assert!(cfg.network.turn_servers.is_empty());
    }

    /// Serves a mock `games.json` from a throwaway registry server and asserts
    /// the async client parses it into the expected `GameListing`s.
    #[tokio::test]
    async fn fetches_and_parses_a_mock_games_json() {
        // High, unlikely-to-collide port for the throwaway registry server.
        const MOCK_BASE: &str = "127.0.0.1:19002";
        let registry_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("registry_server/games.json");
        std::thread::spawn(move || {
            let _ = registry_server::serve(MOCK_BASE, &registry_path);
        });
        // Give the server a moment to bind before the first request.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;

        let games = fetch_registry(&format!("http://{MOCK_BASE}/games.json"))
            .await
            .expect("mock registry must be reachable and parseable");
        assert!(!games.is_empty(), "mock registry should contain games");
        assert!(games
            .iter()
            .all(|g| !g.name.is_empty() && !g.cid.is_empty()));
    }
}
