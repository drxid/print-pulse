//! When the next maintenance print is due, in local time.

use chrono::{Datelike, Days, Local, NaiveDate, TimeZone, Timelike};

pub fn now() -> i64 {
    Local::now().timestamp()
}

/// Unix timestamp of `date` at `minute` minutes past local midnight. Handles DST
/// gaps by taking the earliest valid instant (or an hour later if none exists).
fn at_local(date: NaiveDate, minute: u32) -> i64 {
    let naive = date
        .and_hms_opt(minute / 60 % 24, minute % 60, 0)
        .unwrap_or_else(|| date.and_hms_opt(12, 0, 0).unwrap());
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&(naive + chrono::Duration::hours(1))).earliest())
        .map(|d| d.timestamp())
        .unwrap_or_else(now)
}

/// The next run: `interval_days` after the day of the last maintenance (`base`),
/// at the chosen time. With no history it's the next occurrence of that time.
/// A result in the past means the run was missed (PC off) and is due right away.
pub fn next_run(base: Option<i64>, interval_days: u32, minute: u32) -> i64 {
    let today = Local::now().date_naive();
    match base.and_then(|b| Local.timestamp_opt(b, 0).single()) {
        Some(last) => {
            let day = last.date_naive() + Days::new(interval_days.max(1) as u64);
            at_local(day, minute)
        }
        None => {
            let t = at_local(today, minute);
            if t > now() {
                t
            } else {
                at_local(today + Days::new(1), minute)
            }
        }
    }
}

/// When printing is not allowed: whole weekdays and/or a daily time window.
#[derive(Clone, Copy, Debug, Default)]
pub struct Quiet {
    /// Bit 0 = Monday … bit 6 = Sunday.
    pub days: u8,
    /// `[from, to)` in minutes after midnight; may wrap past midnight.
    pub hours: Option<(u32, u32)>,
}

impl Quiet {
    fn in_hours(&self, minute: u32) -> bool {
        match self.hours {
            Some((from, to)) if from < to => minute >= from && minute < to,
            Some((from, to)) if from > to => minute >= from || minute < to,
            _ => false,
        }
    }

    /// The first instant at or after `t` outside quiet time. A blocked day moves
    /// the run to the next day at `preferred` (the user's chosen time).
    pub fn first_allowed(&self, mut t: i64, preferred: u32) -> i64 {
        // A full week of quiet days would never print; treat it as no day rule.
        let days = if self.days & 0x7f == 0x7f { 0 } else { self.days };
        for _ in 0..16 {
            let Some(dt) = Local.timestamp_opt(t, 0).earliest() else { return t };
            let date = dt.date_naive();
            let minute = dt.hour() * 60 + dt.minute();
            if days & (1 << dt.weekday().num_days_from_monday()) != 0 {
                t = at_local(date + Days::new(1), preferred);
            } else if self.in_hours(minute) {
                let (from, to) = self.hours.unwrap();
                // In the late part of a window that wraps midnight, it ends tomorrow.
                let end_day = if from > to && minute >= from { date + Days::new(1) } else { date };
                t = at_local(end_day, to);
            } else {
                return t;
            }
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> i64 {
        at_local(NaiveDate::from_ymd_opt(y, m, d).unwrap(), h * 60 + min)
    }

    #[test]
    fn quiet_hours_wrap_midnight() {
        let q = Quiet { days: 0, hours: Some((22 * 60, 8 * 60)) };
        // 2026-10-07 is a Wednesday.
        assert_eq!(q.first_allowed(ts(2026, 10, 7, 23, 30), 12 * 60), ts(2026, 10, 8, 8, 0));
        assert_eq!(q.first_allowed(ts(2026, 10, 7, 3, 0), 12 * 60), ts(2026, 10, 7, 8, 0));
        assert_eq!(q.first_allowed(ts(2026, 10, 7, 12, 0), 12 * 60), ts(2026, 10, 7, 12, 0));
    }

    #[test]
    fn quiet_weekend() {
        let q = Quiet { days: 0b110_0000, hours: None };
        // Saturday 2026-10-10 → Monday at the preferred time.
        assert_eq!(q.first_allowed(ts(2026, 10, 10, 9, 0), 12 * 60), ts(2026, 10, 12, 12, 0));
    }

    #[test]
    fn quiet_day_then_hours() {
        let q = Quiet { days: 0b100_0000, hours: Some((0, 9 * 60)) };
        // Sunday → Monday at preferred 07:00, which is quiet → 09:00.
        assert_eq!(q.first_allowed(ts(2026, 10, 11, 10, 0), 7 * 60), ts(2026, 10, 12, 9, 0));
    }
}
