use serde::Serialize;
use serde_json::{json, Value};

use super::state::{
    AgentState, CraftableItemView, FurnaceInputOptionView, GoalOrigin, NodeView,
    ObservationSnapshot,
};

const LOW_VALUE_GROUPS: &[&str] = &["soil", "sand", "leaves", "flora", "plant"];

#[derive(Clone, Debug, Serialize)]
pub struct ProgressionRecommendation {
    pub id: &'static str,
    pub priority: u8,
    pub action: &'static str,
    pub reason: String,
    pub arguments: Value,
    pub success_criteria: String,
}

pub fn progression_recommendations(
    observation: &ObservationSnapshot,
) -> Vec<ProgressionRecommendation> {
    let mut recommendations = Vec::new();

    if let Some((furnace, output)) = observation
        .furnaces
        .iter()
        .filter(|furnace| furnace.accessible && furnace.distance <= 6.0)
        .find_map(|furnace| furnace.output.as_ref().map(|output| (furnace, output)))
    {
        recommendations.push(ProgressionRecommendation {
            id: "furnace:collect_output",
            priority: 5,
            action: "collect_furnace_output",
            reason: format!(
                "The nearby {} already contains {} {} ready to collect.",
                furnace.kind, output.count, output.name
            ),
            arguments: json!({
                "x": furnace.pos[0], "y": furnace.pos[1], "z": furnace.pos[2],
                "item": output.name, "count": output.count.min(16).max(1)
            }),
            success_criteria: format!("Inventory gains {}.", output.name),
        });
    }

    let logs = inventory_role_count(observation, is_log);
    let planks = inventory_role_count(observation, is_planks);
    let sticks = inventory_role_count(observation, is_stick);
    let coal = inventory_role_count(observation, is_coal);
    let torches = inventory_role_count(observation, is_torch);

    if logs == 0 && planks == 0 {
        if let Some(tree) = observation
            .nearby_nodes
            .iter()
            .find(|node| has_group(node, "tree"))
        {
            recommendations.push(ProgressionRecommendation {
                id: "bootstrap:gather_logs",
                priority: 20,
                action: "gather_resource",
                reason: "Logs are the first missing input for planks, sticks, tools, and torches."
                    .to_string(),
                arguments: json!({"node":tree.name,"count":2,"radius":16}),
                success_criteria: "Inventory contains at least one log/tree item.".to_string(),
            });
        }
    } else if logs > 0 {
        if let Some(recipe) = find_craftable(observation, is_planks) {
            recommendations.push(craft_recommendation(
                "bootstrap:craft_planks",
                18,
                recipe,
                "Convert an observed log into the registered plank output before gathering unrelated terrain.",
            ));
        }
    }

    if planks > 0 && sticks < 4 {
        if let Some(recipe) = find_craftable(observation, is_stick) {
            recommendations.push(craft_recommendation(
                "bootstrap:craft_sticks",
                17,
                recipe,
                "Maintain at least four sticks for tools and torches.",
            ));
        }
    }

    if sticks > 0 && coal == 0 {
        if let Some(coal_node) = observation.nearby_nodes.iter().find(|node| {
            let name = node.name.to_ascii_lowercase();
            name.contains("coal")
                && (has_group(node, "ore")
                    || name.contains("_ore")
                    || name.contains("_with_"))
        }) {
            recommendations.push(ProgressionRecommendation {
                id: "bootstrap:gather_coal",
                priority: 16,
                action: "gather_resource",
                reason: "Coal is observed and is the missing ingredient for torches and furnace fuel."
                    .to_string(),
                arguments: json!({"node":coal_node.name,"count":2,"radius":16}),
                success_criteria: "Inventory gains coal from the observed coal ore.".to_string(),
            });
        }
    }

    if sticks > 0 && coal > 0 && torches < 8 {
        if let Some(recipe) = find_craftable(observation, is_torch) {
            recommendations.push(craft_recommendation(
                "bootstrap:craft_torches",
                15,
                recipe,
                "Use existing sticks and coal to establish a small torch reserve; stop after 8-16.",
            ));
        }
    }

    if observation.furnaces.is_empty()
        && inventory_role_count(observation, is_furnace) == 0
    {
        if let Some(recipe) = find_craftable(observation, is_furnace) {
            recommendations.push(craft_recommendation(
                "bootstrap:craft_furnace",
                25,
                recipe,
                "Craft one furnace when its registered recipe is currently available.",
            ));
        }
    }

    if let Some((furnace, input, fuel)) = observation
        .furnaces
        .iter()
        .filter(|furnace| {
            furnace.accessible
                && furnace.distance <= 6.0
                && furnace.input.is_none()
                && furnace.fuel.is_none()
                && !furnace.output_blocked
        })
        .find_map(|furnace| {
            choose_furnace_input(observation, &furnace.input_options).and_then(|input| {
                choose_furnace_fuel(&furnace.fuel_options).map(|fuel| (furnace, input, fuel))
            })
        })
    {
        let input_count = input.count.min(8).max(1);
        let fuel_count = if fuel.replacement.is_some() {
            1
        } else {
            fuel.count.min(2).max(1)
        };
        recommendations.push(ProgressionRecommendation {
            id: "furnace:load",
            priority: if observation.hunger.is_some_and(|hunger| hunger < 12) {
                8
            } else {
                22
            },
            action: "load_furnace",
            reason: format!(
                "The observed {} can cook {} into {}; use registered fuel {}.",
                furnace.kind, input.name, input.output.name, fuel.name
            ),
            arguments: json!({
                "x":furnace.pos[0], "y":furnace.pos[1], "z":furnace.pos[2],
                "input":input.name, "input_count":input_count,
                "fuel":fuel.name, "fuel_count":fuel_count
            }),
            success_criteria: format!(
                "The furnace accepts {} and begins or queues cooking.",
                input.name
            ),
        });
    }

    recommendations.sort_by_key(|recommendation| recommendation.priority);
    recommendations.truncate(6);
    recommendations
}

