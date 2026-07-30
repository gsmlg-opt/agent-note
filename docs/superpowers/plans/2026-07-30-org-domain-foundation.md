# Org Domain Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver Delivery Slice 1 of the approved Org Orchestration PRD as one pure, no-I/O `note-org` crate that parses and safely edits the supported Org subset, preserves opaque source bytes, validates workspace workflow policy, rejects invalid dependency graphs, and calculates deterministic readiness.

**Architecture:** `note-org` is a deep domain module with no sibling `note-*` dependencies. It stores the original Org source unchanged, projects only the explicitly supported semantic subset, and represents edits as isolated source replacements; policy, dependency, and readiness decisions are pure functions over projected values and caller-supplied facts. Storage, migrations, pipelines, leases, MCP, CLI, and frontend work remain in later delivery slices.

**Tech Stack:** Rust 2021, `chrono` 0.4.45, `serde` 1.0.228, `thiserror` 2, `uuid` 1.23.4, standard-library collections, Cargo integration tests.

---

## Execution Prerequisites

The source design, approved PRD, and this plan are currently documentation
changes. Preserve them before creating an implementation worktree:

```sh
git add \
  docs/org-module-design.md \
  docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md \
  docs/superpowers/plans/2026-07-30-org-domain-foundation.md
git diff --cached --check
git commit -m "docs(org): define orchestration system"
```

Use the `using-git-worktrees` skill for execution. Create the branch and
worktree under the project-local `.trees` directory:

```sh
git worktree add \
  .trees/codex/org-domain-foundation \
  -b codex/org-domain-foundation
cd .trees/codex/org-domain-foundation
```

Establish a clean, scoped baseline before implementation:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
git status --short --branch
```

Expected: formatting and workspace checks pass; the worktree is clean. If an
existing out-of-scope check fails, record it and stop rather than repairing it
inside this slice.

## Scope Boundary

This plan implements only PRD Delivery Slice 1:

- loss-preserving Org source parsing;
- stable work-item identity and semantic projection;
- minimal semantic source edits;
- workspace workflow policy;
- dependency validation; and
- readiness calculation from caller-supplied facts.

Do not add storage traits, SQL schemas, migrations, pipeline context, clocks,
leases, events, queues, MCP tools, offline commands, REST routes, or frontend
pages. Later slices consume the pure public interfaces established here.

## File Map

### Workspace registration

- Modify `Cargo.toml`
  - Add `crates/note-org` to workspace members.
- Update `Cargo.lock`
  - Record the new local crate and its already-used registry dependencies.

### Pure Org crate

- Create `crates/note-org/Cargo.toml`
  - Declare the pure crate and pinned dependencies already used by the repo.
- Create `crates/note-org/src/lib.rs`
  - Export the public domain interface.
- Create `crates/note-org/src/error.rs`
  - Define stable parse, edit, policy, dependency, and readiness errors.
- Create `crates/note-org/src/types.rs`
  - Define typed IDs, work-item metadata, timestamps, links, spans, and edits.
- Create `crates/note-org/src/source.rs`
  - Own raw source, the supported parser, semantic projection, and isolated
    source replacement.
- Create `crates/note-org/src/policy.rs`
  - Define and validate workspace workflow policy and transitions.
- Create `crates/note-org/src/dependency.rs`
  - Build and validate a same-workspace finish-to-start dependency graph.
- Create `crates/note-org/src/readiness.rs`
  - Calculate ready, recovery-candidate, or blocked results from explicit
    runtime facts.

### External behavior tests

- Create `crates/note-org/tests/support/mod.rs`
  - Shared IDs, policy, and fixture helpers.
- Create `crates/note-org/tests/fixtures/emacs-rich.org`
  - Emacs-style semantic and opaque constructs with LF endings.
- Create `crates/note-org/tests/fixtures/opaque-constructs.org`
  - Source/example/export blocks, drawers, Unicode, links, and heading-looking
    text that must remain opaque.
- Create `crates/note-org/tests/document_roundtrip_test.rs`
  - Public parser, projection, identity, and byte-preservation behavior.
- Create `crates/note-org/tests/semantic_edits_test.rs`
  - Complete resulting-source assertions for supported edits.
- Create `crates/note-org/tests/policy_test.rs`
  - Default-policy and transition-validation behavior.
- Create `crates/note-org/tests/dependencies_test.rs`
  - Missing targets, self-edges, duplicates, cycles, and satisfied dependencies.
- Create `crates/note-org/tests/readiness_test.rs`
  - Deterministic blocker and recovery-candidate behavior.

### Current architecture documentation

- Modify `README.md`
  - Add `note-org` to the crate architecture only after the crate passes its
    scoped completion gates.

## Public Interface Contract

The tasks below must converge on this public shape. Keep source spans and parser
nodes private; callers operate on stable projected values:

```rust
pub fn parse_document(
    source: impl Into<String>,
    options: &ParseOptions,
) -> Result<OrgDocument, OrgError>;

impl OrgDocument {
    pub fn source(&self) -> &str;
    pub fn items(&self) -> &[WorkItem];
    pub fn item(&self, id: WorkItemId) -> Option<&WorkItem>;
    pub fn apply(&self, edit: SemanticEdit) -> Result<EditedDocument, OrgError>;
}

pub fn move_item(
    source: &OrgDocument,
    target: &OrgDocument,
    item_id: WorkItemId,
    target_parent: Option<WorkItemId>,
) -> Result<MovedDocuments, OrgError>;

pub fn reparent_item(
    document: &OrgDocument,
    item_id: WorkItemId,
    target_parent: Option<WorkItemId>,
) -> Result<EditedDocument, OrgError>;

