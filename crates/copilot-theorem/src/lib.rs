//! SMT-based k-induction for copilot-rs specifications.
//!
//! Discharges the `Property` claims in a specification ahead of time, so that
//! what ships is a monitor whose invariants have been proved rather than
//! tested.
//!
//! ```no_run
//! use copilot_lang::Builder;
//! use copilot_theorem::{Outcome, Settings, prove};
//!
//! let b = Builder::new();
//! let counter = b.stream([0u8], |s| (s + 1u8) % 10u8);
//! b.property_forall("stays_below_ten", counter.lt_val(10));
//! let spec = b.finish()?;
//!
//! for proof in prove(&spec, &Settings::default())? {
//!     assert!(matches!(proof.outcome, Outcome::Valid));
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What a result means
//!
//! | Outcome | Meaning |
//! |---|---|
//! | [`Outcome::Valid`] | Proved, for every trace and every input |
//! | [`Outcome::Invalid`] | Refuted, with a trace that reaches the violation |
//! | [`Outcome::Unknown`] | Neither — most often "not inductive at this depth" |
//!
//! `Unknown` is the interesting one. k-induction is sound and incomplete, so it
//! is a statement about the proof attempt rather than about the property: the
//! inductive step may have found a state that no run can actually reach. Try a
//! larger [`Settings::depth`], or strengthen the property.
//!
//! # Caveats are part of the answer
//!
//! Some encodings are approximations, and a result computed under one is not a
//! proof. Rather than hiding that, every [`Proof`] carries the
//! [`Caveat`]s that applied, and [`Proof::is_conclusive`] is false whenever any
//! did. A caller that ignores caveats cannot accidentally treat an
//! approximation as a guarantee.
//!
//! # Requirements
//!
//! A solver on `PATH` — Z3 or cvc5. Nothing links against one, so a solver is
//! something the user has or does not; [`Solver::available`] says which.

mod check;
mod encode;
mod induct;
mod sexpr;
mod solver;

pub use check::evaluate;
pub use solver::Answer;

use copilot_core::{Spec, Value};
use std::fmt;

/// Result alias for proving.
pub type Result<T> = std::result::Result<T, Error>;

/// Which solver to drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Solver {
    /// Z3.
    #[default]
    Z3,
    /// cvc5.
    Cvc5,
}

/// How to encode floating-point values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FloatEncoding {
    /// Mathematical reals: fast, and wrong in both directions.
    ///
    /// Reals have no NaN, no infinity, no overflow and no rounding, so a
    /// property can be proved under them and still fail on a real machine — and
    /// a counterexample can be spurious. Attaches [`Caveat::FloatsAsReals`],
    /// which makes [`Proof::is_conclusive`] false.
    #[default]
    Reals,
    /// IEEE-754, through SMT-LIB's floating-point theory.
    ///
    /// Exact, and much slower. Worth it when the property is actually about
    /// floating-point behaviour rather than about the logic around it.
    Ieee,
}

/// How to run the prover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Which solver to drive.
    pub solver: Solver,
    /// Fix the induction depth instead of searching for one.
    ///
    /// Left unset, depths are tried from 1 upwards to [`Settings::max_depth`],
    /// which is what lets the prover both find counterexamples further from the
    /// initial state and settle properties that are not inductive at the first
    /// depth tried.
    pub depth: Option<usize>,
    /// How deep the search goes before giving up.
    ///
    /// Raised to the specification's deepest buffer when that is larger, since
    /// a property relating a stream to its own past cannot become inductive
    /// before induction can see that far back.
    pub max_depth: usize,
    /// How to encode floats.
    pub floats: FloatEncoding,
    /// Must match the policy the monitor is generated with, or the prover is
    /// reasoning about a different program.
    pub index_policy: copilot_core::IndexPolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            solver: Solver::default(),
            depth: None,
            max_depth: 12,
            floats: FloatEncoding::default(),
            index_policy: copilot_core::IndexPolicy::default(),
        }
    }
}

/// Something that makes a result less than a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caveat {
    /// Floats were encoded as reals.
    FloatsAsReals,
    /// Operations the theory cannot express became uninterpreted functions.
    ///
    /// Sound for proving — a property true of every interpretation is true of
    /// the real one — but not for refuting, since a counterexample may rest on
    /// an interpretation the actual function does not have.
    Uninterpreted(Vec<&'static str>),
}

impl fmt::Display for Caveat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Caveat::FloatsAsReals => f.write_str(
                "floats were encoded as reals, which have no NaN, infinity, overflow or rounding",
            ),
            Caveat::Uninterpreted(names) => write!(
                f,
                "these operations were left uninterpreted: {}",
                names.join(", ")
            ),
        }
    }
}