pub fn node_is_low_value(node: &NodeView) -> bool {
    let useful = has_group(node, "tree") || has_group(node, "ore");
    !useful
        && LOW_VALUE_GROUPS
            .iter()
            .any(|group| has_group(node, group))
}

pub fn item_is_low_value(observation: &ObservationSnapshot, item: &str) -> bool {
    if observation
        .nearby_nodes
        .iter()
        .any(|node| node.name.eq_ignore_ascii_case(item) && node_is_low_value(node))
    {
        return true;
    }
    let item = item.to_ascii_lowercase();
    [
        "dirt", "soil", "sand", "leaves", "leaf", "flora", "flower", "plant",
    ]
    .iter()
    .any(|term| item.contains(term))
}

pub fn low_value_request_is_authorized(
    state: &AgentState,
    resource_name: &str,
    groups: &[String],
    player_instruction_turn: bool,
) -> bool {
    let terms = low_value_terms(resource_name, groups);
    if terms.is_empty() {
        return true;
    }

    if state.current_goal.as_ref().is_some_and(|goal| {
        matches!(
            goal.origin,
            GoalOrigin::Unknown | GoalOrigin::Configured | GoalOrigin::Player
        ) && text_mentions_any(&goal.description, &terms)
    }) {
        return true;
    }

    player_instruction_turn
        && (state
            .pending_chat
            .iter()
            .any(|message| text_mentions_any(&message.message, &terms))
            || state
                .conversation_history
                .iter()
                .rev()
                .find(|entry| entry.role == "player")
                .is_some_and(|entry| text_mentions_any(&entry.message, &terms)))
}

fn low_value_terms(resource_name: &str, groups: &[String]) -> Vec<String> {
    let mut terms = Vec::new();
    let lower_name = resource_name.to_ascii_lowercase();
    for term in [
        "dirt", "soil", "sand", "leaves", "leaf", "flora", "flower", "plant",
    ] {
        if lower_name.contains(term)
            || groups.iter().any(|group| group.eq_ignore_ascii_case(term))
        {
            terms.push(term.to_string());
        }
    }
    terms
}

