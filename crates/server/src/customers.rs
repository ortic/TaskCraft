//! Customers: who time is for.
//!
//! A customer is an organization (the same list the service desk's requesters belong to) with a
//! number and a contact. A project — or subproject — belongs to a customer by default
//! (`tc_project_customer`: its own, else its parent's); a time log can name another one. The time
//! report groups and filters by customer, hourly rates turn billable time into amounts, and once a
//! period is billed its time logs are closed for everyone but admins.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::NaiveDate;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User, require_admin};
use crate::error::{ApiError, ApiResult};

/// A customer named in a request (null: none): an organization of the workspace that isn't archived.
pub async fn customer_of(state: &AppState, workspace: Uuid, v: &Value) -> ApiResult<Option<Uuid>> {
    if v.is_null() {
        return Ok(None);
    }
    let id: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("customerId is a customer's id"))?;
    let archived: Option<bool> = sqlx::query_scalar("SELECT archived_at IS NOT NULL FROM organizations WHERE id = $1 AND workspace_id = $2")
        .bind(id)
        .bind(workspace)
        .fetch_optional(&state.db)
        .await?;
    match archived {
        None => Err(ApiError::bad("there is no such customer")),
        Some(true) => Err(ApiError::bad("that customer is archived")),
        Some(false) => Ok(Some(id)),
    }
}

/// The day up to which time is closed (billed), if any.
pub async fn closed_until(state: &AppState, workspace: Uuid) -> ApiResult<Option<NaiveDate>> {
    let d: Option<String> =
        sqlx::query_scalar("SELECT settings->>'timeClosedUntil' FROM workspaces WHERE id = $1").bind(workspace).fetch_one(&state.db).await?;
    Ok(d.and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()))
}

/// Time on `date` can still be logged, changed or deleted by `user` (admins always can).
pub async fn check_open(state: &AppState, user: &User, date: NaiveDate) -> ApiResult<()> {
    if user.is_admin() {
        return Ok(());
    }
    match closed_until(state, user.workspace_id).await? {
        Some(closed) if date <= closed => Err(ApiError::Denied(format!("time up to {closed} is closed (it has been billed): ask an admin to change it"))),
        _ => Ok(()),
    }
}

/// An hourly rate and what it's for.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Rate {
    pub customer_id: Option<Uuid>,
    pub category_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    /// in hundredths of the currency
    pub cents: i64,
}

pub async fn rates(state: &AppState, workspace: Uuid) -> ApiResult<Vec<Rate>> {
    Ok(sqlx::query_as("SELECT customer_id, category_id, user_id, (rate * 100)::bigint AS cents FROM time_rates WHERE workspace_id = $1")
        .bind(workspace)
        .fetch_all(&state.db)
        .await?)
}

/// The rate for a time log: of those that fit, the most specific (a customer counts most, then a
/// person, then a category).
pub fn rate_for(rates: &[Rate], customer: Option<Uuid>, category: Option<Uuid>, user: Option<Uuid>) -> Option<i64> {
    rates
        .iter()
        .filter(|r| {
            r.customer_id.is_none_or(|c| Some(c) == customer) && r.category_id.is_none_or(|c| Some(c) == category) && r.user_id.is_none_or(|u| Some(u) == user)
        })
        .max_by_key(|r| r.customer_id.is_some() as u8 * 4 + r.user_id.is_some() as u8 * 2 + r.category_id.is_some() as u8)
        .map(|r| r.cents)
}

/// What `minutes` cost at a rate (hundredths), rounded to the hundredth.
pub fn amount(minutes: i64, rate_cents: i64) -> i64 {
    (minutes * rate_cents + 30) / 60
}

