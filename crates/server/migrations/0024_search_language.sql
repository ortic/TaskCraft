-- Search in the workspace's language: stemmed words next to the exact ones, so "invoices" finds
-- "invoice" and "running" finds "runs"; prefixes still match while typing. And search inside
-- files on issues (their text is read in the background).

-- The text search configuration for a workspace's language (English when none is set).
CREATE FUNCTION tc_search_lang(ws uuid) RETURNS regconfig LANGUAGE sql STABLE AS $$
    SELECT (CASE (SELECT settings->>'defaultLanguage' FROM workspaces WHERE id = ws)
        WHEN 'de' THEN 'german' WHEN 'fr' THEN 'french' WHEN 'it' THEN 'italian' ELSE 'english' END)::regconfig
$$;

ALTER TABLE issues DROP COLUMN search;
ALTER TABLE issues ADD COLUMN search_lang regconfig;
UPDATE issues SET search_lang = tc_search_lang(workspace_id);
ALTER TABLE issues ADD COLUMN search tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('simple', coalesce(title, '')), 'A') || setweight(to_tsvector('simple', coalesce(description_text, '')), 'B') ||
    setweight(to_tsvector(coalesce(search_lang, 'english'::regconfig), coalesce(title, '')), 'A') ||
    setweight(to_tsvector(coalesce(search_lang, 'english'::regconfig), coalesce(description_text, '')), 'B')
) STORED;
CREATE INDEX issues_search ON issues USING gin (search);

-- new issues take their workspace's language (a restored backup keeps the one it has)
CREATE FUNCTION tc_issue_search_lang() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.search_lang IS NULL THEN
        NEW.search_lang := tc_search_lang(NEW.workspace_id);
    END IF;
    RETURN NEW;
END
$$;
CREATE TRIGGER issues_search_lang BEFORE INSERT ON issues FOR EACH ROW EXECUTE FUNCTION tc_issue_search_lang();

-- files: their text (NULL: not read yet; empty: none) and its words
ALTER TABLE attachments ADD COLUMN text_content text;
ALTER TABLE attachments ADD COLUMN search_lang regconfig;
ALTER TABLE attachments ADD COLUMN search tsvector GENERATED ALWAYS AS (
    to_tsvector('simple', coalesce(text_content, '')) || to_tsvector(coalesce(search_lang, 'english'::regconfig), coalesce(text_content, ''))
) STORED;
CREATE INDEX attachments_search ON attachments USING gin (search);
CREATE INDEX attachments_unread ON attachments (created_at) WHERE text_content IS NULL;

-- file names (`text ~` finds "invoice-2026.pdf"), like issue titles in 0023
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_trgm') THEN
        EXECUTE 'CREATE INDEX IF NOT EXISTS attachments_name_trgm ON attachments USING gin (name gin_trgm_ops)';
    END IF;
END
$$;
