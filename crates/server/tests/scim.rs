//! SCIM provisioning, driven by a mock identity provider that talks the way the common ones do
//! (Okta: look up by userName, create, PUT and PATCH with values; Microsoft Entra: PATCH with
//! paths and "True" / "False" strings, members removed by path): people are created, taken over,
//! updated and deactivated — never deleted — and deactivation ends sessions and revokes API
//! tokens at once; groups become teams; everything is audited and errors show for admins.

mod common;

use common::*;
use reqwest::Method;
use serde_json::{Value, json};

/// The identity provider's side: SCIM requests with its bearer token.
struct Directory {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Directory {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut r = self.http.request(method, format!("{}/scim/v2{path}", self.base)).bearer_auth(&self.token);
        if let Some(b) = body {
            r = r.header("content-type", "application/scim+json").body(b.to_string());
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        if status != 204 {
            assert_eq!(r.headers()["content-type"], "application/scim+json", "{path}");
        }
        (status, r.json().await.unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str) -> (u16, Value) {
        self.call(Method::GET, path, None).await
    }
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call(Method::POST, path, Some(body)).await
    }
    async fn patch(&self, path: &str, ops: Value) -> (u16, Value) {
        self.call(Method::PATCH, path, Some(json!({"schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"], "Operations": ops}))).await
    }
    /// How an identity provider checks whether someone exists before creating them.
    async fn find_user(&self, user_name: &str) -> Value {
        let (s, v) = self.get(&format!("/Users?filter=userName%20eq%20%22{}%22&startIndex=1&count=100", user_name.replace('@', "%40"))).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

async fn directory(app: &TestApp, admin: &reqwest::Client, name: &str) -> (Directory, String) {
    let (s, v) = send(admin, Method::POST, app.url("/admin/scim"), json!({"name": name})).await;
    assert_eq!(s, 201, "{v}");
    (Directory { base: app.base.clone(), token: v["token"].as_str().unwrap().to_string(), http: reqwest::Client::new() }, v["id"].as_str().unwrap().to_string())
}

fn user(user_name: &str, given: &str, family: &str, external: &str) -> Value {
    json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"], "userName": user_name, "externalId": external,
        "name": {"givenName": given, "familyName": family}, "emails": [{"primary": true, "value": user_name, "type": "work"}], "active": true,
    })
}

async fn status_of(app: &TestApp, id: &str) -> (String, String) {
    sqlx::query_as("SELECT status, name FROM users WHERE id = $1::uuid").bind(id).fetch_one(&app.state.db).await.unwrap()
}

