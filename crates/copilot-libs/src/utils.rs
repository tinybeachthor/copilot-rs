//! Folds over a bounded window of a stream's future.
//!
//! Upstream's `nfoldl1` and friends fold over `take n s`, the list of a
//! stream's successive tails. Here that is `s.drop(0) .. s.drop(n - 1)`, and
//! the fold happens while the specification is built, so what reaches the IR is
//! a fixed expression with no loop in it.

use copilot_lang::Stream;
use copilot_lang::Typed;
use copilot_lang::classes::*;

/// The first `n` tails of `s`: `[s, drop 1 s, .., drop (n-1) s]`.
///
/// Reading ahead only works as far as a stream's buffer reaches, so every
/// element beyond the first needs `s` to have been buffered accordingly. A
/// window past the end of the buffer is reported by
/// [`Builder::finish`](copilot_lang::Builder::finish) rather than here.
pub fn window<T: Typed>(n: u32, s: Stream<'_, T>) -> Vec<Stream<'_, T>> {
    (0..n).map(|i| s.drop(i)).collect()
}

/// Folds `f` over the first `n` tails of `s`, left to right.
///
/// Upstream's `nfoldl1`. Panics if `n` is zero, which would leave nothing to
/// fold and no value to return; every caller here derives `n` from a window
/// size it has already made positive.
pub fn nfoldl1<'a, T: Typed>(
    n: u32,
    f: impl Fn(Stream<'a, T>, Stream<'a, T>) -> Stream<'a, T>,
    s: Stream<'a, T>,
) -> Stream<'a, T> {
    window(n, s)
        .into_iter()
        .reduce(f)
        .expect("nfoldl1 needs a window of at least one step")
}

/// Folds `f` over the first `n` tails of `s`, starting from `initial`.
///
/// Upstream's `nfoldl`.
pub fn nfoldl<'a, T: Typed, U: Typed>(
    n: u32,
    f: impl Fn(Stream<'a, U>, Stream<'a, T>) -> Stream<'a, U>,
    initial: Stream<'a, U>,
    s: Stream<'a, T>,
) -> Stream<'a, U> {
    window(n, s).into_iter().fold(initial, f)
}

/// Conjunction over a window. `n` counts steps, so `n = 0` is just `s` now.
pub(crate) fn conjoin<'a>(n: u32, s: Stream<'a, bool>) -> Stream<'a, bool> {
    nfoldl1(n + 1, |a, b| a & b, s)
}

/// Disjunction over a window.
pub(crate) fn disjoin<'a>(n: u32, s: Stream<'a, bool>) -> Stream<'a, bool> {
    nfoldl1(n + 1, |a, b| a | b, s)
}

/// A clock's value type: an integer a specification can also name as a Rust
/// constant.
///
/// The metric operators in [`crate::mtl`] compare a clock stream against bounds
/// the user supplies as ordinary numbers, so the library has to be able to lift
/// one into the clock's own type.
pub trait ClockType: Typed + Numeric + Integral + Ordered + Equatable {
    /// Lifts a bound into this type, saturating rather than wrapping.
    ///
    /// A bound outside the clock's range is a specification error, not
    /// something to silently wrap: `always(0, 400, ..)` on a `u8` clock would
    /// otherwise become `always(0, 144, ..)` and quietly check the wrong
    /// interval. Callers compare against [`ClockType::to_bound`] to notice.
    fn from_bound(value: i64) -> Self;

    /// The value as a bound, for checking that `from_bound` did not saturate.
    fn to_bound(self) -> i64;
}

macro_rules! clock_types {
    ($($ty:ty),* $(,)?) => {
        $(
            impl ClockType for $ty {
                fn from_bound(value: i64) -> Self {
                    value.clamp(<$ty>::MIN as i64, <$ty>::MAX as i64) as $ty
                }

                fn to_bound(self) -> i64 {
                    self as i64
                }
            }
        )*
    };
}

// `u64` is excluded: its upper half does not fit in the `i64` bounds are given
// as, so the clamp above could not be written honestly for it.
clock_types!(i8, i16, i32, i64, u8, u16, u32);
