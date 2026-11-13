//! AI assistance against a stand-in for the Messages API: thread summaries from what the asker
//! may read, questions turned into TQL (checked, with one retry), similar issues (no AI), the
//! switches and the per-hour share.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[derive(Default)]
struct Seen {
    requests: Vec<(Value, Vec<(String, String)>)>,
    tql_calls: usize,
}

/// A stand-in for the Messages API: summaries as text, structured answers as JSON (the first
/// TQL answer doesn't parse, to see the retry).
async fn fake_api(seen: Arc<Mutex<Seen>>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                let mut s = seen.lock().unwrap();
                let h = ["x-api-key", "anthropic-version", "anthropic-beta"]
                    .iter()
                    .map(|k| (k.to_string(), headers.get(*k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string()))
                    .collect();
                s.requests.push((body.clone(), h));
                let text = if body["output_config"]["format"].is_object() {
                    s.tql_calls += 1;
                    if s.tql_calls == 1 {
                        json!({"tql": "status = (", "explanation": "broken"}).to_string()
                    } else {
                        json!({"tql": "assignee = me() and category != done order by priority desc", "explanation": "Your open issues, most important first"})
                            .to_string()
                    }
                } else {
                    "Checkout fails for card payments.\n\n**Next steps**\n- Max retries the terminal".to_string()
                };
                axum::Json(json!({
                    "id": "msg_1", "type": "message", "role": "assistant", "model": body["model"],
                    "content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": text}],
                    "stop_reason": "end_turn", "usage": {"input_tokens": 10, "output_tokens": 10}
                }))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

#[tokio::test]
async fn ai_assistance() {
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let base = fake_api(seen.clone()).await;
    let app = spawn_with(move |c| {
        c.ai_api_key = Some("sk-test".into());
        c.ai_base_url = Some(base);
        c.ai_provider = Some("anthropic".into());
        c.ai_per_hour = 4;
    })
    .await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "kanban"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Card payment fails at checkout"})).await;
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": "Happens with every Visa card"})).await;
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": "secret: the acquirer contract ended", "visibility": "admins"})).await;
    assert_eq!(get(&member, app.url("/me")).await["workspace"]["ai"], true);
    let setup = &get(&c, app.url("/me")).await["workspace"]["aiSetup"];
    assert_eq!((setup["provider"].as_str(), setup["model"].as_str(), setup["local"].as_bool()), (Some("anthropic"), Some("claude-opus-5-5"), Some(true)));
    assert_eq!(get(&member, app.url("/me")).await["workspace"]["aiSetup"], Value::Null, "for admins");

    // a summary of what the asker may read, in their language, with the fallback switched on
    let r = post(&member, app.url("/issues/WEB-1/summary?lang=de"), json!({})).await;
    assert!(r["markdown"].as_str().unwrap().contains("Checkout fails"), "{r}");
    assert_eq!(r["comments"], 1);
    assert_eq!(r["doc"]["type"], "doc");
    {
        let s = seen.lock().unwrap();
        let (body, headers) = &s.requests[0];
        let sent = body.to_string();
        assert!(sent.contains("every Visa card") && !sent.contains("acquirer"), "restricted comments stay out: {sent}");
        assert!(body["system"].as_str().unwrap().contains("\"de\""));
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body["fallbacks"], "default");
        assert_eq!(body["output_config"]["effort"], "medium");
        assert!(headers.contains(&("x-api-key".into(), "sk-test".into())));
        assert!(headers.contains(&("anthropic-version".into(), "2026-10-21".into())));
        assert!(headers.contains(&("anthropic-beta".into(), "server-side-fallback-2026-07-01".into())));
    }

    // a question as TQL: the broken first answer is sent back once, the second one parses
    let r = post(&member, app.url("/ai/tql"), json!({"question": "what's still on my plate?", "project": "WEB"})).await;
    assert_eq!(r["tql"], "assignee = me() and category != done order by priority desc");
    {
        let s = seen.lock().unwrap();
        let (body, _) = s.requests.last().unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 3, "the retry carries the error");
        assert!(body["system"].as_str().unwrap().contains("WEB (Website)"));
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    }

    // a fair share per hour (4 here: one summary and two TQL calls so far count as two requests)
    post(&member, app.url("/issues/WEB-1/summary"), json!({})).await;
    post(&member, app.url("/issues/WEB-1/summary"), json!({})).await;
    assert_eq!(send(&member, Method::POST, app.url("/issues/WEB-1/summary"), json!({})).await.0, 429);

    // the workspace turns it off
    send(&c, Method::PUT, app.url("/admin/workspace"), json!({"features": {"ai": false}})).await;
    assert_eq!(get(&c, app.url("/me")).await["workspace"]["ai"], false);
    assert_eq!(send(&c, Method::POST, app.url("/issues/WEB-1/summary"), json!({})).await.0, 400);
}

