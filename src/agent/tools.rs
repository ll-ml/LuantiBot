use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::Url;
use serde_json::{json, Value};

use super::api_client::{post, post_query};
use super::provider::ToolCall;
use super::planner::{item_is_low_value, low_value_request_is_authorized, node_is_low_value};
use super::state::{
    AgentState, CraftableItemView, FurnaceView, GoalStatus, ObservationSnapshot,
};
use super::util::clip_chars;

#[derive(Clone, Debug)]
pub struct ToolExecution {
    pub name: String,
    pub ok: bool,
    pub result: String,
    pub external_action: bool,
}

pub fn tool_definitions(passive: bool, autonomous: bool) -> Vec<Value> {
    let goal_description = if autonomous {
        "Set the durable player mission. Autonomous short-term work belongs in set_objective instead."
    } else {
        "Set the current longer-lived goal from a player-assigned task. Self-directed goals are disabled."
    };
    let mut tools = vec![
        function(
            "say",
            "Send one short in-game chat message. Use for useful replies, not routine narration.",
            object_schema(
                json!({"message":{"type":"string","minLength":1,"maxLength":300}}),
                &["message"],
            ),
        ),
        function(
            "set_goal",
            goal_description,
            object_schema(
                json!({
                    "description":{"type":"string","minLength":1,"maxLength":500},
                    "success_criteria":{"type":["string","null"],"maxLength":500}
                }),
                &["description", "success_criteria"],
            ),
        ),
        function(
            "finish_goal",
            "Finish the current goal only when it is completed, impossible, or explicitly cancelled.",
            object_schema(
                json!({
                    "status":{"type":"string","enum":["completed","failed","cancelled"]},
                    "outcome":{"type":["string","null"],"maxLength":500}
                }),
                &["status", "outcome"],
            ),
        ),
    ];
    if autonomous {
        tools.extend([
            function(
                "set_objective",
                "Set one small autonomous objective without replacing the durable mission. It must be grounded in the observation and finish within the action budget.",
                object_schema(
                    json!({
                        "description":{"type":"string","minLength":1,"maxLength":500},
                        "success_criteria":{"type":["string","null"],"maxLength":500},
                        "action_budget":{"type":"integer","minimum":2,"maximum":12}
                    }),
                    &["description", "success_criteria", "action_budget"],
                ),
            ),
            function(
                "finish_objective",
                "Finish only the current autonomous objective. This does not finish or replace the durable player mission.",
                object_schema(
                    json!({
                        "status":{"type":"string","enum":["completed","failed","cancelled"]},
                        "outcome":{"type":["string","null"],"maxLength":500}
                    }),
                    &["status", "outcome"],
                ),
            ),
        ]);
    }
    if passive {
        return tools;
    }
    tools.extend([
        function("stop", "Stop current movement or following.", object_schema(json!({}), &[])),
        function(
            "move",
            "Move a small number of nodes relative to the bot's current facing.",
            object_schema(
                json!({
                    "direction":{"type":"string","enum":["forward","backward","left","right"]},
                    "steps":{"type":"number","minimum":0.5,"maximum":8.0}
                }),
                &["direction", "steps"],
            ),
        ),
        function(
            "move_to",
            "Move toward an absolute nearby node position. Keep destinations within 32 nodes.",
            object_schema(
                json!({
                    "x":{"type":"number"},"y":{"type":"number"},"z":{"type":"number"}
                }),
                &["x", "y", "z"],
            ),
        ),
        function(
            "navigate_node",
            "Ask the path controller to find and walk to a reachable block with an exact name from observation.nearby_nodes. Prefer this over low-level movement when approaching a resource.",
            object_schema(
                json!({
                    "node":{"type":"string","minLength":1,"description":"Exact name from observation.nearby_nodes"},
                    "radius":{"type":"integer","minimum":2,"maximum":24}
                }),
                &["node", "radius"],
            ),
        ),
        function(
            "follow",
            "Start or continue following a named player. The player may be named by the active goal, authorized_player_names, conversation history, or current observation; they need not be inside the short observation radius.",
            target_schema("player name"),
        ),
        function(
            "attack",
            "Attack a player currently present in observation.players.",
            target_schema("player name"),
        ),
        function(
            "defend",
            "Immediately attack the nearest hostile from observation.hostiles with a short defensive combo. Use this for self-defense; it never targets players or passive mobs.",
            object_schema(json!({}), &[]),
        ),
        function(
            "approach",
            "Move close to an observed player, hostile, or passive/neutral mob.",
            target_schema("exact observed player or entity name"),
        ),
        function(
            "interact",
            "Right-click an observed player, hostile, or passive/neutral mob. Use only when the held item and active goal make the interaction appropriate.",
            target_schema("exact observed player or entity name"),
        ),
        function(
            "fight",
            "Fight an observed player or hostile entity.",
            target_schema("observed player name or hostile entity type"),
        ),
        function(
            "hunt_food",
            "Path to and hunt one observed passive animal only when observation marks both food_source=true and safe_to_hunt=true. Never use this on pets, babies, NPCs, neutral mobs, or unmarked entities.",
            object_schema(
                json!({
                    "target":{"type":"string","minLength":1,"description":"Exact eligible entity name from observation.mobs"},
                    "radius":{"type":"integer","minimum":2,"maximum":24}
                }),
                &["target", "radius"],
            ),
        ),
        function(
            "teleport",
            "Teleport to a currently observed player. Use only when walking is impractical and server privileges allow it.",
            target_schema("observed player name"),
        ),
        function(
            "sleep",
            "Use the nearest bed within the requested radius.",
            object_schema(json!({"radius":{"type":"integer","minimum":1,"maximum":20}}), &["radius"]),
        ),
        function(
            "mine",
            "Mine one block in front for a direct player request, or one specific observed block within six nodes. Autonomous calls must provide the exact observed x/y/z so resource policy can validate the target. Prefer collect_blocks when gathering several blocks.",
            coordinate_schema(),
        ),
        function(
            "collect_blocks",
            "Mine up to eight nearby blocks of one exact observed node name and collect their drops. Repeat after observing again when the goal needs more.",
            object_schema(
                json!({
                    "node":{"type":"string","minLength":1,"description":"Exact name from observation.nearby_nodes"},
                    "count":{"type":"integer","minimum":1,"maximum":8}
                }),
                &["node", "count"],
            ),
        ),
        function(
            "gather_resource",
            "Ask the path controller to locate, walk to, and gather a bounded quantity of one exact resource from observation.nearby_nodes.",
            object_schema(
                json!({
                    "node":{"type":"string","minLength":1,"description":"Exact name from observation.nearby_nodes"},
                    "count":{"type":"integer","minimum":1,"maximum":8},
                    "radius":{"type":"integer","minimum":2,"maximum":24}
                }),
                &["node", "count", "radius"],
            ),
        ),
        function(
            "collect_item",
            "Walk to a dropped item from observation.nearby_items so normal game pickup can collect it.",
            object_schema(
                json!({"item":{"type":"string","minLength":1,"description":"Exact item name from observation.nearby_items"}}),
                &["item"],
            ),
        ),
        function(
            "place",
            "Place the wielded block in front, or at a specific empty position within six nodes.",
            coordinate_schema(),
        ),
        function(
            "deposit_item",
            "Deposit an exact item/count from inventory into an accessible supported chest or barrel in observation.chests. Use the container's exact integer coordinates. Do not store valuables without a relevant goal or player request.",
            chest_transfer_schema("Exact item name from observed inventory"),
        ),
        function(
            "withdraw_item",
            "Withdraw an exact item/count listed in an accessible supported chest or barrel in observation.chests. Use the container's exact integer coordinates.",
            chest_transfer_schema("Exact item name from the observed chest contents"),
        ),
        function(
            "load_furnace",
            "Load one exact cookable input and one exact fuel from the same accessible entry in observation.furnaces. Both must be listed in that furnace's input_options/fuel_options; do not guess or reload an occupied furnace.",
            furnace_load_schema(),
        ),
        function(
            "collect_furnace_output",
            "Collect an exact item/count currently present in the output of an accessible observed furnace, smoker, or blast furnace.",
            furnace_collect_schema(),
        ),
        function(
            "craft_item",
            "Craft an exact output currently listed in observation.crafting.craftable. The server selects and validates its registered recipe; do not guess unavailable outputs.",
            object_schema(
                json!({
                    "item":{"type":"string","minLength":1,"description":"Exact item from observation.crafting.craftable"},
                    "count":{"type":"integer","minimum":1,"maximum":64}
                }),
                &["item", "count"],
            ),
        ),
        function(
            "drop_item",
            "Drop an item that is present in inventory. Omit item to drop the wielded stack.",
            object_schema(
                json!({
                    "item":{"type":["string","null"]},
                    "count":{"type":["integer","null"],"minimum":1,"maximum":99}
                }),
                &["item", "count"],
            ),
        ),
        function(
            "wield",
            "Wield an item present in inventory, using its exact item name.",
            object_schema(json!({"item":{"type":"string","minLength":1}}), &["item"]),
        ),
        function(
            "use_item",
            "Use the wielded item, or first wield and use an exact item name from inventory.",
            object_schema(json!({"item":{"type":["string","null"]}}), &["item"]),
        ),
    ]);
    tools
}

