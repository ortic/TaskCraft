-- Signing in to the command-line client through the browser (crate::cli_login): the client asks
-- for a code, the person approves it in the app, the client picks up an API token made for it.
CREATE TABLE cli_logins (
    -- what the person sees and approves (hashed: a table dump opens nothing)
    code_hash      bytea PRIMARY KEY,
    -- what only the client that asked knows; it picks the token up with it
    verifier_hash  bytea NOT NULL UNIQUE,
    -- how the client names itself ("tc on laptop")
    name           text NOT NULL,
    -- who approved (null: not yet)
    user_id        uuid REFERENCES users(id) ON DELETE CASCADE,
    created_at     timestamptz NOT NULL DEFAULT now(),
    expires_at     timestamptz NOT NULL
);
