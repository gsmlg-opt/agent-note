use super::{
    audit::map_event, OperationalView, OrgArtifactView, OrgAttemptBudgetView, OrgAttemptNoteView,
    OrgAttemptView, OrgContext, OrgDependencyView, OrgDocumentView, OrgError, OrgErrorCode,
    OrgHistorySegment, OrgItemContext, OrgItemView, OrgLeaseView, OrgNoteLinkView,
    OrgOperationalContextView, OrgOriginView, OrgReadyStatus, OrgRecoveryStatusView,
    OrgTimestampView, OrgWorkspaceView,
};
use note_org::{
    decide_claim, evaluate_readiness, AttemptPhase, ClaimDecisionError, ClaimKind, ClaimQueue,
    ClaimRequest, LeaseKind, LeaseStatus, Readiness, ReadinessBlocker, ReadinessContext,
    WorkItemId, WorkspaceId,
};
use note_storage::{
    OrgAttempt, OrgAttemptStatus, OrgDocument, OrgEvent, OrgProjectedWorkItem, StorageTransaction,
    TransactionMode,
};

pub async fn list_documents(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    query: &super::OrgReadQuery,
) -> Result<super::OrgReadPage<super::OrgDocumentView>, OrgError> {
    let session = context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?;
    let workspace = session
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| not_found("workspace"))?;
    if workspace.archived_at.is_some() && !query.include_archived {
        return Ok(super::OrgReadPage {
            items: Vec::new(),
            next_cursor: None,
        });
    }
    let documents = session
        .list_org_documents(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .map(map_document)
        .collect();
    super::paginate_read(
        documents,
        query,
        context.cursor_signer(),
        "documents",
        Some(&workspace_id.to_string()),
        |document| document.id.to_string(),
    )
}

pub async fn get_document(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    document_id: note_org::DocumentId,
) -> Result<super::OrgDocumentSourceView, OrgError> {
    context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?
        .get_org_document(document_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|document| document.workspace_id == workspace_id)
        .map(map_document_source)
        .ok_or_else(|| not_found("document"))
}

/// Resolves document ownership inside the pipeline boundary for offline
/// exports whose stable command contract accepts only a document ID.
pub async fn export_document_by_id(
    context: &OrgContext,
    document_id: note_org::DocumentId,
) -> Result<super::OrgDocumentSourceView, OrgError> {
    context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?
        .get_org_document(document_id)
        .await
        .map_err(OrgError::storage)?
        .map(map_document_source)
        .ok_or_else(|| not_found("document"))
}

pub async fn export_workspace(
    context: &OrgContext,
    workspace_id: WorkspaceId,
) -> Result<super::OrgWorkspaceExport, OrgError> {
    let session = context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?;
    let workspace = session
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| not_found("workspace"))?;
    let mut documents = session
        .list_org_documents(workspace_id)
        .await
        .map_err(OrgError::storage)?;
    documents.sort_by_key(|document| document.id);
    Ok(super::OrgWorkspaceExport {
        workspace: super::map_workspace_view(workspace),
        documents: documents.into_iter().map(map_document_source).collect(),
    })
}

pub async fn get_item(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    item_id: WorkItemId,
) -> Result<OrgItemView, OrgError> {
    context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?
        .get_org_work_item(item_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|item| item.workspace_id == workspace_id)
        .map(map_item)
        .ok_or_else(|| not_found("work item"))
}

pub async fn list_note_work_items(
    context: &OrgContext,
    note_id: &str,
    query: &super::OrgReadQuery,
) -> Result<super::OrgReadPage<OrgItemView>, OrgError> {
    if note_id.trim().is_empty() || note_id != note_id.trim() {
        return Err(OrgError::invalid_input("Org note ID must not be blank"));
    }
    let session = context
        .storage()
        .session()
        .await
        .map_err(OrgError::storage)?;
    let allowed_workspaces = session
        .list_org_workspaces(query.include_archived)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .map(|workspace| workspace.id)
        .collect::<std::collections::BTreeSet<_>>();
    let items = session
        .list_org_work_items_linking_note(note_id)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .filter(|item| allowed_workspaces.contains(&item.workspace_id))
        .map(map_item)
        .collect();
    super::paginate_read(
        items,
        query,
        context.cursor_signer(),
        "note_work_items",
        Some(note_id),
        |item| item.id.to_string(),
    )
}

