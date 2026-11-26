//! Track data from the dbserver answers: metadata, beat grid, waveforms, cues and rekordbox
//! phrases, in a form the web UI and the tracker can use directly.
//!
//! Formats after the Deep Symmetry analysis (dbserver blobs and rekordbox ANLZ tags). The
//! beat grid, the blue waveforms, the old cue list and the metadata menu are checked against
//! captured CDJ-2000nexus answers; the nxs2 colour waveforms, extended cues and phrases (PSSI)
//! only against the documentation so far.

use super::dbserver::Message;
use serde::Serialize;

/// Waveform detail resolution: half frames, 150 per second.
pub const DETAIL_PER_S: u32 = 150;

/// Everything Boothmate knows about one track.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TrackData {
    /// `/api/cdj/track/<key>`.
    pub key: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    /// Musical key as rekordbox shows it, e.g. `12A` or `Am`.
    pub musical_key: Option<String>,
    pub comment: Option<String>,
    pub color: Option<String>,
    pub rating: Option<u8>,
    pub duration_s: Option<u32>,
    pub bpm: Option<f32>,
    pub grid: Option<Grid>,
    /// Whole-track overview.
    pub preview: Option<Wave>,
    /// Scrolling detail, 150 columns per second.
    pub detail: Option<Wave>,
    pub cues: Vec<Cue>,
    pub phrases: Vec<Phrase>,
    /// rekordbox phrase mood: high, mid, low.
    pub mood: Option<String>,
    /// What the player could not deliver.
    pub missing: Vec<String>,
}

/// The beat grid: time of every beat (ms at 0 % pitch) and its beat in bar.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Grid {
    pub ms: Vec<u32>,
    pub bar: Vec<u8>,
}

