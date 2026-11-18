//! The command-line client (`tc`, crates/cli) against a real server: signing in through the
//! browser's approval, profiles, and the everyday commands — with the issue key read from the
//! current Git branch where it's left out.

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use taskcraft_cli::Env;

struct Shell {
    env: Env,
    lines: Arc<Mutex<Vec<String>>>,
    _config: tempfile::TempDir,
    repo: tempfile::TempDir,
}

fn shell() -> Shell {
    let config = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let lines: Arc<Mutex<Vec<String>>> = Default::default();
    let sink = lines.clone();
    let env = Env {
        config_dir: config.path().join("taskcraft"),
        cwd: repo.path().to_path_buf(),
        url: None,
        token: None,
        browser: false,
        out: Arc::new(move |l| sink.lock().unwrap().push(l)),
        stdin: Arc::new(|| "From standard input.".to_string()),
    };
    Shell { env, lines, _config: config, repo }
}

impl Shell {
    /// Runs `tc <args>`; what it printed, or why it failed.
    async fn tc(&self, args: &[&str]) -> Result<String, String> {
        self.lines.lock().unwrap().clear();
        let mut argv = vec!["tc".to_string()];
        argv.extend(args.iter().map(|a| a.to_string()));
        match taskcraft_cli::run(&argv, &self.env).await {
            Ok(()) => Ok(self.lines.lock().unwrap().join("\n")),
            Err(e) => Err(e.message),
        }
    }
    async fn ok(&self, args: &[&str]) -> String {
        self.tc(args).await.unwrap_or_else(|e| panic!("tc {args:?}: {e}"))
    }
    async fn json(&self, args: &[&str]) -> Value {
        let mut a = vec!["--json"];
        a.extend(args);
        serde_json::from_str(&self.ok(&a).await).unwrap()
    }
    fn git(&self, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(self.repo.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sign_in_through_the_browser_and_everyday_commands() {
    let app = spawn().await;
    let owner = app.setup(false).await;
    post(&owner, app.url("/projects"), json!({"key": "WEB", "name": "Web shop", "template": "scrum"})).await;
    let (mia, _) = app.user(&owner, "Mia Member", "mia@example.com", "member").await;
    post(&owner, app.url("/issues"), json!({"projectKey": "WEB", "title": "Login times out", "priority": "high"})).await;
    let sh = shell();

    // nothing works signed out, and it says what to do
    let e = sh.tc(&["issue", "list"]).await.unwrap_err();
    assert!(e.contains("tc login"), "{e}");
    assert!(sh.tc(&["login"]).await.unwrap_err().contains("which server"));

    // tc login: the client shows a page and a code; Mia approves in her browser session
    let (base, lines, browser) = (app.base.clone(), sh.lines.clone(), mia.clone());
    let approve = tokio::spawn(async move {
        for _ in 0..100 {
            let shown = lines.lock().unwrap().join("\n");
            if let Some(at) = shown.find("code=") {
                let code: String = shown[at + 5..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                assert!(shown.contains(&format!("{base}/cli/authorize?code={code}")), "{shown}");
                let url = format!("{base}/api/cli/logins/{code}");
                // signed out, or with a made-up code: nothing
                assert_eq!(send(&client(), Method::POST, format!("{url}/approve"), json!({})).await.0, 401);
                assert_eq!(send(&browser, Method::POST, format!("{base}/api/cli/logins/AAAA-BBBB/approve"), json!({})).await.0, 404);
                let seen = get(&browser, url.clone()).await;
                assert_eq!(seen["approved"], false);
                assert_eq!(send(&browser, Method::POST, format!("{url}/approve"), json!({})).await.0, 204);
                assert_eq!(send(&browser, Method::POST, format!("{url}/approve"), json!({})).await.0, 404, "once");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("tc login never showed a code");
    });
    let out = sh.ok(&["login", &app.base]).await;
    approve.await.unwrap();
    assert!(out.contains("Signed in to") && out.contains("as Mia Member"), "{out}");
    let tokens = get(&mia, app.url("/me/tokens")).await;
    assert!(tokens[0]["name"].as_str().unwrap().starts_with("tc on "), "{tokens}");
    // the token is in the profile, readable by its owner only
    let config = sh.env.config_dir.join("config.json");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert!(saved["profiles"]["default"]["token"].as_str().unwrap().starts_with("tct_"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert!(sh.ok(&["whoami"]).await.starts_with("Mia Member <mia@example.com> · Acme"));
    assert!(sh.ok(&["profile", "list"]).await.contains("default"));

    // a verifier nobody approved waits, and a made-up one is unknown
    let pending = post(&client(), app.url("/cli/logins"), json!({"name": "elsewhere"})).await;
    let (s, v) = send(&client(), Method::POST, app.url("/cli/token"), json!({"verifier": pending["verifier"]})).await;
    assert_eq!((s, v["pending"].as_bool()), (202, Some(true)));
    assert_eq!(send(&client(), Method::POST, app.url("/cli/token"), json!({"verifier": "made-up"})).await.0, 404);
    assert_eq!(send(&mia, Method::DELETE, app.url(&format!("/cli/logins/{}", pending["code"].as_str().unwrap())), Value::Null).await.0, 204);
    assert_eq!(send(&client(), Method::POST, app.url("/cli/token"), json!({"verifier": pending["verifier"]})).await.0, 404, "refused");

    // create, list (TQL), view
    let out = sh
        .ok(&[
            "issue",
            "create",
            "-p",
            "web",
            "Checkout button is grey",
            "--type",
            "bug",
            "--priority",
            "urgent",
            "--label",
            "shop",
            "-d",
            "It should be **green**.",
        ])
        .await;
    assert!(out.starts_with("Created WEB-2 Checkout button is grey [To do]") && out.contains(&format!("{}/issues/WEB-2", app.base)), "{out}");
    assert!(sh.tc(&["issue", "create", "-p", "WEB", "X", "--type", "Nope"]).await.unwrap_err().contains("there are:"));
    let list = sh.ok(&["issue", "list", "project = WEB order by key"]).await;
    let rows: Vec<&str> = list.lines().collect();
    assert!(
        rows[0].starts_with("KEY") && rows[1].starts_with("WEB-1") && rows[2].starts_with("WEB-2") && rows[2].ends_with("Checkout button is grey"),
        "{list}"
    );
    assert_eq!(sh.ok(&["issue", "list"]).await, "No issues.", "nothing is assigned to her yet");
    let found = sh.json(&["issue", "list", "priority = urgent"]).await;
    assert_eq!((found["total"].as_i64(), found["issues"][0]["key"].as_str()), (Some(1), Some("WEB-2")));
    assert!(sh.tc(&["issue", "list", "nonsense ="]).await.is_err());
    let view = sh.ok(&["issue", "view", "web-2"]).await;
    for part in ["WEB-2 Checkout button is grey", "Bug", "urgent", "shop", "It should be green."] {
        assert!(view.contains(part), "{part} in {view}");
    }

    // on the issue's branch the key can be left out
    sh.git(&["init", "-q", "-b", "main"]);
    sh.git(&["commit", "-q", "--allow-empty", "-m", "start"]);
    assert!(sh.tc(&["issue", "view"]).await.unwrap_err().contains("doesn't name one"));
    assert_eq!(sh.ok(&["branch", "WEB-2", "--print"]).await, "web-2-checkout-button-is-grey");
    assert_eq!(sh.ok(&["branch", "WEB-2"]).await, "On branch web-2-checkout-button-is-grey.");
    assert_eq!(sh.git(&["rev-parse", "--abbrev-ref", "HEAD"]), "web-2-checkout-button-is-grey");
    assert_eq!(sh.ok(&["branch", "WEB-2"]).await, "On branch web-2-checkout-button-is-grey.", "again: it's just checked out");
    assert!(sh.ok(&["issue", "view"]).await.starts_with("WEB-2 "));
    assert_eq!(sh.ok(&["open"]).await, format!("{}/issues/WEB-2", app.base));

    // assign, move, edit, comment, link, log
    assert_eq!(sh.ok(&["issue", "assign"]).await, "WEB-2 is assigned to Mia Member.");
    assert_eq!(sh.ok(&["issue", "assign", "WEB-1", "olivia"]).await, "WEB-1 is assigned to Olivia Owner.");
    assert_eq!(sh.ok(&["issue", "assign", "WEB-1", "none"]).await, "WEB-1 is unassigned.");
    assert!(sh.tc(&["issue", "assign", "WEB-1", "nobody-like-this"]).await.unwrap_err().contains("nobody is called"));
    assert_eq!(sh.ok(&["issue", "move", "in prog"]).await, "Moved WEB-2 Checkout button is grey [In progress]");
    assert_eq!(sh.ok(&["issue", "move", "WEB-1", "Done"]).await, "Moved WEB-1 Login times out [Done]");
    assert!(sh.tc(&["issue", "move", "Shipped"]).await.unwrap_err().contains("there are: To do, In progress"));
    assert!(
        sh.ok(&["issue", "edit", "--title", "Checkout button stays grey", "--due", "2030-01-31", "--points", "3"]).await.contains("Checkout button stays grey")
    );
    assert!(sh.tc(&["issue", "edit"]).await.unwrap_err().contains("say what to change"));
    assert_eq!(sh.ok(&["comment", "Fixed in the next build."]).await, "Commented on WEB-2.");
    assert_eq!(sh.ok(&["comment", "WEB-1", "-"]).await, "Commented on WEB-1.");
    assert_eq!(sh.ok(&["link", "WEB-2", "blocks", "web-1"]).await, "WEB-2 blocks WEB-1.");
    assert_eq!(sh.ok(&["log", "1h30m", "-m", "Styles"]).await, "Logged 1h 30m on WEB-2.");
    assert_eq!(sh.ok(&["log", "WEB-1", "15m"]).await, "Logged 15m on WEB-1.");
    let i = get(&owner, app.url("/issues/WEB-2")).await;
    assert_eq!(
        (i["title"].as_str(), i["dueDate"].as_str(), i["storyPoints"].as_f64(), i["spentMinutes"].as_i64()),
        (Some("Checkout button stays grey"), Some("2030-01-31"), Some(3.0), Some(90)),
        "{i}"
    );
    assert_eq!(i["links"].as_array().unwrap().len(), 1);
    let comments = get(&owner, app.url("/issues/WEB-1/comments")).await;
    assert_eq!(comments[0]["author"]["name"], "Mia Member");
    assert!(comments[0].to_string().contains("From standard input."));
    let view = sh.ok(&["issue", "view", "--comments"]).await;
    assert!(view.contains("Fixed in the next build.") && view.contains("Mia Member"), "{view}");
    assert_eq!(sh.ok(&["issue", "list"]).await.lines().count(), 2, "her open issue");

    // the timer
    assert_eq!(sh.ok(&["timer", "status"]).await, "No timer is running.");
    assert_eq!(sh.ok(&["timer", "start"]).await, "Timer started on WEB-2.");
    assert!(sh.ok(&["timer", "status"]).await.starts_with("Timer running on WEB-2 Checkout button stays grey"));
    assert_eq!(sh.ok(&["timer", "pause"]).await, "Timer paused.");
    assert!(sh.ok(&["timer", "status"]).await.starts_with("Timer paused"));
    assert_eq!(sh.ok(&["timer", "resume"]).await, "Timer running again.");
    assert_eq!(sh.ok(&["timer", "stop"]).await, "Timer stopped: logged 1m.");
    assert_eq!(get(&owner, app.url("/issues/WEB-2")).await["spentMinutes"], 91);

    // the environment instead of a profile (scripts, CI); a bad token says so
    let mut ci = shell();
    ci.env.url = Some(format!("{}/", app.base));
    ci.env.token = Some(saved["profiles"]["default"]["token"].as_str().unwrap().to_string());
    assert!(ci.ok(&["whoami"]).await.starts_with("Mia Member"));
    ci.env.token = Some("tct_wrong".into());
    assert!(ci.tc(&["issue", "list"]).await.unwrap_err().contains("doesn't accept the token"));
    // a second profile with a token given directly, then switching and signing out
    let token = post(&owner, app.url("/me/tokens"), json!({"name": "ci", "scope": "read"})).await["token"].as_str().unwrap().to_string();
    assert!(sh.ok(&["--profile", "boss", "login", &app.base, "--token", &token]).await.contains("as Olivia Owner (profile boss)"));
    assert!(sh.ok(&["whoami"]).await.starts_with("Olivia Owner"));
    assert!(sh.tc(&["comment", "WEB-1", "read-only"]).await.is_err(), "a read-only token");
    assert!(sh.ok(&["--profile", "default", "whoami"]).await.starts_with("Mia Member"));
    assert_eq!(sh.ok(&["profile", "use", "default"]).await, "Using default.");
    assert!(sh.ok(&["logout"]).await.starts_with("Signed out of default."));
    assert!(sh.tc(&["whoami"]).await.unwrap_err().contains("not signed in"));
    // help and usage come from the client itself
    assert!(sh.tc(&["--help"]).await.unwrap_err().contains("TaskCraft from the command line"));
    assert!(sh.tc(&["issue", "frobnicate"]).await.is_err());
}
