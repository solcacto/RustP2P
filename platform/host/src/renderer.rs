use crate::avatar_state::AvatarState;
use crate::host_state::HostState;
use bevy::app::AppExit;
use bevy::core::{FrameCount, TaskPoolPlugin, TypeRegistrationPlugin};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::core_pipeline::CorePipelinePlugin;
use bevy::gltf::GltfAssetLabel;
use bevy::log::LogPlugin;
use bevy::pbr::{DirectionalLightBundle, PbrPlugin, StandardMaterial};
use bevy::prelude::*;
use bevy::render::pipelined_rendering::PipelinedRenderingPlugin;
use bevy::render::RenderPlugin;
use bevy::scene::ScenePlugin;
use bevy::transform::TransformPlugin;
use bevy::window::WindowPlugin;
use bevy::winit::{UpdateMode, WakeUp, WinitPlugin, WinitSettings};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wasmtime::TypedFunc;

/// Frames per second the renderer tries to maintain.
pub const TARGET_FPS: u64 = 60;

/// Frames to render before the integration test self-terminates.
pub const TEST_FRAMES: u32 = 180;

/// Shared pose target bridging the Wasm guest and the Bevy scene graph.
#[derive(Resource, Clone)]
pub struct AvatarStateHandle(pub Arc<Mutex<AvatarState>>);

/// Marker identifying the loaded Universal Avatar scene root.
#[derive(Component)]
pub struct Avatar;

/// Holds the instantiated Wasm render module so a Bevy `Update` system can
/// drive exactly one guest tick per rendered frame.
#[derive(Resource)]
pub struct WasmRuntime {
    pub store: Arc<Mutex<wasmtime::Store<HostState>>>,
    pub render_tick: TypedFunc<(), ()>,
}

/// Builds a Bevy app configured purely as a 3D rendering/scene-graph backend.
///
/// Bevy 0.14's winit runner owns the main loop, so the Wasm game loop is
/// registered as the `wasm_render_tick` Update system: each rendered frame
/// runs one guest tick followed by the avatar pose update. Everything stays
/// on the main thread.
pub fn build_app(avatar_state: Arc<Mutex<AvatarState>>) -> App {
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
    ));
    app.insert_resource(WinitSettings {
        focused_mode: UpdateMode::reactive(Duration::from_millis(1000 / TARGET_FPS)),
        unfocused_mode: UpdateMode::reactive_low_power(Duration::from_millis(1000 / TARGET_FPS)),
    });
    app.insert_resource(AvatarStateHandle(avatar_state));
    app.add_systems(Startup, setup_scene);
    app.add_systems(Update, (wasm_render_tick, apply_avatar_pose).chain());
    app
}

/// Runs one Wasm `render_tick` per frame. The guest computes a new avatar pose
/// and pushes it through `update_avatar_transform` into the shared handle.
fn wasm_render_tick(runtime: Res<WasmRuntime>) {
    let mut guard = runtime.store.lock().unwrap();
    let store = &mut *guard;
    store.data_mut().update_frame();
    if let Err(err) = runtime.render_tick.call(store, ()) {
        panic!("wasm render_tick failed: {err}");
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

/// Self-terminates the integration test once enough frames have rendered.
fn terminate_after_frames(frames: Res<FrameCount>, mut exit: EventWriter<AppExit>) {
    if frames.0 >= TEST_FRAMES {
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
}

/// Registers the frame-limit terminator used by the integration test.
pub fn add_test_terminator(app: &mut App) {
    app.add_systems(Update, terminate_after_frames);
}