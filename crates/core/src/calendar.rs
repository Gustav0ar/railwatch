//! Calendar boundaries shared by the native desktop and Noctalia clients.
use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;

pub fn timezone(explicit: Option<&str>) -> Result<Tz> {
    if let Some(name) = explicit {
        return name.parse().context("invalid time zone");
    }
    let name = std::env::var("TZ")
        .ok()
        .or_else(|| iana_time_zone::get_timezone().ok());
    Ok(name.and_then(|s| s.parse().ok()).unwrap_or(chrono_tz::UTC))
}

fn start_of_day(date: NaiveDate, tz: Tz) -> Result<i64> {
    // Some zones advance their clocks at midnight. Use the first existing minute.
    for minute in 0..1440 {
        let local = date
            .and_hms_opt(minute / 60, minute % 60, 0)
            .context("invalid date")?;
        if let Some(time) = local.and_local_timezone(tz).earliest() {
            return Ok(time.timestamp_millis());
        }
    }
    bail!("calendar date does not exist in {tz}")
}

pub fn today(now: DateTime<Utc>, tz: Tz) -> Result<(i64, i64)> {
    let date = now.with_timezone(&tz).date_naive();
    let mut next = date.succ_opt().context("date overflow")?;
    // A zone can skip an entire date, as Pacific/Apia did in 2011.
    for _ in 0..3 {
        if let Ok(end) = start_of_day(next, tz) {
            return Ok((start_of_day(date, tz)?, end));
        }
        next = next.succ_opt().context("date overflow")?;
    }
    bail!("cannot determine the next calendar day in {tz}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn calendar_days_follow_clock_changes_and_skipped_dates() {
        for (at, zone, hours) in [
            ("2025-11-02T12:00:00Z", "America/New_York", 25),
            ("2018-11-04T12:00:00Z", "America/Sao_Paulo", 23),
            ("2011-12-29T12:00:00Z", "Pacific/Apia", 24),
        ] {
            let (start, end) = today(at.parse().unwrap(), zone.parse().unwrap()).unwrap();
            assert_eq!(end - start, hours * 3_600_000, "{zone}");
        }
        assert!(timezone(Some("typo/zone")).is_err());
    }
}
