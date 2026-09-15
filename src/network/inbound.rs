//! Decode server command payloads after transport framing has been removed.

use crate::game::MovementSettings;
use super::events::{ActiveObjectInit, ActiveObjectMessage, MtpEvent};
use super::protocol::{
    TOCLIENT_ACCESS_DENIED,
    TOCLIENT_ACTIVE_OBJECT_MESSAGES,
    TOCLIENT_ACTIVE_OBJECT_REMOVE_ADD,
    TOCLIENT_ANNOUNCE_MEDIA,
    TOCLIENT_AUTH_ACCEPT,
    TOCLIENT_BLOCKDATA,
    TOCLIENT_CHAT_MESSAGE,
    TOCLIENT_HELLO,
    TOCLIENT_ITEMDEF,
    TOCLIENT_MOVEMENT,
    TOCLIENT_MOVE_PLAYER,
    TOCLIENT_NODEDEF,
    TOCLIENT_SRP_BYTES_S_B,
};
use super::wire::{read_bytes_slice, read_f32_slice, read_string_slice, read_u8, read_u16, read_u32, read_v3f32_slice, read_v3s16_slice, read_wstring_slice};

pub(super) fn parse_to_client(cmd: u16, payload: &[u8]) -> Option<MtpEvent> {
    match cmd {
        TOCLIENT_HELLO => {
            let mut offset = 0;
            let ser_ver = read_u8(payload, &mut offset).ok()?;
            let _unused = read_u16(payload, &mut offset).ok()?;
            let proto_ver = read_u16(payload, &mut offset).ok()?;
            let auth_mechs = read_u32(payload, &mut offset).ok()?;
            let _unused_str = read_string_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::ToClientHello {
                auth_mechs,
                proto_ver,
                ser_ver,
            })
        }
        TOCLIENT_AUTH_ACCEPT => {
            let mut offset = 0;
            let _unused_pos = read_v3f32_slice(payload, &mut offset).ok()?;
            if offset + 8 > payload.len() {
                return None;
            }
            offset += 8; // map seed
            let recommended_send_interval = read_f32_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::AuthAccept {
                recommended_send_interval,
            })
        }
        TOCLIENT_MOVE_PLAYER => {
            let mut offset = 0;
            let pos = read_v3f32_slice(payload, &mut offset).ok()?;
            let pitch = read_f32_slice(payload, &mut offset).ok()?.to_radians();
            let yaw = read_f32_slice(payload, &mut offset).ok()?.to_radians();
            if std::env::var("LUANTI_DEBUG_MOVEPLAYER")
                .map(|v| v == "1")
                .unwrap_or(false)
            {
                let hex = payload
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!(
                    "moveplayer pos=({:.3},{:.3},{:.3}) pitch={:.3} yaw={:.3} bytes={}",
                    pos.x, pos.y, pos.z, pitch, yaw, hex
                );
            }
            Some(MtpEvent::MovePlayer { pos, pitch, yaw })
        }
        TOCLIENT_MOVEMENT => {
            let mut offset = 0;
            let mut next = || read_f32_slice(payload, &mut offset).ok();
            let acceleration_default = next()?;
            let acceleration_air = next()?;
            let _acceleration_fast = next()?;
            let speed_walk = next()?;
            let _speed_crouch = next()?;
            let speed_fast = next()?;
            let _speed_climb = next()?;
            let speed_jump = next()?;
            let _liquid_fluidity = next()?;
            let _liquid_fluidity_smooth = next()?;
            let _liquid_sink = next()?;
            let gravity = next()?;
            Some(MtpEvent::Movement(MovementSettings {
                acceleration_default,
                acceleration_air,
                speed_walk,
                speed_fast,
                speed_jump,
                gravity,
            }))
        }
        TOCLIENT_BLOCKDATA => {
            let mut offset = 0;
            let pos = read_v3s16_slice(payload, &mut offset).ok()?;
            if offset > payload.len() {
                return None;
            }
            Some(MtpEvent::BlockData {
                pos,
                data: payload[offset..].to_vec(),
            })
        }
        TOCLIENT_ACTIVE_OBJECT_REMOVE_ADD => {
            let mut offset = 0;
            let remove_count = read_u16(payload, &mut offset).ok()? as usize;
            let mut removed = Vec::with_capacity(remove_count);
            for _ in 0..remove_count {
                removed.push(read_u16(payload, &mut offset).ok()?);
            }
            let add_count = read_u16(payload, &mut offset).ok()? as usize;
            let mut added = Vec::with_capacity(add_count);
            for _ in 0..add_count {
                let id = read_u16(payload, &mut offset).ok()?;
                let _ao_type = read_u8(payload, &mut offset).ok()?;
                let len = read_u32(payload, &mut offset).ok()? as usize;
                if offset + len > payload.len() {
                    return None;
                }
                let data = payload[offset..offset + len].to_vec();
                offset += len;
                added.push(ActiveObjectInit { id, data });
            }
            Some(MtpEvent::ActiveObjectRemoveAdd { removed, added })
        }
        TOCLIENT_ACTIVE_OBJECT_MESSAGES => {
            let mut offset = 0;
            let mut messages = Vec::new();
            while offset < payload.len() {
                let id = read_u16(payload, &mut offset).ok()?;
                let len = read_u16(payload, &mut offset).ok()? as usize;
                if offset + len > payload.len() {
                    return None;
                }
                let data = payload[offset..offset + len].to_vec();
                offset += len;
                messages.push(ActiveObjectMessage { id, data });
            }
            Some(MtpEvent::ActiveObjectMessages { messages })
        }
        TOCLIENT_SRP_BYTES_S_B => {
            let mut offset = 0;
            let salt = read_bytes_slice(payload, &mut offset).ok()?;
            let b = read_bytes_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::SrpBytesSB { salt, b })
        }
        TOCLIENT_NODEDEF => {
            let mut offset = 0;
            let len = read_u32(payload, &mut offset).ok()? as usize;
            if offset + len > payload.len() {
                return None;
            }
            let data = payload[offset..offset + len].to_vec();
            Some(MtpEvent::NodeDef { data })
        }
        TOCLIENT_ITEMDEF => Some(MtpEvent::ItemDef),
        TOCLIENT_ANNOUNCE_MEDIA => Some(MtpEvent::MediaAnnounce),
        TOCLIENT_CHAT_MESSAGE => {
            let mut offset = 0;
            let _version = read_u8(payload, &mut offset).ok()?;
            let message_type = read_u8(payload, &mut offset).ok()?;
            let sender = read_wstring_slice(payload, &mut offset).ok()?;
            let message = read_wstring_slice(payload, &mut offset).ok()?;
            Some(MtpEvent::ChatMessage {
                message_type,
                sender,
                message,
            })
        }
        TOCLIENT_ACCESS_DENIED => {
            let mut offset = 0;
            let reason = read_u8(payload, &mut offset).ok()?;
            Some(MtpEvent::AccessDenied { reason })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_f32(buf: &mut Vec<u8>, value: f32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    #[test]
    fn move_player_uses_network_f32_values() {
        let mut payload = Vec::new();
        for value in [7000.0, 250.0, 3500.0, -15.0, 90.0] {
            push_f32(&mut payload, value);
        }

        let Some(MtpEvent::MovePlayer { pos, pitch, yaw }) =
            parse_to_client(TOCLIENT_MOVE_PLAYER, &payload)
        else {
            panic!("MOVE_PLAYER did not parse");
        };
        assert_eq!(pos.x, 7000.0);
        assert_eq!(pos.y, 250.0);
        assert_eq!(pos.z, 3500.0);
        assert!((pitch.to_degrees() + 15.0).abs() < 0.001);
        assert!((yaw.to_degrees() - 90.0).abs() < 0.001);
    }

    #[test]
    fn movement_settings_use_network_f32_values() {
        let values = [3.0, 2.0, 10.0, 4.0, 1.35, 20.0, 3.0, 6.5, 1.0, 0.5, 10.0, 9.81];
        let mut payload = Vec::new();
        for value in values {
            push_f32(&mut payload, value);
        }

        let Some(MtpEvent::Movement(settings)) = parse_to_client(TOCLIENT_MOVEMENT, &payload)
        else {
            panic!("MOVEMENT did not parse");
        };
        assert_eq!(settings.acceleration_default, 3.0);
        assert_eq!(settings.speed_walk, 4.0);
        assert_eq!(settings.speed_fast, 20.0);
        assert!((settings.gravity - 9.81).abs() < 0.001);
    }

    #[test]
    fn auth_accept_interval_uses_network_f32_value() {
        let mut payload = Vec::new();
        for value in [0.0, 0.0, 0.0] {
            push_f32(&mut payload, value);
        }
        payload.extend_from_slice(&123_u64.to_be_bytes());
        push_f32(&mut payload, 0.1);

        let Some(MtpEvent::AuthAccept {
            recommended_send_interval,
        }) = parse_to_client(TOCLIENT_AUTH_ACCEPT, &payload)
        else {
            panic!("AUTH_ACCEPT did not parse");
        };
        assert!((recommended_send_interval - 0.1).abs() < 0.001);
    }

}
