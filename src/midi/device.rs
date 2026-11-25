//! Device profiles (`devices/*.toml`): which controls a controller has and how it shows feedback.
//!
//! A profile lists control groups. Each group expands to `count` controls named after `name`
//! (`{i}` is replaced with 1, 2, ...), on consecutive note/CC numbers or channels.

use super::msg::{hex_bytes, Addr, MsgType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Id of the built-in test controller in the web UI.
pub const TEST_ID: &str = "test";

/// The test controller in the web UI: 8 RGB buttons, 8 motor faders, 4 encoders.
const TEST_PROFILE: &str = r##"
name = "Test controller (web UI)"

[[controls]]
name = "Button {i}"
kind = "button"
type = "note"
number = 0
count = 8
feedback = "rgb"

[[controls]]
name = "Fader {i}"
kind = "fader"
type = "cc"
number = 0
count = 8
feedback = "value"
touch = 64

[[controls]]
name = "Encoder {i}"
kind = "encoder"
type = "cc"
number = 16
count = 4
relative = "signed-bit"
feedback = "ring"
ring = 48
"##;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Button,
    Fader,
    Encoder,
    Display,
}

/// How an endless encoder encodes a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relative {
    /// Mackie Control: 1..63 up, 65..127 down (bit 6 = sign).
    #[default]
    SignedBit,
    /// 1..63 up, 127..65 down.
    TwosComplement,
    /// 65 = +1, 63 = -1.
    Offset64,
}

