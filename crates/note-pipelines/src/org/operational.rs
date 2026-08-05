use super::{
    decode_cursor, encode_cursor, query_fingerprint, OperationalItemSummary, OperationalPage,
    OperationalView, OrgContext, OrgError, OrgErrorCode, OrgItemView, OrgLeaseView,
    OrgOperationalPage, OrgReadyStatus, OrgReviewLeaseStatus, OrgTimestampView,
};
use note_org::{
    evaluate_readiness, AttemptPhase, ClaimQueue, LeaseKind, LeaseStatus, Readiness,
    ReadinessContext, WorkItemType, WorkspaceId,
};
use note_storage::{
    OrgAttemptStatus, OrgOperationalQuery as StorageQuery, OrgOperationalRow as StorageRow,
    OrgOperationalView as StorageView, OrgProjectedWorkItem, OrgReadyMarker, OrgReviewLeaseMarker,
    StorageTransaction, TransactionMode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;

pub const DEFAULT_OPERATIONAL_LIMIT: usize = 50;
pub const MAX_OPERATIONAL_LIMIT: usize = 200;
const STORAGE_BATCH_LIMIT: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationalQueryFamily {
    Queue,
    Agenda,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationalQuery {
    pub workspace_ids: Vec<WorkspaceId>,
    pub view: OperationalView,
    pub item_type: Option<WorkItemType>,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub scheduled_from: Option<i64>,
    pub scheduled_to: Option<i64>,
    pub deadline_from: Option<i64>,
    pub deadline_to: Option<i64>,
    pub completed_from: Option<i64>,
    pub completed_to: Option<i64>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    pub include_archived: bool,
    pub cursor: Option<String>,
    pub limit: Option<usize>,
}

impl OperationalQuery {
    pub fn for_workspace(workspace_id: WorkspaceId, view: OperationalView) -> Self {
        Self {
            workspace_ids: vec![workspace_id],
            view,
            item_type: None,
            state: None,
            priority: None,
            tags: Vec::new(),
            assignee: None,
            scheduled_from: None,
            scheduled_to: None,
            deadline_from: None,
            deadline_to: None,
            completed_from: None,
            completed_to: None,
            from: None,
            to: None,
            include_archived: false,
            cursor: None,
            limit: None,
        }
    }

    pub(crate) fn normalized_fingerprint_value(
        &self,
        family: OperationalQueryFamily,
    ) -> Result<serde_json::Value, OrgError> {
        let normalized = NormalizedQuery::new(family, self)?;
        Ok(json!({
            "family": family,
            "view": normalized.view,
            "workspace_ids": normalized.workspace_ids,
            "item_type": normalized.item_type,
            "state": normalized.state,
            "priority": normalized.priority,
            "tags": normalized.tags,
            "assignee": normalized.assignee,
            "scheduled_from": normalized.scheduled_from,
            "scheduled_to": normalized.scheduled_to,
            "deadline_from": normalized.deadline_from,
            "deadline_to": normalized.deadline_to,
            "completed_from": normalized.completed_from,
            "completed_to": normalized.completed_to,
            "from": normalized.from,
            "to": normalized.to,
            "include_archived": normalized.include_archived,
        }))
    }
}

pub async fn query_queue(
    context: &OrgContext,
    query: &OperationalQuery,
) -> Result<OrgOperationalPage, OrgError> {
    query_operational(context, OperationalQueryFamily::Queue, query).await
}

pub async fn query_agenda(
    context: &OrgContext,
    query: &OperationalQuery,
) -> Result<OrgOperationalPage, OrgError> {
    query_operational(context, OperationalQueryFamily::Agenda, query).await
}

async fn query_operational(
    context: &OrgContext,
    family: OperationalQueryFamily,
    query: &OperationalQuery,
) -> Result<OperationalPage, OrgError> {
    let normalized = NormalizedQuery::new(family, query)?;
    let fingerprint = query_fingerprint(family, query)?;
    let decoded = query
        .cursor
        .as_deref()
        .map(|cursor| {
            decode_cursor(
                context.cursor_signer(),
                cursor,
                &fingerprint,
                normalized.view,
            )
        })
        .transpose()?;
    let evaluated_at = decoded
        .as_ref()
        .map_or_else(|| context.clock().now(), |cursor| cursor.evaluated_at);
    if evaluated_at <= 0 {
        return Err(OrgError::invalid_input(
            "Org operational evaluation time must be positive",
        ));
    }
    let transaction = context
        .storage()
        .begin(TransactionMode::Deferred)
        .await
        .map_err(OrgError::storage)?;
    let result = query_in_transaction(
        transaction.as_ref(),
        &normalized,
        &fingerprint,
        evaluated_at,
        decoded.map(|cursor| cursor.last_scanned),
        context.cursor_signer(),
    )
    .await;
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

struct NormalizedQuery {
    family: OperationalQueryFamily,
    workspace_ids: Vec<WorkspaceId>,
    view: OperationalView,
    item_type: Option<WorkItemType>,
    state: Option<String>,
    priority: Option<char>,
    tags: Vec<String>,
    assignee: Option<String>,
    scheduled_from: Option<i64>,
    scheduled_to: Option<i64>,
    deadline_from: Option<i64>,
    deadline_to: Option<i64>,
    completed_from: Option<i64>,
    completed_to: Option<i64>,
    from: Option<i64>,
    to: Option<i64>,
    include_archived: bool,
    limit: usize,
}

impl NormalizedQuery {
    fn new(family: OperationalQueryFamily, query: &OperationalQuery) -> Result<Self, OrgError> {
        if query.workspace_ids.is_empty() {
            return Err(OrgError::invalid_input(
                "Org operational queries require at least one explicit workspace ID",
            ));
        }
        if (family == OperationalQueryFamily::Agenda) != query.view.is_agenda() {
            return Err(OrgError::invalid_input(
                "Org operational view does not belong to the requested query family",
            ));
        }
        let limit = query.limit.unwrap_or(DEFAULT_OPERATIONAL_LIMIT);
        if !(1..=MAX_OPERATIONAL_LIMIT).contains(&limit) {
            return Err(OrgError::invalid_input(
                "Org operational limit must be between 1 and 200",
            ));
        }
        let mut workspace_ids = query.workspace_ids.clone();
        workspace_ids.sort();
        workspace_ids.dedup();
        let mut tags = query
            .tags
            .iter()
            .map(|tag| trimmed_nonempty(tag, "tag"))
            .collect::<Result<Vec<_>, _>>()?;
        tags.sort();
        tags.dedup();
        let state = query
            .state
            .as_deref()
            .map(|value| trimmed_nonempty(value, "state"))
            .transpose()?;
        let assignee = query
            .assignee
            .as_deref()
            .map(|value| trimmed_nonempty(value, "assignee"))
            .transpose()?;
        if query
            .priority
            .is_some_and(|value| !value.is_ascii_uppercase())
        {
            return Err(OrgError::invalid_input(
                "Org priority filter must be an uppercase ASCII letter",
            ));
        }
        for (from, to) in [
            (query.scheduled_from, query.scheduled_to),
            (query.deadline_from, query.deadline_to),
            (query.completed_from, query.completed_to),
            (query.from, query.to),
        ] {
            if from.zip(to).is_some_and(|(from, to)| from > to) {
                return Err(OrgError::invalid_input(
                    "Org operational time range is invalid",
                ));
            }
        }
        Ok(Self {
            family,
            workspace_ids,
            view: query.view,
            item_type: query.item_type,
            state,
            priority: query.priority,
            tags,
            assignee,
            scheduled_from: query.scheduled_from,
            scheduled_to: query.scheduled_to,
            deadline_from: query.deadline_from,
            deadline_to: query.deadline_to,
            completed_from: query.completed_from,
            completed_to: query.completed_to,
            from: query.from,
            to: query.to,
            include_archived: query.include_archived,
            limit,
        })
    }
}

async fn query_in_transaction(
    transaction: &dyn StorageTransaction,
    query: &NormalizedQuery,
    fingerprint: &str,
    evaluated_at: i64,
    mut after: Option<note_storage::OrgOperationalCursor>,
    cursor_signer: &dyn super::OrgCursorSigner,
) -> Result<OperationalPage, OrgError> {
    let readiness = preload_readiness(transaction, query, evaluated_at).await?;
    let tag_refs = query.tags.iter().map(String::as_str).collect::<Vec<_>>();
    let mut items = Vec::with_capacity(query.limit);
    let mut next_cursor = None;
    loop {
        let storage_query = storage_query(query, evaluated_at, after.as_ref(), &tag_refs);
        let batch = transaction
            .query_org_operational(storage_query)
            .await
            .map_err(OrgError::storage)?;
        let batch_len = batch.len();
        for (index, row) in batch.iter().enumerate() {
            let cursor = row.cursor(storage_view(query.view));
            let eligible = query.view != OperationalView::Ready
                || readiness
                    .get(&row.item.workspace_id)
                    .is_some_and(|context| context.is_ready(&row));
            after = Some(cursor.clone());
            if eligible {
                items.push(map_operational_row(row.clone(), evaluated_at));
                if items.len() == query.limit {
                    let remaining_has_eligible = batch[index + 1..].iter().any(|candidate| {
                        query.view != OperationalView::Ready
                            || readiness
                                .get(&candidate.item.workspace_id)
                                .is_some_and(|context| context.is_ready(candidate))
                    });
                    let has_more = remaining_has_eligible
                        || (batch_len == STORAGE_BATCH_LIMIT
                            && has_next_eligible(
                                transaction,
                                query,
                                evaluated_at,
                                batch.last().map(|row| row.cursor(storage_view(query.view))),
                                &readiness,
                                &tag_refs,
                            )
                            .await?);
                    if has_more {
                        next_cursor = Some(encode_cursor(
                            cursor_signer,
                            fingerprint,
                            evaluated_at,
                            &cursor,
                        )?);
                    }
                    return Ok(OperationalPage {
                        items,
                        next_cursor,
                        evaluated_at,
                    });
                }
            }
        }
        if batch_len < STORAGE_BATCH_LIMIT {
            break;
        }
    }
    Ok(OperationalPage {
        items,
        next_cursor,
        evaluated_at,
    })
}

async fn has_next_eligible(
    transaction: &dyn StorageTransaction,
    query: &NormalizedQuery,
    evaluated_at: i64,
    mut after: Option<note_storage::OrgOperationalCursor>,
    readiness: &BTreeMap<WorkspaceId, WorkspaceReadiness>,
    tags: &[&str],
) -> Result<bool, OrgError> {
    loop {
        let rows = transaction
            .query_org_operational(storage_query(query, evaluated_at, after.as_ref(), tags))
            .await
            .map_err(OrgError::storage)?;
        let len = rows.len();
        if rows.iter().any(|row| {
            query.view != OperationalView::Ready
                || readiness
                    .get(&row.item.workspace_id)
                    .is_some_and(|context| context.is_ready(row))
        }) {
            return Ok(true);
        }
        after = rows.last().map(|row| row.cursor(storage_view(query.view)));
        if len < STORAGE_BATCH_LIMIT {
            return Ok(false);
        }
    }
}

struct WorkspaceReadiness {
    workspace: note_storage::OrgWorkspace,
    dependencies: note_org::DependencyGraph,
    active_count: i64,
    evaluated_at: i64,
    actor_id: Option<String>,
}

impl WorkspaceReadiness {
    fn is_ready(&self, row: &StorageRow) -> bool {
        let lease = row
            .lease
            .as_ref()
            .map(|lease| {
                let kind = match lease.kind {
                    note_storage::OrgLeaseKind::Execution => LeaseKind::Execution,
                    note_storage::OrgLeaseKind::Review => LeaseKind::Review,
                };
                if lease.expires_at <= self.evaluated_at {
                    LeaseStatus::Expired(kind)
                } else {
                    LeaseStatus::Active(kind)
                }
            })
            .unwrap_or(LeaseStatus::None);
        let context = ReadinessContext {
            now: self.evaluated_at,
            dependencies_satisfied: self.dependencies.dependencies_satisfied(row.item.id),
            workspace_active: self.workspace.archived_at.is_none(),
            lease,
            capacity_available: self.active_count < self.workspace.policy.concurrency_limit as i64,
            actor_id: self.actor_id.clone(),
            scheduled_at: row.item.scheduled.as_ref().map(|value| value.utc_timestamp),
            queue: ClaimQueue::Execution,
            execution_attempt_count: u32::try_from(row.attempt_count).unwrap_or(u32::MAX),
            current_attempt: row.current_attempt_status.map(attempt_phase),
        };
        matches!(
            evaluate_readiness(
                &super::projected_to_domain(&row.item),
                &self.workspace.policy,
                &context,
            ),
            Readiness::Ready | Readiness::RecoveryCandidate
        )
    }
}

async fn preload_readiness(
    transaction: &dyn StorageTransaction,
    query: &NormalizedQuery,
    evaluated_at: i64,
) -> Result<BTreeMap<WorkspaceId, WorkspaceReadiness>, OrgError> {
    let mut result = BTreeMap::new();
    for workspace_id in &query.workspace_ids {
        let workspace = transaction
            .get_org_workspace(*workspace_id)
            .await
            .map_err(OrgError::storage)?
            .ok_or_else(|| not_found("workspace", workspace_id.to_string()))?;
        if query.workspace_ids.len() > 1 && !workspace.policy.allow_cross_workspace_agenda {
            return Err(OrgError::invalid_input(
                "Every selected workspace must allow cross-workspace operational queries",
            ));
        }
        let projection = transaction
            .list_org_workspace_projection(*workspace_id)
            .await
            .map_err(OrgError::storage)?;
        let dependencies = note_org::validate_dependencies(
            &projection
                .iter()
                .map(super::projected_to_domain)
                .collect::<Vec<_>>(),
            &workspace.policy.successful_terminal_states,
        )
        .map_err(|error| {
            OrgError::new(
                OrgErrorCode::StorageFailure,
                "Org workspace projection is inconsistent",
                json!({"reason": error.to_string()}),
                false,
            )
        })?;
        let active_count = transaction
            .count_active_org_leases(*workspace_id, evaluated_at)
            .await
            .map_err(OrgError::storage)?;
        result.insert(
            *workspace_id,
            WorkspaceReadiness {
                workspace,
                dependencies,
                active_count,
                evaluated_at,
                actor_id: query.assignee.clone(),
            },
        );
    }
    Ok(result)
}

fn storage_query<'a>(
    query: &'a NormalizedQuery,
    evaluated_at: i64,
    after: Option<&'a note_storage::OrgOperationalCursor>,
    tags: &'a [&'a str],
) -> StorageQuery<'a> {
    let (scheduled_from, scheduled_to, deadline_from, deadline_to) = match query.family {
        OperationalQueryFamily::Queue => (
            query.scheduled_from,
            query.scheduled_to,
            query.deadline_from,
            query.deadline_to,
        ),
        OperationalQueryFamily::Agenda if query.view == OperationalView::Scheduled => {
            (query.from, query.to, None, None)
        }
        OperationalQueryFamily::Agenda => (None, None, query.from, query.to),
    };
    StorageQuery {
        view: storage_view(query.view),
        workspace_ids: &query.workspace_ids,
        item_type: query.item_type,
        state: query.state.as_deref(),
        priority: query.priority,
        tags,
        assignee: query.assignee.as_deref(),
        scheduled_from,
        scheduled_to,
        deadline_from,
        deadline_to,
        completed_from: query.completed_from,
        completed_to: query.completed_to,
        include_archived: query.include_archived,
        now: evaluated_at,
        after,
        limit: STORAGE_BATCH_LIMIT,
    }
}

