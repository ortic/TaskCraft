//! Link types between issues: the four built-in ones (blocks, relates, duplicates, clones) and
//! the workspace's own (*causes / is caused by*, …). A link is stored with its type's key and a
//! direction (source → target); a type reads one way from the source (`outward`) and the other
//! way from the target (`inward`). Types marked as a *dependency* make the target blocked while
//! the source is open, like blocks does.
//!
//! The built-in types need no row; a row with a built-in key only renames it.

use serde::Serialize;
use uuid::Uuid;

/// key, outward, inward, dependency
pub const BUILTIN: [(&str, &str, &str, bool); 4] = [
    ("blocks", "blocks", "is blocked by", true),
    ("relates", "relates to", "relates to", false),
    ("duplicates", "duplicates", "is duplicated by", false),
    ("clones", "clones", "is cloned by", false),
];

pub fn is_builtin(key: &str) -> bool {
    BUILTIN.iter().any(|b| b.0 == key)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LinkType {
    pub key: String,
    /// as read from the link's source ("causes")
    pub outward: String,
    /// as read from its target ("is caused by"); the same as outward for types without a direction
    pub inward: String,
    pub dependency: bool,
    pub builtin: bool,
    /// a built-in type the workspace renamed
    pub renamed: bool,
}

impl LinkType {
    pub fn label(&self, outward: bool) -> &str {
        if outward { &self.outward } else { &self.inward }
    }

    pub fn symmetric(&self) -> bool {
        self.outward.eq_ignore_ascii_case(&self.inward)
    }
}

/// The workspace's link types: the built-in ones first, then its own in their order.
pub async fn types<'e>(db: impl sqlx::PgExecutor<'e>, workspace: Uuid) -> sqlx::Result<Vec<LinkType>> {
    let rows: Vec<(String, String, String, bool)> =
        sqlx::query_as("SELECT key, outward, inward, dependency FROM link_types WHERE workspace_id = $1 ORDER BY position, created_at")
            .bind(workspace)
            .fetch_all(db)
            .await?;
    Ok(merge(rows))
}

fn merge(rows: Vec<(String, String, String, bool)>) -> Vec<LinkType> {
    let mut out: Vec<LinkType> = BUILTIN
        .iter()
        .map(|(key, outward, inward, dependency)| {
            let own = rows.iter().find(|r| r.0 == *key);
            LinkType {
                key: key.to_string(),
                outward: own.map(|r| r.1.clone()).unwrap_or_else(|| outward.to_string()),
                inward: own.map(|r| r.2.clone()).unwrap_or_else(|| inward.to_string()),
                dependency: *dependency,
                builtin: true,
                renamed: own.is_some(),
            }
        })
        .collect();
    for (key, outward, inward, dependency) in rows {
        if !is_builtin(&key) {
            out.push(LinkType { key, outward, inward, dependency, builtin: false, renamed: false });
        }
    }
    out
}

/// The type a stored kind belongs to; links of a type that's gone read as "relates to".
pub fn find<'a>(types: &'a [LinkType], key: &str) -> Option<&'a LinkType> {
    types.iter().find(|t| t.key == key)
}

/// The link as read from one of its issues.
pub fn label(types: &[LinkType], key: &str, outward: bool) -> String {
    match find(types, key) {
        Some(t) => t.label(outward).to_string(),
        None => key.to_string(),
    }
}

/// Which type and direction a name means, as people and the API write it: a type's key
/// (`causes`, read outward), its outward or inward name ("is caused by"), or one of the
/// spellings the API took before (`blockedBy`, `duplicatedBy`, `relates to`).
/// `inward` asks for the other direction of a key. Returns (key, outward).
pub fn resolve(types: &[LinkType], name: &str, inward: bool) -> Option<(String, bool)> {
    let n = name.trim();
    let legacy = match n {
        "blockedBy" => Some(("blocks", false)),
        "duplicatedBy" => Some(("duplicates", false)),
        "clonedBy" => Some(("clones", false)),
        _ => None,
    };
    if let Some((k, out)) = legacy {
        return Some((k.into(), out));
    }
    if let Some(t) = types.iter().find(|t| t.key.eq_ignore_ascii_case(n)) {
        return Some((t.key.clone(), !inward || t.symmetric()));
    }
    let lower = n.to_lowercase();
    // names as the workspace has them, then the built-in ones (renamed types keep those working)
    for t in types {
        if t.outward.to_lowercase() == lower {
            return Some((t.key.clone(), true));
        }
        if t.inward.to_lowercase() == lower {
            return Some((t.key.clone(), false));
        }
    }
    for (key, outward, inward_name, _) in BUILTIN {
        if outward == lower {
            return Some((key.into(), true));
        }
        if inward_name == lower {
            return Some((key.into(), false));
        }
    }
    None
}

