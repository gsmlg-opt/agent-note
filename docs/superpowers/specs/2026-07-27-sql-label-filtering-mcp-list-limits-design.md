# SQL Label Filtering and MCP List Limits Design

## Goal

Make label selectors reduce the database collection before note pagination and
semantic-search ranking, while preserving the complete existing selector contract.
Also make MCP `list_notes` use the same default and maximum limit as the REST list
endpoint.

Success means:

- MCP `list_notes` defaults to 10 notes and clamps its limit to 1,000.
- Turso and PostgreSQL apply label filters before `LIMIT` and `OFFSET`.
- Label-filtered title and dense retrieval exclude nonmatching notes before ranking
  and retrieval limits are applied.
- All existing selector behavior remains unchanged, including typed comparisons,
  case-insensitive prefix and suffix matching, Rust regular-expression semantics,
  invalid-regex handling, bare-key presence, and AND composition.
- No database schema migration is required.

## Shared Pagination Policy

Move the list pagination constants and normalization helpers out of the REST
transport and into the list-notes pipeline module:

- default limit: 10
- maximum limit: 1,000
- minimum limit: 0
- default and minimum offset: 0

REST continues accepting signed query values and clamps them to this policy. MCP
keeps `limit` and `offset` optional in its JSON schema, converts its unsigned values
to the shared signed representation, and normalizes them before calling the
pipeline. An omitted MCP limit therefore becomes an explicit `Some(10)` instead of
reaching storage as an unbounded `None`. An oversized MCP limit becomes
`Some(1000)`, and `limit: 0` returns an empty page.

Storage retains its existing optional limit contract for internal callers that
intentionally request an unbounded result. This change limits the MCP transport
without changing generic repository semantics.

## Preserving Selector Semantics

Native PostgreSQL and Turso expressions cannot reproduce every current selector
detail consistently. In particular, the application currently defines typed
comparison, Unicode case conversion, floating-point total ordering, version-vector
ordering, and case-insensitive Rust regex behavior in `note-core`.

The storage adapters therefore resolve selectors in two phases:

1. Batch-load the distinct stored label values and value types for the selector
   keys.
2. Use the existing `note-core` matcher to determine the exact raw stored values
   that satisfy each selector.

A bare-key selector does not need value resolution. A missing key, invalid regex, or
value selector with no matching stored values resolves to an empty filter result.
Empty prefix, suffix, and regex operands retain their existing behavior and match
every stored value for that key.

The resolved representation contains the selector key and either:

- presence-only matching for a bare key, or
- the exact set of raw stored values accepted by the shared matcher.

Selectors remain ANDed. Separate predicates are retained when multiple selectors
refer to the same key, so their combined behavior remains identical to applying the
existing matcher to each selector.

This phase evaluates operators over distinct label values, not over every note.
The database still performs the final note filtering, pagination, and retrieval
candidate reduction.

## SQL Filtering

Each backend converts the resolved selectors into correlated `EXISTS` predicates
against `note_labels` and `label_keys`:

- a presence selector matches the requested key;
- a value selector matches the requested key and one of its resolved raw values;
- one `EXISTS` predicate is generated per selector;
- all predicates are joined with `AND`.

PostgreSQL binds each value set as a text array and uses array membership. Turso
binds each value set as a JSON array and reads it through `json_each`. Both avoid
building unbounded placeholder lists.

The predicates are placed in the active-note query before ordering, pagination, or
counting. `list_notes` and `list_note_summaries` therefore hydrate only the selected
page. `count_notes` uses the same predicate builder with `COUNT(*)`, ensuring its
total is independent of pagination.

Add a repository operation that returns only active matching note IDs for a
selector set. It uses the same resolution and SQL predicate path but does not
hydrate note bodies, attachment metadata, or labels. This is the bridge between
label filtering and semantic retrieval.

## Label-Constrained Search

For a nonempty query and positive limit, filtered search runs in this order:

1. Parse label selectors and open one storage session.
2. Query the database for active matching note IDs through the SQL label filter.
3. Return immediately if no IDs match.
4. Create the dense query embedding.
5. Pass the matching ID set to both title and dense retrieval.
6. Apply the ID constraint inside each retrieval query before ranking and `LIMIT`.
7. Fuse the two constrained rankings and hydrate result notes as before.

PostgreSQL binds the allowed IDs as a text array. Turso binds them as a JSON array
and uses `json_each`. The Turso constraint must be part of the initial FTS candidate
query, not the later active-note cleanup query, so excluded title hits cannot occupy
the physical FTS candidate pool.

The retrieval repository methods gain an optional allowed-ID parameter. Direct
unfiltered callers pass `None`. Filtered search passes a nonempty slice; an empty
allowed set is handled by the pipeline before retrieval.

Label-filtered retrieval no longer needs the bounded `limit * 32` over-fetch
workaround because unrelated notes are removed before ranking. Behavior of the
unfiltered retrieval path is otherwise unchanged by this feature. The final
allowed-ID check may remain as a defensive invariant, but correctness must not
depend on post-ranking filtering.

## Error and Edge-Case Behavior

- Missing label keys and resolved empty value sets return empty results without
  running the main list or retrieval query.
- Invalid regex remains non-fatal and matches no values.
- Storage failures during value resolution, SQL filtering, or retrieval propagate
  through the existing storage and transport error mapping.
- Soft-deleted notes remain excluded from list, count, matching-ID, title, and dense
  queries.
- List ordering, weighted reciprocal-rank fusion, and deterministic tie-breaking
  remain unchanged.
- MCP request and response shapes remain unchanged; only limit normalization
  changes.
- MCP `list_notes` continues returning summaries only and does not include note
  bodies, attachment bytes, or Base64 content.

## Testing

Implementation follows test-driven development.

### Pagination

- Unit-test the shared normalization helper for omitted, zero, normal, negative
  REST, and oversized values.
- Test MCP `list_notes` with more than 10 notes and no limit, proving it returns 10.
- Test MCP zero, explicit normal, and oversized limits, including offset handling.
- Keep the MCP input fields optional in its generated schema.
- Keep the existing REST pagination behavior and update its tests to use the shared
  helper.

### Storage Filtering

Extend the shared storage contract, exercised by both Turso and PostgreSQL:

- cover bare-key, equality, inequality, ordering, prefix, suffix, and regex
  selectors;
- retain coverage for text, number, version, date, datetime, and time values;
- retain invalid-regex and non-text string-match behavior;
- prove multiple selectors are ANDed, including selectors on the same key;
- place newer nonmatching notes before an older matching note, request `limit = 1`,
  and assert that the matching note is returned;
- assert that count returns the complete filtered total regardless of page size;
- test the matching-note-ID operation without note hydration;
- retain negative and unbounded internal repository pagination behavior.

Backend-focused tests should additionally cover value-set parameterization and the
empty resolved-filter short circuit.

### Search

- Add a title-search regression where more highly ranked excluded notes precede a
  matching note, then prove a small retrieval limit still returns the matching
  note.
- Add the equivalent dense-search regression.
- Assert that allowed IDs are applied before ranking and `LIMIT` in both storage
  adapters.
- Assert that an empty matching-ID set skips embedding and retrieval.
- Update pipeline event-test adapters and direct retrieval callers for the optional
  allowed-ID parameter.

Run focused tests for `note-core`, `note-storage`, both storage adapters,
`note-storage-contract-tests`, `note-pipelines`, `note-mcp`, and `note-server`.
Finish with native formatting and workspace checks. The frontend is outside this
change unless a shared public type change makes its compilation relevant.

## Scope

In scope:

- shared REST/MCP list-limit normalization;
- SQL-first label filtering for list, summary, count, and matching IDs;
- pre-ranking allowed-ID constraints for title and dense retrieval;
- removal of filtered-search dependence on bounded over-fetching;
- focused tests and documentation.

Out of scope:

- changing selector grammar or operator semantics;
- database schema changes, migrations, or new indexes;
- changing MCP summary response fields;
- changing search weights or fusion behavior;
- frontend behavior or presentation;
- optimizing broad filters with persisted canonical comparison columns.