pub fn execute_tool(
    client: &Client,
    api_base: &Url,
    api_token: &str,
    state: &mut AgentState,
    call: &ToolCall,
    passive: bool,
    autonomous: bool,
    player_instruction_turn: bool,
) -> ToolExecution {
    let name = normalize_tool_name(&call.name);
    let external_action = !matches!(
        name,
        "say" | "set_goal" | "finish_goal" | "set_objective" | "finish_objective"
    );
    if passive
        && !matches!(
            name,
            "say" | "set_goal" | "finish_goal" | "set_objective" | "finish_objective"
        )
    {
        return failure(name, "tool is disabled in passive mode", external_action);
    }
    let safety_action = matches!(name, "stop" | "defend");
    if external_action && !safety_action {
        if state.objective_budget_exhausted() {
            return failure(
                name,
                "objective_action_budget_exhausted: call finish_objective with the observed outcome before choosing another objective",
                true,
            );
        }
        if let Some(reason) = state.repeated_failure_reason(name, &call.arguments) {
            return failure(name, &reason, true);
        }
    }
    state.transition(
        super::state::AgentPhase::Acting,
        format!("executing tool {name}"),
    );
    match execute_checked(
        client,
        api_base,
        api_token,
        state,
        name,
        &call.arguments,
        autonomous,
        player_instruction_turn,
    ) {
        Ok(result) => ToolExecution {
            name: name.to_string(),
            ok: true,
            result,
            external_action,
        },
        Err(error) => failure(name, &format!("{error:#}"), external_action),
    }
}

