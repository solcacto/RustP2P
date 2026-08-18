//! The input snapshot the host exposes to guests each frame.
//!
//! A fresh [`InputState`] is assembled every frame — from real Bevy keyboard
//! events (see `renderer::read_keyboard`) or from scripted mock input (see
//! [`crate::input_poller`]) — and stored on [`crate::host_state::HostState`],
//! where the `get_input_*` host functions read it.

/// The frame's input snapshot, readable by guests through the `get_input_*`
/// host functions.
#[derive(Clone, Copy, Default)]
pub struct InputState {
    /// Whether the "move up" control is held this frame.
    pub move_up: u8,
    /// Whether the "move down" control is held this frame.
    pub move_down: u8,
    /// Whether the "move left" control is held this frame.
    pub move_left: u8,
    /// Whether the "move right" control is held this frame.
    pub move_right: u8,
    /// Whether the primary action is held this frame.
    pub action_1: u8,
    /// Whether the secondary action is held this frame.
    pub action_2: u8,
    /// Whether a gamepad is currently connected (1 = yes, 0 = no).
    pub gamepad_connected: u8,
    /// Normalized gamepad stick X axis in `[-1.0, 1.0]`.
    pub gamepad_axis_x: f32,
    /// Normalized gamepad stick Y axis in `[-1.0, 1.0]`.
    pub gamepad_axis_y: f32,
}