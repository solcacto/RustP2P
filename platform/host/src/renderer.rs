//! Bevy-based 3D renderer and per-frame game loop for the host.
//!
//! [`build_app`] constructs a minimal Bevy app (no `DefaultPlugins`): one
//! camera, one directional light, a ground plane, the local avatar, and remote
//! avatar entities synced over the P2P link. Bevy's winit runner owns the main
//! loop, so the guest tick is registered as the `wasm_render_tick` Update
//! system — each rendered frame runs one guest tick, applies its pose to the
//! local avatar, syncs remote avatars, and refreshes the score HUD. Real
//! keyboard input is read by `read_keyboard` and written into the host's input
//! buffer before the guest tick runs.

use crate::avatar_state::{AvatarPose, AvatarState, WorldObject};
use crate::host_state::{HostState, SignalingStatus};
use crate::input_state::InputState;
use crate::profiling::SharedFrameStats;
use bevy::animation::{AnimationPlugin, RepeatAnimation};
use bevy::app::AppExit;
use bevy::core::{FrameCount, TaskPoolPlugin, TypeRegistrationPlugin};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::core_pipeline::CorePipelinePlugin;
use bevy::diagnostic::{FrameTimeDiagnosticsPlugin, LogDiagnosticsPlugin};
use bevy::gltf::GltfAssetLabel;
use bevy::input::keyboard::{KeyCode, KeyboardInput};
use bevy::input::ButtonState;
use bevy::log::LogPlugin;
use bevy::pbr::{
    DirectionalLight, DirectionalLightBundle, DirectionalLightShadowMap, NotShadowCaster,
    PbrPlugin, StandardMaterial,
};
use bevy::prelude::*;
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::render::RenderPlugin;
use bevy::scene::ScenePlugin;
use bevy::sprite::SpritePlugin;
use bevy::text::{Text, TextPlugin, TextStyle};
use bevy::transform::TransformPlugin;
use bevy::ui::{node_bundles::TextBundle, PositionType, Style, UiPlugin, Val};
use bevy::window::{PresentMode, Window, WindowPlugin};
use bevy::winit::{UpdateMode, WakeUp, WinitPlugin, WinitSettings};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wasmtime::TypedFunc;

/// Frames per second the renderer tries to maintain.
pub const TARGET_FPS: u64 = 60;

/// Frames to render before the integration test self-terminates.
pub const TEST_FRAMES: u32 = 240;

/// Configurable exit-after-N-frames limit for the game binary.
#[derive(Resource)]
pub struct TestFrameLimit(
    /// Number of rendered frames after which the app exits.
    pub u32,
);

/// Shared pose target bridging the Wasm guest and the Bevy scene graph.
#[derive(Resource, Clone)]
pub struct AvatarStateHandle(
    /// The shared local avatar pose written by the guest each frame.
    pub Arc<Mutex<AvatarState>>,
);

/// Latest pose of every remote peer's avatar, shared with the network poll.
#[derive(Resource, Clone)]
pub struct RemoteAvatarsHandle(
    /// Map of peer id to the peer's latest received pose.
    pub Arc<Mutex<HashMap<String, AvatarPose>>>,
);

/// Colored boxes the guest drew this frame, drained by `sync_world_objects`.
#[derive(Resource, Clone)]
pub struct WorldObjectsHandle(pub Arc<Mutex<Vec<WorldObject>>>);

/// Skeletal avatar locomotion state, carried by each avatar's root entity.
///
/// The root entity holds the avatar's world position/facing; a child
/// `SceneBundle` carries the rigged glTF model and its `AnimationPlayer`. The
/// renderer links the player once the scene loads, then drives its clip from
/// the root's world-space velocity (idle stance vs. walk), with no per-game
/// work. The same rig plays on the local avatar and every remote avatar.
#[derive(Component)]
pub struct AvatarAnim {
    /// The avatar model kind selected for this avatar.
    pub kind: AvatarKind,
    /// Entity owning the model's `AnimationPlayer` (filled once the scene
    /// loads; `Entity::PLACEHOLDER` until then).
    pub player: Entity,
    /// Smoothed movement speed in units/second.
    pub speed: f32,
    /// Last frame's translation, used to detect movement.
    pub last_pos: Vec3,
}

/// Which rigged avatar model a game has selected. Games pick one by avatar
/// asset path (e.g. a path containing "fox" selects the animal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvatarKind {
    /// The CC0 humanoid (CesiumMan, CC-BY 4.0).
    Human,
    /// The MIT fox (quadruped). Assets wired in but not yet driven.
    Fox,
}

impl AvatarKind {
    /// Resolves the avatar model from the avatar asset path.
    pub fn for_path(path: &str) -> Self {
        if path.contains("fox") {
            Self::Fox
        } else {
            Self::Human
        }
    }
}

/// Marks the entity that owns a linked `AnimationPlayer` (the player lands on a
/// scene child, not the avatar root entity).
#[derive(Component)]
pub struct AvatarPlayer;

/// Marks the child `SceneBundle` holding an avatar's rigged model.
#[derive(Component)]
pub struct AvatarScene;

/// A rigged avatar model: its glTF scene and a graph over its animation clips,
/// plus the local-space scale/lift that puts the model's feet on the ground
/// (models are authored at very different sizes).
#[derive(Resource, Clone)]
pub struct AvatarModel {
    /// The model's glTF default scene.
    pub scene: Handle<Scene>,
    /// Animation graph over the model's clips.
    pub graph: Handle<AnimationGraph>,
    /// Clip node that plays the model's walk cycle.
    pub walk: AnimationNodeIndex,
    /// Uniform scale bringing the model to the standard ~2-unit avatar height.
    pub scale: f32,
    /// Extra Y offset so the model's feet rest on the ground plane.
    pub lift: f32,
}

/// All rigged avatar models the renderer can spawn, keyed by [`AvatarKind`].
#[derive(Resource, Clone)]
pub struct AvatarModels {
    pub human: AvatarModel,
}

impl AvatarModels {
    /// Resolves the model for a kind. Un-wired kinds (e.g. Fox) fall back to
    /// the humanoid until their assets are integrated.
    pub fn model(&self, kind: AvatarKind) -> &AvatarModel {
        match kind {
            AvatarKind::Human | AvatarKind::Fox => &self.human,
        }
    }
}

