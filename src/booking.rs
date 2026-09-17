//! Anakin Wire transport. Submission is never automatically retried.
//! A completed job is provider data, not yet a verified appointment confirmation.
use crate::availability::{Schedule, Slot};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Serialize)]
pub struct Attendee {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
}
impl Attendee {
    pub fn from_env() -> Result<Self, BookingError> {
        let read = |key| std::env::var(key).map_err(|_| BookingError::NotConfigured);
        let attendee = Self {
            first_name: read("BOOKING_TEST_FIRST_NAME")?,
            last_name: read("BOOKING_TEST_LAST_NAME")?,
            email: read("BOOKING_TEST_EMAIL")?,
        };
        attendee.validate()?;
        if attendee.email.ends_with("@example.com") {
            return Err(BookingError::NotConfigured);
        }
        Ok(attendee)
    }
    fn validate(&self) -> Result<(), BookingError> {
        if [&self.first_name, &self.last_name, &self.email]
            .iter()
            .any(|v| v.trim().is_empty() || v.len() > 254 || v.chars().any(char::is_control))
            || !self.email.contains('@')
            || self.email.chars().any(char::is_whitespace)
        {
            return Err(BookingError::InvalidRequest);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BookingError {
    NotConfigured,
    InvalidRequest,
    Rejected,
    Unavailable,
    /// A write may have reached the provider: do not resubmit automatically.
    SubmissionUnknown,
    InvalidResponse,
}
impl std::fmt::Display for BookingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "Booking provider or test attendee is not configured",
            Self::InvalidRequest => "Invalid appointment or attendee",
            Self::Rejected => "Anakin rejected the request",
            Self::Unavailable => "Anakin is temporarily unavailable",
            Self::SubmissionUnknown => "Booking submission outcome is unknown; do not resubmit",
            Self::InvalidResponse => "Anakin returned an unsupported response",
        })
    }
}
impl std::error::Error for BookingError {}

pub struct BookingRequest {
    pub schedule_id: String,
    pub slot: Slot,
    pub duration_minutes: u16,
    pub attendee: Attendee,
}
impl BookingRequest {
    /// Build only from an offered host slot. The caller must obtain fresh
    /// availability before submitting and persist its operation before any write.
    pub fn from_schedule(
        schedule: &Schedule,
        slot: Slot,
        attendee: Attendee,
    ) -> Result<Self, BookingError> {
        attendee.validate()?;
        if !schedule.slots.contains(&slot)
            || slot.end - slot.start
                != chrono::Duration::minutes(i64::from(schedule.duration_minutes))
        {
            return Err(BookingError::InvalidRequest);
        }
        Ok(Self {
            schedule_id: schedule.schedule_id.clone(),
            slot,
            duration_minutes: schedule.duration_minutes,
            attendee,
        })
    }
    fn payload(&self) -> Result<Value, BookingError> {
        self.attendee.validate()?;
        if self.schedule_id.is_empty()
            || self.schedule_id.len() > 256
            || !self
                .schedule_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.duration_minutes == 0
            || self.duration_minutes > 1440
            || self.slot.start.timestamp_subsec_nanos() != 0
            || self.slot.end - self.slot.start
                != chrono::Duration::minutes(i64::from(self.duration_minutes))
        {
            return Err(BookingError::InvalidRequest);
        }
        Ok(json!({"action_id":"gap_book_slot","params":{
            "schedule_id":self.schedule_id,"slot_start_unix":self.slot.start.timestamp(),"duration_minutes":self.duration_minutes,
            "first_name":self.attendee.first_name,"last_name":self.attendee.last_name,"email":self.attendee.email
        }}))
    }
}

