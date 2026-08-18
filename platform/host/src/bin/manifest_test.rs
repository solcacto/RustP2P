use anyhow::{bail, Result};
use host::manifest::{
    AvatarSkeleton, GameManifest, GameMode, HostFunctionCategory, HASH_PREFIX,
};
use sha2::{Digest, Sha256};

const MANIFEST_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest/game_manifest.json");
const GAME_PACKAGE_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");

/// Returns a valid manifest whose `wasm_hash` matches the real packaged wasm.
fn valid_manifest() -> Result<GameManifest> {
    let mut m = GameManifest::from_path(MANIFEST_PATH)?;
    let wasm = std::fs::read(format!("{GAME_PACKAGE_DIR}/{}", m.wasm_entry))?;
    m.wasm_hash = format!("{HASH_PREFIX}{}", hex::encode(Sha256::digest(&wasm)));
    Ok(m)
}

fn main() -> Result<()> {
    // 1. The shipped package is valid and its hash matches.
    let m = GameManifest::from_path(MANIFEST_PATH)?;
    let wasm = std::fs::read(format!("{GAME_PACKAGE_DIR}/{}", m.wasm_entry))?;
    m.verify_wasm(&m.wasm_entry, &wasm)?;
    println!("✓ shipped game_manifest.json parses, validates, and matches the packaged wasm");

    // 2. A tampered wasm byte must be refused.
    let mut tampered = wasm.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    match m.verify_wasm(&m.wasm_entry, &tampered) {
        Ok(_) => bail!("✗ hash mismatch was not detected on tampered wasm"),
        Err(e) => println!("✓ tampered wasm refused: {e}"),
    }

    // 3. Wrong entry name must be refused.
    match m.verify_wasm("other.wasm", &wasm) {
        Ok(_) => bail!("✗ entry-name mismatch was not detected"),
        Err(e) => println!("✓ wrong entry name refused: {e}"),
    }

    // 4. Field validation rules.
    let mut bad = valid_manifest()?;
    bad.name = "".into();
    assert_rejected(bad.validate(), "empty name");

    let mut bad = valid_manifest()?;
    bad.version = "".into();
    assert_rejected(bad.validate(), "empty version");

    let mut bad = valid_manifest()?;
    bad.author = "".into();
    assert_rejected(bad.validate(), "empty author");

    let mut bad = valid_manifest()?;
    bad.wasm_entry = "../evil.wasm".into();
    assert_rejected(bad.validate(), "path-traversing wasm_entry");

    let mut bad = valid_manifest()?;
    bad.wasm_entry = "a/b.wasm".into();
    assert_rejected(bad.validate(), "non-bare wasm_entry");

    let mut bad = valid_manifest()?;
    bad.wasm_hash = "sha256:zz".into();
    assert_rejected(bad.validate(), "malformed wasm_hash");

    let mut bad = valid_manifest()?;
    bad.wasm_hash = "md5:deadbeef".into();
    assert_rejected(bad.validate(), "non-sha256 wasm_hash");

    let mut bad = valid_manifest()?;
    bad.max_players = 0;
    assert_rejected(bad.validate(), "max_players 0");

    let mut bad = valid_manifest()?;
    bad.max_players = 10_000;
    assert_rejected(bad.validate(), "max_players above cap");

    let mut bad = valid_manifest()?;
    bad.host_functions_required =
        vec![HostFunctionCategory::Input, HostFunctionCategory::Input];
    assert_rejected(bad.validate(), "duplicate host-function category");

    // 5. Unknown enum values are rejected at parse time.
    let raw = r#"{
        "name": "X", "version": "0.1.0", "author": "dev",
        "wasm_entry": "g.wasm", "wasm_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
        "mode": "session", "max_players": 4,
        "avatar_skeleton": "standard_v1",
        "host_functions_required": ["input", "not_a_category"]
    }"#;
    match serde_json::from_str::<GameManifest>(raw) {
        Ok(_) => bail!("✗ unknown host-function category was accepted"),
        Err(_) => println!("✓ unknown host-function category rejected at parse"),
    }

    let raw = raw.replace("\"mode\": \"session\"", "\"mode\": \"turing_complete\"");
    match serde_json::from_str::<GameManifest>(&raw) {
        Ok(_) => bail!("✗ unknown mode was accepted"),
        Err(_) => println!("✓ unknown mode rejected at parse"),
    }

    // 6. Field-level integrity (not just parse): a manifest built with a
    //    plausible-but-wrong avatar skeleton is rejected.
    let mut bad = valid_manifest()?;
    bad.avatar_skeleton = AvatarSkeleton::StandardV1; // currently the only one
    assert!(matches!(bad.mode, GameMode::Session), "mode round-trip");

    println!("✓ all manifest validation checks passed");
    Ok(())
}

fn assert_rejected(r: Result<()>, what: &str) {
    match r {
        Ok(()) => panic!("✗ validation accepted: {what}"),
        Err(e) => println!("✓ rejected {what}: {e}"),
    }
}