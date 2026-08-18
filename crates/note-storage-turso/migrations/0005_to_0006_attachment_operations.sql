CREATE TABLE attachment_operations (
    id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
    kind TEXT NOT NULL CHECK (kind = 'delete_object'),
    note_id TEXT NOT NULL CHECK (length(trim(note_id)) > 0),
    attachment_id TEXT NOT NULL CHECK (length(trim(attachment_id)) > 0),
    storage_generation TEXT NOT NULL CHECK (length(trim(storage_generation)) > 0),
    object_key TEXT NOT NULL UNIQUE CHECK (length(trim(object_key)) > 0),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'dead')),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    next_attempt_at INTEGER,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (updated_at >= created_at),
    CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK (lease_owner IS NULL OR length(trim(lease_owner)) > 0),
    CHECK ((status = 'running') = (lease_owner IS NOT NULL))
);

CREATE INDEX idx_attachment_operations_note
    ON attachment_operations(note_id, created_at, id);

CREATE INDEX idx_attachment_operations_claim
    ON attachment_operations(status, next_attempt_at, lease_expires_at, created_at, id);
