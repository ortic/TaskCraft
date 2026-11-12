//! Automation rules: "when <trigger>, if the issue matches <TQL>, then <actions>".
//!
//! Triggers: an issue is created, its status changes (optionally from / to a given status), it
//! is assigned, a field changes, or someone comments. Actions: set status, assign (a person,
//! the reporter, nobody), set priority, add / remove a label, move to the active sprint or the
//! backlog, comment. Rules act as "Automation" (no person); changes they make may trigger
//! other rules, up to three rounds deep.

use futures::future::BoxFuture;
use serde_json::{Map, Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

use crate::app::AppState;
use crate::issues::{Actor, Change, Summary};

#[derive(sqlx::FromRow)]
struct Rule {
    id: Uuid,
    name: String,
    trigger: Value,
    condition: String,
    actions: Value,
}

/// Whether an issue matches a rule's TQL condition.
pub async fn matches(state: &AppState, s: &Summary, workspace: Uuid, condition: &str) -> bool {
    if condition.trim().is_empty() {
        return true;
    }
    let Ok(q) = crate::query::parse(condition) else { return false };
    let Ok(ctx) = crate::query::context_with(state, workspace, Uuid::nil(), vec![s.project_id], &q).await else { return false };
    let mut qb: QueryBuilder<sqlx::Postgres> = QueryBuilder::new(
        "SELECT EXISTS (SELECT 1 FROM issues i JOIN projects p ON p.id = i.project_id JOIN statuses s ON s.id = i.status_id JOIN issue_types it ON it.id = i.type_id WHERE i.id = ",
    );
    qb.push_bind(s.id);
    qb.push(" AND ");
    if crate::query::push_where(&mut qb, &ctx, &q).is_err() {
        return false;
    }
    qb.push(")");
    qb.build_query_scalar::<bool>().fetch_one(&state.db).await.unwrap_or(false)
}

fn triggered(t: &Value, changes: &[Change], created: bool, commented: bool) -> bool {
    let find = |f: &str| changes.iter().find(|c| c.field == f);
    match t["event"].as_str().unwrap_or("") {
        "created" => created,
        "commented" => commented,
        "status" => match find("status") {
            Some(c) if !created => {
                let to_ok = t["toStatusId"].as_str().is_none_or(|id| c.new["id"].as_str() == Some(id));
                let from_ok = t["fromStatusId"].as_str().is_none_or(|id| c.old["id"].as_str() == Some(id));
                let cat_ok = t["toCategory"].as_str().is_none_or(|cat| c.new["category"].as_str() == Some(cat));
                to_ok && from_ok && cat_ok
            }
            _ => false,
        },
        "assigned" => find("assignee").is_some_and(|c| !c.new.is_null()),
        "field" => !created && t["field"].as_str().is_some_and(|f| find(f).is_some()),
        _ => false,
    }
}

/// Runs the project's rules after a change (boxed: rules change issues, which runs rules).
pub fn run<'a>(state: &'a AppState, actor: &'a Actor, s: &'a Summary, changes: &'a [Change], created: bool) -> BoxFuture<'a, ()> {
    Box::pin(run_inner(state, actor, s, changes, created, false))
}

/// Runs the project's comment rules.
pub fn run_commented<'a>(state: &'a AppState, actor: &'a Actor, s: &'a Summary) -> BoxFuture<'a, ()> {
    Box::pin(run_inner(state, actor, s, &[], false, true))
}

async fn run_inner(state: &AppState, actor: &Actor, s: &Summary, changes: &[Change], created: bool, commented: bool) {
    if actor.depth >= 3 {
        return;
    }
    let rules: Vec<Rule> =
        sqlx::query_as("SELECT id, name, trigger, condition, actions FROM automation_rules WHERE project_id = $1 AND enabled ORDER BY created_at")
            .bind(s.project_id)
            .fetch_all(&state.db)
            .await
            .unwrap_or_default();
    for rule in rules {
        if !triggered(&rule.trigger, changes, created, commented) || !matches(state, s, actor.workspace, &rule.condition).await {
            continue;
        }
        let bot = Actor { user: None, workspace: actor.workspace, depth: actor.depth + 1 };
        if let Err(e) = execute(state, &bot, s, &rule.actions).await {
            tracing::warn!(rule = %rule.name, issue = %s.key, error = %e, "automation rule failed");
            let _ = sqlx::query("UPDATE automation_rules SET last_run_at = now() WHERE id = $1").bind(rule.id).execute(&state.db).await;
            continue;
        }
        let _ = sqlx::query("UPDATE automation_rules SET runs = runs + 1, last_run_at = now() WHERE id = $1").bind(rule.id).execute(&state.db).await;
    }
}

