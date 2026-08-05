use std::fmt;

use gloo_net::http::{Request, Response};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use super::{
    model::{Event, ItemContext, OperationalPage, Page, Workspace, WorkspaceSummary},
    url::{rfc3339_to_epoch, WorkspaceListState, WorkspaceQueryState},
};

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct OrgApiError {
    pub code: String,
    pub message: String,
    pub details: Value,
    pub retryable: bool,
    #[serde(skip)]
    pub status: Option<u16>,
}

impl OrgApiError {
    fn transport(message: &'static str) -> Self {
        Self {
            code: "transport_error".to_owned(),
            message: message.to_owned(),
            details: Value::Object(Default::default()),
            retryable: true,
            status: None,
        }
    }

    fn unexpected(status: u16) -> Self {
        Self {
            code: "unexpected_response".to_owned(),
            message: "The Org service returned an unexpected response".to_owned(),
            details: Value::Object(Default::default()),
            retryable: status >= 500,
            status: Some(status),
        }
    }
}

impl fmt::Display for OrgApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

pub async fn list_workspaces(
    state: &WorkspaceListState,
) -> Result<Page<WorkspaceSummary>, OrgApiError> {
    get_json(&workspace_list_url(state)).await
}

pub async fn get_workspace(workspace_id: &str) -> Result<Workspace, OrgApiError> {
    get_json(&workspace_url(workspace_id)).await
}

pub async fn query_queue(
    workspace_id: &str,
    state: &WorkspaceQueryState,
    include_archived: bool,
) -> Result<OperationalPage, OrgApiError> {
    get_json(&operational_url(
        "queue",
        workspace_id,
        state,
        include_archived,
    ))
    .await
}

pub async fn query_agenda(
    workspace_id: &str,
    state: &WorkspaceQueryState,
    include_archived: bool,
) -> Result<OperationalPage, OrgApiError> {
    get_json(&operational_url(
        "agenda",
        workspace_id,
        state,
        include_archived,
    ))
    .await
}

pub async fn get_item_context(
    workspace_id: &str,
    item_id: &str,
) -> Result<ItemContext, OrgApiError> {
    get_json(&item_context_url(workspace_id, item_id)).await
}

pub async fn list_item_events(
    workspace_id: &str,
    item_id: &str,
    cursor: Option<&str>,
    limit: u16,
) -> Result<Page<Event>, OrgApiError> {
    get_json(&item_events_url(workspace_id, item_id, cursor, limit)).await
}

async fn get_json<T: DeserializeOwned>(url: &str) -> Result<T, OrgApiError> {
    let response = Request::get(url)
        .send()
        .await
        .map_err(|_| OrgApiError::transport("The Org service could not be reached"))?;
    decode_response(response).await
}

async fn decode_response<T: DeserializeOwned>(response: Response) -> Result<T, OrgApiError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|_| OrgApiError::unexpected(status))?;
    decode_body(status, &body)
}

fn decode_body<T: DeserializeOwned>(status: u16, body: &str) -> Result<T, OrgApiError> {
    if (200..300).contains(&status) {
        return serde_json::from_str(body).map_err(|_| OrgApiError::unexpected(status));
    }
    let mut error = serde_json::from_str::<OrgApiError>(body)
        .unwrap_or_else(|_| OrgApiError::unexpected(status));
    error.status = Some(status);
    Err(error)
}

pub fn workspace_list_url(state: &WorkspaceListState) -> String {
    format!("/api/org/workspaces?{}", state.canonical_query())
}

pub fn workspace_url(workspace_id: &str) -> String {
    format!("/api/org/workspaces/{}", encode(workspace_id))
}

pub fn operational_url(
    family: &str,
    workspace_id: &str,
    state: &WorkspaceQueryState,
    include_archived: bool,
) -> String {
    let mut pairs = vec![
        ("workspace_ids", workspace_id.to_owned()),
        ("view", state.view.to_string()),
    ];
    push_optional(&mut pairs, "item_type", state.item_type.as_ref());
    push_optional(&mut pairs, "state", state.state.as_ref());
    if let Some(priority) = state.priority {
        pairs.push(("priority", priority.as_query_value()));
    }
    if !state.tags.is_empty() {
        pairs.push(("tags", state.tags.join(",")));
    }
    push_optional(&mut pairs, "assignee", state.assignee.as_ref());
    if let Some(from) = state.from.as_deref().and_then(rfc3339_to_epoch) {
        pairs.push(("from", from.to_string()));
    }
    if let Some(to) = state.to.as_deref().and_then(rfc3339_to_epoch) {
        pairs.push(("to", to.to_string()));
    }
    if include_archived {
        pairs.push(("include_archived", "true".to_owned()));
    }
    push_optional(&mut pairs, "cursor", state.cursor.as_ref());
    pairs.push(("limit", state.limit.to_string()));
    format!("/api/org/{family}?{}", encode_pairs(&pairs))
}

