//! Requester organizations: the customers whose people write to the service desk. Members are
//! e-mail addresses (requesters have no accounts); someone writing from one of an organization's
//! domains joins it. A request belongs to its requester's organization, which agents see (and can
//! change) on the issue, filter by in TQL (`organization = Acme`) and whose service-level goals can
//! be shorter (`slaPercent`). In the portal a requester shares a request with their organization
//! or with colleagues, who then follow and answer it too.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User};
use crate::error::{ApiError, ApiResult};

fn staff(user: &User) -> ApiResult<()> {
    if user.is_guest() { Err(ApiError::Denied("guests don't manage organizations".into())) } else { Ok(()) }
}

pub fn domain_of(email: &str) -> Option<String> {
    email.rsplit_once('@').map(|(_, d)| d.trim().to_lowercase()).filter(|d| d.contains('.'))
}

/// The organization of an address in a workspace: a member, else one with its domain (they
/// join it then).
pub async fn of_email<'e>(db: impl sqlx::PgExecutor<'e> + Copy, workspace: Uuid, email: &str, name: Option<&str>) -> sqlx::Result<Option<Uuid>> {
    let member: Option<Uuid> = sqlx::query_scalar(
        "SELECT m.organization_id FROM organization_members m JOIN organizations o ON o.id = m.organization_id
          WHERE o.workspace_id = $1 AND lower(m.email) = lower($2) ORDER BY m.created_at LIMIT 1",
    )
    .bind(workspace)
    .bind(email)
    .fetch_optional(db)
    .await?;
    if member.is_some() {
        return Ok(member);
    }
    let Some(domain) = domain_of(email) else { return Ok(None) };
    let by_domain: Option<Uuid> = sqlx::query_scalar("SELECT id FROM organizations WHERE workspace_id = $1 AND $2 = ANY(domains) ORDER BY created_at LIMIT 1")
        .bind(workspace)
        .bind(&domain)
        .fetch_optional(db)
        .await?;
    if let Some(o) = by_domain {
        sqlx::query("INSERT INTO organization_members (organization_id, email, name) VALUES ($1, lower($2), $3) ON CONFLICT DO NOTHING")
            .bind(o)
            .bind(email)
            .bind(name)
            .execute(db)
            .await?;
    }
    Ok(by_domain)
}

/// A new request's organization, from its requester.
pub async fn attach(state: &AppState, issue: Uuid, email: &str, name: Option<&str>) -> sqlx::Result<()> {
    let ws: Uuid = sqlx::query_scalar("SELECT workspace_id FROM issues WHERE id = $1").bind(issue).fetch_one(&state.db).await?;
    if let Some(o) = of_email(&state.db, ws, email, name).await? {
        sqlx::query("UPDATE issue_requesters SET organization_id = $2 WHERE issue_id = $1").bind(issue).bind(o).execute(&state.db).await?;
    }
    Ok(())
}

// ---- for agents ----

fn domains(v: &Value) -> ApiResult<Vec<String>> {
    let list: Vec<String> = match v {
        Value::Null => vec![],
        Value::Array(a) => a.iter().filter_map(|d| d.as_str()).map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect(),
        Value::String(s) => s.split([',', ' ', ';']).map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect(),
        _ => return Err(ApiError::bad("domains is a list like [\"acme.com\"]")),
    };
    for d in &list {
        if !d.contains('.') || d.len() > 100 || !d.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-') {
            return Err(ApiError::bad(format!("{d} isn't a mail domain like acme.com")));
        }
    }
    Ok(list)
}

fn percent(v: &Value) -> ApiResult<Option<i32>> {
    match v {
        Value::Null => Ok(None),
        v => v.as_i64().filter(|p| (10..=500).contains(p)).map(|p| Some(p as i32)).ok_or_else(|| ApiError::bad("slaPercent is 10 to 500")),
    }
}

fn duplicate(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => ApiError::Conflict("there is an organization of that name".into()),
        _ => e.into(),
    }
}

