//! Authenticated browser orchestration and scoped, same-server A2A calls.
use crate::{
    auth::{Auth, current, error, now, origin_ok, random},
    auth_store::{AuthStore, Entry},
    booking_store::{BookingOperation, BookingStore, Stage},
    google_calendar::{Availability, Calendars},
    scheduling::{Slot, Window},
    storage::AgentStore,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub struct Connected {
    pub jobs: Option<Arc<crate::agent::jobs::Jobs>>,
    pub calendars: Arc<dyn Calendars>,
    pub store: Arc<dyn AuthStore>,
    pub bookings: Arc<dyn BookingStore>,
    pub agents: Arc<dyn AgentStore>,
    pub directory: Arc<crate::discovery::PeerDirectory>,
    pub policies: crate::trust::Policies,
    pub website: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Proposal {
    id: String,
    host: String,
    peer: String,
    slot: Slot,
    timezone: String,
    expires: i64,
}
fn digest(v: &str) -> String {
    crate::booking_api::digest(v)
}
fn view(r: &BookingOperation) -> Value {
    json!({"id":r.id,"host":r.host,"peer":r.peer,"slot":r.slot,"status":match r.stage {
        Stage::Booked=>"booked",Stage::SlotUnavailable=>"slot_unavailable",Stage::Failed=>"failed",
        Stage::Pending|Stage::ConfirmationRequired=>"confirming_guest",Stage::Unknown=>"outcome_unknown",Stage::Submitting=>"processing"
    },"reserved":r.stage==Stage::Booked,"retry_after_ms":3000})
}
impl Connected {
    async fn call(
        &self,
        host: &str,
        caller: &str,
        data: Value,
        policy: crate::trust::Policy,
    ) -> Result<Value, &'static str> {
        let token = random();
        let trace_id = a2a::new_message_id();
        let key = format!("a2a-grant:{}", digest(&token));
        let row = Entry {
            value: json!({"recipient":host,"caller":caller,"request":data}),
            expires: now() + 90,
            binding: String::new(),
        };
        self.store
            .create(&key, row)
            .await
            .map_err(|_| "storage_unavailable")?;
        let result = self
            .directory
            .account_call(
                &self.directory.publisher().urn(host),
                &token,
                data,
                &trace_id,
                policy,
            )
            .await
            .map_err(|e| e.code());
        let _ = self.store.delete(&key).await;
        result
    }
    /// The SDK supplies HTTP headers separately from untrusted message metadata.
    pub async fn handle(
        &self,
        tenant: &str,
        params: &a2a_server::ServiceParams,
        data: &Value,
    ) -> Result<Value, &'static str> {
        let values = params
            .get("authorization")
            .filter(|v| v.len() == 1)
            .ok_or("a2a_authorization_required")?;
        let token = values[0]
            .strip_prefix("Bearer ")
            .filter(|v| v.len() == 43)
            .ok_or("a2a_authorization_required")?;
        let row = self
            .store
            .get(&format!("a2a-grant:{}", digest(token)), now())
            .await
            .map_err(|_| "storage_unavailable")?
            .ok_or("a2a_authorization_required")?;
        if row.value["recipient"] != tenant || row.value["request"] != *data {
            return Err("a2a_authorization_required");
        }
        let caller = row.value["caller"]
            .as_str()
            .ok_or("a2a_authorization_required")?;
        match data["operation"].as_str() {
            Some("get_availability") => {
                let window: Window =
                    serde_json::from_value(data["window"].clone()).map_err(|_| "invalid_window")?;
                let a = self.calendars.availability(tenant, &window).await?;
                let profile =
                    crate::agent::preferences::cached(self.store.as_ref(), tenant, caller).await;
                Ok(
                    json!({"status":"availability","agent":self.directory.publisher().urn(tenant),"availability":a,"preferences":profile.preferences}),
                )
            }
            Some("commit_booking") => {
                let id = data["booking_id"].as_str().ok_or("invalid_booking")?;
                let r = self
                    .bookings
                    .get(id)
                    .await
                    .map_err(|_| "storage_unavailable")?
                    .ok_or("invalid_booking")?;
                if r.schedule_id != "google"
                    || r.host != tenant
                    || r.peer != caller
                    || matches!(r.stage, Stage::Failed | Stage::SlotUnavailable)
                {
                    return Err("invalid_booking");
                }
                let event_id = format!("ac{}", digest(id));
                if self
                    .calendars
                    .event(tenant, &event_id, &r.slot)
                    .await?
                    .is_some()
                {
                    return Ok(json!({"status":"event_created","event_id":event_id}));
                }
                if data["submit"] != true {
                    return Err("booking_outcome_unknown");
                }
                let window = Window {
                    start: r.slot.start,
                    end: r.slot.end,
                };
                let available = self.calendars.availability(tenant, &window).await?;
                if !available.slots.contains(&r.slot) {
                    return Err("slot_no_longer_available");
                }
                let email = self.calendars.email(caller).await?;
                self.calendars
                    .insert(tenant, &event_id, &r.slot, &email)
                    .await?;
                Ok(json!({"status":"event_created","event_id":event_id}))
            }
            _ => Err("unsupported_operation"),
        }
    }
    async fn propose(&self, peer: &str, host: &str) -> Result<Option<Proposal>, &'static str> {
        self.propose_with_id(peer, host, &format!("gc{}", &digest(&random())[..40]))
            .await
    }
    pub(crate) async fn propose_with_id(
        &self,
        peer: &str,
        host: &str,
        id: &str,
    ) -> Result<Option<Proposal>, &'static str> {
        if host == peer {
            return Err("same_account");
        }
        let record = self
            .agents
            .get(host)
            .await
            .map_err(|_| "storage_unavailable")?
            .filter(|r| r.published && r.agent.google_account)
            .ok_or("unknown_connected_agent")?;
        if !self.calendars.connected(&record.agent.id).await? {
            return Err("host_calendar_not_connected");
        }
        let window = Window::next_month();
        let (own, remote) = tokio::join!(
            self.calendars.availability(peer, &window),
            self.call(
                host,
                peer,
                json!({"operation":"get_availability","window":window}),
                self.policies.live,
            )
        );
        let own = own?;
        let remote = remote?;
        if remote["status"] == "error" {
            return Err(match remote["code"].as_str() {
                Some("calendar_reconnect_required" | "calendar_connection_required") => {
                    "host_calendar_not_connected"
                }
                _ => "peer_unavailable",
            });
        }
        if remote["agent"] != self.directory.publisher().urn(host) {
            return Err("invalid_peer_response");
        }
        let host_availability: Availability =
            serde_json::from_value(remote["availability"].clone())
                .map_err(|_| "invalid_peer_response")?;
        let own_profile = crate::agent::preferences::cached(self.store.as_ref(), peer, host).await;
        let host_preferences =
            serde_json::from_value(remote["preferences"].clone()).unwrap_or_default();
        let slot = crate::agent::preferences::select(
            &host_availability.slots,
            &own.slots,
            &host_availability.timezone,
            &own.timezone,
            &host_preferences,
            &own_profile.preferences,
            window.start,
        );
        let Some(slot) = slot else {
            return Ok(None);
        };
        let proposal = Proposal {
            id: id.into(),
            host: host.into(),
            peer: peer.into(),
            slot,
            timezone: host_availability.timezone,
            expires: now() + 900,
        };
        self.store
            .put(
                &format!("proposal:{}", proposal.id),
                Entry {
                    value: serde_json::to_value(&proposal).unwrap(),
                    expires: proposal.expires,
                    binding: String::new(),
                },
            )
            .await
            .map_err(|_| "storage_unavailable")?;
        Ok(Some(proposal))
    }
    pub(crate) async fn confirm(&self, peer: &str, id: &str) -> Result<Value, &'static str> {
        if id.len() != 42
            || !id.starts_with("gc")
            || !id[2..].bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid_booking");
        }
        let existing = self
            .bookings
            .get(id)
            .await
            .map_err(|_| "storage_unavailable")?;
        let (mut r, fresh) = if let Some(r) = existing {
            if r.schedule_id != "google" || r.peer != peer {
                return Err("invalid_booking");
            }
            if matches!(
                r.stage,
                Stage::Booked | Stage::Failed | Stage::SlotUnavailable
            ) || r.next_poll > now()
            {
                return Ok(view(&r));
            }
            (r, false)
        } else {
            let proposal = self
                .store
                .get(&format!("proposal:{id}"), now())
                .await
                .map_err(|_| "storage_unavailable")?
                .ok_or("proposal_expired")?;
            let p: Proposal =
                serde_json::from_value(proposal.value).map_err(|_| "invalid_booking")?;
            if p.peer != peer || p.expires <= now() {
                return Err("invalid_booking");
            }
            if p.slot.start.timestamp() <= now() {
                return Err("proposal_expired");
            }
            let available = self
                .calendars
                .availability(
                    peer,
                    &Window {
                        start: p.slot.start,
                        end: p.slot.end,
                    },
                )
                .await?;
            if !available.slots.contains(&p.slot) {
                return Err("slot_no_longer_available");
            }
            let r = BookingOperation {
                id: id.into(),
                request_digest: digest(&serde_json::to_string(&p).unwrap()),
                host: p.host,
                peer: p.peer,
                slot: p.slot,
                schedule_id: "google".into(),
                email_digest: String::new(),
                stage: Stage::Submitting,
                job_id: None,
                created_at: now(),
                next_poll: now() + 35,
                revision: 0,
            };
            if !self
                .bookings
                .begin(&r)
                .await
                .map_err(|_| "storage_unavailable")?
            {
                return match self
                    .bookings
                    .get(id)
                    .await
                    .map_err(|_| "storage_unavailable")?
                {
                    Some(r) if r.peer == peer => Ok(view(&r)),
                    _ => Err("booking_already_in_progress"),
                };
            }
            (r, true)
        };
        if !fresh {
            let previous = r.revision;
            r.revision += 1;
            r.next_poll = now() + 35;
            if !self
                .bookings
                .save(&r, previous, false)
                .await
                .map_err(|_| "storage_unavailable")?
            {
                return Ok(view(&r));
            }
        }
        let previous = r.revision;
        let result = self.advance(&r, fresh).await;
        r.revision += 1;
        r.next_poll = now() + 3;
        let release = match result {
            Ok(stage) => {
                r.stage = stage;
                stage == Stage::Booked
            }
            Err("calendar_preflight_failed") if fresh => {
                r.stage = Stage::Failed;
                true
            }
            Err("slot_no_longer_available") if fresh => {
                r.stage = Stage::SlotUnavailable;
                true
            }
            Err(code) => {
                tracing::warn!(event="connected_booking_pending",operation_id=%r.id,code);
                r.stage = if r.stage == Stage::Pending {
                    Stage::Pending
                } else {
                    Stage::Unknown
                };
                false
            }
        };
        if !self
            .bookings
            .save(&r, previous, release)
            .await
            .map_err(|_| "storage_unavailable")?
        {
            return Err("booking_status_unavailable");
        }
        tracing::info!(event="connected_booking_outcome",operation_id=%r.id,status=?r.stage);
        Ok(view(&r))
    }
    async fn advance(&self, r: &BookingOperation, fresh: bool) -> Result<Stage, &'static str> {
        if fresh {
            let window = Window {
                start: r.slot.start,
                end: r.slot.end,
            };
            let available = self
                .calendars
                .availability(&r.peer, &window)
                .await
                .map_err(|_| "calendar_preflight_failed")?;
            if !available.slots.contains(&r.slot) {
                return Err("slot_no_longer_available");
            }
        }
        let response = self
            .call(
                &r.host,
                &r.peer,
                json!({"operation":"commit_booking","booking_id":r.id,"submit":fresh}),
                self.policies.booking,
            )
            .await?;
        if response["status"] != "event_created" {
            return Err(response["code"]
                .as_str()
                .filter(|s| *s == "slot_no_longer_available")
                .map(|_| "slot_no_longer_available")
                .unwrap_or("booking_outcome_unknown"));
        }
        let event_id = format!("ac{}", digest(&r.id));
        if response["event_id"] != event_id {
            return Err("invalid_peer_response");
        }
        match self.calendars.accept(&r.peer, &event_id, &r.slot).await? {
            Some(e) if e.accepted => Ok(Stage::Booked),
            _ => Ok(Stage::Pending),
        }
    }
}
pub fn router(auth: Auth) -> Router {
    Router::new()
        .route("/calendar/proposals", post(propose))
        .route("/calendar/bookings", post(confirm))
        .route("/calendar/disconnect", post(disconnect))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(axum::middleware::from_fn(crate::auth::private_response))
        .with_state(Arc::new(auth))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposalInput {
    host_url: String,
}
async fn propose(
    State(s): State<Arc<Auth>>,
    headers: HeaderMap,
    Json(input): Json<ProposalInput>,
) -> Response {
    if !origin_ok(&s, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    let account = match current(&s, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(service) = &s.connected else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "calendar_unavailable");
    };
    let host = match parse_host(&input.host_url, &s.website) {
        Ok(h) => h,
        Err(code) => return error(StatusCode::BAD_REQUEST, code),
    };
    match tokio::time::timeout(Duration::from_secs(20), service.propose(&account.id, &host)).await {
        Ok(Ok(Some(p))) => {
            Json(json!({"status":"slot_found","proposal":p,"reserved":false})).into_response()
        }
        Ok(Ok(None)) => Json(json!({"status":"no_common_slot","reserved":false})).into_response(),
        Ok(Err(code)) => error(StatusCode::CONFLICT, code),
        Err(_) => error(StatusCode::GATEWAY_TIMEOUT, "timeout"),
    }
}
pub fn parse_host(url: &str, website: &str) -> Result<String, &'static str> {
    let url = reqwest::Url::parse(url).map_err(|_| "invalid_agent_link")?;
    let base = reqwest::Url::parse(website).map_err(|_| "invalid_agent_link")?;
    if url.origin() != base.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid_agent_link");
    }
    let id = url
        .path()
        .strip_prefix("/book/")
        .filter(|s| crate::valid_tenant(s))
        .ok_or("invalid_agent_link")?;
    Ok(id.into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmInput {
    id: String,
}
async fn confirm(
    State(s): State<Arc<Auth>>,
    headers: HeaderMap,
    Json(input): Json<ConfirmInput>,
) -> Response {
    if !origin_ok(&s, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    let account = match current(&s, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(service) = &s.connected else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "calendar_unavailable");
    };
    match service.confirm(&account.id, &input.id).await {
        Ok(v) => Json(v).into_response(),
        Err(code) => error(StatusCode::CONFLICT, code),
    }
}
async fn disconnect(State(s): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    if !origin_ok(&s, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    let account = match current(&s, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(service) = &s.connected else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "calendar_unavailable");
    };
    match service.calendars.disconnect(&account.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}

#[cfg(test)]
pub(crate) mod tests;