/// A waveform, base64 of four bytes per column: height, red, green, blue (0-255 each).
#[derive(Debug, Clone, Default, Serialize)]
pub struct Wave {
    pub columns: u32,
    /// Colour (nxs2) or blue (older players).
    pub color: bool,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Cue {
    /// Hot cue 1 = A, 0 = memory point.
    pub hot: u8,
    pub ms: u32,
    /// Loop end.
    pub loop_ms: Option<u32>,
    /// `#rrggbb`.
    pub color: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Phrase {
    /// First beat (1-based, like the beat numbers of the players).
    pub beat: u32,
    /// First beat after the phrase.
    pub end: u32,
    /// As rekordbox shows it, e.g. `Chorus 1`, `Verse 2`.
    pub label: String,
    /// For colours: intro, up, down, chorus, outro, verse, bridge.
    pub kind: &'static str,
    /// First beat of the fill-in at the end of the phrase.
    pub fill: Option<u32>,
}

impl TrackData {
    /// Beat number (1-based) at `ms` of track time.
    pub fn beat_at(&self, ms: u32) -> Option<u32> {
        let g = self.grid.as_ref()?;
        Some(g.ms.partition_point(|&t| t <= ms) as u32)
    }

    /// Track time of beat `n` (1-based).
    pub fn beat_ms(&self, n: u32) -> Option<u32> {
        let g = self.grid.as_ref()?;
        g.ms.get(n.checked_sub(1)? as usize).copied()
    }

    /// The phrase that contains beat `n`, and its index.
    pub fn phrase_at(&self, n: u32) -> Option<(usize, &Phrase)> {
        self.phrases
            .iter()
            .enumerate()
            .find(|(_, p)| p.beat <= n && n < p.end)
    }
}

// Metadata menu ----------------------------------------------------------------------------------

/// Menu item types (argument 7).
mod item {
    pub const ALBUM: u32 = 0x02;
    pub const TITLE: u32 = 0x04;
    pub const GENRE: u32 = 0x06;
    pub const ARTIST: u32 = 0x07;
    pub const RATING: u32 = 0x0a;
    pub const DURATION: u32 = 0x0b;
    pub const TEMPO: u32 = 0x0d;
    pub const KEY: u32 = 0x0f;
    pub const COMMENT: u32 = 0x23;
}

const COLORS: [&str; 9] = [
    "", "pink", "red", "orange", "yellow", "green", "aqua", "blue", "purple",
];

/// Fills title, artist and the rest from the items of a metadata menu.
pub fn apply_metadata(t: &mut TrackData, items: &[Message]) {
    for m in items {
        let label = m.str(3).filter(|s| !s.is_empty()).map(str::to_string);
        let id = m.num(1).unwrap_or(0);
        match m.num(6).unwrap_or(0) & 0xffff {
            item::TITLE => t.title = label,
            item::ARTIST => t.artist = label,
            item::ALBUM => t.album = label,
            item::GENRE => t.genre = label,
            item::KEY => t.musical_key = label,
            item::COMMENT => t.comment = label,
            item::DURATION => t.duration_s = Some(id),
            item::TEMPO => t.bpm = Some(id as f32 / 100.0),
            item::RATING => t.rating = Some(id.min(5) as u8),
            c @ 0x13..=0x1b => {
                t.color = (c > 0x13)
                    .then(|| label.unwrap_or_else(|| COLORS[(c - 0x13) as usize].to_string()));
            }
            _ => {}
        }
    }
}

// Beat grid --------------------------------------------------------------------------------------

fn le16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn le32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn be16(d: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([d[o], d[o + 1]])
}
fn be32(d: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

/// Beat grid blob: 16-byte entries from 0x14, beat in bar and time little-endian.
pub fn grid(blob: &[u8]) -> Option<Grid> {
    let entries = blob.get(0x14..)?;
    let mut g = Grid::default();
    for e in entries.as_chunks::<16>().0.iter() {
        let bb = le16(e, 0);
        if !(1..=4).contains(&bb) {
            continue;
        }
        g.bar.push(bb as u8);
        g.ms.push(le32(e, 4));
    }
    (!g.ms.is_empty()).then_some(g)
}

// Waveforms --------------------------------------------------------------------------------------

/// Blue of the older players, mixed towards white by `white` (0-7).
fn blue(white: u8) -> [u8; 3] {
    let w = white.min(7) as u32;
    let mix = |a: u32, b: u32| ((a * (7 - w) + b * w) / 7) as u8;
    [mix(0x1e, 0xe8), mix(0x7c, 0xf0), mix(0xe8, 0xff)]
}

fn wave(cols: Vec<[u8; 4]>, color: bool) -> Option<Wave> {
    if cols.is_empty() {
        return None;
    }
    let raw: Vec<u8> = cols.iter().flatten().copied().collect();
    Some(Wave {
        columns: cols.len() as u32,
        color,
        data: base64(&raw),
    })
}

/// Blue preview (request 0x2004): 400 columns of height (0-31) and whiteness (0-7).
pub fn mono_preview(blob: &[u8]) -> Option<Wave> {
    let cols = blob
        .get(..800)?
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            let [r, g, b] = blue(c[1]);
            [c[0].min(31) * 8, r, g, b]
        })
        .collect();
    wave(cols, false)
}

/// Blue detail (request 0x2904): a short header, then one byte per half frame
/// (whiteness in the top 3 bits, height in the low 5).
pub fn mono_detail(blob: &[u8]) -> Option<Wave> {
    let n = blob.get(..4).map(|b| le32(b, 0) as usize).unwrap_or(0);
    // The header holds the entry count; captured CDJ-2000nexus answers have 20 bytes before.
    let head = if n <= blob.len() && blob.len() - n <= 32 {
        blob.len() - n
    } else {
        20
    };
    let cols = blob
        .get(head..)?
        .iter()
        .map(|&b| {
            let [r, g, bl] = blue(b >> 5);
            [(b & 0x1f) * 8, r, g, bl]
        })
        .collect();
    wave(cols, false)
}

/// The body of an ANLZ tag `cc` in a dbserver blob (the blob holds the whole tag).
pub fn tag<'a>(blob: &'a [u8], cc: &str) -> Option<&'a [u8]> {
    let at = blob.windows(4).take(64).position(|w| w == cc.as_bytes())?;
    let t = &blob[at..];
    let len = (be32(t, 8) as usize).clamp(12, t.len());
    Some(&t[..len])
}

/// Colour preview (`PWV4`): 6 bytes per column. Bytes 3-5 are the energy of lows, mids and
/// highs; they become red, green and blue, the loudest of them the height.
pub fn color_preview(blob: &[u8]) -> Option<Wave> {
    let t = tag(blob, "PWV4")?;
    let n = be32(t, 0x10) as usize;
    let body = t.get(0x18..)?;
    let cols: Vec<[u8; 3]> = body
        .as_chunks::<6>()
        .0
        .iter()
        .take(n)
        .map(|c| [c[3], c[4], c[5]])
        .collect();
    let top = cols.iter().flatten().copied().max().unwrap_or(0).max(1) as u32;
    let cols = cols
        .into_iter()
        .map(|[l, m, h]| {
            let peak = l.max(m).max(h).max(1) as u32;
            let s = |v: u8| (v as u32 * 255 / peak) as u8;
            [(peak * 255 / top) as u8, s(l), s(m), s(h)]
        })
        .collect();
    wave(cols, true)
}

