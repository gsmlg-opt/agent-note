# REST Note Attachment Metadata Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Return attachment metadata, never inline attachment bytes, in REST note responses while preserving frontend display and editing behavior through the dedicated attachment endpoint.

**Architecture:** `note-server` will serialize a metadata-only attachment DTO and use the existing `get_note_metadata` pipeline for `GET /api/notes/{id}`. `note-frontend` will deserialize that metadata and explicitly retrieve each attachment from `/api/notes/{id}/attachments/{path}`, reconstructing its existing text/base64 view model.

**Tech Stack:** Rust 2021, Axum, Utoipa/OpenAPI, Yew/Wasm, gloo-net, base64, Tokio tests.

---

## File Map

- Modify `crates/note-server/src/notes_api.rs`: define metadata-only REST attachment responses, switch note GET to metadata retrieval, and update REST/OpenAPI regression tests.
- Modify `crates/note-frontend/src/api.rs`: deserialize attachment metadata, fetch bytes from the existing attachment endpoint, and reconstruct frontend attachment content.

### Task 1: Establish Failing REST Contract Tests

**Files:**
- Test: `crates/note-server/src/notes_api.rs`

- [ ] **Step 1: Change the attachment round-trip assertion to require metadata only**

In `note_attachments_roundtrip_and_render_as_relative_files`, keep the metadata assertions and replace the inline-content assertions with:

```rust
assert!(attachments[0].get("content").is_none());
assert!(attachments[0].get("content_base64").is_none());
```

- [ ] **Step 2: Add a note GET test that removes the stored file before reading metadata**

Add this test beside the attachment round-trip test:

```rust
#[tokio::test]
async fn get_note_returns_attachment_metadata_without_reading_files() {
    let (app, _ctx, dir) = test_app().await;
    let id = save_note_id(
        app.clone(),
        r#"{"title":"Metadata only","content":"C","attachments":[{"id":"proof","path":"./proof.txt","mime":"text/plain","description":"proof","content":"payload"}],"labels":[]}"#,
    )
    .await;
    std::fs::remove_file(
        dir.path()
            .join("attachments")
            .join(&id)
            .join("proof.txt"),
    )
    .unwrap();

    let resp = app
        .oneshot(get(&format!("/api/notes/{id}")))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        json["attachments"][0],
        serde_json::json!({
            "id": "proof",
            "path": "./proof.txt",
            "mime": "text/plain",
            "description": "proof"
        })
    );
}
```

- [ ] **Step 3: Make the OpenAPI test require exactly four response fields**

In `openapi_documents_attachment_transport_contract`, keep content encoding assertions only for the request schema and assert the response shape:

```rust
let response = &schemas["AttachmentMetadataResponse"];
let response_properties = response["properties"].as_object().unwrap();
assert_eq!(
    response_properties.keys().cloned().collect::<std::collections::BTreeSet<_>>(),
    ["description", "id", "mime", "path"]
        .into_iter()
        .map(str::to_string)
        .collect()
);
assert_eq!(response["properties"]["description"]["default"], "");
```

- [ ] **Step 4: Run the focused tests and verify RED**

Run:

```bash
cargo test -p note-server notes_api::tests::note_attachments_roundtrip_and_render_as_relative_files
cargo test -p note-server notes_api::tests::get_note_returns_attachment_metadata_without_reading_files
cargo test -p note-server notes_api::tests::openapi_documents_attachment_transport_contract
```

Expected: the first test fails because inline content is present; the second fails with HTTP 500 because GET hydrates the removed file; the OpenAPI test fails because `AttachmentMetadataResponse` does not exist.

- [ ] **Step 5: Commit the failing REST tests**

```bash
git add crates/note-server/src/notes_api.rs
git commit -m "test(notes): require metadata-only REST attachments"
```

### Task 2: Implement Metadata-Only REST Note Responses

**Files:**
- Modify: `crates/note-server/src/notes_api.rs`

- [ ] **Step 1: Use the metadata pipeline in the note GET handler**

Replace the `get_note` import with `get_note_metadata`, then call it from `get_note_handler`:

```rust
match get_note_metadata(&ctx, &id)
    .await
    .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
{
    Some(note) => Ok(Json(NoteDto::from(note))),
    None => Err((axum::http::StatusCode::NOT_FOUND, "note not found".into())),
}
```

- [ ] **Step 2: Replace the content-bearing attachment response DTO**

Replace `AttachmentResponse` and its conversion with:

```rust
#[derive(Serialize, utoipa::ToSchema)]
pub struct AttachmentMetadataResponse {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[schema(default = "")]
    pub description: String,
}

impl From<NoteAttachment> for AttachmentMetadataResponse {
    fn from(attachment: NoteAttachment) -> Self {
        Self {
            id: attachment.id,
            path: attachment.path,
            mime: attachment.mime,
            description: attachment.description,
        }
    }
}
```

Change `NoteDto.attachments` to:

```rust
pub attachments: Vec<AttachmentMetadataResponse>,
```

The existing `From<Note> for NoteDto` conversion remains valid and discards each attachment's byte vector when converting to metadata.

- [ ] **Step 3: Run the focused REST tests and verify GREEN**

Run:

```bash
cargo test -p note-server notes_api::tests::note_attachments_roundtrip_and_render_as_relative_files
cargo test -p note-server notes_api::tests::get_note_returns_attachment_metadata_without_reading_files
cargo test -p note-server notes_api::tests::openapi_documents_attachment_transport_contract
```

Expected: PASS.

- [ ] **Step 4: Run the complete note API test module**

