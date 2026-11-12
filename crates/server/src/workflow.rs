//! Workflow rules: what it takes to move an issue into a status, and resolutions.
//!
//! Each status may carry rules (`statuses.rules`):
//! `{"who": "everyone" | "admins" | "teams", "teams": [team id], "require": ["assignee", …],
//!   "comment": "off" | "optional" | "required", "resolution": "Done"}`.
//! They apply to people moving issues (automation and imports aren't held up by them). Project
//! admins may always move. Which status may follow which stays the job of the restricted
//! workflow's transitions.
//!
//! A done issue has a resolution from the project's list (`settings.resolutions`, by default Done,
//! Won't do, Duplicate, Cannot reproduce): entering a done status sets one (the one chosen, else
//! the status's default, else the first), leaving done clears it.

use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::access::Role;
use crate::error::{ApiError, ApiResult};

/// Resolutions of projects that didn't set their own.
pub const DEFAULT_RESOLUTIONS: [&str; 4] = ["Done", "Won't do", "Duplicate", "Cannot reproduce"];

/// Fields a status can require (custom fields as `custom:<id>`).
pub const REQUIRABLE: [&str; 10] = ["assignee", "resolution", "version", "affects", "component", "labels", "dueDate", "startDate", "estimate", "storyPoints"];

/// Fields an issue type can hide; all but priority, security and sprint can also be required.
pub const TYPE_FIELDS: [&str; 12] =
    ["assignee", "priority", "labels", "sprint", "version", "affects", "component", "storyPoints", "estimate", "startDate", "dueDate", "security"];

/// Checks and tidies an issue type's `fields`: `{"hidden": [field], "required": [field]}`.
pub fn check_type_fields(v: &Value) -> ApiResult<Value> {
    if v.is_null() {
        return Ok(json!({}));
    }
    let o = v.as_object().ok_or_else(|| ApiError::bad("fields must be an object"))?;
    let custom = |f: &str| f.strip_prefix("custom:").is_some_and(|id| id.parse::<Uuid>().is_ok());
    let mut out = serde_json::Map::new();
    for (k, val) in o {
        if !matches!(k.as_str(), "hidden" | "required") {
            return Err(ApiError::bad(format!("unknown fields setting {k}: hidden or required")));
        }
        let list = val.as_array().ok_or_else(|| ApiError::bad(format!("{k} must be a list of fields")))?;
        let mut fields: Vec<String> = Vec::new();
        for f in list {
            let f = f.as_str().unwrap_or("");
            let ok = match k.as_str() {
                "hidden" => TYPE_FIELDS.contains(&f) || custom(f),
                _ => (TYPE_FIELDS.contains(&f) && !matches!(f, "priority" | "security" | "sprint")) || custom(f),
            };
            if !ok {
                return Err(ApiError::bad(format!("an issue type can't make {f} {k}")));
            }
            if !fields.iter().any(|x| x == f) {
                fields.push(f.to_string());
            }
        }
        out.insert(k.clone(), json!(fields));
    }
    let hidden: Vec<&str> = out.get("hidden").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).collect();
    if let Some(both) = out.get("required").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).find(|f| hidden.contains(f)) {
        return Err(ApiError::bad(format!("{both} can't be hidden and required")));
    }
    Ok(Value::Object(out))
}

/// The requirement key of a recorded change (`custom:<id>` for custom fields).
pub fn requirement_of(field: &str, new: &Value) -> Option<String> {
    Some(match field {
        "assignee" => "assignee".into(),
        "versions" => "version".into(),
        "affects" => "affects".into(),
        "components" => "component".into(),
        "labels" => "labels".into(),
        "points" => "storyPoints".into(),
        "estimate" => "estimate".into(),
        "start" => "startDate".into(),
        "due" => "dueDate".into(),
        "custom" => format!("custom:{}", new["id"].as_str()?),
        _ => return None,
    })
}

/// Holds a person to what an issue's type requires: everything (`changed` None, as when
/// creating it or changing its type), or the fields just changed (they can't be emptied).
pub async fn enforce_type(tx: &mut Transaction<'_, Postgres>, issue: Uuid, changed: Option<&[String]>) -> ApiResult<()> {
    let (name, fields): (String, Value) = sqlx::query_as("SELECT it.name, it.fields FROM issues i JOIN issue_types it ON it.id = i.type_id WHERE i.id = $1")
        .bind(issue)
        .fetch_one(&mut **tx)
        .await?;
    let required: Vec<&str> =
        fields["required"].as_array().into_iter().flatten().filter_map(Value::as_str).filter(|f| changed.is_none_or(|c| c.iter().any(|x| x == f))).collect();
    if required.is_empty() {
        return Ok(());
    }
    let missing = missing(tx, issue, &required).await?;
    if !missing.is_empty() {
        return Err(ApiError::BadRequest(format!("a {name} needs {}", missing.join(", "))));
    }
    Ok(())
}

