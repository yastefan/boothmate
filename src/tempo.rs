//! Tempo link: the tempo and beats of the CDJs drive grandMA3.
//!
//! - Speed masters follow the BPM of the tempo master (or a fixed player), through the Lua
//!   plugin (`{"op":"speed","master":1,"bpm":128.0}` = `Master 3.1 At BPM 128`). The value is
//!   sent on every change and again once a second, because grandMA3 does not report its speed
//!   masters back (a master moved by hand would otherwise keep the wrong tempo). Optionally
//!   with a phaser re-sync on the first beat of each phrase (`{"op":"phase","master":1}` =
//!   `FastSync`). grandMA3 counts a FastSync as a Learn tap, so it is only sent once per phrase
//!   and the plugin restores the exact tempo right after it.
//! - Beat actions press executor keys over OSC on beats, downbeats, every 4 bars, every phrase
//!   (8 bars), on track and on master changes.
//!
//! Rules live in `tempo.toml` in the data folder and can be edited in the web UI.

use crate::cdj::tracker::{CdjState, Event};
use crate::midi::mapping::Action;
use crate::App;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Smallest BPM change sent to a speed master.
pub const BPM_STEP: f32 = 0.05;
/// At most one speed update per master in this time (ms).
pub const SPEED_INTERVAL_MS: u64 = 200;
/// The current BPM is sent again after this time (ms) even when it did not change, so that a
/// speed master moved in grandMA3 (or reset by a FastSync tap) snaps back to the CDJ tempo.
pub const SPEED_REFRESH_MS: u64 = 1_000;
/// Key press length for beat actions.
const TAP_MS: u64 = 60;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TempoFile {
    #[serde(default)]
    pub speed: Vec<SpeedLink>,
    #[serde(default)]
    pub action: Vec<BeatAction>,
}

/// A speed master that follows the CDJs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpeedLink {
    /// Speed master number (`Master 3.<n>`).
    pub master: u32,
    /// Player number to follow; absent = the tempo master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<u8>,
    /// 0.5 = half time, 2 = double time.
    #[serde(default = "one")]
    pub factor: f32,
    #[serde(default)]
    pub phase: Phase,
}

fn one() -> f32 {
    1.0
}

/// When the speed master's phase is reset to the CDJ beat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    #[default]
    Off,
    /// First beat of every 8-bar phrase.
    Phrase,
}

/// An executor key pressed on a beat event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatAction {
    pub on: Trigger,
    pub exec: u32,
    /// Absent = the current page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    /// Player whose beats count; absent = the tempo master.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Beat,
    Downbeat,
    /// First beat of every 4 bars.
    Bar4,
    /// First beat of every 8-bar phrase (32 beats).
    Phrase,
    /// The followed player loads another track.
    Track,
    /// Another player becomes tempo master.
    Master,
}

/// What the web UI shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TempoState {
    pub rules: TempoFile,
    /// Last BPM per speed master (sent when the plugin is connected).
    pub sent: HashMap<u32, f32>,
    /// Last fire time (ms) per action index, for a flash in the UI.
    pub fired: HashMap<usize, u64>,
    pub note: Option<String>,
}

/// What the engine wants sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    Speed {
        master: u32,
        bpm: f32,
    },
    Phase {
        master: u32,
    },
    Tap {
        page: Option<u32>,
        exec: u32,
        action: usize,
    },
}

/// The pure part: rules + CDJ state + events in, things to send out.
#[derive(Default)]
pub struct Engine {
    sent: HashMap<u32, (f32, u64)>,
}

fn source(cdj: &CdjState, player: Option<u8>) -> Option<u8> {
    player.or(cdj.master)
}

impl Engine {
    /// Forget what was sent, e.g. after the plugin reconnects.
    pub fn reset(&mut self) {
        self.sent.clear();
    }

