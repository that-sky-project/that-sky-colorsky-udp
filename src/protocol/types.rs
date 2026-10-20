//! Types used in the protocol.

use std::fmt::Debug;

/// Represents a TGC UUID, a 16-byte unique identifier.
/// |type|note|
/// |-|-|
/// |[u8; 16]|A 16-bytes uuid.|
#[derive(Clone, Copy, Default, Eq, PartialEq)]
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
