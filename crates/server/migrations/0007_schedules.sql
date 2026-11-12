-- scheduled automation rules ("every weekday at 08:00 …")
ALTER TABLE automation_rules ADD COLUMN next_run_at timestamptz;
CREATE INDEX automation_rules_due ON automation_rules (next_run_at) WHERE next_run_at IS NOT NULL;

-- issue templates; with a schedule they create issues on their own (recurring issues)
CREATE TABLE issue_templates (
    id             uuid PRIMARY KEY,
    project_id     uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name           text NOT NULL,
    -- title of the issues made from it; {date}, {week}, {month} are filled in
    title          text NOT NULL DEFAULT '',
    description    jsonb,
    -- field values like a change: typeId, priority, labels, assigneeId, storyPoints, estimateMinutes,
    -- parentId; plus dueInDays and sprint: "active"
    fields         jsonb NOT NULL DEFAULT '{}',
    -- titles of sub-tasks created with it
    subtasks       jsonb NOT NULL DEFAULT '[]',
    schedule       jsonb,
    next_run_at    timestamptz,
    last_run_at    timestamptz,
    last_issue_id  uuid REFERENCES issues(id) ON DELETE SET NULL,
    position       double precision NOT NULL DEFAULT 0,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX issue_templates_project ON issue_templates (project_id);
CREATE INDEX issue_templates_due ON issue_templates (next_run_at) WHERE next_run_at IS NOT NULL;

-- service-level goals already reported as breached (one alert per issue and goal)
CREATE TABLE sla_alerts (
    issue_id     uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    goal         text NOT NULL,
    notified_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, goal)
);
