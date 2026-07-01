pub type NoteId = String;

#[derive(Debug, Clone, PartialEq)]
pub struct LabelKey {
    pub key: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub key: String,
    pub value: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: NoteId,
    pub title: String,
    pub content: String,
    pub labels: Vec<Label>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub note: Note,
    pub score: f32, // fused RRF score — NOT raw cosine similarity, label it as such in the UI (design.md §7)
}
