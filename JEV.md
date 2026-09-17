# Jev Controller

The Jev controller is the new bounded decision path for this branch. It sends a compact semantic
world state and a closed set of fully parameterized action candidates to TypeSafe System One. Jev
returns a candidate ID, probability distribution, and confidence; ordinary Rust code applies the
confidence and safety policy and then passes the selected action through the existing native
validator and executor.

Jev never supplies an endpoint name, coordinate, item name, or arbitrary JSON argument.

## Start the bot

Run the game client and local API in one terminal:

```bash
export BOT_API_TOKEN='your-local-bot-token'

cargo run --release -- join 127.0.0.1:30000 Bot \
  --api-addr 127.0.0.1:9123 \
  --api-token "$BOT_API_TOKEN"
```

Keep that process running. In another terminal, export the TypeSafe credential:

```bash
export TYPESAFE_API_KEY='your-typesafe-api-key'
```

Start with dry-run mode. This performs real Jev evaluations and publishes the choices and
probabilities to the Web UI, but it does not send any selected action to the bot. Each pending chat
message is shadow-evaluated once and then removed from the action queue to avoid repeated paid
requests; its text remains in the persisted conversation history:

```bash
cargo run --release -- agent \
  --policy jev \
  --api http://127.0.0.1:9123 \
  --api-token "$BOT_API_TOKEN" \
  --jev-model jev-latest \
  --bot-name Bot \
  --allow YourPlayerName \
  --autonomous \
  --goal 'Follow YourPlayerName and establish basic supplies' \
  --state-file ./bot-agent-state-jev.json
```

After inspecting the dry-run decisions, add `--jev-execute` to permit actions:

```bash
cargo run --release -- agent \
  --policy jev \
  --jev-execute \
  --api http://127.0.0.1:9123 \
  --api-token "$BOT_API_TOKEN" \
  --bot-name Bot \
  --allow YourPlayerName \
  --autonomous \
  --state-file ./bot-agent-state-jev-live.json
```

Use a separate state file while evaluating Jev. Never run the LLM and Jev policies against the
same bot simultaneously.

For multi-step work, set a persistent mission with `--goal` at startup or send
`!goal <description>` in game. Use `!goal clear` or `!cancel goal` to stop it. Ordinary chat remains
a bounded instruction rather than silently becoming a permanent mission.

## Safety boundary

The controller always offers `wait`, which means issue no new command and does not cancel an
existing path or follow controller. Candidate generation is bounded to 48 entries and filters
actions using the current server-mod schema and observation. Common terrain such as dirt and stone
is not offered for autonomous gathering without an explicit player mission.

Routine actions require `--jev-min-confidence` (default `0.65`). Material actions such as mining,
crafting, combat, and hunting require at least `0.70`, even if the configured threshold is lower.
An unknown or low-confidence selection becomes `wait`. A hostile within four nodes is handled by a
deterministic defensive override instead of spending a remote model round trip.

After a remote decision returns, material actions are matched against a newly fetched observation;
if their grounded candidate disappeared or changed, the controller falls back to `wait`. Immediately
before dispatch, the existing executor also rechecks death state, distance, observed targets,
inventories, animal safety flags, server capabilities, objective budgets, and repeated failures.
Server-side validation remains authoritative.

The TypeSafe key is used only in the outbound Authorization header. It is redacted from debug
output and is never included in state files or telemetry.

## State sent to Jev

Jev receives the active mission/objective, pending authorized chat, six recent non-pending
conversation entries, health and hunger, controller status, bounded entity/resource/inventory
summaries, recent action outcomes, and failure count. It does not receive the voxel cube, raw
packets, complete inventory/container contents, full history, or provider credentials. Navigation
and geometry remain native.

Current candidates cover waiting, stopping, following, defense, useful resource gathering, dropped
item collection, safe food hunting, basic crafting, furnace loading/output collection, eating, and
sleeping. Chest policy and broader interaction candidates will be migrated after the first live
decision traces are reviewed.

The legacy LLM path remains available temporarily with the default `--policy llm` for comparison
and rollback. It will be removed from this branch only after the Jev controller has demonstrated
live parity.
