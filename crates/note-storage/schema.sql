CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS label_keys (
    id INTEGER PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS note_labels (
    note_id      TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    label_key_id INTEGER NOT NULL REFERENCES label_keys(id) ON DELETE CASCADE,
    value        TEXT NOT NULL,
    PRIMARY KEY (note_id, label_key_id)
);
CREATE INDEX IF NOT EXISTS idx_note_labels_key_value ON note_labels(label_key_id, value);

CREATE TABLE IF NOT EXISTS notes_embeddings (
    note_id TEXT PRIMARY KEY REFERENCES notes(id) ON DELETE CASCADE,
    embedding F32_BLOB(1024) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_notes_embedding ON notes_embeddings(libsql_vector_idx(embedding));

CREATE TABLE IF NOT EXISTS notes_sparse_weights (
    note_id  TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
    token_id INTEGER NOT NULL,
    weight   REAL NOT NULL,
    PRIMARY KEY (note_id, token_id)
);
CREATE INDEX IF NOT EXISTS idx_sparse_token ON notes_sparse_weights(token_id);
