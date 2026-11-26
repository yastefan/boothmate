//! The simulator's database server: answers dbserver requests for the two simulated tracks
//! with made-up but well-formed data (metadata, beat grid, colour waveforms, cues, phrases).
//! It runs over an in-memory stream, so the real client code is used end to end.

use super::analysis::{pssi_mask, DETAIL_PER_S};
use super::dbserver::{self as db, Field, Message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct SimTrack {
    pub id: u32,
    pub title: &'static str,
    pub artist: &'static str,
    pub key: &'static str,
    pub genre: &'static str,
    pub bpm: f32,
    pub len_s: u32,
    /// rekordbox mood: 1 high, 2 mid, 3 low.
    pub mood: u16,
    /// Phrases as (kind, bars).
    pub phrases: &'static [(u16, u32)],
    /// The PSSI tag is masked like a rekordbox 6 export.
    pub masked: bool,
}

const TRACKS: [SimTrack; 2] = [
    SimTrack {
        id: 101,
        title: "Warehouse Lights (Extended Mix)",
        artist: "Simulated Artist",
        key: "8A",
        genre: "Techno",
        bpm: 128.0,
        len_s: 372,
        mood: 1,
        // Intro, Up, Chorus, Down, Up, Chorus, Down, Chorus, Outro.
        phrases: &[
            (1, 16),
            (2, 16),
            (5, 32),
            (3, 16),
            (2, 16),
            (5, 32),
            (3, 8),
            (5, 24),
            (6, 16),
        ],
        masked: false,
    },
    SimTrack {
        id: 202,
        title: "Sunday Signal",
        artist: "Boothmate Test Band",
        key: "11B",
        genre: "House",
        bpm: 124.0,
        len_s: 318,
        mood: 2,
        // Intro, Verse 1, Verse 2, Chorus, Bridge, Verse 3, Chorus, Outro.
        phrases: &[
            (1, 16),
            (2, 16),
            (3, 16),
            (9, 32),
            (8, 16),
            (4, 16),
            (9, 32),
            (10, 16),
        ],
        masked: true,
    },
];

pub fn track(id: u32) -> Option<&'static SimTrack> {
    TRACKS.iter().find(|t| t.id == id)
}

impl SimTrack {
    fn beat_ms(&self) -> f64 {
        60_000.0 / self.bpm as f64
    }

    fn beats(&self) -> u32 {
        (self.len_s as f64 * 1000.0 / self.beat_ms()) as u32
    }

    /// Phrase kind at beat `n` (1-based), 0 after the last phrase.
    fn kind_at(&self, n: u32) -> u16 {
        let mut start = 1;
        for &(k, bars) in self.phrases {
            if n < start + bars * 4 {
                return k;
            }
            start += bars * 4;
        }
        0
    }

    fn energy(&self, kind: u16) -> (f64, bool) {
        // (loudness, kick drum)
        match (self.mood, kind) {
            (_, 0) => (0.05, false),
            (_, 1) => (0.55, true),
            (1, 2) | (2, 2..=7) => (0.7, true),
            (1, 3) | (2, 8) => (0.35, false),
            (1, 5) | (2, 9) => (1.0, true),
            _ => (0.5, true),
        }
    }

    /// Low, mid and high energy (0-1) at half frame `hf`.
    fn bands(&self, hf: u32) -> [f64; 3] {
        let t = hf as f64 / DETAIL_PER_S as f64 * 1000.0;
        let beats = t / self.beat_ms();
        let n = beats as u32 + 1;
        let frac = beats.fract();
        let (e, kick) = self.energy(self.kind_at(n));
        let noise = (hash(hf) % 1000) as f64 / 1000.0;
        let low = if kick {
            (-frac * 6.0).exp()
        } else {
            0.12 + 0.08 * noise
        };
        let mid = 0.2 + 0.15 * noise + 0.15 * (beats * 0.25 * std::f64::consts::TAU).sin().abs();
        let high = if (frac - 0.5).abs() < 0.1 {
            0.7
        } else {
            0.15 + 0.2 * noise
        };
        [low * e, mid * e, high * e]
    }

    pub fn grid_blob(&self) -> Vec<u8> {
        let mut b = vec![0u8; 0x14];
        let bpm = (self.bpm * 100.0) as u16;
        for i in 0..self.beats() {
            let mut e = [0xffu8; 16];
            e[..2].copy_from_slice(&((i % 4 + 1) as u16).to_le_bytes());
            e[2..4].copy_from_slice(&bpm.to_le_bytes());
            let ms = (i as f64 * self.beat_ms()) as u32;
            e[4..8].copy_from_slice(&ms.to_le_bytes());
            b.extend(e);
        }
        b
    }

    fn tag(cc: &str, header: &[u8], body: &[u8]) -> Vec<u8> {
        let mut t = cc.as_bytes().to_vec();
        let lh = 12 + header.len() as u32;
        t.extend(lh.to_be_bytes());
        t.extend((lh + body.len() as u32).to_be_bytes());
        t.extend(header);
        t.extend(body);
        t
    }

    fn wave_header(entry: u32, n: u32, unknown: u32) -> Vec<u8> {
        [entry, n, unknown]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect()
    }

