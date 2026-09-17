//! Official Google Calendar adapter. Tokens never leave this module's private boundary.
use crate::{
    auth_store::{AuthStore, Entry},
    scheduling::{Slot, Window},
};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration as Timeout};

pub const SCOPES: &str = "openid email profile https://www.googleapis.com/auth/calendar.freebusy https://www.googleapis.com/auth/calendar.events.owned https://www.googleapis.com/auth/calendar.calendarlist.readonly";
const API: &str = "https://www.googleapis.com/calendar/v3";
pub type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Serialize, Deserialize)]
pub struct Availability {
    pub timezone: String,
    pub slots: Vec<Slot>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub slot: Slot,
    pub accepted: bool,
}
#[async_trait]
pub trait Calendars: Send + Sync {
    async fn history(
        &self,
        _id: &str,
        _peer_email: &str,
    ) -> Result<(String, Vec<crate::agent::preferences::Observation>)> {
        Err("history_unavailable")
    }
    async fn connected(&self, id: &str) -> Result<bool>;
    async fn connect(
        &self,
        id: &str,
        email: &str,
        refresh: Option<&str>,
        scopes: &[String],
    ) -> Result<()>;
    async fn disconnect(&self, id: &str) -> Result<()>;
    async fn availability(&self, id: &str, window: &Window) -> Result<Availability>;
    async fn email(&self, id: &str) -> Result<String>;
    async fn event(&self, id: &str, event_id: &str, slot: &Slot) -> Result<Option<Event>>;
    async fn insert(
        &self,
        id: &str,
        event_id: &str,
        slot: &Slot,
        guest_email: &str,
    ) -> Result<Event>;
    async fn accept(&self, id: &str, event_id: &str, slot: &Slot) -> Result<Option<Event>>;
}
#[derive(Serialize, Deserialize)]
struct Connection {
    cipher: String,
    email: String,
}
pub struct GoogleCalendar {
    store: Arc<dyn AuthStore>,
    kms: aws_sdk_kms::Client,
    key_id: String,
    secrets: aws_sdk_secretsmanager::Client,
    secret_id: String,
    client_id: String,
    secret: tokio::sync::OnceCell<String>,
    http: reqwest::Client,
    api: String,
}
impl GoogleCalendar {
    pub fn new(
        store: Arc<dyn AuthStore>,
        kms: aws_sdk_kms::Client,
        key_id: String,
        secrets: aws_sdk_secretsmanager::Client,
        secret_id: String,
        client_id: String,
    ) -> std::result::Result<Self, lambda_http::Error> {
        Ok(Self {
            store,
            api: API.into(),
            kms,
            key_id,
            secrets,
            secret_id,
            client_id,
            secret: tokio::sync::OnceCell::new(),
            http: reqwest::Client::builder()
                .connect_timeout(Timeout::from_secs(2))
                .timeout(Timeout::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    async fn connection(&self, id: &str) -> Result<Connection> {
        let row = self
            .store
            .get(&format!("calendar:{id}"), crate::auth::now())
            .await
            .map_err(|_| "storage_unavailable")?
            .ok_or("calendar_connection_required")?;
        serde_json::from_value(row.value).map_err(|_| "storage_unavailable")
    }
    async fn token(&self, id: &str) -> Result<String> {
        let connection = self.connection(id).await?;
        let decoded = a2a_card::canonical::b64url_decode(&connection.cipher)
            .map_err(|_| "calendar_connection_required")?;
        let plain = self
            .kms
            .decrypt()
            .key_id(&self.key_id)
            .ciphertext_blob(aws_sdk_kms::primitives::Blob::new(decoded))
            .encryption_context("account", id)
            .encryption_context("service", "calendar")
            .send()
            .await
            .map_err(|_| "calendar_unavailable")?;
        let refresh =
            std::str::from_utf8(plain.plaintext().ok_or("calendar_unavailable")?.as_ref())
                .map_err(|_| "calendar_unavailable")?;
        let secret = self
            .secret
            .get_or_try_init(|| async {
                let r = self
                    .secrets
                    .get_secret_value()
                    .secret_id(&self.secret_id)
                    .send()
                    .await
                    .map_err(|_| "calendar_unavailable")?;
                r.secret_string()
                    .map(str::to_owned)
                    .ok_or("calendar_unavailable")
            })
            .await?;
        let response = self
            .http
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("client_secret", secret.as_str()),
                ("refresh_token", refresh),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(|_| "calendar_unavailable")?;
        let status = response.status();
        let value: Value = response.json().await.map_err(|_| "calendar_unavailable")?;
        if value["error"] == "invalid_grant" {
            return Err("calendar_reconnect_required");
        }
        if !status.is_success() {
            return Err("calendar_unavailable");
        }
        value["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or("calendar_unavailable")
    }
    async fn response(
        response: std::result::Result<reqwest::Response, reqwest::Error>,
    ) -> Result<Value> {
        let response = response.map_err(|_| "calendar_unavailable")?;
        if response.status() == 401 || response.status() == 403 {
            return Err("calendar_reconnect_required");
        }
        if !response.status().is_success() {
            return Err("calendar_unavailable");
        }
        response.json().await.map_err(|_| "calendar_unavailable")
    }
    async fn get_event(&self, token: &str, event_id: &str, slot: &Slot) -> Result<Option<Event>> {
        let response = self
            .http
            .get(format!("{}/calendars/primary/events/{event_id}", self.api))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| "calendar_unavailable")?;
        if response.status() == 404 {
            return Ok(None);
        }
        let value = Self::response(Ok(response)).await?;
        decode_event(&value, event_id, slot).map(Some)
    }
    async fn accept_with_token(
        &self,
        token: &str,
        email: &str,
        event_id: &str,
        slot: &Slot,
    ) -> Result<Option<Event>> {
        let response = self
            .http
            .get(format!("{}/calendars/primary/events/{event_id}", self.api))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(|_| "calendar_unavailable")?;
        let etag = if response.status() == 404 {
            // An invitation may be hidden by the attendee's invitation setting.
            // Try the scoped RSVP update; never create a separate guest event.
            None
        } else {
            let value = Self::response(Ok(response)).await?;
            let event = decode_event(&value, event_id, slot)?;
            if event.accepted {
                return Ok(Some(event));
            }
            Some(
                value["etag"]
                    .as_str()
                    .ok_or("invalid_calendar_response")?
                    .to_owned(),
            )
        };
        let mut request = self.http.patch(format!("{}/calendars/primary/events/{event_id}", self.api))
            .query(&[("sendUpdates", "all")]).bearer_auth(&token)
            .json(&json!({"attendeesOmitted":true,"attendees":[{"email":email,"responseStatus":"accepted"}]}));
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_MATCH, etag);
        }
        let response = request.send().await.map_err(|_| "calendar_unavailable")?;
        if response.status() == 404 || response.status() == 412 {
            return Ok(None);
        }
        let value = Self::response(Ok(response)).await?;
        decode_event(&value, event_id, slot)?;
        // The guest's copy, with its unchanged time and accepted RSVP, is the
        // source of truth for a successful two-calendar booking.
        self.get_event(&token, event_id, slot).await
    }
}
#[async_trait]
impl Calendars for GoogleCalendar {
    async fn history(
        &self,
        id: &str,
        peer_email: &str,
    ) -> Result<(String, Vec<crate::agent::preferences::Observation>)> {
        let token = self.token(id).await?;
        let now: DateTime<Utc> = std::time::SystemTime::now().into();
        let start = now
            .checked_sub_months(chrono::Months::new(9))
            .ok_or("invalid_window")?;
        let end = now
            .checked_add_months(chrono::Months::new(3))
            .ok_or("invalid_window")?;
        let mut page = String::new();
        let mut events = Vec::new();
        for _ in 0..8 {
            let mut request = self
                .http
                .get(format!("{}/calendars/primary/events", self.api))
                .bearer_auth(&token)
                .query(&[
                    ("timeMin", start.to_rfc3339()),
                    ("timeMax", end.to_rfc3339()),
                    ("singleEvents", "true".into()),
                    ("maxResults", "1000".into()),
                    ("orderBy", "startTime".into()),
                ]);
            if !page.is_empty() {
                request = request.query(&[("pageToken", &page)]);
            }
            let v = Self::response(request.send().await).await?;
            let timezone = v["timeZone"]
                .as_str()
                .ok_or("invalid_calendar_response")?
                .to_owned();
            for e in v["items"].as_array().ok_or("invalid_calendar_response")? {
                if e["status"] == "cancelled"
                    || e["transparency"] == "transparent"
                    || e["eventType"].as_str().is_some_and(|t| t != "default")
                {
                    continue;
                }
                let organized = e["organizer"]["self"] == true;
                let attendees = e["attendees"].as_array();
                if !organized
                    && !attendees.is_some_and(|a| {
                        a.iter()
                            .any(|v| v["self"] == true && v["responseStatus"] == "accepted")
                    })
                {
                    continue;
                }
                let Some(start) = e["start"]["dateTime"]
                    .as_str()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                else {
                    continue;
                };
                let Some(end) = e["end"]["dateTime"]
                    .as_str()
                    .and_then(|s| s.parse::<DateTime<Utc>>().ok())
                else {
                    continue;
                };
                let Some(event_id) = e["id"].as_str() else {
                    continue;
                };
                events.push(crate::agent::preferences::Observation {
                    id: event_id.into(),
                    title: e["summary"]
                        .as_str()
                        .unwrap_or("Untitled meeting")
                        .chars()
                        .take(120)
                        .collect(),
                    description: e["description"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(180)
                        .collect(),
                    start,
                    end,
                    organized,
                    peer: attendees.is_some_and(|a| {
                        a.iter().any(|v| {
                            v["email"]
                                .as_str()
                                .is_some_and(|s| s.eq_ignore_ascii_case(peer_email))
                                && v["responseStatus"] != "declined"
                        })
                    }),
                    series: e["recurringEventId"].as_str().map(str::to_owned),
                });
            }
            match v["nextPageToken"].as_str() {
                Some(p) => page = p.into(),
                None => return Ok((timezone, events)),
            }
        }
        // Do not infer preferences from a silently incomplete oldest-first history.
        Err("history_too_large")
    }
    async fn connected(&self, id: &str) -> Result<bool> {
        Ok(self
            .store
            .get(&format!("calendar:{id}"), crate::auth::now())
            .await
            .map_err(|_| "storage_unavailable")?
            .is_some())
    }
    async fn connect(
        &self,
        id: &str,
        email: &str,
        refresh: Option<&str>,
        scopes: &[String],
    ) -> Result<()> {
        if !SCOPES
            .split_whitespace()
            .filter(|s| s.starts_with("https://www.googleapis.com/auth/calendar"))
            .all(|needed| scopes.iter().any(|s| s == needed))
        {
            return Err("calendar_permission_required");
        }
        let Some(refresh) = refresh else {
            return if self.connected(id).await? {
                Ok(())
            } else {
                Err("calendar_reconnect_required")
            };
        };
        let encrypted = self
            .kms
            .encrypt()
            .key_id(&self.key_id)
            .plaintext(aws_sdk_kms::primitives::Blob::new(refresh.as_bytes()))
            .encryption_context("account", id)
            .encryption_context("service", "calendar")
            .send()
            .await
            .map_err(|_| "calendar_unavailable")?;
        let cipher = a2a_card::canonical::b64url(
            encrypted
                .ciphertext_blob()
                .ok_or("calendar_unavailable")?
                .as_ref(),
        );
        self.store
            .put(
                &format!("calendar:{id}"),
                Entry {
                    value: serde_json::to_value(Connection {
                        cipher,
                        email: email.into(),
                    })
                    .unwrap(),
                    expires: 0,
                    binding: String::new(),
                },
            )
            .await
            .map_err(|_| "storage_unavailable")
    }
    async fn disconnect(&self, id: &str) -> Result<()> {
        self.store
            .delete(&format!("calendar:{id}"))
            .await
            .map_err(|_| "storage_unavailable")
    }
    async fn email(&self, id: &str) -> Result<String> {
        Ok(self.connection(id).await?.email)
    }
    async fn availability(&self, id: &str, window: &Window) -> Result<Availability> {
        if !window.valid() {
            return Err("invalid_window");
        }
        let token = self.token(id).await?;
        let (meta, busy) = tokio::join!(
            async {
                Self::response(
                    self.http
                        .get(format!("{}/users/me/calendarList/primary", self.api))
                        .bearer_auth(&token)
                        .send()
                        .await,
                )
                .await
            },
            async {
                Self::response(self.http.post(format!("{}/freeBusy", self.api)).bearer_auth(&token).json(&json!({"timeMin":window.start,"timeMax":window.end,"timeZone":"UTC","items":[{"id":"primary"}]})).send().await).await
            }
        );
        let timezone = meta?["timeZone"]
            .as_str()
            .ok_or("invalid_calendar_response")?
            .to_owned();
        let busy = busy?;
        let calendar = busy["calendars"]
            .get("primary")
            .ok_or("invalid_calendar_response")?;
        if calendar
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty())
        {
            return Err("calendar_unavailable");
        }
        let busy: Vec<Slot> = serde_json::from_value(
            calendar
                .get("busy")
                .cloned()
                .ok_or("invalid_calendar_response")?,
        )
        .map_err(|_| "invalid_calendar_response")?;
        Ok(Availability {
            slots: working_slots(window, &timezone, &busy)?,
            timezone,
        })
    }
    async fn event(&self, id: &str, event_id: &str, slot: &Slot) -> Result<Option<Event>> {
        self.get_event(&self.token(id).await?, event_id, slot).await
    }
    async fn insert(
        &self,
        id: &str,
        event_id: &str,
        slot: &Slot,
        guest_email: &str,
    ) -> Result<Event> {
        let token = self.token(id).await?;
        let response=self.http.post(format!("{}/calendars/primary/events", self.api)).query(&[("sendUpdates","all")]).bearer_auth(&token).json(&json!({
            "id":event_id,"summary":"Aithos Calendar meeting","start":{"dateTime":slot.start},"end":{"dateTime":slot.end},
            "attendees":[{"email":guest_email}],"guestsCanModify":false,"guestsCanInviteOthers":false,
            "extendedProperties":{"private":{"aithosBooking":event_id}}
        })).send().await.map_err(|_|"booking_outcome_unknown")?;
        if response.status() == 409 {
            return self
                .get_event(&token, event_id, slot)
                .await?
                .ok_or("booking_outcome_unknown");
        }
        // Even an unexpected HTTP failure is reconciled by deterministic event ID later.
        let value = Self::response(Ok(response))
            .await
            .map_err(|_| "booking_outcome_unknown")?;
        decode_event(&value, event_id, slot)
    }
    async fn accept(&self, id: &str, event_id: &str, slot: &Slot) -> Result<Option<Event>> {
        self.accept_with_token(
            &self.token(id).await?,
            &self.email(id).await?,
            event_id,
            slot,
        )
        .await
    }
}
fn decode_event(value: &Value, event_id: &str, slot: &Slot) -> Result<Event> {
    let parse = |field: &str| -> Result<DateTime<Utc>> {
        value[field]["dateTime"]
            .as_str()
            .ok_or("invalid_calendar_response")?
            .parse()
            .map_err(|_| "invalid_calendar_response")
    };
    if value["id"] != event_id
        || value["status"] == "cancelled"
        || parse("start")? != slot.start
        || parse("end")? != slot.end
    {
        return Err("event_changed");
    }
    Ok(Event {
        id: event_id.into(),
        slot: slot.clone(),
        accepted: value["attendees"].as_array().is_some_and(|a| {
            a.iter()
                .any(|a| a["self"] == true && a["responseStatus"] == "accepted")
        }),
    })
}
/// Free intervals within each owner's weekday 09:00–18:00 window.
pub fn working_slots(window: &Window, timezone: &str, busy: &[Slot]) -> Result<Vec<Slot>> {
    let tz: chrono_tz::Tz = timezone.parse().map_err(|_| "invalid_calendar_timezone")?;
    if !window.valid() || busy.iter().any(|s| s.end <= s.start) {
        return Err("invalid_calendar_response");
    }
    let mut day = window.start.with_timezone(&tz).date_naive();
    let last = window.end.with_timezone(&tz).date_naive();
    let mut slots = vec![];
    while day <= last {
        if day.weekday().number_from_monday() <= 5 {
            let local = day.and_time(NaiveTime::from_hms_opt(9, 0, 0).unwrap());
            let end = tz
                .from_local_datetime(&day.and_time(NaiveTime::from_hms_opt(18, 0, 0).unwrap()))
                .single()
                .ok_or("invalid_calendar_timezone")?
                .with_timezone(&Utc);
            let start = tz
                .from_local_datetime(&local)
                .single()
                .ok_or("invalid_calendar_timezone")?
                .with_timezone(&Utc);
            let mut ranges = vec![Slot {
                start: start.max(window.start),
                end: end.min(window.end),
            }];
            ranges.retain(|s| s.end > s.start);
            for block in busy {
                let mut next = vec![];
                for r in ranges {
                    if block.end <= r.start || block.start >= r.end {
                        next.push(r);
                        continue;
                    }
                    if block.start > r.start {
                        next.push(Slot {
                            start: r.start,
                            end: block.start,
                        });
                    }
                    if block.end < r.end {
                        next.push(Slot {
                            start: block.end,
                            end: r.end,
                        });
                    }
                }
                ranges = next;
            }
            slots.extend(ranges);
        }
        day = day.succ_opt().ok_or("invalid_window")?;
    }
    Ok(slots)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot(a: &str, b: &str) -> Slot {
        Slot {
            start: a.parse().unwrap(),
            end: b.parse().unwrap(),
        }
    }
    #[test]
    fn business_hours_subtract_overlap_and_all_day_busy_across_dst_weekend() {
        let w = Window {
            start: "2026-10-23T00:00:00Z".parse().unwrap(),
            end: "2026-10-27T00:00:00Z".parse().unwrap(),
        };
        let busy = vec![
            slot("2026-10-23T08:00:00Z", "2026-10-23T09:30:00Z"),
            slot("2026-10-23T09:00:00Z", "2026-10-23T10:00:00Z"),
        ];
        let slots = working_slots(&w, "Europe/Paris", &busy).unwrap();
        assert_eq!(
            slots,
            vec![
                slot("2026-10-23T07:00:00Z", "2026-10-23T08:00:00Z"),
                slot("2026-10-23T10:00:00Z", "2026-10-23T16:00:00Z"),
                slot("2026-10-26T08:00:00Z", "2026-10-26T17:00:00Z")
            ]
        );
        assert!(
            working_slots(
                &w,
                "Europe/Paris",
                &[slot("2026-10-22T00:00:00Z", "2026-10-28T00:00:00Z")]
            )
            .unwrap()
            .is_empty()
        );
        assert!(working_slots(&w, "invalid", &[]).is_err());
    }
    #[test]
    fn only_matching_uncancelled_event_and_self_rsvp_are_successful() {
        let s = slot("2030-01-15T10:00:00Z", "2030-01-15T10:30:00Z");
        let mut v = json!({"id":"ac123","start":{"dateTime":s.start},"end":{"dateTime":s.end},"attendees":[{"self":false,"responseStatus":"accepted"}]});
        assert!(!decode_event(&v, "ac123", &s).unwrap().accepted);
        v["attendees"][0]["self"] = json!(true);
        assert!(decode_event(&v, "ac123", &s).unwrap().accepted);
        v["status"] = json!("cancelled");
        assert!(decode_event(&v, "ac123", &s).is_err());
    }
}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use axum::{
        Json, Router,
        http::{HeaderMap, StatusCode},
        routing::get,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[tokio::test]
    async fn hidden_invitation_receives_only_self_rsvp_then_readback_verifies_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api = format!("http://{}", listener.local_addr().unwrap());
        let reads = Arc::new(AtomicUsize::new(0));
        let read_count = reads.clone();
        let slot = Slot {
            start: "2030-01-15T10:00:00Z".parse().unwrap(),
            end: "2030-01-15T10:30:00Z".parse().unwrap(),
        };
        let event = json!({"id":"ac123","etag":"etag","start":{"dateTime":slot.start},"end":{"dateTime":slot.end},"attendees":[{"email":"guest@example.com","self":true,"responseStatus":"accepted"}]});
        let read_event = event.clone();
        let routes=Router::new().route("/calendars/primary/events/ac123",get(move||{let count=read_count.clone();let event=read_event.clone();async move {
            if count.fetch_add(1,Ordering::SeqCst)==0{(StatusCode::NOT_FOUND,Json(json!({})))}else{(StatusCode::OK,Json(event))}
        }}).patch(move|headers:HeaderMap,Json(body):Json<Value>|{let event=event.clone();async move{
            assert_eq!(headers["authorization"],"Bearer test-token");
            assert_eq!(body,json!({"attendeesOmitted":true,"attendees":[{"email":"guest@example.com","responseStatus":"accepted"}]}));Json(event)
        }}));
        let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
        let config = aws_config::SdkConfig::builder()
            .behavior_version(aws_config::BehaviorVersion::latest())
            .region(aws_config::Region::new("eu-west-3"))
            .build();
        let mut google = GoogleCalendar::new(
            Arc::new(crate::auth_store::MemoryAuthStore::default()),
            aws_sdk_kms::Client::new(&config),
            "unused".into(),
            aws_sdk_secretsmanager::Client::new(&config),
            "unused".into(),
            "unused".into(),
        )
        .unwrap();
        google.api = api;
        assert!(
            google
                .accept_with_token("test-token", "guest@example.com", "ac123", &slot)
                .await
                .unwrap()
                .unwrap()
                .accepted
        );
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        task.abort();
    }
}
