# Category Labels on the Dashboard

## Goal

Allow administrators to configure multiple existing label keys as categories. A category entry can select either every value for a label key or one exact value. The Home dashboard shows one section for each configured category key, with clickable chips for the selected values and the number of active notes carrying each value. Clicking a chip opens Notes with the exact category value filter applied.

For example, configuring `project` produces chips such as `yellow-dog · 12` and `sigma · 7`. Configuring only `project=yellow-dog` produces the pinned `yellow-dog · 12` chip, including when its count later becomes zero. Selecting `yellow-dog · 12` opens Notes filtered by `project=yellow-dog`.

## Scope

This feature extends persisted system configuration, label-value aggregation in both storage backends, the dashboard API, the System page, and the Home dashboard. Category chips use a backward-compatible `~<percent-encoded-key>==<percent-encoded-value>` exact-equality term while legacy selector meanings, note mutation behavior, and the existing dashboard label summary and recent-updates panels remain unchanged.

## System Configuration

`SystemConfig` has a top-level `category_labels: Vec<String>` field. Serde defaults the field to an empty vector so existing stored configuration remains valid and current installations retain the existing Home layout until categories are configured. Keeping the ordered string array avoids a stored-configuration migration and preserves the existing REST contract.

Each entry uses one of two forms:

- `key` selects every stored value for that label key.
- `key=value` pins one exact value for that label key.

The parser splits an entry at the first `=`. Therefore `project=a=b` means key `project` and exact value `a=b`, while `project=` pins the empty value. Exact values are preserved byte-for-byte, including leading or trailing whitespace, Unicode, and reserved URL characters. Only the key is checked for outer whitespace.

The System page adds a **Category labels** section:

- A category key is selected from the existing label-key catalog; arbitrary keys cannot be entered.
- The administrator chooses **All values** or **Exact value**. Exact values use a free-form input because a pinned value remains valid even if no active note currently carries it.
- Multiple keys and multiple exact values for the same key may be selected. Their order is preserved.
- Selected expressions are displayed as removable chips.
- Duplicate exact expressions cannot be selected.
- Exact entries sharing a key are combined into one Home panel, ordered by their first occurrence in configuration. Their chips retain configured order.
- A bare key supersedes every exact entry for that key. Adding the bare key replaces its exact entries. Exact entries cannot be added while the bare key remains configured.
- The existing system-settings save action persists category labels together with duplicate-check settings.

Configuration validation parses every expression centrally and rejects empty keys, keys with leading or trailing whitespace, duplicate exact expressions, mixed bare and exact entries for the same key, and keys that do not exist in the label-key catalog. Values are neither trimmed nor required to exist. A label key referenced by either form cannot be deleted until all of its category expressions are removed from system configuration. The delete API returns a clear client validation error rather than leaving a dangling configuration reference.

## Storage and Pipeline Contract

The existing storage aggregation accepts the unique parsed category keys and returns distinct stored label values with active-note counts. Turso and PostgreSQL retain the same database aggregation contract; the implementation must not load a paginated note list and count in the browser or pipeline.

Only active notes contribute to counts. Trashed notes are excluded. Values use the same string representation already exposed in note labels and label selectors, including typed label values.

The pipeline centrally parses the configured expressions, combines entries sharing a key, and then combines those selections with label-key metadata and aggregated values. Categories remain in first-seen configuration order. A bare category includes every aggregated value, ordered by descending note count and then lexicographically by displayed value. An exact category includes only its configured values in configured order. A configured exact value missing from the aggregate is still returned with count zero.

## Dashboard API

The existing `GET /api/dashboard` response gains a `categories` field with this shape:

```json
[
  {
    "key": "project",
    "description": "Project",
    "values": [
      { "value": "yellow-dog", "count": 12 },
      { "value": "sigma", "count": 7 }
    ]
  }
]
```

The OpenAPI schema and frontend DTO mirror this contract. An empty `category_labels` configuration returns an empty `categories` array. Exact configuration uses the same response shape: one exact entry produces a singleton `values` list, and multiple exact entries sharing a key produce one category containing those configured values.

The existing dashboard cache includes category groups. Successful system-configuration updates invalidate the cache immediately, as note and label mutations already do, so the next Home request observes the new configuration and current counts.

## Home Dashboard Interaction

Home renders one category panel per parsed key before the existing general label summary and recent updates. Each panel uses the label description as supporting text when it is present and shows one accessible chip per selected value.

Each chip displays the value and matching active-note count. Clicking it navigates to Notes at page 1 with the existing default page size and an exact equality filter. `key=value` is display shorthand; the shareable URL uses the reserved-prefix `~<percent-encoded-key>==<percent-encoded-value>` wire term so spaces, empty values, and special characters round-trip exactly.

A configured bare category with no values still renders its panel with `No notes in this category.` A configured exact value with no matching active notes renders its chip with `0 notes`. When no categories are configured, Home renders no category area and otherwise retains its current layout. Dashboard loading failures retain the existing non-destructive unavailable state.

## Error Handling and Consistency

- Invalid category grammar, duplicate exact expressions, mixed bare/exact entries, and unknown category keys return HTTP 400 and are not persisted.
- Deleting a label key referenced by either category form returns HTTP 400 with instructions to remove it from System configuration first.
- Aggregation errors fail the dashboard request through its existing server-error path; partial or misleading counts are not returned.
- Category configuration and counts share the normal dashboard cache invalidation path.

## Testing and Verification

Focused tests cover:

- core parsing and structural validation for bare, exact, empty, whitespace, Unicode, reserved-character, and embedded-`=` values;
- rejection of duplicate exact expressions and mixed bare/exact entries for the same key;
- pipeline rejection of unknown parsed keys and persistence of valid ordered expressions;
- protection against deleting a label key referenced through either form;
- shared Turso and PostgreSQL aggregation behavior, including distinct values, active-note-only counts, configured-key selection, and stable ordering;
- dashboard JSON and OpenAPI schema compatibility, bare and exact groups, pinned zero counts, empty categories, and cache invalidation after configuration changes;
- frontend All/Exact configuration selection, replacement/removal behavior, and accessible validation feedback;
- frontend category and pinned-value ordering, empty-category presentation data, count labels, and correctly escaped Notes filter URLs.

Verification runs the affected native workspace tests, the frontend test suite, the frontend `wasm32-unknown-unknown` check, targeted formatting, and `git diff --check`.
