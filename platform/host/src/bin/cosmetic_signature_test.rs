use anyhow::{bail, Result};
use host::cosmetic::{verify, verify_package, CosmeticManifest};
use std::path::Path;

const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");
const GOLDEN_SWORD: &str = "cosmetics/golden_sword/cosmetic_manifest.json";
const SIMPLE_HAT: &str = "cosmetics/simple_hat/cosmetic_manifest.json";

/// Commit 16 regression: a valid signed cosmetic verifies (and would render),
/// while tampering with the mesh or the manifest makes the signature fail and
/// the cosmetic must be silently refused.
fn main() -> Result<()> {
    // 1. Both shipped packages verify (valid signatures over identity + mesh).
    for manifest in [GOLDEN_SWORD, SIMPLE_HAT] {
        let pkg = verify_package(ASSETS_DIR, manifest)?;
        println!(
            "✓ verified cosmetic '{id}' -> {point} (mesh {mesh})",
            id = pkg.manifest.item_id,
            point = pkg.manifest.attachment_point,
            mesh = pkg.mesh_asset_path
        );
    }

    // 2. Tampering with the mesh file breaks the signature.
    let pkg = verify_package(ASSETS_DIR, GOLDEN_SWORD)?;
    let mesh_path = Path::new(ASSETS_DIR).join(&pkg.mesh_asset_path);
    let mut tampered_mesh = std::fs::read(&mesh_path)?;
    let last = tampered_mesh.len() - 1;
    tampered_mesh[last] ^= 0xFF;
    match verify(&pkg.manifest, &tampered_mesh) {
        Ok(_) => bail!("✗ tampered mesh still verified — cosmetic would render"),
        Err(e) => println!("✓ tampered mesh refused: {e}"),
    }

    // 3. Tampering with a manifest identity field breaks the signature.
    let mut tampered = pkg.manifest.clone();
    tampered.item_id = "golden_sword_v2".into();
    let mesh = std::fs::read(&mesh_path)?;
    match verify(&tampered, &mesh) {
        Ok(_) => bail!("✗ tampered item_id still verified"),
        Err(e) => println!("✓ tampered item_id refused: {e}"),
    }

    // 4. Tampering with the attachment point breaks the signature.
    let mut tampered = pkg.manifest.clone();
    tampered.attachment_point = "LeftHand".into();
    let mesh = std::fs::read(&mesh_path)?;
    match verify(&tampered, &mesh) {
        Ok(_) => bail!("✗ tampered attachment_point still verified"),
        Err(e) => println!("✓ tampered attachment_point refused: {e}"),
    }

    // 5. An unrelated creator key fails verification.
    let mut forged = pkg.manifest.clone();
    forged.creator_pubkey =
        "ed25519:0000000000000000000000000000000000000000000000000000000000000000".into();
    let mesh = std::fs::read(&mesh_path)?;
    match verify(&forged, &mesh) {
        Ok(_) => bail!("✗ forged creator key verified"),
        Err(e) => println!("✓ forged creator key refused: {e}"),
    }

    // 6. A malformed signature field is rejected.
    let mut bad_sig = pkg.manifest.clone();
    bad_sig.signature = "ed25519_sig:nothex".into();
    let mesh = std::fs::read(&mesh_path)?;
    match verify(&bad_sig, &mesh) {
        Ok(_) => bail!("✗ malformed signature verified"),
        Err(e) => println!("✓ malformed signature refused: {e}"),
    }

    // 7. A missing package is rejected.
    match verify_package(ASSETS_DIR, "cosmetics/nope/cosmetic_manifest.json") {
        Ok(_) => bail!("✗ missing package verified"),
        Err(_) => println!("✓ missing package refused"),
    }

    // 8. Attachment point parse helper must reject unknown nodes.
    let mut unknown = CosmeticManifest {
        item_id: "x".into(),
        mesh: "x.glb".into(),
        attachment_point: "Wrist".into(),
        creator_pubkey: pkg.manifest.creator_pubkey.clone(),
        signature: pkg.manifest.signature.clone(),
    };
    if host::renderer::AttachmentPoint::parse(&unknown.attachment_point).is_some() {
        bail!("✗ unknown attachment point parsed");
    }
    unknown.attachment_point = "RightHand".into();
    match host::renderer::AttachmentPoint::parse(&unknown.attachment_point) {
        Some(host::renderer::AttachmentPoint::RightHand) => {
            println!("✓ attachment point parse OK")
        }
        _ => bail!("✗ known attachment point failed to parse"),
    }

    println!("✓ all cosmetic signature checks passed");
    Ok(())
}