fn storage_view(view: OperationalView) -> StorageView {
    match view {
        OperationalView::Ready => StorageView::Ready,
        OperationalView::Assigned => StorageView::Assigned,
        OperationalView::Running => StorageView::Running,
        OperationalView::Blocked => StorageView::Blocked,
        OperationalView::Review => StorageView::Review,
        OperationalView::Scheduled => StorageView::Scheduled,
        OperationalView::UpcomingDeadline => StorageView::UpcomingDeadline,
        OperationalView::Failed => StorageView::Failed,
        OperationalView::ExpiredLease => StorageView::ExpiredLease,
        OperationalView::Completed => StorageView::Completed,
    }
}

pub(crate) async fn count_ready_in_transaction(
    transaction: &dyn StorageTransaction,
    workspace_id: WorkspaceId,
    evaluated_at: i64,
) -> Result<i64, OrgError> {
    let query = NormalizedQuery::new(
        OperationalQueryFamily::Queue,
        &OperationalQuery::for_workspace(workspace_id, OperationalView::Ready),
    )?;
    let readiness = preload_readiness(transaction, &query, evaluated_at).await?;
    let workspace_readiness = readiness.get(&workspace_id).ok_or_else(|| {
        OrgError::new(
            OrgErrorCode::StorageFailure,
            "Org readiness workspace preload is inconsistent",
            json!({"workspace_id": workspace_id}),
            false,
        )
    })?;
    let mut after = None;
    let mut count = 0_i64;
    loop {
        let storage_query = storage_query(&query, evaluated_at, after.as_ref(), &[]);
        let rows = transaction
            .query_org_operational(storage_query)
            .await
            .map_err(OrgError::storage)?;
        let len = rows.len();
        for row in rows {
            after = Some(row.cursor(StorageView::Ready));
            if workspace_readiness.is_ready(&row) {
                count = count.saturating_add(1);
            }
        }
        if len < STORAGE_BATCH_LIMIT {
            break;
        }
    }
    Ok(count)
}

