//! The platform CLI: build, test, publish, and run games.
//!
//! ```sh
//! platform build    # compile the guest to wasm, generate manifest, validate
//!                   # avatar conformance, sign the bundle
//! platform test     # run the wasm headlessly with mock peers, verify it
//!                   # doesn't crash
//! platform publish  # upload to IPFS, register the CID in the discovery registry
//! platform run      # launch the host with the local guest for quick iteration
//! ```

use anyhow::{bail, Context, Result};
use ed25519_dalek::SigningKey;
use host::{
    avatar_state::{AvatarPose, AvatarState},
    avatar_standard,
    host_functions,
    host_state::HostState,
    input_state::InputState,
    manifest::{GameManifest, HASH_PREFIX},
};
use registry_server::{register_game, GameListing};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Default platform signing key (RFC 8032 test vector #1). Override with
/// `--signer <hex>`.
const DEV_SECRET_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";
const REGISTRY_URL: &str = "http://127.0.0.1:9002";
const DEFAULT_PACKAGE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../guest");
const HOST_ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../host/assets");
const DEFAULT_AVATAR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../host/assets/avatar_standard.glb");

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("build") => cmd_build(&args[2..]),
        Some("test") => cmd_test(&args[2..]),
        Some("publish") => cmd_publish(&args[2..]),
        Some("run") => cmd_run(&args[2..]),
        Some("help") | Some("-h") | Some("--help") | None => {
            print_usage();
            Ok(())
        }
        Some(other) => bail!("unknown subcommand '{other}' (try `platform help`)"),
    }
}

fn print_usage() {
    println!(
        "platform <subcommand>\n\
         \n\
         \x20 build      compile the guest to wasm, generate manifest, validate avatar, sign\n\
         \x20 test       run the wasm headlessly with mock peers (no crash)\n\
         \x20 publish    upload to IPFS and register the CID in the registry\n\
         \x20 run        launch the host with the local guest\n\
         \n\
         common flags: --package <dir> (game crate dir, default platform/guest)"
    );
}

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// The crate name a package builds to: its directory name with `-` -> `_`
/// (matches `cargo new` conventions and the guest crate layout).
fn package_crate_name(package_dir: &Path) -> Result<String> {
    let name = package_dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("package dir has no name")?;
    if !package_dir.join("Cargo.toml").exists() {
        bail!("no Cargo.toml in {}", package_dir.display());
    }
    Ok(name.replace('-', "_"))
}

