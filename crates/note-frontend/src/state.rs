//! Plain view-model types shared across pages. (The app is now multi-page via yew-router, so
//! state lives locally in each page rather than in one global reducer.)

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentContent {
    Text(String),
    Base64(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteAttachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    pub description: String,
    pub content: AttachmentContent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    /// Attached labels as (key, value) pairs.
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeletedNoteSummary {
    pub id: String,
    pub title: String,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResultSummary {
    pub id: String,
    pub title: String,
    /// Fused RRF rank-fusion score — label it as such in the UI, not "similarity" (docs/design.md §7).
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelKey {
    pub key: String,
    pub description: String,
    pub value_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelFilter {
    pub key: String,
    pub operator: String,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemConfig {
    #[serde(default)]
    pub duplicate_check: DuplicateCheckConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<DuplicateCheckRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckRule {
    pub terms: Vec<DuplicateCheckTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckTerm {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SystemInfo {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_path: String,
}

#[cfg(test)]
mod tests {
    use super::SystemInfo;

    #[test]
    fn system_info_accepts_backends_without_filesystem_metadata() {
        let info: SystemInfo = serde_json::from_str(
            r#"{
                "database_engine": "pg",
                "database_path": null,
                "database_size_bytes": null,
                "attachments_path": "/var/lib/agent-note/attachments"
            }"#,
        )
        .unwrap();

        assert_eq!(info.database_engine, "pg");
        assert_eq!(info.database_path, None);
        assert_eq!(info.database_size_bytes, None);
        assert_eq!(info.attachments_path, "/var/lib/agent-note/attachments");
    }
}
