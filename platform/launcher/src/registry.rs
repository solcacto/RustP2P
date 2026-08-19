//! Minimal registry client used by the launcher: fetch and parse `games.json`.

use anyhow::{Context, Result};
use serde::Deserialize;

/// One entry in the game registry (`games.json`). Mirrors the shared schema
/// from `registry_server::GameListing`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameListing {
    pub name: String,
    pub cid: String,
    pub description: String,
    pub author: String,
    pub mode: String,
}

/// Fetches and parses the game list from a full `games.json` URL.
pub fn fetch_registry(url: &str) -> Result<Vec<GameListing>> {
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .with_context(|| format!("failed fetching registry from {url}"))?;
    let body = response
        .into_string()
        .context("registry returned invalid body")?;
    serde_json::from_str(&body).context("registry returned an invalid games.json")
}