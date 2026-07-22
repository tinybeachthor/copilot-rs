//! Every operator is checked against a hand-computed trace.
//!
//! A temporal operator is exactly the kind of thing that looks right and is
//! off by one, so nothing here is asserted against another implementation of
//! the same idea — each expectation is written out step by step from the
//! operator's stated meaning.

use copilot_core::Value;
use copilot_interp::{Monitor, Samples};
use copilot_lang::{Builder, Spec};
use copilot_libs::{clocks, ltl, mtl, ptltl, state_machine, voting};

/// Runs a spec over the given samples and collects one observer's values.
fn observe(spec: &Spec, name: &str, samples: Vec<Samples>) -> Vec<Value> {
    let mut monitor = Monitor::new(spec).expect("spec must validate");
    samples
        .into_iter()
        .map(|mut s| {
            let step = monitor.step(&mut s).expect("step must succeed");
            step.observers
                .into_iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no observer named `{name}`"))
                .1
        })
        .collect()
}

/// Samples feeding one boolean external variable.
fn bools(name: &'static str, values: &[bool]) -> Vec<Samples> {
    values
        .iter()
        .map(|&v| Samples::none().with(name, Value::Bool(v)))
        .collect()
}

fn flags(values: &[bool]) -> Vec<Value> {
    values.iter().map(|&v| Value::Bool(v)).collect()
}

/// Builds a spec observing one boolean, driven by externals named `p` and `q`.
fn spec_of(build: impl for<'a> Fn(&'a Builder) -> copilot_lang::Stream<'a, bool>) -> Spec {
    let b = Builder::new();
    let out = build(&b);
    b.observe("out", out);
    b.finish().unwrap()
}

mod past_time {
    use super::*;

    const P: [bool; 6] = [false, true, true, false, true, true];

    #[test]
    fn previous_lags_by_one_and_starts_false() {
        let spec = spec_of(|b| ptltl::previous(b.extern_::<bool>("p")));
        assert_eq!(
            observe(&spec, "out", bools("p", &P)),
            //     p:  F      T     T     F      T     T
            flags(&[false, false, true, true, false, true])
        );
    }

    #[test]
    fn always_been_latches_off_at_the_first_failure() {
        let spec = spec_of(|b| ptltl::always_been(b.extern_::<bool>("p")));
        assert_eq!(
            observe(&spec, "out", bools("p", &P)),
            // False from the start, since p is false at step 0.
            flags(&[false; 6])
        );
    }

    #[test]
    fn always_been_holds_while_the_input_does() {
        let spec = spec_of(|b| ptltl::always_been(b.extern_::<bool>("p")));
        let trace = [true, true, true, false, true];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            flags(&[true, true, true, false, false])
        );
    }

    #[test]
    fn eventually_prev_latches_on_at_the_first_success() {
        let spec = spec_of(|b| ptltl::eventually_prev(b.extern_::<bool>("p")));
        assert_eq!(
            observe(&spec, "out", bools("p", &P)),
            flags(&[false, true, true, true, true, true])
        );
    }

    /// `s1 since s2`: `s2` must have happened, and `s1` must have held at every
    /// step after it.
    #[test]
    fn since_requires_the_trigger_then_continuous_support() {
        let b = Builder::new();
        let s1 = b.extern_::<bool>("p");
        let s2 = b.extern_::<bool>("q");
        b.observe("out", ptltl::since(s1, s2));
        let spec = b.finish().unwrap();

        //  step:  0      1      2      3      4      5
        //  s1:    F      T      T      T      F      T
        //  s2:    F      T      F      F      F      F
        let s1_trace = [false, true, true, true, false, true];
        let s2_trace = [false, true, false, false, false, false];
        let samples: Vec<Samples> = s1_trace
            .iter()
            .zip(&s2_trace)
            .map(|(&a, &c)| {
                Samples::none()
                    .with("p", Value::Bool(a))
                    .with("q", Value::Bool(c))
            })
            .collect();

        assert_eq!(
            observe(&spec, "out", samples),
            // 0: q never held        -> false
            // 1: q holds now         -> true
            // 2: p held since step 1 -> true
            // 3: still holding       -> true
            // 4: p lapsed            -> false
            // 5: p is back, but q has not recurred -> false
            flags(&[false, true, true, true, false, false])
        );
    }

    /// The trace separating this implementation from upstream's formula.
    ///
    /// Upstream defines `since s1 s2 = eventuallyPrev (s2 ==> alwaysBeen s1)`.
    /// Here `s2` never holds at all, so nothing can have happened "since" it —
    /// but the implication is vacuously true at every step, so upstream's
    /// version reports true from step 0 onwards. See `docs/deviations.md`.
    #[test]
    fn since_is_false_when_its_trigger_never_occurs() {
        let b = Builder::new();
        let s1 = b.extern_::<bool>("p");
        let s2 = b.lit(false);
        b.observe("out", ptltl::since(s1, s2));

        // Upstream's formula, built alongside for the contrast.
        b.observe(
            "upstream",
            ptltl::eventually_prev(s2.implies(ptltl::always_been(s1))),
        );
        let spec = b.finish().unwrap();

        let trace = [true, true, false, true];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            flags(&[false; 4]),
            "s2 never holds, so `since` cannot"
        );
        assert_eq!(
            observe(&spec, "upstream", bools("p", &trace)),
            flags(&[true; 4]),
            "upstream's formula is vacuously true throughout"
        );
    }
}

