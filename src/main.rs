use lambda_http::Error;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let sink = std::sync::Arc::new(calendar::public_logs::Sink::default());
    calendar::logging::init_with(Some(sink.clone()))?;
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
        // Local run: in-memory fixtures, ephemeral operator key.
        let lab = std::sync::Arc::new(calendar::lab::Lab::from_env());
        let trust = calendar::trust::from_env(&base_url, None, &lab).await?;
        let operator = calendar::trust::operator_from_env(&base_url, None).await?;
        let store = std::sync::Arc::new(
            calendar::storage::MemoryStore::fixtures(&base_url, trust.as_ref()).await,
        );
        calendar::build(
            calendar::Config::new(&base_url, trust, store)
                .with_operator(operator)
                .with_lab(lab)
                .with_public_logs(calendar::public_logs::PublicLogs::memory(sink))
                .with_catalog(&catalog_url),
        )?
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
        let kms = aws_sdk_kms::Client::new(&config);
        let public_logs = calendar::public_logs::PublicLogs {
            sink: sink.clone(),
            store: match std::env::var("PUBLIC_LOGS_TABLE") {
                Ok(table) => std::sync::Arc::new(calendar::public_logs::DynamoPublicStore::new(
                    aws_sdk_dynamodb::Client::new(&config),
                    table,
                    std::env::var("PUBLIC_LOGS_TTL_SECONDS")
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(86_400),
                )),
                Err(_) => std::sync::Arc::new(calendar::public_logs::MemoryPublicStore::default()),
            },
        };
        let lab = std::sync::Arc::new(calendar::lab::Lab::from_env());
        let trust = calendar::trust::from_env(&base_url, Some(&kms), &lab).await?;
        let operator = calendar::trust::operator_from_env(&base_url, Some(&kms)).await?;
        let policies = calendar::trust::Policies::from_env()?;
        let trusted_guarantors: Vec<String> = std::env::var("TRUSTED_GUARANTORS")
            .ok()
            .map(|v| {
                v.split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_else(|| vec![trust.identity().to_owned()]);
        let private_store: std::sync::Arc<dyn calendar::auth_store::AuthStore> =
            std::sync::Arc::new(calendar::auth_store::DynamoAuthStore::new(
                aws_sdk_dynamodb::Client::new(&config),
                std::env::var("AUTH_TABLE")?,
            ));
        let booking_store = std::sync::Arc::new(calendar::booking_store::DynamoBookingStore::new(
            aws_sdk_dynamodb::Client::new(&config),
            std::env::var("BOOKINGS_TABLE")?,
        ));
        let agent_state: std::sync::Arc<dyn calendar::agent::state::StateStore> =
            std::sync::Arc::new(calendar::agent::state::DynamoState {
                client: aws_sdk_dynamodb::Client::new(&config),
                table: std::env::var("AGENT_STATE_TABLE")?,
            });
        let jobs = std::sync::Arc::new(calendar::agent::jobs::Jobs {
            store: agent_state.clone(),
            queue: std::sync::Arc::new(calendar::agent::jobs::SqsQueue {
                client: aws_sdk_sqs::Client::new(&config),
                url: std::env::var("AGENT_QUEUE_URL")?,
            }),
        });
        let website = std::env::var("CALENDAR_WEBSITE_URL")?;
        let connected = std::sync::Arc::new(calendar::connected::Connected {
            jobs: Some(jobs.clone()),
            calendars: std::sync::Arc::new(calendar::google_calendar::GoogleCalendar::new(
                private_store.clone(),
                aws_sdk_kms::Client::new(&config),
                std::env::var("GOOGLE_TOKEN_KMS_KEY_ID")?,
                aws_sdk_secretsmanager::Client::new(&config),
                std::env::var("GOOGLE_OAUTH_CLIENT_SECRET_ID")?,
                std::env::var("GOOGLE_OAUTH_CLIENT_ID")?,
            )?),
            store: private_store.clone(),
            bookings: booking_store.clone(),
            agents: store.clone(),
            directory: std::sync::Arc::new(
                calendar::discovery::PeerDirectory::new(
                    &base_url,
                    &catalog_url,
                    trusted_guarantors.clone(),
                )?
                .with_store(store.clone()),
            ),
            policies,
            website: website.clone(),
        });
        if std::env::var("CALENDAR_WORKER").as_deref() == Ok("true") {
            let model = std::sync::Arc::new(calendar::agent::model::Model::new(
                &config,
                calendar::agent::budget::Budget { store: agent_state },
            ));
            lambda_runtime::run(lambda_runtime::service_fn(
                move |event: lambda_runtime::LambdaEvent<serde_json::Value>| {
                    let jobs = jobs.clone();
                    let service = connected.clone();
                    let model = model.clone();
                    let public_logs = public_logs.clone();
                    async move {
                        let outcome = worker(event, &jobs, &service, &model).await;
                        public_logs.flush().await;
                        outcome
                    }
                },
            ))
            .await?;
            return Ok(());
        }
        let app = calendar::build(
            calendar::Config::new(&base_url, trust.clone(), store.clone())
                .with_operator(operator)
                .with_lab(lab)
                .with_public_logs(public_logs.clone())
                .with_catalog(&catalog_url)
                .with_website(&website)
                .with_trusted_guarantors(trusted_guarantors)
                .with_policies(policies)
                .with_reader(Some(std::sync::Arc::new(
                    calendar::availability::GoogleHttpReader::new()?,
                )))
                .with_connected(Some(connected.clone())),
        )?;
        let auth = calendar::auth::Auth {
            store: private_store,
            connected: Some(connected),
            agents: store.clone(),
            provider: std::sync::Arc::new(calendar::google_identity::GoogleIdentity::new(
                std::env::var("GOOGLE_OAUTH_CLIENT_ID")?,
                std::env::var("GOOGLE_OAUTH_REDIRECT_URI")?,
                std::env::var("GOOGLE_OAUTH_CLIENT_SECRET_ID")?,
                aws_sdk_secretsmanager::Client::new(&config),
            )?),
            trust,
            base: base_url.clone(),
            website,
            access: calendar::auth::Access::from_env(),
        };
        let app = app
            .merge(calendar::auth::router(auth.clone()))
            .merge(calendar::connected::router(auth.clone()))
            .merge(calendar::agent::jobs::router(auth));
        let app = if let (Ok(table), Ok(secret_id)) = (
            std::env::var("BOOKINGS_TABLE"),
            std::env::var("ANAKIN_SECRET_ID"),
        ) {
            app.merge(calendar::booking_api::router(
                calendar::booking_api::Bookings {
                    agents: store,
                    store: std::sync::Arc::new(calendar::booking_store::DynamoBookingStore::new(
                        aws_sdk_dynamodb::Client::new(&config),
                        table,
                    )),
                    reader: std::sync::Arc::new(calendar::availability::GoogleHttpReader::new()?),
                    provider: std::sync::Arc::new(calendar::booking::SecretsBooking::new(
                        aws_sdk_secretsmanager::Client::new(&config),
                        secret_id,
                    )),
                },
            ))
        } else {
            app
        };
        // Every route, including the ones merged above, flushes the public
        // log feed before the response leaves the Lambda.
        app.layer(axum::middleware::from_fn_with_state(
            public_logs,
            calendar::public_logs::flush_after,
        ))
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

/// One SQS batch (or an operator diagnostic) for the autonomous worker.
async fn worker(
    event: lambda_runtime::LambdaEvent<serde_json::Value>,
    jobs: &calendar::agent::jobs::Jobs,
    service: &std::sync::Arc<calendar::connected::Connected>,
    model: &std::sync::Arc<calendar::agent::model::Model>,
) -> Result<serde_json::Value, lambda_runtime::Error> {
    // Operator-only Lambda invocation, synthetic data, same global budget.
    // There is no HTTP route for this diagnostic.
    if event.payload["operation"] == "verify_model" {
        let result = model
            .analyze(serde_json::json!({"timezone":"Europe/Paris","observations":[]}))
            .await;
        return Ok(match result {
            Ok(value) => serde_json::json!({"status":"model_available","analysis":value}),
            Err(code) => serde_json::json!({"status":"deterministic_fallback","code":code}),
        });
    }
    let mut failures = vec![];
    if let Some(records) = event.payload["Records"].as_array() {
        for record in records {
            let result = match record["body"].as_str() {
                Some(id) => jobs.process(id, service, Some(model)).await,
                None => Err("invalid_task"),
            };
            if let Err(code) = result {
                tracing::warn!(event = "agent_job_retry", code);
                failures.push(serde_json::json!({"itemIdentifier":record["messageId"]}));
            }
        }
    }
    Ok(serde_json::json!({"batchItemFailures":failures}))
}
