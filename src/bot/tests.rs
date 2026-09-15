#[cfg(test)]
mod api_auth_tests {
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    use crate::types::Vec3;
    use crate::bot::chat::{
        bot_protocol_message, collect_chat_entries, is_invalid_server_command, log_chat,
        parse_control_command, resolve_unsupported_bot_command, ChatLog, ControlCommand,
    };
    use crate::bot::navigation::{
        parse_route_goal, ArrivalAction, MoveGoal, NavigationSnapshot,
    };
    use crate::bot::observation::enrich_server_observation;
    use crate::bot::replies::PendingReplies;
    use crate::bot::tasks::{
        InventoryTransferOperation, NativeCraftTask, NativeInventoryTransferPhase,
        NativeInventoryTransferTask, API_INVENTORY_TRANSFER_TIMEOUT,
        NATIVE_INVENTORY_TASK_TIMEOUT,
    };
    use crate::bot::validation::chest_reply_has_nonce;

    #[test]
    fn server_observation_includes_active_controller_state() {
        let goal = MoveGoal {
            target: Vec3 {
                x: 100.0,
                y: 650.0,
                z: -30.0,
            },
            waypoints: std::collections::VecDeque::new(),
            speed: 3.0,
            stop_dist: 2.0,
            arrival_action: None,
        };
        let mut navigation = NavigationSnapshot::default();
        navigation.begin("moving");
        let enriched = enrich_server_observation(
            r#"{"position":[1,2,3]}"#,
            true,
            Some("test"),
            Some(&goal),
            &navigation,
        );
        let value: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        assert_eq!(value["controller"]["follow_target"], "test");
        assert_eq!(value["controller"]["move_target"], json!([10.0, 65.0, -3.0]));
        assert_eq!(value["controller"]["navigation"]["status"], "moving");
        assert_eq!(
            value["controller"]["navigation"]["current_waypoint"],
            json!([10.0, 65.0, -3.0])
        );
    }

    #[test]
    fn route_reply_uses_stand_position_and_collect_arrival_action() {
        let payload = r#"{
            "ok":true,
            "status":"path_found",
            "target":[3,0,0],
            "stand":[2,0,0],
            "path":[[0,0,0],{"x":1,"y":0,"z":0},[2,0,0]],
            "arrival_action":{"type":"collect","node":"mcl_core:coal_ore","count":3,"radius":6}
        }"#;
        let goal = parse_route_goal(
            payload,
            Vec3 {
                x: 0.0,
                y: 0.0,
                z: 0.0,
            },
            4.0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(goal.target.x, 20.0);
        assert_eq!(goal.current_waypoint().x, 10.0);
        assert_eq!(goal.remaining_waypoints(), 2);
        let Some(ArrivalAction::Collect {
            node,
            count,
            radius,
            target,
        }) = goal.arrival_action
        else {
            panic!("collect arrival action was not preserved");
        };
        assert_eq!(node, "mcl_core:coal_ore");
        assert_eq!(count, 3);
        assert_eq!(radius, 6);
        assert_eq!(
            target,
            Some(crate::types::IVec3 { x: 3, y: 0, z: 0 })
        );
    }