mod future_time {
    use super::*;

    /// Buffers a boolean external so future operators have something to read
    /// ahead into. The result lags the input by `depth` steps.
    fn buffered<'a>(b: &'a Builder, name: &str, depth: usize) -> copilot_lang::Stream<'a, bool> {
        let raw = b.extern_::<bool>(name);
        b.append(&vec![false; depth], raw)
    }

    #[test]
    fn next_reads_one_step_ahead_of_a_buffered_stream() {
        let b = Builder::new();
        let delayed = buffered(&b, "p", 1);
        b.observe("out", ltl::next(delayed));
        let spec = b.finish().unwrap();

        // `delayed` is [F, p0, p1, ..]; `next delayed` is [p0, p1, ..].
        let trace = [true, false, true, true];
        assert_eq!(observe(&spec, "out", bools("p", &trace)), flags(&trace));
    }

    #[test]
    fn always_requires_the_whole_window() {
        let b = Builder::new();
        let delayed = buffered(&b, "p", 2);
        b.observe("out", ltl::always(2, delayed));
        let spec = b.finish().unwrap();

        // delayed = [F, F, p0, p1, p2, ..]; the window at t is
        // delayed[t], delayed[t+1], delayed[t+2].
        let trace = [true, true, true, false, true, true];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            // t=0: F,F,p0 -> false     t=3: p1,p2,p3 -> false
            // t=1: F,p0,p1 -> false    t=4: p2,p3,p4 -> false
            // t=2: p0,p1,p2 -> true    t=5: p3,p4,p5 -> false
            flags(&[false, false, true, false, false, false])
        );
    }

    #[test]
    fn eventually_needs_only_one_step_of_the_window() {
        let b = Builder::new();
        let delayed = buffered(&b, "p", 2);
        b.observe("out", ltl::eventually(2, delayed));
        let spec = b.finish().unwrap();

        let trace = [false, true, false, false, false, false];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            // p1 is the only true value; it sits at delayed[3], so the windows
            // starting at t=1, 2 and 3 all contain it.
            flags(&[false, true, true, true, false, false])
        );
    }

    #[test]
    fn until_wants_the_release_to_actually_arrive() {
        let b = Builder::new();
        let p = buffered(&b, "p", 2);
        let q = buffered(&b, "q", 2);
        b.observe("out", ltl::until(2, p, q));
        let spec = b.finish().unwrap();

        // q never holds, so `until` is false however long p holds.
        let samples: Vec<Samples> = (0..5)
            .map(|_| {
                Samples::none()
                    .with("p", Value::Bool(true))
                    .with("q", Value::Bool(false))
            })
            .collect();
        assert_eq!(observe(&spec, "out", samples), flags(&[false; 5]));
    }

    #[test]
    fn release_holds_vacuously_when_nothing_releases() {
        let b = Builder::new();
        let p = buffered(&b, "p", 2);
        let q = buffered(&b, "q", 2);
        b.observe("out", ltl::release(2, p, q));
        let spec = b.finish().unwrap();

        // q holds throughout the buffered window only from t=2 onwards, since
        // the first two buffered values are false.
        let samples: Vec<Samples> = (0..6)
            .map(|_| {
                Samples::none()
                    .with("p", Value::Bool(false))
                    .with("q", Value::Bool(true))
            })
            .collect();
        assert_eq!(
            observe(&spec, "out", samples),
            flags(&[false, false, true, true, true, true])
        );
    }
}

mod clock {
    use super::*;

    #[test]
    fn a_pattern_clock_repeats_its_period() {
        let b = Builder::new();
        b.observe("out", clocks::clk(&b, 3, 1).unwrap());
        let spec = b.finish().unwrap();

        assert_eq!(
            observe(&spec, "out", vec![Samples::none(); 7]),
            flags(&[false, true, false, false, true, false, false])
        );
    }

