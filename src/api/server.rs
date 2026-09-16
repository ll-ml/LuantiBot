use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use crate::bot::{ApiCommand, ChatLog, PendingReplies};
use crate::types::Vec3;
use super::routes::handle_api_connection;
use super::telemetry::AgentTelemetryStore;

pub(crate) fn run_api_server(
    addr: &str,
    token: &str,
    tx: mpsc::Sender<ApiCommand>,
    last_pos: Arc<Mutex<Vec3>>,
    pending_replies: PendingReplies,
    chat_log: Arc<Mutex<ChatLog>>,
) {
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(v) => v,
        Err(err) => {
            println!("api bind failed: {}", err);
            return;
        }
    };
    let agent_telemetry = AgentTelemetryStore::default();
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(v) => v,
            Err(_) => continue,
        };
        let token = token.to_string();
        let tx = tx.clone();
        let last_pos = Arc::clone(&last_pos);
        let pending_replies = pending_replies.clone();
        let chat_log = Arc::clone(&chat_log);
        let agent_telemetry = agent_telemetry.clone();
        thread::spawn(move || {
            handle_api_connection(
                stream,
                &token,
                tx,
                last_pos,
                pending_replies,
                chat_log,
                agent_telemetry,
            );
        });
    }
}
