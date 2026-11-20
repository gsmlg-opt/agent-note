-- Keep existing vectors until model fingerprint reconciliation rebuilds them.
-- Storage validates dimensions against the persisted vector-space fingerprint.
ALTER TABLE note_chunk_embeddings ALTER COLUMN embedding TYPE vector;
