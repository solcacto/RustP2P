//! IPFS integration for decentralized game distribution.
//!
//! A game is published as a single `.tar` bundle (wasm + manifest + assets)
//! that is added to a local Kubo/IPFS node. The returned **CID** is the game's
//! permanent address: any player can fetch the exact same bundle bytes from
//! the swarm by that CID, so games never depend on a central server.
//!
//! Once a node downloads (and pins) a bundle it becomes a **seeder**: other
//! players requesting the same CID are served by the swarm — the original
//! publisher can even go offline (Commit 19).
//!
//! Talks to a Kubo daemon over its HTTP API using `ureq` — no heavy client
//! crate required. The endpoint is configurable so tests can simulate multiple
//! machines (one node per API port).

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Read;
use std::path::Path;

/// Default Kubo HTTP API base URL.
pub const IPFS_API: &str = "http://127.0.0.1:5001/api/v0";

/// Client for a specific Kubo node's HTTP API.
#[derive(Debug, Clone)]
pub struct IpfsClient {
    api: String,
}

impl IpfsClient {
    /// Creates a client for the node at `api` (e.g.
    /// `http://127.0.0.1:5001/api/v0`).
    pub fn new(api: impl Into<String>) -> Self {
        let mut api = api.into();
        if !api.ends_with("/api/v0") {
            api = format!("{}/api/v0", api.trim_end_matches('/'));
        }
        Self { api }
    }

    /// Adds raw bytes to IPFS and returns the resulting CID (e.g. `Qm...`).
    /// Content added this way is pinned by the node (the publisher seeds it).
    pub fn add_bytes(&self, bytes: &[u8]) -> Result<String> {
        let boundary = "----rustp2p-bundle-boundary";
        let mut body = Vec::with_capacity(bytes.len() + 256);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"bundle.tar\"\r\nContent-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());

        let response = ureq::post(&format!("{}/add", self.api))
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send_bytes(&body)
            .context("IPFS add failed (is a Kubo node running?)")?;
        let json: Value = serde_json::from_str(&response.into_string()?)
            .context("unexpected IPFS add response")?;
        let cid = json["Hash"]
            .as_str()
            .context("IPFS add response missing Hash")?
            .to_string();
        if cid.is_empty() {
            bail!("IPFS add returned an empty CID");
        }
        Ok(cid)
    }

    /// Fetches the raw bytes stored under `cid` from IPFS.
    pub fn cat(&self, cid: &str) -> Result<Vec<u8>> {
        let response = ureq::post(&format!("{}/cat?arg={cid}", self.api))
            .send_bytes(&[])
            .with_context(|| format!("IPFS cat '{cid}' failed"))?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(256 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .with_context(|| format!("failed reading IPFS object '{cid}'"))?;
        Ok(bytes)
    }

    /// Pins `cid` locally so it survives GC and this node reliably seeds it.
    pub fn pin(&self, cid: &str) -> Result<()> {
        ureq::post(&format!("{}/pin/add?arg={cid}", self.api))
            .send_bytes(&[])
            .with_context(|| format!("IPFS pin '{cid}' failed"))?;
        Ok(())
    }
}

impl Default for IpfsClient {
    fn default() -> Self {
        Self::new(IPFS_API)
    }
}

/// Adds raw bytes to IPFS on the default node and returns the CID.
pub fn add_bytes(bytes: &[u8]) -> Result<String> {
    IpfsClient::default().add_bytes(bytes)
}

/// Fetches the raw bytes under `cid` from the default node.
pub fn cat(cid: &str) -> Result<Vec<u8>> {
    IpfsClient::default().cat(cid)
}

/// Builds a `.tar` bundle of a game package + its assets in memory.
///
/// `package_dir` holds `game_manifest.json` and the wasm entry it references;
/// `assets_dir` is walked recursively and stored under `assets/<relpath>`.
/// Returns the tar bytes.
pub fn bundle(package_dir: &Path, assets_dir: &Path) -> Result<Vec<u8>> {
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);

        let entry = "game_manifest.json";
        let data = std::fs::read(package_dir.join(entry))
            .with_context(|| format!("package missing {entry}"))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, entry, &data[..])
            .with_context(|| format!("tar: append {entry}"))?;

        let manifest: Value = serde_json::from_slice(
            &std::fs::read(package_dir.join("game_manifest.json"))
                .context("cannot read game_manifest.json")?,
        )
        .context("invalid game_manifest.json")?;
        let wasm_entry = manifest["wasm_entry"]
            .as_str()
            .context("manifest has no wasm_entry")?;
        let wasm_data = std::fs::read(package_dir.join(wasm_entry))
            .with_context(|| format!("package missing wasm entry '{wasm_entry}'"))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(wasm_data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, wasm_entry, &wasm_data[..])
            .with_context(|| format!("tar: append {wasm_entry}"))?;

        add_dir_to_tar(&mut builder, assets_dir, "assets")?;
        builder.finish().context("tar finish failed")?;
    }
    Ok(tar_bytes)
}

/// Recursively adds the files under `dir` into the tar under `prefix`.
fn add_dir_to_tar(builder: &mut tar::Builder<&mut Vec<u8>>, dir: &Path, prefix: &str) -> Result<()> {
    let mut stack: Vec<(String, std::path::PathBuf)> = vec![(prefix.to_string(), dir.to_path_buf())];
    while let Some((tar_prefix, fs_path)) = stack.pop() {
        for entry in std::fs::read_dir(&fs_path).with_context(|| format!("cannot read {}", fs_path.display()))? {
            let entry = entry?;
            let path = entry.path();
            let rel = format!("{tar_prefix}/{}", entry.file_name().to_string_lossy());
            if path.is_dir() {
                stack.push((rel, path));
            } else {
                let data = std::fs::read(&path)?;
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder
                    .append_data(&mut header, &rel, &data[..])
                    .with_context(|| format!("tar: append {rel}"))?;
            }
        }
    }
    Ok(())
}

/// Extracts a `.tar` bundle into `dest`. Gzipped bundles (`.tar.gz`, as
/// uploaded by `scripts/publish.sh`) are transparently decompressed first;
/// plain `.tar` bundles keep working unchanged.
pub fn extract(tar_bytes: &[u8], dest: &Path) -> Result<()> {
    use std::io::Read;

    std::fs::create_dir_all(dest)?;
    let reader: Box<dyn Read> = if tar_bytes.starts_with(&[0x1f, 0x8b]) {
        // gzip magic bytes: wrap the stream in a GzDecoder.
        Box::new(flate2::read::GzDecoder::new(tar_bytes))
    } else {
        Box::new(tar_bytes)
    };
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(false);
    archive
        .unpack(dest)
        .with_context(|| format!("failed extracting bundle to {}", dest.display()))?;
    Ok(())
}
