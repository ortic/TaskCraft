pub mod admin;
pub mod attachments;
pub mod auth;
pub mod boards;
pub mod comments;
pub mod dashboards;
pub mod email;
pub mod git;
pub mod import;
pub mod integrations;
pub mod issues;
pub mod mail_in;
pub mod moves;
pub mod openapi;
pub mod passkeys;
pub mod portal;
pub mod projects;
pub mod reports;
pub mod security;
pub mod templates;
pub mod time;
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
        .route("/metrics", get(crate::metrics::scrape))
        .route("/events", get(crate::events::stream))
        // this API's OpenAPI description (scripts/gen-openapi.py)
        .route("/openapi.yaml", get(openapi::yaml))
        .route("/openapi.json", get(openapi::json))
        // setup, sign-in, account
        .route("/setup", get(auth::setup_status).post(auth::setup))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/link", get(auth::link_info).post(auth::link_accept))
        .route("/auth/forgot", post(email::forgot))
        .route("/me", get(auth::me))
        .route("/me/profile", put(auth::update_profile))
        .route("/me/password", put(auth::change_password))
        .route("/me/preferences", put(auth::put_preferences))
        .route("/me/tokens", get(auth::list_tokens).post(auth::create_token))
        .route("/me/tokens/{id}", delete(auth::delete_token))
        .route("/me/export", get(users::export_me))
        .route("/me/photo", put(crate::photos::put).delete(crate::photos::delete_mine))
        .route("/users/{id}/photo", get(crate::photos::get).delete(crate::photos::delete))
        .route("/my-work", get(issues::my_work))
        .route("/timesheet", get(comments::timesheet))
        .route("/time-report", get(time::report))
        .route("/time-categories", get(time::categories).post(time::create_category))
        .route("/time-categories/order", put(time::order_categories))
        .route("/time-categories/{id}", patch(time::update_category).delete(time::delete_category))
        // people
        .route("/people", get(users::people))
        .route("/people/{id}", get(users::person))
        .route("/users", get(users::list).post(users::create))
        .route("/users/{id}", patch(users::update).delete(users::remove))
        .route("/users/{id}/export", get(users::export))
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
        .route("/projects/{key}/security-levels", post(projects::create_security_level))
        .route("/projects/{key}/automation", get(projects::rules).post(projects::create_rule))
        .route("/projects/{key}/boards", post(boards::create))
        .route("/projects/{key}/backlog", get(boards::backlog))
        .route("/projects/{key}/sprints", get(boards::list_sprints).post(boards::create_sprint))
        .route("/projects/{key}/trash", get(issues::trash))
        .route("/projects/{key}/export", get(crate::importer::taskcraft::download))
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
        .route("/security-levels/{id}", patch(projects::update_security_level).delete(projects::delete_security_level))
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
        .route("/issues/{key}/vote", put(issues::vote))
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
        .route("/search.xlsx", get(issues::search_xlsx))
        .route("/search/groups", get(dashboards::groups))
        .route("/dashboards", get(dashboards::list).post(dashboards::create))
        .route("/dashboards/{id}", get(dashboards::get).patch(dashboards::update).delete(dashboards::delete))
        .route("/portfolio", get(dashboards::portfolio))
        .route("/quick", get(issues::quick))
        .route("/query/validate", post(issues::validate))
        .route("/query/suggest", get(issues::suggest))
        .route("/activity", get(issues::activity))
        .route("/views", get(views::list).post(views::create))
        .route("/views/{id}", patch(views::update).delete(views::delete))
        .route("/views/{id}/star", put(views::star))
        .route("/views/{id}/subscription", get(views::subscription).put(views::subscribe).delete(views::unsubscribe))
        .route("/notifications", get(views::notifications))
        .route("/notifications/read", post(views::read_notifications))
        // WikiCraft
        .route("/integrations/wikicraft", get(integrations::status))
        .route("/integrations/wikicraft/search", get(integrations::search))
        .route("/integrations/wikicraft/spaces", get(integrations::spaces))
        .route("/wiki-links", get(integrations::linked_issues))
        // Git: webhook receivers, connected repositories, development links
        .route("/integrations/git/{provider}/{secret}", post(git::receive))
        .route("/projects/{key}/git", get(git::settings).post(git::create))
        .route("/projects/{key}/git/rules", put(git::put_rules))
        .route("/git-integrations/{id}", delete(git::delete))
        .route("/git-integrations/{id}/secret", post(git::rotate))
        .route("/issues/{key}/development", get(git::development))
        // e-mail in: messages from mail servers, a project's e-mail settings
        .route("/integrations/mail-in", post(mail_in::receive))
        .route("/projects/{key}/mail-in", get(mail_in::settings).put(mail_in::put_settings))
        .route("/projects/{key}/portal", get(portal::settings).put(portal::put_settings))
        // administration
        .route("/admin/workspace", put(admin::put_workspace))
        .route("/admin/workspace/logo", put(crate::features::put_logo).delete(crate::features::delete_logo))
        .route("/workspace/logo", get(crate::features::logo))
        .route("/admin/audit", get(admin::audit_log))
        .route("/admin/stats", get(admin::stats))
        .route("/admin/mail", get(email::status))
        .route("/admin/mail/test", post(email::test))
        .route("/admin/webhooks", get(admin::webhooks).post(admin::create_webhook))
        .route("/admin/webhooks/{id}", patch(admin::update_webhook).delete(admin::delete_webhook))
        .route("/admin/webhooks/{id}/test", post(admin::test_webhook))
        // migration from another tracker's XML site backup (upload, check, import in the background)
        .route("/admin/backup-imports", get(crate::importer::jobs::list).post(crate::importer::jobs::upload))
        .route("/admin/backup-imports/{id}", get(crate::importer::jobs::get).delete(crate::importer::jobs::discard))
        .route("/admin/backup-imports/{id}/run", post(crate::importer::jobs::start))
        // ... or from another tracker's API (connect, read and check in the background)
        .route("/admin/backup-imports/connect", post(crate::importer::jobs::connect))
        .route("/admin/backup-imports/browse", post(crate::importer::jobs::browse))
        // single sign-on: /auth/providers, /auth/sso/{id}/…, /me/sign-in
        .merge(crate::sso::routes())
        // two-factor authentication, passkeys, sessions
        .merge(security::routes())
}

