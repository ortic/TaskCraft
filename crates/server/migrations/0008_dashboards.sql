-- dashboards people compose from widgets (issue lists, counts, charts, reports, notes)
CREATE TABLE dashboards (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    owner_id      uuid REFERENCES users(id) ON DELETE CASCADE,
    name          text NOT NULL,
    shared        boolean NOT NULL DEFAULT false,
    -- [{"id", "type", "title", "config", "wide"}]
    widgets       jsonb NOT NULL DEFAULT '[]',
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX dashboards_ws ON dashboards (workspace_id);
