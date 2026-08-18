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
use crate::host_state::HostState;
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
            update_score_display,
        )
            .chain(),
    );
    app
}

/// Runs one Wasm `render_tick` per frame. First the network is polled so any
/// remote avatar poses that arrived are registered in the shared map, then the
/// guest computes a new pose and pushes it through `update_avatar_transform`
/// into the shared handle.
fn wasm_render_tick(runtime: Res<WasmRuntime>) {
    let mut guard = runtime.store.lock().unwrap();
    let store = &mut *guard;
    store.data_mut().update_frame();
    poll_network(store);
    if let Err(err) = runtime.render_tick.call(store, ()) {
        panic!("wasm render_tick failed: {err}");
    }
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

/// Updates the on-screen score text from the host's local tag score and the
/// sum of every remote peer's score.
fn update_score_display(runtime: Res<WasmRuntime>, mut query: Query<&mut Text, With<ScoreText>>) {
    let Ok(mut text) = query.get_single_mut() else {
        return;
    };
    let store = runtime.store.lock().unwrap();
    let local = store.data().tag_score();
    let remote_total: u32 = store.data().remote_scores().lock().unwrap().values().sum();
    let label = format!("Local Score: {local} | Remote Score: {remote_total}");
    if text.sections[0].value != label {
        text.sections[0].value = label;
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

    let avatar = asset_server.load(GltfAssetLabel::Scene(0).from_asset("avatar.glb"));
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