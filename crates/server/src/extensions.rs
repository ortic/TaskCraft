//! Extensions and scripted automation: small sandboxed scripts ([Rhai](https://rhai.rs)) that run
//! inside the server.
//!
//! Two shapes:
//!
//! - A **script action** in an automation rule (project admins write it): it sees the issue the
//!   rule fired for and may change that issue — `set("priority", "high")`, `set_field("Customer
//!   tier", "Gold")`, `add_label`, `comment`. Nothing else.
//! - An **extension** a workspace admin installs: a manifest (name, version, the capabilities it
//!   asks for, its hooks) and a script with a function per hook — `action_<name>(issue, params)`
//!   for automation rules, `on_event(event, issue)`, `scheduled()` and `route(request)` under
//!   `/api/ext/<name>/…`. What it may do is what its manifest asked for and the admin installed:
//!   read or write issues (with the permissions of the person it runs as), HTTP calls to the
//!   hosts named, a little state of its own, secrets kept for it.
//!
//! The sandbox: no files, no modules, no `eval`; a limit on operations, time, call depth and the
//! size of strings, arrays and maps; at most ten HTTP calls a run, each to a checked address.
//! Runs are logged per extension (what it printed, what went wrong) and one switch turns an
//! extension off. The functions scripts can call are **extension API 1** (`apiVersion` in the
//! manifest): later TaskCraft versions keep serving it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use rhai::{Dynamic, Engine, EvalAltResult, Scope};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::access::Role;
use crate::app::{AppState, audit, new_id};
use crate::auth::{CurrentUser, User, require_admin};
use crate::error::{ApiError, ApiResult};
use crate::issues::{Actor, Summary};

/// The extension API this server speaks.
pub const API_VERSION: i64 = 1;
const MAX_SCRIPT: usize = 64 * 1024;
const MAX_OPERATIONS: u64 = 500_000;
const MAX_HTTP_CALLS: usize = 10;
const MAX_HTTP_BODY: usize = 1024 * 1024;
const MAX_STATE: usize = 64 * 1024;
const EVENTS: &[&str] = &["created", "status", "assigned", "commented", "changed"];

type ScriptResult<T> = Result<T, Box<EvalAltResult>>;

fn refuse<T>(msg: impl Into<String>) -> ScriptResult<T> {
    Err(msg.into().into())
}

// ---- what a run may do and what it leaves behind ------------------------------------------------

/// What an extension was granted.
#[derive(Clone, Default)]
struct Grant {
    id: Uuid,
    /// read (false) or write (true) issues, as `run_as`
    issues: Option<bool>,
    run_as: Option<User>,
    /// hosts (`erp.example.com`, `*.example.com`)
    http: Vec<String>,
    state: bool,
    secrets: Value,
}

/// Changes a script asks for on the issue it runs for; applied when it's done.
#[derive(Default, Debug)]
pub struct Effects {
    fields: Vec<(String, Value)>,
    custom: Vec<(String, Value)>,
    add_labels: Vec<String>,
    remove_labels: Vec<String>,
    comments: Vec<String>,
}

struct Run {
    state: AppState,
    handle: tokio::runtime::Handle,
    grant: Option<Grant>,
    workspace: Uuid,
    effects: Mutex<Effects>,
    logs: Mutex<Vec<String>>,
    http_calls: AtomicUsize,
    /// rule depth of whoever started the run (changes it makes go one deeper)
    depth: u8,
}

impl Run {
    fn grant(&self, what: &str) -> ScriptResult<&Grant> {
        match &self.grant {
            Some(g) => Ok(g),
            None => refuse(format!("{what} is for installed extensions, not for scripts in rules")),
        }
    }

    fn log(&self, line: &str) {
        let mut logs = self.logs.lock().unwrap();
        if logs.len() < 100 {
            logs.push(line.chars().take(2000).collect());
        }
    }

    /// The person its reads and writes are made as.
    fn actor(&self, write: bool) -> ScriptResult<User> {
        let g = self.grant("reading and changing other issues")?;
        match (g.issues, &g.run_as) {
            (None, _) => refuse("this extension wasn't granted access to issues (capabilities.issues)"),
            (Some(false), _) if write => refuse("this extension may read issues, not change them (capabilities.issues: write)"),
            (_, None) => refuse("nobody is set for this extension to run as"),
            (_, Some(u)) => Ok(u.clone()),
        }
    }
}

fn to_dynamic(v: &Value) -> Dynamic {
    rhai::serde::to_dynamic(v).unwrap_or(Dynamic::UNIT)
}

fn to_json(d: &Dynamic) -> Value {
    rhai::serde::from_dynamic::<Value>(d).unwrap_or(Value::Null)
}

fn api_err(e: ApiError) -> Box<EvalAltResult> {
    e.to_string().into()
}

/// Whether `host` is one of the hosts granted (`*.example.com` covers subdomains).
fn host_allowed(allowed: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allowed.iter().any(|a| {
        let a = a.to_ascii_lowercase();
        match a.strip_prefix("*.") {
            Some(domain) => host.ends_with(&format!(".{domain}")),
            None => a == host,
        }
    })
}

async fn http_call(state: &AppState, method: Method, url: &str, body: Option<Value>, headers: Option<Value>) -> Result<Value, String> {
    let u = crate::webhooks::validate_url(url)?;
    let client = crate::webhooks::pinned_client(state, &u, 10).await?;
    let mut req = client.request(method, u);
    for (k, v) in headers.as_ref().and_then(Value::as_object).into_iter().flatten() {
        if let Some(v) = v.as_str() {
            req = req.header(k.as_str(), v);
        }
    }
    if let Some(b) = body {
        req = match b {
            Value::String(s) => req.body(s),
            other => req.json(&other),
        };
    }
    let res = req.send().await.map_err(|e| format!("the request failed: {}", e.without_url()))?;
    let status = res.status().as_u16();
    let bytes = res.bytes().await.map_err(|e| e.without_url().to_string())?;
    if bytes.len() > MAX_HTTP_BODY {
        return Err("the answer is larger than 1 MB".into());
    }
    let text = String::from_utf8_lossy(&bytes).to_string();
    let parsed = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    Ok(json!({"status": status, "ok": (200..300).contains(&status), "body": parsed}))
}

