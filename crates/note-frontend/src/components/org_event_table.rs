use serde_json::{Map, Value};
use yew::prelude::*;

use crate::org::{
    model::Event,
    time::{format_timestamp, UNAVAILABLE_TIME},
};

#[derive(Clone, Debug, PartialEq)]
struct EventRow {
    event: Event,
    metadata: Value,
}

fn normalize_sensitive_name(value: &str) -> String {
    let mut normalized = String::new();
    let mut previous_was_lower_or_digit = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_was_lower_or_digit {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
        } else if !normalized.ends_with('_') && !normalized.is_empty() {
            normalized.push('_');
            previous_was_lower_or_digit = false;
        }
    }
    normalized.trim_matches('_').to_owned()
}

fn is_sensitive_metadata_key(value: &str) -> bool {
    let normalized = normalize_sensitive_name(value);
    let contains_sensitive_word = normalized.split('_').any(|word| {
        matches!(
            word,
            "token" | "hash" | "secret" | "password" | "authentication" | "authorization"
        )
    });
    let compact = normalized.replace('_', "");
    contains_sensitive_word
        || matches!(compact.as_str(), "apikey" | "accesstoken")
        || ["token", "hash", "secret", "password"]
            .iter()
            .any(|suffix| compact.ends_with(suffix))
}

fn is_sensitive_metadata_value(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
    {
        return true;
    }
    ['=', ':'].iter().any(|separator| {
        trimmed
            .split_once(*separator)
            .is_some_and(|(name, _)| is_sensitive_metadata_key(name.trim()))
    })
}

pub(crate) fn safe_metadata(value: &Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .iter()
                .filter(|(key, _)| !is_sensitive_metadata_key(key))
                .map(|(key, value)| (key.clone(), safe_metadata(value)))
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.iter().map(safe_metadata).collect()),
        Value::String(value) if is_sensitive_metadata_value(value) => {
            Value::String("[redacted]".to_owned())
        }
        _ => value.clone(),
    }
}

pub(crate) fn safe_json_text(value: &Value) -> String {
    serde_json::to_string_pretty(&safe_metadata(value))
        .unwrap_or_else(|_| "Metadata unavailable".to_owned())
}

pub(crate) fn ordered_events(events: &[Event]) -> Vec<Event> {
    let mut events = events.to_vec();
    events.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.id.cmp(&right.id))
    });
    events
}

