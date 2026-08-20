//! Prototype: load rigged glTF models and drive their skeletal animations with
//! Bevy's built-in animation stack (AnimationGraph + AnimationTransitions),
//! switching clips by velocity like the host's universal avatar walk will.
//!
//! Left of the camera: Bevy's MIT-licensed Fox (Survey/Walk/Run clips) to prove
//! the idle <-> walk <-> run blend machinery. Right: the Khronos CesiumMan
//! (CC-BY 4.0) looping a humanoid walk, to judge human-gait quality.
//!
//! Controls:
//!   - ArrowUp / ArrowDown : raise / lower target speed (idle -> walk -> run)
//!   - Space               : pause / resume
//!   - Enter               : cycle clips manually
//!   - Esc                 : quit

use std::time::Duration;

use bevy::animation::animate_targets;
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::prelude::*;

const FOX: &str = "avatars/fox.glb";
const MAN: &str = "avatars/human.glb";

/// Fox clip node indices (Survey = idle, Walk, Run) inside the shared graph.
#[derive(Resource)]
struct FoxAnimations {
    idle: AnimationNodeIndex,
    walk: AnimationNodeIndex,
    run: AnimationNodeIndex,
    graph: Handle<AnimationGraph>,
}

/// Target locomotion speed (0 = idle, 1..2 = walk, 3 = run). Arrows adjust it.
#[derive(Resource, Default)]
struct TargetSpeed(f32);

#[derive(Component)]
struct Fox;

#[derive(Component)]
struct Humanoid;

/// Added to whichever entity ends up owning the AnimationPlayer, so the
/// speed-driver system can target it directly (the player lands on a scene
/// child, not on the entity that carries `Fox`/`Humanoid`).
#[derive(Component)]
struct FoxPlayer;

#[derive(Component)]
struct HumanoidPlayer;

fn main() {
    App::new()
        .insert_resource(AmbientLight {
            color: Color::WHITE,
            brightness: 400.0,
        })
        .add_plugins(DefaultPlugins
            .set(AssetPlugin {
                file_path: concat!(env!("CARGO_MANIFEST_DIR"), "/assets").to_string(),
                ..default()
            })
            .set(WindowPlugin {
            primary_window: Some(Window {
                title: "Skeletal Animation Prototype".into(),
                resolution: (1280.0, 720.0).into(),
                ..default()
            }),
            ..default()
        }))
        .insert_resource(TargetSpeed::default())
        .add_systems(Startup, setup)
        .add_systems(
            Update,
            (setup_scene_once_loaded.before(animate_targets), control),
        )
        .run();
}

fn setup(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
) {
    // Fox graph: Survey(0) = idle, Walk(1), Run(2) from the glTF file.
    let mut graph = AnimationGraph::new();
    let nodes: Vec<AnimationNodeIndex> = graph
        .add_clips(
            [0, 1, 2]
                .into_iter()
                .map(|i| asset_server.load(GltfAssetLabel::Animation(i).from_asset(FOX))),
            1.0,
            graph.root,
        )
        .collect();
    let graph = graphs.add(graph);
    commands.insert_resource(FoxAnimations {
        idle: nodes[0],
        walk: nodes[1],
        run: nodes[2],
        graph,
    });

    commands.spawn((
        SceneBundle {
            scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(FOX)),
            transform: Transform::from_scale(Vec3::splat(0.015))
                .with_translation(Vec3::new(-2.0, 0.0, 0.0)),
            ..default()
        },
        Fox,
    ));

    commands.spawn((
        SceneBundle {
            scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(MAN)),
            transform: Transform::from_xyz(2.0, 0.57, 0.0),
            ..default()
        },
        Humanoid,
    ));

    commands.spawn(Camera3dBundle {
        camera: Camera {
            clear_color: ClearColorConfig::Custom(Color::srgb(0.10, 0.12, 0.14)),
            ..default()
        },
        tonemapping: Tonemapping::None,
        transform: Transform::from_xyz(0.0, 3.0, 12.0)
            .looking_at(Vec3::new(0.0, 1.2, 0.0), Vec3::Y),
        ..default()
    });

    commands.spawn(DirectionalLightBundle {
        directional_light: DirectionalLight {
            shadows_enabled: true,
            ..default()
        },
        transform: Transform::from_rotation(Quat::from_euler(
            EulerRot::XYZ,
            -0.8,
            0.4,
            0.0,
        )),
        ..default()
    });

    commands.spawn(PbrBundle {
        mesh: meshes.add(Plane3d::default().mesh().size(20.0, 20.0)),
        material: materials.add(Color::srgb(0.28, 0.42, 0.35)),
        ..default()
    });

    println!("--- Skeletal animation prototype ---");
    println!("ArrowUp/Down: speed (idle -> walk -> run)   Space: pause   Enter: cycle");
}