/// The script engine with the extension API registered for one run.
fn engine(run: Arc<Run>, deadline: Instant) -> Engine {
    let mut e = Engine::new();
    e.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    e.disable_symbol("eval");
    e.set_max_operations(MAX_OPERATIONS);
    e.set_max_call_levels(32);
    e.set_max_expr_depths(64, 64);
    e.set_max_string_size(256 * 1024);
    e.set_max_array_size(10_000);
    e.set_max_map_size(2_000);
    e.on_progress(move |_| if Instant::now() > deadline { Some("it ran out of time".into()) } else { None });
    let r = run.clone();
    e.on_print(move |s| r.log(s));
    let r = run.clone();
    e.on_debug(move |s, _, _| r.log(s));
    let r = run.clone();
    e.register_fn("log", move |s: Dynamic| r.log(&if s.is_string() { s.into_string().unwrap_or_default() } else { to_json(&s).to_string() }));

    // the issue the run is for
    let r = run.clone();
    e.register_fn("set", move |field: &str, value: Dynamic| r.effects.lock().unwrap().fields.push((field.to_string(), to_json(&value))));
    let r = run.clone();
    e.register_fn("set_field", move |name: &str, value: Dynamic| r.effects.lock().unwrap().custom.push((name.to_string(), to_json(&value))));
    let r = run.clone();
    e.register_fn("add_label", move |l: &str| r.effects.lock().unwrap().add_labels.push(l.to_string()));
    let r = run.clone();
    e.register_fn("remove_label", move |l: &str| r.effects.lock().unwrap().remove_labels.push(l.to_string()));
    let r = run.clone();
    e.register_fn("comment", move |text: &str| r.effects.lock().unwrap().comments.push(text.to_string()));

    // issues, as the person the extension runs as
    let r = run.clone();
    e.register_fn("search", move |tql: &str| -> ScriptResult<Dynamic> {
        let user = r.actor(false)?;
        let found = r.handle.block_on(crate::routes::issues::run_search(&r.state, &user, tql, None, 100, 0, "i.updated_at DESC")).map_err(api_err)?;
        Ok(to_dynamic(&json!(found.issues)))
    });
    let r = run.clone();
    e.register_fn("get_issue", move |key: &str| -> ScriptResult<Dynamic> {
        let user = r.actor(false)?;
        let v = r.handle.block_on(async {
            let (id, _, _) = crate::routes::issues::issue_for(&r.state, &user, key, Role::Viewer).await?;
            Ok::<_, ApiError>(json!(crate::issues::summary(&r.state, crate::issues::load(&r.state, id).await?)))
        });
        match v {
            Ok(v) => Ok(to_dynamic(&v)),
            Err(ApiError::NotFound(_)) => Ok(Dynamic::UNIT),
            Err(e) => Err(api_err(e)),
        }
    });
    let r = run.clone();
    e.register_fn("update_issue", move |key: &str, changes: Dynamic| -> ScriptResult<Dynamic> {
        let user = r.actor(true)?;
        let Value::Object(patch) = to_json(&changes) else { return refuse("update_issue(key, #{field: value, …})") };
        let v = r
            .handle
            .block_on(async {
                let (id, _, _) = crate::routes::issues::issue_for(&r.state, &user, key, Role::Member).await?;
                let actor = Actor { user: Some(user.id), workspace: user.workspace_id, depth: r.depth + 1 };
                let (s, _) = crate::issues::update(&r.state, &actor, id, &patch).await?;
                Ok::<_, ApiError>(json!(s))
            })
            .map_err(api_err)?;
        Ok(to_dynamic(&v))
    });
    let r = run.clone();
    e.register_fn("add_comment", move |key: &str, text: &str| -> ScriptResult<()> {
        let user = r.actor(true)?;
        r.handle
            .block_on(async {
                let (id, _, _) = crate::routes::issues::issue_for(&r.state, &user, key, Role::Member).await?;
                let actor = Actor { user: Some(user.id), workspace: user.workspace_id, depth: r.depth + 1 };
                crate::routes::comments::add(&r.state, &actor, id, crate::doc::from_markdown(text)).await
            })
            .map(|_| ())
            .map_err(api_err)
    });

    // HTTP to the hosts granted
    for (name, method, with_body) in [("http_get", Method::GET, false), ("http_post", Method::POST, true), ("http_put", Method::PUT, true)] {
        let call = {
            let r = run.clone();
            move |url: &str, body: Option<Value>, headers: Option<Value>| -> ScriptResult<Dynamic> {
                let g = r.grant("HTTP calls")?;
                let host = reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
                if !host_allowed(&g.http, &host) {
                    return refuse(format!("this extension wasn't granted HTTP calls to {host} (capabilities.http)"));
                }
                if r.http_calls.fetch_add(1, Ordering::SeqCst) >= MAX_HTTP_CALLS {
                    return refuse(format!("more than {MAX_HTTP_CALLS} HTTP calls in one run"));
                }
                r.handle.block_on(http_call(&r.state, method.clone(), url, body, headers)).map(|v| to_dynamic(&v)).map_err(|e| e.into())
            }
        };
        if with_body {
            let c = call.clone();
            e.register_fn(name, move |url: &str, body: Dynamic| c(url, Some(to_json(&body)), None));
            let c = call.clone();
            e.register_fn(name, move |url: &str, body: Dynamic, headers: Dynamic| c(url, Some(to_json(&body)), Some(to_json(&headers))));
        } else {
            let c = call.clone();
            e.register_fn(name, move |url: &str| c(url, None, None));
            let c = call.clone();
            e.register_fn(name, move |url: &str, headers: Dynamic| c(url, None, Some(to_json(&headers))));
        }
    }

    // state and secrets
    let r = run.clone();
    e.register_fn("secret", move |name: &str| -> ScriptResult<Dynamic> {
        let g = r.grant("secrets")?;
        match g.secrets[name].as_str() {
            Some(s) => Ok(s.to_string().into()),
            None => refuse(format!("the secret {name} isn't set (or not asked for in capabilities.secrets)")),
        }
    });
    let r = run.clone();
    e.register_fn("state_get", move |key: &str| -> ScriptResult<Dynamic> {
        let g = r.grant("state")?;
        if !g.state {
            return refuse("this extension wasn't granted state (capabilities.state)");
        }
        let v: Option<Value> = r
            .handle
            .block_on(sqlx::query_scalar("SELECT state->$2 FROM extensions WHERE id = $1").bind(g.id).bind(key).fetch_one(&r.state.db))
            .map_err(|e| e.to_string())?;
        Ok(to_dynamic(&v.unwrap_or(Value::Null)))
    });
    let r = run.clone();
    e.register_fn("state_set", move |key: &str, value: Dynamic| -> ScriptResult<()> {
        let g = r.grant("state")?;
        if !g.state {
            return refuse("this extension wasn't granted state (capabilities.state)");
        }
        let value = to_json(&value);
        r.handle
            .block_on(async {
                let mut tx = r.state.db.begin().await.map_err(|e| e.to_string())?;
                let mut all: Value = sqlx::query_scalar("SELECT state FROM extensions WHERE id = $1 FOR UPDATE")
                    .bind(g.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
                if !all.is_object() {
                    all = json!({});
                }
                if value.is_null() {
                    all.as_object_mut().unwrap().remove(key);
                } else {
                    all[key] = value;
                }
                if all.to_string().len() > MAX_STATE {
                    return Err("an extension's state holds up to 64 KB".to_string());
                }
                sqlx::query("UPDATE extensions SET state = $2 WHERE id = $1").bind(g.id).bind(&all).execute(&mut *tx).await.map_err(|e| e.to_string())?;
                tx.commit().await.map_err(|e| e.to_string())
            })
            .map_err(|e| e.into())
    });
    let ws = run.workspace;
    e.register_fn("workspace_id", move || ws.to_string());
    e
}

/// What to run of a script.
enum Call {
    /// the whole script, with `issue` in scope (a rule's script action)
    Inline(Value),
    /// one of its functions
    Function(String, Vec<Value>),
}

fn script_error(e: &EvalAltResult) -> String {
    match e {
        EvalAltResult::ErrorTooManyOperations(_) => "it did too much work (an endless loop?)".into(),
        EvalAltResult::ErrorTerminated(why, _) => why.clone().into_string().unwrap_or_else(|_| "it was stopped".into()),
        other => other.to_string(),
    }
}

/// Runs a script off the async threads; its result as JSON.
async fn execute(run: Arc<Run>, script: String, call: Call, seconds: u64) -> Result<Value, String> {
    let r = run.clone();
    tokio::task::spawn_blocking(move || {
        let engine = engine(r, Instant::now() + Duration::from_secs(seconds));
        let ast = engine.compile(&script).map_err(|e| format!("the script doesn't parse: {e}"))?;
        let out = match call {
            Call::Inline(issue) => {
                let mut scope = Scope::new();
                scope.push_constant("issue", to_dynamic(&issue));
                engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast)
            }
            Call::Function(name, args) => {
                let args: Vec<Dynamic> = args.iter().map(to_dynamic).collect();
                engine.call_fn::<Dynamic>(&mut Scope::new(), &ast, &name, args)
            }
        };
        out.map(|d| to_json(&d)).map_err(|e| script_error(&e))
    })
    .await
    .map_err(|_| "the script crashed".to_string())?
}

