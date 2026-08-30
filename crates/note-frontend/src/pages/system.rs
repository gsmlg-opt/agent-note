use note_core::parse_category_label_expression;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_duskmoon::Alert;

use crate::api;
use crate::components::icons;
use crate::state::{DuplicateCheckRule, DuplicateCheckTerm, LabelKey, SystemConfig, SystemInfo};
use crate::theme::{apply_theme, get_saved_theme, ThemeMode};


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CategoryLabelMode {
    All,
    Exact,
}

impl CategoryLabelMode {
    fn value(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Exact => "exact",
        }
    }

    fn from_value(value: &str) -> Self {
        match value {
            "exact" => Self::Exact,
            _ => Self::All,
        }
    }
}

#[function_component(SystemPage)]
pub fn system_page() -> Html {
    let config = use_state(|| None::<SystemConfig>);
    let info = use_state(|| None::<SystemInfo>);
    let labels = use_state(Vec::<LabelKey>::new);
    let labels_error = use_state(|| None::<String>);
    let loading = use_state(|| true);
    let saving = use_state(|| false);
    let error = use_state(|| None::<String>);
    let saved = use_state(|| false);
    let minimum_score_draft = use_state(String::new);
    let minimum_score_error = use_state(|| None::<String>);
    let category_key_draft = use_state(String::new);
    let category_mode_draft = use_state(|| CategoryLabelMode::All);
    let category_value_draft = use_state(String::new);
    let category_draft_error = use_state(|| None::<String>);
    let theme_mode = use_state(get_saved_theme);


    {
        let config = config.clone();
        let info = info.clone();
        let labels = labels.clone();
        let labels_error = labels_error.clone();
        let loading = loading.clone();
        let error = error.clone();
        let minimum_score_draft = minimum_score_draft.clone();
        let minimum_score_error = minimum_score_error.clone();
        use_effect_with((), move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                match api::get_system_config().await {
                    Ok(next) => {
                        minimum_score_draft.set(next.search.minimum_score.to_string());
                        minimum_score_error.set(None);
                        config.set(Some(next));
                    }
                    Err(message) => error.set(Some(message)),
                }
                match api::get_system_info().await {
                    Ok(next) => info.set(Some(next)),
                    Err(message) => error.set(Some(message)),
                }
                match api::list_labels().await {
                    Ok(next) => {
                        labels.set(next);
                        labels_error.set(None);
                    }
                    Err(message) => labels_error.set(Some(message)),
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

    let on_minimum_score = {
        let config = config.clone();
        let saved = saved.clone();
        let minimum_score_draft = minimum_score_draft.clone();
        let minimum_score_error = minimum_score_error.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let draft = input.value();
            minimum_score_draft.set(draft.clone());
            saved.set(false);

            let Some(mut next) = (*config).clone() else {
                return;
            };
            let transition = transition_minimum_score(next.search.minimum_score, draft);
            minimum_score_error.set(transition.error.map(str::to_string));
            if transition.error.is_none() {
                next.search.minimum_score = transition.minimum_score;
                config.set(Some(next));
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

    let on_category_key = {
        let category_key_draft = category_key_draft.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            category_key_draft.set(select.value());
        })
    };

    let on_category_mode = {
        let category_mode_draft = category_mode_draft.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            category_mode_draft.set(CategoryLabelMode::from_value(&select.value()));
        })
    };

    let on_category_value = {
        let category_value_draft = category_value_draft.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            category_value_draft.set(input.value());
        })
    };

    let on_add_category_label = {
        let config = config.clone();
        let saved = saved.clone();
        let category_key_draft = category_key_draft.clone();
        let category_mode_draft = category_mode_draft.clone();
        let category_value_draft = category_value_draft.clone();
        let category_draft_error = category_draft_error.clone();
        Callback::from(move |_| {
            if let Some(mut next) = (*config).clone() {
                match add_category_label(
                    &mut next.category_labels,
                    &category_key_draft,
                    *category_mode_draft,
                    &category_value_draft,
                ) {
                    Ok(()) => {
                        config.set(Some(next));
                        category_value_draft.set(String::new());
                        category_draft_error.set(None);
                        saved.set(false);
                    }
                    Err(message) => category_draft_error.set(Some(message.to_string())),
                }
            }
        })
    };

    let on_theme_select = {
        let theme_mode = theme_mode.clone();
        Callback::from(move |mode: ThemeMode| {
            theme_mode.set(mode);
            apply_theme(mode);
        })
    };

    let on_save = {
        let config = config.clone();
        let saving = saving.clone();
        let error = error.clone();
        let saved = saved.clone();
        let minimum_score_draft = minimum_score_draft.clone();
        let minimum_score_error = minimum_score_error.clone();
        Callback::from(move |_| {
            let minimum_score = match parse_minimum_score(&minimum_score_draft) {
                Ok(minimum_score) => minimum_score,
                Err(message) => {
                    minimum_score_error.set(Some(message.to_string()));
                    saved.set(false);
                    return;
                }
            };
            let Some(mut next) = (*config).clone() else {
                return;
            };
            next.search.minimum_score = minimum_score;
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

    let minimum_score_invalid = parse_minimum_score(&minimum_score_draft).is_err();

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

            <section class="system-section" aria-labelledby="appearance-title">
                <div class="system-section-head">
                    <div>
                        <h3 id="appearance-title">{ "Appearance" }</h3>
                        <p>{ "Choose system theme preference: automatic, light, or dark." }</p>
                    </div>
                </div>

                <div class="appearance-controls">
                    <div class="theme-controller" role="radiogroup" aria-label="System theme">
                        { for ThemeMode::ALL.iter().map(|&mode| {
                            let on_select = {
                                let on_theme_select = on_theme_select.clone();
                                Callback::from(move |_| on_theme_select.emit(mode))
                            };
                            let id = format!("theme-option-{}", mode.as_str());
                            let icon = match mode {
                                ThemeMode::Auto => icons::monitor(),
                                ThemeMode::Light => icons::sun(),
                                ThemeMode::Dark => icons::moon(),
                            };
                            html! {
                                <>
                                    <input
                                        type="radio"
                                        id={id.clone()}
                                        name="system-theme-preference"
                                        class="theme-controller-item"
                                        value={mode.as_str()}
                                        checked={*theme_mode == mode}
                                        onchange={on_select}
                                    />
                                    <label for={id} class="theme-controller-label">
                                        { icon }
                                        <span>{ mode.label() }</span>
                                    </label>
                                </>
                            }
                        }) }
                    </div>
                </div>
            </section>

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
                        if let Some(message) = &*labels_error {
                            <Alert variant={Some("error".to_string())}>
                                <span>{ format!("Label catalog unavailable: {message}") }</span>
                            </Alert>
                        } else if labels.is_empty() {
                            <p class="empty compact">{ "No label keys available. Create one on Labels first." }</p>
                        }

                        <div class="category-label-editor">
                            <label class="field">
                                <span>{ "Label name" }</span>
                                <select
                                    class="select category-label-select"
                                    value={(*category_key_draft).clone()}
                                    disabled={category_label_select_disabled((*labels_error).is_some(), *saving, &*labels, &current.category_labels, *category_mode_draft)}
                                    onchange={on_category_key}
                                >
                                    <option value="">{ "Choose a label" }</option>
                                    { for labels.iter()
                                        .filter(|label| category_label_key_available(&label.key, *category_mode_draft, &current.category_labels))
                                        .map(|label| html! { <option value={label.key.clone()}>{ label.key.clone() }</option> })
                                    }
                                </select>
                            </label>

                            <label class="field">
                                <span>{ "Values" }</span>
                                <select
                                    class="select"
                                    value={category_mode_draft.value()}
                                    disabled={category_label_controls_disabled((*labels_error).is_some(), *saving, &*labels)}
                                    onchange={on_category_mode}
                                >
                                    <option value="all">{ "All values" }</option>
                                    <option value="exact">{ "Exact value" }</option>
                                </select>
                            </label>

                            <div class="category-label-editor-value">
                                if *category_mode_draft == CategoryLabelMode::Exact {
                                    <label class="field">
                                        <span>{ "Exact value" }</span>
                                        <input
                                            type="text"
                                            class="input"
                                            value={(*category_value_draft).clone()}
                                            disabled={category_label_controls_disabled((*labels_error).is_some(), *saving, &*labels)}
                                            oninput={on_category_value}
                                        />
                                    </label>
                                }
                            </div>

                            <button
                                type="button"
                                class="btn btn-outline"
                                disabled={category_label_add_disabled((*labels_error).is_some(), *saving, &*labels, &current.category_labels, &category_key_draft, *category_mode_draft)}
                                onclick={on_add_category_label}
                            >
                                { "Add category" }
                            </button>
                        </div>

                        if let Some(message) = &*category_draft_error {
                            <p class="category-label-error" role="alert">{ message.clone() }</p>
                        }

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

                <section class="system-section" aria-labelledby="note-search-title">
                    <div class="system-section-head">
                        <div>
                            <h3 id="note-search-title">{ "Note search" }</h3>
                            <p>{ "Hide results below this fused weighted-RRF score." }</p>
                        </div>
                    </div>

                    <label class="field system-score-field">
                        <span>{ "Minimum score" }</span>
                        <input
                            type="number"
                            class="input"
                            min="0"
                            step="0.001"
                            value={(*minimum_score_draft).clone()}
                            disabled={*saving}
                            aria-invalid={minimum_score_error.is_some().to_string()}
                            aria-describedby={minimum_score_description_ids(minimum_score_error.is_some())}
                            oninput={on_minimum_score}
                        />
                        <p id="minimum-score-help" class="system-score-help">
                            { "Enter a finite score of zero or greater." }
                        </p>
                        if let Some(message) = &*minimum_score_error {
                            <p id="minimum-score-error" class="system-score-error" role="alert">
                                { message.clone() }
                            </p>
                        }
                    </label>
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
                    </div>
                </section>

                <div class="system-section-actions">
                    <button
                        type="button"
                        class="btn btn-primary"
                        onclick={on_save}
                        disabled={*saving || minimum_score_invalid}
                    >
                        { if *saving { "Saving..." } else { "Save settings" } }
                    </button>
                </div>
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

fn add_category_label(
    entries: &mut Vec<String>,
    key: &str,
    mode: CategoryLabelMode,
    value: &str,
) -> Result<(), &'static str> {
    if key.is_empty() {
        return Err("Choose a label name.");
    }

    match mode {
        CategoryLabelMode::All => {
            if entries.iter().any(|entry| {
                let expression = parse_category_label_expression(entry);
                expression.key == key && expression.value.is_none()
            }) {
                return Err("That all-values category is already configured.");
            }

            let first_exact_index = entries.iter().position(|entry| {
                let expression = parse_category_label_expression(entry);
                expression.key == key && expression.value.is_some()
            });
            match first_exact_index {
                Some(index) => {
                    entries.retain(|entry| {
                        let expression = parse_category_label_expression(entry);
                        expression.key != key || expression.value.is_none()
                    });
                    entries.insert(index, key.to_string());
                }
                None => entries.push(key.to_string()),
            }
        }
        CategoryLabelMode::Exact => {
            if entries.iter().any(|entry| {
                let expression = parse_category_label_expression(entry);
                expression.key == key && expression.value.is_none()
            }) {
                return Err("Remove the all-values category before adding exact values.");
            }

            let expression = format!("{key}={value}");
            if entries.iter().any(|entry| entry == &expression) {
                return Err("That exact category value is already configured.");
            }
            entries.push(expression);
        }
    }

    Ok(())
}

fn remove_category_label(entries: &mut Vec<String>, expression: &str) -> bool {
    let Some(index) = entries.iter().position(|entry| entry == expression) else {
        return false;
    };
    entries.remove(index);
    true
}

fn category_label_controls_disabled(labels_error: bool, saving: bool, labels: &[LabelKey]) -> bool {
    labels_error || saving || labels.is_empty()
}

fn category_label_key_available(key: &str, mode: CategoryLabelMode, selected: &[String]) -> bool {
    match mode {
        CategoryLabelMode::All => !selected.iter().any(|entry| {
            let expression = parse_category_label_expression(entry);
            expression.key == key && expression.value.is_none()
        }),
        CategoryLabelMode::Exact => !selected.iter().any(|entry| {
            let expression = parse_category_label_expression(entry);
            expression.key == key && expression.value.is_none()
        }),
    }
}

fn category_label_select_disabled(
    labels_error: bool,
    saving: bool,
    labels: &[LabelKey],
    selected: &[String],
    mode: CategoryLabelMode,
) -> bool {
    category_label_controls_disabled(labels_error, saving, labels)
        || !labels
            .iter()
            .any(|label| category_label_key_available(&label.key, mode, selected))
}

fn category_label_add_disabled(
    labels_error: bool,
    saving: bool,
    labels: &[LabelKey],
    selected: &[String],
    key: &str,
    mode: CategoryLabelMode,
) -> bool {
    category_label_controls_disabled(labels_error, saving, labels)
        || key.is_empty()
        || !labels.iter().any(|label| label.key == key)
        || !category_label_key_available(key, mode, selected)
}

#[derive(Debug, PartialEq)]
struct MinimumScoreTransition {
    draft: String,
    minimum_score: f32,
    error: Option<&'static str>,
}

fn minimum_score_description_ids(has_error: bool) -> &'static str {
    if has_error {
        "minimum-score-help minimum-score-error"
    } else {
        "minimum-score-help"
    }
}

fn parse_minimum_score(value: &str) -> Result<f32, &'static str> {
    if value.trim().is_empty() {
        return Err("Enter a minimum score.");
    }

    match value.parse::<f32>() {
        Ok(score) if !score.is_finite() => Err("Minimum score must be a finite number."),
        Ok(score) if score < 0.0 => Err("Minimum score must be zero or greater."),
        Ok(score) => Ok(score),
        Err(_) => Err("Enter a valid minimum score."),
    }
}