pub async fn get_item_context(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    item_id: WorkItemId,
) -> Result<OrgItemContext, OrgError> {
    let transaction = context
        .storage()
        .begin(TransactionMode::Deferred)
        .await
        .map_err(OrgError::storage)?;
    let result = get_item_context_in_transaction(
        transaction.as_ref(),
        workspace_id,
        item_id,
        context.clock().now(),
    )
    .await;
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

pub(crate) async fn get_item_context_in_transaction(
    transaction: &dyn StorageTransaction,
    workspace_id: WorkspaceId,
    item_id: WorkItemId,
    now: i64,
) -> Result<OrgItemContext, OrgError> {
    let workspace = transaction
        .get_org_workspace(workspace_id)
        .await
        .map_err(OrgError::storage)?
        .ok_or_else(|| not_found("workspace"))?;
    let item = transaction
        .get_org_work_item(item_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|item| item.workspace_id == workspace_id)
        .ok_or_else(|| not_found("work item"))?;
    let document = transaction
        .get_org_document(item.document_id)
        .await
        .map_err(OrgError::storage)?
        .filter(|document| document.workspace_id == workspace_id)
        .ok_or_else(|| not_found("document"))?;
    let parent = match item.parent_id {
        Some(parent_id) => Some(map_item(
            transaction
                .get_org_work_item(parent_id)
                .await
                .map_err(OrgError::storage)?
                .filter(|parent| parent.workspace_id == workspace_id)
                .ok_or_else(|| {
                    OrgError::new(
                        OrgErrorCode::StorageFailure,
                        "Org parent projection is inconsistent",
                        serde_json::json!({"parent_id": parent_id}),
                        false,
                    )
                })?,
        )),
        None => None,
    };
    let children = transaction
        .list_org_work_item_children(item.id)
        .await
        .map_err(OrgError::storage)?
        .into_iter()
        .filter(|child| child.workspace_id == workspace_id)
        .map(map_item)
        .collect();
    let mut dependencies = Vec::new();
    for dependency_id in &item.dependencies {
        let dependency = transaction
            .get_org_work_item(*dependency_id)
            .await
            .map_err(OrgError::storage)?
            .filter(|dependency| dependency.workspace_id == workspace_id)
            .ok_or_else(|| {
                OrgError::new(
                    OrgErrorCode::StorageFailure,
                    "Org dependency projection is inconsistent",
                    serde_json::json!({"dependency_id": dependency_id}),
                    false,
                )
            })?;
        let satisfied = dependency
            .state
            .as_ref()
            .is_some_and(|state| workspace.policy.successful_terminal_states.contains(state));
        dependencies.push(OrgDependencyView {
            item: map_item(dependency),
            satisfied,
        });
    }
    let mut note_links = Vec::new();
    for link in &item.note_links {
        let available = transaction
            .get_note_revision(&link.note_id.to_string())
            .await
            .map_err(OrgError::storage)?
            .is_some();
        note_links.push(OrgNoteLinkView {
            purpose: link.purpose.clone(),
            note_id: link.note_id.to_string(),
            description: link.description.clone(),
            available,
        });
    }
    let attempts = transaction
        .list_org_attempts(item.id)
        .await
        .map_err(OrgError::storage)?;
    let history = transaction
        .list_org_global_subject_events("work_item", &item.id.to_string())
        .await
        .map_err(OrgError::storage)?;
    let origin = hydrate_origin(transaction, &history).await?;
    let history_segments = history_segments(history);
    let lease_record = transaction
        .get_open_org_lease_internal(item.id)
        .await
        .map_err(OrgError::storage)?;
    let active_count = transaction
        .count_active_org_leases(workspace_id, now)
        .await
        .map_err(OrgError::storage)?;
    let operational = operational_context(
        &workspace,
        &item,
        &dependencies,
        &attempts,
        lease_record.as_ref(),
        active_count,
        now,
    );
    let attempts = attempts.into_iter().map(map_attempt).collect();
    let lease = lease_record.map(|lease| OrgLeaseView {
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
        status: if lease.expires_at > now {
            "active"
        } else {
            "expired"
        }
        .into(),
    });
    Ok(OrgItemContext {
        workspace: OrgWorkspaceView {
            id: workspace.id,
            slug: workspace.slug,
            display_name: workspace.display_name,
            description: workspace.description,
            timezone: workspace.timezone,
            policy_schema_version: workspace.policy_schema_version,
            policy: workspace.policy,
            revision: workspace.revision,
            archived_at: workspace.archived_at,
        },
        workspace_revision: workspace.revision,
        document: OrgDocumentView {
            id: document.id,
            path: document.path,
            revision: document.revision,
        },
        item: map_item(item),
        parent,
        children,
        dependencies,
        note_links,
        attempts,
        origin,
        history_segments,
        lease,
        operational,
    })
}

fn operational_context(
    workspace: &note_storage::OrgWorkspace,
    item: &OrgProjectedWorkItem,
    dependencies: &[OrgDependencyView],
    attempts: &[OrgAttempt],
    lease: Option<&note_storage::OrgLease>,
    active_count: i64,
    now: i64,
) -> OrgOperationalContextView {
    let lease_status = lease
        .map(|lease| {
            let kind = match lease.kind {
                note_storage::OrgLeaseKind::Execution => LeaseKind::Execution,
                note_storage::OrgLeaseKind::Review => LeaseKind::Review,
            };
            if lease.expires_at <= now {
                LeaseStatus::Expired(kind)
            } else {
                LeaseStatus::Active(kind)
            }
        })
        .unwrap_or(LeaseStatus::None);
    let execution_attempt_count = u32::try_from(attempts.len()).unwrap_or(u32::MAX);
    let domain_item = super::projected_to_domain(item);
    let readiness_context = ReadinessContext {
        now,
        dependencies_satisfied: dependencies.iter().all(|value| value.satisfied),
        workspace_active: workspace.archived_at.is_none(),
        lease: lease_status,
        capacity_available: active_count < workspace.policy.concurrency_limit as i64,
        // Context reads have no requester identity. Assignment-sensitive
        // readiness must therefore remain blocked instead of impersonating the
        // item's assignee.
        actor_id: None,
        scheduled_at: item.scheduled.as_ref().map(|value| value.utc_timestamp),
        queue: ClaimQueue::Execution,
        execution_attempt_count,
        current_attempt: attempts.last().map(|attempt| match attempt.status {
            OrgAttemptStatus::Running => AttemptPhase::Running,
            OrgAttemptStatus::Submitted => AttemptPhase::Submitted,
            OrgAttemptStatus::Completed
            | OrgAttemptStatus::Failed
            | OrgAttemptStatus::Cancelled
            | OrgAttemptStatus::Expired => AttemptPhase::Terminal,
        }),
    };
    let readiness = evaluate_readiness(&domain_item, &workspace.policy, &readiness_context);
    let ready_status = match &readiness {
        Readiness::Ready => Some(OrgReadyStatus::Ready),
        Readiness::RecoveryCandidate => Some(OrgReadyStatus::RecoveryCandidate),
        Readiness::Blocked(_) => None,
    };
    let blockers = match &readiness {
        Readiness::Blocked(blockers) => blockers
            .iter()
            .map(blocker_name)
            .map(str::to_owned)
            .collect(),
        Readiness::Ready | Readiness::RecoveryCandidate => Vec::new(),
    };
    let state = item.state.as_deref();
    let terminal = state.is_some_and(|state| workspace.policy.terminal_states.contains(state));
    let mut classifications = Vec::new();
    if ready_status.is_some() {
        classifications.push(OperationalView::Ready);
    }
    if item.assignee.is_some() && !terminal {
        classifications.push(OperationalView::Assigned);
    }
    if state == Some(workspace.policy.running_state.as_str()) {
        classifications.push(OperationalView::Running);
    }
    if workspace.policy.states.contains("BLOCKED") && state == Some("BLOCKED") {
        classifications.push(OperationalView::Blocked);
    }
    if state == Some(workspace.policy.review_state.as_str()) {
        classifications.push(OperationalView::Review);
    }
    if item.scheduled.is_some() && !terminal {
        classifications.push(OperationalView::Scheduled);
    }
    if item
        .deadline
        .as_ref()
        .is_some_and(|deadline| deadline.utc_timestamp >= now)
        && !terminal
    {
        classifications.push(OperationalView::UpcomingDeadline);
    }
    if state == Some(workspace.policy.failed_state.as_str()) {
        classifications.push(OperationalView::Failed);
    }
    if lease.is_some_and(|lease| lease.expires_at <= now) {
        classifications.push(OperationalView::ExpiredLease);
    }
    if state.is_some_and(|state| workspace.policy.successful_terminal_states.contains(state)) {
        classifications.push(OperationalView::Completed);
    }
    let max_attempts = workspace.policy.max_attempts();
    let remaining_attempts = max_attempts.saturating_sub(execution_attempt_count);
    let recovery_candidate = ready_status == Some(OrgReadyStatus::RecoveryCandidate);
    let (recovery_eligible, recovery_blockers) =
        if state == Some(workspace.policy.failed_state.as_str()) {
            match decide_claim(
                &domain_item,
                &workspace.policy,
                &readiness_context,
                ClaimRequest {
                    lease_kind: LeaseKind::Execution,
                    claim_kind: ClaimKind::Retry,
                },
            ) {
                Ok(_) => (true, Vec::new()),
                Err(ClaimDecisionError::Blocked(blockers)) => (
                    false,
                    blockers
                        .iter()
                        .map(blocker_name)
                        .map(str::to_owned)
                        .collect(),
                ),
                Err(ClaimDecisionError::Transition(_)) => {
                    (false, vec!["invalid_recovery_transition".into()])
                }
            }
        } else {
            (recovery_candidate, blockers.clone())
        };
    OrgOperationalContextView {
        classifications,
        readiness: ready_status,
        blockers: blockers.clone(),
        attempt_budget: OrgAttemptBudgetView {
            execution_attempt_count,
            max_attempts,
            remaining_attempts,
            retry_exhausted: remaining_attempts == 0,
        },
        recovery: OrgRecoveryStatusView {
            eligible: recovery_eligible,
            candidate: recovery_candidate,
            blockers: recovery_blockers,
        },
    }
}

fn blocker_name(blocker: &ReadinessBlocker) -> &'static str {
    match blocker {
        ReadinessBlocker::NonExecutableState => "non_executable_state",
        ReadinessBlocker::DependenciesIncomplete => "dependencies_incomplete",
        ReadinessBlocker::ScheduledForFuture => "scheduled_for_future",
        ReadinessBlocker::WorkspaceArchived => "workspace_archived",
        ReadinessBlocker::ActiveLease => "active_lease",
        ReadinessBlocker::LeaseKindMismatch => "lease_kind_mismatch",
        ReadinessBlocker::ConcurrencyLimit => "concurrency_limit",
        ReadinessBlocker::RetryLimit => "retry_limit",
        ReadinessBlocker::AttemptHistoryInconsistent => "attempt_history_inconsistent",
        ReadinessBlocker::RunningAttemptRequired => "running_attempt_required",
        ReadinessBlocker::SubmittedAttemptRequired => "submitted_attempt_required",
        ReadinessBlocker::TerminalAttemptRequired => "terminal_attempt_required",
        ReadinessBlocker::AssignmentRequired => "assignment_required",
        ReadinessBlocker::AssignedToOtherActor => "assigned_to_other_actor",
    }
}

