# TaskCraft

Self-hosted issue and project management with a **Rust backend**: boards and backlogs, sprints,
timelines, an issue query language, reports computed from each issue's history, automation, and
documentation next to the work through [WikiCraft](https://github.com/ortic/WikiCraft).

One binary, PostgreSQL, optional S3. Built with the same stack as WikiCraft.

## Features

**Plan and track**

- **Projects from templates**: scrum, kanban, issue tracker and work plan, each with its own
  workflow, issue types, fields and board
- **Issues**: rich-text descriptions with checklists, `@mentions` and `#issue` references; type
  (epics, stories, tasks, bugs, sub-tasks, your own), status, priority, assignee, reporter, labels,
  epic / parent, sprint, release, component, story points, estimate and time spent, start and due
  dates, custom fields (text, number, single and multiple choice, date, person, checkbox, URL)
- **Links** (blocks / is blocked by, relates, duplicates, clones) — blocked issues are flagged
  everywhere — plus web links, wiki pages and attachments (drag & drop)
- **Comments** with mentions and reactions, **watchers**, in-app **notifications** (also by
  e-mail: bundled a couple of minutes after, or as a daily digest at 08:00 local time), a
  full **history** of every change, **time logs** and a weekly **timesheet**
- **Workflows**: statuses in three categories (to do, in progress, done); free or restricted
  transitions with a transition matrix
- **Move issues to another project** (the old key keeps working; type, status and custom fields are mapped, sub-tasks move along)
- **CSV import** with column mapping and a dry run: statuses, types, people, sprints and releases matched by name (missing statuses created and put on the board); issue numbers kept when the project key stays, other keys keep working as links; parents by key or id; times in hours, durations or seconds, booked to whoever did the work; people who haven't joined yet or have left keep their issues; wiki-markup descriptions converted
- **Trash** with restore, clone, bulk changes, CSV export

**Views**

- **Board**: columns of statuses, drag & drop with ranking, **WIP limits**, swimlanes (people,
  epics, priority, type), quick filters, a person filter and **ageing dots** on cards that sit
  in one status too long
- **Backlog**: ranked, drag issues between sprints and the backlog, epic panel with progress,
  inline creation, start / complete sprints (unfinished work moves on), and a planning hint from
  your recent velocity ("21 of ~22 pts")
- **List**: any query as a table — choose columns, sort, group, edit status / priority / assignee
  in place, select many and change them at once; **saved filters**, private or shared, starred
  into the sidebar
- **Timeline**: epics and issues on a calendar, drag to reschedule, dependency arrows with
  conflict warnings; **Calendar**: issues by due or start date, drag to another day
- **Your work**: assigned, recently viewed, watching, reported, finished; activity of your projects
- **Command palette** (Ctrl K) and keyboard shortcuts (`c` creates an issue)

**TQL, the query language**

```text
project = WEB and sprint in openSprints() and assignee = me() order by rank
type = Bug and priority >= high and created > -14d
due < endOfWeek() and category != done
labels is empty or assignee is empty
"Customer tier" = Gold                      (custom fields by name)
login timeout                               (bare words search titles and descriptions)
```

Fields: `project key title text description comment type status category priority assignee
reporter creator labels sprint version component parent/epic points estimate spent created
updated resolved due start watcher blocked` and custom fields. Operators `= != ~ !~ > >= < <=`,
`in (…)`, `not in`, `is [not] empty`; `and`, `or`, `not`, parentheses; `order by`. Functions:
`me()`, `now()`, `today()`, `startOfWeek()`, `endOfWeek()`, `startOfMonth()`, `endOfMonth()`,
`openSprints()`, `futureSprints()`, `closedSprints()`, `releasedVersions()`,
`unreleasedVersions()`; relative dates like `-7d`, `+2w`, `-1m`. The query bar completes field
names, operators and values.

**Reports** (replayed from the issues' history, so they're right even when estimates or scope
change mid-sprint)

- Sprint **burndown** with the ideal line and scope changes, **velocity** (committed vs
  completed), **cumulative flow**, **cycle and lead time** (median and 85th percentile),
  created vs resolved, **workload** per person, project overview

**Automation**: "when an issue is created / changes status / is assigned / a field changes /
someone comments, if it matches a TQL condition, then set the status, assign, set the priority,
add or remove a label, move it into the running sprint or the backlog, comment or notify
someone". Rules can also run **on a schedule** for every issue matching a query ("every weekday
at 08:00, for issues due tomorrow: notify the assignee"), in the time zone they were set up in.

**Templates and recurring issues**: ready-made issues (title with `{date}` / `{week}` /
`{month}`, description, fields, due in N days, sub-tasks) to pick when creating an issue; with a
schedule a template creates its issue by itself.

**Service levels**: goals in hours per priority for the first response and for resolution; issues
show the time left, a queue lists the most urgent first, and the assignee (else the project lead)
is told when a goal is missed.

**E-mail in**: projects take e-mail — a message to `support@acme.com` (or `tasks+SUP@acme.com`)
becomes an issue, a reply becomes a comment (matched by the conversation or `[SUP-12]` in the
subject, quoted text and signatures left out), files become attachments. People act with their own
permissions; support trackers can let anyone write: the sender becomes the issue's **requester**,
gets an acknowledgement, and comments marked for them go out by e-mail, so the conversation goes
on by mail. Auto-replies, bulk mail, bounces and loops are kept out. TaskCraft reads an IMAP
mailbox or takes messages from your mail server. See [E-mail in](#e-mail-in).

**Dashboards**: pages people put together from widgets — an issue list or a count for any query
or saved filter, a breakdown by status / category / assignee / priority / type / project / label /
sprint, the sprint burndown, velocity, cumulative flow, created vs resolved, recent activity, your
own open work and notes. Private or shared with everyone; every number opens the issues behind it.

**Portfolio**: the epics and releases of several projects on one timeline (bars filled as their
issues get done, late ones marked), and how much open work each team member carries across them.

**Releases**: versions with progress, release dates and a release action that moves unfinished
issues on.

**Live**: boards, backlogs and open issues update as others work (Server-Sent Events, relayed
through Postgres so it works with several replicas).

**Interface**: English, German, French and Italian (more languages are a JSON catalog away, see
below; each person picks theirs, else the workspace default, else the browser's), light, dark and
high-contrast themes, phone layout, checked with axe against WCAG 2.2 AA in the browser tests.

**Git**: commits, branches and pull / merge requests from GitHub, GitLab (or any server, through a
small JSON format) appear on the issues whose keys they mention, with state badges on the issue and
its cards; rules move issues when a branch is pushed or a pull request is merged; a branch name
helper on every issue. See [Git integration](#git-integration).

**AI assistants**: a built-in [MCP](https://modelcontextprotocol.io) server — Claude & co. search
(TQL), read, create and update issues, comment, log time and read sprints, boards and reports with
the user's permissions. See [AI assistants (MCP)](#ai-assistants-mcp).

**Administration**: workspace owner / admins / members / guests; project roles (viewer, member,
admin) for people and teams, a default role per project; invitations and password-reset links;
teams; personal API tokens (read-only or read/write); signed webhooks; audit log. With SMTP
configured, invitations and password-reset links are e-mailed and people can reset a forgotten
password themselves.

**Sign-in**: local passwords and single sign-on — OpenID Connect (Microsoft Entra ID, Google,
GitLab, Okta, Keycloak, Authentik, …), GitHub, LDAP / Active Directory and SAML 2.0 — with
accounts created on first sign-in, linked by verified e-mail, and provider groups mirrored into
teams (see [below](#sign-in-sso-ldap--active-directory-saml)).

**Security**: two-factor authentication with an authenticator app (TOTP, QR code setup) and ten
one-time recovery codes; **passkeys** (WebAuthn) as the second step or to sign in without a
password; *where you're signed in* with "sign out everywhere else"; a workspace setting *Require
two-factor authentication* (off / owners and admins / everyone) — people it applies to see only the
setup until they've added a second factor — and admins reset the second factor of someone who lost
their phone. See [Security](#security) below.

## WikiCraft integration

TaskCraft and WikiCraft work as a pair:

- **In TaskCraft**: link wiki pages to issues (their titles and spaces are looked up live), give
  each project a wiki space (the *Docs* tab shows its recent pages and searches them), and write
  pages from your work — a spec page from an issue, a **sprint review** page when a sprint ends
  (goal, figures, finished and unfinished work, retro sections), **release notes** grouped by
  issue type.
- **In WikiCraft**: TaskCraft issue chips with live status, live issue tables from TQL queries
  (`/taskcraft`), "Linked issues" on pages that issues link, and creating a TaskCraft issue from
  selected text (WikiCraft's side, configured with `WIKICRAFT_TASKCRAFT_URL` and
  `WIKICRAFT_TASKCRAFT_TOKEN`).

Connect TaskCraft to WikiCraft with `TASKCRAFT_WIKICRAFT_URL` and a WikiCraft API token
(`TASKCRAFT_WIKICRAFT_TOKEN`, used for everyone), or let people connect their own token under
*Profile & settings* so page lookups respect their own wiki permissions. For the other direction,
create a read-only TaskCraft token (*Profile & settings → API tokens*) for WikiCraft.

## Git integration

Connect repositories in *Project settings → Integrations → Git*: pick GitHub, GitLab or *Other*,
give the repository's web address, and TaskCraft shows the webhook address and a secret — the
secret only once (make a new one there if it gets lost), with the setup steps:

| Provider | Webhook address | Authenticity | Events |
|---|---|---|---|
| GitHub (also Gitea, Forgejo) | `POST /api/integrations/git/github/{key}` | `X-Hub-Signature-256` (HMAC-SHA256 of the body with the secret) | `push`, `pull_request` (content type `application/json`) |
| GitLab | `POST /api/integrations/git/gitlab/{key}` | `X-Gitlab-Token: <secret>` | Push events, Merge request events |
| Other | `POST /api/integrations/git/generic/{key}` | `X-TaskCraft-Token: <secret>` or `X-Hub-Signature-256` | the JSON below |

Issue keys (`WEB-12`, any case, at word boundaries — also under a project's former keys) are
found in commit messages, branch names and pull request titles, descriptions and source branches.
Each one becomes a *development link* on the issue (one per address: commits, branches, pull
requests with their state open / merged / closed, author and time), shown in the issue's
*Development* section, with a history entry. Only issues of the repository's own project are
linked. Optional rules per project: *when a branch is pushed or a pull request opened* and *when a
pull request is merged*, move the linked issues to a status — as Automation, only forward in the
workflow and, in restricted workflows, only along allowed transitions. *Branch name* on an issue
copies `web-12-short-title` (or `git checkout -b …`).

The generic format, every part optional:

```json
{
  "commits": [{"sha": "3f2a9c1", "message": "WEB-12 Fix the login timeout", "url": "https://git.example.com/shop/commit/3f2a9c1",
               "author": "Max", "timestamp": "2026-11-12T10:00:00Z"}],
  "branch": {"name": "web-12-login-timeout", "url": "https://git.example.com/shop/tree/web-12-login-timeout", "author": "Max"},
  "pullRequest": {"number": 7, "title": "Fix the login timeout", "body": "Closes WEB-12", "url": "https://git.example.com/shop/pulls/7",
                  "state": "open", "author": "Max", "branch": "web-12-login-timeout"}
}
```

`state` is `open`, `merged` or `closed`; commit and branch addresses default to
`<repository>/commit/<sha>` and `<repository>/tree/<branch>`.

```bash
curl -X POST "$WEBHOOK" -H 'Content-Type: application/json' -H "X-TaskCraft-Token: $SECRET" \
  -d '{"commits": [{"sha": "3f2a9c1", "message": "WEB-12 Fix the login timeout"}]}'
```

## AI assistants (MCP)

TaskCraft has a built-in [MCP](https://modelcontextprotocol.io) server at `/api/mcp` (also
`/api/v1/mcp`; streamable HTTP, stateless, so any replica answers). Create a personal API token
(*Profile & settings → API tokens*) and point your MCP client at it:

```bash
claude mcp add --transport http taskcraft https://tasks.example.com/api/mcp --header "Authorization: Bearer tct_…"
```

or in a project's `.mcp.json` (shared with the team; each person sets `TASKCRAFT_TOKEN`):

```json
{
  "mcpServers": {
    "taskcraft": {
      "type": "http",
      "url": "https://tasks.example.com/api/mcp",
      "headers": {"Authorization": "Bearer ${TASKCRAFT_TOKEN}"}
    }
  }
}
```

Claude Desktop starts local (stdio) servers from `claude_desktop_config.json`; connect it through
the `mcp-remote` bridge (needs Node.js):

```json
{
  "mcpServers": {
    "taskcraft": {
      "command": "npx",
      "args": ["-y", "mcp-remote", "https://tasks.example.com/api/mcp", "--header", "Authorization:${AUTH}"],
      "env": {"AUTH": "Bearer tct_…"}
    }
  }
}
```

Tools: `search_issues` (TQL, explained in the tool's description), `get_issue` (fields, description
as Markdown, sub-tasks, links, development links, comments), `create_issue` and `update_issue`
(types, statuses, sprints and people by name; restricted workflows apply), `add_comment`
(Markdown), `log_work`, `list_projects` (statuses, types, running sprint; one project's full setup),
`get_sprint`, `get_board`, `run_report` (overview, burndown, velocity, cumulative flow, cycle time,
created vs resolved, workload as JSON). Every call runs with the token owner's permissions; give an
assistant that should only look a **read-only** token: the changing tools then refuse. The server
only answers for its own host names (`TASKCRAFT_BASE_URL`, localhost), against DNS rebinding.

## Quick start (Docker)

```bash
export TASKCRAFT_DB_PASSWORD=$(openssl rand -hex 16)
docker compose up -d          # builds the image on first start
open http://localhost:8788    # the first visit sets up the workspace (with sample data if you like)
```

## Development

Requirements: Rust ≥ 1.94, Node 20.19+ or 22.12+, PostgreSQL ≥ 15 (or Docker).

```bash
docker run -d --name taskcraft-pg -p 127.0.0.1:55433:5432 \
  -e POSTGRES_USER=taskcraft -e POSTGRES_PASSWORD=taskcraft -e POSTGRES_DB=taskcraft postgres:17-alpine

cd web && npm install && npm run build && cd ..
cargo run -p taskcraft -- serve          # http://127.0.0.1:8788 (applies migrations)
cargo run -p taskcraft -- demo           # sample projects in an existing workspace

cd web && npm run dev                    # http://127.0.0.1:5200 with hot reload (proxies /api)

cargo test --workspace                   # unit + integration tests (fresh database per test)
```

Release builds embed `web/dist`, so the binary alone serves the app.

The single sign-on tests (`crates/server/tests/sso.rs`) run their own mock OpenID Connect provider;
the LDAP tests run when an lldap test directory is reachable and the SAML test needs
`--features saml` and a SimpleSAMLphp test IdP (both described at the top of that file).

Browser tests (Playwright; a journey through a fresh workspace with sample data, including axe
accessibility checks):

```bash
cd web && npm run build && cd ../e2e
npm ci && npx playwright install chromium
npx playwright test          # starts the server on :8798 with a fresh database (scripts/e2e-server.sh)
```

Interface texts go through `t()` with the English text as the key; catalogs are
`web/src/locales/<code>.json` (`de`, `fr`, `it`). Plural forms are objects keyed by the language's
`Intl.PluralRules` categories (`one`, `many`, `other`, …); a key ending in a note like
`Done [[close]]` is translated without the note. `node web/scripts/i18n.mjs extract` refreshes
`messages.json` (every key) and `node web/scripts/i18n.mjs check de fr it` lists missing
translations and lost `{placeholders}`. A new language is a new catalog: once
`web/src/locales/<code>.json` exists, the language (listed in `web/src/i18n.ts` with its own name)
shows up in the profile and workspace pickers.

### CLI

```bash
taskcraft serve        # web server (default)
taskcraft migrate      # apply migrations
taskcraft demo         # add sample data to the workspace
taskcraft healthcheck  # probe a running server (container HEALTHCHECK)
taskcraft backup FILE  # database + attachments into one .tar.gz (`-` writes to standard output)
taskcraft restore FILE # into an empty database (`--force` replaces an existing workspace)
```

### Backup and restore

`taskcraft backup taskcraft-2026-11-12.tar.gz` writes the whole workspace — every table, taken in
one consistent snapshot while the server keeps running, and all attachments — into one file
(`--no-files` leaves the attachments out when the S3 bucket is backed up on its own). With
Docker: `docker compose exec taskcraft taskcraft backup - > taskcraft.tar.gz`.

`taskcraft restore taskcraft.tar.gz` loads it into an empty database (stop the server first; a
database that already holds a workspace is only replaced with `--force`). The schema is built up to
the backup's version, the data loaded, then this version's newer migrations run — so a backup from
an older TaskCraft restores into a newer one, while a backup from a newer one is refused before
anything is touched. Attachments go into whatever storage is configured, which also moves them
from the data directory into S3 or back.

### Kubernetes

[`deploy/kubernetes`](deploy/kubernetes) runs TaskCraft with several replicas (attachments in
S3-compatible storage, PostgreSQL outside the cluster or from an operator), a disruption budget,
an autoscaler, an ingress with the timeouts live updates need, and a nightly backup job that keeps
14 days on a volume:

```bash
cp deploy/kubernetes/secret.example.yaml deploy/kubernetes/secret.yaml   # git-ignored
$EDITOR deploy/kubernetes/{secret,configmap,ingress}.yaml               # database, bucket, host
kubectl apply -k deploy/kubernetes
```

Replicas need no sticky sessions: live updates, scheduled automation, e-mail and webhook delivery
coordinate through PostgreSQL. Build the image with `docker build -t <registry>/taskcraft .` and
set it under `images:` in `kustomization.yaml`.

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `TASKCRAFT_DATABASE_URL` | `postgres://taskcraft:taskcraft@127.0.0.1:55433/taskcraft` | PostgreSQL |
| `TASKCRAFT_BIND` | `127.0.0.1:8788` | listen address |
| `TASKCRAFT_BASE_URL` | `http://127.0.0.1:8788` | public URL (links in pages, webhooks, invitations); its host name is the passkeys' relying party |
| `TASKCRAFT_DATA_DIR` | `./data` | local attachments and uploads |
| `TASKCRAFT_STATIC_DIR` | `./web/dist` | built frontend; the binary's embedded copy is used when this holds no build |
| `TASKCRAFT_S3_BUCKET` / `_ENDPOINT` / `_REGION` / `_ACCESS_KEY_ID` / `_SECRET_ACCESS_KEY` | — | attachments in S3-compatible storage |
| `TASKCRAFT_MAX_UPLOAD_MB` | `256` | largest attachment |
| `TASKCRAFT_SECURE_COOKIES` | `false` | `true` behind HTTPS (an `https://` base URL also enables HSTS) |
| `TASKCRAFT_SESSION_DAYS` | `30` | session lifetime |
| `TASKCRAFT_LOG_FORMAT` | `text` | `json` for log shippers |
| `TASKCRAFT_WEBHOOK_ALLOW_PRIVATE` | `false` | let webhooks call private / loopback addresses |
| `TASKCRAFT_WIKICRAFT_URL` | — | WikiCraft for linked pages, project docs and generated pages |
| `TASKCRAFT_WIKICRAFT_TOKEN` | — | WikiCraft API token used when a person hasn't connected their own |
| `TASKCRAFT_AUTH_PROVIDERS` / `_FILE` | — | sign-in providers as a JSON list, or a file holding it (below) |
| `TASKCRAFT_GOOGLE_CLIENT_ID` / `_CLIENT_SECRET` / `_ALLOWED_DOMAINS` | — | "Continue with Google" |
| `TASKCRAFT_MICROSOFT_CLIENT_ID` / `_CLIENT_SECRET` / `_TENANT` / `_ALLOWED_DOMAINS` | — | "Continue with Microsoft" (Entra ID; the tenant id is required) |
| `TASKCRAFT_OIDC_ISSUER` / `_CLIENT_ID` / `_CLIENT_SECRET` / `_NAME` / `_ALLOWED_DOMAINS` / `_AUTO_CREATE` | — | one OpenID Connect provider (id `oidc`) |
| `TASKCRAFT_DISABLE_PASSWORD_LOGIN` | `false` | sign in through providers only (LDAP accounts still use the password form) |
| `TASKCRAFT_SMTP_URL` | — | outgoing mail: `smtps://user:pass@mail.example.com` (TLS, port 465), `smtp://user:pass@mail.example.com:587?tls=required` (STARTTLS) or `smtp://relay:25` (unencrypted, local relays only); URL-encode special characters in the password. Enables e-mailed invitations, "forgot password" and notification e-mails |
| `TASKCRAFT_MAIL_FROM` | `TaskCraft <taskcraft@localhost>` | sender of e-mails (its domain also names their Message-IDs) |
| `TASKCRAFT_IMAP_URL` | — | e-mail in: a mailbox TaskCraft reads every minute — `imaps://user:pass@imap.example.com/INBOX` (TLS, port 993), `imap://user:pass@imap.example.com/INBOX?tls=starttls` or `imap://…` (unencrypted, local servers only); `?move=Folder` moves handled messages there instead of marking them read |
| `TASKCRAFT_MAIL_IN_TOKEN` | — | e-mail in: the secret mail servers and services send to `POST /api/integrations/mail-in` (the endpoint is off without it) |

### Sign-in: SSO, LDAP / Active Directory, SAML

Local accounts (e-mail + password) work out of the box. Any number of external providers can be
added; the login page shows a button per provider, and LDAP accounts use the normal password form
("E-mail or username"). `TASKCRAFT_AUTH_PROVIDERS` (or a mounted file via
`TASKCRAFT_AUTH_PROVIDERS_FILE`) takes a JSON list:

```json
[
  {"type": "oidc", "id": "microsoft", "preset": "microsoft", "tenant": "<tenant-id>",
   "clientId": "…", "clientSecret": "…", "groupsClaim": "groups", "syncGroups": true},
  {"type": "oidc", "id": "google", "preset": "google", "clientId": "…", "clientSecret": "…",
   "allowedDomains": ["acme.com"]},
  {"type": "oidc", "id": "keycloak", "name": "Acme SSO", "issuer": "https://sso.acme.com/realms/acme",
   "clientId": "taskcraft", "clientSecret": "…"},
  {"type": "github", "id": "github", "clientId": "…", "clientSecret": "…", "allowedOrgs": ["acme"]},
  {"type": "ldap", "id": "ad", "name": "ACME directory", "url": "ldaps://dc1.acme.local",
   "bindDn": "CN=taskcraft,OU=Service,DC=acme,DC=local", "bindPassword": "…",
   "baseDn": "DC=acme,DC=local", "syncGroups": true},
  {"type": "saml", "id": "okta", "name": "Okta", "idpMetadataUrl": "https://acme.okta.com/app/…/sso/saml/metadata"}
]
```

| Type | Covers | Notes |
|---|---|---|
| `oidc` | Microsoft Entra ID (`preset: microsoft` + `tenant`), Google (`preset: google`), GitLab (`preset: gitlab`; self-hosted with `issuer`), Okta, Keycloak, Authentik, Auth0, Zitadel, … (`issuer`) | authorization code + PKCE + nonce; redirect URI `{BASE_URL}/api/auth/sso/{id}/callback`; `groupsClaim` names the claim with group names, `scopes` adds scopes |
| `github` | GitHub and GitHub Enterprise (`apiUrl`, `webUrl`) | verified primary e-mail; `allowedOrgs` restricts sign-in to organization members; callback `{BASE_URL}/api/auth/sso/{id}/callback` |
| `ldap` | OpenLDAP, Active Directory, FreeIPA, lldap, … | search with a service account (`bindDn`), then bind as the person; `startTls`; the default filter matches `uid`, `mail`, `sAMAccountName`, `userPrincipalName` (`userFilter` to override, `{username}` is escaped); `emailAttribute`, `nameAttribute`, `idAttribute`, `groupAttribute` (`memberOf`) |
| `saml` | Okta, Entra ID, ADFS, Keycloak, Shibboleth, OneLogin, … | needs a build with the cargo feature `saml` (`cargo build --release --features saml`, or `docker build --build-arg FEATURES=saml .`; it links libxml2 / libxmlsec1); SP metadata at `{BASE_URL}/api/auth/sso/{id}/metadata`, ACS `{BASE_URL}/api/auth/sso/{id}/acs`; signed responses only (no SHA-1), SP-initiated only; `idpMetadataUrl` or `idpMetadataXml`, `emailAttribute`, `nameAttribute`, `groupsAttribute` |

Common options: `name` (button label), `allowedDomains` (only these e-mail domains),
`trustEmail` (use e-mail addresses the provider doesn't mark as verified — default on for LDAP,
SAML and single-tenant Microsoft, off otherwise; `true` also allows linking existing admin
accounts), `autoCreate` (create accounts on first sign-in; default on for private providers and
for public ones restricted by `allowedDomains` / `allowedOrgs`, off for unrestricted Google /
GitHub / gitlab.com), `defaultRole` (`member` / `guest`), `syncGroups` (mirror the provider's
groups into TaskCraft **teams** — and with them the project roles those teams hold; a name already
used by a local team gets a " (provider id)" suffix instead of taking that team over).

Accounts are linked by the provider's stable subject id; on first sign-in an existing account with
the same verified e-mail is linked, and an invited person's invitation is accepted. Deactivated
people can't sign in through any provider. *Admin → Users* shows how each person signs in.
With `TASKCRAFT_DISABLE_PASSWORD_LOGIN=true`, local passwords (including invitation and
password-reset links) stop working; the first-run setup still asks for the owner's password and
signs them in, and afterwards the owner signs in through a provider with the same e-mail address
(a verified one, or set `trustEmail` for that provider).

### E-mail

With `TASKCRAFT_SMTP_URL` set, e-mails go through an outbox in Postgres that every replica's worker
sends from (retried with backoff for about two hours; *Administration → Overview* shows the state
and sends a test e-mail). Each person picks under *Profile & settings → Notifications* how
notifications reach them by e-mail: a **daily digest** (the default; 08:00 in their time zone,
grouped by issue), **immediately** (a burst of changes within about two minutes becomes one e-mail;
nothing is sent for what they've read or while they're using the app) or **off**. Notification
kinds they switched off aren't e-mailed either, and every e-mail links to these settings.

### E-mail in

A project admin turns e-mail on under *Project settings → E-mail*: the project then takes mail at
an address of its own (`support@acme.com`) and at any address with `+KEY` before the `@`
(`tasks+SUP@acme.com`), matched against To, Cc, Delivered-To and X-Original-To. TaskCraft gets the
mail one of two ways:

- **It reads a mailbox** every minute (`TASKCRAFT_IMAP_URL`; one replica at a time). Give TaskCraft
  a mailbox of its own and deliver or forward the projects' addresses to it. Handled messages are
  marked read (or moved, `?move=Folder`); messages that failed for a passing reason stay unread and
  are tried again. When the mailbox's user name is an e-mail address, its `+KEY` addresses are
  shown in the settings.
- **Your mail server hands messages over** to `POST /api/integrations/mail-in` with the token from
  `TASKCRAFT_MAIL_IN_TOKEN`. It answers 200 whatever became of a message (also when it was refused,
  so nothing bounces back to senders) and 503 when it should be sent again later. With Postfix:

  ```text
  # /etc/postfix/master.cf
  taskcraft unix  -       n       n       -       -       pipe
    flags=DORq user=taskcraft argv=/usr/local/bin/taskcraft-mail-in

  # /etc/postfix/transport (postmap it; main.cf: transport_maps = hash:/etc/postfix/transport)
  support@acme.com    taskcraft:
  ```

  ```sh
  #!/bin/sh
  # /usr/local/bin/taskcraft-mail-in (readable only by the pipe's user: it holds the token)
  # On trouble, exit 75 (EX_TEMPFAIL): Postfix keeps the message and tries again later.
  curl -sS --fail --max-time 120 -H 'Authorization: Bearer <token>' -H 'Content-Type: message/rfc822' \
    --data-binary @- https://tasks.example.com/api/integrations/mail-in > /dev/null || exit 75
  ```

  Mail services that post the raw message as a form (an "email" or "body-mime" field) work too;
  give them the address with `?token=<token>`.

What happens to a message:

- **Replies become comments.** The issue is found by the conversation — TaskCraft's e-mails carry
  Message-IDs naming the issue, and notification e-mails about one issue name the project's address
  as Reply-To — or by `[SUP-12]` in the subject. Quoted text ("On … wrote:", `>` lines, Outlook's
  From/Sent block) and signatures (`-- `) are left out; attached files go to the issue.
- **Other messages become issues** of the project's default type: the subject is the title, the
  text the description (HTML mail is turned into plain text and never shown as HTML), files become
  attachments (each up to `TASKCRAFT_MAX_UPLOAD_MB`; small inline images such as logos are skipped).
- **Who may write**: people with an account write from its address and act as themselves, with
  their permissions (members open issues, viewers comment). For support trackers, *Anyone* also
  lets in people without an account: the sender becomes the issue's **requester** (shown on the
  issue), gets an acknowledgement with the key, and their answers become comments. Comments stay
  internal unless *E-mail this comment to the requester* is ticked (API: `emailRequester: true`);
  those go out under the project's name with its address as Reply-To, so the conversation goes on
  by e-mail. Replies team members send by e-mail stay internal. The assignee (else the project
  lead) follows issues requesters open; for service levels, a team member's comment is the first
  response, the requester's own aren't.
- **Kept out**: automatic replies (`Auto-Submitted`, `X-Autoreply`, …), bulk and list mail
  (`Precedence: bulk/list/junk` — forward with an alias, not through a mailing list), bounces,
  mail from TaskCraft's own address or a project's address, senders your mail server found forged
  (`dmarc=fail` in its `Authentication-Results`), more than 30 messages per sender and hour,
  messages over 40 MB, and anything received before (same Message-ID). Acknowledgements are sent
  at most 5 times per address and hour and marked `Auto-Submitted: auto-replied`; notification
  e-mails are marked `auto-generated`.

*Project settings → E-mail* lists the latest messages and what became of each (workspace admins
also see those that reached no project); the log is kept for 90 days.

## API

Everything the web app does is available under `/api/v1` (the app itself uses `/api`).
Authenticate with a personal token: `Authorization: Bearer tct_…` (*Profile & settings → API
tokens*; read-only tokens may only read and search). Browser sessions must send `x-taskcraft: 1`
on writes.

| Area | Endpoints |
|---|---|
| Issues | `POST /issues` (`projectKey`, `title`, `descriptionMarkdown` or ProseMirror `description`, any field), `GET/PATCH/DELETE /issues/{KEY}`, `POST /issues/{KEY}/move` (`statusId`, `sprintId`, `after`/`before`), `/restore`, `/clone`, `GET/POST /issues/{KEY}/move-project` (preview / move to another project), `GET /issues/{KEY}/history`, `PUT /issues/{KEY}/watch`, `POST /issues/{KEY}/links`, `/web-links`, `/comments`, `/worklogs`, `/attachments`, `POST /issues/bulk` |
| Search | `GET /search?q=<TQL>&limit=&offset=&project=`, `GET /search.csv`, `GET /quick?q=`, `POST /query/validate`, `GET /query/suggest?field=` |
| Projects | `GET/POST /projects`, `GET/PATCH/DELETE /projects/{KEY}`, members, statuses, transitions, types, versions, components, fields, automation, templates (`/projects/{KEY}/templates`, `POST /templates/{id}/create`), service-level queue (`/projects/{KEY}/sla`), CSV import (`POST /projects/{KEY}/import/preview`, `/import` with `mapping`, `dryRun`), boards, `GET /projects/{KEY}/backlog`, `/sprints`, `/trash`, `/reports/{overview,velocity,cumulative-flow,cycle-time,created-resolved,workload}` |
| Sprints & boards | `GET /boards/{id}`, `POST /sprints/{id}/start`, `/complete`, `GET /sprints/{id}/burndown` |
| WikiCraft | `GET /issues/{KEY}/wiki-pages`, `POST /issues/{KEY}/wiki-page`, `POST /sprints/{id}/wiki-page`, `POST /versions/{id}/wiki-page`, `GET /projects/{KEY}/wiki`, `GET /wiki-links?url=` (issues linking a page) |
| Dashboards | `GET/POST /dashboards`, `GET/PATCH/DELETE /dashboards/{id}`, `GET /search/groups?q=<TQL>&by=status\|category\|assignee\|priority\|type\|project\|labels\|sprint`, `GET /portfolio?projects=KEY,KEY` |
| Git | `GET/POST /projects/{KEY}/git` (connected repositories; the secret comes back once), `PUT /projects/{KEY}/git/rules`, `DELETE /git-integrations/{id}`, `POST /git-integrations/{id}/secret`, `GET /issues/{KEY}/development`, webhooks `POST /integrations/git/{github,gitlab,generic}/{key}` |
| E-mail in | `POST /integrations/mail-in` (one raw RFC 822 message as the body, or the form field `email` / `body-mime`; `Authorization: Bearer <TASKCRAFT_MAIL_IN_TOKEN>`, `X-TaskCraft-Token` or `?token=`; answers what became of it), `GET/PUT /projects/{KEY}/mail-in` (`enabled`, `address`, `senders`: `members` / `anyone`; with the latest messages), `POST /issues/{KEY}/comments` with `emailRequester: true` e-mails the comment to the issue's requester |
| MCP | `POST /mcp` (streamable HTTP, see above) |
| Live | `GET /events` (Server-Sent Events) |
| Account & admin | `/me`, `/me/tokens`, `/notifications`, `/views`, `/timesheet`, `/people`, `/teams`, `/users`, `/auth/forgot`, `/admin/{workspace,stats,audit,webhooks,mail}`, `POST /admin/mail/test` |
| Sign-in | `GET /auth/providers`, `GET /auth/sso/{id}/login?next=`, `GET /auth/sso/{id}/callback`, SAML `POST /auth/sso/{id}/acs` and `GET /auth/sso/{id}/metadata`, `GET /me/sign-in` |
| Security | `POST /auth/mfa` (`mfaToken`, `code` or `passkey`), `POST /auth/mfa/passkey`, `POST /auth/passkey/options`, `POST /auth/passkey`, `GET /me/mfa`, `POST /me/mfa/{setup,enable,disable,recovery-codes}`, `GET/POST /me/passkeys`, `POST /me/passkeys/options`, `PATCH/DELETE /me/passkeys/{id}`, `GET /me/sessions`, `DELETE /me/sessions/{id}`, `POST /me/sessions/revoke-others`, `GET/PUT /admin/security`, `POST /users/{id}/reset-mfa` (browser sessions only for changes) |

### Webhooks

Admins register endpoints for `issue.created`, `issue.updated`, `issue.deleted`,
`comment.created`, `sprint.started`, `sprint.completed` and `version.released` (all or per project).
Each delivery is a JSON `POST` `{event, timestamp, webhookId, deliveryId, data}` with
`X-TaskCraft-Event` and `X-TaskCraft-Signature: sha256=<HMAC of the body with the secret>`, retried
with backoff for about eight hours.

## Security

- **Passwords** are hashed with Argon2; failed logins are throttled per e-mail address (10 per
  15 minutes, shared by all replicas). Sessions are `HttpOnly`, `SameSite=Lax` cookies (`Secure`
  with `TASKCRAFT_SECURE_COOKIES`); writes need the `x-taskcraft` header (CSRF).
- **Two-factor authentication** (*Profile & settings → Security*): an authenticator app (TOTP,
  RFC 6238, set up by QR code) with ten one-time recovery codes, and/or **passkeys**. After a
  correct password there is no session yet, only a ten-minute second step: five tries per
  sign-in, and wrong codes count against a per-person limit a new login doesn't reset; each TOTP
  code works once. A password-reset link doesn't get past the second factor either.
- **Passkeys** (WebAuthn, checked by TaskCraft itself: ES256, EdDSA and RS256 keys, signature
  counters) serve as the second step, or sign in without a password when the authenticator
  verified the person (PIN, fingerprint, face). They are bound to the host name of
  `TASKCRAFT_BASE_URL`, so that must be the address people use, over https (or `localhost`).
- **Changes to sign-in methods** (setting up the app, adding or removing a passkey) need a sign-in
  from the last 15 minutes or the password; turning the app off always needs the password. They
  are refused for API tokens, so a leaked token can't take over the account.
- **Require two-factor authentication** (*Administration → Overview*): off, owners and admins, or
  everyone. People it applies to who have no second factor yet can only set one up (or sign out);
  the last second factor can't be removed while it's required. API tokens are separate
  credentials and keep working.
- **Sessions**: *Where you're signed in* lists browsers with their last activity; sign out one or
  everywhere else. Changing the password also signs out everywhere else.
- **Lost phone**: admins reset someone's second factor (*Administration → Users*; only the owner
  resets admins). That removes the app and passkeys, signs the person out everywhere and deletes
  their API tokens; they sign in with their password and set up a new second factor.

## Architecture

```
web/              React 19 + Vite, TanStack Query, Tiptap (rich text), hand-drawn SVG charts
crates/tql        the query language: lexer, parser and syntax tree (no I/O)
crates/server     axum 0.8 HTTP API, sqlx / PostgreSQL, TQL → SQL, history replay for reports,
                  automation, webhooks, live updates (SSE + LISTEN/NOTIFY), WikiCraft client, Git webhooks,
                  MCP server, e-mail in (IMAP / HTTP) → single binary
```

Every change to an issue is written to `issue_events`; the activity stream, notifications,
automation, webhooks and the flow reports all read from it.

See [PLAN.md](PLAN.md) for the roadmap.

## License

MIT or Apache-2.0, at your option.
