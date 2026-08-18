# Category Labels on the Dashboard

## Goal

Allow administrators to configure multiple existing label keys as categories. The Home dashboard shows one section for each configured category, with a clickable chip for every distinct value and the number of active notes carrying that value. Clicking a chip opens Notes with the exact category value filter applied.

For example, configuring `project` produces chips such as `yellow-dog · 12` and `sigma · 7`. Selecting `yellow-dog · 12` opens Notes filtered by `project=yellow-dog`.

## Scope

This feature extends persisted system configuration, label-value aggregation in both storage backends, the dashboard API, the System page, and the Home dashboard. Category chips use a backward-compatible `~<percent-encoded-key>==<percent-encoded-value>` exact-equality term while legacy selector meanings, note mutation behavior, and the existing dashboard label summary and recent-updates panels remain unchanged.

## System Configuration

`SystemConfig` gains a top-level `category_labels: Vec<String>` field. Serde defaults the field to an empty vector so existing stored configuration remains valid and current installations retain the existing Home layout until categories are configured.

The System page adds a **Category labels** section:

- A category key is selected from the existing label-key catalog; arbitrary keys cannot be entered.
- Multiple keys may be selected and their order is preserved.
- Selected keys are displayed as removable chips.
- The same key cannot be selected more than once.
- The existing system-settings save action persists category labels together with duplicate-check settings.

Configuration validation rejects empty keys, keys with leading or trailing whitespace, duplicate keys, and keys that do not exist in the label-key catalog. A label key that is configured as a category cannot be deleted until it is removed from system configuration. The delete API returns a clear client validation error rather than leaving a dangling configuration reference.

## Storage and Pipeline Contract

The storage repository exposes an aggregation operation that accepts the configured category keys and returns distinct stored label values with active-note counts. Turso and PostgreSQL implement the same contract using database aggregation; the implementation must not load a paginated note list and count in the browser or pipeline.

Only active notes contribute to counts. Trashed notes are excluded. Values use the same string representation already exposed in note labels and label selectors, including typed label values.

The pipeline combines the configured keys, label-key metadata, and aggregated values into category groups. Categories remain in configuration order. Values within each category are ordered by descending note count and then lexicographically by their displayed value, giving stable output when counts are equal.

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

The OpenAPI schema and frontend DTO mirror this contract. An empty `category_labels` configuration returns an empty `categories` array.

The existing dashboard cache includes category groups. Successful system-configuration updates invalidate the cache immediately, as note and label mutations already do, so the next Home request observes the new configuration and current counts.

## Home Dashboard Interaction

Home renders one category panel per configured key before the existing general label summary and recent updates. Each panel uses the label description as supporting text when it is present and shows one accessible chip per distinct value.

Each chip displays the value and matching active-note count. Clicking it navigates to Notes at page 1 with the existing default page size and an exact equality filter. `key=value` is display shorthand; the shareable URL uses the reserved-prefix `~<percent-encoded-key>==<percent-encoded-value>` wire term so spaces, empty values, and special characters round-trip exactly.

A configured category with no values still renders its panel with `No notes in this category.` When no categories are configured, Home renders no category area and otherwise retains its current layout. Dashboard loading failures retain the existing non-destructive unavailable state.

## Error Handling and Consistency

- Invalid category configuration returns HTTP 400 and is not persisted.
- Deleting a configured category label returns HTTP 400 with instructions to remove it from System configuration first.
- Aggregation errors fail the dashboard request through its existing server-error path; partial or misleading counts are not returned.
- Category configuration and counts share the normal dashboard cache invalidation path.

## Testing and Verification

Focused tests cover:

- core configuration defaults and structural validation;
- pipeline rejection of unknown category keys and persistence of valid ordered keys;
- protection against deleting a configured label key;
- shared Turso and PostgreSQL aggregation behavior, including distinct values, active-note-only counts, configured-key selection, and stable ordering;
- dashboard JSON and OpenAPI schema compatibility, empty categories, and cache invalidation after configuration changes;
- frontend configuration selection/removal without duplicates;
- frontend category ordering, empty-category presentation data, count labels, and correctly escaped Notes filter URLs.

Verification runs the affected native workspace tests, the frontend test suite, the frontend `wasm32-unknown-unknown` check, targeted formatting, and `git diff --check`.
