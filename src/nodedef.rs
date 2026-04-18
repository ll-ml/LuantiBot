use anyhow::{bail, Context, Result};

use crate::codec::ByteReader;
use crate::mtp::Vec3;

#[derive(Clone, Copy, Debug, Default)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn add_point(&mut self, p: Vec3) {
        self.min.x = self.min.x.min(p.x);
        self.min.y = self.min.y.min(p.y);
        self.min.z = self.min.z.min(p.z);
        self.max.x = self.max.x.max(p.x);
        self.max.y = self.max.y.max(p.y);
        self.max.z = self.max.z.max(p.z);
    }

    pub fn add_box(&mut self, other: Aabb) {
        self.add_point(other.min);
        self.add_point(other.max);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParamType2 {
    None = 0,
    Facedir = 2,
    WallMounted = 3,
    Leveled = 8,
    FourDir = 9,
    DegRotate = 10,
    FlowingLiquid = 11,
    ColoredFacedir = 12,
    ColoredWallMounted = 13,
    ColoredFourDir = 14,
    ColoredDegRotate = 15,
}

impl Default for ParamType2 {
    fn default() -> Self {
        ParamType2::None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeDrawType {
    Normal = 0,
    Airlike = 1,
    Liquid = 2,
    FlowingLiquid = 3,
    Glasslike = 4,
    Allfaces = 5,
    AllfacesOptional = 6,
    Torchlike = 7,
    Signlike = 8,
    Plantlike = 9,
    Firelike = 10,
    Fencelike = 11,
    Raillike = 12,
    Nodebox = 13,
    Mesh = 14,
    GlasslikeFramed = 15,
    GlasslikeFramedOptional = 16,
    AllfacesOptionalVertical = 17,
    PlantlikeRooted = 18,
}

impl Default for NodeDrawType {
    fn default() -> Self {
        NodeDrawType::Normal
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiquidType {
    None = 0,
    Source = 1,
    Flowing = 2,
}

impl Default for LiquidType {
    fn default() -> Self {
        LiquidType::None
    }
}

#[derive(Clone, Debug, Default)]
pub struct ContentFeatures {
    pub walkable: bool,
    pub climbable: bool,
    pub liquid_move_physics: bool,
    pub move_resistance: u8,
    pub node_box: NodeBox,
    pub collision_box: NodeBox,
    pub selection_box: NodeBox,
    pub param_type_2: ParamType2,
    pub drawtype: NodeDrawType,
    pub leveled: u8,
    pub leveled_max: u8,
    pub liquid_type: LiquidType,
    pub liquid_alternative_flowing_id: u16,
    pub liquid_alternative_source_id: u16,
    pub connect_sides: u8,
    pub connects_to_ids: Vec<u16>,
    pub groups: Vec<(String, i16)>,
}

#[derive(Clone, Debug, Default)]
pub struct NodeDefManager {
    pub features: Vec<ContentFeatures>,
    pub name_to_id: Vec<(String, u16)>,
}

impl NodeDefManager {
    pub fn get(&self, id: u16) -> Option<&ContentFeatures> {
        self.features.get(id as usize)
    }

    pub fn resolve_crossrefs(&mut self) {
        let mut map = std::collections::HashMap::new();
        for (name, id) in &self.name_to_id {
            map.insert(name.as_str(), *id);
        }
        for f in &mut self.features {
            if f.liquid_type != LiquidType::None {
                if f.liquid_alternative_flowing_id == 0 {
                    if let Some(id) = map.get("air") {
                        f.liquid_alternative_flowing_id = *id;
                    }
                }
                if f.liquid_alternative_source_id == 0 {
                    if let Some(id) = map.get("air") {
                        f.liquid_alternative_source_id = *id;
                    }
                }
            }
        }
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
        }
        manager.features[id as usize] = features;
        manager.name_to_id.push((name, id));
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
    let mut groups = Vec::with_capacity(groups_size);
    for _ in 0..groups_size {
        let name = reader.read_string16()?;
        let val = reader.read_s16()?;
        groups.push((name, val));
    }

    let _param_type = reader.read_u8()?;
    let param_type_2 = match reader.read_u8()? {
        2 => ParamType2::Facedir,
        3 => ParamType2::WallMounted,
        8 => ParamType2::Leveled,
        9 => ParamType2::FourDir,
        10 => ParamType2::DegRotate,
        11 => ParamType2::FlowingLiquid,
        12 => ParamType2::ColoredFacedir,
        13 => ParamType2::ColoredWallMounted,
        14 => ParamType2::ColoredFourDir,
        15 => ParamType2::ColoredDegRotate,
        _ => ParamType2::None,
    };

    let drawtype = match reader.read_u8()? {
        1 => NodeDrawType::Airlike,
        2 => NodeDrawType::Liquid,
        3 => NodeDrawType::FlowingLiquid,
        4 => NodeDrawType::Glasslike,
        5 => NodeDrawType::Allfaces,
        6 => NodeDrawType::AllfacesOptional,
        7 => NodeDrawType::Torchlike,
        8 => NodeDrawType::Signlike,
        9 => NodeDrawType::Plantlike,
        10 => NodeDrawType::Firelike,
        11 => NodeDrawType::Fencelike,
        12 => NodeDrawType::Raillike,
        13 => NodeDrawType::Nodebox,
        14 => NodeDrawType::Mesh,
        15 => NodeDrawType::GlasslikeFramed,
        16 => NodeDrawType::GlasslikeFramedOptional,
        17 => NodeDrawType::AllfacesOptionalVertical,
        18 => NodeDrawType::PlantlikeRooted,
        _ => NodeDrawType::Normal,
    };

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
    let connect_sides = reader.read_u8()?;
    let connects_to_size = reader.read_u16()? as usize;
    let mut connects_to_ids = Vec::with_capacity(connects_to_size);
    for _ in 0..connects_to_size {
        connects_to_ids.push(reader.read_u16()?);
    }
    let _post_effect_color = read_argb8(reader)?;
    let leveled = reader.read_u8()?;

    let _light_propagates = reader.read_u8()?;
    let _sunlight_propagates = reader.read_u8()?;
    let _light_source = reader.read_u8()?;
    let _is_ground_content = reader.read_u8()?;

    let walkable = reader.read_u8()? != 0;
    let _pointable = reader.read_u8()?;
    let _diggable = reader.read_u8()? != 0;
    let climbable = reader.read_u8()? != 0;
    let _buildable_to = reader.read_u8()? != 0;
    let _rightclickable = reader.read_u8()? != 0;
    let _damage_per_second = reader.read_u32()?;

    let liquid_type = match reader.read_u8()? {
        1 => LiquidType::Source,
        2 => LiquidType::Flowing,
        _ => LiquidType::None,
    };
    let _liquid_alternative_flowing = reader.read_string16()?;
    let _liquid_alternative_source = reader.read_string16()?;
    let liquid_viscosity = reader.read_u8()?;
    let _liquid_renewable = reader.read_u8()?;
    let _liquid_range = reader.read_u8()?;
    let _drowning = reader.read_u8()?;
    let _floodable = reader.read_u8()?;

    let node_box = read_nodebox(reader)?;
    let selection_box = read_nodebox(reader)?;
    let collision_box = read_nodebox(reader)?;

    skip_sound(reader)?;
    skip_sound(reader)?;
    skip_sound(reader)?;

    let _legacy_facedir = reader.read_u8()?;
    let _legacy_wallmounted = reader.read_u8()?;

    let _node_dig_prediction = reader.read_string16()?;
    let leveled_max = if reader.remaining() > 0 {
        reader.read_u8()?
    } else {
        0
    };
    if reader.remaining() > 0 {
        let _alpha = reader.read_u8()?;
        let move_resistance = reader.read_u8()?;
        let liquid_move_physics = reader.read_u8()? != 0;
        if reader.remaining() > 0 {
            let _post_effect_color_shaded = reader.read_u8()?;
        }
        return Ok((
            name,
            ContentFeatures {
                walkable,
                climbable,
                liquid_move_physics: liquid_type != LiquidType::None || liquid_move_physics,
                move_resistance,
                node_box,
                collision_box,
                selection_box,
                param_type_2,
                drawtype,
                leveled,
                leveled_max,
                liquid_type,
                liquid_alternative_flowing_id: 0,
                liquid_alternative_source_id: 0,
                connect_sides,
                connects_to_ids,
                groups,
            },
        ));
    }

    Ok((
        name,
        ContentFeatures {
            walkable,
            climbable,
            liquid_move_physics: liquid_type != LiquidType::None,
            move_resistance: liquid_viscosity,
            node_box,
            collision_box,
            selection_box,
            param_type_2,
            drawtype,
            leveled,
            leveled_max,
            liquid_type,
            liquid_alternative_flowing_id: 0,
            liquid_alternative_source_id: 0,
            connect_sides,
            connects_to_ids,
            groups,
        },
    ))
}

fn read_argb8(reader: &mut ByteReader) -> Result<Color> {
    let a = reader.read_u8()?;
    let r = reader.read_u8()?;
    let g = reader.read_u8()?;
    let b = reader.read_u8()?;
    Ok(Color { r, g, b, a })
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
    let min = read_v3f32(reader)?;
    let max = read_v3f32(reader)?;
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

fn read_v3f32(reader: &mut ByteReader) -> Result<Vec3> {
    Ok(Vec3 {
        x: reader.read_f32()?,
        y: reader.read_f32()?,
        z: reader.read_f32()?,
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (idx, b) in bytes.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{:02x}", b));
    }
    out
}
