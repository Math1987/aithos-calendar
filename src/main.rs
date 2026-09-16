use lambda_http::Error;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    calendar::logging::init()?;
    let listen = std::env::var("CALENDAR_LISTEN").ok();
    let base_url = std::env::var("CALENDAR_PUBLIC_URL").or_else(|error| {
        listen
            .as_ref()
            .map(|address| format!("http://{address}"))
            .ok_or(error)
    })?;
    let catalog_url = std::env::var("CATALOG_URL").unwrap_or_else(|_| {
        format!(
            "{}/.well-known/ai-catalog.json",
            base_url.trim_end_matches('/')
        )
    });
    let app = if listen.is_some() {
        calendar::app_with_catalog(&base_url, &catalog_url)?
    } else {
        let table = std::env::var("AGENTS_TABLE")?;
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .timeout_config(
                aws_config::timeout::TimeoutConfig::builder()
                    .operation_timeout(std::time::Duration::from_secs(2))
                    .build(),
            )
            .retry_config(aws_config::retry::RetryConfig::standard().with_max_attempts(2))
            .load()
            .await;
        let store = std::sync::Arc::new(calendar::storage::DynamoStore::new(
            aws_sdk_dynamodb::Client::new(&config),
            table,
        ));
        let registry = calendar::registry::Registry::new(&std::env::var("REGISTRY_ORIGIN")?)?;
        calendar::app_with_store(
            &base_url,
            &catalog_url,
            store,
            Some(registry),
            std::env::var("CALENDAR_WEBSITE_URL")?,
        )?
    };
    if let Some(address) = listen {
        let listener = tokio::net::TcpListener::bind(&address).await?;
        tracing::info!(event = "listening", %address, "Calendar listening");
        axum::serve(listener, app).await?;
    } else {
        lambda_http::run(app).await?;
    }
    Ok(())
}
