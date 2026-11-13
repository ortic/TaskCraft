//! Report endpoints (the numbers come from `crate::reports`).

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::projects::by_key;
use crate::access::{self, Role};
use crate::app::AppState;
use crate::auth::CurrentUser;
use crate::error::ApiResult;

#[derive(Deserialize)]
pub struct Range {
    days: Option<i64>,
    count: Option<i64>,
    /// the project's subprojects too (overview, created vs resolved, cycle time, workload)
    #[serde(default)]
    subprojects: bool,
}

pub async fn burndown(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let project = crate::reports::sprint_project(&state, id).await?;
    access::require(&state, &user, project, Role::Viewer).await?;
    Ok(Json(crate::reports::burndown(&state, id, Some(user.id)).await?))
}

pub async fn project_report(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path((key, report)): Path<(String, String)>,
    Query(r): Query<Range>,
) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let days = r.days.unwrap_or(30);
    // the project, and with `subprojects` the ones below it that the caller sees
    let projects = if r.subprojects {
        let visible = access::visible_projects(&state, &user).await?;
        super::projects::tree(&state, p.id).await?.into_iter().filter(|id| visible.contains(id)).collect()
    } else {
        vec![p.id]
    };
    Ok(Json(match report.as_str() {
        "velocity" => crate::reports::velocity(&state, p.id, r.count.unwrap_or(8)).await?,
        "cumulative-flow" => crate::reports::cumulative_flow(&state, p.id, days).await?,
        "cycle-time" => crate::reports::cycle_time(&state, &projects, r.days.unwrap_or(90), Some(user.id)).await?,
        "created-resolved" => crate::reports::created_resolved(&state, &projects, days).await?,
        "workload" => crate::reports::workload(&state, &projects).await?,
        "overview" => crate::reports::overview(&state, &projects).await?,
        _ => return Err(crate::error::ApiError::NotFound("report")),
    }))
}
