# TaskCraft admin guide

For workspace owners and administrators, and for project admins. It walks through the settings in
the app; server configuration (environment variables, sign-in providers, e-mail, storage) is in
the [README](../README.md#configuration).

- [Roles at a glance](#roles-at-a-glance)
- [Setting up the workspace](#setting-up-the-workspace)
- [People and teams](#people-and-teams)
- [Signing in and security](#signing-in-and-security)
- [Projects](#projects)
- [Integrations](#integrations)
- [Privacy and retention](#privacy-and-retention)
- [Running TaskCraft](#running-taskcraft)

## Roles at a glance

| Role | Can |
|---|---|
| **Owner** | everything; the only one who can change or remove administrators |
| **Admin** | *Administration*: workspace settings, people, teams, time categories, webhooks, imports, the audit log; every project |
| **Member** | work in the projects they're part of; create projects |
| **Guest** | only the projects they're added to; don't see e-mail addresses |

Inside a project, people and teams have a **project role**: *viewer* (reads), *member* (works on
issues) or *admin* (project settings). Every project has a **default role** for workspace members
who aren't listed (none, viewer, member or admin), so a project can be open to everyone or kept to
its members.

## Setting up the workspace

The first visit to a new server asks for the workspace's name and the owner's account (with sample
projects if you like). Then *Administration → Overview*:

- **Workspace**: name, accent colour, **default language** (people can still choose their own; it is
  also the language search understands word forms in), the first day of the week, the **default
  time zone** (times on screen and the morning digest for people who haven't set one) and the
  **date format** (as the language writes dates, day-month-year, month-day-year or ISO).
- **Logo**: a PNG, JPEG, GIF or WebP up to 512 KB, shown in the top bar.
- **Announcement**: a message at the top of every page — planned maintenance, a new process —
  as information or a warning, until a date or until you take it down. People can close it; a new
  one shows again.
- **Features**: turn off what the workspace doesn't use — time tracking, votes, the service desk
  portal, automation, sharing with links, AI assistance. They disappear for everyone and the API refuses them;
  turning one on again brings everything back as it was.
- **Uploads**: blocked file endings (`exe, bat, cmd, …`) and a size limit. With a virus scanner set
  up on the server every file is checked before it's stored; the card says whether one is.
- **Retention**: how long the trash, the audit log, e-mail logs and read notifications are kept.
- **E-mail**: whether outgoing mail is configured, the sender, and what waits in the outbox; send a
  test message.
- **Two-factor authentication**: required for nobody, for owners and admins, or for everyone.

## People and teams

*Administration → Users*: invite people by e-mail (they get an invitation link when e-mail is set
up; otherwise copy the link from their menu), choose their role, and later **deactivate** them
(they can't sign in; their work stays) or **remove them for good** — their name becomes "Former
user …", their e-mail address, sessions, tokens and photo are deleted, and their open issues are
unassigned, while their comments and history stay. From a person's menu you can also reset their
second factor (when they lost their phone), take down their profile photo and download their
personal data.

*Administration → Teams* (or *People & teams*): teams group people for project roles, security
levels, restricted comments and assignment. With single sign-on, teams can follow the groups of
the identity provider.

## Signing in and security

Sign-in methods are configured on the server: passwords, OpenID Connect, GitHub, LDAP / Active
Directory and SAML, and whether password sign-in is allowed at all — see
[Sign-in](../README.md#sign-in-sso-ldap--active-directory-saml). People can add an authenticator
app and passkeys themselves; *Two-factor authentication* in the overview makes it required.
Sessions end after `TASKCRAFT_SESSION_DAYS`, and after `TASKCRAFT_SESSION_IDLE_MINUTES` without a
request when that's set.

The **audit log** (*Administration → Audit log*) records sign-ins, changes to people and roles,
workspace settings, imports, exports, share links, infected uploads and more.

## Projects

*Projects → New project* starts from a template: **Scrum** (backlog and sprints), **Kanban** (one
board), **Issue tracker** (a workflow for bugs and requests) or **Work plan** (tasks and milestones
with dates, for teams outside software). A project can be a **subproject** of another; filters and
reports can then include the whole tree.

*Project settings* (project admins):

- **Details**: name, key (old keys keep working in links and searches), icon, colour, lead,
  description, estimation in points or hours, the parent project, the WikiCraft space.
- **Members**: people and teams with their project role, and the default role for everyone else.
- **Permissions**: who may delete or move issues, plan sprints, manage releases, edit others'
  comments, log time for others and change who sees an issue; and **security levels** for private
  issues (by role, teams or people, with a default for new issues).
- **Workflow**: statuses with their category (to do, in progress, done), free or restricted
  transitions, and rules per status — who may move issues there, which fields must be filled, a
  required comment, the resolutions to choose from.
- **Issue types**: icons, levels (epic, standard, sub-task), a description template, and per type
  which fields are hidden or required.
- **Fields & components**: custom fields (text, number, select, multi-select, date, person,
  checkbox, link) and components with their leads.
- **Boards**: columns and the statuses in them, WIP limits, a base filter, quick filters, card
  fields and swimlanes.
- **Templates**: issue templates, and recurring issues created on a schedule.
- **Automation**: rules that act when something happens (created, status changed, commented, …)
  or on a schedule, if a TQL condition matches: set the status, priority or sprint, assign, add a
  label, comment, notify people or post to a chat channel.
- **Service levels**: goals for first response and resolution by priority (in calendar hours), a
  queue sorted by what's due, and alerts when a goal is missed.
- **E-mail**: an address that turns messages into issues and replies into comments; who may write.
- **Service desk**: the request form (public, with its fields and spam protection) and the portal
  where requesters follow their requests.
- **Sharing**: read-only links for people without an account — a status page (issues from a TQL
  query) or the roadmap — with an optional password and end date. Private issues and restricted
  comments never show; names only when you choose so. Revoking a link stops it at once.
- **Integrations**: Git hosting (commits, branches and pull requests on the issues they name, and
  rules that move issues when a branch is pushed or merged).
- **Import**: issues from a CSV file into this project.
- **Trash**: deleted issues, restorable until the retention period ends.
- **Export, archive or delete**: workspace admins export the project as a `.zip` that another
  TaskCraft imports (*Administration → Import*); archived projects are read-only and hidden from
  the project list; deleting removes the project with everything in it.

## Integrations

- **Webhooks** (*Administration → Webhooks*): signed JSON events for your own services, or readable
  messages for chat channels (Slack and compatible ones such as Mattermost, Microsoft Teams, Google
  Chat). Private issues and restricted comments never go to chat channels. Failed deliveries are
  retried; each webhook shows how its last delivery went.
- **WikiCraft**: pages linked to issues with live titles, a project's wiki space, pages created
  from issues, sprints and releases — see [WikiCraft integration](../README.md#wikicraft-integration).
- **AI assistants** through the built-in MCP server, with each person's own permissions — see
  [AI assistants (MCP)](../README.md#ai-assistants-mcp).
- **AI assistance**: once the server has a model — Claude through the Anthropic API, or a model
  running in your own network (Ollama, llama.cpp, vLLM, LM Studio, LocalAI) or at another
  OpenAI-compatible service, see [AI assistance](../README.md#ai-assistance) — people can
  summarize long threads and search in plain words. Only what the person asking may read is
  sent, and only when they ask; each person gets `TASKCRAFT_AI_PER_HOUR` requests an hour.
  *Features* shows which model is set up and lets you turn it off for the workspace.
- **The REST API** with personal tokens, described in OpenAPI at `/api/openapi.yaml` and in the
  app's API reference.

## Privacy and retention

- Everyone downloads what TaskCraft keeps about them (*Profile & settings → Your data*);
  administrators can download it for anyone.
- **Retention** rules delete old trash, audit entries, e-mail logs and read notifications.
- Removing a person anonymizes them, as described above.
- Imports, backups and project exports contain personal data: keep the files safe and delete them
  when you're done.

## Running TaskCraft

- **Upgrades**: start the new version; database migrations run on start. Take a backup first.
- **Backups**: `taskcraft backup FILE` writes the whole workspace with its files in one consistent
  file while the server runs; `taskcraft restore FILE` loads it into an empty database — see
  [Backup and restore](../README.md#backup-and-restore). A single project moves with *Export
  project*.
- **Monitoring**: `/api/health` (the process), `/api/ready` (with the database), Prometheus metrics
  at `/api/metrics` with `TASKCRAFT_METRICS_TOKEN`, JSON logs with `TASKCRAFT_LOG_FORMAT=json` —
  see [Monitoring](../README.md#monitoring).
- **Several replicas** share the database and the file storage (S3-compatible); background work
  (e-mails, schedules, imports) is coordinated through the database.
- **Size**: the scale check in CI keeps the main pages of a workspace with 100 000 issues under a
  quarter of a second on ordinary hardware. Give PostgreSQL enough shared memory in containers
  (the Compose file sets 256 MB).
- **Migrating in**: from other trackers or another TaskCraft, see the [migration guide](migrating.md).
