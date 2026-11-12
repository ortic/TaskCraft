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

- [ ] E-mail: invitations, password reset, notification digests (SMTP)
- [x] Single sign-on: OpenID Connect (Entra ID, Google, GitLab, any issuer), GitHub, LDAP / Active Directory, SAML (feature `saml`); group → team sync, password sign-in can be turned off
- [ ] Two-factor authentication and passkeys
- [x] Move issues between projects: new key (the old one keeps working), type / status mapped by name or category, custom fields by name and kind, sub-tasks along; editing time logs
- [x] CSV import with field mapping (suggested from header names, values matched by name, dry run, source keys kept as aliases, parents from the file); TaskCraft's own CSV export imports back
- [x] Scheduled automation ("every weekday at 08:00, issues due tomorrow → notify the assignee", local time zones), a notify action; service-level goals (first response, resolution) per priority with a queue and alerts
- [x] Dashboards people compose from report widgets and saved filters
- [x] Portfolio view across projects (epics and releases on one timeline), capacity per team
- [x] Issue templates (fields, description, sub-tasks) and recurring issues (templates on a schedule)
- [ ] E-mail in (create issues / comments by mail) for support trackers
- [ ] Git integration: link commits, branches and merge requests by issue key
- [ ] MCP server so assistants can search, create and update issues with the user's permissions
- [ ] Translations beyond English and German; accessibility audit (axe, WCAG 2.2 AA) in an e2e suite
- [ ] Browser (Playwright) journey tests in CI
- [ ] Backup / restore command; Kubernetes manifests
