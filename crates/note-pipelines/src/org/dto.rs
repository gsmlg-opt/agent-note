use note_org::WorkspaceId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const ORG_COMMAND_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgCommandKind {
    name: &'static str,
    schema_version: u32,
}

impl OrgCommandKind {
    pub const fn new(name: &'static str, schema_version: u32) -> Self {
        Self {
            name,
            schema_version,
        }
    }

    pub const fn name(self) -> &'static str {
        self.name
    }

    pub const fn schema_version(self) -> u32 {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub actor_id: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgCommandResult {
    pub schema_version: u32,
    pub workspace_id: WorkspaceId,
    pub operation_id: String,
    pub event_ids: Vec<String>,
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub data: serde_json::Value,
}
