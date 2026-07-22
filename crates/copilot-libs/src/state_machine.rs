//! Transition-table state machines.

use copilot_lang::Stream;
use copilot_lang::Typed;
use copilot_lang::classes::Equatable;

/// One edge of a state machine: take it when the machine is in `from` and
/// `input` holds, landing in `to`.
pub struct Transition<'a, S> {
    /// The state this edge leaves.
    pub from: S,
    /// The condition that takes it.
    pub input: Stream<'a, bool>,
    /// The state it arrives at.
    pub to: S,
}

impl<'a, S> Transition<'a, S> {
    /// An edge from `from` to `to`, taken when `input` holds.
    pub fn new(from: S, input: Stream<'a, bool>, to: S) -> Self {
        Transition { from, input, to }
    }
}

/// A state machine as a stream of the state it is in.
///
/// Transitions are tried in order and the first match wins, so an earlier edge
/// shadows a later one that shares its source state. If no edge matches, the
/// machine goes to `bad` — except in `accepting` with `idle` holding, where it
/// stays put.
///
/// The state is one value of `S`, so this costs `size_of::<S>()` bytes plus one
/// comparison per edge, whatever the trace does.
///
/// ```
/// # use copilot_lang::Builder;
/// # use copilot_libs::state_machine::{Transition, state_machine};
/// let b = Builder::new();
/// let go = b.extern_::<bool>("go");
/// let stop = b.extern_::<bool>("stop");
///
/// // 0 idle, 1 running, 2 rejected.
/// let state = state_machine(
///     &b, 0u8, 1u8, 2u8,
///     !go & !stop,
///     &[
///         Transition::new(0, go, 1),
///         Transition::new(1, stop, 0),
///     ],
/// );
/// # b.observe("state", state);
/// # b.finish().unwrap();
/// ```
pub fn state_machine<'a, S: Typed + Equatable>(
    b: &'a copilot_lang::Builder,
    initial: S,
    accepting: S,
    bad: S,
    idle: Stream<'a, bool>,
    transitions: &[Transition<'a, S>],
) -> Stream<'a, S> {
    // `next` reads the machine's state *before* this step and decides where it
    // goes. It is built twice — once as the buffered stream's own transition
    // expression, once to hand back — but the arena interns expressions, so the
    // two builds are the same nodes and the monitor evaluates them once.
    let next = |previous: Stream<'a, S>| -> Stream<'a, S> {
        let stuck = previous
            .eq_(b.lit(accepting))
            .and(idle)
            .mux(b.lit(accepting), b.lit(bad));

        transitions.iter().rev().fold(stuck, |otherwise, edge| {
            previous
                .eq_(b.lit(edge.from))
                .and(edge.input)
                .mux(b.lit(edge.to), otherwise)
        })
    };

    let previous = b.stream([initial], next);
    next(previous)
}
