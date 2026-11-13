#!/usr/bin/env python3
"""Generates the OpenAPI 3.1 description of TaskCraft's REST API from the curated table below.

Writes crates/server/openapi.yaml and crates/server/openapi.json (the same document twice; the
server serves them at /api/openapi.yaml and /api/openapi.json):

    python3 scripts/gen-openapi.py            # writes both files
    python3 scripts/gen-openapi.py OUT_DIR    # writes them to OUT_DIR instead

Run it after adding or changing routes. `cargo test -p taskcraft --lib openapi` fails when a route
or method of the router (routes/mod.rs, routes/security.rs, sso/mod.rs) is missing here.

Check the result with openapi-spec-validator (`pip install openapi-spec-validator`, or in a
virtualenv, with pipx, or `pip install --target DIR` plus PYTHONPATH=DIR):

    python3 -m openapi_spec_validator crates/server/openapi.yaml

Needs PyYAML.
"""
import copy
import json
import pathlib
import re
import sys

import yaml

# ---- schema helpers ------------------------------------------------------------------------

S = lambda name: {"$ref": f"#/components/schemas/{name}"}
uuid = {"type": "string", "format": "uuid"}
dt = {"type": "string", "format": "date-time"}
date = {"type": "string", "format": "date", "description": "YYYY-MM-DD"}
txt = {"type": "string"}
num = {"type": "integer"}
number = {"type": "number"}
boolean = {"type": "boolean"}
email = {"type": "string", "format": "email"}
uri = {"type": "string", "format": "uri"}
binary = {"type": "string", "format": "binary"}
anyobj = {"type": "object", "additionalProperties": True}
anyval = {"description": "any JSON value"}


def d(schema, text):
    """The schema with a description (also next to a $ref, which OpenAPI 3.1 allows)."""
    s = copy.deepcopy(schema)
    s["description"] = text
    return s


def null(schema, desc=None):
    """The schema or null."""
    s = copy.deepcopy(schema)
    if "$ref" in s or "type" not in s or isinstance(s["type"], list):
        out = {"anyOf": [s, {"type": "null"}]}
        if desc or s.get("description"):
            out["description"] = desc or s.pop("description")
        return out
    s["type"] = [s["type"], "null"]
    if "enum" in s and None not in s["enum"]:
        s["enum"] = s["enum"] + [None]
    if desc:
        s["description"] = desc
    return s


def arr(items, desc=None):
    s = {"type": "array", "items": items}
    if desc:
        s["description"] = desc
    return s


def obj(props, req=None, desc=None, additional=None):
    s = {"type": "object", "properties": props}
    if req:
        s["required"] = list(req)
    if desc:
        s["description"] = desc
    if additional is not None:
        s["additionalProperties"] = additional
    return s


def enum(*values, desc=None):
    s = {"type": "string", "enum": list(values)}
    if desc:
        s["description"] = desc
    return s


def one(*schemas, desc=None):
    s = {"oneOf": list(schemas)}
    if desc:
        s["description"] = desc
    return s


def ntxt(desc=None):
    return null(txt, desc)


def nuuid(desc=None):
    return null(uuid, desc)


def ndt(desc=None):
    return null(dt, desc)


def ndate(desc=None):
    return null(date, desc)


# ---- request / response helpers ------------------------------------------------------------


def body(schema, ctype="application/json", required=True, desc=None):
    b = {"required": required, "content": {ctype: {"schema": schema}}}
    if desc:
        b["description"] = desc
    return b


def multipart(props, req=None, desc=None):
    return body(obj(props, req), "multipart/form-data", desc=desc)


def ok(schema=None, desc="OK", code="200", ctype="application/json", headers=None):
    r = {"description": desc}
    if schema is not None:
        r["content"] = {ctype: {"schema": schema}}
    if headers:
        r["headers"] = headers
    return {str(code): r}


def created(schema=None, desc="Created"):
    return ok(schema, desc, "201")


def raw(ctype, desc="OK", code="200", schema=None):
    """A non-JSON response (CSV, a file, text)."""
    return {str(code): {"description": desc, "content": {ctype: {"schema": schema or ({"type": "string"} if ctype.startswith("text/") else binary)}}}}


def redirect(desc, code="303"):
    return {str(code): {"description": desc, "headers": {"Location": {"schema": txt, "description": "where the browser goes next"}}}}


NO = {"204": {"description": "Done (no content)"}}


def q(name, desc, schema=txt, required=False):
    return {"name": name, "in": "query", "required": required, "description": desc, "schema": schema}


def hdr(name, desc, schema=txt, required=False):
    return {"name": name, "in": "header", "required": required, "description": desc, "schema": schema}


def page(default, maximum=None, offset=True):
    """`limit` (and `offset`) query parameters."""
    lim = {"type": "integer", "minimum": 1, "default": default}
    if maximum:
        lim["maximum"] = maximum
    ps = [q("limit", f"at most this many results (default {default}{f', at most {maximum}' if maximum else ''})", lim)]
    if offset:
        ps.append(q("offset", "skip this many results (paging)", {"type": "integer", "minimum": 0, "default": 0}))
    return ps


# ---- tags (in the order the reference shows them) ------------------------------------------

TAGS = [
    ("Auth & account", "First-run setup, signing in and out with a password, invitation and password-reset links, the signed-in person and their API tokens."),
    ("Security", "Two-factor authentication (authenticator app and recovery codes), passkeys, the second step of a sign-in, sessions and the workspace's two-factor policy. Changes to sign-in methods are refused for API tokens."),
    ("Sign-in", "Single sign-on providers (OpenID Connect, GitHub, LDAP, SAML): the provider list, the redirect flows and how the signed-in person signs in."),
    ("People & teams", "The people directory, user administration and teams."),
    ("Projects", "Projects: list, create, read (with their whole configuration), change, archive, delete, star, members and roles."),
    ("Project configuration", "Statuses, transitions, issue types, versions (releases), components, custom fields, automation rules and labels."),
    ("Boards & sprints", "Boards, the backlog and sprints (start, complete, burndown)."),
    ("Issues", "Issues: create, read, change, move on boards and backlogs, move to another project, delete and restore, clone, bulk changes, links, web links, watchers, history and your own work."),
    ("Comments & time", "Comments (with reactions and comments for the requester), time logs, time categories, the weekly timesheet and the time report."),
    ("Attachments", "Files on issues: upload, download, delete."),
    ("Search & TQL", "Issue search with TQL (the query language), CSV export, quick search, query checking and completion, and the activity stream."),
    ("Dashboards & reports", "Dashboards, grouped counts, the portfolio and the project reports replayed from the issues' history."),
    ("Templates & service levels", "Issue templates (also recurring ones) and the service-level queue."),
    ("Import", "CSV import of issues into a project, with a dry run."),
    ("Integrations (WikiCraft, Git)", "WikiCraft (linked pages, project docs, generated pages) and Git (connected repositories, webhook receivers, development links)."),
    ("E-mail in", "Messages handed over by mail servers and a project's e-mail settings."),
    ("Service desk portal", "The portal for requesters (signed in by e-mailed links, a cookie of its own), public request forms and a project's service desk settings."),
    ("Notifications & views", "In-app notifications and saved filters (views)."),
    ("Administration", "Workspace settings, audit log, statistics and outgoing e-mail."),
    ("Webhooks", "Outgoing webhooks: register endpoints and test them. Deliveries carry the WebhookEvent schema."),
    ("Backup import", "Migration from another tracker's XML site backup: upload, check, import in the background."),
    ("Operations", "Health, readiness, Prometheus metrics, live updates and this document."),
]
TAG_NAMES = [t for t, _ in TAGS]

# ---- shared component schemas ----------------------------------------------------------------

PRIORITY = enum("none", "low", "medium", "high", "urgent", desc="issue priority")
CATEGORY = enum("todo", "in_progress", "done", desc="status category")

schemas = {
    "Error": obj(
        {
            "error": enum("bad_request", "unauthorized", "forbidden", "not_found", "conflict", "too_large", "too_many_requests", "unavailable", "internal", desc="machine-readable error code"),
            "message": d(txt, "what went wrong, for people"),
        },
        ["error", "message"],
        "Every error answer has this shape.",
    ),
    "PmNode": {
        "type": "object",
        "description": "Rich text as ProseMirror / Tiptap JSON: a node with `type`, optional `attrs`, `content` (child nodes), `text` and `marks`. "
        "A document is `{\"type\": \"doc\", \"content\": [...]}` (at most 50,000 nodes, 40 levels deep). Where noted, Markdown or plain text is accepted instead.",
        "properties": {"type": txt, "attrs": anyobj, "content": arr(S("PmNode")), "text": txt, "marks": arr(anyobj)},
        "required": ["type"],
        "additionalProperties": True,
    },
    "Named": obj({"id": uuid, "name": txt}, ["id", "name"], "A reference to a person, version, component, … by id with its name."),
    "User": obj(
        {
            "id": uuid,
            "workspaceId": uuid,
            "email": ntxt(),
            "name": txt,
            "role": enum("owner", "admin", "member", "guest", desc="workspace role"),
            "status": enum("active", "invited", "deactivated", desc="account status"),
            "photo": null(txt, "the profile photo's address (in /me)"),
        },
        ["id", "workspaceId", "name", "role", "status"],
        "A person with an account.",
    ),
    "Priority": PRIORITY,
    "StatusCategory": CATEGORY,
    "TypeRef": obj({"id": uuid, "name": txt, "icon": txt, "color": txt, "level": d(num, "0 = standard, 1 = epic (above), -1 = sub-task (below)")}, ["id", "name", "icon", "color", "level"]),
    "StatusRef": obj({"id": uuid, "name": txt, "category": S("StatusCategory"), "color": ntxt()}, ["id", "name", "category"]),
    "ParentRef": obj({"id": uuid, "key": txt, "title": txt, "color": ntxt("the parent's type colour (epics)")}, ["id", "key", "title"]),
    "SprintRef": obj({"id": uuid, "name": txt, "state": enum("future", "active", "closed")}, ["id", "name", "state"]),
    "IssueSummary": obj(
        {
            "id": uuid,
            "key": d(txt, "project key and number, e.g. WEB-12"),
            "number": num,
            "projectId": uuid,
            "projectKey": txt,
            "projectName": txt,
            "title": txt,
            "type": S("TypeRef"),
            "status": S("StatusRef"),
            "priority": S("Priority"),
            "assignee": null(S("Named")),
            "reporter": null(S("Named")),
            "parent": null(S("ParentRef"), "parent issue or epic"),
            "sprint": null(S("SprintRef")),
            "versions": arr(S("Named"), "the releases it's fixed in, in release order"),
            "affectedVersions": arr(S("Named"), "the releases it was found in"),
            "components": arr(S("Named"), "by name"),
            "storyPoints": null(number),
            "estimateMinutes": null(num, "the original estimate"),
            "remainingMinutes": null(num, "what's left: set by someone, else the original estimate minus the time logged (null without either)"),
            "remainingSet": d(boolean, "someone set the remaining estimate (else it follows the time logged)"),
            "spentMinutes": d(num, "time logged, in minutes"),
            "startDate": ndate(),
            "dueDate": ndate(),
            "rank": d(number, "position in backlogs and board columns (lower first)"),
            "labels": arr(txt),
            "custom": d(anyobj, "custom field values by field id (see CustomField for the value of each kind)"),
            "resolvedAt": ndt("when it reached a done status"),
            "resolution": ntxt("why a done issue is done (one of the project's resolutions: Done, Won't do, …); null while open"),
            "security": null(S("Named"), "a private issue's security level (see SecurityLevel); null: everyone in the project sees it"),
            "statusChangedAt": dt,
            "createdAt": dt,
            "updatedAt": dt,
            "childCount": d(num, "sub-tasks / child issues"),
            "childDone": d(num, "of those, done"),
            "commentCount": num,
            "votes": d(num, "how many people voted for it"),
            "pullRequest": d(enum("open", "merged", "closed"), "state of a linked pull request (an open one first); only present when the Git integration linked one"),
            "blocked": d(boolean, "an unfinished issue blocks it"),
            "url": d(uri, "the issue in the web app"),
        },
        ["id", "key", "number", "projectId", "projectKey", "projectName", "title", "type", "status", "priority", "spentMinutes", "rank", "labels", "custom", "statusChangedAt", "createdAt", "updatedAt", "childCount", "childDone", "commentCount", "blocked", "url"],
        "An issue as lists, boards, search results and webhooks show it.",
    ),
    "IssueLink": obj(
        {
            "id": uuid,
            "kind": enum("blocks", "relates", "duplicates", "clones", desc="stored direction-free kind"),
            "label": enum("blocks", "is blocked by", "duplicates", "is duplicated by", "clones", "is cloned by", "relates to", desc="the link as read from this issue"),
            "outward": d(boolean, "this issue is the link's source"),
            "issue": S("IssueSummary"),
        },
        ["id", "kind", "label", "outward", "issue"],
    ),
    "WebLink": obj({"id": uuid, "url": uri, "title": txt, "kind": enum("web", "wiki", desc="wiki: a WikiCraft page"), "createdAt": dt}, ["id", "url", "title", "kind"]),
    "IssueAttachment": obj(
        {"id": uuid, "name": txt, "mime": txt, "size": d(num, "bytes"), "uploadedBy": ntxt("uploader's name"), "createdAt": dt, "url": d(txt, "download path (/api/attachments/{id})")},
        ["id", "name", "mime", "size", "url"],
    ),
    "SlaGoal": obj(
        {
            "goal": enum("response", "resolution"),
            "targetMinutes": num,
            "elapsedMinutes": num,
            "dueAt": dt,
            "state": enum("running", "atRisk", "breached", "met"),
            "stopped": d(boolean, "the clock stopped (responded / resolved)"),
        },
        ["goal", "targetMinutes", "elapsedMinutes", "dueAt", "state", "stopped"],
        "A service-level goal of an issue (calendar hours from the project's settings).",
    ),
    "IssueRequester": obj(
        {
            "email": email,
            "name": ntxt(),
            "mail": d(boolean, "outgoing e-mail works: comments for the requester are e-mailed and they can sign in to the portal"),
            "source": d(txt, "where the request came from: email, form or portal"),
        },
        ["email"],
        "Who an issue was opened for (a requester without an account).",
    ),
    "Issue": {
        "description": "One issue with everything the issue page shows.",
        "allOf": [
            S("IssueSummary"),
            obj(
                {
                    "description": null(S("PmNode"), "rich text (ProseMirror JSON)"),
                    "watchers": arr(S("Named")),
                    "watching": d(boolean, "the caller watches it"),
                    "voters": arr(S("Named"), "who voted for it, first vote first"),
                    "voted": d(boolean, "the caller voted for it"),
                    "links": arr(S("IssueLink"), "links to issues the caller can see"),
                    "webLinks": arr(S("WebLink")),
                    "attachments": arr(S("IssueAttachment")),
                    "children": arr(S("IssueSummary"), "sub-tasks / issues of the epic, by rank"),
                    "sla": arr(S("SlaGoal")),
                    "requester": null(S("IssueRequester")),
                    "permissions": obj({"edit": boolean, "comment": boolean, "admin": boolean}, desc="what the caller may do"),
                },
                ["description", "watchers", "watching", "links", "webLinks", "attachments", "children", "sla", "requester", "permissions"],
            ),
        ],
    },
    "IssueFields": obj(
        {
            "title": d(txt, "1–500 characters (whitespace collapsed)"),
            "description": one(S("PmNode"), d(txt, "plain text"), {"type": "null"}, desc="rich text (ProseMirror JSON), plain text, or null to clear"),
            "descriptionMarkdown": d(txt, "the description as Markdown (instead of `description`)"),
            "typeId": d(uuid, "an issue type of the project"),
            "statusId": d(uuid, "a status of the project (restricted workflows only allow configured transitions; the status's rules may require fields, a comment or a resolution, see StatusRules)"),
            "resolution": ntxt("for done issues: one of the project's resolutions (matched by name, any case); entering a done status without one sets the status's default or the first; leaving done clears it"),
            "comment": d(txt, "a comment (Markdown) added with the change: what a status's rules may ask for when moving into it"),
            "priority": one(S("Priority"), {"type": "integer", "minimum": 0, "maximum": 4}, desc="name, or 0 (none) to 4 (urgent)"),
            "assigneeId": nuuid("someone with access to the project; null unassigns"),
            "reporterId": nuuid(),
            "parentId": nuuid("parent issue or epic"),
            "parentKey": ntxt("parent by key instead of id (\"\" or null clears)"),
            "sprintId": nuuid("a future or active sprint of the project; null = the backlog"),
            "versionIds": arr(uuid, "the releases it's fixed in (the project's own); replaces the list"),
            "affectedVersionIds": arr(uuid, "the releases it was found in; replaces the list"),
            "componentIds": arr(uuid, "its components; replaces the list"),
            "versionId": nuuid("one release (or null): replaces the list of releases it's fixed in"),
            "componentId": nuuid("one component (or null): replaces the list of components"),
            "securityLevelId": nuuid("make the issue private with one of the project's security levels, or null to make it public. Creating an issue, any level of the project (the default level applies when left out); changing it later, as the project's `setSecurity` permission allows and only to a level you belong to (project admins: any)"),
            "storyPoints": null(number),
            "estimateMinutes": null(number, "the original estimate, rounded to whole minutes"),
            "remainingMinutes": null(number, "the remaining estimate; null: follow the original estimate minus the time logged"),
            "startDate": ndate(),
            "dueDate": ndate(),
            "labels": arr(txt, "replaces the labels: lower-cased, spaces become dashes, 60 characters each, at most 30"),
            "custom": d(
                {"type": "object", "additionalProperties": anyval},
                "custom field values by field id, merged into the current ones (null clears one): text (2,000 characters), number, date (YYYY-MM-DD), "
                "checkbox (true/false), user (person id), url, select (an option id), multiselect (a list of option ids)",
            ),
        },
        desc="Fields of an issue that can be set. Only the fields sent change. Read-only fields an issue carries (id, key, number, projectId, projectKey, url, rank, createdAt, updatedAt) "
        "are ignored, any other unknown field is a 400.",
    ),
    "NewIssue": {
        "description": "A new issue: `projectKey` (or `projectId`), `title` and any IssueFields.",
        "allOf": [
            S("IssueFields"),
            obj(
                {
                    "projectKey": d(txt, "project key (or id)"),
                    "projectId": d(txt, "instead of projectKey"),
                    "templateId": d(uuid, "start from this issue template of the project (title and description given here override it)"),
                    "after": d(uuid, "place it right after this issue in the backlog / board column"),
                },
                ["title"],
            ),
        ],
    },
    "IssueEvent": obj(
        {
            "id": uuid,
            "actor": null(S("Named"), "who did it (null: the system, automation or an import)"),
            "kind": d(txt, "created, field (a field changed: see field), comment, worklog, link, attachment, dev (Git development link), moved, deleted, restored"),
            "field": ntxt("the changed field: title, description, type, status, priority, assignee, reporter, parent, sprint, version, component, points, estimate, start, due, labels, custom, link, webLink, project, …"),
            "old": d(anyval, "previous value (shape depends on the field; status: {id, name, category})"),
            "new": d(anyval, "new value"),
            "at": dt,
        },
        ["id", "kind", "at"],
        "An entry of an issue's history.",
    ),
    "SearchResult": obj({"total": d(num, "all matches (not just this page)"), "issues": arr(S("IssueSummary"))}, ["total", "issues"]),
    "Status": obj({"id": uuid, "name": txt, "category": S("StatusCategory"), "color": ntxt(), "position": number, "rules": S("StatusRules")}, ["id", "name", "category"]),
    "StatusRules": obj(
        {
            "who": enum("everyone", "admins", "teams", desc="who may move issues into the status (project admins always may); default everyone who can edit"),
            "teams": arr(uuid, "with who = teams: the teams whose members may"),
            "require": arr(txt, "fields the issue must have afterwards: assignee, resolution, version, component, dueDate, startDate, estimate, storyPoints, custom:<field id>"),
            "comment": enum("off", "optional", "required", desc="whether a comment comes with the move (`comment` in the change)"),
            "resolution": d(txt, "done statuses: the resolution set when none is chosen"),
        },
        desc="What it takes for a person to move an issue into the status (automation and imports aren't held up). A refused move answers 403 (who) or 400 naming what's missing.",
    ),
    "IssueType": obj(
        {
            "id": uuid,
            "name": txt,
            "icon": txt,
            "color": txt,
            "level": d(num, "1 = epic, 0 = standard, -1 = sub-task"),
            "template": null(S("PmNode"), "description new issues of this type start with"),
            "fields": S("TypeFields"),
        },
        ["id", "name", "icon", "color", "level"],
    ),
    "TypeFields": obj(
        {
            "hidden": arr(txt, "fields issues of this type don't show unless they have a value: assignee, priority, labels, sprint, version, affects, component, storyPoints, estimate, startDate, dueDate, security, custom:<id>"),
            "required": arr(txt, "fields people must fill in when creating such an issue or changing an issue to this type, and can't empty (the same names but priority, sprint and security); automation and imports aren't held to them"),
        },
        [],
        "Which fields an issue type hides and requires.",
    ),
    "Version": obj(
        {"id": uuid, "name": txt, "description": ntxt(), "status": enum("unreleased", "released", "archived"), "startDate": ndate(), "releaseDate": ndate(), "releasedAt": ndt()},
        ["id", "name", "status"],
        "A version (release).",
    ),
    "Component": obj({"id": uuid, "name": txt, "description": ntxt(), "leadId": nuuid()}, ["id", "name"]),
    "SecurityLevel": obj(
        {
            "id": uuid,
            "name": txt,
            "description": txt,
            "minRole": {**enum("viewer", "member"), "nullable": True, "description": "everyone with at least this project role sees its issues; null: only the teams and people listed"},
            "teams": arr(S("Named")),
            "users": arr(S("Named")),
            "isDefault": d(boolean, "new issues get it unless the person creating one chooses otherwise"),
            "issueCount": d(num, "issues that carry it"),
            "mine": d(boolean, "the caller belongs to it (and may choose it)"),
        },
        ["id", "name", "minRole", "teams", "users", "isDefault"],
        "Private issues: an issue with a security level is seen only by those the level lists, project admins and the issue's reporter, creator and assignee. Everyone else doesn't find it anywhere — not by key, in lists, boards, search, reports that name issues, activity, live updates or notifications.",
    ),
    "CustomFieldOption": obj({"id": d(txt, "stable option id (generated when left out)"), "label": txt, "color": ntxt()}, ["label"]),
    "CustomField": obj(
        {
            "id": uuid,
            "name": txt,
            "kind": enum("text", "number", "select", "multiselect", "date", "user", "checkbox", "url"),
            "options": arr(S("CustomFieldOption"), "choices of select / multiselect fields"),
            "description": ntxt(),
        },
        ["id", "name", "kind", "options"],
    ),
    "Transition": obj({"from": uuid, "to": uuid}, ["from", "to"], "An allowed move between two statuses (restricted workflows)."),
    "Project": obj(
        {
            "id": uuid,
            "key": d(txt, "2–10 upper-case letters or digits, starting with a letter"),
            "name": txt,
            "description": ntxt(),
            "icon": ntxt(),
            "color": ntxt(),
            "kind": enum("scrum", "kanban", "tracker", desc="board style: scrum (sprints), kanban or tracker; the template it was made from is settings.template"),
            "leadId": nuuid(),
            "parentId": nuuid("the project it sits under (subprojects)"),
            "defaultRole": enum("none", "viewer", "member", "admin", desc="role of workspace members without a grant of their own"),
            "settings": S("ProjectSettings"),
            "createdAt": dt,
            "archivedAt": ndt(),
            "role": enum("viewer", "member", "admin", desc="the caller's role in the project"),
            "starred": boolean,
            "leadName": ntxt(),
            "openCount": d(num, "list only: open issues"),
            "doneCount": d(num, "list only: done issues"),
            "activeSprint": ntxt("list only: name of the running sprint"),
        },
        ["id", "key", "name", "kind", "defaultRole", "settings", "createdAt", "role"],
        "A project as the project list shows it.",
    ),
    "ProjectDetail": {
        "description": "A project with everything its screens need.",
        "allOf": [
            S("Project"),
            obj(
                {
                    "statuses": arr(S("Status"), "in workflow order"),
                    "types": arr(S("IssueType")),
                    "versions": arr(S("Version")),
                    "components": arr(S("Component")),
                    "fields": arr(S("CustomField")),
                    "securityLevels": arr(S("SecurityLevel"), "in order"),
                    "sprints": arr(obj({"id": uuid, "name": txt, "goal": ntxt(), "state": enum("future", "active"), "startAt": ndt(), "endAt": ndt()}, ["id", "name", "state"]), "open sprints (the running one first)"),
                    "boards": arr(obj({"id": uuid, "name": txt, "kind": enum("scrum", "kanban")}, ["id", "name", "kind"])),
                    "transitions": arr(S("Transition")),
                },
                ["statuses", "types", "versions", "components", "fields", "sprints", "boards", "transitions"],
            ),
        ],
    },
    "WebhookEvent": obj(
        {
            "event": enum("issue.created", "issue.updated", "issue.deleted", "comment.created", "sprint.started", "sprint.completed", "version.released", "ping"),
            "timestamp": dt,
            "webhookId": uuid,
            "deliveryId": d(uuid, "unique per delivery (retries of it keep it)"),
            "data": one(
                obj({"issue": S("IssueSummary"), "changed": arr(txt, "issue.updated: the changed fields (status, assignee, …)"), "previousKey": d(txt, "moved to another project: the key it had")}, ["issue"], "issue.created, issue.updated, issue.deleted"),
                obj({"issue": obj({"id": uuid, "key": txt, "title": txt}), "comment": obj({"id": uuid, "text": d(txt, "the comment as plain text")})}, ["issue", "comment"], "comment.created"),
                obj({"sprint": S("Named"), "stats": d(S("SprintStats"), "sprint.completed: the sprint's figures")}, ["sprint"], "sprint.started, sprint.completed"),
                obj({"version": S("Named"), "moved": d(num, "unfinished issues moved on")}, ["version", "moved"], "version.released"),
                obj({}, desc="ping: the test delivery of POST /admin/webhooks/{id}/test (sent once, not retried)"),
                desc="depends on the event",
            ),
        },
        ["event", "timestamp", "webhookId", "deliveryId", "data"],
        "The JSON body of a webhook delivery: a `POST` with `Content-Type: application/json`, `X-TaskCraft-Event: <event>` and "
        "`X-TaskCraft-Signature: sha256=<hex HMAC-SHA256 of the raw body with the webhook's secret>`; failed deliveries are retried up to 7 times with growing pauses (30 seconds, 2 minutes, 8 minutes, … at most 4 hours; about 11 hours in all).",
    ),
}

