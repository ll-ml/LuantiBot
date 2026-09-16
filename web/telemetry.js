const refreshAgentBtn = document.getElementById("refreshAgentBtn");
const agentStatusEl = document.getElementById("agentStatus");
const agentUpdatedEl = document.getElementById("agentUpdated");
const agentPhaseEl = document.getElementById("agentPhase");
const agentModelEl = document.getElementById("agentModel");
const agentTokensEl = document.getElementById("agentTokens");
const agentPositionEl = document.getElementById("agentPosition");
const agentReasonEl = document.getElementById("agentReason");
const agentMissionEl = document.getElementById("agentMission");
const agentObjectiveEl = document.getElementById("agentObjective");
const agentDecisionEl = document.getElementById("agentDecision");
const agentWorldEl = document.getElementById("agentWorld");
const agentToolsEl = document.getElementById("agentTools");

const numberFormatter = new Intl.NumberFormat();
const pollIntervalMs = 2000;
let baseUrlInput;
let authTokenInput;
let requestInFlight = false;
let lastRevision = null;

function buildHeaders() {
  const headers = new Headers();
  const token = authTokenInput.value.trim();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  return headers;
}

function titleCase(value) {
  return String(value || "unknown")
    .replace(/_/g, " ")
    .replace(/\b\w/g, (character) => character.toUpperCase());
}

function formatAge(ageMs) {
  if (!Number.isFinite(ageMs)) return "unknown age";
  if (ageMs < 1000) return "just now";
  if (ageMs < 60000) return `${Math.round(ageMs / 1000)}s ago`;
  return `${Math.round(ageMs / 60000)}m ago`;
}