/// Colour detail (`PWV5`): per half frame a big-endian u16 with 3 bits each of red, green
/// and blue and 5 bits of height.
pub fn color_detail(blob: &[u8]) -> Option<Wave> {
    let t = tag(blob, "PWV5")?;
    let n = be32(t, 0x10) as usize;
    let cols = t
        .get(0x18..)?
        .as_chunks::<2>()
        .0
        .iter()
        .take(n)
        .map(|c| {
            let v = u16::from_be_bytes([c[0], c[1]]);
            let ch = |shift: u16| ((v >> shift) & 7) as u8 * 36;
            [((v >> 2) & 0x1f) as u8 * 8, ch(13), ch(10), ch(7)]
        })
        .collect();
    wave(cols, true)
}

// Cues -------------------------------------------------------------------------------------------

/// Half frames (1/150 s) to ms.
fn hf_ms(hf: u32) -> u32 {
    (hf as u64 * 1000 / 150) as u32
}

/// Cue list (request 0x2104): 36-byte entries. Older format, hot cues A-C only.
pub fn cues(blob: &[u8]) -> Vec<Cue> {
    let mut out: Vec<Cue> = blob
        .as_chunks::<36>()
        .0
        .iter()
        .filter(|e| e[0] != 0 || e[1] != 0)
        .map(|e| Cue {
            hot: e[2],
            ms: hf_ms(le32(e, 12)),
            loop_ms: (e[0] != 0).then(|| hf_ms(le32(e, 16))),
            color: None,
            comment: None,
        })
        .collect();
    out.sort_by_key(|c| c.ms);
    out
}

/// Extended cue list (request 0x2b04): variable-length entries with colours and comments.
pub fn cues_ext(blob: &[u8]) -> Vec<Cue> {
    let mut out = vec![];
    let mut o = 0;
    while o + 0x14 <= blob.len() {
        let len = le32(blob, o) as usize;
        if len < 0x14 || o + len > blob.len() {
            break;
        }
        let e = &blob[o..o + len];
        o += len;
        let (hot, kind) = (e[4], e[6]);
        if hot == 0 && kind == 0 {
            continue;
        }
        let mut cue = Cue {
            hot,
            ms: hf_ms(le32(e, 0x0c)),
            loop_ms: (kind == 2).then(|| hf_ms(le32(e, 0x10))),
            color: None,
            comment: None,
        };
        if e.len() >= 0x4a {
            let lc = le16(e, 0x48) as usize;
            if lc >= 2 && e.len() >= 0x4a + lc {
                cue.comment = utf16(&e[0x4a..0x4a + lc - 2]).filter(|s| !s.is_empty());
            }
            let c = 0x4e + lc;
            if hot > 0 && e.len() >= c + 4 && e[c..c + 4] != [0, 0, 0, 0] {
                cue.color = Some(format!("#{:02x}{:02x}{:02x}", e[c + 1], e[c + 2], e[c + 3]));
            }
        }
        out.push(cue);
    }
    out.sort_by_key(|c| c.ms);
    out
}

/// UTF-16 of unknown byte order: ASCII text has its zero bytes on the high side.
fn utf16(b: &[u8]) -> Option<String> {
    let le = b.as_chunks::<2>().0.iter().filter(|c| c[1] == 0).count()
        >= b.as_chunks::<2>().0.iter().filter(|c| c[0] == 0).count();
    let units: Vec<u16> = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| {
            if le {
                u16::from_le_bytes([c[0], c[1]])
            } else {
                u16::from_be_bytes([c[0], c[1]])
            }
        })
        .collect();
    Some(
        String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string(),
    )
}

// Phrases (song structure) -----------------------------------------------------------------------

/// rekordbox 6 garbles the `PSSI` tag with this mask (plus the entry count) from byte 0x12.
const PSSI_MASK: [u8; 19] = [
    0xcb, 0xe1, 0xee, 0xfa, 0xe5, 0xee, 0xad, 0xee, 0xe9, 0xd2, 0xe9, 0xeb, 0xe1, 0xe9, 0xf3, 0xe8,
    0xe9, 0xf4, 0xe1,
];

/// XORs the mask over a `PSSI` tag (it is its own inverse).
pub fn pssi_mask(t: &mut [u8]) {
    if t.len() < 0x12 {
        return;
    }
    let n = be16(t, 0x10) as u8;
    for (i, b) in t[0x12..].iter_mut().enumerate() {
        *b ^= PSSI_MASK[i % PSSI_MASK.len()].wrapping_add(n);
    }
}