fn execute_checked(
    client: &Client,
    api_base: &Url,
    api_token: &str,
    state: &mut AgentState,
    name: &str,
    args: &Value,
    autonomous: bool,
    player_instruction_turn: bool,
) -> Result<String> {
    if state.observation.health == Some(0)
        && !matches!(
            name,
            "stop" | "say" | "set_goal" | "finish_goal" | "set_objective" | "finish_objective"
        )
    {
        bail!("bot is dead; only stop, say, and goal-management tools are allowed");
    }
    match name {
        "set_goal" => {
            anyhow::ensure!(
                player_instruction_turn,
                "durable goals require a current player instruction; use set_objective for autonomous work"
            );
            let description = required_string(args, "description")?;
            if item_is_low_value(&state.observation, &description)
                && !low_value_request_is_authorized(
                    state,
                    &description,
                    &[],
                    player_instruction_turn,
                )
            {
                bail!(
                    "low-value durable goal rejected: the current player message did not explicitly request dirt/soil/sand/leaves/flora/plants"
                );
            }
            let success = optional_string(args, "success_criteria");
            let id = state.set_goal(&description, success.as_deref())?;
            Ok(format!("goal {id} is now active"))
        }
        "finish_goal" => {
            let status = goal_status(args)?;
            let outcome = optional_string(args, "outcome");
            state.finish_goal(status, outcome.as_deref())?;
            Ok("goal archived".to_string())
        }
        "set_objective" => {
            anyhow::ensure!(autonomous, "autonomous objectives are disabled");
            let description = required_string(args, "description")?;
            let success = optional_string(args, "success_criteria");
            let action_budget = integer(args, "action_budget")?.clamp(2, 12) as u32;
            let id = state.set_objective(&description, success.as_deref(), action_budget)?;
            Ok(format!("objective {id} is now active"))
        }
        "finish_objective" => {
            let status = goal_status(args)?;
            let outcome = optional_string(args, "outcome");
            state.finish_objective(status, outcome.as_deref())?;
            Ok("objective archived; durable mission is unchanged".to_string())
        }
        "stop" => post(client, api_base, api_token, "/stop", None),
        "say" => {
            let message = clip_chars(&required_string(args, "message")?, 300);
            post(
                client,
                api_base,
                api_token,
                "/chat",
                Some(json!({"message": message})),
            )
        }
        "move" => {
            let direction = required_string(args, "direction")?.to_ascii_lowercase();
            if !matches!(direction.as_str(), "forward" | "backward" | "left" | "right") {
                bail!("invalid movement direction");
            }
            let steps = number(args, "steps")?.clamp(0.5, 8.0);
            post_query(
                client,
                api_base,
                api_token,
                "/move",
                &[("direction", direction), ("steps", steps.to_string())],
            )
        }
        "move_to" => {
            let x = number(args, "x")?;
            let y = number(args, "y")?;
            let z = number(args, "z")?;
            ensure_nearby(state.observation.position, [x, y, z], 32.0)?;
            post_query(
                client,
                api_base,
                api_token,
                "/move_to",
                &[("x", x.to_string()), ("y", y.to_string()), ("z", z.to_string())],
            )
        }
        "navigate_node" => {
            let requested = required_string(args, "node")?;
            let node = nearest_observed_node(&state.observation, &requested)?;
            let radius = integer(args, "radius")?.clamp(2, 24);
            post_query(
                client,
                api_base,
                api_token,
                "/navigate_node",
                &[("node", node.name.clone()), ("radius", radius.to_string())],
            )
        }
        "follow" => {
            let target = valid_player_name(&required_string(args, "target")?)?;
            post_query(client, api_base, api_token, "/follow", &[("target", target)])
        }
        "attack" | "teleport" => {
            let target = required_string(args, "target")?;
            let target = observed_player(&state.observation, &target)?;
            let path = match name {
                "attack" => "/attack",
                _ => "/teleport",
            };
            post_query(client, api_base, api_token, path, &[("target", target)])
        }
        "defend" => {
            if state.observation.hostiles.is_empty() {
                bail!("no hostile mob is currently observed");
            }
            post_query(client, api_base, api_token, "/defend", &[])
        }
        "approach" | "interact" | "fight" => {
            let target = required_string(args, "target")?;
            let target = if name == "fight" {
                observed_combat_target(&state.observation, &target)?
            } else {
                observed_entity_target(&state.observation, &target)?
            };
            let path = match name {
                "approach" => "/approach",
                "interact" => "/interact",
                _ => "/fight",
            };
            post_query(client, api_base, api_token, path, &[("target", target)])
        }
        "hunt_food" => {
            let requested = required_string(args, "target")?;
            let target = observed_food_source(&state.observation, &requested)?;
            let radius = integer(args, "radius")?.clamp(2, 24);
            post_query(
                client,
                api_base,
                api_token,
                "/hunt_food",
                &[("target", target), ("radius", radius.to_string())],
            )
        }
        "sleep" => {
            let radius = integer(args, "radius")?.clamp(1, 20);
            post_query(
                client,
                api_base,
                api_token,
                "/sleep",
                &[("radius", radius.to_string())],
            )
        }
        "mine" | "place" => {
            let path = if name == "mine" { "/mine" } else { "/place" };
            let coordinates = optional_coordinates(args)?;
            if let Some([x, y, z]) = coordinates {
                ensure_nearby(
                    state.observation.position,
                    [x as f64, y as f64, z as f64],
                    6.0,
                )?;
                if name == "mine" {
                    let node = state
                        .observation
                        .nearby_nodes
                        .iter()
                        .find(|node| {
                            [
                                i64::from(node.pos[0]),
                                i64::from(node.pos[1]),
                                i64::from(node.pos[2]),
                            ] == [x, y, z]
                        })
                        .with_context(|| {
                            format!("target ({x},{y},{z}) is not an observed diggable block")
                        })?;
                    ensure_autonomous_node_allowed(
                        state,
                        node,
                        autonomous,
                        player_instruction_turn,
                    )?;
                }
                post_query(
                    client,
                    api_base,
                    api_token,
                    path,
                    &[("x", x.to_string()), ("y", y.to_string()), ("z", z.to_string())],
                )
            } else {
                if name == "mine" && autonomous {
                    bail!(
                        "autonomous mine requires exact observed x/y/z coordinates so resource policy can validate the target"
                    );
                }
                post(client, api_base, api_token, path, None)
            }
        }
        "collect_blocks" => {
            let requested = required_string(args, "node")?;
            let node = nearest_observed_node(&state.observation, &requested)?;
            ensure_autonomous_node_allowed(
                state,
                node,
                autonomous,
                player_instruction_turn,
            )?;
            let count = integer(args, "count")?.clamp(1, 8);
            let distance = node_distance(state.observation.position, node.pos);
            anyhow::ensure!(
                distance <= 6.0,
                "nearest '{requested}' block is {distance:.1} nodes away; move closer first"
            );
            post_query(
                client,
                api_base,
                api_token,
                "/collect",
                &[
                    ("node", node.name.clone()),
                    ("count", count.to_string()),
                    ("radius", "6".to_string()),
                ],
            )
        }
        "gather_resource" => {
            let requested = required_string(args, "node")?;
            let node = nearest_observed_node(&state.observation, &requested)?;
            ensure_autonomous_node_allowed(
                state,
                node,
                autonomous,
                player_instruction_turn,
            )?;
            let count = integer(args, "count")?.clamp(1, 8);
            let radius = integer(args, "radius")?.clamp(2, 24);
            post_query(
                client,
                api_base,
                api_token,
                "/gather_resource",
                &[
                    ("node", node.name.clone()),
                    ("count", count.to_string()),
                    ("radius", radius.to_string()),
                ],
            )
        }
        "collect_item" => {
            let requested = required_string(args, "item")?;
            ensure_autonomous_item_allowed(
                state,
                &requested,
                autonomous,
                player_instruction_turn,
            )?;
            let item = state
                .observation
                .nearby_items
                .iter()
                .filter(|item| item.name.as_deref().is_some_and(|name| name.eq_ignore_ascii_case(&requested)))
                .min_by_key(|item| item.dx.abs() + item.dy.abs() + item.dz.abs())
                .with_context(|| format!("dropped item '{requested}' is not currently observed"))?;
            let target = [
                state.observation.position[0] + item.dx,
                state.observation.position[1] + item.dy,
                state.observation.position[2] + item.dz,
            ];
            post_query(
                client,
                api_base,
                api_token,
                "/move_to",
                &[
                    ("x", target[0].to_string()),
                    ("y", target[1].to_string()),
                    ("z", target[2].to_string()),
                ],
            )
        }
        "deposit_item" | "withdraw_item" => {
            let [x, y, z] = required_i32_coordinates(args)?;
            let chest = observed_chest(&state.observation, [x, y, z])?;
            anyhow::ensure!(chest.accessible, "chest is not accessible: {}", chest.status);
            anyhow::ensure!(chest.distance <= 6.0, "chest is {:.1} nodes away", chest.distance);
            let requested = required_string(args, "item")?;
            let count = integer(args, "count")?.clamp(1, 99) as u64;
            let item = if name == "deposit_item" {
                let item = observed_inventory_item(&state.observation, &requested)?;
                let available = inventory_count(&state.observation, &item);
                anyhow::ensure!(
                    available >= count,
                    "requested {count} of '{item}', but only {available} observed in inventory"
                );
                item
            } else {
                let observed = chest
                    .contents
                    .iter()
                    .find(|item| item.name.eq_ignore_ascii_case(&requested))
                    .with_context(|| {
                        format!("item '{requested}' is not listed in the observed chest")
                    })?;
                anyhow::ensure!(
                    u64::from(observed.count) >= count,
                    "requested {count} of '{}', but only {} observed in chest",
                    observed.name,
                    observed.count
                );
                observed.name.clone()
            };
            let path = if name == "deposit_item" {
                "/chest/deposit"
            } else {
                "/chest/withdraw"
            };
            post_query(
                client,
                api_base,
                api_token,
                path,
                &[
                    ("x", x.to_string()),
                    ("y", y.to_string()),
                    ("z", z.to_string()),
                    ("item", item),
                    ("count", count.to_string()),
                ],
            )
        }
        "load_furnace" => {
            let [x, y, z] = required_i32_coordinates(args)?;
            let furnace = observed_furnace(&state.observation, [x, y, z])?;
            ensure_accessible_furnace(furnace)?;
            anyhow::ensure!(
                furnace.input.is_none(),
                "furnace input is already occupied; wait, collect output, or use a different furnace"
            );
            anyhow::ensure!(
                furnace.fuel.is_none(),
                "furnace fuel slot is already occupied; do not autonomously stack more fuel"
            );
            anyhow::ensure!(
                !furnace.output_blocked,
                "furnace output is blocked; collect or clear it before loading more input"
            );
            let requested_input = required_string(args, "input")?;
            let input_count = integer(args, "input_count")?.clamp(1, 99) as u64;
            let input = furnace
                .input_options
                .iter()
                .find(|option| option.name.eq_ignore_ascii_case(&requested_input))
                .with_context(|| {
                    format!(
                        "input '{requested_input}' is not listed in this furnace's input_options"
                    )
                })?;
            anyhow::ensure!(
                u64::from(input.count) >= input_count,
                "requested {input_count} of '{}', but only {} is available",
                input.name,
                input.count
            );

            let requested_fuel = required_string(args, "fuel")?;
            let fuel_count = integer(args, "fuel_count")?.clamp(1, 99) as u64;
            let fuel = furnace
                .fuel_options
                .iter()
                .find(|option| option.name.eq_ignore_ascii_case(&requested_fuel))
                .with_context(|| {
                    format!("fuel '{requested_fuel}' is not listed in this furnace's fuel_options")
                })?;
            anyhow::ensure!(
                u64::from(fuel.count) >= fuel_count,
                "requested {fuel_count} of '{}', but only {} is available",
                fuel.name,
                fuel.count
            );
            anyhow::ensure!(
                fuel.replacement.is_none() || fuel_count == 1,
                "fuel '{}' leaves a replacement container; load exactly one at a time",
                fuel.name
            );
            post(
                client,
                api_base,
                api_token,
                "/furnace/load",
                Some(json!({
                    "pos":[x,y,z],
                    "input":input.name,
                    "input_count":input_count,
                    "fuel":fuel.name,
                    "fuel_count":fuel_count
                })),
            )
        }
        "collect_furnace_output" => {
            let [x, y, z] = required_i32_coordinates(args)?;
            let furnace = observed_furnace(&state.observation, [x, y, z])?;
            ensure_accessible_furnace(furnace)?;
            let requested = required_string(args, "item")?;
            let count = integer(args, "count")?.clamp(1, 99) as u64;
            let output = furnace
                .output
                .as_ref()
                .filter(|output| output.name.eq_ignore_ascii_case(&requested))
                .with_context(|| {
                    format!("item '{requested}' is not in this furnace's observed output")
                })?;
            anyhow::ensure!(
                u64::from(output.count) >= count,
                "requested {count} of '{}', but only {} is in the furnace output",
                output.name,
                output.count
            );
            post(
                client,
                api_base,
                api_token,
                "/furnace/collect",
                Some(json!({"pos":[x,y,z],"item":output.name,"count":count})),
            )
        }
        "craft_item" => {
            let requested = required_string(args, "item")?;
            let count = integer(args, "count")?.clamp(1, 64) as u64;
            let craftable = observed_craftable(&state.observation, &requested)?;
            let available = u64::from(craftable.output_per_batch)
                .saturating_mul(u64::from(craftable.max_batches));
            anyhow::ensure!(
                available >= count,
                "requested {count} of '{}', but only {available} is currently craftable",
                craftable.item
            );
            post(
                client,
                api_base,
                api_token,
                "/craft",
                Some(json!({"item":craftable.item,"count":count})),
            )
        }
        "drop_item" => {
            let item = optional_string(args, "item");
            let count = args
                .get("count")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 99);
            if let Some(item) = item.as_deref() {
                let available = inventory_count(&state.observation, item);
                if available == 0 {
                    bail!("item '{item}' is not in observed inventory");
                }
                if count > available {
                    bail!("requested {count} of '{item}', but only {available} observed");
                }
            } else if state.observation.inventory.wield.is_none() {
                bail!("there is no observed wielded item to drop");
            }
            let mut query = vec![("count", count.to_string())];
            if let Some(item) = item {
                query.push(("item", item));
            }
            post_query(client, api_base, api_token, "/drop", &query)
        }
        "wield" => {
            let requested = required_string(args, "item")?;
            let item = observed_inventory_item(&state.observation, &requested)?;
            post_query(client, api_base, api_token, "/wield", &[("item", item)])
        }
        "use_item" => {
            let item = optional_string(args, "item");
            let mut query = Vec::new();
            if let Some(requested) = item {
                let item = observed_inventory_item(&state.observation, &requested)?;
                query.push(("item", item));
            } else if state.observation.inventory.wield.is_none() {
                bail!("there is no observed wielded item to use");
            }
            post_query(client, api_base, api_token, "/use", &query)
        }
        _ => bail!("unknown or unavailable tool '{name}'"),
    }
}