function formatDuration(durationMs) {
  if (!Number.isFinite(durationMs)) return null;
  if (durationMs < 1000) return `${durationMs}ms`;
  return `${(durationMs / 1000).toFixed(durationMs < 10000 ? 1 : 0)}s`;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes)) return null;
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KB`;
}

function formatPosition(position) {
  if (!Array.isArray(position) || position.length < 3) return "—";
  return position.slice(0, 3).join(", ");
}

function setStatus(label, stateClass) {
  agentStatusEl.textContent = label;
  agentStatusEl.className = `telemetry-status ${stateClass}`;
}

function formatGoal(goal, emptyMessage, includeBudget = false) {
  if (!goal || !goal.description) return emptyMessage;
  const parts = [goal.description];
  if (goal.success_criteria) {
    parts.push(`Success: ${goal.success_criteria}`);
  }
  if (includeBudget && Number.isFinite(goal.action_budget)) {
    parts.push(`${goal.actions_used || 0}/${goal.action_budget} actions used`);
  }
  return parts.join(" · ");
}

function renderDecision(decision) {
  agentDecisionEl.replaceChildren();
  if (!decision) {
    agentDecisionEl.textContent = "No decision reported.";
    return;
  }

  const summary = document.createElement("p");
  summary.className = "activity-copy";
  const parts = [titleCase(decision.status)];
  const latency = formatDuration(decision.request_latency_ms);
  if (latency) parts.push(`model ${latency}`);
  if (Number.isFinite(decision.offered_tools)) {
    parts.push(`${decision.offered_tools} tools offered`);
  }
  if (Number.isFinite(decision.returned_tool_calls)) {
    parts.push(`${decision.returned_tool_calls} returned`);
  }
  const selectedTools = Array.isArray(decision.selected_tools)
    ? decision.selected_tools
    : [];
  parts.push(`${selectedTools.length} selected`);
  const contextSize = formatBytes(decision.context_bytes);
  if (contextSize) parts.push(`${contextSize} context`);
  summary.textContent = parts.join(" · ");
  agentDecisionEl.append(summary);

  if (decision.active_tool) {
    const active = document.createElement("p");
    active.className = "decision-active";
    active.textContent =
      `Executing ${decision.active_tool.name} ` +
      `(${decision.active_tool.index}/${decision.active_tool.total})`;
    agentDecisionEl.append(active);
  }

  if (decision.usage) {
    const usage = document.createElement("p");
    usage.className = "telemetry-detail";
    usage.textContent =
      `This request: ${numberFormatter.format(decision.usage.input || 0)} input ` +
      `(${numberFormatter.format(decision.usage.cached_input || 0)} cached), ` +
      `${numberFormatter.format(decision.usage.output || 0)} output tokens.`;
    agentDecisionEl.append(usage);
  }

  if (decision.last_execution) {
    const execution = document.createElement("p");
    execution.className = decision.last_execution.ok
      ? "telemetry-detail is-success"
      : "telemetry-detail is-error";
    const toolLatency = formatDuration(decision.last_execution.latency_ms);
    const outcome = decision.last_execution.ok ? "succeeded" : "failed";
    execution.textContent =
      `${decision.last_execution.name} ${outcome}` +
      `${toolLatency ? ` in ${toolLatency}` : ""}: ${decision.last_execution.result}`;
    agentDecisionEl.append(execution);
  }

  if (decision.incomplete_reason || decision.error) {
    const warning = document.createElement("p");
    warning.className = "telemetry-detail is-error";
    warning.textContent = decision.error || `Incomplete: ${decision.incomplete_reason}`;
    agentDecisionEl.append(warning);
  }

  if (selectedTools.length > 0) {
    const details = document.createElement("details");
    details.className = "decision-tools";
    const heading = document.createElement("summary");
    heading.textContent = `Selected calls: ${selectedTools.map((tool) => tool.name).join(", ")}`;
    details.append(heading);
    selectedTools.forEach((tool) => {
      const call = document.createElement("div");
      call.className = "decision-call";
      const name = document.createElement("strong");
      name.textContent = tool.name || "unknown tool";
      const argumentsEl = document.createElement("pre");
      argumentsEl.textContent = JSON.stringify(tool.arguments ?? {}, null, 2);
      call.append(name, argumentsEl);
      details.append(call);
    });
    agentDecisionEl.append(details);
  }
}

function renderRecentTools(tools) {
  agentToolsEl.replaceChildren();
  if (!Array.isArray(tools) || tools.length === 0) {
    const empty = document.createElement("p");
    empty.className = "empty-state";
    empty.textContent = "No tool calls reported.";
    agentToolsEl.append(empty);
    return;
  }

  tools.forEach((tool) => {
    const entry = document.createElement("article");
    entry.className = `tool-entry ${tool.ok ? "is-success" : "is-error"}`;

    const header = document.createElement("div");
    header.className = "tool-entry-header";
    const name = document.createElement("strong");
    name.textContent = tool.name || "unknown tool";
    const outcome = document.createElement("span");
    outcome.className = "tool-outcome";
    outcome.textContent = tool.ok ? "Success" : "Failed";
    header.append(name, outcome);

    const meta = document.createElement("p");
    meta.className = "telemetry-detail";
    meta.textContent = `Tick ${tool.tick ?? "?"} · position ${formatPosition(tool.position)}`;

    const result = document.createElement("p");
    result.className = "tool-result";
    result.textContent = tool.result || "No result text.";

    const details = document.createElement("details");
    const detailsSummary = document.createElement("summary");
    detailsSummary.textContent = "Arguments";
    const argumentsEl = document.createElement("pre");
    argumentsEl.textContent = JSON.stringify(tool.arguments ?? {}, null, 2);
    details.append(detailsSummary, argumentsEl);

    entry.append(header, meta, result, details);
    agentToolsEl.append(entry);
  });
}

function renderWorld(world) {
  if (!world) {
    agentWorldEl.textContent = "No observation reported.";
    return;
  }
  const parts = [];
  if (world.health != null) parts.push(`HP ${world.health}`);
  if (world.hunger_available && world.hunger != null) parts.push(`hunger ${world.hunger}`);
  if (world.facing) parts.push(`facing ${world.facing}`);
  parts.push(`${world.inventory_stacks || 0} inventory stacks`);
  parts.push(`${world.hostiles || 0} hostile`);
  parts.push(`${world.mobs || 0} passive/neutral`);
  parts.push(`${world.nearby_items || 0} dropped stacks`);
  if (Array.isArray(world.visible_players) && world.visible_players.length > 0) {
    parts.push(`players: ${world.visible_players.join(", ")}`);
  }
  if (world.follow_enabled) {
    parts.push(`following ${world.follow_target || "target"}`);
  }
  if (world.navigation?.status) {
    let navigation = `navigation ${world.navigation.status}`;
    if (world.navigation.waypoints_remaining) {
      navigation += ` (${world.navigation.waypoints_remaining} waypoints)`;
    }
    if (world.navigation.recovering) navigation += " recovering";
    parts.push(navigation);
  }
  agentWorldEl.textContent = parts.join(" · ");
}

function renderAgent(envelope) {
  const agent = envelope.agent;
  if (!agent) {
    agentPhaseEl.textContent = "—";
    agentModelEl.textContent = "—";
    agentTokensEl.textContent = "—";
    agentPositionEl.textContent = "—";
    agentReasonEl.textContent = "Start the agent process to see its decisions and tool calls.";
    agentMissionEl.textContent = "No mission reported.";
    agentObjectiveEl.textContent = "No objective reported.";
    renderDecision(null);
    renderWorld(null);
    renderRecentTools([]);
    return;
  }

  const visiblePhase = agent.decision?.status === "executing" ? "acting" : agent.phase;
  agentPhaseEl.textContent = titleCase(visiblePhase);
  agentModelEl.textContent = agent.model || "unknown";
  agentTokensEl.textContent = numberFormatter.format(agent.usage?.total_tokens || 0);
  agentPositionEl.textContent = formatPosition(agent.world?.position);

  const mode = agent.mode?.passive
    ? "passive"
    : agent.mode?.autonomous
      ? "autonomous"
      : "goal-directed";
  const stateParts = [
    agent.phase_reason || "No phase detail",
    `${agent.bot_name || "Bot"} is ${mode}`
  ];
  if (agent.last_action) {
    stateParts.push(`last action: ${agent.last_action}`);
  }
  if (agent.consecutive_failures) {
    stateParts.push(`${agent.consecutive_failures} consecutive failures`);
  }
  agentReasonEl.textContent = stateParts.join(" · ");
  agentMissionEl.textContent = formatGoal(agent.mission, "No active mission.");
  agentObjectiveEl.textContent = formatGoal(
    agent.objective,
    "No active autonomous objective.",
    true
  );
  renderDecision(agent.decision);
  renderWorld(agent.world);
  renderRecentTools(agent.recent_tools);
}

function updatePresence(envelope) {
  if (!envelope.agent) {
    setStatus("Not started", "is-offline");
    agentUpdatedEl.textContent = "The bot API is reachable, but no agent has published telemetry.";
    return;
  }
  if (envelope.online) {
    setStatus("Live", "is-online");
  } else {
    setStatus("Stale", "is-stale");
  }
  agentUpdatedEl.textContent =
    `Heartbeat ${formatAge(envelope.age_ms)} · ` +
    `revision ${envelope.revision ?? 0}`;
}

async function poll(force = false) {
  if (document.hidden || requestInFlight) return;
  requestInFlight = true;
  const base = baseUrlInput.value.trim().replace(/\/$/, "");
  const url = `${base}/agent/telemetry`;
  try {
    const response = await fetch(url, {
      method: "GET",
      headers: buildHeaders(),
      cache: "no-store"
    });
    const text = await response.text();
    if (!response.ok) {
      throw new Error(`${response.status} ${response.statusText}${text ? `: ${text}` : ""}`);
    }
    const envelope = JSON.parse(text);
    updatePresence(envelope);
    if (force || envelope.revision !== lastRevision) {
      renderAgent(envelope);
      lastRevision = envelope.revision;
    }
  } catch (error) {
    const unauthorized = String(error.message).startsWith("401 ");
    setStatus(unauthorized ? "Unauthorized" : "Unavailable", "is-error");
    agentUpdatedEl.textContent = unauthorized
      ? "The telemetry request needs the same API token as the bot."
      : `Telemetry request failed: ${error.message}`;
  } finally {
    requestInFlight = false;
  }
}

function reset() {
  lastRevision = null;
  poll(true);
}

export function startAgentTelemetry(connection) {
  baseUrlInput = connection.baseUrlInput;
  authTokenInput = connection.authTokenInput;

  refreshAgentBtn.addEventListener("click", () => poll(true));
  baseUrlInput.addEventListener("change", reset);
  authTokenInput.addEventListener("change", reset);
  document.addEventListener("visibilitychange", () => {
    if (!document.hidden) poll(true);
  });

  poll(true);
  window.setInterval(() => poll(false), pollIntervalMs);
}
