//! Encode client commands; the connection owns UDP delivery and reliability framing.

use anyhow::{bail, Result};
use crate::game::PlayerState;
use crate::types::{BlockPos, IVec3};
use super::connection::MtpConnection;
use super::events::InventoryLocation;
use super::protocol::{
    CLIENT_PROTOCOL_VERSION_MIN,
    LATEST_PROTOCOL_VERSION,
    SER_FMT_VER_HIGHEST_READ,
    TOSERVER_CHAT_MESSAGE,
    TOSERVER_CLIENT_READY,
    TOSERVER_FIRST_SRP,
    TOSERVER_GOTBLOCKS,
    TOSERVER_HAVE_MEDIA,
    TOSERVER_INIT,
    TOSERVER_INIT2,
    TOSERVER_INTERACT,
    TOSERVER_INVENTORY_ACTION,
    TOSERVER_PLAYERITEM,
    TOSERVER_PLAYERPOS,
    TOSERVER_SRP_BYTES_A,
    TOSERVER_SRP_BYTES_M,
    VERSION_MAJOR,
    VERSION_MINOR,
    VERSION_PATCH,
};
use super::wire::{write_bytes, write_f32, write_s32, write_string, write_u32, write_v3s16, write_v3s32, write_wstring};

impl MtpConnection {
    pub fn send_init(&mut self, player: &str) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_INIT.to_be_bytes());
        payload.push(SER_FMT_VER_HIGHEST_READ);
        payload.extend_from_slice(&0u16.to_be_bytes());
        payload.extend_from_slice(&CLIENT_PROTOCOL_VERSION_MIN.to_be_bytes());
        payload.extend_from_slice(&LATEST_PROTOCOL_VERSION.to_be_bytes());
        write_string(&mut payload, player);

        self.send_reliable(payload, "send init")
    }

    pub fn send_first_srp(&mut self, player: &str, password: &str) -> Result<()> {
        let (salt, verifier) = super::srp::generate_srp_verifier_and_salt(player, password)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_FIRST_SRP.to_be_bytes());
        write_bytes(&mut payload, &salt);
        write_bytes(&mut payload, &verifier);
        payload.push(if password.is_empty() { 1 } else { 0 });

        self.send_reliable(payload, "send first_srp")
    }

    pub fn send_srp_a(&mut self, a_bytes: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_SRP_BYTES_A.to_be_bytes());
        write_bytes(&mut payload, a_bytes);
        payload.push(1u8);

        self.send_reliable(payload, "send srp_a")
    }

    pub fn send_srp_m(&mut self, m: &[u8]) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_SRP_BYTES_M.to_be_bytes());
        write_bytes(&mut payload, m);

        self.send_reliable(payload, "send srp_m")
    }

    pub fn send_init2(&mut self) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_INIT2.to_be_bytes());

        self.send_reliable(payload, "send init2")
    }

    pub fn send_client_ready(&mut self) -> Result<()> {
        let full_ver = "luanti-proto-bot";
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_CLIENT_READY.to_be_bytes());
        payload.push(VERSION_MAJOR);
        payload.push(VERSION_MINOR);
        payload.push(VERSION_PATCH);
        payload.push(0u8);
        write_string(&mut payload, full_ver);

        self.send_reliable(payload, "send client_ready")
    }

    pub fn send_have_media(&mut self) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_HAVE_MEDIA.to_be_bytes());
        payload.push(0u8);

        self.send_reliable(payload, "send have_media")
    }

    pub fn send_playerpos(&mut self, state: &PlayerState) -> Result<()> {
        validate_outbound_player_state(state)?;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_PLAYERPOS.to_be_bytes());

        let scale = 100.0;
        let debug_playerpos = std::env::var("LUANTI_DEBUG_PLAYERPOS")
            .map(|v| v == "1")
            .unwrap_or(false);
        if debug_playerpos {
            let px = (state.pos.x * scale).round() as i32;
            let py = (state.pos.y * scale).round() as i32;
            let pz = (state.pos.z * scale).round() as i32;
            let sx = (state.speed.x * scale).round() as i32;
            let sy = (state.speed.y * scale).round() as i32;
            let sz = (state.speed.z * scale).round() as i32;
            let pitch = (state.pitch.to_degrees() * 100.0).round() as i32;
            let yaw = (state.yaw.to_degrees() * 100.0).round() as i32;
            println!(
                "playerpos ints pos=({}, {}, {}) speed=({}, {}, {}) pitch={} yaw={} keys={} fov={} range={} caminv={} ms={} md={}",
                px,
                py,
                pz,
                sx,
                sy,
                sz,
                pitch,
                yaw,
                state.key_pressed,
                (state.fov * 80.0).round() as u8,
                ((state.wanted_range / 16.0).ceil().min(255.0)) as u8,
                state.camera_inverted as u8,
                state.movement_speed,
                state.movement_dir
            );
        }
        write_player_state_fields(&mut payload, state);

        if debug_playerpos {
            let hex = payload
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<Vec<_>>()
                .join(" ");
            println!("playerpos bytes: {}", hex);
        }

        self.send_unreliable(payload, "send playerpos")
    }

    /// Selects the zero-based inventory slot reported as the player's wielded item.
    pub fn select_wield_index(&mut self, wield_index: u16) -> Result<()> {
        let payload = build_wield_selection_payload(wield_index);

        self.send_reliable(payload, "send playeritem")
    }

    /// Sends a node interaction using Luanti's serialized `PointedThing` format.
    pub fn send_node_interact(
        &mut self,
        action: u8,
        wield_index: u16,
        under: IVec3,
        above: IVec3,
        state: &PlayerState,
    ) -> Result<()> {
        let payload = build_node_interact_payload(action, wield_index, under, above, state)?;

        self.send_reliable(payload, "send node interact")
    }

    /// Move a positive number of items from one zero-based inventory slot into
    /// any compatible destination slot. The server applies normal reach,
    /// privilege, protection, inventory callback, and rollback rules.
    pub fn send_inventory_move_somewhere(
        &mut self,
        count: u16,
        from: InventoryLocation,
        from_list: &str,
        from_index: u16,
        to: InventoryLocation,
        to_list: &str,
    ) -> Result<()> {
        let payload = build_inventory_move_somewhere_payload(
            count,
            from,
            from_list,
            from_index,
            to,
            to_list,
        )?;

        self.send_inventory_action(payload)
    }

    /// Move items into one exact zero-based destination slot. This is used for
    /// deterministic crafting-grid placement while retaining the server's
    /// normal inventory checks and callbacks.
    pub fn send_inventory_move(
        &mut self,
        count: u16,
        from: InventoryLocation,
        from_list: &str,
        from_index: u16,
        to: InventoryLocation,
        to_list: &str,
        to_index: u16,
    ) -> Result<()> {
        let payload = build_inventory_move_payload(
            count,
            from,
            from_list,
            from_index,
            to,
            to_list,
            to_index,
        )?;
        self.send_inventory_action(payload)
    }

    /// Ask the server to execute a bounded number of recipes from the current
    /// player's craft grid. The server remains responsible for recipe
    /// resolution, input consumption, replacements, and on-craft callbacks.
    pub fn send_inventory_craft(&mut self, count: u16) -> Result<()> {
        let payload = build_inventory_craft_payload(count)?;
        self.send_inventory_action(payload)
    }

    fn send_inventory_action(&mut self, payload: Vec<u8>) -> Result<()> {
        self.send_reliable(payload, "send inventory action")
    }

    pub fn send_gotblocks(&mut self, blocks: &[BlockPos]) -> Result<()> {
        if blocks.is_empty() {
            return Ok(());
        }
        let count = blocks.len().min(u8::MAX as usize) as u8;
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_GOTBLOCKS.to_be_bytes());
        payload.push(count);
        for pos in blocks.iter().take(count as usize) {
            write_v3s16(&mut payload, *pos);
        }

        self.send_reliable(payload, "send gotblocks")
    }

    pub fn send_chat_message(&mut self, message: &str) -> Result<()> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&TOSERVER_CHAT_MESSAGE.to_be_bytes());
        write_wstring(&mut payload, message);

        self.send_reliable(payload, "send chat_message")
    }

}

