//! The Wasm guest's entire view of the outside world.
//!
//! Every host function a guest can call is registered here on the `env`
//! import module of a wasmtime [`Linker`]. Nothing else is reachable from
//! inside the sandbox. All functions are type-safe (wasmtime checks signatures
//! at instantiation) and every pointer argument is bounds-checked against the
//! guest's linear memory before being read or written.
//!
//! See `docs/HOST_FUNCTIONS.md` in the repository root for the full reference.

use crate::avatar_standard;
use crate::host_state::HostState;
use anyhow::anyhow;
use std::path::Path;
use std::time::Instant;
use wasmtime::{Caller, Linker, Result};

/// Directory (inside the host crate) that `load_avatar` may read avatars from.
const AVATAR_ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/assets");

/// Registers every host function on `linker` under the `env` import module.
///
/// Call this once per [`wasmtime::Engine`], then instantiate guest modules
/// against the linker. Unknown imports that a guest tries to resolve will be
/// rejected at instantiation time.
pub fn register(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "env",
        "increment_counter",
        |mut caller: Caller<'_, HostState>| -> Result<()> {
            caller.data_mut().increment_counter();
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "update",
        |mut caller: Caller<'_, HostState>| -> Result<f64> {
            Ok(caller.data_mut().update_frame())
        },
    )?;

    linker.func_wrap(
        "env",
        "get_frame_count",
        |caller: Caller<'_, HostState>| -> Result<u64> {
            Ok(caller.data().frame_count())
        },
    )?;

    linker.func_wrap(
        "env",
        "get_input_move_up",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().move_up as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_move_down",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().move_down as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_move_left",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().move_left as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_move_right",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().move_right as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_action_1",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().action_1 as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_action_2",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().action_2 as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_gamepad_connected",
        |caller: Caller<'_, HostState>| -> Result<u32> {
            Ok(caller.data().input().gamepad_connected as u32)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_gamepad_axis_x",
        |caller: Caller<'_, HostState>| -> Result<f32> {
            Ok(caller.data().input().gamepad_axis_x)
        },
    )?;
    linker.func_wrap(
        "env",
        "get_input_gamepad_axis_y",
        |caller: Caller<'_, HostState>| -> Result<f32> {
            Ok(caller.data().input().gamepad_axis_y)
        },
    )?;

    linker.func_wrap(
        "env",
        "send_network_message",
        |mut caller: Caller<'_, HostState>,
         peer_id_ptr: i32,
         peer_id_len: i32,
         msg_ptr: i32,
         msg_len: i32| -> Result<i32> {
            if peer_id_ptr < 0 || peer_id_len < 0 || msg_ptr < 0 || msg_len < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let peer_end = peer_id_ptr as usize + peer_id_len as usize;
            let msg_end = msg_ptr as usize + msg_len as usize;
            if peer_end > mem_size || msg_end > mem_size {
                return Ok(0);
            }

            let peer_id = String::from_utf8_lossy(
                &mem.data(&caller)[peer_id_ptr as usize..peer_end],
            )
            .to_string();
            let msg = mem.data(&caller)[msg_ptr as usize..msg_end].to_vec();

            let Some(addr) = ({
                let pc = caller.data().peer_connection().ok_or_else(|| anyhow!("no peer connection"))?;
                pc.peer_addr(&peer_id).copied()
            }) else {
                return Ok(0);
            };

            let pc = caller
                .data()
                .peer_connection()
                .ok_or_else(|| anyhow!("no peer connection"))?;
            match pc.send_udp(addr, &msg) {
                Ok(_) => Ok(1),
                Err(_) => Ok(0),
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "receive_network_message",
        |mut caller: Caller<'_, HostState>, buffer_ptr: i32, buffer_len: i32| -> Result<i32> {
            let msg = match caller.data_mut().incoming_messages_mut().pop_front() {
                Some(m) => m,
                None => return Ok(0),
            };
            if buffer_ptr < 0 || buffer_len < 0 {
                return Ok(-1);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            if buffer_ptr as usize + msg.len() > mem_size {
                return Ok(-1);
            }
            mem.write(&mut caller, buffer_ptr as usize, &msg)?;
            Ok(msg.len() as i32)
        },
    )?;

    linker.func_wrap(
        "env",
        "update_avatar_transform",
        |caller: Caller<'_, HostState>, x: f32, y: f32, z: f32, rot_y: f32| -> Result<()> {
            let start = Instant::now();
            if let Some(avatar) = caller.data().avatar_state() {
                let mut pose = avatar.lock().unwrap();
                pose.x = x;
                pose.y = y;
                pose.z = z;
                pose.rot_y = rot_y;
            }
            host_fn_time("update_avatar_transform", start.elapsed());
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "broadcast_avatar_pose",
        |caller: Caller<'_, HostState>, x: f32, y: f32, z: f32, rot_y: f32| -> Result<()> {
            let start = Instant::now();
            if !(x.is_finite() && y.is_finite() && z.is_finite() && rot_y.is_finite()) {
                return Ok(());
            }
            let mut payload = Vec::with_capacity(16);
            payload.extend_from_slice(&x.to_le_bytes());
            payload.extend_from_slice(&y.to_le_bytes());
            payload.extend_from_slice(&z.to_le_bytes());
            payload.extend_from_slice(&rot_y.to_le_bytes());
            if let Some(pc) = caller.data().peer_connection() {
                pc.send_to_all(&payload)?;
            }
            host_fn_time("broadcast_avatar_pose", start.elapsed());
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "get_remote_avatar_pose",
        |mut caller: Caller<'_, HostState>,
         peer_id_ptr: i32,
         peer_id_len: i32,
         out_pose_ptr: i32| -> Result<i32> {
            let start = Instant::now();
            if peer_id_ptr < 0 || peer_id_len < 0 || out_pose_ptr < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let peer_end = peer_id_ptr as usize + peer_id_len as usize;
            let out_end = out_pose_ptr as usize + 16;
            if peer_end > mem_size || out_end > mem_size {
                return Ok(0);
            }
            let peer_id = String::from_utf8_lossy(
                &mem.data(&caller)[peer_id_ptr as usize..peer_end],
            )
            .to_string();
            let remote = caller.data().remote_avatars().clone();
            let remote = remote.lock().unwrap();
            let Some(pose) = remote.get(&peer_id) else {
                return Ok(0);
            };
            let mut buf = Vec::with_capacity(16);
            buf.extend_from_slice(&pose.x.to_le_bytes());
            buf.extend_from_slice(&pose.y.to_le_bytes());
            buf.extend_from_slice(&pose.z.to_le_bytes());
            buf.extend_from_slice(&pose.rot_y.to_le_bytes());
            mem.write(&mut caller, out_pose_ptr as usize, &buf)?;
            host_fn_time("get_remote_avatar_pose", start.elapsed());
            Ok(1)
        },
    )?;

    linker.func_wrap(
        "env",
        "get_movement_axis",
        |caller: Caller<'_, HostState>| -> Result<i32> {
            Ok(caller.data().movement_axis() as i32)
        },
    )?;

    linker.func_wrap(
        "env",
        "set_tag_score",
        |mut caller: Caller<'_, HostState>, score: u32| -> Result<()> {
            caller.data_mut().set_tag_score(score);
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "broadcast_tag_score",
        |caller: Caller<'_, HostState>, score: u32| -> Result<()> {
            if let Some(pc) = caller.data().peer_connection() {
                pc.send_to_all(&score.to_le_bytes())?;
            }
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "load_avatar",
        |mut caller: Caller<'_, HostState>, path_ptr: i32, path_len: i32| -> Result<i32> {
            if path_ptr < 0 || path_len < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let path_end = path_ptr as usize + path_len as usize;
            if path_end > mem_size {
                return Ok(0);
            }
            let path = String::from_utf8_lossy(&mem.data(&caller)[path_ptr as usize..path_end]);
            let path = path.to_string();
            if !path.ends_with(".glb") {
                return Ok(0);
            }

            // The avatar must resolve inside the host asset folder so a guest
            // can never make the host read an arbitrary file.
            let assets = Path::new(AVATAR_ASSETS_DIR);
            let resolved = match std::fs::canonicalize(assets.join(&path)) {
                Ok(p) => p,
                Err(_) => return Ok(0),
            };
            let Ok(assets_canon) = std::fs::canonicalize(assets) else {
                return Ok(0);
            };
            if !resolved.starts_with(&assets_canon) {
                return Ok(0);
            }

            let bytes = match std::fs::read(&resolved) {
                Ok(b) => b,
                Err(_) => return Ok(0),
            };
            if avatar_standard::validate_avatar_glb(&bytes).is_err() {
                return Ok(0);
            }
            caller.data_mut().set_avatar_path(path.clone());
            Ok(1)
        },
    )?;

    linker.func_wrap(
        "env",
        "broadcast_chunk_claim",
        |mut caller: Caller<'_, HostState>,
         origin_x: i32,
         origin_z: i32,
         extent_x: i32,
         extent_z: i32| -> Result<()> {
            let extent = (
                extent_x.max(1) as u32,
                extent_z.max(1) as u32,
            );
            caller.data_mut().claim_chunk_region(
                crate::chunk::ChunkCoord { x: origin_x, z: origin_z },
                extent,
            )?;
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "get_chunk_owner",
        |mut caller: Caller<'_, HostState>,
         chunk_x: i32,
         chunk_z: i32,
         out_peer_ptr: i32,
         out_peer_len: i32| -> Result<i32> {
            if out_peer_ptr < 0 || out_peer_len < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let out_end = out_peer_ptr as usize + out_peer_len as usize;
            if out_end > mem_size {
                return Ok(0);
            }
            let entry = caller
                .data()
                .chunk_registry()
                .lock()
                .unwrap()
                .get(crate::chunk::ChunkCoord { x: chunk_x, z: chunk_z })
                .cloned();
            let Some(entry) = entry else {
                return Ok(0);
            };
            let bytes = entry.peer_id.as_bytes();
            let n = bytes.len().min(out_peer_len as usize);
            mem.write(&mut caller, out_peer_ptr as usize, &bytes[..n])?;
            Ok(1)
        },
    )?;

    linker.func_wrap(
        "env",
        "save_chunk_state",
        |mut caller: Caller<'_, HostState>,
         edit_x: f32,
         edit_z: f32,
         kind_ptr: i32,
         kind_len: i32,
         value: f32| -> Result<i32> {
            if kind_ptr < 0 || kind_len < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let kind_end = kind_ptr as usize + kind_len as usize;
            if kind_end > mem_size {
                return Ok(0);
            }
            let kind = String::from_utf8_lossy(&mem.data(&caller)[kind_ptr as usize..kind_end])
                .into_owned();
            let edit = crate::chunk::ChunkEdit { x: edit_x, z: edit_z, kind, value };
            match caller.data_mut().record_chunk_edit(edit) {
                Ok(()) => Ok(1),
                Err(_) => Ok(0),
            }
        },
    )?;

    linker.func_wrap(
        "env",
        "publish_chunk_states",
        |mut caller: Caller<'_, HostState>| -> Result<i32> {
            let n = caller.data_mut().publish_chunk_states().unwrap_or(0);
            Ok(n as i32)
        },
    )?;

    linker.func_wrap(
        "env",
        "load_remote_chunk_state",
        |mut caller: Caller<'_, HostState>,
         chunk_x: i32,
         chunk_z: i32,
         out_buf: i32,
         out_len: i32| -> Result<i32> {
            if out_buf < 0 || out_len < 0 {
                return Ok(0);
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mem_size = mem.data_size(&caller);
            let out_end = out_buf as usize + out_len as usize;
            if out_end > mem_size {
                return Ok(0);
            }
            let coord = crate::chunk::ChunkCoord { x: chunk_x, z: chunk_z };
            let Some(state) = caller.data().load_remote_chunk_state(coord).unwrap_or(None) else {
                return Ok(0);
            };
            let json = serde_json::to_vec(&state).unwrap_or_default();
            let n = json.len().min(out_len as usize);
            mem.write(&mut caller, out_buf as usize, &json[..n])?;
            Ok(1)
        },
    )?;

    Ok(())
}

/// Logs a warning when a host function takes longer than 1 ms (a likely
/// bottleneck, e.g. lock contention or an oversized allocation).
fn host_fn_time(function: &str, elapsed: std::time::Duration) {
    let ms = elapsed.as_secs_f64() * 1000.0;
    if ms > 1.0 {
        tracing::warn!(function, time_ms = ms, "Slow host function");
    }
}
