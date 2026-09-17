//! Anakin Wire transport. Submission is never automatically retried.
//! A completed job is provider data, not yet a verified appointment confirmation.
use crate::availability::{PageIdentity, Schedule, Slot};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Attendee {
    pub first_name: String,
    pub last_name: String,
    pub email: String,
}
/// Values requested only when the page's contact details are incomplete.
#[derive(Clone, Default, serde::Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AttendeeDetails {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub email: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttendeeField {
    FirstName,
    LastName,
    Email,
}
impl std::fmt::Display for AttendeeField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::FirstName => "first name",
            Self::LastName => "last name",
            Self::Email => "email",
        })
    }
}
#[derive(Debug)]
pub struct MissingAttendeeDetails {
    pub fields: Vec<AttendeeField>,
}
impl std::fmt::Display for MissingAttendeeDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Attendee details needed: {}",
            self.fields
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
impl std::error::Error for MissingAttendeeDetails {}
fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 254 && !value.chars().any(char::is_control)
}
fn valid_email(value: &str) -> bool {
    // Supported address subset, not an ownership or deliverability check.
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && value.len() <= 254
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&b))
        && domain.contains('.')
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
impl Attendee {
    /// Use the visitor page, never the host's contact or a global fallback.
    /// Two word display names use a first/last heuristic. It is not verified
    /// identity; compound names and other formats require explicit details.
    pub fn from_page(
        identity: &PageIdentity,
        details: &AttendeeDetails,
    ) -> Result<Self, MissingAttendeeDetails> {
        let words: Vec<_> = identity
            .display_name
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .collect();
        let name_word = |s: &str| {
            s.chars().any(char::is_alphabetic)
                && s.chars()
                    .all(|c| c.is_alphabetic() || matches!(c, '-' | '\'' | '’'))
        };
        let inferred = match words.as_slice() {
            [first, last] if name_word(first) && name_word(last) => Some((*first, *last)),
            _ => None,
        };
        let first = details
            .first_name
            .as_deref()
            .or(inferred.map(|v| v.0))
            .map(str::trim);
        let last = details
            .last_name
            .as_deref()
            .or(inferred.map(|v| v.1))
            .map(str::trim);
        let email = details
            .email
            .as_deref()
            .or(identity.email.as_deref())
            .map(str::trim);
        let mut fields = Vec::new();
        if !first.is_some_and(valid_name) {
            fields.push(AttendeeField::FirstName);
        }
        if !last.is_some_and(valid_name) {
            fields.push(AttendeeField::LastName);
        }
        if !email.is_some_and(valid_email) {
            fields.push(AttendeeField::Email);
        }
        if !fields.is_empty() {
            return Err(MissingAttendeeDetails { fields });
        }
        Ok(Self {
            first_name: first.unwrap().into(),
            last_name: last.unwrap().into(),
            email: email.unwrap().into(),
        })
    }
    fn validate(&self) -> Result<(), BookingError> {
        if !valid_name(&self.first_name)
            || !valid_name(&self.last_name)
            || !valid_email(&self.email)
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
            Self::NotConfigured => "Booking provider is not configured",
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
    /// Explicit provider rejection before a slot could be selected.
    SlotUnavailable,
    Failed,
}
#[async_trait]
pub trait BookingProvider: Send + Sync {
    async fn ready(&self) -> Result<(), BookingError> {
        Ok(())
    }
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
        Some("failed")
            if body["credits_used"].as_u64() == Some(0)
                && body["error"]["code"] == "EXECUTION_FAILED"
                && body["error"]["message"]
                    == "[bad_params] The requested slot is no longer available. Please refresh the available slots and try a different one." =>
        {
            Ok(JobStatus::SlotUnavailable)
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
    fn only_observed_preselection_rejection_is_safe_to_release() {
        let original = json!({"status":"failed","credits_used":0,"error":{"code":"EXECUTION_FAILED","message":"[bad_params] The requested slot is no longer available. Please refresh the available slots and try a different one."}});
        assert!(matches!(
            parse_status(original.clone()).unwrap(),
            JobStatus::SlotUnavailable
        ));
        let mut other = original.clone();
        other["error"]["message"] = json!("Could not confirm the slot after submitting");
        assert!(matches!(parse_status(other).unwrap(), JobStatus::Failed));
        let mut charged = original;
        charged["credits_used"] = json!(10);
        assert!(matches!(parse_status(charged).unwrap(), JobStatus::Failed));
    }
    #[test]
    fn visitor_identity_reaches_provider_payload_and_missing_fields_are_specific() {
        let page = PageIdentity {
            display_name: Some("Mathieu Colla".into()),
            email: Some("guest@example.com".into()),
        };
        let attendee = Attendee::from_page(&page, &AttendeeDetails::default()).unwrap();
        let start = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let request = BookingRequest {
            schedule_id: "host-schedule".into(),
            slot: Slot {
                start,
                end: start + chrono::Duration::minutes(30),
            },
            duration_minutes: 30,
            attendee,
        };
        let params = request.payload().unwrap()["params"].clone();
        assert_eq!(params["schedule_id"], "host-schedule");
        assert_eq!(params["email"], "guest@example.com");
        assert_eq!(params["first_name"], "Mathieu");
        assert_eq!(params["last_name"], "Colla");
        let incomplete = PageIdentity {
            email: None,
            ..page.clone()
        };
        assert_eq!(
            Attendee::from_page(&incomplete, &AttendeeDetails::default())
                .err()
                .unwrap()
                .fields,
            vec![AttendeeField::Email]
        );
        let corrected = Attendee::from_page(
            &incomplete,
            &AttendeeDetails {
                email: Some("supplied@example.com".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(corrected.first_name, "Mathieu");
        assert_eq!(corrected.email, "supplied@example.com");
        for name in ["Prince", "María del Carmen", "Support / Sales", ""] {
            let ambiguous = PageIdentity {
                display_name: Some(name.into()),
                ..page.clone()
            };
            assert_eq!(
                Attendee::from_page(&ambiguous, &AttendeeDetails::default())
                    .err()
                    .unwrap()
                    .fields,
                vec![AttendeeField::FirstName, AttendeeField::LastName]
            );
        }
        for email in [
            "",
            "x@y@z.com",
            "Name <x@example.com>",
            "x\n@example.com",
            "x@-example.com",
            "x@localhost",
        ] {
            let invalid = PageIdentity {
                email: Some(email.into()),
                ..page.clone()
            };
            assert_eq!(
                Attendee::from_page(&invalid, &AttendeeDetails::default())
                    .err()
                    .unwrap()
                    .fields,
                vec![AttendeeField::Email]
            );
        }
    }

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

/// Cache a successfully loaded provider client in each warm Lambda environment.
/// Health and read-only A2A requests never need the booking secret.
pub struct SecretsBooking {
    client: aws_sdk_secretsmanager::Client,
    secret_id: String,
    provider: tokio::sync::OnceCell<AnakinBooking>,
}
impl SecretsBooking {
    pub fn new(client: aws_sdk_secretsmanager::Client, secret_id: String) -> Self {
        Self {
            client,
            secret_id,
            provider: tokio::sync::OnceCell::new(),
        }
    }
    async fn get(&self) -> Result<&AnakinBooking, BookingError> {
        self.provider
            .get_or_try_init(|| async {
                let result = self
                    .client
                    .get_secret_value()
                    .secret_id(&self.secret_id)
                    .send()
                    .await
                    .map_err(|_| BookingError::NotConfigured)?;
                AnakinBooking::new(result.secret_string().ok_or(BookingError::NotConfigured)?)
            })
            .await
    }
}
#[async_trait]
impl BookingProvider for SecretsBooking {
    async fn ready(&self) -> Result<(), BookingError> {
        self.get().await.map(|_| ())
    }
    async fn submit(&self, request: &BookingRequest) -> Result<String, BookingError> {
        self.get().await?.submit(request).await
    }
    async fn status(&self, job_id: &str) -> Result<JobStatus, BookingError> {
        self.get().await?.status(job_id).await
    }
}
