//! The Web-Based Studio: a browser IDE for editing guest Rust code, compiling
//! it to wasm server-side with the real toolchain, previewing it in a
//! sandboxed iframe (Three.js + a JS host-function shim), and publishing it to
//! IPFS with one click.
//!
//! - `POST /api/compile`  `{ "source": "<lib.rs>", "name": "MyGame" }` →
//!   compiles the crate server-side and returns a session id for the wasm.
//! - `GET  /api/preview-wasm?session=<id>` → the compiled wasm bytes (loaded
//!   by the sandboxed preview iframe).
//! - `POST /api/publish`   `{ "source": "...", "name": "...", "description": "..." }` →
//!   compiles, uploads to IPFS, registers the CID in the registry.
//! - `GET  /api/games` → the registry's game list.
//! - `GET  /` , `/studio.html` → the IDE, `/preview.html` → the preview runtime.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const STATIC_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/static");
const GUEST_SDK_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest-sdk");
const REGISTRY_URL: &str = "http://127.0.0.1:9002";

/// In-memory store of freshly compiled wasm sessions (id -> wasm bytes).
static SESSIONS: LazyLock<Mutex<HashMap<String, Vec<u8>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Serialize)]
struct CompileResponse {
    ok: bool,
    session: String,
    size: usize,
    log: String,
}

#[derive(Serialize)]
struct PublishResponse {
    ok: bool,
    cid: String,
    name: String,
}

#[derive(Deserialize)]
struct SourceRequest {
    source: String,
    name: Option<String>,
    description: Option<String>,
}

#[derive(Serialize)]
struct GameListing {
    name: String,
    cid: String,
    description: String,
    author: String,
    mode: String,
}

fn main() -> Result<()> {
    let bind = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:9003".to_string());
    let server = tiny_http::Server::http(&bind)
        .map_err(|e| anyhow::anyhow!("failed to bind studio server: {e}"))?;
    println!("Studio listening on http://{bind}");

    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();
        let (status, body, content_type) = route(&method, &url, &mut request);
        let response = tiny_http::Response::from_data(body)
            .with_status_code(status)
            .with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
                    .expect("valid header"),
            );
        if let Err(e) = request.respond(response) {
            eprintln!("respond error: {e}");
        }
    }
    Ok(())
}

fn route(
    method: &tiny_http::Method,
    url: &str,
    request: &mut tiny_http::Request,
) -> (u32, Vec<u8>, String) {
    let path = url.split('?').next().unwrap_or(url);
    match (method, path) {
        (&tiny_http::Method::Get, "/") | (&tiny_http::Method::Get, "/studio.html") => {
            static_file("studio.html")
        }
        (&tiny_http::Method::Get, "/preview.html") => static_file("preview.html"),
        (&tiny_http::Method::Get, "/api/games") => json_ok(api_games()),
        (&tiny_http::Method::Get, "/api/preview-wasm") => {
            let session = query_param(url, "session").unwrap_or_default();
            match SESSIONS.lock().unwrap().get(&session) {
                Some(wasm) => (200, wasm.clone(), "application/wasm".to_string()),
                None => (404, b"session not found".to_vec(), "text/plain".to_string()),
            }
        }
        (&tiny_http::Method::Post, "/api/compile") => {
            let body = read_body(request);
            match serde_json::from_slice::<SourceRequest>(&body) {
                Ok(req) => json_ok(api_compile(&req)),
                Err(e) => (400, format!("invalid JSON: {e}").into_bytes(), "text/plain".into()),
            }
        }
        (&tiny_http::Method::Post, "/api/publish") => {
            let body = read_body(request);
            match serde_json::from_slice::<SourceRequest>(&body) {
                Ok(req) => json_ok(api_publish(&req)),
                Err(e) => (400, format!("invalid JSON: {e}").into_bytes(), "text/plain".into()),
            }
        }
        _ => (404, b"not found".to_vec(), "text/plain".to_string()),
    }
}

fn read_body(request: &mut tiny_http::Request) -> Vec<u8> {
    use std::io::Read;
    let mut body = Vec::new();
    let _ = request.as_reader().take(4 * 1024 * 1024).read_to_end(&mut body);
    body
}

fn json_ok(value: serde_json::Value) -> (u32, Vec<u8>, String) {
    (200, serde_json::to_vec(&value).unwrap_or_default(), "application/json".to_string())
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url.split('?')
        .nth(1)?
        .split('&')
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.to_string())
}

fn static_file(name: &str) -> (u32, Vec<u8>, String) {
    let path = PathBuf::from(STATIC_DIR).join(name);
    match std::fs::read(&path) {
        Ok(bytes) => {
            let content_type = if name.ends_with(".html") {
                "text/html"
            } else if name.ends_with(".js") {
                "application/javascript"
            } else {
                "application/octet-stream"
            };
            (200, bytes, content_type.to_string())
        }
        Err(_) => (404, format!("missing static file {name}").into_bytes(), "text/plain".into()),
    }
}

