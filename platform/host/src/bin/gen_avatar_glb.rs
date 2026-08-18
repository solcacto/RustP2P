//! Generates `assets/avatar_standard.glb`, the conforming reference avatar for
//! the RustP2P avatar standard (see `docs/AVATAR_STANDARD.md`).
//!
//! The output is a single static-mesh glTF 2.0 binary with:
//! - exactly 52 `mixamorig:`-prefixed skeleton bones,
//! - the 7 required attachment nodes,
//! - a low-poly humanoid mesh (well under 50k triangles, no textures),
//! - the 4 required animations (Idle, Walk, Run, Jump) on the root bone.
//!
//! Usage: `cargo run -p host --bin gen_avatar_glb` (writes the asset in place).

use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;

const OUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/avatar_standard.glb");

/// (name, parent bone index in the 52-bone list, local translation).
/// Parent `-1` means a child of the scene root.
const BONES: [(&str, i32, [f32; 3]); 52] = [
    ("Hips", -1, [0.0, 1.0, 0.0]),
    ("Spine", 0, [0.0, 0.1, 0.0]),
    ("Spine1", 1, [0.0, 0.1, 0.0]),
    ("Spine2", 2, [0.0, 0.1, 0.0]),
    ("Neck", 3, [0.0, 0.18, 0.0]),
    ("Head", 4, [0.0, 0.16, 0.0]),
    ("HeadTop_End", 5, [0.0, 0.14, 0.0]),
    ("LeftShoulder", 3, [-0.22, 0.05, 0.0]),
    ("LeftArm", 7, [-0.22, 0.0, 0.0]),
    ("LeftForeArm", 8, [-0.28, 0.0, 0.0]),
    ("LeftHand", 9, [-0.26, 0.0, 0.0]),
    ("LeftHandIndex1", 10, [-0.08, 0.0, 0.0]),
    ("LeftHandIndex2", 11, [-0.05, 0.0, 0.0]),
    ("LeftHandIndex3", 12, [-0.04, 0.0, 0.0]),
    ("LeftHandMiddle1", 10, [-0.08, 0.0, 0.0]),
    ("LeftHandMiddle2", 14, [-0.05, 0.0, 0.0]),
    ("LeftHandMiddle3", 15, [-0.04, 0.0, 0.0]),
    ("LeftHandPinky1", 10, [-0.07, -0.02, 0.0]),
    ("LeftHandPinky2", 17, [-0.04, 0.0, 0.0]),
    ("LeftHandPinky3", 18, [-0.04, 0.0, 0.0]),
    ("LeftHandRing1", 10, [-0.07, 0.0, -0.02]),
    ("LeftHandRing2", 20, [-0.04, 0.0, 0.0]),
    ("LeftHandRing3", 21, [-0.04, 0.0, 0.0]),
    ("LeftHandThumb1", 10, [-0.03, 0.0, -0.05]),
    ("LeftHandThumb2", 23, [-0.03, 0.0, 0.0]),
    ("LeftHandThumb3", 24, [-0.03, 0.0, 0.0]),
    ("LeftUpLeg", 0, [-0.12, -0.1, 0.0]),
    ("LeftLeg", 26, [0.0, -0.28, 0.0]),
    ("LeftFoot", 27, [0.0, -0.28, 0.0]),
    ("LeftToeBase", 28, [0.0, -0.05, 0.1]),
    ("RightShoulder", 3, [0.22, 0.05, 0.0]),
    ("RightArm", 30, [0.22, 0.0, 0.0]),
    ("RightForeArm", 31, [0.28, 0.0, 0.0]),
    ("RightHand", 32, [0.26, 0.0, 0.0]),
    ("RightHandIndex1", 33, [0.08, 0.0, 0.0]),
    ("RightHandIndex2", 34, [0.05, 0.0, 0.0]),
    ("RightHandIndex3", 35, [0.04, 0.0, 0.0]),
    ("RightHandMiddle1", 33, [0.08, 0.0, 0.0]),
    ("RightHandMiddle2", 37, [0.05, 0.0, 0.0]),
    ("RightHandMiddle3", 38, [0.04, 0.0, 0.0]),
    ("RightHandPinky1", 33, [0.07, -0.02, 0.0]),
    ("RightHandPinky2", 40, [0.04, 0.0, 0.0]),
    ("RightHandPinky3", 41, [0.04, 0.0, 0.0]),
    ("RightHandRing1", 33, [0.07, 0.0, -0.02]),
    ("RightHandRing2", 43, [0.04, 0.0, 0.0]),
    ("RightHandRing3", 44, [0.04, 0.0, 0.0]),
    ("RightHandThumb1", 33, [0.03, 0.0, -0.05]),
    ("RightHandThumb2", 46, [0.03, 0.0, 0.0]),
    ("RightHandThumb3", 47, [0.03, 0.0, 0.0]),
    ("RightUpLeg", 0, [0.12, -0.1, 0.0]),
    ("RightLeg", 49, [0.0, -0.28, 0.0]),
    ("RightFoot", 50, [0.0, -0.28, 0.0]),
];

