//! Big-endian wire primitives. String decoding here is deliberately strict.

use anyhow::{bail, Context, Result};
use crate::types::{BlockPos, Vec3};

pub(super) fn write_string(buf: &mut Vec<u8>, s: &str) {
    let mut len = s.len().min(u16::MAX as usize);
    while !s.is_char_boundary(len) {
        len -= 1;
    }
    let len = len as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&s.as_bytes()[..len as usize]);
}

pub(super) fn write_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = data.len().min(u16::MAX as usize) as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&data[..len as usize]);
}

pub(super) fn write_wstring(buf: &mut Vec<u8>, s: &str) {
    let mut wide = Vec::new();
    for ch in s.chars() {
        let needed = ch.len_utf16();
        if wide.len() + needed > u16::MAX as usize {
            break;
        }
        let mut encoded = [0; 2];
        wide.extend_from_slice(ch.encode_utf16(&mut encoded));
    }
    let len = wide.len() as u16;
    buf.extend_from_slice(&len.to_be_bytes());
    for ch in wide {
        buf.extend_from_slice(&ch.to_be_bytes());
    }
}

pub(super) fn read_u8(buf: &[u8], offset: &mut usize) -> Result<u8> {
    if *offset + 1 > buf.len() {
        bail!("read_u8 out of bounds");
    }
    let v = buf[*offset];
    *offset += 1;
    Ok(v)
}

pub(super) fn read_u16(buf: &[u8], offset: &mut usize) -> Result<u16> {
    if *offset + 2 > buf.len() {
        bail!("read_u16 out of bounds");
    }
    let v = u16::from_be_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

pub(super) fn read_u32(buf: &[u8], offset: &mut usize) -> Result<u32> {
    if *offset + 4 > buf.len() {
        bail!("read_u32 out of bounds");
    }
    let v = u32::from_be_bytes([
        buf[*offset],
        buf[*offset + 1],
        buf[*offset + 2],
        buf[*offset + 3],
    ]);
    *offset += 4;
    Ok(v)
}

pub(super) fn read_f32_slice(buf: &[u8], offset: &mut usize) -> Result<f32> {
    let raw = read_u32(buf, offset)?;
    Ok(f32::from_bits(raw))
}

pub(super) fn read_v3f32_slice(buf: &[u8], offset: &mut usize) -> Result<Vec3> {
    let x = read_f32_slice(buf, offset)?;
    let y = read_f32_slice(buf, offset)?;
    let z = read_f32_slice(buf, offset)?;
    Ok(Vec3 { x, y, z })
}

pub(super) fn read_i16_slice(buf: &[u8], offset: &mut usize) -> Result<i16> {
    if *offset + 2 > buf.len() {
        bail!("read_i16_slice out of bounds");
    }
    let v = i16::from_be_bytes([buf[*offset], buf[*offset + 1]]);
    *offset += 2;
    Ok(v)
}

pub(super) fn read_v3s16_slice(buf: &[u8], offset: &mut usize) -> Result<BlockPos> {
    let x = read_i16_slice(buf, offset)?;
    let y = read_i16_slice(buf, offset)?;
    let z = read_i16_slice(buf, offset)?;
    Ok(BlockPos { x, y, z })
}

pub(super) fn write_s32(buf: &mut Vec<u8>, value: f32) {
    let v = value.round() as i32;
    buf.extend_from_slice(&v.to_be_bytes());
}

pub(super) fn write_u32(buf: &mut Vec<u8>, value: u32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn write_f32(buf: &mut Vec<u8>, value: f32) {
    buf.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn write_i16(buf: &mut Vec<u8>, value: i16) {
    buf.extend_from_slice(&value.to_be_bytes());
}

pub(super) fn write_v3s16(buf: &mut Vec<u8>, v: BlockPos) {
    write_i16(buf, v.x);
    write_i16(buf, v.y);
    write_i16(buf, v.z);
}

pub(super) fn write_v3s32(buf: &mut Vec<u8>, v: Vec3, scale: f32) {
    let x = (v.x * scale).round() as i32;
    let y = (v.y * scale).round() as i32;
    let z = (v.z * scale).round() as i32;
    buf.extend_from_slice(&x.to_be_bytes());
    buf.extend_from_slice(&y.to_be_bytes());
    buf.extend_from_slice(&z.to_be_bytes());
}

pub(super) fn read_string_slice(buf: &[u8], offset: &mut usize) -> Result<String> {
    let len = read_u16(buf, offset)? as usize;
    if *offset + len > buf.len() {
        bail!("read_string_slice out of bounds");
    }
    let s = std::str::from_utf8(&buf[*offset..*offset + len])
        .context("utf8 string")?
        .to_string();
    *offset += len;
    Ok(s)
}

pub(super) fn read_bytes_slice(buf: &[u8], offset: &mut usize) -> Result<Vec<u8>> {
    let len = read_u16(buf, offset)? as usize;
    if *offset + len > buf.len() {
        bail!("read_bytes_slice out of bounds");
    }
    let out = buf[*offset..*offset + len].to_vec();
    *offset += len;
    Ok(out)
}

pub(super) fn read_wstring_slice(buf: &[u8], offset: &mut usize) -> Result<String> {
    let len = read_u16(buf, offset)? as usize;
    let bytes_len = len * 2;
    if *offset + bytes_len > buf.len() {
        bail!("read_wstring_slice out of bounds");
    }
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        let hi = buf[*offset];
        let lo = buf[*offset + 1];
        out.push(u16::from_be_bytes([hi, lo]));
        *offset += 2;
    }
    String::from_utf16(&out).context("utf16 string")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_round_trip_without_losing_unicode() {
        let value = "hello \u{1f980} \u{e9}";
        let mut bytes = Vec::new();
        write_string(&mut bytes, value);
        let mut offset = 0;
        assert_eq!(read_string_slice(&bytes, &mut offset).unwrap(), value);
        assert_eq!(offset, bytes.len());

        bytes.clear();
        write_wstring(&mut bytes, value);
        offset = 0;
        assert_eq!(read_wstring_slice(&bytes, &mut offset).unwrap(), value);
        assert_eq!(offset, bytes.len());
    }

    #[test]
    fn string_writers_truncate_only_at_character_boundaries() {
        let prefix = "a".repeat(u16::MAX as usize - 1);
        let value = format!("{prefix}\u{1f980}");
        let mut bytes = Vec::new();
        write_string(&mut bytes, &value);
        assert_eq!(read_string_slice(&bytes, &mut 0).unwrap(), prefix);

        bytes.clear();
        write_wstring(&mut bytes, &value);
        assert_eq!(read_wstring_slice(&bytes, &mut 0).unwrap(), prefix);
    }

    #[test]
    fn string_readers_reject_invalid_encodings_and_truncated_payloads() {
        assert!(read_string_slice(&[0, 1, 0xff], &mut 0).is_err());
        assert!(read_string_slice(&[0, 2, b'a'], &mut 0).is_err());
        assert!(read_wstring_slice(&[0, 1, 0xd8, 0x00], &mut 0).is_err());
        assert!(read_wstring_slice(&[0, 1, 0], &mut 0).is_err());
    }
}
