# Changelog

All notable changes. Versions follow [Semantic Versioning](https://semver.org/); a release is a
`v*` tag, which publishes the container image and the Linux binaries (see
`.github/workflows/release.yml`).

## [0.1.0] - unreleased

The first release.

- **Work**: projects from scrum, kanban, issue-tracker and work-plan templates; issues with rich
  text, sub-tasks, links, attachments, comments, watchers, time logs and history; custom fields,
  components, releases; templates and recurring issues; moving issues between projects.
- **Workflows**: free or restricted transitions; rules per status (who may move there, required
  fields, a comment) with a form that asks for what's missing; resolutions.
- **Views**: board with WIP limits and swimlanes, backlog with sprints and a velocity hint, list
  with TQL, timeline, calendar, timesheet, dashboards, portfolio, your work, command palette.
- **Reports** replayed from history: burndown, velocity, cumulative flow, cycle and lead time,
  created vs resolved, workload.
- **Automation** on events and schedules; service-level goals with a queue and alerts; signed
  webhooks.
- **E-mail**: invitations, password reset, notification e-mails and digests; e-mail in for
  support projects with requesters.
- **Permissions**: project roles for people and teams; per project, who may delete or move
  issues, plan sprints, manage releases, moderate comments, log time for others and change who
  sees an issue; private issues (security levels); comments restricted to members, project
  admins or a team.
- **Sign-in**: passwords, OpenID Connect, GitHub, LDAP / Active Directory, SAML; two-factor
  authentication and passkeys.
- **Integrations**: WikiCraft, Git (commits, branches, pull requests by key), an MCP server for
  assistants, a REST API with tokens, described in OpenAPI 3.1 (`/api/openapi.yaml`).
- **Import** from CSV exports of other tools; **backup and restore** in one file.
- **Operations**: one binary and PostgreSQL, S3-compatible storage, several replicas, Docker
  Compose and Kubernetes manifests.
- English, German, French and Italian; light, dark and high-contrast themes; WCAG 2.2 AA checked.