#[derive(Debug)]
pub enum JobStatus {
    Processing {
        retry_after_ms: u64,
    },
    /// Must be validated against the requested slot and a booking confirmation
    /// before any application/UI can claim success. Actual schema needs a live test.
    CompletedUnverified(Value),
    Failed,
}
#[async_trait]
pub trait BookingProvider: Send + Sync {
    async fn submit(&self, request: &BookingRequest) -> Result<String, BookingError>;
    async fn status(&self, job_id: &str) -> Result<JobStatus, BookingError>;
}
pub struct AnakinBooking {
    http: reqwest::Client,
    key: reqwest::header::HeaderValue,
    origin: String,
}
impl AnakinBooking {
    pub fn from_env() -> Result<Self, BookingError> {
        Self::new(&std::env::var("ANAKIN_API_KEY").map_err(|_| BookingError::NotConfigured)?)
    }
    pub fn new(key: &str) -> Result<Self, BookingError> {
        if key.trim().is_empty() || key == "example" {
            return Err(BookingError::NotConfigured);
        }
        let mut key =
            reqwest::header::HeaderValue::from_str(key).map_err(|_| BookingError::NotConfigured)?;
        key.set_sensitive(true);
        Ok(Self {
            key,
            origin: "https://api.anakin.io".into(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(8))
                .build()
                .map_err(|_| BookingError::NotConfigured)?,
        })
    }
}
async fn response_json(mut response: reqwest::Response) -> Result<Value, BookingError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| BookingError::Unavailable)?
    {
        if bytes.len() + chunk.len() > 256 * 1024 {
            return Err(BookingError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| BookingError::InvalidResponse)
}
fn valid_job(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn parse_status(body: Value) -> Result<JobStatus, BookingError> {
    match body["status"].as_str() {
        Some("processing") => Ok(JobStatus::Processing {
            retry_after_ms: body["retry_after_ms"]
                .as_u64()
                .unwrap_or(2000)
                .clamp(1000, 30000),
        }),
        Some("completed") if body["data"].is_object() => {
            Ok(JobStatus::CompletedUnverified(body["data"].clone()))
        }
        Some("failed") => Ok(JobStatus::Failed),
        _ => Err(BookingError::InvalidResponse),
    }
}
#[async_trait]
impl BookingProvider for AnakinBooking {
    async fn submit(&self, request: &BookingRequest) -> Result<String, BookingError> {
        let response = self
            .http
            .post(format!("{}/v1/wire/task", self.origin))
            .header("x-api-key", self.key.clone())
            .json(&request.payload()?)
            .send()
            .await
            .map_err(|_| BookingError::SubmissionUnknown)?;
        if response.status().is_client_error()
            && response.status() != reqwest::StatusCode::REQUEST_TIMEOUT
        {
            return Err(BookingError::Rejected);
        }
        if response.status() != reqwest::StatusCode::ACCEPTED {
            return Err(BookingError::SubmissionUnknown);
        }
        let body = response_json(response)
            .await
            .map_err(|_| BookingError::SubmissionUnknown)?;
        body["job_id"]
            .as_str()
            .filter(|id| valid_job(id))
            .map(str::to_owned)
            .ok_or(BookingError::SubmissionUnknown)
    }
    async fn status(&self, job_id: &str) -> Result<JobStatus, BookingError> {
        if !valid_job(job_id) {
            return Err(BookingError::InvalidRequest);
        }
        let response = self
            .http
            .get(format!("{}/v1/wire/jobs/{job_id}", self.origin))
            .header("x-api-key", self.key.clone())
            .send()
            .await
            .map_err(|_| BookingError::Unavailable)?;
        if !response.status().is_success() {
            return Err(BookingError::Unavailable);
        }
        parse_status(response_json(response).await?)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_job_requires_further_confirmation_and_unknown_states_fail_closed() {
        assert!(matches!(
            parse_status(json!({"status":"completed","data":{"something":"done"}})),
            Ok(JobStatus::CompletedUnverified(_))
        ));
        assert!(parse_status(json!({"status":"completed","data":null})).is_err());
        assert!(parse_status(json!({"status":"future_status"})).is_err());
        assert!(matches!(
            parse_status(json!({"status":"processing","retry_after_ms":0})),
            Ok(JobStatus::Processing {
                retry_after_ms: 1000
            })
        ));
    }
    #[tokio::test]
    async fn uncertain_submission_is_not_retried() {
        use axum::{Router, http::StatusCode, routing::post};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let app = Router::new().route(
            "/v1/wire/task",
            post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::BAD_GATEWAY }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut client = AnakinBooking::new("test-only").unwrap();
        client.origin = origin;
        let start = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let request = BookingRequest {
            schedule_id: "test".into(),
            slot: Slot {
                start,
                end: start + chrono::Duration::minutes(30),
            },
            duration_minutes: 30,
            attendee: Attendee {
                first_name: "Test".into(),
                last_name: "Attendee".into(),
                email: "test@example.com".into(),
            },
        };
        assert_eq!(
            client.submit(&request).await.unwrap_err(),
            BookingError::SubmissionUnknown
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.abort();
    }
}
