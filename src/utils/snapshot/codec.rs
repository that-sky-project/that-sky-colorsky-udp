//! RLE, XOR and checksum primitives for snapshot frames.

/// Run-length encode a byte slice, compressing runs of zeroes.
///
/// Each zero run of length `n` is encoded as `[0x00, n]`. Non-zero bytes
/// are copied as-is. Maximum run length is 255.
pub(super) fn rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0 {
            let mut count = 0u8;
            while i < data.len() && count < 255 && data[i] == 0 {
                count += 1;
                i += 1;
            }
            out.push(0x00);
            out.push(count);
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// Decode an RLE-encoded byte slice.
///
/// Returns `None` if the data is truncated (0x00 without a valid count).
pub(super) fn rle_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x00 {
            if i + 1 >= data.len() {
                return None;
            }
            let count = data[i + 1];
            if count == 0 {
                return None;
            }
            out.resize(out.len() + count as usize, 0);
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Compute the XOR difference between `new` and `base`.
///
/// Bytes beyond `base.len()` in `new` are copied as-is (they have no
/// base to XOR against).
pub(super) fn xor_diff(new: &[u8], base: &[u8], new_len: usize) -> Vec<u8> {
    let base_len = base.len().min(new_len);
    let mut buf = vec![0u8; new_len];
    for i in 0..base_len {
        buf[i] = new[i] ^ base[i];
    }
    if new_len > base.len() {
        buf[base.len()..].copy_from_slice(&new[base.len()..]);
    }
    buf
}

/// Compute an 8-bit rolling checksum for snapshot frame integrity.
///
/// Used to validate frames before decoding.
pub fn snapshot_checksum(data: &[u8]) -> u8 {
    let mut h: u8 = 0xC5;
    for &b in data {
        h = (0x93u16.wrapping_mul((h ^ b) as u16) & 0xFF) as u8;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        for (hex, exp) in &[
            (
                "00a750008507001d05001d0b00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff003f",
                0x50u8,
            ),
            (
                "00a7b3008506001d06001d0a00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff003f",
                0xb3,
            ),
        ] {
            let d = hex::decode(hex).unwrap();
            assert_eq!(snapshot_checksum(&d[3..]), d[2]);
            assert_eq!(snapshot_checksum(&d[3..]), *exp);
        }
    }

    #[test]
    fn rle_roundtrip() {
        let data = vec![0u8; 40];
        assert_eq!(rle_encode(&data), vec![0x00, 40]);
        assert_eq!(rle_decode(&rle_encode(&data)).unwrap(), data);
        assert!(rle_decode(&[0x00]).is_none());
        assert!(rle_decode(&[0x00, 0x00]).is_none());
    }
}
