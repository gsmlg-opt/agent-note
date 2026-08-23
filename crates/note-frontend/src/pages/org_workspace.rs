use std::{collections::BTreeSet, rc::Rc};

use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::{prelude::*, query::Raw};

use crate::{
    components::{org_table::OrgTable, Modal},
    org::{
        api::{self as org_api, OrgApiError},
        model::{
            OperationalCounts, OperationalPage, OperationalView, Page, Workspace, WorkspaceSummary,
        },
        mutation::MutationSubmission,
        url::{
            PriorityFilter, WorkspaceFilters, WorkspaceListState, WorkspaceQueryState,
            ALLOWED_LIMITS,
        },
        workspace_management::ArchiveWorkspaceBody,
    },
    routes::Route,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestFamily {
    Queue,
    Agenda,
}

fn request_family(view: OperationalView) -> RequestFamily {
    if view.is_agenda() {
        RequestFamily::Agenda
    } else {
        RequestFamily::Queue
    }
}

#[derive(Clone, Copy)]
struct ViewSpec {
    view: OperationalView,
    label: &'static str,
}

const VIEW_SPECS: [ViewSpec; 10] = [
    ViewSpec {
        view: OperationalView::Ready,
        label: "Ready",
    },
    ViewSpec {
        view: OperationalView::Assigned,
        label: "Assigned",
    },
    ViewSpec {
        view: OperationalView::Running,
        label: "Running",
    },
    ViewSpec {
        view: OperationalView::Blocked,
        label: "Blocked",
    },
    ViewSpec {
        view: OperationalView::Review,
        label: "Review",
    },
    ViewSpec {
        view: OperationalView::Scheduled,
        label: "Scheduled",
    },
    ViewSpec {
        view: OperationalView::UpcomingDeadline,
        label: "Due soon",
    },
    ViewSpec {
        view: OperationalView::Failed,
        label: "Failed",
    },
    ViewSpec {
        view: OperationalView::ExpiredLease,
        label: "Expired lease",
    },
    ViewSpec {
        view: OperationalView::Completed,
        label: "Completed",
    },
];

fn count_for(counts: &OperationalCounts, view: OperationalView) -> i64 {
    match view {
        OperationalView::Ready => counts.ready,
        OperationalView::Assigned => counts.assigned,
        OperationalView::Running => counts.running,
        OperationalView::Blocked => counts.blocked,
        OperationalView::Review => counts.review,
        OperationalView::Scheduled => counts.scheduled,
        OperationalView::UpcomingDeadline => counts.upcoming_deadline,
        OperationalView::Failed => counts.failed,
        OperationalView::ExpiredLease => counts.expired_lease,
        OperationalView::Completed => counts.completed,
    }
}

fn raw_query(state: &WorkspaceQueryState) -> Raw<String> {
    Raw(state.canonical_query())
}

fn archive_confirmation_matches(confirmation: &str, workspace_slug: &str) -> bool {
    confirmation == workspace_slug
}

fn submit_archive(
    workspace_id: String,
    body: ArchiveWorkspaceBody,
    busy: UseStateHandle<bool>,
    error: UseStateHandle<Option<OrgApiError>>,
    navigator: Option<Navigator>,
) {
    busy.set(true);
    error.set(None);
    wasm_bindgen_futures::spawn_local(async move {
        match org_api::archive_workspace(&workspace_id, &body).await {
            Ok(_) => {
                busy.set(false);
                if let Some(navigator) = navigator {
                    let destination = WorkspaceListState {
                        include_archived: true,
                        cursor: None,
                        limit: 50,
                    };
                    let _ =
                        navigator.push_with_query(&Route::Org, Raw(destination.canonical_query()));
                }
            }
            Err(next_error) => {
                busy.set(false);
                error.set(Some(next_error));
            }
        }
    });
}

fn view_state(current: &WorkspaceQueryState, view: OperationalView) -> WorkspaceQueryState {
    let mut next = current.clone();
    next.set_view(view);
    next
}

fn next_page_state(current: &WorkspaceQueryState, cursor: &str) -> WorkspaceQueryState {
    let mut next = current.clone();
    next.cursor = Some(cursor.to_owned());
    next
}

fn record_next_page(
    history: &mut Vec<WorkspaceQueryState>,
    current: &WorkspaceQueryState,
    cursor: &str,
) -> WorkspaceQueryState {
    history.push(current.clone());
    next_page_state(current, cursor)
}

fn take_previous_page(history: &mut Vec<WorkspaceQueryState>) -> Option<WorkspaceQueryState> {
    history.pop()
}

fn reconcile_browser_back(history: &mut Vec<WorkspaceQueryState>, current: &WorkspaceQueryState) {
    if history.last() == Some(current) {
        history.pop();
    }
}

fn pagination_scope(state: &WorkspaceQueryState) -> String {
    let mut scope = state.clone();
    scope.cursor = None;
    scope.canonical_query()
}

fn operational_request_url(
    workspace_id: &str,
    state: &WorkspaceQueryState,
    include_archived: bool,
) -> String {
    let family = match request_family(state.view) {
        RequestFamily::Queue => "queue",
        RequestFamily::Agenda => "agenda",
    };
    org_api::operational_url(family, workspace_id, state, include_archived)
}

async fn load_operational_page(
    workspace_id: &str,
    state: &WorkspaceQueryState,
    include_archived: bool,
) -> Result<OperationalPage, OrgApiError> {
    match request_family(state.view) {
        RequestFamily::Queue => org_api::query_queue(workspace_id, state, include_archived).await,
        RequestFamily::Agenda => org_api::query_agenda(workspace_id, state, include_archived).await,
    }
}

#[derive(Clone, Debug, PartialEq)]
enum SummaryLookup {
    Found(WorkspaceSummary),
    Continue(String),
    Missing,
}

fn summary_lookup(workspace_id: &str, page: Page<WorkspaceSummary>) -> SummaryLookup {
    if let Some(summary) = page
        .items
        .into_iter()
        .find(|summary| summary.workspace_id == workspace_id)
    {
        SummaryLookup::Found(summary)
    } else if let Some(cursor) = page.next_cursor {
        SummaryLookup::Continue(cursor)
    } else {
        SummaryLookup::Missing
    }
}

async fn load_workspace_summary(
    workspace_id: &str,
) -> Result<Option<WorkspaceSummary>, OrgApiError> {
    let mut cursor = None;
    let mut seen_cursors = BTreeSet::new();
    loop {
        let page = org_api::list_workspaces(&WorkspaceListState {
            include_archived: true,
            cursor: cursor.clone(),
            limit: 200,
        })
        .await?;
        match summary_lookup(workspace_id, page) {
            SummaryLookup::Found(summary) => return Ok(Some(summary)),
            SummaryLookup::Continue(next_cursor) if seen_cursors.insert(next_cursor.clone()) => {
                cursor = Some(next_cursor);
            }
            SummaryLookup::Continue(_) | SummaryLookup::Missing => return Ok(None),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspacePayload {
    workspace: Workspace,
    page: OperationalPage,
}

#[derive(Clone, Debug, PartialEq)]
enum WorkspaceLoad {
    Loading,
    Ready(WorkspacePayload),
    Failed(OrgApiError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkspaceRenderState {
    Loading,
    Table,
    Empty,
    Error,
    CursorEnd,
}

#[derive(Clone, Debug, PartialEq)]
struct WorkspacePageState {
    generation: u64,
    load: WorkspaceLoad,
}

impl Default for WorkspacePageState {
    fn default() -> Self {
        Self {
            generation: 0,
            load: WorkspaceLoad::Loading,
        }
    }
}

enum WorkspaceAction {
    Loading {
        generation: u64,
    },
    Loaded {
        generation: u64,
        payload: WorkspacePayload,
    },
    Failed {
        generation: u64,
        error: OrgApiError,
    },
}

impl Reducible for WorkspacePageState {
    type Action = WorkspaceAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            WorkspaceAction::Loading { generation } if generation >= self.generation => Self {
                generation,
                load: WorkspaceLoad::Loading,
            }
            .into(),
            WorkspaceAction::Loaded {
                generation,
                payload,
            } if generation == self.generation => Self {
                generation,
                load: WorkspaceLoad::Ready(payload),
            }
            .into(),
            WorkspaceAction::Failed { generation, error } if generation == self.generation => {
                Self {
                    generation,
                    load: WorkspaceLoad::Failed(error),
                }
                .into()
            }
            _ => self,
        }
    }
}

impl WorkspacePageState {
    fn render_state(&self, has_cursor: bool) -> WorkspaceRenderState {
        match &self.load {
            WorkspaceLoad::Loading => WorkspaceRenderState::Loading,
            WorkspaceLoad::Failed(_) => WorkspaceRenderState::Error,
            WorkspaceLoad::Ready(payload) if has_cursor && payload.page.next_cursor.is_none() => {
                WorkspaceRenderState::CursorEnd
            }
            WorkspaceLoad::Ready(payload) if payload.page.items.is_empty() => {
                WorkspaceRenderState::Empty
            }
            WorkspaceLoad::Ready(_) => WorkspaceRenderState::Table,
        }
    }

    fn payload(&self) -> Option<&WorkspacePayload> {
        match &self.load {
            WorkspaceLoad::Ready(payload) => Some(payload),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgWorkspacePageProps {
    pub workspace_id: String,
}

#[function_component(OrgWorkspacePage)]
pub fn org_workspace_page(props: &OrgWorkspacePageProps) -> Html {
    let page_state = use_reducer(WorkspacePageState::default);
    let refresh_tick = use_state(|| 0_u64);
    let pagination_history = use_state(Vec::<WorkspaceQueryState>::new);
    let workspace_summary = use_state(|| None::<WorkspaceSummary>);
    let request_generation = use_mut_ref(|| 0_u64);
    let summary_generation = use_mut_ref(|| 0_u64);
    let archive_open = use_state(|| false);
    let archive_confirmation = use_state(String::new);
    let archive_busy = use_state(|| false);
    let archive_error = use_state(|| None::<OrgApiError>);
    let pending_archive = use_state(|| None::<ArchiveWorkspaceBody>);
    let navigator = use_navigator();
    let location = use_location();
    let raw_location_query = location
        .as_ref()
        .map(|location| location.query_str().trim_start_matches('?').to_owned())
        .unwrap_or_default();
    let query_state = WorkspaceQueryState::parse(&raw_location_query);
    let canonical_query = query_state.canonical_query();
    let is_canonical = raw_location_query == canonical_query;
    let route = Route::OrgWorkspace {
        workspace_id: props.workspace_id.clone(),
    };

    {
        let pagination_history = pagination_history.clone();
        use_effect_with(pagination_scope(&query_state), move |_| {
            pagination_history.set(Vec::new());
            || ()
        });
    }

    {
        let pagination_history = pagination_history.clone();
        let query_state = query_state.clone();
        use_effect_with(canonical_query.clone(), move |_| {
            let mut history = (*pagination_history).clone();
            let before = history.len();
            reconcile_browser_back(&mut history, &query_state);
            if history.len() != before {
                pagination_history.set(history);
            }
            || ()
        });
    }

    {
        let navigator = navigator.clone();
        let route = route.clone();
        let query_state = query_state.clone();
        use_effect_with(
            (raw_location_query.clone(), canonical_query.clone()),
            move |(raw, canonical)| {
                if raw != canonical {
                    if let Some(navigator) = &navigator {
                        let _ = navigator.replace_with_query(&route, raw_query(&query_state));
                    }
                }
                || ()
            },
        );
    }

    {
        let workspace_summary = workspace_summary.clone();
        let summary_generation = summary_generation.clone();
        let workspace_id = props.workspace_id.clone();
        use_effect_with((props.workspace_id.clone(), *refresh_tick), move |_| {
            let generation = {
                let mut generation = summary_generation.borrow_mut();
                *generation = generation.saturating_add(1);
                *generation
            };
            workspace_summary.set(None);
            let workspace_summary = workspace_summary.clone();
            let summary_generation = summary_generation.clone();
            wasm_bindgen_futures::spawn_local(async move {
                if let Ok(Some(summary)) = load_workspace_summary(&workspace_id).await {
                    if *summary_generation.borrow() == generation {
                        workspace_summary.set(Some(summary));
                    }
                }
            });
            || ()
        });
    }

    {
        let page_state = page_state.clone();
        let request_generation = request_generation.clone();
        let workspace_id = props.workspace_id.clone();
        let query_state = query_state.clone();
        use_effect_with(
            (
                props.workspace_id.clone(),
                canonical_query.clone(),
                *refresh_tick,
                is_canonical,
            ),
            move |(_, _, _, is_canonical)| {
                let generation = {
                    let mut generation = request_generation.borrow_mut();
                    *generation = generation.saturating_add(1);
                    *generation
                };
                page_state.dispatch(WorkspaceAction::Loading { generation });
                if *is_canonical {
                    let page_state = page_state.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = async {
                            let workspace = org_api::get_workspace(&workspace_id).await?;
                            let include_archived = workspace.archived_at.is_some();
                            let page = load_operational_page(
                                &workspace_id,
                                &query_state,
                                include_archived,
                            )
                            .await?;
                            Ok::<_, OrgApiError>(WorkspacePayload { workspace, page })
                        }
                        .await;
                        match result {
                            Ok(payload) => page_state.dispatch(WorkspaceAction::Loaded {
                                generation,
                                payload,
                            }),
                            Err(error) => {
                                page_state.dispatch(WorkspaceAction::Failed { generation, error })
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    let item_type_ref = use_node_ref();
    let state_ref = use_node_ref();
    let priority_ref = use_node_ref();
    let tags_ref = use_node_ref();
    let assignee_ref = use_node_ref();
    let from_ref = use_node_ref();
    let to_ref = use_node_ref();

    let on_filter_submit = {
        let navigator = navigator.clone();
        let route = route.clone();
        let query_state = query_state.clone();
        let item_type_ref = item_type_ref.clone();
        let state_ref = state_ref.clone();
        let priority_ref = priority_ref.clone();
        let tags_ref = tags_ref.clone();
        let assignee_ref = assignee_ref.clone();
        let from_ref = from_ref.clone();
        let to_ref = to_ref.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            let value = |reference: &NodeRef| {
                reference
                    .cast::<HtmlInputElement>()
                    .map(|input| input.value())
            };
            let priority = priority_ref
                .cast::<HtmlSelectElement>()
                .map(|select| select.value())
                .and_then(|value| match value.as_str() {
                    "" => None,
                    "none" => Some(PriorityFilter::None),
                    _ => value.chars().next().map(PriorityFilter::Priority),
                });
            let mut next = query_state.clone();
            next.replace_filters(WorkspaceFilters {
                item_type: value(&item_type_ref),
                state: value(&state_ref),
                priority,
                tags: value(&tags_ref)
                    .unwrap_or_default()
                    .split(',')
                    .map(str::to_owned)
                    .collect(),
                assignee: value(&assignee_ref),
                from: value(&from_ref),
                to: value(&to_ref),
            });
            if let Some(navigator) = &navigator {
                let _ = navigator.push_with_query(&route, raw_query(&next));
            }
        })
    };

    let on_limit = {
        let navigator = navigator.clone();
        let route = route.clone();
        let query_state = query_state.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            if let Ok(limit) = select.value().parse() {
                let mut next = query_state.clone();
                next.set_limit(limit);
                if let Some(navigator) = &navigator {
                    let _ = navigator.push_with_query(&route, raw_query(&next));
                }
            }
        })
    };

    let on_refresh = {
        let refresh_tick = refresh_tick.clone();
        Callback::from(move |_| refresh_tick.set((*refresh_tick).saturating_add(1)))
    };
    let on_next = {
        let navigator = navigator.clone();
        let route = route.clone();
        let query_state = query_state.clone();
        let pagination_history = pagination_history.clone();
        let cursor = page_state
            .payload()
            .and_then(|payload| payload.page.next_cursor.clone());
        Callback::from(move |_| {
            let Some(cursor) = &cursor else {
                return;
            };
            let mut history = (*pagination_history).clone();
            let next = record_next_page(&mut history, &query_state, cursor);
            pagination_history.set(history);
            if let Some(navigator) = &navigator {
                let _ = navigator.push_with_query(&route, raw_query(&next));
            }
        })
    };
    let on_previous = {
        let navigator = navigator.clone();
        let route = route.clone();
        let pagination_history = pagination_history.clone();
        Callback::from(move |_| {
            let mut history = (*pagination_history).clone();
            let previous = take_previous_page(&mut history);
            if let (Some(navigator), Some(previous)) = (&navigator, previous) {
                pagination_history.set(history);
                let _ = navigator.push_with_query(&route, raw_query(&previous));
            }
        })
    };
    let on_archive_open = {
        let archive_open = archive_open.clone();
        let archive_confirmation = archive_confirmation.clone();
        let archive_error = archive_error.clone();
        let pending_archive = pending_archive.clone();
        Callback::from(move |_| {
            archive_confirmation.set(String::new());
            archive_error.set(None);
            pending_archive.set(None);
            archive_open.set(true);
        })
    };
    let on_archive_close = {
        let archive_open = archive_open.clone();
        let archive_busy = archive_busy.clone();
        Callback::from(move |(): ()| {
            if !*archive_busy {
                archive_open.set(false);
            }
        })
    };
    let on_archive_confirmation = {
        let archive_confirmation = archive_confirmation.clone();
        let archive_error = archive_error.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            archive_confirmation.set(input.value());
            archive_error.set(None);
        })
    };
    let archive_workspace = page_state
        .payload()
        .map(|payload| payload.workspace.clone());
    let archive_can_submit = archive_workspace.as_ref().is_some_and(|workspace| {
        workspace.archived_at.is_none()
            && archive_confirmation_matches(&archive_confirmation, &workspace.slug)
    });
    let on_archive_submit = {
        let archive_workspace = archive_workspace.clone();
        let archive_confirmation = archive_confirmation.clone();
        let archive_busy = archive_busy.clone();
        let archive_error = archive_error.clone();
        let pending_archive = pending_archive.clone();
        let navigator = navigator.clone();
        Callback::from(move |_| {
            if *archive_busy {
                return;
            }
            let Some(workspace) = &archive_workspace else {
                return;
            };
            if !archive_confirmation_matches(&archive_confirmation, &workspace.slug) {
                return;
            }
            let body = ArchiveWorkspaceBody::new(
                MutationSubmission::new().operation_id,
                workspace.revision,
            );
            pending_archive.set(Some(body.clone()));
            submit_archive(
                workspace.id.clone(),
                body,
                archive_busy.clone(),
                archive_error.clone(),
                navigator.clone(),
            );
        })
    };
    let on_archive_retry = {
        let archive_workspace = archive_workspace.clone();
        let archive_busy = archive_busy.clone();
        let archive_error = archive_error.clone();
        let pending_archive = pending_archive.clone();
        let navigator = navigator.clone();
        Callback::from(move |_| {
            if *archive_busy {
                return;
            }
            if let (Some(workspace), Some(body)) = (&archive_workspace, (*pending_archive).clone())
            {
                submit_archive(
                    workspace.id.clone(),
                    body,
                    archive_busy.clone(),
                    archive_error.clone(),
                    navigator.clone(),
                );
            }
        })
    };

    html! {
        <section
            class="stack org-workspace"
            aria-labelledby="org-workspace-title"
            data-testid="org-workspace-page"
        >
            { workspace_header(page_state.payload(), &query_state, &route, on_refresh, on_archive_open, is_canonical) }
            if *archive_open {
                if let Some(workspace) = archive_workspace {
                    <Modal title="Archive workspace" on_close={on_archive_close.clone()}>
                        <div class="stack org-workspace-archive-dialog" data-testid="org-workspace-archive-dialog">
                            <p>
                                { "Archiving is reversible at the storage level and preserves workspace history, but this UI does not provide restore or hard delete." }
                            </p>
                            <p>{ format!("Type {} to confirm.", workspace.slug) }</p>
                            <label for="org-workspace-archive-confirmation">
                                <span>{ "Workspace slug" }</span>
                                <input
                                    id="org-workspace-archive-confirmation"
                                    class="input"
                                    value={(*archive_confirmation).clone()}
                                    oninput={on_archive_confirmation}
                                    autocomplete="off"
                                    disabled={*archive_busy}
                                    data-testid="org-workspace-archive-confirmation"
                                />
                            </label>
                            if let Some(error) = &*archive_error {
                                <div class="org-workspace-form-errors" role="alert" data-testid="org-workspace-archive-error">
                                    <strong>{ error.message.clone() }</strong>
                                    <code>{ error.code.clone() }</code>
                                    if error.details != serde_json::Value::Null {
                                        <pre>{ serde_json::to_string_pretty(&error.details).unwrap_or_default() }</pre>
                                    }
                                </div>
                            }
                            <div class="org-workspace-form-actions">
                                <button type="button" class="btn btn-outline" onclick={{ let callback = on_archive_close.clone(); Callback::from(move |_| callback.emit(())) }} disabled={*archive_busy}>{ "Cancel" }</button>
                                if archive_error.as_ref().is_some_and(|error| error.retryable) && pending_archive.is_some() {
                                    <button type="button" class="btn btn-outline" onclick={on_archive_retry.clone()} disabled={*archive_busy}>{ "Retry" }</button>
                                }
                                <button type="button" class="btn btn-danger" onclick={on_archive_submit} disabled={!archive_can_submit || *archive_busy} data-testid="org-workspace-archive-submit">
                                    { if *archive_busy { "Archiving…" } else { "Archive workspace" } }
                                </button>
                            </div>
                        </div>
                    </Modal>
                }
            }
            { view_tabs(workspace_summary.as_ref(), &query_state, &route) }
            <form
                key={canonical_query.clone()}
                class="org-triage-filters"
                aria-label="Operational filters"
                onsubmit={on_filter_submit}
                data-testid="org-workspace-filters"
            >
                <label><span>{ "Type" }</span><input ref={item_type_ref} class="input" name="item_type" value={query_state.item_type.clone().unwrap_or_default()} placeholder="task" data-testid="org-filter-type" /></label>
                <label><span>{ "State" }</span><input ref={state_ref} class="input" name="state" value={query_state.state.clone().unwrap_or_default()} placeholder="RUNNING" data-testid="org-filter-state" /></label>
                <label><span>{ "Priority" }</span>
                    <select ref={priority_ref} class="input" name="priority" value={query_state.priority.map(|value| value.as_query_value()).unwrap_or_default()} data-testid="org-filter-priority">
                        <option value="" selected={query_state.priority.is_none()}>{ "Any" }</option>
                        <option value="none" selected={matches!(query_state.priority, Some(PriorityFilter::None))}>{ "None" }</option>
                        { for ('A'..='Z').map(|priority| html! {
                            <option
                                value={priority.to_string()}
                                selected={matches!(query_state.priority, Some(PriorityFilter::Priority(value)) if value == priority)}
                            >{ priority }</option>
                        }) }
                    </select>
                </label>
                <label><span>{ "Tags" }</span><input ref={tags_ref} class="input" name="tags" value={query_state.tags.join(",")} placeholder="ops,delivery" data-testid="org-filter-tags" /></label>
                <label><span>{ "Assignee" }</span><input ref={assignee_ref} class="input" name="assignee" value={query_state.assignee.clone().unwrap_or_default()} placeholder="agent-id" data-testid="org-filter-assignee" /></label>
                <label><span>{ "From (UTC)" }</span><input ref={from_ref} class="input" name="from" value={query_state.from.clone().unwrap_or_default()} placeholder="2026-08-05T01:00:00Z" data-testid="org-filter-from" /></label>
                <label><span>{ "To (UTC)" }</span><input ref={to_ref} class="input" name="to" value={query_state.to.clone().unwrap_or_default()} placeholder="2026-08-06T01:00:00Z" data-testid="org-filter-to" /></label>
                <button type="submit" class="btn btn-outline" data-testid="org-filters-apply">{ "Apply filters" }</button>
            </form>

            <div class="org-live-status" aria-live="polite" aria-atomic="true">
                { live_status(&page_state, query_state.cursor.is_some()) }
            </div>

            { workspace_content(&page_state, &props.workspace_id, &query_state) }

            if let Some(payload) = page_state.payload() {
                <nav class="org-cursor-nav" aria-label="Operational pages">
                    <label class="org-page-size">
                        <span>{ "Rows" }</span>
                        <select class="input" name="limit" value={query_state.limit.to_string()} onchange={on_limit}>
                            { for ALLOWED_LIMITS.iter().map(|limit| html! {
                                <option value={limit.to_string()} selected={*limit == query_state.limit}>{ limit }</option>
                            }) }
                        </select>
                    </label>
                    <span>{ format!("Evaluated at {}", payload.page.evaluated_at) }</span>
                    <button type="button" class="btn btn-outline" onclick={on_previous} disabled={pagination_history.is_empty()}>{ "Previous page" }</button>
                    <button type="button" class="btn btn-outline" onclick={on_next} disabled={payload.page.next_cursor.is_none()}>{ "Next page" }</button>
                </nav>
            }
        </section>
    }
}

fn workspace_header(
    payload: Option<&WorkspacePayload>,
    query_state: &WorkspaceQueryState,
    _route: &Route,
    on_refresh: Callback<MouseEvent>,
    on_archive: Callback<MouseEvent>,
    is_canonical: bool,
) -> Html {
    html! {
        <div class="page-head org-workspace-head">
            <div>
                <Link<Route> to={Route::Org} classes={classes!("org-breadcrumb")}>{ "Org workspaces" }</Link<Route>>
                <p class="org-kicker">{ "Workspace operations" }</p>
                <h2 id="org-workspace-title" class="page-title">
                    { payload.map_or("Loading workspace", |payload| payload.workspace.display_name.as_str()) }
                </h2>
                if let Some(payload) = payload {
                    <p class="page-hint">
                        { format!("{} · revision {} · {} view", payload.workspace.timezone, payload.workspace.revision, query_state.view) }
                    </p>
                    <span class={classes!("org-state", payload.workspace.archived_at.is_some().then_some("is-archived"))}>
                        { if payload.workspace.archived_at.is_some() { "Archived · read-only" } else { "Active · read-only" } }
                    </span>
                }
            </div>
            <div class="org-workspace-head-actions">
                if let Some(payload) = payload {
                    if payload.workspace.archived_at.is_none() {
                        <Link<Route>
                            to={Route::OrgWorkspaceSettings { workspace_id: payload.workspace.id.clone() }}
                            classes={classes!("btn", "btn-primary")}
                        >
                            <span data-testid="org-workspace-edit">{ "Edit workspace" }</span>
                        </Link<Route>>
                        <button type="button" class="btn btn-outline org-workspace-archive-button" onclick={on_archive} data-testid="org-workspace-archive-open">{ "Archive workspace" }</button>
                    }
                }
                <button type="button" class="btn btn-outline" onclick={on_refresh} disabled={!is_canonical} data-testid="org-workspace-refresh">{ "Refresh" }</button>
            </div>
        </div>
    }
}

fn view_tabs(
    summary: Option<&WorkspaceSummary>,
    query_state: &WorkspaceQueryState,
    route: &Route,
) -> Html {
    html! {
        <nav class="org-view-tabs" aria-label="Operational view">
            { for VIEW_SPECS.iter().map(|spec| {
                let active = query_state.view == spec.view;
                let next = view_state(query_state, spec.view);
                let href = crate::org::url::workspace_path(
                    match route {
                        Route::OrgWorkspace { workspace_id } => workspace_id,
                        _ => "",
                    },
                    &next,
                );
                let count = summary.map(|summary| count_for(&summary.counts, spec.view));
                html! {
                    <a
                        {href}
                        class={classes!("org-view-tab", active.then_some("is-active"))}
                        aria-current={active.then_some("page")}
                        data-testid={format!("org-view-{}", spec.view)}
                    >
                        <span>{ spec.label }</span>
                        <strong>{ count.map_or_else(|| "—".to_owned(), |count| count.to_string()) }</strong>
                    </a>
                }
            }) }
        </nav>
    }
}

fn live_status(state: &WorkspacePageState, has_cursor: bool) -> String {
    match state.render_state(has_cursor) {
        WorkspaceRenderState::Loading => "Loading operational work items".to_owned(),
        WorkspaceRenderState::Table => state.payload().map_or_else(
            || "Operational work items loaded".to_owned(),
            |payload| format!("{} operational work items loaded", payload.page.items.len()),
        ),
        WorkspaceRenderState::Empty => "No work items match this operational view".to_owned(),
        WorkspaceRenderState::Error => "Operational work items could not be loaded".to_owned(),
        WorkspaceRenderState::CursorEnd => "Final cursor page loaded".to_owned(),
    }
}

fn workspace_content(
    state: &WorkspacePageState,
    workspace_id: &str,
    query_state: &WorkspaceQueryState,
) -> Html {
    match &state.load {
        WorkspaceLoad::Loading => {
            html! { <p class="loading" data-testid="org-workspace-loading">{ "Loading workspace operations..." }</p> }
        }
        WorkspaceLoad::Failed(error) => html! {
            <div class="org-directory-message is-error" role="alert" data-testid="org-workspace-error">
                <strong>{ "Workspace operations unavailable" }</strong>
                <p>{ error.message.clone() }</p>
                <code>{ error.code.clone() }</code>
                if error.retryable { <span>{ "You can retry with Refresh." }</span> }
            </div>
        },
        WorkspaceLoad::Ready(payload)
            if payload.page.items.is_empty() && query_state.cursor.is_some() =>
        {
            html! {
                <div class="org-directory-message" data-testid="org-workspace-cursor-end">
                    <strong>{ "End of operational results" }</strong>
                    <p>{ "This cursor page contains no additional work items. Use Previous page to return." }</p>
                </div>
            }
        }
        WorkspaceLoad::Ready(payload) if payload.page.items.is_empty() => html! {
            <div class="org-directory-message" data-testid="org-workspace-empty">
                <strong>{ "No work items in this view" }</strong>
                <p>{ "Try another operational view or adjust the URL-backed filters." }</p>
            </div>
        },
        WorkspaceLoad::Ready(payload) => html! {
            <div class="org-workspace-ready" data-testid="org-workspace-ready">
                <OrgTable
                    workspace_id={workspace_id.to_owned()}
                    rows={payload.page.items.clone()}
                    return_state={query_state.clone()}
                />
            </div>
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::model::OperationalItem;
    use serde_json::json;
    use yew::Reducible;

    fn error(code: &str) -> OrgApiError {
        OrgApiError {
            code: code.into(),
            message: "safe message".into(),
            details: json!({}),
            retryable: true,
            status: Some(503),
        }
    }

    fn workspace(archived: bool) -> Workspace {
        Workspace {
            id: "workspace-a".into(),
            slug: "delivery".into(),
            display_name: "Delivery".into(),
            description: "Operations".into(),
            timezone: "Asia/Shanghai".into(),
            policy_schema_version: 1,
            policy: crate::org::workspace_management::WorkspacePolicy::engineering_default(),
            revision: 7,
            archived_at: archived.then_some(1),
        }
    }

    fn loaded(items: Vec<OperationalItem>, next_cursor: Option<&str>) -> WorkspacePayload {
        WorkspacePayload {
            workspace: workspace(false),
            page: OperationalPage {
                items,
                next_cursor: next_cursor.map(str::to_owned),
                evaluated_at: 20,
            },
        }
    }

    fn operational_item() -> OperationalItem {
        serde_json::from_value(json!({
            "item": {
                "id": "item-1", "workspace_id": "workspace-a", "document_id": "document-a",
                "parent_id": null, "item_type": "task", "title": "Ship console",
                "state": "RUNNING", "priority": "A", "scheduled": null, "deadline": null,
                "assignee": "agent-one", "requires_review": false, "created_at": 1,
                "tags": ["ops"]
            },
            "attempt_count": 1, "current_attempt_status": "running",
            "retry_exhausted": false, "ready_status": null,
            "review_lease_status": null, "lease": null, "completion_at": null
        }))
        .unwrap()
    }

    fn workspace_summary(id: &str) -> WorkspaceSummary {
        WorkspaceSummary {
            workspace_id: id.into(),
            slug: id.into(),
            display_name: id.into(),
            description: String::new(),
            timezone: "UTC".into(),
            archived_at: None,
            workspace_revision: 1,
            evaluated_at: 20,
            counts: OperationalCounts {
                ready: 1,
                assigned: 2,
                running: 3,
                blocked: 4,
                review: 5,
                scheduled: 6,
                upcoming_deadline: 7,
                failed: 8,
                expired_lease: 9,
                completed: 10,
            },
        }
    }

    #[test]
    fn every_operational_view_selects_the_approved_rest_family() {
        assert_eq!(VIEW_SPECS.len(), 10);
        for view in OperationalView::ALL {
            let expected = if view.is_agenda() {
                RequestFamily::Agenda
            } else {
                RequestFamily::Queue
            };
            assert_eq!(request_family(view), expected);
            let state = WorkspaceQueryState {
                view,
                ..Default::default()
            };
            let url = operational_request_url("workspace-a", &state, false);
            assert!(url.starts_with(if view.is_agenda() {
                "/api/org/agenda?"
            } else {
                "/api/org/queue?"
            }));
        }
    }

    #[test]
    fn operational_request_is_one_workspace_only_and_carries_every_filter() {
        let state = WorkspaceQueryState::parse("view=upcoming_deadline&item_type=incident&state=WAITING%2FEXTERNAL&priority=none&tags=build%20ready%2Cops%2Fon-call&assignee=agent%2Bone%40example.test&from=2026-08-05T01%3A02%3A03Z&to=2026-08-06T01%3A02%3A03Z&cursor=opaque%2F%2B%3D%20two&limit=100");
        let url = operational_request_url("workspace/one", &state, true);
        for expected in [
            "workspace_ids=workspace%2Fone",
            "view=upcoming_deadline",
            "item_type=incident",
            "state=WAITING%2FEXTERNAL",
            "priority=none",
            "tags=build%20ready%2Cops%2Fon-call",
            "assignee=agent%2Bone%40example.test",
            "from=1785891723",
            "to=1785978123",
            "include_archived=true",
            "cursor=opaque%2F%2B%3D%20two",
            "limit=100",
        ] {
            assert!(url.contains(expected), "missing {expected} in {url}");
        }
        assert_eq!(url.matches("workspace_ids=").count(), 1);
        assert!(!url.contains("workspace_ids=workspace%2Fone%2C"));
    }

    #[test]
    fn view_filter_limit_and_paging_changes_have_canonical_raw_urls() {
        let mut current =
            WorkspaceQueryState::parse("view=ready&state=READY&cursor=page%2F1&limit=25");
        current.set_view(OperationalView::Failed);
        assert_eq!(current.cursor, None);
        current.cursor = Some("page/1".into());
        current.replace_filters(WorkspaceFilters {
            tags: vec!["build ready".into()],
            ..Default::default()
        });
        assert_eq!(current.cursor, None);
        current.cursor = Some("page/1".into());
        current.set_limit(100);
        assert_eq!(current.cursor, None);

        let mut history = Vec::new();
        let second = record_next_page(&mut history, &current, "page/2 +=");
        let third = record_next_page(&mut history, &second, "page/3 +=");
        assert!(raw_query(&third).0.contains("cursor=page%2F3%20%2B%3D"));
        let mut browser_history = history.clone();
        reconcile_browser_back(&mut browser_history, &second);
        assert_eq!(browser_history, vec![current.clone()]);
        assert_eq!(take_previous_page(&mut history), Some(second));
        assert_eq!(take_previous_page(&mut history), Some(current.clone()));
        assert_eq!(take_previous_page(&mut history), None);
        assert_eq!(pagination_scope(&third), pagination_scope(&current));
    }

    #[test]
    fn workspace_count_lookup_follows_cursors_until_the_path_workspace_is_found() {
        assert_eq!(
            summary_lookup(
                "workspace-b",
                Page {
                    items: vec![workspace_summary("workspace-a")],
                    next_cursor: Some("page-2".into()),
                },
            ),
            SummaryLookup::Continue("page-2".into())
        );
        assert!(matches!(
            summary_lookup(
                "workspace-b",
                Page {
                    items: vec![workspace_summary("workspace-b")],
                    next_cursor: None,
                },
            ),
            SummaryLookup::Found(summary)
                if summary.workspace_id == "workspace-b" && summary.counts.completed == 10
        ));
        assert_eq!(
            summary_lookup(
                "workspace-z",
                Page {
                    items: vec![workspace_summary("workspace-b")],
                    next_cursor: None,
                },
            ),
            SummaryLookup::Missing
        );
    }

    #[test]
    fn stale_responses_cannot_replace_the_latest_url_generation() {
        let state = Rc::new(WorkspacePageState::default())
            .reduce(WorkspaceAction::Loading { generation: 1 })
            .reduce(WorkspaceAction::Loading { generation: 2 })
            .reduce(WorkspaceAction::Loaded {
                generation: 1,
                payload: loaded(vec![], None),
            });
        assert_eq!(state.generation, 2);
        assert_eq!(state.render_state(false), WorkspaceRenderState::Loading);
        let state = state.reduce(WorkspaceAction::Failed {
            generation: 2,
            error: error("offline"),
        });
        assert_eq!(state.render_state(false), WorkspaceRenderState::Error);
    }

    #[test]
    fn loading_empty_error_cursor_end_and_manual_refresh_states_are_explicit() {
        let loading = WorkspacePageState::default();
        assert_eq!(loading.render_state(false), WorkspaceRenderState::Loading);
        let empty = WorkspacePageState {
            generation: 1,
            load: WorkspaceLoad::Ready(loaded(vec![], None)),
        };
        assert_eq!(empty.render_state(false), WorkspaceRenderState::Empty);
        let failed = WorkspacePageState {
            generation: 1,
            load: WorkspaceLoad::Failed(error("offline")),
        };
        assert_eq!(failed.render_state(false), WorkspaceRenderState::Error);
        assert!(live_status(&failed, false).contains("could not"));

        let final_cursor_page = WorkspacePageState {
            generation: 1,
            load: WorkspaceLoad::Ready(loaded(vec![], None)),
        };
        assert_eq!(
            final_cursor_page.render_state(true),
            WorkspaceRenderState::CursorEnd
        );
        assert_eq!(
            live_status(&final_cursor_page, true),
            "Final cursor page loaded"
        );

        let final_nonempty_cursor_page = WorkspacePageState {
            generation: 1,
            load: WorkspaceLoad::Ready(loaded(vec![operational_item()], None)),
        };
        assert_eq!(
            final_nonempty_cursor_page.render_state(true),
            WorkspaceRenderState::CursorEnd
        );

        let refreshed =
            Rc::new(final_cursor_page).reduce(WorkspaceAction::Loading { generation: 2 });
        assert_eq!(refreshed.generation, 2);
        assert_eq!(refreshed.render_state(true), WorkspaceRenderState::Loading);
    }

    #[test]
    fn archived_workspaces_are_loaded_read_only_and_with_archived_query_scope() {
        let archived = workspace(true);
        assert!(archived.archived_at.is_some());
        let url = operational_request_url(
            "workspace-a",
            &WorkspaceQueryState::default(),
            archived.archived_at.is_some(),
        );
        assert!(url.contains("include_archived=true"));
    }

    #[test]
    fn archive_confirmation_requires_the_exact_workspace_slug() {
        assert!(archive_confirmation_matches("delivery", "delivery"));
        assert!(!archive_confirmation_matches("Delivery", "delivery"));
        assert!(!archive_confirmation_matches("delivery ", "delivery"));
    }

    #[test]
    fn active_workspace_source_exposes_edit_and_reversible_archive_actions() {
        let source = include_str!("org_workspace.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for required in [
            "Route::OrgWorkspaceSettings",
            "Edit workspace",
            "Archive workspace",
            "ArchiveWorkspaceBody::new",
            "org_api::archive_workspace",
            "include_archived: true",
        ] {
            assert!(source.contains(required), "missing source: {required}");
        }
        for forbidden in ["delete_workspace", "Restore workspace", "Hard delete"] {
            assert!(!source.contains(forbidden), "forbidden source: {forbidden}");
        }
    }

    #[test]
    fn source_has_generation_guard_and_no_polling_or_mutation_transport() {
        let source = include_str!("org_workspace.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("generation == self.generation"));
        assert!(source.contains("Refresh"));
        for forbidden in [
            "Interval",
            "set_interval",
            "Request::post",
            "Request::patch",
            "Request::delete",
            "/mcp",
        ] {
            assert!(!source.contains(forbidden), "forbidden source: {forbidden}");
        }
    }
}
