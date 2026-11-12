//! Migration from an XML site backup: a small synthetic backup (written here) with people,
//! renamed projects, issues, history, comments, time logs, attachments, links, sprints and more.

mod common;

use std::io::Write;

use chrono::{DateTime, Duration, Utc};
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use taskcraft::site_backup::{self, Choice, Options, People, Source};

/// A time `hours` after `base`, as the backup writes it.
fn t(base: DateTime<Utc>, hours: f64) -> String {
    (base + Duration::minutes((hours * 60.0) as i64)).format("%Y-%m-%d %H:%M:%S.0").to_string()
}

fn ms(base: DateTime<Utc>, hours: f64) -> i64 {
    (base + Duration::minutes((hours * 60.0) as i64)).timestamp_millis()
}

/// entities.xml and activeobjects.xml of a site with the projects WEB (once called SHOP) and
/// OPS. `base` is when WEB-2 was created.
fn backup(base: DateTime<Utc>) -> (String, String) {
    let b = |h: f64| t(base, h);
    let entities = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- a synthetic XML site backup -->
<entity-engine-xml>
    <OSPropertyEntry id="1" entityName="site.properties" entityId="1" propertyKey="site.sid.key" type="5"/>
    <OSPropertyString id="1" value="TEST-SITE-0001"/>
    <User id="1" directoryId="1" userName="alice" lowerUserName="alice" active="1" displayName="Alice Admin" emailAddress="ALICE@example.com" externalId="acc:alice"/>
    <User id="2" directoryId="1" userName="bob" lowerUserName="bob" active="1" displayName="Bob Builder" emailAddress="bob@example.com" externalId="acc:bob"/>
    <User id="3" directoryId="1" userName="carol" lowerUserName="carol" active="0" displayName="Carol Former" emailAddress="carol@example.com" externalId="acc:carol"/>
    <ApplicationUser id="1" userKey="ukey-alice" lowerUserName="alice"/>
    <ApplicationUser id="2" userKey="ukey-bob" lowerUserName="bob"/>
    <ApplicationUser id="3" userKey="ukey-carol" lowerUserName="carol"/>
    <Project id="10000" name="Web shop" lead="ukey-alice" key="WEB" originalkey="SHOP" projecttype="software" counter="6"/>
    <Project id="10001" name="Operations" lead="ukey-bob" key="OPS" originalkey="OPS" projecttype="business" counter="1"/>
    <ProjectKey id="1" projectId="10000" projectKey="SHOP"/>
    <ProjectKey id="2" projectId="10000" projectKey="WEB"/>
    <IssueType id="1" name="Bug" hierarchy_level="0"/>
    <IssueType id="2" name="Story" hierarchy_level="0"/>
    <IssueType id="3" name="Epic" hierarchy_level="1"/>
    <IssueType id="4" name="Sub-task" style="subtask"/>
    <IssueType id="5" name="Improvement" hierarchy_level="0"/>
    <Status id="1" name="Open" statuscategory="2"/>
    <Status id="3" name="In Progress" statuscategory="4"/>
    <Status id="6" name="Closed" statuscategory="3"/>
    <Status id="10001" name="Waiting for QA" statuscategory="4"/>
    <Priority id="1" name="Highest" sequence="1"/>
    <Priority id="2" name="High" sequence="2"/>
    <Priority id="3" name="Medium" sequence="3"/>
    <Priority id="4" name="Low" sequence="4"/>
    <Priority id="5" name="Lowest" sequence="5"/>
    <Resolution id="1" name="Fixed" sequence="1"/>
    <IssueSecurityLevel id="10100" name="Staff only" description="" scheme="10000"/>
    <Issue id="100" projectKey="SHOP" number="1" project="10000" reporter="ukey-alice" creator="ukey-alice" type="3" summary="Checkout" priority="3" status="3" created="{e}" updated="{e}"/>
    <Issue id="101" projectKey="SHOP" number="2" project="10000" reporter="ukey-alice" assignee="ukey-bob" creator="ukey-alice" type="2" summary="Pay by   card" priority="2" status="6" resolution="1" resolutiondate="{r}" created="{c}" updated="{r}" duedate="{due}" timeoriginalestimate="7200" timeestimate="1800" timespent="5400">
        <description><![CDATA[h2. Goal
Pay with a card, asked by [~ukey-alice].
!card.png|thumbnail!
See [the spec|https://example.com/spec] and [^gone.pdf].]]></description>
    </Issue>
    <Issue id="102" projectKey="SHOP" number="3" project="10000" reporter="ukey-bob" assignee="ukey-carol" creator="ukey-bob" type="1" summary="Card form loses input" priority="1" status="10001" security="10100" created="{c3}" updated="{c4}"/>
    <Issue id="103" projectKey="SHOP" number="5" project="10000" reporter="ukey-bob" creator="ukey-bob" type="4" summary="Card form layout" priority="3" status="1" created="{c3}" updated="{c3}"/>
    <Issue id="104" projectKey="WEB" number="6" project="10000" reporter="ukey-bob" creator="ukey-bob" type="5" summary="Faster checkout" priority="4" status="1" created="{c4}" updated="{c4}"/>
    <Issue id="200" projectKey="OPS" number="1" project="10001" reporter="ukey-bob" creator="ukey-bob" type="1" summary="Payment gateway timeouts" priority="2" status="1" created="{c}" updated="{c}"/>
    <Label id="1" issue="101" label="checkout"/>
    <Label id="2" issue="101" label="Payments"/>
    <Label id="3" issue="101" fieldid="10500" label="not-a-label"/>
    <Component id="400" project="10000" name="Frontend" lead="ukey-bob"/>
    <Version id="500" project="10000" name="1.0" sequence="1" released="true" releasedate="{c4}"/>
    <NodeAssociation sourceNodeId="101" sourceNodeEntity="Issue" sinkNodeId="400" sinkNodeEntity="Component" associationType="IssueComponent"/>
    <NodeAssociation sourceNodeId="101" sourceNodeEntity="Issue" sinkNodeId="500" sinkNodeEntity="Version" associationType="IssueFixVersion"/>
    <NodeAssociation sourceNodeId="101" sourceNodeEntity="Issue" sinkNodeId="500" sinkNodeEntity="Version" associationType="IssueVersion"/>
    <UserAssociation sourceName="ukey-alice" sinkNodeId="101" sinkNodeEntity="Issue" associationType="WatchIssue"/>
    <UserAssociation sourceName="ukey-bob" sinkNodeId="101" sinkNodeEntity="Issue" associationType="VoteIssue"/>
    <Action id="700" issue="101" author="ukey-bob" type="comment" created="{h26}" updated="{h26}"><body>Started, thanks [~accountid:acc:alice] &amp; team.</body></Action>
    <Action id="701" issue="101" author="ukey-carol" type="comment" level="developers" created="{h30}" updated="{h50}" body="Internal: *card* provider chosen"/>
    <Action id="702" issue="101" author="ukey-bob" type="comment" created="{h31}"><body>   </body></Action>
    <Action id="703" issue="101" author="ukey-bob" type="worklog" created="{h31}"><body>not a comment</body></Action>
    <Worklog id="800" issue="101" author="ukey-bob" body="Wrote the *card* form" created="{h25}" startdate="{h25}" timeworked="5400"/>
    <Worklog id="801" issue="102" author="ukey-carol" created="{h80}" startdate="{h80}" timeworked="0"/>
    <FileAttachment id="300" issue="101" mimetype="image/png" filename="card.png" filesize="12" author="ukey-bob" created="{h2}"/>
    <FileAttachment id="301" issue="101" mimetype="application/pdf" filename="gone.pdf" filesize="99" author="ukey-bob" created="{h2}"/>
    <IssueLinkType id="10000" linkname="Blocks" inward="is blocked by" outward="blocks"/>
    <IssueLinkType id="10001" linkname="subtask-link" inward="is a sub-task of" outward="has the sub-task" style="subtask"/>
    <IssueLinkType id="10002" linkname="Epic link" inward="belongs to the epic" outward="holds"/>
    <IssueLinkType id="10003" linkname="Causes" inward="is caused by" outward="causes"/>
    <IssueLink id="900" linktype="10000" source="200" destination="102" created="{h3}"/>
    <IssueLink id="901" linktype="10001" source="101" destination="103" created="{c3}"/>
    <IssueLink id="902" linktype="10002" source="100" destination="101" created="{c}"/>
    <IssueLink id="903" linktype="10003" source="102" destination="104" created="{c4}"/>
    <IssueParentAssociation issueId="102" parentId="100"/>
    <CustomField id="600" customfieldtypekey="com.example.agile:board-sprint" name="Sprint"/>
    <CustomField id="601" customfieldtypekey="com.example.fields:float" name="Story Points"/>
    <CustomField id="602" customfieldtypekey="com.example.fields:select" name="Severity"/>
    <CustomFieldValue id="1" issue="101" customfield="600" numbervalue="1"/>
    <CustomFieldValue id="2" issue="101" customfield="601" numbervalue="5"/>
    <CustomFieldValue id="3" issue="102" customfield="600" numbervalue="1"/>
    <CustomFieldValue id="4" issue="102" customfield="600" numbervalue="2"/>
    <CustomFieldValue id="5" issue="102" customfield="602" stringvalue="10100"/>
    <ChangeGroup id="1000" issue="101" author="ukey-alice" created="{c}" initialgroup="true"/>
    <ChangeItem id="1" group="1000" fieldtype="sys" field="Status" newvalue="1" newstring="Open"/>
    <ChangeItem id="2" group="1000" fieldtype="sys" field="priority" newvalue="3" newstring="Medium"/>
    <ChangeGroup id="1001" issue="101" author="ukey-alice" created="{h24}"/>
    <ChangeItem id="3" group="1001" fieldtype="sys" field="status" oldvalue="1" oldstring="Open" newvalue="3" newstring="In Progress"/>
    <ChangeItem id="4" group="1001" fieldtype="sys" field="assignee" newvalue="acc:bob" newstring="Bob Builder"/>
    <ChangeItem id="5" group="1001" fieldtype="custom" field="Sprint" newvalue="1" newstring="Sprint 1"/>
    <ChangeItem id="6" group="1001" fieldtype="custom" field="Story Points" oldstring="3" newstring="5"/>
    <ChangeGroup id="1002" issue="101" author="ukey-bob" created="{h25_1}"/>
    <ChangeItem id="7" group="1002" fieldtype="sys" field="WorklogId" newvalue="800"/>
    <ChangeItem id="8" group="1002" fieldtype="sys" field="timeestimate" oldvalue="7200" newvalue="1800"/>
    <ChangeGroup id="1003" issue="101" author="ukey-bob" created="{h48}"/>
    <ChangeItem id="9" group="1003" fieldtype="sys" field="timeoriginalestimate" oldvalue="3600" newvalue="7200"/>
    <ChangeItem id="10" group="1003" fieldtype="sys" field="priority" oldvalue="3" oldstring="Medium" newvalue="2" newstring="High"/>
    <ChangeItem id="11" group="1003" fieldtype="sys" field="summary" oldstring="Pay by card" newstring="Pay by card"/>
    <ChangeGroup id="1004" issue="101" author="ukey-bob" created="{r}"/>
    <ChangeItem id="12" group="1004" fieldtype="sys" field="status" oldvalue="3" oldstring="In Progress" newvalue="6" newstring="Closed"/>
    <ChangeItem id="13" group="1004" fieldtype="sys" field="resolution" newvalue="1" newstring="Fixed"/>
    <ChangeGroup id="1005" issue="102" author="acc:carol" created="{h80}"/>
    <ChangeItem id="14" group="1005" fieldtype="sys" field="status" oldvalue="1" oldstring="Open" newvalue="10001" newstring="Waiting for QA"/>
    <ChangeItem id="15" group="1005" fieldtype="sys" field="assignee" oldvalue="acc:bob" oldstring="Bob Builder" newvalue="acc:carol" newstring="Carol Former"/>
    <ChangeGroup id="1006" issue="104" author="ukey-bob" created="{c4}"/>
    <ChangeItem id="16" group="1006" fieldtype="sys" field="Key" oldvalue="OPS-7" oldstring="OPS-7" newvalue="WEB-6" newstring="WEB-6"/>
    <Unused id="1"><nested>ignored</nested></Unused>
</entity-engine-xml>
"#,
        e = b(-48.0),
        c = b(0.0),
        c3 = b(50.0),
        c4 = b(72.0),
        r = b(104.0),
        due = t(base, 240.0),
        h2 = b(2.0),
        h3 = b(3.0),
        h24 = b(24.0),
        h25 = b(25.0),
        h25_1 = b(25.1),
        h26 = b(26.0),
        h30 = b(30.0),
        h31 = b(31.0),
        h48 = b(48.0),
        h50 = b(50.0),
        h80 = b(80.0),
    );
    let ao = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><backup xmlns="http://www.example.com/ao" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><database><meta key="x" value="y"/></database>
<table name="AO_0A1B2C_SPRINT"><column name="ID" primaryKey="true"/></table>
<data tableName="AO_0A1B2C_SPRINT"><column name="CLOSED"/><column name="COMPLETE_DATE"/><column name="END_DATE"/><column name="GOAL"/><column name="ID"/><column name="NAME"/><column name="SEQUENCE"/><column name="STARTED"/><column name="START_DATE"/>
<row><boolean>true</boolean><integer>{done}</integer><integer>{end}</integer><string>Take payments</string><integer>1</integer><string>Sprint 1</string><integer>1</integer><boolean>true</boolean><integer>{start}</integer></row>
<row><boolean>false</boolean><integer xsi:nil="true"/><integer>{end2}</integer><string xsi:nil="true"/><integer>2</integer><string>Sprint 2</string><integer>2</integer><boolean>true</boolean><integer>{start2}</integer></row>
</data>
<data tableName="AO_0A1B2C_LEXORANK"><column name="FIELD_ID"/><column name="ID"/><column name="ISSUE_ID"/><column name="RANK"/>
<row><integer>10019</integer><integer>1</integer><integer>101</integer><string>0|b</string></row>
<row><integer>10019</integer><integer>2</integer><integer>102</integer><string>0|a</string></row>
</data>
<data tableName="AO_0A1B2C_OTHER"><column name="ID"/><row><integer>1</integer></row></data>
</backup>"#,
        start = ms(base, -24.0),
        end = ms(base, 13.0 * 24.0),
        done = ms(base, 13.0 * 24.0),
        start2 = ms(base, 14.0 * 24.0),
        end2 = ms(base, 28.0 * 24.0),
    );
    (entities, ao)
}

const PNG: &[u8] = b"\x89PNG-bytes!!";

/// The backup unpacked into a folder (attachments under the project's first key).
fn write_dir(base: DateTime<Utc>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let (e, a) = backup(base);
    std::fs::write(dir.path().join("entities.xml"), e).unwrap();
    std::fs::write(dir.path().join("activeobjects.xml"), a).unwrap();
    let files = dir.path().join("data/attachments/SHOP/10000/SHOP-2");
    std::fs::create_dir_all(files.join("thumbs")).unwrap();
    std::fs::write(files.join("300"), PNG).unwrap();
    std::fs::write(files.join("thumbs/300"), b"thumbnail").unwrap();
    dir
}

/// The backup as a zip, as it is downloaded.
fn zip_bytes(base: DateTime<Utc>) -> Vec<u8> {
    let (e, a) = backup(base);
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    w.start_file("entities.xml", opts).unwrap();
    w.write_all(e.as_bytes()).unwrap();
    w.start_file("activeobjects.xml", opts).unwrap();
    w.write_all(a.as_bytes()).unwrap();
    w.start_file("data/attachments/SHOP/10000/SHOP-2/300", opts).unwrap();
    w.write_all(PNG).unwrap();
    w.finish().unwrap().into_inner()
}

async fn owner(app: &TestApp) -> taskcraft::auth::User {
    sqlx::query_as("SELECT id, workspace_id, email, name, role, status FROM users WHERE role = 'owner'").fetch_one(&app.state.db).await.unwrap()
}

fn warned(r: &site_backup::Report, part: &str) -> bool {
    r.warnings.iter().any(|w| w.message.contains(part))
}

async fn search(c: &reqwest::Client, app: &TestApp, q: &str) -> Value {
    c.get(app.url("/search")).query(&[("q", q)]).send().await.unwrap().json().await.unwrap()
}

#[tokio::test]
async fn backup_import_brings_everything_over() {
    let app = spawn().await;
    let c = app.setup(false).await;
    // Alice has an account already (matched by e-mail address, whatever its case)
    let (alice, alice_id) = app.user(&c, "Alice A.", "alice@example.com", "member").await;
    let owner = owner(&app).await;
    let base = Utc::now() - Duration::days(30);
    let dir = write_dir(base);
    let mut src = Source::open(dir.path(), None).unwrap();
    let backup = site_backup::load(&mut src, chrono_tz::UTC).unwrap();
    assert_eq!(backup.issues.len(), 6);
    assert_eq!(backup.comments.len(), 3, "worklog actions aren't comments");
    let mut choices = std::collections::BTreeMap::new();
    choices.insert("OPS".to_string(), Choice::Auto { key: "INFRA".into() });
    let opts = Options { choices, ..Options::default() };

    // a dry run reports and changes nothing
    let dry = site_backup::run(&app.state, &owner, &mut src, &backup, &Options { dry_run: true, ..opts.clone() }, Default::default()).await.unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.total("issues").imported, 6, "{dry}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM projects").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 0, "{dry}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 2, "no people created by a dry run");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM import_sources").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 0);

    let r = site_backup::run(&app.state, &owner, &mut src, &backup, &opts, Default::default()).await.unwrap();
    let text = r.to_string();
    assert_eq!(r.total("issues").imported, 6, "{text}");
    let web = r.projects.iter().find(|p| p.source == "WEB").unwrap();
    assert!(web.created);
    assert_eq!(web.counts["comments"].imported, 2, "{text}");
    assert_eq!(web.counts["worklogs"].imported, 1, "zero-length work logs are left out: {text}");
    assert_eq!(web.counts["attachments"].imported, 1, "{text}");
    let ops = r.projects.iter().find(|p| p.source == "OPS").unwrap();
    assert_eq!(ops.key, "INFRA");
    assert_eq!(r.people.get("matched"), Some(&1), "{text}");
    assert_eq!(r.people.get("invited"), Some(&1), "{text}");
    assert_eq!(r.people.get("deactivated"), Some(&1), "{text}");
    assert!(warned(&r, "whose file isn't in the backup"), "{text}");
    assert!(warned(&r, "only a group or role could see"), "{text}");
    assert!(warned(&r, "“Causes”"), "{text}");
    assert!(warned(&r, "other custom fields"), "{text}");

    // people: invited and deactivated accounts, nobody e-mailed or notified
    let people: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT name, status, email FROM users ORDER BY name").fetch_all(&app.state.db).await.unwrap();
    assert!(people.contains(&("Bob Builder".into(), "invited".into(), Some("bob@example.com".into()))), "{people:?}");
    assert!(people.contains(&("Carol Former".into(), "deactivated".into(), Some("carol@example.com".into()))), "{people:?}");
    assert!(!people.iter().any(|p| p.0 == "Alice Admin"), "Alice is matched, not created");
    let mails: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_outbox").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(mails, 0);
    assert_eq!(get(&alice, app.url("/notifications")).await["unread"], 0);

    // the issue with everything
    let pay = get(&c, app.url("/issues/WEB-2")).await;
    assert_eq!(pay["title"], "Pay by card");
    assert_eq!(pay["type"]["name"], "Story");
    assert_eq!(pay["status"]["category"], "done", "Closed → a done status");
    assert_eq!(pay["priority"], "high");
    assert_eq!(pay["assignee"]["name"], "Bob Builder");
    assert_eq!(pay["reporter"]["id"], alice_id.as_str());
    assert_eq!(pay["labels"], json!(["checkout", "payments"]));
    assert_eq!(pay["components"][0]["name"], "Frontend");
    assert_eq!(pay["versions"][0]["name"], "1.0");
    assert_eq!(pay["affectedVersions"][0]["name"], "1.0", "affected versions come along");
    assert_eq!(pay["sprint"]["name"], "Sprint 1");
    assert_eq!(pay["sprint"]["state"], "closed");
    assert_eq!(pay["storyPoints"], 5.0);
    assert_eq!(pay["estimateMinutes"], 120, "the original estimate");
    assert_eq!((pay["remainingMinutes"].as_i64(), pay["remainingSet"].as_bool()), (Some(30), Some(true)), "and the remaining one");
    assert_eq!(pay["spentMinutes"], 90);
    assert_eq!(pay["parent"]["key"], "WEB-1", "epic links become parents");
    assert_eq!(pay["dueDate"], (base + Duration::hours(240)).date_naive().to_string());
    let created: DateTime<Utc> = pay["createdAt"].as_str().unwrap().parse().unwrap();
    assert!((created - base).num_seconds().abs() < 2, "{created} vs {base}");
    let resolved: DateTime<Utc> = pay["resolvedAt"].as_str().unwrap().parse().unwrap();
    assert!((resolved - (base + Duration::hours(104))).num_seconds().abs() < 2);
    assert_eq!(pay["watchers"].as_array().unwrap().iter().map(|w| w["name"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Alice A."]);
    assert_eq!(pay["children"][0]["key"], "WEB-5", "sub-tasks keep their parent");
    // the description: converted, mentions point to people, images to the imported file
    let att = &pay["attachments"][0];
    assert_eq!(att["name"], "card.png");
    assert_eq!(att["uploadedBy"], "Bob Builder");
    let desc = pay["description"].to_string();
    assert!(desc.contains(r#""type":"heading""#), "{desc}");
    assert!(desc.contains(&format!(r#""id":"{alice_id}""#)), "a mention of Alice: {desc}");
    assert!(desc.contains(att["url"].as_str().unwrap()), "{desc}");
    assert!(desc.contains("https://example.com/spec"));
    // the attachment round trip: the stored file comes back as it was
    let got = c.get(format!("{}{}", app.base, att["url"].as_str().unwrap())).send().await.unwrap();
    assert_eq!(got.status(), 200);
    assert_eq!(got.headers()["content-type"], "image/png");
    assert_eq!(&got.bytes().await.unwrap()[..], PNG);
    // comments and work logs with their authors and dates
    let comments = get(&c, app.url("/issues/WEB-2/comments")).await;
    let comments = comments.as_array().unwrap();
    assert_eq!(comments.len(), 2);
    assert_eq!(comments[0]["author"]["name"], "Bob Builder");
    assert!(comments[0]["body"].to_string().contains(&alice_id), "mention by account id: {}", comments[0]["body"]);
    assert!(comments[0]["body"].to_string().contains("& team"));
    assert_eq!(comments[1]["author"]["name"], "Carol Former");
    assert_eq!(comments[1]["edited"], true);
    assert_eq!(comments[1]["visibility"], "members", "a comment for a role stays restricted");
    let logs = get(&c, app.url("/issues/WEB-2/worklogs")).await;
    assert_eq!(logs[0]["minutes"], 90);
    assert_eq!(logs[0]["user"]["name"], "Bob Builder");
    assert_eq!(logs[0]["date"], (base + Duration::hours(25)).date_naive().to_string());
    assert_eq!(logs[0]["note"], "Wrote the card form");

    // numbers kept, statuses and types created where missing, old keys keep working
    let wip = get(&c, app.url("/issues/SHOP-3")).await;
    assert_eq!(wip["key"], "WEB-3");
    assert_eq!(wip["status"]["name"], "Waiting for QA", "missing statuses are created");
    assert_eq!(wip["status"]["category"], "in_progress");
    assert_eq!(wip["assignee"]["name"], "Carol Former", "deactivated people keep their issues");
    assert_eq!(wip["priority"], "urgent");
    assert_eq!(wip["sprint"]["name"], "Sprint 2", "the latest sprint");
    assert_eq!(wip["parent"]["key"], "WEB-1");
    assert_eq!(wip["security"]["name"], "Staff only", "its security level comes along");
    let mut links: Vec<String> =
        wip["links"].as_array().unwrap().iter().map(|l| format!("{} {}", l["label"].as_str().unwrap(), l["issue"]["key"].as_str().unwrap())).collect();
    links.sort();
    assert_eq!(links, vec!["is blocked by INFRA-1", "relates to WEB-6"]);
    assert_eq!(search(&c, &app, "project = WEB and blocked = true").await["total"], 1);
    assert_eq!(get(&c, app.url("/issues/WEB-5")).await["type"]["name"], "Sub-task");
    let better = get(&c, app.url("/issues/OPS-7")).await;
    assert_eq!(better["key"], "WEB-6", "keys from moves in the source keep working");
    assert_eq!(better["type"]["name"], "Improvement", "missing types are created");
    assert_eq!(get(&c, app.url("/issues/OPS-1")).await["key"], "INFRA-1", "imported under another key");
    let proj = get(&c, app.url("/projects/WEB")).await;
    assert_eq!(proj["name"], "Web shop");
    assert_eq!(proj["kind"], "scrum", "a project with sprints");
    let open_sprints: Vec<&str> = proj["sprints"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(open_sprints, vec!["Sprint 2"], "the template's empty sprint isn't kept");
    assert_eq!(proj["versions"][0]["status"], "released");
    // a new issue follows the highest number
    let fresh = post(&c, app.url("/issues"), json!({"projectKey": "WEB", "title": "After the import"})).await;
    assert_eq!(fresh["key"], "WEB-7");
    // the backlog keeps the source's order
    let r3: f64 = wip["rank"].as_f64().unwrap();
    assert!(r3 < pay["rank"].as_f64().unwrap());

    // history: what changed when and by whom, as reports replay it
    let h = get(&c, app.url("/issues/WEB-2/history")).await;
    let h = h.as_array().unwrap();
    let first = &h[0];
    assert_eq!(first["kind"], "created");
    assert_eq!(first["new"]["status"]["name"], "Open", "{first}");
    assert_eq!(first["new"]["points"], 3.0);
    assert_eq!(first["new"]["sprint"], Value::Null);
    assert_eq!(first["actor"]["id"], alice_id.as_str());
    let field = |f: &str| h.iter().filter(|e| e["field"] == f && e["kind"] == "field").cloned().collect::<Vec<_>>();
    let statuses = field("status");
    assert_eq!(statuses.len(), 2, "{statuses:?}");
    assert_eq!(statuses[0]["new"]["category"], "in_progress");
    assert_eq!(statuses[0]["actor"]["id"], alice_id.as_str());
    assert_eq!(statuses[1]["new"]["category"], "done");
    assert_eq!(statuses[1]["actor"]["name"], "Bob Builder");
    let at: DateTime<Utc> = statuses[1]["at"].as_str().unwrap().parse().unwrap();
    assert!((at - (base + Duration::hours(104))).num_seconds().abs() < 2);
    assert_eq!(field("assignee")[0]["new"]["name"], "Bob Builder", "history names people by account id too");
    assert_eq!(field("sprint")[0]["new"]["name"], "Sprint 1");
    assert_eq!(field("points")[0]["new"], 5.0);
    assert_eq!(field("priority")[0]["old"], "medium");
    assert_eq!(field("priority")[0]["new"], "high");
    let est = field("estimate");
    assert_eq!(est.len(), 1, "logging work isn't an estimate change: {est:?}");
    assert_eq!((est[0]["old"].clone(), est[0]["new"].clone()), (json!(60), json!(120)));
    assert!(field("title").is_empty(), "no change, no entry");
    for kind in ["comment", "worklog", "attachment"] {
        assert!(h.iter().any(|e| e["kind"] == kind), "{kind}");
    }
    let qa = get(&c, app.url("/issues/WEB-3/history")).await;
    assert_eq!(qa.as_array().unwrap().iter().filter(|e| e["field"] == "status").count(), 1);

    // reports replay the imported history
    let cycle = get(&c, app.url("/projects/WEB/reports/cycle-time?days=90")).await;
    let item = cycle["issues"].as_array().unwrap().iter().find(|i| i["key"] == "WEB-2").cloned().unwrap();
    assert_eq!(item["cycleDays"], 3.3, "in progress after a day, done 80 hours later: {item}");
    assert_eq!(item["leadDays"], 4.3);
    let cr = get(&c, app.url("/projects/WEB/reports/created-resolved?days=60")).await;
    let day = |d: DateTime<Utc>| d.date_naive().to_string();
    let days = cr["days"].as_array().unwrap();
    let on = |d: &str| days.iter().find(|x| x["date"] == d).cloned().unwrap();
    assert!(on(&day(base))["created"].as_i64().unwrap() >= 1);
    assert_eq!(on(&day(base + Duration::hours(104)))["resolved"], 1);
    let velocity = get(&c, app.url("/projects/WEB/reports/velocity")).await;
    let s1 = &velocity["sprints"][0];
    assert_eq!(s1["name"], "Sprint 1");
    assert_eq!(s1["completed"], 5.0, "{velocity}");
    assert_eq!(velocity["unit"], "points");
    let flow = get(&c, app.url("/projects/WEB/reports/cumulative-flow?days=40")).await;
    let last = flow["days"].as_array().unwrap().last().cloned().unwrap();
    let total: i64 = last["counts"].as_object().unwrap().values().map(|v| v.as_i64().unwrap()).sum();
    assert_eq!(total, 6, "five imported issues and the new one: {last}");

    // importing the same backup again brings nothing twice
    let again = site_backup::run(&app.state, &owner, &mut src, &backup, &opts, Default::default()).await.unwrap();
    assert_eq!(again.total("issues"), site_backup::Count { imported: 0, existing: 6 }, "{again}");
    assert_eq!(again.people.get("matched"), Some(&3));
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM comments").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 2);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_links").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 2);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 4);
    // into an existing project under a taken key: refused, nothing changes
    let mut taken = std::collections::BTreeMap::new();
    taken.insert("WEB".to_string(), Choice::New { key: "INFRA".into() });
    let e = site_backup::run(&app.state, &owner, &mut src, &backup, &Options { choices: taken, ..Options::default() }, Default::default()).await.unwrap_err();
    assert!(e.to_string().contains("taken"), "{e}");
}

#[tokio::test]
async fn backup_import_people_options_and_zip() {
    let app = spawn().await;
    let _c = app.setup(false).await;
    let owner = owner(&app).await;
    let base = Utc::now() - Duration::days(10);
    let bytes = zip_bytes(base);
    let mut src = Source::from_zip_bytes(bytes).unwrap();
    let backup = site_backup::load(&mut src, chrono_tz::UTC).unwrap();
    assert_eq!(src.file_count(), 1);
    // only one project, without accounts for unknown people
    let opts = Options { people: People::Skip, only: vec!["WEB".into()], ..Options::default() };
    let r = site_backup::run(&app.state, &owner, &mut src, &backup, &opts, Default::default()).await.unwrap();
    assert_eq!(r.projects.len(), 1);
    assert_eq!(r.total("issues").imported, 5);
    assert_eq!(r.people.get("skipped"), Some(&3), "{r}");
    assert!(warned(&r, "links to issues of projects that aren't imported"), "{r}");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 1);
    let comments: i64 = sqlx::query_scalar("SELECT count(*) FROM comments WHERE author_id IS NULL").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(comments, 2, "work without a person");
    // the attachment from the zip
    let size: i64 = sqlx::query_scalar("SELECT size FROM attachments").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(size, PNG.len() as i64);
    // the other project later, with deactivated accounts: earlier issues are found again
    let r = site_backup::run(&app.state, &owner, &mut src, &backup, &Options { people: People::Deactivated, ..Options::default() }, Default::default())
        .await
        .unwrap();
    assert_eq!(r.total("issues"), site_backup::Count { imported: 1, existing: 5 }, "{r}");
    let statuses: Vec<String> = sqlx::query_scalar("SELECT DISTINCT status FROM users WHERE role <> 'owner'").fetch_all(&app.state.db).await.unwrap();
    assert_eq!(statuses, vec!["deactivated".to_string()]);
    let link: i64 = sqlx::query_scalar("SELECT count(*) FROM issue_links WHERE kind = 'blocks'").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(link, 1, "the link to the first import's issue is made now");
}

/// Polls an import until it's no longer busy.
async fn settle(c: &reqwest::Client, app: &TestApp, id: &str) -> Value {
    for _ in 0..200 {
        let j = get(c, app.url(&format!("/admin/backup-imports/{id}"))).await;
        if !matches!(j["status"].as_str(), Some("checking" | "running")) {
            return j;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the import doesn't finish");
}

#[tokio::test]
async fn backup_import_from_the_browser() {
    let app = spawn().await;
    let c = app.setup(false).await;
    let (member, _) = app.user(&c, "Max Member", "max@example.com", "member").await;
    let bytes = zip_bytes(Utc::now() - Duration::days(5));
    let upload = |client: &reqwest::Client, body: Vec<u8>| {
        client.post(app.url("/admin/backup-imports")).query(&[("name", "site-backup.zip")]).header("content-type", "application/zip").body(body).send()
    };
    // administrators only, in the browser only
    assert_eq!(upload(&member, bytes.clone()).await.unwrap().status(), 403);
    let token: Value = post(&c, app.url("/me/tokens"), json!({"name": "script", "scope": "write"})).await;
    let r = reqwest::Client::new()
        .post(app.url("/admin/backup-imports"))
        .bearer_auth(token["token"].as_str().unwrap())
        .header("content-type", "application/zip")
        .body(bytes.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "not with an API token");
    let r = upload(&c, b"not a zip at all".to_vec()).await.unwrap();
    assert_eq!(r.status(), 400);
    // upload, then the check shows what the backup holds
    let r = upload(&c, bytes.clone()).await.unwrap();
    assert_eq!(r.status(), 202);
    let job: Value = r.json().await.unwrap();
    let id = job["id"].as_str().unwrap().to_string();
    let job = settle(&c, &app, &id).await;
    assert_eq!(job["status"], "checked", "{job}");
    let s = &job["summary"];
    let web = s["projects"].as_array().unwrap().iter().find(|p| p["key"] == "WEB").cloned().unwrap();
    assert_eq!(web["issues"], 5);
    assert_eq!(web["comments"], 3);
    assert_eq!(web["attachments"], 2);
    assert_eq!(web["missingFiles"], 1);
    assert_eq!(web["formerKeys"], json!(["SHOP"]));
    assert_eq!(web["existing"], Value::Null);
    assert_eq!(s["people"]["total"], 3);
    assert_eq!(s["files"], 1);
    let list = get(&c, app.url("/admin/backup-imports")).await;
    assert_eq!(list["imports"][0]["id"], id.as_str());
    assert_eq!(list["maxUploadMb"], 16);
    // one at a time
    let (st, _) =
        send(&c, Method::POST, app.url(&format!("/admin/backup-imports/{id}/run")), json!({"projects": {"WEB": {"action": "new", "key": "1X"}}})).await;
    assert_eq!(st, 400, "keys are checked");
    // the choices: WEB as a new project under another key, OPS left out, people deactivated
    let (st, _) = send(
        &c,
        Method::POST,
        app.url(&format!("/admin/backup-imports/{id}/run")),
        json!({"projects": {"WEB": {"action": "new", "key": "SHOP"}, "OPS": {"action": "skip"}}, "people": "deactivated"}),
    )
    .await;
    assert_eq!(st, 202);
    let job = settle(&c, &app, &id).await;
    assert_eq!(job["status"], "done", "{job}");
    let report = &job["report"];
    let shop = report["projects"].as_array().unwrap().iter().find(|p| p["source"] == "WEB").cloned().unwrap();
    assert_eq!(shop["key"], "SHOP");
    assert_eq!(shop["counts"]["issues"]["imported"], 5);
    assert!(report["projects"].as_array().unwrap().iter().any(|p| p["source"] == "OPS" && p["skipped"] == true));
    assert_eq!(job["fileKept"], false, "the backup is removed once it's imported");
    assert_eq!(get(&c, app.url("/issues/SHOP-2")).await["title"], "Pay by card");
    assert_eq!(get(&c, app.url("/issues/WEB-2")).await["key"], "SHOP-2", "the backup's key works too");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM mail_outbox").fetch_one(&app.state.db).await.unwrap();
    assert_eq!(n, 0, "nobody is e-mailed");
    // a second upload of the same backup: the check knows what's there already
    let job: Value = upload(&c, bytes).await.unwrap().json().await.unwrap();
    let id2 = job["id"].as_str().unwrap().to_string();
    let job = settle(&c, &app, &id2).await;
    let web = job["summary"]["projects"].as_array().unwrap().iter().find(|p| p["key"] == "WEB").cloned().unwrap();
    assert_eq!(web["imported"], 5);
    let (st, _) = send(&c, Method::DELETE, app.url(&format!("/admin/backup-imports/{id2}")), Value::Null).await;
    assert_eq!(st, 204);
    let (st, _) = send(&c, Method::GET, app.url(&format!("/admin/backup-imports/{id2}")), Value::Null).await;
    assert_eq!(st, 404);
}
