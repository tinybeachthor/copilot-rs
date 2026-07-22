//! Running the encoding concretely, to check it against another engine.
//!
//! A prover is only as trustworthy as its encoding. Everything else in this
//! crate assumes that the SMT transition system means what the specification
//! means; nothing so far has *checked* it. [`evaluate`] closes that gap: it
//! pins the external inputs to concrete values, asks the solver what each
//! observer then equals, and hands back an answer directly comparable with what
//! the interpreter produces.
//!
//! The two are genuinely independent. The interpreter walks the IR over ring
//! buffers in Rust; the encoding is a shifting window of SMT terms with its own
//! hand-written guards for division, shifting and casts. Agreement across
//! randomly generated specifications is real evidence; a disagreement is a bug
//! in one of them, and either way it is worth knowing before trusting a proof.

use crate::encode::Encoding;
use crate::solver::{Answer, Session};
use crate::{Error, Settings};
use copilot_core::{Spec, Value};

/// What the encoding says a specification observes, given concrete inputs.
///
/// One entry per step, each holding the observers in declaration order.
///
/// Reports [`Error::Protocol`] if the constrained system turns out to be
/// unsatisfiable, which would mean the encoding contradicts itself: a
/// deterministic monitor with its inputs fixed has exactly one behaviour.
pub fn evaluate(
    spec: &Spec,
    settings: &Settings,
    inputs: &[Vec<(String, Value)>],
) -> Result<Vec<Vec<(String, Value)>>, Error> {
    let mut encoding = Encoding::new(spec, settings)?;
    let mut session = Session::start(settings.solver)?;
    let steps = inputs.len();

    for step in 0..steps {
        encoding.declare_step(step);
    }
    encoding.assert_initial();
    for step in 0..steps.saturating_sub(1) {
        encoding.assert_transition(step)?;
    }

    // Every observer at every step, named before the solver is asked anything,
    // so the whole run is one query rather than one per value.
    let mut terms = Vec::new();
    for step in 0..steps {
        for observer in &spec.observers {
            terms.push((step, observer, encoding.term_at(observer.expr, step)?));
        }
    }

    for (step, sample) in inputs.iter().enumerate() {
        for (name, value) in sample {
            encoding.assert_extern(name, value, step)?;
        }
    }
    session.send(&encoding.take())?;

    match session.check_sat()? {
        Answer::Sat => {}
        Answer::Unsat => {
            return Err(Error::Protocol(
                "the encoding is unsatisfiable with the inputs fixed, so it contradicts itself"
                    .into(),
            ));
        }
        Answer::Unknown => {
            return Err(Error::Protocol(
                "the solver could not evaluate the encoding concretely".into(),
            ));
        }
    }

    let mut observed = vec![Vec::new(); steps];
    for (step, observer, term) in terms {
        let value = crate::induct::read_value(&mut session, &observer.ty, &term)?;
        observed[step].push((observer.name.clone(), value));
    }
    Ok(observed)
}
