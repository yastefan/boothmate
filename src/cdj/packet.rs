//! Pro DJ Link packets: parsing what CDJs and mixers send, and building the few packets Boothmate
//! sends itself (keep-alive of the silent virtual player) plus the ones the simulator fakes.
//!
//! Written from the protocol analysis by Deep Symmetry (<https://djl-analysis.deepsymmetry.org/>).
//! All numbers are big-endian.

use std::net::Ipv4Addr;

pub const ANNOUNCE_PORT: u16 = 50000;
pub const BEAT_PORT: u16 = 50001;
pub const STATUS_PORT: u16 = 50002;

/// Every DJ Link packet starts with this.
pub const MAGIC: &[u8; 10] = b"Qspt1WmJOL";

/// Pitch value for 0 % (`0x000000` = -100 %, `0x200000` = +100 %).
const PITCH_ZERO: u32 = 0x10_0000;

/// What kind of device sent a keep-alive (byte 0x34).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Player,
    Mixer,
    Rekordbox,
    Other,
}

impl Kind {
    fn from_byte(b: u8) -> Self {
        match b {
            1 => Kind::Player,
            3 => Kind::Mixer,
            4 => Kind::Rekordbox,
            _ => Kind::Other,
        }
    }

    fn byte(self) -> u8 {
        match self {
            Kind::Player => 1,
            Kind::Mixer => 3,
            Kind::Rekordbox => 4,
            Kind::Other => 5,
        }
    }
}

/// Keep-alive on port 50000 (type 0x06), sent by every device about every 1.5-2 s.
#[derive(Debug, Clone, PartialEq)]
pub struct KeepAlive {
    pub name: String,
    pub number: u8,
    pub kind: Kind,
    pub mac: [u8; 6],
    pub ip: Ipv4Addr,
}

/// Beat packet on port 50001 (type 0x28), sent by a playing player on every beat, and
/// continuously by the mixer.
#[derive(Debug, Clone, PartialEq)]
pub struct Beat {
    pub number: u8,
    pub name: String,
    /// Milliseconds until the next beat and the next bar, at 0 % pitch. `None`: the track ends first.
    pub next_beat_ms: Option<u32>,
    pub next_bar_ms: Option<u32>,
    /// Pitch in percent.
    pub pitch: f32,
    /// Track BPM (without pitch). `None` when unknown.
    pub track_bpm: Option<f32>,
    /// 1-4.
    pub beat_in_bar: u8,
}

/// CDJ-3000 absolute position on port 50001 (type 0x0b), every 30 ms while a track is loaded.
#[derive(Debug, Clone, PartialEq)]
pub struct Position {
    pub number: u8,
    pub track_len_s: u32,
    pub playhead_ms: u32,
    /// Effective BPM, `None` when unknown.
    pub bpm: Option<f32>,
}

/// CDJ status on port 50002 (type 0x0a), about every 200 ms, sent only to devices that announce
/// themselves on the network.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Status {
    pub number: u8,
    pub name: String,
    /// Player the track was loaded from (0 = no track).
    pub track_player: u8,
    /// 0 none, 1 CD, 2 SD, 3 USB, 4 rekordbox, ...
    pub track_slot: u8,
    /// 0 none, 1 rekordbox, 2 unanalysed, 5 audio CD, 6 streaming.
    pub track_type: u8,
    /// rekordbox database ID of the loaded track.
    pub track_id: u32,
    /// Play state P1 (byte 0x7b), see [`play_state_name`].
    pub play_state: u8,
    pub firmware: String,
    pub playing: bool,
    pub master: bool,
    pub sync: bool,
    pub on_air: bool,
    /// Effective pitch in percent.
    pub pitch: f32,
    /// Track BPM without pitch, `None` when no track is loaded.
    pub track_bpm: Option<f32>,
    /// Beat number from the start of the beat grid (1-based), `None` without a rekordbox track.
    pub beat: Option<u32>,
    /// Beats until the next memory cue, `None` when there is none.
    pub beats_to_cue: Option<u16>,
    /// 1-4, 0 without a rekordbox track.
    pub beat_in_bar: u8,
}

