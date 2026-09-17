//! Deterministic, fully-parameterized actions that a policy may choose from.
//!
//! The policy sees only candidate IDs and descriptions. It cannot invent an
//! endpoint, target, coordinate, or argument; execution still goes through the
//! normal tool validator immediately before dispatch.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::{json, Value};

use super::decision::ToolCall;
use super::planner::{
    item_is_low_value, low_value_request_is_authorized, node_is_low_value,
    progression_recommendations, ProgressionRecommendation,
};
use super::policy::{tool_is_available, tool_is_relevant};
use super::state::{AgentState, RelativeEntity};

const MAX_CANDIDATES: usize = 48;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateSource {
    Safety,
    Player,
    Mission,
    Progression,
    Opportunity,
    Recovery,
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionRisk {
    None,
    Low,
    Material,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CandidateAction {
    Wait,
    Stop,
    Defend,
    Follow {
        target: String,
    },
    GatherResource {
        node: String,
        count: u32,
        radius: u32,
    },
    CollectItem {
        item: String,
    },
    HuntFood {
        target: String,
        radius: u32,
    },
    CraftItem {
        item: String,
        count: u32,
    },
    LoadFurnace {
        position: [i32; 3],
        input: String,
        input_count: u32,
        fuel: String,
        fuel_count: u32,
    },
    CollectFurnaceOutput {
        position: [i32; 3],
        item: String,
        count: u32,
    },
    UseItem {
        item: String,
    },
    Sleep {
        radius: u32,
    },
}

impl CandidateAction {
    pub fn tool_name(&self) -> Option<&'static str> {
        match self {
            Self::Wait => None,
            Self::Stop => Some("stop"),
            Self::Defend => Some("defend"),
            Self::Follow { .. } => Some("follow"),
            Self::GatherResource { .. } => Some("gather_resource"),
            Self::CollectItem { .. } => Some("collect_item"),
            Self::HuntFood { .. } => Some("hunt_food"),
            Self::CraftItem { .. } => Some("craft_item"),
            Self::LoadFurnace { .. } => Some("load_furnace"),
            Self::CollectFurnaceOutput { .. } => Some("collect_furnace_output"),
            Self::UseItem { .. } => Some("use_item"),
            Self::Sleep { .. } => Some("sleep"),
        }
    }

    pub fn to_tool_call(&self, candidate_id: &str) -> Option<ToolCall> {
        let (name, arguments) = match self {
            Self::Wait => return None,
            Self::Stop => ("stop", json!({})),
            Self::Defend => ("defend", json!({})),
            Self::Follow { target } => ("follow", json!({"target": target})),
            Self::GatherResource {
                node,
                count,
                radius,
            } => (
                "gather_resource",
                json!({"node": node, "count": count, "radius": radius}),
            ),
            Self::CollectItem { item } => ("collect_item", json!({"item": item})),
            Self::HuntFood { target, radius } => {
                ("hunt_food", json!({"target": target, "radius": radius}))
            }
            Self::CraftItem { item, count } => {
                ("craft_item", json!({"item": item, "count": count}))
            }
            Self::LoadFurnace {
                position,
                input,
                input_count,
                fuel,
                fuel_count,
            } => (
                "load_furnace",
                json!({
                    "x": position[0], "y": position[1], "z": position[2],
                    "input": input, "input_count": input_count,
                    "fuel": fuel, "fuel_count": fuel_count,
                }),
            ),
            Self::CollectFurnaceOutput {
                position,
                item,
                count,
            } => (
                "collect_furnace_output",
                json!({
                    "x": position[0], "y": position[1], "z": position[2],
                    "item": item, "count": count,
                }),
            ),
            Self::UseItem { item } => ("use_item", json!({"item": item})),
            Self::Sleep { radius } => ("sleep", json!({"radius": radius})),
        };
        Some(ToolCall {
            id: format!("jev-{candidate_id}"),
            name: name.to_string(),
            arguments,
        })
    }
}

#[derive(Clone, Debug)]
pub struct ActionCandidate {
    pub id: String,
    pub description: String,
    pub source: CandidateSource,
    pub risk: ActionRisk,
    pub generated_tick: u64,
    pub action: CandidateAction,
}

impl ActionCandidate {
    pub fn to_tool_call(&self) -> Option<ToolCall> {
        self.action.to_tool_call(&self.id)
    }

    pub fn criterion(&self) -> String {
        format!(
            "{} Source: {:?}. Risk: {:?}.",
            self.description, self.source, self.risk
        )
    }
}

pub struct CandidateContext<'a> {
    pub state: &'a AgentState,
    pub allowed_senders: &'a [String],
    pub bot_name: &'a str,
    pub passive: bool,
    pub autonomous: bool,
    pub player_instruction_turn: bool,
    pub unavailable_tools: &'a HashSet<String>,
}

