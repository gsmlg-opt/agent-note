use std::rc::Rc;

use yew::prelude::*;
use yew_router::{prelude::*, query::Raw};

use crate::{
    components::org_event_table::{ordered_events, safe_json_text, OrgEventTable},
    org::{
        api::{self as org_api, OrgApiError},
        model::{Attempt, Event, ItemContext, NoteLink, Origin, Page, WorkItem},
        time::{format_org_timestamp, format_timestamp, UNAVAILABLE_TIME},
        url::{item_path, workspace_path, OrgReturnContext, WorkspaceQueryState},
    },
    routes::Route,
};

const EVENTS_LIMIT: u16 = 50;

#[derive(Clone, Debug, PartialEq)]
enum ContextLoad {
    Loading,
    Ready(Box<ItemContext>),
    Missing,
    Failed(OrgApiError),
}

#[derive(Clone, Debug, PartialEq)]
struct ContextPageState {
    generation: u64,
    load: ContextLoad,
}

impl Default for ContextPageState {
    fn default() -> Self {
        Self {
            generation: 0,
            load: ContextLoad::Loading,
        }
    }
}

enum ContextAction {
    Loading {
        generation: u64,
    },
    Loaded {
        generation: u64,
        context: Box<ItemContext>,
    },
    Failed {
        generation: u64,
        error: OrgApiError,
    },
}

