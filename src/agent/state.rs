use anyhow::{Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::Path;

use super::provider::TokenUsage;
use super::util::clip_chars;

const ACTION_HISTORY_LIMIT: usize = 40;
const REPEATED_FAILURE_LIMIT: usize = 2;

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Booting,
    Observing,
    Planning,
    Acting,
    Waiting,
    Recovering,
    Idle,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Active,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct GoalState {
    pub id: u64,
    pub description: String,
    pub success_criteria: Option<String>,
    pub status: GoalStatus,
    pub created_tick: u64,
    pub updated_tick: u64,
    pub outcome: Option<String>,
    #[serde(default)]
    pub origin: GoalOrigin,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum GoalOrigin {
    /// State files written before goal provenance was recorded. Treat this as
    /// user-authorized for backwards compatibility with configured missions.
    #[default]
    Unknown,
    Configured,
    Player,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectiveOrigin {
    #[default]
    Autonomous,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ObjectiveState {
    pub id: u64,
    pub description: String,
    pub success_criteria: Option<String>,
    pub status: GoalStatus,
    pub origin: ObjectiveOrigin,
    pub created_tick: u64,
    pub updated_tick: u64,
    pub outcome: Option<String>,
    pub action_budget: u32,
    pub actions_used: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ItemStackView {
    pub name: String,
    pub count: u32,
    #[serde(default)]
    pub wear: u32,
    #[serde(default)]
    pub food_points: Option<f32>,
    #[serde(default)]
    pub food: bool,
    #[serde(default)]
    pub food_group: i32,
    #[serde(default)]
    pub food_saturation: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FoodDropView {
    pub name: String,
    #[serde(default = "default_one")]
    pub chance: u32,
    #[serde(default = "default_one")]
    pub min: u32,
    #[serde(default = "default_one")]
    pub max: u32,
    #[serde(default)]
    pub food_points: f32,
    #[serde(default)]
    pub food_saturation: Option<f32>,
}

impl Default for FoodDropView {
    fn default() -> Self {
        Self {
            name: String::new(),
            chance: 1,
            min: 1,
            max: 1,
            food_points: 0.0,
            food_saturation: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct InventoryView {
    pub wield: Option<ItemStackView>,
    pub main: Vec<ItemStackView>,
    pub main_truncated: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct RelativeEntity {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub count: Option<u32>,
    #[serde(default)]
    pub hp: Option<i32>,
    #[serde(default)]
    pub distance: Option<f32>,
    #[serde(default, alias = "huntable_for_food")]
    pub food_source: bool,
    #[serde(default)]
    pub safe_to_hunt: bool,
    #[serde(default)]
    pub passive: bool,
    #[serde(default, alias = "is_baby")]
    pub baby: bool,
    #[serde(default)]
    pub adult: bool,
    #[serde(default)]
    pub named: bool,
    #[serde(default)]
    pub tamed: bool,
    #[serde(default)]
    pub owned: bool,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub food_drops: Vec<FoodDropView>,
    #[serde(default)]
    pub hunt_blocked_reason: Option<String>,
    pub dx: i32,
    pub dy: i32,
    pub dz: i32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NodeView {
    pub name: String,
    pub pos: [i32; 3],
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChestItemView {
    pub name: String,
    pub count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ChestView {
    pub name: String,
    pub kind: String,
    pub pos: [i32; 3],
    pub distance: f32,
    pub accessible: bool,
    pub status: String,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub contents: Vec<ChestItemView>,
    #[serde(default)]
    pub contents_truncated: bool,
    #[serde(default)]
    pub used_slots: Option<u32>,
    #[serde(default)]
    pub slots: Option<u32>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct ContainerItemView {
    pub name: String,
    pub count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FurnaceRecipeView {
    pub output: ContainerItemView,
    #[serde(default)]
    pub cook_time: f32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FurnaceInputOptionView {
    pub name: String,
    pub count: u32,
    pub output: ContainerItemView,
    #[serde(default)]
    pub cook_time: f32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FurnaceFuelOptionView {
    pub name: String,
    pub count: u32,
    #[serde(default)]
    pub burn_time: f32,
    #[serde(default)]
    pub replacement: Option<ContainerItemView>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct FurnaceView {
    pub name: String,
    pub kind: String,
    pub pos: [i32; 3],
    pub distance: f32,
    pub accessible: bool,
    pub status: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default = "default_one_f32")]
    pub speed: f32,
    #[serde(default)]
    pub input: Option<ContainerItemView>,
    #[serde(default)]
    pub fuel: Option<ContainerItemView>,
    #[serde(default)]
    pub output: Option<ContainerItemView>,
    #[serde(default)]
    pub activity: Option<String>,
    #[serde(default)]
    pub cookable: bool,
    #[serde(default)]
    pub output_blocked: bool,
    #[serde(default)]
    pub cook_progress: f32,
    #[serde(default)]
    pub fuel_remaining: f32,
    #[serde(default)]
    pub fuel_total: f32,
    #[serde(default)]
    pub recipe: Option<FurnaceRecipeView>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub input_options: Vec<FurnaceInputOptionView>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub fuel_options: Vec<FurnaceFuelOptionView>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct CraftIngredientView {
    pub item: String,
    pub count: u32,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct CraftableItemView {
    pub item: String,
    /// Number of output items produced by one recipe execution.
    pub output_per_batch: u32,
    #[serde(default)]
    pub max_batches: u32,
    #[serde(default)]
    pub table_required: bool,
    #[serde(default)]
    pub ingredients: Vec<CraftIngredientView>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
pub struct CraftingView {
    pub craftable: Vec<CraftableItemView>,
}

fn deserialize_null_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VoxelPaletteEntry {
    pub name: String,
    pub walkable: bool,
    pub diggable: bool,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct VoxelMapView {
    pub radius: u32,
    pub origin: [i32; 3],
    pub size: [u32; 3],
    pub order: String,
    pub palette: Vec<VoxelPaletteEntry>,
    pub runs: Vec<[u32; 2]>,
    pub complete: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ObstaclesView {
    pub front: String,
    pub left: String,
    pub right: String,
    pub back: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct NavigationView {
    pub status: String,
    pub current_waypoint: Option<[f32; 3]>,
    pub waypoints_remaining: u32,
    pub recovering: bool,
    pub recovery_attempts: u8,
    pub stalled_for_seconds: f32,
    pub last_error: Option<String>,
    pub arrival_action: Option<String>,
}

impl Default for NavigationView {
    fn default() -> Self {
        Self {
            status: "idle".to_string(),
            current_waypoint: None,
            waypoints_remaining: 0,
            recovering: false,
            recovery_attempts: 0,
            stalled_for_seconds: 0.0,
            last_error: None,
            arrival_action: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ControllerView {
    pub follow_enabled: bool,
    pub follow_target: Option<String>,
    pub move_active: bool,
    pub move_target: Option<[f32; 3]>,
    #[serde(default)]
    pub navigation: NavigationView,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ObservationSnapshot {
    #[serde(default)]
    pub server_mod_schema_version: u32,
    pub health: Option<i32>,
    #[serde(default)]
    pub hunger_available: bool,
    #[serde(default)]
    pub hunger: Option<i32>,
    #[serde(default)]
    pub saturation: Option<f32>,
    pub position: [i32; 3],
    pub facing: String,
    pub inventory: InventoryView,
    pub players: Vec<RelativeEntity>,
    pub hostiles: Vec<RelativeEntity>,
    #[serde(default)]
    pub mobs: Vec<RelativeEntity>,
    pub nearby_items: Vec<RelativeEntity>,
    pub nearby_nodes: Vec<NodeView>,
    #[serde(default)]
    pub chests: Vec<ChestView>,
    #[serde(default)]
    pub furnaces: Vec<FurnaceView>,
    #[serde(default)]
    pub crafting: CraftingView,
    #[serde(default)]
    pub voxel_map: VoxelMapView,
    pub obstacles: ObstaclesView,
    #[serde(default)]
    pub controller: ControllerView,
    pub server_goal: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StateEvent {
    pub tick: u64,
    pub phase: AgentPhase,
    pub message: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CumulativeUsage {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct PendingChatMessage {
    pub id: u64,
    pub from: String,
    pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
pub struct ConversationEntry {
    pub tick: u64,
    pub role: String,
    pub sender: String,
    pub message: String,
    #[serde(default)]
    pub source_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ActionRecord {
    pub tick: u64,
    pub action: String,
    pub ok: bool,
    pub result: String,
    pub position: [i32; 3],
    #[serde(default)]
    pub arguments: Value,
    #[serde(default)]
    pub fingerprint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AgentState {
    pub version: u32,
    pub tick: u64,
    pub phase: AgentPhase,
    pub phase_reason: String,
    pub current_goal: Option<GoalState>,
    pub goal_history: VecDeque<GoalState>,
    #[serde(default)]
    pub current_objective: Option<ObjectiveState>,
    #[serde(default)]
    pub objective_history: VecDeque<ObjectiveState>,
    pub observation: ObservationSnapshot,
    pub last_action: Option<String>,
    pub last_result: Option<String>,
    pub consecutive_failures: u32,
    pub usage: CumulativeUsage,
    #[serde(default)]
    pub last_seen_chat_id: u64,
    #[serde(default)]
    pub pending_chat: VecDeque<PendingChatMessage>,
    #[serde(default)]
    pub conversation_history: VecDeque<ConversationEntry>,
    #[serde(default)]
    pub action_history: VecDeque<ActionRecord>,
    pub recent_events: VecDeque<StateEvent>,
    #[serde(default = "default_next_goal_id")]
    next_goal_id: u64,
    #[serde(default = "default_next_goal_id")]
    next_objective_id: u64,
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            version: 3,
            tick: 0,
            phase: AgentPhase::Booting,
            phase_reason: "agent starting".to_string(),
            current_goal: None,
            goal_history: VecDeque::new(),
            current_objective: None,
            objective_history: VecDeque::new(),
            observation: ObservationSnapshot::default(),
            last_action: None,
            last_result: None,
            consecutive_failures: 0,
            usage: CumulativeUsage::default(),
            last_seen_chat_id: 0,
            pending_chat: VecDeque::new(),
            conversation_history: VecDeque::new(),
            action_history: VecDeque::new(),
            recent_events: VecDeque::new(),
            next_goal_id: 1,
            next_objective_id: 1,
        }
    }
}

impl AgentState {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("read agent state {}", path.display()))?;
        let mut state: Self = serde_json::from_str(&raw)
            .with_context(|| format!("parse agent state {}", path.display()))?;
        state.next_goal_id = state
            .current_goal
            .iter()
            .chain(state.goal_history.iter())
            .map(|goal| goal.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        state.next_objective_id = state
            .current_objective
            .iter()
            .chain(state.objective_history.iter())
            .map(|objective| objective.id)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        state.version = 3;
        let history = std::mem::take(&mut state.conversation_history);
        for entry in history {
            state.push_conversation(entry);
        }
        state.transition(AgentPhase::Booting, "resumed from state file");
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent)
                .with_context(|| format!("create state directory {}", parent.display()))?;
        }
        let tmp_path = path.with_extension("tmp");
        let body = serde_json::to_vec_pretty(self).context("serialize agent state")?;
        fs::write(&tmp_path, body)
            .with_context(|| format!("write temporary state {}", tmp_path.display()))?;
        fs::rename(&tmp_path, path)
            .with_context(|| format!("replace agent state {}", path.display()))?;
        Ok(())
    }

    pub fn transition(&mut self, phase: AgentPhase, reason: impl Into<String>) {
        let reason = reason.into();
        self.phase = phase.clone();
        self.phase_reason = reason.clone();
        self.recent_events.push_back(StateEvent {
            tick: self.tick,
            phase,
            message: reason,
        });
        while self.recent_events.len() > 20 {
            self.recent_events.pop_front();
        }
    }

    pub fn set_goal(&mut self, description: &str, success_criteria: Option<&str>) -> Result<u64> {
        self.set_goal_with_origin(description, success_criteria, GoalOrigin::Player)
    }

    pub fn set_configured_goal(
        &mut self,
        description: &str,
        success_criteria: Option<&str>,
    ) -> Result<u64> {
        self.set_goal_with_origin(description, success_criteria, GoalOrigin::Configured)
    }

    fn set_goal_with_origin(
        &mut self,
        description: &str,
        success_criteria: Option<&str>,
        origin: GoalOrigin,
    ) -> Result<u64> {
        let description = description.trim();
        anyhow::ensure!(!description.is_empty(), "goal description is empty");
        self.cancel_objective("superseded by a new mission");
        if let Some(mut previous) = self.current_goal.take() {
            previous.status = GoalStatus::Cancelled;
            previous.updated_tick = self.tick;
            previous.outcome = Some("superseded by a new goal".to_string());
            self.push_goal_history(previous);
        }
        let id = self.next_goal_id;
        self.next_goal_id = self.next_goal_id.saturating_add(1);
        self.current_goal = Some(GoalState {
            id,
            description: clip_chars(description, 500),
            success_criteria: success_criteria
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| clip_chars(value, 500)),
            status: GoalStatus::Active,
            created_tick: self.tick,
            updated_tick: self.tick,
            outcome: None,
            origin,
        });
        self.transition(AgentPhase::Planning, format!("goal {id} set"));
        Ok(id)
    }

    pub fn finish_goal(&mut self, status: GoalStatus, outcome: Option<&str>) -> Result<()> {
        anyhow::ensure!(status != GoalStatus::Active, "goal cannot finish as active");
        let mut goal = self
            .current_goal
            .take()
            .context("there is no active goal")?;
        goal.status = status;
        goal.updated_tick = self.tick;
        goal.outcome = outcome
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| clip_chars(value, 500));
        let id = goal.id;
        self.push_goal_history(goal);
        self.cancel_objective("mission finished");
        self.transition(AgentPhase::Idle, format!("goal {id} finished"));
        Ok(())
    }

    pub fn set_objective(
        &mut self,
        description: &str,
        success_criteria: Option<&str>,
        action_budget: u32,
    ) -> Result<u64> {
        let description = description.trim();
        anyhow::ensure!(!description.is_empty(), "objective description is empty");
        anyhow::ensure!(
            self.current_objective.is_none(),
            "finish the active objective before choosing another"
        );
        let id = self.next_objective_id;
        self.next_objective_id = self.next_objective_id.saturating_add(1);
        self.current_objective = Some(ObjectiveState {
            id,
            description: clip_chars(description, 500),
            success_criteria: success_criteria
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| clip_chars(value, 500)),
            status: GoalStatus::Active,
            origin: ObjectiveOrigin::Autonomous,
            created_tick: self.tick,
            updated_tick: self.tick,
            outcome: None,
            action_budget: action_budget.clamp(2, 12),
            actions_used: 0,
        });
        self.transition(AgentPhase::Planning, format!("objective {id} set"));
        Ok(id)
    }

    pub fn finish_objective(&mut self, status: GoalStatus, outcome: Option<&str>) -> Result<()> {
        anyhow::ensure!(status != GoalStatus::Active, "objective cannot finish as active");
        let mut objective = self
            .current_objective
            .take()
            .context("there is no active objective")?;
        objective.status = status;
        objective.updated_tick = self.tick;
        objective.outcome = outcome
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| clip_chars(value, 500));
        let id = objective.id;
        self.push_objective_history(objective);
        self.transition(AgentPhase::Planning, format!("objective {id} finished"));
        Ok(())
    }

    pub fn objective_budget_exhausted(&self) -> bool {
        self.current_objective
            .as_ref()
            .is_some_and(|objective| objective.actions_used >= objective.action_budget)
    }

    pub fn update_observation(&mut self, observation: ObservationSnapshot) {
        self.observation = observation;
        self.transition(AgentPhase::Planning, "world state refreshed");
    }

    pub fn record_usage(&mut self, usage: &TokenUsage) {
        self.usage.requests = self.usage.requests.saturating_add(1);
        self.usage.input_tokens = self.usage.input_tokens.saturating_add(usage.input);
        self.usage.output_tokens = self.usage.output_tokens.saturating_add(usage.output);
        self.usage.total_tokens = self.usage.total_tokens.saturating_add(usage.total);
    }

    pub fn record_action_result(&mut self, action: &str, ok: bool, result: &str) {
        self.record_result(action, &Value::Null, ok, result);
    }

    pub fn record_tool_result(&mut self, action: &str, args: &Value, ok: bool, result: &str) {
        self.record_result(action, args, ok, result);
    }

    pub fn repeated_failure_reason(&self, action: &str, args: &Value) -> Option<String> {
        if is_bookkeeping_action(action) {
            return None;
        }
        let fingerprint = action_fingerprint(action, args, self.observation.position);
        let mut failures = 0;
        for record in self.action_history.iter().rev() {
            if record.fingerprint.as_deref() != Some(fingerprint.as_str()) {
                continue;
            }
            if record.ok {
                break;
            }
            if transient_failure(&record.result) {
                continue;
            }
            failures += 1;
            if failures >= REPEATED_FAILURE_LIMIT {
                return Some(format!(
                    "suppressed_repeated_failure: '{action}' already failed {failures} times with the same arguments at this position; change the target or move before retrying"
                ));
            }
        }
        None
    }

    fn record_result(&mut self, action: &str, args: &Value, ok: bool, result: &str) {
        self.last_action = Some(action.to_string());
        self.last_result = Some(clip_chars(result, 500));
        if !matches!(action, "observe" | "plan") {
            let external_action = !is_bookkeeping_action(action);
            self.action_history.push_back(ActionRecord {
                tick: self.tick,
                action: action.to_string(),
                ok,
                result: clip_chars(result, 500),
                position: self.observation.position,
                arguments: args.clone(),
                fingerprint: external_action.then(|| {
                    action_fingerprint(action, args, self.observation.position)
                }),
            });
            while self.action_history.len() > ACTION_HISTORY_LIMIT {
                self.action_history.pop_front();
            }
            if external_action {
                if let Some(objective) = self.current_objective.as_mut() {
                    if !is_safety_action(action) {
                        objective.actions_used = objective.actions_used.saturating_add(1);
                        objective.updated_tick = self.tick;
                    }
                }
                if ok {
                    self.consecutive_failures = 0;
                } else {
                    self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                }
            }
        }
        if ok {
            self.transition(AgentPhase::Waiting, format!("{action}: {result}"));
        } else {
            self.transition(AgentPhase::Recovering, format!("{action} failed: {result}"));
        }
    }

    pub fn enqueue_chat(&mut self, messages: impl IntoIterator<Item = PendingChatMessage>) {
        for message in messages {
            if self.pending_chat.iter().any(|pending| pending.id == message.id) {
                continue;
            }
            self.push_conversation(ConversationEntry {
                tick: self.tick,
                role: "player".to_string(),
                sender: message.from.clone(),
                message: clip_chars(&message.message, 500),
                source_id: Some(message.id),
            });
            self.pending_chat.push_back(message);
        }
        while self.pending_chat.len() > 12 {
            self.pending_chat.pop_front();
        }
    }

    pub fn clear_pending_chat(&mut self) {
        self.pending_chat.clear();
    }

    pub fn record_bot_chat(&mut self, sender: &str, message: &str, role: &str) {
        self.push_conversation(ConversationEntry {
            tick: self.tick,
            role: role.to_string(),
            sender: sender.to_string(),
            message: clip_chars(message, 500),
            source_id: None,
        });
    }

    pub fn prompt_view(&self) -> Value {
        serde_json::json!({
            "tick": self.tick,
            "phase": self.phase,
            "phase_reason": self.phase_reason,
            "goal": self.current_goal,
            "mission": self.current_goal,
            "objective": self.current_objective,
            "recent_objectives": tail(&self.objective_history, 8),
            "observation": self.observation,
            "last_action": self.last_action,
            "last_result": self.last_result,
            "consecutive_failures": self.consecutive_failures,
            "pending_chat": self.pending_chat,
            "conversation_history": tail(&self.conversation_history, 24),
            "action_history": tail(&self.action_history, 12),
        })
    }

    fn push_goal_history(&mut self, goal: GoalState) {
        self.goal_history.push_back(goal);
        while self.goal_history.len() > 20 {
            self.goal_history.pop_front();
        }
    }

    fn push_objective_history(&mut self, objective: ObjectiveState) {
        self.objective_history.push_back(objective);
        while self.objective_history.len() > 20 {
            self.objective_history.pop_front();
        }
    }

    fn cancel_objective(&mut self, outcome: &str) {
        if let Some(mut objective) = self.current_objective.take() {
            objective.status = GoalStatus::Cancelled;
            objective.updated_tick = self.tick;
            objective.outcome = Some(outcome.to_string());
            self.push_objective_history(objective);
        }
    }

    fn push_conversation(&mut self, entry: ConversationEntry) {
        let replaces_unsolicited_bot_run = entry.role != "player"
            && self
                .conversation_history
                .back()
                .is_some_and(|previous| previous.role != "player" && previous.sender == entry.sender);
        if replaces_unsolicited_bot_run {
            self.conversation_history.pop_back();
        }
        self.conversation_history.push_back(entry);
        while self.conversation_history.len() > 60 {
            self.conversation_history.pop_front();
        }
    }
}

fn is_bookkeeping_action(action: &str) -> bool {
    matches!(
        action.trim(),
        "observe"
            | "plan"
            | "say"
            | "set_goal"
            | "finish_goal"
            | "set_objective"
            | "finish_objective"
    )
}

fn is_safety_action(action: &str) -> bool {
    matches!(action.trim(), "stop" | "defend")
}

fn transient_failure(result: &str) -> bool {
    let result = result.to_ascii_lowercase();
    [
        "connection refused",
        "connection reset",
        "timed out",
        "timeout",
        "request_already_in_progress",
        "bot_unavailable",
        "429 too many requests",
        "500 internal server error",
        "502 bad gateway",
        "503 service unavailable",
        "504 gateway timeout",
    ]
    .iter()
    .any(|marker| result.contains(marker))
}

fn action_fingerprint(action: &str, args: &Value, position: [i32; 3]) -> String {
    format!(
        "{}@{},{},{}:{}",
        action.trim(),
        position[0],
        position[1],
        position[2],
        canonical_json(args)
    )
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string()),
                        canonical_json(value)
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn tail<T>(values: &VecDeque<T>, limit: usize) -> Vec<&T> {
    values
        .iter()
        .skip(values.len().saturating_sub(limit))
        .collect()
}

pub fn parse_observation(raw: &str) -> Result<ObservationSnapshot> {
    let value: Value = serde_json::from_str(raw).context("parse observation JSON")?;
    let position = parse_position(value.get("position")).unwrap_or([0, 0, 0]);
    let inventory = parse_inventory(value.get("inventory"));
    let mut players = parse_entities(value.get("players"), Some("player"));
    if players.is_empty() {
        players = parse_entities(value.get("items"), Some("player"));
    }
    let nearby_items = parse_entities(value.get("items"), None)
        .into_iter()
        .filter(|entity| entity.kind != "player")
        .collect();
    let hostiles = parse_entities(value.get("hostiles"), None);
    let mobs = parse_entities(value.get("mobs"), None);
    let nearby_nodes = prioritize_and_deduplicate_nodes(parse_nodes(value.get("nodes")), position, 40);
    let mut chests = value
        .get("chests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|chest| serde_json::from_value::<ChestView>(chest.clone()).ok())
        .collect::<Vec<_>>();
    chests.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    chests.truncate(8);
    let mut furnaces = value
        .get("furnaces")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|furnace| serde_json::from_value::<FurnaceView>(furnace.clone()).ok())
        .collect::<Vec<_>>();
    furnaces.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    furnaces.truncate(8);
    let crafting = parse_crafting(&value);
    Ok(ObservationSnapshot {
        server_mod_schema_version: value
            .get("schema_version")
            .and_then(Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .unwrap_or(0),
        health: value
            .get("health")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        hunger_available: value
            .get("hunger_available")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        hunger: value
            .get("hunger")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        saturation: value
            .get("saturation")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        position,
        facing: value
            .get("facing")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        inventory,
        players,
        hostiles,
        mobs,
        nearby_items,
        nearby_nodes,
        chests,
        furnaces,
        crafting,
        voxel_map: parse_voxel_map(value.get("voxel_map")),
        obstacles: parse_obstacles(value.get("obstacles")),
        controller: parse_controller(value.get("controller")),
        server_goal: value
            .get("goal")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

fn parse_voxel_map(value: Option<&Value>) -> VoxelMapView {
    let Some(value) = value else {
        return VoxelMapView::default();
    };
    let mut map: VoxelMapView = serde_json::from_value(value.clone()).unwrap_or_default();
    let expected = map
        .size
        .iter()
        .fold(1_u64, |total, side| total.saturating_mul(u64::from(*side)));
    let actual = map
        .runs
        .iter()
        .fold(0_u64, |total, run| total.saturating_add(u64::from(run[1])));
    let palette_valid = map
        .runs
        .iter()
        .all(|run| run[0] > 0 && usize::try_from(run[0]).is_ok_and(|id| id <= map.palette.len()));
    map.complete &= expected > 0 && actual == expected && palette_valid;
    map
}

fn parse_controller(value: Option<&Value>) -> ControllerView {
    let move_target = value
        .and_then(|value| value.get("move_target"))
        .and_then(parse_f32_position);
    let nested_navigation = value.and_then(|value| value.get("navigation"));
    ControllerView {
        follow_enabled: value
            .and_then(|value| value.get("follow_enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        follow_target: value
            .and_then(|value| value.get("follow_target"))
            .and_then(Value::as_str)
            .map(str::to_string),
        move_active: value
            .and_then(|value| value.get("move_active"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        move_target,
        navigation: NavigationView {
            status: nested_navigation
                .and_then(|value| value.get("status"))
                .or_else(|| value.and_then(|value| value.get("navigation_status")))
                .and_then(Value::as_str)
                .unwrap_or("idle")
                .to_string(),
            current_waypoint: nested_navigation
                .and_then(|value| value.get("current_waypoint"))
                .and_then(parse_f32_position),
            waypoints_remaining: nested_navigation
                .and_then(|value| value.get("waypoints_remaining"))
                .or_else(|| nested_navigation.and_then(|value| value.get("remaining")))
                .or_else(|| value.and_then(|value| value.get("waypoints_remaining")))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(0),
            recovering: nested_navigation
                .and_then(|value| value.get("recovering"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            recovery_attempts: nested_navigation
                .and_then(|value| value.get("recovery_attempts"))
                .or_else(|| value.and_then(|value| value.get("recovery_attempts")))
                .and_then(Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
                .unwrap_or(0),
            stalled_for_seconds: nested_navigation
                .and_then(|value| value.get("stalled_for_seconds"))
                .and_then(Value::as_f64)
                .map(|value| value as f32)
                .unwrap_or(0.0),
            last_error: nested_navigation
                .and_then(|value| value.get("last_error"))
                .and_then(Value::as_str)
                .map(str::to_string),
            arrival_action: nested_navigation
                .and_then(|value| value.get("arrival_action"))
                .and_then(Value::as_str)
                .map(str::to_string),
        },
    }
}

fn parse_f32_position(value: &Value) -> Option<[f32; 3]> {
    let values = value.as_array()?;
    Some([
        values.first()?.as_f64()? as f32,
        values.get(1)?.as_f64()? as f32,
        values.get(2)?.as_f64()? as f32,
    ])
}

fn parse_position(value: Option<&Value>) -> Option<[i32; 3]> {
    let values = value?.as_array()?;
    Some([
        i32::try_from(values.first()?.as_i64()?).ok()?,
        i32::try_from(values.get(1)?.as_i64()?).ok()?,
        i32::try_from(values.get(2)?.as_i64()?).ok()?,
    ])
}

fn parse_inventory(value: Option<&Value>) -> InventoryView {
    let wield = value
        .and_then(|value| value.get("wield"))
        .and_then(parse_item_stack);
    let main = value
        .and_then(|value| value.get("main"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(parse_item_stack).collect())
        .unwrap_or_default();
    let main_truncated = value
        .and_then(|value| value.get("main_truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    InventoryView {
        wield,
        main,
        main_truncated,
    }
}

fn parse_item_stack(value: &Value) -> Option<ItemStackView> {
    let name = value.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    Some(ItemStackView {
        name: name.to_string(),
        count: value
            .get("count")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(1),
        wear: value
            .get("wear")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0),
        food_points: value
            .get("food_points")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
        food: value
            .get("food")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        food_group: value
            .get("food_group")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0),
        food_saturation: value
            .get("food_saturation")
            .and_then(Value::as_f64)
            .map(|value| value as f32),
    })
}

fn parse_food_drops(value: Option<&Value>) -> Vec<FoodDropView> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|drop| {
            if let Some(name) = drop.as_str() {
                let name = name.trim();
                return (!name.is_empty()).then(|| FoodDropView {
                    name: name.to_string(),
                    ..FoodDropView::default()
                });
            }
            let name = drop.get("name")?.as_str()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(FoodDropView {
                name: name.to_string(),
                chance: json_u32(drop, "chance").unwrap_or(1).max(1),
                min: json_u32(drop, "min").unwrap_or(1),
                max: json_u32(drop, "max").unwrap_or(1),
                food_points: drop
                    .get("food_points")
                    .and_then(Value::as_f64)
                    .map(|value| value as f32)
                    .unwrap_or(0.0),
                food_saturation: drop
                    .get("food_saturation")
                    .and_then(Value::as_f64)
                    .map(|value| value as f32),
            })
        })
        .collect()
}

fn parse_entities(value: Option<&Value>, kind_filter: Option<&str>) -> Vec<RelativeEntity> {
    let mut entities = Vec::new();
    let Some(values) = value.and_then(Value::as_array) else {
        return entities;
    };
    for value in values {
        let kind = value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if kind_filter.is_some_and(|filter| kind != filter) {
            continue;
        }
        entities.push(RelativeEntity {
            kind,
            name: value
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string),
            category: value
                .get("category")
                .and_then(Value::as_str)
                .map(str::to_string),
            count: value
                .get("count")
                .and_then(Value::as_u64)
                .and_then(|count| u32::try_from(count).ok()),
            hp: value
                .get("hp")
                .and_then(Value::as_i64)
                .and_then(|hp| i32::try_from(hp).ok()),
            distance: value
                .get("distance")
                .and_then(Value::as_f64)
                .map(|distance| distance as f32),
            food_source: value
                .get("food_source")
                .or_else(|| value.get("huntable_for_food"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            safe_to_hunt: value
                .get("safe_to_hunt")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            passive: value
                .get("passive")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            baby: value
                .get("baby")
                .or_else(|| value.get("is_baby"))
                .or_else(|| value.get("child"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            adult: value
                .get("adult")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            named: value
                .get("named")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            tamed: value
                .get("tamed")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            owned: value
                .get("owned")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            persistent: value
                .get("persistent")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            owner: value
                .get("owner")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|owner| !owner.is_empty())
                .map(str::to_string),
            food_drops: parse_food_drops(value.get("food_drops")),
            hunt_blocked_reason: value
                .get("hunt_blocked_reason")
                .and_then(Value::as_str)
                .map(str::to_string),
            dx: json_i32(value, "dx"),
            dy: json_i32(value, "dy"),
            dz: json_i32(value, "dz"),
        });
    }
    entities.sort_by_key(|entity| entity.dx.abs() + entity.dy.abs() + entity.dz.abs());
    entities.truncate(16);
    entities
}

fn parse_nodes(value: Option<&Value>) -> Vec<NodeView> {
    let Some(values) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(|value| {
            let name = value.get("name")?.as_str()?.trim();
            if name.is_empty() || matches!(name, "air" | "ignore") {
                return None;
            }
            Some(NodeView {
                name: name.to_string(),
                pos: parse_position(value.get("pos"))?,
                groups: value
                    .get("groups")
                    .and_then(Value::as_array)
                    .map(|groups| {
                        groups
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect()
}

fn prioritize_and_deduplicate_nodes(
    mut nodes: Vec<NodeView>,
    position: [i32; 3],
    limit: usize,
) -> Vec<NodeView> {
    // The world mod sends distance-sorted individual voxels. Deduplicate before
    // applying the prompt limit so a dirt floor cannot consume every slot and
    // hide a tree or ore that was present in the same observation.
    nodes.sort_by_key(|node| manhattan(position, node.pos));
    let mut seen = HashSet::new();
    nodes.retain(|node| seen.insert(node.name.to_ascii_lowercase()));
    nodes.sort_by_key(|node| {
        (
            node_resource_priority(node),
            manhattan(position, node.pos),
            node.name.to_ascii_lowercase(),
        )
    });
    nodes.truncate(limit);
    nodes
}

fn node_resource_priority(node: &NodeView) -> u8 {
    let has_group = |wanted: &str| {
        node.groups
            .iter()
            .any(|group| group.eq_ignore_ascii_case(wanted))
    };
    let name = node.name.to_ascii_lowercase();
    if has_group("tree") || has_group("ore") || name.contains("coal") || name.contains("_ore") {
        0
    } else if has_group("wood") || has_group("stone") {
        1
    } else if ["soil", "sand", "leaves", "flora", "plant"]
        .iter()
        .any(|group| has_group(group))
    {
        3
    } else {
        2
    }
}

fn parse_crafting(value: &Value) -> CraftingView {
    let crafting = value.get("crafting");
    let entries = crafting
        .and_then(|crafting| {
            crafting
                .get("craftable")
                .or_else(|| crafting.get("recipes"))
                .or_else(|| crafting.get("items"))
        })
        .or_else(|| crafting.filter(|crafting| crafting.is_array()))
        .or_else(|| value.get("craftable"))
        .and_then(Value::as_array);

    let mut craftable = entries
        .into_iter()
        .flatten()
        .filter_map(parse_craftable_item)
        .collect::<Vec<_>>();
    craftable.sort_by_key(|item| item.item.to_ascii_lowercase());
    craftable.dedup_by(|a, b| {
        if a.item.eq_ignore_ascii_case(&b.item) {
            a.output_per_batch = a.output_per_batch.max(b.output_per_batch);
            a.max_batches = a.max_batches.max(b.max_batches);
            a.table_required |= b.table_required;
            true
        } else {
            false
        }
    });
    craftable.truncate(64);
    CraftingView { craftable }
}

fn parse_craftable_item(value: &Value) -> Option<CraftableItemView> {
    if let Some(item) = value.as_str().map(str::trim).filter(|item| !item.is_empty()) {
        return Some(CraftableItemView {
            item: item.to_string(),
            output_per_batch: 1,
            max_batches: 0,
            table_required: false,
            ingredients: Vec::new(),
        });
    }

    let output = value.get("output").or_else(|| value.get("result"));
    let item = value
        .get("item")
        .or_else(|| value.get("name"))
        .and_then(Value::as_str)
        .or_else(|| {
            output.and_then(|output| {
                output
                    .get("name")
                    .or_else(|| output.get("item"))
                    .and_then(Value::as_str)
            })
        })?
        .trim();
    if item.is_empty() {
        return None;
    }
    let output_per_batch = value
        .get("output_per_batch")
        .or_else(|| output.and_then(|output| output.get("count")))
        .or_else(|| value.get("output_count"))
        .and_then(Value::as_u64)
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(1)
        .max(1);
    let max_batches = value
        .get("max_batches")
        .or_else(|| value.get("max_crafts"))
        .and_then(Value::as_u64)
        .and_then(|batches| u32::try_from(batches).ok())
        .unwrap_or(0);
    let ingredients = value
        .get("ingredients")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|ingredient| {
            let name = ingredient
                .get("name")
                .or_else(|| ingredient.get("item"))
                .and_then(Value::as_str)?
                .trim();
            (!name.is_empty()).then(|| CraftIngredientView {
                item: name.to_string(),
                count: ingredient
                    .get("count")
                    .and_then(Value::as_u64)
                    .and_then(|count| u32::try_from(count).ok())
                    .unwrap_or(1)
                    .max(1),
            })
        })
        .collect();
    Some(CraftableItemView {
        item: item.to_string(),
        output_per_batch,
        max_batches,
        table_required: value
            .get("table_required")
            .or_else(|| value.get("requires_table"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ingredients,
    })
}

fn parse_obstacles(value: Option<&Value>) -> ObstaclesView {
    let get = |key: &str| {
        value
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string()
    };
    ObstaclesView {
        front: get("front"),
        left: get("left"),
        right: get("right"),
        back: get("back"),
    }
}

fn json_i32(value: &Value, key: &str) -> i32 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(0)
}

fn json_u32(value: &Value, key: &str) -> Option<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

fn manhattan(a: [i32; 3], b: [i32; 3]) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs()
}

fn default_next_goal_id() -> u64 {
    1
}

fn default_one() -> u32 {
    1
}

fn default_one_f32() -> f32 {
    1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_keeps_inventory_health_and_entities() {
        let raw = r#"{
            "schema_version":7,"health":18,"hunger_available":true,"hunger":7,"saturation":2.5,"position":[1,2,3],"facing":"north",
            "inventory":{"wield":{"name":"pick","count":1,"wear":4},"main":[{"name":"bread","count":12,"food":true,"food_group":1,"food_points":5,"food_saturation":6}]},
            "players":[{"type":"player","name":"Sam","dx":1,"dy":0,"dz":2}],
            "items":[{"type":"item","name":"mcl_core:wood","count":2,"dx":2,"dy":0,"dz":0}],
            "hostiles":[{"type":"hostile","name":"mobs_mc:zombie","hp":20,"distance":3.2,"dx":3,"dy":0,"dz":1}],
            "mobs":[{"type":"mob","name":"mobs_mc:cow","category":"animal","hp":10,"distance":2.0,"passive":true,"food_source":true,"safe_to_hunt":true,"adult":true,"named":false,"food_drops":[{"name":"mcl_mobitems:beef","chance":1,"min":1,"max":3,"food_points":3}],"dx":2,"dy":0,"dz":0}],
            "nodes":[{"name":"stone","pos":[1,1,3],"groups":["stone"]}],
            "chests":[
                {"name":"mcl_chests:chest_small","kind":"chest","pos":[2,2,3],"distance":1.0,"accessible":true,"status":"accessible","contents":[{"name":"mcl_core:cobble","count":32}],"contents_truncated":false,"used_slots":1,"slots":27},
                {"name":"mcl_chests:chest_small","kind":"chest","pos":[3,2,3],"distance":2.0,"accessible":false,"status":"protected","contents":null,"contents_truncated":false}
            ],
            "voxel_map":{
                "radius":1,"origin":[0,0,0],"size":[1,1,2],"order":"y_x_z_z_fastest",
                "palette":[
                    {"name":"air","walkable":false,"diggable":false,"groups":[]},
                    {"name":"stone","walkable":true,"diggable":true,"groups":["stone"]}
                ],
                "runs":[[1,1],[2,1]],"complete":true
            },
            "obstacles":{"front":"stone","left":"air","right":"air","back":"air"},
            "controller":{"follow_enabled":true,"follow_target":"Sam","move_active":false,"navigation":{"status":"recovering","current_waypoint":[2,2,3],"waypoints_remaining":3,"recovering":true,"recovery_attempts":1,"stalled_for_seconds":1.75,"last_error":null,"arrival_action":"collect"}},
            "goal":""
        }"#;
        let observation = parse_observation(raw).unwrap();
        assert_eq!(observation.health, Some(18));
        assert!(observation.hunger_available);
        assert_eq!(observation.hunger, Some(7));
        assert_eq!(observation.saturation, Some(2.5));
        assert_eq!(observation.server_mod_schema_version, 7);
        assert_eq!(observation.inventory.main[0].count, 12);
        assert_eq!(observation.chests[0].pos, [2, 2, 3]);
        assert_eq!(observation.chests[0].contents[0].count, 32);
        assert_eq!(observation.chests.len(), 2);
        assert!(observation.chests[1].contents.is_empty());
        assert_eq!(observation.inventory.main[0].food_points, Some(5.0));
        assert!(observation.inventory.main[0].food);
        assert_eq!(observation.players[0].name.as_deref(), Some("Sam"));
        assert_eq!(observation.hostiles[0].kind, "hostile");
        assert_eq!(observation.hostiles[0].name.as_deref(), Some("mobs_mc:zombie"));
        assert_eq!(observation.hostiles[0].hp, Some(20));
        assert_eq!(observation.hostiles[0].distance, Some(3.2));
        assert_eq!(observation.mobs[0].name.as_deref(), Some("mobs_mc:cow"));
        assert_eq!(observation.mobs[0].category.as_deref(), Some("animal"));
        assert!(observation.mobs[0].food_source);
        assert!(observation.mobs[0].safe_to_hunt);
        assert!(observation.mobs[0].adult);
        assert_eq!(observation.mobs[0].food_drops[0].name, "mcl_mobitems:beef");
        assert_eq!(observation.mobs[0].food_drops[0].max, 3);
        assert_eq!(observation.nearby_items[0].name.as_deref(), Some("mcl_core:wood"));
        assert_eq!(observation.nearby_items[0].count, Some(2));
        assert_eq!(observation.nearby_nodes[0].groups, vec!["stone"]);
        assert!(observation.voxel_map.complete);
        assert_eq!(observation.voxel_map.runs, vec![[1, 1], [2, 1]]);
        assert!(observation.controller.follow_enabled);
        assert_eq!(observation.controller.follow_target.as_deref(), Some("Sam"));
        assert_eq!(observation.controller.navigation.status, "recovering");
        assert_eq!(observation.controller.navigation.current_waypoint, Some([2.0, 2.0, 3.0]));
        assert_eq!(observation.controller.navigation.waypoints_remaining, 3);
        assert_eq!(observation.controller.navigation.recovery_attempts, 1);
        assert_eq!(observation.controller.navigation.arrival_action.as_deref(), Some("collect"));
    }

    #[test]
    fn schema_eight_parses_furnace_and_crafting_capabilities() {
        let raw = r#"{
            "schema_version":8,"position":[0,10,0],"facing":"north",
            "inventory":{"main":[]},"players":[],"items":[],"hostiles":[],"mobs":[],
            "nodes":[],"chests":[],
            "furnaces":[{
                "name":"mcl_furnaces:furnace","kind":"furnace","pos":[2,10,0],
                "distance":2.0,"accessible":true,"status":"accessible","active":false,"speed":1,
                "activity":"idle","cookable":false,"output_blocked":false,"cook_progress":0,
                "fuel_remaining":0,"fuel_total":0,
                "input_options":[{"name":"mcl_mobitems:beef","count":3,"output":{"name":"mcl_mobitems:cooked_beef","count":1},"cook_time":10}],
                "fuel_options":[{"name":"mcl_core:coal_lump","count":2,"burn_time":80}]
            }],
            "crafting":{"craftable":[{
                "item":"mcl_torches:torch","output_per_batch":4,"max_batches":2,
                "table_required":false,"ingredients":[{"item":"mcl_core:stick","count":1}]
            }]},
            "obstacles":{}
        }"#;
        let observation = parse_observation(raw).unwrap();
        assert_eq!(observation.server_mod_schema_version, 8);
        assert_eq!(observation.furnaces.len(), 1);
        assert_eq!(observation.furnaces[0].input_options[0].count, 3);
        assert_eq!(observation.furnaces[0].fuel_options[0].burn_time, 80.0);
        assert_eq!(observation.crafting.craftable.len(), 1);
        let torch = &observation.crafting.craftable[0];
        assert_eq!(torch.item, "mcl_torches:torch");
        assert_eq!(torch.output_per_batch, 4);
        assert_eq!(torch.max_batches, 2);
        assert_eq!(torch.output_per_batch * torch.max_batches, 8);
        assert_eq!(torch.ingredients[0].item, "mcl_core:stick");
    }

    #[test]
    fn resource_priority_prevents_common_soil_from_crowding_out_tree_and_ore() {
        let mut nodes = (0..60)
            .map(|index| NodeView {
                name: format!("test:dirt_{index}"),
                pos: [index, 0, 0],
                groups: vec!["soil".to_string()],
            })
            .collect::<Vec<_>>();
        nodes.push(NodeView {
            name: "mcl_core:tree".to_string(),
            pos: [100, 0, 0],
            groups: vec!["tree".to_string()],
        });
        nodes.push(NodeView {
            name: "mcl_core:stone_with_coal".to_string(),
            pos: [101, 0, 0],
            groups: vec!["ore".to_string()],
        });
        let prioritized = prioritize_and_deduplicate_nodes(nodes, [0, 0, 0], 40);
        assert_eq!(prioritized[0].name, "mcl_core:tree");
        assert_eq!(prioritized[1].name, "mcl_core:stone_with_coal");
        assert_eq!(prioritized.len(), 40);
    }

    #[test]
    fn replacing_goal_archives_the_previous_goal() {
        let mut state = AgentState::default();
        state.set_goal("first", None).unwrap();
        state.set_goal("second", Some("done")).unwrap();
        assert_eq!(state.current_goal.as_ref().unwrap().description, "second");
        assert_eq!(state.goal_history[0].status, GoalStatus::Cancelled);
    }

    #[test]
    fn autonomous_objective_is_bounded_and_does_not_replace_the_mission() {
        let mut state = AgentState::default();
        state.set_goal("Help Sam survive", None).unwrap();
        state
            .set_objective("Gather nearby coal", Some("Collect one coal"), 2)
            .unwrap();
        state.record_tool_result("navigate_node", &serde_json::json!({"node":"coal"}), true, "OK");
        state.record_tool_result("gather_resource", &serde_json::json!({"node":"coal"}), true, "OK");
        assert!(state.objective_budget_exhausted());
        state
            .finish_objective(GoalStatus::Completed, Some("collected coal"))
            .unwrap();
        assert_eq!(
            state.current_goal.as_ref().map(|goal| goal.description.as_str()),
            Some("Help Sam survive")
        );
        assert!(state.current_objective.is_none());
        assert_eq!(state.objective_history.len(), 1);
        assert_eq!(state.prompt_view()["recent_objectives"][0]["status"], "completed");
    }

    #[test]
    fn emergency_stop_and_defend_do_not_consume_objective_actions() {
        let mut state = AgentState::default();
        state.set_objective("Gather a log", None, 2).unwrap();
        state.record_tool_result("stop", &serde_json::json!({}), true, "OK");
        state.record_tool_result("defend", &serde_json::json!({}), true, "OK");
        assert_eq!(state.current_objective.as_ref().unwrap().actions_used, 0);
        assert!(!state.objective_budget_exhausted());
    }

    #[test]
    fn repeated_failed_calls_are_suppressed_only_at_the_same_position() {
        let mut state = AgentState::default();
        let args = serde_json::json!({"x":null,"y":null,"z":null});
        state.record_tool_result("place", &args, false, "no_space");
        state.record_tool_result("place", &args, false, "no_space");
        state.record_tool_result("say", &serde_json::json!({"message":"recovering"}), true, "OK");
        assert!(state
            .repeated_failure_reason("place", &args)
            .is_some_and(|reason| reason.contains("suppressed_repeated_failure")));
        assert_eq!(state.consecutive_failures, 2);

        state.observation.position = [1, 0, 0];
        assert!(state.repeated_failure_reason("place", &args).is_none());
    }

    #[test]
    fn action_fingerprints_canonicalize_argument_key_order() {
        assert_eq!(
            action_fingerprint("place", &serde_json::json!({"z":3,"x":1,"y":2}), [0, 0, 0]),
            action_fingerprint("place", &serde_json::json!({"x":1,"y":2,"z":3}), [0, 0, 0])
        );
    }

    #[test]
    fn transient_transport_failures_are_not_permanently_suppressed() {
        let mut state = AgentState::default();
        let args = serde_json::json!({"node":"mcl_core:stone","radius":8});
        state.record_tool_result("navigate_node", &args, false, "connection refused");
        state.record_tool_result("navigate_node", &args, false, "504 Gateway Timeout");
        assert!(state
            .repeated_failure_reason("navigate_node", &args)
            .is_none());
    }

    #[test]
    fn older_persisted_state_defaults_new_objective_fields() {
        let mut value = serde_json::to_value(AgentState::default()).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("current_objective");
        object.remove("objective_history");
        object.remove("next_objective_id");
        let state: AgentState = serde_json::from_value(value).unwrap();
        assert!(state.current_objective.is_none());
        assert!(state.objective_history.is_empty());
    }

    #[test]
    fn pending_chat_is_deduplicated_and_cleared_after_acknowledgement() {
        let mut state = AgentState::default();
        let message = PendingChatMessage {
            id: 4,
            from: "Sam".to_string(),
            message: "follow me".to_string(),
        };
        state.enqueue_chat([message.clone(), message]);
        assert_eq!(state.pending_chat.len(), 1);
        assert_eq!(state.prompt_view()["pending_chat"][0]["from"], "Sam");
        assert_eq!(state.conversation_history.len(), 1);
        state.clear_pending_chat();
        assert!(state.pending_chat.is_empty());
        assert_eq!(state.conversation_history[0].message, "follow me");
    }


    #[test]
    fn conversation_and_action_history_are_available_to_the_model() {
        let mut state = AgentState::default();
        state.record_bot_chat("Bot", "I am following Sam.", "assistant");
        state.record_action_result("follow", true, "OK");
        let view = state.prompt_view();
        assert_eq!(view["conversation_history"][0]["sender"], "Bot");
        assert_eq!(view["action_history"][0]["action"], "follow");
    }

    #[test]
    fn consecutive_unsolicited_bot_messages_are_compacted() {
        let mut state = AgentState::default();
        state.record_bot_chat("Bot", "first explanation", "assistant");
        state.record_bot_chat("Bot", "second explanation", "assistant");
        assert_eq!(state.conversation_history.len(), 1);
        assert_eq!(state.conversation_history[0].message, "second explanation");
    }
}
