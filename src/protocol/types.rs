//! Types used in the protocol.

use std::fmt::Debug;

/// Represents a TGC UUID, a 16-byte unique identifier.
/// |type|note|
/// |-|-|
/// |[u8; 16]|A 16-bytes uuid.|
#[derive(Clone, Copy, Default, Eq, PartialEq, Hash)]
pub struct TgcUuid([u8; 16]);

impl TgcUuid {
    pub fn from(v: &dyn AsRef<[u8]>) -> Option<Self> {
        let v = v.as_ref();
        if v.len() != 16 {
            return None;
        }
        Some(Self(v.try_into().unwrap()))
    }

    pub fn raw(&self) -> [u8; 16] {
        self.0
    }

    /// Parse a hyphenated UUID string (the format used by QWD room ids).
    pub fn parse(value: &str) -> Option<Self> {
        let mut bytes = [0u8; 16];
        let mut index = 0usize;
        let mut high: Option<u8> = None;

        for ch in value.chars() {
            if ch == '-' {
                continue;
            }
            let digit = ch.to_digit(16)? as u8;
            match high.take() {
                Some(high) => {
                    if index >= bytes.len() {
                        return None;
                    }
                    bytes[index] = (high << 4) | digit;
                    index += 1;
                }
                None => high = Some(digit),
            }
        }

        (index == bytes.len() && high.is_none()).then_some(Self(bytes))
    }
}

impl Debug for TgcUuid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use std::fmt::Write;

        let mut s = String::with_capacity(36);

        for (i, b) in self.0.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                s.push('-');
            }

            write!(s, "{:02x}", b).unwrap();
        }
        write!(f, "{}", s)
    }
}

#[cfg(test)]
mod tests {
    use super::TgcUuid;

    #[test]
    fn parse_matches_debug_format() {
        let uuid = TgcUuid::from(&[0xabu8; 16]).unwrap();
        assert_eq!(TgcUuid::parse(&format!("{uuid:?}")), Some(uuid));
        assert_eq!(
            TgcUuid::parse("00000000-0000-0000-0000-000000000000"),
            Some(TgcUuid::default())
        );
        assert!(TgcUuid::parse("not-a-uuid").is_none());
        assert!(TgcUuid::parse("00112233-4455-6677-8899-aabbccddee").is_none());
    }
}
