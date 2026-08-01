use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::io::{Cursor, Read};

use crate::nodedef::{Aabb, NodeBoxType, NodeDefManager};
use crate::types::{BlockPos, IVec3};

const MAP_BLOCKSIZE: i32 = 16;
const NODECOUNT: usize = 16 * 16 * 16;
const AIR_CONTENT_ID: u16 = 0;
const IGNORE_CONTENT_ID: u16 = 127;

#[derive(Clone, Copy, Debug, Default)]
pub struct MapNode {
    pub content: u16,
    pub param1: u8,
    pub param2: u8,
}

pub struct World {
    blocks: HashMap<BlockPos, MapBlock>,
    nodedef: Option<NodeDefManager>,
}

impl World {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            nodedef: None,
        }
    }

    pub fn set_nodedef(&mut self, manager: NodeDefManager) {
        self.nodedef = Some(manager);
    }

    pub fn ingest_block(&mut self, pos: BlockPos, data: &[u8], version: u8) -> Result<()> {
        let block = MapBlock::from_serialized(data, version)
            .with_context(|| format!("parse block {:?}", pos))?;
        self.blocks.insert(pos, block);
        Ok(())
    }

    pub fn get_node(&self, node_pos: IVec3) -> Option<MapNode> {
        let block_pos = BlockPos {
            x: div_floor(node_pos.x, MAP_BLOCKSIZE) as i16,
            y: div_floor(node_pos.y, MAP_BLOCKSIZE) as i16,
            z: div_floor(node_pos.z, MAP_BLOCKSIZE) as i16,
        };
        let local = IVec3 {
            x: mod_floor(node_pos.x, MAP_BLOCKSIZE),
            y: mod_floor(node_pos.y, MAP_BLOCKSIZE),
            z: mod_floor(node_pos.z, MAP_BLOCKSIZE),
        };
        let block = self.blocks.get(&block_pos)?;
        let idx =
            (local.z * MAP_BLOCKSIZE * MAP_BLOCKSIZE + local.y * MAP_BLOCKSIZE + local.x) as usize;
        block.nodes.get(idx).copied()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn is_air_or_ignore(&self, node: MapNode) -> bool {
        node.content == AIR_CONTENT_ID || node.content == IGNORE_CONTENT_ID
    }

    pub fn node_name(&self, node: MapNode) -> String {
        if node.content == AIR_CONTENT_ID {
            return "air".to_string();
        }
        if node.content == IGNORE_CONTENT_ID {
            return "ignore".to_string();
        }
        if let Some(manager) = self.nodedef.as_ref() {
            for (name, id) in &manager.name_to_id {
                if *id == node.content {
                    return name.clone();
                }
            }
        }
        format!("content:{}", node.content)
    }

    /// Returns `None` when the containing mapblock is not loaded. Loaded air and
    /// non-walkable nodes return an empty vector.
    pub fn collision_boxes(&self, node_pos: IVec3) -> Option<Vec<Aabb>> {
        let node = match self.get_node(node_pos) {
            Some(node) => node,
            None => return None,
        };
        if node.content == IGNORE_CONTENT_ID {
            return None;
        }
        if node.content == AIR_CONTENT_ID {
            return Some(Vec::new());
        }
        if let Some(manager) = self.nodedef.as_ref() {
            if let Some(features) = manager.get(node.content) {
                if !features.walkable {
                    return Some(Vec::new());
                }
                let collision = &features.collision_box;
                match collision.box_type {
                    NodeBoxType::Regular => {
                        return Some(vec![Aabb {
                            min: crate::mtp::Vec3 {
                                x: -0.5,
                                y: -0.5,
                                z: -0.5,
                            },
                            max: crate::mtp::Vec3 {
                                x: 0.5,
                                y: 0.5,
                                z: 0.5,
                            },
                        }]);
                    }
                    NodeBoxType::Fixed | NodeBoxType::Leveled => {
                        if !collision.fixed.is_empty() {
                            return Some(collision.fixed.clone());
                        }
                    }
                    NodeBoxType::WallMounted => {
                        let mounted = node.param2 & 0x07;
                        let box_ = match mounted {
                            0 | 6 => collision.wall_top,
                            1 | 7 => collision.wall_bottom,
                            _ => collision.wall_side,
                        };
                        return Some(vec![box_]);
                    }
                    NodeBoxType::Connected => {
                        if !collision.fixed.is_empty() {
                            return Some(collision.fixed.clone());
                        }
                        if !collision.connected.disconnected.is_empty() {
                            return Some(collision.connected.disconnected.clone());
                        }
                    }
                }
            }
        }
        Some(vec![Aabb {
            min: crate::mtp::Vec3 {
                x: -0.5,
                y: -0.5,
                z: -0.5,
            },
            max: crate::mtp::Vec3 {
                x: 0.5,
                y: 0.5,
                z: 0.5,
            },
        }])
    }

    #[cfg(test)]
    pub(crate) fn insert_test_node(&mut self, node_pos: IVec3, walkable: bool) {
        const TEST_CONTENT_ID: u16 = 1;
        let block_pos = BlockPos {
            x: div_floor(node_pos.x, MAP_BLOCKSIZE) as i16,
            y: div_floor(node_pos.y, MAP_BLOCKSIZE) as i16,
            z: div_floor(node_pos.z, MAP_BLOCKSIZE) as i16,
        };
        let local = IVec3 {
            x: mod_floor(node_pos.x, MAP_BLOCKSIZE),
            y: mod_floor(node_pos.y, MAP_BLOCKSIZE),
            z: mod_floor(node_pos.z, MAP_BLOCKSIZE),
        };
        let idx =
            (local.z * MAP_BLOCKSIZE * MAP_BLOCKSIZE + local.y * MAP_BLOCKSIZE + local.x) as usize;
        let block = self.blocks.entry(block_pos).or_insert_with(|| MapBlock {
            nodes: vec![MapNode::default(); NODECOUNT],
        });
        block.nodes[idx] = MapNode {
            content: TEST_CONTENT_ID,
            ..MapNode::default()
        };

        let manager = self.nodedef.get_or_insert_with(NodeDefManager::default);
        if manager.features.len() <= TEST_CONTENT_ID as usize {
            manager.features.resize(
                TEST_CONTENT_ID as usize + 1,
                crate::nodedef::ContentFeatures::default(),
            );
        }
        manager.features[TEST_CONTENT_ID as usize].walkable = walkable;
    }
}

