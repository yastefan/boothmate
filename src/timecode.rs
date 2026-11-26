//! Timecode: the playhead of the CDJs as Art-Net timecode (ArtTimeCode) for grandMA3.
//!
//! Experimental. grandMA3 receives ArtTimeCode in a timecode slot, so a timecode show can follow
//! the track that is playing. Stream 0 carries the tempo master, stream n carries player n.
//! Frames are only sent while a player plays, so the show's Auto Stop works like with LTC.
//!
//! The position comes from the CDJ-3000 position packets (exact). Older players only report the
//! beat number, so their position is estimated from beat number and track BPM: it is off by the
//! start of the beat grid and drifts on tracks with tempo changes.

use crate::cdj::tracker::{CdjState, Device};
use crate::state::now_ms;
use crate::App;
use serde::Serialize;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

/// ArtTimeCode type: 0 film 24, 1 EBU 25, 2 DF 29.97, 3 SMPTE 30.
pub fn type_for_fps(fps: u8) -> u8 {
    match fps {
        24 => 0,
        25 => 1,
        _ => 3,
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TimecodeState {
    /// Sending switched on (config or web UI).
    pub on: bool,
    pub target: String,
    pub fps: u8,
    pub error: Option<String>,
    pub packets: u64,
    /// What each stream sends right now.
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stream {
    pub id: u8,
    pub player: u8,
    /// `hh:mm:ss:ff`.
    pub tc: String,
    /// Position from the beat number, not from position packets.
    pub estimated: bool,
}

/// One ArtTimeCode packet (Art-Net 4, OpTimeCode 0x9700).
pub fn packet(stream: u8, ms: u64, fps: u8) -> [u8; 19] {
    let (h, m, s, f) = split(ms, fps);
    let mut b = [0u8; 19];
    b[..8].copy_from_slice(b"Art-Net\0");
    b[8..10].copy_from_slice(&0x9700u16.to_le_bytes());
    b[10] = 0; // ProtVerHi
    b[11] = 14; // ProtVerLo
    b[12] = 0; // Filler
    b[13] = stream;
    b[14] = f;
    b[15] = s;
    b[16] = m;
    b[17] = h;
    b[18] = type_for_fps(fps);
    b
}

pub fn split(ms: u64, fps: u8) -> (u8, u8, u8, u8) {
    let fps = fps.max(1) as u64;
    let s_total = ms / 1000;
    let f = (ms % 1000) * fps / 1000;
    (
        ((s_total / 3600) % 24) as u8,
        ((s_total / 60) % 60) as u8,
        (s_total % 60) as u8,
        f as u8,
    )
}

pub fn format(ms: u64, fps: u8) -> String {
    let (h, m, s, f) = split(ms, fps);
    format!("{h:02}:{m:02}:{s:02}:{f:02}")
}

/// Playhead of a playing player in ms, and whether it is estimated.
pub fn playhead(d: &Device, now: u64) -> Option<(u64, bool)> {
    if d.playing != Some(true) {
        return None;
    }
    if let Some(ms) = d.position_ms {
        return Some((ms as u64, false));
    }
    let (beat, bpm) = (d.beat?, d.track_bpm?);
    if beat == 0 || bpm <= 0.0 {
        return None;
    }
    let beat_ms = 60_000.0 / bpm as f64;
    // Time since the last beat, at the player's pitch, at most one beat.
    let since = d
        .last_beat
        .map(|t| now.saturating_sub(t) as f64 * (1.0 + d.pitch as f64 / 100.0))
        .unwrap_or(0.0)
        .min(beat_ms);
    Some((((beat - 1) as f64 * beat_ms + since) as u64, true))
}

/// The streams to send: tempo master on 0, every playing player on its number.
pub fn streams(cdj: &CdjState, now: u64, fps: u8) -> Vec<(Stream, u64)> {
    let mut out = vec![];
    let mut add = |id: u8, d: &Device| {
        if let Some((ms, estimated)) = playhead(d, now) {
            out.push((
                Stream {
                    id,
                    player: d.number,
                    tc: format(ms, fps),
                    estimated,
                },
                ms,
            ));
        }
    };
    if let Some(d) = cdj.master.and_then(|n| cdj.devices.get(&n)) {
        add(0, d);
    }
    for d in cdj
        .devices
        .values()
        .filter(|d| (1..=15).contains(&d.number))
    {
        add(d.number, d);
    }
    out
}

pub async fn run(app: Arc<App>) {
    let cfg = app.config.timecode.clone();
    app.update(|st| {
        st.timecode.on = cfg.enabled;
        st.timecode.target = cfg.target.clone();
        st.timecode.fps = cfg.fps;
    });
    let sock = match UdpSocket::bind("0.0.0.0:0").and_then(|s| {
        s.set_broadcast(true)?;
        Ok(s)
    }) {
        Ok(s) => s,
        Err(e) => {
            app.update(|st| st.timecode.error = Some(format!("cannot open socket: {e}")));
            return;
        }
    };
    let frame = Duration::from_millis(1000 / cfg.fps.max(1) as u64);
    let mut tick = tokio::time::interval(frame);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ui_last = 0u64;
    loop {
        tick.tick().await;
        let now = now_ms();
        let (on, list) = {
            let st = app.state.lock().unwrap();
            let list = if st.timecode.on {
                streams(&st.cdj, now, cfg.fps)
            } else {
                vec![]
            };
            (st.timecode.on, list)
        };
        let mut error = None;
        for (s, ms) in &list {
            if let Err(e) = sock.send_to(&packet(s.id, *ms, cfg.fps), &cfg.target) {
                error = Some(format!("cannot send to {}: {e}", cfg.target));
            }
        }
        let n = list.len() as u64;
        let shown: Vec<Stream> = list.into_iter().map(|(s, _)| s).collect();
        // The web UI does not need every frame: update it 4 times a second.
        let st_changed = {
            let st = app.state.lock().unwrap();
            (st.timecode.streams.is_empty() != shown.is_empty()) || st.timecode.error != error
        };
        if st_changed || now - ui_last >= 250 {
            ui_last = now;
            app.update(|st| {
                st.timecode.streams = shown;
                st.timecode.error = error;
                st.timecode.packets += n;
            });
        } else if on {
            app.state.lock().unwrap().timecode.packets += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_timecode_packet() {
        // 1:02:03 and 15 frames at 30 fps.
        let ms = ((3600 + 2 * 60 + 3) * 1000 + 500) as u64;
        let p = packet(2, ms, 30);
        assert_eq!(&p[..8], b"Art-Net\0");
        assert_eq!((p[8], p[9]), (0x00, 0x97));
        assert_eq!(p[11], 14);
        assert_eq!(p[13], 2);
        assert_eq!((p[17], p[16], p[15], p[14]), (1, 2, 3, 15));
        assert_eq!(p[18], 3);
        assert_eq!(format(ms, 25), "01:02:03:12");
    }

    #[test]
    fn playhead_from_position_or_beats() {
        let mut d = Device {
            playing: Some(true),
            position_ms: Some(61_234),
            ..Default::default()
        };
        assert_eq!(playhead(&d, 0), Some((61_234, false)));
        d.position_ms = None;
        d.beat = Some(129);
        d.track_bpm = Some(120.0);
        d.last_beat = Some(1_000);
        // 128 beats at 500 ms, plus 250 ms since the beat.
        assert_eq!(playhead(&d, 1_250), Some((64_250, true)));
        d.playing = Some(false);
        assert_eq!(playhead(&d, 1_250), None);
    }
}