/// Checks that a script parses; the names of its functions.
fn functions(script: &str) -> Result<Vec<String>, String> {
    if script.len() > MAX_SCRIPT {
        return Err("a script is at most 64 KB".into());
    }
    let mut e = Engine::new();
    e.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());
    e.disable_symbol("eval");
    e.set_max_expr_depths(64, 64);
    let ast = e.compile(script).map_err(|e| format!("the script doesn't parse: {e}"))?;
    Ok(ast.iter_functions().map(|f| f.name.to_string()).collect())
}

/// Checks a rule's script action when the rule is saved.
pub fn check_inline(script: &str) -> ApiResult<()> {
    if script.trim().is_empty() {
        return Err(ApiError::bad("the script is empty"));
    }
    functions(script).map(|_| ()).map_err(ApiError::BadRequest)
}

// ---- effects on the issue a run is for ----------------------------------------------------------

/// Applies what a script asked for on its issue, as Automation.
async fn apply(state: &AppState, bot: &Actor, s: &Summary, fx: Effects) -> ApiResult<()> {
    let mut patch = Map::new();
    for (field, value) in fx.fields {
        match field.as_str() {
            "title" | "priority" | "dueDate" | "startDate" | "storyPoints" | "estimateMinutes" | "remainingMinutes" => {
                patch.insert(field, value);
            }
            "description" => {
                patch.insert("description".into(), crate::doc::from_markdown(value.as_str().unwrap_or("")));
            }
            "status" => {
                let name = value.as_str().unwrap_or("");
                let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM statuses WHERE project_id = $1 AND lower(name) = lower($2)")
                    .bind(s.project_id)
                    .bind(name)
                    .fetch_optional(&state.db)
                    .await?;
                patch.insert("statusId".into(), json!(id.ok_or_else(|| ApiError::bad(format!("set(\"status\"): the project has no status {name}")))?));
            }
            "assignee" => {
                let who = match value.as_str().map(str::trim).filter(|w| !w.is_empty()) {
                    None => Value::Null,
                    Some(w) => {
                        let id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE workspace_id = $1 AND status = 'active' AND (lower(email) = lower($2) OR lower(name) = lower($2)) LIMIT 1")
                            .bind(bot.workspace)
                            .bind(w)
                            .fetch_optional(&state.db)
                            .await?;
                        json!(id.ok_or_else(|| ApiError::bad(format!("set(\"assignee\"): nobody is called {w}")))?)
                    }
                };
                patch.insert("assigneeId".into(), who);
            }
            other => {
                return Err(ApiError::bad(format!(
                    "set(\"{other}\", …): scripts set title, description, priority, status, assignee, dueDate, startDate, storyPoints, estimateMinutes or remainingMinutes; custom fields with set_field"
                )));
            }
        }
    }
    if !fx.custom.is_empty() {
        let mut custom = Map::new();
        for (name, value) in fx.custom {
            let def: Option<(Uuid, String, Value)> =
                sqlx::query_as("SELECT id, kind, options FROM custom_fields WHERE project_id = $1 AND lower(name) = lower($2)")
                    .bind(s.project_id)
                    .bind(&name)
                    .fetch_optional(&state.db)
                    .await?;
            let (id, kind, options) = def.ok_or_else(|| ApiError::bad(format!("set_field: the project has no field {name}")))?;
            // options by their label
            let by_label = |v: &Value| -> Value {
                let found = v.as_str().and_then(|l| options.as_array()?.iter().find(|o| o["label"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(l))));
                found.map(|o| o["id"].clone()).unwrap_or_else(|| v.clone())
            };
            let value = match kind.as_str() {
                "select" => by_label(&value),
                "multiselect" => json!(value.as_array().map(|a| a.iter().map(by_label).collect::<Vec<_>>()).unwrap_or_default()),
                _ => value,
            };
            custom.insert(id.to_string(), value);
        }
        patch.insert("custom".into(), Value::Object(custom));
    }
    if !fx.add_labels.is_empty() || !fx.remove_labels.is_empty() {
        let mut labels = s.labels.clone();
        for l in fx.add_labels.iter().map(|l| crate::query::normalize_label(l)).filter(|l| !l.is_empty()) {
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
        let gone: Vec<String> = fx.remove_labels.iter().map(|l| crate::query::normalize_label(l)).collect();
        labels.retain(|l| !gone.contains(l));
        if labels != s.labels {
            patch.insert("labels".into(), json!(labels));
        }
    }
    if !patch.is_empty() {
        crate::issues::update(state, bot, s.id, &patch).await?;
    }
    for text in fx.comments.into_iter().filter(|t| !t.trim().is_empty()).take(5) {
        crate::routes::comments::add(state, bot, s.id, crate::doc::from_markdown(&text)).await?;
    }
    Ok(())
}

fn new_run(state: &AppState, workspace: Uuid, grant: Option<Grant>, depth: u8) -> Arc<Run> {
    Arc::new(Run {
        state: state.clone(),
        handle: tokio::runtime::Handle::current(),
        grant,
        workspace,
        effects: Mutex::default(),
        logs: Mutex::default(),
        http_calls: AtomicUsize::new(0),
        depth,
    })
}

/// A rule's script action: the script with the issue in scope; its changes are applied.
pub async fn run_inline(state: &AppState, bot: &Actor, s: &Summary, script: &str) -> ApiResult<()> {
    let run = new_run(state, bot.workspace, None, bot.depth);
    execute(run.clone(), script.to_string(), Call::Inline(json!(s)), 2).await.map_err(|e| ApiError::bad(format!("script: {e}")))?;
    let fx = std::mem::take(&mut *run.effects.lock().unwrap());
    apply(state, bot, s, fx).await
}

// ---- installed extensions ---------------------------------------------------------------------------

#[derive(sqlx::FromRow, Clone)]
struct Ext {
    id: Uuid,
    workspace_id: Uuid,
    name: String,
    manifest: Value,
    script: String,
    run_as: Option<Uuid>,
    secrets: Value,
}

const EXT_SQL: &str = "SELECT id, workspace_id, name, manifest, script, run_as, secrets FROM extensions";

async fn grant_of(state: &AppState, x: &Ext) -> Grant {
    let caps = &x.manifest["capabilities"];
    let run_as: Option<User> = match x.run_as {
        Some(u) => sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {} FROM users WHERE id = $1 AND status = 'active'", crate::auth::USER_COLUMNS)))
            .bind(u)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    Grant {
        id: x.id,
        issues: match caps["issues"].as_str() {
            Some("write") => Some(true),
            Some("read") => Some(false),
            _ => None,
        },
        run_as,
        http: caps["http"].as_array().into_iter().flatten().filter_map(|h| h.as_str().map(str::to_string)).collect(),
        state: caps["state"].as_bool().unwrap_or(false),
        // only the secrets it asked for
        secrets: Value::Object(
            caps["secrets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|n| x.secrets.get(n).map(|v| (n.to_string(), v.clone())))
                .collect(),
        ),
    }
}

async fn write_log(state: &AppState, ext: Uuid, level: &str, hook: &str, message: &str) {
    let _ = sqlx::query("INSERT INTO extension_logs (id, extension_id, level, hook, message) VALUES ($1, $2, $3, $4, $5)")
        .bind(new_id())
        .bind(ext)
        .bind(level)
        .bind(hook)
        .bind(message.chars().take(2000).collect::<String>())
        .execute(&state.db)
        .await;
}

/// Runs one hook of an extension: logs what it printed and what went wrong, counts the run.
async fn run_hook(state: &AppState, x: &Ext, hook: &str, function: &str, args: Vec<Value>, depth: u8) -> Result<(Value, Effects), String> {
    let run = new_run(state, x.workspace_id, Some(grant_of(state, x).await), depth);
    let result = execute(run.clone(), x.script.clone(), Call::Function(function.to_string(), args), 15).await;
    let logs = std::mem::take(&mut *run.logs.lock().unwrap());
    for line in &logs {
        write_log(state, x.id, "info", hook, line).await;
    }
    match &result {
        Ok(_) => {
            let _ = sqlx::query("UPDATE extensions SET runs = runs + 1, last_run_at = now() WHERE id = $1").bind(x.id).execute(&state.db).await;
        }
        Err(e) => {
            write_log(state, x.id, "error", hook, e).await;
            let _ = sqlx::query("UPDATE extensions SET runs = runs + 1, last_run_at = now(), last_error = $2, last_error_at = now() WHERE id = $1")
                .bind(x.id)
                .bind(format!("{hook}: {e}").chars().take(500).collect::<String>())
                .execute(&state.db)
                .await;
        }
    }
    // keep the latest 500 lines per extension
    let _ = sqlx::query(
        "DELETE FROM extension_logs WHERE extension_id = $1 AND id IN (SELECT id FROM extension_logs WHERE extension_id = $1 ORDER BY at DESC OFFSET 500)",
    )
    .bind(x.id)
    .execute(&state.db)
    .await;
    let fx = std::mem::take(&mut *run.effects.lock().unwrap());
    result.map(|v| (v, fx))
}

/// An extension's action in an automation rule: `{"action": "extension", "extension": name,
/// "name": action, "params": {…}}`.
pub async fn run_action(state: &AppState, bot: &Actor, s: &Summary, action: &Value) -> ApiResult<()> {
    let (ext, name) = (action["extension"].as_str().unwrap_or(""), action["name"].as_str().unwrap_or(""));
    let x: Option<Ext> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{EXT_SQL} WHERE workspace_id = $1 AND name = $2 AND enabled")))
        .bind(bot.workspace)
        .bind(ext)
        .fetch_optional(&state.db)
        .await?;
    let x = x.ok_or_else(|| ApiError::bad(format!("the extension {ext} isn't installed (or is switched off)")))?;
    if !x.manifest["actions"].as_array().is_some_and(|a| a.iter().any(|a| a["name"] == name)) {
        return Err(ApiError::bad(format!("the extension {ext} has no action {name}")));
    }
    let (_, fx) = run_hook(state, &x, &format!("action:{name}"), &format!("action_{name}"), vec![json!(s), action["params"].clone()], bot.depth)
        .await
        .map_err(|e| ApiError::bad(format!("{ext}: {e}")))?;
    apply(state, bot, s, fx).await
}

/// After an issue changed (`created`, `status`, `assigned`, `commented`, else `changed`): the
/// extensions listening hear of it, in the background.
pub fn on_event(state: &AppState, actor: &Actor, s: &Summary, kind: &'static str) {
    if actor.depth >= 3 || actor.is_quiet() {
        return;
    }
    let (state, s, workspace, depth) = (state.clone(), s.clone(), actor.workspace, actor.depth);
    tokio::spawn(async move {
        let listening: Vec<Ext> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{EXT_SQL} WHERE workspace_id = $1 AND enabled AND (manifest->'events' ? $2 OR manifest->'events' ? 'changed') ORDER BY created_at"
        )))
        .bind(workspace)
        .bind(kind)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
        for x in listening {
            let bot = Actor { user: None, workspace, depth: depth + 1 };
            match run_hook(&state, &x, &format!("event:{kind}"), "on_event", vec![json!(kind), json!(s)], depth).await {
                Ok((_, fx)) => {
                    if let Err(e) = apply(&state, &bot, &s, fx).await {
                        write_log(&state, x.id, "error", &format!("event:{kind}"), &e.to_string()).await;
                    }
                }
                Err(e) => tracing::debug!(extension = %x.name, error = %e, "extension event handler"),
            }
        }
    });
}

