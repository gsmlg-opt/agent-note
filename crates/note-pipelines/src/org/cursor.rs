use super::{OperationalQuery, OperationalQueryFamily, OperationalView, OrgCursorSigner, OrgError};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use note_org::{WorkItemId, WorkspaceId};
use note_storage::OrgOperationalCursor;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;

const CURSOR_VERSION: u8 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedCursorEnvelope {
    payload: String,
    mac: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorEnvelope {
    version: u8,
    query_fingerprint: String,
    evaluated_at: i64,
    last_scanned: CursorKey,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorKey {
    primary_at: Option<i64>,
    priority: Option<char>,
    deadline_at: Option<i64>,
    scheduled_at: Option<i64>,
    created_at: i64,
    workspace_id: String,
    work_item_id: String,
}

pub(crate) struct DecodedCursor {
    pub evaluated_at: i64,
    pub last_scanned: OrgOperationalCursor,
}

pub(crate) fn query_fingerprint(
    family: OperationalQueryFamily,
    query: &OperationalQuery,
) -> Result<String, OrgError> {
    let normalized = query.normalized_fingerprint_value(family)?;
    let bytes = serde_json::to_vec(&normalized)
        .map_err(|_| OrgError::invalid_input("Org operational query is invalid"))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(crate) fn encode_cursor(
    signer: &dyn OrgCursorSigner,
    fingerprint: &str,
    evaluated_at: i64,
    cursor: &OrgOperationalCursor,
) -> Result<String, OrgError> {
    let payload = CursorEnvelope {
        version: CURSOR_VERSION,
        query_fingerprint: fingerprint.into(),
        evaluated_at,
        last_scanned: CursorKey {
            primary_at: cursor.primary_at,
            priority: cursor.priority,
            deadline_at: cursor.deadline_at,
            scheduled_at: cursor.scheduled_at,
            created_at: cursor.created_at,
            workspace_id: cursor.workspace_id.to_string(),
            work_item_id: cursor.work_item_id.to_string(),
        },
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|_| OrgError::invalid_input("Org operational cursor is invalid"))?;
    seal_cursor(signer, &bytes, invalid_cursor)
}

pub(crate) fn decode_cursor(
    signer: &dyn OrgCursorSigner,
    encoded: &str,
    expected_fingerprint: &str,
    view: OperationalView,
) -> Result<DecodedCursor, OrgError> {
    let bytes = open_cursor(signer, encoded, invalid_cursor)?;
    let payload: CursorEnvelope = serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())?;
    if payload.version != CURSOR_VERSION
        || payload.query_fingerprint != expected_fingerprint
        || payload.evaluated_at <= 0
    {
        return Err(invalid_cursor());
    }
    let workspace_id =
        WorkspaceId::from_str(&payload.last_scanned.workspace_id).map_err(|_| invalid_cursor())?;
    let work_item_id =
        WorkItemId::from_str(&payload.last_scanned.work_item_id).map_err(|_| invalid_cursor())?;
    validate_storage_tuple(view, &payload.last_scanned)?;
    Ok(DecodedCursor {
        evaluated_at: payload.evaluated_at,
        last_scanned: OrgOperationalCursor {
            primary_at: payload.last_scanned.primary_at,
            priority: payload.last_scanned.priority,
            deadline_at: payload.last_scanned.deadline_at,
            scheduled_at: payload.last_scanned.scheduled_at,
            created_at: payload.last_scanned.created_at,
            workspace_id,
            work_item_id,
        },
    })
}

fn validate_storage_tuple(view: OperationalView, key: &CursorKey) -> Result<(), OrgError> {
    if key
        .priority
        .is_some_and(|value| !value.is_ascii_uppercase())
    {
        return Err(invalid_cursor());
    }
    let valid_primary = match view {
        OperationalView::Scheduled => {
            key.primary_at.is_some() && key.primary_at == key.scheduled_at
        }
        OperationalView::UpcomingDeadline => {
            key.primary_at.is_some() && key.primary_at == key.deadline_at
        }
        OperationalView::ExpiredLease => key.primary_at.is_some(),
        OperationalView::Ready
        | OperationalView::Assigned
        | OperationalView::Running
        | OperationalView::Blocked
        | OperationalView::Review
        | OperationalView::Failed
        | OperationalView::Completed => key.primary_at.is_none(),
    };
    if !valid_primary {
        return Err(invalid_cursor());
    }
    Ok(())
}

fn invalid_cursor() -> OrgError {
    OrgError::invalid_input("Org operational cursor is invalid or does not match the query")
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadCursorEnvelope {
    version: u8,
    query_fingerprint: String,
    last_id: String,
}

pub(crate) fn read_fingerprint(
    family: &str,
    scope: Option<&str>,
    include_archived: bool,
) -> String {
    let value = serde_json::json!({
        "family": family,
        "scope": scope,
        "include_archived": include_archived,
    });
    format!("{:x}", Sha256::digest(serde_json::to_vec(&value).unwrap()))
}

pub(crate) fn encode_read_cursor(
    signer: &dyn OrgCursorSigner,
    fingerprint: &str,
    last_id: &str,
) -> Result<String, OrgError> {
    let value = ReadCursorEnvelope {
        version: CURSOR_VERSION,
        query_fingerprint: fingerprint.into(),
        last_id: last_id.into(),
    };
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| OrgError::invalid_input("Org read cursor is invalid"))?;
    seal_cursor(signer, &bytes, invalid_read_cursor)
}

pub(crate) fn decode_read_cursor(
    signer: &dyn OrgCursorSigner,
    encoded: &str,
    expected_fingerprint: &str,
) -> Result<String, OrgError> {
    let bytes = open_cursor(signer, encoded, invalid_read_cursor)?;
    let value: ReadCursorEnvelope = serde_json::from_slice(&bytes)
        .map_err(|_| OrgError::invalid_input("Org read cursor is invalid"))?;
    if value.version != CURSOR_VERSION
        || value.query_fingerprint != expected_fingerprint
        || value.last_id.trim().is_empty()
    {
        return Err(OrgError::invalid_input(
            "Org read cursor is invalid or does not match the query",
        ));
    }
    Ok(value.last_id)
}

fn seal_cursor(
    signer: &dyn OrgCursorSigner,
    payload: &[u8],
    invalid: fn() -> OrgError,
) -> Result<String, OrgError> {
    let mac = signer.sign(payload)?;
    let signed = SignedCursorEnvelope {
        payload: URL_SAFE_NO_PAD.encode(payload),
        mac: URL_SAFE_NO_PAD.encode(mac),
    };
    serde_json::to_vec(&signed)
        .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
        .map_err(|_| invalid())
}

fn open_cursor(
    signer: &dyn OrgCursorSigner,
    encoded: &str,
    invalid: fn() -> OrgError,
) -> Result<Vec<u8>, OrgError> {
    let signed_bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
    let signed: SignedCursorEnvelope =
        serde_json::from_slice(&signed_bytes).map_err(|_| invalid())?;
    let payload = URL_SAFE_NO_PAD
        .decode(signed.payload)
        .map_err(|_| invalid())?;
    let mac = URL_SAFE_NO_PAD.decode(signed.mac).map_err(|_| invalid())?;
    if !signer.verify(&payload, &mac)? {
        return Err(invalid());
    }
    Ok(payload)
}

fn invalid_read_cursor() -> OrgError {
    OrgError::invalid_input("Org read cursor is invalid or does not match the query")
}
