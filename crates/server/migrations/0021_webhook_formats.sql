-- Webhooks for chat channels: json (the signed event), or a readable message for Slack-compatible
-- channels (slack), Microsoft Teams (teams) or Google Chat (google-chat).
ALTER TABLE webhooks ADD COLUMN format text NOT NULL DEFAULT 'json';