#[tokio::test]
async fn similar_issues_without_ai() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    assert_eq!(get(&member, app.url("/me")).await["workspace"]["ai"], false, "no key, no AI");
    assert_eq!(send(&member, Method::POST, app.url("/ai/tql"), json!({"question": "x"})).await.0, 400);
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "kanban"})).await;
    let level = post(&c, app.url("/projects/WEB/security-levels"), json!({"name": "Staff"})).await;
    for title in ["Login fails on Safari", "Checkout total is wrong", "The login page fails to load", "Login failing after password reset"] {
        post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": title})).await;
    }
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login fails for admins", "securityLevelId": level["id"]})).await;
    let keys = |v: Value| v.as_array().unwrap().iter().map(|i| i["key"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let found = keys(get(&member, app.url("/issues/similar?q=Login%20fails%20on%20Chrome&project=WEB")).await);
    assert!(found.contains(&"WEB-1".to_string()) && found.contains(&"WEB-3".to_string()), "{found:?}");
    assert!(found.contains(&"WEB-4".to_string()), "word forms count (failing): {found:?}");
    assert!(!found.contains(&"WEB-2".to_string()), "{found:?}");
    assert!(!found.contains(&"WEB-5".to_string()), "private issues stay private: {found:?}");
    let found = keys(get(&c, app.url("/issues/similar?q=Login%20fails%20on%20Chrome&exclude=WEB-1")).await);
    assert!(found.contains(&"WEB-5".to_string()) && !found.contains(&"WEB-1".to_string()), "{found:?}");
    assert_eq!(get(&member, app.url("/issues/similar?q=login")).await, json!([]), "one word says too little");
}

/// A stand-in for a model running in the network behind an OpenAI-compatible chat API: it takes
/// no JSON schema (400), thinks aloud and fences its JSON.
async fn fake_local(seen: Arc<Mutex<Seen>>) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = format!("http://{}/v1", l.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                let mut s = seen.lock().unwrap();
                let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                s.requests.push((body.clone(), vec![("authorization".into(), auth)]));
                if body.get("response_format").is_some() {
                    return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({"error": {"message": "response_format is not supported"}})));
                }
                let structured = body["messages"][0]["content"].as_str().unwrap_or("").contains("JSON object only");
                let content = if structured {
                    "<think>The person wants their open work.</think>\nHere you go:\n```json\n{\"tql\": \"assignee = me() and category != done\", \"explanation\": \"Your open issues\"}\n```"
                } else {
                    "<think>Reading the thread…</think>\nThe login fails on Safari; a fix is being tested."
                };
                (
                    axum::http::StatusCode::OK,
                    axum::Json(json!({"choices": [{"index": 0, "message": {"role": "assistant", "content": content}, "finish_reason": "stop"}]})),
                )
            }
        }),
    );
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    addr
}

#[tokio::test]
async fn a_model_of_your_own() {
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let base = fake_local(seen.clone()).await;
    let app = spawn_with(move |c| {
        c.ai_provider = Some("openai-compatible".into());
        c.ai_base_url = Some(base);
        c.ai_model = Some("llama3.1".into());
    })
    .await;
    let c = app.setup(false).await;
    let setup = &get(&c, app.url("/me")).await["workspace"]["aiSetup"];
    assert_eq!((setup["provider"].as_str(), setup["model"].as_str(), setup["local"].as_bool()), (Some("openai-compatible"), Some("llama3.1"), Some(true)));
    post(&c, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "kanban"})).await;
    post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login fails on Safari"})).await;
    post(&c, app.url("/issues/WEB-1/comments"), json!({"markdown": "A fix is in testing"})).await;

    let r = post(&c, app.url("/issues/WEB-1/summary"), json!({})).await;
    assert_eq!(r["markdown"], "The login fails on Safari; a fix is being tested.", "without the thinking");
    let r = post(&c, app.url("/ai/tql"), json!({"question": "my open work"})).await;
    assert_eq!(r["tql"], "assignee = me() and category != done");
    let s = seen.lock().unwrap();
    let (first, headers) = &s.requests[0];
    assert_eq!(first["model"], "llama3.1");
    assert_eq!(first["messages"][0]["role"], "system");
    assert_eq!(headers[0].1, "", "no key, no authorization");
    let asked: Vec<bool> = s.requests[1..].iter().map(|(b, _)| b.get("response_format").is_some()).collect();
    assert_eq!(asked, [true, false], "a server without JSON schemas is asked again without one");
}
