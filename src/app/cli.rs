//! Command-line options and dispatch; connection workflows live in dedicated modules.

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

use crate::agent::{
    run_agent_loop, AgentConfig, ControllerConfig, JevControllerConfig, LlmApi,
    LlmControllerConfig,
};
use crate::bot::{follow_command, follow_player, join_bot, move_forward};
use super::diagnostics::{connect, handshake, login, observe, ping_server, send_chat, trace_session};

#[derive(Parser)]
#[command(name = "luanti-proto-bot")]
#[command(about = "Minimal Luanti protocol client", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum PolicyKind {
    Llm,
    Jev,
}

#[derive(Subcommand)]
enum Commands {
    /// Ping a Luanti server (UDP) and read peer id
    Ping { address: String },
    /// Perform a minimal handshake and read TOCLIENT_HELLO
    Handshake { address: String, player: String },
    /// Register/login with FIRST_SRP (empty password recommended for local dev)
    Login {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Login and stay connected, sending player position updates
    Connect {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
    },
    /// Connect and move forward for a duration (simple demo)
    Move {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to move forward
        seconds: f32,
        /// Speed in nodes/sec
        #[arg(long, default_value = "2.0")]
        speed: f32,
    },
    /// Connect and follow the first non-self player seen in chat
    Follow {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to follow before exiting
        #[arg(long, default_value = "30")]
        seconds: f32,
        /// Speed in nodes/sec
        #[arg(long, default_value = "2.0")]
        speed: f32,
        /// Desired follow distance in nodes
        #[arg(long, default_value = "2.0")]
        distance: f32,
    },
    /// Connect, teleport to first non-self player, then send follow command
    FollowCmd {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to wait for a target
        #[arg(long, default_value = "30")]
        seconds: f32,
        /// Teleport command template (use {player})
        #[arg(long, default_value = "/teleport {player}")]
        tp_cmd: String,
        /// Follow command template (use {player})
        #[arg(long, default_value = "/bot_follow {player}")]
        follow_cmd: String,
    },
    /// Connect and stay in-game, accept chat commands
    Join {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Comma-separated list of allowed senders (empty = allow anyone)
        #[arg(long, default_value = "")]
        allow: String,
        /// Teleport command template (use {player})
        #[arg(long, default_value = "/teleport {player}")]
        tp_cmd: String,
        /// Follow command template (use {player})
        #[arg(long, default_value = "/bot_follow {player}")]
        follow_cmd: String,
        /// Stop command template
        #[arg(long, default_value = "/bot_stop")]
        stop_cmd: String,
        /// Follow speed in nodes/sec
        #[arg(long, default_value = "2.0")]
        follow_speed: f32,
        /// Follow distance in nodes
        #[arg(long, default_value = "2.0")]
        follow_distance: f32,
        /// Disable physics (float mode)
        #[arg(long, default_value_t = false)]
        float: bool,
        /// REST API address (empty disables)
        #[arg(long, default_value = "127.0.0.1:9123")]
        api_addr: String,
        /// REST API token (optional)
        #[arg(long, default_value = "")]
        api_token: String,
    },
    /// Send a chat message and exit
    Chat {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        message: String,
    },
    /// Connect and periodically emit a JSON observation
    Observe {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to run before exiting
        #[arg(long, default_value = "15")]
        seconds: u64,
        /// Observation interval in seconds
        #[arg(long, default_value = "1")]
        interval: u64,
    },
    /// Trace inbound packets as JSON
    Trace {
        address: String,
        player: String,
        #[arg(long, default_value = "")]
        password: String,
        /// Seconds to run before exiting
        #[arg(long, default_value = "15")]
        seconds: u64,
    },
    /// Run an autonomous policy loop using the REST API
    Agent {
        /// API base address (host:port or http://host:port)
        #[arg(long, default_value = "127.0.0.1:9123")]
        api: String,
        /// API token (optional)
        #[arg(long, default_value = "")]
        api_token: String,
        /// Decision policy. LLM remains the transition default; select jev to use System One.
        #[arg(long, value_enum, default_value_t = PolicyKind::Llm)]
        policy: PolicyKind,
        /// LLM server URL (OpenAI-compatible)
        #[arg(long, default_value = "http://127.0.0.1:8080/v1/chat/completions")]
        llm_url: String,
        /// LLM API protocol: auto, chat-completions, or responses
        #[arg(long, default_value = "auto")]
        llm_api: LlmApi,
        /// LLM provider key (defaults to OPENAI_API_KEY)
        #[arg(long, env = "OPENAI_API_KEY", default_value = "")]
        llm_api_key: String,
        /// Model name for the LLM server
        #[arg(long, default_value = "local-model")]
        model: String,
        /// TypeSafe System One endpoint
        #[arg(long, default_value = "https://api.typesafe.ai/v1/systemone")]
        jev_url: String,
        /// TypeSafe API key (defaults to TYPESAFE_API_KEY)
        #[arg(long, env = "TYPESAFE_API_KEY", default_value = "")]
        jev_api_key: String,
        /// Jev model or alias
        #[arg(long, default_value = "jev-latest")]
        jev_model: String,
        /// Minimum confidence for routine Jev actions; material actions require at least 0.70
        #[arg(long, default_value = "0.65")]
        jev_min_confidence: f64,
        /// Jev request timeout in seconds
        #[arg(long, default_value = "15")]
        jev_timeout_secs: u64,
        /// Allow Jev selections to execute. Without this flag Jev runs safely in dry-run mode.
        #[arg(long, default_value_t = false)]
        jev_execute: bool,
        /// Bot player name (used to ignore self chat)
        #[arg(long, default_value = "Bot")]
        bot_name: String,
        /// Comma-separated players allowed to give the agent goals (empty = anyone)
        #[arg(long, value_delimiter = ',')]
        allow: Vec<String>,
        /// Passive mode (observe + chat only)
        #[arg(long, default_value_t = false)]
        passive: bool,
        /// Allow the agent to choose small, bounded goals when no player goal is active
        #[arg(long, default_value_t = false)]
        autonomous: bool,
        /// Observe radius
        #[arg(long, default_value = "4")]
        radius: i32,
        /// Decision interval in milliseconds
        #[arg(long, default_value = "2000")]
        interval_ms: u64,
        /// Decision interval when the bot has no goal or new chat
        #[arg(long, default_value = "10000")]
        idle_interval_ms: u64,
        /// LLM temperature
        #[arg(long, default_value = "0.2")]
        temperature: f32,
        /// LLM output-token ceiling (reasoning tokens count toward this)
        #[arg(long, default_value = "768")]
        max_tokens: u32,
        /// Reasoning effort (empty omits it; use none for Chat Completions tools)
        #[arg(long, default_value = "none")]
        reasoning_effort: String,
        /// Initial persistent in-game goal
        #[arg(long)]
        goal: Option<String>,
        /// Persist goals, observations, action results, and token usage here
        #[arg(long)]
        state_file: Option<std::path::PathBuf>,
        /// Maximum tool calls accepted from one model decision
        #[arg(long, default_value = "3")]
        max_tool_calls: usize,
        /// Disable concise in-game narration of successful actions
        #[arg(long, default_value_t = false)]
        quiet_actions: bool,
        /// Minimum seconds between automatic action messages
        #[arg(long, default_value = "10")]
        narration_cooldown_secs: u64,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Ping { address } => ping_server(&address),
        Commands::Handshake { address, player } => handshake(&address, &player),
        Commands::Login {
            address,
            player,
            password,
        } => login(&address, &player, &password),
        Commands::Connect {
            address,
            player,
            password,
        } => connect(&address, &player, &password),
        Commands::Move {
            address,
            player,
            password,
            seconds,
            speed,
        } => move_forward(&address, &player, &password, seconds, speed),
        Commands::Follow {
            address,
            player,
            password,
            seconds,
            speed,
            distance,
        } => follow_player(&address, &player, &password, seconds, speed, distance),
        Commands::FollowCmd {
            address,
            player,
            password,
            seconds,
            tp_cmd,
            follow_cmd,
        } => follow_command(&address, &player, &password, seconds, &tp_cmd, &follow_cmd),
        Commands::Join {
            address,
            player,
            password,
            allow,
            tp_cmd,
            follow_cmd,
            stop_cmd,
            follow_speed,
            follow_distance,
            float,
            api_addr,
            api_token,
        } => join_bot(
            &address,
            &player,
            &password,
            &allow,
            &tp_cmd,
            &follow_cmd,
            &stop_cmd,
            follow_speed,
            follow_distance,
            float,
            &api_addr,
            &api_token,
        ),
        Commands::Chat {
            address,
            player,
            password,
            message,
        } => send_chat(&address, &player, &password, &message),
        Commands::Observe {
            address,
            player,
            password,
            seconds,
            interval,
        } => observe(&address, &player, &password, seconds, interval),
        Commands::Trace {
            address,
            player,
            password,
            seconds,
        } => trace_session(&address, &player, &password, seconds),
        Commands::Agent {
            api,
            api_token,
            policy,
            llm_url,
            llm_api,
            llm_api_key,
            model,
            jev_url,
            jev_api_key,
            jev_model,
            jev_min_confidence,
            jev_timeout_secs,
            jev_execute,
            bot_name,
            allow,
            passive,
            autonomous,
            radius,
            interval_ms,
            idle_interval_ms,
            temperature,
            max_tokens,
            reasoning_effort,
            goal,
            state_file,
            max_tool_calls,
            quiet_actions,
            narration_cooldown_secs,
        } => run_agent_loop(AgentConfig {
            api_base: api,
            api_token,
            controller: match policy {
                PolicyKind::Llm => ControllerConfig::Llm(LlmControllerConfig {
                    url: llm_url,
                    api: llm_api,
                    api_key: llm_api_key,
                    model,
                    temperature,
                    max_tokens,
                    reasoning_effort: (!reasoning_effort.trim().is_empty())
                        .then_some(reasoning_effort),
                }),
                PolicyKind::Jev => ControllerConfig::Jev(JevControllerConfig {
                    url: jev_url,
                    api_key: jev_api_key,
                    model: jev_model,
                    minimum_confidence: jev_min_confidence,
                    timeout_secs: jev_timeout_secs,
                    execute: jev_execute,
                }),
            },
            bot_name,
            allowed_senders: allow,
            passive,
            autonomous,
            observe_radius: radius,
            interval_ms,
            idle_interval_ms,
            initial_goal: goal,
            state_file,
            max_tool_calls,
            narrate_actions: !quiet_actions,
            narration_cooldown_secs,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jev_policy_in_safe_dry_run_mode() {
        let cli = Cli::try_parse_from([
            "luanti-proto-bot",
            "agent",
            "--policy",
            "jev",
            "--jev-api-key",
            "test-key",
        ])
        .expect("Jev CLI should parse");
        let Commands::Agent {
            policy,
            jev_api_key,
            jev_execute,
            ..
        } = cli.command
        else {
            panic!("expected agent command");
        };
        assert_eq!(policy, PolicyKind::Jev);
        assert_eq!(jev_api_key, "test-key");
        assert!(!jev_execute);
    }

    #[test]
    fn legacy_agent_policy_remains_the_transition_default() {
        let cli = Cli::try_parse_from(["luanti-proto-bot", "agent"])
            .expect("legacy CLI should parse");
        let Commands::Agent { policy, .. } = cli.command else {
            panic!("expected agent command");
        };
        assert_eq!(policy, PolicyKind::Llm);
    }
}
