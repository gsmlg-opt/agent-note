use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
};

use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;
use yew_router::{prelude::*, query::Raw};

use crate::{
    components::{Modal, OrgDocumentTable},
    org::{
        api::{self as org_api, OrgApiError},
        document_management::{
            CreateDocumentBody, DocumentDraft, DocumentListState, DocumentRevisionBody,
            DocumentStatus, RenameDocumentBody, DOCUMENT_ALLOWED_LIMITS,
        },
        model::{Document, Page, Workspace},
        mutation::MutationSubmission,
    },
    routes::Route,
};

#[derive(Clone, Debug, PartialEq)]
struct FilesPayload {
    workspace: Workspace,
    page: Page<Document>,
}

#[derive(Clone, Debug, PartialEq)]
enum FilesLoad {
    Loading,
    Ready(FilesPayload),
    Failed(OrgApiError),
}

#[derive(Clone, Debug, PartialEq)]
struct FilesPageState {
    generation: u64,
    load: FilesLoad,
    requires_refresh: bool,
    refresh_generation: Option<u64>,
}

impl Default for FilesPageState {
    fn default() -> Self {
        Self {
            generation: 0,
            load: FilesLoad::Loading,
            requires_refresh: false,
            refresh_generation: None,
        }
    }
}

enum FilesAction {
    WorkspaceChanged {
        generation: u64,
    },
    Loading {
        generation: u64,
        explicit_refresh: bool,
    },
    Loaded {
        generation: u64,
        payload: FilesPayload,
    },
    Failed {
        generation: u64,
        error: OrgApiError,
    },
    RequireRefresh,
}

impl Reducible for FilesPageState {
    type Action = FilesAction;

    fn reduce(self: Rc<Self>, action: Self::Action) -> Rc<Self> {
        match action {
            FilesAction::WorkspaceChanged { generation } if generation >= self.generation => Self {
                generation,
                load: FilesLoad::Loading,
                requires_refresh: false,
                refresh_generation: None,
            }
            .into(),
            FilesAction::Loading {
                generation,
                explicit_refresh,
            } if generation >= self.generation => Self {
                generation,
                load: FilesLoad::Loading,
                requires_refresh: self.requires_refresh,
                refresh_generation: (explicit_refresh && self.requires_refresh)
                    .then_some(generation),
            }
            .into(),
            FilesAction::Loaded {
                generation,
                payload,
            } if generation == self.generation => {
                let refreshed = self.refresh_generation == Some(generation);
                Self {
                    generation,
                    load: FilesLoad::Ready(payload),
                    requires_refresh: self.requires_refresh && !refreshed,
                    refresh_generation: None,
                }
                .into()
            }
            FilesAction::Failed { generation, error } if generation == self.generation => Self {
                generation,
                load: FilesLoad::Failed(error),
                requires_refresh: self.requires_refresh,
                refresh_generation: None,
            }
            .into(),
            FilesAction::RequireRefresh => Self {
                generation: self.generation,
                load: self.load.clone(),
                requires_refresh: true,
                refresh_generation: None,
            }
            .into(),
            _ => self,
        }
    }
}

impl FilesPageState {
    fn payload(&self) -> Option<&FilesPayload> {
        match &self.load {
            FilesLoad::Ready(payload) => Some(payload),
            _ => None,
        }
    }
}

fn payload_for_workspace<'a>(
    state: &'a FilesPageState,
    workspace_id: &str,
) -> Option<&'a FilesPayload> {
    state
        .payload()
        .filter(|payload| payload.workspace.id == workspace_id)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LiveAnnouncement {
    sequence: u64,
    message: Option<String>,
}

impl LiveAnnouncement {
    fn start_request(mut self) -> Self {
        self.sequence = self.sequence.saturating_add(1);
        self.message = None;
        self
    }