pub fn validate_policy(policy: &WorkspacePolicy) -> Result<(), PolicyError>;
pub fn validate_item(
    policy: &WorkspacePolicy,
    item: &WorkItem,
) -> Result<(), PolicyError>;
pub fn validate_transition(
    policy: &WorkspacePolicy,
    input: &TransitionInput,
) -> Result<(), TransitionError>;

pub fn validate_dependencies(
    items: &[WorkItem],
    successful_states: &BTreeSet<String>,
) -> Result<DependencyGraph, DependencyError>;

pub fn evaluate_readiness(
    item: &WorkItem,
    policy: &WorkspacePolicy,
    context: &ReadinessContext,
) -> Readiness;
```

Creation receives a caller-supplied UUID. Random ID generation belongs to a
later effectful pipeline, not this crate.

### Task 1: Scaffold the Pure Crate and Domain Vocabulary

**Files:**

- Modify: `Cargo.toml`
- Create: `crates/note-org/Cargo.toml`
- Create: `crates/note-org/src/lib.rs`
- Create: `crates/note-org/src/error.rs`
- Create: `crates/note-org/src/types.rs`
- Create: `crates/note-org/tests/document_roundtrip_test.rs`

- [ ] **Step 1: Write the failing typed-ID test**

Create `crates/note-org/tests/document_roundtrip_test.rs`:

```rust
use note_org::{WorkItemId, WorkItemType};
use std::str::FromStr;

#[test]
fn stable_work_item_ids_and_types_parse_from_canonical_values() {
    let id = WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap();

    assert_eq!(id.to_string(), "11111111-1111-4111-8111-111111111111");
    assert_eq!(WorkItemType::from_str("task").unwrap(), WorkItemType::Task);
    assert!(WorkItemType::from_str("unknown").is_err());
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```sh
cargo test -p note-org --test document_roundtrip_test \
  stable_work_item_ids_and_types_parse_from_canonical_values
```

Expected: Cargo fails because `note-org` is not a workspace member.

- [ ] **Step 3: Register the crate and declare only pure dependencies**

Add `"crates/note-org"` to the root workspace member list.

Create `crates/note-org/Cargo.toml`:

```toml
[package]
name = "note-org"
version = "0.1.0"
edition.workspace = true

[dependencies]
chrono = { version = "0.4.45", default-features = false, features = ["serde", "std"] }
serde = { version = "1.0.228", features = ["derive"] }
thiserror = "2"
uuid = { version = "1.23.4", features = ["serde"] }
```

Do not add `note-core`, storage, Tokio, parser frameworks, filesystem helpers,
or random UUID generation.

- [ ] **Step 4: Define the stable public vocabulary**

Create `crates/note-org/src/error.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OrgError {
    #[error("line {line}: {message}")]
    Parse { line: usize, message: String },
    #[error("work item {0} was not found")]
    ItemNotFound(crate::WorkItemId),
    #[error("duplicate work item id {0}")]
    DuplicateId(crate::WorkItemId),
    #[error("semantic edit cannot safely isolate work item {0}")]
    UnsafeEdit(crate::WorkItemId),
    #[error(transparent)]
    Policy(#[from] crate::PolicyError),
    #[error(transparent)]
    Dependency(#[from] crate::DependencyError),
}
```

Create the initial `crates/note-org/src/types.rs`:

```rust
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt, str::FromStr};
use uuid::Uuid;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
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
    SetState { item_id: WorkItemId, state: String },
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
```

Create `policy.rs` with the stable policy errors that later validation returns:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("{role} state {state} is not configured")]
    MissingRoleState { role: String, state: String },
    #[error("transition endpoint {state} is not configured")]
    TransitionStateMissing { state: String },
    #[error("{role} state {state} must be executable")]
    RecoveryStateNotExecutable { role: String, state: String },
    #[error("successful state {state} must be terminal")]
    SuccessfulStateNotTerminal { state: String },
    #[error("concurrency limit must be greater than zero")]
    ZeroConcurrency,
    #[error("lease duration must be greater than zero")]
    ZeroLeaseDuration,
    #[error("required tag {tag} is not allowed for {item_type:?}")]
    RequiredTagNotAllowed {
        item_type: crate::WorkItemType,
        tag: String,
    },
    #[error("work item type {0:?} is not allowed")]
    TypeNotAllowed(crate::WorkItemType),
    #[error("tag {tag} is not allowed for {item_type:?}")]
    TagNotAllowed {
        item_type: crate::WorkItemType,
        tag: String,
    },
    #[error("required tag {tag} is missing for {item_type:?}")]
    RequiredTagMissing {
        item_type: crate::WorkItemType,
        tag: String,
    },
}
```

Create `dependency.rs` with the stable graph errors:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DependencyError {
    #[error("work item {item_id} depends on missing item {dependency_id}")]
    MissingTarget {
        item_id: crate::WorkItemId,
        dependency_id: crate::WorkItemId,
    },
    #[error("work item {item_id} depends on itself")]
    SelfDependency { item_id: crate::WorkItemId },
    #[error("dependency cycle: {path:?}")]
    Cycle { path: Vec<crate::WorkItemId> },
}
```

Create `crates/note-org/src/lib.rs`:

```rust
pub mod dependency;
pub use dependency::*;
pub mod error;
pub use error::*;
pub mod policy;
pub use policy::*;
pub mod readiness;
pub use readiness::*;
pub mod source;
pub use source::*;
pub mod types;
pub use types::*;
```

Create compiling `readiness.rs` and `source.rs` module files. They export no
public values until their first red/green task adds tested behavior.

- [ ] **Step 5: Run the focused test and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test document_roundtrip_test \
  stable_work_item_ids_and_types_parse_from_canonical_values