    /// `PWV5`: colour detail, two bytes per half frame.
    pub fn color_detail(&self) -> Vec<u8> {
        let n = self.len_s * DETAIL_PER_S;
        let mut body = Vec::with_capacity(n as usize * 2);
        for hf in 0..n {
            let [l, m, h] = self.bands(hf);
            let c = |v: f64| (v * 7.0).round().clamp(0.0, 7.0) as u16;
            let height = (l.max(m).max(h) * 31.0).round().clamp(0.0, 31.0) as u16;
            let v = c(l) << 13 | c(m) << 10 | c(h) << 7 | height << 2;
            body.extend(v.to_be_bytes());
        }
        Self::tag("PWV5", &Self::wave_header(2, n, 0x0096_0305), &body)
    }

    /// `PWV4`: colour preview, 1200 columns of 6 bytes.
    pub fn color_preview(&self) -> Vec<u8> {
        let total = self.len_s * DETAIL_PER_S;
        let mut body = vec![];
        for col in 0..1200u32 {
            let (a, b) = (total * col / 1200, total * (col + 1) / 1200);
            let mut peak = [0f64; 3];
            for hf in (a..b.max(a + 1)).step_by(3) {
                let v = self.bands(hf);
                for i in 0..3 {
                    peak[i] = peak[i].max(v[i]);
                }
            }
            let q = |v: f64| (v * 120.0) as u8;
            body.extend([
                q(peak[1]),
                q(peak[2]),
                q(peak[0]),
                q(peak[0]),
                q(peak[1]),
                q(peak[2]),
            ]);
        }
        Self::tag("PWV4", &Self::wave_header(6, 1200, 0), &body)
    }

    /// `PSSI`: song structure, masked like rekordbox 6 if `masked`.
    pub fn pssi(&self) -> Vec<u8> {
        let mut entries = vec![];
        let mut beat = 1u32;
        for (i, &(kind, bars)) in self.phrases.iter().enumerate() {
            let mut e = [0u8; 24];
            e[0..2].copy_from_slice(&(i as u16 + 1).to_be_bytes());
            e[2..4].copy_from_slice(&(beat as u16).to_be_bytes());
            e[4..6].copy_from_slice(&kind.to_be_bytes());
            e[7] = 1; // k1: "Intro 1", "Chorus 1"
            if kind == 5 || kind == 9 {
                // Fill-in on the last 4 beats of a chorus.
                e[0x15] = 1;
                e[0x16..0x18].copy_from_slice(&((beat + bars * 4 - 4) as u16).to_be_bytes());
            }
            entries.extend(e);
            beat += bars * 4;
        }
        let mut header = vec![];
        header.extend(24u32.to_be_bytes());
        header.extend((self.phrases.len() as u16).to_be_bytes());
        header.extend(self.mood.to_be_bytes());
        header.extend([0u8; 6]);
        header.extend((beat as u16).to_be_bytes());
        header.extend([0, 0, 1, 0]); // unknown, bank Cool, unknown
        let mut t = Self::tag("PSSI", &header, &entries);
        if self.masked {
            pssi_mask(&mut t);
        }
        t
    }

    /// Extended cue list: hot cue A on the first beat, B on the first chorus, a memory point
    /// on the first down phrase and an 8-beat loop before the outro.
    pub fn cues_ext(&self) -> (Vec<u8>, u32) {
        let hf = |beat: u32| ((beat - 1) as f64 * self.beat_ms() * 0.15) as u32;
        let start_of = |kind: u16| {
            let mut b = 1;
            for &(k, bars) in self.phrases {
                if k == kind {
                    return Some(b);
                }
                b += bars * 4;
            }
            None
        };
        let chorus = start_of(if self.mood == 1 { 5 } else { 9 }).unwrap_or(65);
        let down = start_of(if self.mood == 1 { 3 } else { 8 }).unwrap_or(129);
        let outro = start_of(if self.mood == 1 { 6 } else { 10 }).unwrap_or(257);
        // (hot cue, 1 memory / 2 loop, start, loop end, comment, colour)
        type Entry<'a> = (u8, u8, u32, u32, &'a str, [u8; 3]);
        let list: [Entry; 4] = [
            (1, 1, hf(1), 0, "Start", [0x30, 0xd2, 0x6e]),
            (2, 1, hf(chorus), 0, "Drop", [0xe1, 0x3b, 0x3b]),
            (0, 1, hf(down), 0, "", [0; 3]),
            (0, 2, hf(outro - 8), hf(outro), "Loop", [0; 3]),
        ];
        let mut b = vec![];
        for (hot, kind, cue, end, comment, rgb) in list {
            let units: Vec<u16> = comment.encode_utf16().chain([0]).collect();
            let lc = if comment.is_empty() {
                0
            } else {
                units.len() * 2
            };
            let len = 0x4e + lc + 4 + 0x10;
            let mut e = vec![0u8; len];
            e[0..4].copy_from_slice(&(len as u32).to_le_bytes());
            e[4] = hot;
            e[6] = kind;
            e[0x0c..0x10].copy_from_slice(&cue.to_le_bytes());
            e[0x10..0x14].copy_from_slice(&end.to_le_bytes());
            e[0x48..0x4a].copy_from_slice(&(lc as u16).to_le_bytes());
            if lc > 0 {
                for (i, u) in units.iter().enumerate() {
                    e[0x4a + i * 2..0x4c + i * 2].copy_from_slice(&u.to_le_bytes());
                }
            }
            if hot > 0 {
                let c = 0x4e + lc;
                e[c] = 1;
                e[c + 1..c + 4].copy_from_slice(&rgb);
            }
            b.extend(e);
        }
        (b, list.len() as u32)
    }

    fn menu(&self) -> Vec<(u32, u32, String)> {
        // (type, id, label)
        vec![
            (0x04, self.id, self.title.into()),
            (0x07, 7, self.artist.into()),
            (0x02, 3, String::new()),
            (0x0b, self.len_s, String::new()),
            (0x0d, (self.bpm * 100.0) as u32, String::new()),
            (0x23, 0, "Boothmate simulator".into()),
            (0x0f, 1, self.key.into()),
            (0x0a, 4, String::new()),
            (0x13, 0, String::new()),
            (0x06, 2, self.genre.into()),
        ]
    }
}

