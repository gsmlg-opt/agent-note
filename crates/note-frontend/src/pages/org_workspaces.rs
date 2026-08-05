use std::rc::Rc;

use serde::Serialize;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::prelude::*;
use yew_router::query::Raw;

use crate::{
    org::{
        api::{self as org_api, OrgApiError},
        model::{OperationalCounts, Page, WorkspaceSummary},
        url::{WorkspaceListState, ALLOWED_LIMITS},
    },
    routes::Route,
};

const COUNT_COLUMNS: [(&str, &str); 10] = [
    ("Ready", "Ready work items"),
    ("Assigned", "Assigned work items"),
    ("Running", "Running work items"),
    ("Blocked", "Blocked work items"),
    ("Review", "Work items awaiting review"),
    ("Scheduled", "Scheduled work items"),
    ("Due soon", "Upcoming deadlines"),
    ("Failed", "Failed work items"),
    ("Expired", "Work items with expired leases"),
    ("Completed", "Completed work items"),
];

fn count_values(counts: &OperationalCounts) -> [i64; 10] {
    [
        counts.ready,
        counts.assigned,
        counts.running,
        counts.blocked,
        counts.review,
        counts.scheduled,
        counts.upcoming_deadline,
        counts.failed,
        counts.expired_lease,
        counts.completed,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectoryView {
    Loading,
    Table,
    Empty,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
enum DirectoryLoad {
    Loading,
    Ready(Page<WorkspaceSummary>),
    Failed(OrgApiError),
}

#[derive(Clone, Debug, PartialEq)]
struct DirectoryState {
    generation: u64,
    load: DirectoryLoad,
}

impl Default for DirectoryState {
    fn default() -> Self {
        Self {
            generation: 0,
            load: DirectoryLoad::Loading,
        }
    }
}

enum DirectoryAction {
    Loading {
        generation: u64,
    },
    Loaded {
        generation: u64,
        page: Page<WorkspaceSummary>,
    },
    Failed {
        generation: u64,
        error: OrgApiError,
    },
}

impl Reducible for DirectoryState {
    type Action = DirectoryAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            DirectoryAction::Loading { generation } if generation >= self.generation => Self {
                generation,
                load: DirectoryLoad::Loading,
            }
            .into(),
            DirectoryAction::Loaded { generation, page } if generation == self.generation => Self {
                generation,
                load: DirectoryLoad::Ready(page),
            }
            .into(),
            DirectoryAction::Failed { generation, error } if generation == self.generation => {
                Self {
                    generation,
                    load: DirectoryLoad::Failed(error),
                }
                .into()
            }
            _ => self,
        }
    }
}

impl DirectoryState {
    fn view(&self) -> DirectoryView {
        match &self.load {
            DirectoryLoad::Loading => DirectoryView::Loading,
            DirectoryLoad::Ready(page) if page.items.is_empty() => DirectoryView::Empty,
            DirectoryLoad::Ready(_) => DirectoryView::Table,
            DirectoryLoad::Failed(_) => DirectoryView::Error,
        }
    }

    fn page(&self) -> Option<&Page<WorkspaceSummary>> {
        match &self.load {
            DirectoryLoad::Ready(page) => Some(page),
            _ => None,
        }
    }

    #[cfg(test)]
    fn error(&self) -> Option<&OrgApiError> {
        match &self.load {
            DirectoryLoad::Failed(error) => Some(error),
            _ => None,
        }
    }
}

fn workspace_list_navigation_query(state: &WorkspaceListState) -> Raw<String> {
    Raw(state.canonical_query())
}

#[derive(Clone, PartialEq, Serialize)]
struct ReadyWorkspaceQuery {
    view: &'static str,
    limit: u16,
}

#[function_component(OrgWorkspacesPage)]
pub fn org_workspaces_page() -> Html {
    let directory = use_reducer(DirectoryState::default);
    let refresh_tick = use_state(|| 0_u64);
    let request_generation = use_mut_ref(|| 0_u64);
    let navigator = use_navigator();
    let location = use_location();
    let raw_query = location
        .as_ref()
        .map(|location| location.query_str().trim_start_matches('?').to_owned())
        .unwrap_or_default();
    let list_state = WorkspaceListState::parse(&raw_query);
    let canonical_query = list_state.canonical_query();
    let is_canonical = raw_query == canonical_query;

    {
        let navigator = navigator.clone();
        let list_state = list_state.clone();
        use_effect_with(
            (raw_query.clone(), canonical_query.clone()),
            move |(raw, canonical)| {
                if raw != canonical {
                    if let Some(navigator) = &navigator {
                        let _ = navigator.replace_with_query(
                            &Route::Org,
                            workspace_list_navigation_query(&list_state),
                        );
                    }
                }
                || ()
            },
        );
    }

    {
        let directory = directory.clone();
        let request_generation = request_generation.clone();
        let list_state = list_state.clone();
        use_effect_with(
            (canonical_query.clone(), *refresh_tick, is_canonical),
            move |(_, _, is_canonical)| {
                if *is_canonical {
                    let generation = {
                        let mut generation = request_generation.borrow_mut();
                        *generation = generation.saturating_add(1);
                        *generation
                    };
                    directory.dispatch(DirectoryAction::Loading { generation });
                    let directory = directory.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        match org_api::list_workspaces(&list_state).await {
                            Ok(page) => {
                                directory.dispatch(DirectoryAction::Loaded { generation, page })
                            }
                            Err(error) => {
                                directory.dispatch(DirectoryAction::Failed { generation, error })
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    let on_include_archived = {
        let navigator = navigator.clone();
        let list_state = list_state.clone();
        Callback::from(move |event: Event| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let mut next = list_state.clone();
            next.set_include_archived(input.checked());
            if let Some(navigator) = &navigator {
                let _ =
                    navigator.push_with_query(&Route::Org, workspace_list_navigation_query(&next));
            }
        })
    };
    let on_limit = {
        let navigator = navigator.clone();
        let list_state = list_state.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            if let Ok(limit) = select.value().parse() {
                let mut next = list_state.clone();
                next.set_limit(limit);
                if let Some(navigator) = &navigator {
                    let _ = navigator
                        .push_with_query(&Route::Org, workspace_list_navigation_query(&next));
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
        let list_state = list_state.clone();
        let next_cursor = directory.page().and_then(|page| page.next_cursor.clone());
        Callback::from(move |_| {
            let Some(cursor) = &next_cursor else {
                return;
            };
            let mut next = list_state.clone();
            next.cursor = Some(cursor.clone());
            if let Some(navigator) = &navigator {
                let _ =
                    navigator.push_with_query(&Route::Org, workspace_list_navigation_query(&next));
            }
        })
    };

    html! {
        <section
            class="stack org-directory"
            aria-labelledby="org-directory-title"
            data-testid="org-directory-page"
        >
            <div class="page-head org-directory-head">
                <div>
                    <p class="org-kicker">{ "Operations directory" }</p>
                    <h2 id="org-directory-title" class="page-title">{ "Org workspaces" }</h2>
                    <p class="page-hint">{ "Read-only delivery state, evaluated by the Org service." }</p>
                </div>
                <button
                    type="button"
                    class="btn btn-outline"
                    onclick={on_refresh}
                    disabled={!is_canonical}
                    data-testid="org-directory-refresh"
                >
                    { "Refresh" }
                </button>
            </div>

            <div class="org-directory-controls" aria-label="Workspace directory controls">
                <label class="org-checkbox">
                    <input
                        type="checkbox"
                        name="include_archived"
                        checked={list_state.include_archived}
                        onchange={on_include_archived}
                        data-testid="org-directory-include-archived"
                    />
                    <span>{ "Include archived" }</span>
                </label>
                <label class="org-page-size">
                    <span>{ "Rows" }</span>
                    <select
                        class="input"
                        name="limit"
                        value={list_state.limit.to_string()}
                        onchange={on_limit}
                        data-testid="org-directory-limit"
                    >
                        { for ALLOWED_LIMITS.iter().map(|limit| html! {
                            <option value={limit.to_string()} selected={*limit == list_state.limit}>{ limit }</option>
                        }) }
                    </select>
                </label>
            </div>

            <div class="org-live-status" aria-live="polite" aria-atomic="true">
                { match directory.view() {
                    DirectoryView::Loading => "Loading Org workspaces".to_owned(),
                    DirectoryView::Table => format!(
                        "{} workspaces loaded",
                        directory.page().map_or(0, |page| page.items.len())
                    ),
                    DirectoryView::Empty => "No workspaces found".to_owned(),
                    DirectoryView::Error => "Org workspaces could not be loaded".to_owned(),
                } }
            </div>

            { directory_content(&directory) }

            if let Some(page) = directory.page() {
                <nav class="org-cursor-nav" aria-label="Workspace pages">
                    <span>{ format!("Showing up to {} workspaces", list_state.limit) }</span>
                    <button
                        type="button"
                        class="btn btn-outline"
                        onclick={on_next}
                        disabled={page.next_cursor.is_none()}
                    >
                        { "Next page" }
                    </button>
                </nav>
            }
        </section>
    }
}

fn directory_content(state: &DirectoryState) -> Html {
    match &state.load {
        DirectoryLoad::Loading => html! {
            <p class="loading" data-testid="org-directory-loading">{ "Loading workspaces..." }</p>
        },
        DirectoryLoad::Failed(error) => html! {
            <div class="org-directory-message is-error" role="alert" data-testid="org-directory-error">
                <strong>{ "Workspace directory unavailable" }</strong>
                <p>{ error.message.clone() }</p>
                <code>{ error.code.clone() }</code>
                if error.retryable {
                    <span>{ "You can retry with Refresh." }</span>
                }
            </div>
        },
        DirectoryLoad::Ready(page) if page.items.is_empty() => html! {
            <div class="org-directory-message" data-testid="org-directory-empty">
                <strong>{ "No Org workspaces" }</strong>
                <p>{ "No workspaces match the current archive selection." }</p>
            </div>
        },
        DirectoryLoad::Ready(page) => workspace_table(&page.items),
    }
}

fn workspace_table(workspaces: &[WorkspaceSummary]) -> Html {
    html! {
        <div
            class="org-ledger-scroll"
            tabindex="0"
            aria-label="Org workspace operational counts; scroll horizontally for all ten views"
            data-testid="org-directory-ready"
        >
            <table class="org-ledger org-workspace-ledger" data-testid="org-directory-table">
                <caption>{ "Org workspace operational counts" }</caption>
                <thead>
                    <tr>
                        <th scope="col">{ "Workspace" }</th>
                        <th scope="col">{ "Timezone" }</th>
                        <th scope="col">{ "State" }</th>
                        <th scope="col">{ "Revision" }</th>
                        { for COUNT_COLUMNS.iter().map(|(label, description)| html! {
                            <th scope="col" title={*description}>{ label }</th>
                        }) }
                    </tr>
                </thead>
                <tbody>
                    { for workspaces.iter().map(workspace_row) }
                </tbody>
            </table>
        </div>
    }
}

fn workspace_row(workspace: &WorkspaceSummary) -> Html {
    let archived = workspace.archived_at.is_some();
    let route = Route::OrgWorkspace {
        workspace_id: workspace.workspace_id.clone(),
    };
    html! {
        <tr class={classes!("org-workspace-row", archived.then_some("is-archived"))}>
            <th scope="row">
                <Link<Route, ReadyWorkspaceQuery>
                    to={route}
                    query={ReadyWorkspaceQuery { view: "ready", limit: 50 }}
                    classes={classes!("org-workspace-link")}
                >
                    <strong>{ workspace.display_name.clone() }</strong>
                    <span>{ workspace.slug.clone() }</span>
                </Link<Route, ReadyWorkspaceQuery>>
            </th>
            <td>{ workspace.timezone.clone() }</td>
            <td>
                <span class={classes!("org-state", archived.then_some("is-archived"))}>
                    { if archived { "Archived" } else { "Active" } }
                </span>
            </td>
            <td class="org-ledger-number">{ workspace.workspace_revision }</td>
            { for count_values(&workspace.counts).into_iter().map(|count| html! {
                <td class="org-ledger-number">{ count }</td>
            }) }
        </tr>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::{
        api::OrgApiError,
        model::{OperationalCounts, Page, WorkspaceSummary},
        url::WorkspaceListState,
    };
    use serde_json::json;
    use yew::Reducible;
    use yew_router::query::ToQuery;

    fn workspace(archived: bool) -> WorkspaceSummary {
        WorkspaceSummary {
            workspace_id: "10000000-0000-4000-8000-000000000001".to_owned(),
            slug: "release-ops".to_owned(),
            display_name: "Release operations".to_owned(),
            description: "Production delivery ledger".to_owned(),
            timezone: "Asia/Shanghai".to_owned(),
            archived_at: archived.then_some(1_785_880_000),
            workspace_revision: 12,
            evaluated_at: 1_785_880_020,
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

    fn page(items: Vec<WorkspaceSummary>) -> Page<WorkspaceSummary> {
        Page {
            items,
            next_cursor: Some("next page".to_owned()),
        }
    }

    #[test]
    fn directory_has_explicit_loading_nonempty_empty_and_structured_error_states() {
        let loading = Rc::new(DirectoryState::default());
        assert_eq!(loading.view(), DirectoryView::Loading);

        let nonempty = loading.reduce(DirectoryAction::Loaded {
            generation: 0,
            page: page(vec![workspace(false)]),
        });
        assert_eq!(nonempty.view(), DirectoryView::Table);

        let empty = Rc::new(DirectoryState::default()).reduce(DirectoryAction::Loaded {
            generation: 0,
            page: page(vec![]),
        });
        assert_eq!(empty.view(), DirectoryView::Empty);

        let error = Rc::new(DirectoryState::default()).reduce(DirectoryAction::Failed {
            generation: 0,
            error: OrgApiError {
                code: "invalid_cursor".to_owned(),
                message: "Cursor is invalid".to_owned(),
                details: json!({"field":"cursor"}),
                retryable: false,
                status: Some(400),
            },
        });
        assert_eq!(error.view(), DirectoryView::Error);
        assert_eq!(error.error().unwrap().code, "invalid_cursor");
    }

    #[test]
    fn archived_selection_is_canonical_and_clears_the_cursor() {
        let mut state = WorkspaceListState::parse(
            "include_archived=false&cursor=older%2Fpage&limit=25&unknown=ignored",
        );
        state.set_include_archived(true);
        assert_eq!(state.canonical_query(), "include_archived=true&limit=25");
        assert!(workspace(true).archived_at.is_some());
    }

    #[test]
    fn navigation_query_converges_for_reserved_cursor_characters() {
        let state = WorkspaceListState::parse(
            "include_archived=true&cursor=next%20page%2F%E4%B8%8A%E6%B5%B7%26x&limit=25",
        );
        let canonical = state.canonical_query();
        let navigation_query = workspace_list_navigation_query(&state);
        let navigation = navigation_query.to_query().unwrap();

        assert_eq!(navigation, canonical);
        assert_eq!(
            WorkspaceListState::parse(&navigation).canonical_query(),
            canonical
        );
    }

    #[test]
    fn refresh_generation_rejects_stale_responses() {
        let state = Rc::new(DirectoryState::default())
            .reduce(DirectoryAction::Loading { generation: 1 })
            .reduce(DirectoryAction::Loading { generation: 2 })
            .reduce(DirectoryAction::Loaded {
                generation: 1,
                page: page(vec![workspace(true)]),
            });
        assert_eq!(state.generation, 2);
        assert_eq!(state.view(), DirectoryView::Loading);

        let current = state.reduce(DirectoryAction::Loaded {
            generation: 2,
            page: page(vec![workspace(false)]),
        });
        assert_eq!(current.view(), DirectoryView::Table);
        assert!(current.page().unwrap().items[0].archived_at.is_none());
    }

    #[test]
    fn ledger_exposes_counts_for_all_ten_operational_views() {
        assert_eq!(COUNT_COLUMNS.len(), 10);
        assert_eq!(
            count_values(&workspace(false).counts),
            [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
        );
    }

    #[test]
    fn page_is_manual_refresh_only_and_exposes_accessible_ledger_landmarks() {
        let production = include_str!("org_workspaces.rs")
            .split("#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
        for required in [
            "Refresh",
            "Include archived",
            "aria-live",
            "<caption>",
            "scope=\"col\"",
            "scope=\"row\"",
        ] {
            assert!(production.contains(required), "missing {required}");
        }
        for forbidden in ["Interval", "set_interval", "Request::post", "fencing_token"] {
            assert!(!production.contains(forbidden), "found {forbidden}");
        }
    }
}
