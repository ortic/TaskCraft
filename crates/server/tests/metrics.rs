//! Prometheus metrics: off without a token, protected with one, requests under their route.

mod common;

use common::*;

#[tokio::test]
async fn metrics_need_a_token_and_count_requests_by_route() {
    let off = spawn().await;
    assert_eq!(client().get(off.url("/metrics")).send().await.unwrap().status(), 404, "off without a token");

    let app = spawn_with(|c| c.metrics_token = Some("scrape-me".into())).await;
    let c = app.setup(true).await;
    get(&c, app.url("/issues/WEB-1")).await;
    get(&c, app.url("/issues/WEB-2")).await;
    let r = client().get(app.url("/metrics")).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = client().get(app.url("/metrics")).bearer_auth("wrong").send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = client().get(app.url("/metrics")).bearer_auth("scrape-me").send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/plain"));
    let text = r.text().await.unwrap();
    assert!(text.contains(r#"taskcraft_http_requests_total{method="GET",route="/api/issues/{key}",status="2xx"} 2"#), "one series for both issues:\n{text}");
    assert!(text.contains(r#"taskcraft_http_request_duration_seconds_bucket{method="GET",route="/api/issues/{key}",status="2xx",le="+Inf"} 2"#));
    assert!(text.contains(r#"taskcraft_db_pool_connections{state="idle"}"#));
    assert!(text.contains("taskcraft_mail_outbox{state=\"pending\"}"));
    assert!(text.lines().any(|l| l.starts_with("taskcraft_issues ") && l.split(' ').nth(1).unwrap().parse::<f64>().unwrap() > 20.0), "{text}");
    assert!(text.contains(&format!("taskcraft_build_info{{version=\"{}\"}} 1", env!("CARGO_PKG_VERSION"))));
    // every sample line parses as name{labels} value
    for l in text.lines().filter(|l| !l.starts_with('#')) {
        assert!(l.rsplit_once(' ').is_some_and(|(_, v)| v.parse::<f64>().is_ok()), "bad sample: {l}");
    }
}