fn build_node_interact_payload(
    action: u8,
    wield_index: u16,
    under: IVec3,
    above: IVec3,
    state: &PlayerState,
) -> Result<Vec<u8>> {
    validate_outbound_player_state(state)?;
    let under = checked_block_pos(under, "under")?;
    let above = checked_block_pos(above, "above")?;

    let mut pointed = Vec::with_capacity(14);
    pointed.push(0); // PointedThing serialization version
    pointed.push(1); // POINTEDTHING_NODE
    write_v3s16(&mut pointed, under);
    write_v3s16(&mut pointed, above);

    let mut payload = Vec::with_capacity(2 + 1 + 2 + 4 + pointed.len() + 47);
    payload.extend_from_slice(&TOSERVER_INTERACT.to_be_bytes());
    payload.push(action);
    payload.extend_from_slice(&wield_index.to_be_bytes());
    payload.extend_from_slice(&(pointed.len() as u32).to_be_bytes());
    payload.extend_from_slice(&pointed);
    write_player_state_fields(&mut payload, state);
    Ok(payload)
}

fn build_wield_selection_payload(wield_index: u16) -> Vec<u8> {
    let mut payload = Vec::with_capacity(4);
    payload.extend_from_slice(&TOSERVER_PLAYERITEM.to_be_bytes());
    payload.extend_from_slice(&wield_index.to_be_bytes());
    payload
}

