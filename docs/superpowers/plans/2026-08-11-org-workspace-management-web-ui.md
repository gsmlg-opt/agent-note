# Org Workspace Management Web UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add complete create, edit, and archive workspace management to the Org Web UI while keeping Org documents and operational workflow actions read-only, then deploy and verify it on `ms04`.

**Architecture:** Add a pure typed workspace-management model and validator under the frontend Org module, then reuse one structured Yew form from dedicated create and settings pages. Extend the existing REST client only for the three approved workspace mutations, integrate archival into the workspace page, correct the PRD boundary, and verify the full lifecycle in a disposable browser fixture before publishing the full-stack image.

**Tech Stack:** Rust 2021, Yew 0.23, gloo-net, serde/serde_json, uuid, Yew Router, Trunk/Wasm, Axum REST backend, Docker Buildx, Podman/systemd on `ms04`.

---

## File Map

- Create `crates/note-frontend/src/org/workspace_management.rs`: typed policy, engineering defaults, form draft, validation, mutation bodies, and submission identity.
- Modify `crates/note-frontend/src/org/model.rs`: deserialize workspace policy into the typed model instead of opaque JSON.
- Modify `crates/note-frontend/src/org/api.rs`: POST/PATCH archive clients and reusable JSON response handling.
- Create `crates/note-frontend/src/components/org_workspace_form.rs`: shared structured create/edit form.
- Modify `crates/note-frontend/src/components/mod.rs`: export the shared form.
- Create `crates/note-frontend/src/pages/org_workspace_new.rs`: create lifecycle and navigation.
- Create `crates/note-frontend/src/pages/org_workspace_settings.rs`: load/edit/reload lifecycle and navigation.
- Modify `crates/note-frontend/src/pages/org_workspaces.rs`: Create workspace entry point and corrected copy.
- Modify `crates/note-frontend/src/pages/org_workspace.rs`: edit and archive actions with slug confirmation.
- Modify `crates/note-frontend/src/pages/mod.rs` and `crates/note-frontend/src/routes.rs`: register management pages and titles.
- Modify `crates/note-frontend/app.css`: responsive workspace-management form and destructive-action styling.
- Modify `crates/note-frontend/Cargo.toml`: UUID generation compatible with native tests and Wasm.
- Modify `docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md`, `docs/design.md`, and `README.md`: correct the browser write boundary.
- Create `scripts/verify-org-workspace-management-browser.sh`: disposable create/edit/archive browser acceptance.
- Modify `scripts/verify-org-console-browser.sh`: retain read-only enforcement for Org content and operational actions without rejecting approved workspace mutations.

### Task 1: Typed workspace policy, defaults, and validation

**Files:**
- Create: `crates/note-frontend/src/org/workspace_management.rs`
- Modify: `crates/note-frontend/src/org/mod.rs`
- Modify: `crates/note-frontend/src/org/model.rs`

- [ ] **Step 1: Write failing model/default tests**

Add tests that assert `WorkspacePolicy::engineering_default()` contains all nine work types, the eight documented states, the required transition matrix, assignment-restricted claims, a 900-second lease, retry limit 2, concurrency limit 4, and no tag rules. Add a deserialize/serialize round-trip test using a non-default policy and tag rules.

```rust
#[test]
fn engineering_default_is_a_complete_valid_policy() {
    let policy = WorkspacePolicy::engineering_default();
    assert_eq!(policy.allowed_types.len(), 9);
    assert_eq!(policy.initial_state, "BACKLOG");
    assert!(policy.transitions.contains(&("READY".into(), "RUNNING".into())));
    assert_eq!(policy.claim_policy, ClaimPolicy::AssignmentRestricted);
    assert_eq!(policy.lease_duration_secs, 900);
    assert!(WorkspaceDraft::new().validate().is_ok());
}
```

- [ ] **Step 2: Run the focused test and verify RED**

Run: `cd crates/note-frontend && cargo test workspace_management::tests::engineering_default_is_a_complete_valid_policy`

Expected: compilation fails because `workspace_management` and its types do not exist.

- [ ] **Step 3: Implement typed models and defaults**