/// Mixer status on port 50002 (type 0x29).
#[derive(Debug, Clone, PartialEq)]
pub struct MixerStatus {
    pub number: u8,
    pub name: String,
    pub master: bool,
    pub bpm: Option<f32>,
    pub beat_in_bar: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Packet {
    KeepAlive(KeepAlive),
    /// Another device defends a device number (port 50000, type 0x08).
    Conflict {
        number: u8,
    },
    Beat(Beat),
    Position(Position),
    Status(Status),
    MixerStatus(MixerStatus),
}

fn u16_at(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn name_at(d: &[u8], o: usize) -> String {
    let raw = &d[o..o + 20];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(20);
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
}

fn pitch(raw: u32) -> f32 {
    let p = (raw as f64 - PITCH_ZERO as f64) / PITCH_ZERO as f64 * 100.0;
    (p * 100.0).round() as f32 / 100.0
}

fn pitch_raw(percent: f32) -> u32 {
    ((1.0 + percent as f64 / 100.0) * PITCH_ZERO as f64)
        .round()
        .max(0.0) as u32
}

fn bpm100(raw: u16) -> Option<f32> {
    (raw != 0xffff && raw != 0).then(|| raw as f32 / 100.0)
}

/// Parses a packet received on `port`. Anything else (other software, unknown types, short
/// packets) gives `None`.
pub fn parse(port: u16, d: &[u8]) -> Option<Packet> {
    if d.len() < 0x24 || &d[..10] != MAGIC {
        return None;
    }
    let ty = d[0x0a];
    match (port, ty) {
        (ANNOUNCE_PORT, 0x06) if d.len() >= 0x35 => Some(Packet::KeepAlive(KeepAlive {
            name: name_at(d, 0x0c),
            number: d[0x24],
            kind: Kind::from_byte(d[0x34]),
            mac: d[0x26..0x2c].try_into().ok()?,
            ip: Ipv4Addr::from(u32_at(d, 0x2c)),
        })),
        (ANNOUNCE_PORT, 0x08) if d.len() >= 0x25 => Some(Packet::Conflict { number: d[0x24] }),
        (BEAT_PORT, 0x28) if d.len() >= 0x60 => {
            let ms = |o| Some(u32_at(d, o)).filter(|&v| v != 0xffff_ffff);
            Some(Packet::Beat(Beat {
                number: d[0x21],
                name: name_at(d, 0x0b),
                next_beat_ms: ms(0x24),
                next_bar_ms: ms(0x2c),
                pitch: pitch(u32_at(d, 0x54)),
                track_bpm: bpm100(u16_at(d, 0x5a)),
                beat_in_bar: d[0x5c],
            }))
        }
        (BEAT_PORT, 0x0b) if d.len() >= 0x34 => {
            let bpm = u32_at(d, 0x30);
            Some(Packet::Position(Position {
                number: d[0x21],
                track_len_s: u32_at(d, 0x24),
                playhead_ms: u32_at(d, 0x28),
                bpm: (bpm != 0xffff_ffff).then(|| bpm as f32 / 10.0),
            }))
        }
        // Short status packets come from rekordbox; skip them.
        (STATUS_PORT, 0x0a) if d.len() >= 0xc8 => {
            let flags = d[0x89];
            let beat = u32_at(d, 0xa0);
            let cue = u16_at(d, 0xa4);
            let fw = String::from_utf8_lossy(&d[0x7c..0x80])
                .trim_matches(char::from(0))
                .to_string();
            Some(Packet::Status(Status {
                number: d[0x21],
                name: name_at(d, 0x0b),
                track_player: d[0x28],
                track_slot: d[0x29],
                track_type: d[0x2a],
                track_id: u32_at(d, 0x2c),
                play_state: d[0x7b],
                firmware: fw,
                playing: flags & 0x40 != 0,
                master: flags & 0x20 != 0,
                sync: flags & 0x10 != 0,
                on_air: flags & 0x08 != 0,
                pitch: pitch(u32_at(d, 0x8c)),
                track_bpm: bpm100(u16_at(d, 0x92)),
                beat: (beat != 0xffff_ffff && beat != 0).then_some(beat),
                beats_to_cue: (cue != 0x01ff).then_some(cue),
                beat_in_bar: d[0xa6],
            }))
        }
        (STATUS_PORT, 0x29) if d.len() >= 0x38 => Some(Packet::MixerStatus(MixerStatus {
            number: d[0x21],
            name: name_at(d, 0x0b),
            master: d[0x27] & 0x20 != 0,
            bpm: bpm100(u16_at(d, 0x2e)),
            beat_in_bar: d[0x37],
        })),
        _ => None,
    }
}

/// Human-readable play state (byte P1).
pub fn play_state_name(p: u8) -> &'static str {
    match p {
        0x00 => "empty",
        0x02 => "loading",
        0x03 => "playing",
        0x04 => "looping",
        0x05 => "paused",
        0x06 => "cued",
        0x07 => "cue play",
        0x08 => "cue scratch",
        0x09 => "searching",
        0x0e => "spun down",
        0x11 => "ended",
        0x12 => "emergency loop",
        _ => "unknown",
    }
}

/// Track slot (byte Sr).
pub fn slot_name(s: u8) -> &'static str {
    match s {
        0 => "",
        1 => "CD",
        2 => "SD",
        3 => "USB",
        4 => "rekordbox",
        6 => "streaming",
        7 => "USB 2",
        9 | 0x0e => "Beatport LINK",
        _ => "other",
    }
}