/// glTF handles loading the avatar models, keyed by [`AvatarKind`]. The rigged
/// scenes aren't usable until their assets (and dependencies) finish loading,
/// so `build_avatar_models` waits on these and then publishes [`AvatarModels`].
#[derive(Resource)]
pub struct AvatarGltfLoads {
    pub human: Handle<Gltf>,
}

/// Shared unit cube mesh used for every guest-drawn box.
#[derive(Resource, Clone)]
pub struct WorldObjectMeshHandle(pub Handle<Mesh>);

/// Marks a cube entity that mirrors one guest-drawn world object.
#[derive(Component)]
pub struct WorldObjectCube;

/// Marker identifying the loaded Universal Avatar scene root.
#[derive(Component)]
pub struct Avatar;

/// Marker identifying a spawned remote peer's avatar scene root.
#[derive(Component)]
pub struct RemoteAvatar;

/// Peer id a remote avatar entity mirrors.
#[derive(Component)]
pub struct RemotePeerId(
    /// The remote peer's id this entity mirrors.
    pub String,
);

/// When true, `read_keyboard` ignores real keys and feeds scripted input so the
/// whole stack (input -> guest -> pose -> network -> score) can be exercised
/// headlessly (role A drives toward role B).
#[derive(Resource)]
pub struct AutoInput(
    /// Whether scripted (headless) input is active.
    pub bool,
);

/// Marks the on-screen score text entity.
#[derive(Component)]
pub struct ScoreText;

/// Records which avatar asset a spawned avatar scene was loaded from, so the
/// scene can be hot-swapped when the guest changes the avatar.
#[derive(Component, Clone, PartialEq)]
pub struct AvatarSource(pub String);

/// Named attachment points every standard avatar exposes (see
/// `docs/AVATAR_STANDARD.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentPoint {
    Head,
    Chest,
    LeftHand,
    RightHand,
    Back,
    LeftFoot,
    RightFoot,
}

impl AttachmentPoint {
    /// Parses an attachment point by its node name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "Head" => Self::Head,
            "Chest" => Self::Chest,
            "LeftHand" => Self::LeftHand,
            "RightHand" => Self::RightHand,
            "Back" => Self::Back,
            "LeftFoot" => Self::LeftFoot,
            "RightFoot" => Self::RightFoot,
            _ => return None,
        })
    }

    /// The glb node name this attachment point binds to.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Head => "Head",
            Self::Chest => "Chest",
            Self::LeftHand => "LeftHand",
            Self::RightHand => "RightHand",
            Self::Back => "Back",
            Self::LeftFoot => "LeftFoot",
            Self::RightFoot => "RightFoot",
        }
    }
}

/// A verified cosmetic slot: the signed mesh (glb) to parent onto an
/// attachment point. Only signature-verified packages reach the renderer.
#[derive(Debug, Clone)]
pub struct CosmeticSlot {
    /// The cosmetic's stable item id (from its manifest).
    pub item_id: String,
    /// Mesh path relative to the host asset folder (e.g.
    /// `cosmetics/golden_sword/sword.glb`).
    pub mesh_asset_path: String,
}

/// Cosmetic slots the host attaches to avatar attachment points. Cosmetics in
/// this list have already passed signature verification.
#[derive(Resource, Clone, Default)]
pub struct CosmeticSlots(pub Vec<(AttachmentPoint, CosmeticSlot)>);

/// Marker identifying a cosmetic mesh entity parented to an attachment point.
#[derive(Component)]
pub struct Cosmetic;

/// Holds the instantiated Wasm render module so a Bevy `Update` system can
/// drive exactly one guest tick per rendered frame.
#[derive(Resource)]
pub struct WasmRuntime {
    /// Shared handle to the wasmtime store hosting the guest's host state.
    pub store: Arc<Mutex<wasmtime::Store<HostState>>>,
    /// The guest's `game_tick` (or equivalent) export, called once per frame.
    pub render_tick: TypedFunc<(), ()>,
}

/// Builds a Bevy app configured purely as a 3D rendering/scene-graph backend.
///
/// Bevy 0.14's winit runner owns the main loop, so the Wasm game loop is
/// registered as the `wasm_render_tick` Update system: each rendered frame
/// runs one guest tick followed by the avatar pose update. Everything stays
/// on the main thread.
pub fn build_app(
    avatar_state: Arc<Mutex<AvatarState>>,
    remote_avatars: Arc<Mutex<HashMap<String, AvatarPose>>>,
    world_objects: Arc<Mutex<Vec<WorldObject>>>,
) -> App {
    let mut app = App::new();
    app.add_plugins((
        bevy::app::PanicHandlerPlugin,
        LogPlugin::default(),
        TaskPoolPlugin::default(),
        TypeRegistrationPlugin,
        bevy::core::FrameCountPlugin,
        bevy::time::TimePlugin,
        TransformPlugin,
        bevy::hierarchy::HierarchyPlugin,
        bevy::input::InputPlugin,
        AssetPlugin {
            file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").to_string(),
            ..default()
        },
    ));
    app.add_plugins((
        WindowPlugin {
            primary_window: Some(Window {
                // No vsync: measure real render cost and allow >60 FPS.
                present_mode: PresentMode::Immediate,
                ..default()
            }),
            ..default()
        },
        bevy::a11y::AccessibilityPlugin,
        ScenePlugin,
        WinitPlugin::<WakeUp>::default(),
        RenderPlugin::default(),
        bevy::render::texture::ImagePlugin::default(),
        PipelinedRenderingPlugin,
        CorePipelinePlugin,
        PbrPlugin::default(),
        bevy::gltf::GltfPlugin::default(),
        AnimationPlugin::default(),
        SpritePlugin,
        TextPlugin,
        UiPlugin,
    ));
    app.insert_resource(WinitSettings {
        // Continuous updates (focused or not): the game loop isn't throttled
        // to 60 Hz, so the frame time reflects real rendering cost (target
        // 120+ FPS) regardless of window focus.
        focused_mode: UpdateMode::Continuous,
        unfocused_mode: UpdateMode::Continuous,
    });
    app.insert_resource(AvatarStateHandle(avatar_state));
    app.insert_resource(RemoteAvatarsHandle(remote_avatars));
    app.insert_resource(WorldObjectsHandle(world_objects));
    app.insert_resource(AutoInput(false));
    app.insert_resource(CosmeticSlots::default());
    app.insert_resource(SharedFrameStats::default());
    app.add_plugins((
        FrameTimeDiagnosticsPlugin,
        LogDiagnosticsPlugin::default(),
        bevy::render::diagnostic::RenderDiagnosticsPlugin,
    ));
    app.add_systems(Startup, setup_scene);
    app.add_systems(
        Update,
        (
            read_keyboard,
            wasm_render_tick,
            build_avatar_models,
            sync_avatar_scene,
            apply_avatar_pose,
            sync_world_objects,
            follow_camera,
            update_chunk_claims,
            zone_transition,
            sync_remote_avatars,
            link_avatar_players,
            animate_avatars,
            attach_cosmetics,
            monitor_signaling,
            measure_rtt,
            debug_frustum_culling,
            update_hud,
            record_frame_stats,
            exit_after_wasm_error,
        )
            .chain(),
    );
    app
}

