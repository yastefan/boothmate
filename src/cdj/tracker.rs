//! The live picture of the DJ Link network: which devices are there, and per player the tempo,
//! beat and bar position. Pure logic, fed with parsed packets.

use super::packet::{self, Kind, Packet};
use serde::Serialize;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;

/// A device counts as gone after this long without any packet (keep-alives come every 1.5-2 s).
pub const DEVICE_TIMEOUT_MS: u64 = 5_000;

#[derive(Debug, Clone, Default, Serialize)]
pub struct CdjState {
    pub enabled: bool,
    /// Network interface used for DJ Link, e.g. `en5 192.168.1.20`.
    pub interface: Option<String>,
    pub error: Option<String>,
    /// Device number of Boothmate's silent virtual player, while it announces itself.
    pub vcdj: Option<u8>,
    /// Why status (stage 2) is not available, if it is not.
    pub status_note: Option<String>,
    pub simulator: bool,
    /// Capture file being replayed (`--replay`).
    pub replay: Option<String>,
    /// Recording wanted (web UI) and the file being written.
    pub record: bool,
    pub recording: Option<String>,
    pub recorded: u64,
    pub devices: BTreeMap<u8, Device>,
    /// Number of the tempo master.
    pub master: Option<u8>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Device {
    pub number: u8,
    pub name: String,
    pub kind: Option<Kind>,
    pub ip: Option<Ipv4Addr>,
    /// From the simulator, not the network.
    pub simulated: bool,
    pub last_seen: u64,

    // Stage 1: beat packets (and mixer status).
    pub track_bpm: Option<f32>,
    /// Pitch in percent.
    pub pitch: f32,
    /// Effective BPM: track BPM with pitch.
    pub bpm: Option<f32>,
    /// 1-4.
    pub beat_in_bar: Option<u8>,
    /// Time of the last beat packet (ms), for the beat flash in the UI.
    pub last_beat: Option<u64>,

    // Stage 2: status packets.
    pub status: bool,
    pub firmware: Option<String>,
    pub playing: Option<bool>,
    pub play_state: Option<String>,
    pub master: bool,
    pub sync: bool,
    pub on_air: bool,
    pub track: Option<Track>,
    /// Beat number in the track (1-based), from status, counted on between status packets.
    pub beat: Option<u32>,
    pub beats_to_cue: Option<u16>,

    /// Bar position: from the beat number when known, else counted from beat packets.
    pub bar: Option<u32>,
    /// Bar within a 4-bar group (1-4) and within a 32-beat phrase of 8 bars (1-8). Counted
    /// from the start of the beat grid; real rekordbox phrases come with the track metadata.
    pub bar_in_4: Option<u8>,
    pub bar_in_8: Option<u8>,
    /// The bar count is relative (no beat number from status).
    pub bar_counted: bool,

    // CDJ-3000 position packets.
    pub position_ms: Option<u32>,
    pub track_len_s: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Track {
    pub player: u8,
    pub slot: String,
    /// rekordbox ID.
    pub id: u32,
    /// rekordbox, unanalysed, cd, streaming.
    pub kind: String,
}

/// What happened, for the log and (later) for grandMA3.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Found {
        number: u8,
        name: String,
    },
    Lost {
        number: u8,
        name: String,
    },
    Master(Option<u8>),
    Track {
        number: u8,
        track: Option<Track>,
    },
    Play {
        number: u8,
        playing: bool,
    },
    Beat {
        number: u8,
        beat_in_bar: u8,
        bar: Option<u32>,
        master: bool,
    },
}

impl Device {
    fn new(number: u8) -> Self {
        Device {
            number,
            ..Default::default()
        }
    }

    fn set_bar_from_beat(&mut self) {
        let (Some(beat), Some(bb)) = (self.beat, self.beat_in_bar) else {
            return;
        };
        if !(1..=4).contains(&bb) || beat == 0 {
            return;
        }
        // The first beat of the grid need not be a downbeat: count bars from the beat-in-bar.
        let downbeat = beat as i64 - (bb as i64 - 1);
        let bar = (downbeat - 1).div_euclid(4) + 1;
        self.bar = Some(bar.max(1) as u32);
        self.bar_counted = false;
        self.set_groups();
    }

