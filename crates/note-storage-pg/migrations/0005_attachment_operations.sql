CREATE TABLE attachment_operations (
    id text PRIMARY KEY,
    kind text NOT NULL,
    note_id text NOT NULL,
    attachment_id text NOT NULL,
    storage_generation text NOT NULL,
    object_key text NOT NULL,
    status text NOT NULL,
    attempts bigint NOT NULL,
    next_attempt_at bigint,
    lease_owner text,
    lease_expires_at bigint,
    last_error text,
    created_at bigint NOT NULL,
    updated_at bigint NOT NULL,
    CONSTRAINT attachment_operations_kind_check
        CHECK (kind = 'delete_object'),
    CONSTRAINT attachment_operations_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'dead')),
    CONSTRAINT attachment_operations_attempts_check
        CHECK (attempts >= 0),
    CONSTRAINT attachment_operations_identity_check
        CHECK (
            length(btrim(id)) > 0
            AND length(btrim(note_id)) > 0
            AND length(btrim(attachment_id)) > 0
            AND length(btrim(storage_generation)) > 0
            AND length(btrim(object_key)) > 0
        ),
    CONSTRAINT attachment_operations_lease_pair_check
        CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CONSTRAINT attachment_operations_running_lease_check
        CHECK ((status = 'running') = (lease_owner IS NOT NULL)),
    CONSTRAINT attachment_operations_lease_owner_check
        CHECK (lease_owner IS NULL OR length(btrim(lease_owner)) > 0),
    CONSTRAINT attachment_operations_timestamps_check
        CHECK (updated_at >= created_at),
    UNIQUE (kind, object_key)
);

CREATE INDEX idx_attachment_operations_note
    ON attachment_operations(note_id, created_at, id);

CREATE INDEX idx_attachment_operations_claim
    ON attachment_operations(status, next_attempt_at, lease_expires_at, created_at, id);
