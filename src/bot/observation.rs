//! Local voxel observations and controller-state enrichment.

use serde_json::json;
use std::collections::HashMap;

use crate::game::BS;
use crate::types::{IVec3, Vec3};
use crate::world::World;
use super::chat::json_escape;
use super::entities::RemotePlayer;
use super::navigation::{ArrivalAction, MoveGoal, NavigationSnapshot};

pub(super) fn build_observe_json(
    pos: Vec3,
    yaw: f32,
    radius: i32,
    world: &World,
    players: &HashMap<u16, RemotePlayer>,
    follow_enabled: bool,
    follow_target: Option<&str>,
    move_goal: Option<&MoveGoal>,
    navigation: &NavigationSnapshot,
) -> String {
    let node_pos = IVec3 {
        x: (pos.x / 10.0).floor() as i32,
        y: (pos.y / 10.0).floor() as i32,
        z: (pos.z / 10.0).floor() as i32,
    };

    let facing = facing_from_yaw(yaw);
    let front = offset_from_facing(facing);
    let left = offset_from_facing(turn_left(facing));
    let right = offset_from_facing(turn_right(facing));
    let back = offset_from_facing(turn_back(facing));

    let obstacles = [
        ("front", front),
        ("left", left),
        ("right", right),
        ("back", back),
    ];

    let mut out = String::new();
    out.push('{');
    out.push_str("\"health\":null,");
    out.push_str(&format!(
        "\"position\":[{},{},{}],",
        node_pos.x, node_pos.y, node_pos.z
    ));
    out.push_str(&format!("\"facing\":\"{}\",", facing));

    out.push_str("\"nodes\":[");
    let mut first_node = true;
    for y in (node_pos.y - radius)..=(node_pos.y + radius) {
        for x in (node_pos.x - radius)..=(node_pos.x + radius) {
            for z in (node_pos.z - radius)..=(node_pos.z + radius) {
                let pos = IVec3 { x, y, z };
                let name = match world.get_node(pos) {
                    Some(node) => world.node_name(node),
                    None => "unknown".to_string(),
                };
                if !first_node {
                    out.push(',');
                }
                first_node = false;
                out.push_str(&format!(
                    "{{\"pos\":[{},{},{}],\"name\":\"{}\"}}",
                    x,
                    y,
                    z,
                    json_escape(&name)
                ));
            }
        }
    }
    out.push_str("],");

    out.push_str("\"hostiles\":[");
    out.push_str("],");

    out.push_str("\"items\":[");
    let mut first_item = true;
    for info in players.values() {
        if follow_enabled {
            if let Some(target) = follow_target {
                if info.name == target {
                    continue;
                }
            }
        }
        let dx = ((info.pos.x / 10.0).floor() as i32) - node_pos.x;
        let dy = ((info.pos.y / 10.0).floor() as i32) - node_pos.y;
        let dz = ((info.pos.z / 10.0).floor() as i32) - node_pos.z;
        if dx.abs() > radius || dy.abs() > radius || dz.abs() > radius {
            continue;
        }
        if !first_item {
            out.push(',');
        }
        first_item = false;
        out.push_str(&format!(
            "{{\"type\":\"player\",\"name\":\"{}\",\"dx\":{},\"dy\":{},\"dz\":{}}}",
            json_escape(&info.name),
            dx,
            dy,
            dz
        ));
    }
    out.push_str("],");

    out.push_str("\"obstacles\":{");
    let mut first_obs = true;
    for (label, delta) in obstacles {
        if !first_obs {
            out.push(',');
        }
        first_obs = false;
        let pos = IVec3 {
            x: node_pos.x + delta.0,
            y: node_pos.y,
            z: node_pos.z + delta.1,
        };
        let value = obstacle_kind(world, pos);
        out.push_str(&format!("\"{}\":\"{}\"", label, value));
    }
    out.push_str("},");

    out.push_str("\"controller\":{");
    out.push_str(&format!("\"follow_enabled\":{},", follow_enabled));
    if let Some(target) = follow_target {
        out.push_str(&format!(
            "\"follow_target\":\"{}\",",
            json_escape(target)
        ));
    } else {
        out.push_str("\"follow_target\":null,");
    }
    out.push_str(&format!("\"move_active\":{},", move_goal.is_some()));
    if let Some(goal) = move_goal {
        out.push_str(&format!(
            "\"move_target\":[{:.2},{:.2},{:.2}],",
            goal.target.x / 10.0,
            goal.target.y / 10.0,
            goal.target.z / 10.0
        ));
    } else {
        out.push_str("\"move_target\":null,");
    }
    out.push_str("\"navigation\":");
    out.push_str(&navigation_observation(navigation, move_goal).to_string());
    out.push_str("},");

    if follow_enabled {
        if let Some(target) = follow_target {
            out.push_str(&format!("\"goal\":\"follow {}\"", json_escape(target)));
        } else {
            out.push_str("\"goal\":\"follow\"");
        }
    } else {
        out.push_str("\"goal\":\"idle\"");
    }
    out.push('}');
    out
}

