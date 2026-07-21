# REST Note Attachment Metadata Design

## Goal

Stop embedding attachment bytes in REST note responses. Attachment entries returned in a note
contain metadata only:

- `id`
- `path`
- `mime`
- `description`

Neither `content` nor `content_base64` appears in the response. Clients retrieve bytes from the
existing `GET /api/notes/{id}/attachments/{path}` endpoint.

## REST Contract

Replace the current attachment response shape with a metadata-only response type. `NoteDto` uses
that type for its `attachments` field, so both `GET /api/notes/{id}` and the successful
`PUT /api/notes/{id}` response expose the same metadata-only note representation. Save and update
request payloads keep their existing content-bearing attachment shape.

The OpenAPI schemas must distinguish attachment input from attachment metadata output. The note
response schema must not advertise `content` or `content_base64`.

## Server Data Flow

`GET /api/notes/{id}` calls the existing metadata-only `get_note_metadata` pipeline instead of
`get_note`. This avoids hydrating every attachment from the configured attachment store merely to
discard the bytes during serialization.

The dedicated attachment route remains the only REST response that serves attachment bytes. Its
status codes, MIME handling, path normalization, and `nosniff` header remain unchanged.

The update pipeline already has the submitted attachment bytes in memory. Its response converts the
updated note to the same metadata-only `NoteDto`; no additional storage read is introduced.

## Frontend Compatibility

The frontend note DTO accepts metadata-only attachment entries. When loading a note for display or
editing, it retrieves each attachment through the dedicated attachment endpoint and reconstructs
the existing `AttachmentContent::Text` or `AttachmentContent::Base64` view model:

- valid UTF-8 bytes become `Text`;
- other bytes become base64-backed `Base64`.

Attachment paths continue to be encoded one segment at a time through the existing URL helper.
Fetches may run concurrently, but the reconstructed attachment list preserves the order returned by
the note response. Any attachment fetch failure fails the note load with the existing surfaced API
error behavior, preventing the editor from silently saving an incomplete attachment set.

## Testing

Implementation follows test-driven development:

1. Add a failing REST test proving `GET /api/notes/{id}` returns attachment metadata without
   `content` or `content_base64`.
2. Add a failing test using an attachment store that panics on reads, proving the note GET path is
   metadata-only.
3. Add a failing OpenAPI assertion that the note attachment response schema has exactly the four
   metadata fields.
4. Add failing frontend tests for metadata deserialization and byte-to-text/base64 conversion.
5. Implement the smallest server and frontend changes needed to make those tests pass.

Run the affected server and frontend suites, native formatting and checks, the frontend Wasm check,
and `git diff --check`.

## Out of Scope

- Changing save or update attachment request formats.
- Adding, deleting, or replacing attachment endpoints.
- Changing MCP note responses, which are already metadata-only.
- Changing attachment storage or database schemas.
- Removing attachment previews or editor support from the frontend.
