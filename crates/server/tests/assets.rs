//! The asset inventory: sets with their own permissions, types with fields (also references to
//! other assets), tags, owners, relations and what depends on what, issues about assets (TQL),
//! CSV import and export, rules on changes and dates, and requesters picking their own assets.

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
async fn sets_types_assets_relations_and_issues() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    let (tom, tom_id) = app.user(&admin, "Tom Teammate", "tom@example.com", "member").await;
    let (guest, _) = app.user(&admin, "Gus Guest", "gus@example.com", "guest").await;
    post(&admin, app.url("/projects"), json!({"key": "IT", "name": "IT desk", "template": "kanban"})).await;

    // sets: workspace admins make them; everyone views by default, guests see nothing
    assert_eq!(send(&mia, Method::POST, app.url("/asset-sets"), json!({"name": "Hardware"})).await.0, 403);
    let hw = post(&admin, app.url("/asset-sets"), json!({"name": "Hardware", "description": "Things with a plug"})).await["id"].as_str().unwrap().to_string();
    let secret = post(&admin, app.url("/asset-sets"), json!({"name": "Contracts", "defaultRole": "none"})).await["id"].as_str().unwrap().to_string();
    assert_eq!(send(&admin, Method::POST, app.url("/asset-sets"), json!({"name": "hardware"})).await.0, 409);
    assert_eq!(get(&guest, app.url("/asset-sets")).await["sets"], json!([]));
    let seen = get(&mia, app.url("/asset-sets")).await;
    assert_eq!(seen["sets"].as_array().unwrap().len(), 1, "not the set that's closed by default: {seen}");
    assert_eq!((seen["sets"][0]["role"].as_str(), seen["canCreate"].as_bool()), (Some("viewer"), Some(false)));
    // Mia edits, Tom's team administers
    let team = post(&admin, app.url("/teams"), json!({"name": "IT", "memberIds": [tom_id]})).await;
    let members =
        json!({"members": [{"subjectType": "user", "subjectId": mia_id, "role": "editor"}, {"subjectType": "team", "subjectId": team["id"], "role": "admin"}]});
    assert_eq!(send(&mia, Method::PATCH, app.url(&format!("/asset-sets/{hw}")), members.clone()).await.0, 403);
    assert_eq!(send(&admin, Method::PATCH, app.url(&format!("/asset-sets/{hw}")), members).await.0, 204);
    assert_eq!(get(&mia, app.url("/asset-sets")).await["sets"][0]["role"], "editor");
    assert_eq!(get(&tom, app.url("/asset-sets")).await["sets"][0]["role"], "admin");

    // types with fields: the usual kinds and references to other assets
    let room = post(&tom, app.url(&format!("/asset-sets/{hw}/types")), json!({"name": "Room", "tagPrefix": "RM", "icon": "door"})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/asset-sets/{hw}/types")), json!({"name": "Nope"})).await.0, 403, "editors edit assets, not the set");
    let fields = json!([
        {"name": "Serial", "kind": "text", "required": true},
        {"name": "Warranty ends", "kind": "date"},
        {"name": "Condition", "kind": "select", "options": ["New", "Used"]},
        {"name": "Location", "kind": "asset", "typeId": room},
        {"name": "Price", "kind": "number"},
    ]);
    let laptop = post(&tom, app.url(&format!("/asset-sets/{hw}/types")), json!({"name": "Laptop", "tagPrefix": "lt", "fields": fields})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        send(&tom, Method::POST, app.url(&format!("/asset-sets/{hw}/types")), json!({"name": "X", "fields": [{"name": "Status", "kind": "text"}]})).await.0,
        400
    );
    let cat = post(&tom, app.url(&format!("/asset-sets/{hw}/categories")), json!({"name": "Office"})).await["id"].as_str().unwrap().to_string();
    let sub =
        post(&tom, app.url(&format!("/asset-sets/{hw}/categories")), json!({"name": "Zurich", "parentId": cat})).await["id"].as_str().unwrap().to_string();
    let sets = get(&mia, app.url("/asset-sets")).await;
    assert_eq!(sets["sets"][0]["types"].as_array().unwrap().len(), 2);
    assert_eq!(sets["sets"][0]["categories"].as_array().unwrap().len(), 2);
    assert!(sets["sets"][0]["members"].is_null() && get(&tom, app.url("/asset-sets")).await["sets"][0]["members"].as_array().unwrap().len() == 2);

    // assets: tags count up per type; values are checked by their field's kind
    let r1 = post(&mia, app.url("/assets"), json!({"typeId": room, "name": "Server room", "categoryId": sub})).await;
    assert_eq!(r1["tag"], "RM-0001");
    let (s, v) = send(&mia, Method::POST, app.url("/assets"), json!({"typeId": laptop, "name": "MacBook Pro 14"})).await;
    assert!(s == 400 && v["message"].as_str().unwrap().contains("Serial is required"), "{v}");
    for (values, why) in [
        (json!({"Serial": "X", "Warranty ends": "soon"}), "a date"),
        (json!({"Serial": "X", "Condition": "Broken"}), "one of its options"),
        (json!({"Serial": "X", "Location": "LT-9999"}), "an asset"),
        (json!({"Serial": "X", "Price": "cheap"}), "a number"),
        (json!({"Serial": "X", "Colour": "grey"}), "no field Colour"),
    ] {
        let (s, v) = send(&mia, Method::POST, app.url("/assets"), json!({"typeId": laptop, "name": "L", "values": values})).await;
        assert!(s == 400 && v["message"].as_str().unwrap().contains(why), "{why}: {v}");
    }
    let l1 = post(
        &mia,
        app.url("/assets"),
        json!({"typeId": laptop, "name": "MacBook Pro 14", "ownerId": tom_id, "categoryId": "Zurich",
               "values": {"Serial": "C02XK1", "Warranty ends": "2027-03-31", "Condition": "used", "Location": "RM-0001", "Price": "2400.50"}}),
    )
    .await;
    assert_eq!(l1["tag"], "LT-0001");
    let l2 = post(&mia, app.url("/assets"), json!({"typeId": laptop, "name": "ThinkPad X1", "tag": "lt-0042", "status": "in stock", "ownerEmail": "jane@example.org", "values": {"Serial": "PF2ABC"}})).await;
    assert_eq!(l2["tag"], "LT-0042");
    assert_eq!(
        send(&mia, Method::POST, app.url("/assets"), json!({"typeId": laptop, "name": "Twin", "tag": "LT-0042", "values": {"Serial": "Z"}})).await.0,
        409
    );
    assert_eq!(send(&guest, Method::GET, app.url("/assets/LT-0001"), Value::Null).await.0, 404);
    let a = get(&tom, app.url("/assets/lt-0001")).await;
    assert_eq!(
        (a["name"].as_str(), a["status"].as_str(), a["owner"]["name"].as_str(), a["category"]["name"].as_str(), a["role"].as_str()),
        (Some("MacBook Pro 14"), Some("in_use"), Some("Tom Teammate"), Some("Zurich"), Some("admin")),
        "{a}"
    );
    let field = |name: &str| a["fields"].as_array().unwrap().iter().find(|f| f["name"] == name).unwrap().clone();
    assert_eq!(
        (field("Condition")["label"].as_str(), field("Location")["label"].as_str(), field("Price")["value"].as_f64()),
        (Some("Used"), Some("RM-0001 Server room"), Some(2400.5))
    );
    assert_eq!(get(&tom, app.url("/assets/LT-0042")).await["owner"], json!({"kind": "email", "name": "jane@example.org"}));

    // lists: by set, type, category (with the ones below), status, owner, text
    let list = |q: &'static str| {
        let (c, url) = (mia.clone(), app.url(&format!("/assets{q}")));
        async move { get(&c, url).await["assets"].as_array().unwrap().iter().map(|a| a["tag"].as_str().unwrap().to_string()).collect::<Vec<_>>() }
    };
    assert_eq!(list("").await, ["LT-0001", "LT-0042", "RM-0001"]);
    assert_eq!(list(&*Box::leak(format!("?type={laptop}&status=in_stock").into_boxed_str())).await, ["LT-0042"]);
    assert_eq!(list(&*Box::leak(format!("?category={cat}").into_boxed_str())).await, ["LT-0001", "RM-0001"], "the category and the ones below it");
    assert_eq!(list("?q=c02xk").await, ["LT-0001"], "found by a field's value");
    assert_eq!(get(&tom, app.url("/assets?owner=me")).await["assets"].as_array().unwrap().len(), 1);

    // changes land in the asset's history; viewers don't change anything
    assert_eq!(
        send(&mia, Method::PATCH, app.url("/assets/LT-0001"), json!({"status": "maintenance", "values": {"Condition": "New", "Price": null}})).await.0,
        204
    );
    let (viewer, _) = app.user(&admin, "Vera Viewer", "vera@example.com", "member").await;
    assert_eq!(send(&viewer, Method::PATCH, app.url("/assets/LT-0001"), json!({"name": "Mine"})).await.0, 403);
    assert_eq!(send(&mia, Method::DELETE, app.url("/assets/LT-0042"), Value::Null).await.0, 403, "deleting is the admins'");
    let a = get(&mia, app.url("/assets/LT-0001")).await;
    assert_eq!(a["status"], "maintenance");
    let h = a["history"].as_array().unwrap();
    assert_eq!(
        (h[0]["kind"].as_str(), h[0]["actor"].as_str(), &h[0]["data"]["status"]),
        (Some("changed"), Some("Mia Member"), &json!({"from": "in_use", "to": "maintenance"})),
        "{h:?}"
    );
    assert_eq!(h[0]["data"]["fields"], json!(["Condition", "Price"]));
    assert_eq!(h.last().unwrap()["kind"], "created");

    // relations: the laptop is installed in the room, a service depends on it — what fails with the room?
    let services = post(&admin, app.url("/asset-sets"), json!({"name": "Services"})).await["id"].as_str().unwrap().to_string();
    let service = post(&admin, app.url(&format!("/asset-sets/{services}/types")), json!({"name": "Service", "tagPrefix": "SVC"})).await["id"]
        .as_str()
        .unwrap()
        .to_string();
    post(&admin, app.url("/assets"), json!({"typeId": service, "name": "Build server"})).await;
    assert_eq!(send(&mia, Method::POST, app.url("/assets/LT-0001/relations"), json!({"kind": "installed_in", "target": "RM-0001"})).await.0, 201);
    assert_eq!(send(&mia, Method::POST, app.url("/assets/LT-0001/relations"), json!({"kind": "installed_in", "target": "RM-0001"})).await.0, 409);
    assert_eq!(send(&mia, Method::POST, app.url("/assets/LT-0001/relations"), json!({"kind": "likes", "target": "RM-0001"})).await.0, 400);
    assert_eq!(send(&mia, Method::POST, app.url("/assets/LT-0001/relations"), json!({"kind": "depends_on", "target": "LT-0001"})).await.0, 400);
    assert_eq!(send(&admin, Method::POST, app.url("/assets/SVC-0001/relations"), json!({"kind": "depends_on", "target": "LT-0001"})).await.0, 201);
    let room_page = get(&mia, app.url("/assets/RM-0001")).await;
    assert_eq!(
        room_page["relations"][0],
        json!({"id": room_page["relations"][0]["id"], "kind": "installed_in", "outward": false, "asset": {"id": l1["id"], "tag": "LT-0001", "name": "MacBook Pro 14", "status": "maintenance"}})
    );
    let graph = get(&mia, app.url("/assets/RM-0001/graph")).await;
    assert_eq!(
        (graph["nodes"].as_array().unwrap().len(), graph["edges"].as_array().unwrap().len(), graph["affected"].as_array().unwrap().len()),
        (3, 2, 2),
        "the laptop and, through it, the service: {graph}"
    );
    let rel = get(&mia, app.url("/assets/LT-0001")).await["relations"].as_array().unwrap().iter().find(|r| r["kind"] == "installed_in").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(send(&viewer, Method::DELETE, app.url(&format!("/asset-relations/{rel}")), Value::Null).await.0, 403);

    // issues about assets, both ways, and in searches
    post(&mia, app.url("/issues"), json!({"projectKey": "IT", "title": "Screen flickers"})).await;
    post(&mia, app.url("/issues"), json!({"projectKey": "IT", "title": "Room is too warm"})).await;
    post(&mia, app.url("/issues"), json!({"projectKey": "IT", "title": "Nothing to do with assets"})).await;
    let linked = send(&mia, Method::PUT, app.url("/issues/IT-1/assets"), json!({"assets": ["lt-0001", l2["id"]]})).await;
    assert_eq!((linked.0, linked.1.as_array().map(Vec::len)), (200, Some(2)), "{linked:?}");
    assert_eq!(send(&mia, Method::PUT, app.url("/issues/IT-2/assets"), json!({"assets": ["RM-0001"]})).await.0, 200);
    assert_eq!(send(&mia, Method::PUT, app.url("/issues/IT-2/assets"), json!({"assets": ["NOPE-1"]})).await.0, 404);
    let issue = get(&mia, app.url("/issues/IT-1")).await;
    assert_eq!(issue["assets"].as_array().unwrap().iter().map(|a| a["tag"].as_str().unwrap()).collect::<Vec<_>>(), ["LT-0001", "LT-0042"]);
    assert_eq!(get(&guest_or(&admin), app.url("/assets/LT-0001")).await["issues"][0]["key"], "IT-1");
    assert!(get(&mia, app.url("/issues/IT-1/history")).await.to_string().contains("LT-0042"));
    assert!(get(&mia, app.url("/assets/LT-0042")).await["history"][0].to_string().contains("IT-1"));
    assert_eq!(search(&mia, &app, "asset = \"LT-0001\"").await, ["IT-1"]);
    assert_eq!(search(&mia, &app, "asset = \"server room\"").await, ["IT-2"]);
    assert_eq!(search(&mia, &app, "asset.type = Laptop").await, ["IT-1"]);
    assert_eq!(search(&mia, &app, "asset.type in (Laptop, Room)").await, ["IT-1", "IT-2"]);
    assert_eq!(search(&mia, &app, "asset is empty").await, ["IT-3"]);
    // one asset less
    assert_eq!(send(&mia, Method::PUT, app.url("/issues/IT-1/assets"), json!({"assets": ["LT-0001"]})).await.1.as_array().unwrap().len(), 1);

    // CSV: export, then import — new rows and, by tag, updates; a dry run first
    let csv = mia.get(app.url(&format!("/asset-sets/{hw}/export.csv"))).send().await.unwrap().text().await.unwrap();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "Tag,Name,Type,Status,Category,Owner,Serial,Warranty ends,Condition,Location,Price");
    assert!(lines.contains(&"LT-0001,MacBook Pro 14,Laptop,maintenance,Zurich,Tom Teammate,C02XK1,2027-03-31,New,RM-0001 Server room,"), "{csv}");
    let file = "Asset tag,Device,State,Who,Serial,Warranty ends,Ignore me\nLT-0042,ThinkPad X1 Carbon,in use,mia@example.com,PF2ABC,2028-01-15,x\n,Dell XPS 13,,IT,DX-77,,y\n,Broken row,,,,,\n";
    let mapping =
        json!({"Asset tag": "tag", "Device": "name", "State": "status", "Who": "owner", "Serial": "Serial", "Warranty ends": "Warranty ends", "Ignore me": ""});
    let import = app.url(&format!("/asset-types/{laptop}/import"));
    assert_eq!(send(&viewer, Method::POST, import.clone(), json!({"csv": file})).await.0, 403);
    let dry = post(&mia, import.clone(), json!({"csv": file, "mapping": mapping, "dryRun": true})).await;
    assert_eq!((dry["created"].as_i64(), dry["updated"].as_i64(), dry["errors"][0]["row"].as_i64()), (Some(1), Some(1), Some(4)), "{dry}");
    assert!(dry["errors"][0]["message"].as_str().unwrap().contains("Serial is required"));
    assert_eq!(get(&mia, app.url("/assets/LT-0042")).await["name"], "ThinkPad X1", "a dry run changes nothing");
    let done = post(&mia, import.clone(), json!({"csv": file, "mapping": mapping})).await;
    assert_eq!((done["created"].as_i64(), done["updated"].as_i64(), done["errors"].as_array().map(Vec::len)), (Some(1), Some(1), Some(1)), "{done}");
    let updated = get(&mia, app.url("/assets/LT-0042")).await;
    assert_eq!(
        (updated["name"].as_str(), updated["status"].as_str(), updated["owner"]["name"].as_str()),
        (Some("ThinkPad X1 Carbon"), Some("in_use"), Some("Mia Member"))
    );
    let new = get(&mia, app.url("/assets/LT-0043")).await;
    assert_eq!(
        (new["name"].as_str(), new["owner"].clone(), new["tag"].as_str()),
        (Some("Dell XPS 13"), json!({"kind": "team", "id": team["id"], "name": "IT"}), Some("LT-0043"))
    );
    // headers that name the fields need no mapping
    let auto = post(&mia, import, json!({"csv": "Name,Serial,Condition\nFramework 13,FW-1,New\n", "dryRun": true})).await;
    assert_eq!((auto["created"].as_i64(), auto["columns"][2]["target"].as_str()), (Some(1), Some("Condition")), "{auto}");

    // the closed set: only for those given a role; deleting types and sets
    assert_eq!(send(&mia, Method::POST, app.url(&format!("/asset-sets/{secret}/types")), json!({"name": "Contract"})).await.0, 404);
    assert_eq!(send(&tom, Method::DELETE, app.url(&format!("/asset-types/{laptop}")), Value::Null).await.0, 409, "it has assets");
    assert_eq!(send(&tom, Method::DELETE, app.url("/assets/LT-0043"), Value::Null).await.0, 204);
    assert_eq!(send(&tom, Method::DELETE, app.url(&format!("/asset-sets/{hw}")), Value::Null).await.0, 403, "sets are the workspace admins' to delete");
    // the switch
    assert_eq!(send(&admin, Method::PUT, app.url("/admin/workspace"), json!({"features": {"assets": false}})).await.0 / 100, 2);
    assert_eq!(send(&mia, Method::GET, app.url("/asset-sets"), Value::Null).await.0, 400);
    assert_eq!(get(&mia, app.url("/issues/IT-1")).await["assets"], json!([]));
}

