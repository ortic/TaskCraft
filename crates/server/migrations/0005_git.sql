-- Git integration: repositories that report pushes and pull / merge requests, the commits,
-- branches and pull requests linked to issues by key, and per-project rules that move them.

CREATE TABLE git_integrations (
    id             uuid PRIMARY KEY,
    project_id     uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- github | gitlab | generic
    provider       text NOT NULL,
    repo_url       text NOT NULL,
    -- the last segment of the webhook URL (identifies the repository's hook)
    hook_key       text NOT NULL UNIQUE,
    -- GitHub: the HMAC key of X-Hub-Signature-256; GitLab: X-Gitlab-Token; generic: either.
    -- Shown once when created (kept in plain text: signatures can't be checked against a hash)
    secret         text NOT NULL,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    deliveries     integer NOT NULL DEFAULT 0,
    last_event     text,
    last_event_at  timestamptz,
    last_error     text
);
CREATE INDEX git_integrations_project ON git_integrations (project_id);

CREATE TABLE git_rules (
    project_id        uuid PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    -- when a pull / merge request is merged, linked issues move here
    merged_status_id  uuid REFERENCES statuses(id) ON DELETE SET NULL,
    -- when a branch is pushed or a pull / merge request opened, linked issues move here
    opened_status_id  uuid REFERENCES statuses(id) ON DELETE SET NULL,
    updated_at        timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE dev_links (
    id              uuid PRIMARY KEY,
    issue_id        uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    integration_id  uuid REFERENCES git_integrations(id) ON DELETE SET NULL,
    -- commit | branch | pull_request
    kind            text NOT NULL,
    title           text NOT NULL,
    url             text NOT NULL,
    author          text,
    -- pull requests: open | merged | closed
    state           text,
    sha             text,
    branch          text,
    number          integer,
    -- org/repo, for display
    repo            text,
    -- when it happened (commit time, last pull request update)
    at              timestamptz NOT NULL DEFAULT now(),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issue_id, url)
);
