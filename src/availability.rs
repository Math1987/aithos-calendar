//! Replaceable read-only adapter for Google's undocumented public booking RPCs.
use crate::booking_page::{BookingPage, GoogleBookingPages, normalized};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration as Timeout;

pub use crate::scheduling::Slot;
const RPC: &str = "https://calendar-pa.clients6.google.com/$rpc/google.internal.calendar.v1.AppointmentBookingService/";
const MAX_SLOTS: usize = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadError {
    InvalidPage,
    InvalidWindow,
    Unavailable,
    UnsupportedResponse,
}
impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPage => "Invalid public Google booking page",
            Self::InvalidWindow => "Availability window must be positive and at most 30 days",
            Self::Unavailable => "Google availability is temporarily unavailable",
            Self::UnsupportedResponse => "Google returned an unsupported booking-page response",
        })
    }
}
impl std::error::Error for ReadError {}

/// Public contact details associated with a page, not proof of ownership.
/// Kept out of Schedule serialization (including future A2A availability replies).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PageIdentity {
    pub display_name: Option<String>,
    pub email: Option<String>,
}
impl std::fmt::Debug for PageIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PageIdentity([redacted])")
    }
}
fn identity_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| s.len() <= 254 && !s.chars().any(char::is_control))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Serialize)]
pub struct Schedule {
    pub schedule_id: String,
    #[serde(skip)]
    pub identity: PageIdentity,
    pub title: String,
    pub timezone: String,
    pub duration_minutes: u16,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    /// Discrete appointments offered by the page, not a complete free/busy view.
    pub slots: Vec<Slot>,
}
#[async_trait]
pub trait AvailabilityReader: Send + Sync {
    async fn read(
        &self,
        page: &BookingPage,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Schedule, ReadError>;
}

pub struct GoogleHttpReader {
    pages: GoogleBookingPages,
    http: reqwest::Client,
}
impl GoogleHttpReader {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            pages: GoogleBookingPages::new()?,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Timeout::from_secs(2))
                .timeout(Timeout::from_secs(4))
                .build()?,
        })
    }
    async fn rpc(&self, method: &str, key: &str, body: Value) -> Result<Value, ReadError> {
        let mut response = self
            .http
            .post(format!("{RPC}{method}"))
            .header("content-type", "application/json+protobuf")
            .header("x-goog-api-key", key)
            .header("origin", "https://calendar.google.com")
            .header("referer", "https://calendar.google.com/")
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| ReadError::Unavailable)?;
        if !response.status().is_success() {
            return Err(
                if response.status().is_server_error() || response.status().as_u16() == 429 {
                    ReadError::Unavailable
                } else {
                    ReadError::UnsupportedResponse
                },
            );
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ReadError::Unavailable)? {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(ReadError::UnsupportedResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ReadError::UnsupportedResponse)
    }
}