pub fn generate_candidates(context: CandidateContext<'_>) -> Vec<ActionCandidate> {
    let state = context.state;
    let observation = &state.observation;
    let mut candidates = Vec::with_capacity(24);
    push_unique(
        &mut candidates,
        candidate(
            "wait",
            "Issue no new command and observe the world again. This does not stop existing navigation or following.",
            CandidateSource::Idle,
            ActionRisk::None,
            state.tick,
            CandidateAction::Wait,
        ),
    );

    if context.passive || observation.health == Some(0) {
        return candidates;
    }

    if !observation.hostiles.is_empty() {
        push_unique(
            &mut candidates,
            candidate(
                "defend",
                "Defend against the nearest currently observed hostile using the native combat controller.",
                CandidateSource::Safety,
                ActionRisk::Material,
                state.tick,
                CandidateAction::Defend,
            ),
        );
    }

    let controller = &observation.controller;
    if controller.follow_enabled
        || controller.move_active
        || !matches!(controller.navigation.status.trim(), "" | "idle" | "failed")
    {
        push_unique(
            &mut candidates,
            candidate(
                "stop",
                "Cancel the active movement, path, or follow controller.",
                CandidateSource::Recovery,
                ActionRisk::Low,
                state.tick,
                CandidateAction::Stop,
            ),
        );
    }

    let has_mission = state.current_goal.is_some() || state.current_objective.is_some();
    let may_pursue_work = context.player_instruction_turn || has_mission || context.autonomous;
    if !may_pursue_work {
        return filter_candidates(candidates, &context);
    }

    for (index, target) in follow_targets(&context).into_iter().take(12).enumerate() {
        push_unique(
            &mut candidates,
            candidate(
                format!("follow_{index}"),
                format!("Start or continue following player '{target}'."),
                if context.player_instruction_turn {
                    CandidateSource::Player
                } else {
                    CandidateSource::Mission
                },
                ActionRisk::Low,
                state.tick,
                CandidateAction::Follow { target },
            ),
        );
    }

    for recommendation in progression_recommendations(observation) {
        if let Some(action) = action_from_recommendation(&recommendation) {
            push_unique(
                &mut candidates,
                candidate(
                    sanitize_id(recommendation.id),
                    format!(
                        "{} Success means: {}",
                        recommendation.reason, recommendation.success_criteria
                    ),
                    CandidateSource::Progression,
                    risk_for_action(&action),
                    state.tick,
                    action,
                ),
            );
        }
    }

    let mut seen_nodes = HashSet::new();
    for node in &observation.nearby_nodes {
        let node_key = node.name.to_ascii_lowercase();
        if !seen_nodes.insert(node_key) {
            continue;
        }
        let low_value_authorized = node_is_low_value(node)
            && low_value_request_is_authorized(
                state,
                &node.name,
                &node.groups,
                context.player_instruction_turn,
            );
        let useful = node.groups.iter().any(|group| {
            group.eq_ignore_ascii_case("tree") || group.eq_ignore_ascii_case("ore")
        }) || (!node_is_low_value(node)
            && resource_name_looks_useful(&node.name))
            || low_value_authorized
            || resource_explicitly_requested(
                state,
                &node.name,
                context.player_instruction_turn,
            );
        if !useful {
            continue;
        }
        let index = candidates.len();
        push_unique(
            &mut candidates,
            candidate(
                format!("gather_{index}"),
                format!(
                    "Navigate to and gather up to 2 blocks of observed resource '{}'.",
                    node.name
                ),
                if context.player_instruction_turn {
                    CandidateSource::Player
                } else {
                    CandidateSource::Opportunity
                },
                ActionRisk::Material,
                state.tick,
                CandidateAction::GatherResource {
                    node: node.name.clone(),
                    count: 2,
                    radius: 16,
                },
            ),
        );
        if candidates.len() >= MAX_CANDIDATES {
            break;
        }
    }

    let mut seen_items = HashSet::new();
    for item in &observation.nearby_items {
        let Some(name) = item.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) else {
            continue;
        };
        if !seen_items.insert(name.to_ascii_lowercase()) {
            continue;
        }
        if item_is_low_value(observation, name)
            && !low_value_request_is_authorized(
                state,
                name,
                &[],
                context.player_instruction_turn,
            )
        {
            continue;
        }
        let index = candidates.len();
        push_unique(
            &mut candidates,
            candidate(
                format!("collect_item_{index}"),
                format!("Walk to and collect the nearby dropped item '{name}'."),
                CandidateSource::Opportunity,
                ActionRisk::Low,
                state.tick,
                CandidateAction::CollectItem {
                    item: name.to_string(),
                },
            ),
        );
    }

    let mut seen_mobs = HashSet::new();
    for mob in observation.mobs.iter().filter(|mob| safe_food_source(mob)) {
        let Some(target) = mob.name.as_deref().map(str::trim).filter(|name| !name.is_empty()) else {
            continue;
        };
        if !seen_mobs.insert(target.to_ascii_lowercase()) {
            continue;
        }
        let index = candidates.len();
        push_unique(
            &mut candidates,
            candidate(
                format!("hunt_food_{index}"),
                format!(
                    "Hunt one safe adult '{target}' for food using the native pathing and combat controller."
                ),
                CandidateSource::Opportunity,
                ActionRisk::Material,
                state.tick,
                CandidateAction::HuntFood {
                    target: target.to_string(),
                    radius: 16,
                },
            ),
        );
    }

    for recipe in observation.crafting.craftable.iter().take(8) {
        let index = candidates.len();
        let count = recipe.output_per_batch.max(1);
        push_unique(
            &mut candidates,
            candidate(
                format!("craft_{index}"),
                format!(
                    "Craft {count} '{}' from a currently registered and satisfiable recipe.",
                    recipe.item
                ),
                CandidateSource::Opportunity,
                ActionRisk::Material,
                state.tick,
                CandidateAction::CraftItem {
                    item: recipe.item.clone(),
                    count,
                },
            ),
        );
    }

    if observation.hunger.is_some_and(|hunger| hunger < 15) {
        if let Some(food) = observation.inventory.main.iter().find(|item| {
            item.food || item.food_points.unwrap_or(0.0) > 0.0 || item.food_group > 0
        }) {
            push_unique(
                &mut candidates,
                candidate(
                    "eat_food",
                    format!("Use the observed food item '{}' to restore hunger.", food.name),
                    CandidateSource::Safety,
                    ActionRisk::Low,
                    state.tick,
                    CandidateAction::UseItem {
                        item: food.name.clone(),
                    },
                ),
            );
        }
    }

    if observation.nearby_nodes.iter().any(|node| {
        let name = node.name.to_ascii_lowercase();
        name.contains("bed") || node.groups.iter().any(|group| group == "bed")
    }) {
        push_unique(
            &mut candidates,
            candidate(
                "sleep",
                "Use the nearest observed bed if the server permits sleeping now.",
                CandidateSource::Opportunity,
                ActionRisk::Low,
                state.tick,
                CandidateAction::Sleep { radius: 12 },
            ),
        );
    }

    candidates.truncate(MAX_CANDIDATES);
    filter_candidates(candidates, &context)
}

