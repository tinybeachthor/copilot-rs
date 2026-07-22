//! k-induction over the encoded transition system.
//!
//! Two queries per property.
//!
//! **Base.** Unroll `k` steps from the initial state and look for one where the
//! property fails. A model here is a genuine counterexample: the states are
//! reachable by construction.
//!
//! **Step.** From `k` arbitrary consecutive states in which the property holds,
//! ask whether the next one can fail. No model means the property is inductive
//! at this depth, and base plus step together make it invariant.
//!
//! The technique is sound and incomplete. `unsat` on both is a proof. `sat` on
//! the base is a refutation. `sat` on the step alone means only that the
//! property is not inductive at this `k` — the states it found may be
//! unreachable — so that is reported as unknown rather than as a failure, and a
//! larger `k` may still settle it.

use crate::encode::{Encoding, extern_var};
use crate::solver::{Answer, Session};
use crate::{Caveat, Counterexample, Error, Outcome, Proof, Settings, Step};
use copilot_core::{Prop, Spec, Type, Value};

/// Proves every property in a specification.
pub fn prove(spec: &Spec, settings: &Settings) -> Result<Vec<Proof>, Error> {
    spec.require_universal()?;
    spec.properties
        .iter()
        .map(|property| {
            let Prop::Forall(expr) = property.prop else {
                unreachable!("require_universal rejected the alternative");
            };
            prove_one(spec, settings, &property.name, expr)
        })
        .collect()
}

fn prove_one(
    spec: &Spec,
    settings: &Settings,
    name: &str,
    expr: copilot_core::ExprId,
) -> Result<Proof, Error> {
    // A fixed depth means "answer at exactly this k"; otherwise the depth is
    // searched upwards. Deepening matters in both directions: a longer base case
    // reaches counterexamples further from the initial state, and a longer
    // induction hypothesis rules out more unreachable states. Answering only at
    // the buffer-depth heuristic would report "not inductive" for properties
    // that a slightly deeper attempt settles, and would miss any counterexample
    // that takes more than that many steps to arrive.
    let (first, last) = match settings.depth {
        Some(fixed) => (fixed.max(1), fixed.max(1)),
        None => (1, settings.max_depth.max(default_depth(spec))),
    };

    let mut encoding = Encoding::new(spec, settings)?;
    let mut session = Session::start(settings.solver)?;

    encoding.declare_step(0);
    let mut terms = vec![encoding.term_at(expr, 0)?];
    session.send(&encoding.take())?;

    let mut undecided = format!("no answer within depth {last}");

    for depth in 1..=last {
        // Extend the unrolling by one step. Declarations and transitions
        // accumulate across iterations, so deepening costs one more step rather
        // than a fresh encoding.
        encoding.declare_step(depth);
        encoding.assert_transition(depth - 1)?;
        terms.push(encoding.term_at(expr, depth)?);
        session.send(&encoding.take())?;

        if depth < first {
            continue;
        }

        // -- base: is the property already violated within `depth` steps? ----
        session.push()?;
        encoding.assert_initial();
        session.send(&encoding.take())?;
        let violated = terms[..depth]
            .iter()
            .map(|term| format!("(not {term})"))
            .collect::<Vec<_>>()
            .join(" ");
        session.send(&format!("(assert (or {violated}))"))?;

        let base = session.check_sat()?;
        let counterexample = match base {
            Answer::Sat => Some(read_counterexample(spec, &mut session, depth)?),
            _ => None,
        };
        session.pop()?;

        if let Some(counterexample) = counterexample {
            return Ok(Proof {
                property: name.to_string(),
                outcome: Outcome::Invalid(counterexample),
                caveats: encoding.caveats(),
                depth,
            });
        }
        if base == Answer::Unknown {
            return Ok(Proof {
                property: name.to_string(),
                outcome: Outcome::Unknown("the solver could not decide the base case".into()),
                caveats: encoding.caveats(),
                depth,
            });
        }

        // -- step: can it fail after `depth` steps of holding? ---------------
        session.push()?;
        for term in &terms[..depth] {
            session.send(&format!("(assert {term})"))?;
        }
        session.send(&format!("(assert (not {}))", terms[depth]))?;
        let step = session.check_sat()?;
        session.pop()?;

        match step {
            Answer::Unsat => {
                return Ok(Proof {
                    property: name.to_string(),
                    outcome: Outcome::Valid,
                    caveats: encoding.caveats(),
                    depth,
                });
            }
            Answer::Sat => {
                undecided = format!(
                    "holds for the first {depth} step(s) but is not inductive at that depth"
                );
            }
            Answer::Unknown => {
                return Ok(Proof {
                    property: name.to_string(),
                    outcome: Outcome::Unknown(
                        "the solver could not decide the inductive step".into(),
                    ),
                    caveats: encoding.caveats(),
                    depth,
                });
            }
        }
    }

    Ok(Proof {
        property: name.to_string(),
        outcome: Outcome::Unknown(format!("{undecided}; a larger depth may settle it")),
        caveats: encoding.caveats(),
        depth: last,
    })
}