cargo tree -p note-org --depth 1
```

Expected: the test passes. The dependency tree contains `chrono`, `serde`,
`thiserror`, and `uuid`, and no sibling `note-*` crate.

- [ ] **Step 6: Commit the green scaffold**

```sh
git add Cargo.toml Cargo.lock crates/note-org
git commit -m "feat(org): scaffold pure domain crate"
```

### Task 2: Build the Loss-Preserving Org Source Model

**Files:**

- Create: `crates/note-org/tests/support/mod.rs`
- Create: `crates/note-org/tests/fixtures/emacs-rich.org`
- Create: `crates/note-org/tests/fixtures/opaque-constructs.org`
- Modify: `crates/note-org/tests/document_roundtrip_test.rs`
- Modify: `crates/note-org/src/source.rs`
- Modify: `crates/note-org/src/types.rs`

- [ ] **Step 1: Add realistic fixtures**

Create `crates/note-org/tests/fixtures/emacs-rich.org` exactly as:

```org
#+TITLE: Agent Note Org fixture
#+TODO: BACKLOG READY RUNNING BLOCKED REVIEW | DONE FAILED CANCELLED
#+PROPERTY: preserved-preamble keep-me

# This comment and spacing must remain unchanged.
* READY [#A] Ship Org foundation :backend:urgent:
SCHEDULED: <2026-08-01 Sat 09:30>
DEADLINE: <2026-08-03 Mon>
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: issue
:ASSIGNEE: agent-alpha
:DEPENDS_ON: 22222222-2222-4222-8222-222222222222 33333333-3333-4333-8333-333333333333
:REQUIRES_REVIEW: true
:CUSTOM_FIELD: Keep this opaque value
:END:
Body with *markup* and
[[agent-note:design:aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa][Design note]].

:LOGBOOK:
CLOCK: [2026-07-30 Wed 08:00]--[2026-07-30 Wed 08:30]
:END:

#+BEGIN_SRC org
* DONE This is source text, not a projected item
:PROPERTIES:
:ID: 99999999-9999-4999-8999-999999999999
:END:
#+END_SRC

** DONE Parser spike :research:
:PROPERTIES:
:ID: 22222222-2222-4222-8222-222222222222
:AGENT_NOTE_TYPE: task
:END:

** DONE Policy spike
:PROPERTIES:
:ID: 33333333-3333-4333-8333-333333333333
:AGENT_NOTE_TYPE: task
:REQUIRES_REVIEW: false
:END:
```

Create `opaque-constructs.org` exactly as:

```org
#+TITLE: Opaque constructs
# preserve this comment

* READY Real item
:PROPERTIES:
:ID: 11111111-1111-4111-8111-111111111111
:AGENT_NOTE_TYPE: task
:END:
Unicode body: 编排 remains unchanged.

:CUSTOM_DRAWER:
* DONE Drawer text is not a heading
:END:

#+BEGIN_SRC org
* DONE Source text is not a heading
#+END_SRC

#+BEGIN_EXAMPLE
* DONE Example text is not a heading
#+END_EXAMPLE

#+BEGIN_EXPORT html
* DONE Export text is not a heading
#+END_EXPORT
```

Create `tests/support/mod.rs` so later integration tests do not invent
incompatible fixtures:

```rust
use note_org::{WorkItem, WorkItemId, WorkItemType};
use std::{collections::BTreeSet, str::FromStr};

pub fn id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

pub fn item<const N: usize>(
    item_id: &str,
    state: &str,
    dependencies: [&str; N],
) -> WorkItem {
    WorkItem {
        id: id(item_id),
        item_type: WorkItemType::Task,
        parent_id: None,
        level: 1,
        title: format!("Item {item_id}"),
        state: Some(state.to_string()),
        priority: None,
        tags: BTreeSet::new(),
        scheduled: None,
        deadline: None,
        assignee: None,
        depends_on: dependencies.into_iter().map(id).collect(),
        requires_review: false,
        note_links: vec![],
    }
}

pub fn ready_item() -> WorkItem {
    item("11111111-1111-4111-8111-111111111111", "READY", [])
}
```

- [ ] **Step 2: Write failing raw-preservation and block tests**

Extend `document_roundtrip_test.rs`:

```rust
mod support;

use note_org::{parse_document, ParseOptions};

fn options() -> ParseOptions {
    ParseOptions::new([
        "BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED",
        "CANCELLED",
    ])
}

#[test]
fn emacs_document_roundtrips_byte_for_byte() {
    let source = include_str!("fixtures/emacs-rich.org");
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
}

#[test]
fn heading_looking_lines_inside_blocks_remain_opaque() {
    let source = include_str!("fixtures/opaque-constructs.org");
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source(), source);
    assert_eq!(document.items().len(), 1);
}

#[test]
fn crlf_source_roundtrips_without_normalization() {
    let source = "* READY CRLF\r\n:PROPERTIES:\r\n:ID: 11111111-1111-4111-8111-111111111111\r\n:AGENT_NOTE_TYPE: task\r\n:END:\r\n";
    let document = parse_document(source, &options()).unwrap();

    assert_eq!(document.source().as_bytes(), source.as_bytes());
}
```

- [ ] **Step 3: Run the fixture tests and verify RED**

Run:

```sh
cargo test -p note-org --test document_roundtrip_test \
  emacs_document_roundtrips_byte_for_byte
