use chrono::{TimeZone, Utc};
use chrono_tz::Tz;

use super::model::OrgTimestamp;

pub const UNAVAILABLE_TIME: &str = "Unavailable";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeDisplay {
    pub workspace: String,
    pub browser_local: String,
}

pub fn format_org_timestamp(timestamp: &OrgTimestamp) -> TimeDisplay {
    format_timestamp(timestamp.utc_timestamp, &timestamp.timezone)
}

pub fn format_timestamp(utc_timestamp: i64, workspace_timezone: &str) -> TimeDisplay {
    TimeDisplay {
        workspace: format_in_timezone(utc_timestamp, workspace_timezone),
        browser_local: format_browser_local(utc_timestamp),
    }
}

pub fn format_in_timezone(utc_timestamp: i64, workspace_timezone: &str) -> String {
    let Some(utc) = Utc.timestamp_opt(utc_timestamp, 0).single() else {
        return UNAVAILABLE_TIME.to_owned();
    };
    let Ok(timezone) = workspace_timezone.parse::<Tz>() else {
        return UNAVAILABLE_TIME.to_owned();
    };
    timezone
        .from_utc_datetime(&utc.naive_utc())
        .format("%Y-%m-%d %H:%M:%S %Z")
        .to_string()
}

#[cfg(target_arch = "wasm32")]
pub fn format_browser_local(utc_timestamp: i64) -> String {
    if Utc.timestamp_opt(utc_timestamp, 0).single().is_none() {
        return UNAVAILABLE_TIME.to_owned();
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(
        utc_timestamp as f64 * 1_000.0,
    ));
    String::from(date.to_locale_string("default", &wasm_bindgen::JsValue::UNDEFINED))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn format_browser_local(utc_timestamp: i64) -> String {
    if Utc.timestamp_opt(utc_timestamp, 0).single().is_some() {
        UNAVAILABLE_TIME.to_owned()
    } else {
        UNAVAILABLE_TIME.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_time_observes_named_timezone_and_dst() {
        assert_eq!(
            format_in_timezone(1_767_225_600, "America/New_York"),
            "2025-12-31 19:00:00 EST"
        );
        assert_eq!(
            format_in_timezone(1_751_324_400, "America/New_York"),
            "2025-06-30 19:00:00 EDT"
        );
    }

    #[test]
    fn server_timestamp_uses_utc_instant_and_server_timezone() {
        let display = format_org_timestamp(&OrgTimestamp {
            raw: "<2026-08-05 Wed 09:00>".to_owned(),
            local: "2026-08-05T09:00:00".to_owned(),
            timezone: "Asia/Shanghai".to_owned(),
            utc_timestamp: 1_775_008_800,
        });
        assert_eq!(display.workspace, "2026-04-01 10:00:00 CST");
        assert_eq!(display.browser_local, UNAVAILABLE_TIME);
    }

    #[test]
    fn invalid_timezone_or_timestamp_is_explicitly_unavailable() {
        assert_eq!(format_in_timezone(0, "Mars/Olympus_Mons"), UNAVAILABLE_TIME);
        assert_eq!(format_in_timezone(i64::MAX, "UTC"), UNAVAILABLE_TIME);
        assert_eq!(format_browser_local(i64::MAX), UNAVAILABLE_TIME);
    }
}
