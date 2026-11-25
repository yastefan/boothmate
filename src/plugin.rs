//! Connection to the Boothmate Lua plugin running inside grandMA3.
//!
//! The plugin listens on TCP (default 127.0.0.1:9002); Boothmate connects and reconnects.
//! Both sides send one JSON object per line. See `lua/boothmate.lua` for the message set.

use crate::profile::{self, Fit};
use crate::state::Executor;
use crate::App;
use serde::Deserialize;
use serde_json::json;
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
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => handle_line(app, &line),
                Ok(None) => return "closed by grandMA3".into(),
                Err(e) => return format!("read failed: {e}"),
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
            app.update(|st| {
                st.connections.plugin_connected = true;
                st.connections.plugin_error = None;
                st.connections.plugin_version = Some(plugin);
                st.connections.ma_version = Some(ma);
                st.connections.profile = p.id.clone();
                st.connections.profile_fallback = fit == Fit::Fallback;
            });
        }
        Event::Page { page, executors } => {
            crate::log::debug(&format!(
                "plugin: page {page:?}, {} executors",
                executors.len()
            ));
            app.update(|st| st.apply_page(page, executors));
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
    }
}
