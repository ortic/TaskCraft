-- Workflow approvals: a status can wait for a decision. Its `approval` says who is asked, in
-- which steps, what it takes (quorum) and where the issue goes when it's approved or rejected
-- (crate::approvals). An issue entering the status opens a request; leaving it another way
-- cancels it.
ALTER TABLE statuses ADD COLUMN approval jsonb;

CREATE TABLE approvals (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    issue_id     uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    status_id    uuid REFERENCES statuses(id) ON DELETE SET NULL,
    status_name  text NOT NULL,
    -- pending | approved | rejected | cancelled
    state        text NOT NULL DEFAULT 'pending',
    -- the status's approval as it was when the request opened
    config       jsonb NOT NULL,
    -- who moved the issue into the status (nobody: automation)
    requested_by uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    decided_at   timestamptz,
    -- the issue was moved on after the decision
    applied_at   timestamptz
);
CREATE UNIQUE INDEX approvals_pending ON approvals (issue_id) WHERE state = 'pending';
CREATE INDEX approvals_issue ON approvals (issue_id, created_at);

CREATE TABLE approval_steps (
    id           uuid PRIMARY KEY,
    approval_id  uuid NOT NULL REFERENCES approvals(id) ON DELETE CASCADE,
    position     integer NOT NULL,
    name         text NOT NULL,
    -- waiting (an earlier step comes first) | pending | approved | rejected | skipped | cancelled
    state        text NOT NULL DEFAULT 'waiting',
    started_at   timestamptz,
    decided_at   timestamptz,
    reminded_at  timestamptz,
    -- the timeout handed it to someone else
    escalated_at timestamptz
);
CREATE INDEX approval_steps_approval ON approval_steps (approval_id, position);
CREATE INDEX approval_steps_pending ON approval_steps (state) WHERE state = 'pending';

-- Who is asked in a step: a person, or a requester by address (the portal).
CREATE TABLE approval_votes (
    id          uuid PRIMARY KEY,
    step_id     uuid NOT NULL REFERENCES approval_steps(id) ON DELETE CASCADE,
    user_id     uuid REFERENCES users(id) ON DELETE CASCADE,
    email       text,
    -- approved | rejected (null: still asked)
    decision    text,
    comment     text,
    decided_at  timestamptz,
    -- asked in this person's place (delegated, or their substitute while they're away)
    for_user_id uuid REFERENCES users(id) ON DELETE SET NULL,
    notified_at timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (user_id IS NOT NULL OR email IS NOT NULL)
);
CREATE UNIQUE INDEX approval_votes_user ON approval_votes (step_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX approval_votes_email ON approval_votes (step_id, lower(email)) WHERE user_id IS NULL;
CREATE INDEX approval_votes_open ON approval_votes (user_id) WHERE decision IS NULL;