pub fn item_context_url(workspace_id: &str, item_id: &str) -> String {
    format!(
        "/api/org/items/{}/context?workspace_id={}",
        encode(item_id),
        encode(workspace_id)
    )
}

pub fn item_events_url(
    workspace_id: &str,
    item_id: &str,
    cursor: Option<&str>,
    limit: u16,
) -> String {
    let mut pairs = vec![
        ("subject_kind", "work_item".to_owned()),
        ("subject_id", item_id.to_owned()),
    ];
    if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
        pairs.push(("cursor", cursor.to_owned()));
    }
    pairs.push(("limit", limit.to_string()));
    format!(
        "/api/org/workspaces/{}/events?{}",
        encode(workspace_id),
        encode_pairs(&pairs)
    )
}

fn encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

fn encode_pairs(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn push_optional(
    pairs: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&String>,
) {
    if let Some(value) = value {
        pairs.push((key, value.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::{model::OperationalView, url::PriorityFilter};
    use serde_json::json;

    #[test]
    fn parses_structured_errors_and_uses_safe_display_copy() {
        let error = decode_body::<Value>(
            409,
            r#"{"code":"active_lease","message":"Work item is leased","details":{"lease_id":"safe"},"retryable":false}"#,
        )
        .unwrap_err();
        assert_eq!(error.code, "active_lease");
        assert_eq!(error.details["lease_id"], "safe");
        assert_eq!(error.status, Some(409));
        assert_eq!(error.to_string(), "active_lease: Work item is leased");
        assert!(!error.to_string().contains("lease_id"));
    }

    #[test]
    fn unexpected_non_json_responses_never_echo_raw_bodies() {
        let body = "proxy failure with secret-cookie=do-not-render";
        let error = decode_body::<Value>(502, body).unwrap_err();
        assert_eq!(error.code, "unexpected_response");
        assert!(error.retryable);
        assert!(!format!("{error:?}{error}").contains("secret-cookie"));
    }

    #[test]
    fn url_builders_encode_every_component_and_preserve_pagination() {
        let list = WorkspaceListState {
            include_archived: true,
            cursor: Some("opaque/+= cursor".into()),
            limit: 25,
        };
        assert_eq!(
            workspace_list_url(&list),
            "/api/org/workspaces?include_archived=true&cursor=opaque%2F%2B%3D%20cursor&limit=25"
        );
        assert_eq!(
            workspace_url("workspace/one"),
            "/api/org/workspaces/workspace%2Fone"
        );

        let state = WorkspaceQueryState {
            view: OperationalView::Failed,
            item_type: Some("incident".into()),
            state: Some("WAITING/EXTERNAL".into()),
            priority: Some(PriorityFilter::Priority('A')),
            tags: vec!["build ready".into(), "ops/on-call".into()],
            assignee: Some("agent+one@example.test".into()),
            from: Some("2026-08-05T01:02:03Z".into()),
            to: Some("2026-08-06T01:02:03Z".into()),
            cursor: Some("opaque/+= cursor".into()),
            limit: 100,
        };
        let url = operational_url("queue", "workspace/one", &state, true);
        for expected in [
            "workspace_ids=workspace%2Fone",
            "view=failed",
            "item_type=incident",
            "state=WAITING%2FEXTERNAL",
            "priority=A",
            "tags=build%20ready%2Cops%2Fon-call",
            "assignee=agent%2Bone%40example.test",
            "from=1785891723",
            "to=1785978123",
            "include_archived=true",
            "cursor=opaque%2F%2B%3D%20cursor",
            "limit=100",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
        assert_eq!(
            item_context_url("workspace/one", "item one"),
            "/api/org/items/item%20one/context?workspace_id=workspace%2Fone"
        );
        assert_eq!(item_events_url("workspace/one", "item/one", Some("next/+"), 25), "/api/org/workspaces/workspace%2Fone/events?subject_kind=work_item&subject_id=item%2Fone&cursor=next%2F%2B&limit=25");
    }

    #[test]
    fn successful_payloads_deserialize_without_transport_envelopes() {
        let value: Value =
            decode_body(200, &json!({"items":[],"next_cursor":null}).to_string()).unwrap();
        assert_eq!(value["items"], json!([]));
    }

    #[test]
    fn org_client_source_is_get_only_and_has_no_identity_or_token_inputs() {
        let source = include_str!("api.rs").split("#[cfg(test)]").next().unwrap();
        assert!(source.contains("Request::get"));
        for forbidden in [
            "Request::post",
            "Request::put",
            "Request::patch",
            "Request::delete",
            "/mcp",
            "fencing_token",
            "operation_id",
            "actor_id",
            "authorization",
            "session_storage",
        ] {
            assert!(
                !source.contains(forbidden),
                "Org client contains forbidden surface: {forbidden}"
            );
        }
    }
}
