use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::state::AgentState;

pub(super) fn is_external_game_tool(name: &str) -> bool {
    !matches!(
        name.trim(),
        "say" | "set_goal" | "finish_goal" | "set_objective" | "finish_objective"
    )
}

pub(super) fn is_player_instruction_turn(
    has_pending_chat: bool,
    continuing_after_reply: bool,
) -> bool {
    has_pending_chat || continuing_after_reply
}

pub(super) fn needs_chat_action_followup(
    player_instruction_turn: bool,
    spoke_this_decision: bool,
    external_action_succeeded: bool,
) -> bool {
    player_instruction_turn && spoke_this_decision && !external_action_succeeded
}

pub(super) fn tool_is_available(
    name: &str,
    has_pending_chat: bool,
    server_mod_schema_version: u32,
    unavailable_tools: &HashSet<String>,
) -> bool {
    (name != "say" || has_pending_chat)
        && (server_mod_schema_version >= 2 || name != "collect_item")
        && (server_mod_schema_version >= 5
            || !matches!(name, "navigate_node" | "hunt_food"))
        && (server_mod_schema_version >= 6
            || !matches!(name, "mine" | "collect_blocks" | "gather_resource"))
        && (server_mod_schema_version >= 7
            || !matches!(name, "deposit_item" | "withdraw_item"))
        && (server_mod_schema_version >= 8
            || !matches!(
                name,
                "load_furnace" | "collect_furnace_output" | "craft_item"
            ))
        && !unavailable_tools.contains(name)
}

/// Keep impossible, observation-dependent schemas out of this decision. They
/// become available automatically when the next observation supports them.
pub(super) fn tool_is_relevant(
    name: &str,
    state: &AgentState,
    player_instruction_turn: bool,
) -> bool {
    let observation = &state.observation;
    let has_inventory = observation.inventory.wield.is_some()
        || !observation.inventory.main.is_empty();
    let has_entities = !observation.players.is_empty()
        || !observation.hostiles.is_empty()
        || !observation.mobs.is_empty();

    match name {
        "set_goal" => player_instruction_turn,
        "finish_goal" => state.current_goal.is_some(),
        "set_objective" => state.current_objective.is_none(),
        "finish_objective" => state.current_objective.is_some(),
        "attack" | "teleport" => !observation.players.is_empty(),
        "defend" => !observation.hostiles.is_empty(),
        "fight" => !observation.players.is_empty() || !observation.hostiles.is_empty(),
        "approach" | "interact" => has_entities,
        "hunt_food" => observation.mobs.iter().any(|mob| {
            mob.food_source
                && mob.safe_to_hunt
                && mob.adult
                && !mob.named
                && !mob.tamed
                && !mob.owned
                && !mob.food_drops.is_empty()
        }),
        "navigate_node" | "collect_blocks" | "gather_resource" => {
            !observation.nearby_nodes.is_empty()
        }
        "collect_item" => !observation.nearby_items.is_empty(),
        "deposit_item" => {
            has_inventory && observation.chests.iter().any(|chest| chest.accessible)
        }
        "withdraw_item" => observation
            .chests
            .iter()
            .any(|chest| chest.accessible && !chest.contents.is_empty()),
        "load_furnace" => observation.furnaces.iter().any(|furnace| {
            furnace.accessible
                && !furnace.input_options.is_empty()
                && !furnace.fuel_options.is_empty()
        }),
        "collect_furnace_output" => observation
            .furnaces
            .iter()
            .any(|furnace| furnace.accessible && furnace.output.is_some()),
        "craft_item" => !observation.crafting.craftable.is_empty(),
        "drop_item" | "wield" | "use_item" => has_inventory,
        _ => true,
    }
}

pub(super) fn tool_settle_delay(name: &str, ok: bool) -> Duration {
    if !ok {
        return Duration::from_secs(1);
    }
    match name {
        "follow" => Duration::from_secs(5),
        "move_to" | "collect_item" => Duration::from_secs(4),
        "navigate_node" | "gather_resource" | "hunt_food" => Duration::from_secs(6),
        "move" => Duration::from_secs(3),
        "collect_blocks" | "mine" | "place" | "defend" | "deposit_item"
        | "withdraw_item" | "load_furnace" | "collect_furnace_output" | "craft_item" => {
            Duration::from_secs(2)
        }
        _ => Duration::from_secs(1),
    }
}

pub(super) fn idle_decision_due(
    autonomous: bool,
    has_objective: bool,
    now: Instant,
    next_idle_decision: Instant,
) -> bool {
    autonomous && !has_objective && now >= next_idle_decision
}

