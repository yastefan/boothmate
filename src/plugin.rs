//! Connection to the Boothmate Lua plugin running inside grandMA3.
//!
//! The plugin listens on TCP (default 127.0.0.1:9002); Boothmate connects and reconnects.
//! Both sides send one JSON object per line. See `lua/boothmate.lua` for the message set.

use crate::profile::{self, Fit};
use crate::state::Executor;
use crate::App;
use serde::Deserialize;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "ev", rename_all = "lowercase")]
pub enum Event {
    Hello {
        plugin: String,
        ma: String,
    },
    Page {
        page: Option<u32>,
        #[serde(default)]
        executors: Vec<Executor>,
    },
    Error {
        msg: String,
    },
    Setup(crate::setup::Report),
    Update {
        state: String,
        #[serde(default)]
        msg: Option<String>,
    },
}

/// Asks the plugin for the OSC and Art-Net setup (plugin 0.3.0+), or to fix it.
pub fn request_setup(app: &App, apply: bool) -> Result<(), String> {
    let st = app.state.lock().unwrap();
    if !st.connections.plugin_connected {
        return Err("The plugin is not connected.".into());
    }
    if !st.ma3_setup.supported {
        return Err(format!(
            "Needs plugin {} or later: copy the new plugin in the grandMA3 tab and import it again.",
            crate::setup::SETUP_PLUGIN
        ));
    }
    let n = crate::setup::needs(&app.config, st.timecode.on);
    drop(st);
    let op = if apply {
        crate::setup::apply_op(&n)
    } else {
        json!({ "op": "setup" })
    };
    let _ = app.plugin_ops.send(op.to_string());
    Ok(())
}

/// Time grandMA3 gets to apply an OSC key before the plugin reads the page again.
pub const KEY_REFRESH: Duration = Duration::from_millis(120);
/// Fader and encoder moves come in bursts: read the page once they stop for this long.
pub const FADER_REFRESH: Duration = Duration::from_millis(300);

/// Bumped by every request; a pending resync only goes out if no newer request came in.
static REFRESH_GEN: AtomicU64 = AtomicU64::new(0);

/// Asks the plugin for a fresh page snapshot after `delay`. grandMA3 does not send OSC output
/// for changes caused by OSC input (a key toggling a sequence, a fader pulled to 0 switching it
/// off), so this is how input from Boothmate gets its feedback. A newer request replaces a
/// pending one, so a fader move sends one resync when it stops.
pub fn refresh_after(app: &App, delay: Duration) {
    request_resync(app.plugin_ops.clone(), delay);
}

/// Asks for a page snapshot after [`KEY_REFRESH`], independent of the debounced requests: a
/// fader crossing 0 should show the new running state while the fader is still moving.
pub fn refresh_soon(app: &App) {
    let ops = app.plugin_ops.clone();
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    rt.spawn(async move {
        tokio::time::sleep(KEY_REFRESH).await;
        let _ = ops.send(json!({ "op": "resync" }).to_string());
    });
}

fn request_resync(ops: tokio::sync::broadcast::Sender<String>, delay: Duration) {
    let Ok(rt) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let gen = REFRESH_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    rt.spawn(async move {
        tokio::time::sleep(delay).await;
        if REFRESH_GEN.load(Ordering::SeqCst) == gen {
            let _ = ops.send(json!({ "op": "resync" }).to_string());
        }
    });
}

pub async fn run(app: Arc<App>) {
    let addr = format!("{}:{}", app.config.plugin.host, app.config.plugin.port);
    let mut logged_wait = false;
    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                logged_wait = false;
                crate::log::info(&format!("plugin: connected to {addr}"));
                let reason = session(&app, stream).await;
                crate::log::info(&format!("plugin: disconnected ({reason})"));
                app.update(|st| {
                    st.connections.plugin_connected = false;
                    st.connections.plugin_error = Some(reason);
                });
            }
            Err(e) => {
                if !logged_wait {
                    crate::log::info(&format!(
                        "plugin: waiting for grandMA3 plugin on {addr} ({e}); start it with  Plugin \"boothmate\""
                    ));
                    logged_wait = true;
                }
                app.update(|st| st.connections.plugin_error = Some(format!("not reachable: {e}")));
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn session(app: &Arc<App>, stream: TcpStream) -> String {
    let _ = stream.set_nodelay(true);
    let (read, mut write) = stream.into_split();
    let hello = json!({ "op": "hello", "token": app.config.plugin.token, "client": format!("boothmate {}", env!("CARGO_PKG_VERSION")) });
    if let Err(e) = write.write_all(format!("{hello}\n").as_bytes()).await {
        return format!("write failed: {e}");
    }
    let mut lines = BufReader::new(read).lines();
    let mut ops = app.plugin_ops.subscribe();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => handle_line(app, &line),
                Ok(None) => return "closed by grandMA3".into(),
                Err(e) => return format!("read failed: {e}"),
            },
            op = ops.recv() => match op {
                Ok(op) => {
                    if let Err(e) = write.write_all(format!("{op}\n").as_bytes()).await {
                        return format!("write failed: {e}");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return "stopped".into(),
            },
            _ = app.resync.notified() => {
                if let Err(e) = write.write_all(b"{\"op\":\"resync\"}\n").await {
                    return format!("write failed: {e}");
                }
            }
        }
    }
}