fn ordered_rows(events: &[Event]) -> Vec<EventRow> {
    ordered_events(events)
        .into_iter()
        .map(|event| EventRow {
            metadata: safe_metadata(&event.metadata),
            event,
        })
        .collect()
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgEventTableProps {
    pub events: Vec<Event>,
    pub workspace_timezone: String,
    #[prop_or_else(|| "Work-item event history ordered by server sequence.".to_owned())]
    pub caption: String,
}

#[function_component(OrgEventTable)]
pub fn org_event_table(props: &OrgEventTableProps) -> Html {
    let rows = ordered_rows(&props.events);
    html! {
        <div class="org-ledger-scroll org-events-scroll" tabindex="0" aria-label="Work-item event history table">
            <table class="org-ledger org-event-ledger">
                <caption>{ props.caption.clone() }</caption>
                <thead>
                    <tr>
                        <th scope="col">{ "Sequence" }</th>
                        <th scope="col">{ "Event" }</th>
                        <th scope="col">{ "Actor / attempt" }</th>
                        <th scope="col">{ "Workspace time" }</th>
                        <th scope="col">{ "State" }</th>
                        <th scope="col">{ "Summary / metadata" }</th>
                    </tr>
                </thead>
                <tbody>{ for rows.iter().map(|row| event_row(row, &props.workspace_timezone)) }</tbody>
            </table>
        </div>
    }
}

fn event_row(row: &EventRow, timezone: &str) -> Html {
    let event = &row.event;
    let occurred = format_timestamp(event.occurred_at, timezone);
    let metadata = safe_json_text(&row.metadata);
    html! {
        <tr>
            <th scope="row" class="org-sequence">{ event.sequence }</th>
            <td>
                <span class="org-cell-primary">{ event.event_type.clone() }</span>
                <span class="org-cell-secondary org-monospace">{ event.id.clone() }</span>
                <span class="org-cell-secondary">{ format!("Workspace · {}", event.workspace_id) }</span>
            </td>
            <td>
                <span class="org-cell-primary">{ event.actor_id.clone() }</span>
                <span class="org-cell-secondary">{ event.attempt_id.as_deref().unwrap_or("No attempt") }</span>
            </td>
            <td class={classes!("org-time-cell", (occurred.workspace == UNAVAILABLE_TIME).then_some("is-unavailable"))}>
                <time>{ occurred.workspace }</time>
                <span>{ if occurred.browser_local == UNAVAILABLE_TIME { "Browser local · Unavailable".to_owned() } else { format!("Local · {}", occurred.browser_local) } }</span>
            </td>
            <td>
                <span class="org-cell-primary">{ event.previous_state.as_deref().unwrap_or("—") }</span>
                <span class="org-cell-secondary">{ format!("→ {}", event.resulting_state.as_deref().unwrap_or("—")) }</span>
            </td>
            <td>
                <span class="org-cell-primary org-event-summary">{ event.summary.clone() }</span>
                <details class="org-event-metadata">
                    <summary>{ "Inspect metadata" }</summary>
                    <pre>{ metadata }</pre>
                </details>
            </td>
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(id: &str, workspace_id: &str, sequence: i64, occurred_at: i64) -> Event {
        Event {
            id: id.into(),
            workspace_id: workspace_id.into(),
            sequence,
            subject_kind: "work_item".into(),
            subject_id: "item-a".into(),
            actor_id: "agent-a".into(),
            attempt_id: Some("attempt-a".into()),
            event_type: "progress".into(),
            occurred_at,
            summary: "Recorded progress".into(),
            metadata: json!({"safe":{"phase":"build"}}),
            previous_state: Some("RUNNING".into()),
            resulting_state: Some("RUNNING".into()),
        }
    }

    #[test]
    fn event_rows_follow_sequence_not_timestamp_or_uuid() {
        let events = vec![
            event("0000", "workspace-a", 30, 1),
            event("ffff", "workspace-a", 10, 999),
            event("1111", "workspace-a", 20, 500),
        ];
        let order = ordered_rows(&events)
            .into_iter()
            .map(|row| (row.event.sequence, row.event.id))
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                (10, "ffff".into()),
                (20, "1111".into()),
                (30, "0000".into())
            ]
        );
    }

    #[test]
    fn metadata_recursively_drops_token_shaped_keys_and_values() {
        let value = json!({
            "safe": {"phase": "build", "fencing_token": "41"},
            "authorization": "Bearer secret",
            "fencingToken": "42",
            "token-digest": "digest",
            "leaseToken": "43",
            "tokenHash": "digest",
            "apiKey": "key",
            "password": "password",
            "tokenized_count": 8,
            "hashmap_size": 16,
            "secretary": "available",
            "passwordless": true,
            "items": [
                {"message":"access_token=hidden"},
                {"nested":{"opaque":"Bearer abc"}},
                {"status":"authentication completed"},
                {"result":"ok"}
            ]
        });
        let safe = safe_metadata(&value);
        for key in [
            "authorization",
            "fencingToken",
            "token-digest",
            "leaseToken",
            "tokenHash",
            "apiKey",
            "password",
        ] {
            assert!(safe.get(key).is_none(), "sensitive key survived: {key}");
        }
        let rendered = safe.to_string();
        assert!(rendered.contains("phase"));
        assert!(rendered.contains("result"));
        for preserved in [
            "tokenized_count",
            "hashmap_size",
            "secretary",
            "passwordless",
            "authentication completed",
        ] {
            assert!(rendered.contains(preserved));
        }
        for forbidden in [
            "fencing_token",
            "authorization",
            "fencingToken",
            "token-digest",
            "leaseToken",
            "tokenHash",
            "apiKey",
            "access_token=hidden",
            "Bearer secret",
            "Bearer abc",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "leaked {forbidden}: {rendered}"
            );
        }
    }
}