/// Once a scene's skeleton is loaded, Bevy auto-adds an `AnimationPlayer` to
/// the spawned scene. The player may land on a scene child rather than the
/// entity carrying our marker, so walk up the parent chain to classify which
/// scene it belongs to, then attach the matching animation graph + transitions
/// on the player's entity.
fn setup_scene_once_loaded(
    mut commands: Commands,
    animations: Res<FoxAnimations>,
    asset_server: Res<AssetServer>,
    mut graphs: ResMut<Assets<AnimationGraph>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
    markers: Query<(Entity, Option<&Fox>, Option<&Humanoid>)>,
    parents: Query<&Parent>,
) {
    for (entity, mut player) in &mut players {
        let mut cur = entity;
        let mut kind = None;
        let mut depth = 0;
        loop {
            if let Ok((_, fox, human)) = markers.get(cur) {
                if fox.is_some() {
                    kind = Some(0);
                    break;
                }
                if human.is_some() {
                    kind = Some(1);
                    break;
                }
            }
            match parents.get(cur) {
                Ok(parent) => {
                    cur = parent.get();
                    depth += 1;
                }
                Err(_) => break,
            }
        }

        let Some(kind) = kind else {
            println!("[anim] player on {entity:?} has no known scene marker; skipping");
            continue;
        };

        let mut transitions = AnimationTransitions::new();
        if kind == 0 {
            commands.entity(entity).insert((animations.graph.clone(), FoxPlayer));
            transitions
                .play(&mut player, animations.idle, Duration::ZERO)
                .repeat();
            println!("[anim] fox player on {entity:?}: idle attached");
        } else {
            let mut g = AnimationGraph::new();
            let node = g.add_clip(
                asset_server.load(GltfAssetLabel::Animation(0).from_asset(MAN)),
                1.0,
                g.root,
            );
            commands.entity(entity).insert((graphs.add(g), HumanoidPlayer));
            transitions.play(&mut player, node, Duration::ZERO).repeat();
            println!("[anim] humanoid player on {entity:?}: walk attached");
        }
        commands.entity(entity).insert(transitions);
    }
}

fn control(
    keyboard: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    mut target: ResMut<TargetSpeed>,
    animations: Res<FoxAnimations>,
    mut players: Query<
        (&mut AnimationPlayer, &mut AnimationTransitions),
        (With<FoxPlayer>, Without<HumanoidPlayer>),
    >,
) {
    if keyboard.just_pressed(KeyCode::ArrowUp) {
        target.0 = (target.0 + 0.5).min(3.0);
    }
    if keyboard.just_pressed(KeyCode::ArrowDown) {
        target.0 = (target.0 - 0.5).max(0.0);
    }
    if keyboard.just_pressed(KeyCode::Escape) {
        std::process::exit(0);
    }

    let speed = target.0;
    for (mut player, mut transitions) in &mut players {
        let (node, ms) = if speed <= 0.5 {
            (animations.idle, 300)
        } else if speed <= 2.0 {
            (animations.walk, 200)
        } else {
            (animations.run, 200)
        };

        let current = player
            .playing_animations()
            .next()
            .map(|(i, _)| *i);

        if current != Some(node) {
            println!("[anim] speed {speed:.1}: switching to clip node {node:?}");
            transitions
                .play(&mut player, node, Duration::from_millis(ms))
                .repeat();
        }
    }
}