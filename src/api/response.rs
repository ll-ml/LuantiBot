use std::io::Write;
use serde_json::json;

pub(super) fn json_ok() -> String {
    json!({"ok": true}).to_string()
}

pub(super) fn json_error(message: &str) -> String {
    json!({"ok": false, "error": message}).to_string()
}

pub(super) fn write_json_error(stream: &mut std::net::TcpStream, status: &str, message: &str) {
    let body = json_error(message);
    write_http_response(stream, status, "application/json", &body);
}

pub(super) fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) {
    let reply = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type\r\nConnection: close\r\n\r\n{}",
        status,
        content_type,
        body.len(),
        body
    );
    let _ = stream.write_all(reply.as_bytes());
}
