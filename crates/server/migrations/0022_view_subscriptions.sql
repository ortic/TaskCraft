-- Filter subscriptions: a saved filter's results e-mailed to a person on a schedule.
CREATE TABLE view_subscriptions (
    view_id     uuid NOT NULL REFERENCES views(id) ON DELETE CASCADE,
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    schedule    jsonb NOT NULL,
    -- no e-mail when nothing matches
    skip_empty  boolean NOT NULL DEFAULT true,
    next_run_at timestamptz,
    last_run_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (view_id, user_id)
);
CREATE INDEX view_subscriptions_due ON view_subscriptions (next_run_at);
