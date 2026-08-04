use super::{OrgContext, OrgError, OrgErrorCode, OrgEventPage, OrgEventQuery, OrgEventView};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use note_storage::{OrgEvent, StorageTransaction, TransactionMode};
use serde::{Deserialize, Serialize};

const CURSOR_VERSION: u32 = 1;
const DEFAULT_LIMIT: usize = 50;
const MAX_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EventCursor {
    version: u32,
    workspace_id: note_org::WorkspaceId,
    subject_kind: Option<String>,
    subject_id: Option<String>,
    after_sequence: i64,
}

pub async fn list_event_history(
    context: &OrgContext,
    request: &OrgEventQuery,
) -> Result<OrgEventPage, OrgError> {
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(OrgError::invalid_input(
            "Org event page limit must be between 1 and 200",
        ));
    }
    validate_subject_filter(request)?;
    let after_sequence = match &request.cursor {
        Some(value) => decode_cursor(value, request)?.after_sequence,
        None => 0,
    };
    let transaction = context
        .storage()
        .begin(TransactionMode::Deferred)
        .await
        .map_err(OrgError::storage)?;
    let result =
        list_event_history_in_transaction(transaction.as_ref(), request, after_sequence, limit)
            .await;
    finish_read(transaction, result).await
}

async fn list_event_history_in_transaction(
    transaction: &dyn StorageTransaction,
    request: &OrgEventQuery,
    after_sequence: i64,
    limit: usize,
) -> Result<OrgEventPage, OrgError> {
    if transaction
        .get_org_workspace(request.workspace_id)
        .await
        .map_err(OrgError::storage)?
        .is_none()
    {
        return Err(not_found("workspace"));
    }
    let events = query_events(transaction, request, after_sequence, limit).await?;
    let has_more = if events.len() == limit {
        let last_sequence = events
            .last()
            .map(|event| event.sequence)
            .unwrap_or(after_sequence);
        !query_events(transaction, request, last_sequence, 1)
            .await?
            .is_empty()
    } else {
        false
    };
    let next_cursor = if has_more {
        let last_sequence = events
            .last()
            .map(|event| event.sequence)
            .ok_or_else(|| OrgError::invalid_input("Org event page cursor cannot be empty"))?;
        Some(encode_cursor(&EventCursor {
            version: CURSOR_VERSION,
            workspace_id: request.workspace_id,
            subject_kind: request.subject_kind.clone(),
            subject_id: request.subject_id.clone(),
            after_sequence: last_sequence,
        })?)
    } else {
        None
    };
    Ok(OrgEventPage {
        events: events.into_iter().map(map_event).collect(),
        next_cursor,
    })
}

async fn query_events(
    transaction: &dyn StorageTransaction,
    request: &OrgEventQuery,
    after_sequence: i64,
    limit: usize,
) -> Result<Vec<OrgEvent>, OrgError> {
    match (&request.subject_kind, &request.subject_id) {
        (Some(subject_kind), Some(subject_id)) => transaction
            .list_org_subject_events(
                request.workspace_id,
                subject_kind,
                subject_id,
                Some(after_sequence),
                limit,
            )
            .await
            .map_err(OrgError::storage),
        (None, None) => transaction
            .list_org_events(request.workspace_id, Some(after_sequence), limit)
            .await
            .map_err(OrgError::storage),
        _ => Err(OrgError::invalid_input(
            "Org event subject kind and ID must be provided together",
        )),
    }
}

fn validate_subject_filter(request: &OrgEventQuery) -> Result<(), OrgError> {
    match (&request.subject_kind, &request.subject_id) {
        (None, None) => Ok(()),
        (Some(kind), Some(id))
            if !kind.trim().is_empty()
                && kind == kind.trim()
                && !id.trim().is_empty()
                && id == id.trim() =>
        {
            Ok(())
        }
        (Some(_), Some(_)) => Err(OrgError::invalid_input(
            "Org event subject filter values must be nonblank and trimmed",
        )),
        _ => Err(OrgError::invalid_input(
            "Org event subject kind and ID must be provided together",
        )),
    }
}

fn encode_cursor(cursor: &EventCursor) -> Result<String, OrgError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|_| OrgError::invalid_input("Org event cursor cannot be encoded"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(value: &str, request: &OrgEventQuery) -> Result<EventCursor, OrgError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| OrgError::invalid_input("Org event cursor is malformed"))?;
    let cursor: EventCursor = serde_json::from_slice(&bytes)
        .map_err(|_| OrgError::invalid_input("Org event cursor is malformed"))?;
    if cursor.version != CURSOR_VERSION
        || cursor.workspace_id != request.workspace_id
        || cursor.subject_kind != request.subject_kind
        || cursor.subject_id != request.subject_id
        || cursor.after_sequence < 1
    {
        return Err(OrgError::invalid_input(
            "Org event cursor does not match the requested history",
        ));
    }
    Ok(cursor)
}

pub(crate) fn map_event(event: OrgEvent) -> OrgEventView {
    OrgEventView {
        id: event.id,
        workspace_id: event.workspace_id,
        sequence: event.sequence,
        subject_kind: event.subject_kind,
        subject_id: event.subject_id,
        actor_id: event.actor_id,
        attempt_id: event.attempt_id,
        event_type: event.event_type.to_string(),
        occurred_at: event.occurred_at,
        summary: event.summary,
        metadata: event.metadata,
        previous_state: event.previous_state,
        resulting_state: event.resulting_state,
    }
}

async fn finish_read<T>(
    transaction: Box<dyn StorageTransaction>,
    result: Result<T, OrgError>,
) -> Result<T, OrgError> {
    transaction.rollback().await.map_err(OrgError::storage)?;
    result
}

fn not_found(resource: &'static str) -> OrgError {
    OrgError::new(
        OrgErrorCode::NotFound,
        format!("Org {resource} was not found"),
        serde_json::json!({"resource": resource}),
        false,
    )
}
