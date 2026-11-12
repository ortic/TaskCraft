-- Outgoing e-mail: an outbox drained by every replica's mail worker (retries with backoff).
CREATE TABLE mail_outbox (
    id               uuid PRIMARY KEY,
    to_address       text NOT NULL,
    subject          text NOT NULL,
    body_text        text NOT NULL,
    body_html        text,
    -- invite | reset | notification | digest (rate limits, admin overview)
    purpose          text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    next_attempt_at  timestamptz NOT NULL DEFAULT now(),
    attempts         integer NOT NULL DEFAULT 0,
    sent_at          timestamptz,
    last_error       text
);
CREATE INDEX mail_outbox_pending ON mail_outbox (next_attempt_at) WHERE sent_at IS NULL;
CREATE INDEX mail_outbox_recent ON mail_outbox (to_address, purpose, created_at);

-- Notification e-mails: each notification is mailed (or skipped) once; preferences.email picks
-- off | instant (bundled a couple of minutes after the first one) | digest (daily, the default).
ALTER TABLE notifications ADD COLUMN emailed_at timestamptz;
UPDATE notifications SET emailed_at = created_at;
CREATE INDEX notifications_email_pending ON notifications (user_id, created_at) WHERE emailed_at IS NULL;
