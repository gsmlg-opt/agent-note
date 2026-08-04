CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    attachments TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    note_revision INTEGER NOT NULL DEFAULT 1,
    deleted_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_notes_title_fts
ON notes USING fts (title);

CREATE TABLE IF NOT EXISTS label_keys (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    value_type TEXT NOT NULL DEFAULT 'text'
);

CREATE TABLE IF NOT EXISTS note_labels (
    note_id      TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id INTEGER NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value        TEXT NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);
CREATE INDEX IF NOT EXISTS idx_note_labels_key_value ON note_labels(label_key_id, value);

CREATE TABLE IF NOT EXISTS note_chunks (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    chunk_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    note_revision INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);
CREATE INDEX IF NOT EXISTS idx_note_chunks_hash ON note_chunks(chunk_hash);
CREATE INDEX IF NOT EXISTS idx_note_chunks_status ON note_chunks(status);

CREATE TABLE IF NOT EXISTS embedding_jobs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    chunk_hash TEXT NOT NULL,
    content TEXT NOT NULL,
    note_revision INTEGER NOT NULL DEFAULT 1,
    status TEXT NOT NULL,
    attempts INTEGER NOT NULL,
    error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(note_id, chunk_idx, chunk_hash)
);
CREATE INDEX IF NOT EXISTS idx_embedding_jobs_status ON embedding_jobs(status, created_at);

CREATE TABLE IF NOT EXISTS note_chunk_embeddings (
    note_id TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    embedding F32_BLOB(1024) NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE org_workspaces (
    id TEXT PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    description TEXT NOT NULL,
    timezone TEXT NOT NULL,
    policy_schema_version INTEGER NOT NULL CHECK (policy_schema_version = 1),
    policy TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    last_event_sequence INTEGER NOT NULL DEFAULT 0 CHECK (last_event_sequence >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    archived_at INTEGER
);

CREATE TABLE org_documents (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    path TEXT NOT NULL,
    source TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 1),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE (workspace_id, path)
);

CREATE TABLE org_work_items (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    document_id TEXT NOT NULL REFERENCES org_documents(id) ON DELETE RESTRICT,
    parent_id TEXT REFERENCES org_work_items(id) ON DELETE RESTRICT,
    source_order INTEGER NOT NULL CHECK (source_order >= 0),
    item_type TEXT NOT NULL,
    title TEXT NOT NULL,
    state TEXT,
    priority TEXT CHECK (priority IS NULL OR length(priority) = 1),
    scheduled_raw TEXT,
    scheduled_local TEXT,
    scheduled_timezone TEXT,
    scheduled_utc INTEGER,
    deadline_raw TEXT,
    deadline_local TEXT,
    deadline_timezone TEXT,
    deadline_utc INTEGER,
    assignee TEXT,
    requires_review INTEGER NOT NULL CHECK (requires_review IN (0, 1)),
    created_at INTEGER NOT NULL,
    UNIQUE (document_id, source_order)
);

CREATE INDEX idx_org_work_items_workspace_document
    ON org_work_items(workspace_id, document_id, source_order);

CREATE TABLE org_work_item_tags (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    PRIMARY KEY (work_item_id, tag)
);

CREATE TABLE org_dependencies (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    depends_on_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE RESTRICT,
    PRIMARY KEY (work_item_id, depends_on_id),
    CHECK (work_item_id <> depends_on_id)
);

CREATE TABLE org_note_links (
    work_item_id TEXT NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    purpose TEXT NOT NULL,
    note_id TEXT NOT NULL,
    description TEXT NOT NULL,
    PRIMARY KEY (work_item_id, ordinal)
);

CREATE INDEX idx_org_note_links_note_id ON org_note_links(note_id, work_item_id);

CREATE TABLE org_events (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    subject_kind TEXT NOT NULL,
    subject_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    summary TEXT NOT NULL,
    metadata TEXT NOT NULL,
    UNIQUE (workspace_id, sequence)
);

CREATE TRIGGER org_events_advance_workspace_sequence
AFTER INSERT ON org_events
FOR EACH ROW
BEGIN
    SELECT CASE
        WHEN NEW.sequence <> (
            SELECT last_event_sequence + 1
            FROM org_workspaces
            WHERE id = NEW.workspace_id
        )
        THEN RAISE(ABORT, 'Org event sequence is not next for workspace')
    END;
    UPDATE org_workspaces
    SET last_event_sequence = NEW.sequence
    WHERE id = NEW.workspace_id;
END;

CREATE INDEX idx_org_events_subject
    ON org_events(workspace_id, subject_kind, subject_id, sequence);

CREATE TABLE org_operations (
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    operation_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
