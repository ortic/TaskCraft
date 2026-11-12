//! Who may do what in a project.
//!
//! Roles: viewer (read, comment, watch), member (create and edit issues, run sprints) and admin
//! (settings, workflow, members). Workspace owners and admins are admins everywhere; members get
//! a project's default role plus their own or their teams' grants; guests only their grants.
//! Archived projects are read-only except for workspace admins.

use serde::Serialize;
use uuid::Uuid;

use crate::app::AppState;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    None = 0,
    Viewer = 1,
    Member = 2,
    Admin = 3,
}

impl Role {
    pub fn from_rank(r: i32) -> Role {
        match r {
            3.. => Role::Admin,
            2 => Role::Member,
            1 => Role::Viewer,
            _ => Role::None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::None => "none",
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "none" => Role::None,
            "viewer" => Role::Viewer,
            "member" => Role::Member,
            "admin" => Role::Admin,
            _ => return None,
        })
    }
}

/// Project actions whose minimum role each project chooses (`settings.permissions`): the action,
/// its default, whether "nobody" is allowed, and how a refusal reads.
pub const ACTIONS: [(&str, Role, bool, &str); 7] = [
    ("deleteIssues", Role::Member, false, "delete issues"),
    ("moveIssues", Role::Member, false, "move issues to another project"),
    ("manageSprints", Role::Member, false, "plan, start and complete sprints"),
    ("manageReleases", Role::Member, false, "create, change and release releases"),
    ("moderateComments", Role::Admin, true, "edit or delete other people's comments"),
    ("logTimeForOthers", Role::Admin, true, "log or change time for other people"),
    ("setSecurity", Role::Member, false, "change who sees an issue"),
];

/// Checks a project's `permissions` setting: known actions, member / admin (or nobody where allowed).
pub fn check_permissions(v: &serde_json::Value) -> ApiResult<serde_json::Value> {
    let o = v.as_object().ok_or_else(|| ApiError::bad("permissions must be an object"))?;
    let mut out = serde_json::Map::new();
    for (k, val) in o {
        let (_, _, nobody, _) = ACTIONS.iter().find(|a| a.0 == k).ok_or_else(|| ApiError::bad(format!("unknown permission {k}")))?;
        match val.as_str() {
            Some("member" | "admin") => {}
            Some("nobody") if *nobody => {}
            _ => return Err(ApiError::bad(format!("{k} takes member, admin{}", if *nobody { " or nobody" } else { "" }))),
        }
        out.insert(k.clone(), val.clone());
    }
    Ok(serde_json::Value::Object(out))
}

/// Whether someone with `role` in `project` may do `action` (see [`ACTIONS`]).
pub async fn require_action(state: &AppState, project: Uuid, role: Role, action: &str) -> ApiResult<()> {
    let (_, default, _, what) = ACTIONS.iter().find(|a| a.0 == action).copied().unwrap_or((action, Role::Admin, false, "do that"));
    let settings: serde_json::Value = sqlx::query_scalar("SELECT settings FROM projects WHERE id = $1").bind(project).fetch_one(&state.db).await?;
    let needed = match settings["permissions"][action].as_str() {
        Some("member") => Role::Member,
        Some("admin") => Role::Admin,
        Some("nobody") => return Err(ApiError::Denied(format!("nobody may {what} in this project"))),
        _ => default,
    };
    if role < needed {
        return Err(ApiError::Denied(format!("only {} may {what} in this project", if needed == Role::Admin { "project admins" } else { "members" })));
    }
    Ok(())
}

/// The caller's role in one project (None when the project doesn't exist in their workspace).
pub async fn role(state: &AppState, user: &User, project: Uuid) -> ApiResult<Role> {
    let rank: Option<i32> =
        sqlx::query_scalar("SELECT rank FROM tc_project_roles($1) WHERE project_id = $2").bind(user.id).bind(project).fetch_optional(&state.db).await?;
    Ok(Role::from_rank(rank.unwrap_or(0)))
}

/// Requires at least `min` in the project; a project the caller can't see is "not found".
pub async fn require(state: &AppState, user: &User, project: Uuid, min: Role) -> ApiResult<Role> {
    let r = role(state, user, project).await?;
    if r == Role::None {
        return Err(ApiError::NotFound("project"));
    }
    if r < min {
        return Err(ApiError::Denied(match min {
            Role::Admin => "only project admins may do this".into(),
            _ => "you can only view this project".into(),
        }));
    }
    Ok(r)
}

/// The projects the caller can see.
pub async fn visible_projects(state: &AppState, user: &User) -> ApiResult<Vec<Uuid>> {
    Ok(sqlx::query_scalar("SELECT project_id FROM tc_project_roles($1) WHERE rank >= 1").bind(user.id).fetch_all(&state.db).await?)
}

/// Whether a person (by id) can see a project — for notifications and mentions.
pub async fn can_view(state: &AppState, user_id: Uuid, project: Uuid) -> bool {
    sqlx::query_scalar::<_, i32>("SELECT rank FROM tc_project_roles($1) WHERE project_id = $2")
        .bind(user_id)
        .bind(project)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .is_some_and(|r| r >= 1)
}

/// SQL condition: the person `uid` (a placeholder like `$1`) sees the issue `alias` as far as its
/// security level goes — no level, a level they're in (project admins are in all of them), or
/// they reported, created or are assigned the issue. Their role in its project is checked apart.
pub fn sees_issue(alias: &str, uid: &str) -> String {
    format!(
        "({alias}.security_level_id IS NULL OR {alias}.reporter_id = {uid} OR {alias}.assignee_id = {uid} OR {alias}.created_by = {uid} \
         OR {alias}.security_level_id IN (SELECT tc_visible_levels({uid})))"
    )
}

/// The issues among `ids` that a person sees: their project, and their security level.
pub async fn visible_issues(state: &AppState, user: Uuid, ids: &[Uuid]) -> ApiResult<std::collections::HashSet<Uuid>> {
    if ids.is_empty() {
        return Ok(Default::default());
    }
    let sql = format!(
        "SELECT i.id FROM issues i JOIN tc_project_roles($1) r ON r.project_id = i.project_id AND r.rank >= 1 WHERE i.id = ANY($2) AND {}",
        sees_issue("i", "$1")
    );
    Ok(sqlx::query_scalar::<_, Uuid>(sqlx::AssertSqlSafe(sql)).bind(user).bind(ids).fetch_all(&state.db).await?.into_iter().collect())
}

/// Whether a person sees one issue (its project and its security level) — for notifications.
pub async fn sees(state: &AppState, user: Uuid, issue: Uuid) -> bool {
    visible_issues(state, user, &[issue]).await.is_ok_and(|s| s.contains(&issue))
}
