//! Movement goals, server-provided routes, and navigation state.

use anyhow::Result;
use serde_json::json;
use std::collections::VecDeque;
use std::f32::consts::PI;

use crate::game::{PlayerState, BS};
use crate::network::MtpConnection;
use crate::types::{IVec3, Vec3};
use super::validation::{parse_node_position, valid_command_atom};

#[derive(Clone, Debug)]
pub(crate) enum MoveDirection {
    Forward,
    Backward,
    Left,
    Right,
}

#[derive(Clone, Debug)]
pub(crate) enum MoveSpec {
    Direction { dir: MoveDirection, steps: f32 },
    Delta { dx: f32, dy: f32, dz: f32 },
    Target { x: f32, y: Option<f32>, z: f32 },
}

#[derive(Clone, Debug)]
pub(crate) struct MoveRequest {
    pub(crate) spec: MoveSpec,
    pub(crate) speed: Option<f32>,
}

#[derive(Clone, Debug)]
pub(super) struct MoveGoal {
    /// Final standable destination. Intermediate route points are kept in
    /// `waypoints` and are never resource-node coordinates.
    pub(super) target: Vec3,
    pub(super) waypoints: VecDeque<Vec3>,
    pub(super) speed: f32,
    pub(super) stop_dist: f32,
    pub(super) arrival_action: Option<ArrivalAction>,
}

impl MoveGoal {
    pub(super) fn current_waypoint(&self) -> Vec3 {
        self.waypoints.front().copied().unwrap_or(self.target)
    }

    pub(super) fn remaining_waypoints(&self) -> usize {
        self.waypoints.len()
            + usize::from(
                !self
                    .waypoints
                    .back()
                    .copied()
                    .is_some_and(|last| positions_nearly_equal(last, self.target)),
            )
    }

    /// Returns true when the final destination has been reached.
    pub(super) fn advance_waypoint(&mut self) -> bool {
        if self.waypoints.pop_front().is_some() {
            self.waypoints.is_empty()
        } else {
            true
        }
    }
}

#[derive(Clone, Debug)]
pub(super) enum ArrivalAction {
    Collect {
        node: String,
        count: u16,
        radius: i32,
        target: Option<IVec3>,
    },
    Hunt {
        hunt_id: u64,
    },
}

impl ArrivalAction {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Collect { .. } => "collect",
            Self::Hunt { .. } => "hunt",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct NavigationSnapshot {
    pub(super) status: &'static str,
    pub(super) recovering: bool,
    pub(super) recovery_attempts: u8,
    pub(super) stalled_for_seconds: f32,
    pub(super) last_error: Option<String>,
}

impl Default for NavigationSnapshot {
    fn default() -> Self {
        Self {
            status: "idle",
            recovering: false,
            recovery_attempts: 0,
            stalled_for_seconds: 0.0,
            last_error: None,
        }
    }
}

impl NavigationSnapshot {
    pub(super) fn begin(&mut self, status: &'static str) {
        self.status = status;
        self.recovering = false;
        self.recovery_attempts = 0;
        self.stalled_for_seconds = 0.0;
        self.last_error = None;
    }

    pub(super) fn stop(&mut self) {
        *self = Self::default();
    }