/// The admin's client under another name (reads the asset's issues as someone who sees them all).
fn guest_or(admin: &reqwest::Client) -> reqwest::Client {
    admin.clone()
}

#[tokio::test]
async fn rules_act_on_changes_and_before_dates() {
    let app = spawn().await;
    let admin = app.setup(false).await;
    let (mia, mia_id) = app.user(&admin, "Mia Member", "mia@example.com", "member").await;
    post(&admin, app.url("/projects"), json!({"key": "IT", "name": "IT desk", "template": "kanban"})).await;
    let set = post(&admin, app.url("/asset-sets"), json!({"name": "Hardware", "defaultRole": "editor"})).await["id"].as_str().unwrap().to_string();
    let laptop = post(
        &admin,
        app.url(&format!("/asset-sets/{set}/types")),
        json!({"name": "Laptop", "tagPrefix": "LT", "fields": [{"name": "Warranty ends", "kind": "date"}]}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let field = get(&admin, app.url("/asset-sets")).await["sets"][0]["types"][0]["fields"][0]["id"].as_str().unwrap().to_string();
    let rules = app.url(&format!("/asset-sets/{set}/rules"));

    // rules are the set admins'; they're checked
    assert_eq!(send(&mia, Method::GET, rules.clone(), Value::Null).await.0, 403);
    for bad in [
        json!({"name": "x", "trigger": {"event": "deleted"}, "actions": [{"action": "notifyOwner"}]}),
        json!({"name": "x", "trigger": {"event": "date"}, "actions": [{"action": "notifyOwner"}]}),
        json!({"name": "x", "trigger": {"event": "created"}, "actions": []}),
        json!({"name": "x", "trigger": {"event": "created"}, "actions": [{"action": "createIssue", "title": "no project"}]}),
        json!({"name": "x", "trigger": {"event": "created"}, "actions": [{"action": "setStatus", "status": "gone"}]}),
    ] {
        assert_eq!(send(&admin, Method::POST, rules.clone(), bad.clone()).await.0, 400, "{bad}");
    }
    // 30 days before the warranty ends: an issue for the owner; a new laptop gets set up
    post(
        &admin,
        rules.clone(),
        json!({"name": "Warranty", "trigger": {"event": "date", "fieldId": field, "days": 30, "typeId": laptop},
               "actions": [{"action": "createIssue", "projectKey": "it", "title": "Warranty of {tag} {name} ends on {date}", "description": "Owner: {owner}. Until {field:Warranty ends}.", "assignOwner": true},
                           {"action": "notifyOwner", "message": "The warranty of {tag} ends on {date}"}]}),
    )
    .await;
    post(&admin, rules.clone(), json!({"name": "Set up", "trigger": {"event": "created"}, "actions": [{"action": "createIssue", "projectKey": "IT", "title": "Set up {tag} ({type})"}]})).await;
    post(
        &admin,
        rules.clone(),
        json!({"name": "Retire", "trigger": {"event": "changed", "status": "maintenance"}, "actions": [{"action": "setStatus", "status": "retired"}]}),
    )
    .await;
    post(
        &admin,
        rules.clone(),
        json!({"name": "Lost project", "trigger": {"event": "created"}, "actions": [{"action": "createIssue", "projectKey": "NOPE", "title": "x"}]}),
    )
    .await;

    let day = |n: i64| (chrono::Utc::now().date_naive() + chrono::Duration::days(n)).to_string();
    post(&mia, app.url("/assets"), json!({"typeId": laptop, "name": "Soon", "ownerId": mia_id, "values": {"Warranty ends": day(12)}})).await;
    post(&mia, app.url("/assets"), json!({"typeId": laptop, "name": "Later", "values": {"Warranty ends": day(200)}})).await;
    post(&mia, app.url("/assets"), json!({"typeId": laptop, "name": "Past", "values": {"Warranty ends": day(-5)}})).await;
    // created: an issue each, linked to its asset
    assert_eq!(search(&admin, &app, "title ~ \"Set up\"").await.len(), 3);
    assert_eq!(search(&admin, &app, "asset = \"LT-0001\"").await.len(), 1);

    // the date rule: once, for the one that's due
    taskcraft::assets::tick(&app.state).await.unwrap();
    taskcraft::assets::tick(&app.state).await.unwrap();
    let due = search(&admin, &app, "title ~ Warranty").await;
    assert_eq!(due.len(), 1, "once, and only the laptop whose date is within 30 days");
    let issue = get(&admin, app.url(&format!("/issues/{}", due[0]))).await;
    assert_eq!(
        (issue["title"].as_str(), issue["assignee"]["name"].as_str()),
        (Some(format!("Warranty of LT-0001 Soon ends on {}", day(12)).as_str()), Some("Mia Member"))
    );
    assert_eq!(issue["assets"][0]["tag"], "LT-0001");
    let n = get(&mia, app.url("/notifications")).await;
    assert!(
        n["items"].as_array().unwrap().iter().any(|x| x["kind"] == "automation" && x["data"]["message"].as_str().unwrap().contains("LT-0001 ends on")),
        "{n}"
    );
    // a new date fires again
    assert_eq!(send(&mia, Method::PATCH, app.url("/assets/LT-0002"), json!({"values": {"Warranty ends": day(3)}})).await.0, 204);
    taskcraft::assets::tick(&app.state).await.unwrap();
    assert_eq!(search(&admin, &app, "title ~ Warranty").await.len(), 2);

    // changed: into maintenance → retired by the rule; what goes wrong shows on the rule
    assert_eq!(send(&mia, Method::PATCH, app.url("/assets/LT-0003"), json!({"status": "maintenance"})).await.0, 204);
    assert_eq!(get(&mia, app.url("/assets/LT-0003")).await["status"], "retired");
    let list = get(&admin, rules).await;
    let by = |name: &str| list.as_array().unwrap().iter().find(|r| r["name"] == name).unwrap().clone();
    assert_eq!((by("Warranty")["runs"].as_i64(), by("Set up")["runs"].as_i64(), by("Retire")["runs"].as_i64()), (Some(2), Some(3), Some(1)));
    assert!(by("Lost project")["lastError"].as_str().unwrap().contains("no project NOPE"));
    let rule = by("Set up")["id"].as_str().unwrap().to_string();
    assert_eq!(send(&admin, Method::PATCH, app.url(&format!("/asset-rules/{rule}")), json!({"enabled": false})).await.0, 204);
    post(&mia, app.url("/assets"), json!({"typeId": laptop, "name": "Quiet"})).await;
    assert_eq!(search(&admin, &app, "title ~ \"Set up\"").await.len(), 3);
    assert_eq!(send(&admin, Method::DELETE, app.url(&format!("/asset-rules/{rule}")), Value::Null).await.0, 204);
}
