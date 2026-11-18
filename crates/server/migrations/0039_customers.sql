-- Customers: who time is for. One list with the service desk's requester organizations: a
-- customer is an organization with a number and a contact, that projects belong to and time
-- logs are booked to; hourly rates; billed periods that are closed.
ALTER TABLE organizations
    ADD COLUMN number      text,
    ADD COLUMN contact     text,
    -- archived: kept in reports, not offered for new work
    ADD COLUMN archived_at timestamptz;

-- a project (or subproject) belongs to a customer by default
ALTER TABLE projects ADD COLUMN customer_id uuid REFERENCES organizations(id) ON DELETE SET NULL;
-- a time log can name another one (a shared support project); null: the project's
ALTER TABLE worklogs ADD COLUMN customer_id uuid REFERENCES organizations(id) ON DELETE SET NULL;
CREATE INDEX worklogs_customer ON worklogs (customer_id) WHERE customer_id IS NOT NULL;

-- The customer of a project: its own, else its parent's, else its parent's parent's.
CREATE FUNCTION tc_project_customer(project uuid) RETURNS uuid LANGUAGE sql STABLE AS $$
    SELECT COALESCE(p.customer_id, p1.customer_id, p2.customer_id)
      FROM projects p LEFT JOIN projects p1 ON p1.id = p.parent_id LEFT JOIN projects p2 ON p2.id = p1.parent_id
     WHERE p.id = project
$$;

-- Hourly rates: for a customer, a time category, a person, or a combination (all null: the
-- default). The most specific one that fits a time log counts.
CREATE TABLE time_rates (
    id           uuid PRIMARY KEY,
    workspace_id uuid NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
    customer_id  uuid REFERENCES organizations(id) ON DELETE CASCADE,
    category_id  uuid REFERENCES time_categories(id) ON DELETE CASCADE,
    user_id      uuid REFERENCES users(id) ON DELETE CASCADE,
    -- per hour, in the workspace's currency
    rate         numeric(12, 2) NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX time_rates_scope ON time_rates (
    workspace_id,
    COALESCE(customer_id, '00000000-0000-0000-0000-000000000000'),
    COALESCE(category_id, '00000000-0000-0000-0000-000000000000'),
    COALESCE(user_id, '00000000-0000-0000-0000-000000000000')
);
