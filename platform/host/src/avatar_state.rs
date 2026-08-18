/// Target pose for the Universal Avatar, written by the Wasm guest through the
/// `update_avatar_transform` host function and applied to the Bevy scene by the
/// renderer.
#[derive(Debug, Clone, Copy)]
pub struct AvatarState {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub rot_y: f32,
}

impl Default for AvatarState {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            rot_y: 0.0,
        }
    }
}