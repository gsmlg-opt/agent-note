CREATE TABLE IF NOT EXISTS sync_records (namespace TEXT NOT NULL, record_key TEXT NOT NULL, value JSONB NOT NULL, PRIMARY KEY(namespace, record_key));