    /// The counter form must agree with the pattern form step for step.
    #[test]
    fn the_counter_clock_agrees_with_the_pattern_clock() {
        for (period, phase) in [(1, 0), (2, 0), (2, 1), (3, 2), (5, 3)] {
            let b = Builder::new();
            b.observe(
                "pattern",
                clocks::clk(&b, period as usize, phase as usize).unwrap(),
            );
            b.observe("counter", clocks::clk1::<u32>(&b, period, phase).unwrap());
            let spec = b.finish().unwrap();

            let mut monitor = Monitor::new(&spec).unwrap();
            for step in 0..12 {
                let observed = monitor.step(&mut Samples::none()).unwrap();
                let pattern = &observed.observers[0].1;
                let counter = &observed.observers[1].1;
                assert_eq!(
                    pattern, counter,
                    "period {period}, phase {phase}, step {step}"
                );
            }
        }
    }

    #[test]
    fn a_phase_outside_the_period_is_not_a_clock() {
        let b = Builder::new();
        assert!(clocks::clk(&b, 3, 3).is_none());
        assert!(clocks::clk(&b, 0, 0).is_none());
        assert!(clocks::clk1::<u32>(&b, 3, 3).is_none());
        // 300 does not fit in the counter's own type.
        assert!(clocks::clk1::<u8>(&b, 300, 0).is_none());
    }
}

mod vote {
    use super::*;

    fn sensors<'a>(b: &'a Builder, names: &[&str]) -> Vec<copilot_lang::Stream<'a, u8>> {
        names.iter().map(|n| b.extern_::<u8>(n)).collect()
    }

    fn readings(values: &[[u8; 5]]) -> Vec<Samples> {
        values
            .iter()
            .map(|row| {
                ["a", "b", "c", "d", "e"]
                    .iter()
                    .zip(row)
                    .fold(Samples::none(), |s, (name, &v)| {
                        s.with(name, Value::Word8(v))
                    })
            })
            .collect()
    }

    #[test]
    fn a_real_majority_is_found_and_confirmed() {
        let b = Builder::new();
        let votes = sensors(&b, &["a", "b", "c", "d", "e"]);
        let candidate = voting::majority(&votes).unwrap();
        b.observe("candidate", candidate);
        b.observe("agreed", voting::a_majority(&votes, candidate).unwrap());
        let spec = b.finish().unwrap();

        let trace = readings(&[
            [7, 7, 7, 1, 2], // 7 has three of five
            [1, 2, 3, 4, 5], // no majority at all
            [4, 4, 4, 4, 4], // unanimous
            [1, 1, 2, 2, 3], // two pairs, no majority
        ]);

        assert_eq!(
            observe(&spec, "agreed", trace.clone()),
            flags(&[true, false, true, false])
        );
        assert_eq!(
            observe(&spec, "candidate", trace)[0],
            Value::Word8(7),
            "the candidate is the true majority when one exists"
        );
    }

    /// The point of the second pass: without it, a candidate from a trace with
    /// no majority looks like a real answer.
    #[test]
    fn a_candidate_without_a_majority_is_rejected() {
        let b = Builder::new();
        let votes = sensors(&b, &["a", "b", "c", "d", "e"]);
        let candidate = voting::majority(&votes).unwrap();
        b.observe("agreed", voting::a_majority(&votes, candidate).unwrap());
        let spec = b.finish().unwrap();

        // Two against two against one: nothing holds three of five.
        assert_eq!(
            observe(&spec, "agreed", readings(&[[1, 1, 2, 2, 9]])),
            flags(&[false])
        );
    }

    #[test]
    fn no_votes_is_not_a_vote() {
        let b = Builder::new();
        let empty: Vec<copilot_lang::Stream<'_, u8>> = Vec::new();
        assert!(voting::majority(&empty).is_none());
        assert!(voting::a_majority(&empty, b.lit(0u8)).is_none());
    }
}

mod machine {
    use super::*;
    use state_machine::{Transition, state_machine};

