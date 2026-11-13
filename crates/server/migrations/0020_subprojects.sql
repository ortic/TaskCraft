-- Subprojects: a project can sit under another (three levels at most; no cycles).
ALTER TABLE projects ADD COLUMN parent_id uuid REFERENCES projects(id) ON DELETE SET NULL;
CREATE INDEX projects_parent ON projects (parent_id) WHERE parent_id IS NOT NULL;

-- a project and all projects below it
CREATE FUNCTION tc_project_tree(root uuid) RETURNS SETOF uuid LANGUAGE sql STABLE AS $$
    WITH RECURSIVE t(id, depth) AS (
        SELECT root, 0
        UNION ALL
        SELECT p.id, t.depth + 1 FROM projects p JOIN t ON p.parent_id = t.id WHERE t.depth < 10
    )
    SELECT id FROM t
$$;
