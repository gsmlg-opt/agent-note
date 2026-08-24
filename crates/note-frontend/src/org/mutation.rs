use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const WEB_ACTOR_ID: &str = "web-ui";
pub const ORG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationSubmission {
    pub operation_id: String,
}

impl MutationSubmission {
    pub fn new() -> Self {
        Self {
            operation_id: new_id(),
        }
    }

    pub fn retry(&self) -> Self {
        self.clone()
    }
}

impl Default for MutationSubmission {
    fn default() -> Self {
        Self::new()
    }
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MutationEnvelope {
    schema_version: u32,
    actor_id: &'static str,
    operation_id: String,
}

impl MutationEnvelope {
    pub(crate) fn new(operation_id: String) -> Self {
        Self {
            schema_version: ORG_SCHEMA_VERSION,
            actor_id: WEB_ACTOR_ID,
            operation_id,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgMutationResult {
    pub schema_version: u32,
    pub workspace_id: String,
    pub operation_id: String,
    pub event_ids: Vec<String>,
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mutation_identity_is_a_uuid_and_is_stable_for_retry() {
        let submission = MutationSubmission::new();
        assert!(uuid::Uuid::parse_str(&submission.operation_id).is_ok());
        assert_eq!(submission.retry(), submission);
        assert_ne!(MutationSubmission::new(), submission);
        assert!(uuid::Uuid::parse_str(&new_id()).is_ok());
    }

    #[test]
    fn common_mutation_result_rejects_unknown_fields() {
        let payload = json!({
            "schema_version": 1,
            "workspace_id": "10000000-0000-4000-8000-000000000001",
            "operation_id": "20000000-0000-4000-8000-000000000001",
            "event_ids": ["30000000-0000-4000-8000-000000000001"],
            "workspace_revision": 2,
            "document_revisions": {"40000000-0000-4000-8000-000000000001": 1},
            "data": {"revision": 2}
        });
        let result: OrgMutationResult = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(result.workspace_revision, Some(2));

        let mut unknown = payload;
        unknown["source"] = json!("forbidden");
        assert!(serde_json::from_value::<OrgMutationResult>(unknown).is_err());
    }
}
