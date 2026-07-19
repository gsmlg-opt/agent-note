# OpenAPI and Swagger UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Generate an OpenAPI contract for every Agent Note REST operation and serve it through a vendored Swagger UI while excluding `/mcp`.

**Architecture:** Keep OpenAPI concerns inside `note-server`. Convert the three REST router factories to `OpenApiRouter<Arc<Context>>`, annotate their transport handlers and DTOs, merge their generated contracts in a new `openapi` module, and merge Swagger UI into the final Axum application without changing MCP or pipeline behavior.

**Tech Stack:** Rust 2021, Axum 0.8, Utoipa 5, utoipa-axum 0.2, utoipa-swagger-ui 9, Serde, Tower

---

## File Map

- Create `crates/note-server/src/openapi.rs`: REST contract composition, API
  metadata, documentation-only system schemas, Swagger router, and contract/UI
  tests.
- Modify `crates/note-server/Cargo.toml`: add Utoipa dependencies and test-body
  support already available through existing dev dependencies.
- Modify `Cargo.lock`: lock the selected Utoipa and vendored Swagger UI crates.
- Modify `crates/note-server/src/notes_api.rs`: document and jointly register
  note, trash, dashboard, rendering, and attachment operations.
- Modify `crates/note-server/src/labels_api.rs`: document and jointly register
  label operations.
- Modify `crates/note-server/src/system_api.rs`: document and jointly register
  system operations using server-local schema mirrors.
- Modify `crates/note-server/src/main.rs`: compose REST, MCP, and Swagger
  routers while retaining state and request limits.
- Modify `README.md`: document documentation URLs and their security exposure.

### Task 1: Establish the OpenAPI and Swagger foundation

**Files:**
- Modify: `crates/note-server/Cargo.toml`
- Modify: `Cargo.lock`
- Create: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/main.rs:1-5`
- Modify: `crates/note-server/src/notes_api.rs:679-710`
- Modify: `crates/note-server/src/labels_api.rs:131-142`
- Modify: `crates/note-server/src/system_api.rs:79-87`

- [ ] **Step 1: Add compatible documentation dependencies**

Add:

```toml
utoipa = { version = "5", features = ["axum_extras"] }
utoipa-axum = "0.2"
utoipa-swagger-ui = { version = "9", features = ["axum", "vendored"] }
```

Use the vendored feature so `build.rs` embeds packaged Swagger UI assets rather
than downloading a Swagger archive.

- [ ] **Step 2: Write failing foundation tests**

Declare `mod openapi;` in `main.rs`. Create `openapi.rs` with tests that call
functions which do not exist yet:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa::openapi::OpenApi;

    #[tokio::test]
    async fn serves_openapi_json() {
        let app = swagger_router(OpenApi::default());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
    }

    #[tokio::test]
    async fn serves_vendored_swagger_ui() {
        let app = swagger_router(OpenApi::default());
        let redirect = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(redirect.status(), StatusCode::SEE_OTHER);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/docs/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("/api/openapi.json"));
    }
}
```

- [ ] **Step 3: Run the foundation tests and verify RED**

Run:

```sh
cargo test -p note-server openapi::tests
```

Expected: compilation fails because `swagger_router` is undefined.

- [ ] **Step 4: Add the minimal documentation router**

Implement:

```rust
use axum::Router;
use note_pipelines::Context;
use std::sync::Arc;
use utoipa::openapi::{Info, OpenApi, Tag};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

pub fn rest_router() -> (Router<Arc<Context>>, OpenApi) {
    let (router, mut openapi) = OpenApiRouter::new()
        .merge(crate::notes_api::notes_router())
        .merge(crate::labels_api::labels_router())
        .merge(crate::system_api::system_router())
        .split_for_parts();
    openapi.info = Info::new("Agent Note HTTP API", env!("CARGO_PKG_VERSION"));
    openapi.tags = Some(vec![
        Tag::new("notes").description(Some("Create, list, retrieve, update, search, and download notes")),
        Tag::new("trash").description(Some("Restore or permanently delete trashed notes")),
        Tag::new("dashboard").description(Some("Read dashboard summary data")),
        Tag::new("rendering").description(Some("Render Markdown as HTML")),
        Tag::new("labels").description(Some("Manage the label-key catalog")),
        Tag::new("system").description(Some("Read and update system configuration and backups")),
    ]);
    (router, openapi)
}

pub fn swagger_router(openapi: OpenApi) -> Router {
    SwaggerUi::new("/api/docs")
        .url("/api/openapi.json", openapi)
        .into()
}
```