/// Which of `required` an issue leaves empty, by name.
async fn missing(tx: &mut Transaction<'_, Postgres>, issue: Uuid, required: &[&str]) -> ApiResult<Vec<String>> {
    #[derive(sqlx::FromRow)]
    struct Now {
        assignee_id: Option<Uuid>,
        resolution: Option<String>,
        has_version: bool,
        has_affects: bool,
        has_component: bool,
        has_labels: bool,
        due_date: Option<chrono::NaiveDate>,
        start_date: Option<chrono::NaiveDate>,
        estimate_minutes: Option<i32>,
        story_points: Option<f64>,
        custom: Value,
    }
    let now: Now = sqlx::query_as(
        "SELECT assignee_id, resolution,
                EXISTS (SELECT 1 FROM issue_versions v WHERE v.issue_id = i.id AND v.kind = 'fix') AS has_version,
                EXISTS (SELECT 1 FROM issue_versions v WHERE v.issue_id = i.id AND v.kind = 'affects') AS has_affects,
                EXISTS (SELECT 1 FROM issue_components c WHERE c.issue_id = i.id) AS has_component,
                EXISTS (SELECT 1 FROM issue_labels l WHERE l.issue_id = i.id) AS has_labels,
                due_date, start_date, estimate_minutes, story_points, custom FROM issues i WHERE id = $1",
    )
    .bind(issue)
    .fetch_one(&mut **tx)
    .await?;
    let mut out = Vec::new();
    for f in required {
        let set = match *f {
            "assignee" => now.assignee_id.is_some(),
            "resolution" => now.resolution.is_some(),
            "version" => now.has_version,
            "affects" => now.has_affects,
            "component" => now.has_component,
            "labels" => now.has_labels,
            "dueDate" => now.due_date.is_some(),
            "startDate" => now.start_date.is_some(),
            "estimate" => now.estimate_minutes.is_some(),
            "storyPoints" => now.story_points.is_some(),
            other => other.strip_prefix("custom:").is_none_or(|id| {
                let v = &now.custom[id];
                !(v.is_null() || v.as_str() == Some("") || v.as_array().is_some_and(|a| a.is_empty()))
            }),
        };
        if !set {
            out.push(field_name(tx, f).await?);
        }
    }
    Ok(out)
}

/// The project's resolutions.
pub fn resolutions(settings: &Value) -> Vec<String> {
    match settings["resolutions"].as_array() {
        Some(a) if !a.is_empty() => a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(),
        _ => DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect(),
    }
}

/// Checks and tidies a project's list of resolutions (1 to 20 distinct names).
pub fn check_resolutions(v: &Value) -> ApiResult<Value> {
    let list = v.as_array().ok_or_else(|| ApiError::bad("resolutions must be a list of names"))?;
    let mut out: Vec<String> = Vec::new();
    for r in list {
        let name = r.as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| ApiError::bad("a resolution needs a name"))?;
        if name.chars().count() > 60 {
            return Err(ApiError::bad("resolution names are at most 60 characters"));
        }
        if out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            return Err(ApiError::bad(format!("the resolution {name} is there twice")));
        }
        out.push(name.to_string());
    }
    if out.is_empty() || out.len() > 20 {
        return Err(ApiError::bad("a project has 1 to 20 resolutions"));
    }
    Ok(json!(out))
}

/// Checks and tidies a status's rules.
pub fn check_rules(v: &Value) -> ApiResult<Value> {
    if v.is_null() {
        return Ok(json!({}));
    }
    let o = v.as_object().ok_or_else(|| ApiError::bad("rules must be an object"))?;
    let mut out = serde_json::Map::new();
    for (k, val) in o {
        match k.as_str() {
            "who" => {
                let w = val
                    .as_str()
                    .filter(|w| matches!(*w, "everyone" | "admins" | "teams"))
                    .ok_or_else(|| ApiError::bad("who must be everyone, admins or teams"))?;
                out.insert(k.clone(), json!(w));
            }
            "teams" => {
                let ids: Vec<Uuid> = val
                    .as_array()
                    .ok_or_else(|| ApiError::bad("teams must be a list of team ids"))?
                    .iter()
                    .map(|t| t.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("teams must be a list of team ids")))
                    .collect::<ApiResult<_>>()?;
                out.insert(k.clone(), json!(ids));
            }
            "require" => {
                let mut fields: Vec<String> = Vec::new();
                for f in val.as_array().ok_or_else(|| ApiError::bad("require must be a list of fields"))? {
                    let f = f.as_str().unwrap_or("");
                    let ok = REQUIRABLE.contains(&f) || f.strip_prefix("custom:").is_some_and(|id| id.parse::<Uuid>().is_ok());
                    if !ok {
                        return Err(ApiError::bad(format!("a status can't require {f}")));
                    }
                    if !fields.iter().any(|x| x == f) {
                        fields.push(f.to_string());
                    }
                }
                out.insert(k.clone(), json!(fields));
            }
            "comment" => {
                let c = val
                    .as_str()
                    .filter(|c| matches!(*c, "off" | "optional" | "required"))
                    .ok_or_else(|| ApiError::bad("comment must be off, optional or required"))?;
                out.insert(k.clone(), json!(c));
            }
            "resolution" => {
                if !val.is_null() {
                    let r = val.as_str().map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| ApiError::bad("resolution must be a name"))?;
                    out.insert(k.clone(), json!(r));
                }
            }
            other => return Err(ApiError::bad(format!("unknown rule {other}"))),
        }
    }
    if out.get("who").and_then(Value::as_str) == Some("teams") && out.get("teams").and_then(Value::as_array).is_none_or(|t| t.is_empty()) {
        return Err(ApiError::bad("choose the teams that may move issues here"));
    }
    Ok(Value::Object(out))
}