/// The 7 required attachment nodes (name, local translation).
const ATTACHMENTS: [(&str, [f32; 3]); 7] = [
    ("Head", [0.0, 1.95, 0.0]),
    ("Chest", [0.0, 1.5, 0.0]),
    ("LeftHand", [-1.22, 1.6, 0.0]),
    ("RightHand", [1.22, 1.6, 0.0]),
    ("Back", [0.0, 1.2, -0.22]),
    ("LeftFoot", [-0.12, 0.06, 0.03]),
    ("RightFoot", [0.12, 0.06, 0.03]),
];

/// (center, size) of each box making up the low-poly humanoid body.
const BODY: [([f32; 3], [f32; 3]); 15] = [
    ([0.0, 1.0, 0.0], [0.3, 0.2, 0.2]),     // pelvis
    ([0.0, 1.45, 0.0], [0.36, 0.7, 0.22]), // torso
    ([0.0, 1.98, 0.0], [0.24, 0.28, 0.24]), // head
    ([-0.55, 1.6, 0.0], [0.55, 0.13, 0.13]), // left upper arm
    ([-1.0, 1.6, 0.0], [0.45, 0.11, 0.11]), // left forearm
    ([-1.22, 1.6, 0.0], [0.2, 0.1, 0.1]),  // left hand
    ([0.55, 1.6, 0.0], [0.55, 0.13, 0.13]), // right upper arm
    ([1.0, 1.6, 0.0], [0.45, 0.11, 0.11]), // right forearm
    ([1.22, 1.6, 0.0], [0.2, 0.1, 0.1]),   // right hand
    ([-0.12, 0.72, 0.0], [0.14, 0.55, 0.16]), // left thigh
    ([-0.12, 0.32, 0.0], [0.11, 0.4, 0.12]), // left calf
    ([-0.12, 0.06, 0.03], [0.12, 0.1, 0.22]), // left foot
    ([0.12, 0.72, 0.0], [0.14, 0.55, 0.16]), // right thigh
    ([0.12, 0.32, 0.0], [0.11, 0.4, 0.12]), // right calf
    ([0.12, 0.06, 0.03], [0.12, 0.1, 0.22]), // right foot
];

