# Luanti Bot REST API (LLM Control Guide)

Base URL example: `http://127.0.0.1:9123`

If an API token is configured, include:

```
Authorization: Bearer <TOKEN>
```

For the built-in tool-calling agent, provider setup, goals, and persistent state, see [LLM.md](LLM.md).

## Control Loop (Legacy/custom clients)

1. `GET /observe_server` to fetch state.
2. Build a short prompt with observation + last action.
3. LLM returns exactly one JSON action.
4. Controller validates action (target exists, range ok, etc.).
5. Execute via REST endpoint.
6. Repeat.

The built-in agent uses native function tools when supported. A single JSON object remains supported as a compatibility fallback and for custom controllers.

## Observation

### `GET /observe_server?radius=<1-8>`
Preferred. Server-side Lua observation with real voxel names.

Example response:

```json
{
  "schema_version": 8,
  "health": 18,
  "hunger_available": true,
  "hunger": 14,
  "saturation": 3.5,
  "position": [10, 65, -3],
  "facing": "north",
  "nodes": [
    {"pos":[10,65,-4],"name":"mcl_core:stone","groups":["stone"]},
    {"pos":[9,65,-4],"name":"mcl_core:dirt","groups":["soil"]}
  ],
  "node_total": 27,
  "node_limit": 200,
  "node_truncated": false,
  "voxel_map": {
    "radius": 4,
    "origin": [6,61,-7],
    "size": [9,9,9],
    "order": "y_x_z_z_fastest",
    "palette": [
      {"name":"air","walkable":false,"diggable":false,"groups":[]},
      {"name":"mcl_core:stone","walkable":true,"diggable":true,"groups":["stone"]}
    ],
    "runs": [[2,324],[1,405]],
    "complete": true
  },
  "inventory": {
    "wield": {"name":"mcl_tools:pick_wood","count":1,"wear":128,"food":false,"food_points":0},
    "main": [
      {"name":"mcl_core:cobble","count":12},
      {"name":"mcl_mobitems:beef","count":3,"food":true,"food_points":3}
    ],
    "main_truncated": false
  },
  "chests": [
    {
      "name":"mcl_chests:chest_small","kind":"chest","pos":[12,65,-3],
      "distance":2.0,"accessible":true,"status":"accessible",
      "contents":[{"name":"mcl_core:cobble","count":32}],
      "contents_truncated":false,"used_slots":1,"slots":27
    }
  ],
  "chests_truncated": false,
  "furnaces": [
    {
      "name":"mcl_furnaces:furnace","kind":"furnace","pos":[14,65,-3],
      "distance":4.0,"accessible":true,"status":"accessible",
      "active":false,"speed":1,"input":null,"fuel":null,
      "output":null,"activity":"idle","output_blocked":false,
      "input_options":[
        {"name":"mcl_mobitems:beef","count":3,
         "output":{"name":"mcl_mobitems:cooked_beef","count":1},"cook_time":10}
      ],
      "fuel_options":[
        {"name":"mcl_core:coal_lump","count":4,"burn_time":80}
      ]
    }
  ],
  "furnaces_truncated": false,
  "crafting": {
    "craftable": [
      {
        "item":"mcl_core:stick","output_per_batch":4,"max_batches":3,
        "table_required":false,
        "ingredients":[{"item":"mcl_core:wood","count":2}]
      }
    ]
  },
  "players": [
    {"type":"player","name":"Alice","dx":2,"dy":0,"dz":1}
  ],
  "hostiles": [
    {"type":"hostile","name":"mobs_mc:zombie","hp":20,"distance":4.2,"dx":4,"dy":0,"dz":1}
  ],
  "mobs": [
    {
      "type":"mob","name":"mobs_mc:cow","category":"animal","hp":10,
      "distance":2.5,"dx":2,"dy":0,"dz":1,
      "adult":true,"tamed":false,"owned":false,"named":false,
      "food_source":true,"safe_to_hunt":true,
      "food_drops":[{"name":"mcl_mobitems:beef","min":1,"max":3,"food_points":3}]
    }
  ],
  "hostile_scan_radius": 12,
  "mob_scan_radius": 12,
  "items": [
    {"type":"item","name":"mcl_core:wood","count":2,"dx":1,"dy":0,"dz":0}
  ],
  "obstacles": {
    "front":"stone",
    "left":"air",
    "right":"air",
    "back":"stone"
  },
  "controller": {
    "follow_enabled": true,
    "follow_target": "Alice",
    "move_active": false,
    "move_target": null,
    "navigation": {
      "status":"following","current_waypoint":null,"waypoints_remaining":0,
      "recovering":false,"recovery_attempts":0,"stalled_for_seconds":0,
      "last_error":null,"arrival_action":null
    }
  },
  "goal":"idle"
}
```