    pub fn step(
        &mut self,
        rules: &TempoFile,
        cdj: &CdjState,
        events: &[Event],
        now: u64,
    ) -> Vec<Out> {
        let mut out = vec![];
        for l in &rules.speed {
            let Some(src) = source(cdj, l.player) else {
                continue;
            };
            let Some(d) = cdj.devices.get(&src) else {
                continue;
            };
            // A stopped player keeps its BPM; hold the last value then.
            if d.playing == Some(false) {
                continue;
            }
            let Some(bpm) = d.bpm.filter(|b| *b > 0.0) else {
                continue;
            };
            let bpm = ((bpm * l.factor) * 100.0).round() / 100.0;
            let due = match self.sent.get(&l.master) {
                None => true,
                Some(&(last, t)) => {
                    let age = now.saturating_sub(t);
                    ((bpm - last).abs() >= BPM_STEP && age >= SPEED_INTERVAL_MS)
                        || age >= SPEED_REFRESH_MS
                }
            };
            if due {
                self.sent.insert(l.master, (bpm, now));
                out.push(Out::Speed {
                    master: l.master,
                    bpm,
                });
            }
        }
        for e in events {
            match e {
                Event::Beat {
                    number,
                    beat_in_bar,
                    ..
                } => {
                    let d = cdj.devices.get(number);
                    let bar_in_4 = d.and_then(|d| d.bar_in_4);
                    let bar_in_8 = d.and_then(|d| d.bar_in_8);
                    let down = *beat_in_bar == 1;
                    for l in &rules.speed {
                        if source(cdj, l.player) != Some(*number) {
                            continue;
                        }
                        let fire = match l.phase {
                            Phase::Off => false,
                            Phase::Phrase => down && bar_in_8 == Some(1),
                        };
                        if fire {
                            out.push(Out::Phase { master: l.master });
                        }
                    }
                    for (i, a) in rules.action.iter().enumerate() {
                        if source(cdj, a.player) != Some(*number) {
                            continue;
                        }
                        let fire = match a.on {
                            Trigger::Beat => true,
                            Trigger::Downbeat => down,
                            Trigger::Bar4 => down && bar_in_4 == Some(1),
                            Trigger::Phrase => down && bar_in_8 == Some(1),
                            _ => false,
                        };
                        if fire {
                            out.push(Out::Tap {
                                page: a.page,
                                exec: a.exec,
                                action: i,
                            });
                        }
                    }
                }
                Event::Track {
                    number,
                    track: Some(_),
                } => {
                    for (i, a) in rules.action.iter().enumerate() {
                        if a.on == Trigger::Track && source(cdj, a.player) == Some(*number) {
                            out.push(Out::Tap {
                                page: a.page,
                                exec: a.exec,
                                action: i,
                            });
                        }
                    }
                }
                Event::Master(Some(_)) => {
                    for (i, a) in rules.action.iter().enumerate() {
                        if a.on == Trigger::Master {
                            out.push(Out::Tap {
                                page: a.page,
                                exec: a.exec,
                                action: i,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }
}

/// Speed and phase messages need plugin 0.2.0 or later.
/// `version` (a.b.c) is `min` or later.
pub fn version_at_least(version: &str, min: &str) -> bool {
    let parse = |v: &str| -> Vec<u32> { v.split('.').map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parse(version), parse(min));
    (0..3)
        .map(|i| a.get(i).copied().unwrap_or(0))
        .cmp((0..3).map(|i| b.get(i).copied().unwrap_or(0)))
        != std::cmp::Ordering::Less
}

pub fn plugin_supports_speed(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= (0, 2)
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("tempo.toml")
}

pub fn load(dir: &Path) -> TempoFile {
    let p = path(dir);
    let Ok(text) = std::fs::read_to_string(&p) else {
        return TempoFile::default();
    };
    toml::from_str(&text).unwrap_or_else(|e| {
        crate::log::warn(&format!("tempo: {}: {e}", p.display()));
        TempoFile::default()
    })
}

pub fn save(dir: &Path, rules: &TempoFile) -> Result<(), String> {
    let text = toml::to_string(rules).map_err(|e| e.to_string())?;
    let text = format!(
        "# Tempo link, written by Boothmate. [[speed]]: speed master <master> follows the tempo master\n\
         # (or <player>), times <factor>; phase = off | phrase. [[action]]: press executor <exec>\n\
         # on beat | downbeat | bar4 | phrase | track | master (page absent = current page).\n\n{text}"
    );
    std::fs::write(path(dir), text).map_err(|e| e.to_string())
}

pub async fn run(app: Arc<App>) {
    let rules = load(&app.dir);
    if !rules.speed.is_empty() || !rules.action.is_empty() {
        crate::log::info(&format!(
            "tempo: {} speed link(s), {} beat action(s)",
            rules.speed.len(),
            rules.action.len()
        ));
    }
    app.update(|st| st.tempo.rules = rules);
    let mut rx = app.cdj_events.subscribe();
    let mut engine = Engine::default();
    let mut connected = false;
    let mut last_rules = TempoFile::default();
    loop {
        let events = match rx.recv().await {
            Ok(e) => e,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => return,
        };
        let now = crate::state::now_ms();
        let (outs, page, ok) = {
            let st = app.state.lock().unwrap();
            let c = &st.connections;
            let ok = c.plugin_connected && !c.profile_fallback;
            if ok && !connected {
                // Fresh plugin connection: send the speed again.
                engine.reset();
            }
            connected = ok;
            if st.tempo.rules != last_rules {
                // Edited in the web UI: send the current values for the new rules.
                engine.reset();
                last_rules = st.tempo.rules.clone();
            }
            let outs = engine.step(&st.tempo.rules, &st.cdj, &events, now);
            (outs, st.page, ok)
        };
        if outs.is_empty() {
            continue;
        }
        let mut sent = vec![];
        let mut fired = vec![];
        for o in outs {
            match o {
                Out::Speed { master, bpm } => {
                    if ok {
                        let _ = app.plugin_ops.send(
                            serde_json::json!({ "op": "speed", "master": master, "bpm": bpm })
                                .to_string(),
                        );
                        crate::log::debug(&format!("tempo: speed {master} = {bpm:.2} BPM"));
                    }
                    sent.push((master, bpm));
                }
                Out::Phase { master } => {
                    if ok {
                        let _ = app.plugin_ops.send(
                            serde_json::json!({ "op": "phase", "master": master }).to_string(),
                        );
                    }
                }
                Out::Tap {
                    page: p,
                    exec,
                    action,
                } => {
                    fired.push(action);
                    let Some(page) = p.or(page) else { continue };
                    if !ok {
                        continue;
                    }
                    crate::osc::send_action(
                        &app,
                        &Action::Key {
                            page,
                            exec,
                            pressed: true,
                        },
                    );
                    let app2 = app.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(TAP_MS)).await;
                        crate::osc::send_action(
                            &app2,
                            &Action::Key {
                                page,
                                exec,
                                pressed: false,
                            },
                        );
                    });
                }
            }
        }
        app.update(|st| {
            for (m, b) in sent {
                st.tempo.sent.insert(m, b);
            }
            for i in fired {
                st.tempo.fired.insert(i, now);
            }
            let old = st
                .connections
                .plugin_version
                .as_deref()
                .filter(|v| !plugin_supports_speed(v))
                .map(str::to_string);
            st.tempo.note = if !ok {
                Some("Waiting for the grandMA3 plugin: nothing is sent.".into())
            } else if let (Some(v), false) = (old, st.tempo.rules.speed.is_empty()) {
                Some(format!(
                    "The plugin in grandMA3 is version {v}, which ignores speed masters. Import lua/boothmate.xml from this Boothmate again (0.2.0 or later) and restart the plugin."
                ))
            } else {
                None
            };
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdj::packet::{self, Status, BEAT_PORT, STATUS_PORT};

    fn status(number: u8, beat: u32, master: bool, playing: bool, bpm: f32) -> Vec<u8> {
        packet::status(&Status {
            number,
            name: "CDJ".into(),
            track_player: number,
            track_slot: 3,
            track_type: 1,
            track_id: 7,
            play_state: 3,
            playing,
            master,
            track_bpm: Some(bpm),
            beat: Some(beat),
            beat_in_bar: ((beat - 1) % 4 + 1) as u8,
            ..Default::default()
        })
    }

    fn feed(cdj: &mut CdjState, port: u16, b: &[u8], now: u64) -> Vec<Event> {
        cdj.apply(packet::parse(port, b).unwrap(), None, now, false)
    }

    #[test]
    fn speed_follows_the_master_with_threshold_and_rate_limit() {
        let rules: TempoFile =
            toml::from_str("[[speed]]\nmaster = 1\n[[speed]]\nmaster = 2\nfactor = 0.5").unwrap();
        let mut cdj = CdjState::default();
        let mut e = Engine::default();
        let ev = feed(&mut cdj, STATUS_PORT, &status(1, 1, true, true, 128.0), 0);
        assert_eq!(
            e.step(&rules, &cdj, &ev, 0),
            vec![
                Out::Speed {
                    master: 1,
                    bpm: 128.0
                },
                Out::Speed {
                    master: 2,
                    bpm: 64.0
                }
            ]
        );
        // Same tempo: nothing. Tiny change: nothing. Too soon: nothing.
        assert!(e.step(&rules, &cdj, &[], 300).is_empty());
        feed(
            &mut cdj,
            STATUS_PORT,
            &status(1, 2, true, true, 128.02),
            400,
        );
        assert!(e.step(&rules, &cdj, &[], 400).is_empty());
        feed(&mut cdj, STATUS_PORT, &status(1, 3, true, true, 130.0), 450);
        assert_eq!(e.step(&rules, &cdj, &[], 450).len(), 2);
        feed(&mut cdj, STATUS_PORT, &status(1, 4, true, true, 131.0), 500);
        assert!(e.step(&rules, &cdj, &[], 500).is_empty(), "rate limit");
        assert_eq!(e.step(&rules, &cdj, &[], 700).len(), 2);

        // Master moves to player 2: follow it.
        feed(
            &mut cdj,
            STATUS_PORT,
            &status(1, 5, false, true, 131.0),
            800,
        );
        let ev = feed(&mut cdj, STATUS_PORT, &status(2, 9, true, true, 124.0), 800);
        assert!(ev.contains(&Event::Master(Some(2))));
        assert_eq!(
            e.step(&rules, &cdj, &ev, 1000)[0],
            Out::Speed {
                master: 1,
                bpm: 124.0
            }
        );

        // A stopped master holds the last value.
        feed(
            &mut cdj,
            STATUS_PORT,
            &status(2, 9, true, false, 140.0),
            1300,
        );
        assert!(e.step(&rules, &cdj, &[], 1300).is_empty());
    }

    #[test]
    fn speed_is_refreshed_without_changes() {
        // grandMA3 does not report its speed masters: re-send the same BPM now and then, so a
        // master moved by hand snaps back, also when the phaser sync is off.
        let rules: TempoFile = toml::from_str("[[speed]]\nmaster = 1").unwrap();
        let mut cdj = CdjState::default();
        let mut e = Engine::default();
        let ev = feed(&mut cdj, STATUS_PORT, &status(1, 1, true, true, 128.0), 0);
        assert_eq!(e.step(&rules, &cdj, &ev, 0).len(), 1);
        assert!(e.step(&rules, &cdj, &[], SPEED_REFRESH_MS - 1).is_empty());
        assert_eq!(
            e.step(&rules, &cdj, &[], SPEED_REFRESH_MS),
            vec![Out::Speed {
                master: 1,
                bpm: 128.0
            }]
        );
        assert!(e.step(&rules, &cdj, &[], SPEED_REFRESH_MS + 300).is_empty());
    }

    #[test]
    fn beat_actions_and_phase() {
        let rules: TempoFile = toml::from_str(
            "[[speed]]\nmaster = 1\nphase = \"phrase\"\n\
             [[action]]\non = \"downbeat\"\nexec = 101\n\
             [[action]]\non = \"bar4\"\nexec = 102\npage = 2\n\
             [[action]]\non = \"phrase\"\nexec = 103\n\
             [[action]]\non = \"beat\"\nexec = 104\nplayer = 3\n\
             [[action]]\non = \"master\"\nexec = 105",
        )
        .unwrap();
        let mut cdj = CdjState::default();
        let mut e = Engine::default();
        let ev = feed(&mut cdj, STATUS_PORT, &status(1, 32, true, true, 128.0), 0);
        let out = e.step(&rules, &cdj, &ev, 0);
        assert!(
            out.contains(&Out::Tap {
                page: None,
                exec: 105,
                action: 4
            }),
            "master change"
        );

        let mut taps = vec![];
        let mut phases = 0;
        // Beats 33..=65: bar 9 starts a phrase and a 4-bar group, bar 13 a 4-bar group.
        for beat in 33..=65u32 {
            let now = beat as u64 * 470;
            let bb = ((beat - 1) % 4 + 1) as u8;
            let ev = feed(
                &mut cdj,
                BEAT_PORT,
                &packet::beat("CDJ", 1, 128.0, 0.0, bb),
                now,
            );
            for o in e.step(&rules, &cdj, &ev, now) {
                match o {
                    Out::Tap { exec, .. } => taps.push((beat, exec)),
                    Out::Phase { .. } => phases += 1,
                    _ => {}
                }
            }
        }
        let count = |x| taps.iter().filter(|t| t.1 == x).count();
        assert_eq!(count(101), 9, "downbeats at 33, 37, ..., 65");
        assert_eq!(
            taps.iter()
                .filter(|t| t.1 == 102)
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            [33, 49, 65]
        );
        assert_eq!(
            taps.iter()
                .filter(|t| t.1 == 103)
                .map(|t| t.0)
                .collect::<Vec<_>>(),
            [33, 65]
        );
        assert_eq!(count(104), 0, "player 3 does not play");
        assert_eq!(phases, 2);
    }

    #[test]
    fn plugin_version_check() {
        assert!(!plugin_supports_speed("0.1.0"));
        assert!(plugin_supports_speed("0.2.0"));
        assert!(plugin_supports_speed("1.0"));
    }

    #[test]
    fn file_round_trip() {
        let f = TempoFile {
            speed: vec![SpeedLink {
                master: 1,
                player: None,
                factor: 1.0,
                phase: Phase::Phrase,
            }],
            action: vec![BeatAction {
                on: Trigger::Bar4,
                exec: 201,
                page: Some(3),
                player: Some(2),
            }],
        };
        let text = toml::to_string(&f).unwrap();
        assert_eq!(toml::from_str::<TempoFile>(&text).unwrap(), f);
    }
}
