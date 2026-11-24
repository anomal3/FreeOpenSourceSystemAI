//! What the battery clock of a PC keeps (24.11).
//!
//! Windows keeps **local** time in the RTC, Linux and macOS keep UTC. A
//! system that reads a Windows machine's clock as UTC is ahead or behind by
//! the zone offset — Roman's PC (UTC+4) showed the time 4 h ahead until NTP
//! moved it back in the middle of the session. The first correction from the
//! network tells which kind of time the clock keeps: this module decides,
//! the kernel (`time.rs`) acts and remembers.

/// How far from the zone offset the first correction may be and still mean
/// «the clock keeps the other kind of time»: two minutes (the firmware gives
/// whole seconds, the boot takes a while, a clock drifts).
pub const SLACK_MS: i64 = 120_000;

/// The first network correction moved the clock by `moved_ms`; the zone is
/// `offset_minutes` east of UTC; the clock is read as local time
/// (`read_as_local`) or as UTC. `Some(true)` — the clock keeps local time,
/// `Some(false)` — UTC, `None` — the correction says nothing about it.
#[must_use]
pub fn rtc_guess(moved_ms: i64, offset_minutes: i32, read_as_local: bool) -> Option<bool> {
    let offset_ms = i64::from(offset_minutes) * 60_000;
    if offset_ms == 0 {
        return None;
    }
    // Read as UTC but keeps local time: ahead by the offset, moved back.
    // Read as local but keeps UTC: behind by it, moved forward.
    let expected = if read_as_local { offset_ms } else { -offset_ms };
    ((moved_ms - expected).abs() <= SLACK_MS).then_some(!read_as_local)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// hw-log18: UTC+4, the clock read as UTC, NTP moved it by -14398515 ms.
    #[test]
    fn romans_pc_keeps_local_time() {
        assert_eq!(rtc_guess(-14_398_515, 240, false), Some(true));
    }

    #[test]
    fn utc_clock_read_as_local_is_found() {
        assert_eq!(rtc_guess(14_400_000 + 30_000, 240, true), Some(false));
        assert_eq!(rtc_guess(-3 * 3_600_000, -180, true), Some(false));
    }

    #[test]
    fn ordinary_corrections_say_nothing() {
        assert_eq!(rtc_guess(-1_500, 240, false), None);
        assert_eq!(rtc_guess(14_400_000, 240, false), None);
        assert_eq!(rtc_guess(-3_600_000, 240, false), None);
        assert_eq!(rtc_guess(-14_400_000, 0, false), None);
    }
}
