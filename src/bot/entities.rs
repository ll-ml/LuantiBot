//! Active-object decoding and observed remote-player state.

use anyhow::{bail, Result};
use std::collections::HashMap;

use crate::codec::ByteReader;
use crate::types::Vec3;
use super::chat::normalize_player_name;

#[derive(Clone, Debug)]
pub(super) struct RemotePlayer {
    pub(super) name: String,
    pub(super) pos: Vec3,
}

#[derive(Clone, Debug)]
pub(super) struct ActiveObjectInitInfo {
    pub(super) name: String,
    pub(super) is_player: bool,
    pub(super) pos: Vec3,
}

pub(super) fn find_target_id(players: &HashMap<u16, RemotePlayer>, name: Option<&str>) -> Option<u16> {
    let target = name?;
    let target = normalize_player_name(target);
    players.iter().find_map(|(id, info)| {
        if normalize_player_name(&info.name) == target {
            Some(*id)
        } else {
            None
        }
    })
}

pub(super) fn parse_active_object_init(data: &[u8]) -> Result<ActiveObjectInitInfo> {
    let mut reader = ByteReader::new(data);
    let version = reader.read_u8()?;
    if version < 1 {
        bail!("unsupported active object init version: {version}");
    }
    let name = reader.read_string16()?;
    let is_player = reader.read_u8()? != 0;
    let _id = reader.read_u16()?;
    let pos = reader.read_vec3_f32()?;
    let _rot = reader.read_vec3_f32()?;
    let _hp = reader.read_u16()?;
    let msg_count = reader.read_u8()? as usize;
    for _ in 0..msg_count {
        let _ = reader.read_string32()?;
    }
    Ok(ActiveObjectInitInfo {
        name: normalize_player_name(&name),
        is_player,
        pos,
    })
}

pub(super) fn parse_active_object_update_position(data: &[u8]) -> Result<Option<Vec3>> {
    let mut reader = ByteReader::new(data);
    let cmd = reader.read_u8()?;
    if cmd != 1 {
        return Ok(None);
    }
    let pos = reader.read_vec3_f32()?;
    let _vel = reader.read_vec3_f32()?;
    let _acc = reader.read_vec3_f32()?;
    let _rot = reader.read_vec3_f32()?;
    let _do_interpolate = reader.read_u8()?;
    let _is_end = reader.read_u8()?;
    let _update_interval = reader.read_f32()?;
    Ok(Some(pos))
}