/// One step of a counterexample: what the environment supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// The external variables, in declaration order.
    pub inputs: Vec<(String, Value)>,
}

/// A trace reaching a property violation.
///
/// Only the external inputs are recorded, because everything else follows from
/// them. [`Counterexample::replay`] runs them back through the interpreter,
/// which shares no code with the encoding — so a counterexample that replays is
/// corroborated by a second implementation rather than taken on trust.
#[derive(Debug, Clone, PartialEq)]
pub struct Counterexample {
    /// The steps, from the initial state.
    pub steps: Vec<Step>,
}

impl Counterexample {
    /// Runs the trace through the interpreter, returning what it observed.
    pub fn replay(&self, spec: &Spec) -> Result<Vec<copilot_interp::Observation>> {
        let mut monitor = copilot_interp::Monitor::new(spec)?;
        self.steps
            .iter()
            .map(|step| {
                let mut samples = copilot_interp::Samples::none();
                for (name, value) in &step.inputs {
                    samples = samples.with(name, value.clone());
                }
                monitor.step(&mut samples).map_err(Error::from)
            })
            .collect()
    }

    /// The step at which the property first fails, when replayed.
    ///
    /// `None` means the trace did not reproduce the violation, which is itself
    /// worth knowing: it means the encoding and the interpreter disagree.
    pub fn confirm(&self, spec: &Spec, property: &str) -> Result<Option<usize>> {
        let expr = spec
            .properties
            .iter()
            .find(|p| p.name == property)
            .map(|p| p.prop.expr())
            .ok_or_else(|| Error::Unsupported(format!("no property named `{property}`")))?;

        // Observing the property's expression is how its value per step is
        // recovered without duplicating the evaluator.
        let mut probe = spec.clone();
        probe.observers.clear();
        probe.triggers.clear();
        probe.observe("property_under_test", expr)?;

        for (index, observation) in self.replay(&probe)?.into_iter().enumerate() {
            if observation.observers[0].1 == Value::Bool(false) {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
}

/// What was decided about a property.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Proved for every trace.
    Valid,
    /// Refuted, with a trace that reaches the violation.
    Invalid(Counterexample),
    /// Neither proved nor refuted.
    Unknown(String),
}

/// The result of proving one property.
#[derive(Debug, Clone, PartialEq)]
pub struct Proof {
    /// The property's name.
    pub property: String,
    /// What was decided.
    pub outcome: Outcome,
    /// What makes the result less than a proof; empty when nothing does.
    pub caveats: Vec<Caveat>,
    /// The induction depth used.
    pub depth: usize,
}

impl Proof {
    /// Whether this is a proof, with nothing approximated.
    pub fn is_conclusive(&self) -> bool {
        induct::is_conclusive(&self.outcome, &self.caveats)
    }
}

impl fmt::Display for Proof {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.property)?;
        match &self.outcome {
            Outcome::Valid if self.caveats.is_empty() => {
                write!(f, "proved (k = {})", self.depth)?;
            }
            Outcome::Valid => write!(f, "holds under an approximation (k = {})", self.depth)?,
            Outcome::Invalid(counterexample) => write!(
                f,
                "refuted by a trace of {} step(s)",
                counterexample.steps.len()
            )?,
            Outcome::Unknown(reason) => write!(f, "undecided — {reason}")?,
        }
        for caveat in &self.caveats {
            write!(f, "\n  caveat: {caveat}")?;
        }
        Ok(())
    }
}

/// Proves every property in a specification.
pub fn prove(spec: &Spec, settings: &Settings) -> Result<Vec<Proof>> {
    induct::prove(spec, settings)
}

/// Everything that can go wrong while proving.
#[derive(Debug)]
pub enum Error {
    /// The specification is not valid, or the interpreter refused a replay.
    ///
    /// One variant rather than two: `copilot_interp` re-exports the core error
    /// type rather than defining its own.
    Core(copilot_core::Error),
    /// No solver could be started.
    SolverUnavailable {
        /// The executable that was looked for.
        program: &'static str,
        /// Why starting it failed.
        reason: String,
    },
    /// The solver said something unexpected.
    Protocol(String),
    /// The specification uses something the encoder cannot express.
    Unsupported(String),
    /// The pipe to the solver failed.
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Core(e) => e.fmt(f),
            Error::SolverUnavailable { program, reason } => {
                write!(f, "could not start `{program}`: {reason}")
            }
            Error::Protocol(detail) => write!(f, "unexpected answer from the solver: {detail}"),
            Error::Unsupported(detail) => write!(f, "cannot encode this specification: {detail}"),
            Error::Io(e) => write!(f, "communication with the solver failed: {e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<copilot_core::Error> for Error {
    fn from(e: copilot_core::Error) -> Self {
        Error::Core(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
