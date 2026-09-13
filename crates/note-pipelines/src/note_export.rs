use crate::Context;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{
    BufferQueue, StartTag, TagToken, Token, TokenSink, TokenSinkResult, Tokenizer,
};
use note_attachments::BoundedReadError;
use note_core::{normalize_attachment_path, Label, Note, NoteAttachment};
use note_storage::TransactionMode;
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Parser, Tag};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

pub const DEFAULT_MAX_MARKDOWN_BYTES: u64 = 2 * 1024 * 1024;
pub const DEFAULT_MAX_ASSET_COUNT: usize = 64;
pub const DEFAULT_MAX_ASSET_BYTES: u64 = 8 * 1024 * 1024;
pub const DEFAULT_MAX_COMBINED_ASSET_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteExportLimits {
    pub max_markdown_bytes: u64,
    pub max_asset_count: usize,
    pub max_asset_bytes: u64,
    pub max_combined_asset_bytes: u64,
}

impl Default for NoteExportLimits {
    fn default() -> Self {
        Self {
            max_markdown_bytes: DEFAULT_MAX_MARKDOWN_BYTES,
            max_asset_count: DEFAULT_MAX_ASSET_COUNT,
            max_asset_bytes: DEFAULT_MAX_ASSET_BYTES,
            max_combined_asset_bytes: DEFAULT_MAX_COMBINED_ASSET_BYTES,
        }
    }
}

impl NoteExportLimits {
    pub fn validate(self) -> Result<Self, NoteExportError> {
        if self.max_markdown_bytes == 0
            || self.max_asset_count == 0
            || self.max_asset_bytes == 0
            || self.max_combined_asset_bytes == 0
            || self.max_markdown_bytes > DEFAULT_MAX_MARKDOWN_BYTES
            || self.max_asset_count > DEFAULT_MAX_ASSET_COUNT
            || self.max_asset_bytes > DEFAULT_MAX_ASSET_BYTES
            || self.max_combined_asset_bytes > DEFAULT_MAX_COMBINED_ASSET_BYTES
        {
            return Err(NoteExportError::InvalidInput);
        }
        Ok(self)
    }
}

/// A note state captured by [`capture_note_export`] inside a storage snapshot.
///
/// Callers cannot manufacture a captured state without the capture workflow.
///
/// ```compile_fail
/// use note_pipelines::CapturedNoteExport;
/// let _ = CapturedNoteExport::from_note;
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct CapturedNoteExport {
    note: Note,
}

impl CapturedNoteExport {
    fn new(note: Note) -> Result<Self, NoteExportError> {
        if note.deleted_at.is_some() {
            return Err(NoteExportError::NoteNotFound);
        }
        if note.revision <= 0 || note.id.trim().is_empty() {
            return Err(NoteExportError::InvalidInput);
        }
        if note.content.len() as u64 > DEFAULT_MAX_MARKDOWN_BYTES {
            return Err(NoteExportError::MarkdownLimitExceeded);
        }
        Ok(Self { note })
    }

