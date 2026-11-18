-- SCIM 2.0 provisioning (crate::scim): an identity provider creates, updates and deactivates
-- people and keeps teams in step, with a bearer token an admin made for it.
CREATE TABLE scim_tokens (
    id            uuid PRIMARY KEY,
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    name          text NOT NULL,
    token_hash    bytea NOT NULL UNIQUE,
    -- the single sign-on provider this directory signs people in with: its groups are the teams
    -- the sign-in group sync manages (null: teams of this token's own)
    provider_id   text,
    -- member | guest: the role of people it creates
    default_role  text NOT NULL DEFAULT 'member',
    created_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    last_used_at  timestamptz,
    requests      bigint NOT NULL DEFAULT 0,
    last_error    text,
    last_error_at timestamptz
);

-- The people a token provisions (created by it, or existing accounts it took over by address).
CREATE TABLE scim_users (
    user_id      uuid PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    token_id     uuid NOT NULL REFERENCES scim_tokens(id) ON DELETE CASCADE,
    external_id  text,
    user_name    text NOT NULL,
    given_name   text,
    family_name  text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX scim_users_name ON scim_users (token_id, lower(user_name));

-- The teams a token provisions.
CREATE TABLE scim_groups (
    team_id      uuid PRIMARY KEY REFERENCES teams(id) ON DELETE CASCADE,
    token_id     uuid NOT NULL REFERENCES scim_tokens(id) ON DELETE CASCADE,
    external_id  text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);