/// Runs one Wasm `render_tick` per frame. First the network is polled so any
/// remote avatar poses that arrived are registered in the shared map, then the
/// guest computes a new pose and pushes it through `update_avatar_transform`
/// into the shared handle.
///
/// A guest trap is a recoverable condition: it is recorded in host state (and
/// shown in the HUD), the guest is no longer called, and the app exits
/// gracefully — the host itself never crashes.
fn wasm_render_tick(runtime: Res<WasmRuntime>, stats: Res<SharedFrameStats>) {
    let mut guard = runtime.store.lock().unwrap();
    {
        let store = &mut *guard;
        store.data_mut().update_frame();
        let net_start = std::time::Instant::now();
        poll_network(store);
        // Flush batched messages (≤16ms window) and retry unacked reliable
        // messages once per frame.
        if let Some(pc) = store.data().peer_connection() {
            pc.flush_batches();
            pc.process_reliable_retries();
        }
        stats
            .0
            .lock()
            .unwrap()
            .add_network(net_start.elapsed().as_secs_f64() * 1000.0);
    }
    let trapped = {
        let store = &mut *guard;
        if store.data().wasm_error().is_some() {
            None
        } else {
            // Replenish fuel each tick so a guest cannot starve the host.
            let _ = store.set_fuel(crate::host_state::MAX_FUEL_PER_TICK);
            let frame = store.data().frame_count();
            let wasm_start = std::time::Instant::now();
            let result = runtime.render_tick.call(store, ());
            stats
                .0
                .lock()
                .unwrap()
                .add_wasm(wasm_start.elapsed().as_secs_f64() * 1000.0);
            match result {
                Ok(()) => None,
                Err(err) => Some((format!("{err:#}"), frame)),
            }
        }
    };
    if let Some((error, frame)) = trapped {
        guard.data_mut().set_wasm_error(error.clone(), frame);
        eprintln!("wasm guest trapped: {error}");
    }
}

/// Monitors the signaling-server link and reconnects with exponential backoff.
///
/// While connected it probes the WebSocket every `PROBE_EVERY_FRAMES` frames.
/// On a lost link it switches to [`SignalingStatus::Reconnecting`], waits out
/// the current backoff delay, then attempts a reconnect (keeping the UDP
/// socket so the P2P address and peer map survive). Once the server is back it
/// transitions back to connected.
fn monitor_signaling(runtime: Res<WasmRuntime>) {
    const PROBE_EVERY_FRAMES: u64 = 180;
    const BACKOFF_BASE_MS: u64 = 500;
    const BACKOFF_MAX_MS: u64 = 30_000;

    let mut guard = runtime.store.lock().unwrap();
    let store = &mut *guard;
    let frame = store.data().frame_count();
    let due = store
        .data()
        .last_signaling_probe()
        .is_none_or(|f| frame.saturating_sub(f) >= PROBE_EVERY_FRAMES);
    if !due {
        return;
    }
    store.data_mut().set_last_signaling_probe(Some(frame));
    if store.data().peer_connection().is_none() {
        return;
    }

    let status = store.data().signaling_status().clone();
    let now = Instant::now();
    match status {
        SignalingStatus::Connected => {
            let probe = {
                store
                    .data_mut()
                    .peer_connection_mut()
                    .expect("peer connection is set")
                    .probe_signaling()
            };
            if let Err(e) = probe {
                let delay = backoff(BACKOFF_BASE_MS, BACKOFF_MAX_MS, 1);
                store
                    .data_mut()
                    .set_signaling_status(SignalingStatus::Reconnecting { attempt: 1 });
                store.data_mut().set_signaling_next_retry(Some(now + delay));
                eprintln!("signaling link lost: {e}; reconnect in {delay:?}");
            }
        }
        SignalingStatus::Reconnecting { attempt } => {
            let retry_at = store.data().signaling_next_retry().unwrap_or(now);
            if now < retry_at {
                return;
            }
            let result = {
                store
                    .data_mut()
                    .peer_connection_mut()
                    .expect("peer connection is set")
                    .reconnect()
            };
            match result {
                Ok(_) => {
                    store
                        .data_mut()
                        .set_signaling_status(SignalingStatus::Connected);
                    store.data_mut().set_signaling_next_retry(None);
                    eprintln!("signaling link reconnected");
                }
                Err(e) => {
                    let next = attempt + 1;
                    let delay = backoff(BACKOFF_BASE_MS, BACKOFF_MAX_MS, next);
                    store
                        .data_mut()
                        .set_signaling_status(SignalingStatus::Reconnecting { attempt: next });
                    store.data_mut().set_signaling_next_retry(Some(now + delay));
                    eprintln!(
                        "signaling reconnect failed (attempt {next}): {e}; retry in {delay:?}"
                    );
                }
            }
        }
        SignalingStatus::Down => {
            // Only reachable if something set the status directly; treat like
            // a reconnect that is due immediately.
            let result = {
                store
                    .data_mut()
                    .peer_connection_mut()
                    .expect("peer connection is set")
                    .reconnect()
            };
            if result.is_ok() {
                store
                    .data_mut()
                    .set_signaling_status(SignalingStatus::Connected);
                store.data_mut().set_signaling_next_retry(None);
            }
        }
    }
}