fn map_operational_row(row: StorageRow, evaluated_at: i64) -> OperationalItemSummary {
    OperationalItemSummary {
        item: map_item(row.item),
        attempt_count: row.attempt_count,
        current_attempt_status: row.current_attempt_status.map(attempt_status),
        retry_exhausted: row.retry_exhausted,
        ready_status: row.ready_marker.map(|marker| match marker {
            OrgReadyMarker::Ready => OrgReadyStatus::Ready,
            OrgReadyMarker::RecoveryCandidate => OrgReadyStatus::RecoveryCandidate,
        }),
        review_lease_status: row.review_lease_marker.map(|marker| match marker {
            OrgReviewLeaseMarker::Unleased => OrgReviewLeaseStatus::Unleased,
            OrgReviewLeaseMarker::Active => OrgReviewLeaseStatus::Active,
            OrgReviewLeaseMarker::Expired => OrgReviewLeaseStatus::Expired,
        }),
        lease: row.lease.map(|lease| map_lease(lease, evaluated_at)),
        completion_at: row.completion_at,
    }
}

pub(crate) fn map_item(item: OrgProjectedWorkItem) -> OrgItemView {
    OrgItemView {
        id: item.id,
        workspace_id: item.workspace_id,
        document_id: item.document_id,
        parent_id: item.parent_id,
        item_type: item.item_type,
        title: item.title,
        state: item.state,
        priority: item.priority,
        scheduled: item.scheduled.map(map_timestamp),
        deadline: item.deadline.map(map_timestamp),
        assignee: item.assignee,
        requires_review: item.requires_review,
        created_at: item.created_at,
        tags: item.tags,
    }
}