/// The deepest buffer in the specification, and at least one.
///
/// Upstream's heuristic: a property relating a stream to its own past needs at
/// least as many steps as that stream remembers before induction has anything
/// to work with.
fn default_depth(spec: &Spec) -> usize {
    spec.streams
        .iter()
        .map(|stream| stream.buffer.len())
        .max()
        .unwrap_or(1)
        .max(1)
}

/// Reads the external inputs of a failing trace out of the model.
///
/// Only the inputs are recovered. Everything else the monitor does is a
/// function of them, so replaying these through the interpreter reproduces the
/// whole failing run — and does so through an engine that shares no code with
/// the encoding, which is what makes the counterexample worth believing.
fn read_counterexample(
    spec: &Spec,
    session: &mut Session,
    depth: usize,
) -> Result<Counterexample, Error> {
    let externs = spec.arena.externs().to_vec();

    let mut steps = Vec::with_capacity(depth);
    for step in 0..depth {
        let mut inputs = Vec::new();
        for (name, ty) in &externs {
            let value = read_value(session, ty, &extern_var(name, step))?;
            inputs.push((name.clone(), value));
        }
        steps.push(Step { inputs });
    }
    Ok(Counterexample { steps })
}

/// Reads one value, taking an aggregate apart into scalar queries.
pub(crate) fn read_value(session: &mut Session, ty: &Type, term: &str) -> Result<Value, Error> {
    match ty {
        Type::Array { elem, len } => {
            let mut values = Vec::with_capacity(*len);
            for index in 0..*len {
                let element = format!("(select {term} {})", bits32(index as u32));
                values.push(read_value(session, elem, &element)?);
            }
            Ok(Value::Array(values))
        }
        Type::Struct(definition) => {
            let mut fields = Vec::with_capacity(definition.fields.len());
            for (field, field_ty) in &definition.fields {
                let selector = format!("({}-{field} {term})", definition.name);
                fields.push((field.clone(), read_value(session, field_ty, &selector)?));
            }
            Ok(Value::Struct {
                name: definition.name.clone(),
                fields,
            })
        }
        scalar => {
            let answers = session.get_values(&[term.to_string()])?;
            crate::sexpr::decode(scalar, &answers[0])
        }
    }
}

fn bits32(value: u32) -> String {
    let mut digits = String::from("#b");
    for position in (0..32).rev() {
        digits.push(if value >> position & 1 == 1 { '1' } else { '0' });
    }
    digits
}

/// Which caveats make an outcome less than a proof.
pub(crate) fn is_conclusive(outcome: &Outcome, caveats: &[Caveat]) -> bool {
    matches!(outcome, Outcome::Valid) && caveats.is_empty()
}
