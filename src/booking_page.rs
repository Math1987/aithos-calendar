//! Public Google booking-page identity and bounded HTML access.
use async_trait::async_trait;
use reqwest::{Client, Url};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BookingPage {
    pub url: String,
}
impl BookingPage {
    pub fn agent_id(&self) -> String {
        // The canonical page URL is the unique key: no secondary index or lock row.
        format!(
            "{:x}",
            Sha256::digest(format!("calendar:google-booking-page:v1:{}", self.url))
        )
    }
}
#[derive(Debug, Clone, Copy)]
pub enum PageError {
    Invalid,
    Unavailable,
    Unrecognized,
}
impl PageError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_booking_page_url",
            Self::Unavailable => "google_page_unavailable",
            Self::Unrecognized => "google_page_not_recognized",
        }
    }
}
#[async_trait]
pub trait BookingPages: Send + Sync {
    async fn resolve(&self, input: &str) -> Result<BookingPage, PageError>;
    async fn validate(&self, page: &BookingPage) -> Result<(), PageError>;
}

pub struct GoogleBookingPages {
    http: Client,
}
impl GoogleBookingPages {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(3))
                .build()?,
        })
    }
}

// Every network destination, including every redirect, goes through this allowlist.
// Queries/fragments are presentation/tracking, never part of the schedule identity.
pub(crate) fn normalized(input: &str) -> Result<Url, PageError> {
    let input = input.trim();
    if input.len() > 2048 || input.chars().any(|c| c.is_control() || c == '\\') {
        return Err(PageError::Invalid);
    }
    let mut url = Url::parse(input).map_err(|_| PageError::Invalid)?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(PageError::Invalid);
    }
    let parts: Vec<_> = url
        .path()
        .trim_end_matches('/')
        .split('/')
        .skip(1)
        .collect();
    let token = |id: &str| {
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    let path = match (url.host_str(), parts.as_slice()) {
        (Some("calendar.app.google"), [id]) if token(id) => format!("/{id}"),
        (
            Some("calendar.google.com"),
            ["appointments", "schedules", id] | ["calendar", "appointments", "schedules", id],
        ) if token(id) => format!("/calendar/appointments/schedules/{id}"),
        (
            Some("calendar.google.com"),
            ["calendar", "u", account, "appointments", "schedules", id],
        ) if !account.is_empty() && account.bytes().all(|b| b.is_ascii_digit()) && token(id) => {
            format!("/calendar/appointments/schedules/{id}")
        }
        _ => return Err(PageError::Invalid),
    };
    url.set_path(&path);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}
fn redirect(current: &Url, location: &str) -> Result<Url, PageError> {
    normalized(
        current
            .join(location)
            .map_err(|_| PageError::Invalid)?
            .as_str(),
    )
}

#[async_trait]
impl BookingPages for GoogleBookingPages {
    async fn resolve(&self, input: &str) -> Result<BookingPage, PageError> {
        let mut url = normalized(input)?;
        // Only short links need network resolution; existing long URLs work offline.
        for _ in 0..4 {
            if url.host_str() == Some("calendar.google.com") {
                return Ok(BookingPage {
                    url: url.to_string(),
                });
            }
            let response = self
                .http
                .get(url.clone())
                .send()
                .await
                .map_err(|_| PageError::Unavailable)?;
            if response.status().is_server_error() || response.status().as_u16() == 429 {
                return Err(PageError::Unavailable);
            }
            if !response.status().is_redirection() {
                return Err(PageError::Invalid);
            }
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or(PageError::Invalid)?;
            url = redirect(&url, location)?;
        }
        Err(PageError::Invalid)
    }
    async fn validate(&self, page: &BookingPage) -> Result<(), PageError> {
        recognized_page(&self.html(page).await?, page)
    }
}

