//! Controlled operator test: page-derived attendee and one durable submission.
use calendar::{
    availability::{AvailabilityReader, GoogleHttpReader, PageIdentity, first_host_slot},
    booking::{
        AnakinBooking, Attendee, AttendeeDetails, AttendeeField, BookingProvider, BookingRequest,
        JobStatus,
    },
    booking_page::{BookingPages, GoogleBookingPages},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{IsTerminal, Read, Write},
    os::unix::fs::OpenOptionsExt,
};
type Error = Box<dyn std::error::Error>;
fn record(file: &mut File, value: Value) -> Result<(), Error> {
    writeln!(file, "{}", value)?;
    file.sync_all()?;
    Ok(())
}
fn attendee(identity: &PageIdentity) -> Result<Attendee, Error> {
    let mut details = AttendeeDetails::default();
    for _ in 0..3 {
        match Attendee::from_page(identity, &details) {
            Ok(attendee) => return Ok(attendee),
            Err(missing) => {
                if !std::io::stdin().is_terminal() {
                    return Err(missing.into());
                }
                for field in missing.fields {
                    eprint!("Visitor {field}: ");
                    std::io::stderr().flush()?;
                    let mut value = String::new();
                    if std::io::stdin().read_line(&mut value)? == 0 {
                        return Err("No attendee details supplied".into());
                    }
                    let value = Some(value.trim().to_owned());
                    match field {
                        AttendeeField::FirstName => details.first_name = value,
                        AttendeeField::LastName => details.last_name = value,
                        AttendeeField::Email => details.email = value,
                    }
                }
            }
        }
    }
    Ok(Attendee::from_page(identity, &details)?)
}
async fn prepare_or_submit(args: &[String]) -> Result<(), Error> {
    let submit = args[0] == "submit";
    let offset = if submit { 2 } else { 1 };
    let client = if submit {
        Some(AnakinBooking::from_env()?)
    } else {
        None
    };
    let pages = GoogleBookingPages::new()?;
    let host = pages.resolve(&args[offset]).await.map_err(|e| e.code())?;
    let peer = pages
        .resolve(&args[offset + 1])
        .await
        .map_err(|e| e.code())?;
    if host == peer {
        return Err("Use two different booking pages".into());
    }
    let reader = GoogleHttpReader::new()?;
    let start: DateTime<Utc> = std::time::SystemTime::now().into();
    let end = start + Duration::days(30);
    let (a, b) = tokio::try_join!(
        reader.read(&host, start, end),
        reader.read(&peer, start, end)
    )?;
    let slot = first_host_slot(&a, &b).ok_or("No common offered host slot")?;
    // Only the visitor's contact is used for the host's booking.
    let visitor = attendee(&b.identity)?;
    if !submit {
        BookingRequest::from_schedule(&a, slot.clone(), visitor)?;
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "status":"ready", "slot":slot, "duration_minutes":a.duration_minutes,
                "attendee_source":"visitor_booking_page_or_requested_details",
                "reserved":false, "submitted":false
            }))?
        );
        return Ok(());
    }
    let original_identity = b.identity;
    // Re-read both pages after any prompts and immediately before submitting.
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let (a, b) = tokio::try_join!(reader.read(&host, now, end), reader.read(&peer, now, end))?;
    if b.identity != original_identity {
        return Err("Visitor page contact changed; no booking submitted".into());
    }
    if !a.slots.contains(&slot)
        || first_host_slot(
            &calendar::availability::Schedule {
                slots: vec![slot.clone()],
                ..a.clone()
            },
            &b,
        )
        .is_none()
    {
        return Err("Selected slot is no longer available; no booking submitted".into());
    }
    let request = BookingRequest::from_schedule(&a, slot.clone(), visitor)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args[1])?;
    file.try_lock()?;
    record(
        &mut file,
        json!({"state":"submitting","host":host.url,"peer":peer.url,"slot":slot}),
    )?;
    match client.unwrap().submit(&request).await {
        Ok(job_id) => {
            record(&mut file, json!({"state":"submitted","job_id":job_id}))?;
            println!(
                "Booking submitted once. Job ID: {job_id}. Use poll with the same journal; do not submit again."
            );
        }
        Err(error) => {
            record(
                &mut file,
                json!({"state":"submission_error","error":error.to_string()}),
            )?;
            return Err(error.into());
        }
    }
    Ok(())
}
async fn poll(path: &str) -> Result<(), Error> {
    let client = AnakinBooking::from_env()?;
    let mut file = OpenOptions::new().read(true).append(true).open(path)?;
    file.try_lock()?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    let records: Vec<Value> = text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    if records
        .iter()
        .any(|v| v["state"] == "completed_unverified" || v["state"] == "failed")
    {
        println!("This job already reached a terminal state; inspect the saved journal.");
        return Ok(());
    }
    let job_id = records
        .iter()
        .find_map(|v| v["job_id"].as_str())
        .ok_or("No persisted job ID. Reconcile with Anakin; do not resubmit this journal.")?;
    match client.status(job_id).await? {
        JobStatus::Processing { retry_after_ms } => println!(
            "Still processing; poll again after {} seconds.",
            retry_after_ms.div_ceil(1000)
        ),
        JobStatus::Failed => {
            record(&mut file, json!({"state":"failed"}))?;
            println!("Anakin reported failure. Check the calendar before any new attempt.");
        }
        JobStatus::CompletedUnverified(data) => {
            record(
                &mut file,
                json!({"state":"completed_unverified","data":data}),
            )?;
            println!(
                "Job completed; result saved in the private journal. Verify the booking confirmation and calendar before claiming success."
            );
        }
    }
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("prepare") if args.len() == 3 => prepare_or_submit(&args).await,
        Some("submit") if args.len() == 4 => prepare_or_submit(&args).await,
        Some("poll") if args.len() == 2 => poll(&args[1]).await,
        _ => Err("Usage: booking prepare HOST_URL GUEST_URL | submit JOURNAL HOST_URL GUEST_URL | poll JOURNAL".into()),
    }
}