Temporarily preserve all existing runtime routes by returning
`OpenApiRouter<Arc<Context>>` from each REST factory and converting its existing
plain `Router`:

```rust
pub fn notes_router() -> OpenApiRouter<Arc<Context>> {
    Router::new()
        // existing routes unchanged
        .into()
}
```

Apply the same return-type wrapper to `labels_router` and `system_router`. This
step changes no HTTP behavior; the following tasks replace plain registrations
with documented registrations.

- [ ] **Step 5: Run the foundation tests and verify GREEN**

Run:

```sh
cargo test -p note-server openapi::tests
```

Expected: both tests pass.

- [ ] **Step 6: Commit the foundation**

```sh
git add Cargo.lock crates/note-server/Cargo.toml crates/note-server/src/openapi.rs \
  crates/note-server/src/main.rs crates/note-server/src/notes_api.rs \
  crates/note-server/src/labels_api.rs crates/note-server/src/system_api.rs
git commit -m "feat(server): add OpenAPI documentation scaffold"
```

### Task 2: Document note, trash, dashboard, and rendering operations

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`

- [ ] **Step 1: Write a failing notes-contract test**

Add a test beside the existing `notes_api` tests:

```rust
#[test]
fn openapi_contains_all_note_operations() {
    let (_, document) = notes_router().split_for_parts();
    let value = serde_json::to_value(document).unwrap();
    let paths = value["paths"].as_object().unwrap();
    let expected = [
        ("/api/notes", &["get", "post"][..]),
        ("/api/notes/count", &["get"][..]),
        ("/api/notes/{id}", &["get", "put", "delete"][..]),
        ("/api/notes/{id}/raw", &["get"][..]),
        ("/notes/{id}/content", &["get"][..]),
        ("/api/notes/{id}/attachments/{path}", &["get"][..]),
        ("/api/notes/search", &["post"][..]),
        ("/api/render", &["post"][..]),
        ("/api/trash", &["get"][..]),
        ("/api/trash/restore", &["post"][..]),
        ("/api/trash/{id}", &["delete"][..]),
        ("/api/dashboard", &["get"][..]),
    ];
    for (path, methods) in expected {
        let item = paths.get(path).unwrap_or_else(|| panic!("missing {path}"));
        for method in methods {
            assert!(item.get(*method).is_some(), "missing {method} {path}");
        }
    }
    assert_eq!(
        value["paths"]["/api/notes/{id}/raw"]["get"]["responses"]["200"]
            ["content"]["text/markdown"]["schema"]["type"],
        "string"
    );
    assert_eq!(
        value["paths"]["/api/render"]["post"]["responses"]["200"]["content"]
            ["text/html"]["schema"]["type"],
        "string"
    );
}
```

- [ ] **Step 2: Run the notes-contract test and verify RED**

Run:

```sh
cargo test -p note-server notes_api::tests::openapi_contains_all_note_operations
```

Expected: failure because the converted router has no generated paths.

- [ ] **Step 3: Add schemas and operation metadata**

Derive `utoipa::ToSchema` for all note transport DTOs:

```rust
SaveNoteRequest, AttachmentRequest, SaveNoteResponse, RenderRequest, NoteDto,
AttachmentResponse, NoteListDto, TrashNoteDto, DashboardLabelDto,
DashboardNoteDto, DashboardEmbeddingNoteDto, DashboardDto, CountNotesResponse,
RestoreNotesRequest, SearchQuery, SearchResultDto
```

The REST wire format represents each label as a two-element JSON array
`[key, value]`. Avoid requiring `ToSchema` on Rust tuples by adding
`crate::openapi::label_pairs_schema`, which returns an outer array whose items
are string arrays with `minItems = 2` and `maxItems = 2`, and applying:

```rust
#[schema(schema_with = crate::openapi::label_pairs_schema)]
pub labels: Vec<(String, String)>,
```

Apply this override to every request or response field with the label-pair
wire shape.

Derive `utoipa::IntoParams` with `#[into_params(parameter_in = Query)]` for
`ListNotesQuery` and `NoteContentQuery`. Make the two private request/query
types visible within the crate only where Utoipa requires it.

Add one `#[utoipa::path]` attribute per operation. Use these exact contracts:

| Operation | Tag | Success | Errors |
|---|---|---|---|
| `POST /api/notes` | notes | `200 application/json SaveNoteResponse` | `400`, `409`, `500 text/plain` |
| `GET /api/notes` | notes | `200 application/json [NoteListDto]` | `500 text/plain` |
| `GET /api/notes/count` | notes | `200 application/json CountNotesResponse` | `500 text/plain` |
| `GET /api/notes/{id}` | notes | `200 application/json NoteDto` | `404`, `500 text/plain` |
| `PUT /api/notes/{id}` | notes | `200 application/json NoteDto` | `400`, `404`, `500 text/plain` |
| `DELETE /api/notes/{id}` | notes | `204 empty` | `404`, `500 text/plain` |
| `GET /api/notes/{id}/raw` | notes | `200 text/markdown String`, `200 text/html String` | `400`, `404`, `500 text/plain` |
| `GET /notes/{id}/content` | notes | same as raw | same as raw |
| `GET /api/notes/{id}/attachments/{path}` | notes | `200 application/octet-stream BinaryBody` | `404`, `500 text/plain` |
| `POST /api/notes/search` | notes | `200 application/json [SearchResultDto]` | `500 text/plain` |
| `POST /api/render` | rendering | `200 text/html String` | none |
| `GET /api/trash` | trash | `200 application/json [TrashNoteDto]` | `500 text/plain` |
| `POST /api/trash/restore` | trash | `204 empty` | `400`, `404`, `500 text/plain` |
| `DELETE /api/trash/{id}` | trash | `204 empty` | `404`, `500 text/plain` |
| `GET /api/dashboard` | dashboard | `200 application/json DashboardDto` | `500 text/plain` |

Use explicit `params(("id" = String, Path, ...))` and
`params(("path" = String, Path, ...))` declarations. Reference the query structs
with `params(ListNotesQuery)` and `params(NoteContentQuery)`. Add:

```rust
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
struct BinaryBody(Vec<u8>);
```

Represent the two successful note-content media types as one response:

```rust
(status = 200, content(
    (String = "text/markdown"),
    (String = "text/html")
))
```

Create a thin `get_note_content_handler` that delegates to
`get_note_raw_handler` so each public alias has independent operation metadata
without duplicating content logic.

The Axum catch-all route must stay
`/api/notes/{id}/attachments/{*path}`. Utoipa cannot use that same template as a
valid OpenAPI path, so derive an attachment-only `OpenApi` fragment with the
documented path `/api/notes/{id}/attachments/{path}`, initialize
`OpenApiRouter::with_openapi(fragment)`, and register the runtime catch-all with
plain `.route(...)`. Register every other handler with
`.routes(utoipa_axum::routes!(...))`.

- [ ] **Step 4: Run notes tests and verify GREEN**

Run:

```sh
cargo test -p note-server notes_api::tests
```

Expected: all notes tests, including the generated-contract test, pass.

- [ ] **Step 5: Commit note documentation**

```sh
git add crates/note-server/src/notes_api.rs
git commit -m "feat(api): document note endpoints"
```

### Task 3: Document label operations

**Files:**
- Modify: `crates/note-server/src/labels_api.rs`

- [ ] **Step 1: Write a failing labels-contract test**

```rust
#[test]
fn openapi_contains_all_label_operations() {
    let (_, document) = labels_router().split_for_parts();
    let value = serde_json::to_value(document).unwrap();
    assert!(value["paths"]["/api/labels"]["get"].is_object());
    assert!(value["paths"]["/api/labels"]["post"].is_object());
    assert!(value["paths"]["/api/labels/{key}"]["put"].is_object());
    assert!(value["paths"]["/api/labels/{key}"]["delete"].is_object());
    assert_eq!(
        value["paths"]["/api/labels"]["post"]["responses"]["400"]["content"]
            ["text/plain"]["schema"]["type"],
        "string"
    );
}
```

- [ ] **Step 2: Run the labels-contract test and verify RED**

Run:

```sh
cargo test -p note-server labels_api::tests::openapi_contains_all_label_operations
```

Expected: failure because the router has no generated label paths.

- [ ] **Step 3: Add label schemas and annotations**

Derive `utoipa::ToSchema` for `DefineLabelKeyRequest`,
`UpdateLabelKeyRequest`, and `LabelKeyDto`. Annotate `value_type` descriptions
with the accepted values `text`, `number`, `version`, `date`, `datetime`, and
`time`.

Register the handlers with `utoipa_axum::routes!` and these contracts:

| Operation | Success | Errors |
|---|---|---|
| `POST /api/labels` | `200 empty` | `400`, `500 text/plain` |
| `GET /api/labels` | `200 application/json [LabelKeyDto]` | `500 text/plain` |
| `PUT /api/labels/{key}` | `200 empty` | `400 text/plain` |
| `DELETE /api/labels/{key}` | `200 empty` | `500 text/plain` |

Declare `key` as a required string path parameter and tag every operation
`labels`.

- [ ] **Step 4: Run label tests and verify GREEN**

Run:

