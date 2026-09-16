# LLM Agent

The agent is a separate process that observes and controls a running `join` bot through its REST API. It supports OpenAI's Responses API, OpenAI-compatible Chat Completions servers, bearer-token authentication, native function tools, persistent goals/state, and token accounting.

## 1. Start the game bot

The `llm_bot` world mod must be loaded because combat, entity interaction, inventory, sleep, mine, place, wield, drop, and use rely on its `/bot_*` commands.
After updating this repository, copy both files from `scripts/` into the installed `llm_bot` world mod and restart the Luanti server; schema 8 observations, native mining, container/furnace storage, crafting, path planning, and the server-backed tools require the updated `init.lua`.

```bash
cargo run --release -- join 127.0.0.1:30000 Bot \
  --api-addr 127.0.0.1:9123 \
  --api-token change-this-token
```

Keep this process running.

## 2. OpenAI Responses API

Keep the provider key in the environment so it does not appear in shell history or process arguments:

```bash
export OPENAI_API_KEY='your-api-key'

cargo run --release -- agent \
  --api http://127.0.0.1:9123 \
  --api-token change-this-token \
  --llm-url https://api.openai.com/v1/responses \
  --llm-api responses \
  --model gpt-5.6-sol \
  --reasoning-effort low \
  --max-tokens 768 \
  --bot-name Bot \
  --allow YourPlayerName \
  --autonomous \
  --goal 'Follow YourPlayerName and help gather wood' \
  --state-file ./bot-agent-state.json
```

The model is configurable; the agent does not depend on a hard-coded OpenAI model name. `--llm-api auto` detects the Responses API when the URL ends in `/responses`.

## 3. Local or hosted OpenAI-compatible server

For a server exposing Chat Completions:

```bash
cargo run --release -- agent \
  --api 127.0.0.1:9123 \
  --api-token change-this-token \
  --llm-url http://127.0.0.1:8080/v1/chat/completions \
  --llm-api chat-completions \
  --llm-api-key optional-provider-token \
  --model your-model-name \
  --reasoning-effort none \
  --bot-name Bot \
  --state-file ./bot-agent-state.json
```

Native function calling is attempted first. If a Chat Completions server rejects tool calling, the agent retries with common compatibility fields and finally falls back to one JSON action per decision.

## State machine and goals

The persisted state moves through these phases:

```text
booting -> observing -> planning -> acting -> waiting
                         |             |
                         +-> idle      +-> recovering (on failure)
```

`bot-agent-state.json` records:

- the durable mission, bounded autonomous objective, and their histories;
- health, position, facing, nearby nodes/entities, and obstacles;
- a palette/RLE-compressed voxel cube containing every cell in the requested observation radius;
- wielded item and main inventory;
- nearby supported chests/barrels and furnaces, their access status, contents, and cooking progress;
- registered basic outputs currently craftable from the observed inventory;
- last action/result and failure count;
- the last 60 player/bot conversation entries and last 40 executed game actions;
- cumulative provider request, input-token, cached-input-token, output-token, and total-token usage;
- recent state transitions, the chat cursor, and player messages waiting to be acknowledged.

The state file remains full fidelity, but each model request uses a smaller decision view. It sends
one copy of the mission, the last 12 non-pending conversation entries, the last eight external
actions, and four recent objectives; persistence-only IDs/timestamps, empty/default observation
fields, repeated pending messages, and long result tails are omitted. Tool schemas are also limited
to actions that the current observation can actually support and automatically reappear when they
become relevant. This changes neither the saved history nor server-side validation.

Decision logs include the number of exposed tools, serialized context bytes, provider input/output
tokens, and cached input tokens. Official OpenAI Responses requests use a stable prompt cache key
for each model/instruction/tool profile; other compatible providers receive no OpenAI-specific
cache field. Cached tokens are included in the provider's input-token total, so the separate value
shows how much of that input received cache treatment.

## Live Web UI telemetry

While the agent runs, it automatically publishes a compact status snapshot to the bot API. Open
the controller with `cd web && yarn dev` and its **LLM Agent Activity** panel will show the current
phase and reason, mission/objective, model latency and token usage, selected tool names and
arguments, execution results and timings, recent tool history, and a small world/controller
summary. The panel pauses polling in a hidden browser tab and avoids rebuilding unchanged data.