fn function(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "parameters": parameters,
        "strict": true
    })
}

fn object_schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn target_schema(description: &str) -> Value {
    object_schema(
        json!({"target":{"type":"string","description":description,"minLength":1}}),
        &["target"],
    )
}

fn coordinate_schema() -> Value {
    object_schema(
        json!({
            "x":{"type":["integer","null"]},
            "y":{"type":["integer","null"]},
            "z":{"type":["integer","null"]}
        }),
        &["x", "y", "z"],
    )
}

fn chest_transfer_schema(item_description: &str) -> Value {
    object_schema(
        json!({
            "x":{"type":"integer"},
            "y":{"type":"integer"},
            "z":{"type":"integer"},
            "item":{"type":"string","minLength":1,"description":item_description},
            "count":{"type":"integer","minimum":1,"maximum":99}
        }),
        &["x", "y", "z", "item", "count"],
    )
}

fn furnace_load_schema() -> Value {
    object_schema(
        json!({
            "x":{"type":"integer"},
            "y":{"type":"integer"},
            "z":{"type":"integer"},
            "input":{"type":"string","minLength":1,"description":"Exact item from this furnace's input_options"},
            "input_count":{"type":"integer","minimum":1,"maximum":99},
            "fuel":{"type":"string","minLength":1,"description":"Exact item from this furnace's fuel_options"},
            "fuel_count":{"type":"integer","minimum":1,"maximum":99}
        }),
        &["x", "y", "z", "input", "input_count", "fuel", "fuel_count"],
    )
}

fn furnace_collect_schema() -> Value {
    object_schema(
        json!({
            "x":{"type":"integer"},
            "y":{"type":"integer"},
            "z":{"type":"integer"},
            "item":{"type":"string","minLength":1,"description":"Exact item currently in this furnace's output"},
            "count":{"type":"integer","minimum":1,"maximum":99}
        }),
        &["x", "y", "z", "item", "count"],
    )
}

