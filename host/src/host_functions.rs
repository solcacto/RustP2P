use crate::host_state::HostState;
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

    Ok(())
}
