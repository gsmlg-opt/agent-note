use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::Alert;

use crate::api;
use crate::components::icons;
use crate::state::{DuplicateCheckRule, DuplicateCheckTerm, LabelKey, SystemConfig, SystemInfo};

#[function_component(SystemPage)]
pub fn system_page() -> Html {
    let config = use_state(|| None::<SystemConfig>);
    let info = use_state(|| None::<SystemInfo>);
    let labels = use_state(Vec::<LabelKey>::new);
    let loading = use_state(|| true);
    let saving = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);

    {
        let config = config.clone();
        let info = info.clone();
        let labels = labels.clone();
        let loading = loading.clone();
        let error = error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::get_system_config().await {
                    Ok(next) => config.set(Some(next)),
                    Err(message) => error.set(Some(message)),
                }
                match api::get_system_info().await {
                    Ok(next) => info.set(Some(next)),
                    Err(message) => error.set(Some(message)),
                }
                if let Ok(next) = api::list_labels().await {
                    labels.set(next);
                }
                loading.set(false);
            });
            || ()
        });
    }

    let on_enabled = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.enabled = input.checked();
                config.set(Some(next));
                saved.set(false);
            }
        })
    };

    let on_add_rule = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |_| {
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.rules.push(DuplicateCheckRule {
                    terms: vec![DuplicateCheckTerm {
                        key: String::new(),
                        value: None,
                    }],
                });
                config.set(Some(next));
                saved.set(false);
            }
        })
    };

    let on_add_category_label = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            let key = select.value();
            if let Some(mut next) = (*config).clone() {
                if add_category_label(&mut next.category_labels, &key) {
                    config.set(Some(next));
                    saved.set(false);
                }
            }
        })
    };

    let on_save = {
        let config = config.clone();
        let saving = saving.clone();
        let error = error.clone();
        let saved = saved.clone();
        Callback::from(move |_| {
            let Some(next) = (*config).clone() else {
                return;
            };
            saving.set(true);
            error.set(None);
            saved.set(false);
            let saving = saving.clone();
            let error = error.clone();
            let saved = saved.clone();
            wasm_bindgen_futures::spawn_local(async move {
                match api::update_system_config(&next).await {
                    Ok(()) => saved.set(true),
                    Err(message) => error.set(Some(message)),
                }
                saving.set(false);
            });
        })
    };

    html! {
        <section class="stack system-page">
            <div class="page-head">
                <div>
                    <h2 class="page-title">{ "System" }</h2>
                    <p class="page-hint">{ "Application settings and runtime storage." }</p>
                </div>
            </div>

            if let Some(message) = &*error {
                <Alert variant={Some("error".to_string())}><span>{ message.clone() }</span></Alert>
            }
            if *saved {
                <Alert variant={Some("success".to_string())}><span>{ "System settings saved." }</span></Alert>
            }

            if *loading {
                <p class="loading">{ "Loading..." }</p>
            } else if let Some(current) = &*config {
                <section class="system-section" aria-labelledby="category-labels-title">
                    <div class="system-section-head">
                        <div>
                            <h3 id="category-labels-title">{ "Category labels" }</h3>
                            <p>{ "Group notes by selected label values on the Home dashboard." }</p>
                        </div>
                    </div>

                    <div class="category-label-controls">
                        <select
                            class="select category-label-select"
                            aria-label="Add category label"
                            value=""
                            disabled={*saving || !labels.iter().any(|label| !current.category_labels.iter().any(|key| key == &label.key))}
                            onchange={on_add_category_label}
                        >
                            <option value="" disabled=true>{ "Add category label" }</option>
                            { for labels.iter()
                                .filter(|label| !current.category_labels.iter().any(|key| key == &label.key))
                                .map(|label| html! { <option value={label.key.clone()}>{ label.key.clone() }</option> })
                            }
                        </select>

                        <div class="category-label-selection">
                            if current.category_labels.is_empty() {
                                <p class="empty compact">{ "No category labels configured." }</p>
                            } else {
                                { for current.category_labels.iter().map(|key| {
                                    let config = config.clone();
                                    let saved = saved.clone();
                                    let key = key.clone();
                                    let key_to_remove = key.clone();
                                    let on_remove = Callback::from(move |_| {
                                        if let Some(mut next) = (*config).clone() {
                                            if remove_category_label(&mut next.category_labels, &key_to_remove) {
                                                config.set(Some(next));
                                                saved.set(false);
                                            }
                                        }
                                    });
                                    html! {
                                        <button
                                            type="button"
                                            class="chip chip-clickable chip-primary category-label-selection-chip"
                                            aria-label={format!("Remove category label {key}")}
                                            onclick={on_remove}
                                            disabled={*saving}
                                        >
                                            <span>{ key }</span>
                                            <span aria-hidden="true">{ "×" }</span>
                                        </button>
                                    }
                                }) }
                            }
                        </div>
                    </div>
                </section>

                <section class="system-section" aria-labelledby="duplicate-check-title">
                    <div class="system-section-head">
                        <div>
                            <h3 id="duplicate-check-title">{ "Duplicate note check" }</h3>
                            <p>{ "Applied when a new note is created." }</p>
                        </div>
                        <label class="system-toggle">
                            <input
                                type="checkbox"
                                class="toggle"
                                checked={current.duplicate_check.enabled}
                                disabled={*saving}
                                onchange={on_enabled}
                            />
                            <span>{ if current.duplicate_check.enabled { "Enabled" } else { "Disabled" } }</span>
                        </label>
                    </div>

                    <datalist id="system-label-options">
                        { for labels.iter().map(|label| html! {
                            <option value={label.key.clone()} />
                        }) }
                    </datalist>

                    <div class="duplicate-rules">
                        if current.duplicate_check.rules.is_empty() {
                            <p class="empty compact">{ "No duplicate rules configured." }</p>
                        } else {
                            { for current.duplicate_check.rules.iter().enumerate().map(|(rule_index, rule)| {
                                rule_view(rule_index, rule, &config, &saved, *saving)
                            }) }
                        }
                    </div>

                    <div class="system-section-actions">
                        <button type="button" class="btn btn-outline" onclick={on_add_rule} disabled={*saving}>
                            { icons::plus() }
                            <span>{ "Add rule" }</span>
                        </button>
                        <button type="button" class="btn btn-primary" onclick={on_save} disabled={*saving}>
                            { if *saving { "Saving..." } else { "Save settings" } }
                        </button>
                    </div>
                </section>
            }

            if let Some(info) = &*info {
                <section class="system-section" aria-labelledby="storage-info-title">
                    <div class="system-section-head">
                        <div>
                            <h3 id="storage-info-title">{ "Storage" }</h3>
                            <p>{ "Active storage backends and their locations." }</p>
                        </div>
                    </div>
                    <dl class="system-info">
                        <div>
                            <dt>{ "Database engine" }</dt>
                            <dd>{ info.database_engine.clone() }</dd>
                        </div>
                        if let Some(path) = &info.database_path {
                            <div>
                                <dt>{ "Database path" }</dt>
                                <dd>{ path.clone() }</dd>
                            </div>
                        }
                        if let Some(size) = info.database_size_bytes {
                            <div>
                                <dt>{ "Database size" }</dt>
                                <dd>{ format_bytes(size) }</dd>
                            </div>
                        }
                        <div>
                            <dt>{ "Attachments engine" }</dt>
                            <dd>{ info.attachments_engine.clone() }</dd>
                        </div>
                        if let Some(location) = &info.attachments_location {
                            <div>
                                <dt>{ "Attachments location" }</dt>
                                <dd>{ location.clone() }</dd>
                            </div>
                        }
                        <div>
                            <dt>{ "Embedding engine" }</dt>
                            <dd>{ info.embedding_engine.clone() }</dd>
                        </div>
                        <div>
                            <dt>{ "Embedding model" }</dt>
                            <dd>{ info.embedding_model.clone() }</dd>
                        </div>
                        <div>
                            <dt>{ "Embedding fingerprint" }</dt>
                            <dd>{ info.embedding_fingerprint.clone() }</dd>
                        </div>
                    </dl>
                </section>
            }

            <section class="system-section" aria-labelledby="backup-title">
                <div class="system-section-head">
                    <div>
                        <h3 id="backup-title">{ "Backup" }</h3>
                        <p>{ "Download all notes, labels, and attachments as a compressed archive." }</p>
                    </div>
                    <a class="btn btn-outline" href="/api/system/backup" download="">
                        { icons::download() }
                        <span>{ "Download backup" }</span>
                    </a>
                </div>
            </section>
        </section>
    }
}