Notes:
- `voxel_map` contains every voxel in the requested radius, including air and unloaded `ignore` cells. Runs are `[one-based palette index, length]`; traversal is y, then x, then z with z changing fastest.
- `nodes` is a convenient nearest-200 list of collidable, diggable blocks derived from the same full-radius voxel scan and includes useful resource groups.
- `players` and dropped `items` are separate lists; item offsets are relative to the bot.
- `hostiles` is sorted nearest-first and contains only entities classified as hostile by the game/mod. It includes the exact entity name, current HP, distance in nodes, and relative offsets. Hostiles are scanned to at least 12 nodes even when the voxel radius is smaller.
- `mobs` is a separate nearest-first list of passive and neutral mobs. Schema 5 adds adult/ownership safety flags and registered edible drops. `food_source` means the mob can drop food; only `safe_to_hunt:true` is eligible for the constrained food-hunting action.
- `hunger_available` indicates whether the running game exposes hunger and saturation. Inventory food metadata remains available when it does not.
- `chests` contains at most eight nearby supported normal, trapped, double, or shulker chests and barrels. Contents are aggregated by exact item name and include up to 54 types (the maximum number of occupied slots in a double chest). Protected, obstructed, blocked-lid, or out-of-range containers are listed as inaccessible without revealing contents. Empty `chests` or `contents` values may be encoded as `null` by Luanti's JSON writer; the built-in agent treats them as empty arrays. Ender chests are intentionally excluded because their storage is player-specific.
- `furnaces` contains at most eight exact normal-furnace, blast-furnace, or smoker positions. Accessible entries expose their one-slot input/fuel/output state, registered cooking result and progress, and only inventory inputs/fuels accepted by that specific furnace type. Protected, obstructed, or distant furnaces do not reveal contents.
- `crafting.craftable` is a bounded allowlist of useful basic outputs that the current inventory can make through registered server recipes. Counts are per recipe batch; `table_required:true` means an accessible nearby crafting table is required.
- `controller` reports movement/follow commands and waypoint recovery state currently running in the Rust client. A failed navigation and its `last_error` remain visible until the next movement command or `/stop`.
- `schema_version` advertises world-mod capabilities. Version `8` adds furnaces, barrels, registered basic crafting, and resource prioritization; version `7` added chest observation and native inventory transfers, version `6` added native-mining preparation/verification, version `5` added path planning and food/hunger metadata, version `4` added passive/neutral mobs, and version `3` added richer hostile observations and filtered defensive combat.
- `inventory.main` is capped at 30 entries.

### `GET /observe?radius=<1-8>`
Bot-side cache. May include `content:*` ids. Use only if server-side observe is unavailable.

## Movement

### `POST /move`
Move relative to facing or by delta.

Direction:
```
POST /move?direction=forward&steps=2
```

Delta (nodes):
```
POST /move?dx=2&dy=0&dz=-1
```

Returns: `OK` (text).

### `POST /move_to`
Move toward absolute node position.

```
POST /move_to?x=10&y=65&z=-3
```

Returns: `OK` (text).

### `POST /navigate_node?node=<exact_name>&radius=<2-32>`

Find a loaded matching node, choose a safe adjacent standing position, and queue a server-planned waypoint route. The Rust client traverses the route with normal movement physics.

```text
POST /navigate_node?node=mcl_core:stone_with_coal&radius=24
```

The JSON response reports the selected node, target, stand position, and accepted path. It may fail with `invalid_selector`, `target_not_found`, `no_reachable_target`, or `pathfinder_unavailable`.

### `POST /gather_resource?node=<exact_name>&count=<1-8>&radius=<2-32>`