# ---- operations ------------------------------------------------------------------------------

PATH_PARAMS = {
    "id": (uuid, "id"),
    "what": (enum("statuses", "types", "fields", "versions"), "what to reorder"),
    "report": (enum("overview", "velocity", "cumulative-flow", "cycle-time", "created-resolved", "workload"), "which report"),
}

# security requirements by kind of caller
AUTH = {
    "user": None,  # the document default: an API token or a browser session
    "session": [{"session": []}],  # a browser session; API tokens are refused (403)
    "public": [{}],  # no sign-in
    "optional": [{}, {"token": []}, {"session": []}],  # works signed out, answers more when signed in
    "portal": [{"portalSession": []}],  # the service desk portal's own session
    "portal-optional": [{}, {"portalSession": []}],
    "metrics": [{"metricsToken": []}],
    "mailin": [{"mailInToken": []}, {"mailInHeader": []}, {"mailInQuery": []}],
}

ERRORS = {
    "400": "BadRequest",
    "401": "Unauthorized",
    "403": "Forbidden",
    "404": "NotFound",
    "409": "Conflict",
    "413": "TooLarge",
    "429": "TooMany",
    "503": "Unavailable",
}

R = []


def op(method, path, opid, tag, summary, *, desc=None, req=None, res=None, query=None, headers=None, auth="user", err="", pp=None, extra=None):
    """One operation.

    method, path    as routed (path relative to /api/v1 or /api)
    opid            operationId (camelCase, unique)
    tag             one of TAG_NAMES
    req             request body (body(...), multipart(...))
    res             responses (ok(...), created(...), NO, raw(...), redirect(...); merge several with {**a, **b})
    query, headers  lists of q(...) / hdr(...) parameters
    auth            a key of AUTH
    err             error statuses: defaults are 401 + 403 (signed-in callers), 400 (writes), 404 (path
                    parameters); "409 413" adds, "-404" removes
    pp              path parameter overrides {name: (schema, description)}
    extra           more keys for the operation object (e.g. {"x-feature": "saml"})
    """
    R.append(dict(method=method, path=path, opid=opid, tag=tag, summary=summary, desc=desc, req=req, res=res, query=query, headers=headers, auth=auth, err=err, pp=pp, extra=extra))


# ==== Auth & account, Security, Sign-in, People & teams ====

A_SESSION_COOKIE = {"Set-Cookie": {"schema": txt, "description": "the session cookie `tc_session` (HttpOnly, SameSite=Lax)"}}
A_OK = obj({"ok": d(boolean, "always true")}, ["ok"])
A_CONFIRM = obj({"password": d(txt, "your password; not needed within 15 minutes of signing in")}, desc="Confirms a change to sign-in methods.")
A_PROVIDER_TYPE = enum("oidc", "github", "ldap", "saml")

schemas["Me"] = obj(
    {
        "user": null(S("User"), "null when not signed in"),
        "profile": obj({"title": ntxt("job title"), "locale": ntxt("interface language (null: the workspace default, else the browser's)"), "timezone": ntxt("IANA time zone")}),
        "preferences": d(S("Preferences"), "without the WikiCraft token"),
        "workspace": null(
            obj(
                {
                    "id": uuid,
                    "name": txt,
                    "accentColor": null(txt, "#rrggbb"),
                    "defaultLanguage": null(txt, "interface language for people who haven't chosen one"),
                    "weekStart": d(anyval, "first day of the week as an administrator set it (null: by language)"),
                    "retention": d(anyval, "retention rules (admins only; null for others)"),
                    "uploads": d(anyval, "upload rules: {blockedTypes: [file endings], maxMb} (null: none)"),
                    "virusScanning": d(boolean, "uploads are scanned for viruses (shown to admins; false for others)"),
                    "maxUploadMb": d(num, "the server's largest upload (TASKCRAFT_MAX_UPLOAD_MB)"),
                    "timeZone": null(txt, "default time zone (IANA) for people who haven't picked one"),
                    "dateFormat": null(enum("dmy", "mdy", "ymd"), "how dates are written (null: as the language writes them)"),
                    "announcement": null(obj({"id": uuid, "text": txt, "tone": enum("info", "warning"), "until": null(dt, "shown until"), "at": dt}, ["id", "text", "tone"]),
                                         "the announcement for everyone while it's current"),
                    "features": d(obj({"timeTracking": boolean, "votes": boolean, "portal": boolean, "automation": boolean}), "parts of TaskCraft that are on"),
                    "logo": null(txt, "the logo's address (/api/workspace/logo?v=…); null: TaskCraft's mark"),
                },
                ["id", "name"],
            ),
            "null when not signed in",
        ),
        "wikicraft": obj(
            {
                "url": ntxt("the connected WikiCraft (TASKCRAFT_WIKICRAFT_URL)"),
                "connected": d(boolean, "page lookups work for this person (their own token or the shared one)"),
                "personal": d(boolean, "the person connected their own WikiCraft token"),
            }
        ),
        "baseUrl": d(uri, "TASKCRAFT_BASE_URL"),
        "mustSetUpTwoFactor": d(boolean, "the workspace requires two-factor authentication and this person has no second factor yet: only the setup works until they add one"),
    },
    ["user", "workspace"],
    "The signed-in person, their profile, preferences and workspace. Signed out: only `user` and `workspace`, both null.",
)
schemas["Preferences"] = {
    "type": "object",
    "description": "Personal preferences, stored as a JSON object (20,000 characters at most); keys the server uses are listed, the web app keeps others (theme, board options, …).",
    "properties": {
        "email": enum("off", "instant", "digest", desc="notification e-mails: off, immediately (bundled a couple of minutes after), or a daily digest at 08:00 local time (the default)"),
        "muted": arr(txt, "notification kinds that neither show nor get e-mailed (assigned, mentioned, commented, status, automation, sla, …)"),
        "wikicraftToken": d(txt, "the person's own WikiCraft API token (wct_…), so page lookups respect their wiki permissions; write-only, never returned"),
    },
    "additionalProperties": True,
}
schemas["SecondStep"] = obj(
    {
        "mfaRequired": d(boolean, "always true"),
        "mfaToken": d(txt, "pass to POST /auth/mfa (and /auth/mfa/passkey); valid ten minutes"),
        "methods": arr(enum("code", "passkey"), "second factors the person has: code (authenticator app or recovery code), passkey"),
    },
    ["mfaRequired", "mfaToken", "methods"],
    "A correct password of someone with a second factor: no session yet, the second step follows.",
)
schemas["ApiToken"] = obj(
    {"id": uuid, "name": txt, "scope": enum("read", "write"), "createdAt": dt, "expiresAt": ndt(), "lastUsedAt": ndt()},
    ["id", "name", "scope", "createdAt"],
    "A personal API token (the secret itself is shown only when it is created).",
)
schemas["Session"] = obj(
    {
        "id": uuid,
        "createdAt": dt,
        "lastSeenAt": dt,
        "expiresAt": dt,
        "userAgent": ntxt(),
        "ip": ntxt("client address as the session started"),
        "current": d(boolean, "the session of this request"),
    },
    ["id", "createdAt", "lastSeenAt", "expiresAt", "current"],
    "A browser where the person is signed in.",
)
schemas["Passkey"] = obj(
    {"id": uuid, "name": txt, "backedUp": d(boolean, "synced to the person's other devices"), "createdAt": dt, "lastUsedAt": ndt()},
    ["id", "name", "backedUp", "createdAt"],
)
schemas["MfaStatus"] = obj(
    {
        "enabled": d(boolean, "an authenticator app is set up"),
        "enabledAt": ndt(),
        "recoveryCodesLeft": num,
        "hasPassword": d(boolean, "the account has a local password (single sign-on accounts may not)"),
        "passkeys": d(num, "how many passkeys the person has"),
        "required": d(boolean, "the workspace requires a second factor of this person: the last one can't be removed"),
    },
    ["enabled", "recoveryCodesLeft", "hasPassword", "passkeys", "required"],
)
schemas["PasskeyAssertion"] = obj(
    {
        "id": txt,
        "rawId": d(txt, "credential id, base64url"),
        "type": d(txt, "public-key"),
        "response": obj(
            {"clientDataJSON": d(txt, "base64url"), "authenticatorData": d(txt, "base64url"), "signature": d(txt, "base64url"), "userHandle": ntxt("base64url")},
            ["clientDataJSON", "authenticatorData", "signature"],
        ),
    },
    ["rawId", "response"],
    "The answer of navigator.credentials.get(), as PublicKeyCredential.toJSON() writes it (binary values base64url).",
)
schemas["PasskeyRequestOptions"] = obj(
    {
        "challenge": d(txt, "base64url"),
        "rpId": d(txt, "the host name of TASKCRAFT_BASE_URL"),
        "timeout": d(num, "milliseconds"),
        "userVerification": enum("preferred", "required", desc="required for a sign-in with a passkey alone"),
        "allowCredentials": arr(obj({"type": txt, "id": d(txt, "base64url"), "transports": arr(txt)}), "the person's passkeys (empty for a sign-in with a passkey alone)"),
    },
    ["challenge", "rpId", "timeout", "userVerification", "allowCredentials"],
    "`publicKey` options for navigator.credentials.get() (binary values base64url).",
)
schemas["SignInProvider"] = obj(
    {
        "id": d(txt, "provider id from the configuration"),
        "name": d(txt, "button label"),
        "type": A_PROVIDER_TYPE,
        "preset": null(enum("microsoft", "google", "gitlab"), "OpenID Connect preset"),
        "loginUrl": ntxt("where the sign-in starts (/api/auth/sso/{id}/login); null for LDAP, which uses the password form"),
    },
    ["id", "name", "type"],
)
schemas["SignInMethods"] = obj(
    {
        "password": d(boolean, "signs in with a local password (and password sign-in is on)"),
        "providers": arr(obj({"id": txt, "name": txt, "type": null(A_PROVIDER_TYPE, "null: the provider is no longer configured")}, ["id", "name"]), "single sign-on providers linked to the account"),
    },
    ["password", "providers"],
    "How a person signs in.",
)
schemas["SecurityPolicy"] = obj(
    {
        "requireTwoFactor": enum("off", "admins", "everyone", desc="who must have a second factor (admins: owners and admins)"),
        "enrolled": arr(uuid, "people with a second factor"),
        "people": obj({"total": num, "enrolled": num}, ["total", "enrolled"], "active people, and how many have a second factor"),
        "admins": obj({"total": num, "enrolled": num}, ["total", "enrolled"], "active owners and admins, and how many have a second factor"),
    },
    ["requireTwoFactor", "enrolled", "people", "admins"],
    "The workspace's two-factor policy and who has a second factor.",
)
schemas["Person"] = obj(
    {"id": uuid, "name": txt, "email": ntxt("null for guests asking"), "role": enum("owner", "admin", "member", "guest"), "title": ntxt("job title"),
     "photo": null(txt, "the profile photo's address (/api/users/{id}/photo?v=…)")},
    ["id", "name", "role"],
    "Someone active in the workspace (people pickers, mentions).",
)
schemas["PersonDetail"] = {
    "description": "A person's profile with their teams.",
    "allOf": [S("Person"), obj({"status": enum("active", "invited", "deactivated"), "teams": arr(S("Named"))}, ["status", "teams"])],
}
schemas["UserAccount"] = obj(
    {
        "id": uuid,
        "name": txt,
        "email": ntxt("null once removed"),
        "role": enum("owner", "admin", "member", "guest"),
        "status": enum("active", "invited", "deactivated"),
        "title": ntxt(),
        "createdAt": dt,
        "lastLoginAt": ndt(),
        "removed": d(boolean, "removed for good (DELETE /users/{id}); kept as \"Former user …\" for history"),
        "photo": null(txt, "the profile photo's address"),
        "signIn": null(S("SignInMethods")),
    },
    ["id", "name", "role", "status", "createdAt", "removed"],
    "An account as user administration shows it.",
)
schemas["Team"] = obj(
    {"id": uuid, "name": txt, "description": ntxt(), "color": ntxt(), "memberIds": arr(uuid)},
    ["id", "name", "memberIds"],
    "A team: people who can be given project roles together (provider groups can be mirrored into teams).",
)

# --- Auth & account
op("get", "/setup", "getSetupStatus", "Auth & account", "Whether first-run setup is needed",
   desc="Also allowed while two-factor setup is required.",
   res=ok(obj({"needsSetup": d(boolean, "no workspace exists yet"), "mail": d(boolean, "outgoing e-mail is configured")}, ["needsSetup", "mail"])), auth="public")
op("post", "/setup", "completeSetup", "Auth & account", "First-run setup: create the workspace and its owner",
   desc="Works once (409 afterwards). Signs the owner in. The password needs at least 8 characters.",
   req=body(obj({
       "workspaceName": txt,
       "name": d(txt, "the owner's name"),
       "email": email,
       "password": d(txt, "at least 8 characters"),
       "demo": d(boolean, "fill the workspace with sample projects to explore (default false)"),
   }, ["workspaceName", "name", "email", "password"])),
   res=ok(A_OK, "Set up and signed in", headers=A_SESSION_COOKIE), auth="public", err="409")
op("post", "/auth/login", "signIn", "Auth & account", "Sign in with e-mail (or directory username) and password",
   desc="Answers `{ok: true}` with the session cookie, or — for people with a second factor — a SecondStep without a session: complete it with POST /auth/mfa. "
        "Directory (LDAP) accounts sign in here too. Failed sign-ins are limited to 10 per e-mail address and 15 minutes (then 400). "
        "Wrong e-mail or password: 403. Password sign-in turned off (TASKCRAFT_DISABLE_PASSWORD_LOGIN without a directory): 400.",
   req=body(obj({"email": d(txt, "e-mail address, or a directory username"), "password": txt}, ["email", "password"])),
   res=ok(one(A_OK, S("SecondStep")), "Signed in (session cookie set), or the second step starts", headers=A_SESSION_COOKIE), auth="public", err="403")
op("post", "/auth/logout", "signOut", "Auth & account", "Sign out",
   desc="Ends the session of the cookie sent (if any) and clears the cookie.",
   res=NO, auth="public", err="-400")
op("get", "/auth/link", "getLinkInfo", "Auth & account", "What an invitation or password-reset link is for",
   desc="400 when the link expired or was already used.",
   query=[q("token", "the token from the link (…/welcome?token=…)", required=True)],
   res=ok(obj({"kind": enum("invite", "reset"), "name": txt, "email": ntxt(), "workspace": d(txt, "workspace name")}, ["kind", "name", "workspace"])),
   auth="public", err="400")
op("post", "/auth/link", "acceptLink", "Auth & account", "Accept an invitation or reset the password with a link",
   desc="Sets the password (at least 8 characters), activates an invited account, signs out the person's other sessions and signs them in. "
        "People with a second factor get `{mfaRequired: true}` and a `tc_mfa` cookie instead: the sign-in continues with POST /auth/mfa (without mfaToken). "
        "Links are one-time and valid 7 days (1 hour from POST /auth/forgot). Not available when password sign-in is turned off.",
   req=body(obj({"token": txt, "password": d(txt, "at least 8 characters"), "name": d(txt, "change the name (optional)")}, ["token", "password"])),
   res=ok(one(A_OK, obj({"mfaRequired": boolean}, ["mfaRequired"])), "Signed in, or the second step follows", headers=A_SESSION_COOKIE), auth="public")
op("post", "/auth/forgot", "requestPasswordReset", "Auth & account", "E-mail a password-reset link",
   desc="Always answers 204, whether or not the address has an account; at most three e-mails per address and hour; the link works for an hour. "
        "400 when outgoing e-mail isn't configured or password sign-in is turned off.",
   req=body(obj({"email": txt}, ["email"])), res=NO, auth="public")
op("get", "/me", "getMe", "Auth & account", "The signed-in person, their preferences and workspace",
   desc="Works signed out too (user and workspace null), and while two-factor setup is required (`mustSetUpTwoFactor`).",
   res=ok(S("Me")), auth="optional")
op("put", "/me/profile", "updateProfile", "Auth & account", "Change your name, job title, language and time zone",
   desc="Only the fields sent change; texts are trimmed to 100 characters, an empty title, locale or time zone clears it (the name can't be empty).",
   req=body(obj({"name": txt, "title": d(txt, "job title"), "locale": d(txt, "interface language: en, de, fr, it, …"), "timezone": d(txt, "IANA name, e.g. Europe/Zurich")})),
   res=NO)
op("put", "/me/password", "changePassword", "Auth & account", "Change your password",
   desc="Browser sessions only. Signs out your other sessions. The new password needs at least 8 characters; a wrong current password is a 403, "
        "and after 10 wrong ones in 15 minutes a 400. Not available when password sign-in is turned off.",
   req=body(obj({"current": d(txt, "the current password"), "newPassword": d(txt, "at least 8 characters")}, ["current", "newPassword"])),
   res=NO, auth="session")
op("put", "/me/preferences", "updatePreferences", "Auth & account", "Change your preferences",
   desc="Merges the keys sent into the stored preferences; null removes a key. `email` must be off, instant or digest; `wikicraftToken` must start with wct_.",
   req=body(S("Preferences")), res=NO)
op("get", "/me/tokens", "listApiTokens", "Auth & account", "Your personal API tokens",
   desc="Browser sessions only: tokens are managed in the browser.", res=ok(arr(S("ApiToken"))), auth="session")
op("post", "/me/tokens", "createApiToken", "Auth & account", "Create a personal API token (shown once)",
   desc="Browser sessions only. A `read` token may only read and search (GET, POST /query/validate); `write` (the default) may do whatever its owner may, "
        "except changing sign-in methods and API tokens, which need the browser.",
   req=body(obj({
       "name": txt,
       "scope": d(enum("read", "write"), "default write"),
       "expiresInDays": {"type": "integer", "minimum": 1, "maximum": 3650, "description": "omit for a token that doesn't expire"},
   }, ["name"])),
   res=created(obj({"id": uuid, "token": d(txt, "the token (tct_…); it is not shown again")}, ["id", "token"])), auth="session")
op("delete", "/me/tokens/{id}", "deleteApiToken", "Auth & account", "Delete a personal API token",
   desc="Browser sessions only. Answers 204 also for unknown ids.", res=NO, auth="session", err="-404")
D_EXPORT = ("Everything TaskCraft keeps about the person as one JSON document (a download): profile and preferences, teams and project access, the issues they reported, "
            "created or are assigned (titles only of those they still see), their comments, time logs, attachments, watches, votes, saved filters, notifications, sign-in "
            "sessions and identities, passkeys and API tokens (never secrets), and their entries in the audit log.")
op("put", "/me/photo", "putMyPhoto", "Auth & account", "Set your profile photo",
   desc="In the browser. The image itself is the body: PNG, JPEG, GIF or WebP up to 1 MB (the web app crops it to a 256 px square first).", auth="session",
   req=body(d(binary, "the image"), "image/webp"), res=ok(obj({"photo": d(txt, "its address")}, ["photo"])))
op("delete", "/me/photo", "deleteMyPhoto", "Auth & account", "Remove your profile photo", auth="session", res=NO)
op("get", "/users/{id}/photo", "getPhoto", "People & teams", "A person's profile photo",
   desc="Anyone in the workspace (404 without one). Cached: its address changes with each new photo.", pp={"id": (uuid, "user id")}, res=raw("image/webp", "the image"))
op("delete", "/users/{id}/photo", "deletePhoto", "People & teams", "Take down someone's profile photo",
   desc="Workspace admins (recorded in the audit log).", pp={"id": (uuid, "user id")}, res=NO)
op("get", "/me/export", "exportMyData", "Auth & account", "Download everything about yourself", desc=D_EXPORT, res=raw("application/json", "taskcraft-personal-data.json (attachment)"))

# --- Security
op("post", "/auth/mfa", "completeSecondStep", "Security", "Complete a sign-in with a code or a passkey",
   desc="The second step after a correct password: a code from the authenticator app or a recovery code, or a passkey's answer (after POST /auth/mfa/passkey). "
        "The step comes from `mfaToken` (password sign-in) or the `tc_mfa` cookie (single sign-on, reset links). "
        "Five tries per sign-in, and wrong codes count against a per-person limit (10 per 15 minutes) a new sign-in doesn't reset; each code works once. "
        "Sets the session cookie; `next` is where a single sign-on wanted to go.",
   req=body(obj({
       "mfaToken": d(txt, "from the SecondStep answer; leave out when the step is in the tc_mfa cookie"),
       "code": d(txt, "six digits from the authenticator app, or a recovery code"),
       "passkey": S("PasskeyAssertion"),
   })),
   res=ok(obj({"ok": boolean, "next": ntxt("path to continue at (single sign-on)")}, ["ok"]), "Signed in", headers=A_SESSION_COOKIE),
   auth="public", err="403")
op("post", "/auth/mfa/passkey", "startPasskeySecondStep", "Security", "Start the second step with a passkey",
   desc="Options for navigator.credentials.get() (null when the person has no passkey) and the second factors they have. Then POST /auth/mfa with `passkey`.",
   req=body(obj({"mfaToken": d(txt, "from the SecondStep answer; leave out when the step is in the tc_mfa cookie")})),
   res=ok(obj({"methods": arr(enum("code", "passkey")), "publicKey": null(S("PasskeyRequestOptions"))}, ["methods", "publicKey"])), auth="public")
op("post", "/auth/passkey/options", "startPasskeySignIn", "Security", "Start a sign-in with a passkey alone",
   desc="Options for navigator.credentials.get() and a token for POST /auth/passkey (valid ten minutes). Off (400) when password sign-in is turned off.",
   res=ok(obj({"token": txt, "publicKey": S("PasskeyRequestOptions")}, ["token", "publicKey"])), auth="public")
op("post", "/auth/passkey", "signInWithPasskey", "Security", "Sign in with a passkey that verified you",
   desc="The passkey must have verified the person (PIN, fingerprint, face): that is two factors in one, no password and no second step. "
        "Otherwise 400 — sign in with the password and use the passkey as the second step.",
   req=body(obj({"token": d(txt, "from POST /auth/passkey/options"), "credential": S("PasskeyAssertion")}, ["token", "credential"])),
   res=ok(A_OK, "Signed in", headers=A_SESSION_COOKIE), auth="public", err="403")
op("get", "/me/sessions", "listSessions", "Security", "Where you're signed in",
   desc="Browser sessions that haven't expired, this one first, then by last activity.", res=ok(arr(S("Session"))))
op("post", "/me/sessions/revoke-others", "signOutOtherSessions", "Security", "Sign out everywhere else",
   desc="Browser sessions only.", res=ok(obj({"revoked": d(num, "sessions signed out")}, ["revoked"])), auth="session")
op("delete", "/me/sessions/{id}", "signOutSession", "Security", "Sign out one session", desc="Browser sessions only.", res=NO, auth="session")
op("get", "/me/mfa", "getTwoFactorStatus", "Security", "Your two-factor authentication status", res=ok(S("MfaStatus")))
op("post", "/me/mfa/setup", "startTwoFactorSetup", "Security", "Start setting up an authenticator app",
   desc="A new secret, its otpauth:// address and a QR code; it takes effect once confirmed with POST /me/mfa/enable. "
        "Needs a sign-in from the last 15 minutes or the password (else 403). 400 when an app is already set up (turn it off first). Browser sessions only.",
   req=body(A_CONFIRM, required=False),
   res=ok(obj({"secret": d(txt, "base32 secret, to type in by hand"), "uri": d(txt, "otpauth://totp/… address"), "qrSvg": d(txt, "the address as a QR code (SVG)")}, ["secret", "uri", "qrSvg"])),
   auth="session")
op("post", "/me/mfa/enable", "enableTwoFactor", "Security", "Confirm the authenticator app with a code",
   desc="Turns the app on and returns ten one-time recovery codes (shown once). Browser sessions only.",
   req=body(obj({"code": d(txt, "the app's current six-digit code")}, ["code"])),
   res=ok(obj({"recoveryCodes": arr(txt)}, ["recoveryCodes"])), auth="session")
op("post", "/me/mfa/disable", "disableTwoFactor", "Security", "Turn the authenticator app off",
   desc="Needs the password (accounts without one: a current code). Refused (400) while the workspace requires a second factor and you have no passkey. Browser sessions only.",
   req=body(obj({"password": txt, "code": d(txt, "for accounts without a password")})), res=NO, auth="session")
