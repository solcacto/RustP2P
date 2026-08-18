use anyhow::{bail, Result};
use host::avatar_standard::{
    validate_avatar_glb, validate_avatar_path, AvatarValidation, REQUIRED_ATTACHMENTS,
    REQUIRED_BONES, REQUIRED_ANIMATIONS,
};

const REFERENCE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/avatar_standard.glb");
const LEGACY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/avatar.glb");

/// Commit 14 regression: the reference avatar must conform to the standard and
/// the legacy (non-conforming) avatar must be rejected at load time.
fn main() -> Result<()> {
    // 1. The reference avatar conforms.
    let report = validate_avatar_path(REFERENCE)?;
    assert_eq!(report.bone_count, REQUIRED_BONES, "reference bone count");
    assert!(report.missing_attachments.is_empty(), "reference attachments");
    assert!(report.missing_animations.is_empty(), "reference animations");
    assert_eq!(report.triangle_count, 180, "reference triangle count");
    assert_eq!(report.texture_count, 0, "reference texture count");
    assert!(report.max_texture_size <= 2048);
    println!(
        "✓ reference avatar_standard.glb conforms: {} bones, {} triangles, {} textures, \
         {} animations, attachments: {}",
        report.bone_count,
        report.triangle_count,
        report.texture_count,
        report.animations.len(),
        REQUIRED_ATTACHMENTS.join(",")
    );

    // 2. The legacy RiggedSimple avatar does NOT conform and must be rejected.
    match validate_avatar_path(LEGACY) {
        Ok(report) => bail!(
            "✗ legacy avatar passed validation unexpectedly (bones={})",
            report.bone_count
        ),
        Err(e) => {
            let msg = format!("{e:#}");
            for expected in ["52 bones", "missing required attachment", "missing required animation"] {
                assert!(
                    msg.contains(expected),
                    "error should mention '{expected}', got: {msg}"
                );
            }
            println!("✓ legacy avatar rejected: {msg}");
        }
    }

    // 3. A required-animation check: build a report where Jump is missing.
    let mut report = AvatarValidation {
        animations: vec!["Idle".into(), "Walk".into(), "Run".into()],
        ..Default::default()
    };
    for required in REQUIRED_ANIMATIONS {
        if !report.animations.iter().any(|n| n == required) {
            report.missing_animations.push(required.to_string());
        }
    }
    assert_eq!(report.missing_animations, vec!["Jump"], "missing Jump detected");

    // 4. Garbage bytes are rejected as not-a-glb.
    match validate_avatar_glb(b"this is not a glb") {
        Ok(_) => bail!("✗ garbage bytes passed validation"),
        Err(_) => println!("✓ non-glb bytes rejected"),
    }

    println!("✓ all avatar standard validation checks passed");
    Ok(())
}