fn rule_view(
    rule_index: usize,
    rule: &DuplicateCheckRule,
    config: &UseStateHandle<Option<SystemConfig>>,
    saved: &UseStateHandle<bool>,
    saving: bool,
) -> Html {
    let on_remove_rule = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |_| {
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.rules.remove(rule_index);
                config.set(Some(next));
                saved.set(false);
            }
        })
    };
    let on_add_term = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |_| {
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.rules[rule_index]
                    .terms
                    .push(DuplicateCheckTerm {
                        key: String::new(),
                        value: None,
                    });
                config.set(Some(next));
                saved.set(false);
            }
        })
    };

    html! {
        <article class="duplicate-rule">
            <div class="duplicate-rule-head">
                <div>
                    <h4>{ format!("Rule {}", rule_index + 1) }</h4>
                    <code>{ rule_expression(rule) }</code>
                </div>
                <button
                    type="button"
                    class="btn btn-ghost btn-icon icon-danger"
                    aria-label={format!("Remove rule {}", rule_index + 1)}
                    title="Remove rule"
                    onclick={on_remove_rule}
                    disabled={saving}
                >
                    { icons::trash() }
                </button>
            </div>
            <div class="duplicate-terms">
                { for rule.terms.iter().enumerate().map(|(term_index, term)| {
                    term_view(rule_index, term_index, term, rule.terms.len(), config, saved, saving)
                }) }
            </div>
            <button type="button" class="btn btn-ghost btn-sm duplicate-add-term" onclick={on_add_term} disabled={saving}>
                { icons::plus() }
                <span>{ "Add label" }</span>
            </button>
        </article>
    }
}