This telemetry is generated from state and provider accounting the agent already has. It is sent
to `/agent/telemetry` by a best-effort background worker and is never added to the prompt, so it
uses zero additional LLM tokens. Network I/O cannot block the decision/action loop; the only
in-loop work is constructing a small bounded JSON snapshot. If the API is unavailable, the
publisher drops or replaces pending telemetry while normal agent operation continues. The
snapshot excludes the provider API key, full prompt, voxel map, inventory contents, and hidden
model reasoning.

Players can assign or cancel goals in game:

```text
!goal collect ten wood blocks and return to me
!goal clear
!cancel
```

Bounded autonomy is opt-in with `--autonomous`. The configured/player goal is treated as a durable mission, while the model may create one short, observation-grounded objective without replacing that mission. The controller supplies ranked progression recommendations from actual inventory, resources, registered recipes, and furnace state—for example logs → planks → sticks → coal → torches—rather than relying on the model to invent recipe names. Each objective has a small action budget and must be completed, failed, or cancelled before another is chosen; recent objective history is retained to discourage repetitive busywork. Autonomous soil, dirt, sand, leaves, flora, and plant collection is rejected unless the configured/player mission or current player message explicitly requests it. Player instructions and immediate survival take priority, and `stop`/`defend` remain available even when an objective exhausts its budget.

Food hunting is a separate constrained action rather than general passive-mob combat. The model can hunt one animal only when schema 5 marks it as a food source, and the world mod rejects babies, named animals, and owned or tamed animals. Hunger, saturation, edible inventory values, and possible food drops are included in observation so the model can decide whether hunting is warranted.

Without `--autonomous`, the model can create goals only from player instructions or `--goal`.

Use `--allow Alice,Bob` to restrict whose chat the agent can process. Leaving it empty allows all players.
Player chat is retained in the state until the model successfully acknowledges it with `say`. It then remains in bounded conversation history alongside bot replies and status messages, so later follow-up messages retain context. If the model selects an action without replying, the controller sends one fallback acknowledgement instead of silently ignoring the player.

The `say` tool is only exposed while new player chat is awaiting acknowledgement. Once the reply succeeds, the next decision exposes action tools without `say`, preventing the model from repeatedly talking instead of carrying out the active goal.

## Available model tools

Across observations, active mode can expose:

- `move`, `move_to`, `navigate_node`, `follow`, `stop`, and `teleport`;
- `attack`, `defend`, `approach`, `interact`, `fight`, and the constrained `hunt_food` action;
- `sleep`, `mine`, `collect_blocks`, `gather_resource`, `collect_item`, and `place`;
- `deposit_item` and `withdraw_item` for nearby observed chests and barrels;
- `load_furnace` and `collect_furnace_output` for observed normal/blast/smoker furnaces;
- `craft_item` for a registered basic output listed as currently craftable;
- `wield`, `use_item`, and `drop_item`;
- `say`, `set_goal`, `finish_goal`, `set_objective`, and `finish_objective`.

Tool arguments are validated against current observation. For example, the model cannot wield an unobserved item, target an unobserved player, or request distant mining coordinates. Only one external game action is executed per model decision; `say` and goal bookkeeping can run before it.

The version 4 observation reports hostile mobs and passive/neutral mobs as separate nearest-first lists with exact entity names, health, distance, and relative offsets. Passive/neutral entries also include their game-defined category, such as `animal` or `npc`, and can be used by `approach` and `interact` but are rejected by combat tools. A newly observed hostile triggers a decision immediately instead of waiting for the idle polling interval. The `defend` tool attacks only the nearest entity that the world mod classifies as hostile and schedules a short combat combo, avoiding one full LLM round trip per punch. The model re-observes afterward and keeps defending while a threat remains.

`follow` is intentionally different from combat tools: it accepts a valid player name from the goal, authorized-player list, or conversation even when that player is outside the short observation radius. The observation's `controller` object reports active follow and point-movement state so the model does not continually resend an accepted command.

