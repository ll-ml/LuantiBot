use std::collections::HashMap;
use std::io::Read;
use crate::types::IVec3;

use crate::bot::{MoveDirection, parse_node_position};

const MAX_API_REQUEST_BYTES: usize = 64 * 1024;

pub(super) fn read_http_request(reader: &mut impl Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut request = Vec::with_capacity(2048);
    let mut chunk = [0_u8; 4096];
    let mut expected_length = None;

    loop {
        let bytes_read = reader.read(&mut chunk)?;
        if bytes_read == 0 {
            return Ok((!request.is_empty()).then_some(request));
        }
        request.extend_from_slice(&chunk[..bytes_read]);
        if request.len() > MAX_API_REQUEST_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "API request exceeds size limit",
            ));
        }

        if expected_length.is_none() {
            if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let body_start = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let Some(body_length) = http_content_length(&headers) else {
                    return Ok(Some(request));
                };
                let total_length = body_start.saturating_add(body_length);
                if total_length > MAX_API_REQUEST_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "API request body exceeds size limit",
                    ));
                }
                expected_length = Some(total_length);
            }
        }

        if let Some(length) = expected_length {
            if request.len() >= length {
                request.truncate(length);
                return Ok(Some(request));
            }
        }
    }
}

pub(super) fn http_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    })
}

pub(super) fn parse_query(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        if let Some((k, v)) = pair.split_once('=') {
            out.insert(url_decode(k), url_decode(v));
        } else {
            out.insert(url_decode(pair), String::new());
        }
    }
    out
}

pub(super) fn url_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(h), Some(l)) = (hex_val(hi), hex_val(lo)) {
                    out.push((h << 4 | l) as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            ch => {
                out.push(ch as char);
                i += 1;
            }
        }
    }
    out
}

pub(super) fn hex_val(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}

pub(super) fn parse_json_value(body: &str) -> Option<serde_json::Value> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

pub(super) fn query_or_json_i32(
    params: &HashMap<String, String>,
    body: &str,
    key: &str,
) -> Option<i32> {
    params
        .get(key)
        .and_then(|value| value.parse::<i32>().ok())
        .or_else(|| {
            let payload = parse_json_value(body)?;
            let value = payload.get(key)?;
            value
                .as_i64()
                .and_then(|number| i32::try_from(number).ok())
                .or_else(|| value.as_str()?.parse::<i32>().ok())
        })
}

pub(super) fn query_or_json_bounded_u16(
    params: &HashMap<String, String>,
    body: &str,
    key: &str,
    default: u16,
    maximum: u16,
) -> Option<u16> {
    let parsed = if let Some(value) = params.get(key) {
        Some(value.parse::<u16>().ok()?)
    } else if let Some(value) = parse_json_value(body).and_then(|payload| payload.get(key).cloned())
    {
        Some(
            value
                .as_u64()
                .and_then(|number| u16::try_from(number).ok())
                .or_else(|| value.as_str()?.parse::<u16>().ok())?,
        )
    } else {
        None
    };
    let value = parsed.unwrap_or(default);
    (value >= 1 && value <= maximum).then_some(value)
}

pub(super) fn query_or_json_position(
    params: &HashMap<String, String>,
    body: &str,
) -> Option<IVec3> {
    let coordinates = (
        query_or_json_i32(params, body, "x"),
        query_or_json_i32(params, body, "y"),
        query_or_json_i32(params, body, "z"),
    );
    if let (Some(x), Some(y), Some(z)) = coordinates {
        return Some(IVec3 { x, y, z });
    }
    parse_json_value(body)?.get("pos").and_then(parse_node_position)
}

pub(super) fn request_is_authorized<'a>(headers: impl IntoIterator<Item = &'a str>, token: &str) -> bool {
    if token.is_empty() {
        return true;
    }
    headers.into_iter().take_while(|line| !line.trim().is_empty()).any(|line| {
        let Some((name, value)) = line.split_once(':') else {
            return false;
        };
        if !name.trim().eq_ignore_ascii_case("authorization") {
            return false;
        }
        let mut parts = value.split_whitespace();
        matches!(parts.next(), Some(scheme) if scheme.eq_ignore_ascii_case("bearer"))
            && parts.next() == Some(token)
            && parts.next().is_none()
    })
}