op("post", "/me/mfa/recovery-codes", "renewRecoveryCodes", "Security", "Replace your recovery codes",
   desc="A current code (or recovery code) confirms; returns ten new recovery codes (shown once), the old ones stop working. Browser sessions only.",
   req=body(obj({"code": txt}, ["code"])), res=ok(obj({"recoveryCodes": arr(txt)}, ["recoveryCodes"])), auth="session")
op("get", "/me/passkeys", "listPasskeys", "Security", "Your passkeys", res=ok(arr(S("Passkey"))))
op("post", "/me/passkeys/options", "startPasskeyRegistration", "Security", "Options to create a passkey",
   desc="`publicKey` options for navigator.credentials.create() (binary values base64url) and a token for POST /me/passkeys. "
        "Needs a sign-in from the last 15 minutes or the password (else 403); at most 20 passkeys per person. Browser sessions only.",
   req=body(A_CONFIRM, required=False),
   res=ok(obj({
       "token": txt,
       "publicKey": obj({
           "rp": obj({"id": txt, "name": txt}),
           "user": obj({"id": d(txt, "base64url"), "name": txt, "displayName": txt}),
           "challenge": d(txt, "base64url"),
           "pubKeyCredParams": arr(obj({"type": txt, "alg": num})),
           "timeout": num,
           "excludeCredentials": arr(obj({"type": txt, "id": d(txt, "base64url"), "transports": arr(txt)})),
           "authenticatorSelection": anyobj,
           "attestation": txt,
       }),
   }, ["token", "publicKey"])), auth="session")
op("post", "/me/passkeys", "addPasskey", "Security", "Store a new passkey",
   desc="The answer of navigator.credentials.create(), as PublicKeyCredential.toJSON() writes it. 409 when the passkey is already registered. Browser sessions only.",
   req=body(obj({
       "token": d(txt, "from POST /me/passkeys/options"),
       "name": d(txt, "up to 80 characters (default Passkey)"),
       "credential": obj({
           "rawId": d(txt, "base64url"),
           "response": obj({"clientDataJSON": d(txt, "base64url"), "attestationObject": d(txt, "base64url"), "transports": arr(txt)}, ["clientDataJSON", "attestationObject"]),
       }, ["rawId", "response"]),
   }, ["token", "credential"])),
   res=created(S("Passkey"), "Added"), auth="session", err="409")
op("patch", "/me/passkeys/{id}", "renamePasskey", "Security", "Rename a passkey", desc="Browser sessions only.",
   req=body(obj({"name": d(txt, "up to 80 characters")}, ["name"])), res=ok(S("Passkey")), auth="session")
op("delete", "/me/passkeys/{id}", "deletePasskey", "Security", "Remove a passkey",
   desc="Needs a sign-in from the last 15 minutes or the password (else 403). Refused (400) for your last second factor while the workspace requires one. Browser sessions only.",
   req=body(A_CONFIRM, required=False), res=NO, auth="session")
op("get", "/admin/security", "getSecurityPolicy", "Security", "The workspace's two-factor policy",
   desc="Workspace admins.", res=ok(S("SecurityPolicy")))
op("put", "/admin/security", "updateSecurityPolicy", "Security", "Require two-factor authentication",
   desc="Workspace admins, browser sessions only. People it applies to who have no second factor yet can only set one up (or sign out) until they have; API tokens keep working.",
   req=body(obj({"requireTwoFactor": enum("off", "admins", "everyone")}, ["requireTwoFactor"])), res=ok(S("SecurityPolicy")), auth="session")
op("post", "/users/{id}/reset-mfa", "resetTwoFactor", "Security", "Reset someone's second factor",
   desc="Workspace admins, browser sessions only; only the owner resets admins. Removes the authenticator app and passkeys, signs the person out everywhere "
        "and deletes their API tokens. Not for yourself (400).",
   res=NO, auth="session")

# --- Sign-in
op("get", "/auth/providers", "listSignInProviders", "Sign-in", "Configured sign-in providers",
   desc="What the login page shows: a button per redirect provider; LDAP accounts use the password form.",
   res=ok(obj({"passwordLogin": d(boolean, "local passwords sign in (TASKCRAFT_DISABLE_PASSWORD_LOGIN not set)"), "providers": arr(S("SignInProvider"))}, ["passwordLogin", "providers"])),
   auth="public")
op("get", "/auth/sso/{id}/login", "startSsoSignIn", "Sign-in", "Start a single sign-on",
   desc="Redirects the browser to the provider (OpenID Connect with PKCE, GitHub, or SAML in builds with it) and binds the sign-in to this browser with a `tc_sso` cookie. "
        "Unknown providers and failures redirect to /login?error=….",
   pp={"id": (txt, "sign-in provider id from the configuration")},
   query=[q("next", "same-site path to return to afterwards (default /)")],
   res=redirect("To the identity provider, or to /login?error=…"), auth="public", err="-404")
op("get", "/auth/sso/{id}/callback", "finishSsoSignIn", "Sign-in", "Single sign-on callback (OpenID Connect, GitHub)",
   desc="The provider sends the browser back here. Creates the session (or the second step on /login?mfa=1 for people with a second factor) and redirects to `next`; "
        "refusals redirect to /login?error=…. Register `{TASKCRAFT_BASE_URL}/api/auth/sso/{id}/callback` as the redirect address at the provider.",
   pp={"id": (txt, "sign-in provider id from the configuration")},
   query=[q("code", "authorization code"), q("state", "the sign-in's state"), q("error", "the provider's error code"), q("error_description", "the provider's error text")],
   res=redirect("To the page the sign-in started from, /login?mfa=1, or /login?error=…"), auth="public", err="-404")
op("post", "/auth/sso/{id}/acs", "samlAssertionConsumer", "Sign-in", "SAML assertion consumer service (feature saml)",
   desc="Exists only in builds with the cargo feature `saml`. The identity provider posts its signed response here (HTTP-POST binding, SP-initiated sign-ins only); "
        "redirects like the OpenID Connect callback.",
   pp={"id": (txt, "sign-in provider id from the configuration")},
   req=body(obj({"SAMLResponse": d(txt, "base64-encoded SAML response"), "RelayState": d(txt, "the sign-in's state")}, ["SAMLResponse"]), "application/x-www-form-urlencoded"),
   res=redirect("To the page the sign-in started from, /login?mfa=1, or /login?error=…"), auth="public", err="-400 -404", extra={"x-feature": "saml"})
op("get", "/auth/sso/{id}/metadata", "getSamlMetadata", "Sign-in", "SAML service-provider metadata (feature saml)",
   desc="Exists only in builds with the cargo feature `saml`. The metadata to register TaskCraft at the identity provider (entity id, assertion consumer service).",
   pp={"id": (txt, "sign-in provider id from the configuration")},
   res={**raw("application/samlmetadata+xml", "SP metadata (XML)", schema={"type": "string"}), "404": {"description": "Not a SAML provider (empty body)"}},
   auth="public", extra={"x-feature": "saml"})
op("get", "/me/sign-in", "getMySignInMethods", "Sign-in", "How you sign in",
   desc="Whether you have a usable local password and which single sign-on providers are linked to your account.",
   res=ok(obj({"passwordLogin": d(boolean, "local passwords sign in here"), "password": d(boolean, "you have a local password that signs in"), "providers": arr(obj({"id": txt, "name": txt, "type": null(A_PROVIDER_TYPE, "null: the provider is no longer configured")}, ["id", "name"]), "single sign-on providers linked to your account")}, ["passwordLogin", "password", "providers"])))

# --- People & teams
op("get", "/people", "listPeople", "People & teams", "Everyone active in the workspace",
   desc="For people pickers and mentions, by name. Guests don't see e-mail addresses.", res=ok(arr(S("Person"))))
op("get", "/people/{id}", "getPerson", "People & teams", "A person's profile and teams",
   desc="Any account of the workspace (also invited and deactivated ones). Guests don't see e-mail addresses.", res=ok(S("PersonDetail")))
op("get", "/users", "listUsers", "People & teams", "All accounts, with how each signs in",
   desc="Workspace admins.", res=ok(arr(S("UserAccount"))))
op("post", "/users", "createUser", "People & teams", "Create an account or invite someone",
   desc="Workspace admins. With a password (at least 8 characters) the account is active right away; without one it is invited: "
        "the answer carries an invitation link (valid 7 days), also e-mailed when outgoing mail is configured. 409 when the e-mail address already has an account.",
   req=body(obj({
       "name": txt,
       "email": email,
       "role": d(enum("admin", "member", "guest"), "default member"),
       "password": d(txt, "set a password instead of inviting (not when password sign-in is turned off)"),
   }, ["name", "email"])),
   res=created(obj({"id": uuid, "inviteUrl": ntxt("invitation link (invited accounts)"), "emailed": d(boolean, "the invitation went out by e-mail")}, ["id", "inviteUrl", "emailed"])),
   err="409")
op("patch", "/users/{id}", "updateUser", "People & teams", "Change someone's name, role or status",
   desc="Workspace admins. Only the fields sent change. The owner's role can't change and the owner can't be deactivated; nobody deactivates themselves. "
        "Deactivating signs the person out everywhere and deletes their API tokens.",
   req=body(obj({"name": txt, "role": enum("admin", "member", "guest"), "status": enum("active", "deactivated")})), res=NO)
op("delete", "/users/{id}", "removeUser", "People & teams", "Remove someone for good",
   desc="Workspace admins; only the owner removes admins, the owner and yourself can't be removed. Comments, time logs and history stay, attributed to "
        "\"Former user …\"; everything personal goes (name, e-mail address, password, second factors, sessions, tokens, notifications, private filters and dashboards) "
        "and their open issues become unassigned.",
   res=NO)
op("get", "/users/{id}/export", "exportUserData", "People & teams", "Download everything about one person",
   desc="Workspace admins (recorded in the audit log). " + D_EXPORT, res=raw("application/json", "taskcraft-personal-data.json (attachment)"))
op("post", "/users/{id}/link", "createUserLink", "People & teams", "A new invitation or password-reset link",
   desc="Workspace admins. An invitation for invited people, a password-reset link for everyone else (valid 7 days), to pass on; also e-mailed when outgoing mail is configured. "
        "Reset links need password sign-in to be on.",
   res=ok(obj({"url": txt, "emailed": boolean}, ["url", "emailed"])))
op("get", "/teams", "listTeams", "People & teams", "Teams with their members", res=ok(arr(S("Team"))))
op("post", "/teams", "createTeam", "People & teams", "Create a team",
   desc="Not for guests. Without `memberIds` the creator is the only member. Team names are unique (409).",
   req=body(obj({"name": txt, "description": txt, "color": txt, "memberIds": arr(uuid)}, ["name"])),
   res=created(obj({"id": uuid}, ["id"])), err="409")
op("patch", "/teams/{id}", "updateTeam", "People & teams", "Change a team",
   desc="Not for guests. Only the fields sent change; `memberIds` replaces the members.",
   req=body(obj({"name": txt, "description": txt, "color": txt, "memberIds": arr(uuid)})), res=NO, err="409")
op("delete", "/teams/{id}", "deleteTeam", "People & teams", "Delete a team",
   desc="Workspace admins. The project roles the team held go with it. Answers 204 also for unknown ids.", res=NO, err="-404")

# ==== Projects, Project configuration, Templates & service levels, Import ====

B_ID = obj({"id": uuid}, ["id"])
B_ROLE = enum("none", "viewer", "member", "admin")
B_MOVE_TO = lambda what: [q("moveTo", f"the {what} its issues get instead (required while issues use it)", uuid)]

schemas["SlaSettings"] = {
    "type": "object",
    "description": "Service-level goals in calendar hours per priority (0–8760; null removes one): time to first response and time to resolution. "
    "The response clock stops at the first move out of a to-do status or the first comment by someone other than the reporter; the resolution clock when the issue is done.",
    "properties": {
        g: {"type": "object", "propertyNames": PRIORITY, "additionalProperties": null(number), "description": f"hours to {g} by priority, e.g. {{\"urgent\": 1, \"high\": 4}}"}
        for g in ("response", "resolution")
    },
    "additionalProperties": False,
}
schemas["ProjectSettings"] = obj(
    {
        "estimation": enum("points", "hours", desc="how work is estimated"),
        "workflow": enum("free", "restricted", desc="free: any status to any other; restricted: only the configured transitions (better set with PUT /projects/{key}/transitions)"),
        "sla": null(S("SlaSettings")),
        "wikiSpace": d(txt, "WikiCraft space key for the project's docs and the pages it writes"),
        "wikiParentPage": d(txt, "WikiCraft page id new pages from the project go under"),
        "defaultAssignee": d(uuid, "who new issues without an assignee get"),
        "doneAfterDays": d(number, "stored as given (not used by the server)"),
        "permissions": d(
            {"type": "object", "additionalProperties": enum("member", "admin", "nobody")},
            "who may: deleteIssues, moveIssues, manageSprints, manageReleases (member or admin; default member), setSecurity (member or admin; default member), moderateComments and logTimeForOthers (member, admin or nobody; default admin). Project admins may whatever members may.",
        ),
        "resolutions": arr(txt, "the project's resolutions, 1–20 distinct names (default Done, Won't do, Duplicate, Cannot reproduce); the first is the default"),
        "template": {**enum("scrum", "kanban", "tracker", "business"), "readOnly": True, "description": "the template the project was created from"},
        "mailIn": {"type": "object", "readOnly": True, "description": "e-mail settings (GET/PUT /projects/{key}/mail-in)"},
        "portal": {"type": "object", "readOnly": True, "description": "service desk settings (GET/PUT /projects/{key}/portal)"},
    },
    desc="Project settings. Changes merge into the current ones key by key (null removes a key); only estimation, workflow, sla, resolutions, permissions, wikiSpace, wikiParentPage, defaultAssignee and doneAfterDays can be set here, anything else is a 400.",
    additional=True,
)
schemas["NewProject"] = obj(
    {
        "key": d(txt, "2–10 letters or digits starting with a letter (upper-cased); a key a project had before is refused (409)"),
        "name": txt,
        "template": d(enum("scrum", "kanban", "tracker", "business"), "statuses, issue types, fields and board to start with (default scrum: also a first sprint)"),
        "description": txt,
        "icon": d(txt, "default rocket"),
        "color": d(txt, "#rrggbb, default #4f6bed"),
        "leadId": d(uuid, "project lead (default: the caller)"),
    },
    ["key", "name"],
)
schemas["ProjectUpdate"] = obj(
    {
        "name": txt,
        "key": d(txt, "a new key: the old one keeps working for links and issue keys (409 when taken now or before by another project)"),
        "description": ntxt("empty clears"),
        "icon": ntxt(),
        "color": ntxt(),
        "leadId": nuuid(),
        "parentId": nuuid("make it a subproject of another project the caller sees (not one below it; three levels at most); null: top level"),
        "defaultRole": d(B_ROLE, "role of workspace members without a grant"),
        "settings": S("ProjectSettings"),
    },
    desc="Only the fields sent change; unknown fields are a 400.",
)
schemas["ProjectGrant"] = obj(
    {
        "subjectType": enum("user", "team"),
        "subjectId": uuid,
        "role": enum("viewer", "member", "admin"),
        "name": {**ntxt("the person's or team's name"), "readOnly": True},
    },
    ["subjectType", "subjectId", "role"],
    "A project role granted to a person or a team.",
)
schemas["ProjectMembers"] = obj(
    {
        "defaultRole": B_ROLE,
        "grants": arr(S("ProjectGrant")),
        "people": arr(obj({"id": uuid, "name": txt, "role": enum("viewer", "member", "admin")}, ["id", "name", "role"]), "everyone active who can see the project, with their effective role (for assignee pickers)"),
    },
    ["defaultRole", "grants", "people"],
)
schemas["Schedule"] = obj(
    {
        "every": enum("hour", "day", "weekday", "week", "month"),
        "at": d(txt, "local time HH:MM (default 08:00; for hour only the minutes count)"),
        "weekday": {"type": "integer", "minimum": 1, "maximum": 7, "default": 1, "description": "for week: Monday = 1 … Sunday = 7"},
        "day": {"type": "integer", "minimum": 1, "maximum": 28, "default": 1, "description": "for month: day of the month"},
        "tz": d(txt, "IANA time zone the times are in (default UTC), e.g. Europe/Zurich"),
    },
    ["every"],
    "When a scheduled rule or recurring template runs (checked once a minute).",
)
schemas["AutomationTrigger"] = obj(
    {
        "event": enum("created", "status", "assigned", "field", "commented", "schedule"),
        "fromStatusId": d(uuid, "status: only moves from this status"),
        "toStatusId": d(uuid, "status: only moves to this status"),
        "toCategory": d(S("StatusCategory"), "status: only moves into this category"),
        "field": d(txt, "field: the changed field as the history names it (status, assignee, priority, labels, sprint, due, custom, …)"),
        "schedule": d(S("Schedule"), "schedule: when the rule runs (it then acts on every issue matching its condition, up to 200)"),
    },
    ["event"],
    "When a rule runs: an issue is created, changes status, is assigned, a field changes, someone comments, or on a schedule.",
)
schemas["AutomationAction"] = obj(
    {
        "action": enum("setStatus", "assign", "setPriority", "addLabel", "removeLabel", "setSprint", "comment", "notify", "chat"),
        "statusId": d(uuid, "setStatus"),
        "to": d(
            txt,
            "assign: reporter or nobody (else userId); notify: assignee (default), reporter, watchers or lead (userId wins)",
        ),
        "userId": d(uuid, "assign / notify: this person"),
        "priority": d(S("Priority"), "setPriority"),
        "label": d(txt, "addLabel / removeLabel"),
        "sprint": d(txt, "setSprint: active = the running sprint, anything else = the backlog"),
        "text": d(txt, "comment: Markdown"),
        "message": d(txt, "notify: up to 300 characters; chat: the message, with {key}, {title}, {status}, {assignee}, {priority}, {project} and {url} filled in (default `{key} {title}`)"),
        "url": d(uri, "chat: the channel's incoming-webhook address"),
        "format": d(enum("slack", "teams", "google-chat"), "chat: the chat service (default slack); nothing is posted about private issues"),
    },
    ["action"],
    "Something a rule does. Rules act as Automation (no person); their changes may trigger other rules, up to three rounds deep.",
)
schemas["AutomationRuleInput"] = obj(
    {
        "name": d(txt, "default Rule"),
        "enabled": d(boolean, "default true"),
        "trigger": S("AutomationTrigger"),
        "condition": d(txt, "TQL the issue must match (empty = every issue; required for scheduled rules), e.g. type = Bug and priority >= high"),
        "actions": arr(S("AutomationAction"), "1–10 actions"),
    },
    ["trigger", "actions"],
)
schemas["AutomationRule"] = obj(
    {
        "id": uuid,
        "name": txt,
        "enabled": boolean,
        "trigger": S("AutomationTrigger"),
        "condition": d(txt, "TQL (empty = every issue)"),
        "actions": arr(S("AutomationAction")),
        "runs": d(num, "how often it acted"),
        "lastRunAt": ndt(),
        "nextRunAt": ndt("scheduled rules: the next run"),
    },
    ["id", "name", "enabled", "trigger", "condition", "actions", "runs"],
)
B_TEMPLATE_FIELDS = obj(
    {
        "typeId": uuid,
        "priority": S("Priority"),
        "labels": arr(txt),
        "assigneeId": uuid,
        "storyPoints": number,
        "estimateMinutes": number,
        "parentId": uuid,
        "componentId": uuid,
        "versionId": uuid,
        "componentIds": arr(uuid),
        "versionIds": arr(uuid),
        "affectedVersionIds": arr(uuid),
        "dueInDays": d(num, "due this many days after the issue is created (0–3650)"),
        "sprint": d(enum("active"), "put it into the running sprint (if there is one)"),
        "custom": d(anyobj, "custom field values by field id"),
    },
    desc="Fields the issue starts with (other keys are dropped); checked when an issue is created from it.",
)
schemas["Template"] = obj(
    {
        "id": uuid,
        "name": txt,
        "title": d(txt, "title of the issues it creates; {date}, {week} and {month} become 2026-11-12, 2026-W46 and 2026-11"),
        "description": null(S("PmNode")),
        "fields": B_TEMPLATE_FIELDS,
        "subtasks": arr(txt, "titles of sub-tasks created with it (the project's sub-task type)"),
        "schedule": null(S("Schedule"), "a recurring template creates its issue by itself (in the name of whoever last changed it)"),
        "nextRunAt": ndt(),
        "lastRunAt": ndt(),
        "lastIssueKey": ntxt("the issue it created last on its schedule"),
    },
    ["id", "name", "title", "description", "fields", "subtasks", "schedule"],
    "An issue template (a recurring issue when it has a schedule).",
)
schemas["TemplateInput"] = obj(
    {
        "name": txt,
        "title": d(txt, "up to 500 characters; {date}, {week}, {month} are filled in; required with a schedule"),
        "description": one(d(txt, "Markdown"), S("PmNode"), {"type": "null"}, desc="Markdown or ProseMirror JSON (empty = none)"),
        "fields": B_TEMPLATE_FIELDS,
        "subtasks": arr(txt, "up to 30 sub-task titles"),
        "schedule": null(S("Schedule")),
    },
    ["name"],
)
schemas["SlaQueue"] = obj(
    {
        "goals": null(S("SlaSettings"), "the project's goals"),
        "counts": obj({"breached": num, "atRisk": num, "running": num}, ["breached", "atRisk", "running"], "running goals by state"),
        "issues": arr(obj({"issue": S("IssueSummary"), "sla": arr(S("SlaGoal"))}, ["issue", "sla"]), "open issues with a running goal, the earliest due first"),
    },
    ["goals", "counts", "issues"],
)
B_CSV_FIELD = d(
    txt,
    "key, id, title, description, type, status, priority, assignee, reporter, labels, sprint, version, component, parent, points, estimate, estimateSeconds, spent, spentSeconds, start, due, created, resolved, custom:<field id> or ignore",
)
schemas["CsvImportPreview"] = obj(
    {
        "headers": arr(txt),
        "rows": arr(arr(txt), "the first five rows"),
        "total": d(num, "rows in the file"),
        "mapping": {"type": "object", "additionalProperties": B_CSV_FIELD, "description": "suggested field per column header"},
        "fields": arr(txt, "the fields a column can go to (plus custom:<id> and ignore)"),
    },
    ["headers", "rows", "total", "mapping", "fields"],
)
schemas["CsvImportRequest"] = obj(
    {
        "csv": d(txt, "the file's text: comma, semicolon or tab separated (chosen by the header line), first line names the columns, at most 5,000 rows"),
        "mapping": {"type": "object", "additionalProperties": B_CSV_FIELD, "description": "column header → field; columns left out get the suggested field. One column must go to title"},
        "dryRun": d(boolean, "only check: report what would be created and what fails (default false)"),
        "createMissing": d(boolean, "create statuses, sprints, releases and components that don't exist yet (default true)"),
    },
    ["csv"],
)
schemas["ImportMessage"] = obj({"row": d(num, "line in the file (the header is line 1)"), "message": txt}, ["row", "message"])
schemas["CsvImportResult"] = obj(
    {
        "dryRun": boolean,
        "rows": num,
        "created": d(num, "issues created (dry run: rows that would be)"),
        "errors": arr(S("ImportMessage"), "rows that were (or would be) left out"),
        "warnings": arr(S("ImportMessage"), "values that couldn't be used"),
        "keys": arr(txt, "keys of the new issues"),
    },
    ["dryRun", "rows", "created", "errors", "warnings", "keys"],
)

# ---- Projects ----
op("get", "/projects", "listProjects", "Projects", "Projects the caller can see",
   desc="With counts of open and done issues, the lead and the running sprint; archived projects last.", res=ok(arr(S("Project"))))
op("post", "/projects", "createProject", "Projects", "Create a project",
   desc="From a template (scrum, kanban, tracker or business: statuses, issue types, fields, a board, for scrum a first sprint). The caller becomes its admin and stars it. Guests can't create projects.",
   req=body(S("NewProject")), res=created(obj({"id": uuid, "key": txt}, ["id", "key"])), err="409")
op("get", "/projects/{key}", "getProject", "Projects", "A project with its configuration",
   desc="Statuses, issue types, versions, components, custom fields, open sprints, boards and allowed transitions.", res=ok(S("ProjectDetail")))
op("patch", "/projects/{key}", "updateProject", "Projects", "Change a project (project admins)",
   desc="Name, key (the old one keeps working), description, icon, colour, lead, default role and settings.", req=body(S("ProjectUpdate")), res=ok(obj({"key": d(txt, "the project's key now")}, ["key"])), err="409")
op("delete", "/projects/{key}", "deleteProject", "Projects", "Delete a project with all its issues",
   desc="Workspace admins who are project admins; the key must be typed to confirm. Attachments are deleted too. Cannot be undone.",
   req=body(obj({"confirm": d(txt, "the project key (any case)")}, ["confirm"])), res=NO)
op("post", "/projects/{key}/archive", "archiveProject", "Projects", "Archive or restore a project (project admins)",
   desc="Archived projects stay readable but take no new issues.", req=body(obj({"archived": d(boolean, "false restores it (default true)")})), res=NO)
op("put", "/projects/{key}/star", "starProject", "Projects", "Star or unstar a project for yourself",
   req=body(obj({"starred": d(boolean, "default true")})), res=NO)
op("get", "/projects/{key}/members", "listProjectMembers", "Projects", "Role grants and everyone with access", res=ok(S("ProjectMembers")))
op("put", "/projects/{key}/members", "setProjectMembers", "Projects", "Replace the role grants (project admins)",
   desc="The whole list of grants. You must stay admin yourself (directly or through a team), else 400.", req=body(arr(S("ProjectGrant"))), res=NO)

