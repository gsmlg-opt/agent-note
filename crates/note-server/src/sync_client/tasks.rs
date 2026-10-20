//! Bounded session-local task identities and immutable terminal outcomes.
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};

pub const OPERATIONS: &[&str] = &[
    "inventory_start",
    "state_query",
    "transfer_begin",
    "attachment_begin",
    "blob_chunk",
    "blob_end",
    "transfer_commit",
    "inventory_complete",
    "transfer_abort",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub task_id: String,
    pub operation: String,
    pub payload: Value,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub task_id: String,
    pub status_seq: u64,
    pub state: String,
    pub phase: String,
    pub progress: Value,
    pub result: Option<Value>,
    pub error: Option<TaskError>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskError {
    pub code: String,
    pub retryable: bool,
}
impl Status {
    pub fn terminal(&self) -> bool {
        matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled")
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.status_seq > 0 && self.phase.len() <= 64 && self.progress.is_object(),
            "invalid task status"
        );
        ensure!(
            matches!(
                self.state.as_str(),
                "queued" | "running" | "succeeded" | "failed" | "cancelled"
            ),
            "invalid task state"
        );
        ensure!(
            self.result.as_ref().is_none_or(Value::is_object),
            "invalid task result"
        );
        ensure!(
            match self.state.as_str() {
                "succeeded" => self.result.is_some() && self.error.is_none(),
                "failed" => self.result.is_none() && self.error.is_some(),
                "cancelled" => self.result.is_none() && self.error.is_none(),
                _ => self.result.is_none(),
            },
            "invalid task outcome"
        );
        if let Some(error) = &self.error {
            ensure!(
                !error.code.is_empty()
                    && error.code.len() <= 64
                    && error
                        .code
                        .bytes()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_'),
                "invalid task error"
            );
        }
        Ok(())
    }
    pub fn accept_update(&self, next: &Self) -> Result<bool> {
        next.validate()?;
        ensure!(self.task_id == next.task_id, "task status identity changed");
        if next.status_seq < self.status_seq {
            return Ok(false);
        }
        if next.status_seq == self.status_seq {
            ensure!(self == next, "conflicting task status snapshot");
            return Ok(false);
        }
        ensure!(!self.terminal(), "terminal task outcome changed");
        ensure!(
            next.state != "queued" && !(self.state == "running" && next.state == "queued"),
            "task state regressed"
        );
        Ok(true)
    }
}
struct Entry {
    digest: [u8; 32],
    slot: &'static str,
    pub status: Status,
}
pub enum Admission {
    Start(Status),
    Replay(Status),
}
pub struct Ledger {
    origin: String,
    highwater: u64,
    entries: HashMap<String, Entry>,
    terminal: VecDeque<String>,
}
fn slot(operation: &str) -> &'static str {
    match operation {
        "inventory_start" => "inventory",
        "state_query" => "query",
        "inventory_complete" | "transfer_abort" => "housekeeping",
        _ => "transfer",
    }
}
pub fn sequence(id: &str, origin: &str) -> Option<u64> {
    let (node, seq) = id.rsplit_once(':')?;
    if node != origin {
        return None;
    }
    let value: u64 = seq.parse().ok()?;
    (value > 0 && value.to_string() == seq).then_some(value)
}
impl Ledger {
    pub fn new(origin: String) -> Self {
        Self {
            origin,
            highwater: 0,
            entries: HashMap::new(),
            terminal: VecDeque::new(),
        }
    }
    pub fn admit(&mut self, submit: &Submission) -> std::result::Result<Admission, &'static str> {
        let seq = sequence(&submit.task_id, &self.origin).ok_or("invalid_task_id")?;
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&(&submit.operation, &submit.payload)).unwrap())
                .into();
        if let Some(entry) = self.entries.get(&submit.task_id) {
            return if entry.digest == digest {
                Ok(Admission::Replay(entry.status.clone()))
            } else {
                Err("task_id_conflict")
            };
        }
        if seq <= self.highwater {
            return Err("retired");
        }
        if seq != self.highwater.checked_add(1).ok_or("invalid_task_id")? {
            return Err("invalid_task_id");
        }
        self.highwater = seq;
        let task_slot = slot(&submit.operation);
        let failure =
            if !OPERATIONS.contains(&submit.operation.as_str()) || !submit.payload.is_object() {
                Some("invalid_operation")
            } else if self
                .entries
                .values()
                .any(|entry| entry.slot == task_slot && !entry.status.terminal())
            {
                Some("busy")
            } else {
                None
            };
        let status = Status {
            task_id: submit.task_id.clone(),
            status_seq: 1,
            state: "queued".into(),
            phase: if failure == Some("invalid_operation") {
                "validation".into()
            } else {
                submit.operation.clone()
            },
            progress: json!({}),
            result: None,
            error: None,
        };
        self.entries.insert(
            submit.task_id.clone(),
            Entry {
                digest,
                slot: task_slot,
                status: status.clone(),
            },
        );
        if let Some(code) = failure {
            let mut failed = self.finish(&submit.task_id, Err(code));
            failed.status_seq = 1;
            self.entries.get_mut(&submit.task_id).unwrap().status = failed.clone();
            Ok(Admission::Replay(failed))
        } else {
            Ok(Admission::Start(status))
        }
    }
    pub fn active_transfer(&self) -> Option<&str> {
        self.entries
            .iter()
            .find(|(_, entry)| entry.slot == "transfer" && !entry.status.terminal())
            .map(|(id, _)| id.as_str())
    }
    pub fn get(&self, id: &str) -> std::result::Result<Status, &'static str> {
        self.entries
            .get(id)
            .map(|entry| entry.status.clone())
            .ok_or_else(|| {
                if sequence(id, &self.origin).is_some_and(|seq| seq <= self.highwater) {
                    "retired"
                } else {
                    "unknown_task"
                }
            })
    }
    pub fn update(&mut self, id: &str, phase: &str, progress: Value) -> Status {
        let entry = self.entries.get_mut(id).expect("admitted task");
        if !entry.status.terminal() {
            entry.status.status_seq += 1;
            entry.status.state = "running".into();
            entry.status.phase = phase.into();
            entry.status.progress = progress;
        }
        entry.status.clone()
    }
    pub fn unknown(&mut self, id: &str) -> Status {
        let current = self.entries.get(id).expect("admitted task");
        if current.status.terminal() {
            return current.status.clone();
        }
        let mut status = self.update(id, "outcome_unknown", json!({}));
        status.error = Some(TaskError {
            code: "outcome_unknown".into(),
            retryable: false,
        });
        self.entries.get_mut(id).unwrap().status = status.clone();
        status
    }
    pub fn finish(&mut self, id: &str, result: std::result::Result<Value, &str>) -> Status {
        let entry = self.entries.get_mut(id).expect("admitted task");
        if entry.status.terminal() {
            return entry.status.clone();
        }
        entry.status.status_seq += 1;
        match result {
            Ok(result) => {
                entry.status.state = "succeeded".into();
                entry.status.result = Some(result);
                entry.status.error = None;
            }
            Err(code) => {
                entry.status.state = if code == "cancelled" {
                    "cancelled"
                } else {
                    "failed"
                }
                .into();
                entry.status.result = None;
                entry.status.error = (code != "cancelled").then(|| TaskError {
                    code: code.into(),
                    retryable: false,
                });
            }
        }
        let status = entry.status.clone();
        self.terminal.push_back(id.into());
        while self.terminal.len() > 64 {
            self.entries.remove(&self.terminal.pop_front().unwrap());
        }
        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_and_retirement_never_execute_a_mutation_twice() {
        let origin = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::new(origin.clone());
        for seq in 1..=66 {
            let submit = Submission {
                task_id: format!("{origin}:{seq}"),
                operation: "blob_chunk".into(),
                payload: json!({"data":"YQ=="}),
            };
            assert!(matches!(
                ledger.admit(&submit).unwrap(),
                Admission::Start(_)
            ));
            assert!(matches!(
                ledger.admit(&submit).unwrap(),
                Admission::Replay(_)
            ));
            let terminal = ledger.finish(&submit.task_id, Ok(json!({"received_bytes":seq})));
            assert_eq!(ledger.finish(&submit.task_id, Err("cancelled")), terminal);
            let mut changed = submit.clone();
            changed.payload = json!({"data":"Yg=="});
            assert!(matches!(ledger.admit(&changed), Err("task_id_conflict")));
        }
        let old = Submission {
            task_id: format!("{origin}:1"),
            operation: "blob_chunk".into(),
            payload: json!({"data":"YQ=="}),
        };
        assert!(matches!(ledger.admit(&old), Err("retired")));
        assert_eq!(ledger.entries.len(), 64);
        assert_eq!(sequence(&format!("{origin}:067"), &origin), None);
    }
    #[test]
    fn uncertain_commit_resolves_to_one_valid_immutable_exact_receipt() {
        let origin = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::new(origin.clone());
        let submit = Submission {
            task_id: format!("{origin}:1"),
            operation: "transfer_commit".into(),
            payload: json!({}),
        };
        assert!(matches!(ledger.admit(&submit), Ok(Admission::Start(_))));
        ledger.unknown(&submit.task_id).validate().unwrap();
        let receipt = json!({"event_id":uuid::Uuid::new_v4().to_string()});
        let terminal = ledger.finish(&submit.task_id, Ok(receipt.clone()));
        terminal.validate().unwrap();
        assert_eq!(terminal.result, Some(receipt));
        assert!(terminal.error.is_none());
        assert_eq!(ledger.finish(&submit.task_id, Err("cancelled")), terminal);
        let Admission::Replay(replay) = ledger.admit(&submit).unwrap() else {
            panic!("replay expected")
        };
        assert_eq!(replay, terminal);
    }
    #[test]
    fn oversized_operation_is_rejected_with_a_valid_bounded_status() {
        let origin = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::new(origin.clone());
        let submit = Submission {
            task_id: format!("{origin}:1"),
            operation: "x".repeat(1024),
            payload: json!({}),
        };
        let Admission::Replay(status) = ledger.admit(&submit).unwrap() else {
            panic!("validation rejection expected")
        };
        status.validate().unwrap();
        assert_eq!(status.phase, "validation");
        assert_eq!(status.error.unwrap().code, "invalid_operation");
    }
    #[test]
    fn cancelled_wire_outcome_has_null_result_and_error_and_is_immutable() {
        let origin = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::new(origin.clone());
        let submission = Submission {
            task_id: format!("{origin}:1"),
            operation: "state_query".into(),
            payload: json!({}),
        };
        let Admission::Start(queued) = ledger.admit(&submission).unwrap() else {
            panic!("admission expected")
        };
        let cancelled = ledger.finish(&submission.task_id, Err("cancelled"));
        cancelled.validate().unwrap();
        assert_eq!(cancelled.state, "cancelled");
        let wire = serde_json::to_value(&cancelled).unwrap();
        assert_eq!(wire["result"], Value::Null);
        assert_eq!(wire["error"], Value::Null);
        assert!(queued.accept_update(&cancelled).unwrap());
        let decoded: Status = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, cancelled);
        assert_eq!(ledger.finish(&submission.task_id, Ok(json!({}))), cancelled);
        assert!(!cancelled.accept_update(&decoded).unwrap());
        assert_eq!(ledger.unknown(&submission.task_id), cancelled);
        let mut invalid = cancelled.clone();
        invalid.error = Some(TaskError {
            code: "cancelled".into(),
            retryable: false,
        });
        assert!(invalid.validate().is_err());
        assert!(queued.accept_update(&invalid).is_err());
    }
    #[test]
    fn bounded_slots_keep_active_work_while_busy_submission_is_terminal() {
        let origin = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::new(origin.clone());
        for (index, operation) in [
            "inventory_start",
            "state_query",
            "blob_chunk",
            "inventory_complete",
        ]
        .iter()
        .enumerate()
        {
            let submit = Submission {
                task_id: format!("{origin}:{}", index + 1),
                operation: (*operation).into(),
                payload: json!({}),
            };
            assert!(matches!(ledger.admit(&submit), Ok(Admission::Start(_))));
        }
        let submit = Submission {
            task_id: format!("{origin}:5"),
            operation: "blob_chunk".into(),
            payload: json!({}),
        };
        let Admission::Replay(status) = ledger.admit(&submit).unwrap() else {
            panic!("must reject busy transfer slot")
        };
        assert_eq!(status.error.unwrap().code, "busy");
        assert_eq!(
            ledger
                .entries
                .values()
                .filter(|entry| !entry.status.terminal())
                .count(),
            4
        );
    }
}
