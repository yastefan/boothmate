//! The live picture of grandMA3 that Boothmate keeps: sequences and masters from OSC, executors
//! of the current page from the Lua plugin.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    #[default]
    Unknown,
    Off,
    On,
    Paused,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sequence {
    pub name: String,
    /// `"<cue number> <cue name>"`, as grandMA3 sends it.
    pub cue: Option<String>,
    pub playback: Playback,
    /// Key held down (press seen, release not yet).
    pub pressed: bool,
    pub last_action: Option<String>,
    /// Fader values 0..100 by token (`FaderMaster`, ...).
    pub faders: BTreeMap<String, f32>,
    /// Unix time in ms of the last change.
    pub updated: u64,
}

/// One executor of the current page, as reported by the plugin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Executor {
    pub no: u32,
    /// Class of the assigned object (`Sequence`, `Group`, ...).
    #[serde(default)]
    pub class: String,
    /// Pool number of the assigned object.
    #[serde(default)]
    pub obj: Option<u32>,
    #[serde(default)]
    pub name: String,
    /// Object address such as `14.14.1.7.2`; the OSC address is `/` + this.
    #[serde(default)]
    pub addr: Option<String>,
    /// Appearance background as `#rrggbb`.
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub fader: Option<f32>,
    #[serde(default)]
    pub cue: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Connections {
    pub osc_listen: String,
    pub osc_packets: u64,
    pub osc_last: Option<u64>,
    pub osc_unknown: BTreeMap<String, u64>,
    pub plugin_addr: String,
    pub plugin_connected: bool,
    pub plugin_error: Option<String>,
    pub plugin_version: Option<String>,
    pub ma_version: Option<String>,
    pub profile: String,
    /// The profile is not verified for this grandMA3 version: read-only.
    pub profile_fallback: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct State {
    pub connections: Connections,
    pub page: Option<u32>,
    pub executors: Vec<Executor>,
    pub sequences: BTreeMap<u32, Sequence>,
    pub group_masters: BTreeMap<u32, f32>,
    pub grand_master: Option<f32>,
}

/// A change parsed from one OSC message.
#[derive(Debug, Clone, PartialEq)]
pub enum OscEvent {
    /// `sis`: action, 1 pressed / 0 released / 2 pause, `"<seq name>[;<cue>]"`.
    SequenceKey {
        seq: u32,
        action: String,
        state: i32,
        text: String,
    },
    /// `sifs`: fader token, source (1 command, 3 fader handle), value, seq name.
    SequenceFader {
        seq: u32,
        token: String,
        value: f32,
        name: Option<String>,
    },
    GroupMaster {
        group: u32,
        value: f32,
    },
    GrandMaster {
        value: f32,
    },
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl State {
    pub fn apply_osc(&mut self, ev: OscEvent) {
        let now = now_ms();
        match ev {
            OscEvent::SequenceKey {
                seq,
                action,
                state,
                text,
            } => {
                let s = self.sequences.entry(seq).or_default();
                let (name, cue) = match text.split_once(';') {
                    Some((n, c)) => (n, Some(c.to_string())),
                    None => (text.as_str(), None),
                };
                if !name.is_empty() {
                    s.name = name.to_string();
                }
                match action.as_str() {
                    "Off" => {
                        s.playback = Playback::Off;
                        s.cue = None;
                    }
                    "Pause" => s.playback = Playback::Paused,
                    _ => {
                        if state != 0 {
                            s.playback = Playback::On;
                        }
                        if cue.is_some() {
                            s.cue = cue;
                        }
                    }
                }
                s.pressed = state == 1 && action != "Off";
                s.last_action = Some(action);
                s.updated = now;
            }
            OscEvent::SequenceFader {
                seq,
                token,
                value,
                name,
            } => {
                let s = self.sequences.entry(seq).or_default();
                if let Some(n) = name.filter(|n| !n.is_empty()) {
                    s.name = n;
                }
                s.faders.insert(token, value);
                s.updated = now;
            }
            OscEvent::GroupMaster { group, value } => {
                self.group_masters.insert(group, value);
            }
            OscEvent::GrandMaster { value } => self.grand_master = Some(value),
        }
    }

    /// A page snapshot from the plugin replaces the executor list. Its values are read live in
    /// grandMA3, so they also refresh the sequences (start state on connect and page change).
    pub fn apply_page(&mut self, page: Option<u32>, executors: Vec<Executor>) {
        let now = now_ms();
        for e in &executors {
            if e.class != "Sequence" {
                continue;
            }
            let Some(no) = e.obj else { continue };
            let s = self.sequences.entry(no).or_default();
            s.name = e.name.clone();
            if let Some(a) = e.active {
                s.playback = if a { Playback::On } else { Playback::Off };
                s.cue = if a { e.cue.clone() } else { None };
            }
            if let Some(f) = e.fader {
                s.faders.insert("FaderMaster".into(), f);
            }
            s.updated = now;
        }
        self.page = page;
        self.executors = executors;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seq: u32, action: &str, state: i32, text: &str) -> OscEvent {
        OscEvent::SequenceKey {
            seq,
            action: action.into(),
            state,
            text: text.into(),
        }
    }

    #[test]
    fn key_events_track_cue_and_playback() {
        let mut st = State::default();
        st.apply_osc(key(2, "Go+", 1, "MCP Test;2 Rot"));
        let s = &st.sequences[&2];
        assert_eq!(s.name, "MCP Test");
        assert_eq!(s.cue.as_deref(), Some("2 Rot"));
        assert_eq!(s.playback, Playback::On);
        assert!(s.pressed);

        st.apply_osc(key(2, "Go+", 0, "MCP Test;2 Rot"));
        assert!(!st.sequences[&2].pressed);
        assert_eq!(st.sequences[&2].playback, Playback::On);

        st.apply_osc(key(2, "Pause", 2, "MCP Test"));
        assert_eq!(st.sequences[&2].playback, Playback::Paused);
        assert!(!st.sequences[&2].pressed);

        st.apply_osc(key(2, "Off", 1, "MCP Test"));
        assert_eq!(st.sequences[&2].playback, Playback::Off);
        assert_eq!(st.sequences[&2].cue, None);
        assert!(!st.sequences[&2].pressed);
    }

    #[test]
    fn faders_and_masters() {
        let mut st = State::default();
        st.apply_osc(OscEvent::SequenceFader {
            seq: 1,
            token: "FaderMaster".into(),
            value: 42.0,
            name: Some("A".into()),
        });
        st.apply_osc(OscEvent::GroupMaster {
            group: 1,
            value: 60.0,
        });
        st.apply_osc(OscEvent::GrandMaster { value: 80.0 });
        assert_eq!(st.sequences[&1].faders["FaderMaster"], 42.0);
        assert_eq!(st.group_masters[&1], 60.0);
        assert_eq!(st.grand_master, Some(80.0));
    }

    #[test]
    fn page_snapshot_refreshes_sequences() {
        let mut st = State::default();
        st.apply_osc(key(1, "Go+", 1, "Old;3 Blue"));
        let ex = |no, obj, active| Executor {
            no,
            class: "Sequence".into(),
            obj: Some(obj),
            name: format!("S{obj}"),
            addr: None,
            color: None,
            active: Some(active),
            fader: Some(50.0),
            cue: Some("1 Cue".into()),
        };
        st.apply_page(Some(1), vec![ex(201, 1, false), ex(202, 2, true)]);
        assert_eq!(st.sequences[&1].name, "S1");
        assert_eq!(st.sequences[&1].playback, Playback::Off);
        assert_eq!(st.sequences[&1].cue, None);
        assert_eq!(st.sequences[&2].playback, Playback::On);
        assert_eq!(st.sequences[&2].cue.as_deref(), Some("1 Cue"));
        assert_eq!(st.sequences[&2].faders["FaderMaster"], 50.0);
        assert_eq!(st.executors.len(), 2);
    }
}
