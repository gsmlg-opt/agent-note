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
pub struct LabelSelector {
    pub key: String,
    pub value: Option<String>,
}

pub fn parse_label_selectors(input: &str) -> Vec<LabelSelector> {
    input
        .split('&')
        .filter_map(|term| {
            let term = term.trim();
            if term.is_empty() {
                return None;
            }

            match term.split_once('=') {
                Some((key, value)) => Some(LabelSelector {
                    key: key.trim().to_string(),
                    value: Some(value.trim().to_string()),
                }),
                None => Some(LabelSelector {
                    key: term.to_string(),
                    value: None,
                }),
            }
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_empty_selector_string() {
        assert_eq!(parse_label_selectors(""), vec![]);
    }

    #[test]
    fn parses_key_value_selector() {
        assert_eq!(
            parse_label_selectors("env=prod"),
            vec![LabelSelector {
                key: "env".to_string(),
                value: Some("prod".to_string()),
            }]
        );
    }

    #[test]
    fn parses_bare_key_selector() {
        assert_eq!(
            parse_label_selectors("archived"),
            vec![LabelSelector {
                key: "archived".to_string(),
                value: None,
            }]
        );
    }

    #[test]
    fn parses_multiple_anded_selectors() {
        assert_eq!(
            parse_label_selectors("env=prod&team=core"),
            vec![
                LabelSelector {
                    key: "env".to_string(),
                    value: Some("prod".to_string()),
                },
                LabelSelector {
                    key: "team".to_string(),
                    value: Some("core".to_string()),
                },
            ]
        );
    }

    #[test]
    fn ignores_empty_terms_from_duplicate_ampersands() {
        assert_eq!(
            parse_label_selectors("&env=prod&&team=core&"),
            vec![
                LabelSelector {
                    key: "env".to_string(),
                    value: Some("prod".to_string()),
                },
                LabelSelector {
                    key: "team".to_string(),
                    value: Some("core".to_string()),
                },
            ]
        );
    }

    #[test]
    fn trims_whitespace() {
        assert_eq!(
            parse_label_selectors(" env = prod & archived "),
            vec![
                LabelSelector {
                    key: "env".to_string(),
                    value: Some("prod".to_string()),
                },
                LabelSelector {
                    key: "archived".to_string(),
                    value: None,
                },
            ]
        );
    }
}
