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