async fn hydrate_origin(
    transaction: &dyn StorageTransaction,
    history: &[OrgEvent],
) -> Result<Option<OrgOriginView>, OrgError> {
    let Some(origin) = history
        .iter()
        .filter(|event| event.event_type == note_storage::OrgEventType::Creation)
        .find_map(|event| {
            event
                .metadata
                .get("origin")
                .filter(|value| !value.is_null())
        })
    else {
        return Ok(None);
    };
    match origin.get("kind").and_then(serde_json::Value::as_str) {
        Some("work_item") => {
            let value = origin
                .get("work_item_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| corrupt_origin("work_item_id"))?;
            let work_item_id = value.parse().map_err(|_| corrupt_origin("work_item_id"))?;
            let item = transaction
                .get_org_work_item(work_item_id)
                .await
                .map_err(OrgError::storage)?
                .map(map_item)
                .map(Box::new);
            Ok(Some(OrgOriginView::WorkItem { work_item_id, item }))
        }
        Some("event") => {
            let event_id = origin
                .get("event_id")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| corrupt_origin("event_id"))?;
            let event = find_event(transaction, event_id)
                .await?
                .map(map_event)
                .map(Box::new);
            Ok(Some(OrgOriginView::Event {
                event_id: event_id.to_string(),
                event,
            }))
        }
        _ => Err(corrupt_origin("kind")),
    }
}