fn map_timestamp(value: note_storage::StoredOrgTimestamp) -> OrgTimestampView {
    OrgTimestampView {
        raw: value.raw,
        local: value.local,
        timezone: value.timezone,
        utc_timestamp: value.utc_timestamp,
    }
}

pub(crate) fn map_lease(lease: note_storage::SanitizedOrgLease, evaluated_at: i64) -> OrgLeaseView {
    OrgLeaseView {
        id: lease.id,
        workspace_id: lease.workspace_id,
        work_item_id: lease.work_item_id,
        attempt_id: lease.attempt_id,
        kind: match lease.kind {
            note_storage::OrgLeaseKind::Execution => "execution",
            note_storage::OrgLeaseKind::Review => "review",
        }
        .into(),
        actor_id: lease.actor_id,
        acquired_at: lease.acquired_at,
        last_heartbeat_at: lease.last_heartbeat_at,
        expires_at: lease.expires_at,
        status: if lease.ended_at.is_some() {
            "closed"
        } else if lease.expires_at <= evaluated_at {
            "expired"
        } else {
            "active"
        }
        .into(),
    }
}

fn attempt_status(status: OrgAttemptStatus) -> String {
    match status {
        OrgAttemptStatus::Running => "running",
        OrgAttemptStatus::Submitted => "submitted",
        OrgAttemptStatus::Completed => "completed",
        OrgAttemptStatus::Failed => "failed",
        OrgAttemptStatus::Cancelled => "cancelled",
        OrgAttemptStatus::Expired => "expired",
    }
    .into()
}

