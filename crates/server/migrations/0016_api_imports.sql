-- Imports from other trackers' APIs share the jobs of backup imports: what was read is kept like
-- an uploaded backup (in attachment storage, a day at most). Tokens are never stored.
ALTER TABLE backup_imports
    -- xml-site-backup | github | gitlab | redmine
    ADD COLUMN source text NOT NULL DEFAULT 'xml-site-backup',
    -- API sources: the address, the projects asked for and whether a token was given
    ADD COLUMN connection jsonb;