/// `GET /organizations`: the workspace's organizations with their members and open requests counted.
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    staff(&user)?;
    type R = (Uuid, String, Vec<String>, i32, i64, i64, Option<String>, Option<String>, bool);
    let rows: Vec<R> = sqlx::query_as(
        "SELECT o.id, o.name, o.domains, o.sla_percent,
                (SELECT count(*) FROM organization_members m WHERE m.organization_id = o.id),
                (SELECT count(*) FROM issue_requesters r JOIN issues i ON i.id = r.issue_id JOIN statuses s ON s.id = i.status_id
                  WHERE r.organization_id = o.id AND i.deleted_at IS NULL AND s.category <> 'done'),
                o.number, o.contact, o.archived_at IS NOT NULL
           FROM organizations o WHERE o.workspace_id = $1 ORDER BY lower(o.name)",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(
        rows.into_iter()
            .map(|(id, name, domains, sla, members, open, number, contact, archived)| json!({
                "id": id, "name": name, "domains": domains, "slaPercent": sla, "memberCount": members, "openRequests": open,
                "number": number, "contact": contact, "archived": archived,
            }))
            .collect::<Vec<_>>()
    )))
}

/// `POST /organizations` `{name, domains?, notes?, slaPercent?}`.
pub async fn create(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    staff(&user)?;
    let name = req["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty() && n.chars().count() <= 120)
        .ok_or_else(|| ApiError::bad("an organization has a name (up to 120 characters)"))?;
    let id = new_id();
    sqlx::query("INSERT INTO organizations (id, workspace_id, name, domains, notes, sla_percent, number, contact) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(name)
        .bind(domains(&req["domains"])?)
        .bind(req["notes"].as_str().map(str::trim).filter(|n| !n.is_empty()))
        .bind(percent(&req["slaPercent"])?.unwrap_or(100))
        .bind(short(&req["number"], 60))
        .bind(short(&req["contact"], 300))
        .execute(&state.db)
        .await
        .map_err(duplicate)?;
    audit(&state, user.workspace_id, Some(user.id), "organization.created", Some(name.to_string()), json!({})).await;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// A short optional text of a request.
fn short(v: &Value, max: usize) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(max).collect())
}

async fn org(state: &AppState, user: &User, id: Uuid) -> ApiResult<(String, Vec<String>, Option<String>, i32)> {
    sqlx::query_as("SELECT name, domains, notes, sla_percent FROM organizations WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(user.workspace_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("organization"))
}

/// `GET /organizations/{id}`: with its members, notes and the requests the caller may see.
pub async fn get(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    staff(&user)?;
    let (name, domains, notes, sla) = org(&state, &user, id).await?;
    let members: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT email, name FROM organization_members WHERE organization_id = $1 ORDER BY lower(coalesce(name, email))")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.id FROM issue_requesters r JOIN issues i ON i.id = r.issue_id JOIN statuses s ON s.id = i.status_id
          WHERE r.organization_id = $1 AND i.deleted_at IS NULL ORDER BY s.category = 'done', i.updated_at DESC LIMIT 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let requests = crate::issues::load_many_for(&state, user.id, &ids).await?;
    let (number, contact, archived): (Option<String>, Option<String>, bool) =
        sqlx::query_as("SELECT number, contact, archived_at IS NOT NULL FROM organizations WHERE id = $1").bind(id).fetch_one(&state.db).await?;
    // the projects that belong to it (the ones the caller sees)
    let visible = crate::access::visible_projects(&state, &user).await?;
    let projects: Vec<(String, String)> = sqlx::query_as("SELECT key, name FROM projects WHERE customer_id = $1 AND id = ANY($2) ORDER BY key")
        .bind(id)
        .bind(&visible)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({
        "id": id, "name": name, "domains": domains, "notes": notes, "slaPercent": sla,
        "number": number, "contact": contact, "archived": archived,
        "projects": projects.into_iter().map(|(k, n)| json!({"key": k, "name": n})).collect::<Vec<_>>(),
        "members": members.into_iter().map(|(e, n)| json!({"email": e, "name": n})).collect::<Vec<_>>(),
        "requests": requests,
    })))
}