async fn find_event(
    transaction: &dyn StorageTransaction,
    event_id: &str,
) -> Result<Option<OrgEvent>, OrgError> {
    for workspace in transaction
        .list_org_workspaces(true)
        .await
        .map_err(OrgError::storage)?
    {
        let mut after = None;
        loop {
            let page = transaction
                .list_org_events(workspace.id, after, 200)
                .await
                .map_err(OrgError::storage)?;
            if let Some(event) = page.iter().find(|event| event.id == event_id) {
                return Ok(Some(event.clone()));
            }
            if page.len() < 200 {
                break;
            }
            after = page.last().map(|event| event.sequence);
        }
    }
    Ok(None)
}

fn history_segments(history: Vec<OrgEvent>) -> Vec<OrgHistorySegment> {
    let mut segments: Vec<OrgHistorySegment> = Vec::new();
    for event in history {
        if segments
            .last()
            .is_none_or(|segment| segment.workspace_id != event.workspace_id)
        {
            segments.push(OrgHistorySegment {
                workspace_id: event.workspace_id,
                events: Vec::new(),
            });
        }
        segments.last_mut().unwrap().events.push(map_event(event));
    }
    segments
}

fn map_item(item: OrgProjectedWorkItem) -> OrgItemView {
    OrgItemView {
        id: item.id,
        workspace_id: item.workspace_id,
        document_id: item.document_id,
        parent_id: item.parent_id,
        item_type: item.item_type,
        title: item.title,
        state: item.state,
        priority: item.priority,
        scheduled: item.scheduled.map(|value| OrgTimestampView {
            raw: value.raw,
            local: value.local,
            timezone: value.timezone,
            utc_timestamp: value.utc_timestamp,
        }),
        deadline: item.deadline.map(|value| OrgTimestampView {
            raw: value.raw,
            local: value.local,
            timezone: value.timezone,
            utc_timestamp: value.utc_timestamp,
        }),
        assignee: item.assignee,
        requires_review: item.requires_review,
        created_at: item.created_at,
        tags: item.tags,
    }
}

