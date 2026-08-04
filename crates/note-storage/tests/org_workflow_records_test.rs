use note_storage::{OrgAttemptStatus, OrgEventType};
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