fn normalize_tool_name(name: &str) -> &str {
    match name.trim() {
        "drop" => "drop_item",
        "use" => "use_item",
        name => name,
    }
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    optional_string(value, key).with_context(|| format!("missing non-empty '{key}'"))
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn number(value: &Value, key: &str) -> Result<f64> {
    let number = value
        .get(key)
        .and_then(Value::as_f64)
        .with_context(|| format!("missing numeric '{key}'"))?;
    anyhow::ensure!(number.is_finite(), "'{key}' must be finite");
    Ok(number)
}

fn integer(value: &Value, key: &str) -> Result<i64> {
    value
        .get(key)
        .and_then(Value::as_i64)
        .with_context(|| format!("missing integer '{key}'"))
}

fn goal_status(value: &Value) -> Result<GoalStatus> {
    match required_string(value, "status")?.as_str() {
        "completed" => Ok(GoalStatus::Completed),
        "failed" => Ok(GoalStatus::Failed),
        "cancelled" => Ok(GoalStatus::Cancelled),
        _ => bail!("status must be completed, failed, or cancelled"),
    }
}

fn optional_coordinates(value: &Value) -> Result<Option<[i64; 3]>> {
    let coordinates = [value.get("x"), value.get("y"), value.get("z")];
    if coordinates.iter().all(|value| value.is_none_or(Value::is_null)) {
        return Ok(None);
    }
    if coordinates.iter().any(|value| value.is_none_or(Value::is_null)) {
        bail!("x, y, and z must be supplied together");
    }
    Ok(Some([
        integer(value, "x")?,
        integer(value, "y")?,
        integer(value, "z")?,
    ]))
}

fn required_i32_coordinates(value: &Value) -> Result<[i32; 3]> {
    let coordinates = [integer(value, "x")?, integer(value, "y")?, integer(value, "z")?];
    Ok([
        i32::try_from(coordinates[0]).context("x is outside the game coordinate range")?,
        i32::try_from(coordinates[1]).context("y is outside the game coordinate range")?,
        i32::try_from(coordinates[2]).context("z is outside the game coordinate range")?,
    ])
}

fn observed_player(observation: &ObservationSnapshot, requested: &str) -> Result<String> {
    observation
        .players
        .iter()
        .filter_map(|entity| entity.name.as_deref())
        .find(|name| name.eq_ignore_ascii_case(requested))
        .map(str::to_string)
        .with_context(|| format!("player '{requested}' is not currently observed"))
}

fn valid_player_name(requested: &str) -> Result<String> {
    let name = requested.trim();
    anyhow::ensure!(!name.is_empty(), "player name is empty");
    anyhow::ensure!(
        name.chars().count() <= 64
            && name.chars().all(|ch| !ch.is_control() && !ch.is_whitespace()),
        "invalid player name '{name}'"
    );
    Ok(name.to_string())
}

fn observed_combat_target(observation: &ObservationSnapshot, requested: &str) -> Result<String> {
    if let Ok(player) = observed_player(observation, requested) {
        return Ok(player);
    }
    observation
        .hostiles
        .iter()
        .find_map(|entity| {
            let name = entity.name.as_deref().unwrap_or(&entity.kind);
            (name.eq_ignore_ascii_case(requested) || entity.kind.eq_ignore_ascii_case(requested))
                .then(|| name.to_string())
        })
        .with_context(|| format!("target '{requested}' is not currently observed"))
}

fn observed_entity_target(observation: &ObservationSnapshot, requested: &str) -> Result<String> {
    if let Ok(target) = observed_combat_target(observation, requested) {
        return Ok(target);
    }
    observation
        .mobs
        .iter()
        .find_map(|entity| {
            let name = entity.name.as_deref().unwrap_or(&entity.kind);
            (name.eq_ignore_ascii_case(requested) || entity.kind.eq_ignore_ascii_case(requested))
                .then(|| name.to_string())
        })
        .with_context(|| format!("entity '{requested}' is not currently observed"))
}

fn observed_food_source(observation: &ObservationSnapshot, requested: &str) -> Result<String> {
    observation
        .mobs
        .iter()
        .find(|entity| {
            let name = entity.name.as_deref().unwrap_or(&entity.kind);
            name.eq_ignore_ascii_case(requested)
                && entity.kind.eq_ignore_ascii_case("mob")
                && entity
                    .category
                    .as_deref()
                    .is_some_and(|category| category.eq_ignore_ascii_case("animal"))
                && entity.food_source
                && entity.safe_to_hunt
                && entity.passive
                && entity.adult
                && !entity.baby
                && !entity.named
                && !entity.tamed
                && !entity.owned
                && !entity.persistent
                && entity.owner.is_none()
                && !entity.food_drops.is_empty()
        })
        .and_then(|entity| entity.name.as_deref())
        .map(str::to_string)
        .with_context(|| {
            format!(
                "mob '{requested}' is not an observed safe adult food source; both food_source and safe_to_hunt must be true, with an unnamed, untamed adult animal and known food drops"
            )
        })
}

fn observed_inventory_item(observation: &ObservationSnapshot, requested: &str) -> Result<String> {
    observation
        .inventory
        .wield
        .iter()
        .chain(observation.inventory.main.iter())
        .find(|item| item.name.eq_ignore_ascii_case(requested))
        .map(|item| item.name.clone())
        .with_context(|| format!("item '{requested}' is not in observed inventory"))
}

fn observed_chest(
    observation: &ObservationSnapshot,
    requested: [i32; 3],
) -> Result<&super::state::ChestView> {
    observation
        .chests
        .iter()
        .find(|chest| chest.pos == requested)
        .with_context(|| {
            format!(
                "storage container at ({},{},{}) is not in observation.chests",
                requested[0], requested[1], requested[2]
            )
        })
}

fn observed_furnace(
    observation: &ObservationSnapshot,
    requested: [i32; 3],
) -> Result<&FurnaceView> {
    observation
        .furnaces
        .iter()
        .find(|furnace| furnace.pos == requested)
        .with_context(|| {
            format!(
                "furnace at ({},{},{}) is not in observation.furnaces",
                requested[0], requested[1], requested[2]
            )
        })
}

fn ensure_accessible_furnace(furnace: &FurnaceView) -> Result<()> {
    anyhow::ensure!(
        furnace.accessible,
        "furnace is not accessible: {}",
        furnace.status
    );
    anyhow::ensure!(
        furnace.distance <= 6.0,
        "furnace is {:.1} nodes away",
        furnace.distance
    );
    Ok(())
}

fn observed_craftable<'a>(
    observation: &'a ObservationSnapshot,
    requested: &str,
) -> Result<&'a CraftableItemView> {
    observation
        .crafting
        .craftable
        .iter()
        .find(|item| item.item.eq_ignore_ascii_case(requested))
        .with_context(|| format!("item '{requested}' is not in observation.crafting.craftable"))
}

