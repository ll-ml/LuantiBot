use crate::mtp::{PlayerState, Vec3};
use crate::types::IVec3;
use crate::world::World;

const GRAVITY: f32 = -9.81 * 2.0;
const STEP_SIZE: f32 = 0.5;
const STEP_HEIGHT_BS: f32 = 10.0;
const JUMP_SPEED_BS: f32 = 6.5;
const MAX_FALL_SPEED_BS: f32 = -15.0;
const MAX_HORIZ_SPEED_BS: f32 = 50.0;

#[derive(Clone, Copy, Debug)]
pub struct PlayerCollider {
    pub half_width: f32,
    pub height: f32,
}

impl Default for PlayerCollider {
    fn default() -> Self {
        Self {
            half_width: 3.0,
            height: 18.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct InputState {
    pub forward: bool,
    pub speed: f32,
    pub yaw: f32,
}

pub fn step_player_bs(
    state: &mut PlayerState,
    world: &World,
    collider: PlayerCollider,
    input: InputState,
    dt: f32,
) {
    let mut vel = state.speed;

    if input.forward {
        let speed = input.speed * 10.0;
        let dir = Vec3 {
            x: input.yaw.sin(),
            y: 0.0,
            z: input.yaw.cos(),
        };
        vel.x = dir.x * speed;
        vel.z = dir.z * speed;
    } else {
        vel.x = 0.0;
        vel.z = 0.0;
    }

    if vel.x > MAX_HORIZ_SPEED_BS {
        vel.x = MAX_HORIZ_SPEED_BS;
    }
    if vel.x < -MAX_HORIZ_SPEED_BS {
        vel.x = -MAX_HORIZ_SPEED_BS;
    }
    if vel.z > MAX_HORIZ_SPEED_BS {
        vel.z = MAX_HORIZ_SPEED_BS;
    }
    if vel.z < -MAX_HORIZ_SPEED_BS {
        vel.z = -MAX_HORIZ_SPEED_BS;
    }

    vel.y += GRAVITY * dt;
    if vel.y < MAX_FALL_SPEED_BS {
        vel.y = MAX_FALL_SPEED_BS;
    }

    let mut pos = state.pos;
    let (pos_x, hit_x) = move_axis(pos, vel.x * dt, Axis::X, world, collider);
    pos = pos_x;
    if hit_x {
        if let Some(step_pos) = try_step(pos, vel.x * dt, Axis::X, world, collider) {
            pos = step_pos;
        }
    }
    let (pos_z, hit_z) = move_axis(pos, vel.z * dt, Axis::Z, world, collider);
    pos = pos_z;
    if hit_z {
        if let Some(step_pos) = try_step(pos, vel.z * dt, Axis::Z, world, collider) {
            pos = step_pos;
        }
    }
    if input.forward && (hit_x || hit_z) && vel.y <= 0.0 && is_on_ground(pos, world, collider) {
        vel.y = JUMP_SPEED_BS;
    }

    if collides(pos, world, collider) {
        if let Some(rescued) = resolve_penetration(pos, world, collider) {
            pos = rescued;
            vel.x = 0.0;
            vel.z = 0.0;
        }
    }
    let (pos_y, hit_y) = move_axis(pos, vel.y * dt, Axis::Y, world, collider);
    pos = pos_y;
    if hit_y && vel.y < 0.0 {
        vel.y = 0.0;
    }

    if vel.y <= 0.0 {
        if let Some(snapped) = snap_to_ground(pos, world, collider) {
            pos = snapped;
            vel.y = 0.0;
        }
    }

    state.pos = pos;
    state.speed = vel;
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
    Z,
}

fn move_axis(
    pos: Vec3,
    delta: f32,
    axis: Axis,
    world: &World,
    collider: PlayerCollider,
) -> (Vec3, bool) {
    if delta.abs() < f32::EPSILON {
        return (pos, false);
    }

    let mut remaining = delta;
    let mut current = pos;
    let step = STEP_SIZE.copysign(delta);
    let mut hit = false;

    while remaining.abs() > 0.0 {
        let step_delta = if remaining.abs() > step.abs() {
            step
        } else {
            remaining
        };

        let mut candidate = current;
        match axis {
            Axis::X => candidate.x += step_delta,
            Axis::Y => candidate.y += step_delta,
            Axis::Z => candidate.z += step_delta,
        }

        if collides(candidate, world, collider) {
            hit = true;
            break;
        }

        current = candidate;
        remaining -= step_delta;
    }

    (current, hit)
}

fn try_step(
    pos: Vec3,
    delta: f32,
    axis: Axis,
    world: &World,
    collider: PlayerCollider,
) -> Option<Vec3> {
    if delta.abs() < f32::EPSILON {
        return None;
    }
    let mut candidate = pos;
    candidate.y += STEP_HEIGHT_BS;
    if collides(candidate, world, collider) {
        return None;
    }
    match axis {
        Axis::X => candidate.x += delta,
        Axis::Z => candidate.z += delta,
        Axis::Y => candidate.y += delta,
    }
    if collides(candidate, world, collider) {
        return None;
    }
    Some(candidate)
}

fn snap_to_ground(pos: Vec3, world: &World, collider: PlayerCollider) -> Option<Vec3> {
    let max_drop = 20.0;
    let min = Vec3 {
        x: pos.x - collider.half_width,
        y: pos.y,
        z: pos.z - collider.half_width,
    };
    let max = Vec3 {
        x: pos.x + collider.half_width,
        y: pos.y + collider.height,
        z: pos.z + collider.half_width,
    };
    let min_x = (min.x / 10.0).floor() as i32;
    let max_x = ((max.x - 1e-4) / 10.0).floor() as i32;
    let min_z = (min.z / 10.0).floor() as i32;
    let max_z = ((max.z - 1e-4) / 10.0).floor() as i32;
    let y_bottom = min.y;
    let y_min = ((y_bottom - max_drop) / 10.0).floor() as i32;
    let y_max = (y_bottom / 10.0).floor() as i32;

    let mut best: Option<f32> = None;
    for z in min_z..=max_z {
        for y in y_min..=y_max {
            for x in min_x..=max_x {
                let node_pos = IVec3 { x, y, z };
                let boxes = world.collision_boxes(node_pos);
                if boxes.is_empty() {
                    continue;
                }
                let base_y = y as f32 * 10.0;
                for b in boxes {
                    let top = base_y + b.max.y * 10.0;
                    if top <= y_bottom + 0.01 {
                        if best.map(|v| top > v).unwrap_or(true) {
                            best = Some(top);
                        }
                    }
                }
            }
        }
    }

    best.map(|top| Vec3 {
        x: pos.x,
        y: top,
        z: pos.z,
    })
}

pub fn snap_to_ground_height(pos: Vec3, world: &World, collider: PlayerCollider) -> Option<f32> {
    snap_to_ground(pos, world, collider).map(|p| p.y)
}

fn collides(pos: Vec3, world: &World, collider: PlayerCollider) -> bool {
    let min = Vec3 {
        x: pos.x - collider.half_width,
        y: pos.y,
        z: pos.z - collider.half_width,
    };
    let max = Vec3 {
        x: pos.x + collider.half_width,
        y: pos.y + collider.height,
        z: pos.z + collider.half_width,
    };

    let min_x = (min.x / 10.0).floor() as i32;
    let min_y = (min.y / 10.0).floor() as i32;
    let min_z = (min.z / 10.0).floor() as i32;
    let max_x = ((max.x - 1e-4) / 10.0).floor() as i32;
    let max_y = ((max.y - 1e-4) / 10.0).floor() as i32;
    let max_z = ((max.z - 1e-4) / 10.0).floor() as i32;

    let player_min = min;
    let player_max = max;
    for z in min_z..=max_z {
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let node_pos = IVec3 { x, y, z };
                let boxes = world.collision_boxes(node_pos);
                if boxes.is_empty() {
                    continue;
                }
                let base = Vec3 {
                    x: x as f32 * 10.0,
                    y: y as f32 * 10.0,
                    z: z as f32 * 10.0,
                };
                for b in boxes {
                    let bmin = Vec3 {
                        x: base.x + b.min.x * 10.0,
                        y: base.y + b.min.y * 10.0,
                        z: base.z + b.min.z * 10.0,
                    };
                    let bmax = Vec3 {
                        x: base.x + b.max.x * 10.0,
                        y: base.y + b.max.y * 10.0,
                        z: base.z + b.max.z * 10.0,
                    };
                    if aabb_intersect(player_min, player_max, bmin, bmax) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

fn aabb_intersect(a_min: Vec3, a_max: Vec3, b_min: Vec3, b_max: Vec3) -> bool {
    a_min.x < b_max.x
        && a_max.x > b_min.x
        && a_min.y < b_max.y
        && a_max.y > b_min.y
        && a_min.z < b_max.z
        && a_max.z > b_min.z
}

fn is_on_ground(pos: Vec3, world: &World, collider: PlayerCollider) -> bool {
    let check = Vec3 {
        x: pos.x,
        y: pos.y - 0.5,
        z: pos.z,
    };
    collides(check, world, collider)
}

fn resolve_penetration(pos: Vec3, world: &World, collider: PlayerCollider) -> Option<Vec3> {
    let offsets = [
        Vec3 {
            x: 0.0,
            y: 10.0,
            z: 0.0,
        },
        Vec3 {
            x: 0.0,
            y: 20.0,
            z: 0.0,
        },
        Vec3 {
            x: 0.0,
            y: 30.0,
            z: 0.0,
        },
        Vec3 {
            x: 5.0,
            y: 10.0,
            z: 0.0,
        },
        Vec3 {
            x: -5.0,
            y: 10.0,
            z: 0.0,
        },
        Vec3 {
            x: 0.0,
            y: 10.0,
            z: 5.0,
        },
        Vec3 {
            x: 0.0,
            y: 10.0,
            z: -5.0,
        },
        Vec3 {
            x: 10.0,
            y: 0.0,
            z: 0.0,
        },
        Vec3 {
            x: -10.0,
            y: 0.0,
            z: 0.0,
        },
        Vec3 {
            x: 0.0,
            y: 0.0,
            z: 10.0,
        },
        Vec3 {
            x: 0.0,
            y: 0.0,
            z: -10.0,
        },
    ];
    for off in offsets.iter() {
        let candidate = Vec3 {
            x: pos.x + off.x,
            y: pos.y + off.y,
            z: pos.z + off.z,
        };
        if !collides(candidate, world, collider) {
            return Some(candidate);
        }
    }
    None
}
