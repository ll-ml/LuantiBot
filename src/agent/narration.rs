use serde_json::Value;

use super::decision::ToolCall;

pub(super) fn action_narration(call: &ToolCall) -> Option<String> {
    let args = &call.arguments;
    let string = |key: &str| args.get(key).and_then(Value::as_str);
    let number = |key: &str| args.get(key).and_then(Value::as_f64);
    match call.name.trim() {
        "move" => Some(format!(
            "Moving {} for {:.1} step(s).",
            string("direction").unwrap_or("forward"),
            number("steps").unwrap_or(1.0)
        )),
        "move_to" => Some(format!(
            "Heading to ({:.0}, {:.0}, {:.0}).",
            number("x").unwrap_or(0.0),
            number("y").unwrap_or(0.0),
            number("z").unwrap_or(0.0)
        )),
        "navigate_node" => Some(format!(
            "Pathing to nearby {}.",
            string("node").unwrap_or("resource")
        )),
        "follow" => Some(format!(
            "Following {}.",
            string("target").unwrap_or("the player")
        )),
        "stop" => Some("Stopping here.".to_string()),
        "teleport" => Some(format!(
            "Teleporting to {}.",
            string("target").unwrap_or("the player")
        )),
        "attack" | "approach" | "interact" | "fight" => Some(format!(
            "{} {}.",
            match call.name.trim() {
                "attack" => "Attacking",
                "approach" => "Approaching",
                "interact" => "Interacting with",
                _ => "Fighting",
            },
            string("target").unwrap_or("the target")
        )),
        "defend" => Some("Defending against the nearest hostile.".to_string()),
        "hunt_food" => Some(format!(
            "Hunting one safe food animal: {}.",
            string("target").unwrap_or("the target")
        )),
        "sleep" => Some("Looking for a bed and trying to sleep.".to_string()),
        "mine" => Some(coordinate_narration(args, "Mining the block")),
        "collect_blocks" => Some(format!(
            "Collecting up to {} nearby {} block(s).",
            args.get("count").and_then(Value::as_i64).unwrap_or(1),
            string("node").unwrap_or("resource")
        )),
        "gather_resource" => Some(format!(
            "Pathing to gather up to {} {} block(s).",
            args.get("count").and_then(Value::as_i64).unwrap_or(1),
            string("node").unwrap_or("resource")
        )),
        "collect_item" => Some(format!(
            "Walking over to pick up {}.",
            string("item").unwrap_or("the dropped item")
        )),
        "place" => Some(coordinate_narration(args, "Placing the wielded block")),
        "deposit_item" | "withdraw_item" => Some(format!(
            "{} {} {}.",
            if call.name.trim() == "deposit_item" {
                "Depositing"
            } else {
                "Withdrawing"
            },
            args.get("count").and_then(Value::as_i64).unwrap_or(1),
            string("item").unwrap_or("item")
        )),
        "load_furnace" => Some(format!(
            "Loading {} and {} into the furnace.",
            string("input").unwrap_or("an input"),
            string("fuel").unwrap_or("fuel")
        )),
        "collect_furnace_output" => Some(format!(
            "Collecting {} {} from the furnace.",
            args.get("count").and_then(Value::as_i64).unwrap_or(1),
            string("item").unwrap_or("finished item(s)")
        )),
        "craft_item" => Some(format!(
            "Crafting {} {}.",
            args.get("count").and_then(Value::as_i64).unwrap_or(1),
            string("item").unwrap_or("item(s)")
        )),
        "wield" => Some(format!(
            "Wielding {}.",
            string("item").unwrap_or("an item")
        )),
        "use_item" | "use" => Some(format!(
            "Using {}.",
            string("item").unwrap_or("the wielded item")
        )),
        "drop_item" | "drop" => Some(format!(
            "Dropping {}.",
            string("item").unwrap_or("an item")
        )),
        _ => None,
    }
}

fn coordinate_narration(args: &Value, action: &str) -> String {
    match (
        args.get("x").and_then(Value::as_i64),
        args.get("y").and_then(Value::as_i64),
        args.get("z").and_then(Value::as_i64),
    ) {
        (Some(x), Some(y), Some(z)) => format!("{action} at ({x}, {y}, {z})."),
        _ => format!("{action} in front of me."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn narration_summarizes_actions_without_model_text() {
        let call = ToolCall {
            id: "1".to_string(),
            name: "follow".to_string(),
            arguments: json!({"target":"Alice"}),
        };
        assert_eq!(action_narration(&call).as_deref(), Some("Following Alice."));
        assert!(action_narration(&ToolCall {
            id: "2".to_string(),
            name: "say".to_string(),
            arguments: json!({"message":"hi"}),
        })
        .is_none());
    }
}
