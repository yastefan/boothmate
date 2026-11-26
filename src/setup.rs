//! grandMA3 setup helpers: copy the Lua plugin into a grandMA3 library, so it can be imported
//! from the Plugin pool without looking for the file.
//!
//! Libraries: the computer's (onPC) and one on the drive Boothmate runs from (a show stick), each
//! `.../MALightingTechnology/gma3_library`. Plugins go to `datapools/plugins/`.

use serde::Serialize;
use std::path::{Path, PathBuf};

const PLUGIN_FILES: [&str; 2] = ["boothmate.xml", "boothmate.lua"];

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Library {
    /// `computer` or `drive`.
    pub kind: &'static str,
    pub path: String,
    /// Version of the Boothmate plugin already in this library.
    pub installed: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    /// Version of the plugin that comes with this Boothmate.
    pub bundled: Option<String>,
    pub libraries: Vec<Library>,
}

/// `Version="x"` from a plugin XML.
pub fn xml_version(xml: &str) -> Option<String> {
    // Skip `<GMA3 DataVersion="...">`: the plugin's own version is on `<UserPlugin>`.
    let tag = &xml[xml.find("<UserPlugin")?..];
    let rest = &tag[tag.find(" Version=\"")? + 10..];
    Some(rest[..rest.find('"')?].to_string())
}

fn read_version(xml: &Path) -> Option<String> {
    xml_version(&std::fs::read_to_string(xml).ok()?)
}

/// The computer's grandMA3 library, where onPC keeps it.
fn computer_library() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        std::env::var_os("ProgramData").map(PathBuf::from)
    } else {
        std::env::var_os("HOME").map(PathBuf::from)
    }?;
    Some(base.join("MALightingTechnology").join("gma3_library"))
}

/// A library on the drive Boothmate runs from: next to one of the data folder's parents.
fn drive_library(data_dir: &Path) -> Option<PathBuf> {
    data_dir.ancestors().find_map(|a| {
        let lib = a.join("MALightingTechnology").join("gma3_library");
        lib.is_dir().then_some(lib)
    })
}

pub fn plugin_info(data_dir: &Path) -> PluginInfo {
    let mut libraries = vec![];
    let mut add = |kind, lib: PathBuf| {
        if !lib.is_dir() {
            return;
        }
        let path = lib.to_string_lossy().to_string();
        if libraries.iter().any(|l: &Library| l.path == path) {
            return;
        }
        let installed = read_version(&plugin_dir(&lib).join(PLUGIN_FILES[0]));
        libraries.push(Library {
            kind,
            path,
            installed,
        });
    };
    if let Some(l) = computer_library() {
        add("computer", l);
    }
    if let Some(l) = drive_library(data_dir) {
        add("drive", l);
    }
    PluginInfo {
        bundled: read_version(&data_dir.join("lua").join(PLUGIN_FILES[0])),
        libraries,
    }
}

fn plugin_dir(lib: &Path) -> PathBuf {
    lib.join("datapools").join("plugins")
}

