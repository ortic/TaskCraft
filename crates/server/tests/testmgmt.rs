//! Test management: test cases with versions and folders, coverage of issues, plans and run
//! templates, runs with results per step and files, failure → bug, the reports and release
//! readiness, TQL, permissions, CSV, automation and the feature switch.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn search(c: &reqwest::Client, app: &TestApp, q: &str) -> Vec<String> {
    let v: Value = c.get(app.url("/search")).query(&[("q", q)]).send().await.unwrap().json().await.unwrap();
    let mut keys: Vec<String> = v["issues"].as_array().unwrap_or_else(|| panic!("{q}: {v}")).iter().map(|i| i["key"].as_str().unwrap().to_string()).collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn cases_plans_runs_and_reports() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (guest, _) = app.user(&admin, "Gus Guest", "gus@example.com", "guest").await;
    post(&admin, app.url("/projects"), json!({"key": "WEB", "name": "Website", "template": "scrum"})).await;
    let version = post(&admin, app.url("/projects/WEB/versions"), json!({"name": "2.4"})).await["id"].as_str().unwrap().to_string();
    for title in ["Pay by card", "Pay by invoice", "Nothing tested"] {
        post(&mia, app.url("/issues"), json!({"projectKey": "WEB", "title": title, "versionIds": [version]})).await;
    }

    // test cases: steps or a scenario, in folders; who may
    assert_eq!(send(&guest, Method::GET, app.url("/projects/WEB/tests"), Value::Null).await.0, 404);
    let folder = post(&mia, app.url("/projects/WEB/test-folders"), json!({"name": "Checkout"})).await["id"].as_str().unwrap().to_string();
    let sub = post(&mia, app.url("/projects/WEB/test-folders"), json!({"name": "Payment", "parentId": folder})).await["id"].as_str().unwrap().to_string();
    let steps =
        json!([{"action": "Put a book in the cart", "expected": "The cart shows 1 item"}, {"action": "Pay with the card **4242**", "expected": "A receipt"}]);
    let t1 = post(&mia, app.url("/projects/WEB/tests"), json!({"title": "Card payment", "preconditions": "A signed-in customer", "steps": steps, "priority": "high", "labels": ["smoke"], "folderId": sub, "issues": ["WEB-1"]})).await;
    assert_eq!(t1["key"], "WEB-T1");
    let t2 = post(&mia, app.url("/projects/WEB/tests"), json!({"title": "Invoice payment", "scenario": "Given a cart with one item\nWhen I choose invoice\nThen I get an invoice by e-mail", "issues": ["WEB-2"]})).await;
    assert_eq!(t2["key"], "WEB-T2");
    post(&mia, app.url("/projects/WEB/tests"), json!({"title": "Footer links", "steps": [{"action": "Click every link"}], "priority": "low"})).await;
    for bad in [
        json!({"steps": []}),
        json!({"title": "x", "priority": "urgent"}),
        json!({"title": "x", "kind": "scenario", "scenario": "just text"}),
        json!({"title": "x", "steps": [{"expected": "y"}]}),
        json!({"title": "x", "issues": ["NOPE-1"]}),
    ] {
        assert_eq!(send(&mia, Method::POST, app.url("/projects/WEB/tests"), bad.clone()).await.0, 400, "{bad}");
    }
    let list = get(&mia, app.url("/projects/WEB/tests")).await;
    assert_eq!((list["cases"].as_array().unwrap().len(), list["folders"].as_array().unwrap().len(), list["canManage"].as_bool()), (3, 2, Some(true)));
    let keys = |v: &Value| v["cases"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(keys(&get(&mia, app.url(&format!("/projects/WEB/tests?folder={folder}"))).await), ["WEB-T1"], "the folder and the ones below it");
    assert_eq!(keys(&get(&mia, app.url("/projects/WEB/tests?label=SMOKE")).await), ["WEB-T1"]);
    assert_eq!(keys(&get(&mia, app.url("/projects/WEB/tests?q=invoice")).await), ["WEB-T2"]);
    assert_eq!(keys(&get(&mia, app.url("/projects/WEB/tests?priority=low")).await), ["WEB-T3"]);

    // versions: a change of the steps makes a new one, a change of the priority doesn't
    let case = get(&mia, app.url("/tests/web-t1")).await;
    assert_eq!((case["version"].as_i64(), case["steps"].as_array().unwrap().len(), case["issues"][0]["key"].as_str()), (Some(1), 2, Some("WEB-1")));
    assert!(case["rendered"]["steps"][1]["action"].to_string().contains("bold"), "Markdown as rich text: {}", case["rendered"]);
    assert_eq!(send(&mia, Method::PATCH, app.url("/tests/WEB-T1"), json!({"priority": "critical"})).await.1["version"], 1);
    let mut longer = steps.as_array().unwrap().clone();
    longer.push(json!({"action": "Open the order", "expected": "It is paid"}));
    assert_eq!(send(&mia, Method::PATCH, app.url("/tests/WEB-T1"), json!({"steps": longer})).await.1["version"], 2);
    assert_eq!(get(&mia, app.url("/tests/WEB-T1/versions/1")).await["steps"].as_array().unwrap().len(), 2);
    assert_eq!(get(&mia, app.url("/tests/WEB-T1")).await["versions"].as_array().unwrap().len(), 2);

    // coverage, from the issue
    let it = get(&mia, app.url("/issues/WEB-1/tests")).await;
    assert_eq!((it["tests"][0]["key"].as_str(), &it["tests"][0]["lastResult"]), (Some("WEB-T1"), &Value::Null));
    assert_eq!(send(&mia, Method::POST, app.url("/issues/WEB-1/tests"), json!({"test": "WEB-T3"})).await.0, 204);
    assert_eq!(get(&mia, app.url("/issues/WEB-1/tests")).await["tests"].as_array().unwrap().len(), 2);
    assert_eq!(send(&mia, Method::DELETE, app.url("/issues/WEB-1/tests/WEB-T3"), Value::Null).await.0, 204);
    assert_eq!(search(&mia, &app, "hasTests = true").await, ["WEB-1", "WEB-2"]);
    assert_eq!(search(&mia, &app, "hasTests = false").await, ["WEB-3"]);
    assert_eq!(search(&mia, &app, "testResult = untested").await, ["WEB-1", "WEB-2"]);

    // a plan: one case by hand, the others by a filter; a run template
    let plan = post(
        &mia,
        app.url("/projects/WEB/test-plans"),
        json!({"name": "Regression", "versionId": version, "caseIds": ["WEB-T2"], "filter": {"labels": ["smoke"]}}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(send(&mia, Method::POST, app.url("/projects/WEB/test-plans"), json!({"name": "regression"})).await.0, 409);
    let p = get(&mia, app.url(&format!("/test-plans/{plan}"))).await;
    assert_eq!(p["cases"].as_array().unwrap().iter().map(|c| c["key"].as_str().unwrap()).collect::<Vec<_>>(), ["WEB-T2", "WEB-T1"]);
    assert_eq!(get(&mia, app.url("/projects/WEB/test-plans")).await["plans"][0]["caseCount"], 2);
    let template = post(
        &mia,
        app.url("/projects/WEB/test-run-templates"),
        json!({"name": "Staging", "planId": plan, "environment": "Staging, Firefox", "testerIds": [mia_id]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();

    // automation: a failing test flags the issue it covers; a finished run comments
    post(
        &admin,
        app.url("/projects/WEB/automation"),
        json!({"name": "Flag", "trigger": {"event": "testFailed"}, "actions": [{"action": "addLabel", "label": "test-failed"}]}),
    )
    .await;
    post(
        &admin,
        app.url("/projects/WEB/automation"),
        json!({"name": "Done", "trigger": {"event": "testRunFinished"}, "actions": [{"action": "comment", "text": "The test run is finished."}]}),
    )
    .await;

    // a run from the template: cases at their current version
    let run = post(&mia, app.url("/projects/WEB/test-runs"), json!({"templateId": template})).await;
    assert_eq!(run["key"], "WEB-R1");
    let run_id = run["id"].as_str().unwrap().to_string();
    let r = get(&mia, app.url(&format!("/test-runs/{run_id}"))).await;
    assert_eq!(
        (r["environment"].as_str(), r["version"]["name"].as_str(), r["testers"][0]["name"].as_str(), r["counts"]["untested"].as_i64()),
        (Some("Staging, Firefox"), Some("2.4"), Some("Mia Member"), Some(2))
    );
    let rc = |r: &Value, key: &str| r["cases"].as_array().unwrap().iter().find(|c| c["case"]["key"] == key).unwrap().clone();
    let (invoice, card) = (rc(&r, "WEB-T2"), rc(&r, "WEB-T1"));
    assert_eq!(
        (card["version"].as_i64(), card["steps"].as_array().unwrap().len(), invoice["steps"].as_array().unwrap().len()),
        (Some(2), 3, 3),
        "a scenario's lines are its steps"
    );
    let card_id = card["id"].as_str().unwrap().to_string();
    let invoice_id = invoice["id"].as_str().unwrap().to_string();
    // the case changes while the run is under way: the run keeps its version
    send(&mia, Method::PATCH, app.url("/tests/WEB-T1"), json!({"title": "Card payment (3-D Secure)"})).await;
    let card = rc(&get(&mia, app.url(&format!("/test-runs/{run_id}"))).await, "WEB-T1");
    assert_eq!((card["case"]["title"].as_str(), card["outdated"].as_bool(), card["version"].as_i64()), (Some("Card payment"), Some(true), Some(2)));

    // results: step by step; the steps decide the case's result
    let put = |id: &str, body: Value| {
        let (c, url) = (mia.clone(), app.url(&format!("/test-run-cases/{id}")));
        async move { send(&c, Method::PUT, url, body).await }
    };
    assert_eq!(put(&card_id, json!({"step": {"index": 1, "result": "passed"}})).await.1["result"], "untested");
    let failed = put(&card_id, json!({"step": {"index": 2, "result": "failed", "comment": "The card was declined"}})).await;
    assert_eq!((failed.0, failed.1["result"].as_str()), (200, Some("failed")));
    assert_eq!(put(&card_id, json!({"step": {"index": 9, "result": "passed"}})).await.0, 400);
    assert_eq!(put(&card_id, json!({"result": "great"})).await.0, 400);
    assert_eq!(send(&guest, Method::PUT, app.url(&format!("/test-run-cases/{card_id}")), json!({"result": "passed"})).await.0, 404);
    let passed = put(&invoice_id, json!({"result": "passed", "comment": "Fine"})).await;
    assert!(passed.1["steps"].as_array().unwrap().iter().all(|s| s["result"] == "passed"), "passing a case passes its open steps: {passed:?}");
    // a screenshot on the failing step
    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(b"png bytes".to_vec()).file_name("declined.png").mime_str("image/png").unwrap());
    let up = mia.post(app.url(&format!("/test-run-cases/{card_id}/files?step=2"))).header("x-taskcraft", "1").multipart(form).send().await.unwrap();
    assert_eq!(up.status(), 201);
    let file: Value = up.json().await.unwrap();
    let got = mia.get(format!("{}{}", app.base, file[0]["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!((got.status().as_u16(), got.bytes().await.unwrap().as_ref()), (200, b"png bytes".as_ref()));

    // the failure fired the rule on the covered issue; searches see results
    assert!(get(&mia, app.url("/issues/WEB-1")).await["labels"].to_string().contains("test-failed"));
    assert_eq!(search(&mia, &app, "testResult = failed").await, ["WEB-1"]);
    assert_eq!(search(&mia, &app, "testResult = passed").await, ["WEB-2"]);
    assert_eq!(search(&mia, &app, "testRun = \"WEB-R1\"").await, ["WEB-1", "WEB-2"]);
    assert_eq!(get(&mia, app.url("/issues/WEB-1/tests")).await["tests"][0]["lastResult"], "failed");

    // failure → bug: filled in, linked, with the file
    let bug = post(&mia, app.url(&format!("/test-run-cases/{card_id}/bug")), json!({})).await;
    let b = get(&mia, app.url(&format!("/issues/{}", bug["key"].as_str().unwrap()))).await;
    let text = b["description"].to_string();
    for part in ["WEB-T1", "WEB-R1", "Staging, Firefox", "Pay with the card", "A receipt", "The card was declined", "A signed-in customer"] {
        assert!(text.contains(part), "{part} in {text}");
    }
    assert_eq!((b["title"].as_str(), b["attachments"][0]["name"].as_str()), (Some("WEB-T1: Card payment"), Some("declined.png")));
    assert!(b["links"].to_string().contains("WEB-1"), "{}", b["links"]);
    let card = rc(&get(&mia, app.url(&format!("/test-runs/{run_id}"))).await, "WEB-T1");
    assert_eq!((card["bugs"][0]["key"].as_str(), card["files"][0]["step"].as_i64()), (bug["key"].as_str(), Some(2)));

    // finishing closes the run for results
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/test-runs/{run_id}")), json!({"finished": true})).await.0, 204);
    assert_eq!(put(&card_id, json!({"result": "passed"})).await.0, 409);
    assert!(get(&mia, app.url("/issues/WEB-2/comments")).await.to_string().contains("The test run is finished."));

    // the reports: runs, coverage of the release, open failures; the release's readiness
    let rep = get(&mia, app.url(&format!("/projects/WEB/test-report?version={version}"))).await;
    assert_eq!(
        (rep["runs"][0]["passRate"].as_f64(), rep["coverage"]["issues"].as_i64(), rep["coverage"]["covered"].as_i64(), rep["coverage"]["passing"].as_i64()),
        (Some(50.0), Some(3), Some(2), Some(1)),
        "{rep}"
    );
    assert_eq!(
        (rep["coverage"]["withoutTests"][0]["key"].as_str(), rep["coverage"]["failing"][0]["key"].as_str(), rep["ready"].as_bool()),
        (Some("WEB-3"), Some("WEB-1"), Some(false))
    );
    assert_eq!((rep["failures"][0]["case"]["key"].as_str(), rep["failures"][0]["bugs"][0]["key"].as_str()), (Some("WEB-T1"), bug["key"].as_str()));
    let last = rep["history"].as_array().unwrap().last().unwrap().clone();
    assert_eq!((last["passed"].as_i64(), last["failed"].as_i64()), (Some(1), Some(1)));
    assert_eq!(get(&mia, app.url(&format!("/versions/{version}/tests"))).await["failures"].as_array().unwrap().len(), 1);

    // a second run where it passes: the latest result counts; flipping shows as flaky
    let run2 = post(&mia, app.url("/projects/WEB/test-runs"), json!({"planId": plan, "name": "Retest"})).await["id"].as_str().unwrap().to_string();
    let r2 = get(&mia, app.url(&format!("/test-runs/{run2}"))).await;
    let card2 = rc(&r2, "WEB-T1");
    assert_eq!(card2["version"], 3, "the new run executes the current version");
    let card2_id = card2["id"].as_str().unwrap().to_string();
    for result in ["passed", "failed", "passed"] {
        assert_eq!(put(&card2_id, json!({"result": result})).await.0, 200);
    }
    assert_eq!(put(rc(&r2, "WEB-T2")["id"].as_str().unwrap(), json!({"result": "passed"})).await.0, 200);
    let rep = get(&mia, app.url("/projects/WEB/test-report")).await;
    assert_eq!(
        (rep["failures"].as_array().unwrap().len(), rep["flaky"][0]["case"]["key"].as_str(), rep["coverage"]["failingCount"].as_i64()),
        (0, Some("WEB-T1"), Some(0)),
        "{rep}"
    );
    assert_eq!(search(&mia, &app, "testResult = failed").await, Vec::<String>::new());
    assert_eq!(get(&mia, app.url("/projects/WEB/test-runs")).await.as_array().unwrap().len(), 2);
    assert_eq!(get(&mia, app.url(&format!("/projects/WEB/test-runs?version={version}"))).await.as_array().unwrap().len(), 2);

    // permissions per project role: executing and managing are the project's to set
    assert_eq!(
        send(&admin, Method::PATCH, app.url("/projects/WEB"), json!({"settings": {"permissions": {"manageTests": "admin", "executeTests": "admin"}}})).await.0
            / 100,
        2
    );
    assert_eq!(send(&mia, Method::POST, app.url("/projects/WEB/tests"), json!({"title": "No"})).await.0, 403);
    assert_eq!(send(&mia, Method::PUT, app.url(&format!("/test-run-cases/{card2_id}")), json!({"result": "failed"})).await.0, 403);
    assert_eq!(get(&mia, app.url("/projects/WEB/tests")).await["canManage"], false);
    assert_eq!(send(&admin, Method::PATCH, app.url("/projects/WEB"), json!({"settings": {"permissions": {}}})).await.0 / 100, 2);

    // CSV: out and in again; a dry run first
    let csv = mia.get(app.url("/projects/WEB/tests/export.csv")).send().await.unwrap().text().await.unwrap();
    assert!(csv.starts_with("Key,Title,Folder,Priority,Labels,Preconditions,Steps,Scenario,Covers\n"), "{csv}");
    assert!(
        csv.contains(
            "WEB-T1,Card payment (3-D Secure),Checkout / Payment,critical,smoke,A signed-in customer,\"Put a book in the cart => The cart shows 1 item\n"
        ),
        "{csv}"
    );
    let file = "Key,Title,Folder,Priority,Labels,Steps,Covers\nWEB-T3,Footer links work,Layout / Footer,medium,\"ui, smoke\",\"Click every link => No 404\nRead the imprint\",WEB-3\n,Header shows the logo,Layout,,,,\n,,,,,,\n,Broken,,urgent,,,\nWEB-T99,Missing,,,,,\n";
    let dry = post(&mia, app.url("/projects/WEB/tests/import"), json!({"csv": file, "dryRun": true})).await;
    assert_eq!((dry["created"].as_i64(), dry["updated"].as_i64(), dry["errors"].as_array().unwrap().len()), (Some(1), Some(1), 2), "{dry}");
    assert_eq!(get(&mia, app.url("/tests/WEB-T3")).await["title"], "Footer links");
    let done = post(&mia, app.url("/projects/WEB/tests/import"), json!({"csv": file})).await;
    assert_eq!((done["created"].as_i64(), done["updated"].as_i64()), (Some(1), Some(1)));
    let t3 = get(&mia, app.url("/tests/WEB-T3")).await;
    assert_eq!(
        (
            t3["title"].as_str(),
            t3["steps"][0]["expected"].as_str(),
            t3["steps"].as_array().unwrap().len(),
            t3["labels"].clone(),
            t3["issues"][0]["key"].as_str(),
            t3["version"].as_i64()
        ),
        (Some("Footer links work"), Some("No 404"), 2, json!(["ui", "smoke"]), Some("WEB-3"), Some(2))
    );
    assert_eq!(get(&mia, app.url("/projects/WEB/tests")).await["folders"].as_array().unwrap().len(), 4, "Layout and Layout / Footer were made");

    // archiving keeps results but leaves plans; deleting takes them along
    assert_eq!(send(&mia, Method::PATCH, app.url("/tests/WEB-T2"), json!({"archived": true})).await.0, 200);
    assert_eq!(get(&mia, app.url("/projects/WEB/tests")).await["cases"].as_array().unwrap().len(), 3);
    assert_eq!(get(&mia, app.url("/projects/WEB/tests?archived=true")).await["cases"].as_array().unwrap().len(), 4);
    assert_eq!(get(&mia, app.url(&format!("/test-plans/{plan}"))).await["cases"].as_array().unwrap().len(), 2, "WEB-T1 and, by its label, WEB-T3");
    assert_eq!(send(&mia, Method::DELETE, app.url("/tests/WEB-T1"), Value::Null).await.0, 204);
    assert_eq!(get(&mia, app.url(&format!("/test-runs/{run_id}"))).await["cases"].as_array().unwrap().len(), 1);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/test-runs/{run2}")), Value::Null).await.0, 204);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/test-run-templates/{template}")), Value::Null).await.0, 204);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/test-plans/{plan}")), Value::Null).await.0, 204);

    // the switch
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"features": {"tests": false}})).await.0 / 100, 2);
    assert_eq!(send(&mia, Method::GET, app.url("/projects/WEB/tests"), Value::Null).await.0, 400);
    assert_eq!(send(&mia, Method::GET, app.url("/issues/WEB-1/tests"), Value::Null).await.0, 400);
}
