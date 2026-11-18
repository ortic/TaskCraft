-- Timers: at most one running per person, on an issue, kept here so every tab and device shows it.
-- elapsed = (paused_at, else now) - started_at - paused_seconds
CREATE TABLE timers (
    user_id        uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    issue_id       uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    started_at     timestamptz NOT NULL DEFAULT now(),
    paused_at      timestamptz,
    paused_seconds integer NOT NULL DEFAULT 0,
    note           text,
    -- told it runs long (once)
    reminded_at    timestamptz
);
CREATE INDEX timers_issue ON timers (issue_id);

-- Time drafts: stopped timers not booked yet. Only their owner sees them; reports, the issue's
-- time spent and the timesheet leave them out until they're logged.
CREATE TABLE time_drafts (
    id          uuid PRIMARY KEY,
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    issue_id    uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    minutes     integer NOT NULL,
    work_date   date NOT NULL,
    started_at  timestamptz,
    note        text,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX time_drafts_user ON time_drafts (user_id, created_at);
