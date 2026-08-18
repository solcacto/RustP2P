//! Game package manifests: the metadata every game must ship, validated before
//! the host loads any Wasm.
//!
//! A game package is a directory containing a `game_manifest.json` plus the
//! Wasm module it describes. The manifest pins the Wasm's SHA-256 digest, so
//! the host can refuse to load a tampered or mismatched artifact. This is the
//! seed of the platform's distribution and integrity story.
//!
//! See `docs/GAME_MANIFEST.md` for the full format specification.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Hash digest prefix used by [`GameManifest::wasm_hash`].
pub const HASH_PREFIX: &str = "sha256:";
/// Publisher public key prefix (`ed25519:` + 64 hex).
pub const PUBKEY_PREFIX: &str = "ed25519:";
/// Publisher signature prefix (`ed25519_sig:` + 128 hex).
pub const SIGNATURE_PREFIX: &str = "ed25519_sig:";

/// The game's session topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameMode {
    /// A bounded multiplayer session between `max_players` peers.
    Session,
}

/// The avatar skeleton the game expects the host to provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvatarSkeleton {
    /// The built-in Universal Avatar shipped with the platform (`avatar.glb`).
    StandardV1,
}

/// A family of host functions a game may declare it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFunctionCategory {
    /// Input getters (`get_input_*`).
    Input,
    /// Network host functions (`send_network_message`, pose/score sync).
    Network,
    /// Renderer host functions (`update_avatar_transform`, pose queries).
    Render,
    /// Session host functions (`get_movement_axis`, tag score).
    Session,
}

/// Categories this host build can satisfy. Currently every defined category is
/// provided; as the platform evolves this list is where capability negotiation
/// happens.
pub const SUPPORTED_CATEGORIES: &[HostFunctionCategory] = &[
    HostFunctionCategory::Input,
    HostFunctionCategory::Network,
    HostFunctionCategory::Render,
    HostFunctionCategory::Session,
];

/// Upper bound enforced on [`GameManifest::max_players`].
pub const MAX_PLAYERS_CAP: u32 = 64;

/// The `game_manifest.json` a game package must ship.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameManifest {
    /// Human-readable game title, e.g. `"Chase Tag"`.
    pub name: String,
    /// Game version, e.g. `"0.1.0"`.
    pub version: String,
    /// Peer id / identity of the developer who published the game.
    pub author: String,
    /// Filename of the Wasm module within the game package.
    pub wasm_entry: String,
    /// Integrity digest of the Wasm: `sha256:` followed by 64 hex digits.
    pub wasm_hash: String,
    /// Session topology.
    pub mode: GameMode,
    /// Maximum number of players in a session.
    pub max_players: u32,
    /// Avatar skeleton the host must provide.
    pub avatar_skeleton: AvatarSkeleton,
    /// Host-function families the game requires.
    pub host_functions_required: Vec<HostFunctionCategory>,
    /// Optional publisher public key (`ed25519:<64 hex>`), set by `platform build`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_pubkey: Option<String>,
    /// Optional publisher signature over `name|version|wasm_entry|sha256(wasm)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

impl GameManifest {
    /// Reads, parses, and validates the manifest at `path`.
    ///
    /// Any malformed field (bad mode, bad hash format, out-of-range player
    /// count, unknown host-function category, ...) is rejected here, before
    /// any Wasm is loaded.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read manifest at {}", path.display()))?;
        let manifest: GameManifest = serde_json::from_str(&text)
            .with_context(|| format!("invalid manifest at {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Checks the manifest's fields for consistency and security.
    ///
    /// Current rules:
    /// - `name`, `version`, `author` must be non-empty.
    /// - `wasm_entry` must be a bare filename (no path separators, no `..`).
    /// - `wasm_hash` must be `sha256:` followed by exactly 64 hex digits.
    /// - `max_players` must be within `1..=MAX_PLAYERS_CAP`.
    /// - `host_functions_required` must contain supported, non-duplicate
    ///   categories.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            bail!("manifest name must not be empty");
        }
        if self.version.trim().is_empty() {
            bail!("manifest version must not be empty");
        }
        if self.author.trim().is_empty() {
            bail!("manifest author must not be empty");
        }

        if self.wasm_entry.trim().is_empty() {
            bail!("manifest wasm_entry must not be empty");
        }
        if self.wasm_entry.contains('/') || self.wasm_entry.contains('\\') {
            bail!(
                "manifest wasm_entry must be a bare filename, got '{}'",
                self.wasm_entry
            );
        }
        if self.wasm_entry.contains("..") {
            bail!("manifest wasm_entry must not contain '..'");
        }

        let Some(hex_part) = self.wasm_hash.strip_prefix(HASH_PREFIX) else {
            bail!("manifest wasm_hash must start with '{HASH_PREFIX}'");
        };
        if hex_part.len() != 64 || !hex_part.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("manifest wasm_hash must be sha256:<64 hex digits>");
        }