    fn set_groups(&mut self) {
        if let Some(bar) = self.bar {
            self.bar_in_4 = Some(((bar - 1) % 4 + 1) as u8);
            self.bar_in_8 = Some(((bar - 1) % 8 + 1) as u8);
        }
    }

    fn effective_bpm(&mut self) {
        self.bpm = self
            .track_bpm
            .map(|b| ((b * (1.0 + self.pitch / 100.0)) * 100.0).round() / 100.0);
    }
}

impl CdjState {
    fn device(&mut self, number: u8, now: u64, simulated: bool) -> &mut Device {
        let d = self.devices.entry(number).or_insert_with(|| {
            let mut d = Device::new(number);
            d.simulated = simulated;
            d
        });
        d.last_seen = now;
        d
    }

    /// Applies one packet from `src`.
    pub fn apply(
        &mut self,
        p: Packet,
        src: Option<Ipv4Addr>,
        now: u64,
        simulated: bool,
    ) -> Vec<Event> {
        let mut ev = vec![];
        match p {
            Packet::KeepAlive(k) => {
                if !simulated && Some(k.number) == self.vcdj && k.name == super::VCDJ_NAME {
                    return ev;
                }
                let new = !self.devices.contains_key(&k.number);
                let d = self.device(k.number, now, simulated);
                d.name = k.name.clone();
                d.kind = Some(k.kind);
                d.ip = src.or(Some(k.ip));
                if new {
                    ev.push(Event::Found {
                        number: k.number,
                        name: k.name,
                    });
                }
            }
            Packet::Conflict { .. } => {}
            Packet::Beat(b) => {
                let master_no = self.master;
                let d = self.device(b.number, now, simulated);
                if d.name.is_empty() {
                    d.name = b.name;
                }
                d.track_bpm = b.track_bpm;
                d.pitch = b.pitch;
                d.effective_bpm();
                let prev = d.beat_in_bar;
                d.beat_in_bar = Some(b.beat_in_bar);
                d.last_beat = Some(now);
                // Count on between status packets, unless a status already brought this beat.
                if prev != Some(b.beat_in_bar) {
                    if let Some(n) = d.beat.as_mut() {
                        *n += 1;
                    }
                }
                if d.beat.is_some() {
                    d.set_bar_from_beat();
                } else if b.beat_in_bar == 1 && prev != Some(1) {
                    d.bar = Some(d.bar.map_or(1, |x| x + 1));
                    d.bar_counted = true;
                    d.set_groups();
                }
                let master = master_no == Some(b.number) || d.master;
                ev.push(Event::Beat {
                    number: b.number,
                    beat_in_bar: b.beat_in_bar,
                    bar: d.bar,
                    master,
                });
                // Without status packets, the tempo master is unknown: take the player that beats.
                if !d.status && (1..=6).contains(&b.number) && self.master.is_none() {
                    self.master = Some(b.number);
                    ev.push(Event::Master(self.master));
                }
            }
            Packet::Position(p) => {
                let d = self.device(p.number, now, simulated);
                d.position_ms = Some(p.playhead_ms);
                d.track_len_s = Some(p.track_len_s);
                if p.bpm.is_some() {
                    d.bpm = p.bpm;
                }
            }
            Packet::Status(s) => {
                let d = self.device(s.number, now, simulated);
                if d.name.is_empty() {
                    d.name = s.name.clone();
                }
                d.status = true;
                d.firmware = Some(s.firmware.clone()).filter(|f| !f.is_empty());
                if d.playing != Some(s.playing) {
                    ev.push(Event::Play {
                        number: s.number,
                        playing: s.playing,
                    });
                }
                d.playing = Some(s.playing);
                d.play_state = Some(packet::play_state_name(s.play_state).to_string());
                d.master = s.master;
                d.sync = s.sync;
                d.on_air = s.on_air;
                let track = (s.track_player != 0).then(|| Track {
                    player: s.track_player,
                    slot: packet::slot_name(s.track_slot).to_string(),
                    id: s.track_id,
                    kind: match s.track_type {
                        1 => "rekordbox",
                        2 => "unanalysed",
                        5 => "cd",
                        6 => "streaming",
                        _ => "other",
                    }
                    .to_string(),
                });
                if d.track != track {
                    d.track = track.clone();
                    d.beat = None;
                    d.bar = None;
                    d.bar_in_4 = None;
                    d.bar_in_8 = None;
                    d.position_ms = None;
                    ev.push(Event::Track {
                        number: s.number,
                        track,
                    });
                }
                d.track_bpm = s.track_bpm;
                d.pitch = s.pitch;
                d.effective_bpm();
                d.beat = s.beat;
                d.beats_to_cue = s.beats_to_cue;
                if (1..=4).contains(&s.beat_in_bar) {
                    d.beat_in_bar = Some(s.beat_in_bar);
                }
                d.set_bar_from_beat();
                self.update_master(&mut ev);
            }
            Packet::MixerStatus(m) => {
                let d = self.device(m.number, now, simulated);
                if d.name.is_empty() {
                    d.name = m.name;
                }
                d.kind.get_or_insert(Kind::Mixer);
                d.status = true;
                d.master = m.master;
                d.bpm = m.bpm;
                d.track_bpm = m.bpm;
                self.update_master(&mut ev);
            }
        }
        ev
    }

