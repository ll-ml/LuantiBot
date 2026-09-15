//! Small movement helpers shared by float and physics controller modes.

use crate::game::{snap_to_ground_height, InputState, PlayerCollider, PlayerState};
use crate::types::Vec3;
use crate::world::World;

pub(super) fn advance_position_bs(state: &mut PlayerState, input: InputState, dt: f32) {
    if !dt.is_finite() || dt <= 0.0 {
        state.speed = Vec3::default();
        return;
    }
    let speed = if input.forward && input.speed.is_finite() {
        input.speed.max(0.0) * 10.0
    } else {
        0.0
    };
    let yaw = if input.yaw.is_finite() {
        input.yaw
    } else {
        0.0
    };
    let dir = Vec3 {
        x: -yaw.sin(),
        y: 0.0,
        z: yaw.cos(),
    };
    let desired = Vec3 {
        x: dir.x * speed,
        y: 0.0,
        z: dir.z * speed,
    };
    let smoothing = (dt * 8.0).clamp(0.0, 1.0);
    state.speed.x += (desired.x - state.speed.x) * smoothing;
    state.speed.y += (desired.y - state.speed.y) * smoothing;
    state.speed.z += (desired.z - state.speed.z) * smoothing;
    state.pos.x += state.speed.x * dt;
    state.pos.y += state.speed.y * dt;
    state.pos.z += state.speed.z * dt;
}

pub(super) fn apply_ground_snap(state: &mut PlayerState, world: &World, collider: PlayerCollider) {
    if let Some(y) = snap_to_ground_height(state.pos, world, collider) {
        if (state.pos.y - y).abs() <= 1.0 {
            state.pos.y = y;
            state.speed.y = 0.0;
        }
    }
}