fn build_inventory_move_somewhere_payload(
    count: u16,
    from: InventoryLocation,
    from_list: &str,
    from_index: u16,
    to: InventoryLocation,
    to_list: &str,
) -> Result<Vec<u8>> {
    if count == 0 {
        bail!("inventory move count must be positive");
    }
    validate_inventory_list_name(from_list)?;
    validate_inventory_list_name(to_list)?;
    let from = serialize_inventory_location(from)?;
    let to = serialize_inventory_location(to)?;
    let action = format!(
        "MoveSomewhere {count} {from} {from_list} {from_index} {to} {to_list}"
    );
    let mut payload = Vec::with_capacity(2 + action.len());
    payload.extend_from_slice(&TOSERVER_INVENTORY_ACTION.to_be_bytes());
    payload.extend_from_slice(action.as_bytes());
    Ok(payload)
}

fn build_inventory_move_payload(
    count: u16,
    from: InventoryLocation,
    from_list: &str,
    from_index: u16,
    to: InventoryLocation,
    to_list: &str,
    to_index: u16,
) -> Result<Vec<u8>> {
    if count == 0 {
        bail!("inventory move count must be positive");
    }
    validate_inventory_list_name(from_list)?;
    validate_inventory_list_name(to_list)?;
    let from = serialize_inventory_location(from)?;
    let to = serialize_inventory_location(to)?;
    let action = format!(
        "Move {count} {from} {from_list} {from_index} {to} {to_list} {to_index}"
    );
    let mut payload = Vec::with_capacity(2 + action.len());
    payload.extend_from_slice(&TOSERVER_INVENTORY_ACTION.to_be_bytes());
    payload.extend_from_slice(action.as_bytes());
    Ok(payload)
}

fn build_inventory_craft_payload(count: u16) -> Result<Vec<u8>> {
    if count == 0 {
        bail!("inventory craft count must be positive");
    }
    let action = format!("Craft {count} current_player");
    let mut payload = Vec::with_capacity(2 + action.len());
    payload.extend_from_slice(&TOSERVER_INVENTORY_ACTION.to_be_bytes());
    payload.extend_from_slice(action.as_bytes());
    Ok(payload)
}

fn serialize_inventory_location(location: InventoryLocation) -> Result<String> {
    match location {
        InventoryLocation::CurrentPlayer => Ok("current_player".to_string()),
        InventoryLocation::NodeMeta(pos) => {
            let pos = checked_block_pos(pos, "inventory")?;
            Ok(format!("nodemeta:{},{},{}", pos.x, pos.y, pos.z))
        }
    }
}

fn validate_inventory_list_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("invalid inventory list name '{name}'");
    }
    Ok(())
}

