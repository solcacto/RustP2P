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

    Ok(())
}