    /// 0 idle, 1 running, 2 rejected.
    fn build(b: &Builder) -> copilot_lang::Stream<'_, u8> {
        let go = b.extern_::<bool>("p");
        let stop = b.extern_::<bool>("q");
        state_machine(
            b,
            0u8,
            0u8,
            2u8,
            !go & !stop,
            &[
                Transition::new(0, go, 1),
                Transition::new(1, stop, 0),
                Transition::new(1, !stop, 1),
            ],
        )
    }

    fn drive(go: &[bool], stop: &[bool]) -> Vec<Value> {
        let b = Builder::new();
        let state = build(&b);
        b.observe("out", state);
        let spec = b.finish().unwrap();

        let samples: Vec<Samples> = go
            .iter()
            .zip(stop)
            .map(|(&g, &s)| {
                Samples::none()
                    .with("p", Value::Bool(g))
                    .with("q", Value::Bool(s))
            })
            .collect();
        observe(&spec, "out", samples)
    }

    #[test]
    fn it_follows_its_transitions() {
        let states = drive(
            &[false, true, false, false, false],
            &[false, false, false, true, false],
        );
        assert_eq!(
            states,
            [0u8, 1, 1, 0, 0].map(Value::Word8).to_vec(),
            "idle, start, keep running, stop, idle"
        );
    }

    #[test]
    fn an_unhandled_input_rejects() {
        // Idle with `stop` asserted matches no transition, and idle is the
        // accepting state but the machine is not idle-quiet, so it rejects.
        let states = drive(&[false, false], &[true, false]);
        assert_eq!(states[0], Value::Word8(2));
    }
}

mod metric {
    use super::*;

    /// A clock advancing by one per step, buffered so the future operators can
    /// read ahead.
    fn clock(b: &Builder, depth: usize) -> copilot_lang::Stream<'_, i32> {
        let counter = b.stream([0i32], |c| c + 1i32);
        b.append(&vec![0i32; depth], counter)
    }

    #[test]
    fn eventually_prev_looks_back_over_the_interval() {
        let b = Builder::new();
        let clk = b.stream([0i32], |c| c + 1i32);
        let p = b.extern_::<bool>("p");
        b.observe("out", mtl::eventually_prev(0, 2, clk, 1, p));
        let spec = b.finish().unwrap();

        // p holds only at step 1; the window [t-2, t] contains step 1 for
        // t = 1, 2 and 3.
        let trace = [false, true, false, false, false, false];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            flags(&[false, true, true, true, false, false])
        );
    }

    #[test]
    fn always_been_requires_the_whole_interval() {
        let b = Builder::new();
        let clk = b.stream([0i32], |c| c + 1i32);
        let p = b.extern_::<bool>("p");
        b.observe("out", mtl::always_been(0, 1, clk, 1, p));
        let spec = b.finish().unwrap();

        let trace = [true, true, false, true, true];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            // The window is the current step and the one before it.
            flags(&[true, true, false, false, true])
        );
    }

    #[test]
    fn eventually_reads_ahead_over_the_interval() {
        let b = Builder::new();
        let clk = clock(&b, 2);
        let raw = b.extern_::<bool>("p");
        let p = b.append(&[false, false], raw);
        b.observe("out", mtl::eventually(0, 2, clk, 1, p));
        let spec = b.finish().unwrap();

        // The buffered p is [F, F, p0, p1, ..]; a true at p0 sits at index 2,
        // reachable from windows starting at 0, 1 and 2.
        let trace = [true, false, false, false, false];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            flags(&[true, true, true, false, false])
        );
    }

    /// A non-positive `dist` would divide by zero when sizing the window.
    #[test]
    fn a_clock_that_never_advances_still_builds() {
        let b = Builder::new();
        let clk = b.stream([0i32], |c| c);
        let p = b.extern_::<bool>("p");
        b.observe("out", mtl::eventually_prev(0, 5, clk, 0, p));
        let spec = b.finish().unwrap();

        // Only the current step is checked.
        let trace = [false, true, false];
        assert_eq!(
            observe(&spec, "out", bools("p", &trace)),
            flags(&[false, true, false])
        );
    }
}

/// The libraries must not quietly cost more than they look like they cost.
mod budget {
    use super::*;

    #[test]
    fn past_time_operators_cost_one_bit_of_state_each() {
        let b = Builder::new();
        let p = b.extern_::<bool>("p");
        b.observe("a", ptltl::always_been(p));
        b.observe("b", ptltl::eventually_prev(p));
        b.observe("c", ptltl::previous(p));
        let spec = b.finish().unwrap();

        let footprint = copilot_core::resources(&spec);
        assert_eq!(
            footprint.state_bytes, 3,
            "three operators, one bool of state each"
        );
    }

    /// A bounded-future operator unrolls its window, so its cost grows with it.
    /// This is the fact the crate docs warn about; pinning it keeps the warning
    /// honest.
    #[test]
    fn future_operators_grow_with_their_window() {
        let cost_of = |n: u32| {
            let b = Builder::new();
            let raw = b.extern_::<bool>("p");
            let buffered = b.append(&vec![false; n as usize], raw);
            b.observe("out", ltl::always(n, buffered));
            copilot_core::cost(&b.finish().unwrap()).nodes_shared
        };

        assert!(
            cost_of(8) > cost_of(2),
            "a wider window must show up as more work per step"
        );
    }
}