# ---- Project configuration ----
op("post", "/projects/{key}/statuses", "createStatus", "Project configuration", "Add a status (project admins)",
   desc="Added at the end of the workflow and reachable from every status and back.",
   req=body(obj({"name": txt, "category": d(S("StatusCategory"), "also accepts to do, open, new, backlog, in progress, doing, closed, resolved, …"), "color": txt}, ["name", "category"])),
   res=created(B_ID), err="409")
op("patch", "/statuses/{id}", "updateStatus", "Project configuration", "Rename a status, change its category or colour, or set its rules (project admins)",
   req=body(obj({"name": txt, "category": S("StatusCategory"), "color": ntxt(), "rules": d(S("StatusRules"), "replaces the rules ({} or null: none)")})), res=NO, err="409")
op("delete", "/statuses/{id}", "deleteStatus", "Project configuration", "Delete a status (project admins)",
   desc="Its issues move to `moveTo`; boards drop it from their columns. A project keeps at least one status.", query=B_MOVE_TO("status"), res=NO)
op("put", "/projects/{key}/order/{what}", "reorderProjectItems", "Project configuration", "Put statuses, types, fields, versions or security levels in order (project admins)",
   pp={"what": (enum("statuses", "types", "fields", "versions", "security-levels"), "what to reorder")},
   req=body(obj({"ids": arr(uuid, "in the new order; ids of other projects are ignored")}, ["ids"])), res=NO)
op("put", "/projects/{key}/transitions", "setTransitions", "Project configuration", "Set the workflow mode and allowed transitions (project admins)",
   desc="Replaces all transitions. In a restricted workflow issues only move along them; in a free one they're kept for later.",
   req=body(obj({"workflow": d(enum("free", "restricted"), "default free"), "transitions": arr(S("Transition"), "between statuses of this project")})), res=NO)
op("post", "/projects/{key}/types", "createIssueType", "Project configuration", "Add an issue type (project admins)",
   req=body(obj({
       "name": txt,
       "icon": d(txt, "default task"),
       "color": d(txt, "default #3b82f6"),
       "level": {"type": "integer", "minimum": -1, "maximum": 1, "default": 0, "description": "1 = epic, 0 = standard, -1 = sub-task"},
       "template": one(d(txt, "Markdown"), S("PmNode"), desc="description new issues of this type start with (empty = none)"),
       "fields": S("TypeFields"),
   }, ["name"])), res=created(B_ID), err="409")
op("patch", "/types/{id}", "updateIssueType", "Project configuration", "Change an issue type (project admins)",
   req=body(obj({"name": txt, "icon": txt, "color": txt, "level": {"type": "integer", "minimum": -1, "maximum": 1}, "template": one(d(txt, "Markdown"), S("PmNode"), {"type": "null"}, desc="empty or null removes it"), "fields": S("TypeFields")})),
   res=NO, err="409")
op("delete", "/types/{id}", "deleteIssueType", "Project configuration", "Delete an issue type (project admins)",
   desc="Its issues get the type `moveTo`. A project keeps at least one type.", query=B_MOVE_TO("type"), res=NO)
op("post", "/projects/{key}/versions", "createVersion", "Project configuration", "Add a version (release) (project members)",
   req=body(obj({"name": txt, "description": txt, "startDate": date, "releaseDate": date}, ["name"])), res=created(B_ID), err="409")
op("patch", "/versions/{id}", "updateVersion", "Project configuration", "Change a version (project members)",
   desc="Setting status released stamps the release time, unreleased clears it.",
   req=body(obj({"name": txt, "description": ntxt(), "startDate": ndate(), "releaseDate": ndate(), "status": enum("unreleased", "released", "archived")})), res=NO, err="409")
op("delete", "/versions/{id}", "deleteVersion", "Project configuration", "Delete a version (project members)", desc="Its issues lose the version.", res=NO)
op("post", "/versions/{id}/release", "releaseVersion", "Project configuration", "Release a version (project members)",
   desc="Unfinished issues move to the version `moveTo` (or lose the version); the release date defaults to today. Sends the version.released webhook.",
   req=body(obj({"moveTo": nuuid("another version of the project")})), res=ok(obj({"moved": d(num, "unfinished issues moved on")}, ["moved"])))
op("post", "/projects/{key}/components", "createComponent", "Project configuration", "Add a component (project admins)",
   req=body(obj({"name": txt, "description": txt, "leadId": uuid}, ["name"])), res=created(B_ID), err="409")
op("patch", "/components/{id}", "updateComponent", "Project configuration", "Change a component (project admins)",
   req=body(obj({"name": txt, "description": ntxt(), "leadId": nuuid()})), res=NO, err="409")
op("delete", "/components/{id}", "deleteComponent", "Project configuration", "Delete a component (project admins)", desc="Its issues lose the component.", res=NO)
LEVEL_FIELDS = {
    "name": d(txt, "unique in the project, at most 60 characters"),
    "description": txt,
    "minRole": {**enum("viewer", "member"), "nullable": True, "description": "everyone with at least this project role; null: only the teams and people listed"},
    "teamIds": arr(uuid),
    "userIds": arr(uuid),
    "isDefault": d(boolean, "new issues get it (the project's previous default stops being one)"),
}
op("post", "/projects/{key}/security-levels", "createSecurityLevel", "Project configuration", "Add a security level for private issues (project admins)",
   req=body(obj(LEVEL_FIELDS, ["name"])), res=created(B_ID))
op("patch", "/security-levels/{id}", "updateSecurityLevel", "Project configuration", "Change a security level (project admins)",
   desc="Only the fields sent change.", req=body(obj(LEVEL_FIELDS)), res=NO)
op("delete", "/security-levels/{id}", "deleteSecurityLevel", "Project configuration", "Delete a security level (project admins)",
   desc="A level that issues carry needs `replaceWith`: another level of the project for them, or `none` to make them public.",
   query=[q("replaceWith", "another security level's id, or none")], res=NO)
op("post", "/projects/{key}/fields", "createCustomField", "Project configuration", "Add a custom field (project admins)",
   desc="The name can't be a built-in TQL field; in queries custom fields are used by name.",
   req=body(obj({
       "name": txt,
       "kind": enum("text", "number", "select", "multiselect", "date", "user", "checkbox", "url"),
       "options": arr(S("CustomFieldOption"), "choices of select / multiselect fields (each needs a label)"),
       "description": txt,
   }, ["name", "kind"])), res=created(B_ID), err="409")
op("patch", "/fields/{id}", "updateCustomField", "Project configuration", "Rename a custom field, describe it or replace its options (project admins)",
   desc="The kind can't change. Keep option ids to keep the values that use them.",
   req=body(obj({"name": txt, "description": ntxt(), "options": arr(S("CustomFieldOption"))})), res=NO, err="409")
op("delete", "/fields/{id}", "deleteCustomField", "Project configuration", "Delete a custom field and its values (project admins)", res=NO)
op("get", "/projects/{key}/automation", "listAutomationRules", "Project configuration", "Automation rules of a project", res=ok(arr(S("AutomationRule"))))
op("post", "/projects/{key}/automation", "createAutomationRule", "Project configuration", "Add an automation rule (project admins)",
   desc="When the trigger fires and the issue matches the TQL condition, the actions run. Scheduled rules act on every issue matching the condition.",
   req=body(S("AutomationRuleInput")), res=created(B_ID))
op("patch", "/automation/{id}", "updateAutomationRule", "Project configuration", "Change an automation rule (project admins)",
   desc="Rename it or turn it on and off; to change what it does, send trigger, condition and actions together (they're checked and replaced as one, a missing condition becomes empty).",
   req=body(obj({"name": txt, "enabled": boolean, "trigger": S("AutomationTrigger"), "condition": txt, "actions": arr(S("AutomationAction"))})), res=NO)
op("delete", "/automation/{id}", "deleteAutomationRule", "Project configuration", "Delete an automation rule (project admins)", res=NO)
op("get", "/labels", "listLabels", "Project configuration", "Labels in use, most used first",
   desc="Over the projects the caller can see; at most 50.", query=[q("prefix", "only labels starting with this")],
   res=ok(arr(obj({"label": txt, "count": num}, ["label", "count"]))))

# ---- Templates & service levels ----
op("get", "/projects/{key}/templates", "listTemplates", "Templates & service levels", "Issue templates of a project", res=ok(arr(S("Template"))))
op("post", "/projects/{key}/templates", "createTemplate", "Templates & service levels", "Add an issue template (project admins)",
   desc="With a schedule it is a recurring issue: it creates its issue by itself.", req=body(S("TemplateInput")), res=created(B_ID))
op("patch", "/templates/{id}", "updateTemplate", "Templates & service levels", "Replace an issue template (project admins)",
   desc="The whole template, like when creating it. Recurring issues are then created in the caller's name.", req=body(S("TemplateInput")), res=NO)
op("delete", "/templates/{id}", "deleteTemplate", "Templates & service levels", "Delete an issue template (project admins)", res=NO)
op("post", "/templates/{id}/create", "createIssueFromTemplate", "Templates & service levels", "Create an issue from a template now (project members)",
   desc="With its sub-tasks. Fields in the body win over the template's (any IssueFields; `description` as ProseMirror JSON).",
   req=body(d(S("IssueFields"), "overrides (send {} for none)")), res=created(S("IssueSummary")))
op("get", "/projects/{key}/sla", "getSlaQueue", "Templates & service levels", "Service-level queue: open issues with goals, most urgent first",
   res=ok(S("SlaQueue")))

# ---- Import ----
op("post", "/projects/{key}/import/preview", "previewCsvImport", "Import", "Read a CSV file: headers, first rows, suggested mapping (project admins)",
   desc="Columns are matched to fields by their header names (also custom fields by name); repeated headers share the first one's field; time columns holding seconds are recognised.",
   req=body(obj({"csv": d(txt, "the file's text")}, ["csv"])), res=ok(S("CsvImportPreview")))
op("post", "/projects/{key}/import", "runCsvImport", "Import", "Import issues from CSV, or check them with dryRun (project admins)",
   desc="Statuses, types, people (by e-mail or name), sprints, releases, components and custom field options are matched by name; missing statuses are created and put on the boards. "
   "Keys of this project keep their numbers, other keys keep working as links; parents by key or id; times in hours, durations (1h 30m) or seconds; wiki-markup descriptions are converted. "
   "Imports tell nobody (no notifications, webhooks or automation).",
   req=body(S("CsvImportRequest")), res=ok(S("CsvImportResult")))

# ==== Issues ====

E_TQL = "a TQL query (see the description at the top); empty matches every issue the caller can see"
E_ORDER = q("order", "order when the query has no `order by`: rank, created (newest first), priority (highest first) or updated (default, latest first)", enum("rank", "created", "priority", "updated"))
E_PROJECT = q("project", "limit the search to this project (key or id)")
E_SUBPROJECTS = q("subprojects", "with project: the projects below it too", boolean)

schemas["ActivityEntry"] = {
    "description": "A change in the activity stream: an IssueEvent with the issue's key and title.",
    "allOf": [S("IssueEvent"), obj({"key": d(txt, "issue key"), "title": d(txt, "issue title")}, ["key", "title"])],
}
schemas["TrashedIssue"] = obj({"id": uuid, "key": txt, "title": txt, "deletedAt": ndt(), "deletedBy": ntxt("name of who deleted it")}, ["id", "key", "title"])
schemas["LiveEvent"] = obj(
    {
        "kind": enum("issue", "comment", "sprint", "board", "project", "version", "notification", desc="what changed"),
        "projectId": nuuid(),
        "issueId": nuuid(),
        "key": ntxt("issue key"),
        "actorId": nuuid("who made the change"),
        "userId": d(uuid, "kind notification: the person it is for (only they get it)"),
    },
    ["kind"],
    "The data of a `change` event on the live update stream (GET /events). Refetch what the event touches.",
)

op("post", "/issues", "createIssue", "Issues", "Create an issue",
   desc="Project members. The description may be ProseMirror JSON (`description`), plain text (`description` as a string) or Markdown (`descriptionMarkdown`). "
        "Any IssueFields may be set at once; they are validated like a change. With `templateId` the issue starts from that template of the project. "
        "Notifies watchers, runs automation and sends the issue.created webhook.",
   req=body(S("NewIssue")), res=created(S("IssueSummary")), err="404")
op("post", "/issues/bulk", "bulkUpdateIssues", "Issues", "Change or delete many issues at once",
   desc="Up to 500 issues. Each one is checked and recorded like a single change (the caller needs to be a member of its project); "
        "issues that fail are listed in `errors`, the others are changed.",
   req=body(obj({"ids": arr(uuid, "issue ids (at most 500)"), "patch": d(S("IssueFields"), "fields to set on every issue"), "action": d(enum("delete"), "delete the issues (to the trash) instead of changing them")}, ["ids"])),
   res=ok(obj({"updated": d(num, "issues changed or deleted"), "errors": arr(obj({"id": uuid, "error": txt}, ["id", "error"]))}, ["updated", "errors"])))
op("get", "/issues/{key}", "getIssue", "Issues", "An issue with description, links, files, sub-tasks and service levels",
   query=[q("view", "1 or true: count this as the caller viewing the issue (their recently viewed issues)", enum("1", "true"))],
   res=ok(S("Issue")))
op("patch", "/issues/{key}", "updateIssue", "Issues", "Change fields of an issue",
   desc="Project members. Only the fields sent change; each change goes into the history, notifies watchers, runs automation and sends the issue.updated webhook. "
        "In restricted workflows `statusId` must follow an allowed transition.",
   req=body(S("IssueFields")), res=ok(S("IssueSummary")))
op("delete", "/issues/{key}", "deleteIssue", "Issues", "Move an issue to the trash",
   desc="Project members. The project's trash keeps it (POST /issues/{key}/restore brings it back). Sends the issue.deleted webhook.", res=NO)
op("post", "/issues/{key}/restore", "restoreIssue", "Issues", "Restore an issue from the trash", desc="Project members.", res=ok(S("IssueSummary")))
op("post", "/issues/{key}/move", "moveIssue", "Issues", "Move an issue on a board or in a backlog",
   desc="Optionally a new status and sprint, and a place between two issues (`after` / `before`; ranks are renumbered as needed). Project members.",
   req=body(obj({
       "statusId": d(uuid, "new status (column)"),
       "sprintId": nuuid("present: move into this sprint; null = the backlog"),
       "after": d(uuid, "place it right after this issue"),
       "before": d(uuid, "place it right before this issue"),
       "fields": d(S("IssueFields"), "fields set along with the move (what the target status requires, a resolution)"),
       "comment": d(txt, "a comment (Markdown) added with the move"),
   })),
   res=ok(S("IssueSummary")))
op("post", "/issues/{key}/clone", "cloneIssue", "Issues", "Copy an issue",
   desc="Copies title (\"Copy of …\"), description, type, priority, labels, points, estimate, parent, version and custom fields, and links the copy to the original (clones). Project members.",
   res=created(S("IssueSummary")))
op("get", "/issues/{key}/history", "getIssueHistory", "Issues", "Everything that happened to an issue",
   desc="Oldest first, at most 1,000 entries.", res=ok(arr(S("IssueEvent"))))
op("put", "/issues/{key}/vote", "voteIssue", "Issues", "Vote for an issue, or take the vote back",
   desc="Anyone who sees the issue, once.", req=body(obj({"voted": d(boolean, "default true; false takes the vote back")})), res=NO)
op("put", "/issues/{key}/watch", "watchIssue", "Issues", "Watch or stop watching an issue",
   desc="Watchers are notified of changes and comments. Adding or removing someone else (`userId`) needs edit rights in the project.",
   req=body(obj({"watching": d(boolean, "default true; false stops watching"), "userId": d(uuid, "someone else (default: the caller)")})), res=NO)
op("post", "/issues/{key}/links", "addIssueLink", "Issues", "Link an issue to another one",
   desc="Project members. Recorded in both issues' history; linking an issue to itself is refused.",
   req=body(obj({
       "kind": enum("blocks", "is blocked by", "blockedBy", "relates", "relates to", "duplicates", "is duplicated by", "duplicatedBy", "clones", desc="the link as read from this issue"),
       "target": d(txt, "the other issue: key or id"),
   }, ["kind", "target"])),
   res=created(obj({"id": uuid}, ["id"])))
op("post", "/issues/{key}/web-links", "addWebLink", "Issues", "Add a web link to an issue",
   desc="Project members. A link to a page of the connected WikiCraft becomes a wiki link and takes the page's title when none is given.",
   req=body(obj({"url": d(uri, "http(s) address, at most 2,000 characters"), "title": d(txt, "default: the page's title (wiki pages) or the address")}, ["url"])),
   res=created(obj({"id": uuid, "title": txt, "kind": enum("web", "wiki")}, ["id", "title", "kind"])))
op("delete", "/links/{id}", "deleteIssueLink", "Issues", "Remove a link between issues", desc="Members of the link's source project.", res=NO)
op("delete", "/web-links/{id}", "deleteWebLink", "Issues", "Remove a web link", desc="Project members.", res=NO)
op("get", "/projects/{key}/export", "exportProject", "Projects", "Export a project for another TaskCraft",
   desc="Workspace admins, in the browser (it holds private issues, restricted comments and people's e-mail addresses). A .zip with taskcraft-project.json "
   "(the project, its workflow, types, fields, sprints, releases, components and issues with their history, comments, time logs, watchers and links between them) "
   "and the files as files/<attachment id>; another TaskCraft imports it with POST /admin/backup-imports. People are matched by e-mail address there. Not the trash.",
   auth="session", res=raw("application/zip", "KEY-yyyy-mm-dd.taskcraft.zip (attachment)"))
op("get", "/projects/{key}/trash", "listTrash", "Issues", "Deleted issues of a project (the trash)",
   desc="Project members; the latest 200, last deleted first.", res=ok(arr(S("TrashedIssue"))))
op("get", "/my-work", "getMyWork", "Issues", "The caller's work: assigned, reported, done this week, watching, recently viewed",
   res=ok(obj({
       "assigned": arr(S("IssueSummary"), "open issues assigned to the caller, by due date and priority (at most 200)"),
       "reported": arr(S("IssueSummary"), "open issues the caller reported, assigned to others"),
       "doneThisWeek": arr(S("IssueSummary"), "the caller's issues resolved in the last 7 days"),
       "watching": arr(S("IssueSummary"), "watched issues updated in the last 7 days"),
       "recent": arr(S("IssueSummary"), "recently viewed (at most 12)"),
       "loggedThisWeek": d(num, "minutes the caller logged since Monday"),
   }, ["assigned", "reported", "doneThisWeek", "watching", "recent", "loggedThisWeek"])))

# ==== Search & TQL ====

op("get", "/search", "searchIssues", "Search & TQL", "Search issues with TQL",
   desc="Only issues of projects the caller can see. Errors in the query answer 400 with a message saying what's wrong.",
   query=[q("q", E_TQL), *page(100, 1000), E_PROJECT, E_SUBPROJECTS, E_ORDER], res=ok(S("SearchResult")))
op("get", "/search.csv", "exportIssuesCsv", "Search & TQL", "Search results as CSV (spreadsheet export)",
   desc="The first 1,000 matches with key, title, type, status, priority, assignee, reporter, sprint, releases (fixed in, found in), components, parent, points, original, remaining and spent hours, labels, dates, resolution, security level and votes.",
   query=[q("q", E_TQL), E_PROJECT, E_SUBPROJECTS, E_ORDER], res=raw("text/csv", "issues.csv (attachment)"))
