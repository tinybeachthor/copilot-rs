//! Bounded future-time linear temporal logic.
//!
//! A monitor cannot see the future, so these operators reach forward by reading
//! *ahead in a buffer* — `s.drop(1)` is the value `s` will take next, which is
//! knowable only because `s` was buffered that deeply when it was defined.
//!
//! The practical consequence: every operator here needs its argument to carry
//! at least `n + 1` initial values, and none of them works on a stream read
//! straight from an external variable. That is not a limitation of the
//! implementation but of monitoring: the environment has not produced its next
//! sample yet.
//!
//! ```
//! # use copilot_lang::Builder;
//! # use copilot_libs::ltl;
//! let b = Builder::new();
//! let raw = b.extern_::<bool>("armed");
//!
//! // Buffer three steps of history, so the window can reach two ahead of it.
//! let armed = b.append(&[false, false, false], raw);
//! let steady = ltl::always(2, armed);
//! # b.observe("steady", steady);
//! # b.finish().unwrap();
//! ```

use crate::utils::{conjoin, disjoin};
use copilot_lang::Stream;

/// Does `s` hold at the next step?
pub fn next<'a>(s: Stream<'a, bool>) -> Stream<'a, bool> {
    s.drop(1)
}

/// Does `s` hold at every step from now through `n` steps ahead?
pub fn always<'a>(n: u32, s: Stream<'a, bool>) -> Stream<'a, bool> {
    conjoin(n, s)
}

/// Does `s` hold at some step from now through `n` steps ahead?
pub fn eventually<'a>(n: u32, s: Stream<'a, bool>) -> Stream<'a, bool> {
    disjoin(n, s)
}

/// Does `s0` hold until `s1` does, within `n` steps?
///
/// `s1` must actually occur inside the window; this is the strong form.
pub fn until<'a>(n: u32, s0: Stream<'a, bool>, s1: Stream<'a, bool>) -> Stream<'a, bool> {
    if n == 0 {
        return s1;
    }
    (0..n)
        .map(|i| always(i, s0) & s1.drop(i + 1))
        .fold(s1, |acc, term| acc | term)
}

/// Does `s1` hold up to and including the step where `s0` releases it, or for
/// the whole window if `s0` never does?
///
/// The dual of [`until`], and the weak form: `s0` need not occur.
pub fn release<'a>(n: u32, s0: Stream<'a, bool>, s1: Stream<'a, bool>) -> Stream<'a, bool> {
    if n == 0 {
        return s1;
    }
    let released = (0..n)
        .map(|i| always(i, s1) & s0.drop(i))
        .reduce(|acc, term| acc | term)
        .expect("n is positive, so the window is non-empty");
    always(n, s1) | released
}