        if !(1..=MAX_PLAYERS_CAP).contains(&self.max_players) {
            bail!("manifest max_players must be within 1..={MAX_PLAYERS_CAP}");
        }

        let mut seen = Vec::with_capacity(self.host_functions_required.len());
        for category in &self.host_functions_required {
            if !SUPPORTED_CATEGORIES.contains(category) {
                bail!("manifest requires unsupported host-function category {category:?}");
            }
            if seen.contains(category) {
                bail!("manifest host_functions_required contains duplicate {category:?}");
            }
            seen.push(*category);
        }

        Ok(())
    }

    /// Verifies that `wasm_bytes` matches this manifest.
    ///
    /// Checks two things:
    /// 1. `entry_name` equals the manifest's `wasm_entry` (so the file being
    ///    loaded is the one the manifest describes).
    /// 2. `sha256(wasm_bytes)` equals the manifest's pinned digest.
    ///
    /// Returns an error (and the game is refused) if either check fails.
    pub fn verify_wasm(&self, entry_name: &str, wasm_bytes: &[u8]) -> Result<()> {
        if entry_name != self.wasm_entry {
            bail!(
                "wasm entry mismatch: manifest declares '{}', loaded '{}'",
                self.wasm_entry,
                entry_name
            );
        }
        let actual = format!(
            "{HASH_PREFIX}{}",
            hex::encode(Sha256::digest(wasm_bytes))
        );
        if actual != self.wasm_hash {
            bail!(
                "wasm hash mismatch: manifest pins '{}', loaded module is '{}' \
                 (refusing to load tampered artifact)",
                self.wasm_hash,
                actual
            );
        }
        Ok(())
    }

    /// Verifies the publisher signature when the manifest carries one.
    ///
    /// A manifest without `signature`/`signer_pubkey` (a legacy, unsigned
    /// package) is accepted. If a signature is present it must verify against
    /// the message `name|version|wasm_entry|sha256(wasm)`.
    pub fn verify_publisher(&self, wasm_bytes: &[u8]) -> Result<()> {
        let (Some(pubkey), Some(signature)) = (&self.signer_pubkey, &self.signature) else {
            return Ok(());
        };
        let message = publisher_message(&self.name, &self.version, &self.wasm_entry, wasm_bytes);

        let pubkey_hex = pubkey
            .strip_prefix(PUBKEY_PREFIX)
            .context("signer_pubkey must start with 'ed25519:'")?;
        let pubkey_bytes: [u8; 32] = hex::decode(pubkey_hex)
            .context("signer_pubkey is not valid hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("signer_pubkey must be 32 bytes"))?;
        let verifying_key = VerifyingKey::from_bytes(&pubkey_bytes)
            .context("signer_pubkey is not a valid ed25519 public key")?;

        let sig_hex = signature
            .strip_prefix(SIGNATURE_PREFIX)
            .context("signature must start with 'ed25519_sig:'")?;
        let sig_bytes: [u8; 64] = hex::decode(sig_hex)
            .context("signature is not valid hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("signature must be 64 bytes"))?;
        let signature = Signature::from_bytes(&sig_bytes);

        verifying_key
            .verify(message.as_bytes(), &signature)
            .context("publisher signature verification failed (tampered manifest or wasm?)")
    }

    /// Signs this manifest as the publisher of `wasm_bytes`, setting
    /// `signer_pubkey` and `signature`.
    pub fn sign_publisher(&mut self, wasm_bytes: &[u8], signing_key: &SigningKey) {
        self.signer_pubkey = Some(format!(
            "{PUBKEY_PREFIX}{}",
            hex::encode(signing_key.verifying_key().to_bytes())
        ));
        let message = publisher_message(&self.name, &self.version, &self.wasm_entry, wasm_bytes);
        let sig: Signature = signing_key.sign(message.as_bytes());
        self.signature = Some(format!("{SIGNATURE_PREFIX}{}", hex::encode(sig.to_bytes())));
    }
}

/// The canonical message a publisher signs: identity fields + wasm digest.
pub fn publisher_message(name: &str, version: &str, wasm_entry: &str, wasm_bytes: &[u8]) -> String {
    format!(
        "{name}|{version}|{wasm_entry}|{HASH_PREFIX}{}",
        hex::encode(Sha256::digest(wasm_bytes))
    )
}