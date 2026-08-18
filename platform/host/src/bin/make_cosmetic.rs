//! Creates a signed cosmetic package under the host asset folder.
//!
//! Generates the mesh `.glb`, signs the canonical message
//! (`item_id|attachment_point|sha256(mesh)`) with the platform dev key, and
//! writes both the mesh and its `cosmetic_manifest.json`.
//!
//! Usage:
//! ```sh
//! cargo run -p host --bin make_cosmetic -- \
//!   --kind sword --item golden_sword_v1 --point RightHand \
//!   --out assets/cosmetics/golden_sword
//! ```

use anyhow::Result;
use ed25519_dalek::SigningKey;
use host::cosmetic::{signed_message, sign_message};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Fixed dev signing key (RFC 8032 test vector #1). All shipped reference
/// cosmetics are signed with this key.
const DEV_SECRET_HEX: &str = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let kind = get("--kind").unwrap_or_else(|| "sword".to_string());
    let item_id = get("--item").unwrap_or_else(|| "cosmetic_v1".to_string());
    let attachment_point = get("--point").unwrap_or_else(|| "RightHand".to_string());
    let out_dir = get("--out").unwrap_or_else(|| "assets/cosmetics/dev".to_string());

    let (mesh_file, boxes) = match kind.as_str() {
        "sword" => (
            "sword.glb",
            vec![
                ([0.0, 0.62, 0.0], [0.06, 1.1, 0.06]),  // blade
                ([0.0, 0.08, 0.0], [0.16, 0.05, 0.06]), // guard
                ([0.0, -0.06, 0.0], [0.05, 0.22, 0.05]), // grip
            ],
        ),
        "hat" => (
            "hat.glb",
            vec![
                ([0.0, 0.15, 0.0], [0.2, 0.14, 0.2]),  // crown
                ([0.0, 0.05, 0.0], [0.34, 0.03, 0.34]), // brim
            ],
        ),
        _ => anyhow::bail!("unknown --kind '{kind}' (use sword or hat)"),
    };

    let color: [f32; 3] = match kind.as_str() {
        "sword" => [0.85, 0.85, 0.9],
        _ => [0.8, 0.25, 0.2],
    };

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut min_p = [f32::MAX; 3];
    let mut max_p = [f32::MIN; 3];
    for (center, size) in &boxes {
        let (p, n) = box_geometry(*center, *size);
        for v in &p {
            for i in 0..3 {
                min_p[i] = min_p[i].min(v[i]);
                max_p[i] = max_p[i].max(v[i]);
            }
        }
        positions.extend(p);
        normals.extend(n);
    }

    let mut bin = Vec::new();
    append_f32s(&mut bin, &positions);
    let norm_offset = bin.len();
    append_f32s(&mut bin, &normals);
    pad4(&mut bin);

    let doc = json!({
        "asset": {"version": "2.0", "generator": "RustP2P cosmetic generator"},
        "scene": 0,
        "scenes": [{"name": "Scene", "nodes": [0]}],
        "nodes": [
            {"name": "Root", "children": [1]},
            {"name": "Mesh", "mesh": 0}
        ],
        "meshes": [{
            "name": mesh_file,
            "primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1},
                "material": 0,
                "mode": 4,
            }],
        }],
        "materials": [{
            "name": "cosmetic",
            "pbrMetallicRoughness": {
                "baseColorFactor": [color[0], color[1], color[2], 1.0],
                "metallicFactor": 0.3,
                "roughnessFactor": 0.5,
            },
        }],
        "buffers": [{"byteLength": bin.len() as u32}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": positions.len() as u32 * 12, "target": 34962},
            {"buffer": 0, "byteOffset": norm_offset as u32, "byteLength": normals.len() as u32 * 12, "target": 34962},
        ],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": positions.len() as u32, "type": "VEC3", "min": min_p, "max": max_p},
            {"bufferView": 1, "componentType": 5126, "count": normals.len() as u32, "type": "VEC3"},
        ],
    });

    let json_bytes = serde_json::to_vec(&doc)?;
    let json_padded = pad_to4(json_bytes);
    let bin_padded = {
        let mut b = bin.clone();
        pad4(&mut b);
        b
    };
    let total = 12 + 8 + json_padded.len() + 8 + bin_padded.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_padded);
    glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin_padded);

    let mesh_sha = hex::encode(Sha256::digest(&glb));
    let message = signed_message(&item_id, &attachment_point, &mesh_sha);
    let secret: [u8; 32] = hex::decode(DEV_SECRET_HEX)?.try_into().expect("dev key must be 32 bytes");
    let signing_key = SigningKey::from_bytes(&secret);
    let pubkey_hex = hex::encode(signing_key.verifying_key().to_bytes());
    let signature = sign_message(&message, &signing_key);

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&out_dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(mesh_file), &glb)?;

    let manifest = json!({
        "item_id": item_id,
        "mesh": mesh_file,
        "attachment_point": attachment_point,
        "creator_pubkey": format!("ed25519:{pubkey_hex}"),
        "signature": signature,
    });
    std::fs::write(dir.join("cosmetic_manifest.json"), serde_json::to_string_pretty(&manifest)?)?;
    println!("wrote signed cosmetic package to {}/ ({} triangles)", dir.display(), positions.len() / 3);
    println!("  pubkey: ed25519:{pubkey_hex}");
    Ok(())
}

/// Reuses the box geometry generator (non-indexed triangles + normals).
fn box_geometry(center: [f32; 3], size: [f32; 3]) -> (Vec<[f32; 3]>, Vec<[f32; 3]>) {
    let [cx, cy, cz] = center;
    let (hw, hh, hd) = (size[0] / 2.0, size[1] / 2.0, size[2] / 2.0);
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
        let mut tris = [[corners[0], corners[1], corners[2]], [corners[0], corners[2], corners[3]]];
        for tri in &mut tris {
            let u = [tri[1][0] - tri[0][0], tri[1][1] - tri[0][1], tri[1][2] - tri[0][2]];
            let v = [tri[2][0] - tri[0][0], tri[2][1] - tri[0][1], tri[2][2] - tri[0][2]];
            let cross = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            if cross[0] * n[0] + cross[1] * n[1] + cross[2] * n[2] < 0.0 {
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