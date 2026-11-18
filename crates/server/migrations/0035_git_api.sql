-- Branches and draft pull requests from an issue (crate::gitapi): a connected repository can
-- carry a token for its server's API; pull requests show their checks, reviews and reviewers.
-- github | gitlab | gitea (also Forgejo); null: webhooks only
ALTER TABLE git_integrations ADD COLUMN api_kind text;
-- the API's address when it can't be told from the repository's (installations under a path)
ALTER TABLE git_integrations ADD COLUMN api_url text;
-- kept in plain text like the webhook secret (the server must send it); never shown again
ALTER TABLE git_integrations ADD COLUMN api_token text;
ALTER TABLE git_integrations ADD COLUMN default_branch text;

ALTER TABLE dev_links ADD COLUMN draft boolean;
-- pull requests: pending | success | failure (checks / CI, as the server last reported)
ALTER TABLE dev_links ADD COLUMN checks text;
-- pull requests: approved | changes_requested
ALTER TABLE dev_links ADD COLUMN review text;
-- [{"name": …, "state": "requested" | "approved" | "changes_requested"}]
ALTER TABLE dev_links ADD COLUMN reviewers jsonb;
-- who made it from the issue (null: it came in through the webhook)
ALTER TABLE dev_links ADD COLUMN created_by uuid REFERENCES users(id) ON DELETE SET NULL;
