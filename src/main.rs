use lambda_http::Error;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let listen = std::env::var("CALENDAR_LISTEN").ok();
    let base_url = std::env::var("CALENDAR_PUBLIC_URL").or_else(|error| {
        listen
            .as_ref()
            .map(|address| format!("http://{address}"))
            .ok_or(error)
    })?;
    let app = calendar::app(&base_url);
    if let Some(address) = listen {
        let listener = tokio::net::TcpListener::bind(&address).await?;
        eprintln!("Calendar listening on {address}");
        axum::serve(listener, app).await?;
    } else {
        lambda_http::run(app).await?;
    }
    Ok(())
}
