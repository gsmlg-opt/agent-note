#[cfg(test)]
extern crate self as note_pipelines;

pub mod context;
pub use context::*;

pub mod org;

pub mod note_attachments;
pub use note_attachments::*;

pub mod chunk;
pub use chunk::*;

pub mod embedding_queue;
pub use embedding_queue::*;

pub mod label_keys;
pub use label_keys::*;

pub mod get_note;
pub use get_note::*;

pub mod list_notes;
pub use list_notes::*;

pub mod save_note;
pub use save_note::*;

pub mod search_notes;
pub use search_notes::*;

pub mod update_note;
pub use update_note::*;

pub mod edit_note;
pub use edit_note::*;

pub mod export;
pub use export::*;

pub mod system;
pub use system::*;
