use base64::{engine::general_purpose::STANDARD, Engine as _};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

pub type NoteId = String;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LabelValueType {
    #[default]
    Text,
    Number,
    Version,
    Date,
    DateTime,
    Time,
}

impl LabelValueType {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelValueType::Text => "text",
            LabelValueType::Number => "number",
            LabelValueType::Version => "version",
            LabelValueType::Date => "date",
            LabelValueType::DateTime => "datetime",
            LabelValueType::Time => "time",
        }
    }
}

impl std::fmt::Display for LabelValueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for LabelValueType {
    type Err = String;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "" | "text" => Ok(LabelValueType::Text),
            "number" => Ok(LabelValueType::Number),
            "version" => Ok(LabelValueType::Version),
            "date" => Ok(LabelValueType::Date),
            "datetime" => Ok(LabelValueType::DateTime),
            "time" => Ok(LabelValueType::Time),
            other => Err(format!("unsupported label value type: {other}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelKey {
    pub key: String,
    pub description: String,
    pub value_type: LabelValueType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Label {
    pub key: String,
    pub value: String,
    pub description: String,
    pub value_type: LabelValueType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelOperator {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    StartsWith,
    EndsWith,
    Regex,
}

impl LabelOperator {
    pub fn as_str(self) -> &'static str {
        match self {
            LabelOperator::Eq => "=",
            LabelOperator::NotEq => "!=",
            LabelOperator::Gt => ">",
            LabelOperator::Gte => ">=",
            LabelOperator::Lt => "<",
            LabelOperator::Lte => "<=",
            LabelOperator::StartsWith => "^=",
            LabelOperator::EndsWith => "$=",
            LabelOperator::Regex => "~=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelSelector {
    pub key: String,
    pub value: Option<String>,
    pub operator: LabelOperator,
}

pub fn parse_label_selectors(input: &str) -> Vec<LabelSelector> {
    input
        .split('&')
        .filter_map(|term| {
            let term = term.trim();
            if term.is_empty() {
                return None;
            }

            match split_selector_term(term) {
                Some((key, operator, value)) => Some(LabelSelector {
                    key: key.trim().to_string(),
                    value: Some(value.trim().to_string()),
                    operator,
                }),
                None => Some(LabelSelector {
                    key: term.to_string(),
                    value: None,
                    operator: LabelOperator::Eq,
                }),
            }
        })
        .collect()
}

pub fn label_matches_selector(label: &Label, selector: &LabelSelector) -> bool {
    if label.key != selector.key {
        return false;
    }
    match &selector.value {
        Some(value) => {
            compare_label_values(label.value_type, &label.value, selector.operator, value)
        }
        None => true,
    }
}

pub fn validate_label_value(value_type: LabelValueType, value: &str) -> bool {
    comparable_value(value_type, value).is_some()
}

pub fn compare_label_values(
    value_type: LabelValueType,
    left: &str,
    operator: LabelOperator,
    right: &str,
) -> bool {
    match operator {
        LabelOperator::StartsWith => {
            return left.to_lowercase().starts_with(&right.to_lowercase());
        }
        LabelOperator::EndsWith => {
            return left.to_lowercase().ends_with(&right.to_lowercase());
        }
        LabelOperator::Regex => {
            return RegexBuilder::new(right)
                .case_insensitive(true)
                .build()
                .is_ok_and(|regex| regex.is_match(left));
        }
        _ => {}
    }

    match (
        comparable_value(value_type, left),
        comparable_value(value_type, right),
    ) {
        (Some(left), Some(right)) => compare_ordering(left.cmp(&right), operator),
        _ => false,
    }
}

fn split_selector_term(term: &str) -> Option<(&str, LabelOperator, &str)> {
    let (idx, token, operator) = [
        (">=", LabelOperator::Gte),
        ("<=", LabelOperator::Lte),
        ("!=", LabelOperator::NotEq),
        ("^=", LabelOperator::StartsWith),
        ("$=", LabelOperator::EndsWith),
        ("~=", LabelOperator::Regex),
        ("=", LabelOperator::Eq),
        (">", LabelOperator::Gt),
        ("<", LabelOperator::Lt),
    ]
    .into_iter()
    .filter_map(|(token, operator)| term.find(token).map(|idx| (idx, token, operator)))
    .min_by_key(|(idx, token, _)| (*idx, std::cmp::Reverse(token.len())))?;

    let key = &term[..idx];
    let value = &term[idx + token.len()..];
    if key.trim().is_empty() {
        return None;
    }
    Some((key, operator, value))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ComparableValue {
    Text(String),
    Number(OrderedF64),
    Version(Vec<u64>),
    Date(chrono::NaiveDate),
    DateTime(chrono::NaiveDateTime),
    Time(chrono::NaiveTime),
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct OrderedF64(f64);

impl Eq for OrderedF64 {}

impl PartialOrd for OrderedF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrderedF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

fn comparable_value(value_type: LabelValueType, value: &str) -> Option<ComparableValue> {
    let value = value.trim();
    match value_type {
        LabelValueType::Text => Some(ComparableValue::Text(value.to_string())),
        LabelValueType::Number => value.parse::<f64>().ok().and_then(|n| {
            if n.is_finite() {
                Some(ComparableValue::Number(OrderedF64(n)))
            } else {
                None
            }
        }),
        LabelValueType::Version => parse_version(value).map(ComparableValue::Version),
        LabelValueType::Date => chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .ok()
            .map(ComparableValue::Date),
        LabelValueType::DateTime => parse_datetime(value).map(ComparableValue::DateTime),
        LabelValueType::Time => parse_time(value).map(ComparableValue::Time),
    }
}

fn parse_version(value: &str) -> Option<Vec<u64>> {
    let value = value.trim().trim_start_matches('v');
    if value.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in value.split('.') {
        if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    while parts.last() == Some(&0) {
        parts.pop();
    }
    Some(parts)
}

fn parse_datetime(value: &str) -> Option<chrono::NaiveDateTime> {
    [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| chrono::NaiveDateTime::parse_from_str(value, format).ok())
}

fn parse_time(value: &str) -> Option<chrono::NaiveTime> {
    ["%H:%M:%S", "%H:%M"]
        .iter()
        .find_map(|format| chrono::NaiveTime::parse_from_str(value, format).ok())
}

fn compare_ordering(ordering: std::cmp::Ordering, operator: LabelOperator) -> bool {
    match operator {
        LabelOperator::Eq => ordering.is_eq(),
        LabelOperator::NotEq => !ordering.is_eq(),
        LabelOperator::Gt => ordering.is_gt(),
        LabelOperator::Gte => ordering.is_ge(),
        LabelOperator::Lt => ordering.is_lt(),
        LabelOperator::Lte => ordering.is_le(),
        LabelOperator::StartsWith | LabelOperator::EndsWith | LabelOperator::Regex => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteAttachment {
    pub id: String,
    pub path: String,
    pub mime: String,
    #[serde(default)]
    pub description: String,
    pub content: Vec<u8>,
}

#[derive(Debug)]
pub enum AttachmentContentError {
    MissingContent,
    InvalidBase64(base64::DecodeError),
    ContentMismatch,
}

impl std::fmt::Display for AttachmentContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingContent => {
                write!(f, "attachment content or content_base64 is required")
            }
            Self::InvalidBase64(error) => write!(f, "invalid attachment content_base64: {error}"),
            Self::ContentMismatch => {
                write!(f, "attachment content and content_base64 do not match")
            }
        }
    }
}

impl std::error::Error for AttachmentContentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBase64(error) => Some(error),
            _ => None,
        }
    }
}

pub fn decode_attachment_content(
    content: Option<&str>,
    content_base64: Option<&str>,
) -> Result<Vec<u8>, AttachmentContentError> {
    match (content, content_base64) {
        (None, None) => Err(AttachmentContentError::MissingContent),
        (Some(content), None) => Ok(content.as_bytes().to_vec()),
        (None, Some(content_base64)) => STANDARD
            .decode(content_base64)
            .map_err(AttachmentContentError::InvalidBase64),
        (Some(content), Some(content_base64)) => {
            let decoded = STANDARD
                .decode(content_base64)
                .map_err(AttachmentContentError::InvalidBase64)?;
            if decoded == content.as_bytes() {
                Ok(decoded)
            } else {
                Err(AttachmentContentError::ContentMismatch)
            }
        }
    }
}

pub fn encode_attachment_content(content: &[u8]) -> String {
    STANDARD.encode(content)
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub id: NoteId,
    pub title: String,
    pub content: String,
    pub attachments: Vec<NoteAttachment>,
    pub labels: Vec<Label>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NoteListItem {
    pub id: NoteId,
    pub title: String,
    pub labels: Vec<Label>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub note: NoteListItem,
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
                operator: LabelOperator::Eq,
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
                operator: LabelOperator::Eq,
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
                    operator: LabelOperator::Eq,
                },
                LabelSelector {
                    key: "team".to_string(),
                    value: Some("core".to_string()),
                    operator: LabelOperator::Eq,
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
                    operator: LabelOperator::Eq,
                },
                LabelSelector {
                    key: "team".to_string(),
                    value: Some("core".to_string()),
                    operator: LabelOperator::Eq,
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
                    operator: LabelOperator::Eq,
                },
                LabelSelector {
                    key: "archived".to_string(),
                    value: None,
                    operator: LabelOperator::Eq,
                },
            ]
        );
    }

    #[test]
    fn parses_comparison_selectors() {
        assert_eq!(
            parse_label_selectors("version>=1.2.0&priority<10&status!=done"),
            vec![
                LabelSelector {
                    key: "version".to_string(),
                    value: Some("1.2.0".to_string()),
                    operator: LabelOperator::Gte,
                },
                LabelSelector {
                    key: "priority".to_string(),
                    value: Some("10".to_string()),
                    operator: LabelOperator::Lt,
                },
                LabelSelector {
                    key: "status".to_string(),
                    value: Some("done".to_string()),
                    operator: LabelOperator::NotEq,
                },
            ]
        );
    }

    #[test]
    fn parses_string_match_selectors() {
        assert_eq!(
            parse_label_selectors("name^=Agent&name$=NOTE&name~=^agent-.+$"),
            vec![
                LabelSelector {
                    key: "name".to_string(),
                    value: Some("Agent".to_string()),
                    operator: LabelOperator::StartsWith,
                },
                LabelSelector {
                    key: "name".to_string(),
                    value: Some("NOTE".to_string()),
                    operator: LabelOperator::EndsWith,
                },
                LabelSelector {
                    key: "name".to_string(),
                    value: Some("^agent-.+$".to_string()),
                    operator: LabelOperator::Regex,
                },
            ]
        );
        assert_eq!(LabelOperator::StartsWith.as_str(), "^=");
        assert_eq!(LabelOperator::EndsWith.as_str(), "$=");
        assert_eq!(LabelOperator::Regex.as_str(), "~=");
    }

    #[test]
    fn parses_first_operator_when_values_contain_operator_tokens() {
        for (input, operator, value) in [
            ("name~=^a!=b$", LabelOperator::Regex, "^a!=b$"),
            ("name^=a>=b", LabelOperator::StartsWith, "a>=b"),
            ("name$=a<=b", LabelOperator::EndsWith, "a<=b"),
            ("name~=a^=b$=c", LabelOperator::Regex, "a^=b$=c"),
        ] {
            assert_eq!(
                parse_label_selectors(input),
                vec![LabelSelector {
                    key: "name".to_string(),
                    value: Some(value.to_string()),
                    operator,
                }]
            );
        }
    }

    #[test]
    fn string_match_operators_use_case_insensitive_raw_stored_values() {
        let text_label = Label {
            key: "name".to_string(),
            value: "Agent-Note".to_string(),
            description: String::new(),
            value_type: LabelValueType::Text,
        };
        for (operator, value) in [
            (LabelOperator::StartsWith, "agent"),
            (LabelOperator::EndsWith, "NOTE"),
            (LabelOperator::Regex, "^agent-.+"),
        ] {
            assert!(label_matches_selector(
                &text_label,
                &LabelSelector {
                    key: "name".to_string(),
                    value: Some(value.to_string()),
                    operator,
                }
            ));
        }
        for (operator, value) in [
            (LabelOperator::StartsWith, "gent"),
            (LabelOperator::EndsWith, "not"),
        ] {
            assert!(!label_matches_selector(
                &text_label,
                &LabelSelector {
                    key: "name".to_string(),
                    value: Some(value.to_string()),
                    operator,
                }
            ));
        }

        let number_label = Label {
            key: "priority".to_string(),
            value: "10".to_string(),
            description: String::new(),
            value_type: LabelValueType::Number,
        };
        assert!(label_matches_selector(
            &number_label,
            &LabelSelector {
                key: "priority".to_string(),
                value: Some("1".to_string()),
                operator: LabelOperator::StartsWith,
            }
        ));

        for (value_type, left, operator, right) in [
            (
                LabelValueType::Version,
                "v1.2.3",
                LabelOperator::Regex,
                r"^V1\.",
            ),
            (
                LabelValueType::Date,
                "2026-07-21",
                LabelOperator::StartsWith,
                "2026-",
            ),
            (
                LabelValueType::DateTime,
                "2026-07-21T12:30",
                LabelOperator::Regex,
                r"T12:\d+$",
            ),
            (
                LabelValueType::Time,
                "09:30",
                LabelOperator::EndsWith,
                ":30",
            ),
        ] {
            assert!(compare_label_values(value_type, left, operator, right));
        }

        assert!(compare_label_values(
            LabelValueType::Text,
            "ÄGENT-NÖTE",
            LabelOperator::StartsWith,
            "äge"
        ));
        assert!(compare_label_values(
            LabelValueType::Text,
            "ÄGENT-NÖTE",
            LabelOperator::EndsWith,
            "nöte"
        ));
    }

    #[test]
    fn invalid_label_regex_does_not_match() {
        let label = Label {
            key: "name".to_string(),
            value: "Agent-Note".to_string(),
            description: String::new(),
            value_type: LabelValueType::Text,
        };
        assert!(!label_matches_selector(
            &label,
            &LabelSelector {
                key: "name".to_string(),
                value: Some("[".to_string()),
                operator: LabelOperator::Regex,
            }
        ));
    }

    #[test]
    fn compares_typed_values() {
        assert!(compare_label_values(
            LabelValueType::Number,
            "10",
            LabelOperator::Gt,
            "2"
        ));
        assert!(compare_label_values(
            LabelValueType::Version,
            "1.10.0",
            LabelOperator::Gt,
            "1.2.9"
        ));
        assert!(compare_label_values(
            LabelValueType::Date,
            "2026-07-09",
            LabelOperator::Gte,
            "2026-01-01"
        ));
        assert!(compare_label_values(
            LabelValueType::DateTime,
            "2026-07-09T12:30",
            LabelOperator::Lt,
            "2026-07-09T13:00"
        ));
        assert!(compare_label_values(
            LabelValueType::Time,
            "09:30",
            LabelOperator::Lte,
            "10:00"
        ));
    }

    #[test]
    fn rejects_invalid_typed_values() {
        assert!(!validate_label_value(
            LabelValueType::Number,
            "not-a-number"
        ));
        assert!(!validate_label_value(LabelValueType::Date, "07/09/2026"));
        assert!(!validate_label_value(LabelValueType::Version, "1.2.beta"));
    }

    #[test]
    fn attachment_content_accepts_legacy_text_and_strict_base64() {
        assert_eq!(
            decode_attachment_content(Some("metadata"), None).unwrap(),
            b"metadata"
        );
        assert_eq!(
            decode_attachment_content(None, Some("AP8=")).unwrap(),
            vec![0, 255]
        );
        assert!(matches!(
            decode_attachment_content(None, Some("AP8")),
            Err(AttachmentContentError::InvalidBase64(_))
        ));
    }

    #[test]
    fn attachment_content_requires_matching_dual_inputs() {
        assert_eq!(
            decode_attachment_content(Some("metadata"), Some("bWV0YWRhdGE=")).unwrap(),
            b"metadata"
        );
        assert!(matches!(
            decode_attachment_content(Some("metadata"), Some("b3RoZXI=")),
            Err(AttachmentContentError::ContentMismatch)
        ));
        assert!(matches!(
            decode_attachment_content(None, None),
            Err(AttachmentContentError::MissingContent)
        ));
    }

    #[test]
    fn attachment_content_encoding_is_canonical_base64() {
        assert_eq!(encode_attachment_content(&[0, 255]), "AP8=");
    }
}
