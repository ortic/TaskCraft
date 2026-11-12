-- TaskCraft initial schema

CREATE TABLE workspaces (
    id          uuid PRIMARY KEY,
    name        text NOT NULL,
    settings    jsonb NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    email          text,
    name           text NOT NULL,
    password_hash  text,
    -- owner | admin | member | guest
    role           text NOT NULL DEFAULT 'member',
    -- active | invited | deactivated
    status         text NOT NULL DEFAULT 'active',
    title          text,
    locale         text,
    timezone       text,
    preferences    jsonb NOT NULL DEFAULT '{}',
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_login_at  timestamptz
);
CREATE UNIQUE INDEX users_email ON users (workspace_id, lower(email)) WHERE email IS NOT NULL;

CREATE TABLE sessions (
    token_hash    bytea PRIMARY KEY,
    user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    last_seen_at  timestamptz NOT NULL DEFAULT now(),
    user_agent    text,
    ip            text
);
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE api_tokens (
    id            uuid PRIMARY KEY,
    user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name          text NOT NULL,
    token_hash    bytea NOT NULL UNIQUE,
    -- read | write
    scope         text NOT NULL DEFAULT 'write',
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz,
    last_used_at  timestamptz
);

-- one-time links: invitations and password resets
CREATE TABLE user_links (
    token_hash  bytea PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- invite | reset
    kind        text NOT NULL,
    expires_at  timestamptz NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE login_attempts (
    email         text PRIMARY KEY,
    failures      integer NOT NULL,
    window_start  timestamptz NOT NULL
);

CREATE TABLE teams (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          text NOT NULL,
    description   text,
    color         text,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX teams_name ON teams (workspace_id, lower(name));

CREATE TABLE team_members (
    team_id  uuid NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id  uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE projects (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    key           text NOT NULL,
    name          text NOT NULL,
    description   text,
    -- symbol name from the icon set, colour as #rrggbb
    icon          text,
    color         text,
    -- scrum | kanban | tracker
    kind          text NOT NULL DEFAULT 'scrum',
    lead_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    issue_seq     integer NOT NULL DEFAULT 0,
    -- role every workspace member (not guests) has: none | viewer | member | admin
    default_role  text NOT NULL DEFAULT 'member',
    -- estimation (points | hours), workflow (free | restricted), wiki space, default assignee, …
    settings      jsonb NOT NULL DEFAULT '{}',
    created_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    archived_at   timestamptz
);
CREATE UNIQUE INDEX projects_key ON projects (workspace_id, lower(key));

-- earlier keys of renamed projects: old issue keys keep working
CREATE TABLE project_key_aliases (
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    key           text NOT NULL,
    project_id    uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    PRIMARY KEY (workspace_id, key)
);

-- explicit roles: subject_type user | team, role viewer | member | admin
CREATE TABLE project_members (
    project_id    uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    subject_type  text NOT NULL DEFAULT 'user',
    subject_id    uuid NOT NULL,
    role          text NOT NULL,
    PRIMARY KEY (project_id, subject_type, subject_id)
);

CREATE TABLE project_stars (
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, project_id)
);

CREATE TABLE statuses (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    -- todo | in_progress | done
    category    text NOT NULL,
    color       text,
    position    double precision NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX statuses_name ON statuses (project_id, lower(name));

-- allowed moves when the project's workflow is restricted
CREATE TABLE transitions (
    project_id      uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    from_status_id  uuid NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    to_status_id    uuid NOT NULL REFERENCES statuses(id) ON DELETE CASCADE,
    PRIMARY KEY (from_status_id, to_status_id)
);

CREATE TABLE issue_types (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    icon        text NOT NULL DEFAULT 'task',
    color       text NOT NULL DEFAULT '#4f7cff',
    -- 1 = epic (groups issues), 0 = standard, -1 = sub-task
    level       smallint NOT NULL DEFAULT 0,
    position    double precision NOT NULL DEFAULT 0,
    -- description template for new issues of this type (ProseMirror JSON)
    template    jsonb
);
CREATE UNIQUE INDEX issue_types_name ON issue_types (project_id, lower(name));

CREATE TABLE versions (
    id            uuid PRIMARY KEY,
    project_id    uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          text NOT NULL,
    description   text,
    -- unreleased | released | archived
    status        text NOT NULL DEFAULT 'unreleased',
    start_date    date,
    release_date  date,
    released_at   timestamptz,
    position      double precision NOT NULL DEFAULT 0,
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX versions_name ON versions (project_id, lower(name));

CREATE TABLE components (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    description text,
    lead_id     uuid REFERENCES users(id) ON DELETE SET NULL
);
CREATE UNIQUE INDEX components_name ON components (project_id, lower(name));

CREATE TABLE sprints (
    id            uuid PRIMARY KEY,
    project_id    uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name          text NOT NULL,
    goal          text,
    -- future | active | closed
    state         text NOT NULL DEFAULT 'future',
    start_at      timestamptz,
    end_at        timestamptz,
    completed_at  timestamptz,
    position      double precision NOT NULL DEFAULT 0,
    -- figures kept when the sprint starts and ends (committed / completed points and issues)
    stats         jsonb NOT NULL DEFAULT '{}',
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sprints_project ON sprints (project_id, state);

CREATE TABLE custom_fields (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    -- text | number | select | multiselect | date | user | checkbox | url
    kind        text NOT NULL,
    -- select options: [{"id": "...", "label": "...", "color": "#..."}]
    options     jsonb NOT NULL DEFAULT '[]',
    description text,
    position    double precision NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX custom_fields_name ON custom_fields (project_id, lower(name));

CREATE TABLE issues (
    id                uuid PRIMARY KEY,
    workspace_id      uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_id        uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    number            integer NOT NULL,
    title             text NOT NULL,
    description       jsonb,
    description_text  text NOT NULL DEFAULT '',
    type_id           uuid NOT NULL REFERENCES issue_types(id),
    status_id         uuid NOT NULL REFERENCES statuses(id),
    -- 0 none, 1 low, 2 medium, 3 high, 4 urgent
    priority          smallint NOT NULL DEFAULT 2,
    assignee_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    reporter_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    parent_id         uuid REFERENCES issues(id) ON DELETE SET NULL,
    sprint_id         uuid REFERENCES sprints(id) ON DELETE SET NULL,
    version_id        uuid REFERENCES versions(id) ON DELETE SET NULL,
    component_id      uuid REFERENCES components(id) ON DELETE SET NULL,
    story_points      double precision,
    estimate_minutes  integer,
    spent_minutes     integer NOT NULL DEFAULT 0,
    start_date        date,
    due_date          date,
    -- order in backlogs and board columns (fractional; renumbered when gaps get too small)
    rank              double precision NOT NULL DEFAULT 0,
    custom            jsonb NOT NULL DEFAULT '{}',
    resolved_at       timestamptz,
    status_changed_at timestamptz NOT NULL DEFAULT now(),
    created_by        uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    deleted_at        timestamptz,
    deleted_by        uuid REFERENCES users(id) ON DELETE SET NULL,
    search            tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', coalesce(title, '')), 'A') || setweight(to_tsvector('simple', coalesce(description_text, '')), 'B')
    ) STORED
);
CREATE UNIQUE INDEX issues_number ON issues (project_id, number);
CREATE INDEX issues_rank ON issues (project_id, rank) WHERE deleted_at IS NULL;
CREATE INDEX issues_assignee ON issues (assignee_id) WHERE deleted_at IS NULL;
CREATE INDEX issues_sprint ON issues (sprint_id);
CREATE INDEX issues_parent ON issues (parent_id);
CREATE INDEX issues_updated ON issues (workspace_id, updated_at DESC);
CREATE INDEX issues_search ON issues USING gin (search);

CREATE TABLE issue_labels (
    issue_id  uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    label     text NOT NULL,
    PRIMARY KEY (issue_id, label)
);
CREATE INDEX issue_labels_label ON issue_labels (label);

-- kind: blocks (source blocks target) | relates | duplicates (source duplicates target) | clones
CREATE TABLE issue_links (
    id          uuid PRIMARY KEY,
    source_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    target_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    kind        text NOT NULL,
    created_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (source_id, target_id, kind)
);
CREATE INDEX issue_links_target ON issue_links (target_id);

CREATE TABLE issue_watchers (
    issue_id  uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id   uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_watchers_user ON issue_watchers (user_id);

CREATE TABLE comments (
    id          uuid PRIMARY KEY,
    issue_id    uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    author_id   uuid REFERENCES users(id) ON DELETE SET NULL,
    body        jsonb NOT NULL,
    body_text   text NOT NULL DEFAULT '',
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    edited      boolean NOT NULL DEFAULT false
);
CREATE INDEX comments_issue ON comments (issue_id, created_at);

CREATE TABLE comment_reactions (
    comment_id  uuid NOT NULL REFERENCES comments(id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- thumbs-up | heart | check | eyes | party
    reaction    text NOT NULL,
    PRIMARY KEY (comment_id, user_id, reaction)
);

CREATE TABLE worklogs (
    id          uuid PRIMARY KEY,
    issue_id    uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id     uuid REFERENCES users(id) ON DELETE SET NULL,
    minutes     integer NOT NULL,
    work_date   date NOT NULL,
    note        text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX worklogs_issue ON worklogs (issue_id);
CREATE INDEX worklogs_user_date ON worklogs (user_id, work_date);

CREATE TABLE attachments (
    id           uuid PRIMARY KEY,
    issue_id     uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    name         text NOT NULL,
    mime         text NOT NULL,
    size         bigint NOT NULL,
    storage_key  text NOT NULL,
    uploaded_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attachments_issue ON attachments (issue_id);

-- links to pages elsewhere; kind wiki = a WikiCraft page (title and space looked up live)
CREATE TABLE web_links (
    id          uuid PRIMARY KEY,
    issue_id    uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    url         text NOT NULL,
    title       text NOT NULL,
    kind        text NOT NULL DEFAULT 'web',
    created_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX web_links_issue ON web_links (issue_id);
CREATE INDEX web_links_url ON web_links (url);

-- everything that happened to an issue: the activity feed, history, reports (burndown, flow)
CREATE TABLE issue_events (
    id          uuid PRIMARY KEY,
    issue_id    uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    actor_id    uuid REFERENCES users(id) ON DELETE SET NULL,
    -- created | field | comment | worklog | attachment | link
    kind        text NOT NULL,
    field       text,
    old_value   jsonb,
    new_value   jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX issue_events_issue ON issue_events (issue_id, created_at);
CREATE INDEX issue_events_project ON issue_events (project_id, created_at);

CREATE TABLE recent_issues (
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issue_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    viewed_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, issue_id)
);

CREATE TABLE notifications (
    id          uuid PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issue_id    uuid REFERENCES issues(id) ON DELETE CASCADE,
    actor_id    uuid REFERENCES users(id) ON DELETE SET NULL,
    -- assigned | mentioned | commented | status | created | due
    kind        text NOT NULL,
    data        jsonb NOT NULL DEFAULT '{}',
    created_at  timestamptz NOT NULL DEFAULT now(),
    read_at     timestamptz
);
CREATE INDEX notifications_user ON notifications (user_id, created_at DESC);

CREATE TABLE boards (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    -- kanban | scrum
    kind        text NOT NULL,
    -- columns [{id, name, statusIds, wipLimit}], swimlanes, quick filters, card fields, base filter
    config      jsonb NOT NULL DEFAULT '{}',
    position    double precision NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX boards_project ON boards (project_id);

-- saved views: a TQL query plus how it is shown (list columns, grouping, layout)
CREATE TABLE views (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    owner_id      uuid REFERENCES users(id) ON DELETE CASCADE,
    project_id    uuid REFERENCES projects(id) ON DELETE CASCADE,
    name          text NOT NULL,
    query         text NOT NULL DEFAULT '',
    -- list | board | calendar | timeline
    layout        text NOT NULL DEFAULT 'list',
    config        jsonb NOT NULL DEFAULT '{}',
    shared        boolean NOT NULL DEFAULT false,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE view_stars (
    user_id  uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    view_id  uuid NOT NULL REFERENCES views(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, view_id)
);

-- "when … if … then …" rules per project
CREATE TABLE automation_rules (
    id           uuid PRIMARY KEY,
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name         text NOT NULL,
    enabled      boolean NOT NULL DEFAULT true,
    -- {"event": "created" | "status" | "assigned" | "commented", "statusId": …}
    trigger      jsonb NOT NULL,
    -- TQL the issue must match ("" = any)
    condition    text NOT NULL DEFAULT '',
    -- [{"action": "setStatus" | "assign" | "setPriority" | "addLabel" | "comment" | "setSprint", …}]
    actions      jsonb NOT NULL DEFAULT '[]',
    runs         integer NOT NULL DEFAULT 0,
    last_run_at  timestamptz,
    created_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX automation_rules_project ON automation_rules (project_id);

CREATE TABLE webhooks (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_id    uuid REFERENCES projects(id) ON DELETE CASCADE,
    url           text NOT NULL,
    secret        text NOT NULL,
    events        text[] NOT NULL DEFAULT '{}',
    active        boolean NOT NULL DEFAULT true,
    created_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_status   integer,
    last_error    text,
    last_sent_at  timestamptz
);

CREATE TABLE webhook_outbox (
    id          uuid PRIMARY KEY,
    webhook_id  uuid NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    payload     jsonb NOT NULL,
    attempts    integer NOT NULL DEFAULT 0,
    next_at     timestamptz NOT NULL DEFAULT now(),
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webhook_outbox_next ON webhook_outbox (next_at);

CREATE TABLE audit_log (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    actor_id      uuid REFERENCES users(id) ON DELETE SET NULL,
    action        text NOT NULL,
    target        text,
    data          jsonb NOT NULL DEFAULT '{}',
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_ws ON audit_log (workspace_id, created_at DESC);

-- project roles as numbers: none 0, viewer 1, member 2, admin 3
CREATE FUNCTION tc_role_rank(role text) RETURNS integer LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE role WHEN 'admin' THEN 3 WHEN 'member' THEN 2 WHEN 'viewer' THEN 1 ELSE 0 END
$$;

-- the role a person has in each project of their workspace
CREATE FUNCTION tc_project_roles(uid uuid) RETURNS TABLE (project_id uuid, rank integer) LANGUAGE sql STABLE AS $$
    SELECT p.id,
           CASE WHEN p.archived_at IS NOT NULL AND u.role NOT IN ('owner', 'admin') THEN LEAST(1, r.rank) ELSE r.rank END
      FROM users u
      JOIN projects p ON p.workspace_id = u.workspace_id
      CROSS JOIN LATERAL (
          SELECT GREATEST(
              CASE WHEN u.role IN ('owner', 'admin') THEN 3 ELSE 0 END,
              CASE WHEN u.role <> 'guest' THEN tc_role_rank(p.default_role) ELSE 0 END,
              COALESCE((SELECT max(tc_role_rank(m.role)) FROM project_members m
                         WHERE m.project_id = p.id
                           AND ((m.subject_type = 'user' AND m.subject_id = u.id)
                             OR (m.subject_type = 'team' AND m.subject_id IN (SELECT tm.team_id FROM team_members tm WHERE tm.user_id = u.id)))), 0)
          ) AS rank
      ) r
     WHERE u.id = uid AND u.status = 'active'
$$;
