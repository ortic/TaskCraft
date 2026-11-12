pub mod admin;
pub mod attachments;
pub mod auth;
pub mod boards;
pub mod comments;
pub mod import;
pub mod integrations;
pub mod issues;
pub mod moves;
pub mod projects;
pub mod reports;
pub mod templates;
pub mod users;
pub mod views;

use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, patch, post, put};
use serde_json::json;

use crate::app::AppState;

/// Content-Security-Policy for the app (attachments set a stricter sandbox of their own).
pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob: https:; font-src 'self' data:; \
                       connect-src 'self'; worker-src 'self' blob:; media-src 'self' blob:; frame-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'self'";

async fn health() -> impl IntoResponse {
    axum::Json(json!({"status": "ok"}))
}

async fn ready(State(state): State<AppState>) -> impl IntoResponse {
    let db = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.db).await.is_ok();
    if state.shutting_down.load(std::sync::atomic::Ordering::Relaxed) || !db {
        return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({"status": "unavailable", "database": db})));
    }
    (StatusCode::OK, axum::Json(json!({"status": "ok"})))
}

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/events", get(crate::events::stream))
        // setup, sign-in, account
        .route("/setup", get(auth::setup_status).post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/link", get(auth::link_info).post(auth::link_accept))
        .route("/me", get(auth::me))
        .route("/me/profile", put(auth::update_profile))
        .route("/me/password", put(auth::change_password))
        .route("/me/preferences", put(auth::put_preferences))
        .route("/me/tokens", get(auth::list_tokens).post(auth::create_token))
        .route("/me/tokens/{id}", delete(auth::delete_token))
        .route("/my-work", get(issues::my_work))
        .route("/timesheet", get(comments::timesheet))
        // people
        .route("/people", get(users::people))
        .route("/people/{id}", get(users::person))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", patch(users::update))
        .route("/users/{id}/link", post(users::reset_link))
        .route("/teams", get(users::teams).post(users::create_team))
        .route("/teams/{id}", patch(users::update_team).delete(users::delete_team))
        // projects and their configuration
        .route("/projects", get(projects::list).post(projects::create))
        .route("/projects/{key}", get(projects::get).patch(projects::update).delete(projects::delete))
        .route("/projects/{key}/archive", post(projects::archive))
        .route("/projects/{key}/star", put(projects::star))
        .route("/projects/{key}/members", get(projects::members).put(projects::put_members))
        .route("/projects/{key}/statuses", post(projects::create_status))
        .route("/projects/{key}/order/{what}", put(projects::reorder))
        .route("/projects/{key}/transitions", put(projects::put_transitions))
        .route("/projects/{key}/types", post(projects::create_type))
        .route("/projects/{key}/versions", post(projects::create_version))
        .route("/projects/{key}/components", post(projects::create_component))
        .route("/projects/{key}/fields", post(projects::create_field))
        .route("/projects/{key}/automation", get(projects::rules).post(projects::create_rule))
        .route("/projects/{key}/boards", post(boards::create))
        .route("/projects/{key}/backlog", get(boards::backlog))
        .route("/projects/{key}/sprints", get(boards::list_sprints).post(boards::create_sprint))
        .route("/projects/{key}/trash", get(issues::trash))
        .route("/projects/{key}/reports/{report}", get(reports::project_report))
        .route("/projects/{key}/wiki", get(integrations::project_wiki))
        .route("/projects/{key}/templates", get(templates::list).post(templates::create))
        .route("/projects/{key}/sla", get(templates::sla_queue))
        .route("/projects/{key}/import/preview", post(import::preview))
        .route("/projects/{key}/import", post(import::run))
        .route("/templates/{id}", patch(templates::update).delete(templates::delete))
        .route("/templates/{id}/create", post(templates::instantiate))
        .route("/statuses/{id}", patch(projects::update_status).delete(projects::delete_status))
        .route("/types/{id}", patch(projects::update_type).delete(projects::delete_type))
        .route("/versions/{id}", patch(projects::update_version).delete(projects::delete_version))
        .route("/versions/{id}/release", post(projects::release_version))
        .route("/versions/{id}/wiki-page", post(integrations::version_page))
        .route("/components/{id}", patch(projects::update_component).delete(projects::delete_component))
        .route("/fields/{id}", patch(projects::update_field).delete(projects::delete_field))
        .route("/automation/{id}", patch(projects::update_rule).delete(projects::delete_rule))
        .route("/labels", get(projects::labels))
        // boards and sprints
        .route("/boards/{id}", get(boards::get).put(boards::update).delete(boards::delete))
        .route("/sprints/{id}", patch(boards::update_sprint).delete(boards::delete_sprint))
        .route("/sprints/{id}/start", post(boards::start_sprint))
        .route("/sprints/{id}/complete", post(boards::complete_sprint))
        .route("/sprints/{id}/burndown", get(reports::burndown))
        .route("/sprints/{id}/wiki-page", post(integrations::sprint_page))
        // issues
        .route("/issues", post(issues::create))
        .route("/issues/bulk", post(issues::bulk))
        .route("/issues/{key}", get(issues::get).patch(issues::update).delete(issues::delete))
        .route("/issues/{key}/restore", post(issues::restore))
        .route("/issues/{key}/move", post(issues::move_issue))
        .route("/issues/{key}/clone", post(issues::clone_issue))
        .route("/issues/{key}/move-project", get(moves::preview).post(moves::move_to_project))
        .route("/issues/{key}/history", get(issues::history))
        .route("/issues/{key}/watch", put(issues::watch))
        .route("/issues/{key}/links", post(issues::add_link))
        .route("/issues/{key}/web-links", post(issues::add_web_link))
        .route("/issues/{key}/comments", get(comments::list).post(comments::create))
        .route("/issues/{key}/worklogs", get(comments::worklogs).post(comments::log_work))
        .route("/issues/{key}/attachments", post(attachments::upload))
        .route("/issues/{key}/wiki-pages", get(integrations::issue_pages).post(integrations::add_issue_page))
        .route("/issues/{key}/wiki-page", post(integrations::issue_page))
        .route("/links/{id}", delete(issues::delete_link))
        .route("/web-links/{id}", delete(issues::delete_web_link))
        .route("/comments/{id}", patch(comments::update).delete(comments::delete))
        .route("/comments/{id}/react", post(comments::react))
        .route("/worklogs/{id}", patch(moves::update_worklog).delete(comments::delete_worklog))
        .route("/attachments/{id}", get(attachments::download).delete(attachments::delete))
        // search, views, activity, notifications
        .route("/search", get(issues::search))
        .route("/search.csv", get(issues::search_csv))
        .route("/quick", get(issues::quick))
        .route("/query/validate", post(issues::validate))
        .route("/query/suggest", get(issues::suggest))
        .route("/activity", get(issues::activity))
        .route("/views", get(views::list).post(views::create))
        .route("/views/{id}", patch(views::update).delete(views::delete))
        .route("/views/{id}/star", put(views::star))
        .route("/notifications", get(views::notifications))
        .route("/notifications/read", post(views::read_notifications))
        // WikiCraft
        .route("/integrations/wikicraft", get(integrations::status))
        .route("/integrations/wikicraft/search", get(integrations::search))
        .route("/integrations/wikicraft/spaces", get(integrations::spaces))
        .route("/wiki-links", get(integrations::linked_issues))
        // administration
        .route("/admin/workspace", put(admin::put_workspace))
        .route("/admin/audit", get(admin::audit_log))
        .route("/admin/stats", get(admin::stats))
        .route("/admin/webhooks", get(admin::webhooks).post(admin::create_webhook))
        .route("/admin/webhooks/{id}", patch(admin::update_webhook).delete(admin::delete_webhook))
        .route("/admin/webhooks/{id}/test", post(admin::test_webhook))
}

