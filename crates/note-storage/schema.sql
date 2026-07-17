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
CREATE INDEX IF NOT EXISTS idx_note_chunk_embedding ON note_chunk_embeddings(
    libsql_vector_idx(embedding, 'compress_neighbors=float8', 'max_neighbors=20')
);

CREATE TABLE IF NOT EXISTS note_chunk_sparse (
    note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    chunk_idx INTEGER NOT NULL,
    token_id INTEGER NOT NULL,
    weight   REAL NOT NULL,
    PRIMARY KEY (note_id, chunk_idx, token_id)
);
CREATE INDEX IF NOT EXISTS idx_note_chunk_sparse_token ON note_chunk_sparse(token_id);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
