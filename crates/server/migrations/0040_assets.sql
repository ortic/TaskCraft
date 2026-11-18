-- Asset inventory (crate::assets): the devices, services and other things issues are about.
-- Sets (Hardware, Services, Locations) with their own permissions hold types with fields,
-- categories in a tree, and the assets; assets relate to one another and to issues.
CREATE TABLE asset_sets (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          text NOT NULL,
    description   text,
    -- what everyone in the workspace (but guests) may do: none | viewer | editor
    default_role  text NOT NULL DEFAULT 'viewer',
    created_at    timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX asset_sets_name ON asset_sets (workspace_id, lower(name));

CREATE TABLE asset_set_members (
    set_id        uuid NOT NULL REFERENCES asset_sets(id) ON DELETE CASCADE,
    -- user | team
    subject_type  text NOT NULL,
    subject_id    uuid NOT NULL,
    -- viewer | editor | admin
    role          text NOT NULL,
    PRIMARY KEY (set_id, subject_type, subject_id)
);

CREATE TABLE asset_types (
    id           uuid PRIMARY KEY,
    set_id       uuid NOT NULL REFERENCES asset_sets(id) ON DELETE CASCADE,
    name         text NOT NULL,
    icon         text,
    color        text,
    -- asset tags of this type: LT-0042
    tag_prefix   text NOT NULL,
    next_number  integer NOT NULL DEFAULT 1,
    -- [{id, name, kind: text | number | date | select | checkbox | url | user | asset, options: [{id, label}], typeId (asset: only this type), required}]
    fields       jsonb NOT NULL DEFAULT '[]',
    position     double precision NOT NULL DEFAULT 0
);
CREATE UNIQUE INDEX asset_types_name ON asset_types (set_id, lower(name));

CREATE TABLE asset_categories (
    id         uuid PRIMARY KEY,
    set_id     uuid NOT NULL REFERENCES asset_sets(id) ON DELETE CASCADE,
    parent_id  uuid REFERENCES asset_categories(id) ON DELETE CASCADE,
    name       text NOT NULL
);

CREATE TABLE assets (
    id             uuid PRIMARY KEY,
    workspace_id   uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    set_id         uuid NOT NULL REFERENCES asset_sets(id) ON DELETE CASCADE,
    type_id        uuid NOT NULL REFERENCES asset_types(id) ON DELETE RESTRICT,
    category_id    uuid REFERENCES asset_categories(id) ON DELETE SET NULL,
    tag            text NOT NULL,
    name           text NOT NULL,
    -- in_use | in_stock | maintenance | retired
    status         text NOT NULL DEFAULT 'in_use',
    -- its owner: a person, a team, or someone without an account (a requester's address)
    owner_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    owner_team_id  uuid REFERENCES teams(id) ON DELETE SET NULL,
    owner_email    text,
    -- field id → value
    "values"       jsonb NOT NULL DEFAULT '{}',
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX assets_tag ON assets (workspace_id, upper(tag));
CREATE INDEX assets_set ON assets (set_id, type_id);
CREATE INDEX assets_owner ON assets (owner_id) WHERE owner_id IS NOT NULL;
CREATE INDEX assets_owner_email ON assets (lower(owner_email)) WHERE owner_email IS NOT NULL;

-- source depends on target, or is installed in it
CREATE TABLE asset_relations (
    id         uuid PRIMARY KEY,
    source_id  uuid NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    target_id  uuid NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    -- depends_on | installed_in
    kind       text NOT NULL,
    UNIQUE (source_id, target_id, kind),
    CHECK (source_id <> target_id)
);
CREATE INDEX asset_relations_target ON asset_relations (target_id);

CREATE TABLE asset_events (
    id        uuid PRIMARY KEY,
    asset_id  uuid NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    actor_id  uuid REFERENCES users(id) ON DELETE SET NULL,
    at        timestamptz NOT NULL DEFAULT now(),
    -- created | changed | related | unrelated | issue
    kind      text NOT NULL,
    data      jsonb NOT NULL DEFAULT '{}'
);
CREATE INDEX asset_events_asset ON asset_events (asset_id, at DESC);

-- the assets an issue is about
CREATE TABLE issue_assets (
    issue_id  uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    asset_id  uuid NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, asset_id)
);
CREATE INDEX issue_assets_asset ON issue_assets (asset_id);

-- Automation on assets: when one is created or changes, or some days before a date of its
-- ("warranty ends in 30 days → create an issue").
CREATE TABLE asset_rules (
    id           uuid PRIMARY KEY,
    set_id       uuid NOT NULL REFERENCES asset_sets(id) ON DELETE CASCADE,
    name         text NOT NULL,
    enabled      boolean NOT NULL DEFAULT true,
    -- {event: created | changed | date, typeId?, status?, fieldId (date), days}
    trigger      jsonb NOT NULL,
    -- [{action: createIssue, projectKey, title, description?, assignOwner?}, {action: notifyOwner, message}, {action: setStatus, status}]
    actions      jsonb NOT NULL,
    runs         integer NOT NULL DEFAULT 0,
    last_run_at  timestamptz,
    last_error   text,
    created_at   timestamptz NOT NULL DEFAULT now()
);
-- a date rule acts once per asset and date
CREATE TABLE asset_rule_fires (
    rule_id   uuid NOT NULL REFERENCES asset_rules(id) ON DELETE CASCADE,
    asset_id  uuid NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
    due       date NOT NULL,
    PRIMARY KEY (rule_id, asset_id, due)
);
