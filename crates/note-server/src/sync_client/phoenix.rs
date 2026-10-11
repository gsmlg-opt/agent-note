//! Phoenix Channels V2 JSON serialization: [join_ref, ref, topic, event, payload].
use anyhow::{bail, Result};
use serde_json::Value;

pub const FRAME_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub join_ref: Option<String>,
    pub reference: Option<String>,
    pub topic: String,
    pub event: String,
    pub payload: Value,
}
impl Frame {
    pub fn new(
        join_ref: Option<&str>,
        reference: Option<&str>,
        topic: &str,
        event: &str,
        payload: Value,
    ) -> Self {
        Self {
            join_ref: join_ref.map(str::to_owned),
            reference: reference.map(str::to_owned),
            topic: topic.into(),
            event: event.into(),
            payload,
        }
    }
    pub fn encode(&self) -> Result<String> {
        let text = serde_json::to_string(&(
            self.join_ref.as_deref(),
            self.reference.as_deref(),
            &self.topic,
            &self.event,
            &self.payload,
        ))?;
        if text.len() > FRAME_BYTES {
            bail!("sync frame exceeds negotiated maximum");
        }
        Ok(text)
    }
    pub fn decode(text: &str) -> Result<Self> {
        if text.len() > FRAME_BYTES {
            bail!("sync frame exceeds negotiated maximum");
        }
        let (join_ref, reference, topic, event, payload): (
            Option<String>,
            Option<String>,
            String,
            String,
            Value,
        ) = serde_json::from_str(text)?;
        if !payload.is_object() || topic.len() > 256 || event.len() > 128 {
            bail!("invalid Phoenix V2 frame");
        }
        Ok(Self {
            join_ref,
            reference,
            topic,
            event,
            payload,
        })
    }
    pub fn is_current(&self, join: &str, reference: &str) -> bool {
        self.join_ref.as_deref() == Some(join) && self.reference.as_deref() == Some(reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn phoenix_v2_uses_five_element_array_and_string_refs() {
        let frame = Frame::new(
            Some("9"),
            Some("10"),
            "note_sync:v1",
            "hello",
            json!({"version":1}),
        );
        assert_eq!(
            frame.encode().unwrap(),
            "[\"9\",\"10\",\"note_sync:v1\",\"hello\",{\"version\":1}]"
        );
        assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), frame);
        assert!(Frame::decode(r#"[9,10,"note_sync:v1","hello",{}]"#).is_err());
        assert!(Frame::decode(r#"[null,"1","note_sync:v1","hello"]"#).is_err());
        assert!(Frame::decode(r#"[null,"1","note_sync:v1","hello",[]]"#).is_err());
    }

    #[test]
    fn old_join_frames_cannot_ack_current_requests() {
        let reply = Frame::new(
            Some("old"),
            Some("10"),
            "note_sync:v1",
            "phx_reply",
            json!({"status":"ok","response":{}}),
        );
        assert!(!reply.is_current("new", "10"));
        assert!(reply.is_current("old", "10"));
    }
}
