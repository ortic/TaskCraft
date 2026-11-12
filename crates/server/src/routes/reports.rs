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
}

pub async fn burndown(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    let project = crate::reports::sprint_project(&state, id).await?;
    access::require(&state, &user, project, Role::Viewer).await?;
    Ok(Json(crate::reports::burndown(&state, id).await?))
}

pub async fn project_report(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    Path((key, report)): Path<(String, String)>,
    Query(r): Query<Range>,
) -> ApiResult<Json<Value>> {
    let (p, _) = by_key(&state, &user, &key, Role::Viewer).await?;
    let days = r.days.unwrap_or(30);
    Ok(Json(match report.as_str() {
        "velocity" => crate::reports::velocity(&state, p.id, r.count.unwrap_or(8)).await?,
        "cumulative-flow" => crate::reports::cumulative_flow(&state, p.id, days).await?,
        "cycle-time" => crate::reports::cycle_time(&state, p.id, r.days.unwrap_or(90)).await?,
        "created-resolved" => crate::reports::created_resolved(&state, p.id, days).await?,
        "workload" => crate::reports::workload(&state, p.id).await?,
        "overview" => crate::reports::overview(&state, p.id).await?,
        _ => return Err(crate::error::ApiError::NotFound("report")),
    }))
}
