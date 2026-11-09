//! Download scheduler: only download during configured time windows.
//!
//! The rules are a pure function of the local time ([`Schedule::allows`]), so
//! they can be tested without a clock. The manager checks them on every tick
//! and when settings change.

use chrono::{Datelike, Local, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

const DAY_MINUTES: u32 = 24 * 60;

/// When downloads are allowed to run.
///
/// The window is `[start, end)` in minutes after local midnight. If `end` is
/// before `start` the window crosses midnight (e.g. 23:00–07:00) and belongs
/// to the day it *starts* on: with only Friday selected, 23:00–07:00 means
/// Friday 23:00 until Saturday 07:00. `start == end` means the whole day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    /// Off = download at any time.
    pub enabled: bool,
    /// Minutes after midnight, `0..1440`.
    pub start: u32,
    /// Minutes after midnight, `0..1440`.
    pub end: u32,
    /// Days the window starts on, Monday first.
    pub days: [bool; 7],
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            enabled: false,
            start: 60,   // 01:00
            end: 7 * 60, // 07:00
            days: [true; 7],
        }
    }
}

impl Schedule {
    /// Whether downloading is allowed at `minute` (after midnight) on
    /// `weekday` (0 = Monday … 6 = Sunday).
    pub fn allows(&self, weekday: u32, minute: u32) -> bool {
        if !self.enabled {
            return true;
        }
        let day = |d: u32| self.days[(d % 7) as usize];
        let yesterday = (weekday + 6) % 7;
        let (start, end) = (self.start % DAY_MINUTES, self.end % DAY_MINUTES);
        let minute = minute % DAY_MINUTES;
        if start == end {
            day(weekday)
        } else if start < end {
            day(weekday) && (start..end).contains(&minute)
        } else {
            // Crosses midnight: the evening part belongs to today, the early
            // morning part to the window that started yesterday.
            (day(weekday) && minute >= start) || (day(yesterday) && minute < end)
        }
    }

    /// [`Self::allows`] for a date and time.
    pub fn allows_at(&self, t: NaiveDateTime) -> bool {
        self.allows(
            t.weekday().num_days_from_monday(),
            t.hour() * 60 + t.minute(),
        )
    }