/// Exponential backoff with a ceiling: `base * 2^(attempt-1)`, capped at `max`.
fn backoff(base_ms: u64, max_ms: u64, attempt: u32) -> Duration {
    let shift = (attempt.saturating_sub(1)).min(31);
    Duration::from_millis((base_ms.saturating_mul(1u64 << shift)).min(max_ms))
}

/// Polls real keyboard input each frame and writes it into the host's input
/// buffer (which the guest reads through the `get_input_*` host functions).
/// In auto mode, a scripted input drives role A toward role B instead.
fn read_keyboard(
    mut events: EventReader<KeyboardInput>,
    runtime: Res<WasmRuntime>,
    auto: Res<AutoInput>,
    stats: Res<SharedFrameStats>,
) {
    let start = std::time::Instant::now();
    stats.0.lock().unwrap().begin_frame();
    let mut input = InputState::default();
    if auto.0 {
        let store = runtime.store.lock().unwrap();
        if store.data().movement_axis() == 0 {
            input.move_left = 1;
        }
    } else {
        for event in events.read() {
            if event.state != ButtonState::Pressed {
                continue;
            }
            match event.key_code {
                KeyCode::KeyW => input.move_up = 1,
                KeyCode::KeyS => input.move_down = 1,
                KeyCode::KeyA => input.move_left = 1,
                KeyCode::KeyD => input.move_right = 1,
                KeyCode::Space => input.action_1 = 1,
                KeyCode::ShiftLeft => input.action_2 = 1,
                _ => {}
            }
        }
    }
    let mut store = runtime.store.lock().unwrap();
    store.data_mut().set_input(input);
    stats
        .0
        .lock()
        .unwrap()
        .add_input(start.elapsed().as_secs_f64() * 1000.0);
}

/// Periodically pings every known peer to measure round-trip time, and records
/// the samples into the shared frame stats.
fn measure_rtt(runtime: Res<WasmRuntime>, stats: Res<SharedFrameStats>, frames: Res<FrameCount>) {
    const PING_EVERY_FRAMES: u64 = 60;
    if !(frames.0 as u64).is_multiple_of(PING_EVERY_FRAMES) {
        return;
    }
    let store = runtime.store.lock().unwrap();
    if let Some(pc) = store.data().peer_connection() {
        pc.ping_all();
        if let Some(avg) = pc.avg_rtt_ms() {
            stats.0.lock().unwrap().record_rtt(avg);
        }
    }
}

/// Closes the frame's timing window, pushes the sample, and emits the tracing
/// frame-timing breakdown (visible with `RUST_LOG=debug`).
fn record_frame_stats(stats: Res<SharedFrameStats>) {
    let mut s = stats.0.lock().unwrap();
    s.end_frame();
    let frame = s.frames;
    let t = s.samples().last().copied().unwrap_or_default();
    tracing::debug!(
        frame,
        frame_ms = t.frame_ms,
        input_ms = t.input_ms,
        network_ms = t.network_ms,
        wasm_ms = t.wasm_ms,
        "Frame timing"
    );
    if frame.is_multiple_of(120) {
        tracing::info!(frame, avg_frame_ms = t.frame_ms, "Frame timing");
    }
}

/// Verifies Bevy's frustum culling: logs how many mesh entities were visible
/// vs culled, and how many unique materials are in use (a proxy for batching).
fn debug_frustum_culling(
    visibility: Query<&ViewVisibility>,
    materials: Query<&Handle<StandardMaterial>>,
    frames: Res<FrameCount>,
) {
    const LOG_EVERY: u32 = 120;
    if !frames.0.is_multiple_of(LOG_EVERY) {
        return;
    }
    let total = visibility.iter().len();
    let visible = visibility.iter().filter(|v| v.get()).count();
    let material_count = materials.iter().len();
    let unique_materials: usize = materials
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    tracing::info!(
        visible,
        culled = total.saturating_sub(visible),
        material_entities = material_count,
        unique_materials,
        "Frustum culling / batching"
    );
}

/// Builds and updates the score/status HUD from host state: scores, peer
/// connectivity, signaling status, and any Wasm error. The text is logged
/// whenever it changes so lifecycle transitions are visible in stdout too.
fn update_hud(runtime: Res<WasmRuntime>, mut query: Query<&mut Text, With<ScoreText>>) {
    let Ok(mut text) = query.get_single_mut() else {
        return;
    };
    let store = runtime.store.lock().unwrap();

    let local = store.data().tag_score();
    let remote_total: u32 = store.data().remote_scores().lock().unwrap().values().sum();
    let has_peer = store.data().peer_connection().is_some();

    let (peer_line, sig_line) = if has_peer {
        let now = Instant::now();
        let map = store.data().remote_avatars().lock().unwrap();
        let any_fresh = map
            .values()
            .any(|pose| now.duration_since(pose.last_seen).as_secs_f32() < 2.0);
        let ever_seen = !map.is_empty();
        let peer_line = if any_fresh {
            "Peer: connected".to_string()
        } else if ever_seen {
            "Peer: DISCONNECTED (avatar removed)".to_string()
        } else {
            "Peer: waiting for remote...".to_string()
        };
        let sig_line = format!("Signaling: {}", store.data().signaling_status().as_str());
        (peer_line, sig_line)
    } else {
        ("Peer: n/a".to_string(), "Signaling: n/a".to_string())
    };

    let wasm_line = match store.data().wasm_error() {
        Some(err) => format!("Wasm error: {err}"),
        None => "Wasm: running".to_string(),
    };

    let zone_line = match store.data().current_zone() {
        Some(zone) => format!(
            "Zone: chunk ({},{}) @ {} ({} edits)",
            zone.chunk.x,
            zone.chunk.z,
            zone.owner,
            store
                .data()
                .zone_content()
                .map(|s| s.edits.len())
                .unwrap_or(0)
        ),
        None => "Zone: none".to_string(),
    };

    let label = format!(
        "Local Score: {local} | Remote Score: {remote_total}\n{peer_line}\n{sig_line}\n{zone_line}\n{wasm_line}"
    );
    if text.sections[0].value != label {
        println!("[hud] {label}");
        text.sections[0].value = label;
    }
}

