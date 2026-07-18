CREATE TABLE notes (
    id text PRIMARY KEY,
    title text NOT NULL,
    content text NOT NULL,
    attachments jsonb NOT NULL DEFAULT '[]'::jsonb,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    deleted_at bigint,
    title_fts tsvector GENERATED ALWAYS AS
        (to_tsvector('simple'::regconfig, title)) STORED
);

CREATE INDEX idx_notes_title_fts ON notes USING gin (title_fts);

CREATE TABLE label_keys (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    key text NOT NULL UNIQUE,
    description text NOT NULL,
    value_type text NOT NULL DEFAULT 'text'
);

CREATE TABLE note_labels (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id bigint NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value text NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);

CREATE INDEX idx_note_labels_key_value
    ON note_labels(label_key_id, value);

CREATE TABLE note_chunks (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    chunk_hash text NOT NULL,
    content text NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    status text NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE INDEX idx_note_chunks_hash ON note_chunks(chunk_hash);
CREATE INDEX idx_note_chunks_status ON note_chunks(status);

CREATE TABLE embedding_jobs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    chunk_hash text NOT NULL,
    content text NOT NULL,
    note_revision bigint NOT NULL DEFAULT 1,
    status text NOT NULL,
    attempts bigint NOT NULL,
    error text,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    UNIQUE(note_id, chunk_idx, chunk_hash)
);

CREATE INDEX idx_embedding_jobs_status
    ON embedding_jobs(status, created_at, id);

CREATE TABLE note_chunk_embeddings (
    note_id text NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx bigint NOT NULL,
    embedding vector(1024) NOT NULL,
    PRIMARY KEY (note_id, chunk_idx)
);

CREATE TABLE app_settings (
    key text PRIMARY KEY,
    value jsonb NOT NULL
);