pub async fn execute(state: &AppState, bot: &Actor, s: &Summary, actions: &Value) -> crate::error::ApiResult<()> {
    let mut patch = Map::new();
    let mut comments = Vec::new();
    let mut labels: Option<Vec<String>> = None;
    for a in actions.as_array().into_iter().flatten() {
        match a["action"].as_str().unwrap_or("") {
            "setStatus" => {
                patch.insert("statusId".into(), a["statusId"].clone());
            }
            "assign" => {
                let who = match a["to"].as_str() {
                    Some("reporter") => json!(s.reporter.as_ref().map(|r| r.id)),
                    Some("nobody") => Value::Null,
                    _ => a["userId"].clone(),
                };
                patch.insert("assigneeId".into(), who);
            }
            "setPriority" => {
                patch.insert("priority".into(), a["priority"].clone());
            }
            "addLabel" | "removeLabel" => {
                let l = crate::query::normalize_label(a["label"].as_str().unwrap_or(""));
                let list = labels.get_or_insert_with(|| s.labels.clone());
                if a["action"] == "addLabel" {
                    if !l.is_empty() && !list.contains(&l) {
                        list.push(l);
                    }
                } else {
                    list.retain(|x| *x != l);
                }
            }
            "setSprint" => {
                let sprint = match a["sprint"].as_str() {
                    Some("active") => {
                        sqlx::query_scalar::<_, Uuid>("SELECT id FROM sprints WHERE project_id = $1 AND state = 'active' ORDER BY start_at LIMIT 1")
                            .bind(s.project_id)
                            .fetch_optional(&state.db)
                            .await?
                            .map(|id| json!(id))
                            .unwrap_or(Value::Null)
                    }
                    _ => Value::Null,
                };
                patch.insert("sprintId".into(), sprint);
            }
            "comment" => comments.push(a["text"].as_str().unwrap_or("").to_string()),
            "notify" => {
                let mut to: Vec<Uuid> = match a["to"].as_str() {
                    Some("reporter") => s.reporter.iter().map(|r| r.id).collect(),
                    Some("watchers") => crate::notify::watchers(state, s.id).await,
                    Some("lead") => sqlx::query_scalar::<_, Option<Uuid>>("SELECT lead_id FROM projects WHERE id = $1")
                        .bind(s.project_id)
                        .fetch_one(&state.db)
                        .await?
                        .into_iter()
                        .collect(),
                    _ => s.assignee.iter().map(|r| r.id).collect(),
                };
                if let Some(u) = a["userId"].as_str().and_then(|u| u.parse().ok()) {
                    to = vec![u];
                }
                let message: String = a["message"].as_str().unwrap_or("").chars().take(300).collect();
                for u in to {
                    crate::notify::notify(state, bot.workspace, u, s, None, "automation", json!({"message": message})).await;
                }
            }
            _ => {}
        }
    }
    if let Some(l) = labels {
        patch.insert("labels".into(), json!(l));
    }
    if !patch.is_empty() {
        crate::issues::update(state, bot, s.id, &patch).await?;
    }
    for text in comments.into_iter().filter(|t| !t.trim().is_empty()) {
        crate::routes::comments::add(state, bot, s.id, crate::doc::from_markdown(&text)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers() {
        let to_done = Change { field: "status", old: json!({"id": "a", "category": "todo"}), new: json!({"id": "b", "category": "done"}) };
        assert!(triggered(&json!({"event": "status"}), std::slice::from_ref(&to_done), false, false));
        assert!(triggered(&json!({"event": "status", "toCategory": "done"}), std::slice::from_ref(&to_done), false, false));
        assert!(!triggered(&json!({"event": "status", "toStatusId": "c"}), std::slice::from_ref(&to_done), false, false));
        assert!(triggered(&json!({"event": "created"}), &[], true, false));
        assert!(!triggered(&json!({"event": "status"}), std::slice::from_ref(&to_done), true, false));
        assert!(triggered(&json!({"event": "commented"}), &[], false, true));
    }
}
