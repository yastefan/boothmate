//! MIDI channel messages: parsing incoming bytes and encoding feedback.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MsgType {
    Note,
    Cc,
    #[serde(rename = "pitchbend")]
    PitchBend,
}

/// Where a control sends: type, channel (1-16) and note or controller number (0 for pitch bend).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Addr {
    #[serde(rename = "type")]
    pub ty: MsgType,
    pub channel: u8,
    #[serde(default)]
    pub number: u8,
}

impl fmt::Display for Addr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.ty {
            MsgType::Note => write!(f, "Note {} ch {}", self.number, self.channel),
            MsgType::Cc => write!(f, "CC {} ch {}", self.number, self.channel),
            MsgType::PitchBend => write!(f, "Pitch bend ch {}", self.channel),
        }
    }
}

impl Addr {
    /// Highest value: 127, or 16383 for pitch bend.
    pub fn max_value(&self) -> u16 {
        match self.ty {
            MsgType::PitchBend => 16383,
            _ => 127,
        }
    }

    /// Encodes a value (clamped to `max()`); note value 0 is sent as note on with velocity 0.
    pub fn encode(&self, value: u16) -> Vec<u8> {
        self.encode_on_channel(self.channel, value)
    }

    pub fn encode_on_channel(&self, channel: u8, value: u16) -> Vec<u8> {
        let ch = channel.clamp(1, 16) - 1;
        let v = value.min(self.max_value());
        match self.ty {
            MsgType::Note => vec![0x90 | ch, self.number & 0x7f, v as u8],
            MsgType::Cc => vec![0xb0 | ch, self.number & 0x7f, v as u8],
            MsgType::PitchBend => vec![0xe0 | ch, (v & 0x7f) as u8, (v >> 7) as u8],
        }
    }
}

/// One incoming value. Note off arrives as value 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Input {
    pub addr: Addr,
    pub value: u16,
}

/// Parses one MIDI message. Anything other than note, CC and pitch bend is ignored.
pub fn parse(bytes: &[u8]) -> Option<Input> {
    let status = *bytes.first()?;
    let channel = (status & 0x0f) + 1;
    let d1 = *bytes.get(1)? & 0x7f;
    let d2 = bytes.get(2).map(|b| b & 0x7f);
    let (ty, number, value) = match status & 0xf0 {
        0x80 => (MsgType::Note, d1, 0),
        0x90 => (MsgType::Note, d1, d2? as u16),
        0xb0 => (MsgType::Cc, d1, d2? as u16),
        0xe0 => (MsgType::PitchBend, 0, d1 as u16 | (d2? as u16) << 7),
        _ => return None,
    };
    Some(Input {
        addr: Addr {
            ty,
            channel,
            number,
        },
        value,
    })
}

/// Parses a hex byte string such as `"F0 00 00 66 14 12"`.
pub fn hex_bytes(s: &str) -> Result<Vec<u8>, String> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).map_err(|_| format!("not a hex byte: {b}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_encode() {
        let i = parse(&[0x91, 60, 100]).unwrap();
        assert_eq!(i.addr.ty, MsgType::Note);
        assert_eq!(i.addr.channel, 2);
        assert_eq!(i.addr.number, 60);
        assert_eq!(i.value, 100);
        assert_eq!(parse(&[0x81, 60, 100]).unwrap().value, 0);
        let cc = parse(&[0xb0, 7, 64]).unwrap();
        assert_eq!(cc.addr.to_string(), "CC 7 ch 1");
        assert_eq!(cc.addr.encode(127), vec![0xb0, 7, 127]);
        let pb = parse(&[0xe3, 0x7f, 0x7f]).unwrap();
        assert_eq!((pb.addr.channel, pb.value), (4, 16383));
        assert_eq!(pb.addr.encode(8192), vec![0xe3, 0, 64]);
        assert_eq!(parse(&[0xf8]), None);
        assert_eq!(parse(&[0xc0, 1]), None);
        assert_eq!(parse(&[0x90, 1]), None);
        assert_eq!(hex_bytes("F0 00 7f").unwrap(), vec![0xf0, 0, 0x7f]);
        assert!(hex_bytes("F0 zz").is_err());
    }
}
