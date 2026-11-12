-- E-mail in: every message received (from the IMAP mailbox or the HTTP endpoint) and what
-- became of it. Also deduplicates by Message-ID and rate-limits senders.
CREATE TABLE mail_in (
    id            uuid PRIMARY KEY,
    received_at   timestamptz NOT NULL DEFAULT now(),
    -- imap | http
    source        text NOT NULL,
    -- the message's Message-ID (without <>), or sha256:<hash of the message> when it has none
    message_id    text NOT NULL,
    from_address  text NOT NULL DEFAULT '',
    from_name     text,
    -- the recipient that routed it to a project
    to_address    text,
    subject       text NOT NULL DEFAULT '',
    size          integer NOT NULL DEFAULT 0,
    project_id    uuid REFERENCES projects(id) ON DELETE SET NULL,
    issue_id      uuid REFERENCES issues(id) ON DELETE SET NULL,
    -- processing | created | commented | ignored | rejected | failed | duplicate
    status        text NOT NULL,
    -- why it was ignored, rejected or failed: a code the settings page explains, and details
    reason        text,
    detail        text
);
-- a message is handled once (failed attempts may be retried)
CREATE UNIQUE INDEX mail_in_message ON mail_in (message_id) WHERE status NOT IN ('failed', 'duplicate');
CREATE INDEX mail_in_project ON mail_in (project_id, received_at DESC);
CREATE INDEX mail_in_sender ON mail_in (from_address, received_at);
CREATE INDEX mail_in_received ON mail_in (received_at);

-- How reading the mailbox (TASKCRAFT_IMAP_URL) went last, for the settings page.
CREATE TABLE mail_in_sources (
    source      text PRIMARY KEY,
    checked_at  timestamptz NOT NULL DEFAULT now(),
    ok_at       timestamptz,
    error       text
);

-- The person outside the workspace an issue was opened for by e-mail. Their e-mails become
-- comments; comments marked for them are e-mailed.
CREATE TABLE issue_requesters (
    issue_id         uuid PRIMARY KEY REFERENCES issues(id) ON DELETE CASCADE,
    email            text NOT NULL,
    name             text,
    -- the address they wrote to: replies to them name it as Reply-To
    reply_to         text,
    -- Message-ID of their latest e-mail (e-mails to them continue that thread)
    last_message_id  text,
    created_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX issue_requesters_email ON issue_requesters (email);

-- Comments that came in by e-mail, by requesters (who have no account), and comments e-mailed
-- to the requester.
ALTER TABLE comments
    ADD COLUMN author_email text,
    ADD COLUMN author_name text,
    ADD COLUMN via_email boolean NOT NULL DEFAULT false,
    ADD COLUMN to_requester_at timestamptz;

-- Threading and reply headers of outgoing e-mails: {"messageId", "inReplyTo", "references",
-- "replyTo", "fromName", "autoSubmitted"}.
ALTER TABLE mail_outbox ADD COLUMN headers jsonb;