    pub fn note(&self) -> &Note {
        &self.note
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportAssetKind {
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAssetOmission {
    pub kind: ExportAssetKind,
    pub destination: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAttachmentAsset {
    destination: String,
    attachment: NoteAttachment,
}

impl PlannedAttachmentAsset {
    pub fn destination(&self) -> &str {
        &self.destination
    }

    pub fn attachment(&self) -> &NoteAttachment {
        &self.attachment
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteExportAssetPlan {
    captured_note_id: String,
    captured_revision: i64,
    referenced_image_count: usize,
    assets: Vec<PlannedAttachmentAsset>,
    omissions: Vec<ExportAssetOmission>,
    data_images: Vec<String>,
}

impl NoteExportAssetPlan {
    pub fn assets(&self) -> &[PlannedAttachmentAsset] {
        &self.assets
    }

    pub fn omissions(&self) -> &[ExportAssetOmission] {
        &self.omissions
    }

    pub fn data_images(&self) -> &[String] {
        &self.data_images
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HydratedNoteExportAsset {
    pub destination: String,
    pub attachment: NoteAttachment,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FrozenNoteExport {
    pub note: Note,
    pub assets: Vec<HydratedNoteExportAsset>,
    pub omissions: Vec<ExportAssetOmission>,
    pub data_images: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub enum NoteExportError {
    InvalidExpectedRevision,
    InvalidInput,
    MismatchedAssetPlan,
    NoteNotFound,
    StaleRevision { expected: i64, current: i64 },
    MarkdownLimitExceeded,
    AssetCountLimitExceeded,
    AssetLimitExceeded { attachment_id: String },
    CombinedAssetLimitExceeded,
    AssetInvalid { destination: String },
    AssetMissing { destination: String },
    AssetChecksumMismatch { attachment_id: String },
    AssetSizeMismatch { attachment_id: String },
    StorageFailure,
    UnsupportedBoundedRead,
}

impl fmt::Debug for NoteExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidExpectedRevision => "InvalidExpectedRevision",
            Self::InvalidInput => "InvalidInput",
            Self::MismatchedAssetPlan => "MismatchedAssetPlan",
            Self::NoteNotFound => "NoteNotFound",
            Self::StaleRevision { .. } => "StaleRevision",
            Self::MarkdownLimitExceeded => "MarkdownLimitExceeded",
            Self::AssetCountLimitExceeded => "AssetCountLimitExceeded",
            Self::AssetLimitExceeded { .. } => "AssetLimitExceeded",
            Self::CombinedAssetLimitExceeded => "CombinedAssetLimitExceeded",
            Self::AssetInvalid { .. } => "AssetInvalid",
            Self::AssetMissing { .. } => "AssetMissing",
            Self::AssetChecksumMismatch { .. } => "AssetChecksumMismatch",
            Self::AssetSizeMismatch { .. } => "AssetSizeMismatch",
            Self::StorageFailure => "StorageFailure",
            Self::UnsupportedBoundedRead => "UnsupportedBoundedRead",
        })
    }
}

impl fmt::Display for NoteExportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidExpectedRevision => "expected revision must be positive",
            Self::InvalidInput => "invalid note export limits",
            Self::MismatchedAssetPlan => "note export asset plan does not match captured note",
            Self::NoteNotFound => "note not found",
            Self::StaleRevision { .. } => "note revision is stale",
            Self::MarkdownLimitExceeded => "note Markdown exceeds export limit",
            Self::AssetCountLimitExceeded => "note export image count exceeds limit",
            Self::AssetLimitExceeded { .. } => "note export image exceeds individual limit",
            Self::CombinedAssetLimitExceeded => "note export images exceed combined limit",
            Self::AssetInvalid { .. } => "note export image attachment is invalid",
            Self::AssetMissing { .. } => "note export image attachment is missing",
            Self::AssetChecksumMismatch { .. } => {
                "note export image checksum does not match metadata"
            }
            Self::AssetSizeMismatch { .. } => "note export image size does not match metadata",
            Self::StorageFailure => "note export storage operation failed",
            Self::UnsupportedBoundedRead => "attachment store does not support bounded reads",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for NoteExportError {}

pub fn plan_note_export_assets(
    captured: &CapturedNoteExport,
) -> Result<NoteExportAssetPlan, NoteExportError> {
    let destinations = image_destinations(&captured.note.content);
    let mut seen_destinations = HashSet::new();
    let mut seen_attachments = HashSet::new();
    let mut assets = Vec::new();
    let mut omissions = Vec::new();
    let mut data_images = Vec::new();

    for destination in destinations {
        if !seen_destinations.insert(destination.clone()) {
            continue;
        }
        if seen_destinations.len() > DEFAULT_MAX_ASSET_COUNT {
            return Err(NoteExportError::AssetCountLimitExceeded);
        }
        let lower = destination.to_ascii_lowercase();
        if lower.starts_with("data:") {
            data_images.push(destination);
            continue;
        }
        if is_external_destination(&destination) {
            omissions.push(ExportAssetOmission {
                kind: ExportAssetKind::External,
                destination,
            });
            continue;
        }

        let attachment = resolve_captured_attachment(&captured.note.attachments, &destination)
            .ok_or_else(|| NoteExportError::AssetMissing {
                destination: destination.clone(),
            })?;
        if !attachment.mime.to_ascii_lowercase().starts_with("image/") {
            return Err(NoteExportError::AssetInvalid { destination });
        }
        if seen_attachments.insert(attachment.id.clone()) {
            assets.push(PlannedAttachmentAsset {
                destination,
                attachment: attachment.clone(),
            });
        }
    }

    Ok(NoteExportAssetPlan {
        captured_note_id: captured.note.id.clone(),
        captured_revision: captured.note.revision,
        referenced_image_count: seen_destinations.len(),
        assets,
        omissions,
        data_images,
    })
}

fn resolve_captured_attachment<'a>(
    attachments: &'a [NoteAttachment],
    destination: &str,
) -> Option<&'a NoteAttachment> {
    let raw = normalize_attachment_path(destination);
    let decoded = percent_decode_str(destination)
        .decode_utf8()
        .ok()
        .map(|value| normalize_attachment_path(&value));
    attachments
        .iter()
        .find(|attachment| normalize_attachment_path(&attachment.path) == raw)
        .or_else(|| {
            attachments.iter().find(|attachment| {
                let path = normalize_attachment_path(&attachment.path);
                decoded.as_deref().is_some_and(|decoded| path == decoded)
            })
        })
}

fn is_external_destination(destination: &str) -> bool {
    if destination.starts_with("//") {
        return true;
    }
    let Some((scheme, _)) = destination.split_once(':') else {
        return false;
    };
    !scheme.is_empty()
        && scheme.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'A'..=b'Z' => true,
            b'0'..=b'9' | b'+' | b'-' | b'.' => index > 0,
            _ => false,
        })
}

fn image_destinations(markdown: &str) -> Vec<String> {
    let mut destinations = Vec::new();
    for event in Parser::new(markdown) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => destinations.push(dest_url.into_string()),
            Event::Html(html) | Event::InlineHtml(html) => {
                destinations.extend(html_image_sources(&html));
            }
            _ => {}
        }
    }
    destinations
}