/// The stored kinds a TQL value stands for, by direction: (kinds read outward from the issue,
/// kinds read inward). A key or a name without a direction matches both ways.
pub fn kinds_for(types: &[LinkType], value: &str) -> Option<(Vec<String>, Vec<String>)> {
    let n = value.trim();
    if let Some(t) = types.iter().find(|t| t.key.eq_ignore_ascii_case(n)) {
        return Some((vec![t.key.clone()], vec![t.key.clone()]));
    }
    let (key, outward) = resolve(types, n, false)?;
    let symmetric = find(types, &key).is_some_and(|t| t.symmetric());
    Some(match (outward, symmetric) {
        (_, true) => (vec![key.clone()], vec![key]),
        (true, false) => (vec![key], vec![]),
        (false, false) => (vec![], vec![key]),
    })
}

/// A key for a new type from its name: `Is caused by` → `is-caused-by`.
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s: String = s.trim_end_matches('-').chars().take(40).collect();
    s.trim_end_matches('-').to_string()
}

/// A free key for a name: its slug, or the slug with a number when that's taken.
pub fn free_key(types: &[LinkType], name: &str) -> String {
    let base = match slug(name) {
        s if s.is_empty() => "link".to_string(),
        s => s,
    };
    let taken = |k: &str| types.iter().any(|t| t.key == k);
    if !taken(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|k| !taken(k)).unwrap_or(base)
}

/// SQL: whether an open issue blocks `i` through a link of a dependency type.
pub const BLOCKED_SQL: &str = "EXISTS (SELECT 1 FROM issue_links bl JOIN issues bi ON bi.id = bl.source_id JOIN statuses bs ON bs.id = bi.status_id \
     WHERE bl.target_id = i.id AND (bl.kind = 'blocks' OR bl.kind IN (SELECT lt.key FROM link_types lt WHERE lt.workspace_id = bi.workspace_id AND lt.dependency)) \
     AND bs.category <> 'done' AND bi.deleted_at IS NULL)";

// ---- the API: the workspace's link types (admins change them) ----

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};

fn admin(user: &User) -> ApiResult<()> {
    if user.is_admin() { Ok(()) } else { Err(ApiError::Denied("only workspace admins manage link types".into())) }
}

/// `GET /link-types`: every type with the number of links it has.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    let types = types(&state.db, user.workspace_id).await?;
    let counts: Vec<(String, i64)> =
        sqlx::query_as("SELECT l.kind, count(*) FROM issue_links l JOIN issues i ON i.id = l.source_id WHERE i.workspace_id = $1 GROUP BY l.kind")
            .bind(user.workspace_id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!(
        types
            .iter()
            .map(|t| {
                let mut v = json!(t);
                v["linkCount"] = json!(counts.iter().find(|c| c.0 == t.key).map(|c| c.1).unwrap_or(0));
                v["symmetric"] = json!(t.symmetric());
                v
            })
            .collect::<Vec<_>>()
    )))
}

fn name(v: &Value, what: &str) -> ApiResult<Option<String>> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => {
            let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
            if s.is_empty() {
                return Err(ApiError::bad(format!("the {what} name can't be empty")));
            }
            if s.chars().count() > 60 {
                return Err(ApiError::bad("link type names are at most 60 characters"));
            }
            Ok(Some(s))
        }
        _ => Err(ApiError::bad(format!("{what} must be a text"))),
    }
}

/// Names must say which type and direction they mean: none may read like another type's.
fn check_names(types: &[LinkType], key: Option<&str>, outward: &str, inward: &str) -> ApiResult<()> {
    for t in types.iter().filter(|t| Some(t.key.as_str()) != key) {
        for n in [outward, inward] {
            if t.outward.eq_ignore_ascii_case(n) || t.inward.eq_ignore_ascii_case(n) || t.key.eq_ignore_ascii_case(n) {
                return Err(ApiError::bad(format!("“{n}” is already a name of the link type “{}”", t.outward)));
            }
        }
    }
    Ok(())
}