fn ensure_autonomous_node_allowed(
    state: &AgentState,
    node: &super::state::NodeView,
    autonomous: bool,
    player_instruction_turn: bool,
) -> Result<()> {
    if autonomous
        && node_is_low_value(node)
        && !low_value_request_is_authorized(
            state,
            &node.name,
            &node.groups,
            player_instruction_turn,
        )
    {
        bail!(
            "autonomous_low_value_resource_rejected: '{}' is soil/sand/leaves/flora/plant; gather it only when an active configured/player goal or current player message explicitly requests it",
            node.name
        );
    }
    Ok(())
}

fn ensure_autonomous_item_allowed(
    state: &AgentState,
    item: &str,
    autonomous: bool,
    player_instruction_turn: bool,
) -> Result<()> {
    if !autonomous || !item_is_low_value(&state.observation, item) {
        return Ok(());
    }
    let groups = state
        .observation
        .nearby_nodes
        .iter()
        .find(|node| node.name.eq_ignore_ascii_case(item))
        .map(|node| node.groups.as_slice())
        .unwrap_or(&[]);
    if !low_value_request_is_authorized(state, item, groups, player_instruction_turn) {
        bail!(
            "autonomous_low_value_item_rejected: '{item}' is low-value terrain/debris and was not explicitly requested by a configured/player goal or current chat"
        );
    }
    Ok(())
}

fn inventory_count(observation: &ObservationSnapshot, requested: &str) -> u64 {
    let main_count = observation
        .inventory
        .main
        .iter()
        .filter(|item| item.name.eq_ignore_ascii_case(requested))
        .map(|item| u64::from(item.count))
        .sum();

    // The server's `main` list already contains the selected wield slot. Only
    // fall back to the separate wield snapshot when no main inventory was sent.
    if observation.inventory.main.is_empty() {
        observation
            .inventory
            .wield
            .iter()
            .filter(|item| item.name.eq_ignore_ascii_case(requested))
            .map(|item| u64::from(item.count))
            .sum()
    } else {
        main_count
    }
}

fn nearest_observed_node<'a>(
    observation: &'a ObservationSnapshot,
    requested: &str,
) -> Result<&'a super::state::NodeView> {
    observation
        .nearby_nodes
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case(requested))
        .min_by_key(|node| {
            (node.pos[0] - observation.position[0]).abs()
                + (node.pos[1] - observation.position[1]).abs()
                + (node.pos[2] - observation.position[2]).abs()
        })
        .with_context(|| format!("node '{requested}' is not in observed nearby_nodes"))
}

fn node_distance(origin: [i32; 3], target: [i32; 3]) -> f64 {
    let dx = f64::from(target[0] - origin[0]);
    let dy = f64::from(target[1] - origin[1]);
    let dz = f64::from(target[2] - origin[2]);
    (dx * dx + dy * dy + dz * dz).sqrt()
}

fn ensure_nearby(origin: [i32; 3], target: [f64; 3], max_distance: f64) -> Result<()> {
    let dx = target[0] - f64::from(origin[0]);
    let dy = target[1] - f64::from(origin[1]);
    let dz = target[2] - f64::from(origin[2]);
    let distance = (dx * dx + dy * dy + dz * dz).sqrt();
    anyhow::ensure!(
        distance <= max_distance,
        "target is {distance:.1} nodes away; maximum is {max_distance:.1}"
    );
    Ok(())
}

pub fn post_chat_status(
    client: &Client,
    base: &Url,
    token: &str,
    message: &str,
) -> Result<String> {
    post(
        client,
        base,
        token,
        "/chat",
        Some(json!({"message": clip_chars(message.trim(), 300)})),
    )
}