Plan and walk to a matching resource, then run bounded collection when the bot reaches the safe adjacent position.

```text
POST /gather_resource?node=mcl_core:stone_with_coal&count=4&radius=24
```

The initial response confirms that navigation was accepted. Verify completion through the next observation/inventory update; the route can later report `controller.navigation.status:"failed"` if anti-stuck recovery is exhausted.

## Follow / Stop

### `POST /follow?target=<player>`
Follow a player.

### `POST /stop`
Stop movement and follow.

## Combat / Interaction

These endpoints wait for the world mod to execute the command and return JSON such as
`{"ok":true,"status":"attacked"}`. A rejected target returns `ok: false`; a missing or
outdated world mod returns an HTTP timeout rather than a false success.

### `POST /attack?target=<name>`
Attack an observed player.

### `POST /defend?radius=<1-20>`
Attack the nearest hostile mob with a short, human-paced combo. The default radius is 12 nodes. This endpoint filters out players, dropped items, and passive mobs.

### `POST /approach?target=<name>`
Approach a target.

### `POST /interact?target=<name>`
Interact (right-click) target.

### `POST /fight?target=<name>`
Attack an observed player or hostile entity. Passive/neutral mobs from `mobs` are rejected as combat targets.

### `POST /hunt_food?target=<exact_entity_name>&radius=<2-24>`

Plan a route to one observed passive food animal and perform a bounded, human-paced hunt on arrival. The world mod revalidates the same object before every strike and rejects babies, named animals, pets, owned/tamed animals, non-food mobs, and protected population levels. This does not broaden `/fight` to passive mobs.

```text
POST /hunt_food?target=mobs_mc:cow&radius=16
```

## Chat

### `POST /say?message=<text>`
Send a chat message as the bot.

### `GET /chat?since=<id>&limit=<1-100>`
Chat log (valid JSON).

Example response:

```json
{
  "last": 42,
  "messages": [
    {"id": 41, "ts_ms": 1234, "from": "player", "msg": "hello"}
  ]
}
```

## Teleport

### `POST /teleport?target=<player>`
Alias: `POST /tp?target=<player>`

## Sleep

### `POST /sleep?radius=<1-20>`
Sleep in the nearest bed within radius.

Response (JSON):

```json
{"ok":true,"status":"sleep"}
```

Possible status values:
- `sleep`
- `no_player`
- `no_bed`
- `failed`

## Mining

### `POST /mine`
Dig the block in front of the bot through Luanti's native player interaction protocol.

### `POST /mine?x=<int>&y=<int>&z=<int>`
Dig a specific block. The world helper validates the target and selects the fastest
harvest-capable inventory tool; Rust then sends start-digging, waits the real tool/node
duration, sends digging-completed, and verifies that the server changed the node.

Response (JSON):

```json
{"ok":true,"status":"mined","mined":1,"requested":1,"harvestable":true,"drop_pending":true}
```

Possible status values:
- `mined`
- `mining_busy`
- `no_player`
- `no_block`
- `not_diggable`
- `out_of_range`
- `protected`
- `no_diggable_tool`
- `no_harvest_tool`
- `node_unchanged`
- `prepare_timeout`
- `verify_timeout`

### `POST /collect?node=<exact_name>&count=<1-8>&radius=<1-6>`
Mine several nearby blocks with the exact node name. Targets are processed one at a
time with native player digging; each target is revalidated and the best suitable tool
is selected before starting it. Failed or changed candidates are skipped in favor of a
small bounded substitute set.

Example:

```text
POST /collect?node=mcl_core:tree&count=4&radius=6
```

Response (JSON):

```json
{"ok":true,"status":"mined","node":"mcl_core:tree","mined":4,"requested":4,"failed":0,"harvestable":true,"drop_pending":true}
```

`mined` confirms normal server-side digging, not that a physical drop has already
entered inventory. In survival, walk within pickup range if `nearby_items` still lists
the drop. VoxeLibre creative mode intentionally keeps only one copy of each mined item
type in inventory, matching normal player behavior.

## Container Storage

### `GET /chest?x=<int>&y=<int>&z=<int>`

Inspect one nearby supported chest or barrel. `POST /chest/inspect` accepts the same query parameters or a JSON body.

