//! Live updates: every change is announced through Postgres `NOTIFY`, every replica listens and
//! forwards what it hears to the browsers connected to it (Server-Sent Events). Browsers then
//! refetch what changed, so boards, backlogs and open issues stay current without reloading.

use std::collections::HashSet;
use std::convert::Infallible;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::ApiResult;

const CHANNEL: &str = "taskcraft_events";

/// One change, as browsers receive it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    /// travels next to the event (`Wire.w`), never to browsers
    #[serde(skip)]
    pub workspace: Uuid,
    /// issue | comment | sprint | board | project | version | notification
    pub kind: String,
    pub project_id: Option<Uuid>,
    pub issue_id: Option<Uuid>,
    pub key: Option<String>,
    pub actor_id: Option<Uuid>,
    /// for kind notification: whose it is (others don't get it)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
}

impl Event {
    pub fn new(workspace: Uuid, kind: &str) -> Self {
        Event { workspace, kind: kind.into(), project_id: None, issue_id: None, key: None, actor_id: None, user_id: None }
    }

    pub fn project(mut self, p: Uuid) -> Self {
        self.project_id = Some(p);
        self
    }

    pub fn issue(mut self, id: Uuid, key: &str) -> Self {
        self.issue_id = Some(id);
        self.key = Some(key.to_string());
        self
    }

    pub fn actor(mut self, a: Uuid) -> Self {
        self.actor_id = Some(a);
        self
    }
}

#[derive(Serialize, Deserialize)]
struct Wire {
    w: Uuid,
    #[serde(flatten)]
    e: Event,
}

pub struct Hub {
    tx: broadcast::Sender<Event>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Hub { tx: broadcast::channel(1024).0 }
    }
}

/// Announces a change to every replica (best effort: a lost event means a browser shows the
/// change on its next refetch).
pub async fn publish(state: &AppState, e: Event) {
    let payload = match serde_json::to_string(&Wire { w: e.workspace, e: e.clone() }) {
        Ok(p) => p,
        Err(_) => return,
    };
    if sqlx::query("SELECT pg_notify($1, $2)").bind(CHANNEL).bind(&payload).execute(&state.db).await.is_err() {
        let _ = state.hub.tx.send(e);
    }
}

/// Listens for changes from all replicas (reconnects when the connection drops).
pub fn spawn_listener(state: &AppState) {
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            match sqlx::postgres::PgListener::connect_with(&state.db).await {
                Ok(mut l) => {
                    if l.listen(CHANNEL).await.is_ok() {
                        while let Ok(n) = l.recv().await {
                            match serde_json::from_str::<Wire>(n.payload()) {
                                Ok(w) => {
                                    let mut e = w.e;
                                    e.workspace = w.w;
                                    let _ = state.hub.tx.send(e);
                                }
                                Err(err) => tracing::warn!(error = %err, "live updates: unreadable event"),
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "live updates: can't listen"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

struct Conn {
    rx: broadcast::Receiver<Event>,
    state: AppState,
    user: crate::auth::User,
    visible: HashSet<Uuid>,
    refreshed: Instant,
    hello: bool,
}

/// `GET /api/events`: the caller's live updates (`text/event-stream`).
pub async fn stream(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
) -> ApiResult<([(axum::http::HeaderName, &'static str); 1], Sse<impl Stream<Item = Result<SseEvent, Infallible>>>)> {
    let visible: HashSet<Uuid> = crate::access::visible_projects(&state, &user).await?.into_iter().collect();
    let conn = Conn { rx: state.hub.tx.subscribe(), state: state.clone(), user, visible, refreshed: Instant::now(), hello: true };
    let s = futures::stream::unfold(conn, |mut c| async move {
        if c.hello {
            c.hello = false;
            return Some((Ok(SseEvent::default().event("hello").data("{}")), c));
        }
        loop {
            match c.rx.recv().await {
                Ok(e) => {
                    if e.workspace != c.user.workspace_id || e.user_id.is_some_and(|u| u != c.user.id) {
                        continue;
                    }
                    if e.kind == "project" || c.refreshed.elapsed() > Duration::from_secs(60) {
                        if let Ok(v) = crate::access::visible_projects(&c.state, &c.user).await {
                            c.visible = v.into_iter().collect();
                        }
                        c.refreshed = Instant::now();
                    }
                    if e.project_id.is_some_and(|p| !c.visible.contains(&p)) {
                        continue;
                    }
                    let data = serde_json::to_string(&e).unwrap_or_default();
                    return Some((Ok(SseEvent::default().event("change").data(data)), c));
                }
                // fell behind: tell the browser to refetch everything it shows
                Err(broadcast::error::RecvError::Lagged(_)) => return Some((Ok(SseEvent::default().event("resync").data("{}")), c)),
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    // proxies (nginx and friends) must pass events on at once instead of buffering them
    Ok(([(axum::http::HeaderName::from_static("x-accel-buffering"), "no")], Sse::new(s).keep_alive(KeepAlive::new().interval(Duration::from_secs(20)))))
}