pub(super) fn navigation_blocks_planning(status: &str) -> bool {
    matches!(status.trim(), "moving" | "recovering")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::state::{
        ChestItemView, ChestView, FoodDropView, RelativeEntity,
    };

    #[test]
    fn tool_availability_matches_chat_and_server_capabilities() {
        let unavailable = HashSet::new();
        assert!(!tool_is_available("say", false, 2, &unavailable));
        assert!(tool_is_available("say", true, 2, &unavailable));
        assert!(!tool_is_available("collect_blocks", false, 1, &unavailable));
        assert!(!tool_is_available("collect_blocks", false, 5, &unavailable));
        assert!(tool_is_available("collect_blocks", false, 6, &unavailable));
        assert!(tool_is_available("collect_item", false, 2, &unavailable));
        assert!(!tool_is_available("navigate_node", false, 4, &unavailable));
        assert!(!tool_is_available("gather_resource", false, 4, &unavailable));
        assert!(!tool_is_available("hunt_food", false, 4, &unavailable));
        assert!(tool_is_available("navigate_node", false, 5, &unavailable));
        assert!(!tool_is_available("gather_resource", false, 5, &unavailable));
        assert!(tool_is_available("gather_resource", false, 6, &unavailable));
        assert!(tool_is_available("hunt_food", false, 5, &unavailable));
        assert!(!tool_is_available("deposit_item", false, 6, &unavailable));
        assert!(tool_is_available("deposit_item", false, 7, &unavailable));
        assert!(tool_is_available("withdraw_item", false, 7, &unavailable));
        assert!(!tool_is_available("load_furnace", false, 7, &unavailable));
        assert!(!tool_is_available("collect_furnace_output", false, 7, &unavailable));
        assert!(!tool_is_available("craft_item", false, 7, &unavailable));
        assert!(tool_is_available("load_furnace", false, 8, &unavailable));
        assert!(tool_is_available("collect_furnace_output", false, 8, &unavailable));
        assert!(tool_is_available("craft_item", false, 8, &unavailable));
    }

    #[test]
    fn observation_dependent_tools_are_exposed_only_when_actionable() {
        let mut state = AgentState::default();
        assert!(!tool_is_relevant("defend", &state, false));
        assert!(!tool_is_relevant("collect_item", &state, false));
        assert!(!tool_is_relevant("withdraw_item", &state, false));
        assert!(!tool_is_relevant("set_goal", &state, false));
        assert!(tool_is_relevant("follow", &state, false));
        assert!(tool_is_relevant("set_goal", &state, true));

        state.observation.hostiles.push(RelativeEntity::default());
        state.observation.mobs.push(RelativeEntity {
            food_source: true,
            safe_to_hunt: true,
            adult: true,
            food_drops: vec![FoodDropView::default()],
            ..RelativeEntity::default()
        });
        state.observation.chests.push(ChestView {
            accessible: true,
            contents: vec![ChestItemView {
                name: "mcl_core:stone".to_string(),
                count: 1,
            }],
            ..ChestView::default()
        });
        assert!(tool_is_relevant("defend", &state, false));
        assert!(tool_is_relevant("hunt_food", &state, false));
        assert!(tool_is_relevant("withdraw_item", &state, false));
    }

    #[test]
    fn autonomous_planning_waits_only_for_an_active_objective() {
        let now = Instant::now();
        assert!(idle_decision_due(true, false, now, now));
        assert!(!idle_decision_due(false, false, now, now));
        assert!(!idle_decision_due(true, true, now, now));

        let mut state = AgentState::default();
        state.set_goal("Help Alice", None).unwrap();
        assert!(idle_decision_due(
            true,
            state.current_objective.is_some(),
            now,
            now
        ));
    }

    #[test]
    fn active_pathing_waits_for_progress_without_blocking_follow_autonomy() {
        assert!(navigation_blocks_planning("moving"));
        assert!(navigation_blocks_planning("recovering"));
        assert!(!navigation_blocks_planning("following"));
        assert!(!navigation_blocks_planning("failed"));
        assert!(!navigation_blocks_planning("idle"));
    }

    #[test]
    fn a_chat_reply_gets_one_chance_to_continue_the_requested_action() {
        assert!(needs_chat_action_followup(true, true, false));
        assert!(!needs_chat_action_followup(true, true, true));
        assert!(!needs_chat_action_followup(true, false, false));
        assert!(!needs_chat_action_followup(false, true, false));
    }

    #[test]
    fn chat_followups_keep_player_instruction_authority() {
        assert!(is_player_instruction_turn(true, false));
        assert!(is_player_instruction_turn(false, true));
        assert!(!is_player_instruction_turn(false, false));
    }
}
