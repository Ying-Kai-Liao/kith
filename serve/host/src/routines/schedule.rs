//! Validated schedules. Calendar schedules retain their IANA time zone across restarts.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Trigger {
    Interval {
        seconds: i64,
    },
    Once {
        at: i64,
    },
    Cron {
        expression: String,
        time_zone: String,
    },
    Webhook,
    Event {
        source: String,
        event: String,
        #[serde(default)]
        filters: std::collections::BTreeMap<String, String>,
    },
}

impl Trigger {
    pub fn validate(&self, now: i64) -> Result<()> {
        match self {
            Self::Interval { seconds } if !(1..=31_536_000).contains(seconds) => {
                bail!("interval must be between 1 second and 365 days")
            }
            Self::Once { at } if *at <= now => bail!("one-time trigger must be in the future"),
            Self::Cron { expression, time_zone } => {
                if expression.split_whitespace().count() != 5 {
                    bail!("use a five-field cron expression");
                }
                Cron::from_str(expression).context("invalid cron expression")?;
                time_zone.parse::<Tz>().context("invalid IANA time zone")?;
                self.next(now)?.context("schedule has no future occurrence")?;
            }
            Self::Event { source, event, filters } => {
                if !["slack", "github", "origin", "microsoftTeams", "linear", "sentry", "pagerduty", "email"]
                    .contains(&source.as_str())
                {
                    bail!("unsupported event source");
                }
                if event.is_empty()
                    || event.len() > 200
                    || filters.len() > 20
                    || filters.iter().any(|(k, v)| k.is_empty() || k.len() > 200 || v.len() > 4000)
                {
                    bail!("invalid event filter");
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn next(&self, after: i64) -> Result<Option<i64>> {
        Ok(match self {
            Self::Interval { seconds } => Some(
                after
                    .checked_add(seconds.checked_mul(1000).context("interval overflow")?)
                    .context("schedule overflow")?,
            ),
            Self::Once { at } => (*at > after).then_some(*at),
            Self::Cron { expression, time_zone } => {
                let zone = time_zone.parse::<Tz>().context("invalid time zone")?;
                let date = DateTime::<Utc>::from_timestamp_millis(after)
                    .context("invalid schedule timestamp")?
                    .with_timezone(&zone);
                Some(next_cron(&Cron::from_str(expression)?, date, after)?)
            }
            Self::Webhook | Self::Event { .. } => None,
        })
    }

    /// Skip missed intervals without moving the original cadence forward on each tick.
    pub fn advance(&self, due: i64, now: i64) -> Result<Option<i64>> {
        if let Self::Interval { seconds } = self {
            let step = seconds.checked_mul(1000).filter(|v| *v > 0).context("invalid interval")?;
            let elapsed = now.checked_sub(due).context("schedule overflow")?.max(0);
            let count = elapsed.checked_div(step).and_then(|n| n.checked_add(1)).context("schedule overflow")?;
            return Ok(Some(
                due.checked_add(count.checked_mul(step).context("schedule overflow")?).context("schedule overflow")?,
            ));
        }
        self.next(now)
    }

    pub fn description(&self) -> String {
        match self {
            Self::Interval { seconds } if seconds % 86400 == 0 => format!("Every {} days", seconds / 86400),
            Self::Interval { seconds } if seconds % 3600 == 0 => format!("Every {} hours", seconds / 3600),
            Self::Interval { seconds } if seconds % 60 == 0 => format!("Every {} minutes", seconds / 60),
            Self::Interval { seconds } => format!("Every {seconds} seconds"),
            Self::Once { at } => DateTime::from_timestamp_millis(*at)
                .map_or_else(|| "Once".into(), |d| format!("Once at {}", d.to_rfc3339())),
            Self::Cron { expression, time_zone } => format!(
                "{} · {time_zone}",
                Cron::from_str(expression)
                    .map_or_else(|_| expression.clone(), |cron| cron.describe().trim_end_matches('.').to_owned())
            ),
            Self::Webhook => "When a webhook fires".into(),
            Self::Event { source, event, .. } => format!("{source} · {event}"),
        }
    }

    pub fn matches(&self, payload: &serde_json::Value) -> bool {
        let Self::Event { source, event, filters } = self else { return false };
        payload["source"].as_str() == Some(source)
            && (event == "*" || payload["event"].as_str() == Some(event))
            && filters.iter().all(|(key, value)| {
                if key == "messageContains" {
                    payload["text"].as_str().is_some_and(|text| text.contains(value))
                } else {
                    payload[key].as_str() == Some(value)
                }
            })
    }
}

/// The next cron fire strictly after `after`. On a DST fall-back croner can answer with the
/// earlier offset of a repeated local time, which lies before `after` when `after` is already in
/// the second pass; take the later offset of that local time, else keep searching forward.
fn next_cron(cron: &Cron, mut from: DateTime<Tz>, after: i64) -> Result<i64> {
    for _ in 0..8 {
        let found = cron.find_next_occurrence(&from, false)?;
        if found.timestamp_millis() > after {
            return Ok(found.timestamp_millis());
        }
        if let chrono::LocalResult::Ambiguous(_, late) = found.timezone().from_local_datetime(&found.naive_local())
            && late.timestamp_millis() > after
        {
            return Ok(late.timestamp_millis());
        }
        from = found;
    }
    bail!("schedule has no occurrence after the given time")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn taipei_sunday_afternoon_uses_correct_utc_time() {
        let t = Trigger::Cron { expression: "20 14 * * 0".into(), time_zone: "Asia/Taipei".into() };
        let now = DateTime::parse_from_rfc3339("2026-09-26T12:00:00Z").unwrap().timestamp_millis();
        let expected = DateTime::parse_from_rfc3339("2026-09-27T06:20:00Z").unwrap().timestamp_millis();
        assert_eq!(t.next(now).unwrap(), Some(expected));
    }
    #[test]
    fn calendar_presets_follow_clock_boundaries() {
        let now = DateTime::parse_from_rfc3339("2026-09-27T01:07:00Z").unwrap().timestamp_millis();
        for (expression, expected) in [
            ("*/15 * * * *", "2026-09-27T01:15:00Z"),
            ("37 * * * *", "2026-09-27T01:37:00Z"),
            ("0 9 * * 1,3,5", "2026-09-28T01:00:00Z"),
            ("0 9 31 * *", "2026-10-31T01:00:00Z"),
        ] {
            let trigger = Trigger::Cron { expression: expression.into(), time_zone: "Asia/Taipei".into() };
            trigger.validate(now).unwrap();
            let expected = DateTime::parse_from_rfc3339(expected).unwrap().timestamp_millis();
            assert_eq!(trigger.next(now).unwrap(), Some(expected), "{expression}");
        }
    }

    #[test]
    fn event_filters_require_all_fields() {
        let t = Trigger::Event {
            source: "github".into(),
            event: "pull_request.opened".into(),
            filters: [("repo".into(), "owner/repo".into())].into(),
        };
        assert!(t.matches(&serde_json::json!({"source":"github","event":"pull_request.opened","repo":"owner/repo"})));
        assert!(!t.matches(&serde_json::json!({"source":"github","event":"pull_request.opened","repo":"other/repo"})));
    }
    #[test]
    fn invalid_schedules_are_rejected() {
        assert!(Trigger::Interval { seconds: 0 }.validate(0).is_err());
        assert!(Trigger::Once { at: 0 }.validate(0).is_err());
        assert!(Trigger::Cron { expression: "* * * * *".into(), time_zone: "not-a-zone".into() }.validate(0).is_err());
    }

    fn ms(rfc: &str) -> i64 {
        DateTime::parse_from_rfc3339(rfc).unwrap().timestamp_millis()
    }
    fn cron(expression: &str, zone: &str) -> Trigger {
        Trigger::Cron { expression: expression.into(), time_zone: zone.into() }
    }

    #[test]
    fn fall_back_second_pass_never_returns_a_past_time() {
        // 01:30 happens twice on 2026-11-01 in New York: 05:30Z (EDT) and 06:30Z (EST).
        let t = cron("30 1 * * *", "America/New_York");
        let after = ms("2026-11-01T06:00:00Z");
        assert_eq!(t.next(after).unwrap(), Some(ms("2026-11-01T06:30:00Z")));
        // From the first pass the first fire is still the EDT one.
        assert_eq!(t.next(ms("2026-11-01T05:00:00Z")).unwrap(), Some(ms("2026-11-01T05:30:00Z")));
        // After the second pass the next fire is the following day.
        assert_eq!(t.next(ms("2026-11-01T06:30:00Z")).unwrap(), Some(ms("2026-11-02T06:30:00Z")));
    }

    #[test]
    fn sydney_april_fall_back_is_strictly_after() {
        // Sydney ends DST 2026-04-05 03:00 AEDT -> 02:00 AEST; 02:30 occurs at 15:30Z and 16:30Z (Apr 4).
        let t = cron("30 2 * * *", "Australia/Sydney");
        let after = ms("2026-04-04T16:00:00Z");
        let next = t.next(after).unwrap().unwrap();
        assert!(next > after);
        assert_eq!(next, ms("2026-04-04T16:30:00Z"));
    }

    #[test]
    fn spring_forward_gap_never_returns_a_past_time() {
        // 02:30 does not exist on 2026-03-08 in New York.
        let t = cron("30 2 * * *", "America/New_York");
        let after = ms("2026-03-08T06:00:00Z");
        let next = t.next(after).unwrap().unwrap();
        assert!(next > after);
        assert!(next < ms("2026-03-10T00:00:00Z"));
    }
}
