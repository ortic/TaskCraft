# Changelog

All notable changes. Versions follow [Semantic Versioning](https://semver.org/); a release is a
`v*` tag, which publishes the container image and the Linux binaries (see
`.github/workflows/release.yml`).

## [Unreleased]

- **Test management**: projects get **test cases** (`WEB-T12`) with preconditions, numbered
  steps and an expected result each — or a Given / When / Then scenario —, a priority, labels
  and folders. Cases are **versioned**; a run records the version it executed. A case **covers**
  issues, and an issue shows its test cases with their latest results. **Test plans** pick cases
  by hand or by a filter, optionally for a release; **run templates** add an environment and
  testers. In a **test run** every step gets passed / failed / blocked / skipped with a comment
  and files, and a failure becomes a **bug** in one click — filled in with the case, the failing
  step, expected and actual, the environment and the files, and related to the covered issues.
  **Reports**: progress and pass rate of runs, where the cases stood day by day (replayed from
  the stored results), flaky cases, and coverage — issues without tests or with failing ones; a
  **release** shows its runs and open failures. TQL `hasTests`, `testResult = failed`,
  `testRun = "Regression"`; project permissions *execute tests* and *manage tests*; automation
  triggers *a test fails* and *a test run is finished*; CSV import and export; REST, the MCP
  tools `test_cases` and `create_test_case`; part of backups and project exports; a feature
  switch. (#1)
- **Asset inventory**: the devices, services and other things issues are about. Assets live in
  **sets** (Hardware, Services, …) with their own roles — viewer, editor, admin, for people,
  teams or everyone — and have a **type** with its own fields (text, number, date, choice,
  checkbox, link, person, or a reference to another asset), a tag such as `LT-0042`, a status, a
  category and an owner (a person, a team, or a requester's address). Assets **depend on** or are
  **installed in** others; an asset's page draws what fails with it. Issues name the assets
  they're about (both pages link to each other; TQL `asset = "LT-0042"`, `asset.type = Laptop`),
  and signed-in requesters pick from their own assets on a portal form. **CSV import** with
  column mapping and a check before anything changes (rows with a known tag update), CSV export,
  and **rules** per set: when an asset is added or changes, or some days before one of its dates
  ("30 days before the warranty ends"), create an issue, notify the owner or set the status.
  REST (`/asset-sets`, `/assets`, …), the MCP tool `find_assets`, and a feature switch. (#6)
- **Customers**: organizations carry a customer number and a contact and can be archived;
  projects (and their subprojects) belong to a customer, and a time log can name another one.
  The time report groups and filters by customer, with billable and internal time per month, and
  a dashboard widget shows time per customer. Optional **hourly rates** (per customer, person,
  category) give amounts in the report and its CSV for admins; a **closed period** keeps billed
  time from changing. TQL `customer = "Acme"`.

- **AI coding agents** (*Administration → Agents*): define agents — name, `@handle`,
  instructions, repository and base branch — and assign them issues or mention them in comments.
  A runner (`taskcraft-runner`, in the release archives; register it with a one-time token) works
  on a checkout and the result becomes a branch and a **draft pull request** linked to the
  issue, or an answer in a comment. Git and model credentials stay on the server; the runner
  gets a token for one run. The issue shows queued / working, a live log and *Stop*; agents are
  their own accounts with their own permissions, start in a testing mode for admins, and their
  runs (with token use) are listed and audited. A workspace switch turns them off. See
  `docs/agents.md`.

- **Scripts and extensions**: automation rules can *run a script* — a few sandboxed lines
  (Rhai) that see the issue and set its fields, labels and comments. *Administration →
  Extensions* installs packages (a manifest and a script) with custom automation actions, event
  handlers, a scheduled job and routes under `/api/ext/<name>/`, each limited to the
  capabilities its manifest asks for: issues as the person it runs as, HTTP to named hosts, a
  little state, secrets. Limits on time and work, a log per extension, one switch to turn it
  off, a versioned extension API (`docs/extensions.md`). Rules show why their last run failed.

- **`tc`, a command-line client** (a small separate binary in the release archives and the
  image): `tc issue list` (TQL) / `view` / `create` / `edit` / `move` / `assign`, `tc comment`,
  `tc link`, `tc log`, `tc timer`, `tc branch` (locally or on the Git server), `tc open`. The
  issue key comes from the current Git branch when left out; `--json` for scripts. `tc login`
  signs in through the browser — you approve a code and the client gets an API token of its
  own — or set `TASKCRAFT_URL` and `TASKCRAFT_TOKEN`; several servers as profiles.

- **Branches and draft pull requests from an issue**: a connected repository can carry a token
  for its server's API (GitHub, GitLab, Gitea, Forgejo); *Create branch* on an issue creates the
  branch there from a base branch — optionally with a draft pull / merge request — and links it
  before the first push. A project permission says who may; also in the REST API and as the MCP
  tool `create_branch`. Pull requests show draft state, checks / CI, reviews and reviewers from
  webhook events.

- **SCIM provisioning** (*Administration → Provisioning*): identity providers create, update and
  deactivate people and keep groups as teams through SCIM 2.0 (`/scim/v2/Users`, `/Groups`) with
  a bearer token per provider. Existing accounts are taken over by e-mail address; people are
  never deleted — deactivation ends their sessions and revokes their API tokens at once; groups
  share their teams with the group sync at sign-in. Every change is in the audit log, and the
  page shows each token's last request and last error.

- **Workflow approvals** (*Project settings → Workflow → Approval* on a status): issues entering
  the status wait for a decision. Approvers are people, teams, project roles, a person field, the
  assignee, the reporter or the requester (who decides on the portal); steps run one after
  another or all at once, each needing one, all, a number or a share of approvals. Approvers
  decide on the issue, from *Waiting for your approval* on the start page or from the link in
  the e-mail (a page that asks first), with a comment, or hand the decision to someone else;
  substitutes are asked for people who are away. Approved and rejected issues move on by
  themselves and nobody takes those two moves by hand meanwhile. Reminders and timeouts
  (reassign, skip, reject), TQL `approval = pending` and `approver = me()`, automation triggers
  *an approval is given / rejected*, service-level clocks that pause while waiting; everything
  in the history and the audit log.

- **Requester organizations** (*Organizations*): customers with members (by e-mail address) and
  mail domains — requesters from them join by themselves; requests show their organization (it
  can be changed), TQL `organization = Acme`, an organization's page lists its requests, its
  service-level goals can be shorter; members by hand or CSV. In the portal, requesters share a
  request with their organization or chosen colleagues, who follow and answer it too.

- **Time off**: absences (whole or half days, with a substitute) in *Profile & settings*, entered
  by admins for others too; why someone is away only they and admins see. Public holidays per
  region calendar (*Administration → Time off*). "Away until …" in people pickers, mentions, the
  portfolio and on the project calendar; a sprint's capacity in the backlog (working days minus
  holidays and absences, per person); substitutes hear about assignments and missed goals;
  absences in calendar feeds.

- **Calendar feeds** (*Profile & settings → Calendar feeds*): secret addresses calendar apps
  subscribe to — your work, a saved filter or a project, with due dates (from the start date),
  sprints and releases — read as their owner; shown once, renewable; the workspace can turn them
  off. Projects' calendars subscribe or download `.ics`; any search downloads as `.ics`.

- **Timer**: start one on an issue and it runs in the top bar on every device; pause, resume,
  stop and log (rounded up as the workspace says — *Administration → Time tracking*). Stopped
  timers not logged yet wait as **time drafts** only their owner sees (reports leave them out);
  reminders for timers that run long, and a stop after 24 hours. A stop or a booking sent twice is
  logged once. REST API and an MCP tool.

- **Link types** of the workspace's own (*Administration → Link types*): *causes / is caused by*,
  *implements*, … — each read its way from both issues, optionally a dependency (blocks like
  *blocks* does, arrows on the timeline); built-in types can be renamed; deleting one in use moves
  its links to another type. TQL `link = "is caused by"` and `key in linkedIssues(WEB-12, blocks)`;
  an MCP tool to link issues; imports and project exports keep the source's link types instead of
  turning them into "relates to".

- **AI assistance** (optional): summaries of long issue threads from what the reader may see, and
  searching in plain words (a question becomes a TQL query), with Claude through the Anthropic API
  or any model behind an OpenAI-compatible API — also one running in your own network; similar
  issues while typing a new one (no AI needed).
- Checklist boxes in texts are 24 px targets.

## [0.1.0] - 2026-11-13

The first release.

- **Work**: projects from scrum, kanban, issue-tracker and work-plan templates; issues with rich
  text, sub-tasks, links, attachments, comments, watchers, time logs and history; custom fields,
  components and releases (several per issue: fixed in and found in); templates and recurring
  issues; moving issues between projects; subprojects; votes.
- **Workflows**: free or restricted transitions; rules per status (who may move there, required
  fields, a comment) with a form that asks for what's missing; resolutions.
- **Time tracking**: original and remaining estimates, time logs with categories and a billable
  flag, a weekly timesheet and a time report with CSV export.
- **Views**: board with WIP limits and swimlanes, backlog with sprints and a velocity hint, list
  with TQL, timeline, calendar, timesheet, dashboards, portfolio, your work, command palette;
  saved filters with e-mailed subscriptions; Excel and CSV export; a print view. Search
  understands word forms in the workspace's language and finds the text inside files.
- **Reports** replayed from history: burndown, velocity, cumulative flow, cycle and lead time,
  created vs resolved, workload.
- **Automation** on events and schedules; service-level goals with a queue and alerts; signed
  webhooks, or messages for chat channels.
- **E-mail**: invitations, password reset, notification e-mails and digests, each in the
  person's language; e-mail in for support projects with requesters; a service desk portal.
- **Permissions**: project roles for people and teams; per project, who may delete or move
  issues, plan sprints, manage releases, moderate comments, log time for others and change who
  sees an issue; private issues (security levels); comments restricted to members, project
  admins or a team.
- **Privacy and safety**: personal data export, retention rules, an idle session timeout; upload
  rules (blocked file types, a size limit) and virus scanning with ClamAV.
- **Sign-in**: passwords, OpenID Connect, GitHub, LDAP / Active Directory, SAML; two-factor
  authentication and passkeys.
- **Integrations**: WikiCraft, Git (commits, branches, pull requests by key), an MCP server for
  assistants, a REST API with tokens, described in OpenAPI 3.1 (`/api/openapi.yaml`).
- **Import** from CSV and other trackers' exports and APIs, keeping private issues and comments
  private; **backup and restore** in one file; one project exported to another TaskCraft.
- **Operations**: one binary and PostgreSQL, S3-compatible storage, several replicas, Docker
  Compose and Kubernetes manifests; a scale check with 100 000 issues in CI.
- **Workspace settings**: logo, default time zone and date format, an announcement banner,
  features turned off for everyone (time tracking, votes, the portal, automation, sharing); profile
  photos.
- **Share links**: read-only status pages and roadmaps for people without an account, with a
  password and an end date.
- English, German, French and Italian; light, dark and high-contrast themes; WCAG 2.2 AA checked.
- Guides for users, administrators, migrating and translating in `docs/`.