```sh
cargo test -p note-server labels_api::tests
```

Expected: all label tests pass.

- [ ] **Step 5: Commit label documentation**

```sh
git add crates/note-server/src/labels_api.rs
git commit -m "feat(api): document label endpoints"
```

### Task 4: Document system operations and preserve core boundaries

**Files:**
- Modify: `crates/note-server/src/openapi.rs`
- Modify: `crates/note-server/src/system_api.rs`

- [ ] **Step 1: Write failing system-contract and parity tests**

Add:

```rust
#[test]
fn openapi_contains_all_system_operations() {
    let (_, document) = system_router().split_for_parts();
    let value = serde_json::to_value(document).unwrap();
    assert!(value["paths"]["/api/system/config"]["get"].is_object());
    assert!(value["paths"]["/api/system/config"]["put"].is_object());
    assert!(value["paths"]["/api/system/info"]["get"].is_object());
    assert_eq!(
        value["paths"]["/api/system/backup"]["get"]["responses"]["200"]
            ["content"]["application/gzip"]["schema"]["format"],
        "binary"
    );
}
```

In `openapi.rs`, serialize representative real values and compare their
property sets with the documentation-only schemas:

```rust
#[test]
fn system_schema_fields_match_runtime_serialization() {
    let runtime = serde_json::to_value(note_core::SystemConfig::default()).unwrap();
    let (_, document) = rest_router();
    let document = serde_json::to_value(document).unwrap();
    let runtime_keys = runtime.as_object().unwrap().keys().collect::<BTreeSet<_>>();
    let schema_keys = document["components"]["schemas"]["SystemConfigSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .collect::<BTreeSet<_>>();
    assert_eq!(runtime_keys, schema_keys);
}
```

Add the equivalent assertion for `SystemInfoSchema` using a representative
`note_pipelines::SystemInfo` value with all eight runtime fields. Serializing the
real type makes an added or renamed runtime field fail the parity test instead
of leaving a hand-written JSON fixture unchanged.

- [ ] **Step 2: Run system tests and verify RED**

Run:

```sh
cargo test -p note-server system_api::tests::openapi_contains_all_system_operations
cargo test -p note-server openapi::tests::system_schema_fields_match_runtime_serialization
```

Expected: the path test fails because no system operations are generated, and
the parity test fails because the schemas do not exist.

- [ ] **Step 3: Add server-local system schemas**

In `openapi.rs`, define `ToSchema` mirrors with these exact fields:

```rust
pub(crate) struct SystemConfigSchema {
    pub duplicate_check: DuplicateCheckConfigSchema,
}

pub(crate) struct DuplicateCheckConfigSchema {
    pub enabled: bool,
    pub rules: Vec<DuplicateCheckRuleSchema>,
}

pub(crate) struct DuplicateCheckRuleSchema {
    pub terms: Vec<DuplicateCheckTermSchema>,
}

pub(crate) struct DuplicateCheckTermSchema {
    pub key: String,
    pub value: Option<String>,
}

pub(crate) struct SystemInfoSchema {
    pub database_engine: String,
    pub database_path: Option<String>,
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    pub attachments_location: Option<String>,
    pub embedding_engine: String,
    pub embedding_model: String,
    pub embedding_fingerprint: String,
}
```

Derive `utoipa::ToSchema` on every mirror. Preserve the runtime
`skip_serializing_if = "Option::is_none"` behavior for duplicate-check term
values in the documented optionality.

- [ ] **Step 4: Annotate and register system handlers**

Use these contracts:

| Operation | Success | Errors |
|---|---|---|
| `GET /api/system/config` | `200 application/json SystemConfigSchema` | `500 text/plain` |
| `PUT /api/system/config` | `204 empty` | `400`, `500 text/plain` |
| `GET /api/system/info` | `200 application/json SystemInfoSchema` | `500 text/plain` |
| `GET /api/system/backup` | `200 application/gzip BinaryBody` | `500 text/plain` |

Move the reusable `BinaryBody` documentation schema into `openapi.rs` so both
notes and system operations can reference it. Tag all operations `system` and
register them with `utoipa_axum::routes!`.

- [ ] **Step 5: Run system and parity tests and verify GREEN**

Run:

```sh
cargo test -p note-server system_api::tests
cargo test -p note-server openapi::tests::system_schema
```

Expected: all system route and schema-parity tests pass.

- [ ] **Step 6: Commit system documentation**

```sh
git add crates/note-server/src/openapi.rs crates/note-server/src/notes_api.rs \
  crates/note-server/src/system_api.rs
git commit -m "feat(api): document system endpoints"
```

### Task 5: Integrate documentation into server startup