struct MapBlock {
    nodes: Vec<MapNode>,
}

struct ParsedHeader {
    offset: usize,
    content_width: u8,
    params_width: u8,
    layout: &'static str,
}

impl MapBlock {
    fn from_serialized(data: &[u8], version: u8) -> Result<Self> {
        if data.is_empty() {
            bail!("empty mapblock payload");
        }

        let decompressed = decode_zstd_frame(data)?;
        let decompressed_len = decompressed.len();
        let header = match parse_header(&decompressed) {
            Ok(header) => header,
            Err(err) => {
                let prefix = &decompressed[..decompressed.len().min(16)];
                println!(
                    "mapblock header parse failed: {} (decompressed_len={} prefix={})",
                    err,
                    decompressed_len,
                    hex_bytes(prefix)
                );
                return Err(err);
            }
        };

        println!(
            "mapblock header layout={} hello_version={}",
            header.layout, version
        );

        let mut cursor = Cursor::new(decompressed.as_slice());
        cursor.set_position(header.offset as u64);
        let content_width = header.content_width;
        let params_width = header.params_width;

        let data_len = NODECOUNT * ((content_width + params_width) as usize);
        let start_pos = cursor.position() as usize;
        if decompressed_len < start_pos + data_len {
            let max = decompressed_len.min(start_pos + 32);
            let preview = if start_pos < decompressed_len {
                hex_bytes(&decompressed[start_pos..max])
            } else {
                String::new()
            };
            bail!(
                "mapblock node data truncated: need={} have={} header_layout={} start_pos={} preview={}",
                data_len,
                decompressed_len.saturating_sub(start_pos),
                header.layout,
                start_pos,
                preview
            );
        }
        let mut node_data = vec![0u8; data_len];
        cursor
            .read_exact(&mut node_data)
            .context("read node data")?;

        let mut nodes = vec![MapNode::default(); NODECOUNT];
        if content_width == 1 {
            for i in 0..NODECOUNT {
                nodes[i].content = node_data[i] as u16;
            }
        } else {
            for i in 0..NODECOUNT {
                let base = i * 2;
                nodes[i].content = u16::from_be_bytes([node_data[base], node_data[base + 1]]);
            }
        }
        let param1_start = content_width as usize * NODECOUNT;
        let param2_start = (content_width as usize + 1) * NODECOUNT;
        for i in 0..NODECOUNT {
            nodes[i].param1 = node_data[param1_start + i];
            nodes[i].param2 = node_data[param2_start + i];
        }

        Ok(Self { nodes })
    }
}

