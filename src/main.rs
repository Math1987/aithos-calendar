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
    let app = calendar::app_with_catalog(&base_url, &catalog_url)?;
    if let Some(address) = listen {
        let listener = tokio::net::TcpListener::bind(&address).await?;
        tracing::info!(event = "listening", %address, "Calendar listening");
        axum::serve(listener, app).await?;
    } else {
        lambda_http::run(app).await?;
    }
    Ok(())
}
