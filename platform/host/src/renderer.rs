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

use crate::avatar_state::{AvatarPose, AvatarState};
use crate::host_state::{HostState, SignalingStatus};
use crate::input_state::InputState;
use bevy::app::AppExit;
use bevy::core::{FrameCount, TaskPoolPlugin, TypeRegistrationPlugin};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::core_pipeline::CorePipelinePlugin;
use bevy::gltf::GltfAssetLabel;
use bevy::input::keyboard::{KeyboardInput, KeyCode};
use bevy::input::ButtonState;
use bevy::log::LogPlugin;
use bevy::pbr::{DirectionalLightBundle, PbrPlugin, StandardMaterial};
use bevy::prelude::*;
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::render::RenderPlugin;
use bevy::scene::ScenePlugin;
use bevy::sprite::SpritePlugin;
use bevy::text::{Text, TextStyle, TextPlugin};
use bevy::transform::TransformPlugin;
use bevy::ui::{node_bundles::TextBundle, PositionType, Style, UiPlugin, Val};
use bevy::window::WindowPlugin;
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
        WindowPlugin::default(),
        bevy::a11y::AccessibilityPlugin,
        ScenePlugin,
        WinitPlugin::<WakeUp>::default(),
        RenderPlugin::default(),
        bevy::render::texture::ImagePlugin::default(),
        PipelinedRenderingPlugin,
        CorePipelinePlugin,
        PbrPlugin::default(),
        bevy::gltf::GltfPlugin::default(),
        SpritePlugin,
        TextPlugin,
        UiPlugin,
    ));
    app.insert_resource(WinitSettings {
        focused_mode: UpdateMode::reactive(Duration::from_millis(1000 / TARGET_FPS)),
        unfocused_mode: UpdateMode::reactive_low_power(Duration::from_millis(1000 / TARGET_FPS)),
    });
    app.insert_resource(AvatarStateHandle(avatar_state));
    app.insert_resource(RemoteAvatarsHandle(remote_avatars));
    app.insert_resource(AutoInput(false));
    app.insert_resource(CosmeticSlots::default());
    app.add_systems(Startup, setup_scene);
    app.add_systems(
        Update,
        (
            read_keyboard,
            wasm_render_tick,
            sync_avatar_scene,
            apply_avatar_pose,
            update_chunk_claims,
            zone_transition,
            sync_remote_avatars,
            tint_remote_avatars,
            attach_cosmetics,
            monitor_signaling,
            update_hud,
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
fn wasm_render_tick(runtime: Res<WasmRuntime>) {
    let mut guard = runtime.store.lock().unwrap();
    {
        let store = &mut *guard;
        store.data_mut().update_frame();
        poll_network(store);
    }
    let trapped = {
        let store = &mut *guard;
        if store.data().wasm_error().is_some() {
            None
        } else {
            let frame = store.data().frame_count();
            match runtime.render_tick.call(store, ()) {
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
                store.data_mut().set_signaling_status(SignalingStatus::Reconnecting { attempt: 1 });
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
                    store.data_mut().set_signaling_status(SignalingStatus::Connected);
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
                    eprintln!("signaling reconnect failed (attempt {next}): {e}; retry in {delay:?}");
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
                store.data_mut().set_signaling_status(SignalingStatus::Connected);
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
) {
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
            store.data().zone_content().map(|s| s.edits.len()).unwrap_or(0)
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
    let now = Instant::now();
    for (addr, payload) in incoming {
        match payload.len() {
            16 => {
                let Some(peer_id) = peer_id_for(store, addr) else { continue };
                let x = f32::from_le_bytes(payload[0..4].try_into().unwrap());
                let y = f32::from_le_bytes(payload[4..8].try_into().unwrap());
                let z = f32::from_le_bytes(payload[8..12].try_into().unwrap());
                let rot_y = f32::from_le_bytes(payload[12..16].try_into().unwrap());
                if !(x.is_finite() && y.is_finite() && z.is_finite() && rot_y.is_finite()) {
                    continue;
                }
                map.insert(peer_id, AvatarPose { x, y, z, rot_y, last_seen: now });
            }
            4 => {
                let Some(peer_id) = peer_id_for(store, addr) else { continue };
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

/// Spawns (and hot-swaps) the local avatar scene from the avatar path in host
/// state, so the avatar is configurable before the game starts and can be
/// changed at runtime by the guest's `load_avatar`.
fn sync_avatar_scene(
    runtime: Res<WasmRuntime>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    avatars: Query<(Entity, &AvatarSource), With<Avatar>>,
) {
    let desired = runtime.store.lock().unwrap().data().avatar_path().to_string();
    match avatars.iter().next() {
        Some((entity, source)) if source.0 == desired => {
            let _ = entity;
        }
        Some((entity, _)) => {
            commands.entity(entity).despawn();
            spawn_avatar(&mut commands, &asset_server, desired);
        }
        None => spawn_avatar(&mut commands, &asset_server, desired),
    }
}

/// Spawns the local avatar scene root for `path` (relative to the asset dir).
fn spawn_avatar(commands: &mut Commands, asset_server: &AssetServer, path: String) {
    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(path.clone()));
    commands.spawn((
        SceneBundle {
            scene,
            ..default()
        },
        Avatar,
        AvatarSource(path),
    ));
}

/// Query of every avatar scene root (local or remote).
type AvatarRoots<'w, 's> = Query<'w, 's, Entity, Or<(With<Avatar>, With<RemoteAvatar>)>>;

/// Slots cosmetic meshes onto the avatar's named attachment points.
///
/// Each slot is a **verified** cosmetic package; the mesh glb is spawned as a
/// child of its attachment node so it inherits the avatar's transform. Runs
/// each frame so it picks up attachment nodes as they appear after the
/// asynchronous glb scene load, and tracks already-attached cosmetics so each
/// slot is applied exactly once per avatar.
fn attach_cosmetics(
    mut commands: Commands,
    avatars: AvatarRoots,
    tree: Query<(Option<&Children>, Option<&Name>)>,
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
            let Some(attach_node) = find_named_node(root, point.as_str(), &tree) else {
                continue; // scene not fully spawned yet; retry next frame
            };
            let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(slot.mesh_asset_path.clone()));
            commands.entity(attach_node).with_children(|parent| {
                parent.spawn((SceneBundle { scene, ..default() }, Cosmetic));
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

/// Depth-first search for a descendant node with the given name.
fn find_named_node(
    root: Entity,
    name: &str,
    tree: &Query<(Option<&Children>, Option<&Name>)>,
) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(entity) = stack.pop() {
        if entity != root {
            if let Ok((_, Some(node_name))) = tree.get(entity) {
                if node_name.as_str() == name {
                    return Some(entity);
                }
            }
        }
        if let Ok((Some(children), _)) = tree.get(entity) {
            stack.extend(children.iter().copied());
        }
    }
    None
}

/// Spawns, updates, and despawns remote avatar entities from the shared pose
/// map: one glb clone per remote peer, tinted blue, mirroring their latest pose
/// and removed if it stops reporting for more than 2 seconds.
fn sync_remote_avatars(
    handle: Res<RemoteAvatarsHandle>,
    runtime: Res<WasmRuntime>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut avatars: Query<(Entity, &RemotePeerId, &mut Transform)>,
) {
    let now = Instant::now();
    let map = handle.0.lock().unwrap();
    let avatar_path = runtime.store.lock().unwrap().data().avatar_path().to_string();

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

    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset(avatar_path.clone()));
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
                commands.spawn((
                    SceneBundle {
                        scene: scene.clone(),
                        ..default()
                    },
                    RemoteAvatar,
                    RemotePeerId(peer_id.clone()),
                    AvatarSource(avatar_path.clone()),
                ));
            }
        }
    }
}

/// Lazily tints every material-bearing descendant of a freshly spawned remote
/// avatar with a per-entity blue material. Runs each frame so children spawned
/// by the asynchronous glb scene load are caught, but stops once all currently
/// present descendants have been tinted.
fn tint_remote_avatars(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    avatars: Query<Entity, With<RemoteAvatar>>,
    children: Query<&Children>,
    material_query: Query<&Handle<StandardMaterial>>,
    mut done: Local<Vec<Entity>>,
) {
    for root in &avatars {
        if done.contains(&root) {
            continue;
        }
        let mut stack = vec![root];
        let mut found_material = false;
        let mut tinted_any = false;
        while let Some(entity) = stack.pop() {
            if let Ok(handle) = material_query.get(entity) {
                found_material = true;
                let base = materials.get(handle).cloned().unwrap_or_default();
                let tinted = materials.add(StandardMaterial {
                    base_color: Color::srgb_u8(90, 140, 255),
                    ..base
                });
                commands.entity(entity).insert(tinted);
                tinted_any = true;
            }
            if let Ok(children) = children.get(entity) {
                stack.extend(children.iter().copied());
            }
        }
        // Only stop retrying once the scene has fully spawned and yielded a
        // material (child entities can appear across frames).
        if found_material && tinted_any {
            done.push(root);
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
) {
    commands.spawn(Camera3dBundle {
        transform: Transform::from_xyz(0.0, 3.0, 7.0)
            .looking_at(Vec3::new(0.0, 0.8, 0.0), Vec3::Y),
        tonemapping: Tonemapping::None,
        ..default()
    });

    commands.spawn(DirectionalLightBundle {
        directional_light: Default::default(),
        transform: Transform::from_rotation(Quat::from_euler(EulerRot::XYZ, -0.7, 0.4, 0.0)),
        ..default()
    });

    commands.spawn(MaterialMeshBundle {
        mesh: meshes.add(Plane3d::default().mesh().size(20.0, 20.0)),
        material: materials.add(StandardMaterial {
            base_color: Color::srgb_u8(72, 92, 77),
            perceptual_roughness: 0.9,
            ..default()
        }),
        ..default()
    });

    // The local avatar is spawned and hot-swapped by `sync_avatar_scene`,
    // driven by the avatar path the host (or the guest's `load_avatar`)
    // selects.

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

/// Registers the frame-limit terminator used by the integration tests.
pub fn add_test_terminator(app: &mut App) {
    add_exit_after(app, TEST_FRAMES);
}

/// Registers a terminator that exits the app after `frames` rendered frames.
pub fn add_exit_after(app: &mut App, frames: u32) {
    app.insert_resource(TestFrameLimit(frames));
    app.add_systems(Update, terminate_after_frames);
}