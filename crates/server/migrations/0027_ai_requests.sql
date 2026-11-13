-- AI assistance: requests per person in the last hour (a fair share; older ones are removed).
CREATE TABLE ai_requests (
    user_id    uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ai_requests_user ON ai_requests (user_id, created_at);
