use std::{collections::BTreeMap, fmt, str::FromStr};

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use super::model::OperationalView;

pub const DEFAULT_LIMIT: u16 = 50;
pub const ALLOWED_LIMITS: [u16; 5] = [10, 25, 50, 100, 200];

impl OperationalView {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Assigned => "assigned",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Review => "review",
            Self::Scheduled => "scheduled",
            Self::UpcomingDeadline => "upcoming_deadline",
            Self::Failed => "failed",
            Self::ExpiredLease => "expired_lease",
            Self::Completed => "completed",
        }
    }
}

impl FromStr for OperationalView {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ready" => Ok(Self::Ready),
            "assigned" => Ok(Self::Assigned),
            "running" => Ok(Self::Running),
            "blocked" => Ok(Self::Blocked),
            "review" => Ok(Self::Review),
            "scheduled" => Ok(Self::Scheduled),
            "upcoming_deadline" => Ok(Self::UpcomingDeadline),
            "failed" => Ok(Self::Failed),
            "expired_lease" => Ok(Self::ExpiredLease),
            "completed" => Ok(Self::Completed),
            _ => Err(()),
        }
    }
}

impl fmt::Display for OperationalView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceListState {
    pub include_archived: bool,
    pub cursor: Option<String>,
    pub limit: u16,
}