fn checked_block_pos(pos: IVec3, label: &str) -> Result<BlockPos> {
    let Ok(x) = i16::try_from(pos.x) else {
        bail!("{label} node x coordinate {} does not fit i16", pos.x);
    };
    let Ok(y) = i16::try_from(pos.y) else {
        bail!("{label} node y coordinate {} does not fit i16", pos.y);
    };
    let Ok(z) = i16::try_from(pos.z) else {
        bail!("{label} node z coordinate {} does not fit i16", pos.z);
    };
    Ok(BlockPos { x, y, z })
}

fn write_player_state_fields(buf: &mut Vec<u8>, state: &PlayerState) {
    const SCALE: f32 = 100.0;
    write_v3s32(buf, state.pos, SCALE);
    write_v3s32(buf, state.speed, SCALE);
    write_s32(buf, state.pitch.to_degrees() * 100.0);
    write_s32(buf, state.yaw.to_degrees() * 100.0);
    write_u32(buf, state.key_pressed);
    let fov_scaled = (state.fov * 80.0).round().clamp(0.0, 255.0) as u8;
    buf.push(fov_scaled);
    let wanted = (state.wanted_range / 16.0).ceil().clamp(1.0, 255.0) as u8;
    buf.push(wanted);
    buf.push(u8::from(state.camera_inverted));
    write_f32(buf, state.movement_speed);
    write_f32(buf, state.movement_dir);
}