fn text_mentions_any(text: &str, terms: &[String]) -> bool {
    let text = text.to_ascii_lowercase();
    terms.iter().any(|term| text.contains(term))
}

fn inventory_role_count(
    observation: &ObservationSnapshot,
    predicate: fn(&str) -> bool,
) -> u32 {
    observation
        .inventory
        .main
        .iter()
        .filter(|item| predicate(&item.name))
        .map(|item| item.count)
        .sum()
}

fn find_craftable(
    observation: &ObservationSnapshot,
    predicate: fn(&str) -> bool,
) -> Option<&CraftableItemView> {
    observation
        .crafting
        .craftable
        .iter()
        .find(|recipe| predicate(&recipe.item))
}

fn craft_recommendation(
    id: &'static str,
    priority: u8,
    recipe: &CraftableItemView,
    reason: &str,
) -> ProgressionRecommendation {
    let count = recipe.output_per_batch.max(1);
    ProgressionRecommendation {
        id,
        priority,
        action: "craft_item",
        reason: reason.to_string(),
        arguments: json!({"item":recipe.item,"count":count}),
        success_criteria: format!("Inventory gains {}.", recipe.item),
    }
}

fn choose_furnace_input<'a>(
    observation: &ObservationSnapshot,
    inputs: &'a [FurnaceInputOptionView],
) -> Option<&'a FurnaceInputOptionView> {
    let food_is_useful = observation.hunger.is_some_and(|hunger| hunger < 14);
    inputs
        .iter()
        .find(|input| food_is_useful && item_is_food(observation, &input.name))
        .or_else(|| inputs.iter().find(|input| is_raw_ore(&input.name)))
        .or_else(|| inputs.first())
}

fn choose_furnace_fuel(
    fuels: &[super::state::FurnaceFuelOptionView],
) -> Option<&super::state::FurnaceFuelOptionView> {
    fuels
        .iter()
        .find(|fuel| is_coal(&fuel.name))
        .or_else(|| fuels.iter().max_by(|a, b| a.burn_time.total_cmp(&b.burn_time)))
}

fn item_is_food(observation: &ObservationSnapshot, requested: &str) -> bool {
    observation.inventory.main.iter().any(|item| {
        item.name.eq_ignore_ascii_case(requested) && (item.food || item.food_points.unwrap_or(0.0) > 0.0)
    })
}

fn has_group(node: &NodeView, wanted: &str) -> bool {
    node.groups
        .iter()
        .any(|group| group.eq_ignore_ascii_case(wanted))
}

