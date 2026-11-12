-- Migrations from another tracker's XML site backup.

-- where imported rows came from (the source site and the row's id there): importing the same
-- backup again skips what is already there
CREATE TABLE import_sources (
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    origin        text NOT NULL,
    -- issue | user | sprint
    kind          text NOT NULL,
    source_id     text NOT NULL,
    target_id     uuid NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (workspace_id, origin, kind, source_id)
);

-- backups uploaded in the browser: the check of what they hold, the choices, and the import
-- running in the background (the file itself is kept in attachment storage for a day at most)
CREATE TABLE backup_imports (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    created_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    file_name    text NOT NULL,
    size         bigint NOT NULL,
    -- checking | checked | running | done | failed
    status       text NOT NULL,
    summary      jsonb,
    options      jsonb,
    progress     jsonb,
    report       jsonb,
    error        text,
    -- the file is gone (imported, discarded or expired)
    file_deleted boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz
);
CREATE INDEX backup_imports_workspace ON backup_imports (workspace_id, created_at DESC);
