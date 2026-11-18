-- The workspace's own link types between issues (causes / is caused by, …). The built-in ones —
-- blocks, relates, duplicates, clones — need no row; a row with their key renames them.
-- issue_links.kind holds the type's key. dependency: the target is blocked while the source is open.
CREATE TABLE link_types (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    key          text NOT NULL,
    outward      text NOT NULL,
    inward       text NOT NULL,
    dependency   boolean NOT NULL DEFAULT false,
    position     double precision NOT NULL DEFAULT 0,
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (workspace_id, key)
);
CREATE INDEX issue_links_kind ON issue_links (kind);