// Building ---------------------------------------------------------------------------------------

fn header(ty: u8, len: usize) -> Vec<u8> {
    let mut b = vec![0u8; len];
    b[..10].copy_from_slice(MAGIC);
    b[0x0a] = ty;
    b
}

fn put_name(b: &mut [u8], at: usize, name: &str) {
    let n = name.as_bytes();
    let n = &n[..n.len().min(19)];
    b[at..at + n.len()].copy_from_slice(n);
}

/// Keep-alive (type 0x06) for port 50000. Boothmate's virtual player sends this every 1.5 s so
/// players send it their status. Same bytes as alphatheta-connect's announce packet, which is
/// known to work with CDJ-3000s.
pub fn keep_alive(name: &str, number: u8, kind: Kind, mac: [u8; 6], ip: Ipv4Addr) -> Vec<u8> {
    let mut b = header(0x06, 0x36);
    put_name(&mut b, 0x0c, name);
    b[0x20] = 0x01;
    b[0x21] = 0x02;
    b[0x23] = 0x36;
    b[0x24] = number;
    b[0x25] = if kind == Kind::Mixer { 2 } else { 1 };
    b[0x26..0x2c].copy_from_slice(&mac);
    b[0x2c..0x30].copy_from_slice(&ip.octets());
    b[0x30] = 0x02;
    b[0x34] = kind.byte();
    // Needed so CDJ-3000s on player numbers 5 and 6 accept us.
    b[0x35] = 0x64;
    b
}

fn header_50001(ty: u8, name: &str, number: u8, len: usize) -> Vec<u8> {
    let mut b = header(ty, len);
    put_name(&mut b, 0x0b, name);
    b[0x1f] = 0x01;
    b[0x21] = number;
    b[0x22..0x24].copy_from_slice(&((len - 0x24) as u16).to_be_bytes());
    b
}

/// Beat packet (type 0x28), used by the simulator and tests.
pub fn beat(name: &str, number: u8, track_bpm: f32, pitch_pct: f32, beat_in_bar: u8) -> Vec<u8> {
    let mut b = header_50001(0x28, name, number, 0x60);
    let beat_ms = (60_000.0 / track_bpm) as u32;
    let to_bar = (4 - beat_in_bar as u32) % 4 + 1;
    let fields = [
        (0x24, beat_ms),
        (0x28, beat_ms * 2),
        (0x2c, beat_ms * to_bar),
        (0x30, beat_ms * 4),
        (0x34, beat_ms * (to_bar + 4)),
        (0x38, beat_ms * 8),
    ];
    for (o, v) in fields {
        b[o..o + 4].copy_from_slice(&v.to_be_bytes());
    }
    b[0x54..0x58].copy_from_slice(&pitch_raw(pitch_pct).to_be_bytes());
    b[0x5a..0x5c].copy_from_slice(&((track_bpm * 100.0).round() as u16).to_be_bytes());
    b[0x5c] = beat_in_bar;
    b[0x5f] = number;
    b
}

/// CDJ-3000 position packet (type 0x0b), used by the simulator and tests.
pub fn position(name: &str, number: u8, track_len_s: u32, playhead_ms: u32, bpm: f32) -> Vec<u8> {
    let mut b = header_50001(0x0b, name, number, 0x3c);
    b[0x24..0x28].copy_from_slice(&track_len_s.to_be_bytes());
    b[0x28..0x2c].copy_from_slice(&playhead_ms.to_be_bytes());
    b[0x30..0x34].copy_from_slice(&((bpm * 10.0).round() as u32).to_be_bytes());
    b
}

