use crate::host_state::HostState;
use anyhow::anyhow;
use wasmtime::{Caller, Linker, Result};

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
        "send_message",
        |mut caller: Caller<'_, HostState>, peer_idx: i32, message_ptr: i32, message_len: i32| {
            if message_len <= 0 {
                return Ok(());
            }
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            let mut buf = vec![0u8; message_len as usize];
            mem.read(&caller, message_ptr as usize, &mut buf)?;

            let addr = {
                let state = caller.data();
                let pc = state
                    .peer_connection()
                    .ok_or_else(|| anyhow!("no peer connection"))?;
                let key = state
                    .connected_peers()
                    .get(peer_idx as usize)
                    .ok_or_else(|| anyhow!("unknown peer index {peer_idx}"))?;
                *pc.peer_addr(key).ok_or_else(|| anyhow!("no address known for '{key}'"))?
            };

            let pc = caller
                .data()
                .peer_connection()
                .ok_or_else(|| anyhow!("no peer connection"))?;
            pc.send_udp(addr, &buf)?;
            Ok(())
        },
    )?;

    linker.func_wrap(
        "env",
        "receive_message",
        |mut caller: Caller<'_, HostState>| -> Result<i32> {
            let mut buf = [0u8; 1024];
            let received = caller
                .data()
                .peer_connection()
                .ok_or_else(|| anyhow!("no peer connection"))?
                .try_recv_udp(&mut buf);
            let Some((len, _from)) = received else {
                return Ok(0);
            };
            let mem = caller
                .get_export("memory")
                .and_then(|e| e.into_memory())
                .ok_or_else(|| anyhow!("guest has no memory export"))?;
            mem.write(&mut caller, 0, &buf[..len])?;
            Ok(len as i32)
        },
    )?;

    Ok(())
}
