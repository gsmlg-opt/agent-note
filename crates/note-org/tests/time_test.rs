use chrono::NaiveDate;
use note_org::{
    resolve_org_timestamp, DocumentId, OrgTimestamp, TimeZoneError, WorkspaceId, WorkspacePolicy,
};
use std::str::FromStr;

fn timestamp(date: (i32, u32, u32), time: (u32, u32, u32)) -> OrgTimestamp {
    OrgTimestamp {
        raw: format!(
            "<{:04}-{:02}-{:02} {:02}:{:02}>",
            date.0, date.1, date.2, time.0, time.1
        ),
        local: NaiveDate::from_ymd_opt(date.0, date.1, date.2)
            .unwrap()
            .and_hms_opt(time.0, time.1, time.2)
            .unwrap(),
    }
}

#[test]
fn persistence_ids_roundtrip_as_uuid_strings() {
    let workspace = WorkspaceId::from_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
    let document = DocumentId::from_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap();
    assert_eq!(
        workspace.to_string(),
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    );
    assert_eq!(document.to_string(), "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb");
}

#[test]
fn default_policy_disallows_cross_workspace_agendas() {
    assert!(!WorkspacePolicy::engineering_default().allow_cross_workspace_agenda);
}

#[test]
fn valid_workspace_time_resolves_to_utc() {
    let resolved =
        resolve_org_timestamp(&timestamp((2026, 8, 4), (15, 30, 0)), "Asia/Shanghai").unwrap();
    assert_eq!(resolved.timezone, "Asia/Shanghai");
    assert_eq!(resolved.utc_timestamp, 1_785_828_600);
}

#[test]
fn invalid_and_daylight_saving_times_are_rejected() {
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 8, 4), (15, 30, 0)), "Mars/Olympus"),
        Err(TimeZoneError::InvalidZone(_))
    ));
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 11, 1), (1, 30, 0)), "America/New_York"),
        Err(TimeZoneError::Ambiguous { .. })
    ));
    assert!(matches!(
        resolve_org_timestamp(&timestamp((2026, 3, 8), (2, 30, 0)), "America/New_York"),
        Err(TimeZoneError::Nonexistent { .. })
    ));
}