fn term_view(
    rule_index: usize,
    term_index: usize,
    term: &DuplicateCheckTerm,
    term_count: usize,
    config: &UseStateHandle<Option<SystemConfig>>,
    saved: &UseStateHandle<bool>,
    saving: bool,
) -> Html {
    let on_key = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.rules[rule_index].terms[term_index].key =
                    input.value().trim().to_string();
                config.set(Some(next));
                saved.set(false);
            }
        })
    };
    let on_value = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            if let Some(mut next) = (*config).clone() {
                let value = input.value();
                next.duplicate_check.rules[rule_index].terms[term_index].value =
                    (!value.is_empty()).then_some(value);
                config.set(Some(next));
                saved.set(false);
            }
        })
    };
    let on_remove = {
        let config = config.clone();
        let saved = saved.clone();
        Callback::from(move |_| {
            if let Some(mut next) = (*config).clone() {
                next.duplicate_check.rules[rule_index]
                    .terms
                    .remove(term_index);
                config.set(Some(next));
                saved.set(false);
            }
        })
    };

    html! {
        <div class="duplicate-term">
            <input
                class="input"
                type="text"
                list="system-label-options"
                value={term.key.clone()}
                placeholder="Label key"
                aria-label={format!("Rule {} label {} key", rule_index + 1, term_index + 1)}
                disabled={saving}
                oninput={on_key}
            />
            <span class="label-eq">{ "=" }</span>
            <input
                class="input"
                type="text"
                value={term.value.clone().unwrap_or_default()}
                placeholder="Any incoming value"
                aria-label={format!("Rule {} label {} fixed value", rule_index + 1, term_index + 1)}
                disabled={saving}
                oninput={on_value}
            />
            <button
                type="button"
                class="btn btn-ghost btn-icon icon-danger"
                disabled={saving || term_count == 1}
                aria-label={format!("Remove label from rule {}", rule_index + 1)}
                title="Remove label"
                onclick={on_remove}
            >
                { icons::trash() }
            </button>
        </div>
    }
}

fn rule_expression(rule: &DuplicateCheckRule) -> String {
    let terms = rule
        .terms
        .iter()
        .map(|term| match &term.value {
            Some(value) if !value.is_empty() => format!("{}={value}", term.key),
            _ if term.key.is_empty() => "label".to_string(),
            _ => term.key.clone(),
        })
        .collect::<Vec<_>>()
        .join(" + ");
    format!("[{terms}]")
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn add_category_label(keys: &mut Vec<String>, key: &str) -> bool {
    if key.is_empty() || keys.iter().any(|existing| existing == key) {
        return false;
    }

    keys.push(key.to_string());
    true
}

fn remove_category_label(keys: &mut Vec<String>, key: &str) -> bool {
    let before = keys.len();
    keys.retain(|existing| existing != key);
    keys.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_rule_expression_and_bytes() {
        let rule = DuplicateCheckRule {
            terms: vec![
                DuplicateCheckTerm {
                    key: "kind".to_string(),
                    value: Some("skill".to_string()),
                },
                DuplicateCheckTerm {
                    key: "version".to_string(),
                    value: None,
                },
            ],
        };
        assert_eq!(rule_expression(&rule), "[kind=skill + version]");
        assert_eq!(format_bytes(1536), "1.5 KB");
    }

    #[test]
    fn category_selection_preserves_order_and_rejects_duplicates() {
        let mut keys = vec!["project".to_string()];
        assert!(add_category_label(&mut keys, "team"));
        assert!(!add_category_label(&mut keys, "project"));
        assert!(!add_category_label(&mut keys, ""));
        assert_eq!(keys, vec!["project", "team"]);
    }

    #[test]
    fn category_selection_removes_only_requested_key() {
        let mut keys = vec!["project".to_string(), "team".to_string()];
        assert!(remove_category_label(&mut keys, "project"));
        assert_eq!(keys, vec!["team"]);
        assert!(!remove_category_label(&mut keys, "missing"));
    }
}
