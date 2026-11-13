# TaskCraft user guide

For everyone who works in TaskCraft: finding your way, working on issues, planning, searching and
staying informed. Administrators find the setup in the [admin guide](admin-guide.md).

- [Getting around](#getting-around)
- [Issues](#issues)
- [Boards, backlog and sprints](#boards-backlog-and-sprints)
- [Lists, filters and TQL](#lists-filters-and-tql)
- [Timeline, calendar, releases](#timeline-calendar-releases)
- [Reports, dashboards and the portfolio](#reports-dashboards-and-the-portfolio)
- [Time tracking](#time-tracking)
- [Notifications and e-mail](#notifications-and-e-mail)
- [Your profile and account](#your-profile-and-account)
- [Keyboard shortcuts](#keyboard-shortcuts)

## Getting around

**Your work** (the start page) gathers what's yours: issues assigned to you that aren't done, what
you reported, what you finished this week, what changed on issues you watch, the issues you
opened last, and the time you logged this week.

**Projects** lists the projects you can see; star the ones you use most and they appear in the
sidebar. Inside a project the sidebar switches to its views: *Backlog* and *Active sprint* (scrum
projects) or *Board*, *List*, *Timeline*, *Calendar*, *Reports*, *Releases*, and *Docs* when the
workspace is connected to WikiCraft. Project admins also see *Project settings*.

The **command palette** (Ctrl K, or the search field at the top) finds issues by key or words,
projects, saved filters and actions — "create issue", "go to board", "toggle dark mode". Typing
`WEB-12` jumps straight to that issue.

**Create** (or the `c` key) opens a new issue in the project you're in. The dialog asks only for
what the project requires; the rest can be filled in later.

## Issues

An issue has a **type** (task, bug, story, epic, sub-task — each project has its own list), a
**status** from the project's workflow, a **priority**, an **assignee** and a **reporter**, and
optionally releases (fixed in, found in), components, labels, sprint, story points or estimates,
start and due dates, and the project's custom fields.

- **Change a field** by clicking it in the panel on the right. A status change may ask for
  something first (a resolution, a comment, a required field) when the project's workflow says so.
- **Description and comments** are rich text: headings, lists, checklists, tables, code, images
  (paste or drop them), links. Type `@` to mention someone (they're notified) and `#` to point to
  another issue.
- **Who can read a comment**: everyone who sees the issue by default; *members*, *project admins*
  or a team when you choose so under the comment box.
- **Sub-tasks and epics**: *Add issue* creates a child; epics show their issues with a progress
  bar. *Link issue* relates issues (blocks, relates to, duplicates, clones); a blocked issue shows
  it on its cards.
- **Attachments**: drop files onto the issue or use *Attach*. The workspace may refuse some file
  types or sizes, and files may be checked for viruses before they're stored.
- **Watch** an issue to be notified of its changes (you watch what you create, are assigned or
  comment on). **Vote** for issues you'd like to see done, when the workspace uses votes.
- **History** shows every change with who made it and when.
- **Private issues**: a project can have security levels; an issue with one is seen only by the
  people the level names, project admins, and its reporter, creator and assignee.
- **More** (⋯) copies the link, prints or saves the issue as PDF, moves it to another project
  (statuses, types and fields are matched by name; what doesn't fit is shown first) or deletes it
  into the project's trash, where project admins can restore it.

## Boards, backlog and sprints

**Scrum projects** plan in sprints. The *Backlog* lists the open sprints at the top and the
backlog below, in rank order: drag issues to reorder them or move them into a sprint. *Start
sprint* asks for its dates and goal; *Complete sprint* moves unfinished issues to the backlog or
the next sprint and keeps the sprint's figures for the velocity report. The *Active sprint* board
shows the running sprint's issues.

**Kanban projects** work on one board. Drag cards between columns to change their status; columns
can have WIP limits (the column turns red when over), swimlanes group cards by assignee, epic,
priority or type, and done issues leave the board after a while.

Both board kinds can be filtered (assignee, type, label, text) and configured by project admins
(columns, quick filters, card fields).

## Lists, filters and TQL

The **List** of a project, and **Filters** across all projects, show issues as a table: choose
columns, sort, group, edit in place, select several issues to change them at once, and export
them as CSV or Excel.

The search field understands plain words (titles, descriptions, file names and the text inside
attached files; word forms count, so *invoices* finds *invoice*) and **TQL**, TaskCraft's query
language:

```text
assignee = me() and category != done order by priority desc
project = WEB and sprint in openSprints()
type = Bug and priority >= high and created > -14d
due < endOfWeek() and category != done
labels is empty or assignee is empty
"Customer tier" = Gold
withSubprojects(WEB) and updated > -7d
```

Fields include `project key title text description comment type status category priority
assignee reporter creator labels sprint version affects component parent points estimate spent
created updated resolved due start watcher votes blocked` and custom fields by name; operators
`= != ~ !~ > >= < <= in, not in, is empty`; dates like `2026-11-01`, `-7d`, `+2w`, `today()`,
`startOfWeek()`, `endOfMonth()`. The **?** next to the field shows more examples.

**Save** a query as a filter (just for you or shared with everyone), star it to keep it in the
sidebar, and **subscribe** to it to get its results by e-mail daily, on weekdays, weekly or
monthly (with *skip empty*).

## Timeline, calendar, releases

The **Timeline** shows epics and their issues as bars between start and due dates; drag a bar to
move it or its ends to change the dates, and see dependencies. The **Calendar** shows issues on
their due dates, a month at a time; drag one to another day.

**Releases** lists the project's versions with their issues and progress; *Release* marks one as
released (unfinished issues can move to the next one), and release notes can be created in
WikiCraft when it's connected.

## Reports, dashboards and the portfolio

**Reports** replay each issue's history, so they show the past as it was: sprint burndown,
velocity, cumulative flow, cycle and lead time, created vs resolved, and workload per person.
Reports can include a project's subprojects.

**Dashboards** are pages of gadgets — counts and charts from a TQL query, lists, reports — for
you alone or shared with everyone. The **Portfolio** shows the epics and releases of all your
projects on one timeline.

## Time tracking

When the workspace uses time tracking, log time on an issue (*Time* tab) as `1h 30m` with a date,
a note, a category and whether it's billable; the issue shows the time spent against its
estimate and what remains. The **Timesheet** shows your week (or someone else's, for those who
may) and lets you log time per day; the **Time report** sums time by person, project, issue,
category or period and exports it as CSV.

## Notifications and e-mail

The bell shows what happened on issues you're involved in: assignments, mentions, comments,
status changes, due dates. Notifications arrive live; clicking one opens the issue.

In *Profile & settings → Notifications* choose how e-mails come: **instantly** (but not while
you're using the app), as a **daily digest** in the morning of your time zone, or **not at all**,
and mute the kinds you don't want.

## Your profile and account

*Profile & settings* (your avatar, top right):

- **Profile**: your name, job title, **photo** (cropped to a square in the browser), language and
  time zone. Without a time zone of your own, times show in the workspace's (or your browser's).
- **Appearance**: light, dark or high-contrast, and the system's choice.
- **Security**: change your password; set up **two-factor authentication** with an authenticator
  app (with recovery codes) and/or **passkeys**, which can also sign you in without a password;
  see where you're signed in and sign out other browsers.
- **API tokens** for scripts and assistants (read-only or read/write); see the API section of the
  README and the built-in API reference.
- **WikiCraft**: connect your own WikiCraft account to see page titles you have access to.
- **Your data**: download everything TaskCraft keeps about you as JSON.

## Keyboard shortcuts

| Keys | Does |
|---|---|
| Ctrl K or / | the command palette: issues, projects, filters and actions |
| c | create an issue (in the project you're in) |
| Ctrl Enter | save a comment |
| Esc | close a dialog, a menu or the issue panel |

*Ctrl* is *⌘* on a Mac. Shortcuts don't fire while you type in a field.
