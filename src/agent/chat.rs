use super::state::{AgentState, GoalStatus, PendingChatMessage};

pub(super) fn sender_allowed(allowed: &[String], sender: &str) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(sender.trim()))
}

pub(super) fn advance_chat_cursor(saved: u64, server_last: u64) -> u64 {
    if server_last < saved {
        0
    } else {
        saved.max(server_last)
    }
}

pub(super) fn apply_chat_goal_commands(
    state: &mut AgentState,
    chats: &[PendingChatMessage],
) {
    for chat in chats {
        let message = chat.message.trim();
        if let Some(description) = message.strip_prefix("!goal ") {
            let description = description.trim();
            if description.eq_ignore_ascii_case("clear")
                || description.eq_ignore_ascii_case("cancel")
            {
                if state.current_goal.is_some() {
                    let _ = state.finish_goal(
                        GoalStatus::Cancelled,
                        Some(&format!("cancelled by {}", chat.from)),
                    );
                }
            } else if !description.is_empty() {
                let _ = state.set_goal(description, None);
            }
        } else if matches!(message.to_ascii_lowercase().as_str(), "!cancel" | "!cancel goal")
            && state.current_goal.is_some()
        {
            let _ = state.finish_goal(
                GoalStatus::Cancelled,
                Some(&format!("cancelled by {}", chat.from)),
            );
        }
    }
}

pub(super) fn parse_embedded_sender(message: &str) -> Option<(String, String)> {
    if let Some(rest) = message.strip_prefix('<') {
        let end = rest.find('>')?;
        let from = rest[..end].trim();
        let content = rest[end + 1..].trim();
        if !from.is_empty() && !content.is_empty() {
            return Some((from.to_string(), content.to_string()));
        }
    }
    let (from, content) = message.split_once(':')?;
    let from = from.trim();
    let content = content.trim();
    (!from.is_empty() && !content.is_empty()).then(|| (from.to_string(), content.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_game_goal_commands_update_state() {
        let mut state = AgentState::default();
        apply_chat_goal_commands(
            &mut state,
            &[PendingChatMessage {
                id: 1,
                from: "Alice".to_string(),
                message: "!goal collect wood".to_string(),
            }],
        );
        assert_eq!(
            state.current_goal.as_ref().map(|goal| goal.description.as_str()),
            Some("collect wood")
        );
    }

    #[test]
    fn parses_embedded_chat_sender() {
        assert_eq!(
            parse_embedded_sender("<Alice> hello"),
            Some(("Alice".to_string(), "hello".to_string()))
        );
    }

    #[test]
    fn resets_persisted_chat_cursor_after_bot_server_restart() {
        assert_eq!(advance_chat_cursor(42, 3), 0);
        assert_eq!(advance_chat_cursor(3, 5), 5);
    }
}
