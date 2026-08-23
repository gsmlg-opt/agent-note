ALTER TABLE org_documents ADD COLUMN archived_at INTEGER;

PRAGMA user_version = 7;