pub fn choice_criteria(candidates: &[ActionCandidate]) -> BTreeMap<String, Option<String>> {
    candidates
        .iter()
        .map(|candidate| (candidate.id.clone(), Some(candidate.criterion())))
        .collect()
}

pub fn safety_override<'a>(
    candidates: &'a [ActionCandidate],
    state: &AgentState,
) -> Option<&'a ActionCandidate> {
    if state.observation.health == Some(0) {
        return candidates.iter().find(|candidate| candidate.id == "wait");
    }
    let immediate_hostile = state
        .observation
        .hostiles
        .iter()
        .any(|hostile| entity_distance(hostile) <= 4.0);
    immediate_hostile.then(|| {
        candidates
            .iter()
            .find(|candidate| matches!(candidate.action, CandidateAction::Defend))
    })?
}

pub fn choose_candidate<'a>(
    candidates: &'a [ActionCandidate],
    selected_id: &str,
    confidence: f64,
    minimum_confidence: f64,
) -> (&'a ActionCandidate, Option<String>) {
    let wait = candidates
        .iter()
        .find(|candidate| matches!(candidate.action, CandidateAction::Wait))
        .expect("candidate generation must always include wait");
    let Some(selected) = candidates
        .iter()
        .find(|candidate| candidate.id == selected_id)
    else {
        return (
            wait,
            Some(format!("Jev selected unknown candidate '{selected_id}'")),
        );
    };
    if matches!(selected.action, CandidateAction::Wait) {
        return (selected, None);
    }
    let threshold = confidence_threshold(selected.risk, minimum_confidence);
    if !confidence.is_finite() || confidence < threshold {
        return (
            wait,
            Some(format!(
                "confidence {confidence:.3} is below {threshold:.3} for '{}'",
                selected.id
            )),
        );
    }
    (selected, None)
}

