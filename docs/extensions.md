# Extensions and scripts

Every organization has a few needs of its own: compute a field, sync with an ERP, call an
internal service. TaskCraft has two ways to meet them without changing TaskCraft itself, both
built on small sandboxed scripts that run inside the server:

- a **script action** in an automation rule — a few lines that look at the issue the rule fired
  for and change it (project admins write these);
- an **extension** — a package a workspace admin installs: a manifest that says what it is and
  what it needs, and a script with its hooks (automation actions, event handlers, a scheduled
  job, API routes).

Scripts are written in [Rhai](https://rhai.rs/book/), a small language that reads like
JavaScript or Rust: `let`, `if`, `for`, functions, strings with `` `templates ${like} this` ``,
arrays `[1, 2]` and maps `#{key: value}`.

## The sandbox

A script can't read files, load modules, `eval` text or reach anything it wasn't handed. It's
stopped when it does too much work (an endless loop), runs longer than a couple of seconds
(fifteen for extensions, which may wait for HTTP answers), nests calls too deeply or builds
strings, arrays or maps that are too large. An extension makes at most ten HTTP calls in one
run, each answer at most 1 MB, to hosts its manifest names — never to private addresses unless
`TASKCRAFT_WEBHOOK_ALLOW_PRIVATE` is set — and redirects aren't followed. What goes wrong shows
on the rule (*Last run failed: …*) or in the extension's log.

## A script in an automation rule

*Project settings → Automation → Add rule → run a script.* The issue is in scope as `issue`
(the same fields the API shows: `issue.key`, `issue.title`, `issue.priority`, `issue.status.name`,
`issue.assignee`, `issue.reporter.name`, `issue.labels`, `issue.dueDate`, `issue.storyPoints`,
`issue.custom`, …). It changes that issue, and only that one:

| Call | What it does |
|---|---|
| `set("priority", "urgent")` | sets `title`, `description` (Markdown), `priority`, `status` (by name), `assignee` (name or e-mail, `""` for nobody), `dueDate`, `startDate`, `storyPoints`, `estimateMinutes` or `remainingMinutes` |
| `set_field("Customer tier", "Gold")` | a custom field by its name; options by their label |
| `add_label("triage")`, `remove_label("draft")` | labels |
| `comment("text")` | a comment (Markdown), by Automation |
| `log("text")` | for extensions: a line in the log |

```rhai
let words = issue.title.to_lower();
if words.contains("payment") {
    set("priority", "urgent");
    set_field("Customer tier", "Gold");
    add_label("money");
    comment(`Triage: reported by ${issue.reporter.name}.`);
}
set_field("Risk", issue.title.len() % 7);
```

The changes are made when the script ends, as Automation, and land in the issue's history. A
script that fails changes nothing.

## Extensions

*Administration → Extensions → Install an extension*: paste the manifest and the script, or load
a `.json` file holding `{"manifest": {…}, "script": "…"}`.

```json
{
  "name": "erp-sync",
  "title": "ERP sync",
  "version": "1.2.0",
  "apiVersion": 1,
  "description": "Creates an order in the ERP when an issue is done",
  "capabilities": {
    "issues": "write",
    "http": ["erp.example.com"],
    "state": true,
    "secrets": ["ERP_TOKEN"]
  },
  "actions": [{ "name": "push", "label": "Create an order in the ERP" }],
  "events": ["created", "commented"],
  "schedule": { "every": "day", "at": "06:00" },
  "routes": [{ "method": "GET", "path": "status" }]
}
```

- `name` is its address (`/api/ext/erp-sync/…`) and how rules refer to it; it never changes.
- `apiVersion` is the extension API it was written for. This document describes **API 1**;
  TaskCraft keeps serving it when newer ones appear, so an upgrade doesn't break extensions.
- `capabilities` is everything it may do beyond its own issue — nothing unless asked for here.
  The admin who installs it sees the list and grants it by installing.

### Hooks

Each hook in the manifest needs its function in the script (installing checks this):

| Manifest | Function | When |
|---|---|---|
| `actions: [{name: "push"}]` | `fn action_push(issue, params)` | an automation rule with the action *run an extension's action* fires; `params` is what the rule passes |
| `events: [...]` | `fn on_event(event, issue)` | after an issue was `created`, changed `status`, was `assigned`, `commented` on, or `changed` in any other way (`"changed"` in the list hears everything) |
| `schedule: {…}` | `fn scheduled()` | on the schedule (the same form automation rules use: `{"every": "hour"}`, `{"every": "day", "at": "06:00", "timezone": "Europe/Zurich"}`, `{"every": "week", "on": ["mon"], "at": "08:00"}`) |
| `routes: [{method, path}]` | `fn route(request)` | someone signed in calls `/api/ext/<name>/<path>`; `request` has `method`, `path`, `query`, `body` and `user` (`id`, `name`, `email`, `role`). The value returned is sent as JSON; `#{status: 404, body: #{…}}` sets the status |

In `action_…` and `on_event` the calls of the table above (`set`, `set_field`, `add_label`,
`remove_label`, `comment`) change the issue at hand. Functions can't see variables defined
outside them: put shared values in a function (`fn base() { "https://erp.example.com" }`).

### Capabilities

| Capability | Calls |
|---|---|
| `"issues": "read"` | `search("project = WEB and category != done")` → up to 100 issues; `get_issue("WEB-12")` → the issue, or `()` when it can't be seen |
| `"issues": "write"` | also `update_issue("WEB-12", #{priority: "high", labels: ["x"]})` (the fields of `PATCH /api/issues/{KEY}`) and `add_comment("WEB-12", "text")` |
| `"http": ["erp.example.com", "*.example.net"]` | `http_get(url)`, `http_get(url, headers)`, `http_post(url, body)`, `http_post(url, body, headers)`, `http_put(…)` → `#{status, ok, body}` (the body parsed when it's JSON) |
| `"state": true` | `state_get("key")`, `state_set("key", value)` — up to 64 KB, kept between runs |
| `"secrets": ["ERP_TOKEN"]` | `secret("ERP_TOKEN")` — set under *Secrets*, never shown again |

Issues are read and changed **as a person**: the admin who installed the extension, or whoever
is chosen under *Reads and changes issues as* (a service account with exactly the project roles
the extension should have is a good choice). The extension sees what that person sees — private
issues and other projects stay hidden — whoever triggers it.

```rhai
fn base() { "https://erp.example.com/api" }

fn action_push(issue, params) {
    let r = http_post(base() + "/orders", #{ key: issue.key, title: issue.title, kind: params.kind },
                      #{ authorization: "Bearer " + secret("ERP_TOKEN") });
    if !r.ok { throw `the ERP answered ${r.status}`; }
    state_set("last", r.body.orderId);
    comment(`Order ${r.body.orderId}`);
    add_label("in-erp");
}

fn on_event(event, issue) {
    log(`${event} ${issue.key}`);
}

fn scheduled() {
    for i in search("due < today() and category != done") {
        add_comment(i.key, "This is overdue.");
    }
}

fn route(request) {
    #{ last: state_get("last"), asked_by: request.user.name }
}
```

### Running them

The list shows each extension's capabilities and hooks, who it runs as, how often it ran, its
next scheduled run and its last error. *Log* has what it printed with `log(…)` and what went
wrong (the latest 500 lines); *Run now* runs the scheduled job at once; *Secrets* sets its
secrets. The **switch** turns an extension off entirely — routes answer 404, rules using its
actions fail with a clear message, events and the schedule pass it by — and on again. Installing,
changing, switching and removing are in the audit log. Updating means *Edit*: a new manifest and
script replace the old ones (the name stays), and capabilities can shrink or grow with them.

The same is available over the API: `GET/POST /api/admin/extensions`,
`PATCH/DELETE /api/admin/extensions/{id}`, `…/logs`, `…/run`, and `GET /api/extensions/actions`
for the actions rules can use.
