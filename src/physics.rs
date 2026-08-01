use crate::mtp::{MovementSettings, PlayerState, Vec3};
use crate::types::IVec3;
use crate::world::World;

pub const BS: f32 = 10.0;
const COLLISION_STEP_BS: f32 = 0.25;
const CONTACT_EPSILON_BS: f32 = 0.05;
const MAX_PHYSICS_DT: f32 = 0.025;

#[derive(Clone, Copy, Debug)]
pub struct PhysicsParams {
    pub acceleration_ground_bs: f32,
    pub acceleration_air_bs: f32,
    pub gravity_bs: f32,
    pub jump_speed_bs: f32,
    pub max_horizontal_speed_bs: f32,
    pub max_fall_speed_bs: f32,
    pub step_height_bs: f32,
}

impl Default for PhysicsParams {
    fn default() -> Self {
        Self::from_movement(MovementSettings::default())
    }
}

impl PhysicsParams {
    pub fn from_movement(settings: MovementSettings) -> Self {
        let finite_nonnegative = |value: f32, fallback: f32| {
            if value.is_finite() && (0.0..=1_000.0).contains(&value) {
                value
            } else {
                fallback
            }
        };
        let gravity = finite_nonnegative(settings.gravity, 9.81);
        let max_speed = finite_nonnegative(settings.speed_fast, 20.0)
            .max(finite_nonnegative(settings.speed_walk, 4.0));
        Self {
            acceleration_ground_bs: finite_nonnegative(settings.acceleration_default, 3.0) * BS,
            acceleration_air_bs: finite_nonnegative(settings.acceleration_air, 2.0) * BS,
            gravity_bs: -gravity * BS,
            jump_speed_bs: finite_nonnegative(settings.speed_jump, 6.5) * BS,
            max_horizontal_speed_bs: max_speed * BS,
            max_fall_speed_bs: -50.0 * BS,
            step_height_bs: 0.6 * BS,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PlayerCollider {
    pub half_width: f32,
    pub height: f32,
}

impl Default for PlayerCollider {
    fn default() -> Self {
        Self {
            half_width: 3.0,
            height: 17.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct InputState {
    pub forward: bool,
    pub jump: bool,
    /// Jump only when forward movement is blocked by loaded collision geometry.
    pub auto_jump: bool,
    pub speed: f32,
    /// Horizontal look direction in radians, measured counter-clockwise from +Z.
    pub yaw: f32,
}

pub fn step_player_bs(
    state: &mut PlayerState,
    world: &World,
    collider: PlayerCollider,
    params: PhysicsParams,
    input: InputState,
    dt: f32,
) {
    if !valid_vec(state.pos) || !valid_vec(state.speed) || !dt.is_finite() || dt <= 0.0 {
        state.speed = Vec3::default();
        return;
    }

    let dt = dt.min(0.25);
    let substeps = (dt / MAX_PHYSICS_DT).ceil().max(1.0) as u32;
    let sub_dt = dt / substeps as f32;
    for _ in 0..substeps {
        step_subframe(state, world, collider, params, input, sub_dt);
    }
}

fn step_subframe(
    state: &mut PlayerState,
    world: &World,
    collider: PlayerCollider,
    params: PhysicsParams,
    input: InputState,
    dt: f32,
) {
    let mut pos = state.pos;
    let mut vel = state.speed;

    if space_at(pos, world, collider) == Space::Colliding {
        if let Some(recovered) = resolve_penetration(pos, world, collider) {
            pos = recovered;
        } else {
            state.speed = Vec3::default();
            return;
        }
    }
    if space_at(pos, world, collider) == Space::Unloaded {
        state.speed = Vec3::default();
        return;
    }

    let grounded = is_on_ground(pos, world, collider);
    let requested_speed = if input.forward && input.speed.is_finite() {
        (input.speed.max(0.0) * BS).min(params.max_horizontal_speed_bs)
    } else {
        0.0
    };
    let yaw = if input.yaw.is_finite() { input.yaw } else { 0.0 };
    let desired_x = -yaw.sin() * requested_speed;
    let desired_z = yaw.cos() * requested_speed;
    let acceleration = if grounded {
        params.acceleration_ground_bs
    } else {
        params.acceleration_air_bs
    };
    vel.x = approach(vel.x, desired_x, acceleration * dt);
    vel.z = approach(vel.z, desired_z, acceleration * dt);

    if input.jump && grounded {
        vel.y = params.jump_speed_bs;
    } else if grounded && vel.y <= 0.0 {
        vel.y = 0.0;
    } else {
        vel.y = (vel.y + params.gravity_bs * dt).max(params.max_fall_speed_bs);
    }

    let may_step = grounded && vel.y <= 0.0;
    let mut blocked_by_node = false;
    let x_start = pos;
    let x_move = move_axis(pos, vel.x * dt, Axis::X, world, collider);
    pos = x_move.pos;
    if let Some(hit) = x_move.hit {
        blocked_by_node |= hit == Space::Colliding;
        if may_step && hit == Space::Colliding {
            if let Some(stepped) = try_step(
                x_start,
                vel.x * dt,
                Axis::X,
                params.step_height_bs,
                world,
                collider,
            ) {
                pos = stepped;
            } else {
                vel.x = 0.0;
            }
        } else {
            vel.x = 0.0;
        }
    }

    let z_start = pos;
    let z_move = move_axis(pos, vel.z * dt, Axis::Z, world, collider);
    pos = z_move.pos;
    if let Some(hit) = z_move.hit {
        blocked_by_node |= hit == Space::Colliding;
        if may_step && hit == Space::Colliding {
            if let Some(stepped) = try_step(
                z_start,
                vel.z * dt,
                Axis::Z,
                params.step_height_bs,
                world,
                collider,
            ) {
                pos = stepped;
            } else {
                vel.z = 0.0;
            }
        } else {
            vel.z = 0.0;
        }
    }

    if input.auto_jump && input.forward && grounded && blocked_by_node && vel.y <= 0.0 {
        vel.y = params.jump_speed_bs;
    }

    let y_move = move_axis(pos, vel.y * dt, Axis::Y, world, collider);
    pos = y_move.pos;
    if y_move.hit.is_some() {
        vel.y = 0.0;
    } else if vel.y <= 0.0 {
        if let Some(top) = ground_top(pos, CONTACT_EPSILON_BS * 2.0, world, collider) {
            pos.y = top;
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

struct AxisMove {
    pos: Vec3,
    hit: Option<Space>,
}

fn move_axis(
    pos: Vec3,
    delta: f32,
    axis: Axis,
    world: &World,
    collider: PlayerCollider,
) -> AxisMove {
    if !delta.is_finite() || delta.abs() <= f32::EPSILON {
        return AxisMove {
            pos,
            hit: (!delta.is_finite()).then_some(Space::Unloaded),
        };
    }

    let mut remaining = delta;
    let mut current = pos;
    let direction_step = COLLISION_STEP_BS.copysign(delta);
    let max_iterations = (delta.abs() / COLLISION_STEP_BS).ceil() as usize + 1;

    for _ in 0..max_iterations {
        if remaining.abs() <= f32::EPSILON {
            break;
        }
        let step_delta = if remaining.abs() > COLLISION_STEP_BS {
            direction_step
        } else {
            remaining
        };
        let mut candidate = current;
        add_axis(&mut candidate, axis, step_delta);
        let candidate_space = space_at(candidate, world, collider);
        if candidate_space != Space::Free {
            return AxisMove {
                pos: current,
                hit: Some(candidate_space),
            };
        }
        current = candidate;
        remaining -= step_delta;
    }

    AxisMove {
        pos: current,
        hit: None,
    }
}

fn try_step(
    start: Vec3,
    delta: f32,
    axis: Axis,
    step_height: f32,
    world: &World,
    collider: PlayerCollider,
) -> Option<Vec3> {
    if !delta.is_finite() || delta.abs() <= f32::EPSILON || step_height <= 0.0 {
        return None;
    }

    let raised = move_axis(start, step_height, Axis::Y, world, collider);
    if raised.hit.is_some() || (raised.pos.y - start.y - step_height).abs() > CONTACT_EPSILON_BS {
        return None;
    }
    let horizontal = move_axis(raised.pos, delta, axis, world, collider);
    if horizontal.hit.is_some() {
        return None;
    }
    let top = ground_top(
        horizontal.pos,
        step_height + CONTACT_EPSILON_BS,
        world,
        collider,
    )?;
    let mut stepped = horizontal.pos;
    stepped.y = top;
    (space_at(stepped, world, collider) == Space::Free).then_some(stepped)
}

pub fn snap_to_ground_height(pos: Vec3, world: &World, collider: PlayerCollider) -> Option<f32> {
    ground_top(pos, 1.0, world, collider)
}

fn ground_top(
    pos: Vec3,
    max_drop: f32,
    world: &World,
    collider: PlayerCollider,
) -> Option<f32> {
    let player_min_x = pos.x - collider.half_width;
    let player_max_x = pos.x + collider.half_width;
    let player_min_z = pos.z - collider.half_width;
    let player_max_z = pos.z + collider.half_width;
    let bottom = pos.y;
    let min_x = world_to_node(player_min_x);
    let max_x = world_to_node(player_max_x - CONTACT_EPSILON_BS);
    let min_z = world_to_node(player_min_z);
    let max_z = world_to_node(player_max_z - CONTACT_EPSILON_BS);
    let min_y = world_to_node(bottom - max_drop);
    let max_y = world_to_node(bottom + CONTACT_EPSILON_BS);

    let mut best = None;
    for z in min_z..=max_z {
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let boxes = world.collision_boxes(IVec3 { x, y, z })?;
                let base = node_center_bs(x, y, z);
                for box_ in boxes {
                    let min = add_scaled(base, box_.min);
                    let max = add_scaled(base, box_.max);
                    let overlaps_xz = player_min_x < max.x
                        && player_max_x > min.x
                        && player_min_z < max.z
                        && player_max_z > min.z;
                    if overlaps_xz
                        && max.y <= bottom + CONTACT_EPSILON_BS
                        && max.y >= bottom - max_drop - CONTACT_EPSILON_BS
                        && best.map(|value| max.y > value).unwrap_or(true)
                    {
                        best = Some(max.y);
                    }
                }
            }
        }
    }
    best
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Space {
    Free,
    Colliding,
    Unloaded,
}

fn space_at(pos: Vec3, world: &World, collider: PlayerCollider) -> Space {
    let player_min = Vec3 {
        x: pos.x - collider.half_width,
        y: pos.y,
        z: pos.z - collider.half_width,
    };
    let player_max = Vec3 {
        x: pos.x + collider.half_width,
        y: pos.y + collider.height,
        z: pos.z + collider.half_width,
    };

    let min_x = world_to_node(player_min.x);
    let min_y = world_to_node(player_min.y);
    let min_z = world_to_node(player_min.z);
    let max_x = world_to_node(player_max.x - CONTACT_EPSILON_BS);
    let max_y = world_to_node(player_max.y - CONTACT_EPSILON_BS);
    let max_z = world_to_node(player_max.z - CONTACT_EPSILON_BS);

    for z in min_z..=max_z {
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let Some(boxes) = world.collision_boxes(IVec3 { x, y, z }) else {
                    return Space::Unloaded;
                };
                let base = node_center_bs(x, y, z);
                for box_ in boxes {
                    let box_min = add_scaled(base, box_.min);
                    let box_max = add_scaled(base, box_.max);
                    if aabb_intersect(player_min, player_max, box_min, box_max) {
                        return Space::Colliding;
                    }
                }
            }
        }
    }
    Space::Free
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
    let mut check = pos;
    check.y -= CONTACT_EPSILON_BS;
    space_at(check, world, collider) == Space::Colliding
}

fn resolve_penetration(pos: Vec3, world: &World, collider: PlayerCollider) -> Option<Vec3> {
    for index in 1..=200 {
        let distance = index as f32 * 0.1;
        for offset in [
            Vec3 {
                x: 0.0,
                y: distance,
                z: 0.0,
            },
            Vec3 {
                x: distance,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: -distance,
                y: 0.0,
                z: 0.0,
            },
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: distance,
            },
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: -distance,
            },
        ] {
            let candidate = Vec3 {
                x: pos.x + offset.x,
                y: pos.y + offset.y,
                z: pos.z + offset.z,
            };
            if space_at(candidate, world, collider) == Space::Free {
                return Some(candidate);
            }
        }
    }
    None
}

fn world_to_node(value: f32) -> i32 {
    (value / BS + 0.5).floor() as i32
}

fn node_center_bs(x: i32, y: i32, z: i32) -> Vec3 {
    Vec3 {
        x: x as f32 * BS,
        y: y as f32 * BS,
        z: z as f32 * BS,
    }
}

fn add_scaled(base: Vec3, local: Vec3) -> Vec3 {
    Vec3 {
        x: base.x + local.x * BS,
        y: base.y + local.y * BS,
        z: base.z + local.z * BS,
    }
}

fn add_axis(value: &mut Vec3, axis: Axis, delta: f32) {
    match axis {
        Axis::X => value.x += delta,
        Axis::Y => value.y += delta,
        Axis::Z => value.z += delta,
    }
}

fn approach(current: f32, target: f32, max_delta: f32) -> f32 {
    if current < target {
        (current + max_delta).min(target)
    } else {
        (current - max_delta).max(target)
    }
}

fn valid_vec(value: Vec3) -> bool {
    value.x.is_finite() && value.y.is_finite() && value.z.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_at(pos: Vec3) -> PlayerState {
        PlayerState {
            pos,
            ..PlayerState::default()
        }
    }

    #[test]
    fn regular_node_is_centered_on_its_node_position() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: 0, y: 0, z: 0 }, true);
        let y = snap_to_ground_height(
            Vec3 {
                x: 0.0,
                y: 5.5,
                z: 0.0,
            },
            &world,
            PlayerCollider::default(),
        );
        assert_eq!(y, Some(5.0));
    }

    #[test]
    fn negative_node_coordinates_use_centered_bounds() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: -1, y: 0, z: 0 }, true);
        let y = snap_to_ground_height(
            Vec3 {
                x: -10.0,
                y: 5.5,
                z: 0.0,
            },
            &world,
            PlayerCollider::default(),
        );
        assert_eq!(y, Some(5.0));
    }

    #[test]
    fn falling_player_stops_on_the_node_top() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: 0, y: 0, z: 0 }, true);
        let mut state = state_at(Vec3 {
            x: 0.0,
            y: 12.0,
            z: 0.0,
        });
        let input = InputState {
            forward: false,
            jump: false,
            auto_jump: false,
            speed: 0.0,
            yaw: 0.0,
        };
        for _ in 0..100 {
            step_player_bs(
                &mut state,
                &world,
                PlayerCollider::default(),
                PhysicsParams::default(),
                input,
                0.02,
            );
        }
        assert!((state.pos.y - 5.0).abs() < 0.26, "y={}", state.pos.y);
        assert_eq!(state.speed.y, 0.0);
    }

    #[test]
    fn unloaded_terrain_stops_simulation() {
        let world = World::new();
        let mut state = state_at(Vec3 {
            x: 20.0,
            y: 20.0,
            z: 20.0,
        });
        let original = state.pos;
        step_player_bs(
            &mut state,
            &world,
            PlayerCollider::default(),
            PhysicsParams::default(),
            InputState {
                forward: true,
                jump: false,
                auto_jump: false,
                speed: 4.0,
                yaw: 0.0,
            },
            0.2,
        );
        assert_eq!(state.pos.x, original.x);
        assert_eq!(state.pos.y, original.y);
        assert_eq!(state.pos.z, original.z);
        assert_eq!(state.speed.x, 0.0);
        assert_eq!(state.speed.y, 0.0);
        assert_eq!(state.speed.z, 0.0);
    }

    #[test]
    fn positive_quarter_turn_yaw_moves_toward_negative_x() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: 0, y: 0, z: 0 }, true);
        let mut state = state_at(Vec3 {
            x: 0.0,
            y: 5.0,
            z: 0.0,
        });
        step_player_bs(
            &mut state,
            &world,
            PlayerCollider::default(),
            PhysicsParams::default(),
            InputState {
                forward: true,
                jump: false,
                auto_jump: false,
                speed: 4.0,
                yaw: std::f32::consts::FRAC_PI_2,
            },
            0.02,
        );
        assert!(state.pos.x < 0.0, "x={}", state.pos.x);
        assert!(state.speed.x < 0.0, "vx={}", state.speed.x);
    }

    #[test]
    fn horizontal_motion_stops_at_a_full_node_wall() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: 0, y: 0, z: 0 }, true);
        world.insert_test_node(IVec3 { x: 0, y: 1, z: 1 }, true);
        let mut state = state_at(Vec3 {
            x: 0.0,
            y: 5.0,
            z: 0.0,
        });
        let input = InputState {
            forward: true,
            jump: false,
            auto_jump: false,
            speed: 4.0,
            yaw: 0.0,
        };
        for _ in 0..100 {
            step_player_bs(
                &mut state,
                &world,
                PlayerCollider::default(),
                PhysicsParams::default(),
                input,
                0.02,
            );
        }
        assert!(
            state.pos.z <= 2.0 + CONTACT_EPSILON_BS + 0.01,
            "z={}",
            state.pos.z
        );
        assert_eq!(state.speed.z, 0.0);
    }

    #[test]
    fn auto_jump_clears_a_single_full_node_obstacle() {
        let mut world = World::new();
        for z in 0..=10 {
            world.insert_test_node(IVec3 { x: 0, y: 0, z }, true);
        }
        world.insert_test_node(IVec3 { x: 0, y: 1, z: 1 }, true);
        let mut state = state_at(Vec3 {
            x: 0.0,
            y: 5.0,
            z: 0.0,
        });
        let input = InputState {
            forward: true,
            jump: false,
            auto_jump: true,
            speed: 4.0,
            yaw: 0.0,
        };
        let mut max_y = state.pos.y;
        for _ in 0..120 {
            step_player_bs(
                &mut state,
                &world,
                PlayerCollider::default(),
                PhysicsParams::default(),
                input,
                0.02,
            );
            max_y = max_y.max(state.pos.y);
        }
        assert!(max_y > 15.0, "max_y={max_y}");
        assert!(state.pos.z > 15.0, "z={}", state.pos.z);
    }

    #[test]
    fn jump_uses_the_server_configured_impulse() {
        let mut world = World::new();
        world.insert_test_node(IVec3 { x: 0, y: 0, z: 0 }, true);
        let mut state = state_at(Vec3 {
            x: 0.0,
            y: 5.0,
            z: 0.0,
        });
        let mut params = PhysicsParams::default();
        params.jump_speed_bs = 42.0;
        step_player_bs(
            &mut state,
            &world,
            PlayerCollider::default(),
            params,
            InputState {
                forward: false,
                jump: true,
                auto_jump: false,
                speed: 0.0,
                yaw: 0.0,
            },
            0.02,
        );
        assert_eq!(state.speed.y, 42.0);
        assert!(state.pos.y > 5.0);
    }
}