`collect_blocks` mines a bounded batch of up to eight blocks already within reach. Mining uses the native player protocol one node at a time: the helper selects a harvest-capable tool and its real dig time, while the Rust client sends start/complete interactions so the server remains responsible for drops, wear, protection, and callbacks. `gather_resource` asks the world mod to locate a matching observed resource, find a standable adjacent position, and plan a path; the Rust client then follows those waypoints and starts the same native mining state machine on arrival. `navigate_node` uses the planner without mining. `collect_item` walks to a dropped stack from `nearby_items` so normal pickup behavior can collect it.

Schema 8 keeps schema 7 chest support and also recognizes VoxeLibre barrels in `observation.chests`. `deposit_item` and `withdraw_item` require an accessible observed container and exact observed item/count. Transfers use Luanti's native inventory action and confirm the exact stack count reported by the server callback, preserving reach and visibility checks, protection, game callbacks, shulker restrictions, logging, and rollback. Ender chests are intentionally excluded until their player-specific storage has a separate adapter. The model is instructed not to move valuables autonomously without a relevant mission, objective, or player request.

Schema 8 also reports normal furnaces, blast furnaces, and smokers in `observation.furnaces`. `load_furnace` accepts only an input and fuel listed for the same accessible furnace; specialized furnace groups and registered cooking/fuel results are checked again by the world helper. The input and fuel moves are separately confirmed and a partial two-leg result is never reported as a full success. `collect_furnace_output` uses the native inventory path so output callbacks, XP, achievements, protection, logging, and rollback continue to run.

`observation.crafting.craftable` contains only allowlisted useful basics that can be resolved from registered recipes and the current inventory. `craft_item` requests a minimum output count. Lua validates and concretizes the recipe, while Rust sends native exact grid moves, native `Craft`, and a native result move. This supports planks, sticks, torches, crafting tables, furnaces, and basic wooden/stone tools; 3x3 recipes require an accessible nearby crafting table. Dirt/decorative recipes and arbitrary model-provided grids are not exposed.

Waypoint movement has a progress watchdog. When the bot stops making progress toward its current waypoint, it suppresses continuous jumping and tries bounded side/reverse recovery maneuvers. After three failed recoveries it stops the navigation task and reports a failed controller state instead of jumping forever. Repeating the same failed tool call with identical arguments at the same position is also suppressed after two attempts, forcing the next decision to choose a different target or recovery action.

Observation schema version 2 provides the full voxel cube and dropped-item collection, version 3 adds hostile awareness, version 4 adds passive/neutral mob awareness, version 5 adds path planning plus food and hunger metadata, version 6 adds native-mining preparation and verification, version 7 adds chest observation and native inventory transfers, and version 8 adds barrels, furnaces, registered basic crafting, and resource prioritization. When the running world mod is older, the agent reports the detected version and removes unsupported tools from model requests. If an older server nevertheless returns `Invalid command`, the pending REST call is failed immediately and that tool is disabled for the rest of the agent process rather than retried indefinitely.

Successful actions are summarized in game chat by default. Identical messages are suppressed and other narration is limited by `--narration-cooldown-secs 10`. Use `--quiet-actions` to disable automatic narration. Direct model replies are also counted against the cooldown, avoiding a reply followed immediately by a status message.

With `--passive`, only chat and goal-management tools are exposed.

## Useful options

- `--interval-ms 2000`: minimum loop period while working on a goal.
- `--idle-interval-ms 10000`: model-call interval without a goal or new chat.
- `--autonomous`: allow small, bounded self-directed goals while otherwise idle.
- `--radius 4`: server observation radius, clamped to 1–8.
- `--max-tokens 768`: provider output-token ceiling. Reasoning models count internal reasoning against this limit; repeated decisions with exactly the limit and no tool call usually mean it should be raised to 1024.
- `--max-tool-calls 3`: maximum calls accepted from one decision; only one may affect the game.
- `--narration-cooldown-secs 10`: minimum gap between concise action messages.
- `--quiet-actions`: disable automatic action narration while retaining direct model chat replies.
- `--state-file PATH`: enables restart-safe state and token accounting.

Run `cargo run -- agent --help` for the complete option list.
