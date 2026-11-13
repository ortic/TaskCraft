//! Messages for chat channels: webhook events and automation posts as readable text for
//! Slack-compatible incoming webhooks (also Mattermost and others that take Slack's format),
//! Microsoft Teams (an adaptive card) and Google Chat. Written in the workspace's language.
//!
//! Channels are shared: nothing about a private issue or a restricted comment is posted.

use serde_json::{Value, json};

use crate::i18n::tr;

/// The formats a webhook can send: `json` is the signed event itself, the others chat messages.
pub const FORMATS: [&str; 4] = ["json", "slack", "teams", "google-chat"];

/// A message: a line of text and the issue (or page) it's about.
pub struct Message {
    pub text: String,
    /// (label, address)
    pub link: Option<(String, String)>,
}

fn issue_link(issue: &Value) -> Option<(String, String)> {
    let key = issue["key"].as_str()?;
    let title = issue["title"].as_str().unwrap_or("");
    let url = issue["url"].as_str()?;
    Some((format!("{key} {title}"), url.to_string()))
}

/// Whether an event's data is about something a channel mustn't see.
pub fn private(data: &Value) -> bool {
    !data["issue"]["security"].is_null() || !data["comment"]["visibility"].is_null()
}

/// The message for a webhook event (None: nothing to say, or private).
pub fn event_message(l: &str, event: &str, data: &Value) -> Option<Message> {
    if private(data) {
        return None;
    }
    let issue = &data["issue"];
    let link = issue_link(issue);
    let key = issue["key"].as_str().unwrap_or("");
    let text = match event {
        "issue.created" => {
            let who = issue["assignee"]["name"].as_str();
            match who {
                Some(w) => tr(l, "New issue {key} (assigned to {name})", &[("key", key), ("name", w)]),
                None => tr(l, "New issue {key}", &[("key", key)]),
            }
        }
        "issue.updated" => {
            let status = issue["status"]["name"].as_str().unwrap_or("");
            let changed: Vec<&str> = data["changed"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            if changed.contains(&"status") {
                tr(l, "{key} is now {status}", &[("key", key), ("status", status)])
            } else if changed.contains(&"project") {
                tr(l, "{key} moved to another project", &[("key", key)])
            } else if changed.contains(&"assignee") {
                match issue["assignee"]["name"].as_str() {
                    Some(w) => tr(l, "{key} is assigned to {name}", &[("key", key), ("name", w)]),
                    None => tr(l, "{key} is unassigned", &[("key", key)]),
                }
            } else {
                tr(l, "{key} was changed", &[("key", key)])
            }
        }
        "issue.deleted" => tr(l, "{key} was deleted", &[("key", key)]),
        "comment.created" => {
            let text: String = data["comment"]["text"].as_str().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
            let short: String = if text.chars().count() > 280 { format!("{}…", text.chars().take(279).collect::<String>()) } else { text };
            tr(l, "New comment on {key}: {text}", &[("key", key), ("text", &short)])
        }
        "sprint.started" => tr(l, "Sprint {name} started", &[("name", data["sprint"]["name"].as_str().unwrap_or(""))]),
        "sprint.completed" => tr(l, "Sprint {name} completed", &[("name", data["sprint"]["name"].as_str().unwrap_or(""))]),
        "version.released" => tr(l, "{name} is released", &[("name", data["version"]["name"].as_str().unwrap_or(""))]),
        "test" => tr(l, "TaskCraft can post to this channel.", &[]),
        _ => return None,
    };
    Some(Message { text, link })
}

/// The request body for a chat format (`json` has none: the event is sent as it is).
pub fn body(format: &str, m: &Message) -> Option<Value> {
    // Slack and Google Chat links: <address|label>; their markup characters stay plain text
    let plain = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    match format {
        "slack" | "google-chat" => {
            let text = match &m.link {
                Some((label, url)) => format!("{} — <{}|{}>", plain(&m.text), url, plain(label)),
                None => plain(&m.text),
            };
            Some(json!({ "text": text }))
        }
        "teams" => {
            let mut card = json!({
                "type": "AdaptiveCard",
                "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
                "version": "1.4",
                "body": [{"type": "TextBlock", "text": m.text, "wrap": true}],
            });
            if let Some((label, url)) = &m.link {
                card["body"].as_array_mut().unwrap().push(json!({"type": "TextBlock", "text": label, "wrap": true, "weight": "Bolder"}));
                card["actions"] = json!([{"type": "Action.OpenUrl", "title": label, "url": url}]);
            }
            Some(json!({"type": "message", "attachments": [{"contentType": "application/vnd.microsoft.card.adaptive", "content": card}]}))
        }
        _ => None,
    }
}

/// An automation message: `{key}`, `{title}`, `{status}`, `{assignee}`, `{priority}`, `{project}`
/// and `{url}` are filled from the issue.
pub fn fill(template: &str, issue: &Value) -> String {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = template.to_string();
    for (k, v) in [
        ("key", s(&issue["key"])),
        ("title", s(&issue["title"])),
        ("status", s(&issue["status"]["name"])),
        ("assignee", s(&issue["assignee"]["name"])),
        ("priority", s(&issue["priority"])),
        ("project", s(&issue["projectName"])),
        ("url", s(&issue["url"])),
    ] {
        out = out.replace(&format!("{{{k}}}"), &v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_for_channels() {
        let issue = json!({"key": "WEB-12", "title": "Login <fails>", "url": "https://tc.test/issues/WEB-12", "status": {"name": "Done"}, "security": null});
        let m = event_message("en", "issue.updated", &json!({"issue": issue, "changed": ["status"]})).unwrap();
        assert_eq!(m.text, "WEB-12 is now Done");
        assert_eq!(body("slack", &m).unwrap()["text"], "WEB-12 is now Done — <https://tc.test/issues/WEB-12|WEB-12 Login &lt;fails&gt;>");
        let teams = body("teams", &m).unwrap();
        assert_eq!(teams["attachments"][0]["content"]["actions"][0]["url"], "https://tc.test/issues/WEB-12");
        assert!(body("json", &m).is_none());
        assert_eq!(event_message("de", "issue.updated", &json!({"issue": issue, "changed": ["status"]})).unwrap().text, "WEB-12 ist jetzt Done");
        // private issues and restricted comments stay out of channels
        let secret = json!({"issue": {"key": "SEC-1", "security": {"id": "x", "name": "Staff"}}});
        assert!(event_message("en", "issue.created", &secret).is_none());
        assert!(event_message("en", "comment.created", &json!({"issue": {"key": "WEB-1"}, "comment": {"text": "x", "visibility": "admins"}})).is_none());
        assert_eq!(fill("{key} ({status}): {title}", &issue), "WEB-12 (Done): Login <fails>");
    }
}
