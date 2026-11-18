-- Absences: when people are away. Others see only that someone is away and until when, never
-- why (kind, note): that's for the person and workspace admins. half: am | pm, for a single day.
-- substitute: told about new assignments and missed service-level goals meanwhile.
CREATE TABLE absences (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    user_id       uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    start_date    date NOT NULL,
    end_date      date NOT NULL,
    half          text,
    -- vacation | sick | training | other
    kind          text NOT NULL DEFAULT 'vacation',
    note          text,
    substitute_id uuid REFERENCES users(id) ON DELETE SET NULL,
    created_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (end_date >= start_date)
);
CREATE INDEX absences_user ON absences (user_id, start_date);
CREATE INDEX absences_workspace ON absences (workspace_id, end_date);

-- Public holidays, in region calendars ('' is the workspace's default one); people pick theirs.
CREATE TABLE holidays (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    region        text NOT NULL DEFAULT '',
    day           date NOT NULL,
    name          text NOT NULL,
    UNIQUE (workspace_id, region, day)
);
ALTER TABLE users ADD COLUMN holiday_region text;
