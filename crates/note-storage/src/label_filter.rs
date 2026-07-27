use note_core::{label_matches_selector, Label, LabelSelector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLabelSelector {
    pub key: String,
    /// `None` means bare-key presence. `Some` is always nonempty.
    pub values: Option<Vec<String>>,
}

pub fn resolve_label_selectors(
    selectors: &[LabelSelector],
    distinct_labels: &[Label],
) -> Option<Vec<ResolvedLabelSelector>> {
    let mut resolved = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let values = if selector.value.is_none() {
            if !distinct_labels
                .iter()
                .any(|label| label.key == selector.key)
            {
                return None;
            }
            None
        } else {
            let mut values = distinct_labels
                .iter()
                .filter(|label| label_matches_selector(label, selector))
                .map(|label| label.value.clone())
                .collect::<Vec<_>>();
            values.sort();
            values.dedup();
            if values.is_empty() {
                return None;
            }
            Some(values)
        };
        resolved.push(ResolvedLabelSelector {
            key: selector.key.clone(),
            values,
        });
    }
    Some(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use note_core::{parse_label_selectors, LabelValueType};

    fn label(key: &str, value: &str, value_type: LabelValueType) -> Label {
        Label {
            key: key.into(),
            value: value.into(),
            description: String::new(),
            value_type,
        }
    }

    #[test]
    fn resolves_typed_regex_and_presence_selectors_to_raw_values() {
        let selectors = parse_label_selectors("status~=^RE.*Y$&version>=1.10.0&archived");
        let labels = vec![
            label("status", "ready", LabelValueType::Text),
            label("status", "blocked", LabelValueType::Text),
            label("version", "1.2.0", LabelValueType::Version),
            label("version", "1.10.0", LabelValueType::Version),
            label("archived", "yes", LabelValueType::Text),
        ];

        assert_eq!(
            resolve_label_selectors(&selectors, &labels),
            Some(vec![
                ResolvedLabelSelector {
                    key: "status".into(),
                    values: Some(vec!["ready".into()]),
                },
                ResolvedLabelSelector {
                    key: "version".into(),
                    values: Some(vec!["1.10.0".into()]),
                },
                ResolvedLabelSelector {
                    key: "archived".into(),
                    values: None,
                },
            ])
        );
    }

    #[test]
    fn returns_none_when_any_value_selector_has_no_match() {
        let labels = vec![label("status", "ready", LabelValueType::Text)];

        assert_eq!(
            resolve_label_selectors(&parse_label_selectors("status~=["), &labels),
            None
        );
        assert_eq!(
            resolve_label_selectors(&parse_label_selectors("missing=value"), &labels),
            None
        );
        assert_eq!(
            resolve_label_selectors(&parse_label_selectors("missing"), &labels),
            None
        );
    }

    #[test]
    fn preserves_separate_same_key_selectors_and_deduplicates_values() {
        let labels = vec![
            label("version", "1.10.0", LabelValueType::Version),
            label("version", "1.10.0", LabelValueType::Version),
            label("version", "2.0.0", LabelValueType::Version),
        ];
        let resolved = resolve_label_selectors(
            &parse_label_selectors("version>=1.10.0&version<2.0.0"),
            &labels,
        )
        .unwrap();

        assert_eq!(resolved.len(), 2);
        assert_eq!(
            resolved[0].values,
            Some(vec!["1.10.0".into(), "2.0.0".into()])
        );
        assert_eq!(resolved[1].values, Some(vec!["1.10.0".into()]));
    }
}
