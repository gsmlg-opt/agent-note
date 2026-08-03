CREATE TABLE org_workspaces (
    id text PRIMARY KEY,
    slug text NOT NULL UNIQUE,
    display_name text NOT NULL,
    description text NOT NULL,
    timezone text NOT NULL,
    policy_schema_version bigint NOT NULL CHECK (policy_schema_version = 1),
    policy jsonb NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    last_event_sequence bigint NOT NULL DEFAULT 0 CHECK (last_event_sequence >= 0),
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    archived_at bigint
);

CREATE TABLE org_documents (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    path text NOT NULL,
    source text NOT NULL,
    content_hash text NOT NULL,
    revision bigint NOT NULL CHECK (revision >= 1),
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    UNIQUE (workspace_id, path)
);

CREATE TABLE org_work_items (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    document_id text NOT NULL REFERENCES org_documents(id) ON DELETE RESTRICT,
    parent_id text REFERENCES org_work_items(id) ON DELETE RESTRICT,
    source_order bigint NOT NULL CHECK (source_order >= 0),
    item_type text NOT NULL,
    title text NOT NULL,
    state text,
    priority char(1),
    scheduled_raw text,
    scheduled_local text,
    scheduled_timezone text,
    scheduled_utc bigint,
    deadline_raw text,
    deadline_local text,
    deadline_timezone text,
    deadline_utc bigint,
    assignee text,
    requires_review boolean NOT NULL,
    created_at bigint NOT NULL,
    UNIQUE (document_id, source_order)
);

CREATE INDEX idx_org_work_items_workspace_document
    ON org_work_items(workspace_id, document_id, source_order);

CREATE TABLE org_work_item_tags (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    tag text NOT NULL,
    PRIMARY KEY (work_item_id, tag)
);

CREATE TABLE org_dependencies (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    depends_on_id text NOT NULL REFERENCES org_work_items(id) ON DELETE RESTRICT,
    PRIMARY KEY (work_item_id, depends_on_id),
    CHECK (work_item_id <> depends_on_id)
);

CREATE TABLE org_note_links (
    work_item_id text NOT NULL REFERENCES org_work_items(id) ON DELETE CASCADE,
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    purpose text NOT NULL,
    note_id text NOT NULL,
    description text NOT NULL,
    PRIMARY KEY (work_item_id, ordinal)
);

CREATE INDEX idx_org_note_links_note_id ON org_note_links(note_id, work_item_id);

CREATE TABLE org_events (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    sequence bigint NOT NULL CHECK (sequence >= 1),
    subject_kind text NOT NULL,
    subject_id text NOT NULL,
    actor_id text NOT NULL,
    event_type text NOT NULL,
    occurred_at bigint NOT NULL,
    summary text NOT NULL,
    metadata jsonb NOT NULL,
    UNIQUE (workspace_id, sequence)
);

CREATE INDEX idx_org_events_subject
    ON org_events(workspace_id, subject_kind, subject_id, sequence);

CREATE TABLE org_operations (
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    operation_id text NOT NULL,
    request_fingerprint text NOT NULL,
    result jsonb NOT NULL,
    created_at bigint NOT NULL,
    PRIMARY KEY (workspace_id, operation_id)
);
