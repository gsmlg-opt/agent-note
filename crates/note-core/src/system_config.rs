use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const MAX_DUPLICATE_CHECK_RULES: usize = 64;
pub const MAX_DUPLICATE_CHECK_TERMS: usize = 32;
pub const DEFAULT_MINIMUM_SEARCH_SCORE: f32 = 0.01;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SystemConfig {
    #[serde(default)]
    pub category_labels: Vec<String>,
    #[serde(default)]
    pub duplicate_check: DuplicateCheckConfig,
    #[serde(default)]
    pub search: SearchConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_minimum_search_score")]
    pub minimum_score: f32,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            minimum_score: DEFAULT_MINIMUM_SEARCH_SCORE,
        }
    }
}

fn default_minimum_search_score() -> f32 {
    DEFAULT_MINIMUM_SEARCH_SCORE
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub rules: Vec<DuplicateCheckRule>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckRule {
    pub terms: Vec<DuplicateCheckTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateCheckTerm {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SystemConfigValidationError {
    InvalidSearchMinimumScore,
    EmptyCategoryLabel { index: usize },
    CategoryLabelHasOuterWhitespace { index: usize },
    DuplicateCategoryLabel { key: String },
    TooManyRules { count: usize },
    EmptyRule { rule: usize },
    TooManyTerms { rule: usize, count: usize },
    EmptyTermKey { rule: usize, term: usize },
    TermKeyHasOuterWhitespace { rule: usize, term: usize },
    DuplicateTermKey { rule: usize, key: String },
}

impl std::fmt::Display for SystemConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSearchMinimumScore => {
                write!(f, "search minimum score must be finite and non-negative")
            }
            Self::EmptyCategoryLabel { index } => {
                write!(f, "category label {} must have a label key", index + 1)
            }
            Self::CategoryLabelHasOuterWhitespace { index } => write!(
                f,
                "category label {} key must not have leading or trailing whitespace",
                index + 1
            ),
            Self::DuplicateCategoryLabel { key } => {
                write!(f, "category label key {key} is configured more than once")
            }
            Self::TooManyRules { count } => write!(
                f,
                "duplicate check supports at most {MAX_DUPLICATE_CHECK_RULES} rules, got {count}"
            ),
            Self::EmptyRule { rule } => {
                write!(
                    f,
                    "duplicate check rule {} must have at least one term",
                    rule + 1
                )
            }
            Self::TooManyTerms { rule, count } => write!(
                f,
                "duplicate check rule {} supports at most {MAX_DUPLICATE_CHECK_TERMS} terms, got {count}",
                rule + 1
            ),
            Self::EmptyTermKey { rule, term } => write!(
                f,
                "duplicate check rule {} term {} must have a label key",
                rule + 1,
                term + 1
            ),
            Self::TermKeyHasOuterWhitespace { rule, term } => write!(
                f,
                "duplicate check rule {} term {} label key must not have leading or trailing whitespace",
                rule + 1,
                term + 1
            ),
            Self::DuplicateTermKey { rule, key } => write!(
                f,
                "duplicate check rule {} contains label key {key} more than once",
                rule + 1
            ),
        }
    }
}

impl std::error::Error for SystemConfigValidationError {}

#[derive(Debug, PartialEq, Eq)]
pub enum CategoryLabelConfigError {
    UnknownKey { key: String },
    ConfiguredKeyCannotBeDeleted { key: String },
}

impl std::fmt::Display for CategoryLabelConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownKey { key } => write!(f, "unknown category label key: {key}"),
            Self::ConfiguredKeyCannotBeDeleted { key } => write!(
                f,
                "label key {key} is configured as a category; remove it from System settings first"
            ),
        }
    }
}

impl std::error::Error for CategoryLabelConfigError {}

