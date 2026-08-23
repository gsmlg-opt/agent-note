use serde::{Deserialize, Serialize};

use super::{
    model::Document,
    mutation::{new_id, MutationEnvelope},
    url::encode_query,
};

pub const DOCUMENT_DEFAULT_LIMIT: u16 = 50;
pub const DOCUMENT_ALLOWED_LIMITS: [u16; 5] = [10, 25, 50, 100, 200];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentStatus {
    Active,
    Archived,
}

impl DocumentStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentListState {
    pub status: DocumentStatus,
    pub cursor: Option<String>,
    pub limit: u16,
}

impl Default for DocumentListState {
    fn default() -> Self {
        Self {
            status: DocumentStatus::Active,
            cursor: None,
            limit: DOCUMENT_DEFAULT_LIMIT,
        }
    }
}

impl DocumentListState {
    pub fn canonical_query(&self) -> String {
        let mut pairs = vec![("status", self.status.as_str().to_owned())];
        if let Some(cursor) = self.cursor.as_ref().filter(|cursor| !cursor.is_empty()) {
            pairs.push(("cursor", cursor.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        encode_query(&pairs)
    }

    pub fn set_status(&mut self, status: DocumentStatus) {
        if self.status != status {
            self.status = status;
            self.cursor = None;
        }
    }

    pub fn set_limit(&mut self, limit: u16) {
        let limit = normalize_limit(limit);
        if self.limit != limit {
            self.limit = limit;
            self.cursor = None;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentDraft {
    pub path: String,
}

impl DocumentDraft {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_document(document: &Document) -> Self {
        Self::new(document.path.clone())
    }

    pub fn validate(&self) -> Result<(), DocumentValidationError> {
        note_org::validate_document_path(&self.path).map_err(|error| DocumentValidationError {
            field: "path".into(),
            message: error.to_string(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentValidationError {
    pub field: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateDocumentBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub document_id: String,
    pub path: String,
}

impl CreateDocumentBody {
    pub fn from_draft(operation_id: String, draft: &DocumentDraft) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            document_id: new_id(),
            path: draft.path.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RenameDocumentBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub workspace_id: String,
    pub new_path: String,
    pub expected_revision: i64,
}

impl RenameDocumentBody {
    pub fn new(
        operation_id: String,
        workspace_id: String,
        new_path: String,
        expected_revision: i64,
    ) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            workspace_id,
            new_path,
            expected_revision,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRevisionBody {
    #[serde(flatten)]
    command: MutationEnvelope,
    pub workspace_id: String,
    pub expected_revision: i64,
}

impl DocumentRevisionBody {
    pub fn new(operation_id: String, workspace_id: String, expected_revision: i64) -> Self {
        Self {
            command: MutationEnvelope::new(operation_id),
            workspace_id,
            expected_revision,
        }
    }
}

fn normalize_limit(limit: u16) -> u16 {
    DOCUMENT_ALLOWED_LIMITS
        .contains(&limit)
        .then_some(limit)
        .unwrap_or(DOCUMENT_DEFAULT_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OPERATION_ID: &str = "20000000-0000-4000-8000-000000000001";
    const WORKSPACE_ID: &str = "10000000-0000-4000-8000-000000000001";

    #[test]
    fn document_list_state_defaults_and_builds_canonical_queries() {
        let mut state = DocumentListState::default();
        assert_eq!(state.status, DocumentStatus::Active);
        assert_eq!(state.cursor, None);
        assert_eq!(state.limit, 50);
        assert_eq!(state.canonical_query(), "status=active&limit=50");
        assert_eq!(DOCUMENT_ALLOWED_LIMITS, [10, 25, 50, 100, 200]);

        state.cursor = Some("opaque/+= cursor".into());
        state.set_status(DocumentStatus::Archived);
        assert_eq!(state.cursor, None);
        state.cursor = Some("opaque/+= cursor".into());
        state.set_limit(25);
        assert_eq!(state.cursor, None);
        assert_eq!(state.limit, 25);
        state.cursor = Some("opaque/+= cursor".into());
        assert_eq!(
            state.canonical_query(),
            "status=archived&cursor=opaque%2F%2B%3D%20cursor&limit=25"
        );

        state.set_limit(42);
        assert_eq!(state.limit, 50);
        assert_eq!(state.cursor, None);
    }

    #[test]
    fn document_status_is_limited_to_web_lifecycle_filters() {
        assert_eq!(
            serde_json::to_value(DocumentStatus::Active).unwrap(),
            json!("active")
        );
        assert_eq!(
            serde_json::to_value(DocumentStatus::Archived).unwrap(),
            json!("archived")
        );
        assert!(serde_json::from_value::<DocumentStatus>(json!("all")).is_err());
    }

    #[test]
    fn document_paths_use_the_domain_validator_and_exact_error() {
        let valid = DocumentDraft::new("roadmap/release.org");
        assert_eq!(valid.validate(), Ok(()));
        let stored = Document {
            id: "40000000-0000-4000-8000-000000000001".into(),
            path: valid.path.clone(),
            revision: 3,
            archived_at: None,
        };
        assert_eq!(DocumentDraft::from_document(&stored), valid);

        let invalid = DocumentDraft::new("../release.org");
        assert_eq!(
            invalid.validate(),
            Err(DocumentValidationError {
                field: "path".into(),
                message: "Org document path must be a portable relative lowercase .org path".into(),
            })
        );
    }

    #[test]
    fn document_mutation_bodies_match_the_rest_contract_without_source_fields() {
        let draft = DocumentDraft::new("roadmap/release.org");
        let create = CreateDocumentBody::from_draft(OPERATION_ID.into(), &draft);
        let document_id = create.document_id.clone();
        let create_json = serde_json::to_value(&create).unwrap();
        assert!(uuid::Uuid::parse_str(&document_id).is_ok());
        assert_eq!(create_json["schema_version"], 1);
        assert_eq!(create_json["actor_id"], "web-ui");
        assert_eq!(create_json["operation_id"], OPERATION_ID);
        assert_eq!(create_json["document_id"], document_id);
        assert_eq!(create_json["path"], "roadmap/release.org");
        assert_eq!(create_json.as_object().unwrap().len(), 5);

        let pending = create.clone();
        assert_eq!(pending.document_id, create.document_id);
        assert_eq!(
            serde_json::to_value(&pending).unwrap()["operation_id"],
            OPERATION_ID
        );

        let rename = RenameDocumentBody::new(
            OPERATION_ID.into(),
            WORKSPACE_ID.into(),
            "roadmap/v2.org".into(),
            7,
        );
        assert_eq!(
            serde_json::to_value(rename).unwrap(),
            json!({
                "schema_version": 1,
                "actor_id": "web-ui",
                "operation_id": OPERATION_ID,
                "workspace_id": WORKSPACE_ID,
                "new_path": "roadmap/v2.org",
                "expected_revision": 7
            })
        );

        let revision = DocumentRevisionBody::new(OPERATION_ID.into(), WORKSPACE_ID.into(), 8);
        assert_eq!(
            serde_json::to_value(revision).unwrap(),
            json!({
                "schema_version": 1,
                "actor_id": "web-ui",
                "operation_id": OPERATION_ID,
                "workspace_id": WORKSPACE_ID,
                "expected_revision": 8
            })
        );
    }
}
