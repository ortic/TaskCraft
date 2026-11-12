# TaskCraft — Plan

> Self-hosted issue and project management with a Rust backend; the companion of WikiCraft.
> `[x]` done · `[~]` partly · `[ ]` open. Started 2026-11-12.

## Principles

1. **One binary + Postgres** (+ optional S3). Cheap to run, easy to back up, air-gap friendly.
2. **History is the source of truth.** Every change lands in `issue_events`; reports replay it
   instead of keeping snapshots, so they stay right when estimates or scope change.
3. **One query language everywhere.** Lists, boards, saved filters, automation conditions and the
   WikiCraft issue tables all speak TQL.
4. **Docs next to the work.** WikiCraft pages are first-class links; sprint reviews and release
   notes are written for you.
5. **Fast and keyboard-friendly**: command palette, shortcuts, optimistic updates, live changes.

## Done (first build, 2026-11-12)

- [x] Workspace setup, local accounts, sessions, CSRF header, login throttling, API tokens (read / write)
- [x] Roles: owner / admin / member / guest; project roles for people and teams; default role per project
- [x] Projects from four templates; statuses with categories; free or restricted workflows
- [x] Issue types with levels (epic, standard, sub-task) and description templates
- [x] Custom fields (8 kinds), versions, components
- [x] Issues: all fields, rich text with mentions and issue references, links, web links, attachments
- [x] Comments with reactions, watchers, notifications, history, time logs, timesheet
- [x] TQL: parser crate, SQL compiler with visibility, autocompletion, validation
- [x] Board (WIP limits, swimlanes, quick filters, ageing), backlog (ranking, sprints, epics), list (columns, grouping, inline edits, bulk), timeline, calendar
- [x] Sprints: start, complete with carry-over, figures kept for velocity
- [x] Reports: burndown, velocity, cumulative flow, cycle / lead time, created vs resolved, workload, overview
- [x] Automation rules; signed webhooks with retries; audit log
- [x] Live updates over SSE via Postgres LISTEN/NOTIFY (replica-safe)
- [x] WikiCraft: linked pages with live titles, project wiki space, spec / sprint review / release notes pages, "issues linking this page" for WikiCraft
- [x] Sample data; integration tests against a real Postgres; Docker image and compose; CI

## Next

- [x] E-mail (SMTP): e-mailed invitations, "forgot password", notification e-mails per person (instant bundles or a daily digest), admin status and test e-mail
- [x] Single sign-on: OpenID Connect (Entra ID, Google, GitLab, any issuer), GitHub, LDAP / Active Directory, SAML (feature `saml`); group → team sync, password sign-in can be turned off
- [x] Two-factor authentication (authenticator app, recovery codes) and passkeys; sessions list; "require two-factor" per workspace
- [x] Move issues between projects: new key (the old one keeps working), type / status mapped by name or category, custom fields by name and kind, sub-tasks along; editing time logs
- [x] CSV import with field mapping (suggested from header names, values matched by name, dry run, source keys kept as aliases, parents from the file); TaskCraft's own CSV export imports back
- [x] Scheduled automation ("every weekday at 08:00, issues due tomorrow → notify the assignee", local time zones), a notify action; service-level goals (first response, resolution) per priority with a queue and alerts
- [x] Dashboards people compose from report widgets and saved filters
- [x] Portfolio view across projects (epics and releases on one timeline), capacity per team
- [x] Issue templates (fields, description, sub-tasks) and recurring issues (templates on a schedule)
- [x] E-mail in (create issues / comments by mail) for support trackers: an IMAP mailbox or the mail server handing messages over, a project address or +KEY, replies by conversation or [KEY-12], requesters with acknowledgements and e-mailed replies, auto-reply / bulk / loop protection
- [x] Git integration: link commits, branches and merge requests by issue key
- [x] MCP server so assistants can search, create and update issues with the user's permissions
- [x] Translations beyond English and German: French and Italian
- [x] Accessibility audit (axe, WCAG 2.2 AA) in the e2e suite
- [x] Browser (Playwright) journey tests in CI
- [x] Backup / restore command (one .tar.gz with every table and the attachments; restores across versions and storage backends); Kubernetes manifests (replicas, S3, PDB, autoscaler, nightly backup job)

## Next (round 3)

- [~] Releases: container image on GHCR (amd64 + arm64) and Linux binaries on tagged releases, a changelog — the workflow is ready and checked; the first release is a `v0.1.0` tag away
- [x] Operations: Prometheus metrics (requests by route with durations, database pool, outboxes, e-mail in, live connections, counts), protected by a token
- [x] API description: OpenAPI 3.1 document of the REST API (209 operations, generated, a test keeps it complete) and a browsable reference in the app
- [x] Full migration from another tracker's XML site backup: people, projects, issues with their numbers, comments, time logs, attachments, links, watchers, components and versions, and the status history the reports replay (uploaded, checked and imported in the background from Administration → Import, or `taskcraft import-backup`; a second import skips what's there)
- [x] Service desk portal: requesters follow and answer their requests on a small portal (sign-in by e-mailed link), public request forms per project
- [ ] Chat notifications: post to Slack, Microsoft Teams, Mattermost or Matrix from automation rules and per project
- [ ] Filter subscriptions: a saved filter's results e-mailed on a schedule
- [ ] Personal data export (everything about one person) and retention rules for trash, audit log and mail logs
- [ ] Scale check: a workspace with 100 000 issues — query plans, indexes, timings tracked in CI
- [ ] Virus scanning of uploads with ClamAV (fail closed or open), blocked file types and an attachment size limit in the workspace settings — uploads now also come from e-mail and the portal
- [ ] Webhook formats for chat channels (Slack / Mattermost, Microsoft Teams, Google Chat) as readable messages, alongside the JSON payload
- [ ] Idle session timeout (`TASKCRAFT_SESSION_IDLE_MINUTES`)
- [ ] Search: stemming in the workspace's language and text inside attachments (PDF, Office, text)
- [ ] Project export that imports into another TaskCraft (one project with its history and files), next to the whole-workspace backup
- [ ] Workspace settings: logo, default time zone and date format, an announcement banner, feature switches; profile photos
- [ ] Public read-only projects and share links (expiry, password) for roadmaps and status pages
- [ ] AI (optional, with your own model): similar issues and duplicates while typing, summaries of long threads, semantic search
- [ ] Guides in `docs/`: user guide, admin guide, migrating from another tracker, translating

## Next (round 4): what established trackers have and TaskCraft doesn't

- [ ] Workflow rules per transition: who may make it (project role), fields it requires (e.g. a resolution), a screen to fill them, and a resolution field (Done / Won't do / Duplicate)
- [ ] Finer permissions: custom project roles with a permission set (delete issues, edit others' comments, log time for others, manage sprints, …); private issues (security levels) and comments restricted to a role
- [ ] Several fix versions, affected versions and components per issue
- [ ] Time tracking: remaining estimate next to the original, time categories (activity, billable), time reports grouped by person / project / issue / category with export
- [ ] Fields per issue type: which fields a type shows and which it requires
- [ ] Subprojects: a project hierarchy with views and reports across a project and its subprojects
- [ ] E-mails in each person's language (they're English only)
- [ ] Voting on issues; issue lists and issues as Excel and PDF, a print view
- [ ] Portal: suggest WikiCraft articles while a requester types, before the request is sent
- [ ] Importers from other trackers' APIs (GitHub / GitLab issues and similar), next to CSV and XML site backups

