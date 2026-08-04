use note_org::{WorkItemId, WorkspaceId};
use note_storage::{
    NewOrgLease, OrgAttemptStatus, OrgEventType, OrgLease, OrgLeaseEndReason, OrgLeaseKind,
    OrgLeaseUpdate, StoredOrgOperation,
};
use serde_json::json;
use std::str::FromStr;

#[test]
fn complete_org_event_vocabulary_has_stable_string_and_json_forms() {
    assert_eq!(OrgEventType::KNOWN.len(), 28);
    for event_type in OrgEventType::KNOWN {
        assert_eq!(
            OrgEventType::from_str(event_type.as_str()),
            Ok(event_type.clone())
        );
        assert_eq!(
            serde_json::from_str::<OrgEventType>(
                &serde_json::to_string(&event_type).expect("serialize event type")
            )
            .expect("deserialize event type"),
            event_type
        );
    }
    let legacy = OrgEventType::from_str("legacy_custom_event").unwrap();
    assert_eq!(legacy, OrgEventType::Other("legacy_custom_event".into()));
    assert!(!legacy.is_known());
    assert_eq!(
        serde_json::from_str::<OrgEventType>(
            &serde_json::to_string(&legacy).expect("serialize legacy event type")
        )
        .expect("deserialize legacy event type"),
        legacy
    );
}

#[test]
fn attempt_status_uses_transport_neutral_snake_case_values() {
    assert_eq!(
        serde_json::to_string(&OrgAttemptStatus::Running).unwrap(),
        "\"running\""
    );
    assert_eq!(
        serde_json::from_str::<OrgAttemptStatus>("\"completed\"").unwrap(),
        OrgAttemptStatus::Completed
    );
}

#[test]
fn lease_enums_use_transport_neutral_snake_case_values() {
    assert_eq!(
        serde_json::to_string(&OrgLeaseKind::Execution).unwrap(),
        "\"execution\""
    );
    assert_eq!(
        serde_json::from_str::<OrgLeaseKind>("\"review\"").unwrap(),
        OrgLeaseKind::Review
    );
    assert_eq!(
        serde_json::to_string(&OrgLeaseEndReason::LeaseExpiry).unwrap(),
        "\"lease_expiry\""
    );
    assert_eq!(
        serde_json::from_str::<OrgLeaseEndReason>("\"review_request\"").unwrap(),
        OrgLeaseEndReason::ReviewRequest
    );
}

#[test]
fn lease_records_keep_only_the_token_hash() {
    let workspace_id = WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
    let work_item_id = WorkItemId::from_str("22222222-2222-4222-8222-222222222222").unwrap();
    let lease = OrgLease {
        id: "lease-1".into(),
        workspace_id,
        work_item_id,
        attempt_id: "attempt-1".into(),
        kind: OrgLeaseKind::Execution,
        actor_id: "agent-a".into(),
        fencing_token_hash: "a".repeat(64),
        acquired_at: 10,
        last_heartbeat_at: 11,
        expires_at: 20,
        ended_at: None,
        end_reason: None,
        expiry_event_id: None,
    };
    let new = NewOrgLease {
        id: "lease-2",
        workspace_id,
        work_item_id,
        attempt_id: "attempt-2",
        kind: OrgLeaseKind::Review,
        actor_id: "reviewer-a",
        fencing_token_hash: &"b".repeat(64),
        acquired_at: 30,
        last_heartbeat_at: 30,
        expires_at: 60,
    };
    let update = OrgLeaseUpdate {
        id: "lease-2",
        workspace_id,
        last_heartbeat_at: 40,
        expires_at: 70,
        ended_at: Some(50),
        end_reason: Some(OrgLeaseEndReason::Release),
        expiry_event_id: None,
    };

    assert_eq!(lease.fencing_token_hash.len(), 64);
    assert_eq!(new.fencing_token_hash.len(), 64);
    assert_eq!(update.end_reason, Some(OrgLeaseEndReason::Release));
}

#[test]
fn stored_org_operation_debug_redacts_the_persisted_result() {
    let raw_token = "raw-claim-token";
    let token_hash = "a".repeat(64);
    let result = json!({
        "fencing_token": raw_token,
        "fencing_token_hash": token_hash,
        "lease_id": "lease-1"
    });
    let operation = StoredOrgOperation {
        workspace_id: WorkspaceId::from_str("11111111-1111-4111-8111-111111111111").unwrap(),
        operation_id: "claim-1".into(),
        request_fingerprint: "fingerprint-1".into(),
        result: result.clone(),
        created_at: 10,
    };

    let debug = format!("{operation:?}");

    assert!(!debug.contains(raw_token));
    assert!(!debug.contains(&token_hash));
    assert!(debug.contains("[REDACTED]"));
    assert_eq!(operation.result, result);
}