/// CDJ status packet (type 0x0a, nexus 2 layout), used by the simulator and tests.
pub fn status(s: &Status) -> Vec<u8> {
    let mut b = header_50001(0x0a, &s.name, s.number, 0x11c);
    b[0x20] = 0x03;
    b[0x24] = s.number;
    b[0x27] = s.playing as u8;
    b[0x28] = s.track_player;
    b[0x29] = s.track_slot;
    b[0x2a] = s.track_type;
    b[0x2c..0x30].copy_from_slice(&s.track_id.to_be_bytes());
    b[0x7b] = s.play_state;
    let fw = s.firmware.as_bytes();
    b[0x7c..0x7c + fw.len().min(4)].copy_from_slice(&fw[..fw.len().min(4)]);
    b[0x89] = 0x84
        | if s.playing { 0x40 } else { 0 }
        | if s.master { 0x20 } else { 0 }
        | if s.sync { 0x10 } else { 0 }
        | if s.on_air { 0x08 } else { 0 };
    let p = pitch_raw(s.pitch).to_be_bytes();
    for o in [0x8c, 0x98, 0xc0, 0xc4] {
        b[o..o + 4].copy_from_slice(&p);
    }
    let bpm = s.track_bpm.map_or(0xffff, |v| (v * 100.0).round() as u16);
    b[0x92..0x94].copy_from_slice(&bpm.to_be_bytes());
    b[0x9e] = if s.master { 1 } else { 0 };
    b[0x9f] = 0xff;
    b[0xa0..0xa4].copy_from_slice(&s.beat.unwrap_or(0xffff_ffff).to_be_bytes());
    b[0xa4..0xa6].copy_from_slice(&s.beats_to_cue.unwrap_or(0x01ff).to_be_bytes());
    b[0xa6] = s.beat_in_bar;
    b
}

