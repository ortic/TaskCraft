-- Service desk portal and public request forms (crate::portal): requesters (people without an
-- account) sign in with a link e-mailed to them, follow and answer their requests; projects
-- can have a public form that opens requests.

-- One-time sign-in links: valid 30 minutes, used once.
CREATE TABLE portal_links (
    token_hash  bytea PRIMARY KEY,
    email       text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);
CREATE INDEX portal_links_email ON portal_links (email, created_at);

-- A requester's portal session (cookie tc_portal): they are their e-mail address.
CREATE TABLE portal_sessions (
    token_hash    bytea PRIMARY KEY,
    email         text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    last_seen_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX portal_sessions_email ON portal_sessions (email);
CREATE INDEX portal_sessions_expires ON portal_sessions (expires_at);

-- What the public pages were asked to do (sign-in links, form submissions, replies), for rate
-- limits: kind (sign-in-ip, sign-in-email, form-ip, form-email) and whose (an address, an IP).
CREATE TABLE portal_attempts (
    kind  text NOT NULL,
    key   text NOT NULL,
    at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX portal_attempts_key ON portal_attempts (kind, key, at);
CREATE INDEX portal_attempts_at ON portal_attempts (at);

-- Signs the time a request form was opened (a minimum fill time keeps bots out). Created on
-- first use, so a restored backup keeps its own.
CREATE TABLE portal_secret (
    id      boolean PRIMARY KEY DEFAULT true CHECK (id),
    secret  text NOT NULL
);

-- How a request reached the project: email | form (the public form) | portal (the form, by a
-- requester signed in to the portal).
ALTER TABLE issue_requesters ADD COLUMN source text NOT NULL DEFAULT 'email';

-- Comments the requester sees (in the portal, and e-mailed to them when mail is set up): those
-- the team marked for them and their own. Comments written in the portal.
ALTER TABLE comments
    ADD COLUMN for_requester boolean NOT NULL DEFAULT false,
    ADD COLUMN via_portal boolean NOT NULL DEFAULT false;
UPDATE comments SET for_requester = true WHERE to_requester_at IS NOT NULL OR (author_id IS NULL AND author_email IS NOT NULL);

-- Files the requester added (by e-mail, the form or the portal): the ones they see.
ALTER TABLE attachments ADD COLUMN by_requester boolean NOT NULL DEFAULT false;
UPDATE attachments a SET by_requester = true
 WHERE a.uploaded_by IS NULL AND EXISTS (SELECT 1 FROM issue_requesters r WHERE r.issue_id = a.issue_id);
