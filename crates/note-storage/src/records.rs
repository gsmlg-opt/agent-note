use std::path::PathBuf;

pub const EMBEDDING_DIMENSION: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteChunk {
    pub note_id: String,
    pub chunk_idx: i64,
    pub content_hash: String,
    pub content: String,
    pub note_revision: i64,
    pub status: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingJob {
    pub id: i64,
    pub note_id: String,
    pub chunk_idx: i64,
    pub content_hash: String,
    pub content: String,
    pub note_revision: i64,
    pub attempts: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingEmbeddingNote {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingDashboardStatus {
    pub embedded_note_count: usize,
    pub processing_note: Option<ProcessingEmbeddingNote>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveNoteSource {
    pub id: String,
    pub content: String,
    pub note_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    pub engine: String,
    pub location: Option<PathBuf>,
    pub size_bytes: Option<u64>,
}

pub struct NewNote<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub created_at: i64,
    pub updated_at: i64,
    pub note_revision: i64,
    pub deleted_at: Option<i64>,
}

pub struct NoteUpdate<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub content: &'a str,
    pub attachments: &'a [note_core::NoteAttachment],
    pub updated_at: i64,
    pub note_revision: i64,
}

pub struct UpsertNoteChunk<'a> {
    pub note_id: &'a str,
    pub chunk_idx: i64,
    pub content_hash: &'a str,
    pub content: &'a str,
    pub note_revision: i64,
    pub status: &'a str,
    pub updated_at: i64,
}
