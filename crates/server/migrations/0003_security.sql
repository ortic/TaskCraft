-- Account security: addressable sessions (list / sign out), two-factor authentication with an
-- authenticator app (TOTP) and recovery codes, passkeys (WebAuthn), and the short-lived steps
-- of a sign-in in progress. The workspace setting `requireTwoFactor` (off | admins | everyone)
-- lives in workspaces.settings.

ALTER TABLE sessions ADD COLUMN id uuid NOT NULL DEFAULT gen_random_uuid();
CREATE UNIQUE INDEX sessions_id ON sessions (id);

ALTER TABLE users ADD COLUMN totp_secret text;
-- a secret being set up (active once confirmed with a code)
ALTER TABLE users ADD COLUMN totp_pending text;
ALTER TABLE users ADD COLUMN totp_enabled_at timestamptz;
-- last accepted 30-second step: a code can't be used twice
ALTER TABLE users ADD COLUMN totp_last_step bigint;
-- sha256 hashes (hex) of unused one-time recovery codes
ALTER TABLE users ADD COLUMN recovery_codes text[] NOT NULL DEFAULT '{}';

-- Passkeys (WebAuthn credentials): a second step after the password, or a sign-in on their own
-- when the authenticator verified the person (PIN, fingerprint, face). The WebAuthn user handle
-- is the user's id.
CREATE TABLE passkeys (
    id             uuid PRIMARY KEY,
    user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    credential_id  bytea NOT NULL UNIQUE,
    -- COSE key and its algorithm (-7 ES256, -8 EdDSA, -257 RS256)
    public_key     bytea NOT NULL,
    algorithm      integer NOT NULL,
    sign_count     bigint NOT NULL DEFAULT 0,
    name           text NOT NULL,
    transports     text[] NOT NULL DEFAULT '{}',
    -- synced to the person's other devices (iCloud Keychain, Google Password Manager, …)
    backed_up      boolean NOT NULL DEFAULT false,
    created_at     timestamptz NOT NULL DEFAULT now(),
    last_used_at   timestamptz
);
CREATE INDEX passkeys_user ON passkeys (user_id);

-- Sign-in steps in progress (shared by all replicas, ten minutes at most): the second step
-- after a correct password, adding a passkey, signing in with a passkey alone.
CREATE TABLE auth_challenges (
    token_hash  bytea PRIMARY KEY,
    -- second_step | passkey_add | passkey_login
    kind        text NOT NULL,
    user_id     uuid REFERENCES users(id) ON DELETE CASCADE,
    -- the WebAuthn challenge the answer must sign
    challenge   bytea,
    -- wrong codes in this second step
    attempts    integer NOT NULL DEFAULT 0,
    -- where to go afterwards (sign-ins that arrive by redirect)
    next        text,
    expires_at  timestamptz NOT NULL
);
CREATE INDEX auth_challenges_expires ON auth_challenges (expires_at);
