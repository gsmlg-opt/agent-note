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

DROP TRIGGER org_events_advance_workspace_sequence;
DROP INDEX idx_org_events_subject;
ALTER TABLE org_events RENAME TO org_events_v3;

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

INSERT INTO org_events (
    id, workspace_id, sequence, subject_kind, subject_id, actor_id,
    attempt_id, event_type, occurred_at, summary, metadata,
    previous_state, resulting_state, validation_version
)
SELECT
    id, workspace_id, sequence, subject_kind, subject_id, actor_id,
    NULL, event_type, occurred_at, summary, metadata, NULL, NULL, 3
FROM org_events_v3;

DROP TABLE org_events_v3;

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
