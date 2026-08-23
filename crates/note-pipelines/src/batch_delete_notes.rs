use crate::{
    batch_note_targets::{preflight_batch_note_targets, validate_batch_note_targets},
    BatchNoteTarget, Context,
};
use note_storage::TransactionMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchDeleteNotesInput {
    pub notes: Vec<BatchNoteTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BatchDeleteNotesResult {
    pub requested: usize,
    pub deleted: usize,
}

pub async fn batch_delete_notes(
    ctx: &Context,
    input: BatchDeleteNotesInput,
) -> anyhow::Result<BatchDeleteNotesResult> {
    let targets = validate_batch_note_targets(input.notes)?;
    let requested = targets.len();
    let transaction = ctx.storage().begin(TransactionMode::Immediate).await?;
    let transaction_result = async {
        preflight_batch_note_targets(transaction.as_ref(), &targets).await?;

        let deleted_at = chrono::Utc::now().timestamp();
        for target in &targets {
            crate::mutation_error::mutation_revision(
                &target.id,
                transaction
                    .soft_delete_note(&target.id, target.expected_revision, deleted_at)
                    .await?,
            )?;
            transaction.clear_note_search_data(&target.id).await?;
        }

        Ok(BatchDeleteNotesResult {
            requested,
            deleted: requested,
        })
    }
    .await;

    crate::save_note::finish_transaction(transaction, transaction_result).await
}