fn html_image_sources(fragment: &str) -> Vec<String> {
    struct ImageSourceSink(RefCell<Vec<String>>);

    impl TokenSink for ImageSourceSink {
        type Handle = ();

        fn process_token(&self, token: Token, _line_number: u64) -> TokenSinkResult<()> {
            if let TagToken(tag) = token {
                if tag.kind == StartTag && tag.name.as_ref() == "img" {
                    if let Some(src) = tag
                        .attrs
                        .iter()
                        .find(|attribute| attribute.name.local.as_ref() == "src")
                    {
                        self.0.borrow_mut().push(src.value.to_string());
                    }
                }
            }
            TokenSinkResult::Continue
        }
    }

    let input = BufferQueue::default();
    input.push_back(StrTendril::from(fragment));
    let tokenizer = Tokenizer::new(
        ImageSourceSink(RefCell::new(Vec::new())),
        Default::default(),
    );
    let _ = tokenizer.feed(&input);
    tokenizer.end();
    tokenizer.sink.0.into_inner()
}

pub async fn capture_note_export(
    ctx: &Context,
    note_id: &str,
    expected_revision: i64,
) -> Result<CapturedNoteExport, NoteExportError> {
    capture_note_export_with_limits(ctx, note_id, expected_revision, NoteExportLimits::default())
        .await
}

