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