/// Holds a person to the rules of the status an issue just moved into: who may, the fields it
/// needs (on the issue as it is after the change) and a comment. `comment` says whether one
/// comes with the move.
pub async fn enforce(tx: &mut Transaction<'_, Postgres>, user: Uuid, issue: Uuid, status: Uuid, comment: bool) -> ApiResult<()> {
    let (name, rules, project): (String, Value, Uuid) =
        sqlx::query_as("SELECT name, rules, project_id FROM statuses WHERE id = $1").bind(status).fetch_one(&mut **tx).await?;
    if rules.as_object().is_none_or(|o| o.is_empty()) {
        return Ok(());
    }
    let rank: Option<i32> =
        sqlx::query_scalar("SELECT rank FROM tc_project_roles($1) WHERE project_id = $2").bind(user).bind(project).fetch_optional(&mut **tx).await?;
    let admin = Role::from_rank(rank.unwrap_or(0)) >= Role::Admin;
    match rules["who"].as_str() {
        Some("admins") if !admin => return Err(ApiError::Denied(format!("only project admins may move issues to {name}"))),
        Some("teams") if !admin => {
            let teams: Vec<Uuid> = rules["teams"].as_array().into_iter().flatten().filter_map(|t| t.as_str()?.parse().ok()).collect();
            let member: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM team_members WHERE user_id = $1 AND team_id = ANY($2))")
                .bind(user)
                .bind(&teams)
                .fetch_one(&mut **tx)
                .await?;
            if !member {
                let names: Vec<String> =
                    sqlx::query_scalar("SELECT name FROM teams WHERE id = ANY($1) ORDER BY lower(name)").bind(&teams).fetch_all(&mut **tx).await?;
                return Err(ApiError::Denied(format!("only {} may move issues to {name}", names.join(", "))));
            }
        }
        _ => {}
    }
    let required: Vec<&str> = rules["require"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut missing = if required.is_empty() { Vec::new() } else { missing(tx, issue, &required).await? };
    if rules["comment"].as_str() == Some("required") && !comment {
        missing.push("a comment".into());
    }
    if !missing.is_empty() {
        return Err(ApiError::BadRequest(format!("moving to {name} needs {}", missing.join(", "))));
    }
    Ok(())
}

async fn field_name(tx: &mut Transaction<'_, Postgres>, f: &str) -> ApiResult<String> {
    Ok(match f {
        "assignee" => "an assignee".into(),
        "resolution" => "a resolution".into(),
        "version" => "a release".into(),
        "affects" => "a release it was found in".into(),
        "component" => "a component".into(),
        "labels" => "labels".into(),
        "dueDate" => "a due date".into(),
        "startDate" => "a start date".into(),
        "estimate" => "an estimate".into(),
        "storyPoints" => "story points".into(),
        other => match other.strip_prefix("custom:").and_then(|id| id.parse::<Uuid>().ok()) {
            Some(id) => sqlx::query_scalar::<_, String>("SELECT name FROM custom_fields WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut **tx)
                .await?
                .unwrap_or_else(|| other.into()),
            None => other.into(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_checked_and_tidied() {
        let r = check_rules(&json!({"who": "admins", "require": ["assignee", "assignee", "resolution"], "comment": "required"})).unwrap();
        assert_eq!(r, json!({"who": "admins", "require": ["assignee", "resolution"], "comment": "required"}));
        assert!(check_rules(&json!({"who": "teams"})).is_err(), "teams need team ids");
        assert!(check_rules(&json!({"require": ["shoe size"]})).is_err());
        assert!(check_rules(&json!({"require": [format!("custom:{}", Uuid::nil())]})).is_ok());
        assert!(check_rules(&json!({"bogus": 1})).is_err());
        assert_eq!(check_rules(&Value::Null).unwrap(), json!({}));
    }

    #[test]
    fn resolutions_have_defaults_and_rules() {
        assert_eq!(resolutions(&json!({})), DEFAULT_RESOLUTIONS.to_vec());
        assert_eq!(resolutions(&json!({"resolutions": ["Fixed", "Rejected"]})), vec!["Fixed", "Rejected"]);
        assert!(check_resolutions(&json!(["Fixed", "fixed"])).is_err(), "no duplicates");
        assert!(check_resolutions(&json!([])).is_err());
        assert_eq!(check_resolutions(&json!([" Fixed "])).unwrap(), json!(["Fixed"]));
    }
}
