//! RustP2P Launcher — a small desktop front-end for the game registry.
//!
//! On start it fetches `games.json` from the GitHub Pages registry, shows the
//! games as a grid of cards (placeholder thumbnails), and clicking one launches
//! the bundled `play_game` binary with that game's CID. Solo games launch in
//! `--solo` mode; session games ask whether you are the host (A) or joining
//! (B) and launch accordingly.
//!
//! The launcher and `play_game` live side by side (both inside a macOS `.app`
//! or in the same folder on Windows), so the bundled game assets, `config.toml`,
//! and the downloaded-game cache all resolve next to the executable.

mod registry;

use anyhow::Result;
use eframe::egui;
use egui::{Color32, RichText, Vec2};
use registry::{fetch_registry, GameListing};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Public game registry on GitHub Pages (matches `host/config.toml`).
const REGISTRY_URL: &str = "https://solcacto.github.io/my-platform-registry/games.json";

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1024.0, 700.0])
            .with_min_inner_size([640.0, 480.0])
            .with_title("RustP2P Launcher"),
        ..Default::default()
    };
    eframe::run_native(
        "RustP2P Launcher",
        options,
        Box::new(|_cc| Ok(Box::new(LauncherApp::default()))),
    )
}

/// The game grid + launch flow.
struct LauncherApp {
    /// Latest registry fetch result (None = in flight / not started).
    games: Arc<Mutex<Option<Result<Vec<GameListing>>>>>,
    /// Background-refresh state.
    refreshing: bool,
    /// While a game runs, its stdout is drained here for the status panel.
    child: Option<Child>,
    child_log: Arc<Mutex<VecDeque<String>>>,
    child_log_ui: VecDeque<String>,
    /// Dialog state for session games: which game is pending a role choice.
    pending_role: Option<GameListing>,
    /// Last action/error message shown in the status bar.
    status: String,
    last_refresh: Option<Instant>,
}

impl Default for LauncherApp {
    fn default() -> Self {
        Self {
            games: Arc::new(Mutex::new(None)),
            refreshing: false,
            child: None,
            child_log: Arc::new(Mutex::new(VecDeque::new())),
            child_log_ui: VecDeque::new(),
            pending_role: None,
            status: String::new(),
            last_refresh: None,
        }
    }
}

impl LauncherApp {
    fn refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        let games = self.games.clone();
        std::thread::spawn(move || {
            let result = fetch_registry(REGISTRY_URL);
            *games.lock().unwrap() = Some(result);
        });
        self.status = "Refreshing game registry…".to_string();
    }

    /// Looks for a `play_game` binary: next to this executable first (the
    /// shipped layout), then in the workspace `target` directories.
    fn find_play_game(&self) -> Option<PathBuf> {
        let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
        for dir in [
            exe_dir.clone(),
            exe_dir.join(".."),
            exe_dir.join("../../target/release"),
            exe_dir.join("../../target/debug"),
            exe_dir.join("../target/release"),
            exe_dir.join("../target/debug"),
        ] {
            let candidate = dir.join("play_game");
            if candidate.exists() {
                return Some(candidate);
            }
            #[cfg(windows)]
            {
                let candidate_exe = dir.join("play_game.exe");
                if candidate_exe.exists() {
                    return Some(candidate_exe);
                }
            }
        }
        None
    }

    fn launch(&mut self, game: &GameListing, role: Option<&str>) {
        if self.child.is_some() {
            self.status = "A game is already running — close it first.".to_string();
            return;
        }
        let Some(play_game) = self.find_play_game() else {
            self.status = "play_game not found next to the launcher. Build it with \
                           `cargo build -p host --bin play_game` and place it beside this app."
                .to_string();
            return;
        };

        let mut cmd = Command::new(&play_game);
        cmd.arg("--cid")
            .arg(&game.cid)
            .arg("--role")
            .arg(role.unwrap_or("A"))
            .arg("--no-exit");
        if game.mode == "solo" {
            cmd.arg("--solo");
        }
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .stdin(Stdio::null());

        let log = self.child_log.clone();
        log.lock().unwrap().clear();
        match cmd.spawn() {
            Ok(mut child) => {
                // Drain stdout on a background thread into the shared log.
                if let Some(stdout) = child.stdout.take() {
                    std::thread::spawn(move || {
                        use std::io::BufRead;
                        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                            let mut guard = log.lock().unwrap();
                            guard.push_back(line);
                            while guard.len() > 200 {
                                guard.pop_front();
                            }
                        }
                    });
                }
                self.child = Some(child);
                self.status = format!(
                    "Launching {} (mode: {}, role: {})…",
                    game.name,
                    game.mode,
                    role.unwrap_or("A")
                );
            }
            Err(e) => {
                self.status = format!("Failed to launch play_game: {e}");
            }
        }
    }
}

