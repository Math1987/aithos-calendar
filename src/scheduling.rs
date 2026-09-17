use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    GetAvailability {
        #[serde(default)]
        window: Option<Window>,
    },
    FindCommonSlot {
        peer: String,
        #[serde(default, deserialize_with = "optional_minutes")]
        duration_minutes: Option<u16>,
    },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}
impl Window {
    pub fn next_month() -> Self {
        let start: DateTime<Utc> = std::time::SystemTime::now().into();
        Self {
            start,
            end: start + Duration::days(30),
        }
    }
    pub fn valid(&self) -> bool {
        self.end > self.start && self.end - self.start <= Duration::days(30)
    }
}
fn optional_minutes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u16>, D::Error> {
    whole_minutes(deserializer).map(Some)
}
// A2A data parts travel through protobuf Struct, whose numbers are doubles.
// Accept 30 and 30.0 as the same whole-minute value, never truncate fractions.
pub(crate) fn whole_minutes<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<u16, D::Error> {
    let value = f64::deserialize(deserializer)?;
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=u16::MAX as f64).contains(&value) {
        return Err(serde::de::Error::custom(
            "duration_minutes must be a whole number",
        ));
    }
    Ok(value as u16)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Availability {
    pub status: String,
    pub agent: String,
    pub slots: Vec<Slot>,
    pub mock: bool,
    pub trace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<ScheduleInfo>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInfo {
    pub title: String,
    pub timezone: String,
    #[serde(deserialize_with = "whole_minutes")]
    pub duration_minutes: u16,
    pub window: Window,
}
impl From<&crate::availability::Schedule> for ScheduleInfo {
    fn from(s: &crate::availability::Schedule) -> Self {
        Self {
            title: s.title.clone(),
            timezone: s.timezone.clone(),
            duration_minutes: s.duration_minutes,
            window: Window {
                start: s.window_start,
                end: s.window_end,
            },
        }
    }
}
impl Availability {
    pub fn into_schedule(self) -> Option<crate::availability::Schedule> {
        let info = self.schedule?;
        Some(crate::availability::Schedule {
            schedule_id: self.agent,
            identity: Default::default(),
            title: info.title,
            timezone: info.timezone,
            duration_minutes: info.duration_minutes,
            window_start: info.window.start,
            window_end: info.window.end,
            slots: self.slots,
        })
    }
}

/// Find the earliest full interval, independently of input ordering or time zone.
pub fn first_common_slot(left: &[Slot], right: &[Slot], minutes: u16) -> Option<Slot> {
    let duration = Duration::minutes(i64::from(minutes));
    left.iter()
        .flat_map(|a| {
            right.iter().filter_map(move |b| {
                let start = a.start.max(b.start);
                let end = start.checked_add_signed(duration)?;
                (minutes > 0 && end <= a.end.min(b.end)).then_some(Slot { start, end })
            })
        })
        .min_by_key(|slot| slot.start)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn slot(start: &str, end: &str) -> Slot {
        Slot {
            start: start.parse().unwrap(),
            end: end.parse().unwrap(),
        }
    }
    #[test]
    fn timezone_normalization_exact_fit_and_earliest_match() {
        let a = vec![
            slot("2030-01-15T14:00:00Z", "2030-01-15T15:00:00Z"),
            slot("2030-01-15T10:00:00+01:00", "2030-01-15T11:00:00+01:00"),
        ];
        let b = vec![
            slot("2030-01-15T09:30:00Z", "2030-01-15T10:30:00Z"),
            a[0].clone(),
        ];
        assert_eq!(
            first_common_slot(&a, &b, 30),
            Some(slot("2030-01-15T09:30:00Z", "2030-01-15T10:00:00Z"))
        );
        assert_eq!(first_common_slot(&a[1..], &b[..1], 31), None);
    }
    #[test]
    fn touching_disjoint_and_empty_intervals_have_no_slot() {
        let a = vec![slot("2030-01-15T09:00:00Z", "2030-01-15T10:00:00Z")];
        let b = vec![slot("2030-01-15T10:00:00Z", "2030-01-15T11:00:00Z")];
        assert_eq!(first_common_slot(&a, &b, 30), None);
        assert_eq!(first_common_slot(&[], &b, 30), None);
        assert_eq!(first_common_slot(&a, &a, 0), None);
    }
}
