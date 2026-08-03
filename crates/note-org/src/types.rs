use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};
use uuid::Uuid;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedItem {
    pub item: WorkItem,
    pub heading: Span,
    pub subtree: Span,
    pub state: Option<Span>,
    pub properties: BTreeMap<String, Span>,
    pub planning: BTreeMap<String, Span>,
    pub tags: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkItemId(Uuid);

impl FromStr for WorkItemId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value.trim()).map(Self)
    }
}

impl fmt::Display for WorkItemId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(Uuid);

impl FromStr for WorkspaceId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value.trim()).map(Self)
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DocumentId(Uuid);

impl FromStr for DocumentId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value.trim()).map(Self)
    }
}

impl fmt::Display for DocumentId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemType {
    Project,
    Epic,
    Issue,
    Task,
    Subtask,
    Review,
    Approval,
    Incident,
    Milestone,
}

impl WorkItemType {
    pub const ALL: [Self; 9] = [
        Self::Project,
        Self::Epic,
        Self::Issue,
        Self::Task,
        Self::Subtask,
        Self::Review,
        Self::Approval,
        Self::Incident,
        Self::Milestone,
    ];
}

impl FromStr for WorkItemType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "project" => Ok(Self::Project),
            "epic" => Ok(Self::Epic),
            "issue" => Ok(Self::Issue),
            "task" => Ok(Self::Task),
            "subtask" => Ok(Self::Subtask),
            "review" => Ok(Self::Review),
            "approval" => Ok(Self::Approval),
            "incident" => Ok(Self::Incident),
            "milestone" => Ok(Self::Milestone),
            other => Err(format!("unsupported work item type: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgTimestamp {
    pub raw: String,
    pub local: NaiveDateTime,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteLink {
    pub purpose: String,
    pub note_id: Uuid,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: WorkItemId,
    pub item_type: WorkItemType,
    pub parent_id: Option<WorkItemId>,
    pub level: usize,
    pub title: String,
    pub state: Option<String>,
    pub priority: Option<char>,
    pub tags: BTreeSet<String>,
    pub scheduled: Option<OrgTimestamp>,
    pub deadline: Option<OrgTimestamp>,
    pub assignee: Option<String>,
    pub depends_on: BTreeSet<WorkItemId>,
    pub requires_review: bool,
    pub note_links: Vec<NoteLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOptions {
    pub states: BTreeSet<String>,
}

impl ParseOptions {
    pub fn new(states: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            states: states.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyKey {
    Assignee,
    DependsOn,
    RequiresReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticEdit {
    SetState {
        item_id: WorkItemId,
        state: String,
    },
    SetProperty {
        item_id: WorkItemId,
        key: PropertyKey,
        value: Option<String>,
    },
    SetScheduled {
        item_id: WorkItemId,
        value: Option<String>,
    },
    SetTags {
        item_id: WorkItemId,
        tags: BTreeSet<String>,
    },
    AppendItem {
        parent_id: Option<WorkItemId>,
        item: NewWorkItem,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorkItem {
    pub id: WorkItemId,
    pub item_type: WorkItemType,
    pub title: String,
    pub state: String,
    pub priority: Option<char>,
    pub tags: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditedDocument {
    pub source: String,
    pub changed_items: BTreeSet<WorkItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedDocuments {
    pub source: String,
    pub target: String,
}