```

Expected: compilation fails because `parse_document` and `OrgDocument` do not
exist.

- [ ] **Step 4: Implement span-based source scanning**

Add private byte spans to `types.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedItem {
    pub item: WorkItem,
    pub heading: Span,
    pub subtree: Span,
    pub state: Option<Span>,
    pub properties: std::collections::BTreeMap<String, Span>,
    pub planning: std::collections::BTreeMap<String, Span>,
    pub tags: Option<Span>,
}
```

Implement `source.rs` around these rules:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgDocument {
    source: String,
    items: Vec<WorkItem>,
    index: std::collections::BTreeMap<WorkItemId, IndexedItem>,
    options: ParseOptions,
}

impl OrgDocument {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn items(&self) -> &[WorkItem] {
        &self.items
    }

    pub fn item(&self, id: WorkItemId) -> Option<&WorkItem> {
        self.index.get(&id).map(|entry| &entry.item)
    }
}

pub fn parse_document(
    source: impl Into<String>,
    options: &ParseOptions,
) -> Result<OrgDocument, OrgError> {
    let source = source.into();
    let scanned = scan_headings(&source)?;
    project_document(source, scanned, options.clone())
}
```

`scan_headings` must:

1. iterate with `split_inclusive('\n')` and retain absolute byte offsets;
2. track `#+BEGIN_SRC`, `#+BEGIN_EXAMPLE`, and `#+BEGIN_EXPORT` until the
   matching `#+END_*`, case-insensitively;
3. treat non-`PROPERTIES` drawers from `:<NAME>:` through `:END:` as opaque and
   ignore heading-looking lines inside them;
4. recognize a heading only when the line starts with one or more `*` followed
   by one ASCII space and is outside a block or opaque drawer;
5. retain the complete heading-line and subtree byte spans;
6. never normalize line endings or reconstruct raw source; and
7. report 1-based source lines in `OrgError::Parse`.

`project_document` treats `:AGENT_NOTE_TYPE:` as the orchestration marker. A
marked heading must also have one valid `:ID:`; missing, malformed, or duplicate
IDs are errors. A heading with an ordinary Org `:ID:` but no
`:AGENT_NOTE_TYPE:` remains non-orchestrated. Compute parent IDs using the
nearest lower projected heading level. Non-orchestrated headings remain in
source but are absent from `items`.

Index known property spans as value spans, planning spans as complete line
spans, and state/tag spans as token spans within the heading line. Mark a known
property unsafe to edit when the drawer contains duplicate base keys or a
`KEY+` accumulation form for that property.

Add focused private unit tests beside `scan_headings` for LF, CRLF, nested
blocks, and final lines without a newline. These tests may inspect spans;
integration tests must continue asserting public source and projections only.

- [ ] **Step 5: Run the parser tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test document_roundtrip_test
cargo test -p note-org source::tests
```

Expected: raw LF and CRLF bytes are unchanged, block contents remain opaque,
and only orchestrated headings are projected.

- [ ] **Step 6: Commit the loss-preserving source model**

```sh
git add crates/note-org
git commit -m "feat(org): parse loss-preserving Org source"
```

### Task 3: Project Stable Work-Item Metadata

**Files:**

- Modify: `crates/note-org/src/source.rs`
- Modify: `crates/note-org/src/types.rs`
- Modify: `crates/note-org/tests/document_roundtrip_test.rs`

- [ ] **Step 1: Add failing rich-projection tests**

Add:

```rust
use note_org::{NoteLink, WorkItemId, WorkItemType};
use std::str::FromStr;

#[test]
fn rich_heading_projects_supported_semantics() {
    let document =
        parse_document(include_str!("fixtures/emacs-rich.org"), &options()).unwrap();
    let item = document
        .item(WorkItemId::from_str("11111111-1111-4111-8111-111111111111").unwrap())
        .unwrap();

    assert_eq!(item.item_type, WorkItemType::Issue);
    assert_eq!(item.state.as_deref(), Some("READY"));
    assert_eq!(item.priority, Some('A'));
    assert_eq!(
        item.tags.iter().map(String::as_str).collect::<Vec<_>>(),
        vec!["backend", "urgent"]
    );
    assert_eq!(item.assignee.as_deref(), Some("agent-alpha"));
    assert!(item.requires_review);
    assert_eq!(item.scheduled.as_ref().unwrap().raw, "<2026-08-01 Sat 09:30>");
    assert_eq!(item.deadline.as_ref().unwrap().raw, "<2026-08-03 Mon>");
    assert_eq!(
        item.note_links,
        vec![NoteLink {
            purpose: "design".to_string(),
            note_id: uuid::Uuid::parse_str(
                "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
            )
            .unwrap(),
            description: "Design note".to_string(),
        }]
    );
}

#[test]
fn identity_survives_title_and_hierarchy_changes() {
    let before = parse_document(
        "* READY Old title\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
        &options(),
    )
    .unwrap();
    let after = parse_document(
        "* Project\n** READY New title\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:END:\n",
        &options(),
    )
    .unwrap();

    assert_eq!(before.items()[0].id, after.items()[0].id);
}
```

Add inline invalid-source tests for malformed UUIDs, duplicate IDs, unknown
work-item types, invalid `:REQUIRES_REVIEW:`, invalid dependency UUIDs, and
unsupported repeaters or time ranges in `SCHEDULED`/`DEADLINE`.

- [ ] **Step 2: Run the rich projection test and verify RED**

Run:

```sh
cargo test -p note-org --test document_roundtrip_test \
  rich_heading_projects_supported_semantics
