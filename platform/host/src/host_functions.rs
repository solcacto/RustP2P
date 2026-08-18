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

    Ok(())
}