fn attempt_phase(status: OrgAttemptStatus) -> AttemptPhase {
    match status {
        OrgAttemptStatus::Running => AttemptPhase::Running,
        OrgAttemptStatus::Submitted => AttemptPhase::Submitted,
        OrgAttemptStatus::Completed
        | OrgAttemptStatus::Failed
        | OrgAttemptStatus::Cancelled
        | OrgAttemptStatus::Expired => AttemptPhase::Terminal,
    }
}

fn trimmed_nonempty(value: &str, field: &'static str) -> Result<String, OrgError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(OrgError::invalid_input(format!(
            "Org operational {field} filter must not be blank"
        )));
    }
    Ok(value.into())
}

fn not_found(resource: &'static str, id: String) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        json!({"resource": resource, "id": id}),
        false,
    )
}

pub(crate) fn paginate_read<T>(
    mut items: Vec<T>,
    query: &super::OrgReadQuery,
    signer: &dyn super::OrgCursorSigner,
    family: &str,
    scope: Option<&str>,
    id: impl Fn(&T) -> String,
) -> Result<super::OrgReadPage<T>, OrgError> {
    let limit = query.limit.unwrap_or(DEFAULT_OPERATIONAL_LIMIT);
    if !(1..=MAX_OPERATIONAL_LIMIT).contains(&limit) {
        return Err(OrgError::invalid_input(
            "Org read limit must be between 1 and 200",
        ));
    }
    items.sort_by_key(&id);
    let fingerprint = super::read_fingerprint(family, scope, query.include_archived);
    let after = query
        .cursor
        .as_deref()
        .map(|cursor| super::decode_read_cursor(signer, cursor, &fingerprint))
        .transpose()?;
    if let Some(after) = after {
        items.retain(|item| id(item).as_str() > after.as_str());
    }
    let has_more = items.len() > limit;
    items.truncate(limit);
    let next_cursor = if has_more {
        items
            .last()
            .map(|item| super::encode_read_cursor(signer, &fingerprint, &id(item)))
            .transpose()?
    } else {
        None
    };
    Ok(super::OrgReadPage { items, next_cursor })
}
