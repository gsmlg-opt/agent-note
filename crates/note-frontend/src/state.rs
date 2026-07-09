//! Plain view-model types shared across pages. (The app is now multi-page via yew-router, so
//! state lives locally in each page rather than in one global reducer.)

#[derive(Debug, Clone, PartialEq)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub content: String,
    /// Attached labels as (key, value) pairs.
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResultSummary {
    pub id: String,
    pub title: String,
    /// Fused RRF rank-fusion score — label it as such in the UI, not "similarity" (docs/design.md §7).
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelKey {
    pub key: String,
    pub description: String,
    pub value_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelFilter {
    pub key: String,
    pub operator: String,
    pub value: String,
}
