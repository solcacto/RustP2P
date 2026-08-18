use anyhow::{bail, Result};
use host::{avatar_standard, host_functions, host_state::HostState};
use wasmtime::{Engine, Linker, Module, Store};

const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Commit 15 regression: the guest can select an avatar through `load_avatar`,
/// but only from validated `.glb` assets inside the host asset folder.
fn main() -> Result<()> {
    // 0. Both test avatars conform to the standard (same animation set).
    for path in ["avatar_standard.glb", "avatars/blue.glb"] {
        let report = avatar_standard::validate_avatar_path(format!("{ASSETS_DIR}/{path}"))?;
        assert_eq!(report.animations.len(), 4, "avatar '{path}' must ship 4 animations");
        println!(
            "✓ avatar '{path}' conforms: {} bones, {} triangles, animations: {}",
            report.bone_count,
            report.triangle_count,
            report.animations.join(",")
        );
    }

    let engine = Engine::default();
    let mut linker = Linker::new(&engine);
    host_functions::register(&mut linker)?;
    let wat = include_str!("../test_modules/load_avatar_test.wat");
    let module = Module::new(&engine, wat)?;
    let mut store = Store::new(&engine, HostState::new("avatar-test"));

    let instance = linker.instantiate(&mut store, &module)?;
    let try_valid = instance.get_typed_func::<(), i32>(&mut store, "try_valid")?;
    let try_traversal = instance.get_typed_func::<(), i32>(&mut store, "try_traversal")?;
    let try_non_glb = instance.get_typed_func::<(), i32>(&mut store, "try_non_glb")?;

    // 1. Valid avatar path -> load succeeds and updates host state.
    let r = try_valid.call(&mut store, ())?;
    if r != 1 {
        bail!("✗ load_avatar rejected a valid avatar path (got {r})");
    }
    if store.data().avatar_path() != "avatars/blue.glb" {
        bail!("✗ avatar_path was not updated after a successful load");
    }
    println!("✓ load_avatar loaded 'avatars/blue.glb' and updated host state");

    // 2. Path traversal -> refused, state unchanged.
    let r = try_traversal.call(&mut store, ())?;
    if r != 0 {
        bail!("✗ load_avatar accepted a path-traversal attempt (got {r})");
    }
    if store.data().avatar_path() != "avatars/blue.glb" {
        bail!("✗ failed load corrupted avatar_path");
    }
    println!("✓ path-traversal avatar path refused");

    // 3. Non-glb file -> refused.
    let r = try_non_glb.call(&mut store, ())?;
    if r != 0 {
        bail!("✗ load_avatar accepted a non-glb path (got {r})");
    }
    println!("✓ non-glb avatar path refused");

    println!("✓ all avatar loading & customization checks passed");
    Ok(())
}