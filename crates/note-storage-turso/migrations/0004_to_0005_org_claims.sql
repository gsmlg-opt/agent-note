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