fn hash(n: u32) -> u32 {
    let mut x = n.wrapping_mul(0x9e37_79b9) ^ 0x85eb_ca6b;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2c1b_3c6d);
    x ^ (x >> 12)
}

fn data_answer(tx: u32, kind: u16, req: u16, blob: Option<Vec<u8>>, last: u32) -> Message {
    let blob = blob.unwrap_or_default();
    Message::new(
        tx,
        kind,
        vec![
            Field::N4(req as u32),
            Field::N4(0),
            Field::N4(blob.len() as u32),
            Field::Blob(blob),
            Field::N4(last),
        ],
    )
}

/// Serves one client connection as player `me` (the simulated player with the media).
pub async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut s: S, me: u8) -> std::io::Result<()> {
    let mut g = [0u8; 5];
    s.read_exact(&mut g).await?;
    s.write_all(&db::GREETING).await?;
    let mut buf = vec![];
    let mut menu: Vec<(u32, u32, String)> = vec![];
    loop {
        let (m, n) = loop {
            match db::decode(&buf) {
                Ok(Some(x)) => break x,
                Ok(None) => {}
                Err(_) => return Ok(()),
            }
            let mut chunk = [0u8; 4096];
            let n = s.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        buf.drain(..n);
        let id = m.num(1).unwrap_or(0);
        let t = track(id);
        let mut out = vec![];
        match m.kind {
            db::SETUP => out.push(Message::new(
                m.tx,
                db::OK,
                vec![Field::N4(0), Field::N4(me as u32)],
            )),
            db::METADATA => {
                menu = t.map(SimTrack::menu).unwrap_or_default();
                let count = if t.is_some() {
                    menu.len() as u32
                } else {
                    0xffff_ffff
                };
                out.push(Message::new(
                    m.tx,
                    db::OK,
                    vec![Field::N4(db::METADATA as u32), Field::N4(count)],
                ));
            }
            db::RENDER => {
                out.push(Message::new(
                    m.tx,
                    db::MENU_HEADER,
                    vec![Field::N4(1), Field::N4(0)],
                ));
                for (ty, id, label) in menu.drain(..) {
                    let len = (label.encode_utf16().count() as u32 + 1) * 2;
                    out.push(Message::new(
                        m.tx,
                        db::MENU_ITEM,
                        vec![
                            Field::N4(0),
                            Field::N4(id),
                            Field::N4(len),
                            Field::Str(label),
                            Field::N4(2),
                            Field::Str(String::new()),
                            Field::N4(ty),
                            Field::N4(0),
                            Field::N4(0),
                            Field::N4(0),
                            Field::N4(0),
                            Field::N4(0),
                        ],
                    ));
                }
                out.push(Message::new(m.tx, db::MENU_FOOTER, vec![]));
            }
            db::BEAT_GRID => out.push(data_answer(
                m.tx,
                0x4602,
                m.kind,
                t.map(SimTrack::grid_blob),
                0,
            )),
            db::ANLZ_TAG => {
                let cc = m.num(2).unwrap_or(0);
                let blob = t.and_then(|t| {
                    if cc == db::fourcc("PWV4") {
                        Some(t.color_preview())
                    } else if cc == db::fourcc("PWV5") {
                        Some(t.color_detail())
                    } else if cc == db::fourcc("PSSI") {
                        Some(t.pssi())
                    } else {
                        None
                    }
                });
                out.push(data_answer(m.tx, 0x4f02, m.kind, blob, 0));
            }
            db::CUES_EXT => {
                let (b, n) = t.map(SimTrack::cues_ext).unwrap_or_default();
                out.push(data_answer(m.tx, 0x4e02, m.kind, Some(b), n));
            }
            _ => out.push(Message::new(m.tx, 0x4003, vec![Field::N4(m.kind as u32)])),
        }
        for o in out {
            s.write_all(&o.encode()).await?;
        }
    }
}
