-- keys an issue had before it moved to another project: old links keep working
CREATE TABLE issue_key_aliases (
    workspace_id  uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    project_key   text NOT NULL,
    number        integer NOT NULL,
    issue_id      uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    PRIMARY KEY (workspace_id, project_key, number)
);
CREATE INDEX issue_key_aliases_issue ON issue_key_aliases (issue_id);