/// `POST /link-types` `{outward, inward?, dependency?}`: a type of the workspace's own (admins).
/// Without `inward` it reads the same both ways ("sibling of").
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    admin(&user)?;
    let outward = name(&req["outward"], "outward")?.ok_or_else(|| ApiError::bad("a link type needs a name (outward: how it reads from the first issue)"))?;
    let inward = name(&req["inward"], "inward")?.unwrap_or_else(|| outward.clone());
    let dependency = match &req["dependency"] {
        Value::Null => false,
        v => v.as_bool().ok_or_else(|| ApiError::bad("dependency must be true or false"))?,
    };
    let types = types(&state.db, user.workspace_id).await?;
    check_names(&types, None, &outward, &inward)?;
    let key = free_key(&types, &outward);
    sqlx::query(
        "INSERT INTO link_types (id, workspace_id, key, outward, inward, dependency, position)
         VALUES ($1, $2, $3, $4, $5, $6, (SELECT COALESCE(max(position), 0) + 1 FROM link_types WHERE workspace_id = $2))",
    )
    .bind(new_id())
    .bind(user.workspace_id)
    .bind(&key)
    .bind(&outward)
    .bind(&inward)
    .bind(dependency)
    .execute(&state.db)
    .await?;
    audit(
        &state,
        user.workspace_id,
        Some(user.id),
        "link.type",
        Some(key.clone()),
        json!({"created": true, "outward": outward, "inward": inward, "dependency": dependency}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(json!({"key": key}))))
}

