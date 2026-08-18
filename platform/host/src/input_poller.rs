//! Polls player input once per frame and hands it to the host.
//!
//! This commit uses a **mock input** system that simulates key presses based on
//! frame count, so the input architecture can be tested without real hardware.
//! Real input libraries (gilrs for gamepads, winit for keyboard) will be wired
//! into this module in a future commit; the `HostState::set_input` API and the
//! getter host functions already work with both mock and real input.

use crate::input_state::InputState;

/// Simulates a sequence of key presses based on the current frame number.
///
/// - `move_up` held during frames 10..=20
/// - `move_right` held during frames 15..=25
/// - `action_1` pressed once on frame 30
/// - `action_2` held during frames 40..=50
pub fn poll_mock_input(frame: u64) -> InputState {
    let mut input = InputState::default();

    if (10..=20).contains(&frame) {
        input.move_up = 1;
    }
    if (15..=25).contains(&frame) {
        input.move_right = 1;
    }
    if frame == 30 {
        input.action_1 = 1;
    }
    if (40..=50).contains(&frame) {
        input.action_2 = 1;
    }

    input
}