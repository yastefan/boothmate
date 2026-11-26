//! Small web UI: one page that shows the live state, fed by server-sent events.

use crate::midi::device::{Kind, Relative};
use crate::midi::mapping::{self, Learn};
use crate::App;
use axum::extract::{Path, State as Ax};
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use tokio_stream::wrappers::WatchStream;
use tokio_stream::{Stream, StreamExt};

const INDEX: &str = include_str!("web/index.html");

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(INDEX) }))
        .route("/api/state", get(state))
        .route("/api/events", get(events))
        .route("/api/resync", post(resync))
        .route("/api/midi/learn", post(learn))
        .route("/api/midi/assign", post(assign))
        .route("/api/midi/mapping", post(edit_mapping))
        .route("/api/midi/test", post(test_midi))
        .route("/api/cdj/simulate", post(simulate_cdj))
        .route("/api/cdj/record", post(record_cdj))
        .route("/api/cdj/track/{key}", get(cdj_track))
        .route("/api/tempo", post(set_tempo))
        .route("/api/timecode", post(set_timecode))
        .route("/api/log", post(recent_log))
        .route("/api/modules", post(set_modules))
        .route("/api/plugin", get(plugin_info))
        .route("/api/plugin/install", post(install_plugin))
        .route("/api/plugin/update", post(update_plugin))
        .route("/api/ma3/setup", post(ma3_setup))
        .with_state(app)
}

async fn state(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    Json(app.state.lock().unwrap().clone())
}

async fn resync(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    app.resync.notify_waiters();
    "ok"
}

#[derive(Deserialize)]
struct LearnReq {
    /// false stops learning.
    on: bool,
    #[serde(default = "yes")]
    follow: bool,
}

fn yes() -> bool {
    true
}

async fn learn(Ax(app): Ax<Arc<App>>, Json(r): Json<LearnReq>) -> impl IntoResponse {
    app.update(|st| {
        st.midi.learn = r.on.then(|| Learn {
            follow: r.follow,
            ..Default::default()
        })
    });
    "ok"
}

#[derive(Deserialize)]
struct AssignReq {
    exec: u32,
}

