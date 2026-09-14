use axum::Router;
use std::collections::BTreeMap;
use utoipa::{
    openapi::{
        path::ParameterIn,
        schema::{
            AnyOfBuilder, Array, ArrayBuilder, Object, ObjectBuilder, OneOfBuilder, Schema,
            SchemaType, Type,
        },
        Info, KnownFormat, OpenApi, OpenApiBuilder, Ref, RefOr, Required, SchemaFormat, Tag,
    },
    PartialSchema, ToSchema,
};
use utoipa_axum::router::OpenApiRouter;
use utoipa_swagger_ui::SwaggerUi;

pub(crate) struct Binary;

impl PartialSchema for Binary {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
            .into()
    }
}

impl ToSchema for Binary {}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct OrgDocumentLifecycleData {
    pub document_id: String,
    pub path: String,
    #[schema(required = true)]
    pub archived_at: Option<i64>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct OrgDocumentLifecycleCommandResult {
    pub schema_version: u32,
    pub workspace_id: String,
    pub operation_id: String,
    pub event_ids: Vec<String>,
    #[schema(required = true)]
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub data: OrgDocumentLifecycleData,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SystemConfigSchema {
    /// Ordered category expressions. `key` includes all values; `key=value` includes exactly that value. The first `=` separates key and value. Keys must exist in the label catalog and have no leading or trailing whitespace; values are preserved byte-for-byte.
    #[schema(required = false, default = json!([]))]
    pub category_labels: Vec<String>,
    #[schema(required = false, default = json!({"minimum_score": 0.01}))]
    pub search: SearchConfigSchema,
    #[schema(
        required = false,
        default = json!({"enabled": false, "rules": []})
    )]
    pub duplicate_check: DuplicateCheckConfigSchema,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SearchConfigSchema {
    #[schema(required = false, default = 0.01, minimum = 0.0)]
    pub minimum_score: f32,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckConfigSchema {
    #[schema(required = false, default = false)]
    pub enabled: bool,
    #[schema(required = false, default = json!([]), max_items = 64)]
    pub rules: Vec<DuplicateCheckRuleSchema>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckRuleSchema {
    #[schema(min_items = 1, max_items = 32)]
    pub terms: Vec<DuplicateCheckTermSchema>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct DuplicateCheckTermSchema {
    pub key: String,
    #[schema(required = false)]
    pub value: Option<String>,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub(crate) struct SystemInfoSchema {
    pub database_engine: String,
    #[schema(required = true)]
    pub database_path: Option<String>,
    #[schema(required = true)]
    pub database_size_bytes: Option<u64>,
    pub attachments_engine: String,
    #[schema(required = true)]
    pub attachments_location: Option<String>,
    pub embedding_engine: String,
    pub embedding_model: String,
    pub embedding_fingerprint: String,
}

pub(crate) fn label_pairs_schema() -> Array {
    Array::new(
        ArrayBuilder::new()
            .items(Object::with_type(Type::String))
            .min_items(Some(2))
            .max_items(Some(2))
            .build(),
    )
}

pub(crate) fn label_pairs_with_empty_default_schema() -> Array {
    ArrayBuilder::from(label_pairs_schema())
        .default(Some(serde_json::json!([])))
        .build()
}

pub(crate) fn label_value_type_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some([
            "text", "number", "version", "date", "datetime", "time",
        ]))
        .description(Some("Label value type"))
        .build()
}

pub(crate) fn note_content_type_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .enum_values(Some(["html"]))
        .description(Some("Optional output format; omission returns Markdown."))
        .build()
}

pub fn rest_router() -> (Router<crate::AppState>, OpenApi) {
    let openapi = OpenApiBuilder::new()
        .info(Info::new("Agent Note HTTP API", env!("CARGO_PKG_VERSION")))
        .tags(Some([
            tag(
                "notes",
                "Create, list, retrieve, update, search, and download notes",
            ),
            tag("trash", "Restore or permanently delete trashed notes"),
            tag("dashboard", "Read dashboard summary data"),
            tag("rendering", "Render Markdown as HTML"),
            tag("export", "Download notes in portable formats"),
            tag("labels", "Manage the label-key catalog"),
            tag("system", "Read and update system configuration and backups"),
            tag(
                "org",
                "Manage Org workspaces, documents, work items, and workflows",
            ),
        ]))
        .build();

    let (router, openapi) = OpenApiRouter::with_openapi(openapi)
        .merge(crate::notes_api::notes_router::<crate::AppState>())
        .merge(crate::labels_api::labels_router::<crate::AppState>())
        .merge(crate::system_api::system_router::<crate::AppState>())
        .merge(crate::export_api::export_router::<crate::AppState>())
        .merge(crate::org_api::router())
        .split_for_parts();
    (
        router,
        normalize_org_success_schemas(normalize_org_query_parameters(openapi)),
    )
}

fn scalar(kind: Type) -> RefOr<Schema> {
    Schema::Object(ObjectBuilder::new().schema_type(kind).build()).into()
}

fn nullable_scalar(kind: Type) -> RefOr<Schema> {
    Schema::Object(
        ObjectBuilder::new()
            .schema_type([kind, Type::Null].into_iter().collect::<SchemaType>())
            .build(),
    )
    .into()
}

fn schema_ref(name: &str) -> RefOr<Schema> {
    Ref::from_schema_name(name).into()
}

fn nullable_ref(name: &str) -> RefOr<Schema> {
    Schema::OneOf(
        OneOfBuilder::new()
            .item(Ref::from_schema_name(name))
            .item(ObjectBuilder::new().schema_type(Type::Null))
            .build(),
    )
    .into()
}

fn enum_string(values: &[&str]) -> RefOr<Schema> {
    Schema::Object(
        ObjectBuilder::new()
            .schema_type(Type::String)
            .enum_values(Some(values.iter().copied()))
            .build(),
    )
    .into()
}

fn array_of(schema: RefOr<Schema>) -> RefOr<Schema> {
    Schema::Array(Array::new(schema)).into()
}

fn typed_object(fields: Vec<(&str, RefOr<Schema>, bool)>) -> RefOr<Schema> {
    let mut schema = ObjectBuilder::new().schema_type(Type::Object);
    for (name, field, required) in fields {
        schema = schema.property(name, field);
        if required {
            schema = schema.required(name);
        }
    }
    Schema::Object(schema.build()).into()
}

fn command_schema(data_schema: &str) -> RefOr<Schema> {
    typed_object(vec![
        ("schema_version", scalar(Type::Integer), true),
        ("workspace_id", scalar(Type::String), true),
        ("operation_id", scalar(Type::String), true),
        ("event_ids", array_of(scalar(Type::String)), true),
        ("workspace_revision", nullable_scalar(Type::Integer), true),
        (
            "document_revisions",
            Schema::Object(
                ObjectBuilder::new()
                    .schema_type(Type::Object)
                    .additional_properties(Some(ObjectBuilder::new().schema_type(Type::Integer)))
                    .build(),
            )
            .into(),
            true,
        ),
        ("data", schema_ref(data_schema), true),
    ])
}

fn page_schema(item_schema: &str) -> RefOr<Schema> {
    typed_object(vec![
        ("items", array_of(schema_ref(item_schema)), true),
        ("next_cursor", nullable_scalar(Type::String), true),
    ])
}

fn org_success_schema_name(operation_id: &str) -> &'static str {
    match operation_id {
        "org_list_workspaces" => "OrgWorkspacePageResult",
        "org_create_workspace" | "org_update_workspace" => "OrgWorkspaceCommandResult",
        "org_get_workspace" => "OrgWorkspaceResult",
        "org_archive_workspace" => "OrgArchiveCommandResult",
        "org_list_documents" => "OrgDocumentPageResult",
        "org_get_document" => "OrgDocumentSourceResult",
        "org_put_document" | "org_import_workspace" => "OrgDocumentCountCommandResult",
        "org_create_document"
        | "org_rename_document"
        | "org_archive_document"
        | "org_restore_document" => "OrgDocumentLifecycleCommandResult",
        "org_move_document" => "OrgMoveDocumentCommandResult",
        "org_move_item" => "OrgMoveItemCommandResult",
        "org_export_workspace" => "OrgWorkspaceExportResult",
        "org_create_item"
        | "org_create_follow_up"
        | "org_assign_item"
        | "org_schedule_item"
        | "org_add_dependency"
        | "org_remove_dependency"
        | "org_link_note"
        | "org_unlink_note" => "OrgItemCommandResult",
        "org_get_item" => "OrgItemResult",
        "org_get_item_context" => "OrgItemContextResult",
        "org_query_queue" | "org_query_agenda" => "OrgOperationalPageResult",
        "org_claim_item" | "org_retry_item" => "OrgClaimResult",
        "org_heartbeat_claim" => "OrgHeartbeatCommandResult",
        "org_release_claim"
        | "org_report_progress"
        | "org_submit_result"
        | "org_transition_item"
        | "org_request_review"
        | "org_approve_item"
        | "org_reject_item" => "OrgContextCommandResult",
        "org_list_note_work_items" => "OrgItemPageResult",
        "org_list_events" => "OrgEventPageResult",
        _ => panic!("unknown Org operation {operation_id}"),
    }
}

fn normalize_org_success_schemas(mut openapi: OpenApi) -> OpenApi {
    let components = openapi
        .components
        .as_mut()
        .expect("generated OpenAPI components");
    let string = || scalar(Type::String);
    let integer = || scalar(Type::Integer);
    let boolean = || scalar(Type::Boolean);
    let json = || schema_ref("OrgJsonValue");
    let add = |components: &mut utoipa::openapi::schema::Components,
               name: &str,
               schema: RefOr<Schema>| {
        components.schemas.insert(name.to_owned(), schema);
    };

    add(
        components,
        "OrgJsonValue",
        Schema::AnyOf(
            AnyOfBuilder::new()
                .item(ObjectBuilder::new().schema_type(Type::Null))
                .item(ObjectBuilder::new().schema_type(Type::Boolean))
                .item(ObjectBuilder::new().schema_type(Type::Number))
                .item(ObjectBuilder::new().schema_type(Type::String))
                .item(ArrayBuilder::new().items(Ref::from_schema_name("OrgJsonValue")))
                .item(
                    ObjectBuilder::new()
                        .schema_type(Type::Object)
                        .additional_properties(Some(Ref::from_schema_name("OrgJsonValue"))),
                )
                .build(),
        )
        .into(),
    );
    add(
        components,
        "OrgTagRule",
        typed_object(vec![
            ("allowed", array_of(string()), true),
            ("required", array_of(string()), true),
        ]),
    );
    add(
        components,
        "OrgWorkspacePolicy",
        typed_object(vec![
            ("allow_cross_workspace_agenda", boolean(), true),
            ("allowed_types", array_of(string()), true),
            ("states", array_of(string()), true),
            ("transitions", array_of(array_of(string())), true),
            ("initial_state", string(), true),
            ("running_state", string(), true),
            ("executable_states", array_of(string()), true),
            ("review_state", string(), true),
            ("failed_state", string(), true),
            ("cancelled_state", string(), true),
            ("successful_terminal_states", array_of(string()), true),
            ("terminal_states", array_of(string()), true),
            ("release_state", string(), true),
            ("review_rejection_state", string(), true),
            ("lease_expiry_recovery_state", string(), true),
            ("review_required_types", array_of(string()), true),
            ("claim_policy", string(), true),
            ("lease_duration_secs", integer(), true),
            ("retry_limit", integer(), true),
            ("concurrency_limit", integer(), true),
            (
                "tag_rules",
                Schema::Object(
                    ObjectBuilder::new()
                        .schema_type(Type::Object)
                        .additional_properties(Some(Ref::from_schema_name("OrgTagRule")))
                        .build(),
                )
                .into(),
                true,
            ),
        ]),
    );
    add(
        components,
        "OrgCounts",
        typed_object(
            [
                "ready",
                "assigned",
                "running",
                "blocked",
                "review",
                "scheduled",
                "upcoming_deadline",
                "failed",
                "expired_lease",
                "completed",
            ]
            .into_iter()
            .map(|name| (name, integer(), true))
            .collect(),
        ),
    );
    add(
        components,
        "OrgTimestamp",
        typed_object(vec![
            ("raw", string(), true),
            ("local", string(), true),
            ("timezone", string(), true),
            ("utc_timestamp", integer(), true),
        ]),
    );
    add(
        components,
        "OrgLease",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("work_item_id", string(), true),
            ("attempt_id", string(), true),
            ("kind", string(), true),
            ("actor_id", string(), true),
            ("acquired_at", integer(), true),
            ("last_heartbeat_at", integer(), true),
            ("expires_at", integer(), true),
            ("status", string(), true),
        ]),
    );

    add(
        components,
        "OrgWorkspaceResult",
        typed_object(vec![
            ("id", string(), true),
            ("slug", string(), true),
            ("display_name", string(), true),
            ("description", string(), true),
            ("timezone", string(), true),
            ("policy_schema_version", integer(), true),
            ("policy", schema_ref("OrgWorkspacePolicy"), true),
            ("revision", integer(), true),
            ("archived_at", nullable_scalar(Type::Integer), true),
        ]),
    );
    add(
        components,
        "OrgWorkspaceSummary",
        typed_object(vec![
            ("workspace_id", string(), true),
            ("slug", string(), true),
            ("display_name", string(), true),
            ("description", string(), true),
            ("timezone", string(), true),
            ("archived_at", nullable_scalar(Type::Integer), true),
            ("workspace_revision", integer(), true),
            ("evaluated_at", integer(), true),
            ("counts", schema_ref("OrgCounts"), true),
        ]),
    );
    add(
        components,
        "OrgDocumentResult",
        typed_object(vec![
            ("id", string(), true),
            ("path", string(), true),
            ("revision", integer(), true),
            ("archived_at", nullable_scalar(Type::Integer), true),
        ]),
    );
    add(
        components,
        "OrgDocumentSourceResult",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("path", string(), true),
            ("source", string(), true),
            ("content_hash", string(), true),
            ("revision", integer(), true),
            ("archived_at", nullable_scalar(Type::Integer), true),
        ]),
    );
    add(
        components,
        "OrgItemResult",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("document_id", string(), true),
            ("parent_id", nullable_scalar(Type::String), true),
            ("item_type", string(), true),
            ("title", string(), true),
            ("state", nullable_scalar(Type::String), true),
            ("priority", nullable_scalar(Type::String), true),
            ("scheduled", nullable_ref("OrgTimestamp"), true),
            ("deadline", nullable_ref("OrgTimestamp"), true),
            ("assignee", nullable_scalar(Type::String), true),
            ("requires_review", boolean(), true),
            ("created_at", integer(), true),
            ("tags", array_of(string()), true),
        ]),
    );
    add(
        components,
        "OrgEventResult",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("sequence", integer(), true),
            ("subject_kind", string(), true),
            ("subject_id", string(), true),
            ("actor_id", string(), true),
            ("attempt_id", nullable_scalar(Type::String), true),
            ("event_type", string(), true),
            ("occurred_at", integer(), true),
            ("summary", string(), true),
            ("metadata", json(), true),
            ("previous_state", nullable_scalar(Type::String), true),
            ("resulting_state", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgDependency",
        typed_object(vec![
            ("item", schema_ref("OrgItemResult"), true),
            ("satisfied", boolean(), true),
        ]),
    );
    add(
        components,
        "OrgNoteLink",
        typed_object(vec![
            ("purpose", string(), true),
            ("note_id", string(), true),
            ("description", string(), true),
            ("available", boolean(), true),
        ]),
    );
    add(
        components,
        "OrgAttemptNote",
        typed_object(vec![
            ("purpose", string(), true),
            ("note_id", string(), true),
            ("description", string(), true),
        ]),
    );
    add(
        components,
        "OrgArtifact",
        typed_object(vec![
            ("uri", string(), true),
            ("media_type", string(), true),
            ("name", string(), true),
            ("description", string(), true),
        ]),
    );
    add(
        components,
        "OrgAttempt",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("work_item_id", string(), true),
            ("attempt_number", integer(), true),
            ("actor_id", string(), true),
            ("status", string(), true),
            ("started_at", integer(), true),
            ("ended_at", nullable_scalar(Type::Integer), true),
            ("error", nullable_scalar(Type::String), true),
            ("result_summary", nullable_scalar(Type::String), true),
            ("review_outcome", nullable_scalar(Type::String), true),
            ("note_refs", array_of(schema_ref("OrgAttemptNote")), true),
            ("artifacts", array_of(schema_ref("OrgArtifact")), true),
            ("metadata", json(), true),
        ]),
    );
    add(
        components,
        "OrgOriginWorkItem",
        typed_object(vec![
            ("kind", enum_string(&["work_item"]), true),
            ("work_item_id", string(), true),
            ("item", nullable_ref("OrgItemResult"), true),
        ]),
    );
    add(
        components,
        "OrgOriginEvent",
        typed_object(vec![
            ("kind", enum_string(&["event"]), true),
            ("event_id", string(), true),
            ("event", nullable_ref("OrgEventResult"), true),
        ]),
    );
    add(
        components,
        "OrgOrigin",
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("OrgOriginWorkItem"))
                .item(Ref::from_schema_name("OrgOriginEvent"))
                .build(),
        )
        .into(),
    );
    add(
        components,
        "OrgHistorySegment",
        typed_object(vec![
            ("workspace_id", string(), true),
            ("events", array_of(schema_ref("OrgEventResult")), true),
        ]),
    );
    add(
        components,
        "OrgAttemptBudget",
        typed_object(vec![
            ("execution_attempt_count", integer(), true),
            ("max_attempts", integer(), true),
            ("remaining_attempts", integer(), true),
            ("retry_exhausted", boolean(), true),
        ]),
    );
    add(
        components,
        "OrgRecovery",
        typed_object(vec![
            ("eligible", boolean(), true),
            ("candidate", boolean(), true),
            ("blockers", array_of(string()), true),
        ]),
    );
    add(
        components,
        "OrgOperationalContext",
        typed_object(vec![
            ("classifications", array_of(string()), true),
            ("readiness", nullable_scalar(Type::String), true),
            ("blockers", array_of(string()), true),
            ("attempt_budget", schema_ref("OrgAttemptBudget"), true),
            ("recovery", schema_ref("OrgRecovery"), true),
        ]),
    );
    add(
        components,
        "OrgItemContextResult",
        typed_object(vec![
            ("workspace", schema_ref("OrgWorkspaceResult"), true),
            ("workspace_revision", integer(), true),
            ("document", schema_ref("OrgDocumentResult"), true),
            ("item", schema_ref("OrgItemResult"), true),
            ("parent", nullable_ref("OrgItemResult"), true),
            ("children", array_of(schema_ref("OrgItemResult")), true),
            ("dependencies", array_of(schema_ref("OrgDependency")), true),
            ("note_links", array_of(schema_ref("OrgNoteLink")), true),
            ("attempts", array_of(schema_ref("OrgAttempt")), true),
            ("origin", nullable_ref("OrgOrigin"), true),
            (
                "history_segments",
                array_of(schema_ref("OrgHistorySegment")),
                true,
            ),
            ("lease", nullable_ref("OrgLease"), true),
            ("operational", schema_ref("OrgOperationalContext"), true),
        ]),
    );
    add(
        components,
        "OrgOperationalItem",
        typed_object(vec![
            ("item", schema_ref("OrgItemResult"), true),
            ("attempt_count", integer(), true),
            (
                "current_attempt_status",
                nullable_scalar(Type::String),
                true,
            ),
            ("retry_exhausted", boolean(), true),
            ("ready_status", nullable_scalar(Type::String), true),
            ("review_lease_status", nullable_scalar(Type::String), true),
            ("lease", nullable_ref("OrgLease"), true),
            ("completion_at", nullable_scalar(Type::Integer), true),
        ]),
    );
    add(
        components,
        "OrgWorkspaceRevisionData",
        typed_object(vec![
            ("workspace_id", string(), true),
            ("revision", integer(), true),
        ]),
    );
    add(
        components,
        "OrgArchiveData",
        typed_object(vec![
            ("workspace_id", string(), true),
            ("archived_at", integer(), true),
        ]),
    );
    add(
        components,
        "OrgDocumentCountData",
        typed_object(vec![("document_count", integer(), true)]),
    );
    add(
        components,
        "OrgDocumentLifecycleData",
        typed_object(vec![
            ("document_id", string(), true),
            ("path", string(), true),
            ("archived_at", nullable_scalar(Type::Integer), true),
        ]),
    );
    add(
        components,
        "OrgMoveDocumentData",
        typed_object(vec![
            ("document_id", string(), true),
            ("source_workspace_id", string(), true),
            ("target_workspace_id", string(), true),
            ("source_workspace_revision", integer(), true),
            ("target_workspace_revision", integer(), true),
        ]),
    );
    add(
        components,
        "OrgResultingNoteLink",
        typed_object(vec![
            ("purpose", string(), true),
            ("note_id", string(), true),
            ("description", string(), true),
        ]),
    );
    add(
        components,
        "OrgResultingItem",
        typed_object(vec![
            ("id", string(), true),
            ("workspace_id", string(), true),
            ("document_id", string(), true),
            ("parent_id", nullable_scalar(Type::String), true),
            ("item_type", string(), true),
            ("title", string(), true),
            ("state", nullable_scalar(Type::String), true),
            ("priority", nullable_scalar(Type::String), true),
            ("scheduled", nullable_scalar(Type::String), true),
            ("deadline", nullable_scalar(Type::String), true),
            ("assignee", nullable_scalar(Type::String), true),
            ("requires_review", boolean(), true),
            ("created_at", integer(), true),
            ("tags", array_of(string()), true),
            ("dependencies", array_of(string()), true),
            (
                "note_links",
                array_of(schema_ref("OrgResultingNoteLink")),
                true,
            ),
        ]),
    );
    add(
        components,
        "OrgFollowUpOriginWorkItem",
        typed_object(vec![
            ("kind", enum_string(&["work_item"]), true),
            ("schema_version", integer(), true),
            ("work_item_id", string(), true),
        ]),
    );
    add(
        components,
        "OrgFollowUpOriginEvent",
        typed_object(vec![
            ("kind", enum_string(&["event"]), true),
            ("schema_version", integer(), true),
            ("event_id", string(), true),
        ]),
    );
    add(
        components,
        "OrgFollowUpOrigin",
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("OrgFollowUpOriginWorkItem"))
                .item(Ref::from_schema_name("OrgFollowUpOriginEvent"))
                .build(),
        )
        .into(),
    );
    add(
        components,
        "OrgCreateItemOperation",
        typed_object(vec![
            ("schema_version", integer(), true),
            ("document_id", string(), true),
            ("parent_id", nullable_scalar(Type::String), true),
            ("origin", nullable_ref("OrgFollowUpOrigin"), true),
            ("initial_state", string(), true),
            ("item_type", string(), true),
            ("title", string(), true),
            ("priority", nullable_scalar(Type::String), true),
            ("tags", array_of(string()), true),
            ("requires_review", boolean(), true),
            ("initial_assignee", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgAssignmentOperation",
        typed_object(vec![
            ("previous_assignee", nullable_scalar(Type::String), true),
            ("resulting_assignee", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgScheduleOperation",
        typed_object(vec![
            ("previous_scheduled", nullable_scalar(Type::String), true),
            ("resulting_scheduled", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgDeadlineOperation",
        typed_object(vec![
            ("previous_deadline", nullable_scalar(Type::String), true),
            ("resulting_deadline", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgDependencyOperation",
        typed_object(vec![
            ("dependency_id", string(), true),
            ("action", enum_string(&["add", "remove"]), true),
            ("previous_dependencies", array_of(string()), true),
            ("resulting_dependencies", array_of(string()), true),
        ]),
    );
    add(
        components,
        "OrgNoteLinkOperation",
        typed_object(vec![
            ("action", enum_string(&["link", "unlink"]), true),
            ("purpose", string(), true),
            ("note_id", string(), true),
        ]),
    );
    add(
        components,
        "OrgItemOperation",
        Schema::OneOf(
            OneOfBuilder::new()
                .item(Ref::from_schema_name("OrgCreateItemOperation"))
                .item(Ref::from_schema_name("OrgAssignmentOperation"))
                .item(Ref::from_schema_name("OrgScheduleOperation"))
                .item(Ref::from_schema_name("OrgDeadlineOperation"))
                .item(Ref::from_schema_name("OrgDependencyOperation"))
                .item(Ref::from_schema_name("OrgNoteLinkOperation"))
                .build(),
        )
        .into(),
    );
    add(
        components,
        "OrgMoveItemOperation",
        typed_object(vec![
            ("source_document_id", string(), true),
            ("target_document_id", string(), true),
            ("previous_parent_id", nullable_scalar(Type::String), true),
            ("resulting_parent_id", nullable_scalar(Type::String), true),
        ]),
    );
    add(
        components,
        "OrgItemMutationData",
        typed_object(vec![
            ("affected_document_count", integer(), true),
            (
                "resulting_items",
                array_of(schema_ref("OrgResultingItem")),
                true,
            ),
            ("operation", schema_ref("OrgItemOperation"), true),
        ]),
    );
    add(
        components,
        "OrgMoveItemData",
        typed_object(vec![
            ("affected_document_count", integer(), true),
            (
                "resulting_items",
                array_of(schema_ref("OrgResultingItem")),
                true,
            ),
            ("operation", schema_ref("OrgMoveItemOperation"), true),
        ]),
    );
    add(
        components,
        "OrgContextData",
        typed_object(vec![("context", schema_ref("OrgItemContextResult"), true)]),
    );
    add(
        components,
        "OrgHeartbeatData",
        typed_object(vec![
            ("lease", schema_ref("OrgLease"), true),
            ("context", schema_ref("OrgItemContextResult"), true),
        ]),
    );

    for (name, schema) in [
        ("OrgWorkspacePageResult", page_schema("OrgWorkspaceSummary")),
        (
            "OrgWorkspaceCommandResult",
            command_schema("OrgWorkspaceRevisionData"),
        ),
        ("OrgArchiveCommandResult", command_schema("OrgArchiveData")),
        ("OrgDocumentPageResult", page_schema("OrgDocumentResult")),
        (
            "OrgDocumentCountCommandResult",
            command_schema("OrgDocumentCountData"),
        ),
        (
            "OrgDocumentLifecycleCommandResult",
            command_schema("OrgDocumentLifecycleData"),
        ),
        (
            "OrgMoveDocumentCommandResult",
            command_schema("OrgMoveDocumentData"),
        ),
        (
            "OrgMoveItemCommandResult",
            command_schema("OrgMoveItemData"),
        ),
        (
            "OrgItemCommandResult",
            command_schema("OrgItemMutationData"),
        ),
        ("OrgContextCommandResult", command_schema("OrgContextData")),
        (
            "OrgHeartbeatCommandResult",
            command_schema("OrgHeartbeatData"),
        ),
        ("OrgItemPageResult", page_schema("OrgItemResult")),
        ("OrgEventPageResult", page_schema("OrgEventResult")),
    ] {
        add(components, name, schema);
    }
    add(
        components,
        "OrgWorkspaceExportResult",
        typed_object(vec![
            ("workspace", schema_ref("OrgWorkspaceResult"), true),
            (
                "documents",
                array_of(schema_ref("OrgDocumentSourceResult")),
                true,
            ),
        ]),
    );
    add(
        components,
        "OrgOperationalPageResult",
        typed_object(vec![
            ("items", array_of(schema_ref("OrgOperationalItem")), true),
            ("next_cursor", nullable_scalar(Type::String), true),
            ("evaluated_at", integer(), true),
        ]),
    );
    add(
        components,
        "OrgClaimResult",
        typed_object(vec![
            ("schema_version", integer(), true),
            ("workspace_id", string(), true),
            ("operation_id", string(), true),
            ("lease_id", string(), true),
            ("fencing_token", string(), true),
            ("expires_at", integer(), true),
            ("event_ids", array_of(string()), true),
            ("context", schema_ref("OrgItemContextResult"), true),
        ]),
    );

    for path_item in openapi
        .paths
        .paths
        .iter_mut()
        .filter(|(path, _)| path.starts_with("/api/org"))
        .map(|(_, item)| item)
    {
        for operation in [
            &mut path_item.get,
            &mut path_item.put,
            &mut path_item.post,
            &mut path_item.delete,
            &mut path_item.patch,
        ]
        .into_iter()
        .flatten()
        {
            let operation_id = operation.operation_id.as_deref().expect("Org operation id");
            let schema_name = org_success_schema_name(operation_id);
            for (status, response) in &mut operation.responses.responses {
                if !status.starts_with('2') {
                    continue;
                }
                if let RefOr::T(response) = response {
                    response
                        .content
                        .get_mut("application/json")
                        .expect("Org JSON success response")
                        .schema = Some(Ref::from_schema_name(schema_name).into());
                }
            }
        }
    }
    openapi
}

fn normalize_org_query_parameters(mut openapi: OpenApi) -> OpenApi {
    for (path, path_item) in &mut openapi.paths.paths {
        if !path.starts_with("/api/org") {
            continue;
        }
        let Some(operation) = &mut path_item.get else {
            continue;
        };
        let Some(parameters) = &mut operation.parameters else {
            continue;
        };
        for parameter in parameters {
            let name = parameter.name.clone();
            if path.contains(&format!("{{{name}}}")) {
                continue;
            }
            parameter.parameter_in = ParameterIn::Query;
            match name.as_str() {
                "cursor" => {
                    parameter.required = Required::False;
                    parameter.description = Some(
                        "Opaque cursor returned by the preceding page; clients must not inspect or modify it."
                            .to_owned(),
                    );
                }
                "limit" => {
                    parameter.required = Required::False;
                    if let Some(RefOr::T(Schema::Object(schema))) = &mut parameter.schema {
                        schema.default = Some(serde_json::json!(50));
                        schema.minimum = Some(1usize.into());
                        schema.maximum = Some(200usize.into());
                    }
                }
                "include_archived" => {
                    parameter.required = Required::False;
                    if path.ends_with("/documents") {
                        parameter.description = Some(
                            "Legacy alias: true means `status=all`, false means `status=active`. Mutually exclusive with `status`."
                                .to_owned(),
                        );
                        parameter.schema = Some(scalar(Type::Boolean));
                    } else if let Some(RefOr::T(Schema::Object(schema))) = &mut parameter.schema {
                        schema.default = Some(serde_json::json!(false));
                    }
                }
                "status" if path.ends_with("/documents") => {
                    parameter.required = Required::False;
                    parameter.description = Some(
                        "Document lifecycle filter. Mutually exclusive with the legacy `include_archived` parameter."
                            .to_owned(),
                    );
                    parameter.schema = Some(enum_string(&["active", "archived", "all"]));
                }
                "priority" => {
                    parameter.required = Required::False;
                    parameter.description = Some(
                        "Omit to disable priority filtering, use `none` for items without a priority, or use one uppercase letter A-Z."
                            .to_owned(),
                    );
                }
                _ => {}
            }
        }
    }
    openapi
}

pub fn swagger_router(openapi: OpenApi) -> Router {
    SwaggerUi::new("/api/docs")
        .url("/api/openapi.json", openapi)
        .into()
}

fn tag(name: &str, description: &str) -> Tag {
    let mut tag = Tag::new(name);
    tag.description = Some(description.to_owned());
    tag
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use std::collections::BTreeSet;
    use tower::ServiceExt;
    use utoipa::openapi::OpenApi;

    fn property_keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .expect("serialized object")
            .keys()
            .cloned()
            .collect()
    }

    fn schema_property_keys(document: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
        document["components"]["schemas"][schema_name]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("missing properties for {schema_name}"))
            .keys()
            .cloned()
            .collect()
    }

    fn schema_required_keys(document: &serde_json::Value, schema_name: &str) -> BTreeSet<String> {
        document["components"]["schemas"][schema_name]["required"]
            .as_array()
            .map(|required| {
                required
                    .iter()
                    .map(|key| key.as_str().expect("required property name").to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn assert_nullable_property(
        document: &serde_json::Value,
        schema_name: &str,
        property_name: &str,
    ) {
        let schema_type =
            &document["components"]["schemas"][schema_name]["properties"][property_name]["type"];
        assert!(
            schema_type
                .as_array()
                .expect("nullable OpenAPI 3.1 type array")
                .contains(&serde_json::json!("null")),
            "{schema_name}.{property_name} must accept null"
        );
    }

    #[tokio::test]
    async fn serves_openapi_document() {
        let response = swagger_router(OpenApi::default())
            .oneshot(
                Request::builder()
                    .uri("/api/openapi.json")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let document: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(document["openapi"], "3.1.0");
    }

    #[tokio::test]
    async fn serves_swagger_ui() {
        let app = swagger_router(OpenApi::default());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/docs")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/docs/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("Swagger UI"));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/docs/swagger-initializer.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let javascript = String::from_utf8(body.to_vec()).unwrap();
        assert!(javascript.contains("/api/openapi.json"));
    }

    #[test]
    fn system_schema_fields_match_runtime_serialization() {
        let runtime_config = note_core::SystemConfig {
            category_labels: vec!["project".to_string()],
            search: note_core::SearchConfig::default(),
            duplicate_check: note_core::DuplicateCheckConfig {
                enabled: true,
                rules: vec![note_core::DuplicateCheckRule {
                    terms: vec![note_core::DuplicateCheckTerm {
                        key: "kind".to_string(),
                        value: Some("skill".to_string()),
                    }],
                }],
            },
        };
        let runtime_config = serde_json::to_value(runtime_config).unwrap();
        let runtime_search = &runtime_config["search"];
        let runtime_duplicate_check = &runtime_config["duplicate_check"];
        let runtime_rule = &runtime_duplicate_check["rules"][0];
        let runtime_term = &runtime_rule["terms"][0];

        let runtime_info = note_pipelines::SystemInfo {
            database_engine: "embed".to_string(),
            database_path: Some("/tmp/agent-note.db".to_string()),
            database_size_bytes: Some(1024),
            attachments_engine: "filesystem".to_string(),
            attachments_location: Some("/tmp/attachments".to_string()),
            embedding_engine: "stub".to_string(),
            embedding_model: "stub".to_string(),
            embedding_fingerprint: "stub:0".to_string(),
        };
        let runtime_info = serde_json::to_value(runtime_info).unwrap();

        let (_, document) = rest_router();
        let document = serde_json::to_value(document).unwrap();

        assert_eq!(
            property_keys(&runtime_config),
            schema_property_keys(&document, "SystemConfigSchema")
        );
        assert_eq!(
            property_keys(runtime_search),
            schema_property_keys(&document, "SearchConfigSchema")
        );
        assert_eq!(
            property_keys(runtime_duplicate_check),
            schema_property_keys(&document, "DuplicateCheckConfigSchema")
        );
        assert_eq!(
            property_keys(runtime_rule),
            schema_property_keys(&document, "DuplicateCheckRuleSchema")
        );
        assert_eq!(
            property_keys(runtime_term),
            schema_property_keys(&document, "DuplicateCheckTermSchema")
        );
        assert_eq!(
            property_keys(&runtime_info),
            schema_property_keys(&document, "SystemInfoSchema")
        );

        assert_eq!(
            schema_required_keys(&document, "SystemConfigSchema"),
            BTreeSet::new()
        );
        assert_eq!(
            schema_required_keys(&document, "SearchConfigSchema"),
            BTreeSet::new()
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckConfigSchema"),
            BTreeSet::new()
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckRuleSchema"),
            BTreeSet::from(["terms".to_string()])
        );
        assert_eq!(
            schema_required_keys(&document, "DuplicateCheckTermSchema"),
            BTreeSet::from(["key".to_string()])
        );
        assert_eq!(
            schema_required_keys(&document, "SystemInfoSchema"),
            property_keys(&runtime_info)
        );
        for property in [
            "database_path",
            "database_size_bytes",
            "attachments_location",
        ] {
            assert_nullable_property(&document, "SystemInfoSchema", property);
        }

        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["SystemConfigSchema"]["properties"]["category_labels"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            schemas["SystemConfigSchema"]["properties"]["category_labels"]["description"],
            "Ordered category expressions. `key` includes all values; `key=value` includes exactly that value. The first `=` separates key and value. Keys must exist in the label catalog and have no leading or trailing whitespace; values are preserved byte-for-byte."
        );
        assert_eq!(
            schemas["SystemConfigSchema"]["properties"]["search"]["default"],
            serde_json::json!({"minimum_score": 0.01})
        );
        assert_eq!(
            schemas["SystemConfigSchema"]["properties"]["duplicate_check"]["default"],
            serde_json::json!({"enabled": false, "rules": []})
        );
        assert_eq!(
            schemas["SearchConfigSchema"]["properties"]["minimum_score"]["default"],
            0.01
        );
        assert_eq!(
            schemas["SearchConfigSchema"]["properties"]["minimum_score"]["minimum"],
            0.0
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["enabled"]["default"],
            false
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["rules"]["default"],
            serde_json::json!([])
        );
        assert_eq!(
            schemas["DuplicateCheckConfigSchema"]["properties"]["rules"]["maxItems"],
            note_core::MAX_DUPLICATE_CHECK_RULES
        );
        assert_eq!(
            schemas["DuplicateCheckRuleSchema"]["properties"]["terms"]["minItems"],
            1
        );
        assert_eq!(
            schemas["DuplicateCheckRuleSchema"]["properties"]["terms"]["maxItems"],
            note_core::MAX_DUPLICATE_CHECK_TERMS
        );
    }
}