**Files:**
- Modify: `crates/note-server/src/main.rs:477-487`
- Modify: `README.md:100-152`
- Test: `crates/note-server/src/main.rs`

- [ ] **Step 1: Write a failing transport-composition test**

Extracting router composition makes final wiring testable without opening a
database or TCP listener. First add this test before defining the helper:

```rust
#[tokio::test]
async fn transport_router_serves_openapi_without_documenting_mcp() {
    let (_, generated_openapi) = openapi::rest_router();
    let generated = serde_json::to_value(&generated_openapi).unwrap();
    assert_eq!(generated["paths"].as_object().unwrap().len(), 17);
    assert!(generated["paths"].get("/mcp").is_none());
    assert!(!generated.to_string().contains("\"mcp\""));
    let operation_count = generated["paths"]
        .as_object()
        .unwrap()
        .values()
        .map(|item| item.as_object().unwrap().len())
        .sum::<usize>();
    assert_eq!(operation_count, 23);

    let rest = Router::new();
    let mcp = Router::new().route("/mcp", axum::routing::get(|| async { "mcp" }));
    let app = compose_transport_router(rest, mcp, generated_openapi, 1024);

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/openapi.json")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/mcp")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
```

- [ ] **Step 2: Run the binary test and verify RED**

Run:

```sh
cargo test -p note-server --bin note-server transport_router_serves_openapi
```

Expected: compilation fails because `compose_transport_router` does not exist.

- [ ] **Step 3: Implement final router composition**

Add:

```rust
fn compose_transport_router(
    rest: Router,
    mcp: Router,
    openapi: utoipa::openapi::OpenApi,
    max_request_bytes: usize,
) -> Router {
    rest.merge(mcp)
        .merge(openapi::swagger_router(openapi))
        .layer(DefaultBodyLimit::max(max_request_bytes))
}
```

Replace the inline REST construction with:

```rust
let (rest, openapi) = openapi::rest_router();
let rest = rest.with_state(ctx.clone());
let max_request_bytes = env_u64("NOTE_MAX_REQUEST_BYTES", 512 * 1024 * 1024);
let mut app = compose_transport_router(
    rest,
    note_mcp::mcp_router(ctx),
    openapi,
    max_request_bytes as usize,
);
```

Leave the existing static-directory fallback and server startup code after this
composition unchanged.

- [ ] **Step 4: Document the public documentation endpoints**

Add a README subsection under the end-to-end run instructions:

```markdown
### HTTP API documentation

Open the interactive Swagger UI at `http://127.0.0.1:6222/api/docs` or fetch
the generated OpenAPI document from
`http://127.0.0.1:6222/api/openapi.json`. In frontend development, the same
paths are available through Trunk on port `6221`.

The document covers the REST API only. `/mcp` uses MCP's own discovery and
schema mechanisms and is intentionally excluded.

Swagger UI has request execution enabled, including destructive operations.
It has the same unauthenticated network exposure as the REST API; use it only
on a trusted network or behind an authenticating reverse proxy.
```

- [ ] **Step 5: Run integration checks and verify GREEN**

Run:

```sh
cargo test -p note-server --bin note-server transport_router_serves_openapi
cargo test -p note-server openapi
```

Expected: the transport test and all focused OpenAPI tests pass.

- [ ] **Step 6: Commit server integration and documentation**

```sh
git add crates/note-server/src/main.rs README.md
git commit -m "feat(server): serve Swagger API documentation"
```

### Task 6: Verify the complete feature

**Files:**
- Verify all files changed by Tasks 1-5

- [ ] **Step 1: Check specification coverage**

Fetch the generated JSON in a test or from a locally started server and confirm
the expected operation count is 23 across 17 path templates. Confirm:

```text
/mcp is absent
/api/openapi.json is served
/api/docs redirects to /api/docs/
/api/docs/ serves embedded HTML
```

- [ ] **Step 2: Run formatting**

Run:

```sh
cargo fmt --all -- --check
```

Expected: exit code 0 with no formatting diff.

- [ ] **Step 3: Run the complete server suite**

Run:

```sh
cargo test -p note-server
```

Expected: all `note-server` unit and integration tests pass.

- [ ] **Step 4: Run all-target compilation**

Run:

```sh
cargo check -p note-server --all-targets
```

Expected: exit code 0 with no warnings introduced by this feature.

- [ ] **Step 5: Inspect the final diff and repository state**

Run:

```sh
git diff --check
git status --short --branch
git log --oneline -6
```

Expected: no uncommitted implementation changes, no whitespace errors, and the
design, plan, and focused implementation commits are present on the current
branch.