pub(super) fn parse_json_field(body: &str, key: &str) -> Option<String> {
    parse_json_value(body)?
        .get(key)?
        .as_str()
        .map(str::to_owned)
}

pub(super) fn parse_move_direction(value: &str) -> Option<MoveDirection> {
    match value.trim().to_ascii_lowercase().as_str() {
        "forward" | "fwd" => Some(MoveDirection::Forward),
        "back" | "backward" => Some(MoveDirection::Backward),
        "left" => Some(MoveDirection::Left),
        "right" => Some(MoveDirection::Right),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_header_name_and_scheme_are_case_insensitive() {
        assert!(request_is_authorized(
            ["authorization: Bearer secret", "accept: */*"],
            "secret"
        ));
        assert!(request_is_authorized(
            ["AUTHORIZATION: bearer secret"],
            "secret"
        ));
    }

    #[test]
    fn bearer_token_must_match_exactly() {
        assert!(!request_is_authorized(
            ["Authorization: Bearer wrong"],
            "secret"
        ));
        assert!(!request_is_authorized(
            ["Authorization: Basic secret"],
            "secret"
        ));
        assert!(request_is_authorized(["accept: */*"], ""));
    }

    #[test]
    fn numeric_parameters_accept_query_and_json_forms() {
        let mut query = HashMap::new();
        query.insert("radius".to_string(), "7".to_string());
        assert_eq!(query_or_json_i32(&query, "", "radius"), Some(7));
        assert_eq!(
            query_or_json_i32(&HashMap::new(), r#"{"radius":8}"#, "radius"),
            Some(8)
        );
        assert_eq!(
            query_or_json_i32(&HashMap::new(), r#"{"radius":"9"}"#, "radius"),
            Some(9)
        );
        assert_eq!(
            query_or_json_position(&HashMap::new(), r#"{"x":10,"y":20,"z":-3}"#),
            Some(crate::types::IVec3 { x: 10, y: 20, z: -3 })
        );
        assert_eq!(
            query_or_json_position(&HashMap::new(), r#"{"x":10,"y":20}"#),
            None
        );
        assert_eq!(
            query_or_json_position(&HashMap::new(), r#"{"pos":[10,20,-3]}"#),
            Some(crate::types::IVec3 { x: 10, y: 20, z: -3 })
        );
        assert_eq!(
            super::query_or_json_bounded_u16(&HashMap::new(), "{}", "count", 1, 99),
            Some(1)
        );
        assert_eq!(
            super::query_or_json_bounded_u16(
                &HashMap::new(),
                r#"{"count":"2"}"#,
                "count",
                1,
                99,
            ),
            Some(2)
        );
        for body in [r#"{"count":0}"#, r#"{"count":100}"#, r#"{"count":1.5}"#] {
            assert_eq!(
                super::query_or_json_bounded_u16(
                    &HashMap::new(),
                    body,
                    "count",
                    1,
                    99,
                ),
                None
            );
        }
        assert_eq!(
            super::query_or_json_bounded_u16(
                &HashMap::new(),
                r#"{"count":65}"#,
                "count",
                1,
                64,
            ),
            None
        );
    }

    #[test]
    fn content_length_header_is_case_insensitive() {
        assert_eq!(
            http_content_length("Host: localhost\r\ncontent-LENGTH: 27\r\n"),
            Some(27)
        );
        assert_eq!(http_content_length("Host: localhost\r\n"), None);
    }

    #[test]
    fn http_reader_waits_for_and_limits_the_declared_body() {
        let raw = b"POST /chat HTTP/1.1\r\nContent-Length: 7\r\n\r\n{\"x\":1}ignored";
        let mut input = std::io::Cursor::new(raw);
        let request = read_http_request(&mut input).unwrap().unwrap();
        assert_eq!(request, &raw[..raw.len() - "ignored".len()]);
    }

}
