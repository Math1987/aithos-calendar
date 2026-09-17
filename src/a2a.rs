use crate::logging;
use a2a::jsonrpc::methods;
use a2a::*;
use a2a_server::{RequestHandler, ServiceParams};
use futures::stream::BoxStream;

use crate::{
    discovery::{PeerDirectory, PeerError},
    scheduling::{Availability, Operation, first_common_slot},
};
use serde_json::{Value, json};
use std::time::Duration;

/// Immediate messages only: no task store or work left running after a response.
pub struct CalendarHandler {
    pub directory: PeerDirectory,
    pub reader: Option<std::sync::Arc<dyn crate::availability::AvailabilityReader>>,
    pub store: std::sync::Arc<dyn crate::storage::AgentStore>,
}

fn structured_reply(text: &str, data: Value) -> SendMessageResponse {
    SendMessageResponse::Message(Message::new(
        Role::Agent,
        vec![Part::text(text), Part::data(data)],
    ))
}

impl CalendarHandler {
    async fn agent(&self, tenant: Option<&str>) -> Result<crate::agents::Agent, A2AError> {
        let id = tenant
            .filter(|id| crate::valid_tenant(id))
            .ok_or_else(|| A2AError::invalid_params("Unknown or missing tenant"))?;
        self.store
            .get(id)
            .await
            .map_err(|_| A2AError::internal("Agent storage unavailable"))?
            .filter(|r| r.published)
            .map(|r| r.agent)
            .ok_or_else(|| A2AError::invalid_params("Unknown or missing tenant"))
    }
    async fn unsupported_call<T>(
        &self,
        method: &'static str,
        tenant: Option<&str>,
    ) -> Result<T, A2AError> {
        logging::server_call(method, tenant, &new_message_id(), async {
            self.agent(tenant).await?;
            Err(A2AError::unsupported_operation(
                "This agent only supports immediate SendMessage responses",
            ))
        })
        .await
    }
    async fn read_live(
        &self,
        agent: &crate::agents::Agent,
        window: &crate::scheduling::Window,
    ) -> Result<crate::availability::Schedule, &'static str> {
        let reader = self.reader.as_ref().ok_or("availability_unavailable")?;
        let record = self
            .store
            .get(&agent.id)
            .await
            .map_err(|_| "storage_unavailable")?
            .ok_or("availability_unavailable")?;
        let url = record.booking_page_url.ok_or("availability_unavailable")?;
        tokio::time::timeout(
            Duration::from_secs(8),
            reader.read(
                &crate::booking_page::BookingPage { url },
                window.start,
                window.end,
            ),
        )
        .await
        .map_err(|_| "timeout")?
        .map_err(|_| "availability_unavailable")
    }
    async fn handle_live(
        &self,
        agent: &crate::agents::Agent,
        operation: Operation,
        trace_id: &str,
    ) -> Result<SendMessageResponse, A2AError> {
        use crate::scheduling::{ScheduleInfo, Window};
        let error = |code: &str, peer: Option<&str>| {
            structured_reply(
                "Availability could not be checked; nothing was booked",
                json!({"status":"error", "code":code, "organizer":agent.identifier(), "peer":peer,
                "mock":false,"reserved":false,"trace_id":trace_id}),
            )
        };
        match operation {
            Operation::GetAvailability { window } => {
                let window = window.unwrap_or_else(Window::next_month);
                if !window.valid() {
                    return Err(A2AError::invalid_params("Invalid availability window"));
                }
                let schedule = match self.read_live(agent, &window).await {
                    Ok(s) => s,
                    Err(code) => return Ok(error(code, None)),
                };
                let availability = Availability {
                    status: "availability".into(),
                    agent: agent.identifier(),
                    slots: schedule.slots.clone(),
                    mock: false,
                    trace_id: trace_id.into(),
                    schedule: Some(ScheduleInfo::from(&schedule)),
                };
                Ok(structured_reply(
                    "Offered public booking slots; no booking made",
                    serde_json::to_value(availability).unwrap(),
                ))
            }
            Operation::FindCommonSlot {
                peer,
                duration_minutes,
            } => {
                if peer.is_empty() || peer.len() > 256 || peer == agent.identifier() {
                    return Err(A2AError::invalid_params("Provide another peer identifier"));
                }
                let window = Window::next_month();
                // Both reads run concurrently. The peer read still goes through discovery and A2A.
                let outcome = tokio::time::timeout(Duration::from_secs(15), async {
                    let (host, peer_result) = tokio::join!(
                        self.read_live(agent, &window),
                        self.directory
                            .availability(&peer, trace_id, &agent.id, Some(&window))
                    );
                    Ok::<_, &'static str>((host?, peer_result.map_err(|e| e.code())?))
                })
                .await
                .unwrap_or(Err("timeout"));
                let (host, peer_availability) = match outcome {
                    Ok(value) => value,
                    Err(code) => return Ok(error(code, Some(&peer))),
                };
                if duration_minutes.is_some_and(|d| d != host.duration_minutes) {
                    return Err(A2AError::invalid_params(
                        "duration_minutes must match the host appointment duration; omit it to use the current duration",
                    ));
                }
                let Some(visitor) = peer_availability.into_schedule() else {
                    return Ok(error("invalid_peer_response", Some(&peer)));
                };
                let slot = match crate::availability::first_host_slot_from_tomorrow(
                    &host,
                    &visitor,
                    window.start,
                ) {
                    Ok(slot) => slot,
                    Err(_) => return Ok(error("availability_unavailable", Some(&peer))),
                };
                let status = if slot.is_some() {
                    "slot_found"
                } else {
                    "no_common_slot"
                };
                tracing::info!(event = "negotiation_completed", status, mock = false);
                Ok(structured_reply(
                    if slot.is_some() {
                        "A real common host slot was found; nothing was booked"
                    } else {
                        "No common offered host slot from tomorrow within the next 30 days"
                    },
                    json!({"status":status,"organizer":agent.identifier(),"peer":peer,"slot":slot,
                        "duration_minutes":host.duration_minutes,"schedule":ScheduleInfo::from(&host),
                        "mock":false,"reserved":false,"trace_id":trace_id}),
                ))
            }
        }
    }
    async fn handle_message(
        &self,
        req: SendMessageRequest,
        trace_id: &str,
    ) -> Result<SendMessageResponse, A2AError> {
        let agent = self.agent(req.tenant.as_deref()).await?;
        if req.message.role != Role::User
            || req.message.message_id.is_empty()
            || req.message.parts.is_empty()
        {
            return Err(A2AError::invalid_params(
                "A user message with messageId and parts is required",
            ));
        }
        if req.message.task_id.is_some()
            || req.message.context_id.is_some()
            || req
                .configuration
                .as_ref()
                .is_some_and(|c| c.task_push_notification_config.is_some())
        {
            return Err(A2AError::unsupported_operation(
                "Stateful interactions are not supported",
            ));
        }
        if req
            .message
            .parts
            .iter()
            .all(|part| part.as_text().is_some())
        {
            return Ok(SendMessageResponse::Message(Message::new(
                Role::Agent,
                vec![Part::text(format!("Hello from {}", agent.name))],
            )));
        }
        if agent.google_account {
            return Err(A2AError::unsupported_operation(
                "Calendar access is not enabled for this account yet",
            ));
        }
        let [
            Part {
                content: PartContent::Data(data),
                ..
            },
        ] = req.message.parts.as_slice()
        else {
            return Err(A2AError::content_type_not_supported());
        };
        let operation: Operation = serde_json::from_value(data.clone()).map_err(|_| {
            A2AError::invalid_params(
                "Expected get_availability or find_common_slot with peer and duration_minutes",
            )
        })?;
        let operation_name = match &operation {
            Operation::GetAvailability { .. } => "get_availability",
            Operation::FindCommonSlot { .. } => "find_common_slot",
        };
        tracing::info!(event = "operation_received", operation = operation_name);
        if agent.live {
            return self.handle_live(&agent, operation, trace_id).await;
        }
        match operation {
            Operation::GetAvailability { window } => {
                if window.is_some() {
                    return Err(A2AError::invalid_params(
                        "Mock agent cannot supply real availability",
                    ));
                }
                let availability = Availability {
                    status: "availability".into(),
                    agent: agent.identifier(),
                    slots: agent.availability(),
                    mock: true,
                    schedule: None,
                    trace_id: trace_id.to_owned(),
                };
                Ok(structured_reply(
                    "Mock availability; no calendar was accessed",
                    serde_json::to_value(availability).expect("serializable availability"),
                ))
            }
            Operation::FindCommonSlot {
                peer,
                duration_minutes,
            } => {
                let duration_minutes = duration_minutes.ok_or_else(|| {
                    A2AError::invalid_params("Mock scheduling requires duration_minutes")
                })?;
                if !(1..=480).contains(&duration_minutes)
                    || peer.is_empty()
                    || peer.len() > 256
                    || peer == agent.identifier()
                {
                    return Err(A2AError::invalid_params(
                        "Provide another peer identifier and a duration of 1 to 480 minutes",
                    ));
                }
                let outcome = tokio::time::timeout(
                    Duration::from_secs(10),
                    self.directory
                        .availability(&peer, trace_id, &agent.id, None),
                )
                .await
                .unwrap_or(Err(PeerError::Timeout));
                let (text, data) = match outcome {
                    Ok(availability) => {
                        let slot = first_common_slot(
                            &agent.availability(),
                            &availability.slots,
                            duration_minutes,
                        );
                        let status = if slot.is_some() {
                            "slot_found"
                        } else {
                            "no_common_slot"
                        };
                        (
                            if slot.is_some() {
                                "A mock common slot was found; nothing was booked"
                            } else {
                                "No common slot fits the requested duration"
                            },
                            json!({"status":status, "organizer":agent.identifier(), "peer":peer,
                             "duration_minutes":duration_minutes, "slot":slot,
                             "mock":true, "reserved":false, "trace_id":trace_id}),
                        )
                    }
                    Err(error) => (
                        error.message(),
                        json!({"status":"error", "code":error.code(),
                        "organizer":agent.identifier(), "peer":peer, "mock":true,
                        "reserved":false, "trace_id":trace_id}),
                    ),
                };
                tracing::info!(
                    event = "negotiation_completed",
                    status = data["status"].as_str(),
                    code = data.get("code").and_then(serde_json::Value::as_str),
                );
                Ok(structured_reply(text, data))
            }
        }
    }
}

