-- Time tracking: what's left next to the original estimate, and what time was spent on.

-- the remaining estimate when someone set one; NULL: the original estimate minus the time logged
ALTER TABLE issues ADD COLUMN remaining_minutes integer;

-- what kind of work time goes to (Development, Meetings, Support, …), with whether such time is
-- billable unless the person logging says otherwise
CREATE TABLE time_categories (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         text NOT NULL,
    billable     boolean NOT NULL DEFAULT false,
    -- kept for the time already logged, not offered any more
    archived     boolean NOT NULL DEFAULT false,
    position     double precision NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX time_categories_name ON time_categories (workspace_id, lower(name));

ALTER TABLE worklogs ADD COLUMN category_id uuid REFERENCES time_categories(id) ON DELETE SET NULL;
ALTER TABLE worklogs ADD COLUMN billable boolean NOT NULL DEFAULT false;
CREATE INDEX worklogs_date ON worklogs (work_date);
