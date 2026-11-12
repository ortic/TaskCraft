-- Several releases and components per issue: the releases it's fixed in (fix) and the ones it was
-- found in (affects), and its components.
CREATE TABLE issue_versions (
    issue_id   uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    version_id uuid NOT NULL REFERENCES versions(id) ON DELETE CASCADE,
    kind       text NOT NULL CHECK (kind IN ('fix', 'affects')),
    PRIMARY KEY (issue_id, kind, version_id)
);
CREATE INDEX issue_versions_version ON issue_versions (version_id, kind);

CREATE TABLE issue_components (
    issue_id     uuid NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    component_id uuid NOT NULL REFERENCES components(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, component_id)
);
CREATE INDEX issue_components_component ON issue_components (component_id);

INSERT INTO issue_versions (issue_id, version_id, kind) SELECT id, version_id, 'fix' FROM issues WHERE version_id IS NOT NULL;
INSERT INTO issue_components (issue_id, component_id) SELECT id, component_id FROM issues WHERE component_id IS NOT NULL;
ALTER TABLE issues DROP COLUMN version_id, DROP COLUMN component_id;
