use crate::types::Vec3;

#[derive(Clone, Debug)]
pub struct PlayerState {
    pub pos: Vec3,
    pub speed: Vec3,
    pub pitch: f32,
    pub yaw: f32,
    pub movement_speed: f32,
    pub movement_dir: f32,
    pub key_pressed: u32,
    pub fov: f32,
    pub wanted_range: f32,
    pub camera_inverted: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct MovementSettings {
    pub acceleration_default: f32,
    pub acceleration_air: f32,
    pub speed_walk: f32,
    pub speed_fast: f32,
    pub speed_jump: f32,
    pub gravity: f32,
}

impl Default for MovementSettings {
    fn default() -> Self {
        Self {
            acceleration_default: 3.0,
            acceleration_air: 2.0,
            speed_walk: 4.0,
            speed_fast: 20.0,
            speed_jump: 6.5,
            gravity: 9.81,
        }
    }
}

impl Default for PlayerState {
    fn default() -> Self {
        Self {
            pos: Vec3::default(),
            speed: Vec3::default(),
            pitch: 0.0,
            yaw: 0.0,
            movement_speed: 0.0,
            movement_dir: 0.0,
            key_pressed: 0,
            fov: 1.2,
            wanted_range: 128.0,
            camera_inverted: false,
        }
    }
}
