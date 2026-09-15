//! Validation shared by the HTTP boundary and native bot tasks.

use crate::types::IVec3;

pub(crate) fn valid_command_atom(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control())
}

pub(crate) fn parse_node_position(value: &serde_json::Value) -> Option<IVec3> {
    let (x, y, z) = if let Some(values) = value.as_array() {
        if values.len() < 3 {
            return None;
        }
        (
            values[0].as_i64()?,
            values[1].as_i64()?,
            values[2].as_i64()?,
        )
    } else {
        (
            value.get("x")?.as_i64()?,
            value.get("y")?.as_i64()?,
            value.get("z")?.as_i64()?,
        )
    };
    Some(IVec3 {
        x: i32::try_from(x).ok()?,
        y: i32::try_from(y).ok()?,
        z: i32::try_from(z).ok()?,
    })
}

pub(super) fn chest_reply_has_nonce(value: &serde_json::Value, expected: &str) -> bool {
    value.get("nonce").and_then(serde_json::Value::as_str) == Some(expected)
}