fn signer_key(args: &[String]) -> Result<SigningKey> {
    let hex = arg(args, "--signer").unwrap_or_else(|| DEV_SECRET_HEX.to_string());
    let bytes: [u8; 32] = hex::decode(&hex)
        .context("--signer must be a 32-byte hex secret key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("--signer must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn wasm_path_for(package_dir: &Path, crate_name: &str) -> PathBuf {
    let file = format!("{crate_name}.wasm");
    let candidates = [
        package_dir.join("target/wasm32-unknown-unknown/release").join(&file),
        package_dir.join("../../target/wasm32-unknown-unknown/release").join(&file),
    ];
    candidates
        .iter()
        .find(|p| p.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// `platform build`: compile -> manifest -> avatar validation -> sign.
fn cmd_build(args: &[String]) -> Result<()> {
    let package_dir = PathBuf::from(arg(args, "--package").unwrap_or_else(|| DEFAULT_PACKAGE.to_string()));
    let avatar_arg = arg(args, "--avatar")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AVATAR));

    // 1. Compile the guest to wasm32.
    println!("compiling guest in {}...", package_dir.display());
    let status = std::process::Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-unknown-unknown"])
        .current_dir(&package_dir)
        .status()
        .context("failed to run cargo build")?;
    if !status.success() {
        bail!("wasm build failed");
    }

    let crate_name = package_crate_name(&package_dir)?;
    let built = wasm_path_for(&package_dir, &crate_name);
    let wasm_bytes = std::fs::read(&built)
        .with_context(|| format!("build did not produce {}", built.display()))?;

    // 2. Update (or create) the game manifest, pinned to this wasm.
    let manifest_path = package_dir.join("game_manifest.json");
    let mut manifest = if manifest_path.exists() {
        GameManifest::from_path(&manifest_path)?
    } else {
        GameManifest {
            name: humanize(&crate_name),
            version: "0.1.0".to_string(),
            author: "dev".to_string(),
            wasm_entry: format!("{crate_name}.wasm"),
            wasm_hash: String::new(),
            mode: host::manifest::GameMode::Session,
            max_players: 8,
            avatar_skeleton: host::manifest::AvatarSkeleton::StandardV1,
            host_functions_required: vec![
                host::manifest::HostFunctionCategory::Input,
                host::manifest::HostFunctionCategory::Network,
                host::manifest::HostFunctionCategory::Render,
            ],
            signer_pubkey: None,
            signature: None,
        }
    };
    manifest.wasm_entry = format!("{crate_name}.wasm");
    manifest.wasm_hash = format!("{HASH_PREFIX}{}", hex::encode(Sha256::digest(&wasm_bytes)));

    // Copy the wasm into the package so it is self-contained.
    std::fs::write(package_dir.join(&manifest.wasm_entry), &wasm_bytes)?;

    // 3. Validate the avatar conforms to the standard.
    let avatar = avatar_standard::validate_avatar_path(&avatar_arg)
        .with_context(|| format!("avatar at {} is non-conforming", avatar_arg.display()))?;
    println!(
        "avatar OK: {} bones, {} triangles, {} textures, {} animations",
        avatar.bone_count, avatar.triangle_count, avatar.texture_count, avatar.animations.len()
    );

    // 4. Sign the bundle as the publisher.
    let key = signer_key(args)?;
    manifest.sign_publisher(&wasm_bytes, &key);
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    println!(
        "✓ built {} ({} bytes), manifest signed (pubkey ed25519:{})",
        manifest.wasm_entry,
        wasm_bytes.len(),
        hex::encode(key.verifying_key().to_bytes())
    );
    println!("  package: {}", package_dir.display());
    Ok(())
}

/// `platform test`: run the wasm headlessly with mock peers, no crash.
fn cmd_test(args: &[String]) -> Result<()> {
    let package_dir = PathBuf::from(arg(args, "--package").unwrap_or_else(|| DEFAULT_PACKAGE.to_string()));
    let frames: u64 = arg(args, "--frames").and_then(|s| s.parse().ok()).unwrap_or(120);

    let manifest = GameManifest::from_path(package_dir.join("game_manifest.json"))?;
    let wasm = std::fs::read(package_dir.join(&manifest.wasm_entry))?;
    manifest.verify_wasm(&manifest.wasm_entry, &wasm)?;
    manifest.verify_publisher(&wasm)?;
    println!("✓ manifest verified for '{}'", manifest.name);

    let avatar_state = Arc::new(Mutex::new(AvatarState::default()));
    let remote_avatars: Arc<Mutex<std::collections::HashMap<String, AvatarPose>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));

    // A mock peer: role B standing a little way off, so games that tag have
    // something to see.
    remote_avatars.lock().unwrap().insert(
        "PeerB".to_string(),
        AvatarPose { x: 1.5, y: 0.0, z: 0.0, rot_y: 0.0, last_seen: Instant::now() },
    );

    let engine = wasmtime::Engine::default();
    let mut linker = wasmtime::Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let module = wasmtime::Module::new(&engine, wasm)?;
    let mut store = wasmtime::Store::new(&engine, HostState::new("headless"));
    store.data_mut().set_avatar_state(Some(avatar_state.clone()));
    store.data_mut().set_remote_avatars(remote_avatars.clone());
    store.data_mut().set_movement_axis(0);
    let instance = linker.instantiate(&mut store, &module)?;
    let tick = instance.get_typed_func::<(), ()>(&mut store, "game_tick")?;

    println!("running {frames} frames headlessly (mock peer: PeerB @ (1.5,0,0))...");
    for frame in 0..frames {
        store.data_mut().update_frame();
        // Mock input: alternate directions, press a primary action occasionally.
        store.data_mut().set_input(InputState {
            move_right: (frame % 2 == 0) as u8,
            move_up: (frame % 3 == 0) as u8,
            action_1: (frame % 60 == 0) as u8,
            ..InputState::default()
        });
        if let Err(e) = tick.call(&mut store, ()) {
            bail!("guest trapped on frame {frame}: {e:#}");
        }
    }

    let pose = *avatar_state.lock().unwrap();
    let moved = pose.x != 0.0 || pose.z != 0.0 || pose.rot_y != 0.0;
    println!(
        "✓ ran {frames} frames without crashing (final pose x={:.2} z={:.2} rot={:.2})",
        pose.x, pose.z, pose.rot_y
    );
    if !moved {
        println!("  note: the guest did not move the avatar — is input wired up?");
    }
    println!("✓ headless test passed");
    Ok(())
}

/// `platform publish`: bundle -> IPFS CID -> register in the registry.
fn cmd_publish(args: &[String]) -> Result<()> {
    let package_dir = PathBuf::from(arg(args, "--package").unwrap_or_else(|| DEFAULT_PACKAGE.to_string()));
    let description = arg(args, "--description").unwrap_or_else(|| "A game".to_string());
    let register = !args.iter().any(|a| a == "--no-register");

    let manifest = GameManifest::from_path(package_dir.join("game_manifest.json"))?;

    let tar_bytes = host::ipfs::bundle(&package_dir, Path::new(HOST_ASSETS))?;
    let cid = host::ipfs::add_bytes(&tar_bytes)?;
    println!("✓ published to IPFS: CID = {cid}");

    if register {
        let listing = GameListing {
            name: manifest.name.clone(),
            cid: cid.clone(),
            description,
            author: manifest.author.clone(),
            mode: format!("{:?}", manifest.mode).to_lowercase(),
        };
        match register_game(REGISTRY_URL, &listing) {
            Ok(()) => println!("✓ registered '{}' in the registry at {REGISTRY_URL}", listing.name),
            Err(e) => println!("⚠ registry registration skipped ({e:#}) — start it with: cargo run -p registry_server"),
        }
    }

    println!("play it with: cargo run -p host --bin play_game -- --role A --cid {cid}");
    Ok(())
}

/// `platform run`: launch the host with the local guest, forwarding args.
fn cmd_run(args: &[String]) -> Result<()> {
    println!("launching host (play_game) with the local guest...");
    let status = std::process::Command::new("cargo")
        .args(["run", "-q", "-p", "host", "--bin", "play_game", "--"])
        .args(args)
        .status()
        .context("failed to launch play_game")?;
    if !status.success() {
        bail!("play_game exited with status {status}");
    }
    Ok(())
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
