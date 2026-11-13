-- Share links: a read-only page of a project for people without an account (a list of issues
-- from a query, or the roadmap), with an optional password and end. Revoked links are deleted.
CREATE TABLE share_links (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_id     uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- the secret part of the address
    token          text NOT NULL UNIQUE,
    -- issues | roadmap
    kind           text NOT NULL,
    title          text NOT NULL,
    -- TQL within the project (empty: all its issues)
    query          text NOT NULL DEFAULT '',
    -- {"people": shows names, "comments": shows comments for everyone}
    options        jsonb NOT NULL DEFAULT '{}',
    password_hash  text,
    expires_at     timestamptz,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    views          bigint NOT NULL DEFAULT 0,
    last_viewed_at timestamptz
);
CREATE INDEX share_links_project ON share_links (project_id);
