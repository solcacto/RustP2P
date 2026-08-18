#[derive(Clone, Copy, Default)]
pub struct InputState {
    pub move_up: u8,
    pub move_down: u8,
    pub move_left: u8,
    pub move_right: u8,
    pub action_1: u8,
    pub action_2: u8,
    pub gamepad_connected: u8,
    pub gamepad_axis_x: f32,
    pub gamepad_axis_y: f32,
}