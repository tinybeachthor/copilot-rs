//! Boyer–Moore majority vote.
//!
//! Two passes over a fixed set of redundant inputs — the classic use is
//! agreeing on a value across replicated sensors. Both passes unroll while the
//! specification is built, so what reaches the IR is straight-line and the
//! monitor holds no per-vote state.
//!
//! [`majority`] alone is not enough. It returns *a* candidate, and that
//! candidate is only the true majority if one exists at all; with no majority
//! it returns an arbitrary input. [`a_majority`] is the second pass that says
//! whether to believe it, and safety-relevant code should always run both.
//!
//! ```
//! # use copilot_lang::Builder;
//! # use copilot_libs::voting;
//! let b = Builder::new();
//! let sensors: Vec<_> = ["a", "b", "c"].iter().map(|n| b.extern_::<u8>(n)).collect();
//!
//! let candidate = voting::majority(&sensors).unwrap();
//! let trustworthy = voting::a_majority(&sensors, candidate).unwrap();
//! # b.observe("candidate", candidate);
//! # b.observe("agreed", trustworthy);
//! # b.finish().unwrap();
//! ```

use copilot_lang::Stream;
use copilot_lang::Typed;
use copilot_lang::classes::Equatable;

/// The Boyer–Moore candidate: the only value that *could* hold a strict
/// majority.
///
/// Correct only when a majority exists. When none does, the result is one of
/// the inputs but means nothing — check it with [`a_majority`].
///
/// Returns `None` for an empty list of votes, which has no candidate.
///
/// # Aggregates
///
/// Votes must be scalars, since the algorithm compares them and the IR
/// restricts equality to scalar types. Voting on a struct means voting on the
/// field that matters; see `docs/deviations.md`.
pub fn majority<'a, T: Typed + Equatable>(votes: &[Stream<'a, T>]) -> Option<Stream<'a, T>> {
    let (first, rest) = votes.split_first()?;
    let b = first.builder();

    let mut candidate = *first;
    let mut count = b.lit(1u32);

    for (i, vote) in rest.iter().enumerate() {
        let exhausted = count.eq_val(0);
        let next_candidate = exhausted.mux(*vote, candidate);

        // The count is deliberately not updated on the final vote: nothing
        // reads it afterwards, and leaving it out keeps that much arithmetic
        // out of every step. Upstream stops here too.
        if i + 1 < rest.len() {
            // Compares against the *outgoing* candidate, before replacement.
            let agrees = exhausted | vote.eq_(candidate);
            count = agrees.mux(count + 1u32, count - 1u32);
        }
        candidate = next_candidate;
    }

    Some(candidate)
}

/// Does `candidate` hold a strict majority of the votes?
///
/// The second Boyer–Moore pass. Counts how many votes match and compares
/// against half, so it is meaningful for any candidate, not only the one
/// [`majority`] returned.
///
/// Returns `None` for an empty list of votes.
pub fn a_majority<'a, T: Typed + Equatable>(
    votes: &[Stream<'a, T>],
    candidate: Stream<'a, T>,
) -> Option<Stream<'a, bool>> {
    if votes.is_empty() {
        return None;
    }
    let b = candidate.builder();

    let mut agreeing = b.lit(0u32);
    for vote in votes {
        agreeing = vote.eq_(candidate).mux(agreeing + 1u32, agreeing);
    }

    // `2 * agreeing > total` rather than `agreeing > total / 2`, which would
    // round down and call 2 of 5 a majority.
    Some((agreeing * 2u32).gt_val(votes.len() as u32))
}
