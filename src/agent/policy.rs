use std::collections::HashSet;
use std::time::{Duration, Instant};

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
    use super::super::state::AgentState;

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