pub fn confidence_threshold(risk: ActionRisk, minimum_confidence: f64) -> f64 {
    match risk {
        ActionRisk::None => 0.0,
        ActionRisk::Low => minimum_confidence.clamp(0.0, 1.0),
        ActionRisk::Material => minimum_confidence.max(0.70).clamp(0.0, 1.0),
    }
}

fn filter_candidates(
    candidates: Vec<ActionCandidate>,
    context: &CandidateContext<'_>,
) -> Vec<ActionCandidate> {
    candidates
        .into_iter()
        .filter(|candidate| {
            let Some(name) = candidate.action.tool_name() else {
                return true;
            };
            tool_is_available(
                name,
                !context.state.pending_chat.is_empty(),
                context.state.observation.server_mod_schema_version,
                context.unavailable_tools,
            ) && tool_is_relevant(name, context.state, context.player_instruction_turn)
        })
        .collect()
}

fn action_from_recommendation(
    recommendation: &ProgressionRecommendation,
) -> Option<CandidateAction> {
    let args = &recommendation.arguments;
    match recommendation.action {
        "gather_resource" => Some(CandidateAction::GatherResource {
            node: string_field(args, "node")?.to_string(),
            count: u32_field(args, "count")?,
            radius: u32_field(args, "radius")?,
        }),
        "craft_item" => Some(CandidateAction::CraftItem {
            item: string_field(args, "item")?.to_string(),
            count: u32_field(args, "count")?,
        }),
        "load_furnace" => Some(CandidateAction::LoadFurnace {
            position: coordinate_fields(args)?,
            input: string_field(args, "input")?.to_string(),
            input_count: u32_field(args, "input_count")?,
            fuel: string_field(args, "fuel")?.to_string(),
            fuel_count: u32_field(args, "fuel_count")?,
        }),
        "collect_furnace_output" => Some(CandidateAction::CollectFurnaceOutput {
            position: coordinate_fields(args)?,
            item: string_field(args, "item")?.to_string(),
            count: u32_field(args, "count")?,
        }),
        _ => None,
    }
}

fn string_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    value.get(name)?.as_str()
}

fn u32_field(value: &Value, name: &str) -> Option<u32> {
    u32::try_from(value.get(name)?.as_u64()?).ok()
}

fn coordinate_fields(value: &Value) -> Option<[i32; 3]> {
    Some([
        i32::try_from(value.get("x")?.as_i64()?).ok()?,
        i32::try_from(value.get("y")?.as_i64()?).ok()?,
        i32::try_from(value.get("z")?.as_i64()?).ok()?,
    ])
}

fn candidate(
    id: impl Into<String>,
    description: impl Into<String>,
    source: CandidateSource,
    risk: ActionRisk,
    generated_tick: u64,
    action: CandidateAction,
) -> ActionCandidate {
    ActionCandidate {
        id: id.into(),
        description: description.into(),
        source,
        risk,
        generated_tick,
        action,
    }
}

fn push_unique(candidates: &mut Vec<ActionCandidate>, candidate: ActionCandidate) {
    if !candidates
        .iter()
        .any(|existing| existing.id == candidate.id || existing.action == candidate.action)
    {
        candidates.push(candidate);
    }
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn risk_for_action(action: &CandidateAction) -> ActionRisk {
    match action {
        CandidateAction::Wait => ActionRisk::None,
        CandidateAction::Stop
        | CandidateAction::Follow { .. }
        | CandidateAction::CollectItem { .. }
        | CandidateAction::UseItem { .. }
        | CandidateAction::Sleep { .. } => ActionRisk::Low,
        CandidateAction::Defend
        | CandidateAction::GatherResource { .. }
        | CandidateAction::HuntFood { .. }
        | CandidateAction::CraftItem { .. }
        | CandidateAction::LoadFurnace { .. }
        | CandidateAction::CollectFurnaceOutput { .. } => ActionRisk::Material,
    }
}

fn follow_targets(context: &CandidateContext<'_>) -> Vec<String> {
    let mut targets = BTreeMap::<String, String>::new();
    let mut insert = |name: &str| {
        let name = name.trim();
        if !name.is_empty() && !name.eq_ignore_ascii_case(context.bot_name.trim()) {
            targets
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| name.to_string());
        }
    };
    for name in context.allowed_senders {
        insert(name);
    }
    for message in &context.state.pending_chat {
        insert(&message.from);
    }
    for player in &context.state.observation.players {
        if let Some(name) = player.name.as_deref() {
            insert(name);
        }
    }
    targets.into_values().collect()
}

