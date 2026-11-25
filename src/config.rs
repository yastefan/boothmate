//! `boothmate.toml`: global settings, read from the data folder next to the binary.

use serde::Deserialize;
use std::path::Path;

/// Written to the data folder on first start when no config exists.
pub const DEFAULT_CONFIG: &str = r#"# Boothmate settings. Restart Boothmate after a change.

[osc]
# Where grandMA3 sends its OSC output (OSC line with Send = Yes).
listen = "127.0.0.1:9001"

[plugin]
# The Boothmate Lua plugin inside grandMA3. On a separate machine, set the console IP here
# and start the plugin with  Plugin "boothmate" "bind=0.0.0.0 token=<secret>".
host = "127.0.0.1"
port = 9002
token = ""

[ma]
# grandMA3 profile from ma-profiles/. "auto" picks it from the version the plugin reports.
profile = "auto"

[web]
listen = "127.0.0.1:8080"
"#;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub osc: Osc,
    pub plugin: Plugin,
    pub ma: Ma,
    pub web: Web,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Osc {
    pub listen: String,
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
            listen: "127.0.0.1:8080".into(),
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
    }

    #[test]
    fn partial_config_uses_defaults() {
        let c: Config = toml::from_str("[web]\nlisten = \"0.0.0.0:80\"").unwrap();
        assert_eq!(c.web.listen, "0.0.0.0:80");
        assert_eq!(c.osc.listen, "127.0.0.1:9001");
    }
}
