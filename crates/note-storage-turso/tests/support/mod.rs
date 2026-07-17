use note_storage::{NewNote, NotesRepository};

#[allow(dead_code)]
pub struct Fixture {
    pub _dir: tempfile::TempDir,
    pub storage: note_storage_turso::TursoStorage,
    pub session: note_storage_turso::TursoSession,
}

pub async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let storage = note_storage_turso::TursoStorage::open(dir.path().join("test.db"))
        .await
        .unwrap();
    let session = storage.connect().await.unwrap();
    Fixture {
        _dir: dir,
        storage,
        session,
    }
}

#[allow(dead_code)]
pub async fn insert_test_note(session: &note_storage_turso::TursoSession, id: &str) {
    session
        .insert_note(NewNote {
            id,
            title: id,
            content: "content",
            attachments: &[],
            created_at: 1,
            updated_at: 1,
            note_revision: 1,
            deleted_at: None,
        })
        .await
        .unwrap();
}

#[allow(dead_code)]
pub fn unit(axis: usize) -> Vec<f32> {
    let mut vector = vec![0.0; note_storage::EMBEDDING_DIMENSION];
    vector[axis] = 1.0;
    vector
}