impl Reducible for ContextPageState {
    type Action = ContextAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            ContextAction::Loading { generation } if generation >= self.generation => Self {
                generation,
                load: ContextLoad::Loading,
            }
            .into(),
            ContextAction::Loaded {
                generation,
                context,
            } if generation == self.generation => Self {
                generation,
                load: ContextLoad::Ready(context),
            }
            .into(),
            ContextAction::Failed { generation, error } if generation == self.generation => Self {
                generation,
                load: if error.status == Some(404) {
                    ContextLoad::Missing
                } else {
                    ContextLoad::Failed(error)
                },
            }
            .into(),
            _ => self,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum EventsLoad {
    Loading,
    Ready(Page<Event>),
    Failed(OrgApiError),
}

#[derive(Clone, Debug, PartialEq)]
struct EventsPageState {
    generation: u64,
    load: EventsLoad,
}

impl Default for EventsPageState {
    fn default() -> Self {
        Self {
            generation: 0,
            load: EventsLoad::Loading,
        }
    }
}

enum EventsAction {
    Loading { generation: u64 },
    Loaded { generation: u64, page: Page<Event> },
    Failed { generation: u64, error: OrgApiError },
}

impl Reducible for EventsPageState {
    type Action = EventsAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            EventsAction::Loading { generation } if generation >= self.generation => Self {
                generation,
                load: EventsLoad::Loading,
            }
            .into(),
            EventsAction::Loaded { generation, page } if generation == self.generation => Self {
                generation,
                load: EventsLoad::Ready(page),
            }
            .into(),
            EventsAction::Failed { generation, error } if generation == self.generation => Self {
                generation,
                load: EventsLoad::Failed(error),
            }
            .into(),
            _ => self,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemRenderState {
    Loading,
    Ready,
    Missing,
    Error,
}

fn item_render_state(state: &ContextPageState) -> ItemRenderState {
    match state.load {
        ContextLoad::Loading => ItemRenderState::Loading,
        ContextLoad::Ready(_) => ItemRenderState::Ready,
        ContextLoad::Missing => ItemRenderState::Missing,
        ContextLoad::Failed(_) => ItemRenderState::Error,
    }
}

fn validated_return(workspace_id: &str, query: &str) -> WorkspaceQueryState {
    OrgReturnContext::parse(workspace_id, query).validated(workspace_id)
}

fn return_href(workspace_id: &str, query: &str) -> String {
    workspace_path(workspace_id, &validated_return(workspace_id, query))
}

fn lineage_segments(context: &ItemContext) -> Vec<(String, Vec<Event>)> {
    context
        .history_segments
        .iter()
        .map(|segment| {
            (
                segment.workspace_id.clone(),
                ordered_events(&segment.events),
            )
        })
        .collect()
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgItemPageProps {
    pub workspace_id: String,
    pub item_id: String,
}

#[function_component(OrgItemPage)]
pub fn org_item_page(props: &OrgItemPageProps) -> Html {
    let context_state = use_reducer(ContextPageState::default);
    let events_state = use_reducer(EventsPageState::default);
    let context_generation = use_mut_ref(|| 0_u64);
    let events_generation = use_mut_ref(|| 0_u64);
    let refresh_tick = use_state(|| 0_u64);
    let event_cursor = use_state(|| None::<String>);
    let event_history = use_state(Vec::<Option<String>>::new);
    let navigator = use_navigator();
    let location = use_location();
    let raw_query = location
        .as_ref()
        .map(|location| location.query_str().trim_start_matches('?').to_owned())
        .unwrap_or_default();
    let return_state = validated_return(&props.workspace_id, &raw_query);
    let canonical_query =
        OrgReturnContext::new(&props.workspace_id, return_state.clone()).canonical_query();
    let is_canonical = raw_query == canonical_query;
    let route = Route::OrgItem {
        workspace_id: props.workspace_id.clone(),
        item_id: props.item_id.clone(),
    };

    {
        let navigator = navigator.clone();
        let route = route.clone();
        let return_state = return_state.clone();
        use_effect_with(
            (raw_query.clone(), canonical_query.clone()),
            move |(raw, canonical)| {
                if raw != canonical {
                    if let Some(navigator) = &navigator {
                        let query =
                            OrgReturnContext::new("", return_state.clone()).canonical_query();
                        let _ = navigator.replace_with_query(&route, Raw(query));
                    }
                }
                || ()
            },
        );
    }

    {
        let event_cursor = event_cursor.clone();
        let event_history = event_history.clone();
        use_effect_with(
            (
                props.workspace_id.clone(),
                props.item_id.clone(),
                canonical_query.clone(),
            ),
            move |_| {
                event_cursor.set(None);
                event_history.set(Vec::new());
                || ()
            },
        );
    }

    {
        let state = context_state.clone();
        let generation_ref = context_generation.clone();
        let workspace_id = props.workspace_id.clone();
        let item_id = props.item_id.clone();
        use_effect_with(
            (
                props.workspace_id.clone(),
                props.item_id.clone(),
                canonical_query.clone(),
                *refresh_tick,
                is_canonical,
            ),
            move |(_, _, _, _, is_canonical)| {
                let generation = {
                    let mut current = generation_ref.borrow_mut();
                    *current = current.saturating_add(1);
                    *current
                };
                state.dispatch(ContextAction::Loading { generation });
                if *is_canonical {
                    let state = state.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match org_api::get_item_context(&workspace_id, &item_id).await {
                            Ok(context) => state.dispatch(ContextAction::Loaded {
                                generation,
                                context: Box::new(context),
                            }),
                            Err(error) => {
                                state.dispatch(ContextAction::Failed { generation, error })
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    {
        let state = events_state.clone();
        let generation_ref = events_generation.clone();
        let workspace_id = props.workspace_id.clone();
        let item_id = props.item_id.clone();
        let cursor = (*event_cursor).clone();
        use_effect_with(
            (
                props.workspace_id.clone(),
                props.item_id.clone(),
                canonical_query.clone(),
                cursor.clone(),
                *refresh_tick,
                is_canonical,
            ),
            move |(_, _, _, _, _, is_canonical)| {
                let generation = {
                    let mut current = generation_ref.borrow_mut();
                    *current = current.saturating_add(1);
                    *current
                };
                state.dispatch(EventsAction::Loading { generation });
                if *is_canonical {
                    let state = state.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match org_api::list_item_events(
                            &workspace_id,
                            &item_id,
                            cursor.as_deref(),
                            EVENTS_LIMIT,
                        )
                        .await
                        {
                            Ok(page) => state.dispatch(EventsAction::Loaded { generation, page }),
                            Err(error) => {
                                state.dispatch(EventsAction::Failed { generation, error })
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    let on_refresh = {
        let refresh_tick = refresh_tick.clone();
        Callback::from(move |_| refresh_tick.set((*refresh_tick).saturating_add(1)))
    };
    let next_cursor = match &events_state.load {
        EventsLoad::Ready(page) => page.next_cursor.clone(),
        _ => None,
    };
    let on_next = {
        let event_cursor = event_cursor.clone();
        let event_history = event_history.clone();
        Callback::from(move |_| {
            if let Some(next) = &next_cursor {
                let mut history = (*event_history).clone();
                history.push((*event_cursor).clone());
                event_history.set(history);
                event_cursor.set(Some(next.clone()));
            }
        })
    };
    let on_previous = {
        let event_cursor = event_cursor.clone();
        let event_history = event_history.clone();
        Callback::from(move |_| {
            let mut history = (*event_history).clone();
            if let Some(previous) = history.pop() {
                event_history.set(history);
                event_cursor.set(previous);
            }
        })
    };
    let on_back = {
        let navigator = navigator.clone();
        let workspace_id = props.workspace_id.clone();
        let return_state = return_state.clone();
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            if let Some(navigator) = &navigator {
                let _ = navigator.push_with_query(
                    &Route::OrgWorkspace {
                        workspace_id: workspace_id.clone(),
                    },
                    Raw(return_state.canonical_query()),
                );
            }
        })
    };

    html! {
        <section
            class="stack org-item"
            aria-labelledby="org-item-title"
            data-testid="org-item-page"
        >
            <div class="page-head org-item-head">
                <div>
                    <a
                        href={return_href(&props.workspace_id, &raw_query)}
                        class="org-breadcrumb"
                        onclick={on_back}
                        data-testid="org-item-back"
                    >{ format!("Back to {}", return_state.view) }</a>
                    <p class="org-kicker">{ "Work-item context ledger" }</p>
                    <h2 id="org-item-title" class="page-title">
                        { context_title(&context_state, &props.item_id) }
                    </h2>
                    <p class="page-hint org-monospace">{ props.item_id.clone() }</p>
                </div>
                <button type="button" class="btn btn-outline" onclick={on_refresh} disabled={!is_canonical} data-testid="org-item-refresh">{ "Refresh context and events" }</button>
            </div>
            <div class="org-live-status" aria-live="polite" aria-atomic="true">
                { item_live_status(&context_state, &events_state) }
            </div>
            { item_content(&context_state, &events_state, &return_state, (*event_cursor).as_deref(), event_history.is_empty(), on_previous, on_next) }
        </section>
    }
}

fn context_title(state: &ContextPageState, item_id: &str) -> String {
    match &state.load {
        ContextLoad::Ready(context) => context.item.title.clone(),
        ContextLoad::Missing => "Work item not found".to_owned(),
        ContextLoad::Failed(_) => "Work item unavailable".to_owned(),
        ContextLoad::Loading => format!("Loading {item_id}"),
    }
}

fn item_live_status(context: &ContextPageState, events: &EventsPageState) -> String {
    match item_render_state(context) {
        ItemRenderState::Loading => "Loading work-item context".to_owned(),
        ItemRenderState::Missing => "Work item was not found".to_owned(),
        ItemRenderState::Error => "Work-item context could not be loaded".to_owned(),
        ItemRenderState::Ready => match &events.load {
            EventsLoad::Loading => "Context loaded; loading event history".to_owned(),
            EventsLoad::Failed(_) => "Context loaded; event history unavailable".to_owned(),
            EventsLoad::Ready(page) => format!("Context and {} event(s) loaded", page.items.len()),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn item_content(
    context_state: &ContextPageState,
    events_state: &EventsPageState,
    return_state: &WorkspaceQueryState,
    event_cursor: Option<&str>,
    previous_disabled: bool,
    on_previous: Callback<MouseEvent>,
    on_next: Callback<MouseEvent>,
) -> Html {
    match &context_state.load {
        ContextLoad::Loading => {
            html! { <p class="loading" data-testid="org-item-loading">{ "Loading work-item context..." }</p> }
        }
        ContextLoad::Missing => html! {
            <div class="org-directory-message" data-testid="org-item-missing">
                <strong>{ "Work item not found" }</strong>
                <p>{ "It may have moved, been deleted, or never existed in this workspace." }</p>
            </div>
        },
        ContextLoad::Failed(error) => structured_error("Work-item context unavailable", error),
        ContextLoad::Ready(context) => html! {
            <div class="org-item-ready" data-testid="org-item-ready">
                { context_overview(context) }
                { hierarchy_section(context, return_state) }
                { dependency_section(context, return_state) }
                { assignment_section(context) }
                { lease_recovery_section(context) }
                { note_section(&context.note_links) }
                { attempts_section(&context.attempts, &context.workspace.timezone) }
                { policy_section(context) }
                if !context.history_segments.is_empty() {
                    <section class="org-context-section" aria-labelledby="org-lineage-title">
                        <div class="org-section-head">
                            <div><p class="org-kicker">{ "Workspace move lineage" }</p><h3 id="org-lineage-title">{ "Lineage event history" }</h3></div>
                            <span>{ format!("{} segment(s)", context.history_segments.len()) }</span>
                        </div>
                        <div class="org-lineage-segments">
                            { for lineage_segments(context).into_iter().enumerate().map(|(index, (workspace_id, events))| html! {
                                <article class="org-lineage-segment">
                                    <h4>{ format!("Segment {} · workspace {}", index + 1, workspace_id) }</h4>
                                    <OrgEventTable
                                        events={events}
                                        workspace_timezone={context.workspace.timezone.clone()}
                                        caption={"Segment events ordered by server sequence, independent of timestamp and ID.".to_owned()}
                                    />
                                </article>
                            }) }
                        </div>
                    </section>
                }
                { current_events_section(events_state, &context.workspace.timezone, event_cursor, previous_disabled, on_previous, on_next) }
            </div>
        },
    }
}

fn structured_error(title: &str, error: &OrgApiError) -> Html {
    html! {
        <div class="org-directory-message is-error" role="alert" data-testid="org-item-error">
            <strong>{ title }</strong>
            <p>{ error.message.clone() }</p>
            <code>{ error.code.clone() }</code>
            if error.retryable { <span>{ "You can retry with Refresh." }</span> }
        </div>
    }
}

fn context_overview(context: &ItemContext) -> Html {
    let item = &context.item;
    html! {
        <section class="org-context-section org-context-overview" aria-labelledby="org-overview-title">
            <div class="org-section-head">
                <div><p class="org-kicker">{ "Canonical context" }</p><h3 id="org-overview-title">{ "Item and revision identity" }</h3></div>
                <span class={classes!("org-state", context.workspace.archived_at.is_some().then_some("is-archived"))}>
                    { if context.workspace.archived_at.is_some() { "Archived workspace" } else { "Active workspace" } }
                </span>
            </div>
            <dl class="org-context-grid">
                { datum("Workspace", format!("{} · {}", context.workspace.display_name, context.workspace.timezone)) }
                { datum("Workspace revision", context.workspace_revision.to_string()) }
                { datum("Document", format!("{} · revision {}", context.document.path, context.document.revision)) }
                { datum("Type / state", format!("{} · {}", item.item_type, item.state.as_deref().unwrap_or("Unstated"))) }
                { datum("Priority", item.priority.map_or_else(|| "None".to_owned(), |value| value.to_string())) }
                { datum("Tags", if item.tags.is_empty() { "None".to_owned() } else { item.tags.join(", ") }) }
                { datum("Origin", origin_summary(context.origin.as_ref())) }
            </dl>
        </section>
    }
}

fn origin_summary(origin: Option<&Origin>) -> String {
    match origin {
        Some(Origin::WorkItem { work_item_id, item }) => format!(
            "work item · {}{}",
            work_item_id,
            if item.is_some() {
                " · available"
            } else {
                " · unavailable"
            }
        ),
        Some(Origin::Event { event_id, event }) => format!(
            "event · {}{}",
            event_id,
            if event.is_some() {
                " · available"
            } else {
                " · unavailable"
            }
        ),
        None => "None".to_owned(),
    }
}

fn hierarchy_section(context: &ItemContext, return_state: &WorkspaceQueryState) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-hierarchy-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Hierarchy" }</p><h3 id="org-hierarchy-title">{ "Parent and children" }</h3></div></div>
            <div class="org-relationship-grid">
                <div><h4>{ "Parent" }</h4>{ optional_item(context.parent.as_ref(), &context.workspace.id, return_state, "No parent") }</div>
                <div><h4>{ "Children" }</h4>
                    if context.children.is_empty() { <p class="org-empty-inline">{ "No children" }</p> }
                    else { <ul class="org-relationship-list">{ for context.children.iter().map(|item| linked_item(item, &context.workspace.id, return_state)) }</ul> }
                </div>
            </div>
        </section>
    }
}

fn dependency_section(context: &ItemContext, return_state: &WorkspaceQueryState) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-dependencies-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Eligibility inputs" }</p><h3 id="org-dependencies-title">{ "Dependencies" }</h3></div><span>{ format!("{} blocker(s)", context.operational.blockers.len()) }</span></div>
            if context.dependencies.is_empty() { <p class="org-empty-inline">{ "No dependencies" }</p> }
            else { <ul class="org-relationship-list">{ for context.dependencies.iter().map(|dependency| html! {
                <li class={classes!(dependency.satisfied.then_some("is-satisfied"))}>
                    { linked_item_content(&dependency.item, &context.workspace.id, return_state) }
                    <span>{ if dependency.satisfied { "Satisfied" } else { "Blocking" } }</span>
                </li>
            }) }</ul> }
            if !context.operational.blockers.is_empty() { <p class="org-inline-ledger">{ format!("Server blockers · {}", context.operational.blockers.join(", ")) }</p> }
        </section>
    }
}

fn assignment_section(context: &ItemContext) -> Html {
    let item = &context.item;
    html! {
        <section class="org-context-section" aria-labelledby="org-assignment-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Execution envelope" }</p><h3 id="org-assignment-title">{ "Assignment and schedule" }</h3></div></div>
            <dl class="org-context-grid">
                { datum("Assignee", item.assignee.clone().unwrap_or_else(|| "Unassigned".to_owned())) }
                { datum("Review", if item.requires_review { "Required" } else { "Not required" }.to_owned()) }
                { time_datum("Scheduled", item.scheduled.as_ref()) }
                { time_datum("Deadline", item.deadline.as_ref()) }
                { datum("Created", formatted_epoch(item.created_at, &context.workspace.timezone)) }
            </dl>
        </section>
    }
}

fn lease_recovery_section(context: &ItemContext) -> Html {
    let budget = &context.operational.attempt_budget;
    let recovery = &context.operational.recovery;
    html! {
        <section class="org-context-section" aria-labelledby="org-recovery-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Server-evaluated safety" }</p><h3 id="org-recovery-title">{ "Lease and recovery" }</h3></div>
                if recovery.candidate { <span class="org-recovery-marker">{ "Recovery candidate" }</span> }
            </div>
            <dl class="org-context-grid">
                { datum("Classifications", context.operational.classifications.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")) }
                { datum("Attempt budget", format!("{} used · {} remaining · {} max", budget.execution_attempt_count, budget.remaining_attempts, budget.max_attempts)) }
                { datum("Recovery", format!("eligible {} · candidate {}", recovery.eligible, recovery.candidate)) }
                { datum("Recovery blockers", if recovery.blockers.is_empty() { "None".to_owned() } else { recovery.blockers.join(", ") }) }
            </dl>
            if let Some(lease) = &context.lease {
                <div class="org-lease-ledger">
                    <strong>{ format!("{} lease · {}", lease.kind, lease.status) }</strong>
                    <span>{ format!("Actor · {}", lease.actor_id) }</span>
                    <span>{ format!("Acquired · {}", formatted_epoch(lease.acquired_at, &context.workspace.timezone)) }</span>
                    <span>{ format!("Heartbeat · {}", formatted_epoch(lease.last_heartbeat_at, &context.workspace.timezone)) }</span>
                    <span>{ format!("Expires · {}", formatted_epoch(lease.expires_at, &context.workspace.timezone)) }</span>
                </div>
            } else { <p class="org-empty-inline">{ "No current lease" }</p> }
        </section>
    }
}

fn note_section(notes: &[NoteLink]) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-notes-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Knowledge references" }</p><h3 id="org-notes-title">{ "Linked notes" }</h3></div></div>
            if notes.is_empty() { <p class="org-empty-inline">{ "No linked notes" }</p> }
            else { <ul class="org-note-links">{ for notes.iter().map(|note| html! {
                <li class={classes!((!note.available).then_some("is-unavailable"))}>
                    <div><strong>{ note.purpose.clone() }</strong><span>{ note.description.clone() }</span><code>{ note.note_id.clone() }</code></div>
                    if note.available {
                        <Link<Route> to={Route::NoteShow { id: note.note_id.clone() }}>{ "Open note" }</Link<Route>>
                    } else { <span class="org-unavailable">{ "Unavailable" }</span> }
                </li>
            }) }</ul> }
        </section>
    }
}

fn attempts_section(attempts: &[Attempt], timezone: &str) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-attempts-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Execution record" }</p><h3 id="org-attempts-title">{ "Attempts, results, reviews and artifacts" }</h3></div><span>{ format!("{} attempt(s)", attempts.len()) }</span></div>
            if attempts.is_empty() { <p class="org-empty-inline">{ "No attempts recorded" }</p> }
            else { <div class="org-attempts">{ for attempts.iter().map(|attempt| attempt_card(attempt, timezone)) }</div> }
        </section>
    }
}

fn attempt_card(attempt: &Attempt, timezone: &str) -> Html {
    html! {
        <article class="org-attempt">
            <header><strong>{ format!("Attempt {} · {}", attempt.attempt_number, attempt.status) }</strong><code>{ attempt.id.clone() }</code></header>
            <dl class="org-context-grid">
                { datum("Actor", attempt.actor_id.clone()) }
                { datum("Started", formatted_epoch(attempt.started_at, timezone)) }
                { datum("Ended", attempt.ended_at.map_or_else(|| "Open".to_owned(), |value| formatted_epoch(value, timezone))) }
                { datum("Result", attempt.result_summary.clone().unwrap_or_else(|| "No result".to_owned())) }
                { datum("Review", attempt.review_outcome.clone().unwrap_or_else(|| "Not reviewed".to_owned())) }
                { datum("Error", attempt.error.clone().unwrap_or_else(|| "None".to_owned())) }
            </dl>
            if !attempt.note_refs.is_empty() {
                <p class="org-inline-ledger">{ format!("Attempt notes · {}", attempt.note_refs.iter().map(|note| format!("{} ({})", note.note_id, note.purpose)).collect::<Vec<_>>().join(", ")) }</p>
            }
            if !attempt.artifacts.is_empty() {
                <ul class="org-artifacts">{ for attempt.artifacts.iter().map(|artifact| html! {
                    <li><strong>{ artifact.name.clone() }</strong><span>{ format!("{} · {}", artifact.media_type, artifact.description) }</span><code>{ artifact.uri.clone() }</code></li>
                }) }</ul>
            }
            <details class="org-event-metadata"><summary>{ "Inspect attempt metadata" }</summary><pre>{ safe_json_text(&attempt.metadata) }</pre></details>
        </article>
    }
}

fn policy_section(context: &ItemContext) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-policy-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Workspace policy" }</p><h3 id="org-policy-title">{ "Policy summary" }</h3></div><span>{ format!("schema {}", context.workspace.policy_schema_version) }</span></div>
            <details class="org-event-metadata"><summary>{ "Inspect policy" }</summary><pre>{ safe_json_text(&context.workspace.policy) }</pre></details>
        </section>
    }
}

fn current_events_section(
    state: &EventsPageState,
    timezone: &str,
    cursor: Option<&str>,
    previous_disabled: bool,
    on_previous: Callback<MouseEvent>,
    on_next: Callback<MouseEvent>,
) -> Html {
    html! {
        <section class="org-context-section" aria-labelledby="org-events-title">
            <div class="org-section-head"><div><p class="org-kicker">{ "Current workspace audit" }</p><h3 id="org-events-title">{ "Event history" }</h3></div><span>{ cursor.map_or("First page".to_owned(), |value| format!("Cursor · {value}")) }</span></div>
            { match &state.load {
                EventsLoad::Loading => html! { <p class="loading" data-testid="org-events-loading">{ "Loading event history..." }</p> },
                EventsLoad::Failed(error) => structured_error("Event history unavailable", error),
                EventsLoad::Ready(page) if page.items.is_empty() => html! { <p class="org-empty-inline" data-testid="org-events-empty">{ "No events on this page" }</p> },
                EventsLoad::Ready(page) => html! {
                    <div class="org-events-ready" data-testid="org-events-ready">
                        <OrgEventTable events={page.items.clone()} workspace_timezone={timezone.to_owned()} />
                    </div>
                },
            } }
            if let EventsLoad::Ready(page) = &state.load {
                <nav class="org-cursor-nav" aria-label="Event history pages">
                    <span>{ format!("{} events · subject work_item", page.items.len()) }</span>
                    <button type="button" class="btn btn-outline" onclick={on_previous} disabled={previous_disabled}>{ "Previous events" }</button>
                    <button type="button" class="btn btn-outline" onclick={on_next} disabled={page.next_cursor.is_none()}>{ "Next events" }</button>
                </nav>
            }
        </section>
    }
}

fn datum(label: &'static str, value: String) -> Html {
    html! { <div><dt>{ label }</dt><dd>{ value }</dd></div> }
}

fn time_datum(label: &'static str, value: Option<&crate::org::model::OrgTimestamp>) -> Html {
    let display = value.map(format_org_timestamp);
    let workspace = display
        .as_ref()
        .map_or(UNAVAILABLE_TIME, |value| value.workspace.as_str());
    let local = display
        .as_ref()
        .map_or(UNAVAILABLE_TIME, |value| value.browser_local.as_str());
    datum(label, format!("{workspace} · browser local {local}"))
}

fn formatted_epoch(value: i64, timezone: &str) -> String {
    let display = format_timestamp(value, timezone);
    format!(
        "{} · browser local {}",
        display.workspace, display.browser_local
    )
}

fn optional_item(
    item: Option<&WorkItem>,
    workspace_id: &str,
    state: &WorkspaceQueryState,
    empty: &str,
) -> Html {
    item.map_or_else(|| html! { <p class="org-empty-inline">{ empty.to_owned() }</p> }, |item| html! { <ul class="org-relationship-list">{ linked_item(item, workspace_id, state) }</ul> })
}

fn linked_item(item: &WorkItem, workspace_id: &str, state: &WorkspaceQueryState) -> Html {
    html! { <li>{ linked_item_content(item, workspace_id, state) }</li> }
}

fn linked_item_content(item: &WorkItem, workspace_id: &str, state: &WorkspaceQueryState) -> Html {
    let href = item_path(
        workspace_id,
        &item.id,
        &OrgReturnContext::new(workspace_id, state.clone()),
    );
    html! { <a {href}><strong>{ item.title.clone() }</strong><code>{ item.id.clone() }</code></a> }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::model::{
        AttemptBudget, Dependency, Document, HistorySegment, Lease, NoteLink, OperationalContext,
        OperationalView, OrgTimestamp, RecoveryStatus, Workspace,
    };
    use serde_json::json;
    use yew::Reducible;

    fn error(status: u16) -> OrgApiError {
        OrgApiError {
            code: "safe_error".into(),
            message: "Safe message".into(),
            details: json!({}),
            retryable: true,
            status: Some(status),
        }
    }

    fn work_item(id: &str, parent_id: Option<&str>) -> WorkItem {
        WorkItem {
            id: id.into(),
            workspace_id: "workspace-a".into(),
            document_id: "document-a".into(),
            parent_id: parent_id.map(str::to_owned),
            item_type: "task".into(),
            title: format!("Item {id}"),
            state: Some("RUNNING".into()),
            priority: Some('A'),
            scheduled: Some(OrgTimestamp {
                raw: "<2026-08-05 Wed 09:00>".into(),
                local: "2026-08-05T09:00:00".into(),
                timezone: "Asia/Shanghai".into(),
                utc_timestamp: 1_775_008_800,
            }),
            deadline: None,
            assignee: Some("agent-a".into()),
            requires_review: true,
            created_at: 1,
            tags: vec!["ops".into()],
        }
    }

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
            summary: "Progress".into(),
            metadata: json!({"safe":true}),
            previous_state: Some("RUNNING".into()),
            resulting_state: Some("RUNNING".into()),
        }
    }

    fn context() -> ItemContext {
        let item = work_item("item-a", Some("parent-a"));
        ItemContext {
            workspace: Workspace {
                id: "workspace-a".into(),
                slug: "ops".into(),
                display_name: "Operations".into(),
                description: "Delivery".into(),
                timezone: "Asia/Shanghai".into(),
                policy_schema_version: 2,
                policy: crate::org::workspace_management::WorkspacePolicy::engineering_default(),
                revision: 9,
                archived_at: None,
            },
            workspace_revision: 9,
            document: Document {
                id: "document-a".into(),
                path: "ops.org".into(),
                revision: 4,
            },
            item,
            parent: Some(work_item("parent-a", None)),
            children: vec![work_item("child-a", Some("item-a"))],
            dependencies: vec![Dependency {
                item: work_item("dependency-a", None),
                satisfied: false,
            }],
            note_links: vec![
                NoteLink {
                    purpose: "context".into(),
                    note_id: "available-note".into(),
                    description: "Runbook".into(),
                    available: true,
                },
                NoteLink {
                    purpose: "evidence".into(),
                    note_id: "missing-note".into(),
                    description: "Deleted log".into(),
                    available: false,
                },
            ],
            attempts: vec![Attempt {
                id: "attempt-a".into(),
                workspace_id: "workspace-a".into(),
                work_item_id: "item-a".into(),
                attempt_number: 1,
                actor_id: "agent-a".into(),
                status: "running".into(),
                started_at: 2,
                ended_at: None,
                error: None,
                result_summary: Some("Built".into()),
                review_outcome: Some("approved".into()),
                note_refs: vec![],
                artifacts: vec![crate::org::model::Artifact {
                    uri: "artifact://build".into(),
                    media_type: "text/plain".into(),
                    name: "log".into(),
                    description: "Build log".into(),
                }],
                metadata: json!({"progress":50,"fencing_token":"hidden"}),
            }],
            origin: Some(Origin::WorkItem {
                work_item_id: "origin-a".into(),
                item: None,
            }),
            history_segments: vec![
                HistorySegment {
                    workspace_id: "workspace-b".into(),
                    events: vec![
                        event("0000", "workspace-b", 2, 1),
                        event("ffff", "workspace-b", 1, 999),
                    ],
                },
                HistorySegment {
                    workspace_id: "workspace-a".into(),
                    events: vec![
                        event("1111", "workspace-a", 2, 1),
                        event("eeee", "workspace-a", 1, 999),
                    ],
                },
            ],
            lease: Some(Lease {
                id: "lease-a".into(),
                workspace_id: "workspace-a".into(),
                work_item_id: "item-a".into(),
                attempt_id: "attempt-a".into(),
                kind: "execution".into(),
                actor_id: "agent-a".into(),
                acquired_at: 2,
                last_heartbeat_at: 3,
                expires_at: 4,
                status: "active".into(),
            }),
            operational: OperationalContext {
                classifications: vec![OperationalView::Running, OperationalView::ExpiredLease],
                readiness: None,
                blockers: vec!["dependency".into()],
                attempt_budget: AttemptBudget {
                    execution_attempt_count: 1,
                    max_attempts: 3,
                    remaining_attempts: 2,
                    retry_exhausted: false,
                },
                recovery: RecoveryStatus {
                    eligible: true,
                    candidate: true,
                    blockers: vec![],
                },
            },
        }
    }

    #[test]
    fn typed_return_restores_exact_list_state_and_falls_back_to_ready() {
        let query = "return_view=failed&return_state=FAILED&return_priority=A&return_tags=ops%2Cdelivery&return_assignee=agent-a&return_from=2026-08-05T01%3A00%3A00Z&return_to=2026-08-06T01%3A00%3A00Z&return_cursor=page%2F2&return_limit=25";
        let state = validated_return("workspace-a", query);
        assert_eq!(state.view, OperationalView::Failed);
        assert_eq!(state.cursor.as_deref(), Some("page/2"));
        assert_eq!(
            return_href("workspace-a", query),
            workspace_path("workspace-a", &state)
        );
        assert_eq!(
            validated_return("workspace-a", "return_item_type=wizard&return_limit=25"),
            WorkspaceQueryState::default()
        );
        let cross = OrgReturnContext::new("workspace-b", state);
        assert_eq!(
            cross.validated("workspace-a"),
            WorkspaceQueryState::default()
        );
    }

    #[test]
    fn state_machine_covers_loading_ready_missing_error_and_ignores_stale_results() {
        let initial = Rc::new(ContextPageState::default());
        assert_eq!(item_render_state(&initial), ItemRenderState::Loading);
        let ready = initial
            .reduce(ContextAction::Loading { generation: 2 })
            .reduce(ContextAction::Loaded {
                generation: 2,
                context: Box::new(context()),
            });
        assert_eq!(item_render_state(&ready), ItemRenderState::Ready);
        let stale = ready.clone().reduce(ContextAction::Failed {
            generation: 1,
            error: error(500),
        });
        assert_eq!(stale, ready);
        let missing = ready
            .clone()
            .reduce(ContextAction::Loading { generation: 3 })
            .reduce(ContextAction::Failed {
                generation: 3,
                error: error(404),
            });
        assert_eq!(item_render_state(&missing), ItemRenderState::Missing);
        let failed = missing
            .reduce(ContextAction::Loading { generation: 4 })
            .reduce(ContextAction::Failed {
                generation: 4,
                error: error(503),
            });
        assert_eq!(item_render_state(&failed), ItemRenderState::Error);
    }

    #[test]
    fn context_fixture_covers_hierarchy_dependencies_policy_revisions_execution_and_availability() {
        let context = context();
        assert!(context.parent.is_some());
        assert_eq!(context.children.len(), 1);
        assert!(!context.dependencies[0].satisfied);
        assert_eq!(context.workspace.policy_schema_version, 2);
        assert_eq!(context.workspace_revision, 9);
        assert_eq!(context.document.revision, 4);
        assert_eq!(context.attempts[0].result_summary.as_deref(), Some("Built"));
        assert_eq!(
            context.attempts[0].review_outcome.as_deref(),
            Some("approved")
        );
        assert_eq!(context.attempts[0].artifacts.len(), 1);
        assert_eq!(
            context
                .note_links
                .iter()
                .map(|note| note.available)
                .collect::<Vec<_>>(),
            vec![true, false]
        );
        let lease = context.lease.as_ref().unwrap();
        assert_eq!(
            (
                &lease.kind,
                &lease.actor_id,
                lease.acquired_at,
                lease.last_heartbeat_at,
                lease.expires_at,
                &lease.status
            ),
            (
                &"execution".into(),
                &"agent-a".into(),
                2,
                3,
                4,
                &"active".into()
            )
        );
        assert!(context.operational.recovery.candidate);
        assert!(!safe_json_text(&context.attempts[0].metadata).contains("fencing_token"));
        let started = formatted_epoch(context.attempts[0].started_at, &context.workspace.timezone);
        assert!(started.contains("1970-01-01"));
        assert!(started.contains("browser local"));
    }

    #[test]
    fn lineage_and_event_pages_are_sequence_led_and_subject_filtered() {
        let lineage = lineage_segments(&context());
        assert_eq!(
            lineage
                .iter()
                .map(|(workspace, _)| workspace.as_str())
                .collect::<Vec<_>>(),
            vec!["workspace-b", "workspace-a"]
        );
        assert_eq!(
            lineage
                .iter()
                .map(|(_, events)| events
                    .iter()
                    .map(|event| event.sequence)
                    .collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            vec![vec![1, 2], vec![1, 2]]
        );
        assert_eq!(lineage[0].1[0].id, "ffff");
        assert_eq!(lineage[1].1[0].id, "eeee");
        let url = org_api::item_events_url("workspace/a", "item/a", Some("opaque/+"), EVENTS_LIMIT);
        assert!(url.contains("subject_kind=work_item"));
        assert!(url.contains("subject_id=item%2Fa"));
        assert!(url.contains("cursor=opaque%2F%2B"));
        assert!(url.contains("limit=50"));
    }

    #[test]
    fn item_page_source_has_manual_refresh_but_no_auth_mutation_or_polling() {
        let source = include_str!("org_item.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("Refresh context and events"));
        assert!(source.contains("push_with_query"));
        for forbidden in [
            "Request::post",
            "Request::patch",
            "Request::delete",
            "set_interval",
            "gloo_timers",
            "fencing_token",
            "login",
            "session",
        ] {
            assert!(!source.contains(forbidden), "unexpected {forbidden}");
        }
    }
}
