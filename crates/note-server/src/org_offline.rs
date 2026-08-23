use anyhow::{anyhow, bail, Context as _};
use cap_fs_ext::{DirExt as _, FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::ambient_authority;
use cap_std::fs::{Dir as CapabilityDir, File as CapabilityFile, OpenOptions};
use note_org::{
    validate_document_path, ClaimPolicy, DocumentId, TagRule, WorkItemId, WorkItemType,
    WorkspaceId, WorkspacePolicy,
};
use note_pipelines::org::{
    export_document_by_id, export_workspace, import_offline_document,
    import_offline_workspace_snapshot, CommandEnvelope, DocumentImport,
    ImportWorkspaceSnapshotRequest, LeaseProofInput, OrgContext, OrgError, PutDocumentRequest,
    WorkspaceImportMode, WorkspaceSnapshotMetadata,
};
#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
use rustix::fs::{self as rustix_fs, RenameFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
#[cfg(test)]
use std::fs;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

pub const ORG_SNAPSHOT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    Create,
    Update,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgOfflineCommand {
    ExportWorkspace {
        workspace_id: WorkspaceId,
        output: PathBuf,
    },
    ImportWorkspace {
        input: PathBuf,
        mode: ImportMode,
        actor_id: String,
        operation_id: String,
    },
    ExportDocument {
        document_id: DocumentId,
        output: PathBuf,
    },
    ImportDocument {
        workspace_id: WorkspaceId,
        document_id: DocumentId,
        path: String,
        input: PathBuf,
        mode: ImportMode,
        actor_id: String,
        operation_id: String,
        expected_revision: Option<i64>,
    },
}

impl OrgOfflineCommand {
    fn name(&self) -> &'static str {
        match self {
            Self::ExportWorkspace { .. } => "export-workspace",
            Self::ImportWorkspace { .. } => "import-workspace",
            Self::ExportDocument { .. } => "export-document",
            Self::ImportDocument { .. } => "import-document",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceManifest {
    pub format_version: u32,
    pub workspace: ManifestWorkspace,
    pub documents: Vec<ManifestDocument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestWorkspace {
    pub id: WorkspaceId,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub timezone: String,
    pub policy_schema_version: i64,
    #[serde(deserialize_with = "deserialize_strict_workspace_policy")]
    pub policy: WorkspacePolicy,
    pub revision: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestDocument {
    pub id: DocumentId,
    pub path: String,
    pub revision: i64,
    pub content_hash: String,
    pub file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictTagRule {
    allowed: BTreeSet<String>,
    required: BTreeSet<String>,
}

impl From<StrictTagRule> for TagRule {
    fn from(value: StrictTagRule) -> Self {
        Self {
            allowed: value.allowed,
            required: value.required,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictWorkspacePolicy {
    #[serde(default)]
    allow_cross_workspace_agenda: bool,
    allowed_types: BTreeSet<WorkItemType>,
    states: BTreeSet<String>,
    transitions: BTreeSet<(String, String)>,
    initial_state: String,
    running_state: String,
    executable_states: BTreeSet<String>,
    review_state: String,
    failed_state: String,
    cancelled_state: String,
    successful_terminal_states: BTreeSet<String>,
    terminal_states: BTreeSet<String>,
    release_state: String,
    review_rejection_state: String,
    lease_expiry_recovery_state: String,
    review_required_types: BTreeSet<WorkItemType>,
    claim_policy: ClaimPolicy,
    lease_duration_secs: u64,
    retry_limit: u32,
    concurrency_limit: usize,
    #[serde(deserialize_with = "deserialize_strict_tag_rules")]
    tag_rules: BTreeMap<WorkItemType, StrictTagRule>,
}

fn deserialize_strict_tag_rules<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<WorkItemType, StrictTagRule>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StrictTagRulesVisitor;

    impl<'de> serde::de::Visitor<'de> for StrictTagRulesVisitor {
        type Value = BTreeMap<WorkItemType, StrictTagRule>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a map of unique Org work-item tag rules")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut rules = BTreeMap::new();
            while let Some((kind, rule)) = map.next_entry()? {
                if rules.insert(kind, rule).is_some() {
                    return Err(serde::de::Error::custom("duplicate Org tag-rule key"));
                }
            }
            Ok(rules)
        }
    }

    deserializer.deserialize_map(StrictTagRulesVisitor)
}

impl From<StrictWorkspacePolicy> for WorkspacePolicy {
    fn from(value: StrictWorkspacePolicy) -> Self {
        Self {
            allow_cross_workspace_agenda: value.allow_cross_workspace_agenda,
            allowed_types: value.allowed_types,
            states: value.states,
            transitions: value.transitions,
            initial_state: value.initial_state,
            running_state: value.running_state,
            executable_states: value.executable_states,
            review_state: value.review_state,
            failed_state: value.failed_state,
            cancelled_state: value.cancelled_state,
            successful_terminal_states: value.successful_terminal_states,
            terminal_states: value.terminal_states,
            release_state: value.release_state,
            review_rejection_state: value.review_rejection_state,
            lease_expiry_recovery_state: value.lease_expiry_recovery_state,
            review_required_types: value.review_required_types,
            claim_policy: value.claim_policy,
            lease_duration_secs: value.lease_duration_secs,
            retry_limit: value.retry_limit,
            concurrency_limit: value.concurrency_limit,
            tag_rules: value
                .tag_rules
                .into_iter()
                .map(|(kind, rule)| (kind, rule.into()))
                .collect(),
        }
    }
}

fn deserialize_strict_workspace_policy<'de, D>(deserializer: D) -> Result<WorkspacePolicy, D::Error>
where
    D: serde::Deserializer<'de>,
{
    StrictWorkspacePolicy::deserialize(deserializer).map(Into::into)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OfflineReport {
    pub ok: bool,
    pub command: &'static str,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_id: Option<DocumentId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_revision: Option<i64>,
    pub document_revisions: BTreeMap<String, i64>,
    pub documents: Vec<OfflineDocumentReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OfflineError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OfflineDocumentReport {
    pub document_id: DocumentId,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<OfflineError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OfflineError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Value,
    pub retryable: bool,
}

impl OfflineError {
    fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".into(),
            message: message.into(),
            details: json!({}),
            retryable: false,
        }
    }
}

pub fn command_name(args: &[String]) -> &'static str {
    match args.get(2).map(String::as_str) {
        Some("export-workspace") => "export-workspace",
        Some("import-workspace") => "import-workspace",
        Some("export-document") => "export-document",
        Some("import-document") => "import-document",
        _ => "org",
    }
}

pub fn startup_failure_report(command: &'static str, code: &str, message: &str) -> OfflineReport {
    OfflineReport {
        ok: false,
        command,
        applied: false,
        workspace_id: None,
        document_id: None,
        workspace_revision: None,
        document_revisions: BTreeMap::new(),
        documents: Vec::new(),
        error: Some(OfflineError {
            code: code.to_owned(),
            message: message.to_owned(),
            details: json!({}),
            retryable: false,
        }),
    }
}

impl From<OrgError> for OfflineError {
    fn from(error: OrgError) -> Self {
        Self {
            code: serde_json::to_value(error.code)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_else(|| "storage_failure".into()),
            message: error.message,
            details: error.details,
            retryable: error.retryable,
        }
    }
}

pub fn parse_command(args: &[String]) -> anyhow::Result<Option<OrgOfflineCommand>> {
    if args.get(1).map(String::as_str) != Some("org") {
        return Ok(None);
    }
    let command_name = args
        .get(2)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("missing Org offline command"))?;
    let options = parse_options(&args[3..])?;
    let command = match command_name {
        "export-workspace" => OrgOfflineCommand::ExportWorkspace {
            workspace_id: parse_id(&take_required(&options, "--workspace-id")?, "workspace")?,
            output: PathBuf::from(take_required(&options, "--output")?),
        },
        "import-workspace" => OrgOfflineCommand::ImportWorkspace {
            input: PathBuf::from(take_required(&options, "--input")?),
            mode: parse_import_mode(&take_required(&options, "--mode")?)?,
            actor_id: take_required(&options, "--actor-id")?,
            operation_id: take_required(&options, "--operation-id")?,
        },
        "export-document" => OrgOfflineCommand::ExportDocument {
            document_id: parse_id(&take_required(&options, "--document-id")?, "document")?,
            output: PathBuf::from(take_required(&options, "--output")?),
        },
        "import-document" => {
            let mode = parse_import_mode(&take_required(&options, "--mode")?)?;
            let expected_revision = options
                .get("--expected-revision")
                .map(|value| {
                    value
                        .parse::<i64>()
                        .context("--expected-revision must be an integer")
                })
                .transpose()?;
            match (mode, expected_revision) {
                (ImportMode::Create, Some(_)) => {
                    bail!("--expected-revision is invalid in create mode")
                }
                (ImportMode::Update, None) => {
                    bail!("--expected-revision is required in update mode")
                }
                (_, Some(revision)) if revision < 1 => {
                    bail!("--expected-revision must be positive")
                }
                _ => {}
            }
            let path = take_required(&options, "--path")?;
            validate_document_path(&path).map_err(|error| anyhow!("invalid --path: {error}"))?;
            OrgOfflineCommand::ImportDocument {
                workspace_id: parse_id(&take_required(&options, "--workspace-id")?, "workspace")?,
                document_id: parse_id(&take_required(&options, "--document-id")?, "document")?,
                path,
                input: PathBuf::from(take_required(&options, "--input")?),
                mode,
                actor_id: take_required(&options, "--actor-id")?,
                operation_id: take_required(&options, "--operation-id")?,
                expected_revision,
            }
        }
        _ => bail!("unknown Org offline command: {command_name}"),
    };
    reject_unused_options(&command, &options)?;
    Ok(Some(command))
}

pub async fn execute_command(context: &OrgContext, command: OrgOfflineCommand) -> OfflineReport {
    let name = command.name();
    let result = match command {
        OrgOfflineCommand::ExportWorkspace {
            workspace_id,
            output,
        } => export_workspace_command(context, workspace_id, &output).await,
        OrgOfflineCommand::ImportWorkspace {
            input,
            mode,
            actor_id,
            operation_id,
        } => import_workspace_command(context, &input, mode, actor_id, operation_id).await,
        OrgOfflineCommand::ExportDocument {
            document_id,
            output,
        } => export_document_command(context, document_id, &output).await,
        OrgOfflineCommand::ImportDocument {
            workspace_id,
            document_id,
            path,
            input,
            mode,
            actor_id,
            operation_id,
            expected_revision,
        } => {
            import_document_command(
                context,
                ImportDocumentOptions {
                    workspace_id,
                    document_id,
                    path,
                    input,
                    mode,
                    actor_id,
                    operation_id,
                    expected_revision,
                },
            )
            .await
        }
    };
    result.unwrap_or_else(|failure| OfflineReport {
        ok: false,
        command: name,
        applied: false,
        workspace_id: failure.workspace_id,
        document_id: failure.document_id,
        workspace_revision: None,
        document_revisions: BTreeMap::new(),
        documents: failure
            .documents
            .into_iter()
            .map(|document_id| OfflineDocumentReport {
                document_id,
                applied: false,
                revision: None,
                error: Some(failure.error.clone()),
            })
            .collect(),
        error: Some(failure.error),
    })
}

#[derive(Debug)]
struct CommandFailure {
    workspace_id: Option<WorkspaceId>,
    document_id: Option<DocumentId>,
    documents: Vec<DocumentId>,
    error: OfflineError,
}

impl CommandFailure {
    fn filesystem(message: impl Into<String>) -> Self {
        Self {
            workspace_id: None,
            document_id: None,
            documents: Vec::new(),
            error: OfflineError::invalid_input(message),
        }
    }

    fn pipeline(
        error: OrgError,
        workspace_id: Option<WorkspaceId>,
        document_id: Option<DocumentId>,
        documents: Vec<DocumentId>,
    ) -> Self {
        Self {
            workspace_id,
            document_id,
            documents,
            error: error.into(),
        }
    }
}

async fn export_workspace_command(
    context: &OrgContext,
    workspace_id: WorkspaceId,
    output: &Path,
) -> Result<OfflineReport, CommandFailure> {
    let destination = ExportDestination::open(output, ExportKind::Workspace)
        .map_err(CommandFailure::filesystem)?;
    let export = export_workspace(context, workspace_id)
        .await
        .map_err(|error| CommandFailure::pipeline(error, Some(workspace_id), None, Vec::new()))?;
    let documents = export
        .documents
        .iter()
        .map(|document| ManifestDocument {
            id: document.id,
            path: document.path.clone(),
            revision: document.revision,
            content_hash: document.content_hash.clone(),
            file: format!("documents/{}.org", document.id),
        })
        .collect::<Vec<_>>();
    let manifest = WorkspaceManifest {
        format_version: ORG_SNAPSHOT_FORMAT_VERSION,
        workspace: ManifestWorkspace {
            id: export.workspace.id,
            slug: export.workspace.slug,
            display_name: export.workspace.display_name,
            description: export.workspace.description,
            timezone: export.workspace.timezone,
            policy_schema_version: export.workspace.policy_schema_version,
            policy: export.workspace.policy,
            revision: export.workspace.revision,
            archived_at: export.workspace.archived_at,
        },
        documents,
    };
    write_workspace_snapshot(&destination, &manifest, &export.documents)
        .map_err(CommandFailure::filesystem)?;
    Ok(OfflineReport {
        ok: true,
        command: "export-workspace",
        applied: true,
        workspace_id: Some(workspace_id),
        document_id: None,
        workspace_revision: Some(manifest.workspace.revision),
        document_revisions: manifest
            .documents
            .iter()
            .map(|document| (document.id.to_string(), document.revision))
            .collect(),
        documents: manifest
            .documents
            .iter()
            .map(|document| OfflineDocumentReport {
                document_id: document.id,
                applied: true,
                revision: Some(document.revision),
                error: None,
            })
            .collect(),
        error: None,
    })
}

async fn import_workspace_command(
    context: &OrgContext,
    input: &Path,
    mode: ImportMode,
    actor_id: String,
    operation_id: String,
) -> Result<OfflineReport, CommandFailure> {
    let snapshot = read_workspace_snapshot(input).map_err(CommandFailure::filesystem)?;
    let workspace_id = snapshot.manifest.workspace.id;
    let document_ids = snapshot
        .manifest
        .documents
        .iter()
        .map(|document| document.id)
        .collect::<Vec<_>>();
    let request = ImportWorkspaceSnapshotRequest {
        mode: match mode {
            ImportMode::Create => WorkspaceImportMode::Create,
            ImportMode::Update => WorkspaceImportMode::Update,
        },
        workspace: WorkspaceSnapshotMetadata {
            slug: snapshot.manifest.workspace.slug,
            display_name: snapshot.manifest.workspace.display_name,
            description: snapshot.manifest.workspace.description,
            timezone: snapshot.manifest.workspace.timezone,
            policy_schema_version: snapshot.manifest.workspace.policy_schema_version,
            policy: snapshot.manifest.workspace.policy,
            revision: snapshot.manifest.workspace.revision,
            archived_at: snapshot.manifest.workspace.archived_at,
        },
        documents: snapshot.documents,
        document_revisions: snapshot
            .manifest
            .documents
            .iter()
            .map(|document| (document.id, document.revision))
            .collect(),
        lease_proofs: BTreeMap::<WorkItemId, LeaseProofInput>::new(),
    };
    let result = import_offline_workspace_snapshot(
        context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id,
            actor_id,
            operation_id,
        },
        &request,
    )
    .await
    .map_err(|error| {
        CommandFailure::pipeline(error, Some(workspace_id), None, document_ids.clone())
    })?;
    Ok(command_result_report(
        "import-workspace",
        workspace_id,
        None,
        document_ids,
        result,
    ))
}

async fn export_document_command(
    context: &OrgContext,
    document_id: DocumentId,
    output: &Path,
) -> Result<OfflineReport, CommandFailure> {
    let destination = ExportDestination::open(output, ExportKind::Document)
        .map_err(CommandFailure::filesystem)?;
    let document = export_document_by_id(context, document_id)
        .await
        .map_err(|error| CommandFailure::pipeline(error, None, Some(document_id), Vec::new()))?;
    write_atomic_file(&destination, document.source.as_bytes())
        .map_err(CommandFailure::filesystem)?;
    Ok(OfflineReport {
        ok: true,
        command: "export-document",
        applied: true,
        workspace_id: Some(document.workspace_id),
        document_id: Some(document.id),
        workspace_revision: None,
        document_revisions: BTreeMap::from([(document.id.to_string(), document.revision)]),
        documents: vec![OfflineDocumentReport {
            document_id: document.id,
            applied: true,
            revision: Some(document.revision),
            error: None,
        }],
        error: None,
    })
}

struct ImportDocumentOptions {
    workspace_id: WorkspaceId,
    document_id: DocumentId,
    path: String,
    input: PathBuf,
    mode: ImportMode,
    actor_id: String,
    operation_id: String,
    expected_revision: Option<i64>,
}

async fn import_document_command(
    context: &OrgContext,
    options: ImportDocumentOptions,
) -> Result<OfflineReport, CommandFailure> {
    validate_document_path(&options.path)
        .map_err(|error| CommandFailure::filesystem(error.to_string()))?;
    let source = read_regular_utf8_file(&options.input).map_err(CommandFailure::filesystem)?;
    let result = import_offline_document(
        context,
        &CommandEnvelope {
            schema_version: 1,
            workspace_id: options.workspace_id,
            actor_id: options.actor_id,
            operation_id: options.operation_id,
        },
        &PutDocumentRequest {
            document_id: options.document_id,
            path: options.path,
            source,
            expected_revision: match options.mode {
                ImportMode::Create => None,
                ImportMode::Update => options.expected_revision,
            },
            lease_proofs: BTreeMap::new(),
        },
    )
    .await
    .map_err(|error| {
        CommandFailure::pipeline(
            error,
            Some(options.workspace_id),
            Some(options.document_id),
            vec![options.document_id],
        )
    })?;
    Ok(command_result_report(
        "import-document",
        options.workspace_id,
        Some(options.document_id),
        vec![options.document_id],
        result,
    ))
}

fn command_result_report(
    command: &'static str,
    workspace_id: WorkspaceId,
    document_id: Option<DocumentId>,
    document_ids: Vec<DocumentId>,
    result: note_pipelines::org::OrgCommandResult,
) -> OfflineReport {
    let documents = document_ids
        .into_iter()
        .map(|id| OfflineDocumentReport {
            document_id: id,
            applied: true,
            revision: result.document_revisions.get(&id.to_string()).copied(),
            error: None,
        })
        .collect();
    OfflineReport {
        ok: true,
        command,
        applied: true,
        workspace_id: Some(workspace_id),
        document_id,
        workspace_revision: result.workspace_revision,
        document_revisions: result.document_revisions,
        documents,
        error: None,
    }
}

struct ReadSnapshot {
    manifest: WorkspaceManifest,
    documents: Vec<DocumentImport>,
}

fn read_workspace_snapshot(input: &Path) -> Result<ReadSnapshot, String> {
    let root = open_directory_path_nofollow(input)?;
    let manifest_bytes = read_regular_file_at(&root, OsStr::new("manifest.json"))?;
    let manifest = deserialize_manifest_strict(&manifest_bytes)?;
    validate_manifest(&manifest)?;

    let expected_root = BTreeSet::from(["manifest.json".to_owned(), "documents".to_owned()]);
    let actual_root = read_entry_names(&root)?;
    if actual_root != expected_root {
        return Err("Org workspace snapshot has missing or extra root entries".into());
    }
    let documents_dir = open_directory_at(&root, "documents")?;
    let expected_files = manifest
        .documents
        .iter()
        .map(|document| format!("{}.org", document.id))
        .collect::<BTreeSet<_>>();
    if read_entry_names(&documents_dir)? != expected_files {
        return Err("Org workspace snapshot has missing or extra document files".into());
    }

    let mut documents = Vec::with_capacity(manifest.documents.len());
    for entry in &manifest.documents {
        let file_name = format!("{}.org", entry.id);
        let bytes = read_regular_file_at(&documents_dir, OsStr::new(&file_name))?;
        if content_hash(&bytes) != entry.content_hash {
            return Err("Org workspace document content hash does not match manifest".into());
        }
        let source = String::from_utf8(bytes)
            .map_err(|_| "Org workspace document is not valid UTF-8".to_owned())?;
        documents.push(DocumentImport {
            document_id: entry.id,
            path: entry.path.clone(),
            source,
        });
    }
    Ok(ReadSnapshot {
        manifest,
        documents,
    })
}

fn deserialize_manifest_strict(bytes: &[u8]) -> Result<WorkspaceManifest, String> {
    serde_json::from_slice(bytes).map_err(|_| "Org workspace manifest is malformed".to_owned())
}

fn validate_manifest(manifest: &WorkspaceManifest) -> Result<(), String> {
    if manifest.format_version != ORG_SNAPSHOT_FORMAT_VERSION {
        return Err("Org workspace snapshot format version is unsupported".into());
    }
    if manifest.workspace.revision < 1
        || manifest
            .workspace
            .archived_at
            .is_some_and(|value| value < 1)
    {
        return Err("Org workspace manifest revisions and timestamps must be positive".into());
    }
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut files = BTreeSet::new();
    for document in &manifest.documents {
        if document.revision < 1 {
            return Err("Org document revision must be positive".into());
        }
        validate_document_path(&document.path).map_err(|error| error.to_string())?;
        let expected_file = format!("documents/{}.org", document.id);
        if document.file != expected_file {
            return Err("Org manifest document filename is not canonical".into());
        }
        validate_content_hash(&document.content_hash)?;
        if !ids.insert(document.id)
            || !paths.insert(document.path.clone())
            || !files.insert(document.file.clone())
        {
            return Err("Org manifest contains duplicate document IDs, paths, or files".into());
        }
    }
    Ok(())
}

struct ExportDestination {
    parent: CapabilityDir,
    basename: OsString,
}

#[derive(Clone, Copy)]
enum ExportKind {
    Workspace,
    Document,
}

impl ExportDestination {
    fn open(output: &Path, kind: ExportKind) -> Result<Self, String> {
        if output.as_os_str().is_empty() || output.file_name().is_none() {
            return Err("Org export destination is invalid".into());
        }
        let parent_path = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = open_directory_path_nofollow(parent_path)
            .map_err(|_| "Org export parent must be an existing symlink-free directory")?;
        let basename = output.file_name().unwrap().to_owned();
        match parent.symlink_metadata(&basename) {
            Ok(_) => {
                let subject = match kind {
                    ExportKind::Workspace => "workspace",
                    ExportKind::Document => "document",
                };
                return Err(format!("Org {subject} export destination already exists"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Inspect Org export destination failed".into()),
        }
        Ok(Self { parent, basename })
    }
}

fn write_workspace_snapshot(
    destination: &ExportDestination,
    manifest: &WorkspaceManifest,
    documents: &[note_pipelines::org::OrgDocumentSourceView],
) -> Result<(), String> {
    let temp_name = unique_temp_name(".org-workspace-");
    destination
        .parent
        .create_dir(&temp_name)
        .map_err(|_| "Create temporary Org workspace export directory failed".to_owned())?;
    let preparation = (|| {
        let temp = destination
            .parent
            .open_dir_nofollow(&temp_name)
            .map_err(|_| "Open temporary Org workspace directory failed".to_owned())?;
        temp.create_dir("documents")
            .map_err(|_| "Create Org workspace documents directory failed".to_owned())?;
        let documents_dir = temp
            .open_dir_nofollow("documents")
            .map_err(|_| "Open Org workspace documents directory failed".to_owned())?;
        for document in documents {
            write_new_file(
                &documents_dir,
                OsStr::new(&format!("{}.org", document.id)),
                document.source.as_bytes(),
            )?;
        }
        let mut manifest_bytes = serde_json::to_vec_pretty(manifest)
            .map_err(|_| "Serialize Org workspace manifest failed".to_owned())?;
        manifest_bytes.push(b'\n');
        write_new_file(&temp, OsStr::new("manifest.json"), &manifest_bytes)
    })();
    if let Err(error) = preparation {
        let _ = destination.parent.remove_dir_all(&temp_name);
        return Err(error);
    }
    if publish_noclobber(&destination.parent, &temp_name, &destination.basename).is_err() {
        let _ = destination.parent.remove_dir_all(&temp_name);
        return Err("Atomically publish Org workspace export failed".into());
    }
    Ok(())
}

fn write_atomic_file(destination: &ExportDestination, bytes: &[u8]) -> Result<(), String> {
    write_atomic_file_to_destination(destination, bytes, || {})
}

fn write_atomic_file_to_destination(
    destination: &ExportDestination,
    bytes: &[u8],
    before_publish: impl FnOnce(),
) -> Result<(), String> {
    write_atomic_file_with_steps(destination, bytes, before_publish, publish_noclobber)
}

fn write_atomic_file_with_steps(
    destination: &ExportDestination,
    bytes: &[u8],
    before_publish: impl FnOnce(),
    publisher: impl FnOnce(&CapabilityDir, &OsStr, &OsStr) -> std::io::Result<()>,
) -> Result<(), String> {
    let temp_name = unique_temp_name(".org-document-");
    if write_new_file(&destination.parent, &temp_name, bytes).is_err() {
        let _ = destination.parent.remove_file(&temp_name);
        return Err("Create temporary Org document export file failed".into());
    }
    before_publish();
    if publisher(&destination.parent, &temp_name, &destination.basename).is_err() {
        let _ = destination.parent.remove_file(&temp_name);
        return Err("Atomically publish Org document export failed".into());
    }
    Ok(())
}

#[cfg(test)]
fn write_atomic_file_with_hook(
    output: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(),
) -> Result<(), String> {
    let destination = ExportDestination::open(output, ExportKind::Document)?;
    write_atomic_file_to_destination(&destination, bytes, before_publish)
}

#[cfg(test)]
fn write_atomic_file_with_publisher(
    output: &Path,
    bytes: &[u8],
    publisher: impl FnOnce(&CapabilityDir, &OsStr, &OsStr) -> std::io::Result<()>,
) -> Result<(), String> {
    let destination = ExportDestination::open(output, ExportKind::Document)?;
    write_atomic_file_with_steps(&destination, bytes, || {}, publisher)
}

fn write_new_file(parent: &CapabilityDir, name: &OsStr, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut file = parent
        .open_with(name, &options)
        .map_err(|_| "Create Org snapshot file failed".to_owned())?;
    file.write_all(bytes)
        .map_err(|_| "Write Org snapshot file failed".to_owned())?;
    file.sync_all()
        .map_err(|_| "Sync Org snapshot file failed".to_owned())
}

fn unique_temp_name(prefix: &str) -> OsString {
    format!("{prefix}{}", uuid::Uuid::new_v4()).into()
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn publish_noclobber(
    parent: &CapabilityDir,
    source: &OsStr,
    destination: &OsStr,
) -> std::io::Result<()> {
    Ok(rustix_fs::renameat_with(
        parent,
        source,
        parent,
        destination,
        RenameFlags::NOREPLACE,
    )?)
}

// Safe no-clobber publication is currently implemented only where rustix
// exposes an atomic NOREPLACE rename. Other targets, including Windows, keep
// capability-safe validation and temporary writes but fail closed here rather
// than call a rename API whose contract permits replacement.
#[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
fn publish_noclobber(
    _parent: &CapabilityDir,
    _source: &OsStr,
    _destination: &OsStr,
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-clobber Org export is unsupported on this target",
    ))
}

fn read_regular_utf8_file(path: &Path) -> Result<String, String> {
    String::from_utf8(read_regular_file(path)?)
        .map_err(|_| "Org document input is not valid UTF-8".to_owned())
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| "Org input path is invalid".to_owned())?;
    let directory = open_directory_path_nofollow(parent)?;
    read_regular_file_at(&directory, name)
}

fn open_directory_path_nofollow(path: &Path) -> Result<CapabilityDir, String> {
    if path.as_os_str().is_empty() {
        return Err("Org input path is invalid".into());
    }
    let anchor_path = if path.is_absolute() {
        path.ancestors()
            .last()
            .ok_or_else(|| "Org input path has no filesystem root".to_owned())?
    } else {
        Path::new(".")
    };
    let relative = if path.is_absolute() {
        path.strip_prefix(anchor_path)
            .map_err(|_| "Org input path root is invalid".to_owned())?
    } else {
        path
    };
    let mut directory = CapabilityDir::open_ambient_dir(anchor_path, ambient_authority())
        .map_err(|_| "Open Org input anchor failed".to_owned())?;
    for component in relative.components() {
        match component {
            Component::Normal(name) => {
                directory = directory
                    .open_dir_nofollow(name)
                    .map_err(|_| "Open Org directory without symlinks failed".to_owned())?;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err("Org input path must not traverse its anchor".into());
            }
        }
    }
    Ok(directory)
}

fn open_directory_at(parent: &CapabilityDir, name: &str) -> Result<CapabilityDir, String> {
    parent
        .open_dir_nofollow(name)
        .map_err(|_| "Open Org snapshot directory without symlinks failed".to_owned())
}

fn read_regular_file_at(parent: &CapabilityDir, name: &OsStr) -> Result<Vec<u8>, String> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = parent
        .open_with(name, &options)
        .map_err(|_| "Open Org snapshot file without symlinks failed".to_owned())?;
    if !file
        .metadata()
        .map_err(|_| "Inspect opened Org snapshot file failed".to_owned())?
        .is_file()
    {
        return Err("Org snapshot input must be a regular file".into());
    }
    read_open_regular_file(&mut file)
}

fn read_open_regular_file(file: &mut CapabilityFile) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "Read Org input file failed".to_owned())?;
    Ok(bytes)
}

fn read_entry_names(directory: &CapabilityDir) -> Result<BTreeSet<String>, String> {
    let mut names = BTreeSet::new();
    let entries = directory
        .entries()
        .map_err(|_| "Read Org snapshot directory failed".to_owned())?;
    for entry in entries {
        let entry = entry.map_err(|_| "Read Org snapshot directory entry failed".to_owned())?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Org snapshot filename is not valid UTF-8".to_owned())?;
        names.insert(name);
    }
    Ok(names)
}

fn validate_content_hash(value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("Org document content hash is invalid".into());
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Org document content hash is invalid".into());
    }
    Ok(())
}

fn content_hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn parse_id<T>(value: &str, kind: &str) -> anyhow::Result<T>
where
    T: FromStr + ToString,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let parsed = value
        .parse::<T>()
        .with_context(|| format!("invalid Org {kind} ID"))?;
    if parsed.to_string() != value {
        bail!("Org {kind} ID must use canonical UUID form");
    }
    Ok(parsed)
}

fn parse_options(args: &[String]) -> anyhow::Result<BTreeMap<String, String>> {
    if !args.len().is_multiple_of(2) {
        bail!("Org offline options require a value");
    }
    let mut options = BTreeMap::new();
    for pair in args.chunks_exact(2) {
        let name = &pair[0];
        let value = &pair[1];
        if !name.starts_with("--") {
            bail!("unexpected Org offline argument: {name}");
        }
        if value.is_empty() || value != value.trim() {
            bail!("{name} must be nonblank and trimmed");
        }
        if options.insert(name.clone(), value.clone()).is_some() {
            bail!("duplicate Org offline option: {name}");
        }
    }
    Ok(options)
}

fn take_required(options: &BTreeMap<String, String>, name: &str) -> anyhow::Result<String> {
    options
        .get(name)
        .cloned()
        .ok_or_else(|| anyhow!("missing required Org offline option {name}"))
}

fn parse_import_mode(value: &str) -> anyhow::Result<ImportMode> {
    match value {
        "create" => Ok(ImportMode::Create),
        "update" => Ok(ImportMode::Update),
        _ => bail!("--mode must be create or update"),
    }
}

fn reject_unused_options(
    command: &OrgOfflineCommand,
    options: &BTreeMap<String, String>,
) -> anyhow::Result<()> {
    let allowed: &[&str] = match command {
        OrgOfflineCommand::ExportWorkspace { .. } => &["--workspace-id", "--output"],
        OrgOfflineCommand::ImportWorkspace { .. } => {
            &["--input", "--mode", "--actor-id", "--operation-id"]
        }
        OrgOfflineCommand::ExportDocument { .. } => &["--document-id", "--output"],
        OrgOfflineCommand::ImportDocument { .. } => &[
            "--workspace-id",
            "--document-id",
            "--path",
            "--input",
            "--mode",
            "--actor-id",
            "--operation-id",
            "--expected-revision",
        ],
    };
    if let Some(unexpected) = options
        .keys()
        .find(|name| !allowed.contains(&name.as_str()))
    {
        bail!("unexpected Org offline option: {unexpected}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_publish_stays_on_the_validated_parent_handle_during_ambient_swap() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let validated_parent = root.path().join("validated-parent");
        fs::create_dir(&parent).unwrap();
        let output = parent.join("document.org");

        write_atomic_file_with_hook(&output, b"anchored", || {
            fs::rename(&parent, &validated_parent).unwrap();
            fs::create_dir(&parent).unwrap();
        })
        .unwrap();

        assert_eq!(
            fs::read(validated_parent.join("document.org")).unwrap(),
            b"anchored"
        );
        assert!(fs::read_dir(&parent).unwrap().next().is_none());
        assert!(fs::read_dir(&validated_parent).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".org-")));
    }

    #[test]
    fn failed_document_publish_removes_its_parent_relative_temporary_file() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let output = parent.join("document.org");

        let error = write_atomic_file_with_hook(&output, b"new", || {
            fs::write(&output, b"existing").unwrap();
        })
        .unwrap_err();

        assert!(error.contains("publish"));
        assert_eq!(fs::read(&output).unwrap(), b"existing");
        assert_eq!(
            fs::read_dir(&parent)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>(),
            vec![std::ffi::OsString::from("document.org")]
        );
    }

    #[test]
    fn unsupported_document_publish_is_structured_and_leaves_no_output_or_temp() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        fs::create_dir(&parent).unwrap();
        let output = parent.join("document.org");

        let error =
            write_atomic_file_with_publisher(&output, b"new", |_parent, _source, _target| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "unsupported safe publish",
                ))
            })
            .unwrap_err();

        assert!(error.contains("publish"));
        assert!(fs::read_dir(&parent).unwrap().next().is_none());
    }
}
