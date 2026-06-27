//! Net Level Data
//!

// let we use some black magic
// 22 bytes
#[derive(Clone, Copy, Debug)]
#[repr(C, packed)]
pub struct LevelDataHeader {
    pub elected_player: u8,
    pub level_id: u32,
    unk1: u16,
    pub has_initial_data: u8,
    unk2: u16,
    unk3: u8,
    pub level_hash: u32,
    pub merge_state: u8,
    unk5: u16,
    unk6: u16,
    pub length: u16,
}

#[derive(Clone, Debug)]
pub struct LevelData {
    pub header: LevelDataHeader,
    pub data: Vec<u8>,
}

impl LevelData {
    /// Create a empty level data
    pub fn new(elected_player: u8, level_id: u32, has_initial_data: u8, data: &[u8]) -> Self {
        Self {
            header: LevelDataHeader {
                elected_player,
                level_id,
                unk1: 0,
                has_initial_data,
                unk2: 0,
                unk3: 0,
                level_hash: 0,
                merge_state: 0,
                unk5: 0,
                unk6: 0,
                length: data.len() as u16,
            },
            data: data.to_vec(),
        }
    }

    /// Convert slice to LevelData
    /// return None is length too large.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < 22 {
            return None;
        }
        let mut header = unsafe { *(buf.as_ptr() as *const LevelDataHeader) };
        let data = buf[size_of::<LevelDataHeader>()..].to_vec();

        // Verify the length is right
        if header.length as usize != data.len() {
            let len = header.length;
            println!("header.len={}, data.len={}", len, data.len());
            return None;
        }
        header.length = data.len() as u16;

        Some(Self { header, data })
    }

    /// Convert LevelData to Vec<u8>
    pub fn to_bytes(&self) -> Vec<u8> {
        let header_bytes = unsafe {
            std::slice::from_raw_parts(
                &self.header as *const LevelDataHeader as *const u8,
                size_of::<LevelDataHeader>(),
            )
        };

        let mut buf = Vec::with_capacity(header_bytes.len() + self.data.len());
        buf.extend_from_slice(header_bytes);
        buf.extend_from_slice(&self.data);
        buf
    }

    /// Update the level data with new data.
    pub fn update(&mut self, data: &Self) -> Option<()> {
        if data.header.length as usize != data.data.len() {
            return None;
        }
        let elected_player = self.header.elected_player;
        self.header = data.header;
        self.header.elected_player = elected_player;
        self.data = data.data.clone();
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::hex_preview;

    use super::*;
    #[test]
    fn from_bytes() {
        let bytes = [];

        let level_data = LevelData::from_bytes(&bytes);

        println!(
            "{}",
            hex_preview(&LevelData::new(0xe8, 0x30b82314, 0, &[]).to_bytes(), 64)
        );
        println!("{:?}", level_data);
    }
}
