use a2a::*;
use a2a_server::{RequestHandler, ServiceParams};
use futures::stream::BoxStream;

use crate::{
    agents,
    discovery::{PeerDirectory, PeerError},
    scheduling::{Availability, Operation, first_common_slot},
};
use serde_json::{Value, json};
use std::time::Duration;

/// Immediate messages only: no task store or work left running after a response.
pub struct CalendarHandler {
    pub directory: PeerDirectory,
}

fn structured_reply(text: &str, data: Value) -> SendMessageResponse {
    SendMessageResponse::Message(Message::new(
        Role::Agent,
        vec![Part::text(text), Part::data(data)],
    ))
}

fn unsupported(tenant: Option<&str>) -> A2AError {
    agents::find(tenant).err().unwrap_or_else(|| {
        A2AError::unsupported_operation(
            "This mock agent only supports immediate SendMessage responses",
        )
    })
}

#[async_trait::async_trait]
impl RequestHandler for CalendarHandler {
    async fn send_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<SendMessageResponse, A2AError> {
        let agent = agents::find(req.tenant.as_deref())?;
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
            return Err(unsupported(req.tenant.as_deref()));
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
        let trace_id = req
            .metadata
            .as_ref()
            .and_then(|m| m.get("calendarTraceId"))
            .and_then(Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .map(|id| id.to_string())
            .unwrap_or_else(new_message_id);
        let operation_name = match &operation {
            Operation::GetAvailability => "get_availability",
            Operation::FindCommonSlot { .. } => "find_common_slot",
        };
        eprintln!(
            "{}",
            json!({"event":"operation_received", "tenant":agent.id,
            "operation":operation_name, "trace_id":trace_id})
        );
        match operation {
            Operation::GetAvailability => {
                let availability = Availability {
                    status: "availability".into(),
                    agent: agent.identifier(),
                    slots: agent.availability(),
                    mock: true,
                    trace_id,
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
                    self.directory.availability(&peer, &trace_id, agent.id),
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
                eprintln!(
                    "{}",
                    json!({"event":"negotiation_completed", "tenant":agent.id,
                    "trace_id":trace_id, "status":data["status"], "code":data.get("code")})
                );
                Ok(structured_reply(text, data))
            }
        }
    }

    async fn send_streaming_message(
        &self,
        _params: &ServiceParams,
        req: SendMessageRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn get_task(
        &self,
        _params: &ServiceParams,
        req: GetTaskRequest,
    ) -> Result<Task, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn list_tasks(
        &self,
        _params: &ServiceParams,
        req: ListTasksRequest,
    ) -> Result<ListTasksResponse, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn cancel_task(
        &self,
        _params: &ServiceParams,
        req: CancelTaskRequest,
    ) -> Result<Task, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn subscribe_to_task(
        &self,
        _params: &ServiceParams,
        req: SubscribeToTaskRequest,
    ) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn create_push_config(
        &self,
        _params: &ServiceParams,
        req: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn get_push_config(
        &self,
        _params: &ServiceParams,
        req: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn list_push_configs(
        &self,
        _params: &ServiceParams,
        req: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn delete_push_config(
        &self,
        _params: &ServiceParams,
        req: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<(), A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }

    async fn get_extended_agent_card(
        &self,
        _params: &ServiceParams,
        req: GetExtendedAgentCardRequest,
    ) -> Result<AgentCard, A2AError> {
        Err(unsupported(req.tenant.as_deref()))
    }
}