/// `PATCH /link-types/{key}` `{outward?, inward?, dependency?}` (admins). Built-in types can be
/// renamed; whether they're a dependency stays as it is.
pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(key): Path<String>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let types = types(&state.db, user.workspace_id).await?;
    let t = find(&types, &key).ok_or(ApiError::NotFound("link type"))?.clone();
    let outward = name(&req["outward"], "outward")?.unwrap_or_else(|| t.outward.clone());
    let inward = name(&req["inward"], "inward")?.unwrap_or_else(|| if t.symmetric() && req["inward"].is_null() { outward.clone() } else { t.inward.clone() });
    let dependency = match &req["dependency"] {
        Value::Null => t.dependency,
        v => v.as_bool().ok_or_else(|| ApiError::bad("dependency must be true or false"))?,
    };
    if t.builtin && dependency != t.dependency {
        return Err(ApiError::bad("whether a built-in link type is a dependency can't be changed"));
    }
    check_names(&types, Some(&key), &outward, &inward)?;
    sqlx::query(
        "INSERT INTO link_types (id, workspace_id, key, outward, inward, dependency, position) VALUES ($1, $2, $3, $4, $5, $6, 0)
         ON CONFLICT (workspace_id, key) DO UPDATE SET outward = EXCLUDED.outward, inward = EXCLUDED.inward, dependency = EXCLUDED.dependency",
    )
    .bind(new_id())
    .bind(user.workspace_id)
    .bind(&key)
    .bind(&outward)
    .bind(&inward)
    .bind(dependency)
    .execute(&state.db)
    .await?;
    audit(&state, user.workspace_id, Some(user.id), "link.type", Some(key), json!({"outward": outward, "inward": inward, "dependency": dependency})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteQuery {
    /// the type the links of the deleted one get
    replace_with: Option<String>,
}

/// `DELETE /link-types/{key}[?replaceWith=other]` (admins): a type of the workspace's own (its
/// links move to `replaceWith`; without one, only a type nobody uses), or the names a built-in
/// type was given (it reads as before again).
pub async fn delete(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Query(q): Query<DeleteQuery>,
) -> ApiResult<StatusCode> {
    admin(&user)?;
    let ws = user.workspace_id;
    let types = types(&state.db, ws).await?;
    let t = find(&types, &key).ok_or(ApiError::NotFound("link type"))?;
    if t.builtin {
        sqlx::query("DELETE FROM link_types WHERE workspace_id = $1 AND key = $2").bind(ws).bind(&key).execute(&state.db).await?;
        audit(&state, ws, Some(user.id), "link.type", Some(key), json!({"reset": true})).await;
        return Ok(StatusCode::NO_CONTENT);
    }
    let mut tx = state.db.begin().await?;
    let used: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_links l JOIN issues i ON i.id = l.source_id WHERE i.workspace_id = $1 AND l.kind = $2")
        .bind(ws)
        .bind(&key)
        .fetch_one(&mut *tx)
        .await?;
    let mut moved = 0;
    if used > 0 {
        let Some(to) = q.replace_with.as_deref().filter(|r| !r.is_empty()) else {
            return Err(ApiError::Conflict(format!("{used} links have this type: choose the type they get instead (replaceWith)")));
        };
        if to == key || find(&types, to).is_none() {
            return Err(ApiError::bad("replaceWith must be another link type"));
        }
        // links that would then be there twice go
        sqlx::query(
            "DELETE FROM issue_links a USING issues i WHERE i.id = a.source_id AND i.workspace_id = $1 AND a.kind = $2
               AND EXISTS (SELECT 1 FROM issue_links b WHERE b.source_id = a.source_id AND b.target_id = a.target_id AND b.kind = $3)",
        )
        .bind(ws)
        .bind(&key)
        .bind(to)
        .execute(&mut *tx)
        .await?;
        moved = sqlx::query("UPDATE issue_links l SET kind = $3 FROM issues i WHERE i.id = l.source_id AND i.workspace_id = $1 AND l.kind = $2")
            .bind(ws)
            .bind(&key)
            .bind(to)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    }
    sqlx::query("DELETE FROM link_types WHERE workspace_id = $1 AND key = $2").bind(ws).bind(&key).execute(&mut *tx).await?;
    tx.commit().await?;
    audit(&state, ws, Some(user.id), "link.type", Some(key), json!({"deleted": true, "replaceWith": q.replace_with, "moved": moved})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `PUT /link-types/order` `{keys}`: the order of the workspace's own types (admins).
pub async fn order(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    admin(&user)?;
    let keys: Vec<String> = serde_json::from_value(req["keys"].clone()).map_err(|_| ApiError::bad("keys must be a list of link type keys"))?;
    for (i, k) in keys.iter().enumerate() {
        sqlx::query("UPDATE link_types SET position = $3 WHERE workspace_id = $1 AND key = $2")
            .bind(user.workspace_id)
            .bind(k)
            .bind(i as f64 + 1.0)
            .execute(&state.db)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> Vec<LinkType> {
        merge(vec![
            ("causes".into(), "causes".into(), "is caused by".into(), false),
            ("precedes".into(), "precedes".into(), "follows".into(), true),
            ("relates".into(), "is related to".into(), "is related to".into(), false),
            ("sibling".into(), "sibling of".into(), "sibling of".into(), false),
        ])
    }

    #[test]
    fn built_in_first_then_own() {
        let t = ws();
        let keys: Vec<&str> = t.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, ["blocks", "relates", "duplicates", "clones", "causes", "precedes", "sibling"]);
        assert!(t[1].renamed && t[1].builtin && t[1].outward == "is related to");
        assert!(t[0].dependency && !t[2].dependency && t[5].dependency);
    }

    #[test]
    fn resolves_keys_names_and_old_spellings() {
        let t = ws();
        assert_eq!(resolve(&t, "causes", false), Some(("causes".into(), true)));
        assert_eq!(resolve(&t, "causes", true), Some(("causes".into(), false)));
        assert_eq!(resolve(&t, "Is caused by", false), Some(("causes".into(), false)));
        assert_eq!(resolve(&t, "follows", false), Some(("precedes".into(), false)));
        assert_eq!(resolve(&t, "is blocked by", false), Some(("blocks".into(), false)));
        assert_eq!(resolve(&t, "blockedBy", false), Some(("blocks".into(), false)));
        // a renamed built-in type: the new name and the old one
        assert_eq!(resolve(&t, "is related to", false), Some(("relates".into(), true)));
        assert_eq!(resolve(&t, "relates to", false), Some(("relates".into(), true)));
        // no direction for symmetric types
        assert_eq!(resolve(&t, "sibling", true), Some(("sibling".into(), true)));
        assert_eq!(resolve(&t, "nothing", false), None);
    }

    #[test]
    fn tql_kinds() {
        let t = ws();
        assert_eq!(kinds_for(&t, "causes"), Some((vec!["causes".into()], vec!["causes".into()])));
        assert_eq!(kinds_for(&t, "is caused by"), Some((vec![], vec!["causes".into()])));
        assert_eq!(kinds_for(&t, "blocks"), Some((vec!["blocks".into()], vec!["blocks".into()])));
        assert_eq!(kinds_for(&t, "is blocked by"), Some((vec![], vec!["blocks".into()])));
        assert_eq!(kinds_for(&t, "sibling of"), Some((vec!["sibling".into()], vec!["sibling".into()])));
    }

    #[test]
    fn keys_from_names() {
        assert_eq!(slug(" Is caused by "), "is-caused-by");
        assert_eq!(slug("Tests / is tested by"), "tests-is-tested-by");
        assert_eq!(slug("Übergeordnet"), "übergeordnet");
        let t = ws();
        assert_eq!(free_key(&t, "Causes"), "causes-2");
        assert_eq!(free_key(&t, "Implements"), "implements");
        assert_eq!(free_key(&t, "!!!"), "link");
    }
}