// Parse data, never evaluate scripts. The key is public page configuration, not
// a user credential. Ignore arbitrary RPC origins present in the HTML.
fn public_key(html: &str) -> Result<String, ReadError> {
    let raw = html
        .split_once("window.WIZ_global_data = ")
        .ok_or(ReadError::UnsupportedResponse)?
        .1;
    let config = serde_json::Deserializer::from_str(raw)
        .into_iter::<Value>()
        .next()
        .ok_or(ReadError::UnsupportedResponse)?
        .map_err(|_| ReadError::UnsupportedResponse)?;
    let initial = config["m7LZgc"]
        .as_str()
        .and_then(|s| s.strip_prefix("%.@."))
        .ok_or(ReadError::UnsupportedResponse)?;
    let data: Value =
        serde_json::from_str(&format!("[{initial}")).map_err(|_| ReadError::UnsupportedResponse)?;
    if data[0] != "appointments.AppointmentsInitialData" {
        return Err(ReadError::UnsupportedResponse);
    }
    let key = data[4]
        .as_str()
        .filter(|s| {
            s.starts_with("AIza")
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or(ReadError::UnsupportedResponse)?;
    Ok(key.to_owned())
}
fn minutes(value: &Value) -> Result<u16, ReadError> {
    value
        .as_u64()
        .filter(|n| (1..=1440).contains(n))
        .map(|n| n as u16)
        .ok_or(ReadError::UnsupportedResponse)
}
fn timestamp(value: &Value) -> Result<DateTime<Utc>, ReadError> {
    let sec = value[0]
        .as_i64()
        .or_else(|| value[0].as_str().and_then(|s| s.parse().ok()))
        .ok_or(ReadError::UnsupportedResponse)?;
    let nanos = value
        .get(1)
        .map_or(Some(0), Value::as_u64)
        .filter(|v| *v < 1_000_000_000)
        .ok_or(ReadError::UnsupportedResponse)?;
    DateTime::from_timestamp(sec, nanos as u32).ok_or(ReadError::UnsupportedResponse)
}
fn parse_schedule(
    id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    definition: &Value,
    available: &Value,
) -> Result<Schedule, ReadError> {
    let data = &definition[0];
    if data[6] != id {
        return Err(ReadError::UnsupportedResponse);
    }
    let title = data[1]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 2048)
        .ok_or(ReadError::UnsupportedResponse)?
        .to_owned();
    let timezone = data[24]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 100 && !s.chars().any(char::is_control))
        .ok_or(ReadError::UnsupportedResponse)?
        .to_owned();
    let durations = data[5][0]
        .as_array()
        .filter(|v| v.len() == 1)
        .ok_or(ReadError::UnsupportedResponse)?;
    let duration_minutes = minutes(&durations[0])?;
    let top = available.as_array().ok_or(ReadError::UnsupportedResponse)?;
    // Protobuf JSON arrays omit unset trailing fields: [] is an empty response.
    let empty = Vec::new();
    let entries = if top.is_empty() {
        &empty
    } else {
        top[0].as_array().ok_or(ReadError::UnsupportedResponse)?
    };
    let mut slots = Vec::new();
    for entry in entries {
        let periods = entry
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or(ReadError::UnsupportedResponse)?;
        for period in periods {
            if slots.len() >= MAX_SLOTS {
                return Err(ReadError::UnsupportedResponse);
            }
            let slot_start = timestamp(&period[0])?;
            let duration = minutes(&period[1])?;
            if duration != duration_minutes {
                return Err(ReadError::UnsupportedResponse);
            }
            let slot_end = slot_start
                .checked_add_signed(Duration::minutes(i64::from(duration)))
                .ok_or(ReadError::UnsupportedResponse)?;
            if slot_start < start || slot_end > end {
                continue;
            }
            slots.push(Slot {
                start: slot_start,
                end: slot_end,
            });
        }
    }
    slots.sort_by_key(|s| s.start);
    slots.dedup();
    Ok(Schedule {
        schedule_id: id.into(),
        identity: PageIdentity {
            display_name: identity_text(&data[3]),
            email: identity_text(&data[26]),
        },
        title,
        timezone,
        duration_minutes,
        window_start: start,
        window_end: end,
        slots,
    })
}
#[async_trait]
impl AvailabilityReader for GoogleHttpReader {
    async fn read(
        &self,
        page: &BookingPage,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Schedule, ReadError> {
        if end <= start || end - start > Duration::days(30) {
            return Err(ReadError::InvalidWindow);
        }
        let url = normalized(&page.url).map_err(|_| ReadError::InvalidPage)?;
        if url.host_str() != Some("calendar.google.com") || url.as_str() != page.url {
            return Err(ReadError::InvalidPage);
        }
        let id = url
            .path_segments()
            .and_then(|mut p| p.next_back())
            .ok_or(ReadError::InvalidPage)?;
        let html = self
            .pages
            .html(page)
            .await
            .map_err(|_| ReadError::Unavailable)?;
        let key = public_key(&html)?;
        let (definition, available) = tokio::try_join!(
            self.rpc(
                "GetAppointmentServiceDefinition",
                &key,
                json!([null, null, id])
            ),
            self.rpc(
                "ListAvailableSlots",
                &key,
                json!([
                    null,
                    null,
                    id,
                    null,
                    [[start.timestamp()], [end.timestamp()]]
                ])
            )
        )?;
        parse_schedule(id, start, end, &definition, &available)
    }
}

/// Preserve a complete offered host appointment. Merge only touching/overlapping
/// peer intervals; never invent a host start or bridge a gap in peer coverage.
pub fn first_host_slot(host: &Schedule, peer: &Schedule) -> Option<Slot> {
    let mut coverage = peer.slots.clone();
    coverage.sort_by_key(|s| s.start);
    let mut merged: Vec<Slot> = Vec::new();
    for interval in coverage {
        if interval.start >= interval.end {
            continue;
        }
        if let Some(last) = merged.last_mut().filter(|last| interval.start <= last.end) {
            last.end = last.end.max(interval.end);
        } else {
            merged.push(interval);
        }
    }
    host.slots
        .iter()
        .filter(|slot| {
            slot.start >= host.window_start.max(peer.window_start)
                && slot.end <= host.window_end.min(peer.window_end)
                && slot.end - slot.start == Duration::minutes(i64::from(host.duration_minutes))
                && merged
                    .iter()
                    .any(|p| p.start <= slot.start && p.end >= slot.end)
        })
        .min_by_key(|s| s.start)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + minutes * 60, 0).unwrap()
    }
    fn schedule(duration: u16, ranges: &[(i64, i64)]) -> Schedule {
        Schedule {
            schedule_id: "test".into(),
            identity: PageIdentity::default(),
            title: "Test".into(),
            timezone: "UTC".into(),
            duration_minutes: duration,
            window_start: at(0),
            window_end: at(180),
            slots: ranges
                .iter()
                .map(|(s, e)| Slot {
                    start: at(*s),
                    end: at(*e),
                })
                .collect(),
        }
    }
    #[test]
    fn preserves_host_grid_and_merges_peer_coverage_without_bridging_gaps() {
        let host = schedule(60, &[(60, 120), (0, 60)]);
        assert_eq!(
            first_host_slot(&host, &schedule(30, &[(30, 60), (0, 30)])),
            Some(Slot {
                start: at(0),
                end: at(60)
            })
        );
        assert!(first_host_slot(&host, &schedule(30, &[(0, 29), (30, 60)])).is_none());
        assert!(first_host_slot(&host, &schedule(60, &[(15, 75)])).is_none());
        assert_eq!(
            first_host_slot(&schedule(30, &[(30, 60)]), &schedule(60, &[(0, 60)]))
                .unwrap()
                .start,
            at(30)
        );
    }
    #[test]
    fn parser_rejects_schema_drift_and_accepts_only_requested_identity_and_duration() {
        let mut data = vec![Value::Null; 25];
        data[1] = json!("Meeting");
        data[5] = json!([[30]]);
        data[6] = json!("test");
        data[24] = json!("Europe/Paris");
        let definition = json!([data]);
        let slots = json!([[[[["1800000000"], 30]]]]);
        let result = parse_schedule("test", at(0), at(180), &definition, &slots).unwrap();
        assert_eq!(
            result.slots[0],
            Slot {
                start: at(0),
                end: at(30)
            }
        );
        assert!(parse_schedule("other", at(0), at(180), &definition, &slots).is_err());
        assert!(
            parse_schedule(
                "test",
                at(0),
                at(180),
                &definition,
                &json!([[[[["1800000000"], 60]]]])
            )
            .is_err()
        );
        assert!(
            parse_schedule(
                "test",
                at(0),
                at(180),
                &definition,
                &json!({"error":"unavailable"})
            )
            .is_err()
        );
        assert!(
            parse_schedule("test", at(0), at(180), &definition, &json!([]))
                .unwrap()
                .slots
                .is_empty()
        );
    }
    #[test]
    fn contact_is_optional_and_never_serialized_with_availability() {
        let mut data = vec![Value::Null; 27];
        data[1] = json!("Meeting");
        data[5] = json!([[30]]);
        data[6] = json!("test");
        data[24] = json!("UTC");
        data[3] = json!("Jane Doe");
        data[26] = json!("jane@example.com");
        let parsed =
            parse_schedule("test", at(0), at(180), &json!([data.clone()]), &json!([])).unwrap();
        assert_eq!(parsed.identity.display_name.as_deref(), Some("Jane Doe"));
        assert_eq!(parsed.identity.email.as_deref(), Some("jane@example.com"));
        assert!(
            !serde_json::to_string(&parsed)
                .unwrap()
                .contains("jane@example.com")
        );
        assert!(!format!("{parsed:?}").contains("Jane Doe"));
        data[3] = json!({"unexpected":"shape"});
        data.truncate(25);
        let parsed = parse_schedule("test", at(0), at(180), &json!([data]), &json!([])).unwrap();
        assert!(parsed.identity.display_name.is_none() && parsed.identity.email.is_none());
    }

    #[test]
    fn configuration_is_parsed_as_data() {
        let key = "AIzaPublicFixture";
        let config = json!({"m7LZgc":format!("%.@.\"appointments.AppointmentsInitialData\",null,null,null,\"{key}\"]")});
        assert_eq!(
            public_key(&format!("window.WIZ_global_data = {config};otherScript();")).unwrap(),
            key
        );
        assert!(public_key("window.WIZ_global_data = malicious();").is_err());
    }
}