fn failure(name: &str, result: &str, external_action: bool) -> ToolExecution {
    ToolExecution {
        name: name.to_string(),
        ok: false,
        result: result.to_string(),
        external_action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::state::{
        ChestView, ContainerItemView, CraftableItemView, CraftingView, FoodDropView,
        FurnaceFuelOptionView, FurnaceInputOptionView, FurnaceView, InventoryView,
        ItemStackView, NodeView, RelativeEntity,
    };

    #[test]
    fn validates_observed_players_case_insensitively() {
        let observation = ObservationSnapshot {
            players: vec![RelativeEntity {
                kind: "player".to_string(),
                name: Some("Alice".to_string()),
                ..RelativeEntity::default()
            }],
            ..ObservationSnapshot::default()
        };
        assert_eq!(observed_player(&observation, "alice").unwrap(), "Alice");
        assert!(observed_player(&observation, "Bob").is_err());
    }

    #[test]
    fn chest_targets_require_an_exact_observed_coordinate() {
        let observation = ObservationSnapshot {
            chests: vec![ChestView {
                name: "mcl_chests:chest_small".to_string(),
                kind: "chest".to_string(),
                pos: [10, 20, 30],
                distance: 2.0,
                accessible: true,
                status: "accessible".to_string(),
                ..ChestView::default()
            }],
            ..ObservationSnapshot::default()
        };
        assert_eq!(observed_chest(&observation, [10, 20, 30]).unwrap().kind, "chest");
        assert!(observed_chest(&observation, [10, 20, 31]).is_err());
    }

    #[test]
    fn furnace_and_crafting_targets_are_exact_observed_capabilities() {
        let observation = ObservationSnapshot {
            furnaces: vec![FurnaceView {
                name: "mcl_furnaces:furnace".to_string(),
                kind: "furnace".to_string(),
                pos: [10, 20, 30],
                distance: 2.0,
                accessible: true,
                status: "accessible".to_string(),
                input_options: vec![FurnaceInputOptionView {
                    name: "mcl_mobitems:beef".to_string(),
                    count: 3,
                    output: ContainerItemView {
                        name: "mcl_mobitems:cooked_beef".to_string(),
                        count: 1,
                    },
                    cook_time: 10.0,
                }],
                fuel_options: vec![FurnaceFuelOptionView {
                    name: "mcl_core:coal_lump".to_string(),
                    count: 2,
                    burn_time: 80.0,
                    replacement: None,
                }],
                ..FurnaceView::default()
            }],
            crafting: CraftingView {
                craftable: vec![CraftableItemView {
                    item: "mcl_torches:torch".to_string(),
                    output_per_batch: 4,
                    max_batches: 2,
                    ..CraftableItemView::default()
                }],
            },
            ..ObservationSnapshot::default()
        };
        assert_eq!(
            observed_furnace(&observation, [10, 20, 30]).unwrap().kind,
            "furnace"
        );
        assert!(observed_furnace(&observation, [10, 20, 31]).is_err());
        assert_eq!(
            observed_craftable(&observation, "MCL_TORCHES:TORCH")
                .unwrap()
                .max_batches,
            2
        );
        assert!(observed_craftable(&observation, "mcl_core:dirt").is_err());
    }

    #[test]
    fn load_furnace_rejects_an_occupied_fuel_slot() {
        let mut state = AgentState::default();
        state.observation.furnaces.push(FurnaceView {
            name: "mcl_furnaces:furnace".to_string(),
            kind: "furnace".to_string(),
            pos: [1, 0, 0],
            distance: 1.0,
            accessible: true,
            status: "accessible".to_string(),
            fuel: Some(ContainerItemView {
                name: "mcl_core:coal_lump".to_string(),
                count: 1,
            }),
            ..FurnaceView::default()
        });
        let error = execute_checked(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            "load_furnace",
            &json!({
                "x":1,"y":0,"z":0,"input":"mcl_mobitems:beef","input_count":1,
                "fuel":"mcl_core:coal_lump","fuel_count":1
            }),
            true,
            false,
        )
        .unwrap_err();
        assert!(error.to_string().contains("fuel slot is already occupied"));
    }

    #[test]
    fn load_furnace_rejects_multiple_container_fuels() {
        let mut state = AgentState::default();
        state.observation.furnaces.push(FurnaceView {
            name: "mcl_furnaces:furnace".to_string(),
            kind: "furnace".to_string(),
            pos: [1, 0, 0],
            distance: 1.0,
            accessible: true,
            status: "accessible".to_string(),
            input_options: vec![FurnaceInputOptionView {
                name: "mcl_mobitems:beef".to_string(),
                count: 2,
                output: ContainerItemView {
                    name: "mcl_mobitems:cooked_beef".to_string(),
                    count: 1,
                },
                cook_time: 10.0,
            }],
            fuel_options: vec![FurnaceFuelOptionView {
                name: "mcl_buckets:bucket_lava".to_string(),
                count: 2,
                burn_time: 1000.0,
                replacement: Some(ContainerItemView {
                    name: "mcl_buckets:bucket_empty".to_string(),
                    count: 1,
                }),
            }],
            ..FurnaceView::default()
        });
        let error = execute_checked(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            "load_furnace",
            &json!({
                "x":1,"y":0,"z":0,"input":"mcl_mobitems:beef","input_count":1,
                "fuel":"mcl_buckets:bucket_lava","fuel_count":2
            }),
            true,
            false,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("leaves a replacement container; load exactly one"));
    }

    #[test]
    fn hostile_targets_use_the_exact_observed_entity_name() {
        let observation = ObservationSnapshot {
            hostiles: vec![RelativeEntity {
                kind: "hostile".to_string(),
                name: Some("mobs_mc:zombie".to_string()),
                ..RelativeEntity::default()
            }],
            ..ObservationSnapshot::default()
        };
        assert_eq!(
            observed_combat_target(&observation, "MOBS_MC:ZOMBIE").unwrap(),
            "mobs_mc:zombie"
        );
        assert!(observed_combat_target(&observation, "mobs_mc:cow").is_err());
    }

    #[test]
    fn passive_mobs_are_valid_noncombat_targets_only() {
        let observation = ObservationSnapshot {
            mobs: vec![RelativeEntity {
                kind: "mob".to_string(),
                name: Some("mobs_mc:cow".to_string()),
                category: Some("animal".to_string()),
                ..RelativeEntity::default()
            }],
            ..ObservationSnapshot::default()
        };
        assert_eq!(
            observed_entity_target(&observation, "MOBS_MC:COW").unwrap(),
            "mobs_mc:cow"
        );
        assert!(observed_combat_target(&observation, "mobs_mc:cow").is_err());
    }

    #[test]
    fn food_hunting_requires_every_server_safety_signal() {
        let safe_cow = RelativeEntity {
            kind: "mob".to_string(),
            name: Some("mobs_mc:cow".to_string()),
            category: Some("animal".to_string()),
            food_source: true,
            safe_to_hunt: true,
            passive: true,
            adult: true,
            food_drops: vec![FoodDropView {
                name: "mcl_mobitems:beef".to_string(),
                food_points: 3.0,
                ..FoodDropView::default()
            }],
            ..RelativeEntity::default()
        };
        let observation = ObservationSnapshot {
            mobs: vec![safe_cow.clone()],
            ..ObservationSnapshot::default()
        };
        assert_eq!(
            observed_food_source(&observation, "MOBS_MC:COW").unwrap(),
            "mobs_mc:cow"
        );

        for unsafe_cow in [
            RelativeEntity {
                safe_to_hunt: false,
                ..safe_cow.clone()
            },
            RelativeEntity {
                named: true,
                ..safe_cow.clone()
            },
            RelativeEntity {
                adult: false,
                ..safe_cow.clone()
            },
            RelativeEntity {
                food_drops: Vec::new(),
                ..safe_cow
            },
        ] {
            let observation = ObservationSnapshot {
                mobs: vec![unsafe_cow],
                ..ObservationSnapshot::default()
            };
            assert!(observed_food_source(&observation, "mobs_mc:cow").is_err());
        }
    }

    #[test]
    fn autonomous_tool_catalog_separates_missions_and_objectives() {
        let names = |tools: Vec<Value>| {
            tools
                .into_iter()
                .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        };
        let autonomous = names(tool_definitions(false, true));
        assert!(autonomous.contains(&"set_objective".to_string()));
        assert!(autonomous.contains(&"finish_objective".to_string()));
        assert!(autonomous.contains(&"navigate_node".to_string()));
        assert!(autonomous.contains(&"gather_resource".to_string()));
        assert!(autonomous.contains(&"hunt_food".to_string()));
        assert!(autonomous.contains(&"load_furnace".to_string()));
        assert!(autonomous.contains(&"collect_furnace_output".to_string()));
        assert!(autonomous.contains(&"craft_item".to_string()));

        let directed = names(tool_definitions(false, false));
        assert!(!directed.contains(&"set_objective".to_string()));
        assert!(!directed.contains(&"finish_objective".to_string()));
    }

    #[test]
    fn follow_names_do_not_require_short_range_observation() {
        assert_eq!(valid_player_name("test").unwrap(), "test");
        assert!(valid_player_name("not a player").is_err());
    }

    #[test]
    fn counts_inventory_before_drop() {
        let observation = ObservationSnapshot {
            inventory: InventoryView {
                main: vec![ItemStackView {
                    name: "mcl_core:stone".to_string(),
                    count: 4,
                    wear: 0,
                    food_points: None,
                    food: false,
                    food_group: 0,
                    food_saturation: None,
                }],
                ..InventoryView::default()
            },
            ..ObservationSnapshot::default()
        };
        assert_eq!(inventory_count(&observation, "MCL_CORE:STONE"), 4);
    }

    #[test]
    fn inventory_count_does_not_count_the_wield_slot_twice() {
        let stack = ItemStackView {
            name: "mcl_core:stone".to_string(),
            count: 4,
            wear: 0,
            food_points: None,
            food: false,
            food_group: 0,
            food_saturation: None,
        };
        let observation = ObservationSnapshot {
            inventory: InventoryView {
                wield: Some(stack.clone()),
                main: vec![stack],
                ..InventoryView::default()
            },
            ..ObservationSnapshot::default()
        };
        assert_eq!(inventory_count(&observation, "mcl_core:stone"), 4);
    }

    #[test]
    fn coordinate_arguments_are_all_or_none() {
        assert!(optional_coordinates(&json!({})).unwrap().is_none());
        assert_eq!(
            optional_coordinates(&json!({"x":1,"y":2,"z":3})).unwrap(),
            Some([1, 2, 3])
        );
        assert!(optional_coordinates(&json!({"x":1})).is_err());
    }

    #[test]
    fn repeated_failed_call_is_rejected_before_an_http_request() {
        let args = json!({"x":null,"y":null,"z":null});
        let mut state = AgentState::default();
        state.record_tool_result("place", &args, false, "no_space");
        state.record_tool_result("place", &args, false, "no_space");
        let call = ToolCall {
            id: "repeat".to_string(),
            name: "place".to_string(),
            arguments: args,
        };
        let result = execute_tool(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            &call,
            false,
            true,
            false,
        );
        assert!(!result.ok);
        assert!(result.result.contains("suppressed_repeated_failure"));
    }

    #[test]
    fn exhausted_objective_budget_blocks_more_external_actions() {
        let mut state = AgentState::default();
        state.set_objective("Test a route", None, 2).unwrap();
        state.record_tool_result("move", &json!({"direction":"left"}), true, "OK");
        state.record_tool_result("move", &json!({"direction":"right"}), true, "OK");
        let result = execute_tool(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            &ToolCall {
                id: "over-budget".to_string(),
                name: "move".to_string(),
                arguments: json!({"direction":"forward","steps":1}),
            },
            false,
            true,
            false,
        );
        assert!(!result.ok);
        assert!(result.result.contains("objective_action_budget_exhausted"));
    }

    #[test]
    fn stop_and_defend_bypass_an_exhausted_objective_budget() {
        let mut state = AgentState::default();
        state.set_objective("Test a route", None, 2).unwrap();
        state.record_tool_result("move", &json!({"direction":"left"}), true, "OK");
        state.record_tool_result("move", &json!({"direction":"right"}), true, "OK");
        state.observation.hostiles.push(RelativeEntity {
            kind: "hostile".to_string(),
            name: Some("mobs_mc:zombie".to_string()),
            ..RelativeEntity::default()
        });
        for name in ["stop", "defend"] {
            let result = execute_tool(
                &Client::new(),
                &Url::parse("http://127.0.0.1:1/").unwrap(),
                "",
                &mut state,
                &ToolCall {
                    id: name.to_string(),
                    name: name.to_string(),
                    arguments: json!({}),
                },
                false,
                true,
                false,
            );
            assert!(!result.result.contains("objective_action_budget_exhausted"));
        }
        assert_eq!(state.current_objective.as_ref().unwrap().actions_used, 2);
    }

    #[test]
    fn autonomous_low_value_gathering_requires_an_explicit_player_goal() {
        let dirt = NodeView {
            name: "mcl_core:dirt".to_string(),
            pos: [1, 0, 0],
            groups: vec!["soil".to_string()],
        };
        let mut state = AgentState::default();
        state.observation.nearby_nodes = vec![dirt.clone()];
        let rejected = execute_checked(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            "gather_resource",
            &json!({"node":"mcl_core:dirt","count":1,"radius":4}),
            true,
            false,
        )
        .unwrap_err();
        assert!(rejected
            .to_string()
            .contains("autonomous_low_value_resource_rejected"));

        state.enqueue_chat([super::super::state::PendingChatMessage {
            id: 1,
            from: "Alice".to_string(),
            message: "hello".to_string(),
        }]);
        let invented_goal = execute_checked(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut state,
            "set_goal",
            &json!({"description":"Collect dirt","success_criteria":null}),
            true,
            true,
        )
        .unwrap_err();
        assert!(invented_goal.to_string().contains("low-value durable goal rejected"));

        state.set_configured_goal("Collect dirt for a farm", None).unwrap();
        assert!(ensure_autonomous_node_allowed(&state, &dirt, true, false).is_ok());
        assert!(ensure_autonomous_item_allowed(&state, "mcl_core:dirt", true, false).is_ok());
    }

    #[test]
    fn autonomous_mining_requires_an_exact_observed_target() {
        let error = execute_checked(
            &Client::new(),
            &Url::parse("http://127.0.0.1:1/").unwrap(),
            "",
            &mut AgentState::default(),
            "mine",
            &json!({}),
            true,
            false,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("autonomous mine requires exact observed x/y/z coordinates"));
    }

    #[test]
    fn finds_the_nearest_matching_resource_node() {
        let observation = ObservationSnapshot {
            position: [0, 0, 0],
            nearby_nodes: vec![
                NodeView {
                    name: "tree".to_string(),
                    pos: [4, 0, 0],
                    groups: vec!["tree".to_string()],
                },
                NodeView {
                    name: "tree".to_string(),
                    pos: [1, 0, 0],
                    groups: vec!["tree".to_string()],
                },
            ],
            ..ObservationSnapshot::default()
        };
        assert_eq!(nearest_observed_node(&observation, "TREE").unwrap().pos, [1, 0, 0]);
    }

    #[test]
    fn durable_goals_require_a_current_player_instruction() {
        let client = Client::new();
        let api_base = Url::parse("http://127.0.0.1:9123/").unwrap();
        let args = json!({
            "description": "Inspect the nearby cow",
            "success_criteria": "Approach it once without attacking"
        });

        let mut disabled = AgentState::default();
        assert!(execute_checked(
            &client, &api_base, "", &mut disabled, "set_goal", &args, false, false,
        )
        .unwrap_err()
        .to_string()
        .contains("durable goals require a current player instruction"));

        let mut enabled = AgentState::default();
        assert!(execute_checked(
            &client, &api_base, "", &mut enabled, "set_goal", &args, true, false,
        )
        .unwrap_err()
        .to_string()
        .contains("durable goals require a current player instruction"));

        let mut player_assigned = AgentState::default();
        execute_checked(
            &client,
            &api_base,
            "",
            &mut player_assigned,
            "set_goal",
            &args,
            false,
            true,
        )
        .unwrap();
        assert!(player_assigned.current_goal.is_some());
    }

    #[test]
    fn autonomous_objective_can_coexist_with_a_durable_mission() {
        let client = Client::new();
        let api_base = Url::parse("http://127.0.0.1:9123/").unwrap();
        let mut state = AgentState::default();
        state.set_goal("Help Alice establish a base", None).unwrap();
        let args = json!({
            "description":"Gather nearby coal",
            "success_criteria":"Collect one coal ore drop",
            "action_budget":4
        });
        execute_checked(
            &client,
            &api_base,
            "",
            &mut state,
            "set_objective",
            &args,
            true,
            false,
        )
        .unwrap();
        assert!(state.current_goal.is_some());
        assert_eq!(state.current_objective.as_ref().unwrap().action_budget, 4);

        execute_checked(
            &client,
            &api_base,
            "",
            &mut state,
            "finish_objective",
            &json!({"status":"completed","outcome":"coal collected"}),
            true,
            false,
        )
        .unwrap();
        assert!(state.current_goal.is_some());
        assert!(state.current_objective.is_none());
    }
}