fn validate_outbound_player_state(state: &PlayerState) -> Result<()> {
    const MAX_POSITION_BS: f32 = 500_000.0;
    const MAX_SPEED_BS: f32 = 10_000.0;
    let position_valid = [state.pos.x, state.pos.y, state.pos.z]
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_POSITION_BS);
    let speed_valid = [state.speed.x, state.speed.y, state.speed.z]
        .into_iter()
        .all(|value| value.is_finite() && value.abs() <= MAX_SPEED_BS);
    let scalar_valid = [
        state.pitch,
        state.yaw,
        state.movement_speed,
        state.movement_dir,
        state.fov,
        state.wanted_range,
    ]
    .into_iter()
    .all(f32::is_finite);
    if !position_valid || !speed_valid || !scalar_valid {
        bail!(
            "refusing unsafe player state: pos=({:.3},{:.3},{:.3}) speed=({:.3},{:.3},{:.3}) pitch={:.3} yaw={:.3} movement_speed={:.3} movement_dir={:.3} fov={:.3} wanted_range={:.3}",
            state.pos.x,
            state.pos.y,
            state.pos.z,
            state.speed.x,
            state.speed.y,
            state.speed.z,
            state.pitch,
            state.yaw,
            state.movement_speed,
            state.movement_dir,
            state.fov,
            state.wanted_range
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Vec3;

    #[test]
    fn outbound_state_rejects_million_scale_coordinates() {
        let state = PlayerState {
            pos: Vec3 {
                x: 1_200_000.0,
                y: 250.0,
                z: 3500.0,
            },
            ..PlayerState::default()
        };
        assert!(validate_outbound_player_state(&state).is_err());
    }

    #[test]
    fn node_interact_serializes_pointed_node_and_player_state() {
        let state = PlayerState {
            pos: Vec3 {
                x: 1.25,
                y: -2.0,
                z: 3.0,
            },
            speed: Vec3 {
                x: 0.5,
                y: 0.0,
                z: -0.25,
            },
            pitch: 10.0_f32.to_radians(),
            yaw: -90.0_f32.to_radians(),
            movement_speed: 4.0,
            movement_dir: 0.5,
            key_pressed: 0x0102_0304,
            fov: 1.2,
            wanted_range: 128.0,
            camera_inverted: true,
        };
        let under = IVec3 {
            x: 10,
            y: -5,
            z: 300,
        };
        let above = IVec3 {
            x: 10,
            y: -4,
            z: 300,
        };

        let payload = build_node_interact_payload(0, 2, under, above, &state).unwrap();
        let expected_header_and_pointed = [
            0x00, 0x39, // TOSERVER_INTERACT
            0x00, // INTERACT_START_DIGGING
            0x00, 0x02, // zero-based wield index
            0x00, 0x00, 0x00, 0x0e, // long-string length
            0x00, 0x01, // PointedThing version, POINTEDTHING_NODE
            0x00, 0x0a, 0xff, 0xfb, 0x01, 0x2c, // under
            0x00, 0x0a, 0xff, 0xfc, 0x01, 0x2c, // above
        ];
        assert_eq!(
            &payload[..expected_header_and_pointed.len()],
            expected_header_and_pointed
        );

        let mut player_fields = Vec::new();
        write_player_state_fields(&mut player_fields, &state);
        assert_eq!(&payload[expected_header_and_pointed.len()..], player_fields);
        assert_eq!(player_fields.len(), 47);
    }

    #[test]
    fn node_interact_rejects_coordinates_outside_v3s16() {
        let result = build_node_interact_payload(
            0,
            0,
            IVec3 {
                x: i16::MAX as i32 + 1,
                y: 0,
                z: 0,
            },
            IVec3 { x: 0, y: 0, z: 0 },
            &PlayerState::default(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn wield_selection_serializes_zero_based_index() {
        assert_eq!(
            build_wield_selection_payload(7),
            [0x00, 0x37, 0x00, 0x07]
        );
    }

    #[test]
    fn inventory_move_somewhere_serializes_native_action() {
        let payload = build_inventory_move_somewhere_payload(
            12,
            InventoryLocation::CurrentPlayer,
            "main",
            4,
            InventoryLocation::NodeMeta(IVec3 { x: 10, y: 20, z: -3 }),
            "main",
        )
        .unwrap();
        assert_eq!(&payload[..2], &TOSERVER_INVENTORY_ACTION.to_be_bytes());
        assert_eq!(
            std::str::from_utf8(&payload[2..]).unwrap(),
            "MoveSomewhere 12 current_player main 4 nodemeta:10,20,-3 main"
        );
    }

    #[test]
    fn furnace_inventory_moves_use_native_src_fuel_and_dst_lists() {
        let furnace = InventoryLocation::NodeMeta(IVec3 { x: 4, y: 5, z: 6 });
        for list in ["src", "fuel"] {
            let payload = build_inventory_move_somewhere_payload(
                2,
                InventoryLocation::CurrentPlayer,
                "main",
                3,
                furnace,
                list,
            )
            .unwrap();
            assert_eq!(
                std::str::from_utf8(&payload[2..]).unwrap(),
                format!("MoveSomewhere 2 current_player main 3 nodemeta:4,5,6 {list}")
            );
        }
        let payload = build_inventory_move_somewhere_payload(
            1,
            furnace,
            "dst",
            0,
            InventoryLocation::CurrentPlayer,
            "main",
        )
        .unwrap();
        assert_eq!(
            std::str::from_utf8(&payload[2..]).unwrap(),
            "MoveSomewhere 1 nodemeta:4,5,6 dst 0 current_player main"
        );
    }

    #[test]
    fn inventory_move_serializes_exact_destination_slot() {
        let payload = build_inventory_move_payload(
            3,
            InventoryLocation::CurrentPlayer,
            "main",
            5,
            InventoryLocation::CurrentPlayer,
            "craft",
            2,
        )
        .unwrap();
        assert_eq!(&payload[..2], &TOSERVER_INVENTORY_ACTION.to_be_bytes());
        assert_eq!(
            std::str::from_utf8(&payload[2..]).unwrap(),
            "Move 3 current_player main 5 current_player craft 2"
        );
    }

    #[test]
    fn inventory_craft_serializes_bounded_native_action() {
        let payload = build_inventory_craft_payload(2).unwrap();
        assert_eq!(&payload[..2], &TOSERVER_INVENTORY_ACTION.to_be_bytes());
        assert_eq!(
            std::str::from_utf8(&payload[2..]).unwrap(),
            "Craft 2 current_player"
        );
        assert!(build_inventory_craft_payload(0).is_err());
    }

    #[test]
    fn inventory_move_rejects_zero_count_and_unsafe_lists() {
        assert!(build_inventory_move_somewhere_payload(
            0,
            InventoryLocation::CurrentPlayer,
            "main",
            0,
            InventoryLocation::CurrentPlayer,
            "main",
        )
        .is_err());
        assert!(build_inventory_move_somewhere_payload(
            1,
            InventoryLocation::CurrentPlayer,
            "main injected",
            0,
            InventoryLocation::CurrentPlayer,
            "main",
        )
        .is_err());
    }
}
