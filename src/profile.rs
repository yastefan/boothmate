//! grandMA3 version profiles (`ma-profiles/*.toml`).
//!
//! Everything that changes between grandMA3 versions lives here, so a new version needs a new
//! file, not a rebuild. Address templates use `{n}` for the pool number.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Profile {
    pub name: String,
    /// Version prefixes this profile is verified for, e.g. `["2.5"]`.
    pub versions: Vec<String>,
    pub osc: OscAddresses,
    /// File stem, filled in on load.
    #[serde(skip)]
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OscAddresses {
    pub sequence: String,
    /// Known masters (group, grand, speed, ...), checked in order.
    #[serde(default)]
    pub master: Vec<MasterAddress>,
    /// Any other address with this prefix is shown as a master too (e.g. `/14.13.`).
    #[serde(default)]
    pub master_prefix: Option<String>,
    /// Addresses grandMA3 receives for executors.
    #[serde(default)]
    pub input: InputAddresses,
}

/// OSC input to grandMA3. `{page}` and `{exec}` are replaced; the OSC line's prefix goes in front.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct InputAddresses {
    /// Integer 1 = press, 0 = release.
    pub key: String,
    /// Float 0..`fader_range`.
    pub fader: String,
    /// Float: percent to move the fader, negative = down.
    pub encoder: String,
    pub fader_range: f32,
}

impl Default for InputAddresses {
    fn default() -> Self {
        Self {
            key: "/Page{page}/Key{exec}".into(),
            fader: "/Page{page}/Fader{exec}".into(),
            encoder: "/Page{page}/Encoder{exec}".into(),
            fader_range: 100.0,
        }
    }
}

impl InputAddresses {
    pub fn address(template: &str, prefix: &str, page: u32, exec: u32) -> String {
        let a = template
            .replace("{page}", &page.to_string())
            .replace("{exec}", &exec.to_string());
        let prefix = prefix.trim_matches('/');
        if prefix.is_empty() {
            a
        } else {
            format!("/{prefix}{a}")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MasterAddress {
    pub address: String,
    /// Display name; `{n}` is replaced with the number.
    pub label: String,
}

/// What an OSC address refers to, according to a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Sequence(u32),
    /// `key` is the object address without the leading `/` (`14.13.3.1`), as the plugin reports it.
    Master {
        key: String,
        label: String,
    },
}

/// The profile picked for a grandMA3 version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fit {
    /// A profile lists this version.
    Exact,
    /// No profile lists it; the closest one is used and Boothmate must not write to grandMA3.
    Fallback,
}

impl Profile {
    pub fn target(&self, addr: &str) -> Option<Target> {
        if let Some(n) = match_template(&self.osc.sequence, addr) {
            return Some(Target::Sequence(n));
        }
        let key = addr.trim_start_matches('/').to_string();
        for m in &self.osc.master {
            if let Some(n) = match_template(&m.address, addr) {
                let label = m.label.replace("{n}", &n.to_string());
                return Some(Target::Master { key, label });
            }
        }
        let rest = addr.strip_prefix(self.osc.master_prefix.as_deref()?)?;
        if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
            return None;
        }
        Some(Target::Master {
            key,
            label: format!("Master {rest}"),
        })
    }

    /// The OSC address grandMA3 uses for an object address such as `14.14.1.7.2`.
    pub fn osc_address(object_addr: &str) -> String {
        format!("/{}", object_addr.trim_start_matches('/'))
    }
}

fn match_template(template: &str, addr: &str) -> Option<u32> {
    let (pre, post) = template.split_once("{n}")?;
    let n = addr.strip_prefix(pre)?.strip_suffix(post)?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    n.parse().ok()
}

pub fn load_all(dir: &Path) -> Result<Vec<Profile>, String> {
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let text =
            std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut p: Profile =
            toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        p.id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        out.push(p);
    }
    if out.is_empty() {
        return Err(format!("no profiles in {}", dir.display()));
    }
    out.sort_by_key(|p| version_key(&p.id));
    Ok(out)
}

fn version_key(v: &str) -> Vec<u32> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| p.parse().ok())
        .collect()
}