#[tokio::test]
async fn people_are_created_taken_over_updated_and_deactivated() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    let (tom, tom_id) = app.user(&owner, "Tom Teammate", "tom@acme.test", "member").await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web", "template": "kanban"})).await;
    post(&tom, app.url("/issues"), json!({"projectKey": "WEB", "title": "Tom's issue"})).await;
    let tom_token = post(&tom, app.url("/me/tokens"), json!({"name": "script", "scope": "read"})).await["token"].as_str().unwrap().to_string();

    // tokens are the admins' to make; without one nothing answers
    assert_eq!(send(&tom, Method::POST, app.url("/admin/scim"), json!({"name": "Nope"})).await.0, 403);
    assert_eq!(send(&owner, Method::POST, app.url("/admin/scim"), json!({"name": "X", "providerId": "nope"})).await.0, 400);
    let (okta, okta_id) = directory(&app, &owner, "Okta").await;
    let anon = Directory { base: app.base.clone(), token: "tcscim_made-up".into(), http: reqwest::Client::new() };
    let (s, v) = anon.get("/Users").await;
    assert!(s == 401 && v["schemas"][0] == "urn:ietf:params:scim:api:messages:2.0:Error", "{v}");
    // an API token of a person isn't one either
    assert_eq!(Directory { base: app.base.clone(), token: tom_token.clone(), http: reqwest::Client::new() }.get("/Users").await.0, 401);
    let (s, v) = okta.get("/ServiceProviderConfig").await;
    assert!(s == 200 && v["patch"]["supported"] == true && v["filter"]["supported"] == true, "{v}");
    assert_eq!(okta.get("/ResourceTypes").await.1["totalResults"], 2);
    assert_eq!(okta.get("/Schemas").await.1["totalResults"], 2);

    // someone new: looked up, then created — active, a member, without a password
    assert_eq!(okta.find_user("mia@acme.test").await["totalResults"], 0);
    let (s, mia) = okta.post("/Users", user("mia@acme.test", "Mia", "Member", "00u1")).await;
    assert_eq!(s, 201, "{mia}");
    let mia_id = mia["id"].as_str().unwrap().to_string();
    assert_eq!((mia["userName"].as_str(), mia["active"].as_bool(), mia["externalId"].as_str()), (Some("mia@acme.test"), Some(true), Some("00u1")));
    assert_eq!((mia["name"]["formatted"].as_str(), mia["emails"][0]["value"].as_str()), (Some("Mia Member"), Some("mia@acme.test")));
    assert_eq!(mia["meta"]["location"], format!("{}/scim/v2/Users/{mia_id}", app.base));
    let found = okta.find_user("MIA@acme.test").await;
    assert_eq!((found["totalResults"].as_i64(), found["Resources"][0]["id"].as_str()), (Some(1), Some(mia_id.as_str())));
    let people = get(&owner, app.url("/users")).await;
    let row = people.as_array().unwrap().iter().find(|u| u["email"] == "mia@acme.test").expect("she has an account");
    assert_eq!((row["role"].as_str(), row["status"].as_str()), (Some("member"), Some("active")));
    let (s, v) = okta.post("/Users", user("mia@acme.test", "Mia", "Member", "00u1")).await;
    assert!(s == 409 && v["scimType"] == "uniqueness", "{v}");
    assert_eq!(okta.post("/Users", json!({"name": {"givenName": "No"}})).await.0, 400);

    // someone who has an account already is taken over by their address, not doubled
    let (s, t) = okta.post("/Users", user("tom@acme.test", "Thomas", "Teammate", "00u2")).await;
    assert_eq!((s, t["id"].as_str()), (201, Some(tom_id.as_str())), "{t}");
    assert_eq!(status_of(&app, &tom_id).await, ("active".into(), "Thomas Teammate".into()));
    assert_eq!(get(&tom, app.url("/me")).await["user"]["name"], "Thomas Teammate", "still signed in");

    // Okta replaces (PUT); Entra patches by path, or with a value whose keys are paths
    let mut changed = user("mia.member@acme.test", "Mia", "Member-Jones", "00u1");
    changed["emails"][0]["value"] = json!("mia.member@acme.test");
    let (s, v) = okta.call(Method::PUT, &format!("/Users/{mia_id}"), Some(changed)).await;
    assert_eq!((s, v["userName"].as_str(), v["name"]["formatted"].as_str()), (200, Some("mia.member@acme.test"), Some("Mia Member-Jones")), "{v}");
    let (s, v) = okta.patch(&format!("/Users/{mia_id}"), json!([{"op": "Replace", "path": "name.givenName", "value": "Maria"}, {"op": "Add", "path": "emails[type eq \"work\"].value", "value": "maria@acme.test"}])).await;
    assert_eq!((s, v["displayName"].as_str(), v["emails"][0]["value"].as_str()), (200, Some("Maria Member-Jones"), Some("maria@acme.test")), "{v}");
    let (s, v) = okta.patch(&format!("/Users/{mia_id}"), json!([{"op": "replace", "value": {"name.familyName": "Member", "externalId": "00u1b"}}])).await;
    assert_eq!((s, v["name"]["formatted"].as_str(), v["externalId"].as_str()), (200, Some("Maria Member"), Some("00u1b")), "{v}");
    // an address someone else has
    let (s, v) = okta.patch(&format!("/Users/{mia_id}"), json!([{"op": "replace", "path": "emails[type eq \"work\"].value", "value": "tom@acme.test"}])).await;
    assert!(s == 409 && v["detail"].as_str().unwrap().contains("someone else"), "{v}");

    // Tom leaves the company: deactivated at once — his session and his API token stop working
    let api = reqwest::Client::new();
    assert_eq!(api.get(format!("{}/api/v1/me", app.base)).bearer_auth(&tom_token).send().await.unwrap().status(), 200);
    let (s, v) = okta.patch(&format!("/Users/{tom_id}"), json!([{"op": "Replace", "path": "active", "value": "False"}])).await;
    assert_eq!((s, v["active"].as_bool()), (200, Some(false)), "{v}");
    assert_eq!(status_of(&app, &tom_id).await.0, "deactivated");
    assert!(get(&tom, app.url("/me")).await["user"].is_null(), "his session ended");
    assert_eq!(api.get(format!("{}/api/v1/issues/WEB-1", app.base)).bearer_auth(&tom_token).send().await.unwrap().status(), 401, "his token is revoked");
    let r = client().post(app.url("/auth/login")).json(&json!({"email": "tom@acme.test", "password": "password123"})).send().await.unwrap();
    assert_ne!(r.status(), 200, "and he can't sign in");
    // his work stays
    assert_eq!(get(&owner, app.url("/issues/WEB-1")).await["reporter"]["name"], "Thomas Teammate");
    // ... and he can come back
    let (s, v) = okta.patch(&format!("/Users/{tom_id}"), json!([{"op": "replace", "value": {"active": true}}])).await;
    assert_eq!((s, v["active"].as_bool()), (200, Some(true)), "{v}");
    assert_eq!(status_of(&app, &tom_id).await.0, "active");

    // DELETE deactivates too: the account and its history stay, the directory's entry goes
    assert_eq!(okta.call(Method::DELETE, &format!("/Users/{tom_id}"), None).await.0, 204);
    assert_eq!(okta.get(&format!("/Users/{tom_id}")).await.0, 404);
    assert_eq!(status_of(&app, &tom_id).await.0, "deactivated");
    assert_eq!(get(&owner, app.url("/issues/WEB-1")).await["reporter"]["name"], "Thomas Teammate");

    // the owner can be taken over, but never locked out
    let (s, o) = okta.post("/Users", user("owner@example.com", "Olivia", "Owner", "00u0")).await;
    assert_eq!(s, 201, "{o}");
    let (s, v) = okta.patch(&format!("/Users/{}", o["id"].as_str().unwrap()), json!([{"op": "replace", "path": "active", "value": false}])).await;
    assert!(s == 403 && v["detail"].as_str().unwrap().contains("owner"), "{v}");
    assert_eq!(get(&owner, app.url("/me")).await["user"]["name"], "Olivia Owner");

    // another directory sees only its own people, and can't take over someone else's
    let (entra, _) = directory(&app, &owner, "Entra").await;
    assert_eq!(entra.get("/Users").await.1["totalResults"], 0);
    assert_eq!(entra.get(&format!("/Users/{mia_id}")).await.0, 404);
    assert_eq!(entra.post("/Users", user("maria@acme.test", "Maria", "Member", "x")).await.0, 409);
    assert_eq!(okta.get("/Users?count=1").await.1["totalResults"], 2);
    assert_eq!(okta.get("/Users?filter=externalId%20eq%20%2200u1b%22").await.1["Resources"][0]["id"], mia_id.as_str());
    assert_eq!(okta.get("/Users?filter=userName%20co%20%22mia%22").await.0, 400);

    // admins see what the directory did, and what went wrong last
    let page = get(&owner, app.url("/admin/scim")).await;
    assert_eq!(page["baseUrl"], format!("{}/scim/v2", app.base));
    let t = page["tokens"].as_array().unwrap().iter().find(|t| t["id"] == okta_id.as_str()).unwrap();
    assert_eq!((t["users"].as_i64(), t["name"].as_str()), (Some(2), Some("Okta")), "{t}");
    assert!(t["requests"].as_i64().unwrap() > 10 && !t["lastUsedAt"].is_null());
    assert!(t["lastError"].as_str().unwrap().contains("400"), "{t}");
    assert!(page.to_string().find("tcscim_").is_none(), "tokens are shown once");
    let audit = get(&owner, app.url("/admin/audit")).await.to_string();
    for action in ["scim.token.created", "scim.user.created", "scim.user.linked", "scim.user.updated", "scim.user.deactivated", "scim.user.reactivated"] {
        assert!(audit.contains(action), "{action} in the audit log");
    }

    // a deleted token is turned away; what it provisioned stays
    assert_eq!(send(&tom, Method::DELETE, app.url(&format!("/admin/scim/{okta_id}")), Value::Null).await.0, 401);
    assert_eq!(send(&owner, Method::DELETE, app.url(&format!("/admin/scim/{okta_id}")), Value::Null).await.0, 204);
    assert_eq!(okta.get("/Users").await.0, 401);
    assert_eq!(status_of(&app, &mia_id).await, ("active".into(), "Maria Member".into()));
}

