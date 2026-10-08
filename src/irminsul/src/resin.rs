//! 原粹树脂 (Original Resin) and the clock it regenerates on.
//!
//! Resin is the one value this program reports that moves on its own: the game
//! adds one point every eight minutes, up to the cap (200 as of this writing),
//! and sends **no packet for that**.  A value read out of a packet is therefore
//! already stale when it arrives, and the whole job of this module is to answer
//! "what is the game showing *now*" from the reports that do exist:
//!
//! - a **sync** (login, resync) carries the value at that instant and nothing
//!   else.  The next point lands somewhere in the following eight minutes, so
//!   the extrapolation is never more than one point low;
//! - a **+1 change** *is* a regeneration point.  Its arrival time goes into
//!   [`ResinClock::last_increase_at`], and from there the number of points that
//!   have landed since the last report is a subtraction rather than a guess --
//!   which is what makes the value exact, online *and* offline.
//!
//! Nothing else moves the clock.  Spending resin, restoring it with a fragile
//! resin, a resync: those change the value, so the value is updated, but the
//! eight-minute window is the game's and is not restarted by them.  Should the
//! game ever disagree -- a point arriving off the predicted cadence -- the next
//! observed `+1` re-anchors, which is why an observation always wins over the
//! model.

use chrono::{DateTime, Local};

/// Seconds per point of resin: the game adds one every eight minutes.
pub const TICK_SECONDS: i64 = 8 * 60;

/// Where regeneration stops: the cap the game applies, 200 today.
///
/// Not a natural constant, and it has moved before (it was 160 until the cap was
/// raised), so treat it as a fact about the current game that a log or a capture
/// can settle.  A value *above* the cap is possible while a transient resin is
/// in hand, and is reported as it stands rather than grown.
pub const CAP: i64 = 200;

/// Last reported resin, and the regeneration point its value is extrapolated
/// from.
#[derive(Debug, Clone, Copy, Default)]
pub struct ResinClock {
    /// The last value the game reported.  Kept only to recognise a regeneration
    /// point: a live `+1` is the one observation that anchors the cadence.
    seen: Option<i64>,
    /// The value the game last reported, and when it reported it.
    ///
    /// The instant matters as much as the value: it is the point the
    /// extrapolation counts from, and it must not be moved by a report that
    /// changes nothing -- see [`Self::observe`].
    value: Option<(i64, DateTime<Local>)>,
    /// When the game was last *seen* adding a point.
    ///
    /// This is the anchor the whole extrapolation rests on: points land every
    /// eight minutes from here, so "how many have landed since the value was
    /// reported" is arithmetic.  `None` until a `+1` has been observed, which is
    /// the one case where the value can be a point low.
    last_increase_at: Option<DateTime<Local>>,
}

impl ResinClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note a resin value the game reported.
    ///
    /// `live` is true for an incremental prop update and false for a full sync.
    /// The distinction only matters in one direction: a sync is authoritative
    /// about the value but says nothing about when the next point arrives, so it
    /// can never establish the cadence -- only an observed `+1` does that.  It
    /// does not *break* it either: the window belongs to the game, so a login
    /// leaves the last observed point where it was.
    pub fn observe(&mut self, value: i64, live: bool, at: DateTime<Local>) {
        // The same value again says nothing -- and in particular must not move
        // the instant the extrapolation counts from, or the minutes already
        // spent towards the next point would be thrown away on every redundant
        // report.
        if self.seen == Some(value) {
            return;
        }

        if live && self.seen == Some(value - 1) {
            // A regeneration point, and the only thing that anchors the cadence.
            // It re-anchors even when the previous line had drifted: the game's
            // own point is the authority, not the model.
            self.last_increase_at = Some(at);
        }

        self.value = Some((value, at));
        self.seen = Some(value);
    }

    /// The value the model says the game is showing at `at`, or `None` while
    /// nothing has been reported at all.
    pub fn value_at(&self, at: DateTime<Local>) -> Option<i64> {
        let (value, observed) = self.value?;

        // At or past the cap nothing regenerates, and an overshoot -- a
        // transient resin can put it there -- is left alone rather than grown.
        if value >= CAP {
            return Some(value);
        }

        let points = match self.last_increase_at {
            // Exact: the points that have landed between the report and now are
            // the eight-minute boundaries passed since the anchor, which is a
            // subtraction rather than an assumption.
            Some(anchor) => points_between(anchor, at) - points_between(anchor, observed),
            // Without an anchor the phase is unknown: the next point is
            // somewhere in the eight minutes after the report, so counting from
            // the report can be one low and never more.
            None => points_between(observed, at),
        };

        Some((value + points.max(0)).min(CAP))
    }

    /// When the game was last seen adding a point, if that has ever been seen.
    ///
    /// Published as the anchor of the extrapolation: a reader that has this and
    /// the value at some instant can work out the value at any later instant,
    /// exactly, without needing the core to still be running.
    pub fn last_increase_at(&self) -> Option<DateTime<Local>> {
        self.last_increase_at
    }
}