    #[test]
    fn hunt_route_carries_hunt_id_to_arrival() {
        let payload = r#"{
            "ok":true,
            "status":"path_found",
            "stand":[1,0,0],
            "path":[[1,0,0]],
            "arrival_action":{"type":"hunt","hunt_id":42}
        }"#;
        let goal = parse_route_goal(payload, Vec3::default(), 4.0)
            .unwrap()
            .unwrap();
        assert!(matches!(
            goal.arrival_action,
            Some(ArrivalAction::Hunt { hunt_id: 42 })
        ));
    }

    #[test]
    fn bot_protocol_messages_are_separate_from_player_chat() {
        assert_eq!(
            bot_protocol_message("BOT_HUNT {\"ok\":true}"),
            Some(("BOT_HUNT", "{\"ok\":true}"))
        );
        assert_eq!(bot_protocol_message("hello BOT_HUNT"), None);
    }

    #[test]
    fn navigation_failure_persists_until_stop() {
        let mut navigation = NavigationSnapshot::default();
        navigation.fail("stuck_after_recovery");
        assert_eq!(navigation.status, "failed");
        assert_eq!(navigation.last_error.as_deref(), Some("stuck_after_recovery"));

        navigation.stop();
        assert_eq!(navigation.status, "idle");
        assert_eq!(navigation.last_error, None);
    }

    #[test]
    fn recognizes_builtin_invalid_command_responses() {
        assert!(is_invalid_server_command(
            "-!- @__builtin)Invalid command",
            "bot_collect",
            "bot_collect"
        ));
    }

    #[test]
    fn first_chat_message_is_visible_from_the_initial_cursor() {
        let log = Arc::new(Mutex::new(ChatLog::default()));
        log_chat(&log, "Alice", "hello");

        let (entries, last_id) = collect_chat_entries(&log, 0, 10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 1);
        assert_eq!(last_id, 1);
    }

    #[test]
    fn native_inventory_tasks_get_distinct_reply_nonces() {
        let target = crate::types::IVec3 { x: 1, y: 2, z: 3 };
        let first = NativeInventoryTransferTask::new(
            InventoryTransferOperation::ChestDeposit,
            target,
            "mcl_core:stone".to_string(),
            1,
        );
        let second = NativeInventoryTransferTask::new(
            InventoryTransferOperation::ChestDeposit,
            target,
            "mcl_core:stone".to_string(),
            1,
        );
        assert_ne!(first.id, second.id);
        assert!(NATIVE_INVENTORY_TASK_TIMEOUT < API_INVENTORY_TRANSFER_TIMEOUT);
    }

    #[test]
    fn inventory_transfer_operations_route_to_the_correct_adapter_and_list() {
        let cases = [
            (
                InventoryTransferOperation::ChestDeposit,
                "chest",
                "bot_chest_prepare",
                "main",
                true,
            ),
            (
                InventoryTransferOperation::ChestWithdraw,
                "chest",
                "bot_chest_prepare",
                "main",
                false,
            ),
            (
                InventoryTransferOperation::FurnaceInput,
                "furnace",
                "bot_furnace_prepare",
                "src",
                true,
            ),
            (
                InventoryTransferOperation::FurnaceFuel,
                "furnace",
                "bot_furnace_prepare",
                "fuel",
                true,
            ),
            (
                InventoryTransferOperation::FurnaceOutput,
                "furnace",
                "bot_furnace_prepare",
                "dst",
                false,
            ),
        ];
        for (operation, kind, prepare, list, deposits) in cases {
            assert_eq!(operation.container_kind(), kind);
            assert_eq!(operation.prepare_command(), prepare);
            assert_eq!(operation.expected_container_list(), list);
            assert_eq!(operation.deposits_into_container(), deposits);
        }
    }

    #[test]
    fn dispatched_inventory_actions_report_an_unknown_outcome_when_cancelled() {
        assert!(!NativeInventoryTransferPhase::Ready.action_dispatched());
        assert!(!NativeInventoryTransferPhase::Preparing {
            nonce: "prepare".to_string(),
            deadline: std::time::Instant::now(),
        }
        .action_dispatched());
        assert!(NativeInventoryTransferPhase::WaitingForReceipt {
            nonce: "sent".to_string(),
            action_count: 1,
            poll_at: std::time::Instant::now(),
            expires_at: std::time::Instant::now(),
        }
        .action_dispatched());

        let mut task = NativeInventoryTransferTask::new(
            InventoryTransferOperation::FurnaceInput,
            crate::types::IVec3 { x: 1, y: 2, z: 3 },
            "mcl_mobitems:beef".to_string(),
            2,
        );
        task.moved = 1;
        task.last_error = Some("receipt_timeout_outcome_unknown".to_string());
        assert_eq!(
            task.result_status(),
            ("receipt_timeout_outcome_unknown", false, true)
        );
    }

    #[test]
    fn craft_tasks_get_distinct_nonces_and_keep_minimum_output() {
        let first = NativeCraftTask::new("mcl_core:stick".to_string(), 4);
        let second = NativeCraftTask::new("mcl_core:stick".to_string(), 8);
        assert_ne!(first.id, second.id);
        assert_eq!(first.requested, 4);
        assert_eq!(second.requested, 8);
    }

    #[test]
    fn chest_replies_require_the_current_nonce() {
        let current = json!({"nonce": "a-2", "ok": true});
        assert!(chest_reply_has_nonce(&current, "a-2"));
        assert!(!chest_reply_has_nonce(&current, "a-1"));
        assert!(!chest_reply_has_nonce(&json!({"ok": true}), "a-2"));
    }

    #[test]
    fn attackall_remains_an_alias_for_attack() {
        let Some(ControlCommand::Attack(target)) = parse_control_command("!attackall Alice") else {
            panic!("!attackall did not parse as attack");
        };
        assert_eq!(target, "Alice");
    }

    #[test]
    fn pending_replies_are_resolved_by_tag() {
        let pending = PendingReplies::default();
        let receiver = pending.register("BOT_TEST").unwrap();
        assert!(pending.register("BOT_TEST").is_none());
        assert!(pending.resolve("BOT_TEST", "reply".to_string()));
        assert_eq!(receiver.recv().unwrap(), "reply");
        assert!(!pending.resolve("BOT_TEST", "late".to_string()));
    }

    #[test]
    fn unsupported_world_commands_resolve_the_matching_request() {
        let pending = PendingReplies::default();
        let receiver = pending.register("BOT_FIGHT").unwrap();
        assert_eq!(
            resolve_unsupported_bot_command(
                &pending,
                "-!- @__builtin)Invalid command",
                "bot_fight",
            ),
            Some("bot_fight")
        );
        assert!(receiver.recv().unwrap().contains("unsupported_command"));

        let receiver = pending.register("BOT_CHEST_INSPECT").unwrap();
        assert_eq!(
            resolve_unsupported_bot_command(
                &pending,
                "-!- @__builtin)Invalid command",
                "bot_chest_inspect",
            ),
            Some("bot_chest_inspect")
        );
        assert!(receiver.recv().unwrap().contains("unsupported_command"));
    }
}
