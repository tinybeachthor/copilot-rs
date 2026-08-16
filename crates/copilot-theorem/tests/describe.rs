//! `Proof::describe` (M8): a counterexample read against the property's own
//! text, not a bare name plus a step count.
//!
//! Built by hand rather than through [`prove`](copilot_theorem::prove), so
//! this runs with no solver installed — the rendering has nothing to do with
//! how the [`Proof`] was produced.

use copilot_core::Value;
use copilot_lang::{Builder, Spec};
use copilot_theorem::{Caveat, Counterexample, Outcome, Proof, Step};

/// `counter = [0] ++ (counter + 1)`, with a property that is actually false
/// past the ninth step, so the shape below is a plausible refutation.
fn counter_with_property() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u8], |s| s + 1u8);
    b.observe("counter", counter);
    b.property_forall("stays_below_ten", counter.lt_val(10));
    b.finish().unwrap()
}

#[test]
fn a_refutation_shows_the_property_text_and_the_inputs_by_name() {
    let spec = counter_with_property();
    let proof = Proof {
        property: "stays_below_ten".to_string(),
        outcome: Outcome::Invalid(Counterexample {
            steps: vec![Step { inputs: vec![] }; 11],
        }),
        caveats: Vec::new(),
        depth: 11,
    };

    let text = proof.describe(&spec);
    // The property's own expression, not just its name -- this is the whole
    // point: a reader sees what was claimed, not only that it failed.
    assert!(
        text.contains("stays_below_ten: drop 0 s0 < 10"),
        "missing the property's text:\n{text}"
    );
    assert!(text.contains("step 0:"));
    assert!(text.contains("step 10:"));
}

/// External inputs print by declaration name and value, the way a spec's own
/// externs are named -- not as a solver's internal term names.
#[test]
fn external_inputs_print_by_name_and_value() {
    let b = Builder::new();
    let raw = b.extern_::<f32>("temperature");
    b.observe("celsius", raw);
    b.property_forall("bounded", raw.lt_val(100.0));
    let spec = b.finish().unwrap();

    let proof = Proof {
        property: "bounded".to_string(),
        outcome: Outcome::Invalid(Counterexample {
            steps: vec![Step {
                inputs: vec![("temperature".to_string(), Value::Float(150.0))],
            }],
        }),
        caveats: vec![Caveat::FloatsAsReals],
        depth: 1,
    };

    let text = proof.describe(&spec);
    assert!(text.contains("temperature = 150.0"), "{text}");
    assert!(
        text.contains("caveat: floats were encoded as reals"),
        "{text}"
    );
}

#[test]
fn a_proof_with_no_counterexample_says_so_without_a_step_list() {
    let spec = counter_with_property();
    let proof = Proof {
        property: "stays_below_ten".to_string(),
        outcome: Outcome::Valid,
        caveats: Vec::new(),
        depth: 3,
    };

    let text = proof.describe(&spec);
    assert!(text.contains("stays_below_ten: drop 0 s0 < 10"));
    assert!(text.contains("proved"));
    assert!(!text.contains("step "));
}
