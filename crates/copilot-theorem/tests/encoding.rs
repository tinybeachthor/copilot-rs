//! Does the SMT encoding mean what the specification means?
//!
//! Everything else in this crate rests on that question, and nothing else here
//! asks it. A prover with a subtly wrong encoding does not fail loudly — it
//! returns confident answers about a program nobody wrote.
//!
//! The check is a differential one against the interpreter. The two share no
//! code and are not even the same shape: the interpreter walks the IR over ring
//! buffers in Rust, while the encoding is a shifting window of SMT terms with
//! its own hand-written guards for division, shifting, casts and the signed and
//! unsigned comparisons. Agreement across randomly generated specifications is
//! evidence; a disagreement is a bug in one of them.

use copilot_core::Value;
use copilot_gen::{Config, Rng, spec, trace};
use copilot_interp::Monitor;
use copilot_theorem::{Settings, Solver, evaluate};

/// Reduces a trace to the plain input lists `evaluate` takes.
fn inputs(
    spec: &copilot_core::Spec,
    samples: &[copilot_interp::Samples],
) -> Vec<Vec<(String, Value)>> {
    let externs = spec.arena.externs().to_vec();
    samples
        .iter()
        .map(|sample| {
            externs
                .iter()
                .map(|(name, ty)| {
                    let mut sample = sample.clone();
                    let value = copilot_interp::Env::sample(&mut sample, name, ty)
                        .expect("the generator supplies every external variable");
                    (name.clone(), value)
                })
                .collect()
        })
        .collect()
}

/// Runs one generated specification through both engines.
///
/// Returns the seed on disagreement so the case can be reproduced exactly.
fn agrees_on(seed: u64, settings: &Settings, config: &Config, steps: usize) -> Result<(), String> {
    let mut rng = Rng::new(seed);
    let generated = spec(&mut rng, config);
    let samples = trace(&mut rng, &generated, steps);

    let mut monitor = Monitor::new(&generated).map_err(|e| format!("seed {seed}: {e}"))?;
    let interpreted: Vec<Vec<(String, Value)>> = samples
        .iter()
        .map(|sample| {
            monitor
                .step(&mut sample.clone())
                .map(|observed| observed.observers)
                .map_err(|e| format!("seed {seed}: interpreter: {e}"))
        })
        .collect::<Result<_, _>>()?;

    let encoded = evaluate(&generated, settings, &inputs(&generated, &samples))
        .map_err(|e| format!("seed {seed}: encoding: {e}"))?;

    for (step, (left, right)) in interpreted.iter().zip(&encoded).enumerate() {
        if left != right {
            return Err(format!(
                "seed {seed}, step {step}: the interpreter says {left:?} but the SMT encoding \
                 says {right:?}"
            ));
        }
    }
    Ok(())
}

/// Whether to skip for want of a solver.
///
/// `COPILOT_REQUIRE_SOLVER` turns the skip into a failure, so CI cannot go
/// green having quietly checked nothing.
fn skip_without_solver(settings: &Settings) -> bool {
    if settings.solver.available() {
        return false;
    }
    assert!(
        std::env::var_os("COPILOT_REQUIRE_SOLVER").is_none(),
        "`{}` is not on PATH and COPILOT_REQUIRE_SOLVER is set",
        settings.solver.program()
    );
    eprintln!("skipping: `{}` is not on PATH", settings.solver.program());
    true
}

/// The main event: many random specifications, over every integer width, every
/// arithmetic and bitwise operator, both comparison families, casts, shifts,
/// and division by zero.
#[test]
fn the_encoding_agrees_with_the_interpreter() {
    let settings = Settings::default();
    if skip_without_solver(&settings) {
        return;
    }

    let config = Config::default();
    let mut failures = Vec::new();
    for seed in 0..60 {
        if let Err(report) = agrees_on(seed, &settings, &config, 6) {
            failures.push(report);
        }
    }
    assert!(
        failures.is_empty(),
        "the SMT encoding and the interpreter disagree:\n{}",
        failures.join("\n")
    );
}

/// Deeper expressions and deeper buffers, where the shifting-window encoding of
/// state has more room to be wrong.
#[test]
fn the_encoding_agrees_on_deeper_specifications() {
    let settings = Settings::default();
    if skip_without_solver(&settings) {
        return;
    }

    let config = Config {
        streams: 4,
        externs: 4,
        depth: 6,
        max_buffer: 4,
        extra_observers: 8,
        floats: false,
    };
    let mut failures = Vec::new();
    for seed in 1000..1020 {
        if let Err(report) = agrees_on(seed, &settings, &config, 10) {
            failures.push(report);
        }
    }
    assert!(
        failures.is_empty(),
        "the SMT encoding and the interpreter disagree:\n{}",
        failures.join("\n")
    );
}

/// Both solvers must decode to the same values.
///
/// They print bitvectors differently — z3 says `#x0f` where cvc5 says
/// `#b00001111` — so this exercises the answer parser as much as the encoding.
#[test]
fn both_solvers_decode_alike() {
    let config = Config::default();
    for solver in [Solver::Z3, Solver::Cvc5] {
        let settings = Settings {
            solver,
            ..Settings::default()
        };
        if skip_without_solver(&settings) {
            continue;
        }
        for seed in 0..8 {
            if let Err(report) = agrees_on(seed, &settings, &config, 4) {
                panic!("{}: {report}", solver.program());
            }
        }
    }
}

/// The IEEE encoding must agree with the interpreter on floats too.
///
/// Only under `FloatEncoding::Ieee`: the default real encoding is an
/// approximation with no NaN, infinity or rounding, so disagreement there would
/// be the documented behaviour rather than a bug.
#[test]
fn the_ieee_encoding_agrees_on_floats() {
    let settings = Settings {
        floats: copilot_theorem::FloatEncoding::Ieee,
        ..Settings::default()
    };
    if skip_without_solver(&settings) {
        return;
    }

    let config = Config {
        streams: 2,
        externs: 2,
        depth: 3,
        max_buffer: 2,
        extra_observers: 4,
        floats: true,
    };
    let mut failures = Vec::new();
    for seed in 500..508 {
        if let Err(report) = agrees_on(seed, &settings, &config, 4) {
            failures.push(report);
        }
    }
    assert!(
        failures.is_empty(),
        "the IEEE encoding and the interpreter disagree:\n{}",
        failures.join("\n")
    );
}

/// Generated specifications must be well formed by the same checker every
/// other consumer uses — the generator builds on the arena directly, so this is
/// not guaranteed by construction the way the typed frontend would make it.
#[test]
fn generated_specifications_validate() {
    let mut rng = Rng::new(7);
    for _ in 0..200 {
        let generated = spec(&mut rng, &Config::default());
        copilot_core::validate(&generated).expect("generated specs must validate");
    }
}
