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
    match (
        comparable_value(value_type, left),
        comparable_value(value_type, right),
    ) {
        (Some(left), Some(right)) => compare_ordering(left.cmp(&right), operator),
        _ => false,
    }
}

fn split_selector_term(term: &str) -> Option<(&str, LabelOperator, &str)> {
    for (token, operator) in [
        (">=", LabelOperator::Gte),
        ("<=", LabelOperator::Lte),
        ("!=", LabelOperator::NotEq),
        ("=", LabelOperator::Eq),
        (">", LabelOperator::Gt),
        ("<", LabelOperator::Lt),
    ] {
        if let Some(idx) = term.find(token) {
            let key = &term[..idx];
            let value = &term[idx + token.len()..];
            if key.trim().is_empty() {
                return None;
            }
            return Some((key, operator, value));
        }
    }
    None
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
    }
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
pub struct NoteListItem {
    pub id: NoteId,
    pub title: String,
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
}