/// Copies the plugin into the library at `lib`, which must be one `plugin_info` lists.
pub fn install(data_dir: &Path, lib: &str) -> Result<String, String> {
    let info = plugin_info(data_dir);
    if !info.libraries.iter().any(|l| l.path == lib) {
        return Err(format!("not a grandMA3 library: {lib}"));
    }
    let dest = plugin_dir(Path::new(lib));
    std::fs::create_dir_all(&dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    for f in PLUGIN_FILES {
        let src = data_dir.join("lua").join(f);
        std::fs::copy(&src, dest.join(f)).map_err(|e| format!("{}: {e}", src.display()))?;
    }
    let msg = format!(
        "plugin {} copied to {}",
        info.bundled.as_deref().unwrap_or("?"),
        dest.display()
    );
    crate::log::info(&format!("setup: {msg}"));
    Ok(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_from_xml() {
        let xml = r#"<GMA3 DataVersion="0.8.84.0">
    <UserPlugin Name="boothmate" Author="Boothmate" Version="0.2.0">"#;
        assert_eq!(xml_version(xml).as_deref(), Some("0.2.0"));
        assert_eq!(xml_version("<x/>"), None);
    }

    #[test]
    fn install_to_drive_library() {
        let root = std::env::temp_dir().join(format!("boothmate-setup-{}", std::process::id()));
        let data = root.join("Boothmate");
        let lib = root.join("MALightingTechnology").join("gma3_library");
        std::fs::create_dir_all(data.join("lua")).unwrap();
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(
            data.join("lua/boothmate.xml"),
            r#"<GMA3 DataVersion="1.0"><UserPlugin Name="boothmate" Version="9.9.9">"#,
        )
        .unwrap();
        std::fs::write(data.join("lua/boothmate.lua"), "-- lua").unwrap();

        let info = plugin_info(&data);
        assert_eq!(info.bundled.as_deref(), Some("9.9.9"));
        let l = info.libraries.iter().find(|l| l.kind == "drive").unwrap();
        assert_eq!(l.installed, None);
        install(&data, &l.path).unwrap();
        let info = plugin_info(&data);
        let l = info.libraries.iter().find(|l| l.kind == "drive").unwrap();
        assert_eq!(l.installed.as_deref(), Some("9.9.9"));
        assert!(install(&data, "/somewhere/else").is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

// grandMA3 configuration through the plugin (0.3.0+): OSC lines and the Art-Net timecode input.

/// Plugin version that can read and change the setup.
pub const SETUP_PLUGIN: &str = "0.3.0";

#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct OscLine {
    pub i: u32,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub port: Option<u32>,
    #[serde(default)]
    pub dest: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub send: bool,
    #[serde(default)]
    pub receive: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct OscSetup {
    pub input: bool,
    pub output: bool,
    #[serde(default)]
    pub lines: Vec<OscLine>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct TcRow {
    pub i: u32,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub stream: Option<u32>,
    #[serde(default)]
    pub slot: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct ArtnetSetup {
    pub input: bool,
    #[serde(default)]
    pub timecode: Vec<TcRow>,
}

/// What the plugin reported.
#[derive(Debug, Clone, Default, PartialEq, Serialize, serde::Deserialize)]
pub struct Report {
    #[serde(default)]
    pub osc: Option<OscSetup>,
    #[serde(default)]
    pub artnet: Option<ArtnetSetup>,
    #[serde(default)]
    pub changes: Vec<String>,
}

/// What Boothmate needs in grandMA3, from its own settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Needs {
    /// grandMA3 sends OSC here (Boothmate's `osc.listen`): port and destination IP, if known.
    pub osc_out: Option<(u32, Option<String>)>,
    /// grandMA3 receives OSC on this port (Boothmate's `osc.send`).
    pub osc_in: Option<u32>,
    pub prefix: String,
    /// Art-Net timecode stream to receive (when timecode is on).
    pub timecode_stream: Option<u32>,
}

fn split_addr(a: &str) -> Option<(String, u32)> {
    let (host, port) = a.rsplit_once(':')?;
    Some((host.to_string(), port.parse().ok()?))
}

pub fn needs(cfg: &crate::config::Config, timecode_on: bool) -> Needs {
    let osc_out = split_addr(&cfg.osc.listen).map(|(host, port)| {
        // On 0.0.0.0 Boothmate listens everywhere; the console's own address is unknown here.
        let dest = (host != "0.0.0.0").then_some(host);
        (port, dest)
    });
    Needs {
        osc_out,
        osc_in: split_addr(&cfg.osc.send).map(|(_, p)| p),
        prefix: cfg.osc.prefix.clone(),
        timecode_stream: timecode_on.then_some(0),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Check {
    pub label: String,
    pub ok: bool,
    pub detail: String,
}

pub fn checks(r: &Report, n: &Needs) -> Vec<Check> {
    let mut out = vec![];
    let check = |label: &str, ok: bool, detail: String| Check {
        label: label.into(),
        ok,
        detail,
    };
    match &r.osc {
        None => out.push(check(
            "OSC",
            false,
            "The plugin cannot read the OSC settings.".into(),
        )),
        Some(o) => {
            if let Some((port, dest)) = &n.osc_out {
                let line = o.lines.iter().find(|l| l.port == Some(*port) && l.send);
                let dest_ok = |l: &OscLine| dest.is_none() || l.dest.as_deref() == dest.as_deref();
                out.push(match line {
                    Some(l) if dest_ok(l) => check(
                        "grandMA3 sends OSC to Boothmate",
                        true,
                        format!(
                            "line {} \"{}\", port {port}",
                            l.i,
                            l.name.as_deref().unwrap_or("")
                        ),
                    ),
                    Some(l) => check(
                        "grandMA3 sends OSC to Boothmate",
                        false,
                        format!(
                            "line {} sends to {}, Boothmate listens on {}",
                            l.i,
                            l.dest.as_deref().unwrap_or("?"),
                            dest.as_deref().unwrap_or("?")
                        ),
                    ),
                    None => check(
                        "grandMA3 sends OSC to Boothmate",
                        false,
                        format!("no OSC line with port {port} and Send = Yes"),
                    ),
                });
            }
            if let Some(port) = n.osc_in {
                let line = o.lines.iter().find(|l| l.port == Some(port) && l.receive);
                out.push(match line {
                    Some(l) => check(
                        "grandMA3 receives OSC from Boothmate",
                        true,
                        format!(
                            "line {} \"{}\", port {port}",
                            l.i,
                            l.name.as_deref().unwrap_or("")
                        ),
                    ),
                    None => check(
                        "grandMA3 receives OSC from Boothmate",
                        false,
                        format!("no OSC line with port {port} and Receive = Yes"),
                    ),
                });
            }
            let off: Vec<&str> = [("Enable Input", o.input), ("Enable Output", o.output)]
                .iter()
                .filter(|(_, on)| !on)
                .map(|(n, _)| *n)
                .collect();
            out.push(check(
                "OSC switched on",
                off.is_empty(),
                if off.is_empty() {
                    "Enable Input and Enable Output are on".into()
                } else {
                    format!("{} off", off.join(" and "))
                },
            ));
        }
    }
    if let Some(stream) = n.timecode_stream {
        let label = "Art-Net timecode input";
        out.push(match &r.artnet {
            None => check(
                label,
                false,
                "The plugin cannot read the Art-Net settings.".into(),
            ),
            Some(a) => {
                let row = a.timecode.iter().find(|t| {
                    t.enabled && t.mode.as_deref() == Some("Input") && t.stream == Some(stream)
                });
                match (a.input, row) {
                    (true, Some(t)) => check(
                        label,
                        true,
                        format!("stream {stream} → timecode slot {}", t.slot.unwrap_or(0)),
                    ),
                    (false, _) => check(label, false, "Art-Net input is off".into()),
                    (true, None) => check(
                        label,
                        false,
                        format!("no enabled timecode input for stream {stream}"),
                    ),
                }
            }
        });
    }
    out
}

/// The `setup_apply` message for the plugin.
pub fn apply_op(n: &Needs) -> serde_json::Value {
    let mut osc = vec![];
    if let Some((port, dest)) = &n.osc_out {
        osc.push(serde_json::json!({ "port": port, "dest": dest, "send": true, "name": "Boothmate in", "prefix": n.prefix }));
    }
    if let Some(port) = n.osc_in {
        osc.push(serde_json::json!({ "port": port, "dest": "127.0.0.1", "receive": true, "name": "Boothmate out", "prefix": n.prefix }));
    }
    let mut op = serde_json::json!({ "op": "setup_apply", "osc": osc });
    if let Some(stream) = n.timecode_stream {
        op["artnet_tc"] = serde_json::json!({ "stream": stream, "slot": 1 });
    }
    op
}

/// The setup as shown in the web UI.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Ma3Setup {
    /// The plugin can read and change the setup (0.3.0+).
    pub supported: bool,
    pub report: Option<Report>,
    pub checks: Vec<Check>,
    /// Changes of the last "set up" run.
    pub changes: Vec<String>,
    pub updated: Option<u64>,
}

#[cfg(test)]
mod setup_tests {
    use super::*;

    fn report() -> Report {
        serde_json::from_str(
            r#"{"ev":"setup","osc":{"input":true,"output":false,"lines":[
                {"i":1,"name":"OSCData 1","port":8000,"dest":"127.0.0.1","mode":"UDP","prefix":"","send":false,"receive":true},
                {"i":2,"name":"x","port":9001,"dest":"10.1.255.255","mode":"UDP","prefix":"","send":true,"receive":false}]},
                "artnet":{"input":false}}"#,
        )
        .unwrap()
    }

    #[test]
    fn checks_find_missing_parts() {
        let n = Needs {
            osc_out: Some((9001, Some("127.0.0.1".into()))),
            osc_in: Some(8000),
            prefix: String::new(),
            timecode_stream: Some(0),
        };
        let c = checks(&report(), &n);
        assert_eq!(
            c.iter().map(|c| c.ok).collect::<Vec<_>>(),
            [false, true, false, false]
        );
        assert!(c[0].detail.contains("10.1.255.255"));
        assert!(c[2].detail.contains("Enable Output"));
        let op = apply_op(&n);
        assert_eq!(op["osc"][0]["port"], 9001);
        assert_eq!(op["osc"][1]["receive"], true);
        assert_eq!(op["artnet_tc"]["stream"], 0);
    }

    #[test]
    fn plugin_versions() {
        use crate::tempo::version_at_least as v;
        assert!(v("0.3.0", SETUP_PLUGIN) && v("0.10.0", SETUP_PLUGIN) && v("1.0", SETUP_PLUGIN));
        assert!(!v("0.2.9", SETUP_PLUGIN) && !v("", SETUP_PLUGIN));
    }

    #[test]
    fn needs_from_config() {
        let cfg: crate::config::Config =
            toml::from_str("[osc]\nlisten = \"0.0.0.0:9001\"").unwrap();
        let n = needs(&cfg, false);
        assert_eq!(n.osc_out, Some((9001, None)));
        assert_eq!(n.osc_in, Some(8000));
        assert_eq!(n.timecode_stream, None);
        assert!(apply_op(&n).get("artnet_tc").is_none());
    }
}
