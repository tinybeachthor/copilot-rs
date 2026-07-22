//! Periodic clocks.

use crate::utils::ClockType;
use copilot_lang::{Builder, Stream};

/// A clock that ticks once every `period` steps, first ticking at `phase`.
///
/// The whole clock *is* its buffer: the initial values spell out one period,
/// and the transition expression is the stream itself, so committing a step
/// rotates the pattern rather than computing anything. A period-`n` clock
/// therefore costs `n` bytes and no arithmetic per step at all.
///
/// Returns `None` if the period is zero, or if the phase does not fall inside
/// it — both of which describe no clock.
///
/// ```
/// # use copilot_lang::Builder;
/// # use copilot_libs::clocks;
/// let b = Builder::new();
/// // false, true, false, false, true, false, ...
/// let every_third = clocks::clk(&b, 3, 1).unwrap();
/// # b.observe("tick", every_third);
/// # b.finish().unwrap();
/// ```
pub fn clk(b: &Builder, period: usize, phase: usize) -> Option<Stream<'_, bool>> {
    if period == 0 || phase >= period {
        return None;
    }
    let mut pattern = vec![false; period];
    pattern[phase] = true;
    // The transition expression is the stream itself: the value `period` steps
    // from now is the value now, so the buffer rotates and never changes.
    Some(b.stream_from(&pattern, |clock| clock))
}

/// The same clock, held as a counter rather than as a pattern.
///
/// Trades the period-sized buffer for one counter of type `T`, plus an
/// increment, a remainder and a comparison each step. Worth it when the period
/// is large; [`clk`] is cheaper for short ones and does no work per step at all.
///
/// Returns `None` on the same inputs as [`clk`], and also when the period does
/// not fit in `T` — a `u8` counter cannot count to 300.
///
/// ```
/// # use copilot_lang::Builder;
/// # use copilot_libs::clocks;
/// let b = Builder::new();
/// let every_thousandth = clocks::clk1::<u32>(&b, 1000, 0).unwrap();
/// # b.observe("tick", every_thousandth);
/// # b.finish().unwrap();
/// ```
pub fn clk1<T: ClockType>(b: &Builder, period: i64, phase: i64) -> Option<Stream<'_, bool>> {
    if period <= 0 || phase < 0 || phase >= period {
        return None;
    }
    // `from_bound` saturates, so a period `T` cannot represent comes back
    // changed. Catching it here keeps a silently wrong clock out of the spec.
    let modulus = T::from_bound(period);
    if modulus.to_bound() != period {
        return None;
    }

    let one = b.lit(T::from_bound(1));
    let modulus = b.lit(modulus);
    let counter = b.stream([T::from_bound(0)], |c| (c + one) % modulus);
    Some(counter.eq_(b.lit(T::from_bound(phase))))
}
