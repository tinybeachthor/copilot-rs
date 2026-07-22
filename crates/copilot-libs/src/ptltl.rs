//! Past-time linear temporal logic.
//!
//! The past is what a monitor can actually see, so these are the operators most
//! runtime monitoring is written in. Each holds its whole history in a single
//! bit of state, which is why they stay within the constant-memory budget: the
//! recursions below are all of the form `out(t) = f(s(t), out(t-1))`.

use copilot_lang::Stream;

/// Did `s` hold at the previous step?
///
/// False at the first step, when there is no previous one.
pub fn previous<'a>(s: Stream<'a, bool>) -> Stream<'a, bool> {
    s.builder().append(&[false], s)
}

/// Has `s` held at every step so far, including now?
///
/// One bit of state: the conjunction of everything before now.
pub fn always_been<'a>(s: Stream<'a, bool>) -> Stream<'a, bool> {
    // `earlier(t)` is the conjunction of `s` over `[0, t)`, so it starts true
    // — an empty conjunction — and absorbs each step as it passes.
    let earlier = s.builder().stream([true], |earlier| s & earlier);
    s & earlier
}

/// Did `s` hold at some step so far, including now?
pub fn eventually_prev<'a>(s: Stream<'a, bool>) -> Stream<'a, bool> {
    let earlier = s.builder().stream([false], |earlier| s | earlier);
    s | earlier
}

/// Was there a step where `s2` held, with `s1` holding at every step since?
///
/// The strong form: `s2` must actually have occurred. `since(s1, s2)` at time
/// `t` means there is some `k <= t` with `s2(k)`, and `s1(j)` for every `j` in
/// `(k, t]`.
///
/// # Deviation from upstream
///
/// Upstream Copilot defines this as
/// `eventuallyPrev (s2 ==> alwaysBeen s1)`, which does not mean that. An
/// implication is true wherever its antecedent is false, so as soon as `s2` has
/// once been false, `eventuallyPrev` finds that step and the whole expression
/// is true forever after — regardless of `s1`. Under that definition
/// `since(s1, s2)` is true at every step of almost every trace, including
/// traces where `s1` never holds at all.
///
/// This implementation uses the standard recursion instead, and
/// `docs/deviations.md` records the difference. See
/// `crates/copilot-libs/tests/libs.rs` for the trace that separates them.
pub fn since<'a>(s1: Stream<'a, bool>, s2: Stream<'a, bool>) -> Stream<'a, bool> {
    // out(t) = s2(t) | (s1(t) & out(t-1)), with out(-1) false.
    let earlier = s1.builder().stream([false], |earlier| s2 | (s1 & earlier));
    s2 | (s1 & earlier)
}