fn transition_minimum_score(previous_minimum_score: f32, draft: String) -> MinimumScoreTransition {
    match parse_minimum_score(&draft) {
        Ok(minimum_score) => MinimumScoreTransition {
            draft,
            minimum_score,
            error: None,
        },
        Err(message) => MinimumScoreTransition {
            draft,
            minimum_score: previous_minimum_score,
            error: Some(message),
        },
    }
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
    fn category_selection_adds_and_replaces_entries_by_parsed_key() {
        let mut keys = vec![
            "team".to_string(),
            "project=yellow-dog".to_string(),
            "kind=skill".to_string(),
        ];

        assert_eq!(
            add_category_label(&mut keys, "project", CategoryLabelMode::All, ""),
            Ok(())
        );
        assert_eq!(keys, ["team", "project", "kind=skill"]);
    }

    #[test]
    fn category_selection_preserves_exact_values_and_rejects_conflicting_modes() {
        let mut keys = vec!["team".to_string()];
        let value = "  yellow=dog&%+你好 ";

        assert_eq!(
            add_category_label(&mut keys, "project", CategoryLabelMode::Exact, value),
            Ok(())
        );
        assert_eq!(keys, vec!["team".to_string(), format!("project={value}")]);
        assert_eq!(
            add_category_label(&mut keys, "project", CategoryLabelMode::Exact, value),
            Err("That exact category value is already configured.")
        );
        assert_eq!(
            add_category_label(&mut keys, "team", CategoryLabelMode::Exact, "any"),
            Err("Remove the all-values category before adding exact values.")
        );
        assert_eq!(
            add_category_label(&mut keys, "kind", CategoryLabelMode::Exact, ""),
            Ok(())
        );
        assert_eq!(keys.last(), Some(&"kind=".to_string()));
    }

    #[test]
    fn category_selection_reports_missing_and_duplicate_all_values_keys() {
        let mut keys = Vec::new();

        assert_eq!(
            add_category_label(&mut keys, "", CategoryLabelMode::All, ""),
            Err("Choose a label name.")
        );
        assert_eq!(
            add_category_label(&mut keys, "project", CategoryLabelMode::All, ""),
            Ok(())
        );
        assert_eq!(keys, ["project"]);
        assert_eq!(
            add_category_label(&mut keys, "project", CategoryLabelMode::All, ""),
            Err("That all-values category is already configured.")
        );
    }

    #[test]
    fn category_selection_removes_only_requested_full_expression() {
        let mut keys = vec![
            "project=yellow-dog".to_string(),
            "project=".to_string(),
            "team".to_string(),
        ];
        assert!(!remove_category_label(&mut keys, "project"));
        assert_eq!(keys, ["project=yellow-dog", "project=", "team"]);
        assert!(remove_category_label(&mut keys, "project="));
        assert_eq!(keys, ["project=yellow-dog", "team"]);
        assert!(!remove_category_label(&mut keys, "missing"));
    }

    #[test]
    fn category_label_picker_is_disabled_when_catalog_is_unavailable() {
        let labels = vec![label_key("project")];

        assert!(category_label_select_disabled(
            false,
            true,
            &labels,
            &[],
            CategoryLabelMode::All,
        ));
        assert!(category_label_select_disabled(
            true,
            false,
            &labels,
            &[],
            CategoryLabelMode::All,
        ));
    }

    #[test]
    fn category_label_picker_is_disabled_when_catalog_is_empty_or_selected_mode_is_unavailable() {
        let labels = vec![label_key("project")];

        assert!(category_label_select_disabled(
            false,
            false,
            &[],
            &[],
            CategoryLabelMode::All,
        ));
        assert!(category_label_select_disabled(
            false,
            false,
            &labels,
            &["project".to_string()],
            CategoryLabelMode::Exact,
        ));
    }

    #[test]
    fn category_label_picker_uses_parsed_keys_for_mode_availability() {
        let labels = vec![label_key("project")];

        assert!(!category_label_select_disabled(
            false,
            false,
            &labels,
            &["project=yellow-dog".to_string()],
            CategoryLabelMode::All,
        ));
        assert!(category_label_select_disabled(
            false,
            false,
            &labels,
            &["project".to_string()],
            CategoryLabelMode::Exact,
        ));
    }

    #[test]
    fn minimum_score_draft_accepts_a_valid_finite_non_negative_number() {
        let transition = transition_minimum_score(0.01, "0.025".to_string());

        assert_eq!(transition.draft, "0.025");
        assert_eq!(transition.minimum_score, 0.025);
        assert_eq!(transition.error, None);
    }

    #[test]
    fn minimum_score_draft_rejects_empty_input() {
        let transition = transition_minimum_score(0.01, String::new());

        assert_eq!(transition.error, Some("Enter a minimum score."));
    }

    #[test]
    fn minimum_score_draft_rejects_negative_input() {
        let transition = transition_minimum_score(0.01, "-0.1".to_string());

        assert_eq!(
            transition.error,
            Some("Minimum score must be zero or greater.")
        );
    }

    #[test]
    fn minimum_score_draft_rejects_non_finite_input() {
        let transition = transition_minimum_score(0.01, "NaN".to_string());

        assert_eq!(
            transition.error,
            Some("Minimum score must be a finite number.")
        );
    }

    #[test]
    fn invalid_minimum_score_draft_preserves_the_prior_valid_config() {
        let transition = transition_minimum_score(0.025, "partial".to_string());

        assert_eq!(transition.draft, "partial");
        assert_eq!(transition.minimum_score, 0.025);
        assert_eq!(transition.error, Some("Enter a valid minimum score."));
    }

    #[test]
    fn system_page_has_one_shared_save_action_and_accessible_score_validation() {
        let source = include_str!("system.rs");

        assert_eq!(source.matches(concat!("Save ", "settings")).count(), 1);
        assert_eq!(source.matches(concat!("Add ", "category")).count(), 1);
        assert!(source.contains("Label name"));
        assert!(source.contains("Values"));
        assert!(source.contains("Exact value"));
        assert!(source.contains("category_draft_error"));
        assert!(source.contains("class=\"category-label-error\" role=\"alert\""));
    }

    #[test]
    fn minimum_score_description_ids_match_the_rendered_validation_state() {
        assert_eq!(minimum_score_description_ids(false), "minimum-score-help");
        assert_eq!(
            minimum_score_description_ids(true),
            "minimum-score-help minimum-score-error"
        );
    }

    #[test]
    fn system_page_includes_appearance_theme_section() {
        let source = include_str!("system.rs");

        assert!(source.contains("Appearance"));
        assert!(source.contains("theme-controller"));
        assert!(source.contains("aria-label=\"System theme\""));
        assert!(source.contains("theme-option-"));
        assert!(source.contains("system-theme-preference"));
    }


    fn label_key(key: &str) -> LabelKey {
        LabelKey {
            key: key.to_string(),
            description: String::new(),
            value_type: "string".to_string(),
        }
    }
}
