//! Little-endian binary reader with LSB-first bit support.
//!
//! Mirrors the subset of the XYSKY `BinaryReader` that is needed by packet
//! payloads with packed fields (`Affinity`, `NetLevelDataHeartbeat`, ...).

/// Cursor over a byte slice.
///
/// Byte reads align the cursor to the next byte boundary. Bit reads consume
/// bits LSB-first, matching the client's `SerializeCompressed` encoding.
pub struct Reader<'a> {
    data: &'a [u8],
    offset: usize,
    bit_offset: u8,
}

impl<'a> Reader<'a> {
    /// Create a reader positioned at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            offset: 0,
            bit_offset: 0,
        }
    }

    /// Bytes left after aligning to the next byte boundary.
    pub fn remaining(&self) -> usize {
        let consumed = self.offset + usize::from(self.bit_offset != 0);
        self.data.len().saturating_sub(consumed)
    }

    /// Bits left in the current byte and all following bytes.
    pub fn remaining_bits(&self) -> usize {
        self.data
            .len()
            .saturating_sub(self.offset)
            .saturating_mul(8)
            .saturating_sub(usize::from(self.bit_offset))
    }

    /// Read an unsigned 8-bit integer.
    pub fn read_u8(&mut self) -> Option<u8> {
        Some(self.read_bytes(1)?[0])
    }

    /// Read a little-endian unsigned 16-bit integer.
    pub fn read_u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.read_bytes(2)?.try_into().ok()?))
    }

    /// Read a little-endian unsigned 32-bit integer.
    pub fn read_u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.read_bytes(4)?.try_into().ok()?))
    }

    /// Read a little-endian unsigned 64-bit integer.
    pub fn read_u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.read_bytes(8)?.try_into().ok()?))
    }

    /// Read a little-endian `f32`.
    pub fn read_f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.read_bytes(4)?.try_into().ok()?))
    }

    /// Read a little-endian `f64`.
    pub fn read_f64(&mut self) -> Option<f64> {
        Some(f64::from_le_bytes(self.read_bytes(8)?.try_into().ok()?))
    }

    /// Read exactly `len` bytes after aligning to a byte boundary.
    pub fn read_bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        self.align_to_byte();
        let end = self.offset.checked_add(len)?;
        if end > self.data.len() {
            return None;
        }
        let bytes = &self.data[self.offset..end];
        self.offset = end;
        Some(bytes)
    }

    /// Read up to 32 bits, LSB-first.
    pub fn read_bits(&mut self, count: u8) -> Option<u32> {
        if count > 32 || usize::from(count) > self.remaining_bits() {
            return None;
        }

        let mut value = 0u32;
        for index in 0..count {
            let bit = (self.data[self.offset] >> self.bit_offset) & 1;
            value |= u32::from(bit) << index;
            self.bit_offset += 1;
            if self.bit_offset == 8 {
                self.bit_offset = 0;
                self.offset += 1;
            }
        }
        Some(value)
    }

    /// Read a single packed boolean.
    pub fn read_bool(&mut self) -> Option<bool> {
        Some(self.read_bits(1)? != 0)
    }

    /// Read a `SerializeCompressed(max)` value.
    ///
    /// The client uses a fixed bit width based on `max`, not a varint.
    pub fn read_compressed(&mut self, max: u32) -> Option<u32> {
        let bits = (32 - max.leading_zeros()) as u8;
        let value = self.read_bits(bits)?;
        (value <= max).then_some(value)
    }

    /// Skip padding so the next read starts on a byte boundary.
    pub fn align_to_byte(&mut self) {
        if self.bit_offset != 0 {
            self.offset += 1;
            self.bit_offset = 0;
        }
    }
}

/// Little-endian binary writer with LSB-first bit support.
#[derive(Default)]
pub struct Writer {
    data: Vec<u8>,
    bit_offset: u8,
}

impl Writer {
    /// Create an empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write an unsigned 8-bit integer.
    pub fn write_u8(&mut self, value: u8) {
        self.align_to_byte();
        self.data.push(value);
    }