/// Once a minute: the scheduled jobs that are due (each on one replica).
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    for _ in 0..20 {
        let mut tx = state.db.begin().await?;
        let due: Option<Ext> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "{EXT_SQL} WHERE enabled AND next_run_at <= now() ORDER BY next_run_at LIMIT 1 FOR UPDATE SKIP LOCKED"
        )))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(x) = due else { return Ok(()) };
        let next = crate::schedule::parse(&x.manifest["schedule"]).map(|s| crate::schedule::next_after(&s, Utc::now())).ok();
        sqlx::query("UPDATE extensions SET next_run_at = $2 WHERE id = $1").bind(x.id).bind(next).execute(&mut *tx).await?;
        tx.commit().await?;
        let _ = run_hook(state, &x, "scheduled", "scheduled", vec![], 1).await;
    }
    Ok(())
}

/// `ANY /ext/{name}/{*path}`: an extension's own route, for signed-in people (read-only tokens:
/// GET only). The script's `route(request)` answers with a value (sent as JSON) or
/// `#{status: 404, body: …}`.
pub async fn route(
    State(state): State<AppState>,
    CurrentUser(user): CurrentUser,
    method: Method,
    Path((name, path)): Path<(String, String)>,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> ApiResult<Response> {
    let x: Option<Ext> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{EXT_SQL} WHERE workspace_id = $1 AND name = $2 AND enabled")))
        .bind(user.workspace_id)
        .bind(&name)
        .fetch_optional(&state.db)
        .await?;
    let x = x.ok_or(ApiError::NotFound("extension"))?;
    let path = path.trim_matches('/').to_string();
    let known = x.manifest["routes"]
        .as_array()
        .is_some_and(|r| r.iter().any(|r| r["path"].as_str() == Some(&path) && r["method"].as_str().unwrap_or("GET").eq_ignore_ascii_case(method.as_str())));
    if !known {
        return Err(ApiError::NotFound("route"));
    }
    let query: Map<String, Value> =
        query.as_deref().unwrap_or("").split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (decode(k), json!(decode(v)))).collect();
    let body: Value =
        if body.is_empty() { Value::Null } else { serde_json::from_slice(&body).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string())) };
    let request = json!({
        "method": method.as_str(), "path": path, "query": query, "body": body,
        "user": {"id": user.id, "name": user.name, "email": user.email, "role": user.role},
    });
    let hook = format!("route:{} {path}", method.as_str());
    let (v, _) = run_hook(&state, &x, &hook, "route", vec![request], 1).await.map_err(|e| ApiError::bad(format!("{name}: {e}")))?;
    let status = v["status"].as_u64().filter(|_| v.get("body").is_some()).and_then(|s| StatusCode::from_u16(s as u16).ok());
    Ok(match status {
        Some(s) => (s, Json(v["body"].clone())).into_response(),
        None => Json(v).into_response(),
    })
}

