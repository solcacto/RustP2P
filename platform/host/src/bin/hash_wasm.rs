use anyhow::Result;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Prints the `sha256:<hex>` digest of a file, for pinning `wasm_hash` in a
/// game manifest after rebuilding the guest:
///
/// ```sh
/// cargo run -p host --bin hash_wasm -- platform/guest/guest.wasm
/// ```
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: hash_wasm <path-to-wasm>"))?;
    let bytes = std::fs::read(&path)?;
    println!("sha256:{}", hex::encode(Sha256::digest(&bytes)));
    Ok(())
}