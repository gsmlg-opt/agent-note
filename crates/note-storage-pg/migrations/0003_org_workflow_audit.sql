CREATE TABLE org_attempts (
    id text PRIMARY KEY,
    workspace_id text NOT NULL REFERENCES org_workspaces(id) ON DELETE RESTRICT,
    work_item_id text NOT NULL,
    attempt_number bigint NOT NULL CHECK (attempt_number >= 1),
    actor_id text NOT NULL CHECK (length(btrim(actor_id)) > 0),
    status text NOT NULL CHECK (length(btrim(status)) > 0),
    started_at bigint NOT NULL,
    ended_at bigint,
    error text,
    result_summary text,
    review_outcome text,
    note_refs jsonb NOT NULL,
    artifacts jsonb NOT NULL,
    metadata jsonb NOT NULL,
    CHECK (ended_at IS NULL OR ended_at >= started_at)
);

CREATE UNIQUE INDEX idx_org_attempts_item_number
    ON org_attempts(work_item_id, attempt_number);

ALTER TABLE org_events
    ADD COLUMN attempt_id text REFERENCES org_attempts(id) ON DELETE RESTRICT,
    ADD COLUMN previous_state text,
    ADD COLUMN resulting_state text,
    ADD COLUMN validation_version smallint NOT NULL DEFAULT 3
        CHECK (validation_version >= 3),
    ADD CONSTRAINT org_events_subject_kind_nonempty
        CHECK (validation_version = 3 OR length(btrim(subject_kind)) > 0),
    ADD CONSTRAINT org_events_subject_id_nonempty
        CHECK (validation_version = 3 OR length(btrim(subject_id)) > 0),
    ADD CONSTRAINT org_events_actor_id_nonempty
        CHECK (validation_version = 3 OR length(btrim(actor_id)) > 0),
    ADD CONSTRAINT org_events_event_type_nonempty
        CHECK (validation_version = 3 OR length(btrim(event_type)) > 0),
    ADD CONSTRAINT org_events_summary_nonempty
        CHECK (validation_version = 3 OR length(btrim(summary)) > 0),
    ADD CONSTRAINT org_events_state_pair
        CHECK (validation_version = 3 OR (previous_state IS NULL) = (resulting_state IS NULL)),
    ADD CONSTRAINT org_events_previous_state_nonempty
        CHECK (validation_version = 3 OR previous_state IS NULL OR length(btrim(previous_state)) > 0),
    ADD CONSTRAINT org_events_resulting_state_nonempty
        CHECK (validation_version = 3 OR resulting_state IS NULL OR length(btrim(resulting_state)) > 0);

ALTER TABLE org_events ALTER COLUMN validation_version SET DEFAULT 4;

CREATE INDEX idx_org_events_attempt
    ON org_events(attempt_id, workspace_id, sequence);