```json
{"x":12,"y":65,"z":-3}
```

### `POST /chest/deposit`

Deposit an exact item and count from the bot's `main` inventory.

```json
{"x":12,"y":65,"z":-3,"item":"mcl_core:cobble","count":16}
```

### `POST /chest/withdraw`

Withdraw an exact item and count into the bot's `main` inventory. Query parameters are also accepted.

```text
POST /chest/withdraw?x=12&y=65&z=-3&item=mcl_core:cobble&count=8
```

Transfers use Luanti's native inventory action rather than direct Lua mutation. The helper also requires a visible container, and the server enforces normal interaction distance, protection, inventory callbacks, shulker restrictions, logging, and rollback behavior. Each move is counted from its nonce-correlated server callback, so unrelated inventory activity cannot be mistaken for the bot's transfer. A request may return `partially_deposited` or `partially_withdrawn` when capacity or the eight-slot action limit prevents the full count from moving. The response always includes `requested`, `moved`, and `remaining`.

If a request is cancelled or times out after its native packet was dispatched but before the callback receipt arrives, it returns `ok:false`, `outcome_unknown:true`, and `confirmed_moved`. Re-observe before retrying; this prevents an uncertain late move from being reported as a normal partial success.

Ender chests and entity-backed chest minecarts are not supported by these endpoints.

## Furnaces

The furnace adapter supports exact nearby normal furnaces, blast furnaces, and smokers, including their active node variants. All position-taking endpoints accept either top-level `x`, `y`, and `z` fields or `"pos":[x,y,z]`.

### `GET /furnace?x=<int>&y=<int>&z=<int>`

Inspect one furnace. `POST /furnace/inspect` accepts the same position in a JSON body. The result includes input/fuel/output stacks, activity and progress, plus registered `input_options` and `fuel_options` from the bot's inventory.

### `POST /furnace/input`

Move a validated cookable input into `src`:

```json
{"pos":[14,65,-3],"item":"mcl_mobitems:beef","count":3}
```

### `POST /furnace/fuel`

Move validated fuel into `fuel`. Fuels that leave a replacement container, such as a lava bucket, are limited to one at a time.

```json
{"pos":[14,65,-3],"item":"mcl_core:coal_lump","count":1}
```

### `POST /furnace/output`

Move an exact observed output stack from `dst` to the bot's inventory. `POST /furnace/collect` is an alias intended for the built-in agent.

```json
{"pos":[14,65,-3],"item":"mcl_mobitems:cooked_beef","count":3}
```

### `POST /furnace/load`

Validate and load input first, then fuel:

```json
{
  "pos":[14,65,-3],
  "input":"mcl_mobitems:beef","input_count":3,
  "fuel":"mcl_core:coal_lump","fuel_count":1
}
```

This two-leg operation is deliberately not reported as atomic. Its JSON response contains separate `input` and `fuel` receipts; top-level `ok:true` means both requested counts moved completely. `partially_loaded` means at least one leg was partial or failed, so inspect the furnace again before retrying.

Every furnace transfer uses the same native, nonce-correlated inventory pipeline as containers. The Lua helper checks exact node type, line of sight, range, protection, registered cooking/fuel results, specialized-furnace groups, capacity, and blocked output. Luanti remains responsible for inventory callbacks, consumption, XP, achievements, logging, and rollback.

## Basic Crafting

### `POST /craft`

Craft a useful basic output currently listed in `observation.crafting.craftable`:

```json
{"item":"mcl_torches:torch","count":8}
```

`count` is the minimum desired output. Because recipes produce fixed batch sizes, `produced` may be larger and `surplus` reports the difference. One request is limited to eight batches and one output stack.

The Lua helper selects only an allowlisted registered recipe and returns exact inventory/grid slots. Rust then performs native `Move` actions into the craft grid, native `Craft`, and native `MoveSomewhere` into `main`. This preserves normal recipe precedence, ingredient consumption, replacements policy, `on_craft` callbacks, achievements, and server logs. Planks, sticks, torches, and a crafting table can use the 2x2 grid; wooden/stone tools and a furnace require a nearby accessible crafting table. Occupied grids, missing ingredients, full output inventory, unlisted recipes, and inaccessible tables are rejected without starting a craft.