pub fn router(state: AppState) -> Router {
    let max_upload = (state.config.max_upload_mb as usize).saturating_mul(1024 * 1024);
    let api = api().layer(DefaultBodyLimit::max(max_upload.max(2 * 1024 * 1024)));
    let spa = crate::web_assets::service(&state.config.static_dir);
    let hsts = state.config.base_url.starts_with("https://").then(|| {
        tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::STRICT_TRANSPORT_SECURITY,
            axum::http::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        )
    });
    let app = Router::new()
        .nest("/api/v1", api.clone())
        .nest("/api", api)
        .fallback_service(spa)
        .layer(tower_http::compression::CompressionLayer::new())
        .layer(tower_http::request_id::PropagateRequestIdLayer::x_request_id())
        .layer(tower_http::trace::TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
            let id = req.headers().get("x-request-id").and_then(|v| v.to_str().ok()).unwrap_or("-");
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path(), request_id = %id)
        }))
        .layer(tower_http::request_id::SetRequestIdLayer::x_request_id(tower_http::request_id::MakeRequestUuid))
        .layer(tower_http::catch_panic::CatchPanicLayer::new())
        .layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            axum::http::HeaderValue::from_static("nosniff"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::REFERRER_POLICY,
            axum::http::HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_FRAME_OPTIONS,
            axum::http::HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::CONTENT_SECURITY_POLICY,
            axum::http::HeaderValue::from_static(CSP),
        ));
    match hsts {
        Some(h) => app.layer(h),
        None => app,
    }
    .with_state(state)
}
