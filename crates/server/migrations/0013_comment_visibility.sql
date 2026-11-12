-- Who may read a comment besides its author and project admins: NULL everyone who sees the
-- issue, 'members' (not viewers or guests), 'admins', or 'team:<team id>'.
ALTER TABLE comments ADD COLUMN visibility text;