    fn succeeded(mut self, message: impl Into<String>) -> Self {
        self.sequence = self.sequence.saturating_add(1);
        self.message = Some(message.into());
        self
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FilesLoadCause {
    #[default]
    Navigation,
    ManualRefresh,
    MutationSuccess,
}

fn begin_files_load(announcement: LiveAnnouncement, cause: FilesLoadCause) -> LiveAnnouncement {
    if cause == FilesLoadCause::MutationSuccess {
        announcement
    } else {
        announcement.start_request()
    }
}

fn fail_files_load(announcement: LiveAnnouncement, cause: FilesLoadCause) -> LiveAnnouncement {
    if cause == FilesLoadCause::MutationSuccess {
        announcement.start_request()
    } else {
        announcement
    }
}

fn set_document_status(
    state: &mut DocumentListState,
    history: &mut Vec<DocumentListState>,
    status: DocumentStatus,
) {
    state.set_status(status);
    state.cursor = None;
    history.clear();
}

fn status_filter_transition(
    current: &DocumentListState,
    history: &[DocumentListState],
    status: DocumentStatus,
) -> (DocumentListState, Vec<DocumentListState>) {
    let mut next = current.clone();
    let mut next_history = history.to_vec();
    set_document_status(&mut next, &mut next_history, status);
    (next, next_history)
}

fn set_document_limit(
    state: &mut DocumentListState,
    history: &mut Vec<DocumentListState>,
    limit: u16,
) {
    state.set_limit(limit);
    history.clear();
}

fn record_next_page(
    history: &mut Vec<DocumentListState>,
    current: &DocumentListState,
    cursor: &str,
) -> DocumentListState {
    history.push(current.clone());
    let mut next = current.clone();
    next.cursor = Some(cursor.to_owned());
    next
}

fn take_previous_page(history: &mut Vec<DocumentListState>) -> Option<DocumentListState> {
    history.pop()
}

const MAX_PAGINATION_SNAPSHOTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PaginationSnapshotKey {
    workspace_id: String,
    canonical_query: String,
}

impl PaginationSnapshotKey {
    fn new(workspace_id: &str, target: &DocumentListState) -> Self {
        Self {
            workspace_id: workspace_id.to_owned(),
            canonical_query: target.canonical_query(),
        }
    }
}

#[derive(Default)]
struct PaginationSnapshots {
    entries: BTreeMap<PaginationSnapshotKey, Vec<DocumentListState>>,
    order: VecDeque<PaginationSnapshotKey>,
}

impl PaginationSnapshots {
    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    fn remember(
        &mut self,
        workspace_id: &str,
        target: &DocumentListState,
        history: &[DocumentListState],
    ) {
        let key = PaginationSnapshotKey::new(workspace_id, target);
        self.order.retain(|existing| existing != &key);
        self.order.push_back(key.clone());
        self.entries.insert(key, history.to_vec());
        while self.entries.len() > MAX_PAGINATION_SNAPSHOTS {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn restore(
        &mut self,
        workspace_id: &str,
        target: &DocumentListState,
    ) -> Vec<DocumentListState> {
        let key = PaginationSnapshotKey::new(workspace_id, target);
        if let Some(history) = self.entries.get(&key).cloned() {
            self.order.retain(|existing| existing != &key);
            self.order.push_back(key);
            history
        } else {
            self.remember(workspace_id, target, &[]);
            Vec::new()
        }
    }

    fn reset_to(&mut self, workspace_id: &str, target: &DocumentListState) {
        self.entries
            .retain(|key, _| key.workspace_id != workspace_id);
        self.order.retain(|key| key.workspace_id != workspace_id);
        self.remember(workspace_id, target, &[]);
    }
}

fn raw_query(state: &DocumentListState) -> Raw<String> {
    Raw(state.canonical_query())
}

fn page_size_options(current: u16) -> Vec<(u16, bool)> {
    DOCUMENT_ALLOWED_LIMITS
        .iter()
        .map(|limit| (*limit, *limit == current))
        .collect()
}

fn files_href(workspace_id: &str, state: &DocumentListState) -> String {
    format!(
        "/org/{}/files?{}",
        urlencoding::encode(workspace_id),
        state.canonical_query()
    )
}

fn workspace_allows_mutations(workspace: &Workspace) -> bool {
    workspace.archived_at.is_none()
}

#[derive(Clone, Debug, PartialEq)]
enum PendingDocumentMutation {
    Create {
        draft: DocumentDraft,
        body: CreateDocumentBody,
    },
    Rename {
        document_id: String,
        draft: DocumentDraft,
        body: RenameDocumentBody,
    },
    Archive {
        document_id: String,
        path: String,
        body: DocumentRevisionBody,
    },
    Restore {
        document_id: String,
        path: String,
        body: DocumentRevisionBody,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct PendingDocumentRequest {
    workspace_id: String,
    mutation: PendingDocumentMutation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceIdentity {
    workspace_id: String,
    generation: u64,
}

fn mutation_response_is_current(
    request: &PendingDocumentRequest,
    workspace_generation: u64,
    current_workspace: &WorkspaceIdentity,
) -> bool {
    request.workspace_id == current_workspace.workspace_id
        && workspace_generation == current_workspace.generation
}

impl PendingDocumentMutation {
    fn draft(&self) -> Option<&DocumentDraft> {
        match self {
            Self::Create { draft, .. } | Self::Rename { draft, .. } => Some(draft),
            Self::Archive { .. } | Self::Restore { .. } => None,
        }
    }

    fn success_message(&self) -> &'static str {
        match self {
            Self::Create { .. } => "Created",
            Self::Rename { .. } => "Renamed",
            Self::Archive { .. } => "Archived",
            Self::Restore { .. } => "Restored",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct MutationState {
    pending: Option<PendingDocumentRequest>,
    error: Option<OrgApiError>,
    preserved_draft: Option<DocumentDraft>,
}

impl MutationState {
    fn begin(mut self, pending: PendingDocumentRequest) -> Self {
        self.pending = Some(pending);
        self.error = None;
        self.preserved_draft = None;
        self
    }

    fn failed(mut self, error: OrgApiError) -> Self {
        if error.code == "stale_revision" {
            self.preserved_draft = self
                .pending
                .as_ref()
                .and_then(|request| request.mutation.draft().cloned());
            self.pending = None;
        }
        self.error = Some(error);
        self
    }

    fn retry(&self, workspace_id: &str) -> Option<PendingDocumentRequest> {
        self.error
            .as_ref()
            .filter(|error| error.retryable)
            .and_then(|_| self.pending.as_ref())
            .filter(|request| request.workspace_id == workspace_id)
            .cloned()
    }

    fn draft_changed(mut self, path: &str) -> Self {
        let request_changed = self
            .pending
            .as_ref()
            .and_then(|request| request.mutation.draft())
            .is_some_and(|draft| draft.path != path);
        if request_changed {
            self.pending = None;
            self.error = None;
        }
        self
    }

    #[allow(dead_code)]
    fn draft(&self) -> Option<&DocumentDraft> {
        self.pending
            .as_ref()
            .and_then(|request| request.mutation.draft())
            .or(self.preserved_draft.as_ref())
    }

    fn refreshed(mut self) -> Self {
        self.error = None;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SafeErrorView {
    code: String,
    message: String,
    work_item_ids: Vec<String>,
}

fn error_view(error: &OrgApiError) -> SafeErrorView {
    let work_item_ids = error
        .details
        .get("work_item_ids")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect();
    SafeErrorView {
        code: error.code.clone(),
        message: error.message.clone(),
        work_item_ids,
    }
}

fn lifecycle_error_hint(code: &str) -> &'static str {
    match code {
        "invalid_input" => "Review the file path and submit a valid lifecycle request.",
        "document_path_conflict" => "Choose a different file path and submit again.",
        "archived_workspace" => "This workspace is archived and its files are read-only.",
        "active_lease" => "Blocking work items must release their active leases first.",
        "stale_revision" => "Refresh files before making a new deliberate submission.",
        _ => "The file lifecycle operation could not be completed.",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogKind {
    Add,
    Rename,
    Archive,
    Restore,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocumentDialog {
    workspace_id: String,
    kind: DialogKind,
    document_id: Option<String>,
    path: String,
}

fn dialog_for_workspace<'a>(
    dialog: Option<&'a DocumentDialog>,
    workspace_id: &str,
) -> Option<&'a DocumentDialog> {
    dialog.filter(|dialog| dialog.workspace_id == workspace_id)
}

fn submit_document_mutation(
    request: PendingDocumentRequest,
    current_workspace: Rc<RefCell<WorkspaceIdentity>>,
    busy: UseStateHandle<bool>,
    mutation_state: UseStateHandle<MutationState>,
    page_state: UseReducerHandle<FilesPageState>,
    dialog: UseStateHandle<Option<DocumentDialog>>,
    pending_load_cause: Rc<RefCell<FilesLoadCause>>,
    refresh_tick: UseStateHandle<u64>,
    live_announcement: UseStateHandle<LiveAnnouncement>,
) {
    let workspace_generation = current_workspace.borrow().generation;
    busy.set(true);
    mutation_state.set(MutationState::default().begin(request.clone()));
    let request_announcement = (*live_announcement).clone().start_request();
    live_announcement.set(request_announcement.clone());
    wasm_bindgen_futures::spawn_local(async move {
        let result = match &request.mutation {
            PendingDocumentMutation::Create { body, .. } => {
                org_api::create_document(&request.workspace_id, body).await
            }
            PendingDocumentMutation::Rename {
                document_id, body, ..
            } => org_api::rename_document(document_id, body).await,
            PendingDocumentMutation::Archive {
                document_id, body, ..
            } => org_api::archive_document(document_id, body).await,
            PendingDocumentMutation::Restore {
                document_id, body, ..
            } => org_api::restore_document(document_id, body).await,
        };
        if !mutation_response_is_current(
            &request,
            workspace_generation,
            &current_workspace.borrow(),
        ) {
            return;
        }
        busy.set(false);
        match result {
            Ok(_) => {
                live_announcement
                    .set(request_announcement.succeeded(request.mutation.success_message()));
                mutation_state.set(MutationState::default());
                dialog.set(None);
                *pending_load_cause.borrow_mut() = FilesLoadCause::MutationSuccess;
                refresh_tick.set((*refresh_tick).saturating_add(1));
            }
            Err(error) => {
                if error.code == "stale_revision" {
                    page_state.dispatch(FilesAction::RequireRefresh);
                }
                mutation_state.set(MutationState::default().begin(request).failed(error));
            }
        }
    });
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgWorkspaceFilesPageProps {
    pub workspace_id: String,
}

#[function_component(OrgWorkspaceFilesPage)]
pub fn org_workspace_files_page(props: &OrgWorkspaceFilesPageProps) -> Html {
    let page_state = use_reducer(FilesPageState::default);
    let request_generation = use_mut_ref(|| 0_u64);
    let current_workspace = use_mut_ref(|| WorkspaceIdentity {
        workspace_id: props.workspace_id.clone(),
        generation: 0,
    });
    {
        let mut current = current_workspace.borrow_mut();
        if current.workspace_id != props.workspace_id {
            current.workspace_id = props.workspace_id.clone();
            current.generation = current.generation.saturating_add(1);
        }
    }
    let pending_load_cause = use_mut_ref(FilesLoadCause::default);
    let refresh_tick = use_state(|| 0_u64);
    let pagination_history = use_state(Vec::<DocumentListState>::new);
    let pagination_snapshots = use_mut_ref(PaginationSnapshots::default);
    let dialog = use_state(|| None::<DocumentDialog>);
    let draft = use_state(String::new);
    let confirmation = use_state(String::new);
    let validation_error = use_state(|| None::<String>);
    let mutation_state = use_state(MutationState::default);
    let busy = use_state(|| false);
    let live_announcement = use_state(LiveAnnouncement::default);
    let navigator = use_navigator();
    let location = use_location();
    let raw_location_query = location
        .as_ref()
        .map(|location| location.query_str().trim_start_matches('?').to_owned())
        .unwrap_or_default();
    let query_state = DocumentListState::parse(&raw_location_query);
    let canonical_query = query_state.canonical_query();
    let is_canonical = raw_location_query == canonical_query;
    let route = Route::OrgWorkspaceFiles {
        workspace_id: props.workspace_id.clone(),
    };

    {
        let page_state = page_state.clone();
        let request_generation = request_generation.clone();
        let pending_load_cause = pending_load_cause.clone();
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let dialog = dialog.clone();
        let draft = draft.clone();
        let confirmation = confirmation.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        let busy = busy.clone();
        let live_announcement = live_announcement.clone();
        use_effect_with(props.workspace_id.clone(), move |_| {
            let generation = {
                let mut value = request_generation.borrow_mut();
                *value = value.saturating_add(1);
                *value
            };
            page_state.dispatch(FilesAction::WorkspaceChanged { generation });
            *pending_load_cause.borrow_mut() = FilesLoadCause::Navigation;
            pagination_history.set(Vec::new());
            pagination_snapshots.borrow_mut().clear();
            dialog.set(None);
            draft.set(String::new());
            confirmation.set(String::new());
            validation_error.set(None);
            mutation_state.set(MutationState::default());
            busy.set(false);
            live_announcement.set(LiveAnnouncement::default());
            || ()
        });
    }

    {
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let query_state = query_state.clone();
        let workspace_id = props.workspace_id.clone();
        use_effect_with(
            (props.workspace_id.clone(), canonical_query.clone()),
            move |_| {
                let history = pagination_snapshots
                    .borrow_mut()
                    .restore(&workspace_id, &query_state);
                if *pagination_history != history {
                    pagination_history.set(history);
                }
                || ()
            },
        );
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
        let page_state = page_state.clone();
        let request_generation = request_generation.clone();
        let workspace_id = props.workspace_id.clone();
        let query_state = query_state.clone();
        let mutation_state = mutation_state.clone();
        let pending_load_cause = pending_load_cause.clone();
        let live_announcement = live_announcement.clone();
        use_effect_with(
            (
                props.workspace_id.clone(),
                canonical_query.clone(),
                *refresh_tick,
                is_canonical,
            ),
            move |(_, _, _, is_canonical)| {
                if *is_canonical {
                    let load_cause = std::mem::take(&mut *pending_load_cause.borrow_mut());
                    let is_explicit_refresh = load_cause == FilesLoadCause::ManualRefresh;
                    let generation = {
                        let mut value = request_generation.borrow_mut();
                        *value = value.saturating_add(1);
                        *value
                    };
                    let load_announcement =
                        begin_files_load((*live_announcement).clone(), load_cause);
                    live_announcement.set(load_announcement.clone());
                    page_state.dispatch(FilesAction::Loading {
                        generation,
                        explicit_refresh: is_explicit_refresh,
                    });
                    let page_state = page_state.clone();
                    let mutation_state = mutation_state.clone();
                    let request_generation = request_generation.clone();
                    let live_announcement = live_announcement.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        let result = async {
                            let workspace = org_api::get_workspace(&workspace_id).await?;
                            let page = org_api::list_documents(&workspace_id, &query_state).await?;
                            Ok::<_, OrgApiError>(FilesPayload { workspace, page })
                        }
                        .await;
                        match result {
                            Ok(payload) => {
                                let is_current = *request_generation.borrow() == generation;
                                page_state.dispatch(FilesAction::Loaded {
                                    generation,
                                    payload,
                                });
                                if is_current && is_explicit_refresh {
                                    mutation_state.set((*mutation_state).clone().refreshed());
                                }
                            }
                            Err(error) => {
                                if *request_generation.borrow() == generation {
                                    live_announcement
                                        .set(fail_files_load(load_announcement, load_cause));
                                }
                                page_state.dispatch(FilesAction::Failed { generation, error })
                            }
                        }
                    });
                }
                || ()
            },
        );
    }

    let navigate = {
        let navigator = navigator.clone();
        let route = route.clone();
        move |state: &DocumentListState| {
            if let Some(navigator) = &navigator {
                let _ = navigator.push_with_query(&route, raw_query(state));
            }
        }
    };
    let on_limit = {
        let query_state = query_state.clone();
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let workspace_id = props.workspace_id.clone();
        let navigate = navigate.clone();
        Callback::from(move |event: Event| {
            let select: HtmlSelectElement = event.target_unchecked_into();
            if let Ok(limit) = select.value().parse() {
                let mut next = query_state.clone();
                let mut history = (*pagination_history).clone();
                set_document_limit(&mut next, &mut history, limit);
                pagination_snapshots
                    .borrow_mut()
                    .reset_to(&workspace_id, &next);
                pagination_history.set(history);
                navigate(&next);
            }
        })
    };
    let on_status_filter = |status: DocumentStatus| {
        let query_state = query_state.clone();
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let workspace_id = props.workspace_id.clone();
        let navigate = navigate.clone();
        Callback::from(move |event: MouseEvent| {
            event.prevent_default();
            let (next, next_history) =
                status_filter_transition(&query_state, &pagination_history, status);
            pagination_snapshots
                .borrow_mut()
                .reset_to(&workspace_id, &next);
            pagination_history.set(next_history);
            navigate(&next);
        })
    };
    let on_active_filter = on_status_filter(DocumentStatus::Active);
    let on_archived_filter = on_status_filter(DocumentStatus::Archived);
    let on_next = {
        let query_state = query_state.clone();
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let workspace_id = props.workspace_id.clone();
        let navigate = navigate.clone();
        let cursor = payload_for_workspace(&page_state, &props.workspace_id)
            .and_then(|payload| payload.page.next_cursor.clone());
        Callback::from(move |_| {
            if let Some(cursor) = &cursor {
                let mut history = (*pagination_history).clone();
                let next = record_next_page(&mut history, &query_state, cursor);
                pagination_snapshots
                    .borrow_mut()
                    .remember(&workspace_id, &next, &history);
                pagination_history.set(history);
                navigate(&next);
            }
        })
    };
    let on_previous = {
        let pagination_history = pagination_history.clone();
        let pagination_snapshots = pagination_snapshots.clone();
        let workspace_id = props.workspace_id.clone();
        let navigate = navigate.clone();
        Callback::from(move |_| {
            let mut history = (*pagination_history).clone();
            if let Some(previous) = take_previous_page(&mut history) {
                pagination_snapshots
                    .borrow_mut()
                    .remember(&workspace_id, &previous, &history);
                pagination_history.set(history);
                navigate(&previous);
            }
        })
    };
    let on_refresh = {
        let refresh_tick = refresh_tick.clone();
        let pending_load_cause = pending_load_cause.clone();
        Callback::from(move |_| {
            *pending_load_cause.borrow_mut() = FilesLoadCause::ManualRefresh;
            refresh_tick.set((*refresh_tick).saturating_add(1));
        })
    };

    let open_add = {
        let workspace_id = props.workspace_id.clone();
        let dialog = dialog.clone();
        let draft = draft.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        Callback::from(move |_| {
            draft.set(String::new());
            validation_error.set(None);
            mutation_state.set(MutationState::default());
            dialog.set(Some(DocumentDialog {
                workspace_id: workspace_id.clone(),
                kind: DialogKind::Add,
                document_id: None,
                path: String::new(),
            }));
        })
    };
    let open_document_dialog =
        |kind: DialogKind,
         workspace_id: String,
         document: Document,
         dialog: UseStateHandle<Option<DocumentDialog>>,
         draft: UseStateHandle<String>,
         confirmation: UseStateHandle<String>,
         validation_error: UseStateHandle<Option<String>>,
         mutation_state: UseStateHandle<MutationState>| {
            draft.set(DocumentDraft::from_document(&document).path);
            confirmation.set(String::new());
            validation_error.set(None);
            mutation_state.set(MutationState::default());
            dialog.set(Some(DocumentDialog {
                workspace_id,
                kind,
                document_id: Some(document.id),
                path: document.path,
            }));
        };
    let on_rename = {
        let workspace_id = props.workspace_id.clone();
        let dialog = dialog.clone();
        let draft = draft.clone();
        let confirmation = confirmation.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        Callback::from(move |document: Document| {
            open_document_dialog(
                DialogKind::Rename,
                workspace_id.clone(),
                document,
                dialog.clone(),
                draft.clone(),
                confirmation.clone(),
                validation_error.clone(),
                mutation_state.clone(),
            )
        })
    };
    let on_archive = {
        let workspace_id = props.workspace_id.clone();
        let dialog = dialog.clone();
        let draft = draft.clone();
        let confirmation = confirmation.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        Callback::from(move |document: Document| {
            open_document_dialog(
                DialogKind::Archive,
                workspace_id.clone(),
                document,
                dialog.clone(),
                draft.clone(),
                confirmation.clone(),
                validation_error.clone(),
                mutation_state.clone(),
            )
        })
    };
    let on_restore = {
        let workspace_id = props.workspace_id.clone();
        let dialog = dialog.clone();
        let draft = draft.clone();
        let confirmation = confirmation.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        Callback::from(move |document: Document| {
            open_document_dialog(
                DialogKind::Restore,
                workspace_id.clone(),
                document,
                dialog.clone(),
                draft.clone(),
                confirmation.clone(),
                validation_error.clone(),
                mutation_state.clone(),
            )
        })
    };
    let close_dialog = {
        let dialog = dialog.clone();
        let busy = busy.clone();
        Callback::from(move |(): ()| {
            if !*busy {
                dialog.set(None);
            }
        })
    };
    let on_draft = {
        let draft = draft.clone();
        let validation_error = validation_error.clone();
        let mutation_state = mutation_state.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            let value = input.value();
            mutation_state.set((*mutation_state).clone().draft_changed(&value));
            draft.set(value);
            validation_error.set(None);
        })
    };
    let on_confirmation = {
        let confirmation = confirmation.clone();
        Callback::from(move |event: InputEvent| {
            let input: HtmlInputElement = event.target_unchecked_into();
            confirmation.set(input.value());
        })
    };

    let current_dialog = dialog_for_workspace(dialog.as_ref(), &props.workspace_id);
    let selected_document = current_dialog
        .and_then(|dialog| dialog.document_id.as_ref())
        .and_then(|document_id| {
            payload_for_workspace(&page_state, &props.workspace_id).and_then(|payload| {
                payload
                    .page
                    .items
                    .iter()
                    .find(|document| &document.id == document_id)
                    .cloned()
            })
        });
    let loaded_workspace = payload_for_workspace(&page_state, &props.workspace_id)
        .map(|payload| payload.workspace.clone());
    let on_submit = {
        let workspace = loaded_workspace.clone();
        let dialog_value = current_dialog.cloned();
        let selected_document = selected_document.clone();
        let draft_value = (*draft).clone();
        let confirmation_value = (*confirmation).clone();
        let validation_error = validation_error.clone();
        let busy = busy.clone();
        let mutation_state = mutation_state.clone();
        let page_state = page_state.clone();
        let dialog = dialog.clone();
        let pending_load_cause = pending_load_cause.clone();
        let refresh_tick = refresh_tick.clone();
        let live_announcement = live_announcement.clone();
        let current_workspace = current_workspace.clone();
        Callback::from(move |_| {
            if *busy || page_state.requires_refresh {
                return;
            }
            let (Some(workspace), Some(dialog_value)) = (&workspace, &dialog_value) else {
                return;
            };
            if !workspace_allows_mutations(workspace) {
                return;
            }
            let pending = match dialog_value.kind {
                DialogKind::Add => {
                    let draft = DocumentDraft::new(draft_value.clone());
                    if let Err(error) = draft.validate() {
                        validation_error.set(Some(error.message));
                        return;
                    }
                    PendingDocumentMutation::Create {
                        body: CreateDocumentBody::from_draft(
                            MutationSubmission::new().operation_id,
                            &draft,
                        ),
                        draft,
                    }
                }
                DialogKind::Rename => {
                    let Some(document) = &selected_document else {
                        return;
                    };
                    let draft = DocumentDraft::new(draft_value.clone());
                    if let Err(error) = draft.validate() {
                        validation_error.set(Some(error.message));
                        return;
                    }
                    PendingDocumentMutation::Rename {
                        document_id: document.id.clone(),
                        body: RenameDocumentBody::new(
                            MutationSubmission::new().operation_id,
                            workspace.id.clone(),
                            draft.path.clone(),
                            document.revision,
                        ),
                        draft,
                    }
                }
                DialogKind::Archive => {
                    let Some(document) = &selected_document else {
                        return;
                    };
                    if document.archived_at.is_some() || confirmation_value != document.path {
                        return;
                    }
                    PendingDocumentMutation::Archive {
                        document_id: document.id.clone(),
                        path: document.path.clone(),
                        body: DocumentRevisionBody::new(
                            MutationSubmission::new().operation_id,
                            workspace.id.clone(),
                            document.revision,
                        ),
                    }
                }
                DialogKind::Restore => {
                    let Some(document) = &selected_document else {
                        return;
                    };
                    if document.archived_at.is_none() {
                        return;
                    }
                    PendingDocumentMutation::Restore {
                        document_id: document.id.clone(),
                        path: document.path.clone(),
                        body: DocumentRevisionBody::new(
                            MutationSubmission::new().operation_id,
                            workspace.id.clone(),
                            document.revision,
                        ),
                    }
                }
            };
            let request = PendingDocumentRequest {
                workspace_id: workspace.id.clone(),
                mutation: pending,
            };
            submit_document_mutation(
                request,
                current_workspace.clone(),
                busy.clone(),
                mutation_state.clone(),
                page_state.clone(),
                dialog.clone(),
                pending_load_cause.clone(),
                refresh_tick.clone(),
                live_announcement.clone(),
            );
        })
    };
    let on_retry = {
        let workspace_id = props.workspace_id.clone();
        let dialog_is_current = current_dialog.is_some();
        let current_workspace = current_workspace.clone();
        let busy = busy.clone();
        let mutation_state = mutation_state.clone();
        let page_state = page_state.clone();
        let dialog = dialog.clone();
        let pending_load_cause = pending_load_cause.clone();
        let refresh_tick = refresh_tick.clone();
        let live_announcement = live_announcement.clone();
        Callback::from(move |_| {
            if *busy || !dialog_is_current {
                return;
            }
            if let Some(request) = mutation_state.retry(&workspace_id) {
                submit_document_mutation(
                    request,
                    current_workspace.clone(),
                    busy.clone(),
                    mutation_state.clone(),
                    page_state.clone(),
                    dialog.clone(),
                    pending_load_cause.clone(),
                    refresh_tick.clone(),
                    live_announcement.clone(),
                );
            }
        })
    };

    let (active_filter, _) =
        status_filter_transition(&query_state, &pagination_history, DocumentStatus::Active);
    let (archived_filter, _) =
        status_filter_transition(&query_state, &pagination_history, DocumentStatus::Archived);
    let page_size_options = page_size_options(query_state.limit);

    html! {
        <section class="stack org-files-page" aria-labelledby="org-files-title" data-testid="org-workspace-files-page">
            <header class="page-head org-files-head">
                <div>
                    <Link<Route> to={Route::OrgWorkspace { workspace_id: props.workspace_id.clone() }} classes={classes!("org-breadcrumb")}>{ "Workspace operations" }</Link<Route>>
                    <p class="org-kicker">{ "File lifecycle ledger" }</p>
                    <h1 id="org-files-title" class="page-title">
                        { loaded_workspace.as_ref().map_or("Loading files", |workspace| workspace.display_name.as_str()) }
                    </h1>
                    if let Some(workspace) = &loaded_workspace {
                        <p class="page-hint">{ format!("{} · workspace revision {}", workspace.slug, workspace.revision) }</p>
                        <span class={classes!("org-state", workspace.archived_at.is_some().then_some("is-archived"))}>
                            { if workspace.archived_at.is_some() { "Archived workspace · files read-only" } else { "Active workspace" } }
                        </span>
                    }
                </div>
                <div class="org-files-head-actions">
                    if loaded_workspace.as_ref().is_some_and(workspace_allows_mutations) {
                        <button type="button" class="btn btn-primary org-files-add-button" onclick={open_add} data-testid="org-files-add">{ "Add file" }</button>
                    }
                    <button type="button" class="btn btn-outline" onclick={on_refresh.clone()} disabled={!is_canonical || *busy} data-testid="org-files-refresh">{ "Refresh files" }</button>
                </div>
            </header>

            <div class="org-files-filter-bar">
                <nav class="org-files-status-filters" aria-label="File status">
                    <a href={files_href(&props.workspace_id, &active_filter)} onclick={on_active_filter} class={classes!((query_state.status == DocumentStatus::Active).then_some("is-active"))} aria-current={(query_state.status == DocumentStatus::Active).then_some("page")}>{ "Active" }</a>
                    <a href={files_href(&props.workspace_id, &archived_filter)} onclick={on_archived_filter} class={classes!((query_state.status == DocumentStatus::Archived).then_some("is-active"))} aria-current={(query_state.status == DocumentStatus::Archived).then_some("page")}>{ "Archived" }</a>
                </nav>
                <label class="org-page-size">
                    <span>{ "Rows" }</span>
                    <select id="org-files-page-size" name="limit" class="input" onchange={on_limit} aria-label="Files per page" value={query_state.limit.to_string()}>
                        { for page_size_options.iter().map(|(limit, selected)| html! { <option value={limit.to_string()} selected={*selected}>{ limit }</option> }) }
                    </select>
                </label>
            </div>

            <p class="org-live-status" role="status" aria-live="polite" aria-atomic="true">
                <span key={live_announcement.sequence.to_string()}>
                    { live_announcement.message.clone().unwrap_or_else(|| files_live_status(&page_state, &props.workspace_id)) }
                </span>
            </p>

            { files_content(&page_state, &props.workspace_id, on_rename, on_archive, on_restore) }

            if let Some(payload) = payload_for_workspace(&page_state, &props.workspace_id) {
                <nav class="org-cursor-nav org-files-pagination" aria-label="File pages">
                    <span>{ format!("{} file(s) on this page", payload.page.items.len()) }</span>
                    <button type="button" class="btn btn-outline" onclick={on_previous} disabled={pagination_history.is_empty()}>{ "Previous page" }</button>
                    <button type="button" class="btn btn-outline" onclick={on_next} disabled={payload.page.next_cursor.is_none()}>{ "Next page" }</button>
                </nav>
            }

            if let Some(current_dialog) = current_dialog {
                <Modal title={dialog_title(current_dialog.kind).to_owned()} on_close={close_dialog.clone()}>
                    <div class="stack org-files-dialog">
                        { dialog_content(current_dialog, selected_document.as_ref(), &draft, &confirmation, on_draft.clone(), on_confirmation, *busy) }
                        if let Some(message) = &*validation_error {
                            <p class="org-files-field-error" role="alert">{ message.clone() }</p>
                        }
                        if let Some(error) = &mutation_state.error {
                            { mutation_error_content(error, page_state.requires_refresh, on_refresh.clone()) }
                        }
                        <div class="org-workspace-form-actions">
                            <button type="button" class="btn btn-outline" onclick={{ let close_dialog = close_dialog.clone(); Callback::from(move |_| close_dialog.emit(())) }} disabled={*busy}>{ "Cancel" }</button>
                            if mutation_state.retry(&props.workspace_id).is_some() {
                                <button type="button" class="btn btn-outline" onclick={on_retry} disabled={*busy}>{ "Retry" }</button>
                            }
                            <button
                                type="button"
                                class={classes!("btn", (current_dialog.kind == DialogKind::Archive).then_some("btn-danger"), (current_dialog.kind != DialogKind::Archive).then_some("btn-primary"))}
                                onclick={on_submit}
                                disabled={*busy || page_state.requires_refresh || !dialog_can_submit(current_dialog, selected_document.as_ref(), &draft, &confirmation)}
                            >
                                { if *busy { "Working…" } else { dialog_submit_label(current_dialog.kind) } }
                            </button>
                        </div>
                    </div>
                </Modal>
            }
        </section>
    }
}

fn files_live_status(state: &FilesPageState, workspace_id: &str) -> String {
    if matches!(state.load, FilesLoad::Ready(_))
        && payload_for_workspace(state, workspace_id).is_none()
    {
        return "Loading files".to_owned();
    }
    match &state.load {
        FilesLoad::Loading => "Loading files".to_owned(),
        FilesLoad::Failed(_) => "Files could not be loaded".to_owned(),
        FilesLoad::Ready(payload) if payload.page.items.is_empty() => {
            "No files match this status".to_owned()
        }
        FilesLoad::Ready(payload) => format!("{} files loaded", payload.page.items.len()),
    }
}

fn files_content(
    state: &FilesPageState,
    workspace_id: &str,
    on_rename: Callback<Document>,
    on_archive: Callback<Document>,
    on_restore: Callback<Document>,
) -> Html {
    if matches!(state.load, FilesLoad::Ready(_))
        && payload_for_workspace(state, workspace_id).is_none()
    {
        return html! { <p class="loading">{ "Loading workspace files..." }</p> };
    }
    match &state.load {
        FilesLoad::Loading => html! { <p class="loading">{ "Loading workspace files..." }</p> },
        FilesLoad::Failed(error) => {
            let safe = error_view(error);
            html! {
                <div class="org-directory-message is-error" role="alert">
                    <strong>{ "Workspace files unavailable" }</strong>
                    <p>{ safe.message }</p>
                    <code>{ safe.code }</code>
                </div>
            }
        }
        FilesLoad::Ready(payload) if payload.page.items.is_empty() => html! {
            <div class="org-directory-message">
                <strong>{ "No files in this status" }</strong>
                <p>{ "Choose the other status or add a file to an active workspace." }</p>
            </div>
        },
        FilesLoad::Ready(payload) => html! {
            <OrgDocumentTable
                documents={payload.page.items.clone()}
                workspace_archived={payload.workspace.archived_at.is_some()}
                {on_rename}
                {on_archive}
                {on_restore}
            />
        },
    }
}

fn dialog_title(kind: DialogKind) -> &'static str {
    match kind {
        DialogKind::Add => "Add Org file",
        DialogKind::Rename => "Rename Org file",
        DialogKind::Archive => "Archive Org file",
        DialogKind::Restore => "Restore Org file",
    }
}

fn dialog_submit_label(kind: DialogKind) -> &'static str {
    match kind {
        DialogKind::Add => "Add file",
        DialogKind::Rename => "Rename file",
        DialogKind::Archive => "Archive file",
        DialogKind::Restore => "Restore file",
    }
}

fn dialog_can_submit(
    dialog: &DocumentDialog,
    document: Option<&Document>,
    draft: &str,
    confirmation: &str,
) -> bool {
    match dialog.kind {
        DialogKind::Add => !draft.is_empty(),
        DialogKind::Rename => document.is_some() && !draft.is_empty(),
        DialogKind::Archive => document.is_some_and(|document| {
            document.archived_at.is_none() && confirmation == document.path
        }),
        DialogKind::Restore => document.is_some_and(|document| document.archived_at.is_some()),
    }
}

fn dialog_content(
    dialog: &DocumentDialog,
    document: Option<&Document>,
    draft: &str,
    confirmation: &str,
    on_draft: Callback<InputEvent>,
    on_confirmation: Callback<InputEvent>,
    busy: bool,
) -> Html {
    match dialog.kind {
        DialogKind::Add | DialogKind::Rename => html! {
            <label class="org-files-field" for="org-files-path">
                <span>{ "File path" }</span>
                <input id="org-files-path" class="input org-files-path-input" value={draft.to_owned()} oninput={on_draft} disabled={busy} autocomplete="off" aria-describedby="org-files-path-hint" />
                <small id="org-files-path-hint">{ "Portable lowercase relative path ending in .org" }</small>
            </label>
        },
        DialogKind::Archive => html! {
            <div class="stack org-files-danger-confirmation">
                <p>{ format!("Archive {} at revision {}?", dialog.path, document.map_or(0, |document| document.revision)) }</p>
                <p>{ format!("Type {} to confirm.", dialog.path) }</p>
                <label class="org-files-field" for="org-files-archive-confirmation">
                    <span>{ "Exact file path" }</span>
                    <input id="org-files-archive-confirmation" class="input org-files-path-input" value={confirmation.to_owned()} oninput={on_confirmation} disabled={busy} autocomplete="off" />
                </label>
            </div>
        },
        DialogKind::Restore => html! {
            <div class="org-files-confirmation-view">
                <p>{ "Restore this archived file to the active ledger?" }</p>
                <dl>
                    <div><dt>{ "Path" }</dt><dd>{ dialog.path.clone() }</dd></div>
                    <div><dt>{ "Revision" }</dt><dd>{ document.map_or(0, |document| document.revision) }</dd></div>
                </dl>
            </div>
        },
    }
}

fn mutation_error_content(
    error: &OrgApiError,
    requires_refresh: bool,
    on_refresh: Callback<MouseEvent>,
) -> Html {
    let safe = error_view(error);
    html! {
        <div class="org-files-error" role="alert">
            <strong>{ safe.message }</strong>
            <code>{ safe.code }</code>
            <p>{ lifecycle_error_hint(&error.code) }</p>
            if !safe.work_item_ids.is_empty() {
                <p>{ "Blocking work items" }</p>
                <ul>{ for safe.work_item_ids.into_iter().map(|id| html! { <li><code>{ id }</code></li> }) }</ul>
            }
            if requires_refresh {
                <p>{ "The loaded revision is stale. Refresh before submitting this draft again." }</p>
                <button type="button" class="btn btn-outline" onclick={on_refresh}>{ "Refresh files" }</button>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use serde_json::{json, Value};
    use yew::Reducible;

    use super::*;
    use crate::org::{
        api::OrgApiError,
        document_management::{
            CreateDocumentBody, DocumentDraft, DocumentListState, DocumentStatus,
            RenameDocumentBody,
        },
        model::{Document, Page, Workspace},
    };

    const OPERATION_ID: &str = "20000000-0000-4000-8000-000000000001";

    fn workspace(archived: bool) -> Workspace {
        Workspace {
            id: "workspace-a".into(),
            slug: "delivery".into(),
            display_name: "Delivery".into(),
            description: "Operations".into(),
            timezone: "Asia/Shanghai".into(),
            policy_schema_version: 1,
            policy: crate::org::workspace_management::WorkspacePolicy::engineering_default(),
            revision: 4,
            archived_at: archived.then_some(1),
        }
    }

    fn document(revision: i64, archived: bool) -> Document {
        Document {
            id: "40000000-0000-4000-8000-000000000001".into(),
            path: "roadmap/release.org".into(),
            revision,
            archived_at: archived.then_some(1),
        }
    }

    fn payload(revision: i64) -> FilesPayload {
        FilesPayload {
            workspace: workspace(false),
            page: Page {
                items: vec![document(revision, false)],
                next_cursor: Some("next/+= page".into()),
            },
        }
    }

    fn error(code: &str, retryable: bool) -> OrgApiError {
        OrgApiError {
            code: code.into(),
            message: "Safe lifecycle message".into(),
            details: json!({"work_item_ids": ["item-a", 42], "secret": "do-not-render"}),
            retryable,
            status: Some(409),
        }
    }

    #[test]
    fn filter_limit_and_cursor_navigation_reset_or_restore_exact_state() {
        let mut same_filter =
            DocumentListState::parse("status=active&cursor=current%2Fpage&limit=25");
        let mut same_filter_history = vec![DocumentListState::default()];
        set_document_status(
            &mut same_filter,
            &mut same_filter_history,
            DocumentStatus::Active,
        );
        assert_eq!(same_filter.cursor, None);
        assert!(same_filter_history.is_empty());

        let mut current = DocumentListState::parse("status=active&cursor=page%2F1&limit=25");
        let mut history = vec![DocumentListState::default()];
        set_document_status(&mut current, &mut history, DocumentStatus::Archived);
        assert_eq!(current.cursor, None);
        assert!(history.is_empty());

        current.cursor = Some("page/2".into());
        history.push(DocumentListState::default());
        set_document_limit(&mut current, &mut history, 100);
        assert_eq!(current.cursor, None);
        assert_eq!(current.limit, 100);
        assert!(history.is_empty());

        let second = record_next_page(&mut history, &current, "next/+= page");
        assert_eq!(second.cursor.as_deref(), Some("next/+= page"));
        assert_eq!(take_previous_page(&mut history), Some(current));
        assert_eq!(take_previous_page(&mut history), None);
    }

    #[test]
    fn same_status_filter_transition_clears_the_real_pagination_history() {
        for status in [DocumentStatus::Active, DocumentStatus::Archived] {
            let current = DocumentListState {
                status,
                cursor: Some("page-2".into()),
                limit: 25,
            };
            let history = vec![DocumentListState {
                status,
                cursor: None,
                limit: 25,
            }];
            let (next, next_history) = status_filter_transition(&current, &history, status);
            assert_eq!(next.cursor, None);
            assert!(next_history.is_empty());
        }

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("on_status_filter"));
        assert!(source.contains("pagination_history.set(next_history)"));
    }

    #[test]
    fn browser_back_and_forward_restore_exact_pagination_snapshots() {
        let page_one = DocumentListState::parse("status=active&limit=25");
        let page_two = DocumentListState::parse("status=active&cursor=page-2&limit=25");
        let page_three = DocumentListState::parse("status=active&cursor=page-3&limit=25");

        let mut snapshots = PaginationSnapshots::default();
        snapshots.remember("workspace-a", &page_one, &[]);
        snapshots.remember("workspace-a", &page_two, std::slice::from_ref(&page_one));
        snapshots.remember(
            "workspace-a",
            &page_three,
            &[page_one.clone(), page_two.clone()],
        );

        let back_two = snapshots.restore("workspace-a", &page_two);
        assert_eq!(back_two, vec![page_one.clone()]);
        assert_eq!(back_two.last(), Some(&page_one));

        let back_one = snapshots.restore("workspace-a", &page_one);
        assert!(back_one.is_empty());

        let forward_two = snapshots.restore("workspace-a", &page_two);
        assert_eq!(forward_two, vec![page_one.clone()]);
        assert_eq!(forward_two.last(), Some(&page_one));

        let forward_three = snapshots.restore("workspace-a", &page_three);
        assert_eq!(forward_three, vec![page_one.clone(), page_two.clone()]);
        assert_eq!(forward_three.last(), Some(&page_two));

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains(".restore(&workspace_id, &query_state)"));
        assert!(source.contains(".remember(&workspace_id"));
        assert!(source.contains("(props.workspace_id.clone(), canonical_query.clone())"));
    }

    #[test]
    fn unknown_direct_pagination_query_clears_stale_history_and_stays_bounded() {
        let page_one = DocumentListState::parse("status=active&limit=25");
        let unknown = DocumentListState::parse("status=active&cursor=direct&limit=25");
        let mut snapshots = PaginationSnapshots::default();
        snapshots.remember("workspace-a", &page_one, &[]);

        assert!(snapshots.restore("workspace-a", &unknown).is_empty());
        for index in 0..(MAX_PAGINATION_SNAPSHOTS + 10) {
            snapshots.remember(
                "workspace-a",
                &DocumentListState {
                    cursor: Some(format!("bounded-{index}")),
                    ..page_one.clone()
                },
                &[],
            );
        }
        assert!(snapshots.entries.len() <= MAX_PAGINATION_SNAPSHOTS);
    }

    #[test]
    fn pagination_snapshots_never_cross_workspace_route_parameters() {
        let page_one = DocumentListState::parse("status=active&limit=25");
        let page_two = DocumentListState::parse("status=active&cursor=page-2&limit=25");
        let mut snapshots = PaginationSnapshots::default();

        snapshots.remember("workspace-a", &page_two, std::slice::from_ref(&page_one));
        assert!(snapshots.restore("workspace-b", &page_two).is_empty());
        assert_eq!(snapshots.restore("workspace-a", &page_two), vec![page_one]);
    }

    #[test]
    fn noncanonical_query_normalizes_before_loading_or_announcement_changes() {
        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        let load_effect = source
            .split("move |(_, _, _, is_canonical)|")
            .nth(1)
            .unwrap();
        let canonical_branch = load_effect.find("if *is_canonical").unwrap();
        let loading = load_effect.find("FilesAction::Loading").unwrap();
        let announcement = load_effect.find("begin_files_load").unwrap();
        assert!(canonical_branch < loading);
        assert!(canonical_branch < announcement);
    }

    #[test]
    fn refresh_generation_ignores_stale_async_responses() {
        let state = Rc::new(FilesPageState::default())
            .reduce(FilesAction::Loading {
                generation: 1,
                explicit_refresh: false,
            })
            .reduce(FilesAction::Loading {
                generation: 2,
                explicit_refresh: false,
            })
            .reduce(FilesAction::Loaded {
                generation: 1,
                payload: payload(1),
            });
        assert_eq!(state.generation, 2);
        assert!(matches!(state.load, FilesLoad::Loading));

        let state = state.reduce(FilesAction::Loaded {
            generation: 2,
            payload: payload(3),
        });
        assert_eq!(state.payload().unwrap().page.items[0].revision, 3);
    }

    #[test]
    fn only_the_current_explicit_refresh_response_clears_the_stale_gate() {
        let state = Rc::new(FilesPageState::default())
            .reduce(FilesAction::RequireRefresh)
            .reduce(FilesAction::Loading {
                generation: 1,
                explicit_refresh: true,
            })
            .reduce(FilesAction::Loading {
                generation: 2,
                explicit_refresh: false,
            })
            .reduce(FilesAction::Loaded {
                generation: 1,
                payload: payload(1),
            });
        assert_eq!(state.generation, 2);
        assert!(state.requires_refresh);

        let state = state.reduce(FilesAction::Loaded {
            generation: 2,
            payload: payload(2),
        });
        assert!(state.requires_refresh);

        let state = state
            .reduce(FilesAction::Loading {
                generation: 3,
                explicit_refresh: true,
            })
            .reduce(FilesAction::Loaded {
                generation: 3,
                payload: payload(3),
            });
        assert!(!state.requires_refresh);
    }

    #[test]
    fn load_and_mutation_requests_clear_success_before_repeated_announcements() {
        let first = LiveAnnouncement::default().succeeded("Created");
        assert_eq!(first.message.as_deref(), Some("Created"));

        let loading = first.clone().start_request();
        assert_eq!(loading.message, None);
        assert!(loading.sequence > first.sequence);

        let repeated = loading.succeeded("Created");
        assert_eq!(repeated.message.as_deref(), Some("Created"));
        assert!(repeated.sequence > first.sequence);

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.matches(".start_request()").count() >= 2);
        assert!(source.contains("key={live_announcement.sequence.to_string()}"));
        assert!(!source.contains("live_message"));
    }

    #[test]
    fn automatic_post_mutation_refresh_preserves_success_exactly_once() {
        let first_success = LiveAnnouncement::default()
            .start_request()
            .succeeded("Created");
        let mut pending_cause = FilesLoadCause::MutationSuccess;

        let automatic = begin_files_load(first_success.clone(), std::mem::take(&mut pending_cause));
        assert_eq!(automatic, first_success);
        assert_eq!(automatic.message.as_deref(), Some("Created"));
        assert_eq!(pending_cause, FilesLoadCause::Navigation);

        let consumed = begin_files_load(automatic.clone(), pending_cause);
        assert_eq!(consumed.message, None);
        assert!(consumed.sequence > automatic.sequence);

        for cause in [FilesLoadCause::ManualRefresh, FilesLoadCause::Navigation] {
            let cleared = begin_files_load(first_success.clone(), cause);
            assert_eq!(cleared.message, None);
            assert!(cleared.sequence > first_success.sequence);
        }

        let failed_automatic =
            fail_files_load(first_success.clone(), FilesLoadCause::MutationSuccess);
        assert_eq!(failed_automatic.message, None);

        let next_request = first_success.clone().start_request();
        let repeated = next_request.succeeded("Created");
        assert_eq!(repeated.message.as_deref(), Some("Created"));
        assert!(repeated.sequence > first_success.sequence);
    }

    #[test]
    fn transient_retry_retains_the_complete_create_body_and_identifiers() {
        let draft = DocumentDraft::new("roadmap/new.org");
        let body = CreateDocumentBody::from_draft(OPERATION_ID.into(), &draft);
        let document_id = body.document_id.clone();
        let request = PendingDocumentRequest {
            workspace_id: "workspace-a".into(),
            mutation: PendingDocumentMutation::Create {
                draft: draft.clone(),
                body,
            },
        };
        let state = MutationState::default()
            .begin(request.clone())
            .failed(error("transport_error", true));
        assert_eq!(state.retry("workspace-a"), Some(request.clone()));
        let PendingDocumentMutation::Create { body, draft: kept } =
            state.retry("workspace-a").unwrap().mutation
        else {
            panic!("expected create retry");
        };
        assert_eq!(kept, draft);
        let body: Value = serde_json::to_value(body).unwrap();
        assert_eq!(body["operation_id"], OPERATION_ID);
        assert_eq!(body["document_id"], document_id);
    }

    #[test]
    fn pending_mutations_are_bound_to_their_originating_workspace() {
        let draft = DocumentDraft::new("roadmap/new.org");
        let request = PendingDocumentRequest {
            workspace_id: "workspace-a".into(),
            mutation: PendingDocumentMutation::Create {
                body: CreateDocumentBody::from_draft(OPERATION_ID.into(), &draft),
                draft,
            },
        };
        let state = MutationState::default()
            .begin(request.clone())
            .failed(error("transport_error", true));

        assert_eq!(state.retry("workspace-a"), Some(request.clone()));
        assert_eq!(state.retry("workspace-b"), None);
        let workspace_a = WorkspaceIdentity {
            workspace_id: "workspace-a".into(),
            generation: 3,
        };
        assert!(mutation_response_is_current(&request, 3, &workspace_a));
        assert!(!mutation_response_is_current(&request, 2, &workspace_a));
        assert!(!mutation_response_is_current(
            &request,
            3,
            &WorkspaceIdentity {
                workspace_id: "workspace-b".into(),
                generation: 3,
            }
        ));
    }

    #[test]
    fn editing_a_failed_create_or_rename_invalidates_only_the_changed_request() {
        for mutation in [
            PendingDocumentMutation::Create {
                draft: DocumentDraft::new("roadmap/original.org"),
                body: CreateDocumentBody::from_draft(
                    OPERATION_ID.into(),
                    &DocumentDraft::new("roadmap/original.org"),
                ),
            },
            PendingDocumentMutation::Rename {
                document_id: document(7, false).id,
                draft: DocumentDraft::new("roadmap/original.org"),
                body: RenameDocumentBody::new(
                    OPERATION_ID.into(),
                    "workspace-a".into(),
                    "roadmap/original.org".into(),
                    7,
                ),
            },
        ] {
            let request = PendingDocumentRequest {
                workspace_id: "workspace-a".into(),
                mutation,
            };
            let failed = MutationState::default()
                .begin(request.clone())
                .failed(error("transport_error", true));

            let unchanged = failed.clone().draft_changed("roadmap/original.org");
            assert_eq!(unchanged.retry("workspace-a"), Some(request.clone()));

            let changed = failed.draft_changed("roadmap/changed.org");
            assert_eq!(changed.retry("workspace-a"), None);
            assert!(changed.error.is_none());

            let replacement_mutation = match &request.mutation {
                PendingDocumentMutation::Create { .. } => PendingDocumentMutation::Create {
                    draft: DocumentDraft::new("roadmap/changed.org"),
                    body: CreateDocumentBody::from_draft(
                        "20000000-0000-4000-8000-000000000002".into(),
                        &DocumentDraft::new("roadmap/changed.org"),
                    ),
                },
                PendingDocumentMutation::Rename { document_id, .. } => {
                    PendingDocumentMutation::Rename {
                        document_id: document_id.clone(),
                        draft: DocumentDraft::new("roadmap/changed.org"),
                        body: RenameDocumentBody::new(
                            "20000000-0000-4000-8000-000000000002".into(),
                            "workspace-a".into(),
                            "roadmap/changed.org".into(),
                            7,
                        ),
                    }
                }
                PendingDocumentMutation::Archive { .. }
                | PendingDocumentMutation::Restore { .. } => unreachable!(),
            };
            let replacement = PendingDocumentRequest {
                workspace_id: "workspace-a".into(),
                mutation: replacement_mutation,
            };
            assert_ne!(replacement, request);
            let serialized = match replacement.mutation {
                PendingDocumentMutation::Create { body, .. } => serde_json::to_value(body).unwrap(),
                PendingDocumentMutation::Rename { body, .. } => serde_json::to_value(body).unwrap(),
                PendingDocumentMutation::Archive { .. }
                | PendingDocumentMutation::Restore { .. } => unreachable!(),
            };
            assert_eq!(
                serialized["operation_id"],
                "20000000-0000-4000-8000-000000000002"
            );
            assert!(
                serialized["path"] == "roadmap/changed.org"
                    || serialized["new_path"] == "roadmap/changed.org"
            );
        }
    }

    #[test]
    fn workspace_change_clears_loaded_and_stale_page_state_with_a_new_generation() {
        let state = Rc::new(FilesPageState {
            generation: 4,
            load: FilesLoad::Ready(payload(7)),
            requires_refresh: true,
            refresh_generation: Some(4),
        })
        .reduce(FilesAction::WorkspaceChanged { generation: 5 });

        assert_eq!(state.generation, 5);
        assert!(matches!(state.load, FilesLoad::Loading));
        assert!(!state.requires_refresh);
        assert_eq!(state.refresh_generation, None);
    }

    #[test]
    fn dialog_is_hidden_synchronously_when_the_workspace_prop_changes() {
        let dialog = DocumentDialog {
            workspace_id: "workspace-a".into(),
            kind: DialogKind::Add,
            document_id: None,
            path: String::new(),
        };

        assert_eq!(
            dialog_for_workspace(Some(&dialog), "workspace-a"),
            Some(&dialog)
        );
        assert_eq!(dialog_for_workspace(Some(&dialog), "workspace-b"), None);

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for guard in [
            "let current_dialog = dialog_for_workspace(dialog.as_ref(), &props.workspace_id)",
            "let dialog_value = current_dialog.cloned()",
            "let dialog_is_current = current_dialog.is_some()",
            "if let Some(current_dialog) = current_dialog",
        ] {
            assert!(
                source.contains(guard),
                "missing dialog origin guard: {guard}"
            );
        }
    }

    #[test]
    fn stale_loaded_payload_is_not_renderable_for_a_new_workspace_prop() {
        let state = FilesPageState {
            generation: 4,
            load: FilesLoad::Ready(payload(7)),
            requires_refresh: false,
            refresh_generation: None,
        };
        assert!(payload_for_workspace(&state, "workspace-a").is_some());
        assert!(payload_for_workspace(&state, "workspace-b").is_none());

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.matches("payload_for_workspace").count() >= 4);
    }

    #[test]
    fn workspace_change_resets_dialog_mutation_and_pagination_component_state() {
        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for reset in [
            "dialog.set(None)",
            "draft.set(String::new())",
            "confirmation.set(String::new())",
            "validation_error.set(None)",
            "mutation_state.set(MutationState::default())",
            "busy.set(false)",
            "live_announcement.set(LiveAnnouncement::default())",
            "pagination_history.set(Vec::new())",
            "pagination_snapshots.borrow_mut().clear()",
            "FilesAction::WorkspaceChanged",
        ] {
            assert!(source.contains(reset), "missing workspace reset: {reset}");
        }
        assert!(source.contains("mutation_response_is_current"));
        assert!(source.contains("workspace_generation"));
        assert!(source.contains("org_api::create_document(&request.workspace_id"));
    }

    #[test]
    fn stale_revision_preserves_draft_but_disables_retry_until_refresh() {
        let draft = DocumentDraft::new("roadmap/v2.org");
        let request = PendingDocumentRequest {
            workspace_id: "workspace-a".into(),
            mutation: PendingDocumentMutation::Rename {
                document_id: document(7, false).id,
                draft: draft.clone(),
                body: RenameDocumentBody::new(
                    OPERATION_ID.into(),
                    "workspace-a".into(),
                    draft.path.clone(),
                    7,
                ),
            },
        };
        let stale = MutationState::default()
            .begin(request)
            .failed(error("stale_revision", false));
        assert_eq!(stale.draft(), Some(&draft));
        assert_eq!(stale.retry("workspace-a"), None);

        let refreshed = stale.refreshed();
        assert!(refreshed.error.is_none());
        let replacement = refreshed.begin(PendingDocumentRequest {
            workspace_id: "workspace-a".into(),
            mutation: PendingDocumentMutation::Rename {
                document_id: document(9, false).id,
                draft: draft.clone(),
                body: RenameDocumentBody::new(
                    "20000000-0000-4000-8000-000000000002".into(),
                    "workspace-a".into(),
                    draft.path,
                    9,
                ),
            },
        });
        let Some(PendingDocumentRequest {
            mutation: PendingDocumentMutation::Rename { body, .. },
            ..
        }) = replacement.pending
        else {
            panic!("expected new rename submission");
        };
        let serialized = serde_json::to_value(body).unwrap();
        assert_eq!(serialized["expected_revision"], 9);
        assert_ne!(serialized["operation_id"], OPERATION_ID);
    }

    #[test]
    fn structured_error_view_exposes_only_safe_blocker_ids() {
        let view = error_view(&error("active_lease", false));
        assert_eq!(view.code, "active_lease");
        assert_eq!(view.message, "Safe lifecycle message");
        assert_eq!(view.work_item_ids, vec!["item-a"]);
        assert!(!format!("{view:?}").contains("secret"));
        assert!(lifecycle_error_hint("invalid_input").contains("path"));
        assert!(lifecycle_error_hint("document_path_conflict").contains("different"));
        assert!(lifecycle_error_hint("archived_workspace").contains("read-only"));
        assert!(lifecycle_error_hint("active_lease").contains("Blocking"));
        assert!(lifecycle_error_hint("stale_revision").contains("Refresh"));
    }

    #[test]
    fn workspace_and_document_lifecycle_gate_every_dialog_action() {
        assert!(workspace_allows_mutations(&workspace(false)));
        assert!(!workspace_allows_mutations(&workspace(true)));

        let active = document(7, false);
        let archived = document(8, true);
        let archive = DocumentDialog {
            workspace_id: "workspace-a".into(),
            kind: DialogKind::Archive,
            document_id: Some(active.id.clone()),
            path: active.path.clone(),
        };
        assert!(dialog_can_submit(
            &archive,
            Some(&active),
            "",
            "roadmap/release.org"
        ));
        assert!(!dialog_can_submit(
            &archive,
            Some(&active),
            "",
            "Roadmap/release.org"
        ));

        let restore = DocumentDialog {
            workspace_id: "workspace-a".into(),
            kind: DialogKind::Restore,
            document_id: Some(archived.id.clone()),
            path: archived.path.clone(),
        };
        assert!(dialog_can_submit(&restore, Some(&archived), "", ""));
        assert!(!dialog_can_submit(&restore, Some(&active), "", ""));

        for document in [&active, &archived] {
            let rename = DocumentDialog {
                workspace_id: "workspace-a".into(),
                kind: DialogKind::Rename,
                document_id: Some(document.id.clone()),
                path: document.path.clone(),
            };
            assert!(dialog_can_submit(
                &rename,
                Some(document),
                "roadmap/renamed.org",
                ""
            ));
        }
    }

    #[test]
    fn page_source_is_status_scoped_accessible_and_lifecycle_only() {
        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        for required in [
            "DocumentListState::parse",
            "org_api::get_workspace",
            "org_api::list_documents",
            "org_api::create_document",
            "org_api::rename_document",
            "org_api::archive_document",
            "org_api::restore_document",
            "Add file",
            "Refresh files",
            "Created",
            "Renamed",
            "Archived",
            "Restored",
            "<Modal",
            "role=\"status\"",
            "aria-live=\"polite\"",
        ] {
            assert!(
                source.contains(required),
                "missing page contract: {required}"
            );
        }
        for forbidden in [
            "textarea",
            "Request::put",
            "Request::delete",
            "/mcp",
            "set_interval",
            "set_timeout",
            "Interval::",
            "Timeout::",
            "fencing_token",
            "authorization",
        ] {
            assert!(
                !source.contains(forbidden),
                "forbidden page surface: {forbidden}"
            );
        }
    }

    #[test]
    fn files_styles_define_dense_focusable_and_responsive_ledger_contracts() {
        let css = include_str!("../../app.css");
        for required in [
            ".org-files-page",
            ".org-files-filter-bar",
            ".org-files-status-filters",
            ".org-files-path-input",
            ".org-files-row-actions",
            ".org-files-status",
            ".org-files-page :focus-visible",
            "min-height: 2.75rem",
            "@media (max-width: 620px)",
            ".org-file-row",
        ] {
            assert!(
                css.contains(required),
                "missing files CSS contract: {required}"
            );
        }
    }

    #[test]
    fn files_document_and_page_expose_language_and_one_primary_heading() {
        let index = include_str!("../../index.html");
        assert!(index.contains("<html lang=\"en\">"));

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert_eq!(source.matches("<h1").count(), 1);
        assert!(source.contains("<h1 id=\"org-files-title\" class=\"page-title\">"));
        assert!(source.contains("aria-labelledby=\"org-files-title\""));

        let modal = include_str!("../components/modal.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(modal.contains("<h2 id=\"app-modal-title\""));
        assert!(!modal.contains("<h1"));

        assert_eq!(
            Route::OrgWorkspaceFiles {
                workspace_id: "workspace-a".into(),
            }
            .document_title(),
            "Org files workspace-a | agent-note"
        );
    }

    #[test]
    fn page_size_control_has_stable_browser_form_identity() {
        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains(
            "<select id=\"org-files-page-size\" name=\"limit\" class=\"input\" onchange={on_limit} aria-label=\"Files per page\""
        ));
        assert!(source.contains("<input id=\"org-files-path\""));
        assert!(source.contains("<input id=\"org-files-archive-confirmation\""));
    }

    #[test]
    fn every_allowed_page_size_marks_only_the_current_limit_selected() {
        for current in DOCUMENT_ALLOWED_LIMITS {
            let options = page_size_options(current);
            assert_eq!(options.len(), DOCUMENT_ALLOWED_LIMITS.len());
            assert_eq!(
                options
                    .iter()
                    .filter_map(|(limit, selected)| selected.then_some(*limit))
                    .collect::<Vec<_>>(),
                vec![current]
            );
        }

        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(source.contains("let page_size_options = page_size_options(query_state.limit);"));
        assert!(source.contains("selected={*selected}"));
    }

    #[test]
    fn files_contrast_rules_are_scoped_to_breadcrumb_and_add_action() {
        let source = include_str!("org_workspace_files.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert!(
            source.contains("class=\"btn btn-primary org-files-add-button\" onclick={open_add}")
        );

        let css = include_str!("../../app.css");
        assert!(css.contains(".org-files-page .org-breadcrumb {"));
        assert!(css.contains("color: #7a4a00;"));
        assert!(css.contains(".org-files-add-button {"));
        assert!(css.contains("background: #8a5200;"));
        assert!(css.contains("border-color: #8a5200;"));
        assert!(css.contains("color: #ffffff;"));
        assert!(css.contains(".org-files-add-button:hover,"));
        assert!(css.contains("background: #7a4a00;"));
        assert!(css.contains("[data-theme=\"moonlight\"] .org-files-page .org-breadcrumb"));
        assert!(css.contains(":root:not([data-theme]) .org-files-page .org-breadcrumb"));
    }
}
