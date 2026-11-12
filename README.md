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
- **Comments** with mentions and reactions, **watchers**, in-app **notifications**, a full
  **history** of every change, **time logs** and a weekly **timesheet**
- **Workflows**: statuses in three categories (to do, in progress, done); free or restricted
  transitions with a transition matrix
- **Move issues to another project** (the old key keeps working; type, status and custom fields are mapped, sub-tasks move along)
- **CSV import** with column mapping and a dry run (statuses, types, people, sprints and releases matched by name; keys from the other tool keep working)
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

**Interface**: English and German (more languages are a JSON catalog away, see below), light, dark
and high-contrast themes, phone layout, checked with axe against WCAG 2.2 AA in the browser tests.

**Administration**: workspace owner / admins / members / guests; project roles (viewer, member,
admin) for people and teams, a default role per project; invitations and password-reset links;
teams; personal API tokens (read-only or read/write); signed webhooks; audit log.

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

Browser tests (Playwright; a journey through a fresh workspace with sample data, including axe
accessibility checks):

```bash
cd web && npm run build && cd ../e2e
npm ci && npx playwright install chromium
npx playwright test          # starts the server on :8798 with a fresh database (scripts/e2e-server.sh)
```

Interface texts go through `t()` with the English text as the key; catalogs are
`web/src/locales/<code>.json`. `node web/scripts/i18n.mjs check de` lists missing translations.

### CLI

```bash
taskcraft serve        # web server (default)
taskcraft migrate      # apply migrations
taskcraft demo         # add sample data to the workspace
taskcraft healthcheck  # probe a running server (container HEALTHCHECK)
```

## Configuration

| Variable | Default | Meaning |
|---|---|---|
| `TASKCRAFT_DATABASE_URL` | `postgres://taskcraft:taskcraft@127.0.0.1:55433/taskcraft` | PostgreSQL |
| `TASKCRAFT_BIND` | `127.0.0.1:8788` | listen address |
| `TASKCRAFT_BASE_URL` | `http://127.0.0.1:8788` | public URL (links in pages, webhooks, invitations) |
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
| Live | `GET /events` (Server-Sent Events) |
| Account & admin | `/me`, `/me/tokens`, `/notifications`, `/views`, `/timesheet`, `/people`, `/teams`, `/users`, `/admin/{workspace,stats,audit,webhooks}` |

### Webhooks

Admins register endpoints for `issue.created`, `issue.updated`, `issue.deleted`,
`comment.created`, `sprint.started`, `sprint.completed` and `version.released` (all or per project).
Each delivery is a JSON `POST` `{event, timestamp, webhookId, deliveryId, data}` with
`X-TaskCraft-Event` and `X-TaskCraft-Signature: sha256=<HMAC of the body with the secret>`, retried
with backoff for about eight hours.

## Architecture

```
web/              React 19 + Vite, TanStack Query, Tiptap (rich text), hand-drawn SVG charts
crates/tql        the query language: lexer, parser and syntax tree (no I/O)
crates/server     axum 0.8 HTTP API, sqlx / PostgreSQL, TQL → SQL, history replay for reports,
                  automation, webhooks, live updates (SSE + LISTEN/NOTIFY), WikiCraft client → single binary
```

Every change to an issue is written to `issue_events`; the activity stream, notifications,
automation, webhooks and the flow reports all read from it.

See [PLAN.md](PLAN.md) for the roadmap.

## License

MIT or Apache-2.0, at your option.