Define `ClaimPolicy`, `TagRule`, `WorkspacePolicy`, `WorkspaceDraft`, and `WorkspaceValidationError`. Use the REST wire names with `serde(rename_all = "snake_case")`. Keep set-like form values in deterministic `Vec<String>` form and tag rules in `BTreeMap<String, TagRule>`.

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimPolicy {
    Open,
    AssignmentRestricted,
    ExplicitlyDispatched,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePolicy {
    pub allow_cross_workspace_agenda: bool,
    pub allowed_types: Vec<String>,
    pub states: Vec<String>,
    pub transitions: Vec<(String, String)>,
    pub initial_state: String,
    pub running_state: String,
    pub executable_states: Vec<String>,
    pub review_state: String,
    pub failed_state: String,
    pub cancelled_state: String,
    pub successful_terminal_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub release_state: String,
    pub review_rejection_state: String,
    pub lease_expiry_recovery_state: String,
    pub review_required_types: Vec<String>,
    pub claim_policy: ClaimPolicy,
    pub lease_duration_secs: u64,
    pub retry_limit: u32,
    pub concurrency_limit: usize,
    pub tag_rules: BTreeMap<String, TagRule>,
}
```

Implement `WorkspaceDraft::new()`, `WorkspaceDraft::from_workspace`, and `validate`. Validation rejects blank identity fields, duplicate states/transitions, unknown transition endpoints or role states, zero lease/concurrency values, invalid retry parsing, and required tags absent from allowed tags.

- [ ] **Step 4: Type `Workspace.policy` and verify GREEN**

Change `Workspace.policy` from `serde_json::Value` to `WorkspacePolicy`, export the module from `org/mod.rs`, and run:

`cd crates/note-frontend && cargo test workspace_management`

Expected: all workspace-management model tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/note-frontend/src/org/model.rs crates/note-frontend/src/org/mod.rs crates/note-frontend/src/org/workspace_management.rs
git commit -m "feat(frontend): model Org workspace management"
```

### Task 2: Mutation request contracts and idempotent submission identity

**Files:**
- Modify: `crates/note-frontend/Cargo.toml`
- Modify: `crates/note-frontend/src/org/workspace_management.rs`
- Modify: `crates/note-frontend/src/org/api.rs`

- [ ] **Step 1: Write failing request-body tests**

Test exact create, update, and archive JSON. Assert schema version 1, actor `web-ui`, generated workspace/operation UUIDs, expected revision on update/archive, complete policy, and exact URLs.

```rust
#[test]
fn update_body_uses_web_identity_revision_and_complete_policy() {
    let submission = WorkspaceSubmission::for_update("op-id".into(), 7);
    let body = UpdateWorkspaceBody::from_draft(&WorkspaceDraft::new(), submission);
    assert_eq!(body.schema_version, 1);
    assert_eq!(body.actor_id, "web-ui");
    assert_eq!(body.operation_id, "op-id");
    assert_eq!(body.expected_revision, 7);
    assert_eq!(body.policy, WorkspacePolicy::engineering_default());
}
```

- [ ] **Step 2: Verify RED**

Run: `cd crates/note-frontend && cargo test org::api::tests::workspace_mutation_contracts`

Expected: failure because mutation bodies and client functions are absent.

- [ ] **Step 3: Implement mutation bodies and UUID generation**

Add `uuid = { version = "1", features = ["v4", "js"] }`. Implement serializable `CreateWorkspaceBody`, `UpdateWorkspaceBody`, `ArchiveWorkspaceBody`, `MutationResult`, and `WorkspaceSubmission`. A submission retains its operation ID across retry and creates a new ID only for a new submission.

- [ ] **Step 4: Implement REST mutation functions**

Add:

```rust
pub async fn create_workspace(body: &CreateWorkspaceBody) -> Result<MutationResult, OrgApiError>;
pub async fn update_workspace(id: &str, body: &UpdateWorkspaceBody) -> Result<MutationResult, OrgApiError>;
pub async fn archive_workspace(id: &str, body: &ArchiveWorkspaceBody) -> Result<MutationResult, OrgApiError>;
```

Use `Request::post`, `Request::patch`, and `Request::post` respectively, `Content-Type: application/json`, the existing structured-error decoder, and URL encoding for IDs. Do not add authorization headers, storage, sessions, MCP calls, or any other mutation endpoint.

- [ ] **Step 5: Verify GREEN and commit**

Run: `cd crates/note-frontend && cargo test org::api`

Expected: mutation contract, error decoding, and existing read tests pass.

```bash
git add crates/note-frontend/Cargo.toml crates/note-frontend/Cargo.lock crates/note-frontend/src/org/api.rs crates/note-frontend/src/org/workspace_management.rs
git commit -m "feat(frontend): add workspace REST mutations"
```

### Task 3: Shared structured workspace form

**Files:**
- Create: `crates/note-frontend/src/components/org_workspace_form.rs`
- Modify: `crates/note-frontend/src/components/mod.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing form-source and reducer tests**

Test that the shared form exposes all ten approved sections, every policy field, native labels, save/cancel controls, and callbacks that preserve unrelated values. Test transition/state and tag-rule mutations through pure `WorkspaceDraft` methods rather than DOM mocks.

- [ ] **Step 2: Verify RED**

Run: `cd crates/note-frontend && cargo test org_workspace_form`

Expected: failure because the component is absent.

- [ ] **Step 3: Implement the form component**

Create `OrgWorkspaceForm` with props for draft, mode, busy state, server error, `on_change`, `on_submit`, `on_cancel`, and retry/reload actions. Render structured sections using existing `input`, `btn`, page-head, and operations-console conventions. Use add/remove rows for states and transitions, checkbox groups for types/state sets, selects for state roles/claim policy, numeric inputs for limits, and per-type allowed/required tag inputs.

- [ ] **Step 4: Add responsive styling and verify GREEN**

Add namespaced `.org-workspace-form-*` CSS with a restrained industrial ledger aesthetic, clear section hierarchy, 44px minimum interactive targets, visible focus, two-column desktop layout, single-column mobile layout, and distinct danger styling.

Run: `cd crates/note-frontend && cargo test org_workspace_form`

Expected: all form/model tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/note-frontend/app.css crates/note-frontend/src/components/mod.rs crates/note-frontend/src/components/org_workspace_form.rs
git commit -m "feat(frontend): add structured workspace form"
```

### Task 4: Create and settings routes

**Files:**
- Create: `crates/note-frontend/src/pages/org_workspace_new.rs`
- Create: `crates/note-frontend/src/pages/org_workspace_settings.rs`
- Modify: `crates/note-frontend/src/pages/mod.rs`
- Modify: `crates/note-frontend/src/pages/org_workspaces.rs`
- Modify: `crates/note-frontend/src/routes.rs`

- [ ] **Step 1: Write failing route and page-state tests**

Assert route recognition/title/switch behavior for `/org/new` and `/org/:workspace_id/settings`. Add reducer tests for loading, submitting, successful navigation, validation failure, stale revision with Reload latest, retry preserving operation ID, and archived workspace suppression.

- [ ] **Step 2: Verify RED**

Run:

```bash
cd crates/note-frontend
cargo test routes::tests::recognizes_org_workspace_management_routes
cargo test org_workspace_new
cargo test org_workspace_settings
```

Expected: missing route/page failures.

- [ ] **Step 3: Implement create page**

Initialize `WorkspaceDraft::new()`, generate a workspace UUID and one operation UUID per submission, validate, call `org_api::create_workspace`, disable duplicate submission, preserve errors, and navigate to `Route::OrgWorkspace` on success. Add **Create workspace** to the directory header and replace copy that describes the entire page as read-only with content-specific wording.

- [ ] **Step 4: Implement settings page**

Load the workspace with the existing GET client, map it losslessly into the shared form, reject edit rendering when archived, submit the loaded revision to PATCH, and support Reload latest and retry semantics. On success navigate to the workspace operations route.

- [ ] **Step 5: Verify GREEN and commit**

Run:

```bash
cd crates/note-frontend
cargo test routes
cargo test org_workspace_new
cargo test org_workspace_settings
```

Expected: route, state, and existing page tests pass.

```bash
git add crates/note-frontend/src/pages/mod.rs crates/note-frontend/src/pages/org_workspace_new.rs crates/note-frontend/src/pages/org_workspace_settings.rs crates/note-frontend/src/pages/org_workspaces.rs crates/note-frontend/src/routes.rs
git commit -m "feat(frontend): add workspace create and settings pages"
```

### Task 5: Edit and archive actions on the workspace page

**Files:**
- Modify: `crates/note-frontend/src/pages/org_workspace.rs`
- Modify: `crates/note-frontend/app.css`

- [ ] **Step 1: Write failing action and archive-state tests**

Test that active workspace data renders edit/archive actions, archived data renders neither, exact-slug confirmation gates archival, an active-lease error remains visible, retry retains the operation ID, and success targets `/org?include_archived=true&limit=50`.

- [ ] **Step 2: Verify RED**

Run: `cd crates/note-frontend && cargo test pages::org_workspace::tests::workspace_management_actions`

Expected: missing controls and archive state.

- [ ] **Step 3: Implement actions and confirmation**

Link **Edit workspace** to settings. Reuse `Modal` for **Archive workspace**, require exact slug input, create an archive submission using the loaded revision, disable while pending, show structured errors, retry idempotently, and navigate to the archived directory on success. Do not expose hard delete, restore, work-item, claim, review, or document controls.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cd crates/note-frontend && cargo test pages::org_workspace`

Expected: active/archived/action tests pass.

```bash
git add crates/note-frontend/app.css crates/note-frontend/src/pages/org_workspace.rs
git commit -m "feat(frontend): manage workspace lifecycle"
```

### Task 6: Correct the PRD and architecture boundary

**Files:**
- Modify: `docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md`
- Modify: `docs/design.md`
- Modify: `README.md`
- Modify: `crates/note-frontend/src/org/api.rs`

- [ ] **Step 1: Write a failing boundary regression test**

Replace the broad GET-only source assertion with an allowlist assertion: the frontend may call only the three workspace mutation paths, and production sources must still exclude document/import/item/claim/transition/review mutations, MCP, auth/session state, and fencing tokens.

- [ ] **Step 2: Verify RED**

Run: `cd crates/note-frontend && cargo test org_console_browser_mutation_boundary`

Expected: current read-only wording/assertion conflicts with the approved workspace mutation surface.

- [ ] **Step 3: Revise documentation and boundary assertions**

Change the master PRD and architecture language from “no browser mutations” to “workspace create/update/archive only.” State explicitly that Org source and operational workflow remain read-only. Update README routes and capability summary.

- [ ] **Step 4: Verify GREEN and commit**

Run: `cd crates/note-frontend && cargo test org_console_browser_mutation_boundary`

Expected: the exact mutation allowlist passes and forbidden surfaces remain absent.

```bash
git add README.md docs/design.md docs/superpowers/specs/2026-07-30-org-orchestration-system-prd.md crates/note-frontend/src/org/api.rs
git commit -m "docs(org): correct workspace UI mutation boundary"
```

### Task 7: Browser acceptance and scoped frontend verification

**Files:**
- Create: `scripts/verify-org-workspace-management-browser.sh`
- Modify: `scripts/verify-org-console-browser.sh`

- [ ] **Step 1: Extend the browser verifier before implementation acceptance**

Create a disposable local database/server flow that opens `/org/new`, verifies all form sections,
creates a workspace, edits its description and concurrency limit, verifies stale/conflict rendering,
archives it with exact-slug confirmation, and confirms the archived page exposes no management
actions. Inspect network requests and allow only GET plus the exact POST/PATCH/archive endpoints.

- [ ] **Step 2: Run the browser verifier and fix only in-scope failures**

Run: `scripts/verify-org-workspace-management-browser.sh`

Expected: PASS with desktop and 390px mobile screenshots and no console errors.

- [ ] **Step 3: Run all scoped frontend checks**

```bash
cd crates/note-frontend && cargo test
cargo check --manifest-path crates/note-frontend/Cargo.toml --target wasm32-unknown-unknown
cd crates/note-frontend && trunk build --release
git diff --check
```

Expected: all frontend tests pass, Wasm check and release build exit 0, and no whitespace errors.

- [ ] **Step 4: Commit**

```bash
git add scripts/verify-org-console-browser.sh scripts/verify-org-workspace-management-browser.sh
git commit -m "test(frontend): verify workspace management lifecycle"
```

### Task 8: Integrate to main and deploy to ms04

**Files:**
- No source edits expected; deployment follows `.agents/skills/deploy/SKILL.md`.

- [ ] **Step 1: Re-read the deployment skill and verify the integration state**

Run the deploy skill preflight, confirm the implementation branch is clean, merge it into local
`main`, and rerun the required merged-main checks. Preserve unrelated changes.

- [ ] **Step 2: Build and publish the full-stack image**

Build the current checkout for `linux/amd64`, publish the exact image to the configured GHCR tag,
and record both the local image config ID and remote digest.

- [ ] **Step 3: Roll out only the Agent Note service**

On `ms04`, pre-pull the image or safely transfer the exact image if the configured registry proxy
is unavailable. Restart only `podman-agent-note.service` and verify it is active with the expected
image identity. Do not modify application authentication behavior.

- [ ] **Step 4: Run production acceptance**

Verify:

```text
https://agent-note.gsmlg.net/org
https://agent-note.gsmlg.net/org/new
https://agent-note.gsmlg.net/api/openapi.json
```

Use a disposable production workspace to exercise create, edit, and archive through the Web UI,
then confirm it remains archived. Verify no console errors, no auth controls, no forbidden Org
content/workflow mutations, and the exact deployed image identity.

- [ ] **Step 5: Final repository and runtime audit**

Run `git status --short --branch`, `git log --oneline -n 10`, service health, container logs, and
public URL checks. Completion requires clean local integration plus verified `ms04` runtime behavior.
