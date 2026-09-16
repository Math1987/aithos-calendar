//! One JSON sink for application and SDK events; `target` identifies the source.
use a2a::A2AError;
use a2a_server::middleware::{CallContext, CallInterceptor, LoggingInterceptor};
use serde_json::Value;
use std::future::Future;
use tracing::Instrument;
use tracing_subscriber::EnvFilter;

pub fn init() -> Result<(), lambda_http::Error> {
    let filter = std::env::var("RUST_LOG")
        .unwrap_or_else(|_| "warn,calendar=info,a2a_client=info,a2a_server=info".into());
    tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(true)
        .with_span_list(false)
        .with_target(true)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::try_new(filter)?)
        .try_init()?;
    Ok(())
}

/// a2a-server-lf 0.4.4 exposes logging hooks but does not wire InterceptedHandler
/// into RequestHandler. Call the SDK hooks around our handler without serializing
/// request/response bodies. This adapter is specific to LoggingInterceptor,
/// whose hooks only inspect the method and result status (not payloads/headers).
/// Stream methods currently return unsupported errors; this does not trace a
/// future stream's lifetime if streaming support is added later.
pub(crate) async fn server_call<T>(
    method: &'static str,
    tenant: Option<&str>,
    trace_id: &str,
    call: impl Future<Output = Result<T, A2AError>>,
) -> Result<T, A2AError> {
    let span = tracing::info_span!("a2a_request", tenant, trace_id);
    async {
        let interceptor = LoggingInterceptor;
        let mut context = CallContext::new(method, Default::default());
        context.tenant = tenant.map(str::to_owned);
        interceptor.before(&mut context, &Value::Null).await?;
        let result = call.await;
        let status = result.as_ref().map(|_| Value::Null).map_err(Clone::clone);
        interceptor.after(&context, &status).await?;
        result
    }
    .instrument(span)
    .await
}
