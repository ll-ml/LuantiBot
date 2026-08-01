use anyhow::{bail, Result};

pub struct ByteReader<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.offset)
    }

    pub fn position(&self) -> usize {
        self.offset
    }

    pub fn peek_bytes_at(&self, offset: usize, len: usize) -> Vec<u8> {
        if offset >= self.data.len() || len == 0 {
            return Vec::new();
        }
        let end = offset.saturating_add(len).min(self.data.len());
        self.data[offset..end].to_vec()
    }

    pub fn read_u8(&mut self) -> Result<u8> {
        if self.offset + 1 > self.data.len() {
            bail!("read_u8 out of bounds");
        }
        let v = self.data[self.offset];
        self.offset += 1;
        Ok(v)
    }

    pub fn read_u16(&mut self) -> Result<u16> {
        if self.offset + 2 > self.data.len() {
            bail!("read_u16 out of bounds");
        }
        let v = u16::from_be_bytes([self.data[self.offset], self.data[self.offset + 1]]);
        self.offset += 2;
        Ok(v)
    }

    pub fn read_s16(&mut self) -> Result<i16> {
        Ok(self.read_u16()? as i16)
    }

    pub fn read_u32(&mut self) -> Result<u32> {
        if self.offset + 4 > self.data.len() {
            bail!("read_u32 out of bounds");
        }
        let v = u32::from_be_bytes([
            self.data[self.offset],
            self.data[self.offset + 1],
            self.data[self.offset + 2],
            self.data[self.offset + 3],
        ]);
        self.offset += 4;
        Ok(v)
    }

    pub fn read_f32(&mut self) -> Result<f32> {
        let raw = self.read_u32()?;
        Ok(f32::from_be_bytes(raw.to_be_bytes()))
    }

    pub fn read_string16(&mut self) -> Result<String> {
        let len = self.read_u16()? as usize;
        if self.offset + len > self.data.len() {
            bail!("read_string16 out of bounds");
        }
        let s = String::from_utf8_lossy(&self.data[self.offset..self.offset + len]).to_string();
        self.offset += len;
        Ok(s)
    }

    pub fn read_bytes16(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u16()? as usize;
        if self.offset + len > self.data.len() {
            bail!("read_bytes16 out of bounds");
        }
        let out = self.data[self.offset..self.offset + len].to_vec();
        self.offset += len;
        Ok(out)
    }

    pub fn read_string32(&mut self) -> Result<Vec<u8>> {
        let len = self.read_u32()? as usize;
        if self.offset + len > self.data.len() {
            bail!("read_string32 out of bounds");
        }
        let out = self.data[self.offset..self.offset + len].to_vec();
        self.offset += len;
        Ok(out)
    }

}