/// Exits cleanly (exit code 0) shortly after a Wasm guest trap, so the error
/// is visible before the process shuts down without crashing.
fn exit_after_wasm_error(
    runtime: Res<WasmRuntime>,
    frames: Res<FrameCount>,
    mut exit: EventWriter<AppExit>,
) {
    const GRACE_FRAMES: u32 = 90;
    let store = runtime.store.lock().unwrap();
    let Some(err_frame) = store.data().wasm_error_frame() else {
        return;
    };
    if frames.0 as u64 >= err_frame + u64::from(GRACE_FRAMES) {
        exit.send(AppExit::Success);
    }
}

/// Drains the peer socket and dispatches datagrams: 16-byte payloads are
/// validated avatar poses (timestamped for stale-avatar cleanup), 4-byte
/// payloads are tag scores broadcast by the remote guest, and longer payloads
/// are chunk claims, chunk-state pointers, or zone-join/zone-state messages.
///
/// Made public so tests can drive the wire path directly.
pub fn poll_network(store: &mut wasmtime::Store<HostState>) {
    let incoming = match store.data().peer_connection() {
        Some(pc) => pc.poll_incoming_from(),
        None => return,
    };
    if incoming.is_empty() {
        return;
    }
    let remote = store.data().remote_avatars().clone();
    let mut map = remote.lock().unwrap();
    let remote_scores = store.data().remote_scores().clone();
    let mut scores = remote_scores.lock().unwrap();
    let chunk_registry = store.data().chunk_registry().clone();
    let mut chunks = chunk_registry.lock().unwrap();
    for (addr, payload) in incoming {
        match payload.len() {
            16 | 8 => {
                let Some(peer_id) = peer_id_for(store, addr) else {
                    continue;
                };
                // 8-byte payloads are delta-compressed poses; reconstruct using
                // the previous known pose for this peer.
                let pose = crate::net::decode_pose(map.get(&peer_id), &payload);
                let Some(pose) = pose else { continue };
                map.insert(peer_id, pose);
            }
            4 => {
                let Some(peer_id) = peer_id_for(store, addr) else {
                    continue;
                };
                let score = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                scores.insert(peer_id, score);
            }
            _ => {
                // Chunk claim: the claim carries its own peer id and is
                // self-authenticating (no reverse lookup needed).
                if let Some(claim) = crate::chunk::ChunkClaim::from_wire(&payload) {
                    chunks.apply_claim(&claim, addr);
                    println!(
                        "[chunk] {} hosts chunk region ({},{})",
                        claim.peer_id, claim.origin_x, claim.origin_z
                    );
                } else if let Some(pointer) = crate::chunk::ChunkStatePointer::from_wire(&payload) {
                    // Cache the owner's chunk state so it survives their
                    // departure ("ruins" available while the owner is offline).
                    store.data_mut().ingest_state_pointer(&pointer, addr);
                    println!(
                        "[chunk-state] cached '{}' for chunk ({},{})",
                        pointer.peer_id, pointer.chunk_x, pointer.chunk_z
                    );
                } else if let Some(req) = crate::chunk::ZoneJoinRequest::from_wire(&payload) {
                    if let Err(e) = store.data_mut().handle_zone_join_request(&req, addr) {
                        eprintln!("[zone] join response failed: {e}");
                    }
                } else if let Some(zone) = crate::chunk::ZoneState::from_wire(&payload) {
                    store.data_mut().handle_zone_state(&zone);
                }
            }
        }
    }
}

/// Resolves the peer id for a sender address, if the link knows it.
fn peer_id_for(store: &wasmtime::Store<HostState>, addr: std::net::SocketAddr) -> Option<String> {
    store
        .data()
        .peer_connection()
        .and_then(|pc| pc.peer_id_for_addr(&addr).cloned())
}

/// Copies the pose written by the Wasm guest onto the avatar's `Transform`.
fn apply_avatar_pose(
    handle: Res<AvatarStateHandle>,
    mut query: Query<&mut Transform, With<Avatar>>,
) {
    if let Ok(mut transform) = query.get_single_mut() {
        let pose = *handle.0.lock().unwrap();
        transform.translation = Vec3::new(pose.x, pose.y, pose.z);
        transform.rotation = Quat::from_rotation_y(pose.rot_y);
    }
}

/// Drains the boxes the guest drew this frame and mirrors them as cube
/// entities, reusing existing cubes and spawning/despawning to match the
/// guest's draw count.
fn sync_world_objects(
    mut commands: Commands,
    objects: Res<WorldObjectsHandle>,
    cube_mesh: Res<WorldObjectMeshHandle>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut cubes: Query<(Entity, &mut Transform, &mut Handle<StandardMaterial>), With<WorldObjectCube>>,
    mut palette: Local<HashMap<(u8, u8, u8), Handle<StandardMaterial>>>,
) {
    let mut list = objects.0.lock().unwrap();
    let mut slots = cubes.iter_mut().collect::<Vec<_>>();
    // Despawn cubes that outnumber this frame's draw list.
    if slots.len() > list.len() {
        for (entity, _, _) in slots.drain(list.len()..) {
            commands.entity(entity).despawn();
        }
    }
    // Reuse existing cubes: reposition, resize, recolor.
    for (object, (_, transform, material)) in list.iter().zip(slots.iter_mut()) {
        let key = color_key(object);
        let mat = palette
            .entry(key)
            .or_insert_with(|| make_cube_material(&mut materials, key));
        **material = mat.clone();
        transform.translation = Vec3::new(object.x, object.y, object.z);
        transform.scale = Vec3::new(object.sx, object.sy, object.sz);
    }
    // Spawn fresh cubes for objects beyond the current count.
    for object in list.iter().skip(slots.len()) {
        let key = color_key(object);
        let material = palette
            .entry(key)
            .or_insert_with(|| make_cube_material(&mut materials, key))
            .clone();
        commands.spawn((
            MaterialMeshBundle {
                mesh: cube_mesh.0.clone(),
                material,
                transform: Transform {
                    translation: Vec3::new(object.x, object.y, object.z),
                    scale: Vec3::new(object.sx, object.sy, object.sz),
                    ..default()
                },
                ..default()
            },
            WorldObjectCube,
        ));
    }
    list.clear();
}