pub fn validate_system_config(config: &SystemConfig) -> Result<(), SystemConfigValidationError> {
    if !config.search.minimum_score.is_finite() || config.search.minimum_score < 0.0 {
        return Err(SystemConfigValidationError::InvalidSearchMinimumScore);
    }

    let mut category_keys = HashSet::new();
    for (index, key) in config.category_labels.iter().enumerate() {
        let trimmed = key.trim();
        if trimmed.is_empty() {
            return Err(SystemConfigValidationError::EmptyCategoryLabel { index });
        }
        if trimmed != key {
            return Err(SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index });
        }
        if !category_keys.insert(key.as_str()) {
            return Err(SystemConfigValidationError::DuplicateCategoryLabel { key: key.clone() });
        }
    }

    if config.duplicate_check.rules.len() > MAX_DUPLICATE_CHECK_RULES {
        return Err(SystemConfigValidationError::TooManyRules {
            count: config.duplicate_check.rules.len(),
        });
    }
    for (rule_index, rule) in config.duplicate_check.rules.iter().enumerate() {
        if rule.terms.is_empty() {
            return Err(SystemConfigValidationError::EmptyRule { rule: rule_index });
        }
        if rule.terms.len() > MAX_DUPLICATE_CHECK_TERMS {
            return Err(SystemConfigValidationError::TooManyTerms {
                rule: rule_index,
                count: rule.terms.len(),
            });
        }

        let mut keys = HashSet::new();
        for (term_index, term) in rule.terms.iter().enumerate() {
            let key = term.key.trim();
            if key.is_empty() {
                return Err(SystemConfigValidationError::EmptyTermKey {
                    rule: rule_index,
                    term: term_index,
                });
            }
            if key != term.key {
                return Err(SystemConfigValidationError::TermKeyHasOuterWhitespace {
                    rule: rule_index,
                    term: term_index,
                });
            }
            if !keys.insert(key) {
                return Err(SystemConfigValidationError::DuplicateTermKey {
                    rule: rule_index,
                    key: key.to_string(),
                });
            }
        }
    }
    Ok(())
}

pub fn resolve_duplicate_rule(
    rule: &DuplicateCheckRule,
    labels: &[(String, String)],
) -> Option<Vec<(String, String)>> {
    rule.terms
        .iter()
        .map(|term| {
            let input_value = labels
                .iter()
                .find_map(|(key, value)| (key == &term.key).then_some(value))?;
            match &term.value {
                Some(fixed_value) if input_value != fixed_value => None,
                Some(fixed_value) => Some((term.key.clone(), fixed_value.clone())),
                None => Some((term.key.clone(), input_value.clone())),
            }
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
pub struct DuplicateNoteError {
    pub existing_note_id: String,
    pub labels: Vec<(String, String)>,
}

impl std::fmt::Display for DuplicateNoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let labels = self
            .labels
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" + ");
        write!(
            f,
            "duplicate note: {labels} already exists ({})",
            self.existing_note_id
        )
    }
}