impl eframe::App for LauncherApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Auto-refresh on first frame.
        if self.last_refresh.is_none() {
            self.refresh();
            self.last_refresh = Some(Instant::now());
        }

        // Drain the child log into the UI buffer and reap a finished child.
        {
            let mut ui_log = std::mem::take(&mut self.child_log_ui);
            let shared = self.child_log.lock().unwrap();
            for line in shared.iter() {
                ui_log.push_back(line.clone());
            }
            self.child_log_ui = ui_log;
        }
        if let Some(child) = &mut self.child {
            if let Ok(Some(status)) = child.try_wait() {
                self.child = None;
                self.status = format!("Game exited: {status}");
            }
        }

        // Top bar.
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustP2P Launcher");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Refresh").clicked() {
                        self.refresh();
                    }
                });
            });
        });

        // Status bar.
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            if !self.status.is_empty() {
                ui.label(RichText::new(&self.status).color(Color32::LIGHT_GRAY));
            }
        });

        // Game log panel (visible while a game is running).
        if self.child.is_some() {
            egui::TopBottomPanel::bottom("log")
                .resizable(true)
                .default_height(180.0)
                .show(ctx, |ui| {
                    ui.label(RichText::new("Game output").strong());
                    ui.separator();
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(
                        ui,
                        |ui| {
                            for line in &self.child_log_ui {
                                ui.monospace(line);
                            }
                        },
                    );
                });
        }

        // The game grid. The grid renderer only reads state and returns what
        // was clicked; actual state changes happen after the lock is released.
        enum PanelAction {
            None,
            Refresh,
            Render(Vec<GameListing>),
        }
        let action = egui::CentralPanel::default().show(ctx, |ui| {
            let games = self.games.lock().unwrap();
            match &*games {
                None => {
                    ui.centered_and_justified(|ui| {
                        ui.spinner();
                        ui.label("Fetching the game registry…");
                    });
                    PanelAction::None
                }
                Some(Err(e)) => {
                    ui.colored_label(
                        Color32::LIGHT_RED,
                        format!("Could not load the game registry: {e:#}"),
                    );
                    if ui.button("Retry").clicked() {
                        PanelAction::Refresh
                    } else {
                        PanelAction::None
                    }
                }
                Some(Ok(list)) if list.is_empty() => {
                    ui.label("No games published yet.");
                    PanelAction::None
                }
                Some(Ok(list)) => PanelAction::Render(list.clone()),
            }
        });
        match action.inner {
            PanelAction::Refresh => self.refresh(),
            PanelAction::Render(list) => {
                let clicked = self.render_game_grid(ctx, list);
                if let Some((game, role)) = clicked {
                    if let Some(role) = role {
                        self.launch(&game, Some(&role));
                    } else {
                        self.pending_role = Some(game);
                    }
                }
            }
            PanelAction::None => {}
        }

        // Role dialog for session games.
        if let Some(game) = self.pending_role.clone() {
            let mut close = false;
            egui::Window::new("How will you play?").show(ctx, |ui| {
                ui.label(format!(
                    "{} is a multiplayer game. Host the session (role A) or join another player's session (role B)?",
                    game.name
                ));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Host session").clicked() {
                        self.launch(&game, Some("A"));
                        close = true;
                    }
                    if ui.button("Join session").clicked() {
                        self.launch(&game, Some("B"));
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.pending_role = None;
            }
        }

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl LauncherApp {
    /// Renders the game grid in a full-height scroll area and returns the card
    /// that was clicked, if any. It only reads `self` (the click is acted upon
    /// by the caller after the registry lock is released).
    fn render_game_grid(
        &self,
        ctx: &egui::Context,
        list: Vec<GameListing>,
    ) -> Option<(GameListing, Option<String>)> {
        let mut clicked = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            const CARD_W: f32 = 190.0;
            const CARD_H: f32 = 220.0;
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (i, game) in list.iter().enumerate() {
                        if let Some(c) = self.game_card(ui, game, CARD_W, CARD_H) {
                            clicked = Some(c);
                        }
                        let _ = i;
                    }
                });
            });
        });
        clicked
    }

    fn game_card(
        &self,
        ui: &mut egui::Ui,
        game: &GameListing,
        w: f32,
        h: f32,
    ) -> Option<(GameListing, Option<String>)> {
        let mode_badge = if game.mode == "solo" { "Solo" } else { "Multiplayer" };
        let frame = egui::Frame::group(ui.style()).inner_margin(10.0);
        let mut clicked = None;
        frame.show(ui, |ui| {
            ui.set_min_size(Vec2::new(w, h));

            // Placeholder thumbnail: a colored tile with the game initials.
            let initials = game
                .name
                .split_whitespace()
                .filter_map(|w| w.chars().next())
                .take(2)
                .collect::<String>()
                .to_uppercase();
            let hue = (game.name.bytes().fold(0u32, |a, b| a.wrapping_add(b as u32)) % 360) as f32;
            let (r, g, b) = hsv_to_rgb(hue, 0.55, 0.45);
            let (w_avail, _) = (ui.available_width(), ui.available_height());
            let thumb_h = w_avail.min(110.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(w_avail, thumb_h), egui::Sense::hover());
            ui.painter().rect_filled(rect, 8.0, Color32::from_rgb(r, g, b));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initials,
                egui::FontId::proportional(36.0),
                Color32::WHITE,
            );

            ui.add_space(6.0);
            ui.label(RichText::new(&game.name).strong().size(15.0));
            ui.label(RichText::new(&game.description).size(12.0).color(Color32::GRAY));
            ui.label(
                RichText::new(format!("by {} · {}", game.author, mode_badge))
                    .size(11.0)
                    .color(Color32::GRAY),
            );
            ui.add_space(4.0);
            if ui.button("Play").clicked() {
                if game.mode == "solo" {
                    clicked = Some((game.clone(), Some("A".to_string())));
                } else {
                    clicked = Some((game.clone(), None));
                }
            }
        });
        ui.add_space(14.0);
        clicked
    }
}

/// Converts an HSV color to RGB (used for the deterministic placeholder tile).
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = (h % 360.0) / 60.0;
    let c = v * s;
    let x = c * (1.0 - ((h % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}