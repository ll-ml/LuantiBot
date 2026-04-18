# Luanti Bot REST API (LLM Control Guide)

Base URL example: `http://127.0.0.1:9123`

If an API token is configured, include:

```
Authorization: Bearer <TOKEN>
```

## Control Loop (Recommended)

1. `GET /observe_server` to fetch state.
2. Build a short prompt with observation + last action.
3. LLM returns exactly one JSON action.
4. Controller validates action (target exists, range ok, etc.).
5. Execute via REST endpoint.
6. Repeat.

LLM outputs must be a single JSON object.

## Observation

### `GET /observe_server?radius=<1-8>`
Preferred. Server-side Lua observation with real voxel names.

Example response:

```json
{
  "health": 18,
  "position": [10, 65, -3],
  "facing": "north",
  "nodes": [
    {"pos":[10,65,-4],"name":"mcl_core:stone"},
    {"pos":[9,65,-4],"name":"mcl_core:dirt"}
  ],
  "node_total": 27,
  "node_limit": 200,
  "node_truncated": false,
  "inventory": {
    "wield": {"name":"mcl_tools:pick_wood","count":1,"wear":128},
    "main": [
      {"name":"mcl_core:cobble","count":12},
      {"name":"mcl_core:torch","count":5}
    ],
    "main_truncated": false
  },
  "hostiles": [],
  "items": [],
  "obstacles": {
    "front":"stone",
    "left":"air",
    "right":"air",
    "back":"stone"
  },
  "goal":"idle"
}
```

Notes:
- `nodes` only includes collidable, diggable blocks in immediate vicinity.
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

## Follow / Stop

### `POST /follow?target=<player>`
Follow a player.

### `POST /stop`
Stop movement and follow.

## Combat / Interaction

### `POST /attack?target=<name>`
Attack a player or entity.

### `POST /approach?target=<name>`
Approach a target.

### `POST /interact?target=<name>`
Interact (right-click) target.

### `POST /fight?target=<name>`
Attack + interact combo.

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
Dig the block in front of the bot using the wielded tool.

### `POST /mine?x=<int>&y=<int>&z=<int>`
Dig a specific block.

Response (JSON):

```json
{"ok":true,"status":"mined"}
```

Possible status values:
- `mined`
- `no_player`
- `no_block`
- `not_diggable`
- `out_of_range`

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

## Health / Where

### `GET /health`
Liveness check. Returns `OK`.

### `GET /where`
Returns last known bot position.

Example:
```
pos=(10.00,65.00,-3.00)
```

## Action Schema for LLM Output

Recommended JSON actions:

```json
{"action":"stop"}
{"action":"move","direction":"forward","steps":2}
{"action":"move","direction":"left","steps":1}
{"action":"move","dx":1,"dy":0,"dz":0}
{"action":"move_to","x":10,"y":65,"z":-3}
{"action":"follow","target":"player1"}
{"action":"attack","target":"nearest_hostile"}
{"action":"approach","target":"player1"}
{"action":"interact","target":"player1"}
{"action":"fight","target":"player1"}
{"action":"teleport","target":"player1"}
{"action":"say","message":"hello"}
{"action":"sleep","radius":6}
{"action":"mine"}
{"action":"mine","x":10,"y":65,"z":-3}
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