async fn assign(Ax(app): Ax<Arc<App>>, Json(r): Json<AssignReq>) -> impl IntoResponse {
    let mut result = Ok(());
    app.update(|st| result = mapping::assign(st, r.exec));
    match result {
        Ok(()) => (StatusCode::OK, "ok".to_string()),
        Err(e) => (StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
struct MappingReq {
    index: usize,
    #[serde(default)]
    delete: bool,
    /// true: follow the current page.
    follow: Option<bool>,
    /// A fixed page.
    page: Option<u32>,
    kind: Option<Kind>,
    relative: Option<Relative>,
}

async fn edit_mapping(Ax(app): Ax<Arc<App>>, Json(r): Json<MappingReq>) -> impl IntoResponse {
    let mut found = false;
    app.update(|st| {
        if r.index >= st.midi.mappings.len() {
            return;
        }
        found = true;
        if r.delete {
            st.midi.mappings.remove(r.index);
        } else {
            let m = &mut st.midi.mappings[r.index];
            if r.follow == Some(true) {
                m.page = None;
            }
            if let Some(p) = r.page {
                m.page = Some(p);
            }
            if let Some(k) = r.kind {
                m.kind = k;
            }
            if let Some(rel) = r.relative {
                m.relative = rel;
            }
        }
        st.midi.rev += 1;
    });
    if found {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::NOT_FOUND, "no such mapping")
    }
}

#[derive(Deserialize)]
struct TestReq {
    bytes: Vec<u8>,
}

/// A message from the test controller in the web UI.
async fn test_midi(Ax(app): Ax<Arc<App>>, Json(r): Json<TestReq>) -> impl IntoResponse {
    let _ = app.midi_test.send(r.bytes);
    "ok"
}

#[derive(Deserialize)]
struct SimReq {
    on: bool,
}

/// Switches the DJ Link simulator (two fake players and a mixer) on or off.
async fn simulate_cdj(Ax(app): Ax<Arc<App>>, Json(r): Json<SimReq>) -> impl IntoResponse {
    app.update(|st| st.cdj.simulator = r.on);
    "ok"
}

/// Starts or stops recording DJ Link packets to logs/.
async fn record_cdj(Ax(app): Ax<Arc<App>>, Json(r): Json<SimReq>) -> impl IntoResponse {
    app.update(|st| st.cdj.record = r.on);
    "ok"
}

/// The bundled plugin version and the grandMA3 libraries it can be copied to.
/// Track data (waveform, beat grid, cues, phrases) by the key in the player's `meta`.
async fn cdj_track(Ax(app): Ax<Arc<App>>, Path(key): Path<String>) -> impl IntoResponse {
    match app.tracks.get(&key) {
        Some(t) => (
            [(axum::http::header::CACHE_CONTROL, "max-age=3600")],
            Json(serde_json::to_value(&*t).unwrap_or_default()),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "no such track").into_response(),
    }
}

async fn plugin_info(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    Json(crate::setup::plugin_info(&app.dir))
}

#[derive(Deserialize)]
struct SetupReq {
    #[serde(default)]
    apply: bool,
}

/// Reads the OSC and Art-Net setup in grandMA3 again, or fixes it (through the plugin).
async fn ma3_setup(Ax(app): Ax<Arc<App>>, Json(r): Json<SetupReq>) -> impl IntoResponse {
    match crate::plugin::request_setup(&app, r.apply) {
        Ok(()) => (StatusCode::OK, "ok".to_string()),
        Err(e) => (StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
struct InstallReq {
    library: String,
}

/// Copies the plugin into a grandMA3 library.
async fn install_plugin(Ax(app): Ax<Arc<App>>, Json(r): Json<InstallReq>) -> impl IntoResponse {
    match crate::setup::install(&app.dir, &r.library) {
        Ok(m) => (StatusCode::OK, m),
        Err(e) => (StatusCode::BAD_REQUEST, e),
    }
}

/// Asks the running plugin to replace itself with the bundled one (plugin 0.4.0+).
async fn update_plugin(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    match crate::plugin_update::request(&app) {
        Ok(m) => (StatusCode::OK, m),
        Err(e) => (StatusCode::CONFLICT, e),
    }
}

#[derive(Deserialize)]
struct ModulesReq {
    midi: Option<bool>,
    cdj: Option<bool>,
    virtual_controller: Option<bool>,
}

/// Switches MIDI, DJ Link or the virtual controller on or off and saves it to boothmate.toml.
async fn set_modules(Ax(app): Ax<Arc<App>>, Json(r): Json<ModulesReq>) -> impl IntoResponse {
    let mut saves = vec![];
    app.modules.send_modify(|m| {
        if let Some(v) = r.midi {
            m.midi = v;
            saves.push(("midi", "enabled", v));
        }
        if let Some(v) = r.cdj {
            m.cdj = v;
            saves.push(("cdj", "enabled", v));
        }
        if let Some(v) = r.virtual_controller {
            m.virtual_controller = v;
            saves.push(("midi", "virtual_controller", v));
        }
    });
    let m = *app.modules.borrow();
    app.update(|st| st.modules = m);
    for (section, key, v) in saves {
        if let Err(e) =
            crate::config::save_value(&app.dir, section, key, if v { "true" } else { "false" })
        {
            crate::log::warn(&format!("settings: {e}"));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Switched, but not saved: {e}"),
            );
        }
    }
    (StatusCode::OK, "ok".to_string())
}

#[derive(Deserialize)]
struct LogReq {
    #[serde(default)]
    after: u64,
}

/// Recent log lines (info and warnings) after a sequence number.
async fn recent_log(Json(r): Json<LogReq>) -> impl IntoResponse {
    Json(crate::log::recent(r.after))
}

/// Switches Art-Net timecode on or off (until restart; the default is in boothmate.toml).
async fn set_timecode(Ax(app): Ax<Arc<App>>, Json(r): Json<SimReq>) -> impl IntoResponse {
    app.update(|st| st.timecode.on = r.on);
    "ok"
}

/// Replaces the tempo rules and saves them to tempo.toml.
async fn set_tempo(
    Ax(app): Ax<Arc<App>>,
    Json(rules): Json<crate::tempo::TempoFile>,
) -> impl IntoResponse {
    if let Err(e) = crate::tempo::save(&app.dir, &rules) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot save tempo.toml: {e}"),
        );
    }
    app.update(|st| {
        st.tempo.rules = rules;
        st.tempo.sent.clear();
    });
    (StatusCode::OK, "ok".to_string())
}

/// Sends the whole state on every change, at most 30 times a second.
async fn events(Ax(app): Ax<Arc<App>>) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = WatchStream::new(app.changes.subscribe())
        .throttle(Duration::from_millis(33))
        .map(move |_| {
            let json = serde_json::to_string(&*app.state.lock().unwrap()).unwrap_or_default();
            Ok(Event::default().data(json))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}
