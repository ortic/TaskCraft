# Migrating to TaskCraft

How to plan and run a move to TaskCraft: from **Jira** (its XML site backup), from **GitHub**,
**GitLab** or **Redmine** (through their APIs), from a **CSV** file, or a single project from
**another TaskCraft** — and the documentation from **Confluence** into WikiCraft, next to it. The [README](../README.md#migrating-from-another-tracker) describes each
importer in detail — what it reads and how every field maps; this guide is the plan around it.

- [Before you start](#before-you-start)
- [Choosing the way in](#choosing-the-way-in)
- [A trial run](#a-trial-run)
- [The real import](#the-real-import)
- [After the import](#after-the-import)
- [Moving in steps](#moving-in-steps)
- [Documentation: Confluence to WikiCraft](#documentation-confluence-to-wikicraft)
- [Troubleshooting](#troubleshooting)

## Before you start

1. **Set up TaskCraft first**: the workspace, sign-in (single sign-on matches people by e-mail
   address, so set it up before inviting anyone), e-mail, and the default language and time zone.
2. **Decide what moves**: which projects, under which keys (keep the old ones where you can — links
   in documents and commit messages keep working), and which closed or test projects stay behind.
3. **Decide about people**: everyone named in the source is matched by e-mail address, then by
   name. Those without an account here can get **invited accounts** (nobody is e-mailed until you
   send the invitations from *Users*) or **deactivated accounts** that only keep their names on
   their work.
4. **Mind the size**: uploads in the browser go up to `TASKCRAFT_MAX_UPLOAD_MB` (256 MB by
   default). Larger XML site backups are imported on the server with `taskcraft import-backup`,
   which has no limit.
5. **Take a backup** of TaskCraft before a large import: `taskcraft backup before-import.tar.gz`.

## Choosing the way in

| Coming from | Way in | Comes along |
|---|---|---|
| **Jira** (cloud or server) | its XML site backup (`.zip` with `entities.xml`, attachments, `activeobjects.xml`), uploaded under *Administration → Import*, or `taskcraft import-backup` | people, projects, issues with numbers, comments, time logs, attachments, links, watchers, labels, components, versions, sprints, estimates and points, security levels and restricted comments, and the history the reports replay |
| **GitHub** | the API with a personal access token | issues (not pull requests), comments, labels, assignees, milestones as releases, attachments in texts, history |
| **GitLab** | the API with a token (`read_api`) | issues, notes, labels, assignees, milestones, time spent, confidential issues and internal notes (with a token that sees them), attachments, history |
| **Redmine** | the API with an API key | issues with trackers and statuses, journals as comments and history, time entries with their activities, versions, categories, attachments, private issues and notes |
| **A spreadsheet or another tool's CSV export** | *Project settings → Import* (CSV) | issues with the columns you map |
| **Another TaskCraft** | *Project settings → Export* there, *Administration → Import* here | one project with everything in it |

Every way except CSV runs through the same steps in the browser: **read and check** (nothing
changes), **choose** per project and for people, **test**, **import**.

## A trial run

1. *Administration → Import*, pick the source, upload the file or connect with the token.
2. The check lists each project with its issues, comments, time logs, attachments and links,
   how many people already have an account here, and notes about anything that won't come over
   exactly (files missing from a backup, link types it doesn't know, issue numbers already taken).
3. Choose for each project: **a new project** (same key or another), **an existing project**, or
   **leave it out**; and what happens to people without an account.
4. **Test the import**: it runs the whole import and rolls it back, so you get the real report —
   counts, warnings, what would be created — without changing anything.

Read the report's warnings and fix what you can at the source (for example a missing attachment
folder), then check again.

## The real import

*Import* runs in the background — the page can be closed and reopened — and ends with a report and
links to the new projects. An import is **one transaction**: it lands completely or not at all.
It **tells nobody**: no notifications, e-mails, webhooks or automation run for imported issues.

On the command line, for large XML site backups:

```bash
taskcraft import-backup backup.zip --dry-run                 # the report, nothing changes
taskcraft import-backup backup.zip --project OLD=NEW --skip TEST --people deactivated
```

## After the import

- [ ] Open a few issues of each project: descriptions, images, comments, links, history.
- [ ] Check the **workflow**: statuses that were created on the way, and which ones are done.
- [ ] Look at the **boards**: columns for new statuses, WIP limits, quick filters.
- [ ] Set **project roles** and the default role; check **security levels** — imported ones start
      out visible to the project's members.
- [ ] Recreate **automation** and **service levels**; they aren't imported.
- [ ] Point **Git hosting** webhooks at TaskCraft so commits and pull requests appear on issues.
- [ ] Send the **invitations** (*Administration → Users*) once the projects look right.
- [ ] Tell people about the old keys: `WEB-12` links and searches keep working under a new key.
- [ ] Keep the source read-only for a while, then retire it.

## Moving in steps

Importing the same source again **skips what's already there**, so you can import early, let
people try TaskCraft, and import again just before the switch to bring in the issues created in
the meantime. Changes made in the source to issues already imported aren't carried over — for a
clean cut-over, freeze the source, import a last time, then switch.

A project can also move **between TaskCraft servers** (or workspaces): export it under
*Project settings → Export*, then import the `.zip` under *Administration → Import →
TaskCraft (project export)*. People are matched by e-mail address; mentions, `#` references and
images in texts point to what was imported.

## Documentation: Confluence to WikiCraft

Teams that leave Jira often leave Confluence too. Spaces and pages move into
[WikiCraft](https://github.com/ortic/WikiCraft), the wiki TaskCraft works with: its
*Administration → Import* takes Confluence's XML exports (Data Center and Server) and Cloud
exports — pages with their history, attachments, comments (inline ones too), labels, restrictions
and space permissions, 50+ macros mapped, links rewritten, and the old page addresses redirecting
to the imported pages. It has a dry run and update imports, like TaskCraft's. See WikiCraft's
own migration guide for the details.

Then connect the two (`TASKCRAFT_WIKICRAFT_URL`, see the
[WikiCraft integration](../README.md#wikicraft-integration)): issues show their linked pages with
live titles, pages can be created from issues, sprints and releases, and each project can get its
wiki space. Keep the issue keys when importing from Jira, so keys mentioned in the imported pages
still name the same issues.

## Troubleshooting

| What you see | What to do |
|---|---|
| "that's not a .zip file" | Upload the backup or export as downloaded; don't unpack and re-pack it |
| Missing attachments | The backup was made without attachments, or they sit elsewhere: `taskcraft import-backup … --attachments DIR` |
| Times are off by hours | The backup's times are the old server's local time: import it on the command line with `--time-zone Europe/Zurich` |
| An in-house server can't be reached | Servers on private addresses are refused unless `TASKCRAFT_IMPORT_ALLOW_PRIVATE=true` |
| Rate limits | The import waits them out; the progress says until when |
| "the uploaded file is gone" | Uploads and read data are kept for a day; upload or connect again |
| People appear twice | They had different e-mail addresses there and here; merge by deactivating one and reassigning |
| Custom field values missing | Fields are matched by name and kind; values that don't fit the field are reported, not imported |