Run:

```bash
cargo test -p note-server notes_api::tests
```

Expected: PASS with no changed attachment download behavior.

- [ ] **Step 5: Commit the server implementation**

```bash
git add crates/note-server/src/notes_api.rs
git commit -m "fix(notes): return attachment metadata from REST"
```

### Task 3: Establish Failing Frontend Compatibility Tests

**Files:**
- Test: `crates/note-frontend/src/api.rs`

- [ ] **Step 1: Replace legacy response tests with metadata and byte-conversion tests**

Replace `attachment_responses_accept_legacy_text_and_canonical_base64` and `attachment_responses_prefer_utf8_text_when_both_are_present` with:

```rust
#[test]
fn attachment_metadata_deserializes_without_inline_content() {
    let metadata: NoteAttachmentMetadataDto = serde_json::from_value(serde_json::json!({
        "id": "proof",
        "path": "./proof.txt",
        "mime": "text/plain",
        "description": "proof"
    }))
    .unwrap();

    assert_eq!(metadata.id, "proof");
    assert_eq!(metadata.path, "./proof.txt");
    assert_eq!(metadata.mime, "text/plain");
    assert_eq!(metadata.description, "proof");
}

#[test]
fn attachment_bytes_prefer_utf8_and_fall_back_to_base64() {
    assert_eq!(
        attachment_content_from_bytes(b"hello".to_vec()),
        AttachmentContent::Text("hello".to_string())
    );
    assert_eq!(
        attachment_content_from_bytes(vec![0xff, 0x00]),
        AttachmentContent::Base64("/wA=".to_string())
    );
}
```

- [ ] **Step 2: Run frontend tests and verify RED**

Run:

```bash
cd crates/note-frontend
cargo test attachment_metadata_deserializes_without_inline_content
cargo test attachment_bytes_prefer_utf8_and_fall_back_to_base64
```

Expected: compilation fails because `NoteAttachmentMetadataDto` and `attachment_content_from_bytes` do not exist.

- [ ] **Step 3: Commit the failing frontend tests**

```bash
git add crates/note-frontend/src/api.rs
git commit -m "test(frontend): cover metadata-only attachments"
```

### Task 4: Hydrate Frontend Attachments Through the Dedicated Endpoint

**Files:**
- Modify: `crates/note-frontend/src/api.rs`

- [ ] **Step 1: Import base64 encoding support**

Add:

```rust
use base64::{engine::general_purpose::STANDARD, Engine as _};
```

- [ ] **Step 2: Replace the content-bearing response DTO with metadata**

Replace `NoteAttachmentResponseDto` and its `TryFrom` implementation with:

```rust
#[derive(Deserialize)]
struct NoteAttachmentMetadataDto {
    id: String,
    path: String,
    mime: String,
    #[serde(default)]
    description: String,
}

fn attachment_content_from_bytes(bytes: Vec<u8>) -> AttachmentContent {
    match String::from_utf8(bytes) {
        Ok(content) => AttachmentContent::Text(content),
        Err(error) => AttachmentContent::Base64(STANDARD.encode(error.into_bytes())),
    }
}

async fn fetch_note_attachment(
    note_id: &str,
    attachment: NoteAttachmentMetadataDto,
) -> Result<NoteAttachment, String> {
    let base = format!("/api/notes/{note_id}/attachments");
    let response = Request::get(&attachment_url(&base, &attachment.path))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    let bytes = ok_or_body_error(response)
        .await?
        .binary()
        .await
        .map_err(|error| error.to_string())?;
    Ok(NoteAttachment {
        id: attachment.id,
        path: attachment.path,
        mime: attachment.mime,
        description: attachment.description,
        content: attachment_content_from_bytes(bytes),
    })
}
```

Change `NoteDto.attachments` to `Vec<NoteAttachmentMetadataDto>`.

- [ ] **Step 3: Hydrate attachments while loading a note**

Replace the current `TryFrom` collection in `get_note` with an order-preserving loop:

```rust
let mut attachments = Vec::with_capacity(d.attachments.len());
for attachment in d.attachments {
    attachments.push(fetch_note_attachment(&d.id, attachment).await?);
}
```

Keep the existing `NoteSummary` construction unchanged. A failed fetch returns immediately, so the editor never receives a partial attachment set.

- [ ] **Step 4: Run frontend tests and Wasm checking**

Run:

```bash
cd crates/note-frontend
cargo test
cargo check --target wasm32-unknown-unknown
rustfmt --edition 2021 --check src/api.rs
```

Expected: all frontend tests PASS and the Wasm target compiles.

- [ ] **Step 5: Commit frontend compatibility**

```bash
git add crates/note-frontend/src/api.rs
git commit -m "fix(frontend): fetch attachment content explicitly"
```

### Task 5: Final Scoped Verification

**Files:**
- Verify all files listed above.

- [ ] **Step 1: Run native formatting and server verification**

Run:

```bash
cargo fmt --all -- --check
cargo test -p note-server notes_api::tests
cargo check --workspace --all-targets
```

Expected: PASS.

- [ ] **Step 2: Run frontend verification**

Run:

```bash
cd crates/note-frontend
cargo test
cargo check --target wasm32-unknown-unknown
rustfmt --edition 2021 --check src/api.rs
```

Expected: PASS.

- [ ] **Step 3: Validate repository state**

Run from the repository root:

```bash
git diff --check
git status --short --branch
git log -7 --oneline
```

Expected: no uncommitted changes, no whitespace errors, and the focused test/server/frontend commits follow the design and plan commits.
