-- Extensions and scripted automation (crate::extensions): sandboxed scripts that compute fields in
-- automation rules, and packages admins install — custom automation actions, event handlers,
-- scheduled jobs and API routes — with the capabilities they were granted.
ALTER TABLE automation_rules ADD COLUMN last_error text;

CREATE TABLE extensions (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    -- its address under /api/ext/ and how rules name it: lower-case letters, digits, hyphens
    name          text NOT NULL,
    title         text NOT NULL,
    version       text NOT NULL DEFAULT '0.1.0',
    description   text,
    -- the version of the extension API it was written for
    api_version   integer NOT NULL DEFAULT 1,
    -- as installed: {capabilities: {issues, http, state, secrets}, actions, events, schedule, routes}
    manifest      jsonb NOT NULL,
    script        text NOT NULL,
    enabled       boolean NOT NULL DEFAULT true,
    -- whose permissions its reads and writes have (null: it can't read or write issues)
    run_as        uuid REFERENCES users(id) ON DELETE SET NULL,
    -- name → value, kept for the extension, never shown
    secrets       jsonb NOT NULL DEFAULT '{}',
    -- a little state of its own
    state         jsonb NOT NULL DEFAULT '{}',
    next_run_at   timestamptz,
    last_run_at   timestamptz,
    runs          integer NOT NULL DEFAULT 0,
    last_error    text,
    last_error_at timestamptz,
    installed_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, name)
);
CREATE INDEX extensions_due ON extensions (next_run_at) WHERE enabled AND next_run_at IS NOT NULL;

CREATE TABLE extension_logs (
    id            uuid PRIMARY KEY,
    extension_id  uuid NOT NULL REFERENCES extensions(id) ON DELETE CASCADE,
    at            timestamptz NOT NULL DEFAULT now(),
    -- info | error
    level         text NOT NULL,
    -- what ran: action:<name>, event:<kind>, scheduled, route:<METHOD path>, install
    hook          text NOT NULL,
    message       text NOT NULL
);
CREATE INDEX extension_logs_ext ON extension_logs (extension_id, at DESC);