    /// [`Self::allows`] for the current local time.
    pub fn allows_now(&self) -> bool {
        !self.enabled || self.allows_at(Local::now().naive_local())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    const MON: u32 = 0;
    const TUE: u32 = 1;
    const FRI: u32 = 4;
    const SAT: u32 = 5;
    const SUN: u32 = 6;

    fn hm(h: u32, m: u32) -> u32 {
        h * 60 + m
    }

    fn sched(start: u32, end: u32, days: [bool; 7]) -> Schedule {
        Schedule {
            enabled: true,
            start,
            end,
            days,
        }
    }

    fn only(day: u32) -> [bool; 7] {
        let mut d = [false; 7];
        d[day as usize] = true;
        d
    }

    #[test]
    fn disabled_always_allows() {
        let s = Schedule {
            enabled: false,
            days: [false; 7],
            ..Default::default()
        };
        for d in 0..7 {
            for m in (0..DAY_MINUTES).step_by(17) {
                assert!(s.allows(d, m));
            }
        }
    }

    #[test]
    fn same_day_window() {
        let s = sched(hm(9, 0), hm(17, 30), [true; 7]);
        assert!(!s.allows(MON, hm(8, 59)));
        assert!(s.allows(MON, hm(9, 0)), "start is inclusive");
        assert!(s.allows(MON, hm(12, 0)));
        assert!(s.allows(MON, hm(17, 29)));
        assert!(!s.allows(MON, hm(17, 30)), "end is exclusive");
        assert!(!s.allows(MON, hm(23, 59)));
        assert!(!s.allows(MON, 0));
    }

    #[test]
    fn same_day_window_respects_days() {
        let s = sched(hm(9, 0), hm(17, 0), only(TUE));
        assert!(!s.allows(MON, hm(10, 0)));
        assert!(s.allows(TUE, hm(10, 0)));
        assert!(!s.allows(TUE, hm(18, 0)));
        assert!(!s.allows(3, hm(10, 0)));
    }

    #[test]
    fn crosses_midnight() {
        let s = sched(hm(23, 0), hm(7, 0), [true; 7]);
        assert!(s.allows(MON, hm(23, 0)));
        assert!(s.allows(MON, hm(23, 59)));
        assert!(s.allows(TUE, 0));
        assert!(s.allows(TUE, hm(6, 59)));
        assert!(!s.allows(TUE, hm(7, 0)));
        assert!(!s.allows(TUE, hm(12, 0)));
        assert!(!s.allows(TUE, hm(22, 59)));
    }

    #[test]
    fn crossing_window_belongs_to_its_start_day() {
        // Friday night only: Fri 23:00 → Sat 07:00.
        let s = sched(hm(23, 0), hm(7, 0), only(FRI));
        assert!(!s.allows(FRI, hm(3, 0)), "Thursday's night isn't selected");
        assert!(!s.allows(FRI, hm(22, 0)));
        assert!(s.allows(FRI, hm(23, 30)));
        assert!(s.allows(SAT, hm(3, 0)), "continues past midnight");
        assert!(!s.allows(SAT, hm(7, 0)));
        assert!(!s.allows(SAT, hm(23, 30)), "Saturday isn't selected");
    }

    #[test]
    fn crossing_window_wraps_from_sunday_to_monday() {
        let s = sched(hm(22, 0), hm(2, 0), only(SUN));
        assert!(s.allows(SUN, hm(22, 0)));
        assert!(s.allows(MON, hm(1, 0)));
        assert!(!s.allows(MON, hm(22, 0)));
        assert!(!s.allows(SAT, hm(23, 0)));
    }

    #[test]
    fn equal_start_and_end_means_whole_day() {
        let s = sched(hm(8, 0), hm(8, 0), only(MON));
        assert!(s.allows(MON, 0));
        assert!(s.allows(MON, hm(23, 59)));
        assert!(!s.allows(TUE, hm(8, 0)));
    }

    #[test]
    fn no_days_never_allows() {
        let s = sched(hm(1, 0), hm(7, 0), [false; 7]);
        for d in 0..7 {
            for m in 0..DAY_MINUTES {
                assert!(!s.allows(d, m));
            }
        }
    }

    #[test]
    fn midnight_edges() {
        let s = sched(0, hm(6, 0), [true; 7]);
        assert!(s.allows(MON, 0));
        assert!(!s.allows(MON, hm(23, 59)));
        let s = sched(hm(18, 0), 0, [true; 7]);
        assert!(s.allows(MON, hm(23, 59)));
        assert!(!s.allows(TUE, 0), "end 00:00 is exclusive");
    }

    #[test]
    fn crossing_window_equals_union_of_two_parts() {
        // Exhaustive check against a naive model for every minute of a week.
        for (start, end) in [(hm(23, 0), hm(7, 0)), (hm(12, 0), hm(11, 59)), (1, 0)] {
            for mask in [0b1010101u8, 0b0000001, 0b1000000, 0b1111111] {
                let mut days = [false; 7];
                (0..7).for_each(|i| days[i] = mask & (1 << i) != 0);
                let s = sched(start, end, days);
                for d in 0..7u32 {
                    for m in 0..DAY_MINUTES {
                        let prev = ((d + 6) % 7) as usize;
                        let expect = (days[d as usize] && m >= start) || (days[prev] && m < end);
                        assert_eq!(
                            s.allows(d, m),
                            expect,
                            "{start}-{end} mask {mask:b} d{d} m{m}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn allows_at_uses_weekday_and_time() {
        let s = sched(hm(23, 0), hm(7, 0), only(FRI));
        // 2026-11-13 is a Friday.
        let fri = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let sat = fri.succ_opt().unwrap();
        assert!(s.allows_at(fri.and_hms_opt(23, 15, 0).unwrap()));
        assert!(s.allows_at(sat.and_hms_opt(6, 59, 59).unwrap()));
        assert!(!s.allows_at(sat.and_hms_opt(7, 0, 0).unwrap()));
        assert!(!s.allows_at(fri.and_hms_opt(12, 0, 0).unwrap()));
    }

    #[test]
    fn out_of_range_values_are_wrapped() {
        let s = sched(hm(24, 0) + hm(9, 0), hm(17, 0), [true; 7]);
        assert!(s.allows(MON, hm(10, 0)));
    }

    #[test]
    fn deserializes_with_defaults() {
        let s: Schedule = serde_json::from_str(r#"{"enabled":true}"#).unwrap();
        assert!(s.enabled);
        assert_eq!(s.days, [true; 7]);
        assert_eq!((s.start, s.end), (60, 420));
    }
}
