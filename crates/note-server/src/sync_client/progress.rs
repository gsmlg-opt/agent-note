use note_core::sync::SyncEvent;
use serde::Serialize;
use std::collections::BTreeSet;
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncProgress {
    pub updated_at: i64,
    pub tracked_notes: Option<usize>,
    pub upload: SyncDirectionProgress,
    pub download: SyncDirectionProgress,
}

impl SyncProgress {
    pub fn new(basis: &str) -> Self {
        Self {
            updated_at: chrono::Utc::now().timestamp_millis(),
            tracked_notes: None,
            upload: SyncDirectionProgress::new(basis),
            download: SyncDirectionProgress::new(basis),
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = chrono::Utc::now().timestamp_millis();
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncDirectionProgress {
    pub run_id: String,
    pub phase: String,
    pub basis: String,
    pub notes_total: Option<usize>,
    pub notes_checked: usize,
    pub notes_equal: usize,
    pub notes_planned: usize,
    pub notes_completed: usize,
    pub notes_superseded: usize,
    pub notes_remaining: Option<usize>,
    pub blocked_notes: usize,
    pub current_note: Option<SyncCurrentNote>,
    pub active_task: Option<SyncActiveTask>,
    pub manager: Option<SyncManagerProgress>,
    #[serde(skip)]
    resolved_ids: BTreeSet<String>,
    #[serde(skip)]
    blocked_ids: BTreeSet<String>,
    #[serde(skip)]
    remote_blocked_notes: usize,
}

impl SyncDirectionProgress {
    pub fn new(basis: &str) -> Self {
        Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            phase: "checking".into(),
            basis: basis.into(),
            notes_total: None,
            notes_checked: 0,
            notes_equal: 0,
            notes_planned: 0,
            notes_completed: 0,
            notes_superseded: 0,
            notes_remaining: None,
            blocked_notes: 0,
            current_note: None,
            active_task: None,
            manager: None,
            resolved_ids: BTreeSet::new(),
            blocked_ids: BTreeSet::new(),
            remote_blocked_notes: 0,
        }
    }

    pub fn begin_note(&mut self, event: &SyncEvent) {
        self.phase = "transferring".into();
        self.current_note = Some(SyncCurrentNote {
            id: event.note_id.clone(),
            title: event
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.title.chars().take(160).collect()),
            phase: "preparing".into(),
            attachments_total: event
                .snapshot
                .as_ref()
                .map_or(0, |snapshot| snapshot.attachments.len()),
            attachments_completed: 0,
            attachment_bytes_total: event.snapshot.as_ref().map_or(0, |snapshot| {
                snapshot.attachments.iter().fold(0u64, |total, attachment| {
                    total.saturating_add(attachment.size_bytes)
                })
            }),
            attachment_bytes_done: 0,
        });
    }

    pub fn finish_check(&mut self, total: usize) {
        self.notes_total = Some(total);
        self.notes_checked = total;
        self.notes_planned = total
            .saturating_sub(self.notes_equal)
            .saturating_sub(self.notes_superseded);
        let remaining = self.notes_planned.saturating_sub(self.notes_completed);
        self.notes_remaining = Some(remaining);
        self.phase = if remaining == 0 {
            "idle"
        } else {
            "transferring"
        }
        .into();
    }

    pub fn complete_note(&mut self, note_id: &str) {
        if self.resolve_note(note_id) {
            self.notes_completed += 1;
        }
    }

    pub fn supersede_note(&mut self, note_id: &str) {
        if self.resolve_note(note_id) {
            self.notes_superseded += 1;
        }
    }

    pub fn note_equal(&mut self, note_id: &str) {
        if self.resolve_note(note_id) {
            self.notes_equal += 1;
        }
    }

    fn resolve_note(&mut self, note_id: &str) -> bool {
        if !self.resolved_ids.insert(note_id.into()) {
            return false;
        }
        if let Some(remaining) = self.notes_remaining.as_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        self.blocked_ids.remove(note_id);
        self.blocked_notes = self.remote_blocked_notes.max(self.blocked_ids.len());
        self.clear_current(note_id);
        true
    }

    pub fn block_note(&mut self, note_id: &str) {
        if !self.resolved_ids.contains(note_id) {
            self.blocked_ids.insert(note_id.into());
            self.blocked_notes = self.remote_blocked_notes.max(self.blocked_ids.len());
        }
        self.clear_current(note_id);
    }

    pub fn update_remote_blocked(&mut self, blocked: usize) {
        self.remote_blocked_notes = blocked;
        self.blocked_notes = blocked.max(self.blocked_ids.len());
    }

    fn clear_current(&mut self, note_id: &str) {
        if self
            .current_note
            .as_ref()
            .is_some_and(|current| current.id == note_id)
        {
            self.current_note = None;
        }
    }

    pub fn finish_round(&mut self) {
        self.current_note = None;
        self.phase = if self.notes_remaining.is_some_and(|remaining| remaining > 0)
            || self.blocked_notes > 0
        {
            "blocked"
        } else {
            "idle"
        }
        .into();
    }

    /// Task mode keeps successful round membership on disk. Unresolved local
    /// rejections survive other notes' confirmations until their own success.
    pub fn release_page_ids(&mut self, completed_note: &str) {
        self.resolved_ids.clear();
        self.blocked_ids.remove(completed_note);
        self.blocked_notes = self.remote_blocked_notes.max(self.blocked_ids.len());
    }
    pub fn suspend(&mut self, phase: &str) {
        self.phase = phase.into();
        self.current_note = None;
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncManagerProgress {
    pub state: Option<String>,
    pub error: Option<String>,
    pub mode: String,
    pub page_size: u64,
    pub pages_scanned: u64,
    pub pages_checked: u64,
    pub notes_scanned: u64,
    pub scan_complete: bool,
    pub source_cursor: Option<String>,
    pub last_progress_at: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncActiveTask {
    pub id: String,
    pub operation: String,
    pub state: String,
    pub phase: String,
    pub updated_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SyncCurrentNote {
    pub id: String,
    pub title: Option<String>,
    pub phase: String,
    pub attachments_total: usize,
    pub attachments_completed: usize,
    pub attachment_bytes_total: u64,
    pub attachment_bytes_done: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_core::sync::{Hlc, SyncAttachment, SyncEventKind, SyncSnapshot};

    fn event(note_id: &str) -> SyncEvent {
        SyncEvent {
            event_id: "event".into(),
            note_id: note_id.into(),
            generation: Hlc::default(),
            control: Hlc::default(),
            version: Hlc::default(),
            kind: SyncEventKind::Snapshot,
            snapshot_hash: "a".repeat(64),
            snapshot: Some(SyncSnapshot {
                title: "界".repeat(200),
                content: "private content".into(),
                labels: vec![],
                created_at: 0,
                updated_at: 0,
                deleted_at: None,
                attachments: vec![SyncAttachment {
                    id: "attachment".into(),
                    path: "private/path".into(),
                    mime: "text/plain".into(),
                    description: String::new(),
                    size_bytes: 12,
                    checksum_sha256: "b".repeat(64),
                }],
            }),
        }
    }

    fn assert_partition(progress: &SyncDirectionProgress) {
        assert_eq!(
            progress.notes_total.unwrap(),
            progress.notes_equal
                + progress.notes_completed
                + progress.notes_superseded
                + progress.notes_remaining.unwrap()
        );
    }

    #[test]
    fn a_new_round_has_unknown_counts_and_a_fresh_id() {
        let progress = SyncProgress::new("remote_verified");
        assert!(progress.updated_at > 1_000_000_000_000);
        assert_ne!(progress.upload.run_id, progress.download.run_id);
        assert!(uuid::Uuid::parse_str(&progress.upload.run_id).is_ok());
        assert_eq!(progress.upload.phase, "checking");
        assert_eq!(progress.upload.basis, "remote_verified");
        assert_eq!(progress.upload.notes_total, None);
        assert_eq!(progress.upload.notes_remaining, None);
        assert_eq!(progress.upload.notes_completed, 0);
        assert!(progress.upload.current_note.is_none());
    }

    #[test]
    fn checking_keeps_remaining_unknown_and_counts_each_note_once() {
        let mut progress = SyncDirectionProgress::new("remote_verified");
        progress.note_equal("equal");
        progress.note_equal("equal");
        progress.supersede_note("changed");
        progress.supersede_note("changed");
        assert_eq!(progress.notes_remaining, None);
        progress.finish_check(4);
        assert_eq!(progress.notes_checked, 4);
        assert_eq!(progress.notes_equal, 1);
        assert_eq!(progress.notes_superseded, 1);
        assert_eq!(progress.notes_planned, 2);
        assert_eq!(progress.notes_remaining, Some(2));
        assert_eq!(progress.phase, "transferring");
        assert_partition(&progress);
    }

    #[test]
    fn exact_confirmations_and_superseded_notes_resolve_only_once() {
        let mut progress = SyncDirectionProgress::new("local_queue");
        progress.finish_check(3);
        progress.begin_note(&event("first"));
        progress.complete_note("first");
        progress.complete_note("first");
        progress.supersede_note("first");
        progress.supersede_note("second");
        progress.supersede_note("second");
        progress.note_equal("second");
        assert_eq!(progress.notes_completed, 1);
        assert_eq!(progress.notes_superseded, 1);
        assert_eq!(progress.notes_equal, 0);
        assert_eq!(progress.notes_remaining, Some(1));
        assert_partition(&progress);
        progress.finish_round();
        assert_eq!(progress.phase, "blocked");
    }

    #[test]
    fn repeated_blocks_are_a_subset_of_unresolved_notes_and_can_recover() {
        let mut progress = SyncDirectionProgress::new("local_queue");
        progress.finish_check(1);
        progress.begin_note(&event("note"));
        progress.block_note("note");
        progress.block_note("note");
        assert_eq!(progress.blocked_notes, 1);
        assert_eq!(progress.notes_remaining, Some(1));
        assert!(progress.current_note.is_none());
        progress.finish_round();
        assert_eq!(progress.phase, "blocked");
        progress.begin_note(&event("note"));
        progress.complete_note("note");
        progress.block_note("note");
        assert_eq!(progress.blocked_notes, 0);
        assert_eq!(progress.notes_remaining, Some(0));
        progress.finish_round();
        assert_eq!(progress.phase, "idle");
        assert_partition(&progress);
    }

    #[test]
    fn remote_blocks_survive_local_resolution_and_accept_recovery() {
        let mut progress = SyncDirectionProgress::new("remote_verified");
        progress.update_remote_blocked(2);
        progress.block_note("local");
        assert_eq!(progress.blocked_notes, 2);
        progress.complete_note("local");
        assert_eq!(progress.blocked_notes, 2);
        progress.update_remote_blocked(0);
        assert_eq!(progress.blocked_notes, 0);
    }

    #[test]
    fn current_note_bounds_unicode_title_and_only_exposes_progress_fields() {
        let mut progress = SyncDirectionProgress::new("local_queue");
        progress.begin_note(&event("note"));
        let current = progress.current_note.as_ref().unwrap();
        assert_eq!(current.title.as_ref().unwrap(), &"界".repeat(160));
        assert_eq!(current.phase, "preparing");
        assert_eq!(current.attachments_total, 1);
        assert_eq!(current.attachments_completed, 0);
        assert_eq!(current.attachment_bytes_total, 12);
        assert_eq!(current.attachment_bytes_done, 0);
        let json = serde_json::to_string(&progress).unwrap();
        assert!(!json.contains("private"));
        assert!(!json.contains("resolved"));
        assert!(!json.contains("blocked_ids"));
    }

    #[test]
    fn zero_work_and_control_notes_need_no_attachments() {
        let mut progress = SyncDirectionProgress::new("local_queue");
        progress.finish_check(0);
        assert_eq!(progress.phase, "idle");
        assert_eq!(progress.notes_remaining, Some(0));
        assert_partition(&progress);
        let mut control = event("control");
        control.snapshot = None;
        progress.begin_note(&control);
        let current = progress.current_note.as_ref().unwrap();
        assert_eq!(current.title, None);
        assert_eq!(current.attachments_total, 0);
        assert_eq!(current.attachment_bytes_total, 0);
    }

    #[test]
    fn observed_downloads_do_not_invent_a_remaining_count() {
        let mut progress = SyncDirectionProgress::new("legacy_replay");
        progress.complete_note("one");
        progress.complete_note("one");
        progress.notes_total = Some(20);
        progress.finish_round();
        assert_eq!(progress.notes_completed, 1);
        assert_eq!(progress.notes_remaining, None);
    }

    #[test]
    fn duplicate_ack_does_not_clear_the_next_note_and_suspend_preserves_counts() {
        let mut progress = SyncProgress::new("local_queue");
        progress.upload.finish_check(2);
        progress.upload.complete_note("first");
        progress.upload.begin_note(&event("second"));
        progress.upload.complete_note("first");
        assert_eq!(progress.upload.current_note.as_ref().unwrap().id, "second");
        progress.upload.suspend("reconnecting");
        assert_eq!(progress.upload.phase, "reconnecting");
        assert_eq!(progress.upload.notes_completed, 1);
        assert_eq!(progress.upload.notes_remaining, Some(1));
        assert!(progress.upload.current_note.is_none());
        progress.updated_at = 0;
        progress.touch();
        assert!(progress.updated_at > 1_000_000_000_000);
    }
}
