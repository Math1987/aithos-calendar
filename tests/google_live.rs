//! Opt-in, read-only provider check. Never part of the deterministic CI suite.
use calendar::booking_page::{BookingPages, GoogleBookingPages};

#[tokio::test]
#[ignore = "requires GOOGLE_BOOKING_TEST_URL and network access"]
async fn live_google_short_and_long_links_share_identity() {
    let input = std::env::var("GOOGLE_BOOKING_TEST_URL").expect("set a public booking-page URL");
    let reader = GoogleBookingPages::new().unwrap();
    let page = reader.resolve(&input).await.unwrap();
    reader.validate(&page).await.unwrap();
    let alias = format!(
        "{}?gv=true#test",
        page.url
            .replace("/calendar/appointments/", "/calendar/u/0/appointments/")
    );
    let again = reader.resolve(&alias).await.unwrap();
    assert_eq!(page, again);
    assert_eq!(page.agent_id(), again.agent_id());
    let invalid = reader.resolve("https://calendar.google.com/calendar/appointments/schedules/this_page_does_not_exist_gate4").await.unwrap();
    assert!(reader.validate(&invalid).await.is_err());
}