fn safe_food_source(mob: &RelativeEntity) -> bool {
    mob.food_source
        && mob.safe_to_hunt
        && mob.adult
        && !mob.baby
        && !mob.named
        && !mob.tamed
        && !mob.owned
        && !mob.persistent
        && !mob.food_drops.is_empty()
}

fn entity_distance(entity: &RelativeEntity) -> f32 {
    entity.distance.unwrap_or_else(|| {
        let dx = entity.dx as f32;
        let dy = entity.dy as f32;
        let dz = entity.dz as f32;
        (dx * dx + dy * dy + dz * dz).sqrt()
    })
}

fn resource_name_looks_useful(name: &str) -> bool {
    let name = name.rsplit(':').next().unwrap_or(name).to_ascii_lowercase();
    [
        "coal", "iron", "copper", "gold", "diamond", "tree", "log",
    ]
    .iter()
    .any(|term| name.contains(term))
        || name == "ore"
        || name.contains("_ore")
        || name.starts_with("ore_")
        || name.contains("stone_with_")
}

fn resource_explicitly_requested(
    state: &AgentState,
    resource_name: &str,
    player_instruction_turn: bool,
) -> bool {
    let full_name = resource_name.to_ascii_lowercase();
    let component = full_name.rsplit(':').next().unwrap_or(&full_name);
    if component.len() < 3 {
        return false;
    }
    let mentions = |text: &str| {
        let text = text.to_ascii_lowercase();
        text.contains(&full_name) || text.contains(component)
    };
    state
        .current_goal
        .as_ref()
        .is_some_and(|goal| mentions(&goal.description))
        || (player_instruction_turn
            && (state
                .pending_chat
                .iter()
                .any(|message| mentions(&message.message))
                || state
                    .conversation_history
                    .iter()
                    .rev()
                    .find(|entry| entry.role == "player")
                    .is_some_and(|entry| mentions(&entry.message))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::state::{FoodDropView, NodeView, ObservationSnapshot};

    fn context<'a>(
        state: &'a AgentState,
        unavailable_tools: &'a HashSet<String>,
    ) -> CandidateContext<'a> {
        CandidateContext {
            state,
            allowed_senders: &[],
            bot_name: "Bot",
            passive: false,
            autonomous: true,
            player_instruction_turn: false,
            unavailable_tools,
        }
    }

    #[test]
    fn candidate_ids_are_unique_and_wait_is_always_present() {
        let state = AgentState::default();
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].id, "wait");
        let ids = candidates
            .iter()
            .map(|candidate| candidate.id.as_str())
            .collect::<HashSet<_>>();
        assert_eq!(ids.len(), candidates.len());
    }

    #[test]
    fn immediate_hostile_has_a_deterministic_defend_override() {
        let mut state = AgentState::default();
        state.observation.hostiles.push(RelativeEntity {
            name: Some("mobs_mc:zombie".to_string()),
            distance: Some(2.0),
            ..RelativeEntity::default()
        });
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        let selected = safety_override(&candidates, &state).expect("safety override");
        assert!(matches!(selected.action, CandidateAction::Defend));
    }

    #[test]
    fn progression_candidate_keeps_grounded_tool_arguments() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 8;
        state.observation.nearby_nodes.push(NodeView {
            name: "mcl_core:tree".to_string(),
            groups: vec!["tree".to_string()],
            ..NodeView::default()
        });
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        let tree = candidates
            .iter()
            .find(|candidate| candidate.id == "bootstrap_gather_logs")
            .expect("tree progression candidate");
        let call = tree.to_tool_call().expect("tool call");
        assert_eq!(call.name, "gather_resource");
        assert_eq!(call.arguments["node"], "mcl_core:tree");
        assert_eq!(call.arguments["count"], 2);
    }

    #[test]
    fn old_server_schema_filters_native_gathering() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 5;
        state.observation.nearby_nodes.push(NodeView {
            name: "mcl_core:tree".to_string(),
            groups: vec!["tree".to_string()],
            ..NodeView::default()
        });
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        assert!(!candidates
            .iter()
            .any(|candidate| matches!(candidate.action, CandidateAction::GatherResource { .. })));
    }

    #[test]
    fn autonomy_does_not_offer_common_terrain_without_player_authority() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 8;
        state.observation.nearby_nodes.extend([
            NodeView {
                name: "mcl_core:dirt".to_string(),
                groups: vec!["soil".to_string()],
                ..NodeView::default()
            },
            NodeView {
                name: "mcl_core:stone".to_string(),
                groups: vec!["cracky".to_string()],
                ..NodeView::default()
            },
        ]);
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        assert!(!candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                CandidateAction::GatherResource { node, .. }
                    if node == "mcl_core:dirt" || node == "mcl_core:stone"
            )
        }));
    }

    #[test]
    fn explicit_player_request_can_offer_an_observed_common_node() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 8;
        state.observation.nearby_nodes.push(NodeView {
            name: "mcl_core:stone".to_string(),
            groups: vec!["cracky".to_string()],
            ..NodeView::default()
        });
        state.enqueue_chat([super::super::state::PendingChatMessage {
            id: 1,
            from: "Alice".to_string(),
            message: "Please mine some stone".to_string(),
        }]);
        let unavailable = HashSet::new();
        let mut ctx = context(&state, &unavailable);
        ctx.player_instruction_turn = true;
        let candidates = generate_candidates(ctx);
        assert!(candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                CandidateAction::GatherResource { node, .. } if node == "mcl_core:stone"
            )
        }));
    }

    #[test]
    fn followup_turn_can_recover_resource_request_from_conversation() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 8;
        state.observation.nearby_nodes.push(NodeView {
            name: "mcl_core:stone".to_string(),
            groups: vec!["cracky".to_string()],
            ..NodeView::default()
        });
        state.enqueue_chat([super::super::state::PendingChatMessage {
            id: 1,
            from: "Alice".to_string(),
            message: "Please mine some stone".to_string(),
        }]);
        state.clear_pending_chat();
        let unavailable = HashSet::new();
        let mut ctx = context(&state, &unavailable);
        ctx.player_instruction_turn = true;

        let candidates = generate_candidates(ctx);
        assert!(candidates.iter().any(|candidate| {
            matches!(
                &candidate.action,
                CandidateAction::GatherResource { node, .. } if node == "mcl_core:stone"
            )
        }));
    }

    #[test]
    fn unsafe_animals_never_become_hunt_candidates() {
        let mut state = AgentState::default();
        state.observation.server_mod_schema_version = 8;
        state.observation.mobs.push(RelativeEntity {
            name: Some("mobs_mc:cow".to_string()),
            food_source: true,
            safe_to_hunt: true,
            adult: true,
            tamed: true,
            food_drops: vec![FoodDropView {
                name: "mcl_mobitems:beef".to_string(),
                ..FoodDropView::default()
            }],
            ..RelativeEntity::default()
        });
        let candidates = generate_candidates(context(&state, &HashSet::new()));
        assert!(!candidates
            .iter()
            .any(|candidate| matches!(candidate.action, CandidateAction::HuntFood { .. })));
    }

    #[test]
    fn low_confidence_material_action_falls_back_to_wait() {
        let candidates = vec![
            candidate(
                "wait",
                "wait",
                CandidateSource::Idle,
                ActionRisk::None,
                1,
                CandidateAction::Wait,
            ),
            candidate(
                "mine",
                "mine",
                CandidateSource::Mission,
                ActionRisk::Material,
                1,
                CandidateAction::GatherResource {
                    node: "mcl_core:stone_with_coal".to_string(),
                    count: 1,
                    radius: 8,
                },
            ),
        ];
        let (selected, reason) = choose_candidate(&candidates, "mine", 0.69, 0.55);
        assert_eq!(selected.id, "wait");
        assert!(reason.is_some());
    }

    #[test]
    fn passive_mode_has_no_external_candidates() {
        let mut state = AgentState::default();
        state.observation = ObservationSnapshot {
            hostiles: vec![RelativeEntity {
                name: Some("mobs_mc:zombie".to_string()),
                ..RelativeEntity::default()
            }],
            ..ObservationSnapshot::default()
        };
        let unavailable = HashSet::new();
        let mut ctx = context(&state, &unavailable);
        ctx.passive = true;
        let candidates = generate_candidates(ctx);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].action, CandidateAction::Wait));
    }
}
