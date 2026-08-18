//! Signed cosmetic packages: avatars can be dressed only with items whose
//! `cosmetic_manifest.json` carries a valid ed25519 signature from the item's
//! creator.
//!
//! The signature binds the manifest identity fields **and** the mesh content:
//! the signed message is `item_id | attachment_point | sha256(mesh)`, so
//! tampering with either the manifest or the mesh file invalidates the item
//! and the host silently refuses to render it. This is the avatar
//! customization analogue of the game-package integrity story (manifest.rs).
//!
//! See `docs/AVATAR_STANDARD.md` for the cosmetic package layout.

use anyhow::{Context, Result};
use ed25519_dalek::{Signature, Signer, Verifier, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Prefix for the creator's public key (`ed25519:` + 64 hex chars).
pub const PUBKEY_PREFIX: &str = "ed25519:";
/// Prefix for the signature (`ed25519_sig:` + 128 hex chars).
pub const SIGNATURE_PREFIX: &str = "ed25519_sig:";

/// The `cosmetic_manifest.json` every cosmetic package must ship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosmeticManifest {
    /// Stable item id, e.g. `"golden_sword_v1"`.
    pub item_id: String,
    /// Mesh filename within the cosmetic package, e.g. `"sword.glb"`.
    pub mesh: String,
    /// Attachment point node name, e.g. `"RightHand"`.
    pub attachment_point: String,
    /// Creator public key: `ed25519:<64 hex>`.
    pub creator_pubkey: String,
    /// Signature over `item_id|attachment_point|sha256(mesh)`:
    /// `ed25519_sig:<128 hex>`.
    pub signature: String,
}

/// A cosmetic package verified at load time, ready to be rendered.
#[derive(Debug, Clone)]
pub struct CosmeticPackage {
    /// The verified manifest.
    pub manifest: CosmeticManifest,
    /// The mesh path relative to the host asset folder (e.g.
    /// `cosmetics/golden_sword/sword.glb`).
    pub mesh_asset_path: String,
}

/// The canonical signed message for a cosmetic.
pub fn signed_message(item_id: &str, attachment_point: &str, mesh_sha256_hex: &str) -> String {
    format!("{item_id}|{attachment_point}|sha256:{mesh_sha256_hex}")
}

/// Signs a cosmetic message with `signing_key`, returning
/// `ed25519_sig:<hex>`.
pub fn sign_message(message: &str, signing_key: &SigningKey) -> String {
    let sig: Signature = signing_key.sign(message.as_bytes());
    format!("{SIGNATURE_PREFIX}{}", hex::encode(sig.to_bytes()))
}

/// Verifies a cosmetic's signature against its identity + mesh content.
///
/// Reconstructs the signed message from `item_id`, `attachment_point`, and the
/// SHA-256 of the actual mesh bytes, then verifies the ed25519 signature with
/// the creator's public key. Any mismatch (bad signature, tampered manifest
/// field, or tampered mesh file) fails.
pub fn verify(
    manifest: &CosmeticManifest,
    mesh_bytes: &[u8],
) -> Result<()> {
    let mesh_hash = hex::encode(Sha256::digest(mesh_bytes));
    let message = signed_message(&manifest.item_id, &manifest.attachment_point, &mesh_hash);

    let pubkey_hex = manifest
        .creator_pubkey
        .strip_prefix(PUBKEY_PREFIX)
        .context("creator_pubkey must start with 'ed25519:'")?;
    let pubkey_bytes: [u8; 32] = hex::decode(pubkey_hex)
        .context("creator_pubkey is not valid hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("creator_pubkey must be 32 bytes"))?;
    let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
        .context("creator_pubkey is not a valid ed25519 public key")?;

    let sig_hex = manifest
        .signature
        .strip_prefix(SIGNATURE_PREFIX)
        .context("signature must start with 'ed25519_sig:'")?;
    let sig_bytes: [u8; 64] = hex::decode(sig_hex)
        .context("signature is not valid hex")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&sig_bytes);

    verifying_key
        .verify(message.as_bytes(), &signature)
        .context("cosmetic signature verification failed (tampered manifest or mesh?)")
}

/// Loads and verifies a cosmetic package from the host asset folder.
///
/// `assets_dir` is the absolute asset folder; `manifest_relative` is the
/// package's manifest path relative to it (e.g.
/// `cosmetics/golden_sword/cosmetic_manifest.json`). The mesh filename is
/// resolved relative to the manifest's directory. Returns the verified package
/// on success, or an error describing why the cosmetic must not be rendered.
pub fn verify_package(
    assets_dir: impl AsRef<Path>,
    manifest_relative: &str,
) -> Result<CosmeticPackage> {
    let assets_dir = assets_dir.as_ref();
    let manifest_path = assets_dir.join(manifest_relative);
    let manifest: CosmeticManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("cannot read cosmetic manifest at {}", manifest_path.display()))?,
    )
    .with_context(|| format!("invalid cosmetic manifest at {}", manifest_path.display()))?;

    let mesh_relative = {
        let dir = manifest_relative
            .rsplit_once('/')
            .map(|(d, _)| d)
            .unwrap_or(".");
        format!("{dir}/{}", manifest.mesh)
    };
    let mesh_bytes = std::fs::read(assets_dir.join(&mesh_relative)).with_context(|| {
        format!("cannot read cosmetic mesh '{mesh_relative}'")
    })?;

    verify(&manifest, &mesh_bytes)?;
    Ok(CosmeticPackage {
        manifest,
        mesh_asset_path: mesh_relative,
    })
}