fn decode(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(h) = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(h);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

// ---- administration ---------------------------------------------------------------------------------

fn word(s: &str, max: usize, extra: &[char]) -> bool {
    !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

/// Checks and tidies a manifest against its script: (manifest, name, title, version, description).
fn check_manifest(v: &Value, script: &str) -> ApiResult<Value> {
    let bad = |m: String| ApiError::bad(m);
    let o = v.as_object().ok_or_else(|| bad("the manifest is a JSON object".into()))?;
    if let Some(k) = o.keys().find(|k| {
        !matches!(k.as_str(), "name" | "title" | "version" | "description" | "apiVersion" | "capabilities" | "actions" | "events" | "schedule" | "routes")
    }) {
        return Err(bad(format!("the manifest has an unknown key {k}")));
    }
    let name = v["name"].as_str().unwrap_or("");
    if !word(name, 40, &['-']) || name.len() < 2 {
        return Err(bad("name: 2 to 40 lower-case letters, digits and hyphens (it's the extension's address: /api/ext/<name>/)".into()));
    }
    let api = v["apiVersion"].as_i64().unwrap_or(API_VERSION);
    if api != API_VERSION {
        return Err(bad(format!("this TaskCraft speaks extension API {API_VERSION}; the extension asks for {api}")));
    }
    let title: String = v["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(name).chars().take(80).collect();
    let version: String = v["version"].as_str().unwrap_or("0.1.0").chars().take(20).collect();
    let caps = &v["capabilities"];
    if !(caps.is_null() || caps.is_object()) {
        return Err(bad("capabilities is an object".into()));
    }
    if let Some(k) = caps.as_object().into_iter().flatten().map(|(k, _)| k).find(|k| !matches!(k.as_str(), "issues" | "http" | "state" | "secrets")) {
        return Err(bad(format!("unknown capability {k} (there are: issues, http, state, secrets)")));
    }
    let mut out_caps = Map::new();
    match caps["issues"].as_str() {
        Some(i @ ("read" | "write")) => {
            out_caps.insert("issues".into(), json!(i));
        }
        None if caps["issues"].is_null() => {}
        _ => return Err(bad("capabilities.issues is read or write".into())),
    }
    let hosts: Vec<&str> = caps["http"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if hosts.len() > 20
        || hosts.iter().any(|h| h.is_empty() || h.len() > 200 || h.contains('/') || h.contains(' ') || *h == "*" || h.trim_start_matches("*.").contains('*'))
    {
        return Err(bad("capabilities.http lists up to 20 host names (erp.example.com, *.example.com)".into()));
    }
    if !hosts.is_empty() {
        out_caps.insert("http".into(), json!(hosts));
    }
    if caps["state"].as_bool() == Some(true) {
        out_caps.insert("state".into(), json!(true));
    }
    let secrets: Vec<&str> = caps["secrets"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if secrets.len() > 20 || secrets.iter().any(|s| s.is_empty() || s.len() > 60 || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')) {
        return Err(bad("capabilities.secrets lists up to 20 names of letters, digits and _".into()));
    }
    if !secrets.is_empty() {
        out_caps.insert("secrets".into(), json!(secrets));
    }
    let fns = functions(script).map_err(ApiError::BadRequest)?;
    let need = |f: &str, why: &str| if fns.iter().any(|x| x == f) { Ok(()) } else { Err(ApiError::bad(format!("the script needs a function {f} ({why})"))) };
    let mut out = json!({"name": name, "title": title, "version": version, "apiVersion": api, "capabilities": out_caps});
    if let Some(d) = v["description"].as_str() {
        out["description"] = json!(d.chars().take(500).collect::<String>());
    }
    let mut actions = Vec::new();
    for a in v["actions"].as_array().into_iter().flatten() {
        let n = a["name"].as_str().unwrap_or("");
        if !word(n, 40, &['_']) {
            return Err(bad("an action's name: up to 40 lower-case letters, digits and _".into()));
        }
        need(&format!("action_{n}"), "for the action")?;
        actions
            .push(json!({"name": n, "label": a["label"].as_str().map(str::trim).filter(|l| !l.is_empty()).unwrap_or(n).chars().take(80).collect::<String>()}));
    }
    if actions.len() > 20 {
        return Err(bad("up to 20 actions".into()));
    }
    if !actions.is_empty() {
        out["actions"] = json!(actions);
    }
    let events: Vec<&str> = v["events"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    if let Some(e) = events.iter().find(|e| !EVENTS.contains(e)) {
        return Err(bad(format!("unknown event {e} (there are: {})", EVENTS.join(", "))));
    }
    if !events.is_empty() {
        need("on_event", "for its events")?;
        out["events"] = json!(events);
    }
    if !v["schedule"].is_null() {
        crate::schedule::parse(&v["schedule"]).map_err(|e| bad(format!("schedule: {e}")))?;
        need("scheduled", "for its schedule")?;
        out["schedule"] = v["schedule"].clone();
    }
    let mut routes = Vec::new();
    for r in v["routes"].as_array().into_iter().flatten() {
        let method = r["method"].as_str().unwrap_or("GET").to_ascii_uppercase();
        let path = r["path"].as_str().unwrap_or("").trim_matches('/');
        if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "DELETE") || !word(path, 80, &['-', '_', '/']) {
            return Err(bad("a route has a method (GET, POST, PUT, DELETE) and a path of lower-case letters, digits, - _ and /".into()));
        }
        routes.push(json!({"method": method, "path": path}));
    }
    if routes.len() > 20 {
        return Err(bad("up to 20 routes".into()));
    }
    if !routes.is_empty() {
        need("route", "for its routes")?;
        out["routes"] = json!(routes);
    }
    Ok(out)
}

fn next_run(manifest: &Value, enabled: bool) -> Option<DateTime<Utc>> {
    if !enabled || manifest["schedule"].is_null() {
        return None;
    }
    crate::schedule::parse(&manifest["schedule"]).ok().map(|s| crate::schedule::next_after(&s, Utc::now()))
}

#[derive(sqlx::FromRow)]
struct AdminRow {
    id: Uuid,
    name: String,
    title: String,
    version: String,
    description: Option<String>,
    manifest: Value,
    script: String,
    enabled: bool,
    run_as: Option<Uuid>,
    run_as_name: Option<String>,
    secrets: Value,
    next_run_at: Option<DateTime<Utc>>,
    last_run_at: Option<DateTime<Utc>>,
    runs: i32,
    last_error: Option<String>,
    last_error_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
}

const ADMIN_SQL: &str = "SELECT x.id, x.name, x.title, x.version, x.description, x.manifest, x.script, x.enabled, x.run_as, u.name AS run_as_name, x.secrets, x.next_run_at, x.last_run_at, x.runs,
                                x.last_error, x.last_error_at, x.updated_at FROM extensions x LEFT JOIN users u ON u.id = x.run_as";

fn admin_json(r: &AdminRow) -> Value {
    let wanted: Vec<&str> = r.manifest["capabilities"]["secrets"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    json!({
        "id": r.id, "name": r.name, "title": r.title, "version": r.version, "description": r.description, "manifest": r.manifest, "script": r.script,
        "enabled": r.enabled, "runAs": r.run_as.map(|id| json!({"id": id, "name": r.run_as_name})),
        // which secrets it asks for and whether each is set (never the values)
        "secrets": wanted.iter().map(|n| json!({"name": n, "set": r.secrets.get(*n).is_some_and(|v| v.as_str().is_some_and(|s| !s.is_empty()))})).collect::<Vec<_>>(),
        "nextRunAt": r.next_run_at, "lastRunAt": r.last_run_at, "runs": r.runs, "lastError": r.last_error, "lastErrorAt": r.last_error_at, "updatedAt": r.updated_at,
    })
}

async fn admin_row(state: &AppState, workspace: Uuid, id: Uuid) -> ApiResult<AdminRow> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!("{ADMIN_SQL} WHERE x.id = $1 AND x.workspace_id = $2")))
        .bind(id)
        .bind(workspace)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound("extension"))
}

/// `GET /admin/extensions`: the installed extensions (workspace admins).
pub async fn list(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let rows: Vec<AdminRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{ADMIN_SQL} WHERE x.workspace_id = $1 ORDER BY lower(x.title)")))
        .bind(user.workspace_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(json!({"apiVersion": API_VERSION, "extensions": rows.iter().map(admin_json).collect::<Vec<_>>()})))
}

async fn run_as_of(state: &AppState, user: &User, v: &Value) -> ApiResult<Option<Uuid>> {
    match v {
        Value::Null => Ok(None),
        v => {
            let id: Uuid = v.as_str().and_then(|s| s.parse().ok()).ok_or_else(|| ApiError::bad("runAs is a person's id"))?;
            let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND workspace_id = $2 AND status = 'active')")
                .bind(id)
                .bind(user.workspace_id)
                .fetch_one(&state.db)
                .await?;
            if !ok {
                return Err(ApiError::bad("runAs isn't an active person of this workspace"));
            }
            Ok(Some(id))
        }
    }
}

/// `POST /admin/extensions` `{manifest, script, runAs?}`: installs an extension with the
/// capabilities its manifest asks for.
pub async fn install(State(state): State<AppState>, CurrentUser(user): CurrentUser, Json(req): Json<Value>) -> ApiResult<(StatusCode, Json<Value>)> {
    require_admin(&user)?;
    let script = req["script"].as_str().unwrap_or("");
    let manifest = check_manifest(&req["manifest"], script)?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM extensions WHERE workspace_id = $1").bind(user.workspace_id).fetch_one(&state.db).await?;
    if count >= 50 {
        return Err(ApiError::bad("a workspace holds up to 50 extensions"));
    }
    // reads and writes are made as someone: the installing admin unless said otherwise
    let run_as = match req.get("runAs") {
        Some(v) => run_as_of(&state, &user, v).await?,
        None => Some(user.id),
    };
    let id = new_id();
    sqlx::query("INSERT INTO extensions (id, workspace_id, name, title, version, description, api_version, manifest, script, run_as, next_run_at, installed_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)")
        .bind(id)
        .bind(user.workspace_id)
        .bind(manifest["name"].as_str())
        .bind(manifest["title"].as_str())
        .bind(manifest["version"].as_str())
        .bind(manifest["description"].as_str())
        .bind(API_VERSION as i32)
        .bind(&manifest)
        .bind(script)
        .bind(run_as)
        .bind(next_run(&manifest, true))
        .bind(user.id)
        .execute(&state.db)
        .await
        .map_err(|e| if e.as_database_error().is_some_and(|d| d.is_unique_violation()) { ApiError::Conflict(format!("an extension named {} is installed already", manifest["name"].as_str().unwrap_or(""))) } else { e.into() })?;
    audit(
        &state,
        user.workspace_id,
        Some(user.id),
        "extension.installed",
        manifest["name"].as_str().map(str::to_string),
        json!({"version": manifest["version"], "capabilities": manifest["capabilities"]}),
    )
    .await;
    Ok((StatusCode::CREATED, Json(admin_json(&admin_row(&state, user.workspace_id, id).await?))))
}

/// `PATCH /admin/extensions/{id}` `{enabled?, manifest?, script?, runAs?, secrets?: {NAME: value | null}}`.
pub async fn update(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>, Json(req): Json<Value>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    let cur = admin_row(&state, user.workspace_id, id).await?;
    let mut manifest = cur.manifest.clone();
    let mut script = cur.script.clone();
    if req.get("manifest").is_some() || req.get("script").is_some() {
        script = req["script"].as_str().map(str::to_string).unwrap_or(script);
        let wanted = if req.get("manifest").is_some() { req["manifest"].clone() } else { manifest.clone() };
        manifest = check_manifest(&wanted, &script)?;
        if manifest["name"] != cur.manifest["name"] {
            return Err(ApiError::bad("an extension keeps its name: install the other one next to it"));
        }
    }
    let enabled = req["enabled"].as_bool().unwrap_or(cur.enabled);
    let run_as = match req.get("runAs") {
        Some(v) => run_as_of(&state, &user, v).await?,
        None => cur.run_as,
    };
    let mut secrets = cur.secrets.as_object().cloned().unwrap_or_default();
    for (name, value) in req["secrets"].as_object().into_iter().flatten() {
        if !manifest["capabilities"]["secrets"].as_array().is_some_and(|s| s.iter().any(|x| x == name)) {
            return Err(ApiError::bad(format!("the extension doesn't ask for a secret {name}")));
        }
        match value.as_str().filter(|v| !v.is_empty()) {
            Some(v) if v.len() <= 4000 => secrets.insert(name.clone(), json!(v)),
            Some(_) => return Err(ApiError::bad("a secret is at most 4000 characters")),
            None => secrets.remove(name),
        };
    }
    sqlx::query(
        "UPDATE extensions SET manifest = $2, script = $3, title = $4, version = $5, description = $6, enabled = $7, run_as = $8, secrets = $9, next_run_at = $10, updated_at = now(),
                last_error = CASE WHEN $3 <> script THEN NULL ELSE last_error END WHERE id = $1",
    )
    .bind(id)
    .bind(&manifest)
    .bind(&script)
    .bind(manifest["title"].as_str())
    .bind(manifest["version"].as_str())
    .bind(manifest["description"].as_str())
    .bind(enabled)
    .bind(run_as)
    .bind(Value::Object(secrets))
    .bind(next_run(&manifest, enabled))
    .execute(&state.db)
    .await?;
    let what = if enabled != cur.enabled { if enabled { "extension.enabled" } else { "extension.disabled" } } else { "extension.updated" };
    audit(&state, user.workspace_id, Some(user.id), what, Some(cur.name.clone()), json!({"version": manifest["version"], "capabilities": manifest["capabilities"], "secrets": req["secrets"].as_object().map(|s| s.keys().cloned().collect::<Vec<_>>())})).await;
    Ok(Json(admin_json(&admin_row(&state, user.workspace_id, id).await?)))
}

/// `DELETE /admin/extensions/{id}`: removes an extension with its state, secrets and logs.
pub async fn remove(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    require_admin(&user)?;
    let cur = admin_row(&state, user.workspace_id, id).await?;
    sqlx::query("DELETE FROM extensions WHERE id = $1").bind(id).execute(&state.db).await?;
    audit(&state, user.workspace_id, Some(user.id), "extension.removed", Some(cur.name), json!({})).await;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /admin/extensions/{id}/logs`: what it printed and what went wrong, latest first.
pub async fn logs(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    admin_row(&state, user.workspace_id, id).await?;
    let rows: Vec<(DateTime<Utc>, String, String, String)> =
        sqlx::query_as("SELECT at, level, hook, message FROM extension_logs WHERE extension_id = $1 ORDER BY at DESC, id DESC LIMIT 200")
            .bind(id)
            .fetch_all(&state.db)
            .await?;
    Ok(Json(json!(rows.iter().map(|(at, level, hook, message)| json!({"at": at, "level": level, "hook": hook, "message": message})).collect::<Vec<_>>())))
}

/// `POST /admin/extensions/{id}/run`: runs its scheduled job now (to try it).
pub async fn run_now(State(state): State<AppState>, CurrentUser(user): CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    require_admin(&user)?;
    admin_row(&state, user.workspace_id, id).await?;
    let x: Ext = sqlx::query_as(sqlx::AssertSqlSafe(format!("{EXT_SQL} WHERE id = $1"))).bind(id).fetch_one(&state.db).await?;
    if x.manifest["schedule"].is_null() {
        return Err(ApiError::bad("this extension has no scheduled job"));
    }
    Ok(Json(match run_hook(&state, &x, "scheduled", "scheduled", vec![], 1).await {
        Ok((v, _)) => json!({"ok": true, "result": v}),
        Err(e) => json!({"ok": false, "error": e}),
    }))
}

/// `GET /extensions/actions`: the actions automation rules can use (not for guests).
pub async fn actions(State(state): State<AppState>, CurrentUser(user): CurrentUser) -> ApiResult<Json<Value>> {
    if user.is_guest() {
        return Err(ApiError::Forbidden);
    }
    let rows: Vec<(String, String, Value)> = sqlx::query_as(
        "SELECT name, title, manifest->'actions' FROM extensions WHERE workspace_id = $1 AND enabled AND manifest ? 'actions' ORDER BY lower(title)",
    )
    .bind(user.workspace_id)
    .fetch_all(&state.db)
    .await?;
    let mut out = Vec::new();
    for (name, title, list) in &rows {
        for a in list.as_array().into_iter().flatten() {
            out.push(json!({"extension": name, "extensionTitle": title, "name": a["name"], "label": a["label"]}));
        }
    }
    Ok(Json(json!(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts() {
        let allowed = vec!["erp.example.com".to_string(), "*.acme.test".to_string()];
        assert!(host_allowed(&allowed, "ERP.example.com"));
        assert!(host_allowed(&allowed, "api.acme.test"));
        assert!(!host_allowed(&allowed, "acme.test"), "the domain itself isn't a subdomain");
        assert!(!host_allowed(&allowed, "example.com") && !host_allowed(&allowed, "evil-erp.example.com.attacker.test"));
    }

    #[test]
    fn manifests() {
        let script = "fn action_push(issue, params) { } fn on_event(e, issue) { } fn scheduled() { } fn route(r) { 1 }";
        let m = check_manifest(
            &json!({"name": "erp-sync", "title": " ERP ", "capabilities": {"issues": "read", "http": ["erp.example.com"], "state": true, "secrets": ["ERP_TOKEN"]},
                    "actions": [{"name": "push", "label": "Push to ERP"}], "events": ["created"], "schedule": {"every": "day", "at": "06:00"}, "routes": [{"path": "/status/"}]}),
            script,
        )
        .unwrap();
        assert_eq!((m["title"].as_str(), m["version"].as_str(), m["apiVersion"].as_i64()), (Some("ERP"), Some("0.1.0"), Some(1)));
        assert_eq!(m["routes"], json!([{"method": "GET", "path": "status"}]));
        for (bad, why) in [
            (json!({"name": "X"}), "lower-case"),
            (json!({"name": "ok", "apiVersion": 2}), "extension API 1"),
            (json!({"name": "ok", "capabilities": {"files": true}}), "unknown capability"),
            (json!({"name": "ok", "capabilities": {"http": ["*"]}}), "host names"),
            (json!({"name": "ok", "capabilities": {"http": ["https://x.test/path"]}}), "host names"),
            (json!({"name": "ok", "actions": [{"name": "missing"}]}), "action_missing"),
            (json!({"name": "ok", "events": ["deleted"]}), "unknown event"),
            (json!({"name": "ok", "wasm": true}), "unknown key"),
        ] {
            let e = check_manifest(&bad, script).unwrap_err().to_string();
            assert!(e.contains(why), "{why}: {e}");
        }
        assert!(check_manifest(&json!({"name": "ok", "events": ["created"]}), "fn nope() {}").unwrap_err().to_string().contains("on_event"));
        assert!(check_manifest(&json!({"name": "ok"}), "fn (").unwrap_err().to_string().contains("doesn't parse"));
    }
}
