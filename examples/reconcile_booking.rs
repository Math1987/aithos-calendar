//! Reconcile one saved unknown job; only the observed preselection rejection unlocks it.
//! No provider submission and no raw provider/attendee data in output.
use calendar::{
    booking::{AnakinBooking, BookingProvider, JobStatus},
    booking_store::{BookingStore, DynamoBookingStore, Stage},
};
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let id = std::env::args()
        .nth(1)
        .ok_or("Usage: reconcile_booking OPERATION_UUID")?;
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let client = aws_sdk_dynamodb::Client::new(&config);
    let store = DynamoBookingStore::new(client, "calendar-production-bookings".into());
    let mut record = store
        .get(&id)
        .await
        .map_err(|_| "Storage read failed")?
        .ok_or("Unknown operation")?;
    if record.stage == Stage::SlotUnavailable {
        println!("Already reconciled: {id}");
        return Ok(());
    }
    if record.stage != Stage::Unknown {
        return Err("Only unknown operations may be reconciled here".into());
    }
    let job = record
        .job_id
        .as_deref()
        .ok_or("No saved job; manual investigation required")?;
    let provider = AnakinBooking::from_env()?;
    if !matches!(provider.status(job).await?, JobStatus::SlotUnavailable) {
        return Err(
            "Provider has not reported the known preselection rejection; operation remains locked"
                .into(),
        );
    }
    let previous = record.revision;
    record.stage = Stage::SlotUnavailable;
    record.revision += 1;
    if !store
        .save(&record, previous, true)
        .await
        .map_err(|_| "Storage reconciliation failed")?
    {
        return Err("Operation changed concurrently; no reconciliation applied".into());
    }
    println!(
        "Reconciled {id}: slot_unavailable; only this operation's guards released. No booking submitted."
    );
    Ok(())
}
