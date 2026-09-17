//! Jev-backed selection over deterministic action candidates.

use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use serde_json::{json, Value};

use super::candidates::{
    choice_criteria, choose_candidate, confidence_threshold, safety_override, ActionCandidate,
    CandidateAction,
};
use super::decision::TokenUsage;
use super::jev::{ChoiceAnswer, ChoiceQuestion, ChoiceQuestions, JevClient};
use super::state::{AgentState, RelativeEntity};

const NEXT_ACTION_QUESTION: &str = "next_action";

#[derive(Clone, Debug)]
pub struct JevPolicyDecision {
    /// The model's raw selection, before confidence gating.
    pub proposed_id: String,
    /// The candidate that the controller may execute after gating.
    pub selected_id: String,
    pub confidence: f64,
    /// Confidence required for Jev's raw proposal under the local risk policy.
    pub required_confidence: f64,
    /// Probability assigned to Jev's raw proposal.
    pub proposed_probability: f64,
    /// Probability assigned to the candidate left after local safety gating.
    pub selected_probability: f64,
    pub probabilities: BTreeMap<String, f64>,
    pub usage: TokenUsage,
    pub fallback_reason: Option<String>,
    pub deterministic: bool,
    pub context_bytes: usize,
}

impl JevPolicyDecision {
    pub fn selected<'a>(
        &self,
        candidates: &'a [ActionCandidate],
    ) -> Option<&'a ActionCandidate> {
        candidates
            .iter()
            .find(|candidate| candidate.id == self.selected_id)
    }
}

#[derive(Clone, Debug)]
pub struct JevPolicy {
    client: JevClient,
    minimum_confidence: f64,
}

impl JevPolicy {
    pub fn new(client: JevClient, minimum_confidence: f64) -> Result<Self> {
        anyhow::ensure!(
            minimum_confidence.is_finite() && (0.0..=1.0).contains(&minimum_confidence),
            "Jev minimum confidence must be between 0 and 1"
        );
        Ok(Self {
            client,
            minimum_confidence,
        })
    }

    pub fn model(&self) -> &str {
        self.client.config().model()
    }

    pub fn minimum_confidence(&self) -> f64 {
        self.minimum_confidence
    }

    pub fn decide(
        &self,
        state: &AgentState,
        allowed_senders: &[String],
        candidates: &[ActionCandidate],
    ) -> Result<JevPolicyDecision> {
        anyhow::ensure!(!candidates.is_empty(), "candidate list is empty");
        anyhow::ensure!(
            candidates
                .iter()
                .any(|candidate| matches!(candidate.action, CandidateAction::Wait)),
            "candidate list is missing wait"
        );
        anyhow::ensure!(
            candidates
                .iter()
                .all(|candidate| candidate.generated_tick == state.tick),
            "candidate list is stale for state tick {}",
            state.tick
        );

        let state_view = compact_state(state, allowed_senders);
        let context_bytes = serde_json::to_vec(&state_view)
            .context("serialize compact Jev state")?
            .len();

        if let Some(candidate) = safety_override(candidates, state) {
            return Ok(local_decision(candidate, context_bytes, true, None));
        }
        if candidates.len() == 1 {
            return Ok(local_decision(
                &candidates[0],
                context_bytes,
                true,
                Some("no actionable candidate was available".to_string()),
            ));
        }

        let mut questions = ChoiceQuestions::new();
        questions.insert(
            NEXT_ACTION_QUESTION.to_string(),
            ChoiceQuestion::new(
                "Choose exactly one available action for the Luanti bot now. Prioritize immediate survival, then an explicit authorized player request, then the active mission, hunger, recovery, and useful progression. Avoid repeating recently failed actions. Choose wait when no offered action is presently justified.",
                choice_criteria(candidates),
            ),
        );
        let response = self
            .client
            .evaluate_choices(&state_view, &questions)
            .context("evaluate Jev action candidates")?;
        let answer = response
            .answers
            .get(NEXT_ACTION_QUESTION)
            .context("Jev response omitted next_action")?;
        Ok(decision_from_answer(
            candidates,
            answer,
            TokenUsage {
                input: response.usage.input_tokens,
                cached_input: 0,
                output: response.usage.output_tokens,
                total: response
                    .usage
                    .input_tokens
                    .saturating_add(response.usage.output_tokens),
            },
            self.minimum_confidence,
            context_bytes,
        ))
    }
}