impl GoogleBookingPages {
    pub(crate) async fn html(&self, page: &BookingPage) -> Result<String, PageError> {
        let mut url = normalized(&page.url)?;
        if url.host_str() != Some("calendar.google.com") {
            return Err(PageError::Invalid);
        }
        for _ in 0..4 {
            let mut response = self
                .http
                .get(url.clone())
                .header("accept-language", "en")
                .send()
                .await
                .map_err(|_| PageError::Unavailable)?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|v| v.to_str().ok())
                    .ok_or(PageError::Invalid)?;
                let next = redirect(&url, location)?;
                // A different schedule cannot silently take the requested identity.
                if next.as_str() != page.url {
                    return Err(PageError::Unrecognized);
                }
                url = next;
                continue;
            }
            if !response.status().is_success() {
                return Err(
                    if response.status().is_client_error() && response.status().as_u16() != 429 {
                        PageError::Invalid
                    } else {
                        PageError::Unavailable
                    },
                );
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| PageError::Unavailable)? {
                if bytes.len() + chunk.len() > 512 * 1024 {
                    return Err(PageError::Unrecognized);
                }
                bytes.extend_from_slice(&chunk);
            }
            let html = std::str::from_utf8(&bytes).map_err(|_| PageError::Unrecognized)?;
            return Ok(html.to_owned());
        }
        Err(PageError::Unrecognized)
    }
}

fn recognized_page(html: &str, page: &BookingPage) -> Result<(), PageError> {
    // Small, fail-closed adapter for Google's current HTML shell, not a public API.
    // Invalid schedule IDs also return HTTP 200 and a canonical link, but omit
    // the booking-page Open Graph image. Do not accept that generic shell.
    use std::sync::LazyLock;
    static TAG: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"(?is)<(?:link|meta)\b[^>]*>").unwrap());
    static ATTR: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?i)([\w:-]+)\s*=\s*["']([^"']*)["']"#).unwrap());
    let mut canonical = false;
    let mut image = false;
    for tag in TAG.find_iter(html) {
        let attrs: std::collections::HashMap<_, _> = ATTR
            .captures_iter(tag.as_str())
            .map(|c| (c[1].to_ascii_lowercase(), c[2].to_owned()))
            .collect();
        if attrs
            .get("rel")
            .is_some_and(|v| v.eq_ignore_ascii_case("canonical"))
        {
            canonical |= attrs
                .get("href")
                .and_then(|v| normalized(v).ok())
                .is_some_and(|v| v.as_str() == page.url);
        }
        image |= attrs
            .get("property")
            .is_some_and(|v| v.eq_ignore_ascii_case("og:image"))
            && attrs
                .get("content")
                .is_some_and(|v| v.starts_with("https://"));
    }
    if canonical && image && html.contains("appointments.AppointmentsInitialData") {
        Ok(())
    } else {
        Err(PageError::Unrecognized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_share_identity_and_untrusted_destinations_are_rejected() {
        let expected = "https://calendar.google.com/calendar/appointments/schedules/Ab_cd-123";
        for url in [
            expected,
            "https://calendar.google.com/appointments/schedules/Ab_cd-123?gv=true#fragment",
            "https://calendar.google.com/calendar/u/2/appointments/schedules/Ab_cd-123/",
        ] {
            assert_eq!(normalized(url).unwrap().as_str(), expected);
        }
        let short = normalized("https://calendar.app.google/abc123").unwrap();
        assert_eq!(redirect(&short, expected).unwrap().as_str(), expected);
        for bad in [
            "http://calendar.google.com/appointments/schedules/id",
            "https://calendar.google.com.evil.test/appointments/schedules/id",
            "https://calendar.google.com@127.0.0.1/appointments/schedules/id",
            "https://calendar.google.com:444/appointments/schedules/id",
            "https://calendar.google.com/calendar/ical/example",
            "https://calendar.google.com/appointments/schedules/%61bc",
            "https://calendar.app.google/a/b",
            "https://127.0.0.1/private",
            "https://accounts.google.com/login",
        ] {
            assert!(normalized(bad).is_err(), "{bad}");
            assert!(redirect(&short, bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn generic_google_shell_or_mismatched_page_is_not_validated() {
        let page = BookingPage {
            url: "https://calendar.google.com/calendar/appointments/schedules/example".into(),
        };
        let html = format!(
            "<link href='{}' rel='canonical'><meta content='https://example.test/photo' property='og:image'>appointments.AppointmentsInitialData",
            page.url
        );
        assert!(recognized_page(&html, &page).is_ok());
        assert!(recognized_page(&html.replace("og:image", "other"), &page).is_err());
        assert!(
            recognized_page(
                &html.replace("/schedules/example", "/schedules/another"),
                &page
            )
            .is_err()
        );
        assert!(recognized_page("<title>(No title)</title>", &page).is_err());
    }
}
