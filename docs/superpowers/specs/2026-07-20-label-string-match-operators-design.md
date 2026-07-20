# Label String-Match Operators Design

## Goal

Extend note label filters with three string-match operators:

- `key^=value`: case-insensitive starts-with match.
- `key$=value`: case-insensitive ends-with match.
- `key~=pattern`: case-insensitive regular-expression match.

All three operators apply to the stored string representation of every label value type. Existing
equality, inequality, ordering, bare-key presence, and multi-selector AND behavior remain unchanged
for valid selector keys.

## Core Design

Add `StartsWith`, `EndsWith`, and `Regex` variants to `note_core::LabelOperator`, including their
string representations. Update selector parsing to recognize `^=`, `$=`, and `~=` before the plain
`=` token so the operator prefix cannot become part of the label key.

The shared label matcher remains the single behavior boundary used by the Turso and PostgreSQL
storage adapters:

- Starts-with and ends-with matching lowercase both operands before comparison.
- Regex matching compiles the caller-supplied pattern with case-insensitive matching enabled.
- An invalid regex returns `false` for that selector instead of failing the search request.

To keep operator parsing unambiguous, newly defined or auto-created label keys cannot contain the
selector-reserved characters `&`, `=`, `!`, `<`, `>`, `^`, `$`, or `~`. Existing cataloged keys are
still accepted when saving notes, but a pre-existing key containing one of these characters must be
renamed with a storage migration before selectors can address it unambiguously. The `&` term
separator is also reserved in operands; this selector grammar does not define escaping.

Empty prefix, suffix, and regex operands follow normal string/regex behavior and match any existing
value. The web UI continues converting an empty filter value into a bare-key presence selector.

## Frontend and Transport Surfaces

Add the three operators to the notes-page filter dropdown and its URL-state parser. The existing
frontend selector serializer already concatenates the key, operator, and value generically, so it
requires regression coverage but no behavior change.

The REST and MCP label-filter field descriptions will list the supported syntax so clients can
discover the new operators. Request and response payload shapes do not change.

## Testing

Implementation follows test-driven development:

1. Add failing `note-core` tests for parsing, case-insensitive prefix/suffix matching,
   case-insensitive regex matching, matching stored values of non-text types, and invalid regexes.
2. Add failing shared storage-contract tests proving list, summary, and count filtering.
3. Add a failing semantic-search test proving the search pipeline honors a new operator.
4. Add failing frontend tests for URL parsing and selector serialization.
5. Implement only the code needed to make those tests pass.

Run focused tests for the affected crates, frontend Wasm checking, formatting checks, and diff
validation. PostgreSQL contract verification may require the repository's configured test database;
if unavailable, report that environmental limitation without changing unrelated infrastructure.

## Out of Scope

- Database schema or storage trait changes.
- Changes to existing operator semantics.
- Regex capture extraction or replacement.
- Regex caching or a new precompiled selector abstraction.