/// Compiles the posted guest source to wasm server-side with the real
/// toolchain (a standalone crate depending on the guest SDK).
fn compile_source(source: &str) -> Result<Vec<u8>> {
    let temp = std::env::temp_dir().join(format!(
        "studio_game_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(temp.join("src"))?;
    let guest_sdk = Path::new(GUEST_SDK_DIR)
        .canonicalize()
        .context("cannot resolve guest-sdk path")?;
    std::fs::write(
        temp.join("Cargo.toml"),
        format!(
            "[package]\nname = \"studio_game\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
             [lib]\ncrate-type = [\"cdylib\"]\n\n\
             [dependencies]\nguest-sdk = {{ path = {:?} }}\n\n\
             [workspace]\n\n\
             [profile.release]\nopt-level = \"s\"\nlto = true\npanic = \"abort\"\n",
            guest_sdk
        ),
    )?;
    std::fs::write(temp.join("src/lib.rs"), source)?;

    let output = std::process::Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&temp)
        .output()
        .context("failed to run cargo")?;
    if !output.status.success() {
        let log = String::from_utf8_lossy(&output.stderr).into_owned();
        let _ = std::fs::remove_dir_all(&temp);
        bail!("compile failed:\n{log}");
    }

    let wasm = std::fs::read(
        temp.join("target/wasm32-unknown-unknown/release/studio_game.wasm"),
    )
    .context("build produced no wasm")?;
    let _ = std::fs::remove_dir_all(&temp);
    Ok(wasm)
}

fn api_compile(req: &SourceRequest) -> serde_json::Value {
    match compile_source(&req.source) {
        Ok(wasm) => {
            let session = hex::encode(Sha256::digest(&wasm))[..16].to_string();
            SESSIONS.lock().unwrap().insert(session.clone(), wasm.clone());
            serde_json::to_value(CompileResponse {
                ok: true,
                session,
                size: wasm.len(),
                log: String::new(),
            })
            .unwrap()
        }
        Err(e) => serde_json::to_value(CompileResponse {
            ok: false,
            session: String::new(),
            size: 0,
            log: format!("{e:#}"),
        })
        .unwrap(),
    }
}

/// One-click publish: compile, bundle (manifest + wasm), add to IPFS, and
/// register the CID in the discovery registry.
fn api_publish(req: &SourceRequest) -> serde_json::Value {
    let result = (|| -> Result<String> {
        let wasm = compile_source(&req.source)?;
        let name = req.name.clone().unwrap_or_else(|| "Studio Game".to_string());
        let description = req.description.clone().unwrap_or_else(|| "Made in the Studio".to_string());

        // Build a self-contained package (manifest + wasm) in a temp dir.
        let temp = std::env::temp_dir().join(format!(
            "studio_pkg_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(temp.join("assets"))?;
        let wasm_entry = "studio_game.wasm".to_string();
        std::fs::write(temp.join(&wasm_entry), &wasm)?;
        let wasm_hash = format!("sha256:{}", hex::encode(Sha256::digest(&wasm)));
        let manifest = serde_json::json!({
            "name": name,
            "version": "0.1.0",
            "author": "studio",
            "wasm_entry": wasm_entry,
            "wasm_hash": wasm_hash,
            "mode": "session",
            "max_players": 8,
            "avatar_skeleton": "standard_v1",
            "host_functions_required": ["input", "network", "render"],
        });
        std::fs::write(temp.join("game_manifest.json"), serde_json::to_string_pretty(&manifest)?)?;

        let tar_bytes = host::ipfs::bundle(&temp, &temp.join("assets"))?;
        let cid = host::ipfs::add_bytes(&tar_bytes)?;

        let _ = registry_server::register_game(
            REGISTRY_URL,
            &registry_server::GameListing {
                name: manifest["name"].as_str().unwrap_or("Studio Game").to_string(),
                cid: cid.clone(),
                description,
                author: "studio".to_string(),
                mode: "session".to_string(),
            },
        );
        let _ = std::fs::remove_dir_all(&temp);
        Ok(cid)
    })();

    match result {
        Ok(cid) => {
            let name = req.name.clone().unwrap_or_else(|| "Studio Game".to_string());
            serde_json::to_value(PublishResponse { ok: true, cid, name }).unwrap()
        }
        Err(e) => {
            serde_json::to_value(PublishResponse {
                ok: false,
                cid: format!("{e:#}"),
                name: String::new(),
            })
            .unwrap()
        }
    }
}

fn api_games() -> serde_json::Value {
    let games: Vec<GameListing> = registry_server::fetch_games(REGISTRY_URL)
        .map(|gs| {
            gs.into_iter()
                .map(|g| GameListing {
                    name: g.name,
                    cid: g.cid,
                    description: g.description,
                    author: g.author,
                    mode: g.mode,
                })
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!(games)
}
