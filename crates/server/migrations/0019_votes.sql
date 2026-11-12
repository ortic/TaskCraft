-- Votes: people who'd like an issue done (one vote each).
CREATE TABLE issue_votes (
    issue_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_votes_user ON issue_votes (user_id);
