use note_org::WorkspaceId;
use note_pipelines::org::{CommandEnvelope, OrgError, OrgReadQuery};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

fn default_limit() -> u16 {
    50
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgMutationEnvelope {
    pub schema_version: u32,
    pub actor_id: String,
    pub operation_id: String,
}

impl OrgMutationEnvelope {
    pub fn into_pipeline(self, workspace_id: &str) -> Result<CommandEnvelope, OrgError> {
        let workspace_id = workspace_id
            .parse::<WorkspaceId>()
            .map_err(|_| OrgError::invalid_input("workspace_id must be a valid UUID"))?;
        Ok(CommandEnvelope {
            schema_version: self.schema_version,
            workspace_id,
            actor_id: self.actor_id,
            operation_id: self.operation_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OrgPageQuery {
    /// Opaque cursor returned by the preceding page; clients must not inspect or modify it.
    pub cursor: Option<String>,
    #[serde(default = "default_limit")]
    #[schema(default = 50, minimum = 1, maximum = 200)]
    pub limit: u16,
    #[serde(default)]
    #[schema(default = false)]
    pub include_archived: bool,
}

impl From<OrgPageQuery> for OrgReadQuery {
    fn from(value: OrgPageQuery) -> Self {
        Self {
            cursor: value.cursor,
            limit: Some(usize::from(value.limit)),
            include_archived: value.include_archived,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_envelope_conversion_preserves_transport_neutral_fields() {
        let rest = OrgMutationEnvelope {
            schema_version: 1,
            actor_id: "agent-a".to_owned(),
            operation_id: "operation-a".to_owned(),
        };
        let rest_json = serde_json::to_value(&rest).unwrap();
        let pipeline = rest
            .into_pipeline("10000000-0000-4000-8000-000000000001")
            .unwrap();
        let pipeline_json = serde_json::to_value(pipeline).unwrap();

        for field in ["schema_version", "actor_id", "operation_id"] {
            assert_eq!(rest_json[field], pipeline_json[field]);
        }
        assert_eq!(
            pipeline_json["workspace_id"],
            "10000000-0000-4000-8000-000000000001"
        );
    }

    #[test]
    fn page_query_conversion_uses_documented_default_and_bounds_type() {
        let query: OrgPageQuery = serde_json::from_value(serde_json::json!({
            "cursor": "opaque-cursor"
        }))
        .unwrap();
        assert_eq!(query.limit, 50);

        let pipeline: OrgReadQuery = query.into();
        assert_eq!(pipeline.cursor.as_deref(), Some("opaque-cursor"));
        assert_eq!(pipeline.limit, Some(50));
        assert!(!pipeline.include_archived);
    }
}
