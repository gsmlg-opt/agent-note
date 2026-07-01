#[derive(Debug, Clone, PartialEq, Default)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SearchResultSummary {
    pub id: String,
    pub title: String,
    pub score: f32, // fused RRF score, label as such (docs/design.md §7) — not "similarity"
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    pub notes: Vec<NoteSummary>,
    pub search_results: Vec<SearchResultSummary>,
    pub loading: bool,
    pub error: Option<String>,
}

pub enum Action {
    SearchStarted,
    SearchSucceeded(Vec<SearchResultSummary>),
    SearchFailed(String),
}

pub fn reduce(state: &AppState, action: Action) -> AppState {
    match action {
        Action::SearchStarted => AppState { loading: true, error: None, ..state.clone() },
        Action::SearchSucceeded(results) => AppState { loading: false, search_results: results, ..state.clone() },
        Action::SearchFailed(err) => AppState { loading: false, error: Some(err), ..state.clone() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_started_sets_loading_and_clears_error() {
        let state = AppState { error: Some("old error".into()), ..Default::default() };
        let next = reduce(&state, Action::SearchStarted);
        assert!(next.loading);
        assert_eq!(next.error, None);
    }

    #[test]
    fn search_succeeded_stores_results_and_clears_loading() {
        let state = AppState { loading: true, ..Default::default() };
        let results = vec![SearchResultSummary { id: "1".into(), title: "T".into(), score: 0.5 }];
        let next = reduce(&state, Action::SearchSucceeded(results.clone()));
        assert!(!next.loading);
        assert_eq!(next.search_results, results);
    }

    #[test]
    fn search_failed_stores_error_and_clears_loading() {
        let state = AppState { loading: true, ..Default::default() };
        let next = reduce(&state, Action::SearchFailed("boom".into()));
        assert!(!next.loading);
        assert_eq!(next.error, Some("boom".into()));
    }
}
