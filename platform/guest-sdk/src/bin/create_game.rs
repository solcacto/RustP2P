//! Scaffolds a new game from a guest-sdk template — the `cargo new --template`
//! equivalent for this platform.
//!
//! Usage:
//! ```sh
//! cargo run -p guest-sdk --bin create_game -- \
//!   --template session-shooter --name my_shooter [--description "..." ] [--author peer]
//! ```
//!
//! Creates `platform/games/<name>/`, substitutes the template placeholders,
//! compiles the crate to `wasm32-unknown-unknown`, and writes a
//! `game_manifest.json` pinned to the built wasm's SHA-256. The resulting
//! package can be loaded with `play_game --package platform/games/<name>`.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

const TEMPLATES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/templates");
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../games");
const MANIFEST_TEMPLATE: &str = r#"{
  "name": "{{name}}",
  "version": "0.1.0",
  "author": "{{author}}",
  "wasm_entry": "{{wasm_entry}}",
  "wasm_hash": "sha256:{{wasm_hash}}",
  "mode": "session",
  "max_players": 8,
  "avatar_skeleton": "standard_v1",
  "host_functions_required": ["input", "network", "render"]
}
"#;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let template = arg(&args, "--template").unwrap_or_else(|| "session-shooter".to_string());
    let name = arg(&args, "--name").context("--name <crate-name> is required")?;
    let description = arg(&args, "--description").unwrap_or_else(|| "A game".to_string());
    let author = arg(&args, "--author").unwrap_or_else(|| "dev".to_string());

    let crate_name = name.replace('-', "_");
    let crate_struct = pascal(&crate_name);
    let display_name = humanize(&crate_name);

    // Validate the template exists.
    let template_dir = PathBuf::from(TEMPLATES_DIR).join(&template);
    if !template_dir.join("Cargo.toml").exists() {
        bail!(
            "unknown template '{}' (available: session-shooter, open-world)",
            template
        );
    }

    // Scaffold into <repo>/platform/games/<name>.
    let game_dir = PathBuf::from(GAMES_DIR).join(&name);
    if game_dir.exists() {
        bail!("{} already exists", game_dir.display());
    }
    std::fs::create_dir_all(game_dir.join("src"))?;
    copy_with(&template_dir.join("Cargo.toml"), &game_dir.join("Cargo.toml"), &[
        ("{{crate_name}}", &crate_name),
    ])?;
    copy_with(&template_dir.join("src/lib.rs"), &game_dir.join("src/lib.rs"), &[
        ("{{crate_name}}", &crate_name),
        ("{{crate_struct}}", &crate_struct),
        ("{{name}}", &display_name),
    ])?;
    println!("✓ scaffolded game at {}", game_dir.display());

    // Build to wasm32.
    println!("building {} for wasm32-unknown-unknown...", crate_name);
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&game_dir)
        .status()
        .context("failed to run cargo build")?;
    if !status.success() {
        bail!(
            "wasm build failed — fix the errors above, then run:\n  \
             cargo build --release --target wasm32-unknown-unknown (in {})",
            game_dir.display()
        );
    }

    // Write the manifest pinned to the built wasm, and copy the wasm into the
    // package root so the package dir is self-contained.
    let wasm_entry = format!("{crate_name}.wasm");
    let built_wasm = game_dir
        .join("target/wasm32-unknown-unknown/release")
        .join(&wasm_entry);
    let wasm_bytes = std::fs::read(&built_wasm)
        .with_context(|| format!("build did not produce {}", built_wasm.display()))?;
    std::fs::write(game_dir.join(&wasm_entry), &wasm_bytes)?;
    let wasm_hash = hex::encode(Sha256::digest(&wasm_bytes));

    let manifest = MANIFEST_TEMPLATE
        .replace("{{name}}", &display_name)
        .replace("{{author}}", &author)
        .replace("{{wasm_entry}}", &wasm_entry)
        .replace("{{wasm_hash}}", &wasm_hash);
    std::fs::write(game_dir.join("game_manifest.json"), manifest)?;

    println!("✓ built {wasm_entry} ({} bytes) and pinned game_manifest.json", wasm_bytes.len());
    println!(
        "run it with: cargo run -p host --bin play_game -- --role A --package {}",
        game_dir.display()
    );
    let _ = description;
    Ok(())
}

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn pascal(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn humanize(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn copy_with(src: &PathBuf, dst: &PathBuf, subs: &[(&str, &String)]) -> Result<()> {
    let mut text = std::fs::read_to_string(src)
        .with_context(|| format!("cannot read template {}", src.display()))?;
    for (from, to) in subs {
        text = text.replace(from, to);
    }
    std::fs::write(dst, text)?;
    Ok(())
}
