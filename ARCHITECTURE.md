# Architecture

The executable entry point in `src/main.rs` only enters the command-line application. The Rust
source is organized around runtime boundaries rather than placing every workflow in one file.

- `src/app/` parses CLI arguments, runs short diagnostic commands, and owns shared connection and
  authentication setup.
- `src/agent/` owns the LLM decision loop, provider protocol, prompt, persistent state, policy,
  narration, and tool adapters. `agent/mod.rs` is the single facade; the former parallel
  `src/agent.rs` no longer exists.
- `src/api/` is the local HTTP boundary used by the web controller and LLM agent. Request parsing,
  responses, asynchronous replies, routing, and listener startup are separated.
- `src/bot/` owns the live in-game controller. API command dispatch, chat history, observations,
  entity decoding, navigation, standalone movement modes, and the main session loop are separate.
  Native mining, inventory/furnace transfers, and crafting each have an explicit state machine in
  `src/bot/tasks/`.
- `src/network/` owns Luanti UDP transport and authentication. Inbound decoding, outbound encoding,
  event types, wire primitives, reliability/connection state, protocol constants, and SRP are
  isolated from game behavior.
- `src/game/` contains player movement, collision physics, and anti-stuck navigation.
- `src/world/` contains map-block storage and node definitions.
- `src/codec.rs` and `src/types.rs` hold small primitives shared across those boundaries.

The command names, flags, REST endpoints, payloads, and startup workflow are unchanged by this
layout. `cargo test` exercises the pure protocol, physics, agent-policy, API parsing, and native task
logic without requiring a live Luanti server.
