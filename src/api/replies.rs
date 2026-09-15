use std::sync::mpsc;
use std::time::Duration;

use crate::bot::{ApiCommand, PendingReplies};
use super::response::write_json_error;

pub(super) enum BotReplyError {
    Busy,
    CommandChannelClosed,
    Timeout,
}

pub(super) fn send_and_wait_for_reply(
    tx: &mpsc::Sender<ApiCommand>,
    pending_replies: &PendingReplies,
    tag: &'static str,
    command: ApiCommand,
    timeout: Duration,
) -> std::result::Result<String, BotReplyError> {
    let Some(receiver) = pending_replies.register(tag) else {
        return Err(BotReplyError::Busy);
    };
    if tx.send(command).is_err() {
        pending_replies.cancel(tag);
        return Err(BotReplyError::CommandChannelClosed);
    }
    let reply = match receiver.recv_timeout(timeout) {
        Ok(reply) => Ok(reply),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(BotReplyError::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(BotReplyError::CommandChannelClosed),
    };
    pending_replies.cancel(tag);
    reply
}

pub(super) fn await_bot_reply(
    stream: &mut std::net::TcpStream,
    tx: &mpsc::Sender<ApiCommand>,
    pending_replies: &PendingReplies,
    tag: &'static str,
    command: ApiCommand,
    timeout: Duration,
    timeout_error: &str,
) -> Option<String> {
    match send_and_wait_for_reply(tx, pending_replies, tag, command, timeout) {
        Ok(reply) => Some(reply),
        Err(BotReplyError::Busy) => {
            write_json_error(stream, "409 Conflict", "request_already_in_progress");
            None
        }
        Err(BotReplyError::CommandChannelClosed) => {
            write_json_error(stream, "503 Service Unavailable", "bot_unavailable");
            None
        }
        Err(BotReplyError::Timeout) => {
            write_json_error(stream, "504 Gateway Timeout", timeout_error);
            None
        }
    }
}