```

Expected: at least one metadata assertion fails because only ID/type/hierarchy
are projected.

- [ ] **Step 3: Implement the supported semantic subset**

Extend heading projection with these exact rules:

- the first heading token is a state only when it exists in
  `ParseOptions.states`;
- `[#[A-Z]]` immediately after state is the optional priority;
- only tags on the current heading suffix are semantic; no tag inheritance;
- `SCHEDULED:` and `DEADLINE:` apply only when directly under the heading;
- supported planning timestamps are date-only and local date-time forms;
- repeaters and time ranges remain raw source but make that orchestrated item
  fail semantic projection as unsupported;
- `:DEPENDS_ON:` is a whitespace-separated set of UUIDs and rejects duplicates
  and self-reference;
- `:REQUIRES_REVIEW:` accepts only `true` or `false`;
- unknown property keys remain opaque;
- an Agent Note link has the exact target form
  `agent-note:<purpose>:<note-uuid>`; malformed targets remain ordinary opaque
  links rather than partially projected links.

Use this parser for date-only and date-time planning values:

```rust
fn parse_planning_timestamp(raw: &str) -> Result<OrgTimestamp, String> {
    let inner = raw
        .strip_prefix('<')
        .and_then(|value| value.strip_suffix('>'))
        .ok_or_else(|| "planning timestamp must be active Org syntax".to_string())?;
    if inner.contains('+') || inner.contains("++") || inner.contains(".+")
        || inner.matches('-').count() > 2
    {
        return Err("repeaters and time ranges are unsupported".to_string());
    }
    let parts = inner.split_whitespace().collect::<Vec<_>>();
    let value = match parts.as_slice() {
        [date, _weekday] => format!("{date} 00:00"),
        [date, _weekday, time] => format!("{date} {time}"),
        _ => return Err("unsupported planning timestamp".to_string()),
    };
    let local = chrono::NaiveDateTime::parse_from_str(&value, "%Y-%m-%d %H:%M")
        .map_err(|_| "invalid planning timestamp".to_string())?;
    Ok(OrgTimestamp {
        raw: raw.to_string(),
        local,
    })
}
```

Do not convert timezone-less Org timestamps to UTC in this crate. Later
pipelines resolve workspace timezone and pass UTC facts into readiness.

- [ ] **Step 4: Run projection tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test document_roundtrip_test
```

Expected: the rich fixture projects identity, hierarchy, state, priority, tags,
planning, properties, dependencies, and note links while raw source remains
byte-identical.

- [ ] **Step 5: Commit stable metadata projection**

```sh
git add crates/note-org
git commit -m "feat(org): project stable work item metadata"
```

### Task 4: Apply Minimal Semantic Source Edits

**Files:**

- Create: `crates/note-org/tests/semantic_edits_test.rs`
- Modify: `crates/note-org/src/source.rs`
- Modify: `crates/note-org/src/types.rs`

- [ ] **Step 1: Write failing complete-source edit tests**

Create `semantic_edits_test.rs` with one test per edit. Start with:

```rust
mod support;

use note_org::{
    move_item, parse_document, reparent_item, NewWorkItem, ParseOptions, SemanticEdit,
    WorkItemId, WorkItemType,
};
use std::{collections::BTreeSet, str::FromStr};

fn id(value: &str) -> WorkItemId {
    WorkItemId::from_str(value).unwrap()
}

fn options() -> ParseOptions {
    ParseOptions::new(["BACKLOG", "READY", "RUNNING", "DONE"])
}

#[test]
fn transition_changes_only_the_todo_token() {
    let source = "* READY Keep title :tag:\n:PROPERTIES:\n:ID: 11111111-1111-4111-8111-111111111111\n:AGENT_NOTE_TYPE: task\n:CUSTOM: keep\n:END:\nBody.\n";
    let document = parse_document(source, &options()).unwrap();

    let edited = document
        .apply(SemanticEdit::SetState {
            item_id: id("11111111-1111-4111-8111-111111111111"),
            state: "RUNNING".to_string(),
        })
        .unwrap();

    assert_eq!(
        edited.source,
        source.replacen("* READY ", "* RUNNING ", 1)
    );
}

#[test]
fn create_item_uses_the_caller_supplied_id() {
    let document = parse_document("", &options()).unwrap();
    let item_id = id("44444444-4444-4444-8444-444444444444");
    let edited = document
        .apply(SemanticEdit::AppendItem {
            parent_id: None,
            item: NewWorkItem {
                id: item_id,
                item_type: WorkItemType::Task,
                title: "Write parser".to_string(),
                state: "BACKLOG".to_string(),
                priority: None,
                tags: BTreeSet::new(),
            },
        })
        .unwrap();

    assert_eq!(
        edited.source,
        "* BACKLOG Write parser\n:PROPERTIES:\n:ID: 44444444-4444-4444-8444-444444444444\n:AGENT_NOTE_TYPE: task\n:END:\n"
    );
}
```

Add tests asserting complete source for assignment, dependency, schedule, and
tag edits. Add an unsafe-edit test where the property drawer contains both
`:ASSIGNEE:` and `:ASSIGNEE+:`; parsing and raw round-trip succeed, but changing
the assignee returns `OrgError::UnsafeEdit`.

Add a two-document move test that proves:

- the full subtree leaves the source;
- it becomes the last child of the target parent;
- heading levels change by one consistent delta;
- IDs and bodies are unchanged; and
- unrelated bytes in both documents are identical.

Add a same-document `reparent_item` test proving the moved subtree becomes the
last child of the target parent without changing its IDs or body.

- [ ] **Step 2: Run the semantic edit test and verify RED**

Run:

```sh
cargo test -p note-org --test semantic_edits_test \
  transition_changes_only_the_todo_token
```

Expected: compilation fails because `OrgDocument::apply`,
`EditedDocument`, and `move_item` do not exist.

- [ ] **Step 3: Implement isolated replacements**

Add:

```rust
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
```

Use one replacement primitive:

```rust
fn replace_span(source: &str, span: Span, replacement: &str) -> String {
    let mut result =
        String::with_capacity(source.len() - (span.end - span.start) + replacement.len());
    result.push_str(&source[..span.start]);
    result.push_str(replacement);
    result.push_str(&source[span.end..]);
    result
}
```

Implement edits as follows:

- `SetState` replaces only the indexed state token. Inserting a missing state
  occurs immediately after heading stars and one space.
- `SetProperty` replaces only the known property's value. If absent, insert it
  before `:END:` in a valid property drawer; if no drawer exists, insert a new
  drawer immediately after the heading and planning lines. Removing the final
  known property must not delete unknown properties.
- `SetScheduled` replaces the complete indexed `SCHEDULED:` line, inserts it
  directly below the heading when absent, and removes only that line when
  `None`.
- `SetTags` replaces only the heading tag suffix and renders tags in sorted
  order.
- `AppendItem` validates the caller-supplied ID is unused, renders the canonical
  heading and property drawer, and inserts as the final child or final
  top-level item.
- `move_item` cuts the indexed subtree, adjusts only leading heading stars
  inside that subtree, and inserts it as the target parent's final child or at
  target EOF.
- `reparent_item` performs the same cut, level adjustment, and insertion inside
  one document while rejecting a target parent inside the moved subtree.

Every edit must validate the requested state/property/timestamp first, reject
malformed indexed spans, return a new source string, and leave the original
`OrgDocument` unchanged. Reparse the result in tests to prove the source still
projects.

- [ ] **Step 4: Run semantic edit tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test semantic_edits_test
cargo test -p note-org --test document_roundtrip_test
```

Expected: all complete-source assertions pass and parser behavior remains
green.

- [ ] **Step 5: Commit minimal source editing**

```sh
git add crates/note-org
git commit -m "feat(org): preserve source during semantic edits"
```

### Task 5: Model and Validate Workspace Workflow Policy

**Files:**

- Create: `crates/note-org/tests/policy_test.rs`
- Modify: `crates/note-org/src/policy.rs`
- Modify: `crates/note-org/src/types.rs`

- [ ] **Step 1: Write failing default-policy and transition tests**

Create `policy_test.rs`:

```rust
use note_org::{
    validate_policy, validate_transition, TransitionInput, WorkspacePolicy, WorkItemType,
};

#[test]
fn default_engineering_policy_has_prd_states_and_roles() {
    let policy = WorkspacePolicy::engineering_default();

    validate_policy(&policy).unwrap();
    assert_eq!(policy.initial_state, "BACKLOG");
    assert_eq!(policy.running_state, "RUNNING");
    assert_eq!(policy.review_state, "REVIEW");
    assert_eq!(policy.release_state, "READY");
    assert_eq!(policy.review_rejection_state, "READY");
    assert_eq!(policy.lease_expiry_recovery_state, "READY");
    assert!(policy.successful_terminal_states.contains("DONE"));
    assert_eq!(policy.lease_duration_secs, 900);
    assert_eq!(policy.max_attempts(), 3);
    assert_eq!(policy.concurrency_limit, 4);
}

#[test]
fn required_review_blocks_direct_completion() {
    let policy = WorkspacePolicy::engineering_default();

    let error = validate_transition(
        &policy,
        &TransitionInput {
            item_type: WorkItemType::Task,
            from: "RUNNING".to_string(),
            to: "DONE".to_string(),
            item_requires_review: true,
            review_approved: false,
            dependencies_satisfied: true,
        },
    )
    .unwrap_err();

    assert_eq!(error.to_string(), "review is required before DONE");
}
```

Add table-driven cases for undefined role states, transition endpoints outside
the state set, recovery states that are not executable, work-item types absent
from `allowed_types`, required tags, and retry accounting (`retry_limit = N`
permits attempts `1..=N+1`).

- [ ] **Step 2: Run the policy test and verify RED**

Run:

```sh
cargo test -p note-org --test policy_test
```

Expected: compilation fails because the policy and transition types do not
exist.

- [ ] **Step 3: Implement the policy model and default**

Define:

```rust
use crate::{ParseOptions, WorkItemType};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TagRule {
    pub allowed: BTreeSet<String>,
    pub required: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPolicy {
    Open,
    AssignmentRestricted,
    ExplicitlyDispatched,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorkspacePolicy {
    pub allowed_types: BTreeSet<WorkItemType>,
    pub states: BTreeSet<String>,
    pub transitions: BTreeSet<(String, String)>,
    pub initial_state: String,
    pub running_state: String,
    pub executable_states: BTreeSet<String>,
    pub review_state: String,
    pub failed_state: String,
    pub cancelled_state: String,
    pub successful_terminal_states: BTreeSet<String>,
    pub terminal_states: BTreeSet<String>,
    pub release_state: String,
    pub review_rejection_state: String,
    pub lease_expiry_recovery_state: String,
    pub review_required_types: BTreeSet<WorkItemType>,
    pub claim_policy: ClaimPolicy,
    pub lease_duration_secs: u64,
    pub retry_limit: u32,
    pub concurrency_limit: usize,
    pub tag_rules: BTreeMap<WorkItemType, TagRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionInput {
    pub item_type: WorkItemType,
    pub from: String,
    pub to: String,
    pub item_requires_review: bool,
    pub review_approved: bool,
    pub dependencies_satisfied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransitionError {
    #[error("work item type {0:?} is not allowed")]
    TypeNotAllowed(WorkItemType),
    #[error("transition {from} -> {to} is not allowed")]
    NotAllowed { from: String, to: String },
    #[error("dependencies are incomplete")]
    DependenciesIncomplete,
    #[error("review is required before {0}")]
    ReviewRequired(String),
}
```

`engineering_default` must encode exactly:

```text
BACKLOG -> READY, CANCELLED
READY -> RUNNING, CANCELLED
RUNNING -> BLOCKED, REVIEW, DONE, FAILED, CANCELLED
BLOCKED -> READY, CANCELLED
REVIEW -> DONE, READY, CANCELLED
FAILED -> READY, CANCELLED
```

Its non-state defaults are:

```rust
allowed_types: WorkItemType::ALL.into_iter().collect(),
states: [
    "BACKLOG", "READY", "RUNNING", "BLOCKED", "REVIEW", "DONE", "FAILED",
    "CANCELLED",
]
.into_iter()
.map(str::to_string)
.collect(),
executable_states: BTreeSet::from(["READY".to_string()]),
successful_terminal_states: BTreeSet::from(["DONE".to_string()]),
terminal_states: BTreeSet::from([
    "DONE".to_string(),
    "CANCELLED".to_string(),
]),
initial_state: "BACKLOG".to_string(),
running_state: "RUNNING".to_string(),
review_state: "REVIEW".to_string(),
failed_state: "FAILED".to_string(),
cancelled_state: "CANCELLED".to_string(),
release_state: "READY".to_string(),
review_rejection_state: "READY".to_string(),
lease_expiry_recovery_state: "READY".to_string(),
claim_policy: ClaimPolicy::AssignmentRestricted,
lease_duration_secs: 900,
retry_limit: 2,
concurrency_limit: 4,
review_required_types: BTreeSet::new(),
tag_rules: BTreeMap::new(),
```

`validate_policy` must prove every role, configured state set, and transition
endpoint belongs to `states`, every recovery/release state is executable,
successful states are terminal, `concurrency_limit > 0`,
`lease_duration_secs > 0`, and required tags are allowed when an allowed set is
non-empty.

`validate_item` must reject item types outside `allowed_types`, tags outside a
non-empty allowed set, and missing required tags. Add one public-behavior test
for each error variant.

Expose retry accounting without duplicating it in callers:

```rust
impl WorkspacePolicy {
    pub fn max_attempts(&self) -> u32 {
        self.retry_limit.saturating_add(1)
    }

    pub fn parse_options(&self) -> ParseOptions {
        ParseOptions::new(self.states.iter().cloned())
    }
}
```

`validate_transition` must check allowed type, configured edge, satisfied
dependencies before entering an executable or successful state, and review
requirements before successful completion. An item-level `true` strengthens
policy; item-level `false` cannot bypass a type-level requirement.

- [ ] **Step 4: Run policy tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test policy_test
```

Expected: default and invalid-policy cases pass with stable typed errors.

- [ ] **Step 5: Commit workflow policy**

```sh
git add crates/note-org
git commit -m "feat(org): validate workspace workflows"
```

### Task 6: Validate Dependencies and Calculate Readiness

**Files:**

- Create: `crates/note-org/tests/dependencies_test.rs`
- Create: `crates/note-org/tests/readiness_test.rs`
- Modify: `crates/note-org/src/dependency.rs`
- Modify: `crates/note-org/src/readiness.rs`
- Modify: `crates/note-org/src/types.rs`

- [ ] **Step 1: Write failing dependency graph tests**

Create `dependencies_test.rs` using a helper that builds `WorkItem` values.
Cover valid edges, self-dependency, missing prerequisite, two-node cycle,
longer cycle, and configured successful states:

```rust
mod support;

use note_org::validate_dependencies;
use std::collections::BTreeSet;
use support::{id, item};

#[test]
fn only_configured_successful_terminal_states_satisfy_dependencies() {
    let done = item("22222222-2222-4222-8222-222222222222", "ARCHIVED", []);
    let dependent = item(
        "11111111-1111-4111-8111-111111111111",
        "READY",
        ["22222222-2222-4222-8222-222222222222"],
    );
    let successful = BTreeSet::from(["ARCHIVED".to_string()]);

    let graph = validate_dependencies(&[dependent, done], &successful).unwrap();

    assert!(graph.dependencies_satisfied(
        id("11111111-1111-4111-8111-111111111111")
    ));
}
```

Assert exact `DependencyError` variants for:

```rust
MissingTarget { item_id, dependency_id }
SelfDependency { item_id }
Cycle { path }
```

- [ ] **Step 2: Run dependency tests and verify RED**

Run:

```sh
cargo test -p note-org --test dependencies_test
```

Expected: compilation fails because `DependencyGraph` and
`validate_dependencies` do not exist.

- [ ] **Step 3: Implement deterministic graph validation**

Define:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyGraph {
    edges: BTreeMap<WorkItemId, BTreeSet<WorkItemId>>,
    successful: BTreeSet<WorkItemId>,
}

impl DependencyGraph {
    pub fn dependencies_satisfied(&self, item_id: WorkItemId) -> bool {
        self.edges
            .get(&item_id)
            .is_none_or(|dependencies| dependencies.is_subset(&self.successful))
    }
}
```

Build the item map first, reject self-edges and missing targets, then run a
three-color depth-first search in sorted ID order. When a gray node is reached,
return the deterministic cycle path beginning and ending with the smallest ID
in that cycle. Hierarchy must not create dependency edges.

- [ ] **Step 4: Write failing reason-bearing readiness tests**

Create `readiness_test.rs`:

```rust
mod support;

use note_org::{
    evaluate_readiness, Readiness, ReadinessBlocker, ReadinessContext, WorkspacePolicy,
};
use support::ready_item;

#[test]
fn reports_every_blocker_in_deterministic_order() {
    let mut item = ready_item();
    item.assignee = Some("agent-a".to_string());
    let policy = WorkspacePolicy::engineering_default();
    let context = ReadinessContext {
        now: 100,
        dependencies_satisfied: false,
        workspace_active: false,
        active_lease: true,
        capacity_available: false,
        actor_id: Some("agent-b".to_string()),
        scheduled_at: Some(101),
        lease_expired_running: false,
    };

    assert_eq!(
        evaluate_readiness(&item, &policy, &context),
        Readiness::Blocked(vec![
            ReadinessBlocker::DependenciesIncomplete,
            ReadinessBlocker::ScheduledForFuture,
            ReadinessBlocker::WorkspaceArchived,
            ReadinessBlocker::ActiveLease,
            ReadinessBlocker::ConcurrencyLimit,
            ReadinessBlocker::AssignedToOtherActor,
        ])
    );
}

#[test]
fn expired_running_item_is_a_recovery_candidate() {
    let mut item = ready_item();
    item.state = Some("RUNNING".to_string());
    let policy = WorkspacePolicy::engineering_default();
    let context = ReadinessContext {
        lease_expired_running: true,
        ..ReadinessContext::ready_at(100)
    };

    assert_eq!(
        evaluate_readiness(&item, &policy, &context),
        Readiness::RecoveryCandidate
    );
}
```

Also cover exact schedule boundary (`scheduled_at == now` is due), deadline
metadata not blocking readiness, configurable successful states, unassigned
items under open and assignment-restricted policies, explicit dispatch requiring
an assignee, and one test for each blocker.

- [ ] **Step 5: Run readiness tests and verify RED**

Run:

```sh
cargo test -p note-org --test readiness_test
```

Expected: compilation fails because readiness types do not exist.

- [ ] **Step 6: Implement pure readiness evaluation**

Define:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessContext {
    pub now: i64,
    pub dependencies_satisfied: bool,
    pub workspace_active: bool,
    pub active_lease: bool,
    pub capacity_available: bool,
    pub actor_id: Option<String>,
    pub scheduled_at: Option<i64>,
    pub lease_expired_running: bool,
}

impl ReadinessContext {
    pub fn ready_at(now: i64) -> Self {
        Self {
            now,
            dependencies_satisfied: true,
            workspace_active: true,
            active_lease: false,
            capacity_available: true,
            actor_id: None,
            scheduled_at: None,
            lease_expired_running: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReadinessBlocker {
    NonExecutableState,
    DependenciesIncomplete,
    ScheduledForFuture,
    WorkspaceArchived,
    ActiveLease,
    ConcurrencyLimit,
    AssignmentRequired,
    AssignedToOtherActor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    RecoveryCandidate,
    Blocked(Vec<ReadinessBlocker>),
}
```

`evaluate_readiness` must append blockers in enum order, never short-circuit,
and return:

- `RecoveryCandidate` only for the configured running state with
  `lease_expired_running = true` and no other blocker; this combination does
  not add `NonExecutableState` or `ActiveLease`;
- `Ready` only for an executable state with no blocker; or
- `Blocked` with every applicable reason.

Assignment blocking compares `item.assignee` with `context.actor_id`:

- `Open` ignores assignment for readiness;
- `AssignmentRestricted` allows unassigned work and otherwise requires the
  matching actor; and
- `ExplicitlyDispatched` requires an assignee and the matching actor.

Do not read the clock, storage, leases, or workspace state inside this crate.
Later pipelines construct `ReadinessContext`.

- [ ] **Step 7: Run dependency and readiness tests and verify GREEN**

Run:

```sh
cargo fmt --all
cargo test -p note-org --test dependencies_test
cargo test -p note-org --test readiness_test
cargo test -p note-org
```

Expected: graph validation, satisfied-dependency behavior, readiness blockers,
and recovery candidates all pass.

- [ ] **Step 8: Commit dependency and readiness logic**

```sh
git add crates/note-org
git commit -m "feat(org): calculate dependency readiness"
```

### Task 7: Document and Verify Delivery Slice 1

**Files:**

- Modify: `README.md`
- Verify: `Cargo.toml`
- Verify: `Cargo.lock`
- Verify: `crates/note-org/**`

- [ ] **Step 1: Update the current crate architecture**

Add `note-org` to the README crate overview and architecture diagram with this
boundary:

```text
note-org         pure Org source projection/editing, workflow policy,
                 dependency validation, and readiness (no I/O)
```

State that storage, pipelines, leases, events, MCP tools, and UI integration
are later Org delivery slices. Do not document unimplemented commands or APIs.

- [ ] **Step 2: Run the scoped completion gates**

Run:

```sh
cargo fmt --all -- --check
cargo check -p note-org --all-targets
cargo test -p note-org
cargo tree -p note-org --depth 1
cargo check --workspace --all-targets
git diff --check
git status --short --branch
```

Expected:

- all `note-org` tests pass;
- workspace checking passes;
- `cargo tree` shows no sibling `note-*` dependency;
- `git diff --check` is silent; and
- only Slice 1 files and README are modified.

Do not run frontend tests or `cargo test --workspace`; no frontend or existing
runtime behavior changes in this slice. If an out-of-scope workspace check
fails, report it and stop without changing unrelated files.

- [ ] **Step 3: Verify PRD Slice 1 coverage**

Map the finished behavior back to the approved PRD:

```text
Org domain foundation
  [x] pure parser and loss-preserving source model
  [x] stable IDs and semantic metadata
  [x] workspace policy and transition validation
  [x] dependency graph and readiness calculation
```

Confirm that no storage, pipeline, lease, MCP, CLI, REST, or frontend
implementation entered the diff.

- [ ] **Step 4: Commit documentation and completion gates**

```sh
git add README.md
git commit -m "docs(org): document domain foundation"
```

After the commit:

```sh
git status --short --branch
git log --oneline --decorate -8
```

Expected: clean worktree with six focused `feat(org)` commits, one
`docs(org)` README commit, and the earlier design/PRD/plan documentation
commit.

## Slice 1 Completion Boundary

Stop when every Task 7 gate passes. Do not proceed directly into canonical
persistence. Delivery Slice 2 requires a separate plan covering storage
contracts, embedded schema migration, PostgreSQL migration, document CAS,
projections, weak note links, event sequencing, and import/export transactions.
