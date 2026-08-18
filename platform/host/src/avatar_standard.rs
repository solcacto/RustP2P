//! Validation of avatar assets against the RustP2P avatar standard.
//!
//! Every game renders the same avatar skeleton, so the host must reject any
//! `.glb` that does not conform before it is loaded (see
//! `docs/AVATAR_STANDARD.md`). Validation checks the skeleton bone count,
//! the required attachment points, the triangle budget, the texture budget and
//! dimensions, and the required animation clips.
//!
//! A conforming avatar must:
//! - be a valid glTF 2.0 binary (`.glb`),
//! - contain exactly [`REQUIRED_BONES`] bones, named `mixamorig:*`,
//! - expose every [`REQUIRED_ATTACHMENTS`] node,
//! - have at most [`MAX_TRIANGLES`] triangles across all meshes,
//! - use at most [`MAX_TEXTURES`] textures, none larger than
//!   [`MAX_TEXTURE_SIZE`] on either axis,
//! - contain every [`REQUIRED_ANIMATIONS`] clip.

use anyhow::{bail, Context, Result};
use gltf::mesh::Semantic;
use std::path::Path;

/// Exact number of skeleton bones (Mixamo-compatible naming).
pub const REQUIRED_BONES: usize = 52;
/// Attachment point node names every avatar must expose.
pub const REQUIRED_ATTACHMENTS: [&str; 7] = [
    "Head",
    "Chest",
    "LeftHand",
    "RightHand",
    "Back",
    "LeftFoot",
    "RightFoot",
];
/// Animation clips every avatar must ship.
pub const REQUIRED_ANIMATIONS: [&str; 4] = ["Idle", "Walk", "Run", "Jump"];
/// Maximum triangle count across all meshes.
pub const MAX_TRIANGLES: usize = 50_000;
/// Maximum number of textures.
pub const MAX_TEXTURES: usize = 4;
/// Maximum texture dimension (width or height), in pixels.
pub const MAX_TEXTURE_SIZE: u32 = 2048;

/// Breakdown of a validated avatar, produced by [`validate_avatar_glb`].
#[derive(Debug, Clone, Default)]
pub struct AvatarValidation {
    /// Number of `mixamorig:`-prefixed bones found.
    pub bone_count: usize,
    /// Required attachment nodes that were missing.
    pub missing_attachments: Vec<String>,
    /// Total triangle count across all meshes.
    pub triangle_count: usize,
    /// Number of textures used.
    pub texture_count: usize,
    /// Largest texture dimension seen, in pixels.
    pub max_texture_size: u32,
    /// Animation clip names found in the asset.
    pub animations: Vec<String>,
    /// Required animation clips that were missing.
    pub missing_animations: Vec<String>,
}

/// Validates a `.glb` asset from raw bytes against the avatar standard.
///
/// Returns `Ok(AvatarValidation)` for a conforming avatar, or an error
/// describing every rule that was violated.
pub fn validate_avatar_glb(bytes: &[u8]) -> Result<AvatarValidation> {
    let gltf = gltf::Gltf::from_slice(bytes)
        .context("not a valid glTF 2.0 binary asset")?;

    let mut report = AvatarValidation::default();
    let mut problems: Vec<String> = Vec::new();

    // --- skeleton bones (mixamorig:* nodes) ---
    report.bone_count = gltf
        .nodes()
        .filter(|n| n.name().is_some_and(|s| s.starts_with("mixamorig:")))
        .count();
    if report.bone_count != REQUIRED_BONES {
        problems.push(format!(
            "skeleton must have exactly {REQUIRED_BONES} bones (mixamorig:*), found {}",
            report.bone_count
        ));
    }

    // --- attachment points ---
    let present: Vec<String> = gltf.nodes().filter_map(|n| n.name().map(str::to_string)).collect();
    for required in REQUIRED_ATTACHMENTS {
        if !present.iter().any(|n| n == required) {
            report.missing_attachments.push(required.to_string());
        }
    }
    if !report.missing_attachments.is_empty() {
        problems.push(format!(
            "missing required attachment point(s): {}",
            report.missing_attachments.join(", ")
        ));
    }

    // --- triangle budget ---
    for mesh in gltf.meshes() {
        for primitive in mesh.primitives() {
            let triangles = match primitive.indices() {
                Some(indices) => indices.count() / 3,
                None => primitive
                    .get(&Semantic::Positions)
                    .map(|a| a.count() / 3)
                    .unwrap_or(0),
            };
            report.triangle_count += triangles;
        }
    }
    if report.triangle_count > MAX_TRIANGLES {
        problems.push(format!(
            "avatar exceeds the {MAX_TRIANGLES}-triangle budget ({})",
            report.triangle_count
        ));
    }

    // --- texture budget & dimensions ---
    report.texture_count = gltf.textures().count();
    if report.texture_count > MAX_TEXTURES {
        problems.push(format!(
            "avatar exceeds the {MAX_TEXTURES}-texture budget ({})",
            report.texture_count
        ));
    }
    for texture in gltf.textures() {
        let image = texture.source();
        let image_name = image.name().unwrap_or("?").to_string();
        let bytes = match image_bytes(&gltf, image) {
            Ok(bytes) => bytes,
            Err(e) => {
                problems.push(format!("texture '{image_name}': {e}"));
                continue;
            }
        };
        let dim = match image::load_from_memory(&bytes) {
            Ok(decoded) => decoded.width().max(decoded.height()),
            Err(_) => {
                problems.push(format!("texture '{image_name}' is not a decodable image"));
                continue;
            }
        };
        report.max_texture_size = report.max_texture_size.max(dim);
        if dim > MAX_TEXTURE_SIZE {
            problems.push(format!(
                "texture '{image_name}' is {dim}px on its longest axis; max is {MAX_TEXTURE_SIZE}px"
            ));
        }
    }

    // --- animations ---
    report.animations = gltf
        .animations()
        .filter_map(|a| a.name().map(str::to_string))
        .collect();
    for required in REQUIRED_ANIMATIONS {
        if !report.animations.iter().any(|n| n == required) {
            report.missing_animations.push(required.to_string());
        }
    }
    if !report.missing_animations.is_empty() {
        problems.push(format!(
            "missing required animation clip(s): {} (found: {})",
            report.missing_animations.join(", "),
            if report.animations.is_empty() {
                "none".to_string()
            } else {
                report.animations.join(", ")
            }
        ));
    }

    if !problems.is_empty() {
        bail!("avatar does not conform to the standard:\n  - {}", problems.join("\n  - "));
    }

    Ok(report)
}

/// Validates a `.glb` file on disk against the avatar standard.
pub fn validate_avatar_path(path: impl AsRef<Path>) -> Result<AvatarValidation> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read avatar asset at {}", path.display()))?;
    validate_avatar_glb(&bytes)
}

/// Extracts a texture's encoded image bytes from the glb binary chunk.
fn image_bytes<'a>(gltf: &gltf::Gltf, image: gltf::image::Image<'a>) -> Result<Vec<u8>> {
    let source = image.source();
    if let gltf::image::Source::View { view, mime_type } = source {
        let blob = gltf
            .blob
            .as_deref()
            .context("glb has no binary chunk for embedded texture")?;
        let start = view.offset();
        let end = start + view.length();
        if end > blob.len() {
            bail!("texture buffer view out of bounds");
        }
        let _ = mime_type;
        Ok(blob[start..end].to_vec())
    } else {
        bail!(
            "texture '{}' uses an external URI, which is not allowed in a conforming avatar",
            image.name().unwrap_or("?")
        )
    }
}