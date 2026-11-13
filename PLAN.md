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
- [x] Chat notifications: webhooks for all or one project can send readable messages to Slack-compatible channels (also Mattermost), Microsoft Teams or Google Chat in the workspace's language, and automation rules can post a message of their own; nothing about private issues or restricted comments
- [x] Filter subscriptions: a saved filter's results e-mailed on a schedule (as the subscriber sees them, in their language; skipped when empty unless asked)
- [x] Personal data export (everything about one person, from their settings or by an admin) and retention rules for the trash, audit log, mail logs and read notifications
- [x] Scale check: a workspace with 100 000 issues — query plans, indexes, timings tracked in CI
- [x] Virus scanning of uploads with ClamAV (fail closed or open), blocked file types and an attachment size limit in the workspace settings — uploads now also come from e-mail and the portal
- [x] Webhook formats for chat channels (Slack / Mattermost, Microsoft Teams, Google Chat) as readable messages, alongside the JSON payload
- [x] Idle session timeout (`TASKCRAFT_SESSION_IDLE_MINUTES`)
- [x] Search: stemming in the workspace's language and text inside attachments (PDF, Office, text)
- [x] Project export that imports into another TaskCraft (one project with its history and files), next to the whole-workspace backup
- [ ] Workspace settings: logo, default time zone and date format, an announcement banner, feature switches; profile photos
- [ ] Public read-only projects and share links (expiry, password) for roadmaps and status pages
- [ ] AI (optional, with your own model): similar issues and duplicates while typing, summaries of long threads, semantic search
- [ ] Guides in `docs/`: user guide, admin guide, migrating from another tracker, translating

## Next (round 4): what established trackers have and TaskCraft doesn't

- [x] Workflow rules: per status who may move issues there (everyone, project admins, chosen teams), the fields they need and a comment, with a form asking for what's missing (board, issue, bulk); resolutions per project (Done / Won't do / Duplicate / …) in history, TQL, CSV and both imports
- [x] Finer permissions: per project who may delete or move issues, plan sprints, manage releases, moderate comments, log time for others and change who sees an issue; comments restricted to members, project admins or a team; private issues (security levels by role, teams or people; a default level; enforced in lookups, lists, boards, search, reports that name issues, activity, live updates, notifications, e-mail replies and wiki pages)
- [x] Several fix versions, affected versions and components per issue (TQL, CSV both ways, imports, workflow rules, releasing moves only that release)
- [x] Time tracking: remaining estimate next to the original (follows the time logged or set by hand; logging reduces, keeps or sets it), time categories with a billable default, time reports grouped by person / project / issue / category / day / week / month (twice), filtered, with a CSV of every log
- [x] Fields per issue type: which fields a type shows, hides and requires (enforced on create, type change and edits; the create dialog asks for them)
- [x] Subprojects: a project hierarchy (three levels) with the issue list, exports, reports (overview, cycle time, created vs resolved, workload), the time report and TQL `withSubprojects()` across a project and its subprojects
- [x] E-mails in each person's language: their own, else the workspace's default, else their browser's (German, French, Italian; requesters get the workspace's)
- [x] Voting on issues (TQL `votes`, `voter`, order by votes); issue lists as Excel; a print view of issues and lists (print or save as PDF from the browser)
- [x] Portal: suggest WikiCraft articles while a requester types, before the request is sent (a space per form, read with the installation's token, that space only)
- [x] Importers from other trackers' APIs (GitHub / GitLab issues and similar), next to CSV and XML site backups: three sources, read with a token into the same check, choices, background import and report (Administration → Import, and `taskcraft import-…` on the command line)