/// Quantized RGB key for the material cache.
fn color_key(object: &WorldObject) -> (u8, u8, u8) {
    (
        (object.r.clamp(0.0, 1.0) * 255.0) as u8,
        (object.g.clamp(0.0, 1.0) * 255.0) as u8,
        (object.b.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

/// Builds a material for a quantized color.
fn make_cube_material(
    materials: &mut Assets<StandardMaterial>,
    key: (u8, u8, u8),
) -> Handle<StandardMaterial> {
    materials.add(StandardMaterial {
        base_color: Color::srgb(
            key.0 as f32 / 255.0,
            key.1 as f32 / 255.0,
            key.2 as f32 / 255.0,
        ),
        perceptual_roughness: 0.4,
        ..default()
    })
}

/// Smoothly tracks the avatar so it stays centered as the player moves.
fn follow_camera(
    avatar: Res<AvatarStateHandle>,
    mut camera: Query<&mut Transform, (With<Camera3d>, Without<Avatar>)>,
) {
    let pose = *avatar.0.lock().unwrap();
    for mut transform in &mut camera {
        let target = Vec3::new(pose.x, 3.0, pose.z + 7.0);
        transform.translation = transform.translation.lerp(target, 0.15);
        transform.look_at(Vec3::new(pose.x, 0.8, pose.z), Vec3::Y);
    }
}

/// Claims chunks for the local avatar: whenever the avatar crosses into a new
/// chunk, this peer broadcasts "I am hosting chunks X,Y and X+1,Y" and records
/// the ownership in the shared DHT.
fn update_chunk_claims(
    avatar: Res<AvatarStateHandle>,
    runtime: Res<WasmRuntime>,
    mut last: Local<Option<crate::chunk::ChunkCoord>>,
) {
    let pose = *avatar.0.lock().unwrap();
    let coord = crate::chunk::ChunkCoord::at(pose.x, pose.z);
    if *last == Some(coord) {
        return;
    }
    *last = Some(coord);
    let mut store = runtime.store.lock().unwrap();
    match store.data_mut().claim_around_position(pose.x, pose.z) {
        Ok(()) => println!(
            "[chunk] now hosting chunks ({},{}) and ({},{})",
            coord.x,
            coord.z,
            coord.x + 1,
            coord.z
        ),
        Err(e) => println!("[chunk] claim failed: {e}"),
    }
}

/// Handles seamless zone handoff: whenever the avatar crosses a chunk
/// boundary, the host disconnects from the previous zone's state stream,
/// resolves the new chunk's owner from the DHT, connects to it directly, and
/// downloads its live state — the "server" changes transparently.
fn zone_transition(
    avatar: Res<AvatarStateHandle>,
    runtime: Res<WasmRuntime>,
    mut last: Local<Option<crate::chunk::ChunkCoord>>,
) {
    let pose = *avatar.0.lock().unwrap();
    let coord = crate::chunk::ChunkCoord::at(pose.x, pose.z);
    if *last == Some(coord) {
        return;
    }
    *last = Some(coord);
    let mut store = runtime.store.lock().unwrap();
    if let Err(e) = store.data_mut().transition_zone(coord) {
        println!("[zone] transition failed: {e}");
    }
}

/// Builds the [`AvatarModels`] resources once each rigged glTF has finished
/// loading. For each model it picks the default scene, wraps its first
/// animation clip in a graph (walk clip = node 0), and records the scale/lift
/// that place the model's feet on the ground.
fn build_avatar_models(
    mut commands: Commands,
    loads: Option<Res<AvatarGltfLoads>>,
    mut events: EventReader<AssetEvent<Gltf>>,
    gltf_assets: Res<Assets<Gltf>>,
    mut graph_assets: ResMut<Assets<AnimationGraph>>,
    existing: Option<Res<AvatarModels>>,
) {
    let Some(loads) = loads else { return };
    if existing.is_some() {
        return;
    }
    for event in events.read() {
        let AssetEvent::LoadedWithDependencies { id } = *event else {
            continue;
        };
        if id != loads.human.id() {
            continue;
        }
        let Some(gltf) = gltf_assets.get(id) else {
            continue;
        };
        let Some(scene) = gltf.default_scene.clone() else {
            continue;
        };
        // The humanoid's single clip is its walk cycle (node 0 of the graph).
        let Some(clip) = gltf.animations.first() else {
            continue;
        };
        let (graph, walk) = AnimationGraph::from_clip(clip.clone());
        let graph = graph_assets.add(graph);
        commands.insert_resource(AvatarModels {
            human: AvatarModel {
                scene,
                graph,
                walk,
                // CesiumMan is 1.14 units tall with feet at y=-0.569; raise and
                // scale so it reads as a ~2-unit avatar standing on the plane.
                scale: 1.75,
                lift: 1.0,
            },
        });
        println!("[avatar] human rig loaded (walk clip node {walk:?})");
        break;
    }
}

/// Spawns (and hot-swaps) the local avatar scene from the avatar path in host
/// state, so the avatar is configurable before the game starts and can be
/// changed at runtime by the guest's `load_avatar`.
fn sync_avatar_scene(
    runtime: Res<WasmRuntime>,
    mut commands: Commands,
    models: Option<Res<AvatarModels>>,
    avatars: Query<(Entity, &AvatarSource), With<Avatar>>,
) {
    let desired = runtime
        .store
        .lock()
        .unwrap()
        .data()
        .avatar_path()
        .to_string();
    match avatars.iter().next() {
        Some((entity, source)) if source.0 == desired => {
            let _ = entity;
        }
        Some((entity, _)) => {
            commands.entity(entity).despawn();
            if let Some(models) = models.as_ref() {
                spawn_avatar(&mut commands, models, desired);
            }
        }
        None => {
            if let Some(models) = models.as_ref() {
                spawn_avatar(&mut commands, models, desired);
            }
        }
    }
}

/// Spawns the local avatar: a root entity carrying the pose/walk state and a
/// child `SceneBundle` holding the rigged model selected by the avatar path.
/// The child's `AnimationPlayer` is linked by `link_avatar_players` once the
/// scene loads. Mirrors the pose applied by `apply_avatar_pose`.
fn spawn_avatar(commands: &mut Commands, models: &AvatarModels, path: String) {
    let kind = AvatarKind::for_path(&path);
    let model = models.model(kind);
    let root = commands
        .spawn((
            Avatar,
            AvatarSource(path),
            TransformBundle::default(),
            VisibilityBundle::default(),
            AvatarAnim {
                kind,
                player: Entity::PLACEHOLDER,
                speed: 0.0,
                last_pos: Vec3::ZERO,
            },
        ))
        .id();
    commands.entity(root).with_children(|parent| {
        parent.spawn((
            SceneBundle {
                scene: model.scene.clone(),
                transform: Transform {
                    translation: Vec3::new(0.0, model.lift, 0.0),
                    scale: Vec3::splat(model.scale),
                    ..default()
                },
                ..default()
            },
            AvatarScene,
        ));
    });
}

/// Query of every avatar scene root (local or remote).
type AvatarRoots<'w, 's> = Query<'w, 's, Entity, Or<(With<Avatar>, With<RemoteAvatar>)>>;

/// Slots cosmetic meshes onto avatar attachment points.
///
/// Avatars are single batched mesh entities, so a cosmetic is parented to the
/// avatar root at a fixed world offset per attachment point (it inherits the
/// avatar's transform). Applies each slot exactly once per avatar.
fn attach_cosmetics(
    mut commands: Commands,
    avatars: AvatarRoots,
    asset_server: Res<AssetServer>,
    slots: Res<CosmeticSlots>,
    mut attached: Local<Vec<(Entity, AttachmentPoint, String)>>,
) {
    if slots.0.is_empty() {
        return;
    }
    for root in &avatars {
        for (point, slot) in &slots.0 {
            let key = (root, *point, slot.item_id.clone());
            if attached.contains(&key) {
                continue;
            }
            let offset = attachment_offset(*point);
            let scene = asset_server
                .load(GltfAssetLabel::Scene(0).from_asset(slot.mesh_asset_path.clone()));
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    SceneBundle {
                        scene,
                        transform: Transform::from_translation(offset),
                        ..default()
                    },
                    Cosmetic,
                ));
            });
            println!(
                "[cosmetic] attached '{}' to attachment point '{}'",
                slot.item_id,
                point.as_str()
            );
            attached.push(key);
        }
    }
}

