use super::{
    audit::map_event, OrgArtifactView, OrgAttemptNoteView, OrgAttemptView, OrgContext,
    OrgDependencyView, OrgDocumentView, OrgError, OrgErrorCode, OrgHistorySegment, OrgItemContext,
    OrgItemView, OrgNoteLinkView, OrgOriginView, OrgTimestampView, OrgWorkspaceView,
};
use note_org::{WorkItemId, WorkspaceId};
use note_storage::{
    OrgAttempt, OrgAttemptStatus, OrgEvent, OrgProjectedWorkItem, StorageTransaction,
    TransactionMode,
};

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
    let result = get_item_context_in_transaction(transaction.as_ref(), workspace_id, item_id).await;
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

async fn get_item_context_in_transaction(
    transaction: &dyn StorageTransaction,
    workspace_id: WorkspaceId,
    item_id: WorkItemId,
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
        .map_err(OrgError::storage)?
        .into_iter()
        .map(map_attempt)
        .collect();
    let history = transaction
        .list_org_global_subject_events("work_item", &item.id.to_string())
        .await
        .map_err(OrgError::storage)?;
    let origin = hydrate_origin(transaction, &history).await?;
    let history_segments = history_segments(history);
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
        lease: None,
    })
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
