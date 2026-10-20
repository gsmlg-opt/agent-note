//! Socket-only workers: bounded queues and a heartbeat watchdog independent of business I/O.
use super::{
    phoenix::{Frame, FRAME_BYTES},
    runtime::Socket,
};
use futures::{SinkExt, StreamExt};
use std::{collections::VecDeque, io, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, watch, Mutex},
    task::JoinHandle,
    time::Instant,
};
use tokio_tungstenite::tungstenite::{Error, Message};

type WireResult = Result<Message, Error>;
const HEARTBEAT: Duration = Duration::from_secs(30);
const HEARTBEAT_WAIT: Duration = Duration::from_secs(10);

pub enum Io {
    #[cfg(test)]
    Raw(Socket),
    #[cfg(test)]
    Empty,
    Pump(Pump),
}
pub struct Pump {
    outgoing: mpsc::Sender<Message>,
    incoming: mpsc::Receiver<WireResult>,
    failure: watch::Receiver<Option<&'static str>>,
    workers: Vec<JoinHandle<()>>,
}
impl Drop for Pump {
    fn drop(&mut self) {
        for worker in &self.workers {
            worker.abort();
        }
    }
}
fn failed(message: &str) -> Error {
    Error::Io(io::Error::other(message.to_owned()))
}
// Four active task slots plus 64 cached terminal snapshots and control commands.
// At most 80 * FRAME_BYTES bytes; excess is an unsafe peer flood, never a timeout.
const BUSINESS_BACKLOG: usize = 80;
fn buffer_business(queued: &mut VecDeque<Message>, message: Message) -> Result<(), ()> {
    let frame = match &message {
        Message::Text(text) if text.len() <= FRAME_BYTES => Frame::decode(text).ok(),
        Message::Text(_) | Message::Binary(_) => return Err(()),
        _ => None,
    };
    if let Some(frame) = &frame {
        let coalescible = matches!(
            frame.event.as_str(),
            "task_status" | "sync_manager_status" | "task_get"
        );
        if coalescible {
            let key = if frame.event == "sync_manager_status" {
                Some("")
            } else {
                frame.payload["task_id"].as_str()
            };
            if let Some(index) = queued.iter().position(|old| {
                let Message::Text(text) = old else {
                    return false;
                };
                let Ok(old) = Frame::decode(text) else {
                    return false;
                };
                old.topic == frame.topic
                    && old.join_ref == frame.join_ref
                    && old.event == frame.event
                    && key.is_some()
                    && (frame.event == "sync_manager_status"
                        || old.payload["task_id"].as_str() == key)
            }) {
                let Message::Text(text) = &queued[index] else {
                    unreachable!()
                };
                let old = Frame::decode(text).map_err(|_| ())?;
                if frame.event == "task_status" {
                    let old: super::tasks::Status =
                        serde_json::from_value(old.payload).map_err(|_| ())?;
                    let next: super::tasks::Status =
                        serde_json::from_value(frame.payload.clone()).map_err(|_| ())?;
                    if !old.accept_update(&next).map_err(|_| ())? {
                        return Ok(());
                    }
                } else if frame.event == "sync_manager_status" {
                    let previous = old.payload["status_seq"]
                        .as_u64()
                        .filter(|seq| *seq > 0)
                        .ok_or(())?;
                    let next = frame.payload["status_seq"]
                        .as_u64()
                        .filter(|seq| *seq > 0)
                        .ok_or(())?;
                    if next < previous {
                        return Ok(());
                    }
                    if next == previous {
                        return if frame.payload == old.payload {
                            Ok(())
                        } else {
                            Err(())
                        };
                    }
                }
                queued[index] = message;
                return Ok(());
            }
        }
    }
    if queued.len() == BUSINESS_BACKLOG {
        return Err(());
    }
    queued.push_back(message);
    Ok(())
}
impl Io {
    #[cfg(test)]
    pub fn raw(socket: Socket) -> Self {
        Self::Raw(socket)
    }
    #[cfg(test)]
    pub fn start_pump(&mut self) {
        let Self::Raw(socket) = std::mem::replace(self, Self::Empty) else {
            panic!("raw socket required")
        };
        *self = Self::start(socket);
    }
    pub fn pumped(&self) -> bool {
        matches!(self, Self::Pump(_))
    }
    pub fn start(socket: Socket) -> Self {
        let (mut writer, mut reader) = socket.split();
        // Four bounded frames, at most 512KiB, plus two control frames.
        let (outgoing, mut outbound) = mpsc::channel::<Message>(4);
        let (control, mut controls) = mpsc::channel::<Message>(2);
        let (inbound, incoming) = mpsc::channel::<WireResult>(8);
        let (failure_tx, failure) = watch::channel(None);
        let pending = Arc::new(Mutex::new(None::<(String, Instant)>));
        let writer_failure = failure_tx.clone();
        let mut stop_writer = failure.clone();
        let write_task = tokio::spawn(async move {
            loop {
                let message = tokio::select! { biased;
                    _ = stop_writer.changed() => break,
                    message = controls.recv() => message,
                    message = outbound.recv() => message,
                };
                let Some(message) = message else { break };
                let result = tokio::select! {
                    _ = stop_writer.changed() => break,
                    result = writer.send(message) => result,
                };
                if result.is_err() {
                    let _ = writer_failure.send(Some("sync socket write failed"));
                    break;
                }
            }
        });
        let read_failure = failure_tx.clone();
        let pending_read = pending.clone();
        let controls_read = control.clone();
        let mut stop_read = failure.clone();
        let read_task = tokio::spawn(async move {
            // Keep reading controls while a slow business consumer is backpressured.
            // Lifecycle snapshots coalesce; commands and terminal outcomes remain bounded.
            let mut queued = VecDeque::<Message>::new();
            loop {
                let next = tokio::select! {
                    _ = stop_read.changed() => break,
                    permit = inbound.reserve(), if !queued.is_empty() => {
                        let Ok(permit) = permit else { break };
                        permit.send(Ok(queued.pop_front().unwrap()));
                        continue;
                    }
                    message = reader.next() => message,
                };
                let Some(Ok(message)) = next else {
                    let _ = read_failure.send(Some("sync socket closed"));
                    break;
                };
                match &message {
                    Message::Text(text) => {
                        if let Ok(frame) = Frame::decode(text) {
                            if frame.topic == "phoenix"
                                && frame.event == "phx_reply"
                                && frame.payload["status"] == "ok"
                            {
                                let mut active = pending_read.lock().await;
                                if active.as_ref().is_some_and(|(reference, _)| {
                                    frame.reference.as_ref() == Some(reference)
                                }) {
                                    *active = None;
                                }
                                continue;
                            }
                        }
                    }
                    Message::Ping(bytes) => {
                        if controls_read
                            .try_send(Message::Pong(bytes.clone()))
                            .is_err()
                        {
                            let _ = read_failure.send(Some("sync socket control queue full"));
                            break;
                        }
                        continue;
                    }
                    Message::Pong(_) => continue,
                    Message::Close(_) => {
                        let _ = read_failure.send(Some("sync socket closed"));
                        break;
                    }
                    _ => {}
                }
                if buffer_business(&mut queued, message).is_err() {
                    let _ = read_failure.send(Some(
                        "sync protocol business queue exceeded or conflicting status",
                    ));
                    break;
                }
            }
        });
        let mut stop_heartbeat = failure.clone();
        let heartbeat_task = tokio::spawn(async move {
            let mut next = Instant::now() + HEARTBEAT;
            let mut sequence = 0u64;
            loop {
                let expires = pending
                    .lock()
                    .await
                    .as_ref()
                    .map(|(_, at)| *at + HEARTBEAT_WAIT)
                    .unwrap_or(next);
                tokio::select! {
                    _ = stop_heartbeat.changed() => break,
                    _ = tokio::time::sleep_until(expires) => {},
                }
                let now = Instant::now();
                let mut active = pending.lock().await;
                if active
                    .as_ref()
                    .is_some_and(|(_, at)| now >= *at + HEARTBEAT_WAIT)
                {
                    let _ = failure_tx.send(Some("sync heartbeat reply timed out"));
                    break;
                }
                if active.is_none() && now >= next {
                    sequence += 1;
                    let reference = format!("hb-{sequence}");
                    let frame = Frame::new(
                        None,
                        Some(&reference),
                        "phoenix",
                        "heartbeat",
                        serde_json::json!({}),
                    );
                    // Mark queued before touching the writer: a blocked sink cannot disable this watchdog.
                    *active = Some((reference, now));
                    if control
                        .try_send(Message::Text(frame.encode().unwrap().into()))
                        .is_err()
                    {
                        let _ = failure_tx.send(Some("sync socket control queue full"));
                        break;
                    }
                    next = now + HEARTBEAT;
                }
            }
        });
        Self::Pump(Pump {
            outgoing,
            incoming,
            failure,
            workers: vec![write_task, read_task, heartbeat_task],
        })
    }
    pub async fn next(&mut self) -> Option<WireResult> {
        match self {
            #[cfg(test)]
            Self::Empty => unreachable!(),
            #[cfg(test)]
            Self::Raw(socket) => socket.next().await,
            Self::Pump(pump) => {
                if let Some(error) = *pump.failure.borrow() {
                    return Some(Err(failed(error)));
                }
                tokio::select! { biased;
                    _ = pump.failure.changed() => Some(Err(failed(pump.failure.borrow().unwrap_or("sync socket closed")))),
                    message = pump.incoming.recv() => message,
                }
            }
        }
    }
    pub async fn send(&mut self, message: Message) -> Result<(), Error> {
        match self {
            #[cfg(test)]
            Self::Empty => unreachable!(),
            #[cfg(test)]
            Self::Raw(socket) => socket.send(message).await,
            Self::Pump(pump) => {
                if let Message::Text(text) = &message {
                    if text.len() > FRAME_BYTES {
                        return Err(failed("sync frame exceeds negotiated maximum"));
                    }
                }
                if let Some(error) = *pump.failure.borrow() {
                    return Err(failed(error));
                }
                tokio::select! { biased;
                    _ = pump.failure.changed() => Err(failed(pump.failure.borrow().unwrap_or("sync socket closed"))),
                    result = pump.outgoing.send(message) => result.map_err(|_| failed("sync socket closed")),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn business(event: &str, payload: serde_json::Value) -> Message {
        Message::Text(
            Frame::new(Some("join"), None, "note_sync:v1", event, payload)
                .encode()
                .unwrap()
                .into(),
        )
    }
    #[test]
    fn coalescing_preserves_status_validation_and_terminal_outcomes() {
        let mut queue = VecDeque::new();
        let status = super::super::tasks::Status {
            task_id: "task:1".into(),
            status_seq: 1,
            state: "running".into(),
            phase: "checking".into(),
            progress: json!({}),
            result: None,
            error: None,
        };
        buffer_business(&mut queue, business("task_status", json!(status))).unwrap();
        let mut conflict = status.clone();
        conflict.phase = "changed".into();
        assert!(buffer_business(&mut queue, business("task_status", json!(conflict))).is_err());
        let mut terminal = status.clone();
        terminal.status_seq = 2;
        terminal.state = "succeeded".into();
        terminal.result = Some(json!({}));
        buffer_business(&mut queue, business("task_status", json!(terminal))).unwrap();
        let mut changed = terminal.clone();
        changed.status_seq = 3;
        assert!(buffer_business(&mut queue, business("task_status", json!(changed))).is_err());
        assert_eq!(queue.len(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn full_business_queue_does_not_block_heartbeats_or_lose_terminal_results() {
        let (client, server) = tokio::io::duplex(1024 * 1024);
        let client: Box<dyn super::super::runtime::Transport> = Box::new(client);
        let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
            tokio_tungstenite::MaybeTlsStream::Plain(client),
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let mut peer = tokio_tungstenite::WebSocketStream::from_raw_socket(
            server,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let mut io = Io::start(client);
        let peer_worker = tokio::spawn(async move {
            for seq in 1..=100 {
                peer.send(business(
                    "sync_manager_status",
                    json!({"round_id":"round","status_seq":seq,"progress":{"checked_notes":seq}}),
                ))
                .await
                .unwrap();
                for task in 1..=4 {
                    peer.send(business("task_status",json!({"task_id":format!("task:{task}"),"status_seq":seq,"state":"running","phase":"checking","progress":{"checked_notes":seq},"result":null,"error":null}))).await.unwrap();
                }
            }
            for task in 1..=4 {
                peer.send(business("task_status",json!({"task_id":format!("task:{task}"),"status_seq":101,"state":"succeeded","phase":"checking","progress":{},"result":{},"error":null}))).await.unwrap();
            }
            for _ in 0..3 {
                let message = peer.next().await.unwrap().unwrap();
                let frame = Frame::decode(&message.into_text().unwrap()).unwrap();
                assert_eq!(frame.event, "heartbeat");
                peer.send(Message::Text(
                    Frame::new(
                        None,
                        frame.reference.as_deref(),
                        "phoenix",
                        "phx_reply",
                        json!({"status":"ok","response":{}}),
                    )
                    .encode()
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            }
            peer
        });
        // No business consumption until three complete heartbeat intervals have passed.
        let peer = peer_worker.await.unwrap();
        let mut latest_manager = 0;
        let mut terminals = std::collections::BTreeSet::new();
        while latest_manager < 100 || terminals.len() < 4 {
            let message = io.next().await.unwrap().unwrap();
            let frame = Frame::decode(&message.into_text().unwrap()).unwrap();
            if frame.event == "sync_manager_status" {
                latest_manager = frame.payload["status_seq"].as_u64().unwrap();
            }
            if frame.event == "task_status" && frame.payload["state"] == "succeeded" {
                terminals.insert(frame.payload["task_id"].as_str().unwrap().to_owned());
            }
        }
        assert_eq!(latest_manager, 100);
        drop(peer);
    }
}