/// World-space offset of each attachment point relative to the avatar root.
fn attachment_offset(point: AttachmentPoint) -> Vec3 {
    match point {
        AttachmentPoint::Head => Vec3::new(0.0, 1.95, 0.0),
        AttachmentPoint::Chest => Vec3::new(0.0, 1.4, 0.0),
        AttachmentPoint::LeftHand => Vec3::new(-1.0, 1.5, 0.0),
        AttachmentPoint::RightHand => Vec3::new(1.0, 1.5, 0.0),
        AttachmentPoint::Back => Vec3::new(0.0, 1.3, -0.25),
        AttachmentPoint::LeftFoot => Vec3::new(-0.15, 0.1, 0.03),
        AttachmentPoint::RightFoot => Vec3::new(0.15, 0.1, 0.03),
    }
}

/// Spawns, updates, and despawns remote avatar entities from the shared pose
/// map: one rigged scene root per remote peer (same model as the local avatar),
/// removed if it stops reporting for more than 2 seconds.
fn sync_remote_avatars(
    handle: Res<RemoteAvatarsHandle>,
    runtime: Res<WasmRuntime>,
    mut commands: Commands,
    models: Option<Res<AvatarModels>>,
    mut avatars: Query<(Entity, &RemotePeerId, &mut Transform)>,
) {
    let now = Instant::now();
    let map = handle.0.lock().unwrap();
    let avatar_path = runtime
        .store
        .lock()
        .unwrap()
        .data()
        .avatar_path()
        .to_string();

    let mut stale = Vec::new();
    for (entity, peer, _) in avatars.iter() {
        let fresh = map
            .get(&peer.0)
            .is_some_and(|pose| now.duration_since(pose.last_seen).as_secs_f32() < 2.0);
        if !fresh {
            stale.push(entity);
        }
    }
    for entity in stale {
        commands.entity(entity).despawn();
    }

    let kind = AvatarKind::for_path(&avatar_path);
    let Some(model) = models.as_ref().map(|m| m.model(kind)) else {
        return;
    };
    for (peer_id, pose) in map.iter() {
        if now.duration_since(pose.last_seen).as_secs_f32() >= 2.0 {
            continue;
        }
        match avatars.iter_mut().find(|(_, p, _)| p.0 == *peer_id) {
            Some((_, _, mut transform)) => {
                transform.translation = Vec3::new(pose.x, pose.y, pose.z);
                transform.rotation = Quat::from_rotation_y(pose.rot_y);
            }
            None => {
                let root = commands
                    .spawn((
                        RemoteAvatar,
                        RemotePeerId(peer_id.clone()),
                        AvatarSource(avatar_path.clone()),
                        TransformBundle::default(),
                        VisibilityBundle::default(),
                        AvatarAnim {
                            kind,
                            player: Entity::PLACEHOLDER,
                            speed: 0.0,
                            last_pos: Vec3::new(pose.x, pose.y, pose.z),
                        },
                    ))
                    .id();
                commands.entity(root).with_children(|parent| {
                    parent.spawn((
                        SceneBundle {
                            scene: model.scene.clone(),
                            transform: Transform {
                                translation: Vec3::new(0.0, model.lift, 0.0),
                                scale: Vec3::splat(model.scale),
                                ..default()
                            },
                            ..default()
                        },
                        AvatarScene,
                        // Only the local avatar casts shadows (cheaper shadow pass).
                        NotShadowCaster,
                    ));
                });
            }
        }
    }
}

/// Links each avatar's rigged scene to its `AnimationPlayer` once the scene has
/// loaded (Bevy auto-adds the player to the spawned scene). Walks up the parent
/// chain to find the owning avatar root, attaches the model's animation graph,
/// freezes the clip at its neutral stance, and records the player entity on the
/// root so `animate_avatars` can drive it.
fn link_avatar_players(
    mut commands: Commands,
    models: Option<Res<AvatarModels>>,
    mut avatars: Query<(Entity, &mut AvatarAnim)>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
    markers: Query<(Entity, Option<&Avatar>, Option<&RemoteAvatar>)>,
    parents: Query<&Parent>,
) {
    let Some(models) = models else { return };
    for (player_entity, mut player) in &mut players {
        let mut cur = player_entity;
        let mut root = None;
        loop {
            if let Ok((_, avatar, remote)) = markers.get(cur) {
                if avatar.is_some() || remote.is_some() {
                    root = Some(cur);
                    break;
                }
            }
            match parents.get(cur) {
                Ok(parent) => cur = parent.get(),
                Err(_) => break,
            }
        }
        let Some(root) = root else { continue };

        let kind = avatars.get(root).ok().map(|(_, anim)| anim.kind);
        let Some(kind) = kind else { continue };
        let model = models.model(kind);

        player.play(model.walk);
        if let Some(anim) = player.animation_mut(model.walk) {
            anim.set_repeat(RepeatAnimation::Forever);
            anim.pause();
        }
        commands.entity(player_entity).insert((AvatarPlayer, model.graph.clone()));
        if let Ok((_, mut anim)) = avatars.get_mut(root) {
            anim.player = player_entity;
        }
    }
}