/// Song structure (`PSSI` tag): mood and phrases.
pub fn phrases(blob: &[u8]) -> Option<(String, Vec<Phrase>)> {
    let mut t = tag(blob, "PSSI")?.to_vec();
    if t.len() < 0x20 {
        return None;
    }
    if !(1..=3).contains(&be16(&t, 0x12)) {
        pssi_mask(&mut t);
    }
    let mood = be16(&t, 0x12);
    if !(1..=3).contains(&mood) {
        return None;
    }
    let count = be16(&t, 0x10) as usize;
    let size = (be32(&t, 0x0c) as usize).max(24);
    let end = be16(&t, 0x1a) as u32;
    let entries: Vec<&[u8]> = t[0x20..].chunks_exact(size).take(count).collect();
    let mut out = vec![];
    for (i, e) in entries.iter().enumerate() {
        let beat = be16(e, 2) as u32;
        let next = entries
            .get(i + 1)
            .map(|n| be16(n, 2) as u32)
            .unwrap_or(end.max(beat + 1));
        let (label, kind) = phrase_label(mood, be16(e, 4), e[7], e[9], e[0x13]);
        out.push(Phrase {
            beat,
            end: next,
            label,
            kind,
            fill: (e[0x15] != 0)
                .then(|| be16(e, 0x16) as u32)
                .filter(|&f| f > beat),
        });
    }
    let mood = ["high", "mid", "low"][mood as usize - 1].to_string();
    Some((mood, out))
}

fn phrase_label(mood: u16, kind: u16, k1: u8, k2: u8, k3: u8) -> (String, &'static str) {
    let n = |one: bool| if one { "1" } else { "2" };
    let (label, class) = match (mood, kind) {
        (1, 1) => (format!("Intro {}", n(k1 == 1)), "intro"),
        (1, 2) => (
            format!(
                "Up {}",
                match (k2, k3) {
                    (0, 0) => "1",
                    (0, _) => "2",
                    _ => "3",
                }
            ),
            "up",
        ),
        (1, 3) => ("Down".into(), "down"),
        (1, 5) => (format!("Chorus {}", n(k1 == 1)), "chorus"),
        (1, 6) => (format!("Outro {}", n(k1 == 1)), "outro"),
        (_, 1) => ("Intro".into(), "intro"),
        (2, 2..=7) => (format!("Verse {}", kind - 1), "verse"),
        (3, 2..=4) => ("Verse 1".into(), "verse"),
        (3, 5..=7) => ("Verse 2".into(), "verse"),
        (2 | 3, 8) => ("Bridge".into(), "bridge"),
        (2 | 3, 9) => ("Chorus".into(), "chorus"),
        (2 | 3, 10) => ("Outro".into(), "outro"),
        _ => (format!("Phrase {kind}"), "verse"),
    };
    (label, class)
}

// Base64 -----------------------------------------------------------------------------------------

pub fn base64(d: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn captured_beat_grid() {
        // Start of a CDJ-2000nexus beat grid answer (dysentery capture S06): 132.01 BPM.
        let mut b = hex("000008000e040000e040000001000000484a1106");
        for e in [
            "010091330f010000ffffffffffffffff",
            "02009133d5020000ffffffffffffffff",
            "030091339c040000ffffffffffffffff",
            "0400913362060000ffffffffffffffff",
            "0100913329080000ffffffffffffffff",
        ] {
            b.extend(hex(e));
        }
        let g = grid(&b).unwrap();
        assert_eq!(g.ms, vec![271, 725, 1180, 1634, 2089]);
        assert_eq!(g.bar, vec![1, 2, 3, 4, 1]);
    }

    #[test]
    fn captured_cue_list() {
        // Three hot cues from a CDJ-2000nexus (dysentery capture S06).
        let mut b = vec![];
        for (h, hf) in [(1u8, 0x28u32), (2, 0x2571), (3, 0x345b)] {
            let mut e = vec![0u8; 36];
            e[1] = 1;
            e[2] = h;
            e[12..16].copy_from_slice(&hf.to_le_bytes());
            b.extend(e);
        }
        let c = cues(&b);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].hot, c[0].ms), (1, 266));
        assert_eq!((c[1].hot, c[1].ms), (2, 63_900));
        assert!(c.iter().all(|c| c.loop_ms.is_none()));
    }

    #[test]
    fn mono_detail_header_from_count() {
        let mut b = vec![0u8; 20];
        b[..4].copy_from_slice(&3u32.to_le_bytes());
        b.extend([0xff, 0x1f, 0x00]);
        let w = mono_detail(&b).unwrap();
        assert_eq!(w.columns, 3);
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