    fn update_master(&mut self, ev: &mut Vec<Event>) {
        let claimed = self
            .devices
            .values()
            .find(|d| d.status && d.master)
            .map(|d| d.number);
        // Keep a beat-guessed master (a player without status) until a status names one.
        let master = claimed.or_else(|| {
            self.master
                .filter(|m| self.devices.get(m).is_some_and(|d| !d.status))
        });
        if master != self.master {
            self.master = master;
            ev.push(Event::Master(master));
        }
    }

    /// Drops devices that went quiet.
    pub fn prune(&mut self, now: u64) -> Vec<Event> {
        let mut ev = vec![];
        let gone: Vec<u8> = self
            .devices
            .values()
            .filter(|d| now.saturating_sub(d.last_seen) > DEVICE_TIMEOUT_MS)
            .map(|d| d.number)
            .collect();
        for n in gone {
            if let Some(d) = self.devices.remove(&n) {
                ev.push(Event::Lost {
                    number: n,
                    name: d.name,
                });
            }
            if self.master == Some(n) {
                self.master = None;
                ev.push(Event::Master(None));
            }
        }
        ev
    }

    /// Removes everything the simulator added.
    pub fn clear_simulated(&mut self) {
        self.devices.retain(|_, d| !d.simulated);
        if self.master.is_some_and(|m| !self.devices.contains_key(&m)) {
            self.master = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdj::packet::{self, Status, ANNOUNCE_PORT, BEAT_PORT, STATUS_PORT};

    fn feed(st: &mut CdjState, port: u16, b: &[u8], now: u64) -> Vec<Event> {
        st.apply(packet::parse(port, b).unwrap(), None, now, false)
    }

    fn status(beat: Option<u32>, bb: u8, master: bool) -> Status {
        Status {
            number: 1,
            name: "CDJ-2000NXS2".into(),
            track_player: 1,
            track_slot: 3,
            track_type: 1,
            track_id: 42,
            play_state: 3,
            firmware: "1.85".into(),
            playing: true,
            master,
            track_bpm: Some(128.0),
            beat,
            beat_in_bar: bb,
            ..Default::default()
        }
    }

    #[test]
    fn discovery_and_timeout() {
        let mut st = CdjState::default();
        let ka = packet::keep_alive(
            "CDJ-3000",
            2,
            Kind::Player,
            [0; 6],
            Ipv4Addr::new(10, 0, 0, 2),
        );
        let ev = feed(&mut st, ANNOUNCE_PORT, &ka, 1_000);
        assert_eq!(
            ev,
            vec![Event::Found {
                number: 2,
                name: "CDJ-3000".into()
            }]
        );
        assert!(feed(&mut st, ANNOUNCE_PORT, &ka, 2_500).is_empty());
        assert_eq!(st.devices[&2].ip, Some(Ipv4Addr::new(10, 0, 0, 2)));
        assert!(st.prune(6_000).is_empty());
        assert_eq!(
            st.prune(8_000),
            vec![Event::Lost {
                number: 2,
                name: "CDJ-3000".into()
            }]
        );
        assert!(st.devices.is_empty());
    }

    #[test]
    fn beats_without_status_count_bars() {
        let mut st = CdjState::default();
        let mut bars = vec![];
        for (i, bb) in [3u8, 4, 1, 2, 3, 4, 1].into_iter().enumerate() {
            let ev = feed(
                &mut st,
                BEAT_PORT,
                &packet::beat("CDJ-900", 3, 125.0, 2.0, bb),
                i as u64 * 480,
            );
            assert!(ev
                .iter()
                .any(|e| matches!(e, Event::Beat { beat_in_bar, .. } if *beat_in_bar == bb)));
            bars.push(st.devices[&3].bar);
        }
        assert_eq!(
            bars,
            [None, None, Some(1), Some(1), Some(1), Some(1), Some(2)]
        );
        let d = &st.devices[&3];
        assert!(d.bar_counted);
        assert_eq!(d.bpm, Some(127.5));
        assert_eq!(
            st.master,
            Some(3),
            "the beating player stands in for the master"
        );
    }

    #[test]
    fn status_gives_beat_number_bar_and_phrase() {
        let mut st = CdjState::default();
        // Beat 37 = bar 10, beat 1 (grid starts on a downbeat).
        let ev = feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(37), 1, true)),
            0,
        );
        assert!(ev.contains(&Event::Master(Some(1))));
        assert!(ev.iter().any(|e| matches!(e, Event::Track { number: 1, track: Some(t) } if t.id == 42 && t.slot == "USB")));
        let d = &st.devices[&1];
        assert_eq!(
            (d.bar, d.bar_in_4, d.bar_in_8),
            (Some(10), Some(2), Some(2))
        );
        assert_eq!(d.play_state.as_deref(), Some("playing"));