pub(super) fn facing_from_yaw(yaw: f32) -> &'static str {
    let x = -yaw.sin();
    let z = yaw.cos();
    if x.abs() > z.abs() {
        if x > 0.0 {
            "east"
        } else {
            "west"
        }
    } else if z > 0.0 {
        "north"
    } else {
        "south"
    }
}

pub(super) fn turn_left(facing: &str) -> &'static str {
    match facing {
        "north" => "west",
        "west" => "south",
        "south" => "east",
        "east" => "north",
        _ => "north",
    }
}

pub(super) fn turn_right(facing: &str) -> &'static str {
    match facing {
        "north" => "east",
        "east" => "south",
        "south" => "west",
        "west" => "north",
        _ => "north",
    }
}

pub(super) fn turn_back(facing: &str) -> &'static str {
    match facing {
        "north" => "south",
        "south" => "north",
        "east" => "west",
        "west" => "east",
        _ => "south",
    }
}

pub(super) fn offset_from_facing(facing: &str) -> (i32, i32) {
    match facing {
        "north" => (0, 1),
        "south" => (0, -1),
        "east" => (1, 0),
        "west" => (-1, 0),
        _ => (0, 1),
    }
}

pub(super) fn obstacle_kind(world: &World, pos: IVec3) -> &'static str {
    match world.get_node(pos) {
        Some(node) if world.is_air_or_ignore(node) => "air",
        Some(_) => "solid",
        None => "unknown",
    }
}

pub(super) fn enrich_server_observation(
    raw: &str,
    follow_enabled: bool,
    follow_target: Option<&str>,
    move_goal: Option<&MoveGoal>,
    navigation: &NavigationSnapshot,
) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return raw.to_string();
    };
    let move_target = move_goal.map(|goal| {
        json!([
            goal.target.x / 10.0,
            goal.target.y / 10.0,
            goal.target.z / 10.0
        ])
    });
    value["controller"] = json!({
        "follow_enabled": follow_enabled,
        "follow_target": follow_target,
        "move_active": move_goal.is_some(),
        "move_target": move_target,
        "navigation": navigation_observation(navigation, move_goal),
    });
    value.to_string()
}

pub(super) fn navigation_observation(
    navigation: &NavigationSnapshot,
    move_goal: Option<&MoveGoal>,
) -> serde_json::Value {
    let current_waypoint = move_goal.map(|goal| {
        let waypoint = goal.current_waypoint();
        json!([waypoint.x / BS, waypoint.y / BS, waypoint.z / BS])
    });
    let arrival_action = move_goal
        .and_then(|goal| goal.arrival_action.as_ref())
        .map(ArrivalAction::kind);
    json!({
        "status": navigation.status,
        "current_waypoint": current_waypoint,
        "waypoints_remaining": move_goal.map(MoveGoal::remaining_waypoints).unwrap_or(0),
        "recovering": navigation.recovering,
        "recovery_attempts": navigation.recovery_attempts,
        "stalled_for_seconds": navigation.stalled_for_seconds,
        "last_error": navigation.last_error,
        "arrival_action": arrival_action,
    })
}