fn main() -> Result<()> {
    assert_eq!(BONES.len(), 52, "skeleton must have exactly 52 bones");

    // CLI: --out <path> (default the reference asset) and --color r,g,b (0..1).
    let args: Vec<String> = std::env::args().collect();
    let out_path = args
        .iter()
        .position(|a| a == "--out")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| OUT.to_string());
    let color: [f32; 3] = args
        .iter()
        .position(|a| a == "--color")
        .and_then(|i| args.get(i + 1))
        .map(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|x| x.parse().ok()).collect();
            [v.first().copied().unwrap_or(0.72), v.get(1).copied().unwrap_or(0.72), v.get(2).copied().unwrap_or(0.78)]
        })
        .unwrap_or([0.72, 0.72, 0.78]);

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut min_p = [f32::MAX; 3];
    let mut max_p = [f32::MIN; 3];

    for (center, size) in BODY {
        let (p, n) = box_geometry(center, size);
        for v in &p {
            for i in 0..3 {
                min_p[i] = min_p[i].min(v[i]);
                max_p[i] = max_p[i].max(v[i]);
            }
        }
        positions.extend(p);
        normals.extend(n);
    }

    let triangles = positions.len() / 3;
    assert!(triangles < 50_000, "reference mesh must stay under 50k triangles");

    // ---- build the binary chunk ----
    let mut bin = Vec::new();
    let pos_offset = 0usize;
    append_f32s(&mut bin, &positions);
    let norm_offset = bin.len();
    append_f32s(&mut bin, &normals);

    // Animation data: one shared time track + one translation track per clip.
    let times: [f32; 3] = [0.0, 0.5, 1.0];
    let clips: [(&str, [[f32; 3]; 3]); 4] = [
        ("Idle", [[0.0, 0.0, 0.0], [0.0, 0.02, 0.0], [0.0, 0.0, 0.0]]),
        ("Walk", [[0.0, 0.0, 0.0], [-0.15, 0.05, 0.0], [0.0, 0.0, 0.0]]),
        ("Run", [[0.0, 0.0, 0.0], [-0.3, 0.1, 0.0], [0.0, 0.0, 0.0]]),
        ("Jump", [[0.0, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.0]]),
    ];
    let times_offset = bin.len();
    for t in &times {
        bin.extend_from_slice(&t.to_le_bytes());
    }
    let mut clip_offsets = [0usize; 4];
    for (i, (_, frames)) in clips.iter().enumerate() {
        clip_offsets[i] = bin.len();
        for frame in frames {
            for c in frame {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
    }
    pad4(&mut bin);

    // ---- build the JSON chunk ----
    let positions_count = positions.len() as u32;
    let normals_count = normals.len() as u32;

    let mut nodes: Vec<Value> = Vec::new();
    // node 0: scene root
    let mut root_children = vec![1u32, 2]; // mesh node, Hips bone
    for a in 0..ATTACHMENTS.len() {
        root_children.push((54 + a) as u32);
    }
    nodes.push(json!({
        "name": "AvatarRoot",
        "children": root_children,
    }));
    // node 1: mesh
    nodes.push(json!({
        "name": "Mesh",
        "mesh": 0,
    }));
    // nodes 2..=53: the 52 bones
    let mut bone_children: Vec<Vec<u32>> = vec![Vec::new(); BONES.len()];
    for (i, (_, parent, _)) in BONES.iter().enumerate() {
        if *parent >= 0 {
            bone_children[*parent as usize].push(2 + i as u32);
        }
    }
    for (i, (name, _parent, translation)) in BONES.iter().enumerate() {
        let mut node = json!({
            "name": format!("mixamorig:{name}"),
            "translation": translation,
        });
        if !bone_children[i].is_empty() {
            node["children"] = json!(bone_children[i]);
        }
        nodes.push(node);
    }
    // nodes 54..=60: attachment points
    for (name, translation) in ATTACHMENTS {
        nodes.push(json!({
            "name": name,
            "translation": translation,
        }));
    }

    // Buffer views / accessors.
    let pos_bytes = positions_count * 3 * 4;
    let norm_bytes = normals_count * 3 * 4;
    let times_bytes = 12u32;
    let clip_bytes = 48u32;

    let mut buffer_views = vec![
        json!({"buffer": 0, "byteOffset": pos_offset, "byteLength": pos_bytes, "target": 34962}),
        json!({"buffer": 0, "byteOffset": norm_offset, "byteLength": norm_bytes, "target": 34962}),
        json!({"buffer": 0, "byteOffset": times_offset, "byteLength": times_bytes}),
    ];
    for (i, _) in clips.iter().enumerate() {
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": clip_offsets[i],
            "byteLength": clip_bytes,
        }));
    }

    let mut accessors = vec![json!({
        "bufferView": 0, "componentType": 5126, "count": positions_count,
        "type": "VEC3",
        "min": min_p, "max": max_p,
    })];
    accessors.push(json!({
        "bufferView": 1, "componentType": 5126, "count": normals_count, "type": "VEC3",
    }));
    accessors.push(json!({
        "bufferView": 2, "componentType": 5126, "count": 3, "type": "SCALAR",
        "min": [0.0], "max": [1.0],
    }));
    for i in 0..4 {
        accessors.push(json!({
            "bufferView": 3 + i, "componentType": 5126, "count": 3, "type": "VEC3",
        }));
    }

    let animations: Vec<Value> = clips
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            json!({
                "name": name,
                "samplers": [{"input": 2, "output": 3 + i}],
                "channels": [{"sampler": 0, "target": {"node": 2, "path": "translation"}}],
            })
        })
        .collect();

    let doc = json!({
        "asset": {"version": "2.0", "generator": "RustP2P avatar generator"},
        "scene": 0,
        "scenes": [{"name": "Scene", "nodes": [0]}],
        "nodes": nodes,
        "meshes": [{
            "name": "avatar_standard",
            "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1},
                "material": 0,
                "mode": 4,
            }],
        }],
        "materials": [{
            "name": "avatar_body",
            "pbrMetallicRoughness": {
                "baseColorFactor": [color[0], color[1], color[2], 1.0],
                "metallicFactor": 0.0,
                "roughnessFactor": 0.85,
            },
        }],
        "buffers": [{"byteLength": bin.len() as u32}],
        "bufferViews": buffer_views,
        "accessors": accessors,
        "animations": animations,
    });

    // ---- assemble the glb container ----
    let json_bytes = serde_json::to_vec(&doc)?;
    let json_padded = pad_to4(json_bytes);
    let bin_padded = {
        let mut b = bin.clone();
        pad4(&mut b);
        b
    };
    let total = 12 + 8 + json_padded.len() + 8 + bin_padded.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_padded);
    out.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin_padded);

    let path = {
        let raw = PathBuf::from(&out_path);
        if raw.is_absolute() {
            raw
        } else {
            // Resolve relative paths against the host crate so the asset lands
            // next to the other avatar assets regardless of the working dir.
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(raw)
        }
    };
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&path, &out)?;
    println!(
        "wrote {} ({triangles} triangles, {} bones, {} animations, {} bytes)",
        path.display(),
        BONES.len(),
        clips.len(),
        out.len()
    );
    Ok(())
}