fn map_document(document: OrgDocument) -> OrgDocumentView {
    OrgDocumentView {
        id: document.id,
        path: document.path,
        revision: document.revision,
    }
}

fn map_document_source(document: OrgDocument) -> super::OrgDocumentSourceView {
    super::OrgDocumentSourceView {
        id: document.id,
        workspace_id: document.workspace_id,
        path: document.path,
        source: document.source,
        content_hash: document.content_hash,
        revision: document.revision,
    }
}

fn map_attempt(attempt: OrgAttempt) -> OrgAttemptView {
    OrgAttemptView {
        id: attempt.id,
        workspace_id: attempt.workspace_id,
        work_item_id: attempt.work_item_id,
        attempt_number: attempt.attempt_number,
        actor_id: attempt.actor_id,
        status: attempt_status(attempt.status).into(),
        started_at: attempt.started_at,
        ended_at: attempt.ended_at,
        error: attempt.error,
        result_summary: attempt.result_summary,
        review_outcome: attempt.review_outcome,
        note_refs: attempt
            .note_refs
            .into_iter()
            .map(|note| OrgAttemptNoteView {
                purpose: note.purpose,
                note_id: note.note_id,
                description: note.description,
            })
            .collect(),
        artifacts: attempt
            .artifacts
            .into_iter()
            .map(|artifact| OrgArtifactView {
                uri: artifact.uri,
                media_type: artifact.media_type,
                name: artifact.name,
                description: artifact.description,
            })
            .collect(),
        metadata: attempt.metadata,
    }
}

fn attempt_status(status: OrgAttemptStatus) -> &'static str {
    match status {
        OrgAttemptStatus::Running => "running",
        OrgAttemptStatus::Submitted => "submitted",
        OrgAttemptStatus::Completed => "completed",
        OrgAttemptStatus::Failed => "failed",
        OrgAttemptStatus::Cancelled => "cancelled",
        OrgAttemptStatus::Expired => "expired",
    }
}

fn corrupt_origin(field: &'static str) -> OrgError {
    OrgError::new(
        OrgErrorCode::StorageFailure,
        "Org follow-up origin metadata is invalid",
        serde_json::json!({"field": field}),
        false,
    )
}

fn not_found(resource: &'static str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        serde_json::json!({"resource": resource}),
        false,
    )
}