/// `PATCH /organizations/{id}` `{name?, domains?, notes?, slaPercent?}`.
pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    staff(&user)?;
    let (name, cur_domains, notes, sla) = org(&state, &user, id).await?;
    let name = match &req["name"] {
        Value::Null => name,
        v => v
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.chars().count() <= 120)
            .ok_or_else(|| ApiError::bad("an organization has a name"))?
            .to_string(),
    };
    let dom = if req.get("domains").is_some() { domains(&req["domains"])? } else { cur_domains };
    let notes = if req.get("notes").is_some() { req["notes"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string) } else { notes };
    let sla = percent(&req["slaPercent"])?.unwrap_or(sla);
    sqlx::query("UPDATE organizations SET name = $2, domains = $3, notes = $4, sla_percent = $5 WHERE id = $1")
        .bind(id)
        .bind(&name)
        .bind(dom)
        .bind(notes)
        .bind(sla)
        .execute(&state.db)
        .await
        .map_err(duplicate)?;
    // the customer's side: number, contact, archived
    if req.get("number").is_some() {
        sqlx::query("UPDATE organizations SET number = $2 WHERE id = $1").bind(id).bind(short(&req["number"], 60)).execute(&state.db).await?;
    }
    if req.get("contact").is_some() {
        sqlx::query("UPDATE organizations SET contact = $2 WHERE id = $1").bind(id).bind(short(&req["contact"], 300)).execute(&state.db).await?;
    }
    if let Some(a) = req["archived"].as_bool() {
        sqlx::query("UPDATE organizations SET archived_at = CASE WHEN $2 THEN COALESCE(archived_at, now()) END WHERE id = $1")
            .bind(id)
            .bind(a)
            .execute(&state.db)
            .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /organizations/{id}` (workspace admins): its requests keep their requesters.
pub async fn delete(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if !user.is_admin() {
        return Err(ApiError::Denied("only workspace admins delete organizations".into()));
    }
    let (name, ..) = org(&state, &user, id).await?;
    sqlx::query("DELETE FROM organizations WHERE id = $1").bind(id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "organization.deleted", Some(name), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

fn email(v: &Value) -> ApiResult<String> {
    let e = v.as_str().map(|s| s.trim().to_lowercase()).unwrap_or_default();
    if e.len() > 254 || !e.contains('@') || e.starts_with('@') || e.ends_with('@') || e.contains(char::is_whitespace) {
        return Err(ApiError::bad("that isn't an e-mail address"));
    }
    Ok(e)
}

/// `POST /organizations/{id}/members` `{email, name?}`; their requests without an organization
/// join it.
pub async fn add_member(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    staff(&user)?;
    org(&state, &user, id).await?;
    let e = email(&req["email"])?;
    sqlx::query("INSERT INTO organization_members (organization_id, email, name) VALUES ($1, $2, $3) ON CONFLICT (organization_id, email) DO UPDATE SET name = COALESCE(EXCLUDED.name, organization_members.name)")
        .bind(id)
        .bind(&e)
        .bind(req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()))
        .execute(&state.db)
        .await?;
    sqlx::query(
        "UPDATE issue_requesters r SET organization_id = $1 FROM issues i
          WHERE i.id = r.issue_id AND i.workspace_id = $3 AND lower(r.email) = $2 AND r.organization_id IS NULL",
    )
    .bind(id)
    .bind(&e)
    .bind(user.workspace_id)
    .execute(&state.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_member(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path((id, address)): Path<(Uuid, String)>) -> ApiResult<StatusCode> {
    staff(&user)?;
    org(&state, &user, id).await?;
    sqlx::query("DELETE FROM organization_members WHERE organization_id = $1 AND lower(email) = lower($2)").bind(id).bind(&address).execute(&state.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /organizations/import` (CSV: organization, email, name — a header row is skipped):
/// organizations made as needed, members added.
pub async fn import(State(state): State<AppState>, CurrentUser(user): CurrentUser, body: String) -> ApiResult<Json<Value>> {
    staff(&user)?;
    let mut rdr = csv::ReaderBuilder::new().has_headers(false).flexible(true).from_reader(body.as_bytes());
    let (mut orgs, mut members, mut skipped) = (0, 0, 0);
    let mut tx = state.db.begin().await?;
    for (n, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| ApiError::bad(format!("line {}: {e}", n + 1)))?;
        let name = rec.get(0).map(str::trim).unwrap_or("");
        let mail = rec.get(1).map(|s| s.trim().to_lowercase()).unwrap_or_default();
        if n == 0 && name.eq_ignore_ascii_case("organization") {
            continue;
        }
        if name.is_empty() || !mail.contains('@') {
            skipped += 1;
            continue;
        }
        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM organizations WHERE workspace_id = $1 AND lower(name) = lower($2)")
            .bind(user.workspace_id)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
        let id = match id {
            Some(id) => id,
            None => {
                let id = new_id();
                sqlx::query("INSERT INTO organizations (id, workspace_id, name) VALUES ($1, $2, $3)")
                    .bind(id)
                    .bind(user.workspace_id)
                    .bind(name)
                    .execute(&mut *tx)
                    .await?;
                orgs += 1;
                id
            }
        };
        let added = sqlx::query("INSERT INTO organization_members (organization_id, email, name) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(id)
            .bind(&mail)
            .bind(rec.get(2).map(str::trim).filter(|s| !s.is_empty()))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        members += added;
        sqlx::query("UPDATE issue_requesters r SET organization_id = $1 FROM issues i WHERE i.id = r.issue_id AND i.workspace_id = $3 AND lower(r.email) = $2 AND r.organization_id IS NULL")
            .bind(id)
            .bind(&mail)
            .bind(user.workspace_id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "organization.imported", None, json!({"organizations": orgs, "members": members})).await;
    Ok(Json(json!({"organizations": orgs, "members": members, "skipped": skipped})))
}

/// `PUT /issues/{key}/organization` `{organizationId}` (project members): which organization a
/// request belongs to (null: none).
pub async fn set_for_issue(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<StatusCode> {
    let (id, project, _) = crate::routes::issues::issue_for(&state, &user, &key, Role::Member).await?;
    let org: Option<Uuid> = match &req["organizationId"] {
        Value::Null => None,
        v => {
            let o: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("organizationId is an organization's id or null"))?;
            self::org(&state, &user, o).await?;
            Some(o)
        }
    };
    let n = sqlx::query("UPDATE issue_requesters SET organization_id = $2 WHERE issue_id = $1").bind(id).bind(org).execute(&state.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::bad("this issue has no requester"));
    }
    crate::events::publish(&state, crate::events::Event::new(user.workspace_id, "issue").project(project).actor(user.id)).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---- for requesters (the portal) ----

/// The organization of a portal visitor (by their address), if any: (id, name).
pub async fn of_requester(state: &AppState, email: &str) -> sqlx::Result<Option<(Uuid, String)>> {
    sqlx::query_as(
        "SELECT o.id, o.name FROM organization_members m JOIN organizations o ON o.id = m.organization_id WHERE lower(m.email) = lower($1) ORDER BY m.created_at LIMIT 1",
    )
    .bind(email)
    .fetch_optional(&state.db)
    .await
}

/// SQL: the requests (`r`: issue_requesters) a portal visitor may see — their own, ones shared
/// with their organization, ones shared with them; `p` is the parameter with their address.
pub fn visible_sql(p: &str) -> String {
    format!(
        "(lower(r.email) = lower({p})
          OR (r.shared AND r.organization_id IN (SELECT m.organization_id FROM organization_members m WHERE lower(m.email) = lower({p})))
          OR r.issue_id IN (SELECT rs.issue_id FROM request_shares rs WHERE lower(rs.email) = lower({p})))"
    )
}

/// `GET /api/portal/colleagues`: the signed-in requester's organization and its members (to
/// share a request with).
pub async fn colleagues(State(state): State<AppState>, crate::portal::Requester(email): crate::portal::Requester) -> ApiResult<Json<Value>> {
    let Some((id, name)) = of_requester(&state, &email).await? else {
        return Ok(Json(json!({"organization": null, "members": []})));
    };
    let members: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT email, name FROM organization_members WHERE organization_id = $1 AND lower(email) <> lower($2) ORDER BY lower(coalesce(name, email)) LIMIT 500",
    )
    .bind(id)
    .bind(&email)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"organization": name, "members": members.into_iter().map(|(e, n)| json!({"email": e, "name": n})).collect::<Vec<_>>()})))
}

