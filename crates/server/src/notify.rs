//! In-app notifications: assignments, mentions, comments and status changes on issues people
//! watch. The actor never notifies themselves; people only hear about issues they can see.

use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, new_id};
use crate::events::{self, Event};
use crate::issues::{Actor, Change, Summary};

async fn send(state: &AppState, workspace: Uuid, user: Uuid, s: &Summary, actor: Option<Uuid>, kind: &str, data: Value) {
    if Some(user) == actor || !crate::access::can_view(state, user, s.project_id).await {
        return;
    }
    let muted: bool = sqlx::query_scalar("SELECT COALESCE((preferences->'muted') ? $2, false) FROM users WHERE id = $1")
        .bind(user)
        .bind(kind)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    if muted {
        return;
    }
    let mut data = data;
    data["key"] = json!(s.key);
    data["title"] = json!(s.title);
    let ok = sqlx::query("INSERT INTO notifications (id, user_id, issue_id, actor_id, kind, data) VALUES ($1, $2, $3, $4, $5, $6)")
        .bind(new_id())
        .bind(user)
        .bind(s.id)
        .bind(actor)
        .bind(kind)
        .bind(data)
        .execute(&state.db)
        .await
        .is_ok();
    if ok {
        let mut e = Event::new(workspace, "notification");
        e.user_id = Some(user);
        events::publish(state, e).await;
    }
}

/// A notification about an issue (automation and service-level alerts use it).
pub async fn notify(state: &AppState, workspace: Uuid, user: Uuid, s: &Summary, actor: Option<Uuid>, kind: &str, data: Value) {
    send(state, workspace, user, s, actor, kind, data).await
}

pub async fn watchers(state: &AppState, issue: Uuid) -> Vec<Uuid> {
    sqlx::query_scalar("SELECT user_id FROM issue_watchers WHERE issue_id = $1").bind(issue).fetch_all(&state.db).await.unwrap_or_default()
}

/// After fields changed.
pub async fn changes(state: &AppState, actor: &Actor, s: &Summary, changes: &[Change], _created: bool) {
    for c in changes {
        match c.field {
            "assignee" => {
                if let Some(u) = c.new["id"].as_str().and_then(|s| s.parse().ok()) {
                    send(state, actor.workspace, u, s, actor.user, "assigned", json!({})).await;
                }
            }
            "status" => {
                for w in watchers(state, s.id).await {
                    send(
                        state,
                        actor.workspace,
                        w,
                        s,
                        actor.user,
                        "status",
                        json!({"from": c.old["name"], "to": c.new["name"], "category": c.new["category"]}),
                    )
                    .await;
                }
            }
            _ => {}
        }
    }
}

/// People mentioned in a description or comment.
pub async fn mentions(state: &AppState, actor: &Actor, s: &Summary, body: &Value, comment: Option<Uuid>) -> Vec<Uuid> {
    let people = crate::doc::mentioned_people(body);
    for u in &people {
        send(state, actor.workspace, *u, s, actor.user, "mentioned", json!({"commentId": comment})).await;
    }
    people
}

/// A new comment: mentioned people and watchers.
pub async fn comment(state: &AppState, actor: &Actor, s: &Summary, comment: Uuid, body: &Value) {
    let mentioned = mentions(state, actor, s, body, Some(comment)).await;
    let excerpt: String = crate::doc::text(body).chars().take(140).collect();
    for w in watchers(state, s.id).await {
        if !mentioned.contains(&w) {
            send(state, actor.workspace, w, s, actor.user, "commented", json!({"commentId": comment, "excerpt": excerpt})).await;
        }
    }
}
