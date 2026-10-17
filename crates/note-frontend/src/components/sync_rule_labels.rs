use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::state::LabelKey;

#[derive(Properties, PartialEq)]
pub struct SyncRuleLabelsProps {
    pub id: String,
    pub selector: String,
    pub labels: Vec<LabelKey>,
    pub disabled: bool,
    pub onchange: Callback<String>,
    pub ondraftchange: Callback<bool>,
}

#[function_component(SyncRuleLabels)]
pub fn sync_rule_labels(props: &SyncRuleLabelsProps) -> Html {
    let key = use_state(String::new);
    let value = use_state(String::new);
    let has_draft = !key.is_empty() || !value.is_empty();
    {
        let ondraftchange = props.ondraftchange.clone();
        use_effect_with(has_draft, move |pending| {
            ondraftchange.emit(*pending);
            || ()
        });
    }
    let on_key_input = {
        let key = key.clone();
        Callback::from(move |event: InputEvent| {
            key.set(event.target_unchecked_into::<HtmlInputElement>().value());
        })
    };
    let on_value_input = {
        let value = value.clone();
        Callback::from(move |event: InputEvent| {
            value.set(event.target_unchecked_into::<HtmlInputElement>().value());
        })
    };
    let on_add = {
        let key = key.clone();
        let value = value.clone();
        let selector = props.selector.clone();
        let onchange = props.onchange.clone();
        Callback::from(move |_| {
            if let Some(next) = append_label_condition(&selector, &key, &value) {
                onchange.emit(next);
                key.set(String::new());
                value.set(String::new());
            }
        })
    };
    let on_clear = {
        let key = key.clone();
        let value = value.clone();
        Callback::from(move |_| {
            key.set(String::new());
            value.set(String::new());
        })
    };
    let on_selector_input = {
        let onchange = props.onchange.clone();
        Callback::from(move |event: InputEvent| {
            onchange.emit(event.target_unchecked_into::<HtmlInputElement>().value());
        })
    };
    let datalist_id = format!("sync-label-keys-{}", props.id);
    let conditions = props
        .selector
        .split('&')
        .enumerate()
        .filter(|(_, term)| !term.trim().is_empty())
        .collect::<Vec<_>>();

    html! {
        <fieldset class="label-picker sync-label-picker" disabled={props.disabled}>
            <legend>{"Matching labels"}</legend>
            <p class="muted">{"All conditions in this rule must match. Labels added here match exact values, including case."}</p>
            if conditions.is_empty() {
                <p class="muted">{"Add a label to choose which notes this rule syncs."}</p>
            } else {
                <ul class="sync-label-list">
                    {for conditions.into_iter().map(|(index, term)| {
                        let label = condition_text(term);
                        let on_remove = {
                            let selector = props.selector.clone();
                            let onchange = props.onchange.clone();
                            Callback::from(move |_| onchange.emit(remove_label_condition(&selector, index)))
                        };
                        html! {
                            <li class="sync-label-chip" key={index}>
                                <code>{label.clone()}</code>
                                <button type="button" class="btn btn-sm btn-ghost"
                                    aria-label={format!("Remove condition {label}")}
                                    onclick={on_remove} disabled={props.disabled}>
                                    {"Remove"}
                                </button>
                            </li>
                        }
                    })}
                </ul>
            }
            <div class="label-picker-row">
                <label class="field label-key-input">
                    <span>{"Label key"}</span>
                    <input class="input" type="text" list={datalist_id.clone()}
                        placeholder="project" value={(*key).clone()} oninput={on_key_input}
                        disabled={props.disabled} />
                </label>
                <datalist id={datalist_id}>
                    {for props.labels.iter().map(|label| html! {
                        <option value={label.key.clone()} />
                    })}
                </datalist>
                <span class="label-eq">{"="}</span>
                <label class="field label-value-input">
                    <span>{"Label value"}</span>
                    <input class="input" type="text" placeholder="bear_note"
                        value={(*value).clone()} oninput={on_value_input} disabled={props.disabled} />
                </label>
                <button type="button" class="btn btn-outline" onclick={on_add}
                    disabled={props.disabled || key.trim().is_empty()}>{"Add label"}</button>
                if has_draft {
                    <button type="button" class="btn btn-ghost" onclick={on_clear}
                        disabled={props.disabled}>{"Clear label fields"}</button>
                }
            </div>
            if has_draft {
                <p class="muted" role="status">{"Add this label or clear the fields before saving settings."}</p>
            }
            <details>
                <summary>{"Advanced label selector"}</summary>
                <label class="field">
                    <span>{"Label selector"}</span>
                    <input class="input sync-label-selector" type="text"
                        value={props.selector.clone()} oninput={on_selector_input}
                        placeholder="project=bear_note" disabled={props.disabled} />
                </label>
            </details>
        </fieldset>
    }
}

