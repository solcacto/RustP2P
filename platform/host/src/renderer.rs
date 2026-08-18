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
    app.add_systems(Startup, setup_scene);
    app.add_systems(
        Update,
        (
            read_keyboard,
            wasm_render_tick,
            apply_avatar_pose,
            sync_remote_avatars,
            tint_remote_avatars,
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

    let label = format!(
        "Local Score: {local} | Remote Score: {remote_total}\n{peer_line}\n{sig_line}\n{wasm_line}"
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
/// payloads are tag scores broadcast by the remote guest.
fn poll_network(store: &mut wasmtime::Store<HostState>) {
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
    let now = Instant::now();
    for (addr, payload) in incoming {
        let peer_id = match store
            .data()
            .peer_connection()
            .and_then(|pc| pc.peer_id_for_addr(&addr).cloned())
        {
            Some(id) => id,
            None => continue,
        };
        match payload.len() {
            16 => {
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
                let score = u32::from_le_bytes(payload[0..4].try_into().unwrap());
                scores.insert(peer_id, score);
            }
            _ => {}
        }
    }
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

/// Spawns, updates, and despawns remote avatar entities from the shared pose
/// map: one glb clone per remote peer, tinted blue, mirroring their latest pose
/// and removed if it stops reporting for more than 2 seconds.
fn sync_remote_avatars(
    handle: Res<RemoteAvatarsHandle>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut avatars: Query<(Entity, &RemotePeerId, &mut Transform)>,
) {
    let now = Instant::now();
    let map = handle.0.lock().unwrap();

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

    let scene = asset_server.load(GltfAssetLabel::Scene(0).from_asset("avatar.glb"));
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
    asset_server: Res<AssetServer>,
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

    let avatar = asset_server.load(GltfAssetLabel::Scene(0).from_asset("avatar_standard.glb"));
    commands.spawn((SceneBundle {
        scene: avatar,
        ..default()
    }, Avatar));

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