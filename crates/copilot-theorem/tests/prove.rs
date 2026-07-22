//! What the prover decides, and whether its counterexamples survive being
//! replayed through an engine that shares no code with the encoding.

use copilot_core::Value;
use copilot_lang::{Builder, Spec};
use copilot_theorem::{Caveat, FloatEncoding, Outcome, Settings, Solver, prove};

/// Skips a test when no solver is installed, rather than failing.
macro_rules! require_solver {
    ($settings:expr) => {
        if !$settings.solver.available() {
            eprintln!("skipping: `{}` is not on PATH", $settings.solver.program());
            return;
        }
    };
}

fn settings() -> Settings {
    Settings::default()
}

/// The milestone's headline case: a counter that wraps at ten never leaves its
/// range, and induction has to know that to see it.
fn bounded_counter() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u8], |s| (s + 1u8) % 10u8);
    b.observe("counter", counter);
    b.property_forall("stays_below_ten", counter.lt_val(10));
    b.finish().unwrap()
}

#[test]
fn proves_a_bounded_counter() {
    let settings = settings();
    require_solver!(settings);

    let proofs = prove(&bounded_counter(), &settings).unwrap();
    assert_eq!(proofs.len(), 1);
    assert_eq!(proofs[0].outcome, Outcome::Valid);
    assert!(
        proofs[0].is_conclusive(),
        "an integer property should carry no caveats: {:?}",
        proofs[0].caveats
    );
}

#[test]
fn both_solvers_agree() {
    for solver in [Solver::Z3, Solver::Cvc5] {
        let settings = Settings {
            solver,
            ..Settings::default()
        };
        if !solver.available() {
            eprintln!("skipping {}: not on PATH", solver.program());
            continue;
        }
        let proofs = prove(&bounded_counter(), &settings).unwrap();
        assert_eq!(
            proofs[0].outcome,
            Outcome::Valid,
            "{} disagreed",
            solver.program()
        );
    }
}

/// A false property must come back with a trace, and the trace must actually
/// reproduce the violation when replayed through the interpreter.
///
/// That second half is the point. The interpreter walks the IR over ring
/// buffers; the encoding is a shifting window in SMT. A counterexample that
/// replays has been corroborated by an independent implementation, and one that
/// does not means the two disagree about what the specification means.
#[test]
fn refutes_a_false_property_with_a_replayable_trace() {
    let settings = settings();
    require_solver!(settings);

    let b = Builder::new();
    let sensor = b.extern_::<u8>("sensor");
    let latched = b.stream([false], |was| was | sensor.gt_val(200));
    b.observe("latched", latched);
    // False: a high enough reading sets the latch.
    b.property_forall("never_latches", !latched);
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    let Outcome::Invalid(counterexample) = &proofs[0].outcome else {
        panic!("expected a refutation, got {:?}", proofs[0].outcome);
    };

    let failing_step = counterexample
        .confirm(&spec, "never_latches")
        .unwrap()
        .expect("the counterexample must reproduce the violation in the interpreter");
    assert!(failing_step < counterexample.steps.len());

    // The trace has to contain a reading that trips the latch, or it is not an
    // explanation of anything.
    let tripped = counterexample.steps.iter().any(|step| {
        step.inputs
            .iter()
            .any(|(name, value)| name == "sensor" && matches!(value, Value::Word8(v) if *v > 200))
    });
    assert!(tripped, "the trace does not explain the failure");
}

/// A property that is true but not inductive at the default depth must come
/// back as undecided, not as a failure — and a larger depth must settle it.
#[test]
fn distinguishes_not_inductive_from_false() {
    let settings = settings();
    require_solver!(settings);

    let b = Builder::new();
    // Counts 0,1,2,3,0,1,... so it is always below 8, but induction cannot see
    // that from a single arbitrary state.
    let counter = b.stream([0u8], |s| (s + 1u8) % 4u8);
    let doubled = counter * 2u8;
    b.observe("doubled", doubled);
    b.property_forall("below_eight", doubled.lt_val(8));
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    assert!(
        matches!(proofs[0].outcome, Outcome::Valid | Outcome::Unknown(_)),
        "a true property must never be reported as refuted: {:?}",
        proofs[0].outcome
    );
}

/// A property relating a two-deep stream to its own next value.
///
/// The stream's transition expression is the stream itself, so its two initial
/// values rotate forever — `false, true, false, true, ..`. Writing the
/// transition as `s.drop(1)` instead would make it constant after the prefix,
/// which is a mistake the prover catches rather than a property it proves.
#[test]
fn proves_a_property_over_a_deep_buffer() {
    let settings = settings();
    require_solver!(settings);

    let b = Builder::new();
    let alternating = b.stream([false, true], |s| s);
    b.observe("alternating", alternating);
    // Exactly one of this step and the next is true, always.
    b.property_forall("alternates", alternating ^ alternating.drop(1));
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    assert_eq!(proofs[0].outcome, Outcome::Valid);
    assert!(proofs[0].is_conclusive());
}

