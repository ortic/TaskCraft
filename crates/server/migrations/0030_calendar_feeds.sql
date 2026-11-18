-- Calendar feeds (iCalendar): a secret address per feed that calendar apps subscribe to. Only the
-- token's hash is kept; the address is shown once (a new one revokes the old). A feed shows what
-- its owner may see. kind: mine (issues assigned to them, sprints and releases of their projects),
-- filter (a saved filter's issues), project (a project's issues, sprints and releases).
CREATE TABLE calendar_feeds (
    id           uuid PRIMARY KEY,
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash   bytea NOT NULL UNIQUE,
    kind         text NOT NULL,
    view_id      uuid REFERENCES views(id) ON DELETE CASCADE,
    project_id   uuid REFERENCES projects(id) ON DELETE CASCADE,
    -- what goes in: {"issues": true, "sprints": true, "releases": true}
    parts        jsonb NOT NULL DEFAULT '{}',
    created_at   timestamptz NOT NULL DEFAULT now(),
    last_used_at timestamptz
);
CREATE INDEX calendar_feeds_user ON calendar_feeds (user_id);