/// Picks a profile by id (`"2.5"`), or for a reported version (`"2.5.1.1"`) when `wanted` is
/// `"auto"`. Without a version, the newest profile is used.
pub fn select<'a>(
    profiles: &'a [Profile],
    wanted: &str,
    ma_version: Option<&str>,
) -> (&'a Profile, Fit) {
    if wanted != "auto" {
        if let Some(p) = profiles.iter().find(|p| p.id == wanted) {
            return (p, Fit::Exact);
        }
    }
    let Some(version) = ma_version else {
        return (profiles.last().expect("at least one profile"), Fit::Exact);
    };
    let exact = profiles.iter().find(|p| {
        p.versions
            .iter()
            .any(|v| version == v || version.starts_with(&format!("{v}.")))
    });
    if let Some(p) = exact {
        return (p, Fit::Exact);
    }
    // Closest: the newest profile not newer than the version, else the oldest.
    let key = version_key(version);
    let p = profiles
        .iter()
        .rev()
        .find(|p| version_key(&p.id) <= key)
        .unwrap_or(&profiles[0]);
    (p, Fit::Fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: &str, versions: &[&str]) -> Profile {
        Profile {
            name: id.into(),
            versions: versions.iter().map(|s| s.to_string()).collect(),
            osc: OscAddresses {
                sequence: "/14.14.1.7.{n}".into(),
                master: vec![
                    MasterAddress {
                        address: "/14.14.1.6.{n}".into(),
                        label: "Group {n}".into(),
                    },
                    MasterAddress {
                        address: "/14.13.3.{n}".into(),
                        label: "Speed {n}".into(),
                    },
                ],
                master_prefix: Some("/14.13.".into()),
                input: InputAddresses::default(),
            },
            id: id.into(),
        }
    }

    #[test]
    fn targets() {
        let p = profile("2.5", &["2.5"]);
        assert_eq!(p.target("/14.14.1.7.2"), Some(Target::Sequence(2)));
        assert_eq!(p.target("/14.14.1.7.10"), Some(Target::Sequence(10)));
        let m = |k: &str, l: &str| {
            Some(Target::Master {
                key: k.into(),
                label: l.into(),
            })
        };
        assert_eq!(p.target("/14.14.1.6.1"), m("14.14.1.6.1", "Group 1"));
        assert_eq!(p.target("/14.13.3.1"), m("14.13.3.1", "Speed 1"));
        assert_eq!(p.target("/14.13.2.1"), m("14.13.2.1", "Master 2.1"));
        assert_eq!(p.target("/14.13.x"), None);
        assert_eq!(p.target("/14.14.1.7."), None);
        assert_eq!(p.target("/14.14.1.7.2.1"), None);
        assert_eq!(p.target("/Page1/Fader201"), None);
    }

    #[test]
    fn selection() {
        let ps = vec![profile("2.4", &["2.4"]), profile("2.5", &["2.5"])];
        assert_eq!(select(&ps, "auto", Some("2.5.1.1")).0.id, "2.5");
        assert_eq!(select(&ps, "auto", Some("2.5.1.1")).1, Fit::Exact);
        assert_eq!(select(&ps, "auto", Some("2.4.0.2")).0.id, "2.4");
        assert_eq!(
            select(&ps, "auto", Some("2.6.0.0")),
            (&ps[1], Fit::Fallback)
        );
        assert_eq!(
            select(&ps, "auto", Some("2.3.0.0")),
            (&ps[0], Fit::Fallback)
        );
        assert_eq!(select(&ps, "auto", Some("2.50.0")), (&ps[1], Fit::Fallback));
        assert_eq!(select(&ps, "2.4", Some("2.5.1.1")).0.id, "2.4");
        assert_eq!(select(&ps, "auto", None).0.id, "2.5");
    }

    #[test]
    fn input_addresses() {
        let i = InputAddresses::default();
        assert_eq!(InputAddresses::address(&i.key, "", 1, 201), "/Page1/Key201");
        assert_eq!(
            InputAddresses::address(&i.fader, "gma3", 2, 301),
            "/gma3/Page2/Fader301"
        );
    }

    #[test]
    fn shipped_profiles_load() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("ma-profiles");
        let ps = load_all(&dir).unwrap();
        assert!(ps.iter().any(|p| p.id == "2.5"));
    }
}
