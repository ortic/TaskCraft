-- AI coding agents (crate::agents): an agent is a person of its own kind — assignable and
-- mentionable, with its own permissions; a runner picks its runs up and works on them.
ALTER TABLE users ADD COLUMN agent boolean NOT NULL DEFAULT false;

CREATE TABLE agents (
    id              uuid PRIMARY KEY,
    workspace_id    uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- its account (name, project roles, what it sees)
    user_id         uuid NOT NULL UNIQUE REFERENCES users(id) ON DELETE CASCADE,
    -- @handle in comments
    handle          text NOT NULL,
    instructions    text NOT NULL DEFAULT '',
    -- another model than the server's (TASKCRAFT_AI_MODEL), on the same connection
    model           text,
    -- the repository it works in (a Git integration with API access) and the branch it starts from
    integration_id  uuid REFERENCES git_integrations(id) ON DELETE SET NULL,
    base_branch     text,
    -- testing (only workspace admins start it) | active | off
    state           text NOT NULL DEFAULT 'testing',
    created_by      uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX agents_handle ON agents (workspace_id, lower(handle));

-- Runners: separate programs (containers) that do the work. Registered with a one-time token.
CREATE TABLE agent_runners (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name           text NOT NULL,
    -- the registration token until it registered, then the runner's own
    token_hash     bytea NOT NULL UNIQUE,
    registered_at  timestamptz,
    last_seen_at   timestamptz,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE agent_runs (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    agent_id      uuid NOT NULL REFERENCES agents(id) ON DELETE CASCADE,
    issue_id      uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    -- queued | running | succeeded | failed | cancelled
    state         text NOT NULL DEFAULT 'queued',
    -- assigned | mentioned
    trigger       text NOT NULL,
    triggered_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    -- what the mention said
    prompt        text,
    runner_id     uuid REFERENCES agent_runners(id) ON DELETE SET NULL,
    -- the credential of this run alone (model calls, the source, the result); gone when it ends
    token_hash    bytea,
    created_at    timestamptz NOT NULL DEFAULT now(),
    started_at    timestamptz,
    heartbeat_at  timestamptz,
    finished_at   timestamptz,
    -- {kind: answer | pull_request, summary, branch, url, number, files}
    result        jsonb,
    error         text,
    model_calls   integer NOT NULL DEFAULT 0,
    tokens_in     bigint NOT NULL DEFAULT 0,
    tokens_out    bigint NOT NULL DEFAULT 0
);
-- one run at a time per agent and issue: a second mention doesn't start a second one
CREATE UNIQUE INDEX agent_runs_active ON agent_runs (agent_id, issue_id) WHERE state IN ('queued', 'running');
CREATE INDEX agent_runs_issue ON agent_runs (issue_id, created_at DESC);
CREATE INDEX agent_runs_queue ON agent_runs (workspace_id, created_at) WHERE state = 'queued';

CREATE TABLE agent_run_logs (
    id      bigserial PRIMARY KEY,
    run_id  uuid NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    at      timestamptz NOT NULL DEFAULT now(),
    line    text NOT NULL
);
CREATE INDEX agent_run_logs_run ON agent_run_logs (run_id, id);