fn decision_from_answer(
    candidates: &[ActionCandidate],
    answer: &ChoiceAnswer,
    usage: TokenUsage,
    minimum_confidence: f64,
    context_bytes: usize,
) -> JevPolicyDecision {
    let proposed_id = answer.choice.clone();
    let required_confidence = candidates
        .iter()
        .find(|candidate| candidate.id == proposed_id)
        .map(|candidate| confidence_threshold(candidate.risk, minimum_confidence))
        .unwrap_or(1.0);
    let (selected, fallback_reason) = choose_candidate(
        candidates,
        &proposed_id,
        answer.confidence,
        minimum_confidence,
    );
    JevPolicyDecision {
        proposed_probability: answer.selected_probability().unwrap_or_default(),
        selected_probability: answer
            .probabilities
            .get(&selected.id)
            .copied()
            .unwrap_or_default(),
        proposed_id,
        selected_id: selected.id.clone(),
        confidence: answer.confidence,
        required_confidence,
        probabilities: answer.probabilities.clone(),
        usage,
        fallback_reason,
        deterministic: false,
        context_bytes,
    }
}

pub fn compact_state(state: &AgentState, allowed_senders: &[String]) -> Value {
    let observation = &state.observation;
    let recent_actions = state
        .action_history
        .iter()
        .rev()
        .filter(|record| !matches!(record.action.as_str(), "observe" | "plan"))
        .take(6)
        .map(|record| {
            json!({
                "action": record.action,
                "ok": record.ok,
                "result": record.result,
                "position": record.position,
            })
        })
        .collect::<Vec<_>>();
    let pending_chat = state
        .pending_chat
        .iter()
        .take(6)
        .map(|message| json!({"from": message.from, "message": message.message}))
        .collect::<Vec<_>>();
    let pending_chat_ids = state
        .pending_chat
        .iter()
        .map(|message| message.id)
        .collect::<HashSet<_>>();
    let recent_conversation = state
        .conversation_history
        .iter()
        .filter(|entry| {
            entry
                .source_id
                .is_none_or(|source_id| !pending_chat_ids.contains(&source_id))
        })
        .rev()
        .take(6)
        .map(|entry| {
            json!({
                "role": entry.role,
                "sender": entry.sender,
                "message": entry.message,
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let inventory = observation
        .inventory
        .main
        .iter()
        .take(24)
        .map(|item| {
            json!({
                "name": item.name,
                "count": item.count,
                "food": item.food || item.food_group > 0 || item.food_points.unwrap_or(0.0) > 0.0,
            })
        })
        .collect::<Vec<_>>();
    let resources = observation
        .nearby_nodes
        .iter()
        .filter(|node| {
            node.groups.iter().any(|group| {
                matches!(group.as_str(), "tree" | "ore" | "bed")
            }) || {
                let name = node.name.to_ascii_lowercase();
                ["coal", "iron", "copper", "gold", "diamond", "tree", "log", "bed"]
                    .iter()
                    .any(|term| name.contains(term))
            }
        })
        .take(24)
        .map(|node| json!({"name": node.name, "position": node.pos, "groups": node.groups}))
        .collect::<Vec<_>>();

    json!({
        "mission": state.current_goal.as_ref().map(|goal| json!({
            "description": goal.description,
            "success_criteria": goal.success_criteria,
            "origin": goal.origin,
        })),
        "objective": state.current_objective.as_ref().map(|objective| json!({
            "description": objective.description,
            "success_criteria": objective.success_criteria,
            "actions_used": objective.actions_used,
            "action_budget": objective.action_budget,
        })),
        "authorized_players": allowed_senders,
        "pending_player_messages": pending_chat,
        "recent_conversation": recent_conversation,
        "bot": {
            "health": observation.health,
            "hunger": observation.hunger,
            "position": observation.position,
            "facing": observation.facing,
            "controller": observation.controller,
        },
        "players": summarize_entities(&observation.players, 12),
        "hostiles": summarize_entities(&observation.hostiles, 16),
        "mobs": summarize_entities(&observation.mobs, 16),
        "nearby_dropped_items": summarize_entities(&observation.nearby_items, 16),
        "inventory": inventory,
        "nearby_resources": resources,
        "accessible_containers": {
            "chests": observation.chests.iter().filter(|chest| chest.accessible).count(),
            "furnaces": observation.furnaces.iter().filter(|furnace| furnace.accessible).count(),
        },
        "recent_actions": recent_actions,
        "consecutive_failures": state.consecutive_failures,
    })
}

fn summarize_entities(entities: &[RelativeEntity], limit: usize) -> Vec<Value> {
    entities
        .iter()
        .take(limit)
        .map(|entity| {
            json!({
                "name": entity.name,
                "category": entity.category,
                "hp": entity.hp,
                "distance": entity.distance,
                "relative_position": [entity.dx, entity.dy, entity.dz],
                "food_source": entity.food_source,
                "safe_to_hunt": entity.safe_to_hunt,
                "adult": entity.adult,
                "named": entity.named,
                "tamed": entity.tamed,
                "owned": entity.owned,
            })
        })
        .collect()
}

fn local_decision(
    candidate: &ActionCandidate,
    context_bytes: usize,
    deterministic: bool,
    fallback_reason: Option<String>,
) -> JevPolicyDecision {
    JevPolicyDecision {
        proposed_id: candidate.id.clone(),
        selected_id: candidate.id.clone(),
        confidence: 1.0,
        required_confidence: 0.0,
        proposed_probability: 1.0,
        selected_probability: 1.0,
        probabilities: BTreeMap::from([(candidate.id.clone(), 1.0)]),
        usage: TokenUsage::default(),
        fallback_reason,
        deterministic,
        context_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::candidates::{ActionRisk, CandidateSource};
    use super::super::state::{ActionRecord, VoxelMapView, VoxelPaletteEntry};

    #[test]
    fn compact_state_excludes_voxel_runs_and_bounds_history() {
        let mut state = AgentState::default();
        state.observation.voxel_map = VoxelMapView {
            runs: vec![[1, 100_000]],
            palette: vec![VoxelPaletteEntry {
                name: "mcl_core:stone".to_string(),
                ..VoxelPaletteEntry::default()
            }],
            ..VoxelMapView::default()
        };
        for index in 0..20 {
            state.action_history.push_back(ActionRecord {
                tick: index,
                action: "move".to_string(),
                ok: true,
                result: "OK".to_string(),
                position: [0, 0, 0],
                arguments: Value::Null,
                fingerprint: None,
            });
        }
        let view = compact_state(&state, &[]);
        let encoded = view.to_string();
        assert!(!encoded.contains("voxel_map"));
        assert!(!encoded.contains("100000"));
        assert_eq!(view["recent_actions"].as_array().unwrap().len(), 6);
    }

    #[test]
    fn compact_state_keeps_bounded_non_pending_conversation() {
        let mut state = AgentState::default();
        for index in 0..10 {
            state.record_bot_chat("Bot", &format!("status {index}"), "status");
            state.enqueue_chat([super::super::state::PendingChatMessage {
                id: index + 1,
                from: "Alice".to_string(),
                message: format!("request {index}"),
            }]);
            state.clear_pending_chat();
        }
        state.enqueue_chat([super::super::state::PendingChatMessage {
            id: 99,
            from: "Alice".to_string(),
            message: "pending request".to_string(),
        }]);

        let view = compact_state(&state, &["Alice".to_string()]);
        let recent = view["recent_conversation"].as_array().unwrap();
        assert_eq!(recent.len(), 6);
        assert!(!recent.iter().any(|entry| entry["message"] == "pending request"));
        assert_eq!(view["pending_player_messages"][0]["message"], "pending request");
    }

    #[test]
    fn confidence_fallback_reports_wait_probability_not_proposal_probability() {
        let candidates = vec![
            ActionCandidate {
                id: "wait".to_string(),
                description: "wait".to_string(),
                source: CandidateSource::Idle,
                risk: ActionRisk::None,
                generated_tick: 1,
                action: CandidateAction::Wait,
            },
            ActionCandidate {
                id: "mine".to_string(),
                description: "mine".to_string(),
                source: CandidateSource::Mission,
                risk: ActionRisk::Material,
                generated_tick: 1,
                action: CandidateAction::GatherResource {
                    node: "mcl_core:stone_with_coal".to_string(),
                    count: 1,
                    radius: 8,
                },
            },
        ];
        let answer = ChoiceAnswer {
            answer_type: super::super::jev::ChoiceAnswerType::Choice,
            choice: "mine".to_string(),
            probabilities: BTreeMap::from([
                ("mine".to_string(), 0.75),
                ("wait".to_string(), 0.25),
            ]),
            confidence: 0.60,
        };

        let decision = decision_from_answer(
            &candidates,
            &answer,
            TokenUsage::default(),
            0.65,
            100,
        );
        assert_eq!(decision.proposed_id, "mine");
        assert_eq!(decision.selected_id, "wait");
        assert_eq!(decision.proposed_probability, 0.75);
        assert_eq!(decision.selected_probability, 0.25);
        assert_eq!(decision.required_confidence, 0.70);
        assert!(decision.fallback_reason.is_some());
    }
}