fn condition_text(term: &str) -> String {
    match note_core::try_parse_label_selectors(term) {
        Ok(parsed) => match parsed.first() {
            Some(condition) => match &condition.value {
                Some(value) => format!(
                    "{} {} {value:?}",
                    condition.key,
                    condition.operator.as_str()
                ),
                None => format!("{} (any value)", condition.key),
            },
            None => term.to_string(),
        },
        Err(_) => format!("{term} (invalid condition)"),
    }
}

fn append_label_condition(selector: &str, key: &str, value: &str) -> Option<String> {
    if key.trim().is_empty() {
        return None;
    }
    let condition = format!(
        "~{}=={}",
        urlencoding::encode(key),
        urlencoding::encode(value)
    );
    Some(if selector.trim().is_empty() {
        condition
    } else {
        format!("{selector}&{condition}")
    })
}

fn remove_label_condition(selector: &str, index: usize) -> String {
    selector
        .split('&')
        .enumerate()
        .filter_map(|(position, term)| (position != index).then_some(term))
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_core::{label_matches_selector, try_parse_label_selectors, Label, LabelValueType};

    #[test]
    fn added_labels_roundtrip_reserved_characters_and_match_exactly() {
        for (key, value) in [
            ("project", "bear_note"),
            ("项目&scope!=%", "笔记 &=!<>^$~% +/"),
            ("kind", ""),
            ("kind", "  spaced value  "),
            ("number", "01"),
        ] {
            let encoded = append_label_condition("", key, value).unwrap();
            let parsed = try_parse_label_selectors(&encoded).unwrap();
            assert_eq!(parsed.len(), 1, "input must remain one condition");
            assert_eq!(parsed[0].key, key);
            assert_eq!(parsed[0].value.as_deref(), Some(value));
            let label = Label {
                key: key.into(),
                value: value.into(),
                description: String::new(),
                value_type: LabelValueType::Text,
            };
            assert!(label_matches_selector(&label, &parsed[0]));
            let different = Label {
                value: format!("{value}x"),
                ..label
            };
            assert!(!label_matches_selector(&different, &parsed[0]));
        }
        let selector = append_label_condition("", "number", "01").unwrap();
        let parsed = try_parse_label_selectors(&selector).unwrap();
        let number = Label {
            key: "number".into(),
            value: "1".into(),
            description: String::new(),
            value_type: LabelValueType::Number,
        };
        assert!(!label_matches_selector(&number, &parsed[0]));
    }

    #[test]
    fn appending_preserves_advanced_terms_and_exact_value() {
        let existing = " release>=1.2 & team^=Core & title~=bear.* ";
        let combined = append_label_condition(existing, "project", " Bear ").unwrap();
        assert!(combined.starts_with(&format!("{existing}&")));
        let parsed = try_parse_label_selectors(&combined).unwrap();
        assert_eq!(&parsed[..3], try_parse_label_selectors(existing).unwrap());
        assert_eq!(parsed[3].key, "project");
        assert_eq!(parsed[3].value.as_deref(), Some(" Bear "));
    }

    #[test]
    fn catalog_key_whitespace_preserves_label_identity() {
        let catalog_key = LabelKey {
            key: " project ".into(),
            description: String::new(),
            value_type: "text".into(),
        };
        let selector = append_label_condition("", &catalog_key.key, "bear_note").unwrap();
        let parsed = try_parse_label_selectors(&selector).unwrap();
        assert_eq!(parsed[0].key, catalog_key.key);
        let selected = Label {
            key: catalog_key.key,
            value: "bear_note".into(),
            description: String::new(),
            value_type: LabelValueType::Text,
        };
        assert!(label_matches_selector(&selected, &parsed[0]));
        let unpadded = Label {
            key: "project".into(),
            ..selected
        };
        assert!(!label_matches_selector(&unpadded, &parsed[0]));
    }

    #[test]
    fn an_empty_key_cannot_create_a_condition() {
        assert_eq!(
            append_label_condition("project=bear_note", "", "value"),
            None
        );
        assert_eq!(append_label_condition("", " \t ", "value"), None);
    }

    #[test]
    fn removal_preserves_other_terms_and_uses_the_selected_position() {
        let selector = "project=bear_note& team^=Core &project=bear_note";
        assert_eq!(
            remove_label_condition(selector, 0),
            " team^=Core &project=bear_note"
        );
        assert_eq!(
            remove_label_condition(selector, 1),
            "project=bear_note&project=bear_note"
        );
        assert_eq!(
            remove_label_condition(selector, 2),
            "project=bear_note& team^=Core "
        );
        assert_eq!(remove_label_condition(selector, 9), selector);
    }

    #[test]
    fn removing_the_final_condition_cannot_produce_a_match_all_rule() {
        let selector = remove_label_condition("project=bear_note", 0);
        assert!(try_parse_label_selectors(&selector).unwrap().is_empty());
        let config = note_core::SystemConfig {
            sync: note_core::SyncConfig {
                rules: vec![note_core::SyncRule {
                    id: "rule".into(),
                    enabled: true,
                    selector,
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(note_core::validate_system_config(&config).is_err());
    }
}
