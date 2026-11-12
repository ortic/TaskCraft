//! Several releases (fixed in, found in) and components per issue.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn issues_have_several_releases_and_components() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    post(&admin, app.url("/projects"), json!({"key": "REL", "name": "Releases", "template": "kanban"})).await;
    post(&admin, app.url("/projects"), json!({"key": "OTH", "name": "Other", "template": "kanban"})).await;
    let id = |v: &Value| v["id"].as_str().unwrap().to_string();
    let v1 = id(&post(&admin, app.url("/projects/REL/versions"), json!({"name": "1.0"})).await);
    let v2 = id(&post(&admin, app.url("/projects/REL/versions"), json!({"name": "1.1"})).await);
    let v3 = id(&post(&admin, app.url("/projects/REL/versions"), json!({"name": "2.0"})).await);
    let api = id(&post(&admin, app.url("/projects/REL/components"), json!({"name": "API"})).await);
    let ui = id(&post(&admin, app.url("/projects/REL/components"), json!({"name": "UI"})).await);
    let foreign = id(&post(&admin, app.url("/projects/OTH/versions"), json!({"name": "9.9"})).await);

    let a = post(
        &admin,
        app.url("/issues"),
        json!({"projectKey": "REL", "title": "Crash on save", "versionIds": [v2, v3], "affectedVersionIds": [v1], "componentIds": [ui, api]}),
    )
    .await;
    let key = a["key"].as_str().unwrap().to_string();
    let names = |v: &Value| v.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(names(&a["versions"]), ["1.1", "2.0"], "in release order");
    assert_eq!(names(&a["affectedVersions"]), ["1.0"]);
    assert_eq!(names(&a["components"]), ["API", "UI"], "by name");
    // the old single fields still work: they replace the list
    let b = post(&admin, app.url("/issues"), json!({"projectKey": "REL", "title": "Typo", "versionId": v1, "componentId": ui})).await;
    assert_eq!((names(&b["versions"]), names(&b["components"])), (vec!["1.0".to_string()], vec!["UI".to_string()]));
    // only the project's own
    let (s, _) = send(&admin, Method::PATCH, app.url(&format!("/issues/{key}")), json!({"versionIds": [foreign]})).await;
    assert_eq!(s, 400);
    // a change is recorded with both lists
    patch(&admin, app.url(&format!("/issues/{key}")), json!({"componentIds": [api]})).await;
    let history = get(&admin, app.url(&format!("/issues/{key}/history"))).await;
    let change = history.as_array().unwrap().iter().find(|e| e["field"] == "components").cloned().unwrap();
    assert_eq!((names(&change["old"]), names(&change["new"])), (vec!["API".to_string(), "UI".to_string()], vec!["API".to_string()]));

    // TQL: has one of them / has none of them / empty
    let found = |q: &str| {
        let (c, url, q) = (admin.clone(), app.url("/search"), q.to_string());
        async move {
            let v: Value = c.get(url).query(&[("q", q.as_str())]).send().await.unwrap().json().await.unwrap();
            v["issues"].as_array().unwrap().iter().map(|i| i["title"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        }
    };
    assert_eq!(found("project = REL and version = \"2.0\"").await, ["Crash on save"]);
    assert_eq!(found("project = REL and affects = \"1.0\"").await, ["Crash on save"]);
    assert_eq!(found("project = REL and version = \"1.0\"").await, ["Typo"]);
    assert_eq!(found("project = REL and version != \"1.0\"").await, ["Crash on save"]);
    assert_eq!(found("project = REL and component = UI").await, ["Typo"]);
    assert_eq!(found("project = REL and affects is empty").await, ["Typo"]);

    // releasing 1.1 moves the unfinished issue on to 2.0 and keeps its other releases
    let r = post(&admin, app.url(&format!("/versions/{v2}/release")), json!({"moveTo": v3})).await;
    assert_eq!(r["moved"], 1);
    assert_eq!(names(&get(&admin, app.url(&format!("/issues/{key}"))).await["versions"]), ["2.0"]);
    // CSV: comma separated
    let csv = admin.get(app.url("/search.csv")).query(&[("q", "project = REL order by key")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.lines().next().unwrap().contains("Version,Affects version,Component"), "{csv}");
    // deleting a release takes it off its issues
    send(&admin, Method::DELETE, app.url(&format!("/versions/{v1}")), json!(null)).await;
    assert_eq!(get(&admin, app.url(&format!("/issues/{key}"))).await["affectedVersions"], json!([]));
    // moving to another project clears them
    let moved = post(&admin, app.url(&format!("/issues/{key}/move-project")), json!({"projectKey": "OTH"})).await;
    assert_eq!((moved["versions"].clone(), moved["components"].clone()), (json!([]), json!([])));
}