fn parse_header(data: &[u8]) -> Result<ParsedHeader> {
    if data.len() < 6 {
        bail!("mapblock too short for header");
    }

    let layouts = [
        (1usize, "plain"),
        (2usize, "ver"),
        (3usize, "light"),
        (4usize, "ver+light"),
    ];

    for (header_len, layout) in layouts {
        let width_offset = header_len - 1;
        let content_width = data[width_offset];
        let params_width = data[width_offset + 1];
        if content_width != 1 && content_width != 2 {
            continue;
        }
        if params_width != 2 {
            continue;
        }

        let node_offset = width_offset + 2;
        let data_len = NODECOUNT * ((content_width + params_width) as usize);
        if data.len() < node_offset + data_len {
            continue;
        }

        return Ok(ParsedHeader {
            offset: node_offset,
            content_width,
            params_width,
            layout,
        });
    }

    bail!("no valid header layout found");
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

fn decode_zstd_frame(data: &[u8]) -> Result<Vec<u8>> {
    const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

    let start = if data.starts_with(&ZSTD_MAGIC) {
        Some(0)
    } else {
        find_subslice(data, &ZSTD_MAGIC)
    };

    if let Some(pos) = start {
        log_block_prefix(data, pos);
        let mut decoder =
            zstd::stream::Decoder::new(Cursor::new(&data[pos..])).context("zstd decoder init")?;
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match decoder.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(err) => {
                    let msg = err.to_string();
                    if msg.contains("Unknown frame descriptor") && !out.is_empty() {
                        break;
                    }
                    return Err(err).context("zstd decompress mapblock (stream)");
                }
            }
        }
        if out.is_empty() {
            bail!("zstd decompressed empty frame");
        }
        return Ok(out);
    }

    log_block_prefix(data, usize::MAX);
    bail!("zstd magic not found in block payload");
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn log_block_prefix(data: &[u8], magic_pos: usize) {
    let max = data.len().min(32);
    let mut out = String::new();
    for (idx, b) in data[..max].iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push_str(&format!("{:02x}", b));
    }
    if magic_pos == usize::MAX {
        println!("block zstd magic not found; first_bytes={}", out);
    } else {
        println!(
            "block zstd magic at {} bytes; first_bytes={}",
            magic_pos, out
        );
    }
}

fn div_floor(a: i32, b: i32) -> i32 {
    let mut q = a / b;
    let r = a % b;
    if r < 0 {
        q -= 1;
    }
    q
}

fn mod_floor(a: i32, b: i32) -> i32 {
    let mut r = a % b;
    if r < 0 {
        r += b;
    }
    r
}