fn item_component(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn is_log(name: &str) -> bool {
    let name = item_component(name).to_ascii_lowercase();
    (name.contains("tree") || name.contains("log"))
        && !name.contains("leaves")
        && !name.contains("sapling")
}

fn is_planks(name: &str) -> bool {
    let name = item_component(name).to_ascii_lowercase();
    (name == "wood" || name.contains("plank")) && !name.contains("tree")
}

fn is_stick(name: &str) -> bool {
    item_component(name).to_ascii_lowercase().contains("stick")
}

fn is_coal(name: &str) -> bool {
    let name = item_component(name).to_ascii_lowercase();
    name.contains("coal") && !name.contains("stone_with") && !name.contains("_ore")
}

fn is_torch(name: &str) -> bool {
    item_component(name).to_ascii_lowercase().contains("torch")
}

fn is_furnace(name: &str) -> bool {
    let name = item_component(name).to_ascii_lowercase();
    name == "furnace" || name.ends_with("_furnace")
}

fn is_raw_ore(name: &str) -> bool {
    let name = item_component(name).to_ascii_lowercase();
    name.contains("raw_") || name.contains("_lump") || name.contains("ore")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::state::{
        ContainerItemView, CraftingView, FurnaceFuelOptionView, FurnaceInputOptionView,
        FurnaceView, GoalStatus, ItemStackView, ObjectiveOrigin, ObjectiveState,
    };

    fn node(name: &str, group: &str) -> NodeView {
        NodeView {
            name: name.to_string(),
            pos: [1, 0, 0],
            groups: vec![group.to_string()],
        }
    }

    #[test]
    fn autonomous_progression_prefers_tree_and_never_recommends_dirt() {
        let observation = ObservationSnapshot {
            nearby_nodes: vec![
                node("mcl_core:tree", "tree"),
                node("mcl_core:dirt", "soil"),
            ],
            ..ObservationSnapshot::default()
        };
        let recommendations = progression_recommendations(&observation);
        assert_eq!(recommendations[0].id, "bootstrap:gather_logs");
        assert!(recommendations.iter().all(|recommendation| {
            recommendation.arguments["node"] != "mcl_core:dirt"
        }));
    }

    #[test]
    fn progression_advances_from_logs_to_registered_planks() {
        let observation = ObservationSnapshot {
            inventory: super::super::state::InventoryView {
                main: vec![ItemStackView {
                    name: "mcl_core:tree".to_string(),
                    count: 1,
                    ..ItemStackView::default()
                }],
                ..super::super::state::InventoryView::default()
            },
            crafting: CraftingView {
                craftable: vec![CraftableItemView {
                    item: "mcl_core:wood".to_string(),
                    output_per_batch: 4,
                    max_batches: 1,
                    ..CraftableItemView::default()
                }],
            },
            ..ObservationSnapshot::default()
        };
        let recommendations = progression_recommendations(&observation);
        assert!(recommendations
            .iter()
            .any(|recommendation| recommendation.id == "bootstrap:craft_planks"));
    }

    #[test]
    fn coal_ore_name_is_grounded_even_without_a_literal_ore_group() {
        let observation = ObservationSnapshot {
            inventory: super::super::state::InventoryView {
                main: vec![ItemStackView {
                    name: "mcl_core:stick".to_string(),
                    count: 4,
                    ..ItemStackView::default()
                }],
                ..super::super::state::InventoryView::default()
            },
            nearby_nodes: vec![node("mcl_core:stone_with_coal", "stone")],
            ..ObservationSnapshot::default()
        };
        assert!(progression_recommendations(&observation)
            .iter()
            .any(|recommendation| recommendation.id == "bootstrap:gather_coal"));
    }

    #[test]
    fn furnace_recommendation_avoids_occupied_fuel_and_caps_container_fuel() {
        let furnace = FurnaceView {
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
                count: 4,
                burn_time: 1000.0,
                replacement: Some(ContainerItemView {
                    name: "mcl_buckets:bucket_empty".to_string(),
                    count: 1,
                }),
            }],
            ..FurnaceView::default()
        };
        let mut observation = ObservationSnapshot {
            furnaces: vec![furnace],
            ..ObservationSnapshot::default()
        };
        let load = progression_recommendations(&observation)
            .into_iter()
            .find(|recommendation| recommendation.id == "furnace:load")
            .unwrap();
        assert_eq!(load.arguments["fuel_count"], 1);

        observation.furnaces[0].fuel = Some(ContainerItemView {
            name: "mcl_core:coal_lump".to_string(),
            count: 1,
        });
        assert!(progression_recommendations(&observation)
            .iter()
            .all(|recommendation| recommendation.id != "furnace:load"));
    }

    #[test]
    fn low_value_policy_requires_an_explicit_player_mission() {
        let dirt = node("mcl_core:dirt", "soil");
        let mut state = AgentState::default();
        assert!(!low_value_request_is_authorized(
            &state,
            &dirt.name,
            &dirt.groups,
            false
        ));
        state.set_goal("Please collect dirt for the farm", None).unwrap();
        assert!(low_value_request_is_authorized(
            &state,
            &dirt.name,
            &dirt.groups,
            false
        ));

        // An autonomous objective mentioning dirt is not player authority.
        state.current_goal = None;
        state.current_objective = Some(ObjectiveState {
            id: 1,
            description: "collect dirt".to_string(),
            success_criteria: None,
            status: GoalStatus::Active,
            origin: ObjectiveOrigin::Autonomous,
            created_tick: 0,
            updated_tick: 0,
            outcome: None,
            action_budget: 2,
            actions_used: 0,
        });
        assert!(!low_value_request_is_authorized(
            &state,
            &dirt.name,
            &dirt.groups,
            false
        ));
    }
}
