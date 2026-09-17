//! Read-only live adapter check. Prints counts, never calendar content or tokens.
use calendar::google_calendar::{Calendars, GoogleCalendar};
use std::sync::Arc;
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let account = std::env::var("HISTORY_TEST_ACCOUNT")?;
    let peer = std::env::var("HISTORY_TEST_PEER")?;
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let store = Arc::new(calendar::auth_store::DynamoAuthStore::new(
        aws_sdk_dynamodb::Client::new(&config),
        "calendar-production-auth".into(),
    ));
    let calendar = GoogleCalendar::new(
        store,
        aws_sdk_kms::Client::new(&config),
        "alias/calendar-production-google-tokens".into(),
        aws_sdk_secretsmanager::Client::new(&config),
        "calendar/production/google-oauth-client".into(),
        std::env::var("GOOGLE_OAUTH_CLIENT_ID")?,
    )?;
    let peer_email = calendar.email(&peer).await?;
    let (timezone, events) = calendar.history(&account, &peer_email).await?;
    println!(
        "PASS live Calendar history: {} usable observations, {} with peer; timezone {}",
        events.len(),
        events.iter().filter(|e| e.peer).count(),
        timezone
    );
    Ok(())
}
