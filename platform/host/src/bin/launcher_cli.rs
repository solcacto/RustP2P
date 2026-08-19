use anyhow::{bail, Context, Result};
use host::deployment::{download_from_gateway, fetch_registry, DeploymentConfig};
use host::manifest::GameManifest;
use registry_server::GameListing;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Default deployment config (deployment URLs) shipped next to this crate.
const CONFIG_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/config.toml");
/// Directory where downloaded game bundles are cached and seeded.
const GAMES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/games");

/// Usage: cargo run -p host --bin launcher [--config <path>] [--cid <CID>] [--list] [--ipfs-gateway <base>] [--role A|B] [--auto]
///
/// Fetches the game registry from the GitHub Pages `games.json` configured in
/// `config.toml`, lets the user pick a game (or `--cid`), downloads its bundle
/// from a public IPFS gateway, then hands off to the game client.
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CONFIG_PATH));
    let cid = args
        .iter()
        .position(|a| a == "--cid")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let list_only = args.iter().any(|a| a == "--list");
    let gateway_override = args
        .iter()
        .position(|a| a == "--ipfs-gateway")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let registry_override = args
        .iter()
        .position(|a| a == "--registry")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let cfg = DeploymentConfig::load(&config_path)?;
    println!(
        "deployment config from {} (signaling: {}, registry: {})",
        config_path.display(),
        cfg.network.signaling_url,
        cfg.network.registry_url
    );
    let gateway = gateway_override.unwrap_or(cfg.network.ipfs_gateway);
    let registry_url = registry_override.unwrap_or(cfg.network.registry_url);

    // --list: just show the registry and exit (no download, no prompt).
    if list_only {
        let games = fetch_registry(&registry_url).await?;
        if games.is_empty() {
            println!("registry returned no games — publish one with scripts/publish.sh first");
            return Ok(());
        }
        print_list(&games);
        return Ok(());
    }

    // Resolve which game to launch: an explicit --cid, or a choice from the
    // live registry (interactive prompt).
    let selected: GameListing = if let Some(cid) = cid {
        GameListing {
            name: cid.clone(),
            cid,
            description: String::new(),
            author: String::new(),
            mode: String::new(),
        }
    } else {
        println!("\nfetching game list from {} ...", registry_url);
        let games = fetch_registry(&registry_url).await?;
        if games.is_empty() {
            bail!("registry returned no games — publish one with scripts/publish.sh first");
        }
        print_list(&games);
        prompt_select(&games)?
    };

    // Download the bundle from the public IPFS gateway (async, off-thread).
    let game_dir = games_dir(&selected.cid);
    if game_dir.join("game_manifest.json").exists() {
        println!(
            "\n'{}' already downloaded at {}",
            selected.cid,
            game_dir.display()
        );
    } else {
        println!(
            "\ndownloading '{}' (CID {}) from {} ...",
            selected.name,
            selected.cid,
            gateway.trim_end_matches('/')
        );
        let tar_bytes = download_from_gateway(&gateway, &selected.cid)
            .await
            .with_context(|| format!("could not download CID {}", selected.cid))?;
        println!("fetched {} bytes", tar_bytes.len());
        let _ = std::fs::remove_dir_all(&game_dir);
        host::ipfs::extract(&tar_bytes, &game_dir)?;
        println!("extracted bundle to {}", game_dir.display());
    }

    // Sanity-check the bundle is a real game package before launching.
    let manifest_path = game_dir.join("game_manifest.json");
    let manifest = GameManifest::from_path(&manifest_path).with_context(|| {
        format!(
            "downloaded bundle is not a valid game package ({})",
            manifest_path.display()
        )
    })?;
    println!(
        "✓ game package OK: '{}' v{} by {} (mode {:?}, max_players {})",
        manifest.name, manifest.version, manifest.author, manifest.mode, manifest.max_players
    );

    // Hand off to the game client, which performs full hash + publisher
    // verification before instantiating the Wasm guest.
    launch_game(&game_dir, &args)
}

/// Returns the cache directory for a game bundle (`games/<cid>`).
fn games_dir(cid: &str) -> PathBuf {
    PathBuf::from(GAMES_DIR).join(cid)
}

/// Prints the registry as a numbered list the user can pick from.
fn print_list(games: &[GameListing]) {
    println!();
    for (i, g) in games.iter().enumerate() {
        println!(
            "  {:>2}. {:<20} by {:<12} [{}]",
            i + 1,
            g.name,
            g.author,
            g.mode
        );
        println!("       {:<20} {:<30}", g.cid, g.description);
    }
    println!();
}

/// Prompts on stdin for a game: a number, a name, or a CID (blank = first).
fn prompt_select(games: &[GameListing]) -> Result<GameListing> {
    print!("select a game (enter number, name, or CID, blank for the first): ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("failed reading selection from stdin")?;
    let input = line.trim();

    let selected: Option<&GameListing> = if input.is_empty() {
        games.first()
    } else if let Ok(idx) = input.parse::<usize>() {
        games.get(idx.saturating_sub(1))
    } else {
        games
            .iter()
            .find(|g| g.name == input || g.cid == input)
            .or_else(|| {
                games
                    .iter()
                    .find(|g| g.name.to_lowercase().contains(&input.to_lowercase()))
            })
    };
    match selected {
        Some(g) => Ok(g.clone()),
        None => bail!("no game matches '{input}' — re-run with --cid <CID> to download it anyway"),
    }
}

/// Spawns the game client (`play_game`) for the downloaded package, passing
/// through the caller's `--role`, `--auto`, and `--frames` flags.
fn launch_game(game_dir: &Path, args: &[String]) -> Result<()> {
    let mut exe = std::env::current_exe().context("cannot resolve current executable")?;
    exe.set_file_name("play_game");
    if !exe.exists() {
        bail!(
            "game client not found at {} — build it with: cargo build -p host --bin play_game",
            exe.display()
        );
    }

    let mut cmd = std::process::Command::new(&exe);
    let mut role = "A".to_string();
    let mut auto = false;
    let mut frames = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--role" => {
                role = args.get(i + 1).cloned().unwrap_or_else(|| "A".to_string());
                i += 1;
            }
            "--auto" => auto = true,
            "--frames" => {
                frames = args.get(i + 1).cloned();
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    cmd.arg("--role").arg(&role).arg("--package").arg(game_dir);
    if auto {
        cmd.arg("--auto");
    }
    if let Some(frames) = frames {
        cmd.arg("--frames").arg(frames);
    }
    println!(
        "\nlaunching game client: {exe:?} --role {role} --package {}",
        game_dir.display()
    );

    let status = cmd.status().context("failed to spawn the game client")?;
    if !status.success() {
        bail!("game client exited with {status}");
    }
    Ok(())
}
