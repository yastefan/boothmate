//! Mappings from controls to executors, MIDI learn, and the feedback each control should show.
//! Pure logic on [`State`]; the MIDI runtime in `mod.rs` does the I/O.

use super::device::{Control, DeviceProfile, Feedback, Kind, Relative};
use super::msg::{Addr, Input, MsgType};
use crate::state::{OscEvent, Playback, State};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One control mapped to one executor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mapping {
    /// Device profile id, or the MIDI port name for devices without a profile.
    pub device: String,
    pub control: String,
    pub kind: Kind,
    pub midi: Addr,
    #[serde(default, skip_serializing_if = "is_default")]
    pub relative: Relative,
    pub exec: u32,
    /// Fixed page; none = follow the current page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
}

fn is_default(r: &Relative) -> bool {
    *r == Relative::default()
}

impl Mapping {
    pub fn control(&self) -> Control {
        Control::generic(self.control.clone(), self.kind, self.midi, self.relative)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct MappingFile {
    #[serde(default)]
    pub mapping: Vec<Mapping>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Nothing assigned (or no connection).
    #[default]
    Off,
    /// Assigned, not running.
    Dim,
    /// Running.
    On,
}

/// What a control should show, independent of how the device shows it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Fb {
    pub level: Level,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LearnedControl {
    pub device: String,
    pub control: String,
    pub kind: Kind,
    pub midi: Addr,
    pub relative: Relative,
    /// Whether the kind was guessed (device without profile).
    pub guessed: bool,
    #[serde(skip)]
    values: Vec<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Learn {
    /// New mappings follow the current page (else they stay on the page shown now).
    pub follow: bool,
    /// The control moved last, waiting for an executor.
    pub control: Option<LearnedControl>,
    /// The mapping made last, for the UI.
    pub done: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub port: String,
    pub name: String,
    pub profile: Option<String>,
    pub output: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LastInput {
    pub device: String,
    pub control: Option<String>,
    pub msg: String,
    pub value: u16,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MidiState {
    pub devices: Vec<DeviceInfo>,
    pub mappings: Vec<Mapping>,
    pub learn: Option<Learn>,
    /// Feedback per device id and control name.
    pub feedback: BTreeMap<String, BTreeMap<String, Fb>>,
    pub last_input: Option<LastInput>,
    /// Bumped when the mappings change (they are saved then).
    pub rev: u64,
    pub error: Option<String>,
}

/// What a control does in grandMA3.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Key { page: u32, exec: u32, pressed: bool },
    Fader { page: u32, exec: u32, value: f32 },
    Encoder { page: u32, exec: u32, delta: f32 },
}

/// The control an input belongs to: from the profile, an existing mapping, or a new generic one.
pub fn resolve(st: &State, device: &str, profile: Option<&DeviceProfile>, addr: &Addr) -> Control {
    if let Some(c) = profile.and_then(|p| p.controls.iter().find(|c| c.addr.as_ref() == Some(addr)))
    {
        return c.clone();
    }
    if let Some(m) = st
        .midi
        .mappings
        .iter()
        .find(|m| m.device == device && m.midi == *addr)
    {
        return m.control();
    }
    let kind = match addr.ty {
        MsgType::Note => Kind::Button,
        _ => Kind::Fader,
    };
    Control::generic(addr.to_string(), kind, *addr, Relative::default())
}

/// Handles one input on a control. Returns what to send to grandMA3.
pub fn on_input(
    st: &mut State,
    device: &str,
    control: &Control,
    generic: bool,
    input: Input,
    encoder_step: f32,
) -> Vec<Action> {
    st.midi.last_input = Some(LastInput {
        device: device.into(),
        control: Some(control.name.clone()),
        msg: input.addr.to_string(),
        value: input.value,
    });
    if let Some(learn) = &mut st.midi.learn {
        learn_control(learn, device, control, generic, input);
        return vec![];
    }
    let current = st.page.unwrap_or(1);
    st.midi
        .mappings
        .iter()
        .filter(|m| m.device == device && m.control == control.name)
        .filter_map(|m| {
            let page = m.page.unwrap_or(current);
            let max = input.addr.max_value();
            Some(match m.kind {
                Kind::Button => Action::Key {
                    page,
                    exec: m.exec,
                    pressed: match input.addr.ty {
                        MsgType::Note => input.value > 0,
                        _ => input.value * 2 > max,
                    },
                },
                Kind::Fader => Action::Fader {
                    page,
                    exec: m.exec,
                    value: input.value as f32 * 100.0 / max as f32,
                },
                Kind::Encoder => {
                    let delta = m.relative.delta(input.value);
                    if delta == 0 {
                        return None;
                    }
                    Action::Encoder {
                        page,
                        exec: m.exec,
                        delta: delta as f32 * encoder_step,
                    }
                }
                Kind::Display => return None,
            })
        })
        .collect()
}

fn learn_control(learn: &mut Learn, device: &str, control: &Control, generic: bool, input: Input) {
    let Some(addr) = control.addr else { return };
    let same = learn
        .control
        .as_ref()
        .is_some_and(|c| c.device == device && c.midi == addr);
    if !same {
        learn.control = Some(LearnedControl {
            device: device.into(),
            control: control.name.clone(),
            kind: control.kind,
            midi: addr,
            relative: control.relative,
            guessed: generic,
            values: vec![],
        });
    }
    let c = learn.control.as_mut().expect("set above");
    if c.values.len() < 16 {
        c.values.push(input.value);
    }
    if c.guessed && addr.ty == MsgType::Cc {
        (c.kind, c.relative) = guess_cc(&c.values);
    }
}

/// Guesses what a CC control is from the values it sent while being moved.
fn guess_cc(values: &[u16]) -> (Kind, Relative) {
    if values.len() < 2 {
        return (Kind::Fader, Relative::default());
    }
    if values.iter().all(|v| *v == 0 || *v == 127) && values.contains(&0) {
        return (Kind::Button, Relative::default());
    }
    let near = |v: u16| matches!(v, 1..=15 | 49..=63 | 65..=79 | 113..=127);
    let repeats = values.windows(2).any(|w| w[0] == w[1]);
    if repeats && values.iter().all(|v| near(*v)) {
        let rel = if values.iter().any(|v| *v >= 113) {
            Relative::TwosComplement
        } else if values.iter().any(|v| (49..=63).contains(v)) {
            Relative::Offset64
        } else {
            Relative::SignedBit
        };
        return (Kind::Encoder, rel);
    }
    (Kind::Fader, Relative::default())
}

/// Finishes learning: maps the learned control to executor `exec`.
pub fn assign(st: &mut State, exec: u32) -> Result<(), String> {
    let page = st.page;
    let learn = st.midi.learn.as_mut().ok_or("learn is not active")?;
    let c = learn
        .control
        .take()
        .ok_or("move a control on the MIDI controller first")?;
    let mapping = Mapping {
        device: c.device,
        control: c.control,
        kind: c.kind,
        midi: c.midi,
        relative: c.relative,
        exec,
        page: if learn.follow {
            None
        } else {
            Some(page.unwrap_or(1))
        },
    };
    learn.done = Some(format!("{} -> {}", mapping.control, target_name(&mapping)));
    st.midi
        .mappings
        .retain(|m| !(m.device == mapping.device && m.control == mapping.control));
    st.midi.mappings.push(mapping);
    st.midi.rev += 1;
    Ok(())
}

pub fn target_name(m: &Mapping) -> String {
    match m.page {
        Some(p) => format!("Exec {p}.{}", m.exec),
        None => format!("Exec {} (page follow)", m.exec),
    }
}

/// While learning: a key press or fader move in grandMA3 picks the executor.
pub fn learn_from_osc(st: &mut State, ev: &OscEvent) {
    let waiting = st.midi.learn.as_ref().is_some_and(|l| l.control.is_some());
    if !waiting {
        return;
    }
    let exec = match ev {
        OscEvent::SequenceKey { seq, state: 1, .. } | OscEvent::SequenceFader { seq, .. } => st
            .executors
            .iter()
            .find(|e| e.class == "Sequence" && e.obj == Some(*seq)),
        OscEvent::Master { key, .. } => st
            .executors
            .iter()
            .find(|e| e.addr.as_deref() == Some(key.as_str())),
        _ => None,
    };
    if let Some(no) = exec.map(|e| e.no) {
        let _ = assign(st, no);
    }
}

/// What a mapped executor should show.
pub fn feedback(st: &State, m: &Mapping) -> Fb {
    if !st.connections.plugin_connected || st.page.is_none() {
        return Fb::default();
    }
    if m.page.is_some() && m.page != st.page {
        // Only the current page is known; other pages stay dark.
        return Fb::default();
    }
    let Some(e) = st.executors.iter().find(|e| e.no == m.exec) else {
        return Fb::default();
    };
    let seq = (e.class == "Sequence")
        .then(|| e.obj.and_then(|o| st.sequences.get(&o)))
        .flatten();
    let master = e.addr.as_ref().and_then(|a| st.masters.get(a));
    let value = seq
        .and_then(|s| s.faders.get("FaderMaster").copied())
        .or_else(|| master.and_then(|m| m.values.get("FaderMaster").copied()))
        .or(e.fader);
    let active = match seq {
        Some(s) if s.playback != Playback::Unknown => s.playback == Playback::On,
        _ => e.active.unwrap_or(false),
    };
    let name = seq
        .map(|s| s.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| e.name.clone());
    Fb {
        level: if active { Level::On } else { Level::Dim },
        color: e.color.clone(),
        value,
        text: Some(name),
    }
}

/// Feedback for every control of a device: its profile controls plus generic mapped controls.
pub fn device_feedback(
    st: &State,
    device: &str,
    profile: Option<&DeviceProfile>,
) -> Vec<(Control, Fb)> {
    let mapped = |name: &str| {
        st.midi
            .mappings
            .iter()
            .find(|m| m.device == device && m.control == name)
    };
    let mut out = Vec::new();
    if let Some(p) = profile {
        for c in &p.controls {
            let source = c.shows.as_deref().unwrap_or(&c.name);
            let fb = mapped(source).map(|m| feedback(st, m)).unwrap_or_default();
            out.push((c.clone(), fb));
        }
    }
    for m in st.midi.mappings.iter().filter(|m| m.device == device) {
        if out.iter().any(|(c, _)| c.name == m.control) {
            continue;
        }
        out.push((m.control(), feedback(st, m)));
    }
    out
}

/// MIDI messages that show `fb` on a control. Empty when the control has no feedback or the
/// value is unknown.
pub fn encode(profile: Option<&DeviceProfile>, c: &Control, fb: &Fb) -> Vec<Vec<u8>> {
    let scale = |v: f32, min: u16, max: u16| {
        let v = (v.clamp(0.0, 100.0) / 100.0) * (max - min) as f32;
        min + v.round() as u16
    };
    match (&c.feedback, c.addr) {
        (Feedback::Led { on, dim, off }, Some(a)) => {
            let v = match fb.level {
                Level::On => *on,
                Level::Dim => *dim,
                Level::Off => *off,
            };
            vec![a.encode(v)]
        }
        (Feedback::Rgb { bright, dim }, Some(a)) => {
            if fb.level == Level::Off {
                return vec![a.encode_on_channel(*bright, 0)];
            }
            let rgb = fb
                .color
                .as_deref()
                .and_then(super::device::parse_color)
                .unwrap_or([255, 255, 255]);
            let idx = profile.map(|p| p.nearest(rgb)).unwrap_or(127);
            let ch = if fb.level == Level::On { *bright } else { *dim };
            vec![a.encode_on_channel(ch, idx as u16)]
        }
        (Feedback::Value, Some(a)) => match fb.value {
            Some(v) => vec![a.encode(scale(v, 0, a.max_value()))],
            None => vec![],
        },
        (Feedback::Ring { addr, min, max }, _) => match fb.value {
            Some(v) => vec![addr.encode(scale(v, *min, *max))],
            None => vec![],
        },
        (Feedback::Display { template, pos, len }, _) => {
            let text = fb.text.as_deref().unwrap_or("");
            super::device::display_sysex(template, *pos, *len, text)
                .map(|m| vec![m])
                .unwrap_or_default()
        }
        _ => vec![],
    }
}

/// Whether feedback for this control moves something the user may be holding.
pub fn is_motorised(c: &Control) -> bool {
    matches!(c.feedback, Feedback::Value | Feedback::Ring { .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Executor;

    fn note(n: u8) -> Addr {
        Addr {
            ty: MsgType::Note,
            channel: 1,
            number: n,
        }
    }

    fn cc(n: u8) -> Addr {
        Addr {
            ty: MsgType::Cc,
            channel: 1,
            number: n,
        }
    }

    fn state() -> State {
        let mut st = State::default();
        st.connections.plugin_connected = true;
        let ex = |no, obj, color: &str| Executor {
            no,
            class: "Sequence".into(),
            obj: Some(obj),
            name: format!("Seq {obj}"),
            addr: Some(format!("14.14.1.7.{obj}")),
            color: Some(color.into()),
            active: Some(false),
            fader: Some(100.0),
            cue: None,
        };
        st.apply_page(Some(1), vec![ex(201, 1, "#ff0000"), ex(202, 2, "#0000ff")]);
        st
    }

    #[test]
    fn learn_by_ui_and_by_grandma3() {
        let mut st = state();
        let p = DeviceProfile::test();
        st.midi.learn = Some(Learn {
            follow: true,
            ..Default::default()
        });
        // Move "Button 1", pick executor 202 in the UI.
        let c = resolve(&st, "test", Some(&p), &note(0));
        assert_eq!(c.name, "Button 1");
        let acts = on_input(
            &mut st,
            "test",
            &c,
            false,
            Input {
                addr: note(0),
                value: 127,
            },
            1.0,
        );
        assert!(acts.is_empty());
        assign(&mut st, 202).unwrap();
        assert_eq!(st.midi.mappings[0].exec, 202);
        assert_eq!(st.midi.mappings[0].page, None);
        assert!(assign(&mut st, 203).is_err());

        // Move "Fader 1", press sequence 1 in grandMA3 -> executor 201.
        let c = resolve(&st, "test", Some(&p), &cc(0));
        on_input(
            &mut st,
            "test",
            &c,
            false,
            Input {
                addr: cc(0),
                value: 64,
            },
            1.0,
        );
        learn_from_osc(
            &mut st,
            &OscEvent::SequenceKey {
                seq: 1,
                action: "Go+".into(),
                state: 1,
                text: String::new(),
            },
        );
        assert_eq!(st.midi.mappings.len(), 2);
        assert_eq!(st.midi.mappings[1].exec, 201);
        assert_eq!(st.midi.mappings[1].kind, Kind::Fader);
        assert_eq!(st.midi.rev, 2);

        // Relearning a control replaces its mapping.
        on_input(
            &mut st,
            "test",
            &c,
            false,
            Input {
                addr: cc(0),
                value: 1,
            },
            1.0,
        );
        assign(&mut st, 202).unwrap();
        assert_eq!(st.midi.mappings.len(), 2);

        // Play: button presses executor 202 on the current page.
        st.midi.learn = None;
        st.page = Some(3);
        let c = resolve(&st, "test", Some(&p), &note(0));
        let acts = on_input(
            &mut st,
            "test",
            &c,
            false,
            Input {
                addr: note(0),
                value: 127,
            },
            1.0,
        );
        assert_eq!(
            acts,
            vec![Action::Key {
                page: 3,
                exec: 202,
                pressed: true
            }]
        );
        let acts = on_input(
            &mut st,
            "test",
            &c,
            false,
            Input {
                addr: note(0),
                value: 0,
            },
            1.0,
        );
        assert_eq!(
            acts,
            vec![Action::Key {
                page: 3,
                exec: 202,
                pressed: false
            }]
        );
    }

    #[test]
    fn guesses_generic_cc_controls() {
        assert_eq!(guess_cc(&[10, 12, 15, 20]).0, Kind::Fader);
        assert_eq!(guess_cc(&[127, 0, 127]).0, Kind::Button);
        assert_eq!(guess_cc(&[1, 1, 2]), (Kind::Encoder, Relative::SignedBit));
        assert_eq!(
            guess_cc(&[127, 127]),
            (Kind::Encoder, Relative::TwosComplement)
        );
        assert_eq!(guess_cc(&[63, 63, 62]), (Kind::Encoder, Relative::Offset64));
        assert_eq!(guess_cc(&[127]).0, Kind::Fader);
    }

    #[test]
    fn generic_device_learn_and_play() {
        let mut st = state();
        st.midi.learn = Some(Learn::default());
        let c = resolve(&st, "Some Port", None, &cc(20));
        for v in [1, 1, 1] {
            on_input(
                &mut st,
                "Some Port",
                &c,
                true,
                Input {
                    addr: cc(20),
                    value: v,
                },
                1.0,
            );
        }
        assign(&mut st, 201).unwrap();
        let m = &st.midi.mappings[0];
        assert_eq!((m.kind, m.page), (Kind::Encoder, Some(1)));
        st.midi.learn = None;
        let c = resolve(&st, "Some Port", None, &cc(20));
        assert_eq!(c.kind, Kind::Encoder);
        let acts = on_input(
            &mut st,
            "Some Port",
            &c,
            true,
            Input {
                addr: cc(20),
                value: 66,
            },
            2.0,
        );
        assert_eq!(
            acts,
            vec![Action::Encoder {
                page: 1,
                exec: 201,
                delta: -4.0
            }]
        );
    }

    #[test]
    fn feedback_follows_state() {
        let mut st = state();
        let m = Mapping {
            device: "test".into(),
            control: "Button 1".into(),
            kind: Kind::Button,
            midi: note(0),
            relative: Relative::default(),
            exec: 201,
            page: None,
        };
        let fb = feedback(&st, &m);
        assert_eq!(fb.level, Level::Dim);
        assert_eq!(fb.color.as_deref(), Some("#ff0000"));
        assert_eq!(fb.value, Some(100.0));
        st.apply_osc(OscEvent::SequenceKey {
            seq: 1,
            action: "Go+".into(),
            state: 1,
            text: "Seq 1;1 Cue".into(),
        });
        st.apply_osc(OscEvent::SequenceFader {
            seq: 1,
            token: "FaderMaster".into(),
            value: 40.0,
            name: None,
        });
        let fb = feedback(&st, &m);
        assert_eq!((fb.level, fb.value), (Level::On, Some(40.0)));

        // Fixed page 2 is not shown: dark.
        let fixed = Mapping {
            page: Some(2),
            ..m.clone()
        };
        assert_eq!(feedback(&st, &fixed), Fb::default());
        // Empty executor: dark.
        let empty = Mapping {
            exec: 299,
            ..m.clone()
        };
        assert_eq!(feedback(&st, &empty).level, Level::Off);
        st.connections.plugin_connected = false;
        assert_eq!(feedback(&st, &m), Fb::default());
    }

    #[test]
    fn encodes_feedback() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("devices");
        let ps = super::super::device::load_all(&dir);
        let apc = ps.iter().find(|p| p.id == "akai-apc-mini-mk2").unwrap();
        let pad = apc.control("Pad 1").unwrap();
        let fb = |level, color: &str, value| Fb {
            level,
            color: Some(color.into()),
            value,
            text: Some("Wash".into()),
        };
        assert_eq!(
            encode(Some(apc), pad, &fb(Level::On, "#ff0000", None)),
            vec![vec![0x96, 0, 5]]
        );
        assert_eq!(
            encode(Some(apc), pad, &fb(Level::Dim, "#0000ff", None)),
            vec![vec![0x91, 0, 45]]
        );
        assert_eq!(
            encode(Some(apc), pad, &fb(Level::Off, "#0000ff", None)),
            vec![vec![0x96, 0, 0]]
        );
        let track = apc.control("Track 1").unwrap();
        assert_eq!(
            encode(Some(apc), track, &fb(Level::On, "#fff", None)),
            vec![vec![0x90, 100, 1]]
        );
        // APC faders have no motor.
        assert!(encode(
            Some(apc),
            apc.control("Fader 1").unwrap(),
            &fb(Level::On, "", Some(50.0))
        )
        .is_empty());

        let mcu = ps.iter().find(|p| p.id == "mackie-control").unwrap();
        let f = mcu.control("Fader 2").unwrap();
        assert_eq!(
            encode(Some(mcu), f, &fb(Level::On, "", Some(100.0))),
            vec![vec![0xe1, 0x7f, 0x7f]]
        );
        assert!(encode(Some(mcu), f, &Fb::default()).is_empty());
        let ring = mcu.control("Encoder 1").unwrap();
        assert_eq!(
            encode(Some(mcu), ring, &fb(Level::On, "", Some(50.0))),
            vec![vec![0xb0, 48, 38]]
        );
        assert!(is_motorised(f) && is_motorised(ring));
    }
}