/// The service desk portal (public: requesters sign in by e-mailed links) and request forms; off
/// when the workspace turns the portal off.
fn portal_routes() -> Router<AppState> {
    Router::new()
        .route("/portal", get(portal::info))
        .route("/portal/sign-in", post(portal::sign_in))
        .route("/portal/session", post(portal::start_session).delete(portal::end_session))
        .route("/portal/requests", get(portal::requests))
        .route("/portal/requests/{key}", get(portal::request))
        .route("/portal/requests/{key}/comments", post(portal::reply))
        .route("/portal/requests/{key}/resolve", post(portal::resolve))
        .route("/portal/attachments/{id}", get(portal::download))
        .route("/portal/forms/{path}", get(portal::form).post(portal::submit))
        .route("/portal/forms/{path}/suggest", get(portal::suggest))
}

pub fn router(state: AppState) -> Router {
    let max_upload = (state.config.max_upload_mb as usize).saturating_mul(1024 * 1024);
    // route_layer: counted under the matched route pattern (unknown paths aren't routes)
    let api = api()
        .merge(portal_routes().route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::features::portal_switch)))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), crate::metrics::track))
        .layer(DefaultBodyLimit::max(max_upload.max(2 * 1024 * 1024)));
    let spa = crate::web_assets::service(&state.config.static_dir);
    let hsts = state.config.base_url.starts_with("https://").then(|| {
        tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::STRICT_TRANSPORT_SECURITY,
            axum::http::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        )
    });
    // MCP server for AI assistants (authenticated per tool call with the caller's token / session)
    let mcp = crate::mcp::service(state.clone());
    let app = Router::new()
        .nest_service("/api/mcp", mcp.clone())
        .nest_service("/api/v1/mcp", mcp)
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
