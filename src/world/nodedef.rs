use anyhow::{bail, Context, Result};

use crate::codec::{hex_bytes, ByteReader};
use crate::types::Vec3;

#[derive(Clone, Copy, Debug, Default)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeBoxType {
    Regular = 0,
    Fixed = 1,
    WallMounted = 2,
    Leveled = 3,
    Connected = 4,
}

impl Default for NodeBoxType {
    fn default() -> Self {
        NodeBoxType::Regular
    }
}

#[derive(Clone, Debug, Default)]
pub struct NodeBox {
    pub box_type: NodeBoxType,
    pub fixed: Vec<Aabb>,
    pub wall_top: Aabb,
    pub wall_bottom: Aabb,
    pub wall_side: Aabb,
    pub connected: ConnectedNodeBox,
}

#[derive(Clone, Debug, Default)]
pub struct ConnectedNodeBox {
    pub connect_top: Vec<Aabb>,
    pub connect_bottom: Vec<Aabb>,
    pub connect_front: Vec<Aabb>,
    pub connect_left: Vec<Aabb>,
    pub connect_back: Vec<Aabb>,
    pub connect_right: Vec<Aabb>,
    pub disconnected_top: Vec<Aabb>,
    pub disconnected_bottom: Vec<Aabb>,
    pub disconnected_front: Vec<Aabb>,
    pub disconnected_left: Vec<Aabb>,
    pub disconnected_back: Vec<Aabb>,
    pub disconnected_right: Vec<Aabb>,
    pub disconnected: Vec<Aabb>,
    pub disconnected_sides: Vec<Aabb>,
}

#[derive(Clone, Debug, Default)]
pub struct ContentFeatures {
    pub walkable: bool,
    pub collision_box: NodeBox,
}

#[derive(Clone, Debug, Default)]
pub struct NodeDefManager {
    pub features: Vec<ContentFeatures>,
    names: Vec<String>,
}

impl NodeDefManager {
    pub fn get(&self, id: u16) -> Option<&ContentFeatures> {
        self.features.get(id as usize)
    }

    pub fn name(&self, id: u16) -> Option<&str> {
        self.names
            .get(id as usize)
            .map(String::as_str)
            .filter(|name| !name.is_empty())
    }
}

pub fn parse_nodedef_zstd(data: &[u8], protocol_version: u16) -> Result<NodeDefManager> {
    let decompressed =
        zstd::stream::decode_all(std::io::Cursor::new(data)).context("zstd decompress nodedef")?;
    let mut reader = ByteReader::new(&decompressed);
    let version = reader.read_u8()?;
    if version < 1 {
        bail!("unsupported NodeDefManager version: {version}");
    }
    let count = reader.read_u16()?;
    let content_bytes = reader.read_string32()?;
    let mut inner = ByteReader::new(&content_bytes);

    let mut manager = NodeDefManager::default();
    for _ in 0..count {
        let id = inner.read_u16()?;
        let wrapper = inner.read_bytes16()?;
        let mut feat_reader = ByteReader::new(&wrapper);
        let (name, features) = parse_content_features(&mut feat_reader, protocol_version)?;
        if id as usize >= manager.features.len() {
            manager
                .features
                .resize(id as usize + 1, ContentFeatures::default());
            manager.names.resize(id as usize + 1, String::new());
        }
        manager.features[id as usize] = features;
        manager.names[id as usize] = name;
    }

    Ok(manager)
}

fn parse_content_features(
    reader: &mut ByteReader,
    protocol_version: u16,
) -> Result<(String, ContentFeatures)> {
    let version = reader.read_u8()?;
    if version < 13 {
        bail!("unsupported ContentFeatures version: {version}");
    }

    let name = reader.read_string16()?;
    let groups_size = reader.read_u16()? as usize;
    for _ in 0..groups_size {
        let _name = reader.read_string16()?;
        let _value = reader.read_s16()?;
    }

    let _param_type = reader.read_u8()?;
    let _param_type_2 = reader.read_u8()?;
    let _drawtype = reader.read_u8()?;

    let _mesh = reader.read_string16()?;
    let _visual_scale = reader.read_f32()?;
    let tile_pos = reader.position();
    let tile_count = reader.read_u8()? as usize;
    if tile_count != 6 {
        let context = reader.peek_bytes_at(tile_pos.saturating_sub(8), 16);
        println!(
            "nodedef tile_count={} at offset={} context={}",
            tile_count,
            tile_pos,
            hex_bytes(&context)
        );
        bail!("unsupported tile count: {tile_count}");
    }
    for _ in 0..6 {
        skip_tiledef(reader, protocol_version)?;
    }
    for _ in 0..6 {
        skip_tiledef(reader, protocol_version)?;
    }
    let special_count = reader.read_u8()?;
    for _ in 0..special_count {
        skip_tiledef(reader, protocol_version)?;
    }
    let _legacy_alpha = reader.read_u8()?;
    let _color_r = reader.read_u8()?;
    let _color_g = reader.read_u8()?;
    let _color_b = reader.read_u8()?;
    let _palette = reader.read_string16()?;
    let _waving = reader.read_u8()?;
    let _connect_sides = reader.read_u8()?;
    let connects_to_size = reader.read_u16()? as usize;
    for _ in 0..connects_to_size {
        let _connected_id = reader.read_u16()?;
    }
    skip_argb8(reader)?;
    let _leveled = reader.read_u8()?;

    let _light_propagates = reader.read_u8()?;
    let _sunlight_propagates = reader.read_u8()?;
    let _light_source = reader.read_u8()?;
    let _is_ground_content = reader.read_u8()?;

    let walkable = reader.read_u8()? != 0;
    let _pointable = reader.read_u8()?;
    let _diggable = reader.read_u8()? != 0;
    let _climbable = reader.read_u8()? != 0;
    let _buildable_to = reader.read_u8()? != 0;
    let _rightclickable = reader.read_u8()? != 0;
    let _damage_per_second = reader.read_u32()?;

    let _liquid_type = reader.read_u8()?;
    let _liquid_alternative_flowing = reader.read_string16()?;
    let _liquid_alternative_source = reader.read_string16()?;
    let _liquid_viscosity = reader.read_u8()?;
    let _liquid_renewable = reader.read_u8()?;
    let _liquid_range = reader.read_u8()?;
    let _drowning = reader.read_u8()?;
    let _floodable = reader.read_u8()?;

    let _node_box = read_nodebox(reader)?;
    let _selection_box = read_nodebox(reader)?;
    let collision_box = read_nodebox(reader)?;

    skip_sound(reader)?;
    skip_sound(reader)?;
    skip_sound(reader)?;

    let _legacy_facedir = reader.read_u8()?;
    let _legacy_wallmounted = reader.read_u8()?;

    let _node_dig_prediction = reader.read_string16()?;
    if reader.remaining() > 0 {
        let _leveled_max = reader.read_u8()?;
    }
    if reader.remaining() > 0 {
        let _alpha = reader.read_u8()?;
        let _move_resistance = reader.read_u8()?;
        let _liquid_move_physics = reader.read_u8()? != 0;
        if reader.remaining() > 0 {
            let _post_effect_color_shaded = reader.read_u8()?;
        }
    }

    Ok((name, ContentFeatures { walkable, collision_box }))
}

