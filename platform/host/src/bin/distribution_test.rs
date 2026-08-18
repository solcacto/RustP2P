use anyhow::{bail, Result};
use host::{ipfs, manifest::GameManifest};
use std::path::{Path, PathBuf};

const PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Commit 17 regression: publish the game to the local IPFS node, fetch it
/// back by CID, and verify the bundle is byte-identical and its wasm hash
/// still verifies (the distribution round-trip). Requires a Kubo daemon on
/// 127.0.0.1:5001.
fn main() -> Result<()> {
    let package_dir = PathBuf::from(PACKAGE_DIR);
    let assets_dir = PathBuf::from(ASSETS_DIR);

    // 1. Publish.
    let tar_bytes = ipfs::bundle(&package_dir, &assets_dir)?;
    let cid = ipfs::add_bytes(&tar_bytes)?;
    println!("✓ published bundle ({cid}, {} bytes)", tar_bytes.len());

    // 2. Fetch back by CID — must be byte-identical.
    let fetched = ipfs::cat(&cid)?;
    if fetched != tar_bytes {
        bail!("✗ fetched bundle differs from the published bundle");
    }
    println!("✓ fetched bundle by CID is byte-identical");

    // 3. Extract and verify the game package integrity end to end.
    let tmp = std::env::temp_dir().join(format!("rustp2p_dist_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    ipfs::extract(&fetched, &tmp)?;

    let manifest = GameManifest::from_path(tmp.join("game_manifest.json"))?;
    let wasm = std::fs::read(tmp.join(&manifest.wasm_entry))?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm)?;
    println!(
        "✓ extracted bundle verifies: {} v{} ({} bytes, hash OK)",
        manifest.name,
        manifest.version,
        wasm.len()
    );

    // 4. The distributed wasm matches the local build.
    let local_wasm = std::fs::read(package_dir.join(&manifest.wasm_entry))?;
    if local_wasm != wasm {
        bail!("✗ distributed wasm differs from the local package");
    }
    println!("✓ distributed wasm matches the local build");

    // 5. The bundle includes the game's assets.
    for required in ["assets/avatar_standard.glb", "assets/cosmetics/golden_sword/cosmetic_manifest.json"] {
        let p = Path::new(&tmp).join(required);
        if !p.exists() {
            bail!("✗ bundle missing '{required}'");
        }
    }
    println!("✓ bundle carries the game's assets");

    let _ = std::fs::remove_dir_all(&tmp);
    println!("✓ all IPFS distribution checks passed");
    Ok(())
}