pub async fn capture_note_export_with_limits(
    ctx: &Context,
    note_id: &str,
    expected_revision: i64,
    limits: NoteExportLimits,
) -> Result<CapturedNoteExport, NoteExportError> {
    if expected_revision <= 0 {
        return Err(NoteExportError::InvalidExpectedRevision);
    }
    if note_id.trim().is_empty() {
        return Err(NoteExportError::InvalidInput);
    }
    let limits = limits.validate()?;
    let transaction = ctx
        .storage()
        .begin(TransactionMode::Snapshot)
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    let note = transaction
        .get_note(note_id)
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    transaction
        .rollback()
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    let note = note.ok_or(NoteExportError::NoteNotFound)?;
    if note.revision != expected_revision {
        return Err(NoteExportError::StaleRevision {
            expected: expected_revision,
            current: note.revision,
        });
    }
    if note.content.len() as u64 > limits.max_markdown_bytes {
        return Err(NoteExportError::MarkdownLimitExceeded);
    }
    CapturedNoteExport::new(note)
}

pub async fn hydrate_note_export(
    ctx: &Context,
    captured: CapturedNoteExport,
    plan: NoteExportAssetPlan,
    limits: NoteExportLimits,
) -> Result<FrozenNoteExport, NoteExportError> {
    let limits = limits.validate()?;
    if plan.captured_note_id != captured.note.id || plan.captured_revision != captured.note.revision
    {
        return Err(NoteExportError::MismatchedAssetPlan);
    }
    if captured.note.content.len() as u64 > limits.max_markdown_bytes {
        return Err(NoteExportError::MarkdownLimitExceeded);
    }
    if plan.referenced_image_count > limits.max_asset_count {
        return Err(NoteExportError::AssetCountLimitExceeded);
    }
    let mut combined = 0_u64;
    for planned in &plan.assets {
        if let Some(storage) = &planned.attachment.storage {
            if storage.size_bytes > limits.max_asset_bytes {
                return Err(NoteExportError::AssetLimitExceeded {
                    attachment_id: planned.attachment.id.clone(),
                });
            }
            combined = combined
                .checked_add(storage.size_bytes)
                .ok_or(NoteExportError::CombinedAssetLimitExceeded)?;
            if combined > limits.max_combined_asset_bytes {
                return Err(NoteExportError::CombinedAssetLimitExceeded);
            }
        }
    }
    let mut assets = Vec::with_capacity(plan.assets.len());
    for planned in plan.assets {
        let read = match &planned.attachment.storage {
            Some(storage) => {
                ctx.attachments()
                    .read_object_bounded(&storage.object_key, limits.max_asset_bytes)
                    .await
            }
            None => {
                ctx.attachments()
                    .read_legacy_bounded(
                        &captured.note.id,
                        &planned.attachment.path,
                        limits.max_asset_bytes,
                    )
                    .await
            }
        };
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(error) => {
                confirm_snapshot(ctx, &captured.note).await?;
                match error.downcast_ref::<BoundedReadError>() {
                    Some(BoundedReadError::Unsupported) => {
                        return Err(NoteExportError::UnsupportedBoundedRead)
                    }
                    Some(BoundedReadError::LimitExceeded) => {
                        return Err(NoteExportError::AssetLimitExceeded {
                            attachment_id: planned.attachment.id,
                        })
                    }
                    Some(BoundedReadError::Missing) => {
                        return Err(NoteExportError::AssetMissing {
                            destination: planned.destination,
                        })
                    }
                    Some(BoundedReadError::StorageFailure) | None => {
                        return Err(NoteExportError::StorageFailure)
                    }
                }
            }
        };
        if let Some(storage) = &planned.attachment.storage {
            if bytes.len() as u64 != storage.size_bytes {
                confirm_snapshot(ctx, &captured.note).await?;
                return Err(NoteExportError::AssetSizeMismatch {
                    attachment_id: planned.attachment.id.clone(),
                });
            }
            if format!("{:x}", Sha256::digest(&bytes)) != storage.checksum_sha256 {
                confirm_snapshot(ctx, &captured.note).await?;
                return Err(NoteExportError::AssetChecksumMismatch {
                    attachment_id: planned.attachment.id.clone(),
                });
            }
        }
        if planned.attachment.storage.is_none() {
            combined = combined
                .checked_add(bytes.len() as u64)
                .ok_or(NoteExportError::CombinedAssetLimitExceeded)?;
            if combined > limits.max_combined_asset_bytes {
                return Err(NoteExportError::CombinedAssetLimitExceeded);
            }
        }
        assets.push(HydratedNoteExportAsset {
            destination: planned.destination,
            attachment: planned.attachment,
            bytes,
        });
    }
    confirm_snapshot(ctx, &captured.note).await?;
    Ok(FrozenNoteExport {
        note: captured.note,
        assets,
        omissions: plan.omissions,
        data_images: plan.data_images,
    })
}

