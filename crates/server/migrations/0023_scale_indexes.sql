-- Indexes found with the scale check (100 000 issues, crates/server/tests/scale.rs).

-- "reported by me" (your work) and epics (the portfolio, backlogs)
CREATE INDEX IF NOT EXISTS issues_reporter ON issues (reporter_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS issues_type ON issues (type_id) WHERE deleted_at IS NULL;
-- created vs resolved, newest first, recently resolved
CREATE INDEX IF NOT EXISTS issues_project_created ON issues (project_id, created_at) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS issues_project_resolved ON issues (project_id, resolved_at) WHERE resolved_at IS NOT NULL AND deleted_at IS NULL;
-- the activity feed across projects
CREATE INDEX IF NOT EXISTS issue_events_created ON issue_events (created_at);

-- Words inside titles (`title ILIKE '%…%'`, the command palette): a trigram index when the
-- pg_trgm extension can be created (it ships with PostgreSQL; managed services allow it).
-- Without it searches still work, they just read every issue.
DO $$
BEGIN
    BEGIN
        CREATE EXTENSION IF NOT EXISTS pg_trgm;
    EXCEPTION WHEN OTHERS THEN
        RAISE NOTICE 'pg_trgm not available: title search without an index (%)', SQLERRM;
    END;
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm') THEN
        EXECUTE 'CREATE INDEX IF NOT EXISTS issues_title_trgm ON issues USING gin (title gin_trgm_ops)';
    END IF;
END
$$;
