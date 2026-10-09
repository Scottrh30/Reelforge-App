//! Exact, rational time.
//!
//! Video editors must never accumulate time in `f64` seconds: 29.97 fps is really
//! 30000/1001, and floating error turns into dropped or duplicated frames after a
//! few minutes. Everything here is integer math.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Sub};

fn gcd(mut a: i64, mut b: i64) -> i64 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.max(1)
}

/// A reduced fraction `num / den` with `den > 0`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rational {
    pub num: i64,
    pub den: i64,
}

impl Rational {
    pub fn new(num: i64, den: i64) -> Self {
        assert!(den != 0, "zero denominator");
        let s = if den < 0 { -1 } else { 1 };
        let g = gcd(num, den);
        Self { num: s * num / g, den: s * den / g }
    }

    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        // dens are positive, so cross-multiplication preserves order.
        (self.num as i128 * other.den as i128).cmp(&(other.num as i128 * self.den as i128))
    }
}

/// Frames per second, e.g. `FrameRate::new(30000, 1001)` for 29.97.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FrameRate(pub Rational);

impl FrameRate {
    pub fn new(num: i64, den: i64) -> Self {
        Self(Rational::new(num, den))
    }
    pub fn fps(self) -> f64 {
        self.0.as_f64()
    }
    /// Duration of a single frame.
    pub fn frame_duration(self) -> Time {
        Time::from_rational(Rational::new(self.0.den, self.0.num))
    }
    /// Nominal integer rate used for timecode (29.97 -> 30).
    pub fn timecode_base(self) -> i64 {
        (self.0.num + self.0.den / 2) / self.0.den
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::new(30, 1)
    }
}

/// A point or span on a timeline, stored as integer ticks of [`Time::TICKS_PER_SECOND`].
///
/// 705_600_000 is divisible by every common video rate (23.976, 24, 25, 29.97,
/// 30, 50, 59.94, 60, 120) and audio rate (44.1k, 48k, 96k), so frame and sample
/// boundaries land exactly on ticks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Time(pub i64);

impl Time {
    pub const TICKS_PER_SECOND: i64 = 705_600_000;
    pub const ZERO: Time = Time(0);

    pub fn from_secs_f64(s: f64) -> Self {
        Self((s * Self::TICKS_PER_SECOND as f64).round() as i64)
    }
    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / Self::TICKS_PER_SECOND as f64
    }
    pub fn from_rational(r: Rational) -> Self {
        Self(((r.num as i128 * Self::TICKS_PER_SECOND as i128) / r.den as i128) as i64)
    }
    /// Convert a stream timestamp (`pts * time_base`) to ticks.
    pub fn from_pts(pts: i64, time_base: Rational) -> Self {
        Self(
            ((pts as i128 * time_base.num as i128 * Self::TICKS_PER_SECOND as i128)
                / time_base.den as i128) as i64,
        )
    }
    /// Convert ticks back to a stream timestamp in `time_base` units.
    pub fn to_pts(self, time_base: Rational) -> i64 {
        ((self.0 as i128 * time_base.den as i128)
            / (time_base.num as i128 * Self::TICKS_PER_SECOND as i128)) as i64
    }
    pub fn from_frame(frame: i64, rate: FrameRate) -> Self {
        Self(((frame as i128 * rate.0.den as i128 * Self::TICKS_PER_SECOND as i128) / rate.0.num as i128) as i64)
    }
    /// The frame this time falls in (floor).
    pub fn to_frame(self, rate: FrameRate) -> i64 {
        let n = self.0 as i128 * rate.0.num as i128;
        let d = rate.0.den as i128 * Self::TICKS_PER_SECOND as i128;
        n.div_euclid(d) as i64
    }
    /// Snap down to the start of the containing frame.
    pub fn snap(self, rate: FrameRate) -> Self {
        Self::from_frame(self.to_frame(rate), rate)
    }
    pub fn max(self, o: Self) -> Self {
        if self >= o { self } else { o }
    }
    pub fn min(self, o: Self) -> Self {
        if self <= o { self } else { o }
    }

    /// `HH:MM:SS:FF` non-drop-frame timecode.
    pub fn timecode(self, rate: FrameRate) -> String {
        let base = rate.timecode_base().max(1);
        let f = self.to_frame(rate).max(0);
        let (ff, s) = (f % base, f / base);
        format!("{:02}:{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60, ff)
    }
}

impl Add for Time {
    type Output = Time;
    fn add(self, o: Time) -> Time {
        Time(self.0 + o.0)
    }
}
impl Sub for Time {
    type Output = Time;
    fn sub(self, o: Time) -> Time {
        Time(self.0 - o.0)
    }
}
impl std::ops::Neg for Time {
    type Output = Time;
    fn neg(self) -> Time {
        Time(-self.0)
    }
}
impl fmt::Display for Time {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3}s", self.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntsc_frames_are_exact() {
        let r = FrameRate::new(30000, 1001);
        for f in [0, 1, 29, 30, 1798, 1799, 107_892] {
            assert_eq!(Time::from_frame(f, r).to_frame(r), f);
        }
        // One hour of 29.97 is 107892.1 frames -> frame 107892.
        assert_eq!(Time::from_secs_f64(3600.0).to_frame(r), 107_892);
    }

    #[test]
    fn ticks_divisible_by_common_rates() {
        for (n, d) in [(24000, 1001), (24, 1), (25, 1), (30000, 1001), (50, 1), (60000, 1001), (60, 1), (120, 1), (48000, 1), (44100, 1)] {
            assert_eq!((Time::TICKS_PER_SECOND * d) % n, 0, "{n}/{d}");
        }
    }

    #[test]
    fn pts_roundtrip() {
        let tb = Rational::new(1, 90_000);
        assert_eq!(Time::from_pts(90_000, tb), Time(Time::TICKS_PER_SECOND));
        assert_eq!(Time::from_pts(123_456, tb).to_pts(tb), 123_456);
    }

    #[test]
    fn timecode() {
        let r = FrameRate::new(25, 1);
        assert_eq!(Time::from_frame(25 * 3661 + 7, r).timecode(r), "01:01:01:07");
    }

    #[test]
    fn rational_reduces_and_orders() {
        assert_eq!(Rational::new(2, -4), Rational::new(-1, 2));
        assert!(Rational::new(1, 3) < Rational::new(1, 2));
    }
}
