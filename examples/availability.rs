//! Read-only provider check. No Anakin call and no appointment creation.
use calendar::{
    availability::{AvailabilityReader, GoogleHttpReader, first_host_slot},
    booking::{Attendee, AttendeeDetails},
    booking_page::{BookingPages, GoogleBookingPages},
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err(
            "Usage: cargo run --example availability -- HOST_BOOKING_URL GUEST_BOOKING_URL".into(),
        );
    }
    let pages = GoogleBookingPages::new()?;
    let host = pages.resolve(&args[0]).await.map_err(|e| e.code())?;
    let peer = pages.resolve(&args[1]).await.map_err(|e| e.code())?;
    if host == peer {
        return Err("Use two different booking pages".into());
    }
    let reader = GoogleHttpReader::new()?;
    let start: DateTime<Utc> = std::time::SystemTime::now().into();
    let end = start + Duration::days(30);
    let (host, peer) = tokio::try_join!(
        reader.read(&host, start, end),
        reader.read(&peer, start, end)
    )?;
    let missing = Attendee::from_page(&peer.identity, &AttendeeDetails::default())
        .err()
        .map(|e| e.fields)
        .unwrap_or_default();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "host":{"title":host.title,"duration_minutes":host.duration_minutes,"timezone":host.timezone,"offered_slots":host.slots.len()},
            "peer":{"title":peer.title,"duration_minutes":peer.duration_minutes,"timezone":peer.timezone,"offered_slots":peer.slots.len()},
            "attendee_identity":{"source":"visitor_booking_page","ready":missing.is_empty(),"missing_fields":missing},
            "slot":first_host_slot(&host,&peer),"mock":false,"reserved":false,
            "coverage":"public_booking_pages_only","window_start":start,"window_end":end
        }))?
    );
    Ok(())
}
