use anyhow::Result;
use registry_server::serve;
use std::path::PathBuf;

const REGISTRY_FILE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/games.json");

fn main() -> Result<()> {
    let file = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(REGISTRY_FILE));
    serve(registry_server::REGISTRY_ADDR, &file)
}
