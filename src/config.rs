//! `boothmate.toml`: global settings, read from the data folder next to the binary.

use serde::Deserialize;
use std::path::Path;

/// Written to the data folder on first start when no config exists.
pub const DEFAULT_CONFIG: &str = r#"# Boothmate settings. Restart Boothmate after a change.

[osc]
# Where grandMA3 sends its OSC output (OSC line with Send = Yes).
listen = "127.0.0.1:9001"
# Where Boothmate sends executor keys and faders: a second OSC line with Receive = Yes.
send = "127.0.0.1:8000"
# The OSC lines' Prefix in grandMA3, without slashes (empty = none).
prefix = ""

[plugin]
# The Boothmate Lua plugin inside grandMA3. On a separate machine, set the console IP here
# and start the plugin with  Plugin "boothmate" "bind=0.0.0.0 token=<secret>".
host = "127.0.0.1"
port = 9002
token = ""

[ma]
# grandMA3 profile from ma-profiles/. "auto" picks it from the version the plugin reports.
profile = "auto"

[midi]
# MIDI controllers. Off = no MIDI thread and no port scanning. Also switchable in the web UI.
enabled = true
# The virtual controller in the web UI (MIDI tab), for trying mappings without hardware.
virtual_controller = false
# macOS: offer a virtual MIDI port "Boothmate" for software controllers.
virtual_port = true
# MIDI ports to leave alone (parts of the name), e.g. ["IAC", "Network"].
ignore = []

[cdj]
# Pioneer Pro DJ Link, read-only: finds CDJs and mixers on the network and reads tempo and beats.
# Off = no sockets and no timers. Also switchable in the web UI.
enabled = true
# Network interface for DJ Link, by name ("en5", "Ethernet 2") or IP. Empty = the one the first
# CDJ or mixer is seen on.
interface = ""
# Announce Boothmate as a silent virtual player, so players also send play state, track and beat
# number. It never controls a player.
status = true
# Device number of that virtual player, 7-15: outside the player numbers, so it never takes a
# CDJ's place.
player = 7

[timecode]
# Experimental: the CDJ playhead as Art-Net timecode (ArtTimeCode) for grandMA3 timecode shows.
# Stream 0 = tempo master, stream n = player n. Can also be switched on in the web UI.
enabled = false
# Where to send: grandMA3's Art-Net interface (IP:6454) or a broadcast address.
target = "127.0.0.1:6454"
# 24, 25 or 30.
fps = 30

[web]
listen = "127.0.0.1:8088"
# Open the Boothmate window (macOS, Windows). Off = only the web UI; open it in a browser.
# The web UI is always there too, also with the window.
window = true
"#;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub osc: Osc,
    pub plugin: Plugin,
    pub ma: Ma,
    pub midi: Midi,
    pub cdj: Cdj,
    pub timecode: Timecode,
    pub web: Web,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Timecode {
    pub enabled: bool,
    pub target: String,
    pub fps: u8,
}

impl Default for Timecode {
    fn default() -> Self {
        Self {
            enabled: false,
            target: "127.0.0.1:6454".into(),
            fps: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Cdj {
    pub enabled: bool,
    pub interface: String,
    pub status: bool,
    pub player: u8,
}

impl Default for Cdj {
    fn default() -> Self {
        Self {
            enabled: true,
            interface: String::new(),
            status: true,
            player: 7,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Midi {
    pub enabled: bool,
    pub virtual_controller: bool,
    pub virtual_port: bool,
    pub ignore: Vec<String>,
}

impl Default for Midi {
    fn default() -> Self {
        Self {
            enabled: true,
            virtual_controller: false,
            virtual_port: true,
            ignore: vec![],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Osc {
    pub listen: String,
    pub send: String,
    pub prefix: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Plugin {
    pub host: String,
    pub port: u16,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Ma {
    pub profile: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Web {
    pub listen: String,
    /// Show the web UI in a native window (macOS, Windows).
    pub window: bool,
}

impl Default for Osc {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:9001".into(),
            send: "127.0.0.1:8000".into(),
            prefix: String::new(),
        }
    }
}

impl Default for Plugin {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 9002,
            token: String::new(),
        }
    }
}

impl Default for Ma {
    fn default() -> Self {
        Self {
            profile: "auto".into(),
        }
    }
}

impl Default for Web {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8088".into(),
            window: true,
        }
    }
}

impl Config {
    /// Loads `boothmate.toml` from `dir`, creating it with defaults if it does not exist.
    pub fn load_or_create(dir: &Path) -> Result<Self, String> {
        let path = dir.join("boothmate.toml");
        if !path.exists() {
            std::fs::write(&path, DEFAULT_CONFIG)
                .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(c.osc.listen, "127.0.0.1:9001");
        assert_eq!(c.plugin.port, 9002);
        assert_eq!(c.ma.profile, "auto");
        assert_eq!(c.osc.send, "127.0.0.1:8000");
        assert!(c.midi.virtual_port);
        assert!(c.cdj.enabled && c.cdj.status);
        assert_eq!(c.cdj.player, 7);
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = toml::from_str("[web]\nlisten = \"0.0.0.0:80\"").unwrap();
        assert_eq!(c.web.listen, "0.0.0.0:80");
        assert!(c.web.window);
        assert_eq!(c.osc.listen, "127.0.0.1:9001");
    }
}

/// Sets `key = value` in `[section]` of the TOML text, keeping comments and everything else.
/// Adds the key (after the section header) or the section (at the end) when missing.
pub fn set_value(text: &str, section: &str, key: &str, value: &str) -> String {
    let header = format!("[{section}]");
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let Some(start) = lines.iter().position(|l| l.trim() == header) else {
        let mut out = text.trim_end().to_string();
        out.push_str(&format!("\n\n{header}\n{key} = {value}\n"));
        return out;
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let is_key = |l: &str| {
        let t = l.trim_start();
        t.strip_prefix(key)
            .is_some_and(|r| r.trim_start().starts_with('='))
    };
    match (start + 1..end).find(|&i| is_key(&lines[i])) {
        Some(i) => lines[i] = format!("{key} = {value}"),
        None => lines.insert(start + 1, format!("{key} = {value}")),
    }
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Saves one setting to `boothmate.toml` in the data folder.
pub fn save_value(dir: &Path, section: &str, key: &str, value: &str) -> Result<(), String> {
    let path = dir.join("boothmate.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, set_value(&text, section, key, value))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

#[cfg(test)]
mod edit_tests {
    use super::*;

    #[test]
    fn set_value_keeps_comments() {
        let t =
            "# top\n[midi]\n# on or off\nenabled = true\nignore = []\n\n[cdj]\nenabled = true\n";
        let out = set_value(t, "cdj", "enabled", "false");
        assert_eq!(
            out,
            "# top\n[midi]\n# on or off\nenabled = true\nignore = []\n\n[cdj]\nenabled = false\n"
        );
        let out = set_value(&out, "midi", "virtual_controller", "true");
        assert!(out.contains("[midi]\nvirtual_controller = true\n# on or off"));
        let out = set_value(&out, "timecode", "enabled", "true");
        assert!(out.ends_with("[timecode]\nenabled = true\n"));
        let c: Config = toml::from_str(&out).unwrap();
        assert!(!c.cdj.enabled && c.midi.virtual_controller && c.timecode.enabled);
    }

    #[test]
    fn default_config_parses() {
        let c: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert!(c.midi.enabled && !c.midi.virtual_controller && c.cdj.enabled);
    }
}
