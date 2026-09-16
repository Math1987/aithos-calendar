use a2a::*;
use a2a_server::{RequestHandler, ServiceParams};
use futures::stream::BoxStream;

use crate::agents;

/// Immediate messages only: no task store or work left running after a response.
pub struct GreetingHandler;

fn unsupported(tenant: Option<&str>) -> A2AError {
    agents::find(tenant).err().unwrap_or_else(|| {
        A2AError::unsupported_operation("This mock agent only supports SendMessage")
    })
}

#[async_trait::async_trait]
impl RequestHandler for GreetingHandler {
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
        if req
            .message
            .parts
            .iter()
            .any(|part| part.as_text().is_none())
        {
            return Err(A2AError::content_type_not_supported());
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
        Ok(SendMessageResponse::Message(Message::new(
            Role::Agent,
            vec![Part::text(format!("Hello from {}", agent.name))],
        )))
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