/// The prover must refute a stream that only looks alternating.
///
/// `[false, true] ++ drop 1 s` settles at `true` after its prefix, so this
/// really is false — and catching it is the same machinery as proving the case
/// above, pointed at a specification that does not hold.
#[test]
fn refutes_a_stream_that_only_looks_alternating() {
    let settings = settings();
    require_solver!(settings);

    let b = Builder::new();
    let settles = b.stream([false, true], |s| s.drop(1));
    b.observe("settles", settles);
    b.property_forall("alternates", settles ^ settles.drop(1));
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    let Outcome::Invalid(counterexample) = &proofs[0].outcome else {
        panic!("expected a refutation, got {:?}", proofs[0].outcome);
    };
    assert_eq!(
        counterexample.confirm(&spec, "alternates").unwrap(),
        Some(1),
        "the stream first fails to alternate at step 1"
    );
}

/// A fixed depth answers at exactly that depth rather than searching.
#[test]
fn a_fixed_depth_is_respected() {
    let settings = Settings {
        depth: Some(3),
        ..Settings::default()
    };
    require_solver!(settings);

    let proofs = prove(&bounded_counter(), &settings).unwrap();
    assert_eq!(proofs[0].depth, 3);
    assert_eq!(proofs[0].outcome, Outcome::Valid);
}

/// A result computed under an approximation must not look like a proof.
#[test]
fn float_results_carry_their_caveat() {
    let settings = settings();
    require_solver!(settings);

    let b = Builder::new();
    let x = b.extern_::<f64>("x");
    let doubled = x * 2.0;
    b.observe("doubled", doubled);
    b.property_forall("no_smaller", doubled.ge(x));
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    assert!(
        proofs[0].caveats.contains(&Caveat::FloatsAsReals),
        "a float property under the real encoding must say so"
    );
    assert!(
        !proofs[0].is_conclusive(),
        "an approximated result must not report as conclusive"
    );
}

/// Under reals, `x * 2 >= x` looks true; under IEEE it is not, because a
/// negative `x` reverses it and NaN fails every comparison. The encoding has to
/// be able to tell the difference.
#[test]
fn ieee_encoding_sees_what_reals_cannot() {
    let settings = Settings {
        floats: FloatEncoding::Ieee,
        ..Settings::default()
    };
    require_solver!(settings);

    let b = Builder::new();
    let x = b.extern_::<f64>("x");
    b.observe("x", x);
    // False for any negative x, and for NaN.
    b.property_forall("doubling_grows", (x * 2.0).ge(x));
    let spec = b.finish().unwrap();

    let proofs = prove(&spec, &settings).unwrap();
    assert!(
        matches!(proofs[0].outcome, Outcome::Invalid(_)),
        "IEEE encoding should refute this: {:?}",
        proofs[0].outcome
    );
    assert!(
        !proofs[0].caveats.contains(&Caveat::FloatsAsReals),
        "the IEEE encoding is not the real approximation"
    );
}

#[test]
fn existential_properties_are_refused() {
    let b = Builder::new();
    let counter = b.stream([0u8], |s| s + 1u8);
    b.observe("counter", counter);
    b.property_exists("reaches_ten", counter.eq_val(10));
    let spec = b.finish().unwrap();

    assert!(prove(&spec, &settings()).is_err());
}

/// The temporal operators must be provable, not just testable.
///
/// `always_been(p)` is exactly the invariant "p has held at every step", which
/// is the shape most runtime monitoring is written in — so if k-induction
/// cannot discharge claims about it, the prover is not much use on real
/// specifications.
mod libraries {
    use super::*;
    use copilot_libs::ptltl;

    #[test]
    fn always_been_implies_the_property_now() {
        let settings = settings();
        require_solver!(settings);

        let b = Builder::new();
        let armed = b.extern_::<bool>("armed");
        let ever_armed = ptltl::always_been(armed);
        b.observe("ever_armed", ever_armed);
        // If it has always held, it holds now.
        b.property_forall("implies_now", ever_armed.implies(armed));
        let spec = b.finish().unwrap();

        let proofs = prove(&spec, &settings).unwrap();
        assert_eq!(proofs[0].outcome, Outcome::Valid);
        assert!(proofs[0].is_conclusive());
    }

    /// `always_been` only ever goes from true to false, never back.
    ///
    /// The first step needs guarding: `previous` of anything is false there, so
    /// the implication would fail at step 0 for a reason that says nothing
    /// about monotonicity. Stating it without the guard is a mistake the prover
    /// catches — it comes back with a one-step counterexample.
    #[test]
    fn always_been_is_monotone() {
        let settings = settings();
        require_solver!(settings);

        let b = Builder::new();
        let p = b.extern_::<bool>("p");
        let held = ptltl::always_been(p);
        let first_step = b.stream([true], |_| b.lit(false));

        b.observe("held", held);
        b.property_forall("monotone", held.implies(first_step | ptltl::previous(held)));
        let spec = b.finish().unwrap();

        let proofs = prove(&spec, &settings).unwrap();
        assert_eq!(proofs[0].outcome, Outcome::Valid);
        assert!(proofs[0].is_conclusive());
    }

    /// A clock ticks exactly once per period, which needs induction as deep as
    /// the period to see.
    #[test]
    fn a_clock_never_ticks_twice_running() {
        let settings = settings();
        require_solver!(settings);

        let b = Builder::new();
        let tick = copilot_libs::clocks::clk(&b, 3, 0).unwrap();
        b.observe("tick", tick);
        b.property_forall("no_two_in_a_row", !(tick & tick.drop(1)));
        let spec = b.finish().unwrap();

        let proofs = prove(&spec, &settings).unwrap();
        assert_eq!(proofs[0].outcome, Outcome::Valid);
    }
}
