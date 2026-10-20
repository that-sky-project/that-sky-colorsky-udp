//! Network address used by move and transfer packets.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::protocol::binary::{Reader, Writer};

/// IPv4/IPv6 host plus UDP port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NetAddress {
    pub host: IpAddr,
    pub port: u16,
}

impl NetAddress {
    /// Create an address.
    pub const fn new(host: IpAddr, port: u16) -> Self {
        Self { host, port }
    }

    /// Decode the untyped 6-byte form used by `MoveGame`.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 6 {
            return None;
        }
        let mut reader = Reader::new(data);
        Self::read(&mut reader)
    }

    /// Encode the untyped 6-byte form used by `MoveGame`.
    pub fn to_bytes(&self) -> Option<[u8; 6]> {
        let IpAddr::V4(ipv4) = self.host else {
            return None;
        };
        let octets = ipv4.octets();
        Some([
            octets[0],
            octets[1],
            octets[2],
            octets[3],
            (self.port & 0xff) as u8,
            (self.port >> 8) as u8,
        ])
    }

    /// Read the untyped address.
    pub fn read(reader: &mut Reader<'_>) -> Option<Self> {
        let bytes = reader.read_bytes(6)?;
        let host = IpAddr::V4(Ipv4Addr::new(bytes[0], bytes[1], bytes[2], bytes[3]));
        let port = u16::from_le_bytes([bytes[4], bytes[5]]);
        Some(Self { host, port })
    }

    /// Write the untyped address. IPv6 cannot be represented.
    pub fn write(&self, writer: &mut Writer) -> Option<()> {
        writer.write_bytes(&self.to_bytes()?);
        Some(())
    }

    /// Read the typed address used by `JoinGame.transferAddress`.
    pub fn read_typed(reader: &mut Reader<'_>) -> Option<Self> {
        let address_type = reader.read_compressed(3)? as u8;

        let host = match address_type {
            0 | 3 => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            1 => {
                let value = reader.read_u32()?;
                IpAddr::V4(Ipv4Addr::from(value.to_le_bytes()))
            }
            2 => {
                let bytes = reader.read_bytes(16)?;
                let mut octets = [0u8; 16];
                octets.copy_from_slice(bytes);
                IpAddr::V6(Ipv6Addr::from(octets))
            }
            _ => return None,
        };

        let has_port = reader.read_bool()?;
        let port = if has_port { reader.read_u16()? } else { 0 };
        Some(Self { host, port })
    }

    /// Write the typed address used by `JoinGame.transferAddress`.
    pub fn write_typed(&self, writer: &mut Writer) -> Option<()> {
        let address_type = match self.host {
            IpAddr::V4(ipv4) if ipv4.is_unspecified() => 0,
            IpAddr::V4(_) => 1,
            IpAddr::V6(_) => 2,
        };
        writer.write_compressed(address_type, 3)?;

        match self.host {
            IpAddr::V4(ipv4) if !ipv4.is_unspecified() => {
                writer.write_u32(u32::from_le_bytes(ipv4.octets()));
            }
            IpAddr::V6(ipv6) => {
                writer.write_bytes(&ipv6.octets());
            }
            IpAddr::V4(_) => {}
        }

        let has_port = self.port != 0;
        writer.write_bool(has_port);
        if has_port {
            writer.write_u16(self.port);
        }
        Some(())
    }

    /// JSON-like representation for logs and control responses.
    pub fn to_api(&self) -> (String, u16) {
        (self.host.to_string(), self.port)
    }
}

impl fmt::Display for NetAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untyped_ipv4_roundtrip() {
        let address = NetAddress::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)), 19133);
        let bytes = address.to_bytes().unwrap();
        assert_eq!(bytes, [192, 168, 1, 4, 0xbd, 0x4a]);
        assert_eq!(NetAddress::from_bytes(&bytes), Some(address));
    }

    #[test]
    fn typed_ipv4_roundtrip() {
        let address = NetAddress::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), 7777);
        let mut writer = Writer::new();
        address.write_typed(&mut writer).unwrap();
        let bytes = writer.into_bytes();

        let mut reader = Reader::new(&bytes);
        assert_eq!(NetAddress::read_typed(&mut reader), Some(address));
    }

    #[test]
    fn typed_address_without_port() {
        let address = NetAddress::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let mut writer = Writer::new();
        address.write_typed(&mut writer).unwrap();
        let bytes = writer.into_bytes();

        let mut reader = Reader::new(&bytes);
        assert_eq!(NetAddress::read_typed(&mut reader), Some(address));
    }

    #[test]
    fn typed_ipv6_roundtrip() {
        let address = NetAddress::new(
            IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)),
            19133,
        );
        let mut writer = Writer::new();
        address.write_typed(&mut writer).unwrap();
        let bytes = writer.into_bytes();

        let mut reader = Reader::new(&bytes);
        assert_eq!(NetAddress::read_typed(&mut reader), Some(address));
    }
}