#[tokio::test]
async fn groups_become_teams() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    // a team of the workspace's own, with the name a group will have
    let local = post(&owner, app.url("/teams"), json!({"name": "Design"})).await;
    let (entra, token_id) = directory(&app, &owner, "Entra").await;
    let mut ids = Vec::new();
    for (n, (given, mail)) in [("Mia", "mia@acme.test"), ("Tom", "tom@acme.test"), ("Ann", "ann@acme.test")].iter().enumerate() {
        let (s, v) = entra.post("/Users", user(mail, given, "Example", &format!("e{n}"))).await;
        assert_eq!(s, 201, "{v}");
        ids.push(v["id"].as_str().unwrap().to_string());
    }
    let members = |app: &TestApp, team: String| {
        let db = app.state.db.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT u.name FROM team_members m JOIN users u ON u.id = m.user_id WHERE m.team_id = $1::uuid ORDER BY u.name")
                .bind(team)
                .fetch_all(&db)
                .await
                .unwrap()
        }
    };

    // Entra creates the group empty, then adds members
    assert_eq!(entra.get("/Groups?excludedAttributes=members&filter=displayName%20eq%20%22Engineering%22").await.1["totalResults"], 0);
    let (s, g) =
        entra.post("/Groups", json!({"schemas": ["urn:ietf:params:scim:schemas:core:2.0:Group"], "displayName": "Engineering", "externalId": "g-1"})).await;
    assert_eq!((s, g["displayName"].as_str(), g["members"].as_array().map(Vec::len)), (201, Some("Engineering"), Some(0)), "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    let (s, v) = entra.patch(&format!("/Groups/{gid}"), json!([{"op": "Add", "path": "members", "value": [{"value": ids[0]}, {"value": ids[1]}]}])).await;
    assert_eq!((s, v["members"].as_array().map(Vec::len)), (200, Some(2)), "{v}");
    assert_eq!(members(&app, gid.clone()).await, ["Mia Example", "Tom Example"]);
    let teams = get(&owner, app.url("/teams")).await;
    let team = teams.as_array().unwrap().iter().find(|t| t["name"] == "Engineering").expect("the group is a team");
    assert_eq!(team["id"], gid.as_str());
    assert_eq!(entra.get(&format!("/Users/{}", ids[0])).await.1["groups"][0]["display"], "Engineering");
    // unknown people aren't members
    let (s, v) = entra.patch(&format!("/Groups/{gid}"), json!([{"op": "add", "path": "members", "value": [{"value": uuid::Uuid::new_v4()}]}])).await;
    assert_eq!(s, 400, "{v}");

    // members leave by path (Entra) or by value (Okta); the group is renamed
    let (s, _) = entra.patch(&format!("/Groups/{gid}"), json!([{"op": "Remove", "path": format!("members[value eq \"{}\"]", ids[0])}])).await;
    assert_eq!(s, 200);
    assert_eq!(members(&app, gid.clone()).await, ["Tom Example"]);
    let (s, _) = entra.patch(&format!("/Groups/{gid}"), json!([{"op": "remove", "path": "members", "value": [{"value": ids[1]}]}, {"op": "add", "value": {"members": [{"value": ids[2]}]}}, {"op": "replace", "value": {"displayName": "Platform"}}]))
        .await;
    assert_eq!(s, 200);
    assert_eq!(members(&app, gid.clone()).await, ["Ann Example"]);
    assert_eq!(entra.get(&format!("/Groups/{gid}")).await.1["displayName"], "Platform");
    // PUT sets it all
    let (s, v) = entra
        .call(
            Method::PUT,
            &format!("/Groups/{gid}"),
            Some(json!({"displayName": "Platform", "members": [{"value": ids[0]}, {"value": ids[1]}, {"value": ids[2]}]})),
        )
        .await;
    assert_eq!((s, v["members"].as_array().map(Vec::len)), (200, Some(3)), "{v}");
    assert_eq!(entra.get(&format!("/Groups?filter=members.value%20eq%20%22{}%22", ids[2])).await.1["totalResults"], 1);

    // a local team of the same name is never taken over
    let (s, d) = entra.post("/Groups", json!({"displayName": "Design", "members": [{"value": ids[0]}]})).await;
    assert_eq!(s, 201, "{d}");
    assert_ne!(d["id"], local["id"]);
    assert_eq!(d["displayName"], "Design (SCIM)");
    assert!(!members(&app, local["id"].as_str().unwrap().to_string()).await.contains(&"Mia Example".to_string()));
    assert_eq!(entra.post("/Groups", json!({"displayName": "Design"})).await.0, 409);
    assert_eq!(entra.get(&format!("/Groups/{}", local["id"].as_str().unwrap())).await.0, 404, "not this directory's");

    // someone deprovisioned leaves the directory's teams; a deleted group's team goes, its people stay
    assert_eq!(entra.call(Method::DELETE, &format!("/Users/{}", ids[2]), None).await.0, 204);
    assert_eq!(members(&app, gid.clone()).await, ["Mia Example", "Tom Example"]);
    assert_eq!(entra.call(Method::DELETE, &format!("/Groups/{gid}"), None).await.0, 204);
    assert_eq!(entra.get(&format!("/Groups/{gid}")).await.0, 404);
    assert!(!get(&owner, app.url("/teams")).await.as_array().unwrap().iter().any(|t| t["name"] == "Platform"));
    assert_eq!(entra.get(&format!("/Users/{}", ids[0])).await.1["active"], true);
    let page = get(&owner, app.url("/admin/scim")).await;
    let t = page["tokens"].as_array().unwrap().iter().find(|t| t["id"] == token_id.as_str()).unwrap();
    assert_eq!((t["users"].as_i64(), t["groups"].as_i64()), (Some(2), Some(1)), "{t}");
    let audit = get(&owner, app.url("/admin/audit")).await.to_string();
    for action in ["scim.group.created", "scim.group.updated", "scim.group.deleted"] {
        assert!(audit.contains(action), "{action} in the audit log");
    }
}