/// `PUT /api/portal/requests/{key}/share` `{organization?, emails?}` (the requester): who else
/// follows the request — the whole organization and/or colleagues in it.
pub async fn share(
    State(state): State<AppState>,
    crate::portal::Requester(email): crate::portal::Requester,
    Path(key): Path<String>,
    Json(req): Json<Value>,
) -> ApiResult<Json<Value>> {
    let (k, n) = crate::query::split_key(&key).ok_or(ApiError::NotFound("request"))?;
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT i.id, r.organization_id FROM issues i JOIN projects p ON p.id = i.project_id JOIN issue_requesters r ON r.issue_id = i.id
          WHERE upper(p.key) = $1 AND i.number = $2 AND lower(r.email) = lower($3) AND i.deleted_at IS NULL",
    )
    .bind(&k)
    .bind(n)
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;
    let (issue, org) = row.ok_or(ApiError::NotFound("request"))?;
    let org = org.ok_or_else(|| ApiError::bad("this request belongs to no organization"))?;
    let me = email.to_lowercase();
    let emails: Vec<String> =
        req["emails"].as_array().map(|a| a.iter().filter_map(|e| e.as_str()).map(|e| e.trim().to_lowercase()).collect()).unwrap_or_default();
    let mut tx = state.db.begin().await?;
    if let Some(on) = req["organization"].as_bool() {
        sqlx::query("UPDATE issue_requesters SET shared = $2 WHERE issue_id = $1").bind(issue).bind(on).execute(&mut *tx).await?;
    }
    if req.get("emails").is_some() {
        sqlx::query("DELETE FROM request_shares WHERE issue_id = $1").bind(issue).execute(&mut *tx).await?;
        for e in emails.iter().filter(|e| **e != me) {
            let member: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM organization_members WHERE organization_id = $1 AND lower(email) = $2)")
                .bind(org)
                .bind(e)
                .fetch_one(&mut *tx)
                .await?;
            if !member {
                return Err(ApiError::bad(format!("{e} isn't in your organization")));
            }
            sqlx::query("INSERT INTO request_shares (issue_id, email) VALUES ($1, $2) ON CONFLICT DO NOTHING").bind(issue).bind(e).execute(&mut *tx).await?;
        }
    }
    tx.commit().await?;
    let shared: bool = sqlx::query_scalar("SELECT shared FROM issue_requesters WHERE issue_id = $1").bind(issue).fetch_one(&state.db).await?;
    let with: Vec<String> = sqlx::query_scalar("SELECT email FROM request_shares WHERE issue_id = $1 ORDER BY email").bind(issue).fetch_all(&state.db).await?;
    Ok(Json(json!({"shared": shared, "sharedWith": with})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_are_checked() {
        assert_eq!(domains(&json!("acme.com, @Acme.ch")).unwrap(), ["acme.com", "acme.ch"]);
        assert!(domains(&json!(["acme"])).is_err());
        assert_eq!(domain_of("Mia@Acme.COM").as_deref(), Some("acme.com"));
        assert_eq!(domain_of("nobody"), None);
    }
}
