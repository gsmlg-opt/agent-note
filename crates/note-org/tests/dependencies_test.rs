mod support;

use note_org::{validate_dependencies, DependencyError};
use std::collections::BTreeSet;
use support::{id, item, ready_item};

const ONE: &str = "11111111-1111-4111-8111-111111111111";
const TWO: &str = "22222222-2222-4222-8222-222222222222";
const THREE: &str = "33333333-3333-4333-8333-333333333333";
const FOUR: &str = "44444444-4444-4444-8444-444444444444";
const FIVE: &str = "55555555-5555-4555-8555-555555555555";

fn strings(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn valid_edges_are_satisfied_only_when_every_dependency_succeeded() {
    let first = item(ONE, "READY", [TWO, THREE]);
    let second = item(TWO, "ARCHIVED", []);
    let third = item(THREE, "READY", []);
    let successful = strings(&["ARCHIVED"]);

    let graph = validate_dependencies(&[first, second, third], &successful).unwrap();

    assert!(!graph.dependencies_satisfied(id(ONE)));
    assert!(graph.dependencies_satisfied(id(TWO)));
    assert!(graph.dependencies_satisfied(id(THREE)));
}

#[test]
fn item_without_dependency_edges_is_satisfied() {
    let graph = validate_dependencies(&[ready_item()], &BTreeSet::new()).unwrap();

    assert!(graph.dependencies_satisfied(id(ONE)));
}

#[test]
fn unknown_item_is_not_satisfied() {
    let graph = validate_dependencies(&[ready_item()], &BTreeSet::new()).unwrap();

    assert!(!graph.dependencies_satisfied(id(TWO)));
}

#[test]
fn conflicting_duplicate_ids_fail_identically_in_both_input_orders() {
    let first = item(ONE, "READY", []);
    let mut conflicting = item(ONE, "FAILED", [TWO]);
    conflicting.title = "Conflicting duplicate".to_string();
    let expected = DependencyError::DuplicateItem { item_id: id(ONE) };

    for items in [
        vec![first.clone(), conflicting.clone()],
        vec![conflicting, first],
    ] {
        assert_eq!(
            validate_dependencies(&items, &BTreeSet::new()),
            Err(expected.clone())
        );
    }
    assert_eq!(
        expected.to_string(),
        format!("duplicate work item id {ONE}")
    );
}

#[test]
fn identical_duplicate_ids_are_rejected() {
    let duplicate = item(ONE, "READY", []);

    assert_eq!(
        validate_dependencies(&[duplicate.clone(), duplicate], &BTreeSet::new()),
        Err(DependencyError::DuplicateItem { item_id: id(ONE) })
    );
}

#[test]
fn smallest_duplicate_id_is_reported_regardless_of_input_order() {
    let first_a = item(ONE, "READY", []);
    let second_a = item(ONE, "FAILED", []);
    let first_b = item(TWO, "READY", []);
    let second_b = item(TWO, "FAILED", []);

    let orders = [
        vec![
            first_b.clone(),
            second_b.clone(),
            first_a.clone(),
            second_a.clone(),
        ],
        vec![
            second_a.clone(),
            first_b.clone(),
            first_a.clone(),
            second_b.clone(),
        ],
        vec![second_b, first_a, first_b, second_a],
    ];

    for items in orders {
        assert_eq!(
            validate_dependencies(&items, &BTreeSet::new()),
            Err(DependencyError::DuplicateItem { item_id: id(ONE) })
        );
    }
}

#[test]
fn self_dependency_is_rejected_before_other_invalid_edges() {
    let subject = item(ONE, "READY", [ONE, TWO]);

    assert_eq!(
        validate_dependencies(&[subject], &BTreeSet::new()),
        Err(DependencyError::SelfDependency { item_id: id(ONE) })
    );
}

#[test]
fn missing_dependency_target_reports_source_and_target() {
    let subject = item(ONE, "READY", [TWO]);

    assert_eq!(
        validate_dependencies(&[subject], &BTreeSet::new()),
        Err(DependencyError::MissingTarget {
            item_id: id(ONE),
            dependency_id: id(TWO),
        })
    );
}

#[test]
fn two_node_cycle_has_canonical_path() {
    let first = item(ONE, "READY", [TWO]);
    let second = item(TWO, "READY", [ONE]);

    assert_eq!(
        validate_dependencies(&[second, first], &BTreeSet::new()),
        Err(DependencyError::Cycle {
            path: vec![id(ONE), id(TWO), id(ONE)],
        })
    );
}

#[test]
fn longer_cycle_is_rotated_to_its_smallest_id_in_edge_order() {
    let tail = item(ONE, "READY", [THREE]);
    let second = item(TWO, "READY", [THREE]);
    let third = item(THREE, "READY", [FOUR]);
    let fourth = item(FOUR, "READY", [TWO]);
    let unrelated = item(FIVE, "READY", []);

    assert_eq!(
        validate_dependencies(&[unrelated, fourth, third, second, tail], &BTreeSet::new()),
        Err(DependencyError::Cycle {
            path: vec![id(TWO), id(THREE), id(FOUR), id(TWO)],
        })
    );
}

#[test]
fn large_acyclic_chain_validates_without_using_the_call_stack() {
    const ITEM_COUNT: usize = 20_000;
    let ids: Vec<_> = (0..ITEM_COUNT)
        .map(|index| format!("00000000-0000-4000-8000-{index:012x}"))
        .collect();
    let items: Vec<_> = (0..ITEM_COUNT)
        .map(|index| {
            if index + 1 == ITEM_COUNT {
                item(ids[index].as_str(), "ARCHIVED", [])
            } else {
                item(
                    ids[index].as_str(),
                    if index == 0 { "READY" } else { "ARCHIVED" },
                    [ids[index + 1].as_str()],
                )
            }
        })
        .collect();

    let graph = validate_dependencies(&items, &strings(&["ARCHIVED"])).unwrap();

    assert!(graph.dependencies_satisfied(id(&ids[0])));
    assert!(graph.dependencies_satisfied(id(&ids[ITEM_COUNT - 1])));
}

#[test]
fn hierarchy_does_not_create_dependency_edges() {
    let mut child = item(ONE, "READY", []);
    child.parent_id = Some(id(TWO));

    let graph = validate_dependencies(&[child], &BTreeSet::new()).unwrap();

    assert!(graph.dependencies_satisfied(id(ONE)));
}

#[test]
fn only_configured_successful_states_satisfy_dependencies() {
    for dependency_state in ["DONE", "FAILED", "CANCELLED", "REVIEW", "ARCHIVED"] {
        let dependent = item(ONE, "READY", [TWO]);
        let dependency = item(TWO, dependency_state, []);

        let graph = validate_dependencies(
            &[dependent.clone(), dependency.clone()],
            &strings(&["ARCHIVED"]),
        )
        .unwrap();
        assert_eq!(
            graph.dependencies_satisfied(id(ONE)),
            dependency_state == "ARCHIVED",
            "state {dependency_state} must be evaluated from configuration"
        );

        let graph =
            validate_dependencies(&[dependent, dependency], &strings(&[dependency_state])).unwrap();
        assert!(graph.dependencies_satisfied(id(ONE)));
    }
}