fn handle_line(app: &Arc<App>, line: &str) {
    let ev: Event = match serde_json::from_str(line) {
        Ok(ev) => ev,
        Err(e) => {
            crate::log::debug(&format!("plugin: ignoring line ({e}): {line}"));
            return;
        }
    };
    match ev {
        Event::Hello { plugin, ma } => {
            let (p, fit) = profile::select(&app.profiles, &app.config.ma.profile, Some(&ma));
            crate::log::info(&format!(
                "plugin: grandMA3 {ma}, plugin {plugin}, profile {}",
                p.id
            ));
            if fit == Fit::Fallback {
                crate::log::warn(&format!(
                    "grandMA3 {ma} has no verified profile; using {} read-only. Add ma-profiles/<version>.toml.",
                    p.id
                ));
            }
            *app.profile.write().unwrap() = p.clone();
            let supported = crate::tempo::version_at_least(&plugin, crate::setup::SETUP_PLUGIN);
            app.update(|st| {
                crate::plugin_update::on_hello(st, &plugin);
                st.ma3_setup = crate::setup::Ma3Setup {
                    supported,
                    ..Default::default()
                };
                st.connections.plugin_connected = true;
                st.connections.plugin_error = None;
                st.connections.plugin_version = Some(plugin);
                st.connections.ma_version = Some(ma);
                st.connections.profile = p.id.clone();
                st.connections.profile_fallback = fit == Fit::Fallback;
            });
            if supported {
                let _ = app.plugin_ops.send(json!({ "op": "setup" }).to_string());
            }
        }
        Event::Page { page, executors } => {
            crate::log::debug(&format!(
                "plugin: page {page:?}, {} executors",
                executors.len()
            ));
            app.update(|st| st.apply_page(page, executors));
        }
        Event::Setup(r) => {
            let timecode_on = app.state.lock().unwrap().timecode.on;
            let checks = crate::setup::checks(&r, &crate::setup::needs(&app.config, timecode_on));
            app.update(|st| {
                let s = &mut st.ma3_setup;
                if !r.changes.is_empty() {
                    s.changes = r.changes.clone();
                }
                s.checks = checks;
                s.report = Some(r);
                s.updated = Some(crate::state::now_ms());
            });
        }
        Event::Update { state, msg } => {
            app.update(|st| crate::plugin_update::on_event(st, &state, msg));
        }
        Event::Error { msg } => {
            crate::log::warn(&format!("plugin: {msg}"));
            app.update(|st| st.connections.plugin_error = Some(msg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plugin_messages() {
        let hello: Event =
            serde_json::from_str(r#"{"ev":"hello","plugin":"0.1.0","ma":"2.5.1.1"}"#).unwrap();
        assert_eq!(
            hello,
            Event::Hello {
                plugin: "0.1.0".into(),
                ma: "2.5.1.1".into()
            }
        );

        let page: Event = serde_json::from_str(
            r##"{"ev":"page","page":1,"executors":[{"no":201,"class":"Sequence","obj":2,"name":"MCP Test",
                "addr":"14.14.1.7.2","color":"#ff0000","active":true,"fader":100,"cue":"2 Rot"},
                {"no":202,"class":"Group","obj":1,"name":"Spots"}]}"##,
        )
        .unwrap();
        let Event::Page { page, executors } = page else {
            panic!()
        };
        assert_eq!(page, Some(1));
        assert_eq!(executors.len(), 2);
        assert_eq!(executors[0].fader, Some(100.0));
        assert_eq!(executors[1].color, None);

        // The plugin leaves out an empty list (Lua's json cannot tell it from an object).
        let empty: Event = serde_json::from_str(r#"{"ev":"page","page":2}"#).unwrap();
        assert_eq!(
            empty,
            Event::Page {
                page: Some(2),
                executors: vec![]
            }
        );

        let update: Event =
            serde_json::from_str(r#"{"ev":"update","state":"failed","version":"0.4.0","msg":"x"}"#)
                .unwrap();
        assert_eq!(
            update,
            Event::Update {
                state: "failed".into(),
                msg: Some("x".into())
            }
        );
    }

    #[tokio::test]
    async fn resync_is_requested_once_input_stops() {
        let (tx, mut rx) = tokio::sync::broadcast::channel::<String>(8);
        // A burst of fader moves: only the last request sends.
        for _ in 0..5 {
            request_resync(tx.clone(), Duration::from_millis(50));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            rx.try_recv().is_err(),
            "not at once: grandMA3 applies the input first"
        );
        let op = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(op, r#"{"op":"resync"}"#);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(rx.try_recv().is_err(), "one resync per burst");
    }
}
