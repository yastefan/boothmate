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
# macOS: offer a virtual MIDI port "Boothmate" for software controllers.
virtual_port = true
# MIDI ports to leave alone (parts of the name), e.g. ["IAC", "Network"].
ignore = []

[web]
listen = "127.0.0.1:8088"
"#;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub osc: Osc,
    pub plugin: Plugin,
    pub ma: Ma,
    pub midi: Midi,
    pub web: Web,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Midi {
    pub virtual_port: bool,
    pub ignore: Vec<String>,
}

impl Default for Midi {
    fn default() -> Self {
        Self {
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
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = toml::from_str("[web]\nlisten = \"0.0.0.0:80\"").unwrap();
        assert_eq!(c.web.listen, "0.0.0.0:80");
        assert_eq!(c.osc.listen, "127.0.0.1:9001");
    }
}