/// Drives the universal avatar walk on every rigged avatar.
///
/// Velocity is inferred from each avatar root's world-space translation (no
/// game involvement needed): when moving, the avatar turns to face its travel
/// direction and the walk clip plays at a speed-proportional rate; when idle it
/// freezes at the neutral stance frame and keeps the game-set rotation.
fn animate_avatars(
    time: Res<Time>,
    models: Option<Res<AvatarModels>>,
    mut avatars: Query<(&mut AvatarAnim, &mut Transform)>,
    mut players: Query<(Entity, &mut AnimationPlayer), With<AvatarPlayer>>,
) {
    let Some(models) = models else { return };
    let dt = time.delta_seconds_f64().max(1e-6) as f32;
    for (mut anim, mut transform) in &mut avatars {
        let pos = transform.translation;
        let disp = pos - anim.last_pos;
        let instant = (disp.length() / dt).min(20.0);
        anim.speed += (instant - anim.speed) * 0.3;
        anim.last_pos = pos;

        if anim.speed > 0.15 {
            // Face the horizontal direction of travel (local +Z = forward).
            let yaw = disp.x.atan2(disp.z);
            transform.rotation = Quat::from_rotation_y(yaw);
        }

        let Ok((_, mut player)) = players.get_mut(anim.player) else {
            continue;
        };
        let model = models.model(anim.kind);
        let moving = anim.speed > 0.15;
        let Some(clip) = player.animation_mut(model.walk) else {
            continue;
        };
        if moving {
            let rate = (anim.speed / 3.0).clamp(0.25, 2.0);
            clip.resume();
            clip.set_speed(rate);
        } else {
            clip.pause();
            clip.seek_to(0.0);
        }
    }
}

/// Self-terminates the integration test once enough frames have rendered.
fn terminate_after_frames(
    limit: Res<TestFrameLimit>,
    frames: Res<FrameCount>,
    mut exit: EventWriter<AppExit>,
) {
    if frames.0 >= limit.0 {
        exit.send(AppExit::Success);
    }
}

fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    asset_server: Res<AssetServer>,
) {
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(0.0, 3.0, 7.0).looking_at(Vec3::new(0.0, 0.8, 0.0), Vec3::Y),
        tonemapping: Tonemapping::None,
        ..default()
    });

    // Single directional light; shadows on (modest 1024 map).
    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            shadows_enabled: true,
            shadow_depth_bias: 0.5,
            illuminance: 8000.0,
            ..default()
        },
        transform: Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.7, 0.4, 0.0)),
        ..default()
    });
    commands.insert_resource(DirectionalLightShadowMap { size: 1024 });

    commands.spawn(MaterialMeshBundle {
        mesh: meshes.add(Plane3d::default().mesh().size(20.0, 20.0)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb_u8(72, 92, 77),
            perceptual_roughness: 0.9,
            ..default()
        }),
        ..default()
    });

    // Rigged avatar models: the glTF scenes are loaded here and their animation
    // graphs are built in `build_avatar_models` once the assets finish loading.
    // All avatar material/color variants are gone — the model carries its own
    // baked materials.
    let human_scene = asset_server.load::<Gltf>("avatars/human.glb");
    commands.insert_resource(AvatarGltfLoads { human: human_scene });
    commands.insert_resource(WorldObjectMeshHandle(meshes.add(Cuboid::new(1.0, 1.0, 1.0))));

    commands.spawn((
        TextBundle::from_section(
            "Local Score: 0 | Remote Score: 0",
            TextStyle {
                font_size: 24.0,
                color: Color::srgb_u8(240, 240, 240),
                ..default()
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            left: Val::Px(10.0),
            top: Val::Px(10.0),
            ..default()
        }),
        ScoreText,
    ));
}

#[cfg(test)]
mod avatar_tests {
    use super::*;

    #[test]
    fn kind_resolves_from_avatar_path() {
        assert_eq!(AvatarKind::for_path("avatars/blue.glb"), AvatarKind::Human);
        assert_eq!(AvatarKind::for_path("avatar_standard.glb"), AvatarKind::Human);
        assert_eq!(AvatarKind::for_path("avatars/fox.glb"), AvatarKind::Fox);
        assert_eq!(AvatarKind::for_path("fox.glb"), AvatarKind::Fox);
    }

    #[test]
    fn un_wired_kinds_fall_back_to_human() {
        let models = AvatarModels {
            human: AvatarModel {
                scene: Handle::default(),
                graph: Handle::default(),
                walk: AnimationNodeIndex::default(),
                scale: 1.75,
                lift: 1.0,
            },
        };
        assert_eq!(models.model(AvatarKind::Human).scale, 1.75);
        assert_eq!(models.model(AvatarKind::Fox).scale, 1.75);
    }

    #[test]
    fn walk_rate_is_clamped() {
        let rate = |speed: f32| (speed / 3.0).clamp(0.25, 2.0);
        assert_eq!(rate(0.1), 0.25, "idle-ish speed floors at quarter rate");
        assert_eq!(rate(3.0), 1.0, "typical walk speed plays at 1x");
        assert_eq!(rate(9.0), 2.0, "sprint caps at double rate");
    }
}

/// Registers the frame-limit terminator used by the integration tests.
pub fn add_test_terminator(app: &mut App) {
    add_exit_after(app, TEST_FRAMES);
}

/// Registers a terminator that exits the app after `frames` rendered frames.
pub fn add_exit_after(app: &mut App, frames: u32) {
    app.insert_resource(TestFrameLimit(frames));
    app.add_systems(Update, terminate_after_frames);
}