impl Relative {
    pub fn delta(self, v: u16) -> i32 {
        let v = (v & 0x7f) as i32;
        match self {
            Relative::SignedBit => {
                if v & 0x40 != 0 {
                    -(v & 0x3f)
                } else {
                    v
                }
            }
            Relative::TwosComplement => {
                if v >= 64 {
                    v - 128
                } else {
                    v
                }
            }
            Relative::Offset64 => v - 64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Advance {
    #[default]
    Number,
    Channel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FeedbackKind {
    #[default]
    None,
    Led,
    Rgb,
    Value,
    Ring,
}

fn one() -> u8 {
    1
}

#[derive(Debug, Clone, Deserialize)]
struct Group {
    name: String,
    kind: Kind,
    #[serde(rename = "type")]
    ty: Option<MsgType>,
    #[serde(default = "one")]
    channel: u8,
    #[serde(default)]
    number: u8,
    #[serde(default = "one")]
    count: u8,
    #[serde(default)]
    advance: Advance,
    #[serde(default)]
    feedback: FeedbackKind,
    // led
    led_on: Option<u16>,
    led_dim: Option<u16>,
    led_off: Option<u16>,
    // rgb
    bright_channel: Option<u8>,
    dim_channel: Option<u8>,
    // fader
    touch: Option<u8>,
    // encoder
    #[serde(default)]
    relative: Relative,
    ring: Option<u8>,
    ring_min: Option<u16>,
    ring_max: Option<u16>,
    // display
    shows: Option<String>,
    sysex: Option<String>,
    #[serde(default)]
    pos: u8,
    #[serde(default)]
    pos_step: u8,
    len: Option<u8>,
}

#[derive(Debug, Clone, Deserialize)]
struct File {
    name: String,
    #[serde(default)]
    ports: Vec<String>,
    /// Palette for RGB buttons: velocity -> `#rrggbb`.
    #[serde(default)]
    palette: BTreeMap<String, String>,
    /// Fader percent per encoder step.
    encoder_step: Option<f32>,
    #[serde(default)]
    controls: Vec<Group>,
}

/// How a control shows feedback.
#[derive(Debug, Clone, PartialEq)]
pub enum Feedback {
    None,
    /// Single-colour LED: note/CC on the control's address with these values.
    Led {
        on: u16,
        dim: u16,
        off: u16,
    },
    /// Palette colour as velocity; the channel sets the brightness.
    Rgb {
        bright: u8,
        dim: u8,
    },
    /// Motor fader: the value on the control's address.
    Value,
    /// LED ring: CC with a value between `min` and `max`.
    Ring {
        addr: Addr,
        min: u16,
        max: u16,
    },
    /// Text by SysEx: template with `{pos}` and `{text}`.
    Display {
        template: String,
        pos: u8,
        len: u8,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub name: String,
    pub kind: Kind,
    /// Input address (none for displays).
    pub addr: Option<Addr>,
    pub feedback: Feedback,
    pub relative: Relative,
    /// Touch-sensitive fader: note that reports touch.
    pub touch: Option<Addr>,
    /// Display: name of the control whose executor it shows.
    pub shows: Option<String>,
}

impl Control {
    /// A control learned from a device without a profile. Feedback mirrors the input.
    pub fn generic(name: String, kind: Kind, addr: Addr, relative: Relative) -> Self {
        let feedback = match kind {
            Kind::Button => Feedback::Led {
                on: 127,
                dim: 0,
                off: 0,
            },
            Kind::Fader => Feedback::Value,
            _ => Feedback::None,
        };
        Control {
            name,
            kind,
            addr: Some(addr),
            feedback,
            relative,
            touch: None,
            shows: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeviceProfile {
    pub id: String,
    pub name: String,
    pub ports: Vec<String>,
    pub palette: Vec<(u8, [u8; 3])>,
    pub encoder_step: f32,
    pub controls: Vec<Control>,
}

impl DeviceProfile {
    pub fn parse(id: &str, text: &str) -> Result<Self, String> {
        let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut palette = Vec::new();
        for (k, v) in &f.palette {
            let idx: u8 = k.parse().map_err(|_| format!("palette: bad index {k}"))?;
            palette.push((
                idx,
                parse_color(v).ok_or(format!("palette: bad colour {v}"))?,
            ));
        }
        let mut controls = Vec::new();
        for g in &f.controls {
            for i in 0..g.count {
                controls.push(expand(g, i)?);
            }
        }
        Ok(DeviceProfile {
            id: id.into(),
            name: f.name,
            ports: f.ports,
            palette,
            encoder_step: f.encoder_step.unwrap_or(1.0),
            controls,
        })
    }

    pub fn test() -> Self {
        Self::parse(TEST_ID, TEST_PROFILE).expect("built-in test profile")
    }

    pub fn matches_port(&self, port: &str) -> bool {
        let port = port.to_lowercase();
        self.ports.iter().any(|p| port.contains(&p.to_lowercase()))
    }

    pub fn control(&self, name: &str) -> Option<&Control> {
        self.controls.iter().find(|c| c.name == name)
    }

    /// The fader whose touch sensor sends on `addr`.
    pub fn touched(&self, addr: &Addr) -> Option<&Control> {
        self.controls
            .iter()
            .find(|c| c.touch.as_ref() == Some(addr))
    }

    /// Nearest palette entry for a colour, compared at full brightness so that dark appearance
    /// colours keep their hue.
    pub fn nearest(&self, rgb: [u8; 3]) -> u8 {
        let norm = |c: [u8; 3]| {
            let m = c.iter().copied().max().unwrap_or(0).max(1) as f32;
            c.map(|v| v as f32 * 255.0 / m)
        };
        let want = norm(rgb);
        self.palette
            .iter()
            .filter(|(_, c)| *c != [0, 0, 0])
            .min_by(|a, b| {
                let d = |c: [u8; 3]| {
                    let c = norm(c);
                    (0..3).map(|i| (c[i] - want[i]).powi(2)).sum::<f32>()
                };
                d(a.1).total_cmp(&d(b.1))
            })
            .map(|(i, _)| *i)
            .unwrap_or(127)
    }
}

fn expand(g: &Group, i: u8) -> Result<Control, String> {
    let name = g.name.replace("{i}", &(i + 1).to_string());
    let (channel, number) = match g.advance {
        Advance::Number => (g.channel, g.number.wrapping_add(i)),
        Advance::Channel => (g.channel + i, g.number),
    };
    let addr = g.ty.map(|ty| Addr {
        ty,
        channel,
        number,
    });
    if g.kind != Kind::Display && addr.is_none() {
        return Err(format!("{name}: missing type"));
    }
    let feedback = match (g.kind, g.feedback) {
        (Kind::Display, _) => Feedback::Display {
            template: g.sysex.clone().ok_or(format!("{name}: missing sysex"))?,
            pos: g.pos + i * g.pos_step,
            len: g.len.unwrap_or(7),
        },
        (_, FeedbackKind::None) => Feedback::None,
        (_, FeedbackKind::Led) => Feedback::Led {
            on: g.led_on.unwrap_or(127),
            dim: g.led_dim.unwrap_or(0),
            off: g.led_off.unwrap_or(0),
        },
        (_, FeedbackKind::Rgb) => Feedback::Rgb {
            bright: g.bright_channel.unwrap_or(channel),
            dim: g.dim_channel.unwrap_or(channel),
        },
        (_, FeedbackKind::Value) => Feedback::Value,
        (_, FeedbackKind::Ring) => Feedback::Ring {
            addr: Addr {
                ty: MsgType::Cc,
                channel,
                number: g
                    .ring
                    .ok_or(format!("{name}: missing ring"))?
                    .wrapping_add(i),
            },
            min: g.ring_min.unwrap_or(0),
            max: g.ring_max.unwrap_or(127),
        },
    };
    let touch = g.touch.map(|n| Addr {
        ty: MsgType::Note,
        channel: 1,
        number: n.wrapping_add(i),
    });
    Ok(Control {
        name,
        kind: g.kind,
        addr,
        feedback,
        relative: g.relative,
        touch,
        shows: g
            .shows
            .as_ref()
            .map(|s| s.replace("{i}", &(i + 1).to_string())),
    })
}

pub fn parse_color(s: &str) -> Option<[u8; 3]> {
    let h = s.trim_start_matches('#');
    if h.len() < 6 {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// Builds a display SysEx message from a template such as `F0 00 00 66 14 12 {pos} {text} F7`.
pub fn display_sysex(template: &str, pos: u8, len: u8, text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for tok in template.split_whitespace() {
        match tok {
            "{pos}" => out.push(pos & 0x7f),
            "{text}" => {
                let mut chars: Vec<u8> = text
                    .chars()
                    .map(|c| {
                        if c.is_ascii() && !c.is_control() {
                            c as u8
                        } else {
                            b'?'
                        }
                    })
                    .take(len as usize)
                    .collect();
                chars.resize(len as usize, b' ');
                out.extend(chars);
            }
            _ => out.extend(hex_bytes(tok)?),
        }
    }
    Ok(out)
}

/// Loads `devices/*.toml`. A broken file is reported and skipped.
pub fn load_all(dir: &Path) -> Vec<DeviceProfile> {
    let mut out = vec![DeviceProfile::test()];
    let Ok(entries) = std::fs::read_dir(dir) else {
        crate::log::warn(&format!("MIDI: no device profiles in {}", dir.display()));
        return out;
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| DeviceProfile::parse(&id, &t))
        {
            Ok(p) => out.push(p),
            Err(e) => crate::log::warn(&format!("MIDI: {}: {e}", path.display())),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_encodings() {
        assert_eq!(Relative::SignedBit.delta(1), 1);
        assert_eq!(Relative::SignedBit.delta(65), -1);
        assert_eq!(Relative::SignedBit.delta(70), -6);
        assert_eq!(Relative::TwosComplement.delta(127), -1);
        assert_eq!(Relative::TwosComplement.delta(3), 3);
        assert_eq!(Relative::Offset64.delta(63), -1);
        assert_eq!(Relative::Offset64.delta(66), 2);
    }

    #[test]
    fn test_profile_expands() {
        let p = DeviceProfile::test();
        assert_eq!(p.controls.len(), 20);
        let f = p.control("Fader 3").unwrap();
        assert_eq!(f.addr.unwrap().number, 2);
        assert_eq!(f.touch.unwrap().number, 66);
        assert_eq!(p.touched(&f.touch.unwrap()).unwrap().name, "Fader 3");
        let e = p.control("Encoder 2").unwrap();
        assert!(matches!(e.feedback, Feedback::Ring { addr, .. } if addr.number == 49));
    }

    #[test]
    fn shipped_profiles_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("devices");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            let text = std::fs::read_to_string(&path).unwrap();
            let p = DeviceProfile::parse("x", &text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(!p.controls.is_empty());
        }
        let ps = load_all(&dir);
        let apc = ps.iter().find(|p| p.id == "akai-apc-mini-mk2").unwrap();
        assert!(apc.matches_port("APC mini mk2 Control"));
        assert_eq!(apc.nearest([255, 0, 0]), 5);
        assert_eq!(apc.nearest([0, 0, 128]), 45);
        let mcu = ps.iter().find(|p| p.id == "mackie-control").unwrap();
        let fader = mcu.control("Fader 2").unwrap();
        assert_eq!(fader.addr.unwrap().ty, MsgType::PitchBend);
        assert_eq!(fader.addr.unwrap().channel, 2);
        let d = mcu.control("Display 2").unwrap();
        let Feedback::Display { template, pos, len } = &d.feedback else {
            panic!()
        };
        assert_eq!(
            display_sysex(template, *pos, *len, "Wash").unwrap(),
            vec![0xf0, 0, 0, 0x66, 0x14, 0x12, 7, b'W', b'a', b's', b'h', b' ', b' ', b' ', 0xf7]
        );
    }
}