fn skip_argb8(reader: &mut ByteReader) -> Result<()> {
    for _ in 0..4 {
        let _component = reader.read_u8()?;
    }
    Ok(())
}

fn skip_tiledef(reader: &mut ByteReader, _protocol_version: u16) -> Result<()> {
    let version = reader.read_u8()?;
    if version < 6 {
        bail!("unsupported TileDef version: {version}");
    }
    let _name = reader.read_string16()?;
    skip_tile_animation(reader)?;
    let flags = reader.read_u16()?;
    if flags & (1 << 3) != 0 {
        let _r = reader.read_u8()?;
        let _g = reader.read_u8()?;
        let _b = reader.read_u8()?;
    }
    if flags & (1 << 4) != 0 {
        let _scale = reader.read_u8()?;
    }
    if flags & (1 << 5) != 0 {
        let _align_style = reader.read_u8()?;
    }
    Ok(())
}

fn skip_tile_animation(reader: &mut ByteReader) -> Result<()> {
    let anim_type = reader.read_u8()?;
    match anim_type {
        0 => {}
        1 => {
            let _aspect_w = reader.read_u16()?;
            let _aspect_h = reader.read_u16()?;
            let _length = reader.read_f32()?;
        }
        2 => {
            let _frames_w = reader.read_u8()?;
            let _frames_h = reader.read_u8()?;
            let _frame_length = reader.read_f32()?;
        }
        _ => {}
    }
    Ok(())
}

fn skip_sound(reader: &mut ByteReader) -> Result<()> {
    let _name = reader.read_string16()?;
    let _gain = reader.read_f32()?;
    let _pitch = reader.read_f32()?;
    let _fade = reader.read_f32()?;
    Ok(())
}

fn read_nodebox(reader: &mut ByteReader) -> Result<NodeBox> {
    let version = reader.read_u8()?;
    if version < 6 {
        bail!("unsupported NodeBox version: {version}");
    }
    let nodebox_type = match reader.read_u8()? {
        1 => NodeBoxType::Fixed,
        2 => NodeBoxType::WallMounted,
        3 => NodeBoxType::Leveled,
        4 => NodeBoxType::Connected,
        _ => NodeBoxType::Regular,
    };

    let mut nodebox = NodeBox {
        box_type: nodebox_type,
        ..Default::default()
    };

    match nodebox_type {
        NodeBoxType::Fixed | NodeBoxType::Leveled => {
            let count = reader.read_u16()? as usize;
            for _ in 0..count {
                nodebox.fixed.push(read_aabb(reader)?);
            }
        }
        NodeBoxType::WallMounted => {
            nodebox.wall_top = read_aabb(reader)?;
            nodebox.wall_bottom = read_aabb(reader)?;
            nodebox.wall_side = read_aabb(reader)?;
        }
        NodeBoxType::Connected => {
            nodebox.fixed = read_aabb_vec(reader)?;
            nodebox.connected.connect_top = read_aabb_vec(reader)?;
            nodebox.connected.connect_bottom = read_aabb_vec(reader)?;
            nodebox.connected.connect_front = read_aabb_vec(reader)?;
            nodebox.connected.connect_left = read_aabb_vec(reader)?;
            nodebox.connected.connect_back = read_aabb_vec(reader)?;
            nodebox.connected.connect_right = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_top = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_bottom = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_front = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_left = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_back = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_right = read_aabb_vec(reader)?;
            nodebox.connected.disconnected = read_aabb_vec(reader)?;
            nodebox.connected.disconnected_sides = read_aabb_vec(reader)?;
        }
        NodeBoxType::Regular => {}
    }

    Ok(nodebox)
}

fn read_aabb(reader: &mut ByteReader) -> Result<Aabb> {
    let min = reader.read_vec3_f32()?;
    let max = reader.read_vec3_f32()?;
    Ok(Aabb { min, max })
}

fn read_aabb_vec(reader: &mut ByteReader) -> Result<Vec<Aabb>> {
    let count = reader.read_u16()? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_aabb(reader)?);
    }
    Ok(out)
}