#[async_trait::async_trait]
impl RequestHandler for CalendarHandler {
    async fn send_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        let trace_id = req
            .metadata
            .as_ref()
            .and_then(|m| m.get("calendarTraceId"))
            .and_then(Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|id| id.to_string())
            .unwrap_or_else(new_message_id);
        let tenant = req.tenant.clone();
        logging::server_call(
            methods::SEND_MESSAGE,
            tenant.as_deref(),
            &trace_id,
            self.handle_message(req, &trace_id),
        )
        .await
    }

    async fn send_streaming_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.unsupported_call(methods::SEND_STREAMING_MESSAGE, req.tenant.as_deref())
            .await
    }

    async fn get_task(
        &self,
        _params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        self.unsupported_call(methods::GET_TASK, req.tenant.as_deref())
            .await
    }

    async fn list_tasks(
        &self,
        _params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        self.unsupported_call(methods::LIST_TASKS, req.tenant.as_deref())
            .await
    }

    async fn cancel_task(
        &self,
        _params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        self.unsupported_call(methods::CANCEL_TASK, req.tenant.as_deref())
            .await
    }

    async fn subscribe_to_task(
        &self,
        _params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.unsupported_call(methods::SUBSCRIBE_TO_TASK, req.tenant.as_deref())
            .await
    }

    async fn create_push_config(
        &self,
        _params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.unsupported_call(methods::CREATE_PUSH_CONFIG, req.tenant.as_deref())
            .await
    }

    async fn get_push_config(
        &self,
        _params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        self.unsupported_call(methods::GET_PUSH_CONFIG, req.tenant.as_deref())
            .await
    }

    async fn list_push_configs(
        &self,
        _params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        self.unsupported_call(methods::LIST_PUSH_CONFIGS, req.tenant.as_deref())
            .await
    }

    async fn delete_push_config(
        &self,
        _params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        self.unsupported_call(methods::DELETE_PUSH_CONFIG, req.tenant.as_deref())
            .await
    }

    async fn get_extended_agent_card(
        &self,
        _params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        self.unsupported_call(methods::GET_EXTENDED_AGENT_CARD, req.tenant.as_deref())
            .await
    }
}
