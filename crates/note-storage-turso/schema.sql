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

CREATE TABLE org_attempts (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    work_item_id TEXT NOT NULL,
    attempt_number INTEGER NOT NULL CHECK (attempt_number >= 1),
    actor_id TEXT NOT NULL CHECK (length(trim(actor_id)) > 0),
    status TEXT NOT NULL CHECK (length(trim(status)) > 0),
    started_at INTEGER NOT NULL,
    ended_at INTEGER,
    error TEXT,
    result_summary TEXT,
    review_outcome TEXT,
    note_refs TEXT NOT NULL CHECK (json_valid(note_refs)),
    artifacts TEXT NOT NULL CHECK (json_valid(artifacts)),
    metadata TEXT NOT NULL CHECK (json_valid(metadata)),
    CHECK (ended_at IS NULL OR ended_at >= started_at)
);

CREATE UNIQUE INDEX idx_org_attempts_item_number
    ON org_attempts(work_item_id, attempt_number);

CREATE TABLE org_leases (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    work_item_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL REFERENCES org_attempts(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK (kind IN ('execution', 'review')),
    actor_id TEXT NOT NULL CHECK (length(trim(actor_id)) > 0),
    fencing_token_hash TEXT NOT NULL CHECK (length(fencing_token_hash) = 64),
    acquired_at INTEGER NOT NULL CHECK (acquired_at > 0),
    last_heartbeat_at INTEGER NOT NULL CHECK (last_heartbeat_at >= acquired_at),
    expires_at INTEGER NOT NULL CHECK (expires_at >= last_heartbeat_at),
    ended_at INTEGER CHECK (ended_at IS NULL OR ended_at >= acquired_at),
    end_reason TEXT CHECK (
        end_reason IS NULL OR end_reason IN (
            'release', 'completion', 'failure', 'block', 'cancellation',
            'lease_expiry', 'review_request', 'approval', 'rejection', 'reassignment'
        )
    ),
    expiry_event_id TEXT,
    CHECK ((ended_at IS NULL) = (end_reason IS NULL)),
    CHECK (
        (expiry_event_id IS NOT NULL) =
        (end_reason IS NOT NULL AND end_reason = 'lease_expiry')
    )
);

CREATE UNIQUE INDEX idx_org_leases_one_open_per_item
    ON org_leases(work_item_id) WHERE ended_at IS NULL;

CREATE INDEX idx_org_leases_workspace_open_expiry
    ON org_leases(workspace_id, expires_at, work_item_id) WHERE ended_at IS NULL;

CREATE INDEX idx_org_leases_item_history
    ON org_leases(work_item_id, acquired_at, id);

CREATE INDEX idx_org_leases_actor_kind
    ON org_leases(actor_id, kind, expires_at) WHERE ended_at IS NULL;

CREATE INDEX idx_org_work_items_operational_order
    ON org_work_items(
        workspace_id, state, priority, deadline_utc, scheduled_utc, created_at, id
    );

CREATE INDEX idx_org_work_items_assignment_order
    ON org_work_items(
        workspace_id, assignee, state, priority, deadline_utc, scheduled_utc, created_at, id
    );

CREATE INDEX idx_org_work_items_schedule_order
    ON org_work_items(
        workspace_id, scheduled_utc, priority, deadline_utc, created_at, id
    );

CREATE INDEX idx_org_work_items_deadline_order
    ON org_work_items(
        workspace_id, deadline_utc, priority, scheduled_utc, created_at, id
    );

CREATE INDEX idx_org_dependencies_target
    ON org_dependencies(depends_on_id, work_item_id);

CREATE TABLE org_events (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL CHECK (sequence >= 1),
    subject_kind TEXT NOT NULL CHECK (validation_version = 3 OR length(trim(subject_kind)) > 0),
    subject_id TEXT NOT NULL CHECK (validation_version = 3 OR length(trim(subject_id)) > 0),
    actor_id TEXT NOT NULL CHECK (validation_version = 3 OR length(trim(actor_id)) > 0),
    attempt_id TEXT REFERENCES org_attempts(id) ON DELETE RESTRICT,
    event_type TEXT NOT NULL CHECK (validation_version = 3 OR length(trim(event_type)) > 0),
    occurred_at INTEGER NOT NULL,
    summary TEXT NOT NULL CHECK (validation_version = 3 OR length(trim(summary)) > 0),
    metadata TEXT NOT NULL CHECK (json_valid(metadata)),
    previous_state TEXT,
    resulting_state TEXT,
    validation_version INTEGER NOT NULL DEFAULT 4 CHECK (validation_version >= 3),
    UNIQUE (workspace_id, sequence),
    CHECK (validation_version = 3 OR (previous_state IS NULL) = (resulting_state IS NULL)),
    CHECK (validation_version = 3 OR previous_state IS NULL OR length(trim(previous_state)) > 0),
    CHECK (validation_version = 3 OR resulting_state IS NULL OR length(trim(resulting_state)) > 0)
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

CREATE INDEX idx_org_events_attempt
    ON org_events(attempt_id, workspace_id, sequence);

CREATE TABLE org_operations (
    workspace_id TEXT NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    operation_id TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    result TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
