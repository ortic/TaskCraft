-- Private issues: a project's security levels say who sees the issues that carry one. Besides
-- those listed (a minimum project role, teams, people), project admins and the issue's reporter,
-- assignee and creator always see it.
CREATE TABLE security_levels (
    id          uuid PRIMARY KEY,
    project_id  uuid NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    name        text NOT NULL,
    description text NOT NULL DEFAULT '',
    position    double precision NOT NULL DEFAULT 0,
    -- everyone with at least this project role (1 viewer, 2 member); NULL: only those listed
    min_rank    integer,
    team_ids    uuid[] NOT NULL DEFAULT '{}',
    user_ids    uuid[] NOT NULL DEFAULT '{}',
    -- new issues get it unless the person creating them chooses otherwise
    is_default  boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX security_levels_name ON security_levels (project_id, lower(name));

-- a level in use can't simply go away (its issues would turn public): it's replaced first (NO ACTION,
-- not RESTRICT, so deleting a whole project still cascades)
ALTER TABLE issues ADD COLUMN security_level_id uuid REFERENCES security_levels(id);
CREATE INDEX issues_security_level ON issues (security_level_id) WHERE security_level_id IS NOT NULL;

-- the security levels whose issues a person sees
CREATE FUNCTION tc_visible_levels(uid uuid) RETURNS SETOF uuid LANGUAGE sql STABLE AS $$
    SELECT l.id
      FROM security_levels l
      JOIN tc_project_roles(uid) r ON r.project_id = l.project_id
     WHERE r.rank >= 3
        OR (r.rank >= 1 AND l.min_rank IS NOT NULL AND r.rank >= l.min_rank)
        OR (r.rank >= 1 AND uid = ANY(l.user_ids))
        OR (r.rank >= 1 AND l.team_ids && ARRAY(SELECT tm.team_id FROM team_members tm WHERE tm.user_id = uid))
$$;
