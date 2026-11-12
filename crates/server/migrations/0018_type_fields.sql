-- Fields per issue type: which fields a type hides and which ones it requires
-- ({"hidden": [field], "required": [field]}; custom fields as `custom:<id>`).
ALTER TABLE issue_types ADD COLUMN fields jsonb NOT NULL DEFAULT '{}';
