//! Small web UI: one page that shows the live state, fed by server-sent events.

use crate::App;
use axum::extract::State as Ax;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
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
        .with_state(app)
}

async fn state(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    Json(app.state.lock().unwrap().clone())
}

async fn resync(Ax(app): Ax<Arc<App>>) -> impl IntoResponse {
    app.resync.notify_waiters();
    "ok"
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
