use lambda_http::{Body, Error, Request, Response, run, service_fn};

async fn health(_request: Request) -> Result<Response<Body>, Error> {
    Ok(Response::builder()
        .status(200)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Body::Text(
            r#"{"status":"ok","service":"calendar"}"#.to_owned(),
        ))?)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    run(service_fn(health)).await
}