    pub(super) fn fail(&mut self, error: impl Into<String>) {
        self.status = "failed";
        self.recovering = false;
        self.last_error = Some(error.into());
    }
}

pub(super) fn yaw_for_direction(yaw: f32, dir: &MoveDirection) -> f32 {
    match dir {
        MoveDirection::Forward => yaw,
        MoveDirection::Backward => yaw + PI,
        MoveDirection::Left => yaw + PI * 0.5,
        MoveDirection::Right => yaw - PI * 0.5,
    }
}

pub(super) fn wrap_angle(mut angle: f32) -> f32 {
    while angle > PI {
        angle -= 2.0 * PI;
    }
    while angle < -PI {
        angle += 2.0 * PI;
    }
    angle
}

pub(super) fn approach_angle(current: f32, target: f32, factor: f32) -> f32 {
    let diff = wrap_angle(target - current);
    current + diff * factor.clamp(0.0, 1.0)
}

pub(super) fn build_move_goal(state: &PlayerState, request: MoveRequest, default_speed: f32) -> MoveGoal {
    let default_speed = if default_speed.is_finite() && default_speed > 0.0 {
        default_speed
    } else {
        4.0
    };
    let requested_speed = request.speed.unwrap_or(default_speed);
    let speed = if requested_speed.is_finite() {
        requested_speed.max(0.1)
    } else {
        default_speed.max(0.1)
    };
    // Network positions use 10 units per node. A half-unit tolerance made goals
    // require five-centimetre precision and caused oscillation around targets.
    let stop_dist = 2.0;
    let target = match request.spec {
        MoveSpec::Direction { dir, steps } => {
            let steps = if steps.is_finite() {
                steps.max(0.0)
            } else {
                0.0
            };
            let step_bs = steps * 10.0;
            let yaw = yaw_for_direction(state.yaw, &dir);
            let dir_vec = Vec3 {
                x: -yaw.sin(),
                y: 0.0,
                z: yaw.cos(),
            };
            Vec3 {
                x: state.pos.x + dir_vec.x * step_bs,
                y: state.pos.y,
                z: state.pos.z + dir_vec.z * step_bs,
            }
        }
        MoveSpec::Delta { dx, dy, dz } => {
            let finite_or_zero = |value: f32| if value.is_finite() { value } else { 0.0 };
            Vec3 {
                x: state.pos.x + finite_or_zero(dx) * 10.0,
                y: state.pos.y + finite_or_zero(dy) * 10.0,
                z: state.pos.z + finite_or_zero(dz) * 10.0,
            }
        }
        MoveSpec::Target { x, y, z } => Vec3 {
            x: if x.is_finite() { x * 10.0 } else { state.pos.x },
            y: y.filter(|value| value.is_finite())
                .unwrap_or(state.pos.y / 10.0)
                * 10.0,
            z: if z.is_finite() { z * 10.0 } else { state.pos.z },
        },
    };
    MoveGoal {
        target,
        waypoints: VecDeque::new(),
        speed,
        stop_dist,
        arrival_action: None,
    }
}

pub(super) const MAX_ROUTE_WAYPOINTS: usize = 256;

pub(super) fn parse_route_goal(
    payload: &str,
    current_position: Vec3,
    default_speed: f32,
) -> std::result::Result<Option<MoveGoal>, String> {
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| format!("invalid JSON: {error}"))?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return Ok(None);
    }

    let path = value
        .get("path")
        .or_else(|| value.get("waypoints"))
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "successful path response has no path array".to_string())?;
    if path.len() > MAX_ROUTE_WAYPOINTS {
        return Err(format!(
            "path has {} waypoints; maximum is {MAX_ROUTE_WAYPOINTS}",
            path.len()
        ));
    }

    let stand = value
        .get("stand")
        .and_then(parse_route_position)
        .or_else(|| path.last().and_then(parse_route_position))
        .ok_or_else(|| "successful path response has no stand position".to_string())?;

    let mut waypoints = VecDeque::new();
    for raw in path {
        let waypoint = parse_route_position(raw)
            .ok_or_else(|| "path contains an invalid waypoint".to_string())?;
        if waypoints
            .back()
            .copied()
            .is_some_and(|previous| positions_nearly_equal(previous, waypoint))
        {
            continue;
        }
        waypoints.push_back(waypoint);
    }
    if !waypoints
        .back()
        .copied()
        .is_some_and(|last| positions_nearly_equal(last, stand))
    {
        waypoints.push_back(stand);
    }
    while waypoints
        .front()
        .copied()
        .is_some_and(|waypoint| waypoint_reached(current_position, waypoint, 2.0))
    {
        waypoints.pop_front();
    }

    let speed = if default_speed.is_finite() && default_speed > 0.0 {
        default_speed
    } else {
        4.0
    };
    Ok(Some(MoveGoal {
        target: stand,
        waypoints,
        speed,
        stop_dist: 2.0,
        arrival_action: parse_arrival_action(
            value.get("arrival_action"),
            value.get("target").and_then(parse_node_position),
        )?,
    }))
}

pub(super) fn parse_route_position(value: &serde_json::Value) -> Option<Vec3> {
    let (x, y, z) = if let Some(values) = value.as_array() {
        if values.len() < 3 {
            return None;
        }
        (
            values[0].as_f64()?,
            values[1].as_f64()?,
            values[2].as_f64()?,
        )
    } else {
        (
            value.get("x")?.as_f64()?,
            value.get("y")?.as_f64()?,
            value.get("z")?.as_f64()?,
        )
    };
    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
        return None;
    }
    Some(Vec3 {
        x: x as f32 * BS,
        y: y as f32 * BS,
        z: z as f32 * BS,
    })
}

pub(super) fn parse_arrival_action(
    value: Option<&serde_json::Value>,
    route_target: Option<IVec3>,
) -> std::result::Result<Option<ArrivalAction>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let action_type = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    match action_type {
        "none" => Ok(None),
        "collect" => {
            let node = value
                .get("node")
                .and_then(serde_json::Value::as_str)
                .filter(|node| valid_command_atom(node))
                .ok_or_else(|| "collect arrival action has no valid node".to_string())?;
            let count = value
                .get("count")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 8) as u16;
            let radius = value
                .get("radius")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(6)
                .clamp(1, 6) as i32;
            Ok(Some(ArrivalAction::Collect {
                node: node.to_string(),
                count,
                radius,
                target: route_target,
            }))
        }
        "hunt" => {
            let hunt_id = value
                .get("hunt_id")
                .and_then(serde_json::Value::as_u64)
                .ok_or_else(|| "hunt arrival action has no hunt_id".to_string())?;
            Ok(Some(ArrivalAction::Hunt { hunt_id }))
        }
        other => Err(format!("unsupported arrival action '{other}'")),
    }
}

pub(super) fn positions_nearly_equal(a: Vec3, b: Vec3) -> bool {
    (a.x - b.x).abs() <= 0.05 && (a.y - b.y).abs() <= 0.05 && (a.z - b.z).abs() <= 0.05
}

pub(super) fn waypoint_reached(position: Vec3, waypoint: Vec3, stop_dist: f32) -> bool {
    let dx = waypoint.x - position.x;
    let dz = waypoint.z - position.z;
    (dx * dx + dz * dz).sqrt() <= stop_dist && (waypoint.y - position.y).abs() <= 5.0
}

pub(super) fn route_response_status(payload: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("status")?
        .as_str()
        .map(str::to_string)
}

pub(super) fn invalid_route_response(error: &str) -> String {
    json!({
        "ok": false,
        "status": "invalid_path_response",
        "error": error,
    })
    .to_string()
}

pub(super) fn send_hunt_arrival_action(conn: &mut MtpConnection, hunt_id: u64) -> Result<()> {
    conn.send_chat_message(&format!("/bot_hunt {hunt_id}"))
}