/// Produces non-indexed triangle geometry for an axis-aligned box.
///
/// Each face is a quad split into two triangles. Triangles are ordered
/// counter-clockwise when viewed from outside (so the front face points along
/// the outward normal); a winding check fixes any that aren't.
fn box_geometry(center: [f32; 3], size: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let [cx, cy, cz] = center;
    let (hw, hh, hd) = (size[0] / 2.0, size[1] / 2.0, size[2] / 2.0);
    // 6 faces: (normal, 4 corners in CCW order when viewed from the outside)
    let faces: [([f32; 3], [[f32; 3]; 4]); 6] = [
        ([1.0, 0.0, 0.0], [[cx + hw, cy - hh, cz - hd], [cx + hw, cy - hh, cz + hd], [cx + hw, cy + hh, cz + hd], [cx + hw, cy + hh, cz - hd]]),
        ([-1.0, 0.0, 0.0], [[cx - hw, cy - hh, cz + hd], [cx - hw, cy - hh, cz - hd], [cx - hw, cy + hh, cz - hd], [cx - hw, cy + hh, cz + hd]]),
        ([0.0, 1.0, 0.0], [[cx - hw, cy + hh, cz - hd], [cx - hw, cy + hh, cz + hd], [cx + hw, cy + hh, cz + hd], [cx + hw, cy + hh, cz - hd]]),
        ([0.0, -1.0, 0.0], [[cx - hw, cy - hh, cz + hd], [cx - hw, cy - hh, cz - hd], [cx + hw, cy - hh, cz - hd], [cx + hw, cy - hh, cz + hd]]),
        ([0.0, 0.0, 1.0], [[cx - hw, cy - hh, cz + hd], [cx + hw, cy - hh, cz + hd], [cx + hw, cy + hh, cz + hd], [cx - hw, cy + hh, cz + hd]]),
        ([0.0, 0.0, -1.0], [[cx + hw, cy - hh, cz - hd], [cx - hw, cy - hh, cz - hd], [cx - hw, cy + hh, cz - hd], [cx + hw, cy + hh, cz - hd]]),
    ];
    let mut positions = Vec::new();
    let mut normals = Vec::new();
    for (n, corners) in faces {
        // Two triangles per quad: (0,1,2) and (0,2,3).
        let mut tris = [[corners[0], corners[1], corners[2]], [corners[0], corners[2], corners[3]]];
        // Ensure CCW winding as seen from the outward normal.
        for tri in &mut tris {
            let u = [tri[1][0] - tri[0][0], tri[1][1] - tri[0][1], tri[1][2] - tri[0][2]];
            let v = [tri[2][0] - tri[0][0], tri[2][1] - tri[0][1], tri[2][2] - tri[0][2]];
            let cross = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let dot = cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2];
            if dot < 0.0 {
                tri.swap(1, 2);
            }
        }
        for tri in tris {
            positions.push(tri[0]);
            positions.push(tri[1]);
            positions.push(tri[2]);
            normals.push(n);
            normals.push(n);
            normals.push(n);
        }
    }
    (positions, normals)
}

fn append_f32s(out: &mut Vec<u8>, vals: &[[f32; 3]]) {
    for v in vals {
        for c in v {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
}

fn pad4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn pad_to4(mut buf: Vec<u8>) -> Vec<u8> {
    while !buf.len().is_multiple_of(4) {
        buf.push(b' ');
    }
    buf
}