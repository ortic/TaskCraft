-- Requester organizations: the customers whose people write to the service desk. Members are
-- e-mail addresses; requesters from one of its domains join by themselves. A request belongs to
-- its requester's organization; shared, the organization's members follow it in their portal.
-- sla_percent: its service-level goals as a share of the project's (50 = twice as fast).
CREATE TABLE organizations (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name         text NOT NULL,
    domains      text[] NOT NULL DEFAULT '{}',
    notes        text,
    sla_percent  integer NOT NULL DEFAULT 100,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX organizations_name ON organizations (workspace_id, lower(name));

CREATE TABLE organization_members (
    organization_id uuid NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    email           text NOT NULL,
    name            text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, email)
);
CREATE INDEX organization_members_email ON organization_members (lower(email));

ALTER TABLE issue_requesters
    ADD COLUMN organization_id uuid REFERENCES organizations(id) ON DELETE SET NULL,
    -- the organization's members follow it in their portal
    ADD COLUMN shared boolean NOT NULL DEFAULT false;
CREATE INDEX issue_requesters_organization ON issue_requesters (organization_id);

-- A request shared with named colleagues (addresses in its organization).
CREATE TABLE request_shares (
    issue_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    email      text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, email)
);
CREATE INDEX request_shares_email ON request_shares (lower(email));