impl std::error::Error for DuplicateNoteError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_labels_default_to_empty() {
        assert!(SystemConfig::default().category_labels.is_empty());
    }

    #[test]
    fn category_labels_deserialize_when_omitted() {
        let config: SystemConfig = serde_json::from_str(r#"{"duplicate_check": {}}"#).unwrap();

        assert!(config.category_labels.is_empty());
    }

    #[test]
    fn search_minimum_score_defaults_to_point_zero_one() {
        assert_eq!(SystemConfig::default().search.minimum_score, 0.01);
    }

    #[test]
    fn search_minimum_score_deserializes_to_default_when_search_is_omitted() {
        let config: SystemConfig = serde_json::from_str(r#"{"duplicate_check": {}}"#).unwrap();

        assert_eq!(config.search.minimum_score, 0.01);
    }

    #[test]
    fn accepts_finite_non_negative_search_minimum_scores() {
        for minimum_score in [0.0, 0.01, 1.0, f32::MAX] {
            let mut config = SystemConfig::default();
            config.search.minimum_score = minimum_score;

            assert_eq!(validate_system_config(&config), Ok(()));
        }
    }

    #[test]
    fn rejects_invalid_search_minimum_scores() {
        for minimum_score in [-0.01, f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut config = SystemConfig::default();
            config.search.minimum_score = minimum_score;

            assert_eq!(
                validate_system_config(&config),
                Err(SystemConfigValidationError::InvalidSearchMinimumScore)
            );
        }
    }

    #[test]
    fn accepts_ordered_category_labels_without_normalizing_keys() {
        let config = SystemConfig {
            category_labels: vec!["Project.Team".to_string(), "project".to_string()],
            search: SearchConfig::default(),
            ..SystemConfig::default()
        };

        assert_eq!(validate_system_config(&config), Ok(()));
        assert_eq!(
            config.category_labels,
            vec!["Project.Team".to_string(), "project".to_string()]
        );
    }

    #[test]
    fn rejects_invalid_category_label_shapes() {
        for (labels, expected) in [
            (
                vec!["".to_string()],
                SystemConfigValidationError::EmptyCategoryLabel { index: 0 },
            ),
            (
                vec![" \t ".to_string()],
                SystemConfigValidationError::EmptyCategoryLabel { index: 0 },
            ),
            (
                vec![" project ".to_string()],
                SystemConfigValidationError::CategoryLabelHasOuterWhitespace { index: 0 },
            ),
            (
                vec!["project".to_string(), "project".to_string()],
                SystemConfigValidationError::DuplicateCategoryLabel {
                    key: "project".to_string(),
                },
            ),
        ] {
            let config = SystemConfig {
                category_labels: labels,
                search: SearchConfig::default(),
                ..SystemConfig::default()
            };
            assert_eq!(validate_system_config(&config), Err(expected));
        }
    }

    #[test]
    fn category_label_config_errors_are_actionable() {
        assert_eq!(
            CategoryLabelConfigError::UnknownKey {
                key: "project".to_string(),
            }
            .to_string(),
            "unknown category label key: project"
        );
        assert_eq!(
            CategoryLabelConfigError::ConfiguredKeyCannotBeDeleted {
                key: "project".to_string(),
            }
            .to_string(),
            "label key project is configured as a category; remove it from System settings first"
        );
    }

    fn term(key: &str, value: Option<&str>) -> DuplicateCheckTerm {
        DuplicateCheckTerm {
            key: key.to_string(),
            value: value.map(str::to_string),
        }
    }

    #[test]
    fn validates_rule_shape() {
        let config = SystemConfig {
            category_labels: Vec::new(),
            duplicate_check: DuplicateCheckConfig {
                enabled: true,
                rules: vec![DuplicateCheckRule {
                    terms: vec![term("skill-name", None), term("version", None)],
                }],
            },
            search: SearchConfig::default(),
        };

        assert_eq!(validate_system_config(&config), Ok(()));
    }

    #[test]
    fn rejects_duplicate_keys_within_a_rule() {
        let config = SystemConfig {
            category_labels: Vec::new(),
            duplicate_check: DuplicateCheckConfig {
                enabled: true,
                rules: vec![DuplicateCheckRule {
                    terms: vec![term("version", None), term("version", Some("1.0.0"))],
                }],
            },
            search: SearchConfig::default(),
        };

        assert_eq!(
            validate_system_config(&config),
            Err(SystemConfigValidationError::DuplicateTermKey {
                rule: 0,
                key: "version".to_string(),
            })
        );
    }

    #[test]
    fn rejects_label_keys_with_outer_whitespace() {
        let config = SystemConfig {
            category_labels: Vec::new(),
            duplicate_check: DuplicateCheckConfig {
                enabled: true,
                rules: vec![DuplicateCheckRule {
                    terms: vec![term(" version ", None)],
                }],
            },
            search: SearchConfig::default(),
        };

        assert_eq!(
            validate_system_config(&config),
            Err(SystemConfigValidationError::TermKeyHasOuterWhitespace { rule: 0, term: 0 })
        );
    }

    #[test]
    fn resolves_bare_and_fixed_terms_against_input_labels() {
        let rule = DuplicateCheckRule {
            terms: vec![
                term("kind", Some("skill")),
                term("skill-name", None),
                term("version", None),
            ],
        };
        let labels = vec![
            ("version".to_string(), "1.0.0".to_string()),
            ("kind".to_string(), "skill".to_string()),
            ("skill-name".to_string(), "zddi-hooks".to_string()),
        ];

        assert_eq!(
            resolve_duplicate_rule(&rule, &labels),
            Some(vec![
                ("kind".to_string(), "skill".to_string()),
                ("skill-name".to_string(), "zddi-hooks".to_string()),
                ("version".to_string(), "1.0.0".to_string()),
            ])
        );
    }

    #[test]
    fn rule_does_not_apply_when_a_term_is_missing_or_fixed_value_differs() {
        let rule = DuplicateCheckRule {
            terms: vec![term("kind", Some("skill")), term("version", None)],
        };

        assert_eq!(
            resolve_duplicate_rule(
                &rule,
                &[
                    ("kind".to_string(), "note".to_string()),
                    ("version".to_string(), "1".to_string())
                ]
            ),
            None
        );
        assert_eq!(
            resolve_duplicate_rule(&rule, &[("kind".to_string(), "skill".to_string())]),
            None
        );
    }
}
