//! Small web UI: one page that shows the live state, fed by server-sent events.

use crate::midi::device::{Kind, Relative};
use crate::midi::mapping::{self, Learn};
use crate::App;
use axum::extract::State as Ax;
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
        .route("/api/tempo", post(set_tempo))
        .route("/api/timecode", post(set_timecode))
        .route("/api/log", post(recent_log))
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