pub async fn freeze_note_export(
    ctx: &Context,
    note_id: &str,
    expected_revision: i64,
    limits: NoteExportLimits,
) -> Result<FrozenNoteExport, NoteExportError> {
    let captured = capture_note_export_with_limits(ctx, note_id, expected_revision, limits).await?;
    let plan = plan_note_export_assets(&captured)?;
    hydrate_note_export(ctx, captured, plan, limits).await
}

async fn confirm_snapshot(ctx: &Context, captured: &Note) -> Result<(), NoteExportError> {
    let transaction = ctx
        .storage()
        .begin(TransactionMode::Snapshot)
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    let current = transaction
        .get_note(&captured.id)
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    transaction
        .rollback()
        .await
        .map_err(|_| NoteExportError::StorageFailure)?;
    match current {
        None => Err(NoteExportError::NoteNotFound),
        Some(current) if !note_snapshots_equal(captured, &current) => {
            Err(NoteExportError::StaleRevision {
                expected: captured.revision,
                current: current.revision,
            })
        }
        Some(_) => Ok(()),
    }
}

fn note_snapshots_equal(captured: &Note, current: &Note) -> bool {
    captured.id == current.id
        && captured.title == current.title
        && captured.content == current.content
        && captured.attachments == current.attachments
        && labels_semantically_equal(&captured.labels, &current.labels)
        && captured.created_at == current.created_at
        && captured.updated_at == current.updated_at
        && captured.revision == current.revision
        && captured.deleted_at == current.deleted_at
}

fn labels_semantically_equal(captured: &[Label], current: &[Label]) -> bool {
    fn canonical(labels: &[Label]) -> Vec<(&str, &str, &str, &str)> {
        let mut labels = labels
            .iter()
            .map(|label| {
                (
                    label.key.as_str(),
                    label.value.as_str(),
                    label.description.as_str(),
                    label.value_type.as_str(),
                )
            })
            .collect::<Vec<_>>();
        labels.sort_unstable();
        labels
    }

    canonical(captured) == canonical(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_core::{Label, LabelValueType};

    fn note_with_labels(labels: Vec<Label>) -> Note {
        Note {
            id: "note".into(),
            title: "title".into(),
            content: "body".into(),
            attachments: vec![],
            labels,
            created_at: 1,
            updated_at: 2,
            revision: 3,
            deleted_at: None,
        }
    }

    fn label(key: &str, value: &str) -> Label {
        Label {
            key: key.into(),
            value: value.into(),
            description: format!("{key} label"),
            value_type: LabelValueType::Text,
        }
    }

    #[test]
    fn snapshot_comparison_accepts_label_reordering_but_rejects_label_changes() {
        let captured = note_with_labels(vec![label("status", "ready"), label("team", "core")]);
        let reordered = note_with_labels(vec![label("team", "core"), label("status", "ready")]);
        let changed = note_with_labels(vec![label("team", "other"), label("status", "ready")]);

        assert!(note_snapshots_equal(&captured, &reordered));
        assert!(!note_snapshots_equal(&captured, &changed));
    }
}
