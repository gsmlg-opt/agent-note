use yew::prelude::*;

use crate::org::{
    model::{OperationalItem, OrgTimestamp, ReadyStatus, ReviewLeaseStatus},
    time::{format_org_timestamp, TimeDisplay, UNAVAILABLE_TIME},
    url::{item_path, OrgReturnContext, WorkspaceQueryState},
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowSignals {
    lease: String,
    attempt: String,
    dependency_gate: String,
}

fn row_signals(row: &OperationalItem) -> RowSignals {
    let readiness = match row.ready_status {
        Some(ReadyStatus::Ready) => "server-ready",
        Some(ReadyStatus::RecoveryCandidate) => "recovery candidate",
        None => "not classified",
    };
    let review = match row.review_lease_status {
        Some(ReviewLeaseStatus::Unleased) => "review unleased",
        Some(ReviewLeaseStatus::Active) => "review leased",
        Some(ReviewLeaseStatus::Expired) => "review lease expired",
        None => "no review lease",
    };
    let lease = row.lease.as_ref().map_or_else(
        || format!("{readiness}; {review}"),
        |lease| {
            format!(
                "{} · {} · {} · expires {}",
                lease.kind, lease.actor_id, lease.status, lease.expires_at
            )
        },
    );
    let attempt = match &row.current_attempt_status {
        Some(status) => format!("{} · {} attempt(s)", status, row.attempt_count),
        None if row.attempt_count == 0 => "No attempts".to_owned(),
        None => format!("{} attempt(s)", row.attempt_count),
    };
    let dependency_gate = match row.ready_status {
        Some(ReadyStatus::Ready) => "Dependency gate · server-ready",
        Some(ReadyStatus::RecoveryCandidate) => "Dependency gate · recovery candidate",
        None => "Dependency gate · not included in this view",
    }
    .to_owned();
    RowSignals {
        lease,
        attempt,
        dependency_gate,
    }
}

fn operational_time(timestamp: Option<&OrgTimestamp>) -> TimeDisplay {
    timestamp
        .map(format_org_timestamp)
        .unwrap_or_else(|| TimeDisplay {
            workspace: UNAVAILABLE_TIME.to_owned(),
            browser_local: UNAVAILABLE_TIME.to_owned(),
        })
}

fn item_href(workspace_id: &str, item_id: &str, state: &WorkspaceQueryState) -> String {
    item_path(
        workspace_id,
        item_id,
        &OrgReturnContext::new(workspace_id, state.clone()),
    )
}

#[derive(Clone, PartialEq, Properties)]
pub struct OrgTableProps {
    pub workspace_id: String,
    pub rows: Vec<OperationalItem>,
    pub return_state: WorkspaceQueryState,
}

#[function_component(OrgTable)]
pub fn org_table(props: &OrgTableProps) -> Html {
    html! {
        <div class="org-ledger-scroll org-operations-scroll" tabindex="0">
            <table class="org-ledger org-operations-ledger">
                <caption>{ "Operational work items. Eligibility and lease status are server-evaluated." }</caption>
                <thead>
                    <tr>
                        <th scope="col">{ "Work item" }</th>
                        <th scope="col">{ "Type / state" }</th>
                        <th scope="col">{ "Priority" }</th>
                        <th scope="col">{ "Assignment" }</th>
                        <th scope="col">{ "Schedule" }</th>
                        <th scope="col">{ "Deadline" }</th>
                        <th scope="col">{ "Lease / recovery" }</th>
                        <th scope="col">{ "Attempts / eligibility" }</th>
                    </tr>
                </thead>
                <tbody>
                    { for props.rows.iter().map(|row| operational_row(
                        row,
                        &props.workspace_id,
                        &props.return_state,
                    )) }
                </tbody>
            </table>
        </div>
    }
}

fn operational_row(
    row: &OperationalItem,
    workspace_id: &str,
    return_state: &WorkspaceQueryState,
) -> Html {
    let item = &row.item;
    let scheduled = operational_time(item.scheduled.as_ref());
    let deadline = operational_time(item.deadline.as_ref());
    let signals = row_signals(row);
    let href = item_href(workspace_id, &item.id, return_state);
    let state = item.state.as_deref().unwrap_or("Unstated");
    let priority = item
        .priority
        .map_or_else(|| "None".to_owned(), |value| value.to_string());

    html! {
        <tr class={classes!(
            "org-operation-row",
            row.retry_exhausted.then_some("is-exhausted"),
            matches!(row.ready_status, Some(ReadyStatus::RecoveryCandidate))
                .then_some("is-recovery"),
        )}>
            <th scope="row">
                <a {href} class="org-item-link">
                    <strong>{ item.title.clone() }</strong>
                    <span>{ item.id.clone() }</span>
                </a>
            </th>
            <td>
                <span class="org-cell-primary">{ item.item_type.clone() }</span>
                <span class="org-cell-secondary">{ state }</span>
            </td>
            <td><span class="org-priority">{ priority }</span></td>
            <td>
                <span class="org-cell-primary">{ item.assignee.as_deref().unwrap_or("Unassigned") }</span>
                if item.requires_review {
                    <span class="org-cell-secondary">{ "Review required" }</span>
                }
            </td>
            { time_cell(&scheduled) }
            { time_cell(&deadline) }
            <td>
                <span class="org-cell-primary">{ signals.lease }</span>
                if matches!(row.ready_status, Some(ReadyStatus::RecoveryCandidate)) {
                    <span class="org-cell-secondary org-recovery-marker">{ "Recovery candidate" }</span>
                }
            </td>
            <td>
                <span class="org-cell-primary">{ signals.attempt }</span>
                <span class="org-cell-secondary">{ signals.dependency_gate }</span>
                if row.retry_exhausted {
                    <span class="org-cell-secondary">{ "Retry budget exhausted" }</span>
                }
            </td>
        </tr>
    }
}

fn time_cell(display: &TimeDisplay) -> Html {
    html! {
        <td class={classes!(
            "org-time-cell",
            (display.workspace == UNAVAILABLE_TIME).then_some("is-unavailable"),
        )}>
            <time>{ display.workspace.clone() }</time>
            if display.browser_local != UNAVAILABLE_TIME {
                <span>{ format!("Local · {}", display.browser_local) }</span>
            } else {
                <span>{ "Browser local · Unavailable" }</span>
            }
        </td>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::{
        model::{Lease, OperationalView, WorkItem},
        url::PriorityFilter,
    };

    fn row() -> OperationalItem {
        OperationalItem {
            item: WorkItem {
                id: "item/one".into(),
                workspace_id: "workspace-a".into(),
                document_id: "document-a".into(),
                parent_id: None,
                item_type: "task".into(),
                title: "Recover worker".into(),
                state: Some("RUNNING".into()),
                priority: Some('A'),
                scheduled: None,
                deadline: None,
                assignee: Some("agent-one".into()),
                requires_review: true,
                created_at: 1,
                tags: vec!["ops".into()],
            },
            attempt_count: 2,
            current_attempt_status: Some("failed".into()),
            retry_exhausted: false,
            ready_status: Some(ReadyStatus::RecoveryCandidate),
            review_lease_status: Some(ReviewLeaseStatus::Active),
            lease: None,
            completion_at: None,
        }
    }

    #[test]
    fn unavailable_operational_time_is_explicit() {
        let display = operational_time(None);
        assert_eq!(display.workspace, "Unavailable");
        assert_eq!(display.browser_local, "Unavailable");
    }

    #[test]
    fn recovery_and_review_lease_signals_are_visible_without_tokens() {
        let signals = row_signals(&row());
        assert!(signals.lease.contains("recovery candidate"));
        assert!(signals.lease.contains("review leased"));
        assert_eq!(signals.attempt, "failed · 2 attempt(s)");
        assert_eq!(
            signals.dependency_gate,
            "Dependency gate · recovery candidate"
        );
        assert!(!format!("{signals:?}").contains("token"));
    }

    #[test]
    fn safe_active_lease_summary_contains_only_operational_fields() {
        let mut row = row();
        row.lease = Some(Lease {
            id: "lease-1".into(),
            workspace_id: "workspace-a".into(),
            work_item_id: "item-1".into(),
            attempt_id: "attempt-1".into(),
            kind: "execution".into(),
            actor_id: "agent-one".into(),
            acquired_at: 1,
            last_heartbeat_at: 2,
            expires_at: 3,
            status: "active".into(),
        });
        assert_eq!(
            row_signals(&row).lease,
            "execution · agent-one · active · expires 3"
        );
    }

    #[test]
    fn item_query_carries_the_complete_exact_return_context() {
        let state = WorkspaceQueryState {
            view: OperationalView::Failed,
            item_type: Some("incident".into()),
            state: Some("FAILED/EXTERNAL".into()),
            priority: Some(PriorityFilter::None),
            tags: vec!["build ready".into(), "ops/on-call".into()],
            assignee: Some("agent+one".into()),
            from: Some("2026-08-05T01:02:03Z".into()),
            to: Some("2026-08-06T01:02:03Z".into()),
            cursor: Some("page/+= two".into()),
            limit: 25,
        };
        let query = item_href("workspace-a", "item/one", &state);
        for expected in [
            "return_view=failed",
            "return_item_type=incident",
            "return_state=FAILED%2FEXTERNAL",
            "return_priority=none",
            "return_tags=build%20ready%2Cops%2Fon-call",
            "return_assignee=agent%2Bone",
            "return_from=2026-08-05T01%3A02%3A03Z",
            "return_to=2026-08-06T01%3A02%3A03Z",
            "return_cursor=page%2F%2B%3D%20two",
            "return_limit=25",
        ] {
            assert!(query.contains(expected), "missing {expected} in {query}");
        }
        assert!(query.starts_with("/org/workspace-a/items/item%2Fone?"));
    }
}
