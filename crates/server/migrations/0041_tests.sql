-- Test management (crate::tests): test cases per project (versioned, in folders, covering
-- issues), test plans, run templates, and test runs with a result per case and step. Every
-- result is also kept as history (test_result_events), which the reports replay.
CREATE TABLE test_folders (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    parent_id   uuid REFERENCES test_folders(id) ON DELETE CASCADE,
    name        text NOT NULL,
    position    double precision NOT NULL DEFAULT 0
);
CREATE INDEX test_folders_project ON test_folders (project_id);

CREATE TABLE test_cases (
    id             uuid PRIMARY KEY,
    project_id     uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- T12 in its project: WEB-T12
    number         integer NOT NULL,
    folder_id      uuid REFERENCES test_folders(id) ON DELETE SET NULL,
    title          text NOT NULL,
    -- Markdown
    preconditions  text,
    -- steps | scenario
    kind           text NOT NULL DEFAULT 'steps',
    -- [{action, expected}] (Markdown each)
    steps          jsonb NOT NULL DEFAULT '[]',
    -- Given / When / Then lines
    scenario       text,
    -- low | medium | high | critical
    priority       text NOT NULL DEFAULT 'medium',
    labels         text[] NOT NULL DEFAULT '{}',
    -- counts up with every change of what a tester does (title, preconditions, steps, scenario)
    version        integer NOT NULL DEFAULT 1,
    archived_at    timestamptz,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (project_id, number)
);
CREATE INDEX test_cases_folder ON test_cases (folder_id);
ALTER TABLE projects ADD COLUMN next_test_number integer NOT NULL DEFAULT 1;
ALTER TABLE projects ADD COLUMN next_test_run_number integer NOT NULL DEFAULT 1;

-- what a test case said at each version: a run shows the version it executed
CREATE TABLE test_case_versions (
    case_id        uuid NOT NULL REFERENCES test_cases(id) ON DELETE CASCADE,
    version        integer NOT NULL,
    title          text NOT NULL,
    preconditions  text,
    kind           text NOT NULL,
    steps          jsonb NOT NULL,
    scenario       text,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (case_id, version)
);

-- coverage: the issues (stories, requirements) a test case checks
CREATE TABLE test_case_issues (
    case_id   uuid NOT NULL REFERENCES test_cases(id) ON DELETE CASCADE,
    issue_id  uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    PRIMARY KEY (case_id, issue_id)
);
CREATE INDEX test_case_issues_issue ON test_case_issues (issue_id);

CREATE TABLE test_plans (
    id           uuid PRIMARY KEY,
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name         text NOT NULL,
    description  text,
    -- the release it checks
    version_id   uuid REFERENCES versions(id) ON DELETE SET NULL,
    -- cases picked by a filter, next to the ones picked by hand: {folderId, labels: [], priority, text}
    filter       jsonb,
    created_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX test_plans_name ON test_plans (project_id, lower(name));

CREATE TABLE test_plan_cases (
    plan_id   uuid NOT NULL REFERENCES test_plans(id) ON DELETE CASCADE,
    case_id   uuid NOT NULL REFERENCES test_cases(id) ON DELETE CASCADE,
    position  integer NOT NULL DEFAULT 0,
    PRIMARY KEY (plan_id, case_id)
);

-- a plan with an environment and testers: a run in one click
CREATE TABLE test_run_templates (
    id           uuid PRIMARY KEY,
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name         text NOT NULL,
    plan_id      uuid NOT NULL REFERENCES test_plans(id) ON DELETE CASCADE,
    environment  text,
    tester_ids   uuid[] NOT NULL DEFAULT '{}'
);
CREATE UNIQUE INDEX test_run_templates_name ON test_run_templates (project_id, lower(name));

CREATE TABLE test_runs (
    id           uuid PRIMARY KEY,
    project_id   uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- R7 in its project
    number       integer NOT NULL,
    name         text NOT NULL,
    plan_id      uuid REFERENCES test_plans(id) ON DELETE SET NULL,
    version_id   uuid REFERENCES versions(id) ON DELETE SET NULL,
    environment  text,
    tester_ids   uuid[] NOT NULL DEFAULT '{}',
    created_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    finished_at  timestamptz,
    UNIQUE (project_id, number)
);
CREATE INDEX test_runs_version ON test_runs (version_id);

CREATE TABLE test_run_cases (
    id            uuid PRIMARY KEY,
    run_id        uuid NOT NULL REFERENCES test_runs(id) ON DELETE CASCADE,
    case_id       uuid NOT NULL REFERENCES test_cases(id) ON DELETE CASCADE,
    -- the version of the case this run executes
    case_version  integer NOT NULL,
    position      integer NOT NULL DEFAULT 0,
    assignee_id   uuid REFERENCES users(id) ON DELETE SET NULL,
    -- untested | passed | failed | blocked | skipped
    result        text NOT NULL DEFAULT 'untested',
    comment       text,
    -- [{result, comment}] by step, as long as the case's steps
    steps         jsonb NOT NULL DEFAULT '[]',
    executed_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    executed_at   timestamptz,
    UNIQUE (run_id, case_id)
);
CREATE INDEX test_run_cases_case ON test_run_cases (case_id);

-- screenshots and other files on a result (step: the step they belong to, if any)
CREATE TABLE test_files (
    id           uuid PRIMARY KEY,
    run_case_id  uuid NOT NULL REFERENCES test_run_cases(id) ON DELETE CASCADE,
    step         integer,
    name         text NOT NULL,
    mime         text NOT NULL,
    size         bigint NOT NULL,
    storage_key  text NOT NULL,
    uploaded_by  uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX test_files_run_case ON test_files (run_case_id);

-- bugs created from a failure
CREATE TABLE test_bugs (
    run_case_id  uuid NOT NULL REFERENCES test_run_cases(id) ON DELETE CASCADE,
    issue_id     uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    PRIMARY KEY (run_case_id, issue_id)
);
CREATE INDEX test_bugs_issue ON test_bugs (issue_id);

-- the history of results: one row whenever a case's result in a run changes
CREATE TABLE test_result_events (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    run_id      uuid NOT NULL REFERENCES test_runs(id) ON DELETE CASCADE,
    case_id     uuid NOT NULL REFERENCES test_cases(id) ON DELETE CASCADE,
    result      text NOT NULL,
    actor_id    uuid REFERENCES users(id) ON DELETE SET NULL,
    at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX test_result_events_case ON test_result_events (case_id, at);
CREATE INDEX test_result_events_project ON test_result_events (project_id, at);