/// Mixer status packet (type 0x29), used by the simulator and tests.
pub fn mixer_status(name: &str, number: u8, master: bool, bpm: f32, beat_in_bar: u8) -> Vec<u8> {
    let mut b = header_50001(0x29, name, number, 0x38);
    b[0x24] = number;
    b[0x27] = if master { 0xf0 } else { 0xd0 };
    b[0x28..0x2c].copy_from_slice(&PITCH_ZERO.to_be_bytes());
    b[0x2e..0x30].copy_from_slice(&((bpm * 100.0).round() as u16).to_be_bytes());
    b[0x30..0x34].copy_from_slice(&PITCH_ZERO.to_be_bytes());
    b[0x36] = 0xff;
    b[0x37] = beat_in_bar;
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured keep-alive of a CDJ-2000nexus (from alphatheta-connect's test data, MIT).
    const CAPTURED_KEEP_ALIVE: &[u8] = &[
        0x51, 0x73, 0x70, 0x74, 0x31, 0x57, 0x6d, 0x4a, 0x4f, 0x4c, 0x06, 0x00, 0x43, 0x44, 0x4a,
        0x2d, 0x32, 0x30, 0x30, 0x30, 0x6e, 0x65, 0x78, 0x75, 0x73, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x01, 0x02, 0x00, 0x36, 0x02, 0x02, 0x74, 0x5e, 0x1c, 0x57, 0x82, 0xd8, 0x0a,
        0x00, 0x00, 0xcf, 0x04, 0x00, 0x00, 0x00, 0x01, 0x00,
    ];

    /// Captured status of a CDJ-2000nexus with no track (alphatheta-connect test data, MIT):
    /// only the non-zero bytes, as (offset, bytes).
    fn captured_status() -> Vec<u8> {
        let mut b = vec![0u8; 0x11c];
        let parts: &[(usize, &[u8])] = &[
            (0x00, b"Qspt1WmJOL\x0aCDJ-2000nexus"),
            (0x1f, &[0x01, 0x04, 0x03, 0x00, 0xf8, 0x03]),
            (0x68, &[0x01, 0x00, 0x04, 0x04, 0x00, 0x00, 0x00, 0x04]),
            (0x73, &[0x04, 0x00, 0x01]),
            (0x7c, b"1.43"),
            (
                0x87,
                &[0x03, 0x00, 0x84, 0xff, 0xfe, 0x00, 0x10, 0x00, 0x00],
            ),
            (
                0x90,
                &[0x7f, 0xff, 0xff, 0xff, 0x7f, 0xff, 0xff, 0xff, 0x00, 0x10],
            ),
            (0x9f, &[0xff, 0xff, 0xff, 0xff, 0xff, 0x01, 0xff]),
            (0xb6, &[0x01]),
            (0xc1, &[0x10, 0x00, 0x00, 0x00, 0x10]),
            (0xc9, &[0x01, 0x21, 0x7c, 0x0f, 0x01]),
            (0x113, &[0x05]),
        ];
        for (o, bytes) in parts {
            b[*o..*o + bytes.len()].copy_from_slice(bytes);
        }
        b
    }

    #[test]
    fn captured_keep_alive() {
        let Some(Packet::KeepAlive(k)) = parse(ANNOUNCE_PORT, CAPTURED_KEEP_ALIVE) else {
            panic!("not parsed");
        };
        assert_eq!(k.name, "CDJ-2000nexus");
        assert_eq!(k.number, 2);
        assert_eq!(k.kind, Kind::Player);
        assert_eq!(k.ip, Ipv4Addr::new(10, 0, 0, 207));
        assert_eq!(k.mac, [0x74, 0x5e, 0x1c, 0x57, 0x82, 0xd8]);
    }

    #[test]
    fn captured_status_without_track() {
        let Some(Packet::Status(s)) = parse(STATUS_PORT, &captured_status()) else {
            panic!("not parsed");
        };
        assert_eq!(s.number, 3);
        assert_eq!(s.name, "CDJ-2000nexus");
        assert_eq!(s.firmware, "1.43");
        assert_eq!(s.play_state, 0x00);
        assert!(!s.playing && !s.master && !s.on_air);
        assert_eq!(s.pitch, 0.0);
        assert_eq!(s.track_bpm, None);
        assert_eq!(s.beat, None);
        assert_eq!(s.beats_to_cue, None);
        assert_eq!(s.beat_in_bar, 0);
    }

    #[test]
    fn keep_alive_round_trip() {
        let mac = [1, 2, 3, 4, 5, 6];
        let ip = Ipv4Addr::new(192, 168, 1, 50);
        let b = keep_alive("Boothmate", 7, Kind::Player, mac, ip);
        assert_eq!(b.len(), 0x36);
        let Some(Packet::KeepAlive(k)) = parse(ANNOUNCE_PORT, &b) else {
            panic!()
        };
        assert_eq!(
            (k.name.as_str(), k.number, k.kind, k.mac, k.ip),
            ("Boothmate", 7, Kind::Player, mac, ip)
        );
        // Same layout as the captured packet apart from the variable fields.
        assert_eq!(&b[0x20..0x24], &CAPTURED_KEEP_ALIVE[0x20..0x24]);
    }

    #[test]
    fn beat_round_trip() {
        let b = beat("CDJ-3000", 2, 128.0, 2.5, 3);
        let Some(Packet::Beat(p)) = parse(BEAT_PORT, &b) else {
            panic!()
        };
        assert_eq!(p.number, 2);
        assert_eq!(p.name, "CDJ-3000");
        assert_eq!(p.track_bpm, Some(128.0));
        assert_eq!(p.pitch, 2.5);
        assert_eq!(p.beat_in_bar, 3);
        assert_eq!(p.next_beat_ms, Some(468));
        assert_eq!(p.next_bar_ms, Some(468 * 2));
        // The same bytes on another port mean something else.
        assert_eq!(parse(STATUS_PORT, &b), None);
    }

    #[test]
    fn status_round_trip() {
        let s = Status {
            number: 1,
            name: "CDJ-2000NXS2".into(),
            track_player: 1,
            track_slot: 3,
            track_type: 1,
            track_id: 4711,
            play_state: 3,
            firmware: "1.85".into(),
            playing: true,
            master: true,
            sync: false,
            on_air: true,
            pitch: -1.5,
            track_bpm: Some(124.0),
            beat: Some(129),
            beats_to_cue: Some(16),
            beat_in_bar: 1,
        };
        assert_eq!(parse(STATUS_PORT, &status(&s)), Some(Packet::Status(s)));
    }

    #[test]
    fn position_and_mixer() {
        let Some(Packet::Position(p)) =
            parse(BEAT_PORT, &position("CDJ-3000", 2, 300, 61_234, 126.4))
        else {
            panic!()
        };
        assert_eq!(
            (p.number, p.track_len_s, p.playhead_ms, p.bpm),
            (2, 300, 61_234, Some(126.4))
        );
        let Some(Packet::MixerStatus(m)) = parse(
            STATUS_PORT,
            &mixer_status("DJM-900NXS2", 33, false, 128.0, 2),
        ) else {
            panic!()
        };
        assert_eq!(
            (m.number, m.master, m.bpm, m.beat_in_bar),
            (33, false, Some(128.0), 2)
        );
    }

    #[test]
    fn foreign_packets_are_ignored() {
        assert_eq!(parse(BEAT_PORT, b"hello"), None);
        assert_eq!(parse(BEAT_PORT, &[0u8; 0x60]), None);
        let mut short = beat("X", 1, 120.0, 0.0, 1);
        short.truncate(0x40);
        assert_eq!(parse(BEAT_PORT, &short), None);
    }
}
