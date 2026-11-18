//! Customers: time is booked to the customer of its project (or the project above it) unless a
//! log names another one; the time report groups and filters by customer and — for admins, with
//! rates — shows amounts; a billed period is closed for everyone but admins; TQL finds issues by
//! customer.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

async fn report(c: &reqwest::Client, app: &TestApp, extra: &[(&str, &str)]) -> Value {
    let mut q: Vec<(&str, &str)> = vec![("from", "2026-10-01"), ("to", "2026-11-30")];
    q.extend_from_slice(extra);
    let r = c.get(app.url("/time-report")).query(&q).send().await.unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

fn row<'a>(v: &'a Value, label: &str) -> &'a Value {
    v["rows"].as_array().unwrap().iter().find(|r| r["label"] == label).unwrap_or_else(|| panic!("no row {label} in {v}"))
}

#[tokio::test]
async fn time_for_customers_with_rates_and_closed_periods() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    for (key, name) in [("WEB", "Acme website"), ("APP", "Acme app"), ("SUP", "Shared support"), ("INT", "Internal")] {
        post(&admin, app.url("/projects"), json!({"key": key, "name": name, "template": "kanban"})).await;
    }
    patch(&admin, app.url("/projects/APP"), json!({"parentId": get(&admin, app.url("/projects/WEB")).await["id"]})).await;

    // customers are organizations with a number and a contact
    let acme = post(&admin, app.url("/organizations"), json!({"name": "Acme", "number": "C-1001", "contact": "Wile E. · wile@acme.test"})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let globex = post(&admin, app.url("/organizations"), json!({"name": "Globex", "number": "C-1002"})).await["id"].as_str().unwrap().to_string();
    let old = post(&admin, app.url("/organizations"), json!({"name": "Old Corp"})).await["id"].as_str().unwrap().to_string();
    assert_eq!(send(&admin, Method::PATCH, app.url(&format!("/organizations/{old}")), json!({"archived": true})).await.0, 204);
    let list = get(&mia, app.url("/organizations")).await;
    let a = list.as_array().unwrap().iter().find(|o| o["name"] == "Acme").unwrap();
    assert_eq!((a["number"].as_str(), a["contact"].as_str(), a["archived"].as_bool()), (Some("C-1001"), Some("Wile E. · wile@acme.test"), Some(false)));
    assert_eq!(list.as_array().unwrap().iter().find(|o| o["name"] == "Old Corp").unwrap()["archived"], true);

    // a project belongs to a customer; its subproject has it from there
    assert_eq!(send(&mia, Method::PATCH, app.url("/projects/WEB"), json!({"customerId": acme})).await.0, 403);
    patch(&admin, app.url("/projects/WEB"), json!({"customerId": acme})).await;
    let (s, v) = send(&admin, Method::PATCH, app.url("/projects/SUP"), json!({"customerId": old})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("archived"), "{v}");
    assert_eq!(get(&mia, app.url("/projects/WEB")).await["customer"], json!({"id": acme, "name": "Acme", "inherited": false}));
    assert_eq!(get(&mia, app.url("/projects/APP")).await["customer"], json!({"id": acme, "name": "Acme", "inherited": true}));
    assert!(get(&mia, app.url("/projects/SUP")).await["customer"].is_null());
    assert_eq!(get(&mia, app.url(&format!("/organizations/{acme}"))).await["projects"], json!([{"key": "WEB", "name": "Acme website"}]));

    for (key, title) in [("WEB", "Landing page"), ("APP", "Push messages"), ("SUP", "Password reset"), ("INT", "Team day")] {
        post(&mia, app.url("/issues"), json!({"projectKey": key, "title": title})).await;
    }
    let dev = post(&admin, app.url("/time-categories"), json!({"name": "Development", "billable": true})).await["id"].as_str().unwrap().to_string();
    let log = |c: &reqwest::Client, key: &'static str, body: Value| {
        let (c, url) = (c.clone(), app.url(&format!("/issues/{key}/worklogs")));
        async move { send(&c, Method::POST, url, body).await }
    };
    // the project's customer by default; a log in the shared project names its own
    assert_eq!(log(&mia, "WEB-1", json!({"time": "2h", "date": "2026-10-20", "categoryId": dev})).await.0, 201);
    assert_eq!(log(&mia, "APP-1", json!({"time": "1h", "date": "2026-11-03", "categoryId": dev})).await.0, 201);
    assert_eq!(log(&mia, "SUP-1", json!({"time": "30m", "date": "2026-11-04", "billable": true, "customerId": globex})).await.0, 201);
    assert_eq!(log(&admin, "SUP-1", json!({"time": "45m", "date": "2026-11-05", "billable": true, "customerId": acme})).await.0, 201);
    assert_eq!(log(&mia, "INT-1", json!({"time": "3h", "date": "2026-11-06"})).await.0, 201);
    let (s, v) = log(&mia, "SUP-1", json!({"time": "1h", "customerId": old})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("archived"), "{v}");
    let logs = get(&mia, app.url("/issues/SUP-1/worklogs")).await;
    assert_eq!(logs[1]["customer"]["name"], "Globex");
    assert!(get(&mia, app.url("/issues/WEB-1/worklogs")).await[0]["customer"].is_null(), "the project's: nothing of its own");

    // the report by customer: billable and internal, per month inside
    let by = report(&mia, &app, &[("group", "customer"), ("then", "month")]).await;
    let acme_row = row(&by, "Acme");
    assert_eq!((acme_row["minutes"].as_i64(), acme_row["billableMinutes"].as_i64()), (Some(225), Some(225)), "{by}");
    let months: Vec<(&str, i64)> =
        acme_row["children"].as_array().unwrap().iter().map(|m| (m["label"].as_str().unwrap(), m["minutes"].as_i64().unwrap())).collect();
    assert_eq!(months, [("2026-10", 120), ("2026-11", 105)]);
    assert_eq!(row(&by, "Globex")["minutes"], 30);
    let internal = by["rows"].as_array().unwrap().iter().find(|r| r["id"] == "none").unwrap();
    assert_eq!((internal["minutes"].as_i64(), internal["billableMinutes"].as_i64()), (Some(180), Some(0)));
    assert!(acme_row.get("amount").is_none() && by["amount"].is_null(), "no rates yet, and not for members");
    assert_eq!(report(&mia, &app, &[("group", "project"), ("customer", &acme)]).await["totalMinutes"], 225);
    assert_eq!(report(&mia, &app, &[("customer", "none")]).await["totalMinutes"], 180);

    // rates (admins): the most specific one counts; amounts for admins only
    assert_eq!(send(&mia, Method::GET, app.url("/time-rates"), Value::Null).await.0, 403);
    assert_eq!(send(&mia, Method::PUT, app.url("/time-rates"), json!({"rates": []})).await.0, 403);
    let (s, v) = send(&admin, Method::PUT, app.url("/time-rates"), json!({"rates": [{"rate": 100}, {"rate": 120}]})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("same customer"), "{v}");
    assert_eq!(send(&admin, Method::PUT, app.url("/time-rates"), json!({"rates": [{"rate": -1}]})).await.0, 400);
    let rates = json!({"rates": [{"rate": 100}, {"customerId": acme, "rate": 150}, {"customerId": acme, "userId": mia_id, "rate": 180.5}]});
    assert_eq!(send(&admin, Method::PUT, app.url("/time-rates"), rates).await.0, 204);
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"currency": "chf"})).await.0 / 100, 2);
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"currency": "francs"})).await.0, 400);
    let page = get(&admin, app.url("/time-rates")).await;
    assert_eq!((page["currency"].as_str(), page["rates"].as_array().unwrap().len()), (Some("CHF"), 3));
    assert!(
        page["rates"].as_array().unwrap().iter().any(|r| r["customer"]["name"] == "Acme" && r["user"]["name"] == "Mia Member" && r["rate"] == 180.5),
        "{page}"
    );
    let money = report(&admin, &app, &[("group", "customer")]).await;
    // Acme: Mia 3h at 180.50, the admin 0.75h at 150; Globex: 0.5h at the default 100; internal time costs nothing
    assert_eq!(row(&money, "Acme")["amount"], 654.0);
    assert_eq!(row(&money, "Globex")["amount"], 50.0);
    assert_eq!(money["rows"].as_array().unwrap().iter().find(|r| r["id"] == "none").unwrap()["amount"], 0.0);
    assert_eq!((money["amount"].as_f64(), money["currency"].as_str()), (Some(704.0), Some("CHF")));
    assert!(report(&mia, &app, &[("group", "customer")]).await["amount"].is_null(), "amounts are the admins'");
    let csv = admin
        .get(app.url("/time-report"))
        .query(&[("from", "2026-10-01"), ("to", "2026-11-30"), ("format", "csv")])
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "Date,Person,Project,Issue,Title,Category,Customer,Customer number,Billable,Hours,Minutes,Note,Rate,Amount,Currency");
    assert!(lines.contains(&"2026-10-20,Mia Member,WEB,WEB-1,Landing page,Development,Acme,C-1001,yes,2.00,120,,180.50,361.00,CHF"), "{csv}");
    assert!(lines.contains(&"2026-11-06,Mia Member,INT,INT-1,Team day,,,,no,3.00,180,,,0.00,CHF"), "{csv}");
    let csv =
        mia.get(app.url("/time-report")).query(&[("from", "2026-10-01"), ("to", "2026-11-30"), ("format", "csv")]).send().await.unwrap().text().await.unwrap();
    assert!(csv.lines().next().unwrap().ends_with("Minutes,Note"), "no rates for members");

    // October is billed: its time is closed, except for admins
    assert_eq!(send(&mia, Method::PUT, app.url("/admin/workspace"), json!({"timeClosedUntil": "2026-10-31"})).await.0, 403);
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"timeClosedUntil": "2026-10-31"})).await.0 / 100, 2);
    assert_eq!(get(&mia, app.url("/me")).await["workspace"]["timeClosedUntil"], "2026-10-31");
    let (s, v) = log(&mia, "WEB-1", json!({"time": "1h", "date": "2026-10-31"})).await;
    assert!(s == 403 && v["message"].as_str().unwrap().contains("closed"), "{v}");
    assert_eq!(log(&mia, "WEB-1", json!({"time": "1h", "date": "2026-11-01"})).await.0, 201);
    let web = get(&mia, app.url("/issues/WEB-1/worklogs")).await;
    let (october, november) = (web[1]["id"].as_str().unwrap().to_string(), web[0]["id"].as_str().unwrap().to_string());
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/worklogs/{october}")), json!({"minutes": 5})).await.0, 403);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/worklogs/{october}")), Value::Null).await.0, 403);
    assert_eq!(
        send(&mia, Method::PATCH, app.url(&format!("/worklogs/{november}")), json!({"date": "2026-10-15"})).await.0,
        403,
        "nor moved into the closed period"
    );
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/worklogs/{november}")), json!({"customerId": globex, "billable": true})).await.0, 200);
    assert_eq!(send(&admin, Method::PATCH, app.url(&format!("/worklogs/{october}")), json!({"minutes": 150})).await.0, 200, "admins correct what was billed");
    let after = report(&admin, &app, &[("group", "customer")]).await;
    assert_eq!((row(&after, "Acme")["minutes"].as_i64(), row(&after, "Globex")["minutes"].as_i64()), (Some(255), Some(90)));
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"timeClosedUntil": null})).await.0 / 100, 2);
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/worklogs/{october}")), json!({"minutes": 120})).await.0, 200);

    // TQL: issues by their project's customer, by name or number
    let found = |q: &'static str| {
        let (c, url) = (mia.clone(), app.url("/search"));
        async move {
            let v: Value = c.get(url).query(&[("q", q)]).send().await.unwrap().json().await.unwrap();
            let mut keys: Vec<String> =
                v["issues"].as_array().unwrap_or_else(|| panic!("{q}: {v}")).iter().map(|i| i["key"].as_str().unwrap().to_string()).collect();
            keys.sort();
            keys
        }
    };
    assert_eq!(found("customer = Acme").await, ["APP-1", "WEB-1"]);
    assert_eq!(found("customer = \"C-1001\" and project = WEB").await, ["WEB-1"]);
    assert_eq!(found("customer is empty").await, ["INT-1", "SUP-1"]);
    assert_eq!(found("customer != Acme").await, ["INT-1", "SUP-1"]);
    // a custom field people called "Customer" keeps its meaning in its project
    post(&admin, app.url("/projects/INT/fields"), json!({"name": "Customer", "kind": "text"})).await;
    let field = get(&admin, app.url("/projects/INT")).await["fields"][0]["id"].as_str().unwrap().to_string();
    let mut custom = serde_json::Map::new();
    custom.insert(field, json!("Acme"));
    patch(&mia, app.url("/issues/INT-1"), json!({"custom": custom})).await;
    assert_eq!(found("project = INT and customer = Acme").await, ["INT-1"]);
    // an archived customer stays in reports; a deleted one leaves its time without a customer
    assert_eq!(send(&admin, Method::PATCH, app.url(&format!("/organizations/{globex}")), json!({"archived": true})).await.0, 204);
    assert_eq!(row(&report(&admin, &app, &[("group", "customer")]).await, "Globex")["minutes"], 90);
}