    /// Write a little-endian unsigned 16-bit integer.
    pub fn write_u16(&mut self, value: u16) {
        self.align_to_byte();
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a little-endian unsigned 32-bit integer.
    pub fn write_u32(&mut self, value: u32) {
        self.align_to_byte();
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a little-endian unsigned 64-bit integer.
    pub fn write_u64(&mut self, value: u64) {
        self.align_to_byte();
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a little-endian `f32`.
    pub fn write_f32(&mut self, value: f32) {
        self.align_to_byte();
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Write a little-endian `f64`.
    pub fn write_f64(&mut self, value: f64) {
        self.align_to_byte();
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    /// Write raw bytes after aligning to a byte boundary.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.align_to_byte();
        self.data.extend_from_slice(bytes);
    }

    /// Write up to 32 bits, LSB-first.
    pub fn write_bits(&mut self, value: u32, count: u8) {
        let count = count.min(32);
        let mask = if count == 32 {
            u32::MAX
        } else if count == 0 {
            0
        } else {
            (1u32 << count) - 1
        };
        let value = value & mask;

        for index in 0..count {
            if self.bit_offset == 0 {
                self.data.push(0);
            }
            if (value >> index) & 1 == 1
                && let Some(last) = self.data.last_mut()
            {
                *last |= 1 << self.bit_offset;
            }
            self.bit_offset += 1;
            if self.bit_offset == 8 {
                self.bit_offset = 0;
            }
        }
    }

    /// Write a single packed boolean.
    pub fn write_bool(&mut self, value: bool) {
        self.write_bits(u32::from(value), 1);
    }

    /// Write a `SerializeCompressed(max)` value.
    pub fn write_compressed(&mut self, value: u32, max: u32) -> Option<()> {
        if value > max {
            return None;
        }
        let bits = (32 - max.leading_zeros()) as u8;
        self.write_bits(value, bits);
        Some(())
    }

    /// Finish the buffer.
    pub fn into_bytes(self) -> Vec<u8> {
        self.data
    }

    fn align_to_byte(&mut self) {
        self.bit_offset = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_reads() {
        let bytes = [0x34, 0x12, 0x78, 0x56, 0x34, 0x12];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_u16(), Some(0x1234));
        assert_eq!(reader.read_u32(), Some(0x12345678));
        assert_eq!(reader.remaining(), 0);
        assert_eq!(reader.read_u8(), None);
    }

    #[test]
    fn packed_bits_are_lsb_first() {
        // bool 1, then a 4-bit compressed value 5 -> bits 1,0101
        let bytes = [0b0010_1011, 0xaa];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_bool(), Some(true));
        assert_eq!(reader.read_compressed(8), Some(5));
        // Byte reads align, skipping the rest of the first byte.
        assert_eq!(reader.read_u8(), Some(0xaa));
    }

    #[test]
    fn compressed_widths() {
        assert_eq!(Reader::new(&[0x00]).read_compressed(0), Some(0));
        assert_eq!(Reader::new(&[0x01]).read_compressed(1), Some(1));
        assert_eq!(Reader::new(&[0x03]).read_compressed(3), Some(3));
        assert_eq!(Reader::new(&[0x08, 0x00]).read_compressed(8), Some(8));
        assert_eq!(Reader::new(&[0xff]).read_compressed(0xff), Some(0xff));
        assert_eq!(Reader::new(&[0x09]).read_compressed(8), None);
    }

    #[test]
    fn bytes_align_after_bits() {
        let bytes = [0x01, 0x7f, 0xff];
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_bool(), Some(true));
        assert_eq!(reader.remaining(), 2);
        assert_eq!(reader.read_u8(), Some(0x7f));
        assert_eq!(reader.read_u8(), Some(0xff));
    }

    #[test]
    fn writer_matches_reader() {
        let mut writer = Writer::new();
        writer.write_bool(true);
        writer.write_compressed(5, 8).unwrap();
        writer.write_u8(0xaa);
        writer.write_u16(0x1234);

        let bytes = writer.into_bytes();
        assert_eq!(bytes, vec![0b0000_1011, 0xaa, 0x34, 0x12]);

        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.read_bool(), Some(true));
        assert_eq!(reader.read_compressed(8), Some(5));
        assert_eq!(reader.read_u8(), Some(0xaa));
        assert_eq!(reader.read_u16(), Some(0x1234));
    }

    #[test]
    fn writer_handles_trailing_partial_byte() {
        let mut writer = Writer::new();
        writer.write_bool(false);
        writer.write_bool(true);
        assert_eq!(writer.into_bytes(), vec![0b0000_0010]);
    }
}
