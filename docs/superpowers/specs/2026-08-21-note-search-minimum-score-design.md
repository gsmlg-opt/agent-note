# Note Search Minimum Score and Results Table Design

## Purpose

Agent Note's hybrid search currently returns every ranked dense candidate until the requested
limit is filled. Dense retrieval can rank irrelevant notes, so a search may appear to match every
note. Add one persisted System setting that removes low-scoring fused results consistently from
the web UI, REST API, and MCP tool. Present web search results with the same information and
actions as the normal Notes table, with Score as the first column.

## Scope

This change covers canonical Markdown-note hybrid search only. It changes the shared search
pipeline, System configuration and UI, REST search response, MCP/REST behavior tests, and the
Notes-page result table. It does not add request-specific score overrides, change weighted-RRF
scoring, change retrieval candidate generation, or change Org search.

## Persisted Configuration

Extend `SystemConfig` with a nested search configuration:

```json
{
  "search": {
    "minimum_score": 0.01
  }
}
```

`minimum_score` is a fused weighted-RRF score. Its default is `0.01`. The nested `search` value
and its field use Serde defaults so configurations stored before this change deserialize with the
new default without a database migration.

System configuration validation accepts finite, non-negative values. It rejects negative,
positive-infinity, negative-infinity, and NaN values without writing the configuration. There is
no upper bound: an administrator may intentionally choose a value that yields no results.

The System page adds a "Note search" section with a numeric "Minimum score" input and helper
text explaining that results below the fused-score threshold are hidden. All editable System
sections share one "Save settings" action so the entire configuration is persisted together.

## Search Behavior

`note-pipelines::search_notes_filtered` remains the single behavior boundary for web REST search
and MCP semantic search. It reads the persisted System configuration through the same storage
session used for retrieval.

After title and dense rankings are combined with weighted RRF, the pipeline walks the fused list
in descending score order and accepts only entries satisfying:

```text
score >= search.minimum_score
```

Filtering is inclusive. It happens before note hydration contributes toward the caller's requested
limit. Low-scoring entries therefore do not consume result slots, and the pipeline continues
through remaining fused candidates until it reaches the requested limit or exhausts eligible
candidates. Existing label filtering, deterministic score ordering, deleted-note exclusion, blank
query behavior, and zero-limit behavior remain unchanged.

Because REST and MCP both call the shared pipeline, the setting is global and neither transport
accepts a per-request threshold override.

## REST and MCP Contracts

The existing MCP semantic-search response already includes the fields needed by the table and
keeps its shape:

- `id`
- `title`
- `revision`
- `score`
- `labels`
- `created_at`
- `updated_at`

Extend the REST `SearchResultDto` to include `labels`, `created_at`, and `updated_at`, preserving
the existing fields. Update the generated OpenAPI schema and contract tests accordingly. Content,
attachments, and deleted timestamps remain excluded.

Both transports return only results meeting the saved minimum score.

## Notes Search Results UI

Replace the current search-result card list with the normal Notes table structure and prepend one
column:

```text
Score | Title | Labels | Created | Updated | Actions
```

The Score cell displays the fused weighted-RRF score to four decimal places. Search rows preserve
the normal table's title link, clickable label filters, formatted timestamps, view/edit/remove
actions, horizontal overflow behavior, pagination controls, and Notes URL context. Search-result
pagination continues to operate over the result collection returned by the server.

The frontend search model gains labels and timestamps from REST. The table implementation should
share row/table behavior with the notes list where practical so the two presentations do not
silently drift, without introducing a general-purpose table abstraction.

## Errors and Compatibility

Invalid settings use the existing System configuration error response and success/error alerts.
Failed validation does not modify stored settings. Existing stored configurations load with
`minimum_score = 0.01`; no schema migration or settings rewrite is required merely to read them.

Search storage or embedding failures keep their current transport error behavior. An empty set
after threshold filtering is a successful search response and renders the existing no-matches
state.

## Verification

Use test-first implementation with focused checks:

- Core tests prove the `0.01` default, backward-compatible deserialization, valid values, and
  rejection of negative or non-finite values.
- Shared storage contract tests prove the search setting round-trips for both storage engines.
- Pipeline tests prove below-threshold exclusion, inclusive equality, and that rejected candidates
  do not consume the requested limit.
- REST/OpenAPI tests prove the expanded result schema and saved global threshold behavior.
- MCP tests prove semantic search inherits the same saved threshold.
- Frontend model and rendering tests prove labels, timestamps, score, column order, and normal
  Notes-table interactions are retained.
- Run the affected core, storage-contract, pipeline, server, MCP, and frontend test targets,
  frontend Wasm checking, Rust formatting, and `git diff --check`.

## Out of Scope

- Changing RRF constants or score normalization
- A threshold supplied by individual REST or MCP requests
- Independent thresholds for title and dense retrieval
- Search-setting environment variables or command-line flags
- Changes to Org search or note-list filtering
- Deployment, image publication, pushing, or merging