## Placement

### `POST /place`
Place the wielded block in front of the bot.

### `POST /place?x=<int>&y=<int>&z=<int>`
Place a block at a specific position using the wielded item.

Response (JSON):

```json
{"ok":true,"status":"placed"}
```

Possible status values:
- `placed`
- `no_player`
- `no_item`
- `no_space`
- `out_of_range`

## Agent Telemetry

### `GET /agent/telemetry`

Returns the latest status published by the separate `agent` process. The envelope includes an
`online` flag, heartbeat `age_ms`, a monotonic `revision`, and the last agent snapshot. An API that
has not heard from an agent returns `agent:null`; a snapshot becomes stale after eight seconds
without a heartbeat but remains available for inspection.

```json
{
  "online": true,
  "age_ms": 412,
  "revision": 27,
  "agent": {
    "schema_version": 1,
    "bot_name": "Bot",
    "model": "gpt-5-nano",
    "tick": 84,
    "phase": "acting",
    "phase_reason": "move: OK",
    "mission": {"description":"gather wood"},
    "objective": null,
    "usage": {"requests":12,"input_tokens":21000,"cached_input_tokens":8000,"output_tokens":950,"total_tokens":21950},
    "decision": {
      "status": "executing",
      "request_latency_ms": 740,
      "offered_tools": 9,
      "selected_tools": [{"name":"move","arguments":{"direction":"forward","steps":2}}]
    },
    "recent_tools": [
      {"tick":83,"name":"move","arguments":{"direction":"forward"},"ok":true,"result":"OK","position":[1,64,2]}
    ]
  }
}
```

Telemetry reports phases, goals, selected calls, arguments, results, timings, controller/world
summary, and existing token accounting. It intentionally does not expose provider credentials,
prompts, or hidden model reasoning.

### `POST /agent/telemetry`

Used internally by the built-in agent to publish a schema-version-1 snapshot. Reposting the same
snapshot refreshes its heartbeat without incrementing `revision`. This endpoint uses the same
Bearer token as every other bot API endpoint.

Publishing runs on a bounded background path and the Web UI polls only this local endpoint, so the
telemetry is never inserted into an LLM request and consumes no additional model tokens.

## Health / Where

### `GET /health`
Liveness check. Returns `OK`.

### `GET /where`
Returns last known bot position.

Example:
```
pos=(10.00,65.00,-3.00)
```

## Legacy JSON Action Schema

Recommended JSON actions:

```json
{"action":"stop"}
{"action":"move","direction":"forward","steps":2}
{"action":"move","direction":"left","steps":1}
{"action":"move","dx":1,"dy":0,"dz":0}
{"action":"move_to","x":10,"y":65,"z":-3}
{"action":"navigate_node","node":"mcl_core:stone_with_coal","radius":24}
{"action":"follow","target":"player1"}
{"action":"defend"}
{"action":"approach","target":"player1"}
{"action":"interact","target":"player1"}
{"action":"fight","target":"player1"}
{"action":"teleport","target":"player1"}
{"action":"say","message":"hello"}
{"action":"sleep","radius":6}
{"action":"mine"}
{"action":"mine","x":10,"y":65,"z":-3}
{"action":"collect_blocks","node":"mcl_core:tree","count":4}
{"action":"gather_resource","node":"mcl_core:stone_with_coal","count":4,"radius":24}
{"action":"hunt_food","target":"mobs_mc:cow","radius":16}
{"action":"collect_item","item":"mcl_core:wood"}
{"action":"deposit_item","x":12,"y":65,"z":-3,"item":"mcl_core:cobble","count":16}
{"action":"withdraw_item","x":12,"y":65,"z":-3,"item":"mcl_core:stone","count":8}
{"action":"place"}
{"action":"place","x":10,"y":65,"z":-3}
```

## Controller Rules (Hard Validation)

- If health is zero, ignore the model and stop.
- If requested target doesn't exist, reject and stop.
- If output JSON invalid, default to stop.
- If action is impossible (blocked path, out of range, no bed), stop or retry.

## Minimal LLM Prompt Template

```
You control a Luanti bot.
Output exactly one JSON action and nothing else.

LastAction: <action>
Observation: <json>
```