op("get", "/search.xlsx", "exportIssuesXlsx", "Search & TQL", "Search results as an Excel workbook",
   desc="The same columns as the CSV export, numbers as numbers, with a frozen header row and a filter.",
   query=[q("q", E_TQL), E_PROJECT, E_SUBPROJECTS, E_ORDER], res=raw("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "issues.xlsx (attachment)"))
op("get", "/quick", "quickSearch", "Search & TQL", "Quick search by key or words (pickers, command palette)",
   desc="Up to 15 issues whose key, title or description match; without `q` the caller's recently viewed issues.",
   query=[q("q", "words or an issue key"), E_PROJECT], res=ok(arr(S("IssueSummary"))))
op("post", "/query/validate", "validateQuery", "Search & TQL", "Check a TQL query",
   desc="Open to read-only tokens. Answers 200 either way: `ok`, or the message saying what's wrong.",
   req=body(obj({"q": d(txt, "the query")}, ["q"])),
   res=ok(obj({"ok": boolean, "message": d(txt, "when not ok: what's wrong")}, ["ok"])))
op("get", "/query/suggest", "suggestQueryValues", "Search & TQL", "Field names and values for query completion",
   desc="Without `field`: the field names (custom fields quoted). With a field: its values in the projects the caller can see "
        "(projects, statuses, types, categories, priorities, people and me(), labels, sprints and sprint functions, versions and version functions, components, relative dates, the options of custom fields), "
        "quoted for TQL where needed.",
   query=[q("field", "a field name, e.g. status, assignee, sprint or a custom field's name")], res=ok(arr(txt)))
op("get", "/activity", "listActivity", "Search & TQL", "Recent activity in the caller's projects",
   desc="The latest 60 changes (description edits left out), newest first.",
   query=[q("project", "only this project (key or id)")], res=ok(arr(S("ActivityEntry"))))

# ==== Boards & sprints, Comments & time, Attachments, Dashboards & reports, Notifications & views, moving issues ====

C_TIME = d(txt, "RFC 3339 date-time, or YYYY-MM-DD (09:00 UTC that day)")
C_TQL = d(txt, "TQL query")

schemas["BoardColumn"] = obj(
    {
        "id": d(txt, "column id"),
        "name": txt,
        "statusIds": arr(uuid, "the statuses shown in this column (an issue dropped here gets the first)"),
        "wipLimit": null(num, "work-in-progress limit (null: none)"),
    },
    ["name", "statusIds"],
)
schemas["BoardConfig"] = obj(
    {
        "columns": arr(S("BoardColumn"), "1–30 columns"),
        "swimlanes": enum("none", "assignee", "epic", "priority", "type"),
        "quickFilters": arr(obj({"name": txt, "query": C_TQL}, ["name", "query"]), "one-click filters (TQL, checked on save)"),
        "cardFields": arr(txt, "fields shown on cards: priority, assignee, points, due, labels, …"),
        "hideDoneAfterDays": {"type": "integer", "minimum": 0, "maximum": 3650, "default": 14, "description": "kanban: done issues leave the board after this many days"},
        "filter": d(txt, "TQL that limits which issues the board shows (\"\" = all of the project)"),
    },
    ["columns"],
    "How a board looks. Keys not listed are kept as they are.",
)
schemas["Board"] = obj(
    {"id": uuid, "name": txt, "kind": enum("kanban", "scrum", desc="scrum boards show the running sprint"), "config": S("BoardConfig"), "projectId": uuid},
    ["id", "name", "kind", "config", "projectId"],
)
schemas["BoardView"] = obj(
    {
        "board": S("Board"),
        "canEdit": d(boolean, "the caller may move issues (project members)"),
        "canConfigure": d(boolean, "the caller may change columns and filters (project admins)"),
        "sprint": null(obj({"id": uuid, "name": txt, "goal": ntxt(), "startAt": ndt(), "endAt": ndt()}, ["id", "name"]), "scrum: the sprint shown (null: none running)"),
        "activeSprints": arr(obj({"id": uuid, "name": txt, "endAt": ndt()}, ["id", "name"]), "running sprints of the project"),
        "issues": arr(S("IssueSummary"), "issues in the board's columns (no epics), in rank order, at most 2,000"),
    },
    ["board", "canEdit", "canConfigure", "sprint", "activeSprints", "issues"],
    "A board with its issues.",
)
schemas["SprintStats"] = obj(
    {
        "unit": enum("points", "issues", desc="points when the sprint's issues carry story points, else issue counts"),
        "committed": d(number, "work in the sprint when it started"),
        "completed": d(number, "work done by its end"),
        "committedIssues": num,
        "completedIssues": num,
        "added": d(num, "issues added after the start"),
        "removed": d(num, "issues taken out after the start"),
        "moved": d(num, "unfinished issues moved on when it was completed"),
    },
    desc="Figures kept on a sprint when it is completed (empty before).",
)
schemas["Sprint"] = obj(
    {
        "id": uuid,
        "name": txt,
        "goal": ntxt(),
        "state": enum("future", "active", "closed"),
        "startAt": ndt(),
        "endAt": ndt(),
        "completedAt": ndt(),
        "stats": S("SprintStats"),
    },
    ["id", "name", "state", "stats"],
)
schemas["Backlog"] = obj(
    {
        "canEdit": d(boolean, "the caller may plan (project members)"),
        "sprints": arr(
            {"allOf": [S("Sprint"), obj({"issues": arr(S("IssueSummary"))}, ["issues"])]},
            "open sprints (the running one first, then planned ones in order), each with its issues in rank order",
        ),
        "backlog": arr(S("IssueSummary"), "unfinished issues without a sprint, in rank order"),
        "epics": arr(S("IssueSummary"), "the project's epics (for the epic panel)"),
    },
    ["canEdit", "sprints", "backlog", "epics"],
    "Sprints and the backlog of a project (standard issues only: no epics or sub-tasks in the lists).",
)
schemas["Burndown"] = obj(
    {
        "sprint": obj(
            {"id": uuid, "name": txt, "state": enum("future", "active", "closed"), "goal": ntxt(), "startAt": ndt(), "endAt": ndt(), "completedAt": ndt()},
            ["id", "name", "state"],
        ),
        "unit": enum("points", "issues"),
        "committed": d(number, "work in the sprint at its start"),
        "committedIssues": num,
        "remaining": d(number, "work not done yet (at the end, for a completed sprint)"),
        "completed": number,
        "completedIssues": num,
        "added": d(num, "issues added after the start"),
        "removed": d(num, "issues taken out after the start"),
        "start": d(dt, "start of the ideal line"),
        "end": d(dt, "end of the ideal line (completion, planned end, or two weeks after the start)"),
        "now": dt,
        "steps": arr(
            obj(
                {
                    "at": dt,
                    "remaining": d(number, "remaining work after this step"),
                    "event": enum("start", "added", "removed", "completed", "reopened", "estimate", "changed"),
                    "key": d(ntxt(), "the issue that changed it (not on the start step)"),
                },
                ["at", "remaining", "event"],
            ),
            "every change of the remaining work, replayed from the issues' history",
        ),
    },
    ["sprint", "unit", "committed", "committedIssues", "remaining", "completed", "completedIssues", "added", "removed", "start", "end", "now", "steps"],
    "Sprint burndown: remaining work over the sprint, with scope changes.",
)
C_STAT = lambda: {"average": number, "median": number, "p85": d(number, "85th percentile")}
schemas["OverviewReport"] = obj(
    {
        "categories": d({"type": "object", "additionalProperties": num}, "issues per status category (todo, in_progress, done; categories without issues are left out)"),
        "statuses": arr(obj({"id": uuid, "name": txt, "category": S("StatusCategory"), "color": ntxt(), "count": num}, ["id", "name", "category", "count"]), "issues per status, in workflow order"),
        "types": arr(obj({"name": txt, "icon": txt, "color": txt, "count": num}, ["name", "count"]), "open issues per type"),
        "priorities": arr(obj({"priority": S("Priority"), "count": num}, ["priority", "count"]), "open issues per priority, urgent first"),
        "overdue": d(num, "open issues past their due date"),
        "unassigned": d(num, "open issues without an assignee"),
        "createdLastWeek": num,
        "resolvedLastWeek": num,
        "stale": d(num, "issues in progress whose status hasn't changed for 7 days"),
    },
    ["categories", "statuses", "types", "priorities", "overdue", "unassigned", "createdLastWeek", "resolvedLastWeek", "stale"],
    "report=overview",
)
schemas["VelocityReport"] = obj(
    {
        "sprints": arr(
            obj(
                {
                    "id": uuid,
                    "name": txt,
                    "startAt": ndt(),
                    "completedAt": ndt(),
                    "committed": number,
                    "completed": number,
                    "committedIssues": num,
                    "completedIssues": num,
                    "unit": enum("points", "issues"),
                },
                ["id", "name", "committed", "completed", "committedIssues", "completedIssues", "unit"],
            ),
            "completed sprints, oldest first",
        ),
        "average": d(number, "average completed work per sprint"),
        "unit": enum("points", "issues"),
    },
    ["sprints", "average", "unit"],
    "report=velocity: committed vs completed work of the latest completed sprints (`count`, default 8, 1–24)",
)
schemas["CumulativeFlowReport"] = obj(
    {
        "statuses": arr(obj({"id": uuid, "name": txt, "category": S("StatusCategory"), "color": ntxt()}, ["id", "name", "category"]), "in workflow order"),
        "days": arr(obj({"date": date, "counts": d({"type": "object", "additionalProperties": num}, "issues per status id at the end of the day")}, ["date", "counts"])),
    },
    ["statuses", "days"],
    "report=cumulative-flow: issues per status at the end of each of the last `days` days (default 30, 7–365)",
)
schemas["CycleTimeReport"] = obj(
    {
        "issues": arr(
            obj(
                {
                    "key": txt,
                    "title": txt,
                    "type": d(txt, "issue type name"),
                    "resolvedAt": dt,
                    "cycleDays": d(number, "first move to in progress (or done) → done, in days (one decimal)"),
                    "leadDays": d(number, "created → done, in days"),
                },
                ["key", "title", "type", "resolvedAt", "cycleDays", "leadDays"],
            ),
            "issues finished in the period, oldest first",
        ),
        "cycle": obj(C_STAT(), ["average", "median", "p85"]),
        "lead": obj(C_STAT(), ["average", "median", "p85"]),
    },
    ["issues", "cycle", "lead"],
    "report=cycle-time: cycle and lead time of issues finished in the last `days` days (default 90, 7–730)",
)
schemas["CreatedResolvedReport"] = obj(
    {"days": arr(obj({"date": date, "created": num, "resolved": num}, ["date", "created", "resolved"]))},
    ["days"],
    "report=created-resolved: issues created and resolved per day over the last `days` days (default 30, 7–365)",
)
schemas["WorkloadReport"] = obj(
    {
        "people": arr(
            obj(
                {
                    "id": nuuid("null: unassigned work"),
                    "name": ntxt(),
                    "todo": num,
                    "inProgress": num,
                    "points": d(number, "story points of their open issues"),
                    "overdue": num,
                },
                ["id", "name", "todo", "inProgress", "points", "overdue"],
            ),
            "most open issues first",
        )
    },
    ["people"],
    "report=workload: open work per assignee",
)
schemas["Comment"] = obj(
    {
        "id": uuid,
        "author": null(S("Named"), "the person who wrote it (null: a requester without an account, or automation)"),
        "external": null(obj({"email": email, "name": ntxt()}, ["email"]), "an author without an account (the issue's requester, by e-mail or in the portal)"),
        "body": S("PmNode"),
        "createdAt": dt,
        "updatedAt": dt,
        "edited": boolean,
        "viaEmail": d(boolean, "it came in by e-mail"),
        "viaPortal": d(boolean, "written in the service desk portal"),
        "forRequester": d(boolean, "the issue's requester sees it (in the portal, and by e-mail)"),
        "emailedAt": ndt("when it was e-mailed to the requester"),
        "visibility": ntxt("restricted comments: `members`, `admins` or `team:<id>` (its author and project admins read it too); null: everyone who sees the issue"),
        "visibleTo": ntxt("the team's name for a team comment"),
        "reactions": arr(obj({"reaction": S("Reaction"), "people": arr(S("Named"))}, ["reaction", "people"])),
    },
    ["id", "author", "external", "body", "createdAt", "updatedAt", "edited", "viaEmail", "viaPortal", "forRequester", "emailedAt", "reactions"],
)
schemas["Reaction"] = enum("thumbs-up", "heart", "check", "eyes", "party")
schemas["CommentInput"] = obj(
    {
        "body": one(S("PmNode"), d(txt, "plain text"), desc="the comment as rich text (ProseMirror JSON) or plain text"),
        "markdown": d(txt, "the comment as Markdown (instead of body)"),
        "forRequester": d(boolean, "new comments: visible to the issue's requester (in the portal, and e-mailed when outgoing mail is set up); people who can edit the issue, issues with a requester only"),
        "emailRequester": d(boolean, "the earlier name of forRequester"),
        "visibility": ntxt("new comments: who may read it besides its author and project admins — `members` (not viewers or guests), `admins` (project admins writing), `team:<id>` (a team the writer is in); not with forRequester. Restricted comments are left out of history, activity, notifications and comment search for everyone else."),
    },
    desc="`body` or `markdown` is required; mentions in rich text notify the people mentioned.",
)
schemas["Worklog"] = obj(
    {
        "id": uuid,
        "user": null(S("Named"), "who did the work"),
        "minutes": num,
        "date": d(date, "the day worked"),
        "note": ntxt(),
        "category": null(S("Named"), "what the time went to (see TimeCategory)"),
        "billable": boolean,
    },
    ["id", "user", "minutes", "date", "note", "category", "billable"],
    "A time log.",
)
schemas["TimeCategory"] = obj(
    {
        "id": uuid,
        "name": txt,
        "billable": d(boolean, "time logged with it is billable unless the person logging says otherwise"),
        "archived": d(boolean, "kept for time already logged, not offered any more"),
        "logCount": d(num, "time logs with it"),
    },
    ["id", "name", "billable", "archived"],
    "What time is spent on (workspace-wide).",
)
schemas["TimeReportRow"] = obj(
    {
        "id": d(txt, "the person's, project's, issue's or category's id (`none`: no category; empty: someone removed), or the period: YYYY-MM-DD for days and weeks (their first day), YYYY-MM for months"),
        "label": d(txt, "name; issues as `KEY title`"),
        "minutes": num,
        "billableMinutes": num,
        "children": arr({"$ref": "#/components/schemas/TimeReportRow"}, "the second grouping (`then`)"),
    },
    ["id", "label", "minutes", "billableMinutes"],
)
schemas["TimeReport"] = obj(
    {
        "from": date,
        "to": date,
        "group": txt,
        "then": ntxt(),
        "totalMinutes": num,
        "billableMinutes": num,
        "logCount": num,
        "truncated": d(boolean, "more than 100,000 logs: only those were counted"),
        "rows": arr(S("TimeReportRow"), "periods in order, everything else most time first"),
    },
    ["from", "to", "group", "totalMinutes", "billableMinutes", "logCount", "truncated", "rows"],
)
schemas["Timesheet"] = obj(
    {
        "from": date,
        "to": date,
        "days": num,
        "issues": arr(
            obj(
                {"id": uuid, "key": txt, "title": txt, "days": d({"type": "object", "additionalProperties": num}, "minutes logged per day (YYYY-MM-DD)")},
                ["id", "key", "title", "days"],
            ),
            "issues with time logged in the period (in projects the caller can see), by key",
        ),
    },
    ["from", "to", "days", "issues"],
)
schemas["MovePreview"] = obj(
    {
        "target": obj({"key": txt, "name": txt}, ["key", "name"]),
        "typeId": d(uuid, "the type it would get (same name, else same level)"),
        "statusId": d(uuid, "the status it would get (same name, else the first of the same category)"),
        "droppedFields": arr(txt, "custom fields whose values would be lost (no field of the same name and kind, or no matching option)"),
        "cleared": arr(enum("sprint", "release", "component"), "project-specific fields that would be cleared"),
        "subtasks": d(num, "sub-tasks that move along"),
        "securityLevel": ntxt("the security level a private issue gets there (the one of the same name, else the target's default); a private issue the target has no level for can't move"),
    },
    ["target", "typeId", "statusId", "droppedFields", "cleared", "subtasks"],
    "What moving an issue to another project would do.",
)
schemas["WidgetConfig"] = obj(
    {
        "query": d(txt, "issues, count, breakdown: the TQL query (checked on save)"),
        "limit": d(num, "issues, mywork: how many issues (5, 10, 20, 50; default 10)"),
        "by": d(enum("status", "category", "assignee", "priority", "type", "project", "labels", "sprint"), "breakdown: what to count by"),
        "project": d(txt, "burndown, velocity, flow, created: the project key (required); activity: one project (else all)"),
        "text": d(txt, "note: the note's text"),
        "tone": d(enum("danger", "success"), "count: colour of the number"),
    },
    desc="Settings of a widget; which keys apply depends on its type. Other keys are kept.",
    additional=True,
)
schemas["Widget"] = obj(
    {
        "id": d(txt, "generated when left out"),
        "type": enum("issues", "count", "breakdown", "burndown", "velocity", "flow", "created", "activity", "mywork", "note"),
        "title": d(txt, "up to 120 characters"),
        "config": S("WidgetConfig"),
        "wide": d(boolean, "spans the full width"),
    },
    ["type"],
)
schemas["Dashboard"] = obj(
    {
        "id": uuid,
        "ownerId": nuuid(),
        "ownerName": ntxt(),
        "name": txt,
        "shared": d(boolean, "everyone in the workspace sees it"),
        "widgets": arr(S("Widget")),
        "updatedAt": dt,
        "mine": d(boolean, "the caller owns it"),
    },
    ["id", "ownerId", "name", "shared", "widgets", "updatedAt", "mine"],
)
schemas["GroupCounts"] = obj(
    {
        "by": txt,
        "total": d(num, "sum of the counts"),
        "groups": arr(
            obj(
                {
                    "key": d(txt, "what the group stands for: status name (lower case), category, person id, priority number, type name, project key, label or sprint id; \"\" for none"),
                    "label": ntxt("for people (priority: its name)"),
                    "count": num,
                    "points": d(number, "sum of story points"),
                    "color": ntxt("status, type or project colour"),
                    "extra": ntxt("status: its category; type, project: icon; sprint: its state"),
                },
                ["key", "label", "count", "points"],
            ),
            "largest first, at most 50",
        ),
    },
    ["by", "total", "groups"],
)
schemas["Portfolio"] = obj(
    {
        "projects": arr(
            obj(
                {
                    "id": uuid,
                    "key": txt,
                    "name": txt,
                    "color": ntxt(),
                    "icon": ntxt(),
                    "kind": txt,
                    "epics": arr(
                        obj(
                            {
                                "id": uuid,
                                "key": txt,
                                "title": txt,
                                "color": ntxt(),
                                "startDate": ndate(),
                                "dueDate": ndate(),
                                "category": S("StatusCategory"),
                                "childCount": num,
                                "childDone": num,
                                "points": number,
                                "pointsDone": number,
                                "childStart": ndate("earliest start (or due) date of its issues"),
                                "childEnd": ndate("latest due (or start) date of its issues"),
                            },
                            ["id", "key", "title", "category", "childCount", "childDone", "points", "pointsDone"],
                        ),
                        "open epics and those finished in the last 60 days",
                    ),
                    "versions": arr(
                        obj(
                            {"id": uuid, "name": txt, "status": txt, "startDate": ndate(), "releaseDate": ndate(), "total": num, "done": num},
                            ["id", "name", "status", "total", "done"],
                        ),
                        "unreleased versions and those released in the last 60 days",
                    ),
                },
                ["id", "key", "name", "kind", "epics", "versions"],
            )
        ),
        "teams": arr(
            obj(
                {
                    "id": uuid,
                    "name": txt,
                    "members": arr(
                        obj(
                            {"id": uuid, "name": txt, "todo": num, "inProgress": num, "points": number, "overdue": num, "dueThisWeek": num},
                            ["id", "name", "todo", "inProgress", "points", "overdue", "dueThisWeek"],
                        )
                    ),
                },
                ["id", "name", "members"],
            ),
            "every team with its active members' open work in these projects",
        ),
    },
    ["projects", "teams"],
)
schemas["View"] = obj(
    {
        "id": uuid,
        "ownerId": nuuid(),
        "ownerName": ntxt(),
        "projectId": nuuid("the project it belongs to (null: across projects)"),
        "projectKey": ntxt(),
        "name": txt,
        "query": C_TQL,
        "layout": enum("list", "board", "calendar", "timeline"),
        "config": d(anyobj, "how it is shown (list: `columns`, `group`, …)"),
        "shared": d(boolean, "everyone who can see its project sees it"),
        "updatedAt": dt,
        "starred": d(boolean, "starred by the caller (shown in the sidebar)"),
        "mine": d(boolean, "the caller owns it"),
        "subscribed": d(boolean, "the caller gets its results by e-mail"),
    },
    ["id", "name", "query", "layout", "config", "shared", "updatedAt", "starred", "mine"],
    "A saved filter: a TQL query and how it is shown.",
)
schemas["Notification"] = obj(
    {
        "id": uuid,
        "actor": null(S("Named"), "who caused it (null: the system, automation or a requester)"),
        "kind": enum("assigned", "status", "mentioned", "commented", "sla", "automation"),
        "data": d(
            obj(
                {
                    "key": d(txt, "the issue's key"),
                    "title": d(txt, "the issue's title"),
                    "from": d(txt, "status: previous status"),
                    "to": d(txt, "status: new status"),
                    "category": d(txt, "status: new status category"),
                    "commentId": d(nuuid(), "mentioned, commented"),
                    "excerpt": d(txt, "commented: the start of the comment"),
                    "by": d(txt, "a requester without an account who did it"),
                    "goal": d(txt, "sla: response or resolution"),
                    "targetMinutes": d(num, "sla: the missed goal"),
                    "message": d(txt, "automation: the rule's message"),
                },
                ["key", "title"],
                additional=True,
            ),
            "details by kind",
        ),
        "createdAt": dt,
        "read": boolean,
    },
    ["id", "actor", "kind", "data", "createdAt", "read"],
)

# ---- Boards & sprints

op(
    "post",
    "/projects/{key}/boards",
    "createBoard",
    "Boards & sprints",
    "Add a board to a project",
    desc="Project admins. The board starts with a column per status, no swimlanes, an \"Only my issues\" quick filter and the usual card fields.",
    req=body(obj({"name": d(txt, "default: Board"), "kind": d(enum("kanban", "scrum"), "default: kanban"), "filter": d(txt, "TQL that limits the board's issues")})),
    res=created(obj({"id": uuid}, ["id"])),
)
op(
    "get",
    "/boards/{id}",
    "getBoard",
    "Boards & sprints",
    "A board with its issues",
    desc="Scrum boards show the issues of a running sprint (the one asked for, else the first); kanban boards hide done issues resolved more than `hideDoneAfterDays` ago.",
    query=[q("q", "extra TQL (quick filters, search) combined with the board's filter"), q("sprint", "scrum: which running sprint to show", uuid)],
    res=ok(S("BoardView")),
    err="400",
)
op(
    "put",
    "/boards/{id}",
    "updateBoard",
    "Boards & sprints",
    "Rename a board or change its configuration",
    desc="The keys given in `config` replace those of the board (the whole configuration is checked). Members may change `swimlanes`; the name and everything else need a project admin.",
    req=body(obj({"name": txt, "config": d(S("BoardConfig"), "keys to replace (partial)")})),
    res=NO,
)
op("delete", "/boards/{id}", "deleteBoard", "Boards & sprints", "Delete a board", desc="Project admins; a project keeps at least one board (400).", res=NO)
op(
    "get",
    "/projects/{key}/backlog",
    "getBacklog",
    "Boards & sprints",
    "Sprints and the backlog of a project",
    query=[q("q", "TQL filter for the issues")],
    res=ok(S("Backlog")),
    err="400",
)
op("get", "/projects/{key}/sprints", "listSprints", "Boards & sprints", "All sprints of a project", desc="The running sprint first, then planned ones, then completed ones (latest first).", res=ok(arr(S("Sprint"))))
op(
    "post",
    "/projects/{key}/sprints",
    "createSprint",
    "Boards & sprints",
    "Plan a sprint",
    desc="Project members. Without a name it is called \"<KEY> Sprint <n>\".",
    req=body(obj({"name": txt, "goal": txt, "startAt": C_TIME, "endAt": C_TIME})),
    res=created(obj({"id": uuid, "name": txt}, ["id", "name"])),
)
op(
    "patch",
    "/sprints/{id}",
    "updateSprint",
    "Boards & sprints",
    "Change a sprint's name, goal or dates",
    desc="Project members. Only the fields sent change; an empty goal or a null date clears it.",
    req=body(obj({"name": txt, "goal": ntxt(), "startAt": null(C_TIME), "endAt": null(C_TIME)})),
    res=NO,
)
op("delete", "/sprints/{id}", "deleteSprint", "Boards & sprints", "Delete a planned sprint", desc="Project members; only planned (future) sprints. Its issues go back to the backlog.", res=NO)
op(
    "post",
    "/sprints/{id}/start",
    "startSprint",
    "Boards & sprints",
    "Start a planned sprint",
    desc="Project members. One sprint runs at a time (400 while another runs). The start defaults to the planned start or now (never later than now), the end to the planned end or `weeks` after the start. Sends the `sprint.started` webhook.",
    req=body(obj({"startAt": C_TIME, "endAt": C_TIME, "weeks": {"type": "integer", "minimum": 1, "maximum": 8, "default": 2}, "goal": txt})),
    res=ok(obj({"startAt": dt, "endAt": dt}, ["startAt", "endAt"])),
)
op(
    "post",
    "/sprints/{id}/complete",
    "completeSprint",
    "Boards & sprints",
    "Complete the running sprint",
    desc="Project members. Unfinished issues move to `moveTo` — a planned sprint's id, `new` (a new sprint), or `backlog` (the default). The sprint's figures are kept on it; sends the `sprint.completed` webhook.",
    req=body(obj({"moveTo": d(txt, "a planned sprint id, new or backlog")})),
    res=ok(obj({"moved": d(num, "unfinished issues moved"), "movedTo": nuuid("the sprint they moved to (null: the backlog)"), "stats": S("SprintStats")}, ["moved", "movedTo", "stats"])),
)
op("get", "/sprints/{id}/burndown", "getBurndown", "Boards & sprints", "Sprint burndown", desc="Replayed from the issues' history, so it stays right when scope or estimates change mid-sprint.", res=ok(S("Burndown")))

# ---- Issues: moving to another project

op(
    "get",
    "/issues/{key}/move-project",
    "previewMoveToProject",
    "Issues",
    "Preview moving an issue to another project",
    desc="Project members of both projects. Nothing changes.",
    query=[q("to", "the target project's key", required=True)],
    res=ok(S("MovePreview")),
    err="400",
)
op(
    "post",
    "/issues/{key}/move-project",
    "moveToProject",
    "Issues",
    "Move an issue to another project",
    desc="Project members of both projects; not into an archived project. The issue gets a new key (the old one keeps working) and its history moves along; type and status are mapped by name "
    "(or given), custom fields kept where the target has a field of the same name and kind, sprint, release and component cleared; sub-tasks move along. A private issue takes the target's security level of the same name (else its default) and doesn't move without one. Sends `issue.updated` with `previousKey`.",
    req=body(obj({"projectKey": d(txt, "the target project"), "typeId": d(uuid, "a type of the target project (default: mapped)"), "statusId": d(uuid, "a status of the target project (default: mapped)")}, ["projectKey"])),
    res=ok(S("IssueSummary"), "Moved"),
)

# ---- Comments & time

op("get", "/issues/{key}/comments", "listComments", "Comments & time", "Comments of an issue", desc="Oldest first, internal ones included; restricted comments only for those they're for.", res=ok(arr(S("Comment"))))
op(
    "post",
    "/issues/{key}/comments",
    "createComment",
    "Comments & time",
    "Comment on an issue",
    desc="Anyone who can see the issue. Rich text (`body`, ProseMirror JSON or plain text) or `markdown`. With `forRequester` (people who can edit the issue; only issues with a requester) the requester sees it in the portal and it is e-mailed to them. "
    "Commenting makes the author a watcher; sends the `comment.created` webhook.",
    req=body(S("CommentInput")),
    res=created(obj({"id": uuid, "forRequester": boolean, "emailed": d(boolean, "it was e-mailed to the requester")}, ["id", "forRequester", "emailed"])),
)
op(
    "patch",
    "/comments/{id}",
    "updateComment",
    "Comments & time",
    "Edit a comment",
    desc="Its author, or whoever the project's `moderateComments` permission allows (project admins by default). Replaces the text (`body` or `markdown`); `forRequester` can't be changed here.",
    req=body(S("CommentInput")),
    res=NO,
)
op("delete", "/comments/{id}", "deleteComment", "Comments & time", "Delete a comment", desc="Its author, or whoever the project's `moderateComments` permission allows (project admins by default; see ProjectSettings.permissions).", res=NO)
op(
    "post",
    "/comments/{id}/react",
    "reactToComment",
    "Comments & time",
    "Add or take back a reaction",
    desc="Toggles the caller's reaction on the comment.",
    req=body(obj({"reaction": S("Reaction")}, ["reaction"])),
    res=NO,
)
op("get", "/issues/{key}/worklogs", "listWorklogs", "Comments & time", "Time logged on an issue", desc="Latest day first.", res=ok(arr(S("Worklog"))))
op(
    "post",
    "/issues/{key}/worklogs",
    "logWork",
    "Comments & time",
    "Log time on an issue",
    desc="Project members, as themselves or — with `userId` — for someone else where the project's `logTimeForOthers` permission allows it (project admins by default). `minutes`, or `time` like `1h 30m`, `45m`, `1.5h`, `2d` (8-hour days), `1w` (5 days); at most a week. The issue's time spent is updated, and its remaining estimate as `remaining` says.",
    req=body(
        obj(
            {
                "minutes": d(number, "time spent in minutes (instead of time)"),
                "userId": d(uuid, "whose time it is (default: the caller)"),
                "time": d(txt, "time spent as a duration: 1h 30m, 45m, 1.5h, 2d"),
                "date": d(date, "the day worked (default: today)"),
                "note": txt,
                "categoryId": nuuid("a time category (not an archived one)"),
                "billable": d(boolean, "default: the category's setting, else false"),
                "remaining": one(
                    enum("auto", "keep"),
                    d(txt, "a new remaining estimate such as 3h"),
                    d(number, "a new remaining estimate in minutes"),
                    desc="the remaining estimate afterwards: auto (default: down by the time logged — by itself unless someone set it), keep (as it was before this log), or a new value",
                ),
            }
        )
    ),
    res=created(obj({"id": uuid, "minutes": num}, ["id", "minutes"])),
)
op(
    "patch",
    "/worklogs/{id}",
    "updateWorklog",
    "Comments & time",
    "Change a time log",
    desc="The person who logged it or a project admin. Only the fields sent change; an empty or null note clears it.",
    req=body(obj({"minutes": number, "time": d(txt, "duration such as 1h 30m (instead of minutes)"), "date": date, "note": ntxt(), "categoryId": nuuid(), "billable": boolean})),
    res=ok(obj({"id": uuid, "minutes": num, "date": date, "note": ntxt(), "categoryId": nuuid(), "billable": boolean}, ["id", "minutes", "date", "note", "categoryId", "billable"])),
)
op("delete", "/worklogs/{id}", "deleteWorklog", "Comments & time", "Delete a time log", desc="The person who logged it or a project admin.", res=NO)
op(
    "get",
    "/timesheet",
    "getTimesheet",
    "Comments & time",
    "A person's time logged per issue and day",
    desc="Your own timesheet; workspace admins may ask for anyone's (`user`).",
    query=[
        q("from", "first day (default: this week's Monday)", date),
        q("days", "how many days (default 7, 1–62)", {"type": "integer", "minimum": 1, "maximum": 62, "default": 7}),
        q("user", "whose timesheet (admins; default: yours)", uuid),
    ],
    res=ok(S("Timesheet")),
    err="400",
)
op(
    "get",
    "/time-report",
    "getTimeReport",
    "Comments & time",
    "Time logged in a period, grouped",
    desc="Over the issues the caller sees (private ones only for those who see them). Grouped by `group` and, inside, by `then`; or, with `format=csv`, every log as a row (Date, Person, Project, Issue, Title, Category, Billable, Hours, Minutes, Note).",
    query=[
        q("from", "first day (default: the first of this month)", date),
        q("to", "last day (default: today; at most two years after from)", date),
        q("group", "person (default), project, issue, category, day, week or month"),
        q("then", "a second grouping inside the first (same choices), or none"),
        q("projects", "project keys, comma separated (default: all the caller sees)"),
        q("subprojects", "with projects: the projects below them too", boolean),
        q("user", "only this person's time", uuid),
        q("category", "a time category's id, or none"),
        q("billable", "only billable (true) or not billable (false) time", boolean),
        q("format", "csv for a download of every log"),
    ],
    res={"200": {"description": "The report (CSV with format=csv)", "content": {"application/json": {"schema": S("TimeReport")}, "text/csv": {"schema": {"type": "string"}}}}},
    err="400",
)
op("get", "/time-categories", "listTimeCategories", "Comments & time", "The workspace's time categories", desc="Archived ones too (marked).", res=ok(arr(S("TimeCategory"))))
op("post", "/time-categories", "createTimeCategory", "Comments & time", "Add a time category (workspace admins)",
   req=body(obj({"name": d(txt, "unique, at most 60 characters"), "billable": d(boolean, "default false")}, ["name"])), res=created(B_ID))
op("patch", "/time-categories/{id}", "updateTimeCategory", "Comments & time", "Rename, or change whether it's billable or archived (workspace admins)",
   req=body(obj({"name": txt, "billable": boolean, "archived": boolean})), res=NO)
op("delete", "/time-categories/{id}", "deleteTimeCategory", "Comments & time", "Delete a time category (workspace admins)",
   desc="Its time logs lose the category and keep their billable flag. Archiving keeps it for them instead.", res=NO)
op("put", "/time-categories/order", "orderTimeCategories", "Comments & time", "Put the time categories in order (workspace admins)",
   req=body(obj({"ids": arr(uuid)}, ["ids"])), res=NO)

# ---- Attachments

op(
    "post",
    "/issues/{key}/attachments",
    "uploadAttachments",
    "Attachments",
    "Attach files to an issue",
    desc="Project members. Every multipart part with a file name is stored (names up to 200 characters); each file may be up to TASKCRAFT_MAX_UPLOAD_MB (413 otherwise). "
    "The workspace's upload rules apply (400: a blocked file type or larger than its limit), and with TASKCRAFT_CLAMAV_ADDRESS every file is scanned first: "
    "400 when it looks infected (recorded as file.infected in the audit log) or the scanner can't be reached (unless TASKCRAFT_CLAMAV_FAIL_OPEN). "
    "When one file is refused, none is stored.",
    req=multipart({"file": d(arr(binary), "one or more files (any part name works)")}, ["file"]),
    res=created(arr(S("IssueAttachment")), "Stored, in upload order"),
    err="413",
)
op(
    "get",
    "/attachments/{id}",
    "downloadAttachment",
    "Attachments",
    "Download a file",
    desc="Anyone who can see the issue. Images (not SVG), PDFs, videos and plain text are served inline with their type, everything else as `application/octet-stream` for download; always sandboxed (Content-Security-Policy).",
    res={
        "200": {
            "description": "The file",
            "headers": {"Content-Disposition": {"schema": txt, "description": "inline or attachment, with the file name"}},
            "content": {"application/octet-stream": {"schema": binary}, "*/*": {"schema": binary}},
        }
    },
)
op("delete", "/attachments/{id}", "deleteAttachment", "Attachments", "Delete a file", desc="Project members.", res=NO)

# ---- Dashboards & reports

op(
    "get",
    "/projects/{key}/reports/{report}",
    "getProjectReport",
    "Dashboards & reports",
    "A project report",
    desc="Computed from the issues' history. The shape depends on `report`: overview (OverviewReport), velocity (VelocityReport), cumulative-flow (CumulativeFlowReport), "
    "cycle-time (CycleTimeReport), created-resolved (CreatedResolvedReport), workload (WorkloadReport).",
    query=[
        q("days", "cumulative-flow, created-resolved: days back (default 30, 7–365); cycle-time: issues finished in the last N days (default 90, 7–730)", num),
        q("count", "velocity: how many completed sprints (default 8, 1–24)", num),
        q("subprojects", "overview, cycle-time, created-resolved, workload: the projects below it too (those the caller sees)", boolean),
    ],
    res=ok(
        {
            "anyOf": [S("OverviewReport"), S("VelocityReport"), S("CumulativeFlowReport"), S("CycleTimeReport"), S("CreatedResolvedReport"), S("WorkloadReport")],
            "description": "depends on the report",
        }
    ),
    err="400",
)
op(
    "get",
    "/search/groups",
    "countByGroup",
    "Dashboards & reports",
    "Count issues matching a query by group",
    desc="Issues matching a TQL query (among those the caller can see), counted per status, category, assignee, priority, type, project, label or sprint — the numbers behind breakdown charts.",
    query=[q("q", "TQL query (empty: all issues)"), q("by", "what to group by", enum("status", "category", "assignee", "priority", "type", "project", "labels", "sprint"), required=True)],
    res=ok(S("GroupCounts")),
    err="400",
)
op("get", "/dashboards", "listDashboards", "Dashboards & reports", "Your dashboards and the shared ones", desc="Your own first.", res=ok(arr(S("Dashboard"))))
op(
    "post",
    "/dashboards",
    "createDashboard",
    "Dashboards & reports",
    "Create a dashboard",
    desc="Without `widgets` it starts with a few useful ones. Up to 40 widgets; queries are checked. Guests can't share.",
    req=body(obj({"name": txt, "shared": d(boolean, "default false"), "widgets": arr(S("Widget"))}, ["name"])),
    res=created(obj({"id": uuid}, ["id"])),
)
op("get", "/dashboards/{id}", "getDashboard", "Dashboards & reports", "A dashboard", desc="Your own or a shared one.", res=ok(S("Dashboard")))
op(
    "patch",
    "/dashboards/{id}",
    "updateDashboard",
    "Dashboards & reports",
    "Change a dashboard",
    desc="Its owner or a workspace admin. Only the fields sent change; `widgets` replaces the whole list.",
    req=body(obj({"name": txt, "shared": boolean, "widgets": arr(S("Widget"))})),
    res=NO,
)
op("delete", "/dashboards/{id}", "deleteDashboard", "Dashboards & reports", "Delete a dashboard", desc="Its owner or a workspace admin.", res=NO)
op(
    "get",
    "/portfolio",
    "getPortfolio",
    "Dashboards & reports",
    "Epics, releases and team load across projects",
    desc="For the projects asked for (default: your starred projects, else all you can see; archived ones left out).",
    query=[q("projects", "comma-separated project keys")],
    res=ok(S("Portfolio")),
)

# ---- Notifications & views

op("get", "/views", "listViews", "Notifications & views", "Saved filters", desc="Your own and the shared ones of projects you can see, by name.", res=ok(arr(S("View"))))
op(
    "post",
    "/views",
    "createView",
    "Notifications & views",
    "Save a filter",
    desc="The query is checked. The new view is starred for you. Guests can't share.",
    req=body(
        obj(
            {
                "name": txt,
                "query": C_TQL,
                "projectKey": d(txt, "the project it belongs to (default: none)"),
                "layout": d(enum("list", "board", "calendar", "timeline"), "default list"),
                "config": d(anyobj, "how it is shown (list: `columns`, `group`, …)"),
                "shared": d(boolean, "default false"),
            },
            ["name"],
        )
    ),
    res=created(obj({"id": uuid}, ["id"])),
    err="404",
)
op(
    "patch",
    "/views/{id}",
    "updateView",
    "Notifications & views",
    "Change a saved filter",
    desc="Its owner or a workspace admin. Only the fields sent change.",
    req=body(obj({"name": txt, "query": C_TQL, "layout": enum("list", "board", "calendar", "timeline"), "config": anyobj, "shared": boolean})),
    res=NO,
)
op("delete", "/views/{id}", "deleteView", "Notifications & views", "Delete a saved filter", desc="Its owner or a workspace admin.", res=NO)
op(
    "put",
    "/views/{id}/star",
    "starView",
    "Notifications & views",
    "Star or unstar a saved filter",
    desc="Starred filters show in your sidebar. Unknown ids are ignored.",
    req=body(obj({"starred": d(boolean, "default true")})),
    res=NO,
    err="-404",
)
D_SUBSCRIPTION = obj(
    {
        "schedule": S("Schedule"),
        "skipEmpty": d(boolean, "no e-mail when nothing matches (default true)"),
        "nextRunAt": ndt(),
        "lastRunAt": ndt(),
    },
    ["schedule", "skipEmpty"],
)
op("get", "/views/{id}/subscription", "getViewSubscription", "Notifications & views", "Your subscription to a saved filter",
   desc="A filter you see (yours, or a shared one in a project you see). Null when you aren't subscribed.", res=ok({**D_SUBSCRIPTION, "nullable": True}))
op("put", "/views/{id}/subscription", "subscribeView", "Notifications & views", "Get a saved filter's results by e-mail on a schedule",
   desc="The issues it finds — as you'd see them, private ones only when you see them, the 50 most recently changed — are e-mailed to you in your language "
   "with a link to the filter. Needs an e-mail address on your account.",
   req=body(obj({"schedule": S("Schedule"), "skipEmpty": d(boolean, "default true")}, ["schedule"])), res=ok(D_SUBSCRIPTION))
op("delete", "/views/{id}/subscription", "unsubscribeView", "Notifications & views", "Stop getting a saved filter by e-mail", res=NO)
op(
    "get",
    "/notifications",
    "listNotifications",
    "Notifications & views",
    "Your notifications",
    desc="The latest 100, newest first, and how many are unread.",
    res=ok(obj({"unread": num, "items": arr(S("Notification"))}, ["unread", "items"])),
)
op(
    "post",
    "/notifications/read",
    "markNotificationsRead",
    "Notifications & views",
    "Mark notifications read",
    desc="`ids`, or `all: true` for every one.",
    req=body(obj({"ids": arr(uuid), "all": boolean})),
    res=NO,
)

# ==== Integrations (WikiCraft, Git), E-mail in, Service desk portal, Administration, Webhooks, Backup import ====

schemas["WikiPage"] = obj(
    {
        "id": d(uuid, "the page's id in WikiCraft"),
        "title": txt,
        "spaceKey": txt,
        "spaceName": d(txt, "empty for a page just created"),
        "url": d(uri, "<WikiCraft>/pages/<id>"),
        "updatedAt": ntxt("when it last changed (as WikiCraft reports it)"),
        "excerpt": ntxt("search results: the matching text"),
    },
    ["id", "title", "spaceKey", "spaceName", "url"],
    "A WikiCraft page as TaskCraft shows it (looked up live with the person's own WikiCraft token, else the installation's).",
)
schemas["WikiSpace"] = obj({"key": txt, "name": txt, "url": uri}, ["key", "name", "url"], "A WikiCraft space (archived ones left out).")
schemas["NewWikiPage"] = obj(
    {
        "spaceKey": d(txt, "the space to write it in (default: the project's wiki space from its settings, `wikiSpace`)"),
        "title": d(txt, "default: a title made from the issue, sprint or version"),
        "parentId": d(uuid, "the page to put it under (default: the project's `wikiParentPage` setting)"),
    },
    desc="Where a generated page goes; every field is optional.",
)
schemas["GitIntegration"] = obj(
    {
        "id": uuid,
        "provider": enum("github", "gitlab", "generic"),
        "repoUrl": d(uri, "the repository's web address (without .git)"),
        "repo": d(txt, "owner/name, for display"),
        "webhookUrl": d(uri, "where the Git server sends its webhooks (POST /integrations/git/{provider}/{secret})"),
        "createdAt": dt,
        "deliveries": d(num, "webhook deliveries received"),
        "lastEvent": ntxt("the last delivery's event (push, pull_request, Push Hook, generic, rejected, …)"),
        "lastEventAt": ndt(),
        "lastError": ntxt("what went wrong with the last delivery"),
        "secret": d(txt, "only in the answers that create it (connecting, a new secret): the webhook secret, shown once"),
    },
    ["id", "provider", "repoUrl", "repo", "webhookUrl", "createdAt", "deliveries"],
    "A repository connected to a project.",
)
schemas["GitRules"] = obj(
    {
        "mergedStatusId": nuuid("when a pull request is merged, move its issues to this status (null: off)"),
        "openedStatusId": nuuid("when a branch is pushed or a pull request opened, move its issues to this status (null: off)"),
    },
    desc="A project's Git rules. Issues only move forward in the workflow and, in restricted workflows, only along allowed transitions.",
)
schemas["GitPush"] = obj(
    {
        "commits": arr(
            obj(
                {
                    "sha": d(txt, "commit id (also accepted as `id`)"),
                    "message": txt,
                    "url": d(uri, "default: <repository>/commit/<sha>"),
                    "author": txt,
                    "timestamp": d(txt, "RFC 3339"),
                },
                ["sha"],
            ),
            "at most 200 are read",
        ),
        "branch": one(
            obj({"name": txt, "url": d(uri, "default: <repository>/tree/<name>"), "author": txt}, ["name"]),
            d(txt, "just the branch name"),
            desc="a pushed branch",
        ),
        "pullRequest": obj(
            {
                "number": num,
                "title": txt,
                "body": txt,
                "url": uri,
                "state": d(txt, "open (default), merged or closed (declined / rejected count as closed)"),
                "author": txt,
                "branch": d(txt, "source branch"),
                "updatedAt": d(txt, "RFC 3339 (also accepted as `timestamp`)"),
            },
            ["url"],
            "a pull / merge request (also accepted as `pull_request`)",
        ),
    },
    desc="The generic delivery format for Git servers without a built-in format (provider `generic`); every part is optional. "
    "Issue keys are found in commit messages, branch names, pull request titles, descriptions and source branches.",
)
schemas["DevelopmentLink"] = obj(
    {
        "id": uuid,
        "kind": enum("commit", "branch", "pull_request"),
        "title": d(txt, "commit message, branch name or pull request title"),
        "url": uri,
        "author": ntxt(),
        "state": null(enum("open", "merged", "closed"), "pull requests: their state"),
        "sha": ntxt("commits"),
        "branch": ntxt(),
        "number": null(num, "pull requests: their number"),
        "repo": ntxt("owner/name"),
        "at": dt,
    },
    ["id", "kind", "title", "url", "at"],
    "A commit, branch or pull request linked to an issue by the Git integration.",
)
schemas["MailInResult"] = obj(
    {
        "status": enum("created", "commented", "ignored", "rejected", "duplicate", desc="what became of the message"),
        "reason": d(
            txt,
            "ignored / rejected / duplicate: why — auto-reply, bulk, bounce, loop, empty, too-large, no-sender, forged, rate-limit, no-project, not-allowed, archived, invalid, unreadable, failing, duplicate",
        ),
        "detail": d(txt, "more about it (e.g. the addresses no project takes, notes about skipped files)"),
        "issue": d(txt, "created / commented: the issue's key"),
    },
    ["status"],
    "What became of a message handed over to e-mail in.",
)
schemas["MailInSettings"] = obj(
    {
        "enabled": d(boolean, "the project takes e-mail"),
        "address": d(email, "its own address (lower case); mail to `+KEY` addresses of the mailbox works without one"),
        "senders": enum("members", "anyone", desc="members: people with an account and access to the project; anyone: unknown senders become requesters"),
    },
    ["enabled", "senders"],
    "A project's e-mail settings.",
)
schemas["MailInMessage"] = obj(
    {
        "id": uuid,
        "receivedAt": dt,
        "source": enum("imap", "http", desc="read from the mailbox, or handed over by a mail server"),
        "from": txt,
        "fromName": ntxt(),
        "to": ntxt("the project address it came in at"),
        "subject": txt,
        "status": enum("processing", "created", "commented", "ignored", "rejected", "duplicate", "failed"),
        "reason": ntxt("see MailInResult"),
        "detail": ntxt(),
        "issueKey": ntxt("the issue it created or commented on"),
        "routed": d(boolean, "it reached a project (workspace admins also see messages that reached none)"),
    },
    ["id", "receivedAt", "source", "from", "subject", "status", "routed"],
    "A received message in the e-mail log (kept 90 days).",
)
schemas["MailInOverview"] = obj(
    {
        "settings": S("MailInSettings"),
        "imap": null(
            obj(
                {
                    "mailbox": d(txt, "user@host:port/mailbox (security), no password"),
                    "checkedAt": ndt("last time it was read"),
                    "okAt": ndt("last time reading it worked"),
                    "error": ntxt("why the last read failed"),
                }
            ),
            "the mailbox TaskCraft reads (TASKCRAFT_IMAP_URL), null when none is set up",
        ),
        "http": d(boolean, "mail servers can hand messages over (TASKCRAFT_MAIL_IN_TOKEN is set)"),
        "endpoint": d(uri, "the address mail servers post messages to"),
        "smtp": d(boolean, "outgoing e-mail works (acknowledgements, replies to requesters)"),
        "plusAddress": ntxt("the mailbox's +KEY address for this project, when the mailbox's user name is an e-mail address"),
        "messages": arr(S("MailInMessage"), "the latest 50 messages"),
    },
    ["settings", "imap", "http", "endpoint", "smtp", "plusAddress", "messages"],
    "A project's e-mail settings, how this server receives mail and the latest messages.",
)

D_PORTAL_STATE = enum("open", "in_progress", "resolved", desc="the status in plain words")
D_SHOW = enum("off", "optional", "required", desc="whether the form asks for it, and whether it must be filled")

schemas["PortalRequest"] = obj(
    {
        "key": d(txt, "the issue key"),
        "title": txt,
        "project": d(txt, "the project's name"),
        "state": D_PORTAL_STATE,
        "createdAt": dt,
        "updatedAt": d(dt, "the last change the requester can see (status, a comment for them)"),
        "resolvedAt": ndt(),
    },
    ["key", "title", "project", "state", "createdAt", "updatedAt"],
    "A request as its requester sees it in the portal.",
)
schemas["PortalMessage"] = obj(
    {
        "id": uuid,
        "mine": d(boolean, "written by the requester"),
        "author": ntxt("the team member's name (null for the requester's own)"),
        "body": d(S("PmNode"), "the text, with mentions and issue references as plain text and no images"),
        "createdAt": dt,
        "edited": boolean,
    },
    ["id", "mine", "body", "createdAt", "edited"],
    "A message of the conversation: the requester's own and the comments marked visible to the requester (never internal ones).",
)
schemas["PortalFile"] = obj(
    {"id": uuid, "name": txt, "mime": txt, "size": d(num, "bytes"), "createdAt": dt, "url": d(txt, "/api/portal/attachments/{id}")},
    ["id", "name", "mime", "size", "createdAt", "url"],
    "A file the requester added.",
)
schemas["PortalRequestDetail"] = {
    "description": "A request with its description, conversation and the requester's files.",
    "allOf": [
        S("PortalRequest"),
        obj(
            {
                "description": null(S("PmNode"), "as the requester sees it (mentions as plain text, no images)"),
                "messages": arr(S("PortalMessage")),
                "files": arr(S("PortalFile")),
                "canReply": d(boolean, "the project isn't archived"),
                "canResolve": d(boolean, "the project lets requesters mark requests resolved, and it isn't resolved yet"),
                "limits": obj(
                    {"files": d(num, "files per message"), "mb": d(num, "MB of files per message, together"), "text": d(num, "characters per message")},
                    ["files", "mb", "text"],
                ),
            },
            ["description", "messages", "files", "canReply", "canResolve", "limits"],
        ),
    ],
}
schemas["RequestFormSettings"] = obj(
    {
        "enabled": d(boolean, "the form is published"),
        "slug": d(txt, "its address instead of the project key (/portal/forms/<slug>): 2–40 letters a–z, digits and dashes, unique"),
        "suggestSpace": ntxt("a WikiCraft space whose pages are suggested while requesters type the summary (one they can read without signing in)"),
        "listed": d(boolean, "shown on the portal's start page (default true)"),
        "title": d(txt, "up to 200 characters (empty: the portal's own)"),
        "intro": d(txt, "shown above the fields, plain text, up to 5,000 characters"),
        "success": d(txt, "shown after sending, up to 2,000 characters"),
        "description": D_SHOW,
        "name": D_SHOW,
        "types": arr(uuid, "standard issue types to choose from (none or one: no choice)"),
        "typeId": d(uuid, "the type when there's no choice (else the project's first)"),
        "priority": d(boolean, "ask for a priority (low, medium, high, urgent)"),
        "custom": arr(obj({"id": d(uuid, "a custom field of the project (not a people field)"), "required": boolean}, ["id"]), "custom fields, in order"),
    },
    desc="A project's public request form. Texts left empty show the portal's own (translated) ones.",
)
schemas["PortalSettings"] = obj(
    {
        "resolve": d(boolean, "requesters may mark their requests resolved"),
        "resolveStatus": d(uuid, "the done status resolved requests move to (default: the first done status)"),
        "form": S("RequestFormSettings"),
    },
    desc="A project's service desk settings.",
)
schemas["RequestForm"] = obj(
    {
        "path": d(txt, "where the form lives: its slug, else the project key in lower case"),
        "project": d(txt, "the project's name"),
        "title": txt,
        "intro": txt,
        "success": d(txt, "the message after sending"),
        "description": D_SHOW,
        "name": D_SHOW,
        "types": arr(S("Named"), "issue types to choose from (only when there are several)"),
        "priority": d(boolean, "the form asks for a priority"),
        "suggest": d(boolean, "articles are suggested while the summary is typed (see suggestArticles)"),
        "fields": arr(
            obj(
                {
                    "id": uuid,
                    "name": txt,
                    "kind": enum("text", "number", "select", "multiselect", "date", "checkbox", "url"),
                    "description": ntxt(),
                    "required": boolean,
                    "options": arr(obj({"id": txt, "label": txt}, ["id", "label"]), "select / multiselect choices"),
                },
                ["id", "name", "kind", "required", "options"],
            )
        ),
        "stamp": d(txt, "a signed time stamp of when the form was opened: send it back with the submission (valid for a day)"),
        "minSeconds": d(num, "submissions sooner than this after opening are refused"),
    },
    ["path", "project", "title", "intro", "success", "description", "name", "types", "priority", "fields", "stamp", "minSeconds"],
    "A public request form.",
)
schemas["RequestSubmission"] = obj(
    {
        "stamp": d(txt, "the form's `stamp`"),
        "email": d(email, "the sender's address (ignored for requesters signed in to the portal: theirs is used)"),
        "name": d(txt, "up to 100 characters, when the form asks for it"),
        "summary": d(txt, "the request's title: required, up to 200 characters"),
        "description": d(txt, "plain text, up to 20,000 characters, when the form asks for it"),
        "typeId": d(txt, "one of the form's types, when it offers several"),
        "priority": enum("low", "medium", "high", "urgent", desc="when the form asks for one"),
        "custom": d({"type": "object", "additionalProperties": anyval}, "values of the form's custom fields by field id (as for issues; each up to 4,000 characters)"),
        "website": d(txt, "a hidden field people never see: leave it empty (submissions that fill it are answered as if they worked, and dropped)"),
    },
    ["stamp", "summary"],
    "A request form submission.",
)
D_HOOK_FORMAT = d(
    enum("json", "slack", "teams", "google-chat"),
    "json (default): the signed event (WebhookEvent); slack (also Mattermost and other Slack-compatible incoming webhooks), teams (an adaptive card) or google-chat: "
    "a readable message in the workspace's language for a chat channel's incoming-webhook address — nothing about private issues or restricted comments",
)
schemas["Webhook"] = obj(
    {
        "id": uuid,
        "projectId": nuuid("only this project's events (null: all projects)"),
        "projectKey": ntxt(),
        "url": uri,
        "events": arr(txt, "the events it gets (empty or `*`: all)"),
        "active": boolean,
        "lastStatus": null(num, "HTTP status of the last delivery"),
        "lastError": ntxt(),
        "lastSentAt": ndt(),
        "format": D_HOOK_FORMAT,
    },
    ["id", "url", "events", "active"],
    "An outgoing webhook (the secret is shown only when it's created).",
)
schemas["AuditEntry"] = obj(
    {
        "id": uuid,
        "actor": ntxt("who did it (name)"),
        "action": d(txt, "what happened: workspace.updated, webhook.created, git.connected, project.mail_in, backup.uploaded, …"),
        "target": ntxt("what it happened to (a project key, an address, …)"),
        "data": d(anyval, "details"),
        "at": dt,
    },
    ["id", "action", "at"],
)
D_COUNT = obj({"imported": num, "existing": d(num, "already there from an earlier import")}, ["imported", "existing"])
D_WARNING = obj(
    {
        "code": d(txt, "what happened (missing-file, link-type, …)"),
        "message": d(txt, "in English"),
        "params": {"type": "object", "additionalProperties": txt},
        "count": d(num, "how often"),
    },
    ["code", "message", "params", "count"],
)
D_API_SOURCE = enum("github", "gitlab", "redmine", desc="the tracker whose API is read")
D_IMPORT_SOURCE = enum("xml-site-backup", "taskcraft", "github", "gitlab", "redmine", desc="where the import comes from (taskcraft: a project export of another TaskCraft)")
schemas["BackupSummary"] = obj(
    {
        "source": D_IMPORT_SOURCE,
        "origin": d(txt, "the source site's id (or a fingerprint of its projects)"),
        "projects": arr(
            obj(
                {
                    "key": d(txt, "the project's key in the source (or its path, owner/repo)"),
                    "suggestedKey": d(txt, "a key that works here, proposed for a new project"),
                    "name": txt,
                    "formerKeys": arr(txt),
                    "parent": ntxt("the source project's parent (subprojects)"),
                    "issues": num,
                    "comments": num,
                    "worklogs": num,
                    "attachments": num,
                    "missingFiles": d(num, "attachments without their file in the backup"),
                    "links": num,
                    "sprints": num,
                    "history": num,
                    "confidential": d(num, "issues only some could see in the source"),
                    "existing": null(obj({"key": txt, "name": txt, "issues": num}, ["key", "name", "issues"]), "a project with the same key here"),
                    "imported": d(num, "issues an earlier import of this backup brought in already"),
                    "validKey": d(boolean, "the key works here (2–10 letters or digits, starting with a letter)"),
                },
                ["key", "name", "issues"],
            )
        ),
        "people": obj(
            {
                "total": d(num, "people the projects' work refers to"),
                "matched": d(num, "of them, people with an account here (same e-mail address or name)"),
                "active": num,
                "inactive": num,
            },
            ["total", "matched", "active", "inactive"],
        ),
        "files": d(num, "attachment files the source holds"),
        "hasSprints": boolean,
        "markup": null(enum("textile", "markdown"), "sources whose markup is a server setting: how the texts are read (found from them)"),
        "warnings": arr(D_WARNING),
    },
    ["origin", "projects", "people", "files", "hasSprints", "warnings"],
    "What an uploaded backup or a source read through its API holds, for the choices before importing.",
)
schemas["BackupImportReport"] = obj(
    {
        "dryRun": boolean,
        "origin": txt,
        "people": d({"type": "object", "additionalProperties": num}, "people by outcome: matched, invited, deactivated, skipped"),
        "projects": arr(
            obj(
                {
                    "source": d(txt, "the key in the backup"),
                    "key": d(txt, "the key here"),
                    "name": txt,
                    "id": nuuid(),
                    "created": d(boolean, "made by this import (else imported into an existing project)"),
                    "skipped": boolean,
                    "parent": ntxt("the source project's parent (subprojects)"),
                    "counts": d(
                        {"type": "object", "additionalProperties": D_COUNT},
                        "by kind: issues, comments, worklogs, attachments, links, parents, watchers, labels, history, components, versions, sprints, statuses, types, fields, confidential",
                    ),
                },
                ["source", "key", "name", "created", "skipped", "counts"],
            )
        ),
        "warnings": arr(D_WARNING),
    },
    ["dryRun", "origin", "people", "projects", "warnings"],
    "What an import brought in (or, in a dry run, would bring in).",
)
D_CHOICE = one(
    obj({"action": enum("new", desc="a new project with this key (refused when it's taken)"), "key": txt}, ["action", "key"]),
    obj({"action": enum("into", desc="into this existing project"), "key": txt}, ["action", "key"]),
    obj({"action": enum("auto", desc="into the project with this key: an existing one, else a new one"), "key": txt}, ["action", "key"]),
    obj({"action": enum("skip", desc="leave the project out")}, ["action"]),
    desc="what to do with one project of the backup",
)
D_PEOPLE = enum("invite", "deactivated", "skip", desc="people without an account here: invited accounts (nobody is e-mailed; people inactive in the backup get deactivated ones), deactivated accounts, or no accounts")
schemas["BackupImportRun"] = obj(
    {
        "projects": d({"type": "object", "additionalProperties": D_CHOICE}, "per project key of the backup; projects not named go to the same key"),
        "people": D_PEOPLE,
        "dryRun": d(boolean, "run the whole import and roll it back: nothing changes"),
        "timeZone": d(txt, "IANA time zone the backup's times are written in (default UTC)"),
        "token": d(txt, "API sources: the token again (attachments are downloaded while importing; required when the connection had one, except for dry runs). Never stored."),
        "markup": enum("textile", "markdown", desc="API sources whose markup is a server setting: how texts are written (default: what the check found)"),
    },
    desc="The choices for an import.",
)
schemas["ImportConnection"] = obj(
    {
        "source": D_API_SOURCE,
        "baseUrl": d(txt, "the server's address (empty: github.com or gitlab.com; else the Enterprise or self-managed server; required for Redmine)"),
        "token": d(txt, "a personal access token or API key with read access (Redmine: an administrator's key also brings watchers, e-mail addresses and custom field definitions); used for this request only, never stored"),
        "projects": arr(txt, "repositories or projects to read: owner/repo, group/project or identifiers (1 to 50; Redmine projects bring their subprojects)"),
        "markup": enum("textile", "markdown", desc="how texts are written, when the server doesn't say"),
    },
    ["source", "projects"],
    "Another tracker's API to read projects from.",
)
schemas["BackupImport"] = obj(
    {
        "id": uuid,
        "source": D_IMPORT_SOURCE,
        "connection": null(
            obj({"baseUrl": txt, "projects": arr(txt), "token": d(boolean, "a token was given (it isn't kept)"), "markup": ntxt()}),
            "API sources: what was asked for",
        ),
        "fileName": d(txt, "the backup's file name, or the source and its projects"),
        "size": d(num, "bytes"),
        "status": enum(
            "checking", "checked", "running", "done", "failed", desc="checking: being read after the upload; checked: ready to import; running: importing; done; failed (see error)"
        ),
        "summary": null(S("BackupSummary"), "after the check"),
        "options": null(
            obj({"projects": {"type": "object", "additionalProperties": D_CHOICE}, "people": D_PEOPLE, "dryRun": boolean, "timeZone": txt}),
            "the choices of the last run",
        ),
        "progress": null(
            obj(
                {
                    "phase": enum("reading", "fetching", "waiting", "people", "issues", "links", "saving"),
                    "project": ntxt("the project being read or imported"),
                    "done": num,
                    "total": num,
                    "until": d(dt, "waiting: when the source's rate limit lets requests through again"),
                },
                ["phase", "done", "total"],
            ),
            "while checking or running",
        ),
        "report": null(S("BackupImportReport"), "after a run"),
        "error": ntxt("why it failed"),
        "fileKept": d(boolean, "the uploaded file (or what was read) is still there (removed after a real import, else after a day)"),
        "createdAt": dt,
        "updatedAt": dt,
        "finishedAt": ndt(),
        "expiresAt": d(dt, "when the uploaded file is removed at the latest"),
    },
    ["id", "fileName", "size", "status", "summary", "options", "progress", "report", "error", "fileKept", "createdAt", "updatedAt", "finishedAt", "expiresAt"],
    "An upload of another tracker's XML site backup, or a connection to its API, and the import.",
)

# ---- Integrations (WikiCraft, Git) ----

D_WIKI_NOTE = "Needs WikiCraft (TASKCRAFT_WIKICRAFT_URL and a token: the person's own, else TASKCRAFT_WIKICRAFT_TOKEN); 400 when it isn't connected or can't be reached."
op("get", "/integrations/wikicraft", "getWikiCraftStatus", "Integrations (WikiCraft, Git)", "Whether WikiCraft is connected, and with whose token",
   res=ok(obj({"url": ntxt("WikiCraft's address (TASKCRAFT_WIKICRAFT_URL)"), "connected": d(boolean, "a token is available"), "personal": d(boolean, "the person connected their own token")}, ["url", "connected", "personal"])))
op("get", "/integrations/wikicraft/search", "searchWikiCraft", "Integrations (WikiCraft, Git)", "Search WikiCraft pages",
   desc="Full-text search over the pages the token may see (15 at most). A page address (`url`, or `q` starting with http) is looked up directly; "
   "an empty `q` with a `space` lists that space's recently updated pages. " + D_WIKI_NOTE,
   query=[q("q", "search words, or a page address"), q("space", "only this space (key)"), q("url", "look up this page address instead")],
   res=ok(arr(S("WikiPage"))), err="400 404")
op("get", "/integrations/wikicraft/spaces", "listWikiCraftSpaces", "Integrations (WikiCraft, Git)", "WikiCraft spaces the token can see",
   desc=D_WIKI_NOTE, res=ok(arr(S("WikiSpace"))), err="400")
op("get", "/projects/{key}/wiki", "getProjectWiki", "Integrations (WikiCraft, Git)", "The project's wiki space and its recently updated pages",
   desc="The space comes from the project's settings (`wikiSpace`). Without a WikiCraft connection: `connected: false` and no pages.",
   res=ok(obj({"connected": boolean, "space": ntxt("the space key"), "spaceUrl": d(uri, "when connected and a space is set"), "pages": arr(S("WikiPage"), "up to 12, latest first")}, ["connected", "space", "pages"])))
op("get", "/issues/{key}/wiki-pages", "listIssueWikiPages", "Integrations (WikiCraft, Git)", "The issue's wiki pages with their current titles",
   desc="Linked WikiCraft pages (web links of kind wiki), looked up live; pages the person can't open show with their stored title and `available: false`.",
   res=ok(arr(obj({"id": d(uuid, "the web link's id (DELETE /web-links/{id} removes it)"), "url": uri, "title": txt, "page": null(S("WikiPage"), "the live page, if it can be opened"), "available": boolean}, ["id", "url", "title", "page", "available"]))))
op("post", "/issues/{key}/wiki-pages", "linkIssueWikiPage", "Integrations (WikiCraft, Git)", "Link an existing WikiCraft page to the issue",
   desc="Project members. A page linked already isn't linked twice. " + D_WIKI_NOTE,
   req=body(obj({"url": d(uri, "the page's address (/pages/<id> or /wiki/<SPACE>/<name>)")}, ["url"])), res=created(S("WikiPage"), "Linked: the page"))
op("post", "/issues/{key}/wiki-page", "createIssueWikiPage", "Integrations (WikiCraft, Git)", "Write a specification page for the issue and link it",
   desc="Project members. The page starts from the issue (key, type, status, description as Markdown) with sections to fill in. " + D_WIKI_NOTE,
   req=body(S("NewWikiPage")), res=created(S("WikiPage"), "Written and linked"))
op("post", "/sprints/{id}/wiki-page", "createSprintWikiPage", "Integrations (WikiCraft, Git)", "Write a sprint review page",
   desc="Project members. Goal, dates, committed and completed work, scope changes, finished and unfinished issues, and retrospective sections. " + D_WIKI_NOTE,
   pp={"id": (uuid, "sprint id")}, req=body(S("NewWikiPage")), res=created(S("WikiPage"), "Written"))
op("post", "/versions/{id}/wiki-page", "createVersionWikiPage", "Integrations (WikiCraft, Git)", "Write release notes for a version",
   desc="Project members. Done issues grouped by type, the others under known issues. " + D_WIKI_NOTE,
   pp={"id": (uuid, "version id")}, req=body(S("NewWikiPage")), res=created(S("WikiPage"), "Written"))
op("get", "/wiki-links", "listIssuesLinkingPage", "Integrations (WikiCraft, Git)", "Issues that link a wiki page",
   desc="WikiCraft asks this to show a page's linked issues. `/pages/<id>` addresses match by the page id, whatever host they were linked with; other addresses match exactly. Only issues the caller can see.",
   query=[q("url", "the page's address", required=True)], res=ok(arr(S("IssueSummary"))), err="400")

op("post", "/integrations/git/{provider}/{secret}", "receiveGitWebhook", "Integrations (WikiCraft, Git)", "Receive a webhook delivery from a Git server",
   desc="The address comes from connecting a repository (`webhookUrl`). GitHub (also Gitea, Forgejo): `push` and `pull_request` events, signed with X-Hub-Signature-256. "
   "GitLab: Push and Merge request events with X-Gitlab-Token. generic: the GitPush format with X-TaskCraft-Token (or `Authorization: Bearer <secret>`) or X-Hub-Signature-256. "
   "Issue keys of the repository's project (also former keys) become development links; the project's Git rules may move the issues. "
   "Events with nothing to link (ping, tags, deleted branches, others) are answered with `ignored`.",
   auth="public",
   pp={"provider": (enum("github", "gitlab", "generic"), "the kind of Git server"),
       "secret": (txt, "the integration's address key (a random value in its webhook address, replaced with each new secret)")},
   headers=[hdr("X-Hub-Signature-256", "github (and optionally generic): `sha256=<hex HMAC-SHA256 of the raw body with the secret>`"),
            hdr("X-Gitlab-Token", "gitlab: the secret"),
            hdr("X-TaskCraft-Token", "generic: the secret"),
            hdr("X-GitHub-Event", "github: the event (push, pull_request, …; X-Gitea-Event and X-Forgejo-Event work too)"),
            hdr("X-Gitlab-Event", "gitlab: the event (Push Hook, Merge Request Hook)")],
   req=body(one(S("GitPush"), d(anyobj, "a GitHub or GitLab webhook payload"))),
   res=ok(one(
       obj({"ok": boolean, "event": txt, "issues": arr(txt, "keys of issues that got a new link or a changed pull request"), "newLinks": num, "moved": arr(txt, "keys of issues the project's rules moved")}, ["ok", "event", "issues", "newLinks", "moved"]),
       obj({"ok": boolean, "ignored": d(txt, "the event, which carried nothing to link")}, ["ok", "ignored"]),
   ), "Handled"),
   err="401")
op("get", "/projects/{key}/git", "listGitIntegrations", "Integrations (WikiCraft, Git)", "Connected repositories and the Git rules",
   desc="Project admins.", res=ok(obj({"integrations": arr(S("GitIntegration")), "rules": S("GitRules")}, ["integrations", "rules"])))
op("post", "/projects/{key}/git", "connectGitRepository", "Integrations (WikiCraft, Git)", "Connect a repository",
   desc="Project admins; up to 50 repositories per project. The answer carries the webhook address and the secret, once.",
   req=body(obj({"provider": enum("github", "gitlab", "generic"), "repoUrl": d(uri, "the repository's web address (https://…)")}, ["provider", "repoUrl"])),
   res=created(S("GitIntegration"), "Connected (with `secret`)"))
op("put", "/projects/{key}/git/rules", "setGitRules", "Integrations (WikiCraft, Git)", "Set the Git rules (which status linked issues move to)",
   desc="Project admins. Statuses must belong to the project; null turns a rule off.", req=body(S("GitRules")), res=NO)
op("delete", "/git-integrations/{id}", "deleteGitIntegration", "Integrations (WikiCraft, Git)", "Disconnect a repository",
   desc="Project admins. The development links it made stay on the issues.", pp={"id": (uuid, "integration id")}, res=NO)
op("post", "/git-integrations/{id}/secret", "rotateGitSecret", "Integrations (WikiCraft, Git)", "Make a new secret (and webhook address)",
   desc="Project admins. The old secret and address stop working; the answer shows the new secret once.", pp={"id": (uuid, "integration id")},
   res=ok(S("GitIntegration"), "The integration with its new `secret` and `webhookUrl`"))
op("get", "/issues/{key}/development", "getIssueDevelopment", "Integrations (WikiCraft, Git)", "The issue's branches, commits and pull requests",
   desc="Pull requests first, then branches, then commits (newest first, at most 300), whether the project has a repository connected, and a branch name to start work with.",
   res=ok(obj({"links": arr(S("DevelopmentLink")), "connected": boolean, "branchName": d(txt, "e.g. web-12-short-title")}, ["links", "connected", "branchName"])))

# ---- E-mail in ----

op("post", "/integrations/mail-in", "receiveMail", "E-mail in", "Hand over one received e-mail",
   desc="For mail servers and services (TASKCRAFT_MAIL_IN_TOKEN; off, 404, without one). The body is one raw RFC 822 message, or a multipart form with the message in the field "
   "`email` (also `body-mime`, `message` or `mime`). Answers 200 with what became of it — also when it was ignored or refused, so nothing bounces back to senders — and 503 "
   "when it should be sent again later. Messages over 40 MB are refused (`too-large`); at most 30 messages per sender and hour.",
   auth="mailin",
   req={"required": True, "content": {
       "message/rfc822": {"schema": binary},
       "multipart/form-data": {"schema": obj({"email": d(binary, "the raw message (or `body-mime`, `message`, `mime`)"), "body-mime": binary})},
       "application/octet-stream": {"schema": d(binary, "any other content type: the body is the message")},
   }},
   res=ok(S("MailInResult"), "What became of the message"), err="401 404 503")
op("get", "/projects/{key}/mail-in", "getMailInSettings", "E-mail in", "The project's e-mail settings and latest messages",
   desc="Project admins (workspace admins also see messages that reached no project).", res=ok(S("MailInOverview")))
op("put", "/projects/{key}/mail-in", "setMailInSettings", "E-mail in", "Change the project's e-mail settings",
   desc="Project admins. An address can't be TaskCraft's own sender address, and belongs to one project only (409).",
   req=body(obj({"enabled": boolean, "address": null(txt, "its own address (empty or null: none)"), "senders": enum("members", "anyone", desc="default members")}, ["enabled"])),
   res=ok(S("MailInOverview")), err="409")

# ---- Service desk portal ----

D_PORTAL_NOTE = "Needs a portal session (cookie tc_portal, from POST /portal/session); 401 without one."
op("get", "/portal", "getPortal", "Service desk portal", "The portal's start: workspace, who is signed in, request forms",
   auth="portal-optional",
   res=ok(obj({
       "workspace": obj({"name": txt, "accentColor": ntxt()}, ["name"]),
       "email": ntxt("the address signed in to the portal"),
       "signIn": d(boolean, "signing in works (it needs outgoing e-mail)"),
       "forms": arr(obj({"path": d(txt, "/portal/forms/<path>"), "title": txt, "project": d(txt, "the project's name")}, ["path", "title", "project"]), "published forms listed on the start page"),
   }, ["workspace", "email", "signIn", "forms"])))
op("post", "/portal/sign-in", "requestPortalSignIn", "Service desk portal", "E-mail a sign-in link to a requester",
   desc="Answers 204 whether or not the address has requests; the link (valid 30 minutes, once) goes out in the background, only to addresses with requests, at most 3 per address "
   "and quarter hour. Each IP address may ask 10 times per quarter hour (429). 400 when outgoing e-mail isn't set up.",
   auth="public",
   req=body(obj({"email": email, "request": d(txt, "the request (issue key) the link should lead to")}, ["email"])), res=NO, err="429")
op("post", "/portal/session", "startPortalSession", "Service desk portal", "Sign in to the portal with a link's token",
   desc="The token works once. Sets the portal session cookie (`tc_portal`, 7 days, sent only to /api/portal); it never signs anyone in to TaskCraft itself.",
   auth="public",
   req=body(obj({"token": d(txt, "the token from the e-mailed link")}, ["token"])),
   res=ok(obj({"email": email}, ["email"]), "Signed in", headers={"Set-Cookie": {"schema": txt, "description": "the portal session (tc_portal)"}}))
op("delete", "/portal/session", "endPortalSession", "Service desk portal", "Sign out of the portal",
   auth="portal-optional", res={"204": {"description": "Signed out", "headers": {"Set-Cookie": {"schema": txt, "description": "clears tc_portal"}}}})
op("get", "/portal/requests", "listPortalRequests", "Service desk portal", "The signed-in requester's requests, latest first",
   desc="Requests opened for the address in every project (at most 500). " + D_PORTAL_NOTE, auth="portal",
   res=ok(obj({"email": email, "requests": arr(S("PortalRequest"))}, ["email", "requests"])), err="401")
op("get", "/portal/requests/{key}", "getPortalRequest", "Service desk portal", "A request as its requester sees it",
   desc="The description, the conversation (their own messages and the comments marked for them, never internal ones) and their files. "
   "Requests of other addresses answer 404 like ones that don't exist. " + D_PORTAL_NOTE, auth="portal",
   res=ok(S("PortalRequestDetail")), err="401")
op("post", "/portal/requests/{key}/comments", "replyToPortalRequest", "Service desk portal", "Answer a request (text and files)",
   desc="A comment by the requester, like an answer by e-mail: watchers are told, the service-level clock keeps running. Up to 5 files per message, 20 MB together "
   "(or TASKCRAFT_MAX_UPLOAD_MB if smaller; 413), text up to 20,000 characters, 30 messages per hour (429). Not for requests of archived projects. " + D_PORTAL_NOTE,
   auth="portal",
   req=multipart({"text": d(txt, "the message (plain text)"), "files": arr(binary, "files (repeat the field)")}, desc="A message, files, or both."),
   res=created(obj({"id": d(uuid, "the comment's id")}, ["id"])), err="401 413 429")
op("post", "/portal/requests/{key}/resolve", "resolvePortalRequest", "Service desk portal", "Mark a request resolved",
   desc="Where the project lets requesters (else 403): the request moves to the chosen done status (else the first one) in their name, with an optional last word as their comment. "
   "A resolved request answers `resolved` again. " + D_PORTAL_NOTE,
   auth="portal",
   req=body(obj({"text": d(txt, "a last word (up to 20,000 characters)")})),
   res=ok(obj({"state": D_PORTAL_STATE}, ["state"])), err="401 403")
op("get", "/portal/attachments/{id}", "downloadPortalAttachment", "Service desk portal", "Download one of the requester's own files",
   desc="Images, PDFs, videos and plain text inline, everything else as a download. " + D_PORTAL_NOTE, auth="portal",
   pp={"id": (uuid, "file id")}, res=raw("application/octet-stream", "The file"), err="401")
op("get", "/portal/forms/{path}", "getRequestForm", "Service desk portal", "A public request form",
   desc="Published forms of projects that aren't archived; 404 for others. Carries a signed `stamp` to send back.",
   auth="public", pp={"path": (txt, "the form's slug, or the project key (also a former one)")}, res=ok(S("RequestForm")))
op("get", "/portal/forms/{path}/suggest", "suggestArticles", "Service desk portal", "Articles that might answer a request",
   desc="Up to five pages of the form's WikiCraft space (`suggestSpace`) matching `q`, read with the installation's WikiCraft token; an empty list when the form suggests nothing, "
   "`q` is shorter than 3 characters, WikiCraft doesn't answer, or an address asks more than 60 times a minute.",
   auth="public", pp={"path": (txt, "the form's slug, or the project key (also a former one)")}, query=[q("q", "what the requester typed so far")],
   res=ok(arr(obj({"title": txt, "url": uri, "excerpt": ntxt("the matching text")}, ["title", "url"]))))
op("post", "/portal/forms/{path}", "submitRequestForm", "Service desk portal", "Send a request form",
   desc="Opens an issue with the sender as its requester, who gets an acknowledgement with a link to the request in the portal. Requesters signed in to the portal send it "
   "under their own address and learn the new key (`request`); for others it is null. Kept free of spam by the hidden `website` field, a minimum fill time of 3 seconds "
   "(the stamp, valid for a day), 20 requests per IP address and 5 per e-mail address and hour (429) and size limits.",
   auth="portal-optional", pp={"path": (txt, "the form's slug, or the project key (also a former one)")},
   req=body(S("RequestSubmission")),
   res=created(obj({"ok": boolean, "request": ntxt("the new request's key (only for requesters signed in to the portal)")}, ["ok", "request"]), "Sent"),
   err="429")
op("get", "/projects/{key}/portal", "getPortalSettings", "Service desk portal", "The project's service desk settings",
   desc="Project admins.",
   res=ok(obj({"formUrl": d(uri, "the form's public address"), "portalUrl": uri, "mail": d(boolean, "outgoing e-mail works (the portal needs it)"), "settings": S("PortalSettings")}, ["formUrl", "portalUrl", "mail", "settings"])))
op("put", "/projects/{key}/portal", "setPortalSettings", "Service desk portal", "Change the project's service desk settings and request form",
   desc="Project admins. Forms offer the project's standard issue types and its custom fields except people fields; the resolve status must be a done status of the project; "
   "a slug can't be another project's key or slug (409).",
   req=body(S("PortalSettings")),
   res=ok(obj({"formUrl": uri, "portalUrl": uri, "mail": boolean, "settings": S("PortalSettings")}, ["formUrl", "portalUrl", "mail", "settings"])), err="409")

# ---- Administration ----

op("put", "/admin/workspace", "updateWorkspace", "Administration", "Rename the workspace and set its look",
   desc="Workspace admins. Only the fields sent change.",
   req=body(obj({
       "name": d(txt, "the workspace's name (empty: unchanged)"),
       "accentColor": null(txt, "#rrggbb (null: the built-in colour)"),
       "defaultLanguage": null(txt, "interface language for people who haven't picked one: en, de, fr or it (null: the browser's)"),
       "weekStart": null(enum("monday", "sunday"), "first day of the week in calendars (default monday)"),
       "retention": obj(
           {
               "trashDays": null(num, "deleted issues (with their files) go for good after this many days"),
               "auditDays": null(num, "audit log entries"),
               "mailDays": null(num, "sent and received e-mail logs (default 30 and 90)"),
               "notificationDays": null(num, "read notifications"),
           },
           [],
           "how long things are kept, 1–3650 days each; null or left out: kept (applied once an hour)",
       ),
       "timeZone": null(txt, "default time zone (IANA, e.g. Europe/Zurich) for people who haven't picked one: times on screen, digests at 8:00"),
       "dateFormat": null(enum("auto", "dmy", "mdy", "ymd"), "how dates are written (auto or null: as the language writes them)"),
       "announcement": null(obj({"text": d(txt, "up to 500 characters (empty: taken down)"), "tone": enum("info", "warning"), "until": null(dt, "shown until (null: until taken down)")}, ["text"]),
                            "a message at the top of every page (null: taken down)"),
       "features": d(obj({"timeTracking": boolean, "votes": boolean, "portal": boolean, "automation": boolean}),
                     "false turns a part off: it's hidden and the API refuses it (400; the portal answers 404); left out: on"),
       "uploads": obj(
           {
               "blockedTypes": d(arr(txt), "file endings that can't be uploaded (exe, bat, …; a leading dot and case don't matter)"),
               "maxMb": null(num, "largest file in megabytes, 1 to TASKCRAFT_MAX_UPLOAD_MB (null: the server's limit)"),
           },
           [],
           "what may be uploaded to issues from the app, the portal and e-mail",
       ),
   })), res=NO)
op("put", "/admin/workspace/logo", "putWorkspaceLogo", "Administration", "Set the workspace's logo",
   desc="Workspace admins, in the browser. The image itself is the body: PNG, JPEG, GIF or WebP up to 512 KB (not SVG).", auth="session",
   req=body(d(binary, "the image"), "image/png"), res=NO)
op("delete", "/admin/workspace/logo", "deleteWorkspaceLogo", "Administration", "Remove the logo (TaskCraft's mark again)",
   desc="Workspace admins, in the browser.", auth="session", res=NO)
op("get", "/workspace/logo", "getWorkspaceLogo", "Administration", "The workspace's logo",
   desc="Anyone in the workspace (404 without one).", res=raw("image/png", "the image (or JPEG, GIF, WebP)"))
op("get", "/admin/audit", "getAuditLog", "Administration", "The audit log (latest 300 entries)",
   desc="Workspace admins.", res=ok(arr(S("AuditEntry"))))
op("get", "/admin/stats", "getWorkspaceStats", "Administration", "Workspace statistics",
   desc="Workspace admins.",
   res=ok(obj({"users": d(num, "active people"), "projects": num, "issues": d(num, "not deleted"), "comments": num, "attachments": num, "attachmentBytes": num},
              ["users", "projects", "issues", "comments", "attachments", "attachmentBytes"])))
op("get", "/admin/mail", "getMailStatus", "Administration", "Outgoing e-mail: configuration and outbox",
   desc="Workspace admins.",
   res=ok(obj({
       "enabled": d(boolean, "TASKCRAFT_SMTP_URL is set"),
       "from": d(txt, "the sender (TASKCRAFT_MAIL_FROM)"),
       "server": ntxt("the mail server (without the password)"),
       "pending": d(num, "waiting to be sent"),
       "failed": d(num, "given up in the last 7 days"),
       "sentLastDay": num,
       "lastError": ntxt("the latest error of an unsent e-mail (last 7 days)"),
       "lastErrorAt": ndt(),
   }, ["enabled", "from", "server", "pending", "failed", "sentLastDay", "lastError", "lastErrorAt"])))
op("post", "/admin/mail/test", "sendTestMail", "Administration", "Send a test e-mail to yourself",
   desc="Workspace admins. Sent right away to the admin's own address; 400 with what the mail server said when it fails, or when e-mail isn't configured.",
   res=ok(obj({"ok": boolean, "to": email}, ["ok", "to"])))

# ---- Webhooks ----

op("get", "/admin/webhooks", "listWebhooks", "Webhooks", "Webhooks and the events they can get",
   desc="Workspace admins.", res=ok(obj({"events": arr(txt, "issue.created, issue.updated, issue.deleted, comment.created, sprint.started, sprint.completed, version.released"), "webhooks": arr(S("Webhook"))}, ["events", "webhooks"])))
op("post", "/admin/webhooks", "createWebhook", "Webhooks", "Add a webhook (the answer shows its secret once)",
   desc="Workspace admins (and admins of the project for `projectKey`). The URL must be http(s) without credentials; private and loopback addresses are refused at delivery "
   "unless TASKCRAFT_WEBHOOK_ALLOW_PRIVATE is set. Unknown events are dropped; no events (or `*`) means all of them. Deliveries carry the WebhookEvent schema.",
   req=body(obj({
       "url": uri,
       "events": arr(txt, "event names, or `*`"),
       "projectKey": d(txt, "only this project's events"),
       "secret": d(txt, "the signing secret (at least 8 characters; default: generated)"),
       "format": D_HOOK_FORMAT,
   }, ["url"])),
   res=created(obj({"id": uuid, "secret": d(txt, "signs the deliveries (X-TaskCraft-Signature)")}, ["id", "secret"])))
op("patch", "/admin/webhooks/{id}", "updateWebhook", "Webhooks", "Turn a webhook on or off, or change its events or format",
   desc="Workspace admins. Only the fields sent change.", pp={"id": (uuid, "webhook id")},
   req=body(obj({"active": boolean, "events": arr(txt, "replaces the events (unknown ones are dropped; empty or `*`: all)"), "format": D_HOOK_FORMAT})), res=NO, err="-404")
op("delete", "/admin/webhooks/{id}", "deleteWebhook", "Webhooks", "Delete a webhook",
   desc="Workspace admins.", pp={"id": (uuid, "webhook id")}, res=NO, err="-404")
op("post", "/admin/webhooks/{id}/test", "testWebhook", "Webhooks", "Send a ping delivery now and report how it went",
   desc="Workspace admins. Delivers a `ping` event (signed like any delivery; chat formats get a short message) right away, without retries.", pp={"id": (uuid, "webhook id")},
   res=ok(one(
       obj({"ok": d(boolean, "true"), "status": d(num, "the target's HTTP status")}, ["ok", "status"]),
       obj({"ok": d(boolean, "false"), "error": d(txt, "what went wrong (HTTP status, connection, private address, …)")}, ["ok", "error"]),
   )))

# ---- Backup import ----

D_BACKUP_NOTE = "Workspace owners and admins, in the browser (API tokens are refused)."
op("get", "/admin/backup-imports", "listBackupImports", "Backup import", "Recent uploads and imports (latest 20)",
   desc=D_BACKUP_NOTE, auth="session",
   res=ok(obj({"imports": arr(S("BackupImport")), "maxUploadMb": d(num, "the largest upload (TASKCRAFT_MAX_UPLOAD_MB)")}, ["imports", "maxUploadMb"])))
op("post", "/admin/backup-imports", "uploadBackup", "Backup import", "Upload an XML site backup or a TaskCraft project export (.zip) and check it in the background",
   desc="The .zip itself is the request body, up to TASKCRAFT_MAX_UPLOAD_MB (413). Answers at once with the job in `checking`; poll it until it's `checked` (with its summary) or `failed`. "
   "A .zip with taskcraft-project.json is a project export (the job's source becomes `taskcraft`). "
   "One backup at a time is checked or imported (409). The file is kept for a day at most and deleted once it's imported. " + D_BACKUP_NOTE,
   auth="session", query=[q("name", "the file's name (default backup.zip)")],
   req=body(d(binary, "the backup .zip as downloaded"), "application/zip"),
   res=ok(S("BackupImport"), "Uploaded; being checked", "202"), err="409 413")
op("get", "/admin/backup-imports/{id}", "getBackupImport", "Backup import", "An upload: its check, progress and report",
   desc=D_BACKUP_NOTE, auth="session", pp={"id": (uuid, "import id")}, res=ok(S("BackupImport")))
op("delete", "/admin/backup-imports/{id}", "discardBackupImport", "Backup import", "Discard an upload and its file",
   desc="Not while it's being checked or imported (409). " + D_BACKUP_NOTE, auth="session", pp={"id": (uuid, "import id")}, res=NO, err="409")
op("post", "/admin/backup-imports/{id}/run", "runBackupImport", "Backup import", "Start the import (or a dry run) with the choices made",
   desc="Runs in the background as one transaction: it lands completely or not at all; imports tell nobody (no e-mails, notifications, webhooks or automation), and importing "
   "the same backup again skips what's already there. Allowed when the upload is `checked`, `failed` or `done` (else 409) and its file is still there. Poll the job for "
   "progress and the report. " + D_BACKUP_NOTE,
   auth="session", pp={"id": (uuid, "import id")}, req=body(S("BackupImportRun")),
   res=ok(S("BackupImport"), "Started", "202"), err="409")
op("post", "/admin/backup-imports/connect", "connectImportSource", "Backup import", "Read projects from another tracker's API and check them in the background",
   desc="Answers at once with the job in `checking` while the projects are read (issues, comments, history, people; attachments are downloaded later, while importing). "
   "Requests go to the given server only; private and loopback addresses are refused unless TASKCRAFT_IMPORT_ALLOW_PRIVATE allows them; rate limits are waited out (progress phase `waiting`). "
   "What was read is kept for a day at most; the token is never stored. One import at a time (409). " + D_BACKUP_NOTE,
   auth="session", req=body(S("ImportConnection")), res=ok(S("BackupImport"), "Connected; being read", "202"), err="409")
op("post", "/admin/backup-imports/browse", "browseImportSource", "Backup import", "Projects of another tracker's API to choose from",
   desc="The token's own repositories or projects, or those of an owner (user, organization, group); at most 300. 400 with the source's answer when it refuses. " + D_BACKUP_NOTE,
   auth="session", req=body(obj({**{"source": D_API_SOURCE, "baseUrl": txt, "token": txt}, "owner": d(txt, "a user, organization or group")}, ["source"])),
   res=ok(obj({"projects": arr(obj({"key": txt, "name": txt, "description": ntxt(), "issues": d(num, "open issues, when the source says"), "private": boolean, "parent": ntxt()}, ["key", "name"]))}, ["projects"])))

# ==== Operations ====

op("get", "/health", "getHealth", "Operations", "Liveness: answers while the process runs", auth="public", res=ok(obj({"status": enum("ok")}, ["status"])))
op("get", "/ready", "getReadiness", "Operations", "Readiness: whether this replica can serve",
   desc="503 while it drains connections before a stop, or without its database.", auth="public",
   res={**ok(obj({"status": enum("ok")}, ["status"])), **ok(obj({"status": enum("unavailable"), "database": d(boolean, "the database answers")}, ["status", "database"]), "Not ready", "503")})
op("get", "/metrics", "getMetrics", "Operations", "Prometheus metrics of this replica",
   desc="Requests by method, route pattern and status class with durations, database pool, e-mail and webhook outboxes, e-mail in, live connections and counts. "
        "Off (404) unless TASKCRAFT_METRICS_TOKEN is set; scrape every replica.",
   auth="metrics", res={**raw("text/plain; version=0.0.4", "Prometheus text format"), "401": {"description": "Wrong or missing token"}, "404": {"description": "Metrics are off (no TASKCRAFT_METRICS_TOKEN)"}})
op("get", "/events", "streamEvents", "Operations", "Live updates (Server-Sent Events)",
   desc="A `text/event-stream` of the changes in projects the caller can see: first `hello`, then a `change` event (data: LiveEvent) per change, "
        "`resync` (data `{}`) when the stream fell behind and everything shown should be refetched; keep-alive comments every 20 seconds. Works across replicas.",
   res=raw("text/event-stream", "the event stream", schema=d(txt, "event: hello | change | resync, data: LiveEvent JSON")))
op("get", "/openapi.yaml", "getOpenApiYaml", "Operations", "This document (YAML)", auth="public", res=raw("application/yaml", "OpenAPI 3.1 document", schema=txt))
op("get", "/openapi.json", "getOpenApiJson", "Operations", "This document (JSON)", auth="public", res=ok(anyobj, "OpenAPI 3.1 document"))


# ---- assembly --------------------------------------------------------------------------------


def path_param(path, name, pp):
    if pp and name in pp:
        schema, desc = pp[name]
    elif name == "key" and (path.startswith("/issues/") or path.startswith("/portal/requests/")):
        schema, desc = txt, "issue key (e.g. WEB-12; former keys keep working) or issue id"
    elif name == "key":
        schema, desc = txt, "project key (former keys keep working) or project id"
    elif name in PATH_PARAMS:
        schema, desc = PATH_PARAMS[name]
    else:
        raise SystemExit(f"{path}: describe the path parameter {{{name}}} (pp=...)")
    return {"name": name, "in": "path", "required": True, "description": desc, "schema": schema}


def build():
    paths = {}
    seen_ids = set()
    for o in sorted(R, key=lambda o: TAG_NAMES.index(o["tag"]) if o["tag"] in TAG_NAMES else len(TAG_NAMES)):
        method, path = o["method"], o["path"]
        assert method in ("get", "post", "put", "patch", "delete"), f"{path}: method {method}"
        assert o["tag"] in TAG_NAMES, f"{method} {path}: unknown tag {o['tag']}"
        assert o["opid"] not in seen_ids, f"duplicate operationId {o['opid']}"
        assert re.fullmatch(r"[a-z][A-Za-z0-9]*", o["opid"]), f"operationId {o['opid']} is not camelCase"
        assert o["auth"] in AUTH, f"{method} {path}: unknown auth {o['auth']}"
        seen_ids.add(o["opid"])
        assert method not in paths.get(path, {}), f"{method} {path} twice"
        names = re.findall(r"\{(\w+)\}", path)
        params = [path_param(path, n, o["pp"]) for n in names] + (o["query"] or []) + (o["headers"] or [])
        x = {"tags": [o["tag"]], "summary": o["summary"], "operationId": o["opid"]}
        if o["desc"]:
            x["description"] = o["desc"]
        if params:
            x["parameters"] = params
        if o["req"]:
            x["requestBody"] = o["req"]
        responses = dict(o["res"] or ok())
        codes = set()
        signed_in = o["auth"] in ("user", "session")
        if signed_in:
            codes |= {"401", "403"}
        if method != "get" or o["req"]:
            codes.add("400")
        if names:
            codes.add("404")
        for c in o["err"].split():
            if c.startswith("-"):
                codes.discard(c[1:])
            else:
                assert c in ERRORS, f"{method} {path}: unknown error status {c}"
                codes.add(c)
        for c in sorted(codes):
            responses.setdefault(c, {"$ref": f"#/components/responses/{ERRORS[c]}"})
        x["responses"] = dict(sorted(responses.items()))
        if AUTH[o["auth"]] is not None:
            x["security"] = AUTH[o["auth"]]
        if o["extra"]:
            x.update(o["extra"])
        paths.setdefault(path, {})[method] = x
    return paths


def error_response(desc):
    return {"description": desc, "content": {"application/json": {"schema": S("Error")}}}


INFO_DESCRIPTION = """\
Everything the TaskCraft web app does, as a REST API with JSON bodies.

## Base paths

`/api/v1` is the versioned base path for integrations; the web app calls the same API under `/api`. The paths in this
document are relative to either, e.g. `GET /api/v1/issues/WEB-12`. The MCP server for AI assistants lives at `/api/mcp`
(streamable HTTP, not described here).

## Authentication

- **API token**: `Authorization: Bearer tct_…`, a personal token (*Profile & settings → API tokens*) that acts with its
  owner's permissions. Read-only tokens may only read (GET, plus `POST /query/validate`); anything else answers 403.
  API tokens, the password, sessions, two-factor and passkey changes, the two-factor policy and the backup import are
  managed in the browser only and refuse tokens (403).
- **Browser session**: the `tc_session` cookie that signing in sets. Requests that change something must also send the
  header `x-taskcraft: 1` (CSRF protection), else 400.
- **Public operations** have an empty security requirement (`{}`): health and readiness, first-run setup, the sign-in
  flows, the service desk portal and request forms (the portal has a session cookie of its own, `tc_portal`), the Git
  and e-mail-in receivers (they check secrets of their own), metrics (a bearer token of its own) and this document.
- When the workspace requires two-factor authentication, browser sessions of people without a second factor get 403
  on everything but setting one up.

Results are always limited to what the caller may see; things they may not see answer 404.

## Issues and rich text

Issues are addressed by key (`WEB-12`; keys an issue had before a move or a project rename keep working) or by id;
projects by key or id. Descriptions are ProseMirror / Tiptap JSON (`PmNode`, a document is
`{"type": "doc", "content": [...]}`); where noted, Markdown (`descriptionMarkdown`, comments' `markdown`) or plain text
is accepted instead. Writing an issue takes the fields of `IssueFields`; only the fields sent change.

## TQL, the query language

Search (`GET /search?q=`), CSV export, saved filters, dashboards and automation conditions take TQL, e.g.
`project = WEB and sprint in openSprints() and assignee = me() order by rank` or
`type = Bug and priority >= high and created > -14d`. Fields: `project key title text description comment type status
category priority assignee reporter creator labels sprint version affects component parent/epic points estimate spent created
updated resolved resolution security due start remaining watcher voter votes blocked` and custom fields by name (quoted when they contain spaces). Operators
`= != ~ !~ > >= < <=`, `in (…)`, `not in`, `is [not] empty`; `and`, `or`, `not`, parentheses; `order by`. Functions
`me()`, `now()`, `today()`, `startOfWeek()`, `endOfWeek()`, `startOfMonth()`, `endOfMonth()`, `openSprints()`,
`futureSprints()`, `closedSprints()`, `releasedVersions()`, `unreleasedVersions()`, `withSubprojects(KEY)` (`project in withSubprojects(SHOP)`); relative dates like `-7d`, `+2w`,
`-1m`. Bare words search titles, descriptions and the names and text of files on issues (words also match by their stem in the workspace's language). See [TQL in the README](https://github.com/ortic/TaskCraft#features);
`POST /query/validate` checks a query and `GET /query/suggest` completes field names and values.

## Pagination

`GET /search` pages with `limit` (default 100, at most 1,000) and `offset` and answers `{total, issues}`; other
operations that take `limit` / `offset` say so. Everything else answers a bounded list (e.g. the latest 60 activity
entries, the latest 200 trashed issues), as each operation describes.

## Errors

Errors answer with a status code and `{"error": "<code>", "message": "<text>"}` (schema `Error`): 400 `bad_request`
(invalid input; the message says what), 401 `unauthorized`, 403 `forbidden`, 404 `not_found`, 409 `conflict`, 413
`too_large` (uploads above `TASKCRAFT_MAX_UPLOAD_MB`), 429 `too_many_requests`, 503 `unavailable`, 500 `internal`.
Requests the framework refuses before they reach the API answer plain text instead: a body that isn't valid JSON (400),
a JSON body without `Content-Type: application/json` (415), a typed body with a missing or mistyped field (422), a
malformed id in the path (400) and a body above the server's overall size limit (413).

## Rate limits

Failed sign-ins are limited to 10 per e-mail address in 15 minutes and wrong two-factor codes to 5 per sign-in; both
answer 400 with a message. The service desk portal answers 429 for sign-in links (10 requests per IP address per
quarter hour; at most 3 links go out per address, silently), messages from requesters (30 per hour) and request forms
(20 per IP address and 5 per e-mail address per hour).

## Webhooks

Admins register endpoints under `/admin/webhooks` for `issue.created`, `issue.updated`, `issue.deleted`,
`comment.created`, `sprint.started`, `sprint.completed` and `version.released` (all projects or one). Each delivery is
a JSON `POST` with the `WebhookEvent` body and the headers `X-TaskCraft-Event: <event>` and
`X-TaskCraft-Signature: sha256=<hex>`, the HMAC-SHA256 of the raw body with the webhook's secret (shown once, when the
webhook is created); compare it in constant time. A delivery counts when the endpoint answers 2xx within 10 seconds;
others are retried up to 7 times with growing pauses (30 seconds, 2 minutes, 8 minutes, … at most 4 hours; about 11 hours in all).

## Live updates

`GET /events` is a Server-Sent Events stream of changes in the caller's projects (`LiveEvent`); refetch what an event
touches.
"""


def document():
    paths = build()
    return {
        "openapi": "3.1.0",
        "info": {
            "title": "TaskCraft API",
            "version": "0.1.0",
            "summary": "Self-hosted issue and project management",
            "description": INFO_DESCRIPTION.strip(),
            "license": {"name": "MIT OR Apache-2.0", "identifier": "MIT OR Apache-2.0"},
        },
        "servers": [
            {"url": "/api/v1", "description": "versioned base path for integrations"},
            {"url": "/api", "description": "the same API, as the web app calls it"},
        ],
        "security": [{"token": []}, {"session": []}],
        "tags": [{"name": n, "description": t} for n, t in TAGS],
        "paths": paths,
        "webhooks": {
            "event": {
                "post": {
                    "tags": ["Webhooks"],
                    "summary": "A webhook delivery (TaskCraft calls your endpoint)",
                    "operationId": "webhookDelivery",
                    "description": "Sent to every active webhook whose events (and project) match, from a Postgres outbox any replica delivers. "
                    "Check `X-TaskCraft-Signature` (HMAC-SHA256 of the raw body with the webhook's secret, hex, prefixed `sha256=`) in constant time before trusting the body. "
                    "Redirects aren't followed; private and loopback addresses are refused unless TASKCRAFT_WEBHOOK_ALLOW_PRIVATE is set.",
                    "security": [],
                    "parameters": [
                        hdr("X-TaskCraft-Event", "the event, as in the body's `event`", required=True),
                        hdr("X-TaskCraft-Signature", "`sha256=<hex>`: HMAC-SHA256 of the raw body with the webhook's secret", required=True),
                        hdr("User-Agent", "`TaskCraft-Webhooks/<version>`"),
                    ],
                    "requestBody": body(S("WebhookEvent")),
                    "responses": {
                        "2XX": {"description": "Delivered"},
                        "default": {"description": "Anything else, or no answer within 10 seconds, is tried again up to 7 times with growing pauses: 30 seconds, 2 minutes, 8 minutes, and four times longer each time"},
                    },
                }
            }
        },
        "components": {
            "securitySchemes": {
                "token": {
                    "type": "http",
                    "scheme": "bearer",
                    "bearerFormat": "tct_…",
                    "description": "A personal API token (`Authorization: Bearer tct_…`, created under Profile & settings → API tokens). "
                    "Read-only tokens may only read (GET, plus POST /query/validate); anything else answers 403.",
                },
                "session": {
                    "type": "apiKey",
                    "in": "cookie",
                    "name": "tc_session",
                    "description": "The web app's session cookie. Requests that change data (anything but GET, HEAD, OPTIONS) must also send the header `x-taskcraft: 1` (CSRF protection), else 400.",
                },
                "portalSession": {
                    "type": "apiKey",
                    "in": "cookie",
                    "name": "tc_portal",
                    "description": "The service desk portal's own session (7 days), started from an e-mailed sign-in link; sent only to /api/portal and never a TaskCraft sign-in. Writes need `x-taskcraft: 1`.",
                },
                "metricsToken": {"type": "http", "scheme": "bearer", "description": "The value of TASKCRAFT_METRICS_TOKEN (the endpoint answers 404 without one configured)."},
                "mailInToken": {"type": "http", "scheme": "bearer", "description": "The value of TASKCRAFT_MAIL_IN_TOKEN (the endpoint answers 404 without one configured)."},
                "mailInHeader": {"type": "apiKey", "in": "header", "name": "X-TaskCraft-Token", "description": "TASKCRAFT_MAIL_IN_TOKEN in a header of its own."},
                "mailInQuery": {"type": "apiKey", "in": "query", "name": "token", "description": "TASKCRAFT_MAIL_IN_TOKEN in the address, for mail services that can't set headers."},
            },
            "responses": {
                "BadRequest": error_response("Invalid input (the message says what), or a browser-session write without the `x-taskcraft` header. A body that isn't JSON at all is refused as plain text"),
                "Unauthorized": error_response("Not signed in, or an unknown / expired API token"),
                "Forbidden": error_response("Not allowed: missing role or project permission, a read-only token on a write, an API token where the browser is required, or two-factor setup required first"),
                "NotFound": error_response("Not found, or not visible to the caller"),
                "Conflict": error_response("Conflicts with the current state (e.g. already exists)"),
                "TooLarge": error_response("The request body is too large"),
                "TooMany": error_response("A rate limit was reached; the message says when to try again"),
                "Unavailable": error_response("Not available right now; try again later"),
            },
            "schemas": schemas,
        },
    }


class Dumper(yaml.SafeDumper):
    """Writes shared sub-schemas out in full instead of as YAML anchors and aliases."""

    def ignore_aliases(self, data):
        return True


def main():
    doc = document()
    root = pathlib.Path(__file__).resolve().parent.parent
    out = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / "crates/server"
    out.mkdir(parents=True, exist_ok=True)
    header = "# Generated by scripts/gen-openapi.py — edit the script, not this file.\n"
    (out / "openapi.yaml").write_text(header + yaml.dump(doc, Dumper=Dumper, sort_keys=False, allow_unicode=True, width=120))
    (out / "openapi.json").write_text(json.dumps(doc, ensure_ascii=False, indent=1) + "\n")
    ops = sum(len(m) for m in doc["paths"].values())
    print(f"{ops} operations, {len(doc['paths'])} paths, {len(schemas)} schemas -> {out}/openapi.yaml, openapi.json")


if __name__ == "__main__":
    main()
