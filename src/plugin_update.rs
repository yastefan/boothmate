//! One-click plugin update (plugin 0.4.0+).
//!
//! Boothmate sends the plugin files that come with it over the plugin connection. The running
//! plugin checks them, writes them into the grandMA3 plugin library, stores a macro that deletes,
//! imports and restarts it, fires the macro and stops. When the new plugin says hello again, the
//! update is done.

use crate::state::{now_ms, State};
use crate::tempo::version_at_least;
use crate::App;
use serde::Serialize;
use serde_json::json;
use std::path::Path;

/// First plugin version that can replace itself.
pub const UPDATE_PLUGIN: &str = "0.4.0";

/// Without a new hello this long after "restarting", the web UI points to the manual way.
pub const RESTART_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PluginUpdate {
    /// The running plugin can replace itself.
    pub supported: bool,
    /// `sent`, `restarting`, `done` or `failed`.
    pub state: Option<String>,
    /// Version being installed.
    pub target: Option<String>,
    pub msg: Option<String>,
    pub updated: Option<u64>,
}

impl PluginUpdate {
    fn set(&mut self, state: &str, msg: Option<String>) {
        self.state = Some(state.into());
        self.msg = msg;
        self.updated = Some(now_ms());
    }

    fn pending(&self) -> bool {
        matches!(self.state.as_deref(), Some("sent" | "restarting"))
    }
}

/// The bundled plugin as an update message: (version, op).
pub fn op(data_dir: &Path) -> Result<(String, serde_json::Value), String> {
    let dir = data_dir.join("lua");
    let read = |f: &str| {
        std::fs::read_to_string(dir.join(f)).map_err(|e| format!("{}: {e}", dir.join(f).display()))
    };
    let xml = read("boothmate.xml")?;
    let lua = read("boothmate.lua")?;
    let version = crate::setup::xml_version(&xml)
        .ok_or_else(|| "the bundled boothmate.xml has no version".to_string())?;
    if !lua.contains(&format!("local VERSION = \"{version}\"")) {
        return Err(format!(
            "the bundled boothmate.lua is not version {version} like boothmate.xml"
        ));
    }
    let op = json!({ "op": "update", "version": version, "xml": xml, "lua": lua });
    Ok((version, op))
}

/// Asks the running plugin to replace itself with the bundled one.
pub fn request(app: &App) -> Result<String, String> {
    let (version, op) = op(&app.dir)?;
    let st = app.state.lock().unwrap();
    let c = &st.connections;
    if !c.plugin_connected {
        return Err("The plugin is not connected.".into());
    }
    let running = c.plugin_version.clone().unwrap_or_default();
    if !st.plugin_update.supported {
        return Err(format!(
            "Plugin {running} cannot update itself (needs {UPDATE_PLUGIN}): copy the new plugin and import it once by hand."
        ));
    }
    if version_at_least(&running, &version) {
        return Err(format!("grandMA3 already runs plugin {running}."));
    }
    let u = &st.plugin_update;
    if u.pending() && now_ms().saturating_sub(u.updated.unwrap_or(0)) < RESTART_TIMEOUT_MS {
        return Err("An update is already running.".into());
    }
    drop(st);
    app.update(|st| {
        st.plugin_update.target = Some(version.clone());
        st.plugin_update.set("sent", None);
    });
    crate::log::info(&format!("plugin: updating {running} to {version}"));
    let _ = app.plugin_ops.send(op.to_string());
    Ok(format!("Updating plugin {running} to {version}"))
}

/// A plugin said hello: decides whether it can update itself and finishes a pending update.
pub fn on_hello(st: &mut State, plugin: &str) {
    let u = &mut st.plugin_update;
    u.supported = version_at_least(plugin, UPDATE_PLUGIN);
    if !u.pending() {
        return;
    }
    let target = u.target.clone().unwrap_or_default();
    if version_at_least(plugin, &target) {
        u.set(
            "done",
            Some(format!(
                "grandMA3 runs plugin {plugin}. Save the show to keep it."
            )),
        );
        crate::log::info(&format!("plugin: updated to {plugin}"));
    } else {
        u.set(
            "failed",
            Some(format!("grandMA3 still runs plugin {plugin}.")),
        );
        crate::log::warn(&format!(
            "plugin: update to {target} did not happen, still {plugin}"
        ));
    }
}

/// The plugin's answer to an update request.
pub fn on_event(st: &mut State, state: &str, msg: Option<String>) {
    let u = &mut st.plugin_update;
    match state {
        "restarting" => u.set("restarting", None),
        _ => {
            let m = msg.unwrap_or_else(|| "unknown error".into());
            crate::log::warn(&format!("plugin: update failed: {m}"));
            u.set("failed", Some(m));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_plugin_makes_an_update_op() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let (version, op) = op(dir).unwrap();
        assert!(version_at_least(&version, UPDATE_PLUGIN));
        assert_eq!(op["op"], "update");
        assert_eq!(op["version"], version.as_str());
        assert!(op["lua"].as_str().unwrap().contains("selfUpdate"));
        assert!(op["xml"]
            .as_str()
            .unwrap()
            .contains("<UserPlugin Name=\"boothmate\""));
    }

    #[test]
    fn hello_finishes_or_fails_a_pending_update() {
        let mut st = State::default();
        on_hello(&mut st, "0.3.0");
        assert!(!st.plugin_update.supported);
        assert_eq!(st.plugin_update.state, None);

        on_hello(&mut st, "0.4.0");
        assert!(st.plugin_update.supported);
        st.plugin_update.target = Some("0.5.0".into());
        st.plugin_update.set("sent", None);
        on_event(&mut st, "restarting", None);
        assert_eq!(st.plugin_update.state.as_deref(), Some("restarting"));
        on_hello(&mut st, "0.5.0");
        assert_eq!(st.plugin_update.state.as_deref(), Some("done"));

        st.plugin_update.set("restarting", None);
        on_hello(&mut st, "0.4.0");
        assert_eq!(st.plugin_update.state.as_deref(), Some("failed"));

        on_event(&mut st, "failed", Some("cannot write".into()));
        assert_eq!(st.plugin_update.msg.as_deref(), Some("cannot write"));
        // A later hello does not touch a finished update.
        on_hello(&mut st, "0.4.0");
        assert_eq!(st.plugin_update.msg.as_deref(), Some("cannot write"));
    }
}