/// `GET /time-rates` (workspace admins): the rates, the currency and the day time is closed up to.
pub async fn get_rates(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    type Row = (Uuid, Option<Uuid>, Option<String>, Option<Uuid>, Option<String>, Option<Uuid>, Option<String>, f64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT r.id, r.customer_id, o.name, r.category_id, c.name, r.user_id, u.name, r.rate::float8
           FROM time_rates r LEFT JOIN organizations o ON o.id = r.customer_id LEFT JOIN time_categories c ON c.id = r.category_id LEFT JOIN users u ON u.id = r.user_id
          WHERE r.workspace_id = $1 ORDER BY lower(o.name) NULLS FIRST, lower(u.name) NULLS FIRST, lower(c.name) NULLS FIRST",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    let settings: Value = sqlx::query_scalar("SELECT settings FROM workspaces WHERE id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    Ok(Json(json!({
        "currency": settings["currency"], "closedUntil": settings["timeClosedUntil"],
        "rates": rows.iter().map(|(id, cust, cust_name, cat, cat_name, uid, user_name, rate)| json!({
            "id": id, "customer": cust.map(|i| json!({"id": i, "name": cust_name})), "category": cat.map(|i| json!({"id": i, "name": cat_name})),
            "user": uid.map(|i| json!({"id": i, "name": user_name})), "rate": rate,
        })).collect::<Vec<_>>(),
    })))
}

/// `PUT /time-rates` `{rates: [{customerId?, categoryId?, userId?, rate}]}` (workspace admins):
/// replaces the rates.
pub async fn put_rates(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let list = req["rates"].as_array().ok_or_else(|| ApiError::bad("rates is a list"))?;
    if list.len() > 500 {
        return Err(ApiError::bad("up to 500 rates"));
    }
    let id_of = |v: &Value, what: &str| -> ApiResult<Option<Uuid>> {
        match v {
            Value::Null => Ok(None),
            v => v.as_str().and_then(|s| s.parse().ok()).map(Some).ok_or_else(|| ApiError::bad(format!("{what} is an id"))),
        }
    };
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for r in list {
        let (customer, category, person) = (id_of(&r["customerId"], "customerId")?, id_of(&r["categoryId"], "categoryId")?, id_of(&r["userId"], "userId")?);
        let rate = r["rate"].as_f64().filter(|x| (0.0..=100_000.0).contains(x)).ok_or_else(|| ApiError::bad("a rate is a number per hour, 0 to 100000"))?;
        if !seen.insert((customer, category, person)) {
            return Err(ApiError::bad("two rates are for the same customer, category and person"));
        }
        rows.push((customer, category, person, (rate * 100.0).round() / 100.0));
    }
    let mut tx = state.db.begin().await?;
    sqlx::query("DELETE FROM time_rates WHERE workspace_id = $1").bind(user.workspace_id).execute(&mut *tx).await?;
    for (customer, category, person, rate) in &rows {
        // each of them belongs to this workspace
        let ok: bool = sqlx::query_scalar(
            "SELECT ($2::uuid IS NULL OR EXISTS (SELECT 1 FROM organizations WHERE id = $2 AND workspace_id = $1))
                AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM time_categories WHERE id = $3 AND workspace_id = $1))
                AND ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM users WHERE id = $4 AND workspace_id = $1))",
        )
        .bind(user.workspace_id)
        .bind(customer)
        .bind(category)
        .bind(person)
        .fetch_one(&mut *tx)
        .await?;
        if !ok {
            return Err(ApiError::bad("a rate names a customer, category or person that doesn't exist"));
        }
        sqlx::query(
            "INSERT INTO time_rates (id, workspace_id, customer_id, category_id, user_id, rate) VALUES ($1, $2, $3, $4, $5, $6::float8::numeric(12,2))",
        )
        .bind(new_id())
        .bind(user.workspace_id)
        .bind(customer)
        .bind(category)
        .bind(person)
        .bind(rate)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    audit(&state, user.workspace_id, Some(user.id), "time.rates", None, json!({"count": rows.len()})).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_specific_rate_counts() {
        let (acme, globex, dev, mia) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let r = |customer_id, category_id, user_id, cents| Rate { customer_id, category_id, user_id, cents };
        let rates = vec![
            r(None, None, None, 10_000),
            r(None, Some(dev), None, 12_000),
            r(None, None, Some(mia), 13_000),
            r(Some(acme), None, None, 15_000),
            r(Some(acme), Some(dev), None, 16_000),
        ];
        assert_eq!(rate_for(&rates, None, None, None), Some(10_000));
        assert_eq!(rate_for(&rates, Some(globex), Some(dev), None), Some(12_000));
        assert_eq!(rate_for(&rates, Some(globex), Some(dev), Some(mia)), Some(13_000), "a person before a category");
        assert_eq!(rate_for(&rates, Some(acme), None, Some(mia)), Some(15_000), "a customer before a person");
        assert_eq!(rate_for(&rates, Some(acme), Some(dev), Some(mia)), Some(16_000));
        assert_eq!(rate_for(&[], Some(acme), None, None), None);
        assert_eq!(amount(90, 12_000), 18_000);
        assert_eq!(amount(20, 10_000), 3_333);
    }
}
