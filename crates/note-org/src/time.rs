use crate::OrgTimestamp;
use chrono::{LocalResult, TimeZone};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOrgTimestamp {
    pub raw: String,
    pub local: chrono::NaiveDateTime,
    pub timezone: String,
    pub utc_timestamp: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeZoneError {
    #[error("invalid IANA timezone {0}")]
    InvalidZone(String),
    #[error("local time {local} is ambiguous in {timezone}")]
    Ambiguous { local: String, timezone: String },
    #[error("local time {local} does not exist in {timezone}")]
    Nonexistent { local: String, timezone: String },
}

pub fn resolve_org_timestamp(
    value: &OrgTimestamp,
    timezone: &str,
) -> Result<ResolvedOrgTimestamp, TimeZoneError> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| TimeZoneError::InvalidZone(timezone.to_string()))?;
    let zoned = match zone.from_local_datetime(&value.local) {
        LocalResult::Single(value) => value,
        LocalResult::Ambiguous(_, _) => {
            return Err(TimeZoneError::Ambiguous {
                local: value.local.to_string(),
                timezone: timezone.to_string(),
            });
        }
        LocalResult::None => {
            return Err(TimeZoneError::Nonexistent {
                local: value.local.to_string(),
                timezone: timezone.to_string(),
            });
        }
    };
    Ok(ResolvedOrgTimestamp {
        raw: value.raw.clone(),
        local: value.local,
        timezone: timezone.to_string(),
        utc_timestamp: zoned.timestamp(),
    })
}