/// How many eight-minute boundaries fall between two instants.
fn points_between(from: DateTime<Local>, to: DateTime<Local>) -> i64 {
    (to - from).num_seconds().max(0) / TICK_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A fixed instant, so every case below is arithmetic rather than timing.
    fn at(minutes: i64) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap() + chrono::Duration::minutes(minutes)
    }

    /// A sync alone gives the value, and nothing about the phase -- so the
    /// extrapolation can be one low, and never more than that.
    #[test]
    fn a_sync_alone_can_be_one_low() {
        let mut clock = ResinClock::new();
        clock.observe(150, false, at(0));

        assert_eq!(clock.value_at(at(0)), Some(150));
        assert_eq!(clock.value_at(at(7)), Some(150), "the next point is not due yet");
        assert_eq!(clock.value_at(at(8)), Some(151));
        assert_eq!(clock.value_at(at(60)), Some(157));
        assert_eq!(clock.last_increase_at(), None, "a sync is not a regeneration point");
    }

    /// A live +1 is a regeneration point: from it the cadence is exact, and the
    /// instant it arrived is what a reader would need to reproduce that.
    #[test]
    fn a_regeneration_point_anchors_the_cadence() {
        let mut clock = ResinClock::new();
        clock.observe(150, false, at(0));

        // The game adds one three minutes later; the phase is now known.
        clock.observe(151, true, at(3));
        assert_eq!(clock.last_increase_at(), Some(at(3)));
        assert_eq!(clock.value_at(at(3)), Some(151));
        assert_eq!(clock.value_at(at(10)), Some(151), "the next point is due at +11");
        assert_eq!(clock.value_at(at(11)), Some(152));
        assert_eq!(clock.value_at(at(19)), Some(153));
    }

    /// A point that arrives after the value was already reported as something
    /// else still anchors: the value is taken as truth, and the anchor fixes the
    /// phase from then on.
    #[test]
    fn a_point_off_the_cadence_re_anchors() {
        let mut clock = ResinClock::new();
        clock.observe(100, false, at(0));
        clock.observe(101, true, at(0));

        // The next point arrives five minutes later rather than eight.
        clock.observe(102, true, at(5));
        assert_eq!(clock.last_increase_at(), Some(at(5)), "the game's own point wins");
        assert_eq!(clock.value_at(at(12)), Some(102), "five minutes on, not due yet");
        assert_eq!(clock.value_at(at(13)), Some(103));
    }

    /// A sync that arrives with a different value cannot anchor the cadence even
    /// if it happens to be one higher than the last one seen -- and it must not
    /// *drop* an anchor that an earlier +1 established, because the window is
    /// the game's and a login does not restart it.
    #[test]
    fn a_sync_neither_anchors_nor_un_anchors() {
        let mut clock = ResinClock::new();
        clock.observe(150, false, at(0));
        clock.observe(151, false, at(1));
        assert_eq!(clock.last_increase_at(), None, "a sync is not a point");
        assert_eq!(clock.value_at(at(1)), Some(151), "the reported value is taken as truth");

        clock.observe(152, true, at(2));
        clock.observe(120, false, at(30));
        assert_eq!(clock.last_increase_at(), Some(at(2)), "the login leaves the anchor alone");
        assert_eq!(clock.value_at(at(30)), Some(120));
        // Points at +10, +18, +26 since the anchor have landed: the one due at
        // +34 is the next.
        assert_eq!(clock.value_at(at(33)), Some(120));
        assert_eq!(clock.value_at(at(34)), Some(121));
    }

    /// Spending resin does not reset the game's window: the value drops, and the
    /// points that were already counting keep counting.
    #[test]
    fn spending_keeps_the_cadence() {
        let mut clock = ResinClock::new();
        clock.observe(150, false, at(0));
        clock.observe(151, true, at(0));

        // 40 resin spent two minutes after the point: the next one is still due
        // six minutes later, on the cadence the +1 anchored.
        clock.observe(111, true, at(2));
        assert_eq!(clock.value_at(at(2)), Some(111));
        assert_eq!(clock.value_at(at(7)), Some(111));
        assert_eq!(clock.value_at(at(8)), Some(112));
    }

    /// Restoring resin with a fragile resin is the same story in the other
    /// direction.
    #[test]
    fn restoring_resin_keeps_the_cadence() {
        let mut clock = ResinClock::new();
        clock.observe(40, false, at(0));
        clock.observe(41, true, at(0));

        clock.observe(101, true, at(4));
        assert_eq!(clock.value_at(at(4)), Some(101));
        assert_eq!(clock.value_at(at(8)), Some(102), "still the cadence from +0");
    }

    /// The same value reported twice must not restart the window: report rates
    /// are not the game's clock.
    #[test]
    fn the_same_value_again_changes_nothing() {
        let mut clock = ResinClock::new();
        clock.observe(150, false, at(0));
        clock.observe(150, false, at(4));
        clock.observe(150, true, at(6));

        assert_eq!(clock.value_at(at(7)), Some(150), "the count still starts at +0");
        assert_eq!(clock.value_at(at(8)), Some(151));
    }

    /// Regeneration stops at the cap, and a value already past it -- a transient
    /// resin can put it there -- is reported as it stands.
    #[test]
    fn regeneration_stops_at_the_cap() {
        let mut clock = ResinClock::new();
        clock.observe(199, false, at(0));
        assert_eq!(clock.value_at(at(8)), Some(200));
        assert_eq!(clock.value_at(at(10_000)), Some(200), "a day later it is still the cap");

        clock.observe(220, false, at(10_001));
        assert_eq!(clock.value_at(at(20_000)), Some(220), "past the cap nothing grows");
    }
}
