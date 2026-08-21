//! Plain view-model types shared across pages. (The app is now multi-page via yew-router, so
//! state lives locally in each page rather than in one global reducer.)

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use yew::Reducible;

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
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeletedNoteSummary {
    pub id: String,
    pub title: String,
    pub labels: Vec<(String, String)>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: i64,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResultSummary {
    pub id: String,
    pub title: String,
    pub revision: i64,
    /// Fused RRF rank-fusion score — label it as such in the UI, not "similarity" (docs/design.md §7).
    pub score: f32,
}

pub fn stale_retry_blocked(pending: bool, has_conflict: bool) -> bool {
    pending || has_conflict
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaleRevisionGateAction {
    Conflict,
    Dismiss,
    RefreshFinished { started_epoch: u64, succeeded: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaleRevisionGate {
    conflict_epoch: u64,
    blocked: bool,
}

impl StaleRevisionGate {
    pub fn refresh_epoch(&self) -> u64 {
        self.conflict_epoch
    }

    pub fn blocked(&self) -> bool {
        self.blocked
    }

    pub fn conflict_notice_visible(&self) -> bool {
        self.blocked
    }
}

impl Reducible for StaleRevisionGate {
    type Action = StaleRevisionGateAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            StaleRevisionGateAction::Conflict => Self {
                conflict_epoch: self.conflict_epoch.saturating_add(1),
                blocked: true,
            }
            .into(),
            StaleRevisionGateAction::Dismiss => self,
            StaleRevisionGateAction::RefreshFinished {
                started_epoch,
                succeeded: true,
            } if started_epoch == self.conflict_epoch => Self {
                conflict_epoch: self.conflict_epoch,
                blocked: false,
            }
            .into(),
            StaleRevisionGateAction::RefreshFinished { .. } => self,
        }
    }
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemConfig {
    #[serde(default)]
    pub category_labels: Vec<String>,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub duplicate_check: DuplicateCheckConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_search_minimum_score")]
    pub minimum_score: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            minimum_score: default_search_minimum_score(),
        }
    }
}

fn default_search_minimum_score() -> f32 {
    0.01
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
    pub attachments_engine: String,
    pub attachments_location: Option<String>,
    pub embedding_engine: String,
    pub embedding_model: String,
    pub embedding_fingerprint: String,
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use yew::Reducible;

    use super::{
        stale_retry_blocked, StaleRevisionGate, StaleRevisionGateAction, SystemConfig, SystemInfo,
    };

    #[test]
    fn stale_conflicts_block_retry_until_reload_clears_them() {
        assert!(stale_retry_blocked(false, true));
        assert!(stale_retry_blocked(true, false));
        assert!(!stale_retry_blocked(false, false));
    }

    #[test]
    fn stale_conflict_survives_dismiss_and_failed_reload() {
        let gate = Rc::new(StaleRevisionGate::default());
        let old_refresh_epoch = gate.refresh_epoch();
        let gate = gate.reduce(StaleRevisionGateAction::Conflict);
        let current_refresh_epoch = gate.refresh_epoch();
        let gate = gate.reduce(StaleRevisionGateAction::Dismiss);
        assert!(stale_retry_blocked(false, gate.blocked()));

        let gate = gate.reduce(StaleRevisionGateAction::RefreshFinished {
            started_epoch: old_refresh_epoch,
            succeeded: false,
        });
        assert!(stale_retry_blocked(false, gate.blocked()));
        let gate = gate.reduce(StaleRevisionGateAction::RefreshFinished {
            started_epoch: old_refresh_epoch,
            succeeded: true,
        });
        assert!(stale_retry_blocked(false, gate.blocked()));
        assert!(gate.conflict_notice_visible());

        let gate = gate.reduce(StaleRevisionGateAction::RefreshFinished {
            started_epoch: current_refresh_epoch,
            succeeded: true,
        });
        assert!(!stale_retry_blocked(false, gate.blocked()));
        assert!(!gate.conflict_notice_visible());
    }

    #[test]
    fn system_info_accepts_backends_without_filesystem_metadata() {
        let info: SystemInfo = serde_json::from_str(
            r#"{
                "database_engine": "pg",
                "database_path": null,
                "database_size_bytes": null,
                "attachments_engine": "s3",
                "attachments_location": null,
                "embedding_engine": "openai",
                "embedding_model": "bge-m3",
                "embedding_fingerprint": "bge-m3:1024"
            }"#,
        )
        .unwrap();

        assert_eq!(info.database_engine, "pg");
        assert_eq!(info.database_path, None);
        assert_eq!(info.database_size_bytes, None);
        assert_eq!(info.attachments_engine, "s3");
        assert_eq!(info.attachments_location, None);
        assert_eq!(info.embedding_engine, "openai");
        assert_eq!(info.embedding_model, "bge-m3");
        assert_eq!(info.embedding_fingerprint, "bge-m3:1024");
    }

    #[test]
    fn system_config_deserializes_category_labels_with_an_empty_backward_compatible_default() {
        let omitted: SystemConfig = serde_json::from_str(r#"{"duplicate_check": {}}"#).unwrap();
        let configured: SystemConfig =
            serde_json::from_str(r#"{"category_labels":["team","project"],"duplicate_check":{}}"#)
                .unwrap();

        assert!(omitted.category_labels.is_empty());
        assert_eq!(configured.category_labels, ["team", "project"]);
    }

    #[test]
    fn system_config_defaults_omitted_search_minimum_score() {
        let config: SystemConfig =
            serde_json::from_str(r#"{"category_labels":[],"duplicate_check":{}}"#).unwrap();

        assert_eq!(config.search.minimum_score, 0.01);
    }

    #[test]
    fn system_config_preserves_configured_search_minimum_score() {
        let config: SystemConfig = serde_json::from_str(
            r#"{"category_labels":[],"search":{"minimum_score":0.025},"duplicate_check":{}}"#,
        )
        .unwrap();

        assert_eq!(config.search.minimum_score, 0.025);
    }

    #[test]
    fn system_config_defaults_omitted_search_minimum_score_field() {
        let config: SystemConfig =
            serde_json::from_str(r#"{"category_labels":[],"search":{},"duplicate_check":{}}"#)
                .unwrap();

        assert_eq!(config.search.minimum_score, 0.01);
    }
}
