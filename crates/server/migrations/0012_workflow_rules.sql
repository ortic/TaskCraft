-- Rules for moving an issue into a status: who may, which fields it needs, whether a comment is
-- asked for (see crate::workflow).
ALTER TABLE statuses ADD COLUMN rules jsonb NOT NULL DEFAULT '{}';

-- Why a done issue is done (Done, Won't do, Duplicate, …); empty while it's open.
ALTER TABLE issues ADD COLUMN resolution text;
UPDATE issues i SET resolution = 'Done'
  FROM statuses s
 WHERE s.id = i.status_id AND s.category = 'done' AND i.resolution IS NULL;
