//! Evidence-grounded private profiles. Only bounded scheduling preferences cross A2A.
use super::model::Model;
use crate::{
    auth_store::{AuthStore, Entry},
    google_calendar::Calendars,
    scheduling::Slot,
};
use chrono::{DateTime, Datelike, Timelike, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashSet, sync::Arc};
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Observation {
    pub id: String,
    pub title: String,
    pub description: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub peer: bool,
    pub organized: bool,
    pub series: Option<String>,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    #[serde(deserialize_with = "optional_integer")]
    pub preferred_weekday: Option<u32>,
    #[serde(deserialize_with = "optional_integer")]
    pub preferred_hour: Option<u32>,
    #[serde(deserialize_with = "crate::scheduling::whole_minutes")]
    pub duration_minutes: u16,
    pub allow_lunch: bool,
}
fn optional_integer<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<u32>, D::Error> {
    let value = Option::<f64>::deserialize(d)?;
    value
        .map(|n| {
            if n.is_finite() && n.fract() == 0.0 && (0.0..=u32::MAX as f64).contains(&n) {
                Ok(n as u32)
            } else {
                Err(serde::de::Error::custom("expected whole number"))
            }
        })
        .transpose()
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            preferred_weekday: None,
            preferred_hour: None,
            duration_minutes: 30,
            allow_lunch: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub preferences: Preferences,
    pub source: String,
    pub previous_meetings: Vec<Observation>,
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            preferences: Preferences::default(),
            source: "defaults".into(),
            previous_meetings: vec![],
        }
    }
}
pub fn key(account: &str, peer: &str) -> String {
    format!("preferences:{account}:{}", crate::booking_api::digest(peer))
}
pub async fn cached(store: &dyn AuthStore, account: &str, peer: &str) -> Profile {
    store
        .get(&key(account, peer), crate::auth::now())
        .await
        .ok()
        .flatten()
        .and_then(|r| serde_json::from_value(r.value).ok())
        .unwrap_or_default()
}
fn distinct_past<'a>(events: &'a [Observation], now: DateTime<Utc>) -> Vec<&'a Observation> {
    let mut seen = HashSet::new();
    events
        .iter()
        .filter(|e| {
            e.end < now
                && e.end > e.start
                && seen.insert(e.series.as_ref().unwrap_or(&e.id).clone())
        })
        .collect()
}
fn evidenced(
    p: &Preferences,
    events: &[Observation],
    tz: chrono_tz::Tz,
    now: DateTime<Utc>,
) -> bool {
    if ![30, 45, 60].contains(&p.duration_minutes)
        || p.preferred_weekday.is_some_and(|d| !(1..=5).contains(&d))
        || p.preferred_hour.is_some_and(|h| !(9..18).contains(&h))
    {
        return false;
    }
    let all = distinct_past(events, now);
    let relevant: Vec<_> = all.iter().copied().filter(|e| e.peer).collect();
    let evidence = if relevant.len() >= 3 { relevant } else { all };
    let enough =
        |test: &dyn Fn(&Observation) -> bool| evidence.iter().filter(|e| test(e)).count() >= 3;
    if let Some(d) = p.preferred_weekday {
        if !enough(&|e| e.start.with_timezone(&tz).weekday().number_from_monday() == d) {
            return false;
        }
    }
    if let Some(h) = p.preferred_hour {
        if !enough(&|e| e.start.with_timezone(&tz).hour() == h) {
            return false;
        }
    }
    if p.allow_lunch
        && !enough(&|e| {
            (12..14).contains(&e.start.with_timezone(&tz).hour()) && (e.organized || e.peer)
        })
    {
        return false;
    }
    if p.duration_minutes != 30
        && !enough(&|e| (e.end - e.start).num_minutes() == i64::from(p.duration_minutes))
    {
        return false;
    }
    true
}
pub async fn prepare(
    calendars: &dyn Calendars,
    store: &dyn AuthStore,
    model: Option<&Arc<Model>>,
    account: &str,
    peer: &str,
) -> Profile {
    let k = key(account, peer);
    if let Ok(Some(row)) = store.get(&k, crate::auth::now()).await {
        if let Ok(profile) = serde_json::from_value(row.value) {
            return profile;
        }
    }
    let now: DateTime<Utc> = std::time::SystemTime::now().into();
    let result = async {
        let email = calendars.email(peer).await?;
        let (timezone, mut events) = calendars.history(account, &email).await?;
        let tz: chrono_tz::Tz = timezone.parse().map_err(|_| "invalid_calendar_timezone")?;
        events.sort_by_key(|e| std::cmp::Reverse(e.start));
        let previous_meetings = events
            .iter()
            .filter(|e| e.peer && e.end < now)
            .take(20)
            .cloned()
            .collect();
        // Include distinct recent meetings plus meetings with this peer. Recurrences
        // contribute one observation, preventing one mandatory series dominating.
        let mut recent = distinct_past(&events, now);
        recent.sort_by_key(|e| (!e.peer, std::cmp::Reverse(e.start)));
        let sample: Vec<Observation> = recent.into_iter().take(60).cloned().collect();
        let mut profile = Profile {
            previous_meetings,
            ..Profile::default()
        };
        if sample.len() >= 3 {
            if let Some(model) = model {
                match model
                    .analyze(json!({"timezone":timezone,"observations":sample}))
                    .await
                {
                    Ok(mut value) => {
                        let evidence_ids = value["evidence_ids"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let evidence: Vec<_> = sample
                            .iter()
                            .filter(|e| evidence_ids.iter().any(|id| id.as_str() == Some(&e.id)))
                            .cloned()
                            .collect();
                        value.as_object_mut().map(|m| m.remove("evidence_ids"));
                        if let Ok(p) = serde_json::from_value::<Preferences>(value) {
                            if evidence.len() >= 3 && evidenced(&p, &evidence, tz, now) {
                                profile.preferences = p;
                                profile.source = "learned".into();
                            }
                        }
                    }
                    Err(code) => {
                        tracing::info!(event = "preference_fallback", code);
                    }
                }
            }
        }
        Ok::<_, &'static str>(profile)
    }
    .await;
    let profile = match result {
        Ok(p) => p,
        Err(code) => {
            tracing::info!(event = "history_fallback", code);
            Profile::default()
        }
    };
    // Cache defaults too: an exhausted budget must not cause an inference loop.
    let _ = store
        .put(
            &k,
            Entry {
                value: serde_json::to_value(&profile).unwrap(),
                expires: crate::auth::now() + 86400,
                binding: String::new(),
            },
        )
        .await;
    profile
}
/// Hard free/busy comes first; soft preferences never make an occupied slot valid.
pub fn score(slot: &Slot, tz: &str, p: &Preferences, today: DateTime<Utc>) -> Option<i32> {
    let tz: chrono_tz::Tz = tz.parse().ok()?;
    let a = slot.start.with_timezone(&tz);
    let b = slot.end.with_timezone(&tz);
    if a.date_naive() <= today.with_timezone(&tz).date_naive()
        || a.weekday().number_from_monday() > 5
        || a.date_naive() != b.date_naive()
        || a.hour() < 9
        || b.hour() > 18
        || (b.hour() == 18 && b.minute() > 0)
    {
        return None;
    }
    let start = a.hour() * 60 + a.minute();
    let end = b.hour() * 60 + b.minute();
    if !p.allow_lunch && start < 14 * 60 && end > 12 * 60 {
        return None;
    }
    let mut score = 50;
    if let Some(d) = p.preferred_weekday {
        if d == a.weekday().number_from_monday() {
            score += 25;
        }
    }
    if let Some(h) = p.preferred_hour {
        if h == a.hour() {
            score += 15;
        }
    }
    if (slot.end - slot.start).num_minutes() == i64::from(p.duration_minutes) {
        score += 10;
    }
    Some(score)
}
pub fn select(
    left: &[Slot],
    right: &[Slot],
    left_tz: &str,
    right_tz: &str,
    a: &Preferences,
    b: &Preferences,
    now: DateTime<Utc>,
) -> Option<Slot> {
    let mut durations = vec![30, a.duration_minutes, b.duration_minutes];
    durations.sort();
    durations.dedup();
    let mut best: Option<((i32, i32, std::cmp::Reverse<DateTime<Utc>>), Slot)> = None;
    for l in left {
        for r in right {
            let start = l.start.max(r.start);
            let end = l.end.min(r.end);
            let mut t = DateTime::from_timestamp(((start.timestamp() + 899) / 900) * 900, 0)?;
            while t < end {
                for minutes in &durations {
                    let slot = Slot {
                        start: t,
                        end: t + chrono::Duration::minutes(i64::from(*minutes)),
                    };
                    if slot.end > end {
                        continue;
                    }
                    if let (Some(x), Some(y)) = (
                        score(&slot, left_tz, a, now),
                        score(&slot, right_tz, b, now),
                    ) {
                        let rank = (x.min(y), x + y, std::cmp::Reverse(t));
                        if best.as_ref().is_none_or(|(old, _)| rank > *old) {
                            best = Some((rank, slot));
                        }
                    }
                }
                t += chrono::Duration::minutes(15);
            }
        }
    }
    best.map(|(_, s)| s)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dt(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    #[test]
    fn lunch_busy_time_and_tomorrow_remain_constraints() {
        let now = dt("2026-09-17T08:00:00Z");
        let slots = vec![Slot {
            start: dt("2026-09-18T10:00:00Z"),
            end: dt("2026-09-18T14:00:00Z"),
        }];
        let p = Preferences::default();
        let s = select(&slots, &slots, "Europe/Paris", "Europe/Paris", &p, &p, now).unwrap();
        assert_eq!(s.start, dt("2026-09-18T12:00:00Z"));
        let lunch = Preferences {
            allow_lunch: true,
            ..p.clone()
        };
        assert_eq!(
            select(
                &slots,
                &slots,
                "Europe/Paris",
                "Europe/Paris",
                &lunch,
                &lunch,
                now
            )
            .unwrap()
            .start,
            slots[0].start
        );
        assert!(select(&slots, &[], "Europe/Paris", "Europe/Paris", &p, &p, now).is_none());
    }
    #[test]
    fn repeated_series_is_not_three_independent_lunch_choices() {
        let e = Observation {
            id: "1".into(),
            title: "Weekly".into(),
            description: String::new(),
            start: dt("2026-09-11T11:00:00Z"),
            end: dt("2026-09-11T12:00:00Z"),
            peer: true,
            organized: false,
            series: Some("series".into()),
        };
        let p = Preferences {
            allow_lunch: true,
            ..Default::default()
        };
        assert!(!evidenced(
            &p,
            &[e.clone(), e.clone(), e],
            "Europe/Paris".parse().unwrap(),
            dt("2026-09-17T08:00:00Z")
        ));
    }
}
