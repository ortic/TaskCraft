-- Single sign-on (OpenID Connect, GitHub, LDAP / Active Directory, SAML)

-- External identities: one row per (identity provider, subject), linked to an account
CREATE TABLE identities (
    -- stable key of the identity provider: OIDC issuer, `github:<url>`, `ldap:<id>`, SAML entity id
    provider       text NOT NULL,
    subject        text NOT NULL,
    user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    -- the configured provider (TASKCRAFT_AUTH_PROVIDERS id) it signed in through last
    provider_id    text NOT NULL,
    email          text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_login_at  timestamptz,
    PRIMARY KEY (provider, subject)
);
CREATE INDEX identities_user ON identities (user_id);

-- Pending sign-ins (PKCE verifier, nonce, SAML request id, browser binding), shared by all
-- replicas; ten minutes, single use
CREATE TABLE sso_states (
    state       text PRIMARY KEY,
    data        jsonb NOT NULL,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX sso_states_expires ON sso_states (expires_at);

-- Teams mirrored from a provider's groups (`sso:<provider id>:<group>`)
ALTER TABLE teams ADD COLUMN external_ref text;
CREATE UNIQUE INDEX teams_external_ref ON teams (workspace_id, external_ref) WHERE external_ref IS NOT NULL;