        // Next beat packet before the next status: counted on.
        feed(
            &mut st,
            BEAT_PORT,
            &packet::beat("CDJ-2000NXS2", 1, 128.0, 0.0, 2),
            470,
        );
        assert_eq!(st.devices[&1].beat, Some(38));
        // The status for the same beat does not count twice.
        feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(38), 2, true)),
            500,
        );
        feed(
            &mut st,
            BEAT_PORT,
            &packet::beat("CDJ-2000NXS2", 1, 128.0, 0.0, 2),
            510,
        );
        assert_eq!(st.devices[&1].beat, Some(38));

        // Grid that starts on beat 3 of a bar: beat 1 has beat-in-bar 3, beat 3 is the next downbeat.
        feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(3), 1, true)),
            600,
        );
        assert_eq!(st.devices[&1].bar, Some(1));
        feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(7), 1, true)),
            700,
        );
        assert_eq!(st.devices[&1].bar, Some(2));
    }

    #[test]
    fn master_moves_and_track_changes() {
        let mut st = CdjState::default();
        feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(1), 1, true)),
            0,
        );
        let mut other = status(Some(5), 1, false);
        other.number = 2;
        feed(&mut st, STATUS_PORT, &packet::status(&other), 0);
        assert_eq!(st.master, Some(1));
        other.master = true;
        let ev = feed(&mut st, STATUS_PORT, &packet::status(&other), 100);
        // Both claim master for one packet; the first one wins until player 1 lets go.
        assert!(ev.is_empty() || ev.contains(&Event::Master(Some(1))));
        let ev = feed(
            &mut st,
            STATUS_PORT,
            &packet::status(&status(Some(2), 2, false)),
            200,
        );
        assert!(ev.contains(&Event::Master(Some(2))));

        let mut t = status(Some(1), 1, false);
        t.track_id = 43;
        let ev = feed(&mut st, STATUS_PORT, &packet::status(&t), 300);
        assert!(ev
            .iter()
            .any(|e| matches!(e, Event::Track { number: 1, track: Some(t) } if t.id == 43)));
    }
}