impl Default for WorkspaceListState {
    fn default() -> Self {
        Self {
            include_archived: false,
            cursor: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl WorkspaceListState {
    pub fn parse(query: &str) -> Self {
        let params = parse_query(query);
        Self {
            include_archived: matches!(
                params.get("include_archived").map(String::as_str),
                Some("true")
            ),
            cursor: nonempty(params.get("cursor")),
            limit: parse_limit(params.get("limit")),
        }
    }

    pub fn canonical_query(&self) -> String {
        let mut pairs = vec![("include_archived", self.include_archived.to_string())];
        if let Some(cursor) = &self.cursor {
            pairs.push(("cursor", cursor.clone()));
        }
        pairs.push(("limit", self.limit.to_string()));
        encode_query(&pairs)
    }

    pub fn set_include_archived(&mut self, include_archived: bool) {
        if self.include_archived != include_archived {
            self.include_archived = include_archived;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriorityFilter {
    Priority(char),
    None,
}

impl PriorityFilter {
    fn parse(value: &str) -> Option<Self> {
        if value == "none" {
            return Some(Self::None);
        }
        let mut characters = value.chars();
        let priority = characters.next()?;
        (characters.next().is_none() && priority.is_ascii_uppercase())
            .then_some(Self::Priority(priority))
    }

    pub fn as_query_value(self) -> String {
        match self {
            Self::Priority(priority) => priority.to_string(),
            Self::None => "none".to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceQueryState {
    pub view: OperationalView,
    pub item_type: Option<String>,
    pub state: Option<String>,
    pub priority: Option<PriorityFilter>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub cursor: Option<String>,
    pub limit: u16,
}

impl Default for WorkspaceQueryState {
    fn default() -> Self {
        Self {
            view: OperationalView::Ready,
            item_type: None,
            state: None,
            priority: None,
            tags: Vec::new(),
            assignee: None,
            from: None,
            to: None,
            cursor: None,
            limit: DEFAULT_LIMIT,
        }
    }
}

impl WorkspaceQueryState {
    pub fn parse(query: &str) -> Self {
        Self::from_params(&parse_query(query), "", false).unwrap_or_default()
    }

    fn from_params(
        params: &BTreeMap<String, String>,
        prefix: &str,
        strict: bool,
    ) -> Result<Self, ()> {
        let key = |name: &str| format!("{prefix}{name}");
        let parse_or_default = |name: &str, valid: bool| {
            if strict && params.contains_key(&key(name)) && !valid {
                Err(())
            } else {
                Ok(())
            }
        };

        let view_value = params.get(&key("view"));
        let view = view_value
            .and_then(|value| value.parse().ok())
            .unwrap_or(OperationalView::Ready);
        parse_or_default(
            "view",
            view_value.is_none() || view_value.unwrap().parse::<OperationalView>().is_ok(),
        )?;

        let priority_value = params.get(&key("priority"));
        let priority = priority_value.and_then(|value| PriorityFilter::parse(value));
        parse_or_default("priority", priority_value.is_none() || priority.is_some())?;

        let item_type_value = params.get(&key("item_type"));
        let item_type = item_type_value.and_then(|value| canonical_item_type(value));
        parse_or_default(
            "item_type",
            item_type_value.is_none() || item_type.is_some(),
        )?;

        let limit_value = params.get(&key("limit"));
        let parsed_limit = limit_value.and_then(|value| value.parse::<u16>().ok());
        parse_or_default(
            "limit",
            limit_value.is_none()
                || parsed_limit.is_some_and(|limit| ALLOWED_LIMITS.contains(&limit)),
        )?;

        let from_value = params.get(&key("from"));
        let from = from_value.and_then(|value| canonical_utc(value));
        parse_or_default("from", from_value.is_none() || from.is_some())?;
        let to_value = params.get(&key("to"));
        let to = to_value.and_then(|value| canonical_utc(value));
        parse_or_default("to", to_value.is_none() || to.is_some())?;

        Ok(Self {
            view,
            item_type,
            state: normalized_text(params.get(&key("state"))),
            priority,
            tags: canonical_tags(params.get(&key("tags"))),
            assignee: normalized_text(params.get(&key("assignee"))),
            from,
            to,
            cursor: nonempty(params.get(&key("cursor"))),
            limit: parsed_limit
                .filter(|limit| ALLOWED_LIMITS.contains(limit))
                .unwrap_or(DEFAULT_LIMIT),
        })
    }

    pub fn canonical_query(&self) -> String {
        encode_query(&self.pairs(""))
    }

    fn pairs(&self, prefix: &'static str) -> Vec<(&'static str, String)> {
        let names = ReturnNames::for_prefix(prefix);
        let mut pairs = vec![(names.view, self.view.to_string())];
        push_optional(&mut pairs, names.item_type, self.item_type.as_ref());
        push_optional(&mut pairs, names.state, self.state.as_ref());
        if let Some(priority) = self.priority {
            pairs.push((names.priority, priority.as_query_value()));
        }
        if !self.tags.is_empty() {
            pairs.push((names.tags, self.tags.join(",")));
        }
        push_optional(&mut pairs, names.assignee, self.assignee.as_ref());
        push_optional(&mut pairs, names.from, self.from.as_ref());
        push_optional(&mut pairs, names.to, self.to.as_ref());
        push_optional(&mut pairs, names.cursor, self.cursor.as_ref());
        pairs.push((names.limit, self.limit.to_string()));
        pairs
    }

    pub fn set_view(&mut self, view: OperationalView) {
        if self.view != view {
            self.view = view;
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

    pub fn replace_filters(&mut self, filters: WorkspaceFilters) {
        let before = self.clone();
        self.item_type = filters
            .item_type
            .and_then(|value| canonical_item_type(&value));
        self.state = normalize_owned(filters.state);
        self.priority = filters.priority;
        self.tags = canonical_owned_tags(filters.tags);
        self.assignee = normalize_owned(filters.assignee);
        self.from = filters.from.and_then(|value| canonical_utc(&value));
        self.to = filters.to.and_then(|value| canonical_utc(&value));
        if self.without_cursor() != before.without_cursor() {
            self.cursor = None;
        }
    }

    fn without_cursor(&self) -> Self {
        let mut state = self.clone();
        state.cursor = None;
        state
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceFilters {
    pub item_type: Option<String>,
    pub state: Option<String>,
    pub priority: Option<PriorityFilter>,
    pub tags: Vec<String>,
    pub assignee: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgReturnContext {
    workspace_id: String,
    pub state: WorkspaceQueryState,
}

impl OrgReturnContext {
    pub fn new(workspace_id: impl Into<String>, state: WorkspaceQueryState) -> Self {
        Self {
            workspace_id: workspace_id.into(),
            state,
        }
    }

    pub fn parse(workspace_id: &str, query: &str) -> Self {
        let params = parse_query(query);
        let state = WorkspaceQueryState::from_params(&params, "return_", true).unwrap_or_default();
        Self::new(workspace_id, state)
    }

    pub fn validated(&self, workspace_id: &str) -> WorkspaceQueryState {
        if self.workspace_id == workspace_id {
            self.state.clone()
        } else {
            WorkspaceQueryState::default()
        }
    }

    pub fn canonical_query(&self) -> String {
        encode_query(&self.state.pairs("return_"))
    }
}

pub fn workspace_path(workspace_id: &str, state: &WorkspaceQueryState) -> String {
    format!(
        "/org/{}?{}",
        urlencoding::encode(workspace_id),
        state.canonical_query()
    )
}

pub fn item_path(workspace_id: &str, item_id: &str, context: &OrgReturnContext) -> String {
    let context = OrgReturnContext::new(workspace_id, context.validated(workspace_id));
    format!(
        "/org/{}/items/{}?{}",
        urlencoding::encode(workspace_id),
        urlencoding::encode(item_id),
        context.canonical_query()
    )
}

pub fn rfc3339_to_epoch(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date_time| date_time.timestamp())
}

fn canonical_utc(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value.trim())
        .ok()
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
}

fn parse_query(query: &str) -> BTreeMap<String, String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Some((
                urlencoding::decode(key).ok()?.into_owned(),
                urlencoding::decode(value).ok()?.into_owned(),
            ))
        })
        .collect()
}

pub(crate) fn encode_query(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn normalize_limit(limit: u16) -> u16 {
    ALLOWED_LIMITS
        .contains(&limit)
        .then_some(limit)
        .unwrap_or(DEFAULT_LIMIT)
}

fn parse_limit(value: Option<&String>) -> u16 {
    value
        .and_then(|value| value.parse().ok())
        .map(normalize_limit)
        .unwrap_or(DEFAULT_LIMIT)
}

fn nonempty(value: Option<&String>) -> Option<String> {
    value.filter(|value| !value.is_empty()).cloned()
}

fn normalized_text(value: Option<&String>) -> Option<String> {
    value.and_then(|value| normalize_owned(Some(value.clone())))
}

fn normalize_owned(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn canonical_item_type(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_lowercase();
    matches!(
        value.as_str(),
        "project"
            | "epic"
            | "issue"
            | "task"
            | "subtask"
            | "review"
            | "approval"
            | "incident"
            | "milestone"
    )
    .then_some(value)
}

fn canonical_tags(value: Option<&String>) -> Vec<String> {
    canonical_owned_tags(
        value
            .map(|value| value.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
    )
}

fn canonical_owned_tags(tags: Vec<String>) -> Vec<String> {
    let mut tags = tags
        .into_iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>();
    tags.sort();
    tags.dedup();
    tags
}

fn push_optional<'a>(
    pairs: &mut Vec<(&'static str, String)>,
    key: &'static str,
    value: Option<&'a String>,
) {
    if let Some(value) = value {
        pairs.push((key, value.clone()));
    }
}

struct ReturnNames {
    view: &'static str,
    item_type: &'static str,
    state: &'static str,
    priority: &'static str,
    tags: &'static str,
    assignee: &'static str,
    from: &'static str,
    to: &'static str,
    cursor: &'static str,
    limit: &'static str,
}

impl ReturnNames {
    const fn for_prefix(prefix: &str) -> Self {
        if prefix.is_empty() {
            Self {
                view: "view",
                item_type: "item_type",
                state: "state",
                priority: "priority",
                tags: "tags",
                assignee: "assignee",
                from: "from",
                to: "to",
                cursor: "cursor",
                limit: "limit",
            }
        } else {
            Self {
                view: "return_view",
                item_type: "return_item_type",
                state: "return_state",
                priority: "return_priority",
                tags: "return_tags",
                assignee: "return_assignee",
                from: "return_from",
                to: "return_to",
                cursor: "return_cursor",
                limit: "return_limit",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_list_normalizes_and_canonicalizes() {
        let state =
            WorkspaceListState::parse("unknown=x&include_archived=true&cursor=a%2Fb%20c&limit=25");
        assert_eq!(state.cursor.as_deref(), Some("a/b c"));
        assert_eq!(
            state.canonical_query(),
            "include_archived=true&cursor=a%2Fb%20c&limit=25"
        );
        assert_eq!(
            WorkspaceListState::parse("limit=11"),
            WorkspaceListState::default()
        );
    }

    #[test]
    fn workspace_query_round_trips_every_filter_and_reserved_character() {
        let query = "view=upcoming_deadline&item_type=incident&state=WAITING%2FEXTERNAL&priority=none&tags=z%2Fa%2Cbuild%20ready&assignee=agent%2Bone%40example.test&from=2026-08-05T01%3A02%3A03Z&to=2026-08-06T09%3A02%3A03%2B08%3A00&cursor=opaque%2F%2B%3D&limit=100";
        let state = WorkspaceQueryState::parse(query);
        assert_eq!(state.view, OperationalView::UpcomingDeadline);
        assert_eq!(state.tags, vec!["build ready", "z/a"]);
        assert_eq!(state.to.as_deref(), Some("2026-08-06T01:02:03Z"));
        let canonical = state.canonical_query();
        assert_eq!(WorkspaceQueryState::parse(&canonical), state);
        assert!(canonical.contains("cursor=opaque%2F%2B%3D"));
    }

    #[test]
    fn invalid_local_values_fall_back_and_filter_changes_clear_cursor() {
        let mut state = WorkspaceQueryState::parse(
            "view=nope&item_type=wizard&priority=1&from=yesterday&limit=999&cursor=next",
        );
        assert_eq!(state.view, OperationalView::Ready);
        assert_eq!(state.item_type, None);
        assert_eq!(state.priority, None);
        assert_eq!(state.from, None);
        assert_eq!(state.limit, DEFAULT_LIMIT);
        state.set_view(OperationalView::Review);
        assert_eq!(state.cursor, None);
        state.cursor = Some("next".into());
        state.replace_filters(WorkspaceFilters {
            tags: vec!["b".into(), "a".into(), "b".into()],
            ..Default::default()
        });
        assert_eq!(state.tags, vec!["a", "b"]);
        assert_eq!(state.cursor, None);
    }

    #[test]
    fn typed_item_return_restores_exact_state_and_rejects_malformed_or_cross_workspace() {
        let state = WorkspaceQueryState::parse("view=failed&state=FAILED&cursor=page%2F2&limit=25");
        let context = OrgReturnContext::new("workspace-a", state.clone());
        let query = context.canonical_query();
        assert_eq!(
            OrgReturnContext::parse("workspace-a", &query).validated("workspace-a"),
            state
        );
        assert_eq!(
            OrgReturnContext::parse(
                "workspace-a",
                "return_view=failed&return_item_type=wizard&return_limit=25",
            )
            .validated("workspace-a"),
            WorkspaceQueryState::default()
        );
        assert_eq!(
            context.validated("workspace-b"),
            WorkspaceQueryState::default()
        );
        assert_eq!(
            workspace_path("workspace/a", &state),
            format!("/org/workspace%2Fa?{}", state.canonical_query())
        );
        assert!(item_path("workspace-a", "item/one", &context)
            .contains("item%2Fone?return_view=failed"));
    }

    #[test]
    fn cursor_is_cleared_when_directory_controls_change() {
        let mut state = WorkspaceListState::parse("include_archived=false&cursor=next&limit=50");
        state.set_include_archived(true);
        assert_eq!(state.cursor, None);
        state.cursor = Some("next".into());
        state.set_limit(25);
        assert_eq!(state.cursor, None);
    }
}
