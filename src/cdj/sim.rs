//! A fake DJ Link network for trying Boothmate without CDJs: two players and a mixer. It builds
//! real packets, which go through the same parser as packets from the network.
//!
//! The scenario repeats every 64 seconds:
//! - 0-16 s: player 1 plays (tempo master), player 2 (a CDJ-3000) waits at the cue point.
//! - 16-32 s: both play, player 1 is master.
//! - 32-48 s: player 2 takes over as master.
//! - 48-64 s: player 1 pauses.

use super::packet::{self, Kind, Status, ANNOUNCE_PORT, BEAT_PORT, STATUS_PORT};
use std::net::Ipv4Addr;

const CYCLE_MS: u64 = 64_000;

struct Player {
    number: u8,
    name: &'static str,
    cdj3000: bool,
    track_id: u32,
    track_bpm: f32,
    pitch: f32,
    track_len_ms: f64,
    /// Playhead in ms of track time.
    pos: f64,
    playing: bool,
    master: bool,
    last_beat: u32,
}

impl Player {
    fn beat_ms(&self) -> f64 {
        60_000.0 / self.track_bpm as f64
    }

    /// Beat number at the playhead (1-based).
    fn beat(&self) -> u32 {
        (self.pos / self.beat_ms()) as u32 + 1
    }

    fn beat_in_bar(&self) -> u8 {
        ((self.beat() - 1) % 4 + 1) as u8
    }

    fn status(&self) -> Status {
        Status {
            number: self.number,
            name: self.name.into(),
            track_player: self.number,
            track_slot: 3,
            track_type: 1,
            track_id: self.track_id,
            play_state: if self.playing {
                0x03
            } else if self.pos == 0.0 {
                0x06
            } else {
                0x05
            },
            firmware: if self.cdj3000 { "3.20" } else { "1.85" }.into(),
            playing: self.playing,
            master: self.master,
            sync: false,
            on_air: self.playing,
            pitch: self.pitch,
            track_bpm: Some(self.track_bpm),
            beat: Some(self.beat()),
            beats_to_cue: None,
            beat_in_bar: self.beat_in_bar(),
        }
    }
}

pub struct Sim {
    start: u64,
    last: u64,
    players: [Player; 2],
    next_keep_alive: u64,
    next_status: u64,
    next_position: u64,
}

const MIXER: (u8, &str) = (33, "DJM-900NXS2");

impl Sim {
    pub fn new(now: u64) -> Self {
        Sim {
            start: now,
            last: now,
            players: [
                Player {
                    number: 1,
                    name: "CDJ-2000NXS2",
                    cdj3000: false,
                    track_id: 101,
                    track_bpm: 128.0,
                    pitch: 0.0,
                    track_len_ms: 372_000.0,
                    pos: 0.0,
                    playing: true,
                    master: true,
                    last_beat: 0,
                },
                Player {
                    number: 2,
                    name: "CDJ-3000",
                    cdj3000: true,
                    track_id: 202,
                    track_bpm: 124.0,
                    // 124 BPM pitched up to 128.
                    pitch: 3.23,
                    track_len_ms: 318_000.0,
                    pos: 0.0,
                    playing: false,
                    master: false,
                    last_beat: 0,
                },
            ],
            next_keep_alive: now,
            next_status: now,
            next_position: now,
        }
    }

    /// Packets due at `now` (ms), as (port, bytes).
    pub fn tick(&mut self, now: u64) -> Vec<(u16, Vec<u8>)> {
        let mut out = vec![];
        let dt = now.saturating_sub(self.last) as f64;
        self.last = now;
        let t = (now - self.start) % CYCLE_MS;
        let [p1, p2] = &mut self.players;
        p1.playing = t < 48_000;
        p2.playing = t >= 16_000;
        p1.master = t < 32_000;
        p2.master = t >= 32_000;
        if t < 100 {
            // New round: player 2 is back at its cue point.
            p2.pos = 0.0;
            p2.last_beat = 0;
        }

        for p in &mut self.players {
            if p.playing {
                p.pos += dt * (1.0 + p.pitch as f64 / 100.0);
                if p.pos >= p.track_len_ms {
                    p.pos = 0.0;
                    p.last_beat = 0;
                }
                let beat = p.beat();
                if beat != p.last_beat {
                    p.last_beat = beat;
                    let b = packet::beat(p.name, p.number, p.track_bpm, p.pitch, p.beat_in_bar());
                    out.push((BEAT_PORT, b));
                }
            }
        }

        if now >= self.next_keep_alive {
            self.next_keep_alive = now + 1_500;
            for p in &self.players {
                let ip = Ipv4Addr::new(127, 0, 0, p.number);
                out.push((
                    ANNOUNCE_PORT,
                    packet::keep_alive(p.name, p.number, Kind::Player, [0; 6], ip),
                ));
            }
            let ip = Ipv4Addr::new(127, 0, 0, MIXER.0);
            out.push((
                ANNOUNCE_PORT,
                packet::keep_alive(MIXER.1, MIXER.0, Kind::Mixer, [0; 6], ip),
            ));
        }

        if now >= self.next_status {
            self.next_status = now + 200;
            for p in &self.players {
                out.push((STATUS_PORT, packet::status(&p.status())));
            }
            let m = self
                .players
                .iter()
                .find(|p| p.master)
                .unwrap_or(&self.players[0]);
            let bpm = m.track_bpm * (1.0 + m.pitch / 100.0);
            out.push((
                STATUS_PORT,
                packet::mixer_status(MIXER.1, MIXER.0, false, bpm, m.beat_in_bar()),
            ));
        }

        if now >= self.next_position {
            self.next_position = now + 30;
            for p in self.players.iter().filter(|p| p.cdj3000) {
                let bpm = p.track_bpm * (1.0 + p.pitch / 100.0);
                let len = (p.track_len_ms / 1000.0) as u32;
                out.push((
                    BEAT_PORT,
                    packet::position(p.name, p.number, len, p.pos as u32, bpm),
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdj::tracker::{CdjState, Event};

    #[test]
    fn simulated_network_through_the_parser() {
        let mut st = CdjState::default();
        let mut sim = Sim::new(0);
        let mut beats = 0;
        let mut master_changes = vec![];
        // 40 s in 10 ms steps.
        for now in (0..40_000).step_by(10) {
            for (port, bytes) in sim.tick(now) {
                let p = packet::parse(port, &bytes).expect("simulator packet parses");
                for e in st.apply(p, None, now, true) {
                    match e {
                        Event::Beat { number: 1, .. } => beats += 1,
                        Event::Master(m) => master_changes.push((now, m)),
                        _ => {}
                    }
                }
            }
        }
        // 128 BPM for 40 s.
        assert!((84..=86).contains(&beats), "{beats} beats");
        assert_eq!(st.devices.len(), 3);
        assert_eq!(st.master, Some(2));
        assert_eq!(master_changes.first(), Some(&(0, Some(1))));
        assert!(master_changes
            .iter()
            .any(|&(t, m)| m == Some(2) && (32_000..32_300).contains(&t)));
        let p2 = &st.devices[&2];
        assert_eq!(p2.bpm, Some(128.0));
        assert!(p2.position_ms.is_some_and(|ms| ms > 20_000));
        assert_eq!(p2.playing, Some(true));
        let p1 = &st.devices[&1];
        assert_eq!(p1.bar, Some((p1.beat.unwrap() - 1) / 4 + 1));
        assert!(st.devices[&33].kind == Some(Kind::Mixer));
    }
}
