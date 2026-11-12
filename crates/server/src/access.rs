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
