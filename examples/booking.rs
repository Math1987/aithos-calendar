//! Controlled operator test: one submission, append-only local recovery journal.
//! Usage: booking submit JOURNAL HOST_URL GUEST_URL | booking poll JOURNAL
use calendar::{
    availability::{AvailabilityReader, GoogleHttpReader, first_host_slot},
    booking::{AnakinBooking, Attendee, BookingProvider, BookingRequest, JobStatus},
    booking_page::{BookingPages, GoogleBookingPages},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
};
type Error = Box<dyn std::error::Error>;
fn record(file: &mut File, value: Value) -> Result<(), Error> {
    writeln!(file, "{}", value)?;
    file.sync_all()?;
    Ok(())
}
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Error> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let client = AnakinBooking::from_env()?;
    match args.first().map(String::as_str) {
        Some("submit") if args.len()==4=> {
            let attendee=Attendee::from_env()?;
            let pages=GoogleBookingPages::new()?;
            let host=pages.resolve(&args[2]).await.map_err(|e|e.code())?;
            let peer=pages.resolve(&args[3]).await.map_err(|e|e.code())?;
            if host==peer {return Err("Use two different booking pages".into());}
            let reader=GoogleHttpReader::new()?;
            let start:DateTime<Utc>=std::time::SystemTime::now().into();
            let end=start+Duration::days(30);
            let (a,b)=tokio::try_join!(reader.read(&host,start,end),reader.read(&peer,start,end))?;
            let slot=first_host_slot(&a,&b).ok_or("No common offered host slot")?;
            // Re-read both pages immediately before the single submission.
            let (a,b)=tokio::try_join!(reader.read(&host,start,end),reader.read(&peer,start,end))?;
            if !a.slots.contains(&slot) || first_host_slot(&calendar::availability::Schedule{slots:vec![slot.clone()],..a.clone()},&b).is_none() {
                return Err("Selected slot is no longer available; no booking submitted".into());
            }
            let request=BookingRequest::from_schedule(&a,slot.clone(),attendee)?;
            // An existing journal is never overwritten or resubmitted.
            let mut file=OpenOptions::new().write(true).create_new(true).mode(0o600).open(&args[1])?;
            file.try_lock()?;
            record(&mut file,json!({"state":"submitting","host":host.url,"peer":peer.url,"slot":slot}))?;
            match client.submit(&request).await {
                Ok(job_id)=> {
                    record(&mut file,json!({"state":"submitted","job_id":job_id}))?;
                    println!("Booking submitted once. Job ID: {job_id}. Use poll with the same journal; do not submit again.");
                }
                Err(error)=> {
                    record(&mut file,json!({"state":"submission_error","error":error.to_string()}))?;
                    return Err(error.into());
                }
            }
        }
        Some("poll") if args.len()==2=> {
            let mut file=OpenOptions::new().read(true).append(true).open(&args[1])?;
            file.try_lock()?;
            let mut text=String::new();file.read_to_string(&mut text)?;
            let records:Vec<Value>=text.lines().map(serde_json::from_str).collect::<Result<_,_>>()?;
            if records.iter().any(|v|v["state"]=="completed_unverified" || v["state"]=="failed") {
                println!("This job already reached a terminal state; inspect the saved journal.");return Ok(());
            }
            let job_id=records.iter().find_map(|v|v["job_id"].as_str()).ok_or("No persisted job ID. Reconcile with Anakin; do not resubmit this journal.")?;
            match client.status(job_id).await? {
                JobStatus::Processing{retry_after_ms}=>println!("Still processing; poll again after {} seconds.",retry_after_ms.div_ceil(1000)),
                JobStatus::Failed=>{record(&mut file,json!({"state":"failed"}))?;println!("Anakin reported failure. Check the calendar before any new attempt.");},
                JobStatus::CompletedUnverified(data)=>{
                    record(&mut file,json!({"state":"completed_unverified","data":data}))?;
                    println!("Job completed; result saved in the private journal. Verify the booking confirmation and calendar before claiming success.");
                }
            }
        }
        _=>return Err("Usage: cargo run --example booking -- submit JOURNAL HOST_URL GUEST_URL | poll JOURNAL".into()),
    }
    Ok(())
}
