//! Metric temporal logic: temporal operators over a time interval rather than a
//! step count.
//!
//! Every operator takes a `clock` stream carrying the current time and a `dist`
//! saying how far the clock advances between samples. `dist` is what turns a
//! time bound into a step bound: the recursion unrolls `u / dist + 1` times,
//! which is the most samples the interval `[l, u]` can span.
//!
//! Two consequences worth knowing before reaching for these:
//!
//! - `dist` must be a *lower* bound on the clock's advance. Understate it and
//!   the unrolling is longer than needed — correct, but larger. Overstate it
//!   and the window is too short to reach the end of the interval, which is
//!   unsound.
//! - The future-time operators read ahead on both the clock and the property,
//!   so both must be buffered at least `u / dist + 1` deep, exactly as in
//!   [`crate::ltl`]. The past-time operators have no such requirement.

use crate::utils::ClockType;
use copilot_lang::Stream;

/// Takes the next step of an unrolling, or stops at the last one.
///
/// Rust evaluates arguments before the call it passes them to, so writing these
/// recursions the way upstream's lazy Haskell does would build the shifted
/// streams even on the step that discards them. That is one `drop` further than
/// the window reaches — an error outright when the argument came from an
/// external variable — and, for the past-time operators, a buffered stream the
/// monitor would carry and never read.
fn step<'a>(
    remaining: u32,
    next: impl FnOnce() -> Stream<'a, bool>,
    at_the_end: Stream<'a, bool>,
) -> Stream<'a, bool> {
    if remaining > 1 { next() } else { at_the_end }
}

/// How many samples an interval ending at `u` can span, given `dist`.
fn depth(u: i64, dist: i64) -> u32 {
    // A non-positive `dist` would mean the clock never advances, so no finite
    // unrolling reaches the end of the interval. One sample is the honest
    // answer: check only right now.
    if dist <= 0 {
        return 1;
    }
    ((u / dist) + 1).clamp(1, i64::from(u32::MAX)) as u32
}

/// Bounds for one operator, lifted into the clock's own type once.
struct Bounds<'a, T: ClockType> {
    lower: Stream<'a, T>,
    upper: Stream<'a, T>,
}

impl<'a, T: ClockType> Bounds<'a, T> {
    /// `[clock + l, clock + u]`, for the future-time operators.
    fn ahead(clock: Stream<'a, T>, l: i64, u: i64) -> Self {
        let b = clock.builder();
        Bounds {
            lower: clock + b.lit(T::from_bound(l)),
            upper: clock + b.lit(T::from_bound(u)),
        }
    }

    /// `[clock - u, clock - l]`, for the past-time operators.
    fn behind(clock: Stream<'a, T>, l: i64, u: i64) -> Self {
        let b = clock.builder();
        Bounds {
            lower: clock - b.lit(T::from_bound(u)),
            upper: clock - b.lit(T::from_bound(l)),
        }
    }
}

/// Does `s` hold at some sample whose time falls in `[l, u]` from now?
pub fn eventually<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::ahead(clock, l, u);
    eventually_from(clock, s, &bounds, depth(u, dist))
}

fn eventually_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(false);
    }
    let here = bounds.lower.le(c) & s;
    let later = step(
        k,
        || eventually_from(c.drop(1), s.drop(1), bounds, k - 1),
        b.lit(false),
    );
    c.le(bounds.upper) & (here | later)
}

/// Does `s` hold at every sample whose time falls in `[l, u]` from now?
pub fn always<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::ahead(clock, l, u);
    always_from(clock, s, &bounds, depth(u, dist))
}

fn always_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(true);
    }
    let here = bounds.lower.le(c).implies(s);
    let later = step(
        k,
        || always_from(c.drop(1), s.drop(1), bounds, k - 1),
        b.lit(true),
    );
    c.gt(bounds.upper) | (here & later)
}

/// Does `s0` hold until `s1` does, within `[l, u]` from now?
pub fn until<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::ahead(clock, l, u);
    until_from(clock, s0, s1, &bounds, depth(u, dist))
}

fn until_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(false);
    }
    let released = bounds.lower.le(c) & s1;
    let later = s0
        & step(
            k,
            || until_from(c.drop(1), s0.drop(1), s1.drop(1), bounds, k - 1),
            b.lit(false),
        );
    c.le(bounds.upper) & (released | later)
}

/// Does `s1` hold until `s0` releases it, within `[l, u]` from now?
///
/// The weak dual of [`until`]: `s0` need not occur.
pub fn release<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::ahead(clock, l, u);
    let outside = bounds.lower.gt(clock) | clock.gt(bounds.upper) | s1;
    let rest = release_from(clock.drop(1), s0, s1.drop(1), &bounds, depth(u, dist) - 1);
    outside & rest
}

fn release_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(true);
    }
    let here = (bounds.lower.le(c) & c.le(bounds.upper)).implies(s1);
    let stop = s0;
    let later = step(
        k,
        || release_from(c.drop(1), s0.drop(1), s1.drop(1), bounds, k - 1),
        b.lit(true),
    );
    here & (stop | later)
}

/// Was there a sample in `[l, u]` before now where `s1` held, with `s0` holding
/// at every sample since?
pub fn since<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::behind(clock, l, u);
    since_from(clock, s0, s1, &bounds, depth(u, dist))
}

fn since_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s0: Stream<'a, bool>,
    s1: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(false);
    }
    let met = c.le(bounds.upper) & s1;
    // Stepping into the past prepends: the clock reads zero before the trace
    // began, `s0` is vacuously true there and `s1` vacuously false.
    let earlier = s0
        & step(
            k,
            || {
                since_from(
                    b.append(&[T::from_bound(0)], c),
                    b.append(&[true], s0),
                    b.append(&[false], s1),
                    bounds,
                    k - 1,
                )
            },
            b.lit(false),
        );
    bounds.lower.le(c) & (met | earlier)
}

/// Did `s` hold at every sample in `[l, u]` before now?
pub fn always_been<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::behind(clock, l, u);
    always_been_from(clock, s, &bounds, depth(u, dist))
}

fn always_been_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(true);
    }
    let here = c.le(bounds.upper).implies(s);
    let earlier = step(
        k,
        || {
            always_been_from(
                b.append(&[T::from_bound(0)], c),
                b.append(&[true], s),
                bounds,
                k - 1,
            )
        },
        b.lit(true),
    );
    c.lt(bounds.lower) | (here & earlier)
}

/// Did `s` hold at some sample in `[l, u]` before now?
pub fn eventually_prev<'a, T: ClockType>(
    l: i64,
    u: i64,
    clock: Stream<'a, T>,
    dist: i64,
    s: Stream<'a, bool>,
) -> Stream<'a, bool> {
    let bounds = Bounds::behind(clock, l, u);
    eventually_prev_from(clock, s, &bounds, depth(u, dist))
}

fn eventually_prev_from<'a, T: ClockType>(
    c: Stream<'a, T>,
    s: Stream<'a, bool>,
    bounds: &Bounds<'a, T>,
    k: u32,
) -> Stream<'a, bool> {
    let b = c.builder();
    if k == 0 {
        return b.lit(false);
    }
    let here = c.le(bounds.upper) & s;
    let earlier = step(
        k,
        || {
            eventually_prev_from(
                b.append(&[T::from_bound(0)], c),
                b.append(&[false], s),
                bounds,
                k - 1,
            )
        },
        b.lit(false),
    );
    bounds.lower.le(c) & (here | earlier)
}
