//! Layer-1 testing for the Bluespec backend: `bsc` compiles every corpus
//! monitor, bluesim runs it, and the trace it prints must be the one the
//! interpreter produces.
//!
//! This is the same claim `copilot-rust`'s differential test makes, discharged
//! the only way it can be for a hardware description: by simulating the
//! hardware. The suite skips cleanly when no Bluespec toolchain is installed,
//! since `bsc` is a heavy dependency to require of everyone who runs the tests.
//! Set `COPILOT_REQUIRE_BSC=1` — as CI does — to turn a missing toolchain into a
//! failure, because a suite that skips is indistinguishable from one that
//! passes.

mod support;

use copilot_bluespec::{Package, Settings, generate, testbench};
use copilot_core::{IndexPolicy, Spec, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use support::Event;

/// Whether a Bluespec toolchain is installed, or the suite should be skipped.
fn toolchain() -> Option<()> {
    let found = Command::new("bsc")
        .arg("-v")
        .output()
        .is_ok_and(|out| out.status.success());
    if found {
        return Some(());
    }
    assert!(
        std::env::var_os("COPILOT_REQUIRE_BSC").is_none(),
        "COPILOT_REQUIRE_BSC is set but `bsc` is not on PATH"
    );
    eprintln!("bsc not found; skipping the Bluespec simulation tests");
    None
}

/// A directory the generated packages and the simulator's output live in.
///
/// Under `target/`, not a temporary directory: when a comparison fails, the
/// generated Bluespec is the evidence, and it is no use if it has been deleted
/// by the time the failure is printed.
fn workspace(name: &str) -> PathBuf {
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("a scratch directory must be creatable");
    directory
}

fn run(command: &mut Command, directory: &Path) -> std::process::Output {
    let output = command
        .current_dir(directory)
        .output()
        .unwrap_or_else(|e| panic!("could not run {command:?}: {e}"));
    let (out, err) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        output.status.success(),
        "{command:?} failed in {}:\n{out}\n{err}",
        directory.display(),
    );
    // Generated code is meant to compile clean in the user's own build, the
    // same claim `copilot-rust` makes by running the generated crate under
    // `-D warnings`. A warning here is a defect in the generator.
    let warnings: Vec<&str> = out
        .lines()
        .chain(err.lines())
        .filter(|line| line.starts_with("Warning:"))
        .collect();
    assert!(
        warnings.is_empty(),
        "{command:?} warned in {}:\n{}",
        directory.display(),
        warnings.join("\n"),
    );
    output
}

/// Compiles the packages and returns what the simulation printed.
fn simulate(directory: &Path, packages: &[Package], top: &str) -> Vec<Event> {
    for package in packages {
        std::fs::write(directory.join(package.file_name()), &package.source)
            .expect("a generated package must be writable");
    }

    let simulation = packages.last().expect("a testbench is always last");
    run(
        Command::new("bsc")
            .args(["-sim", "-u", "-g", top])
            .arg(simulation.file_name()),
        directory,
    );
    run(
        Command::new("bsc")
            .args(["-sim", "-e", top, "-o", "sim.out"])
            .arg(format!("{top}.ba")),
        directory,
    );

    let output = run(&mut Command::new("./sim.out"), directory);
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(Event::parse)
        .collect()
}

/// Generates a monitor and its testbench, simulates it, and returns the events.
fn simulate_spec(name: &str, spec: &Spec, trace: &[BTreeMap<String, Value>]) -> Vec<Event> {
    let settings = Settings::default();
    let mut packages =
        generate(spec, &settings).unwrap_or_else(|e| panic!("{name} must generate: {e}"));
    packages.push(
        testbench(spec, &settings, trace)
            .unwrap_or_else(|e| panic!("{name} must produce a testbench: {e}")),
    );
    simulate(&workspace(name), &packages, "mkMonitorSim")
}

#[test]
fn every_corpus_monitor_simulates_as_the_interpreter_runs_it() {
    if toolchain().is_none() {
        return;
    }

    for (name, spec, trace) in support::all() {
        let simulated = simulate_spec(name, &spec, &trace);
        let interpreted = support::interpret(&spec, &trace);

        assert!(
            !interpreted.is_empty(),
            "{name}: the corpus entry reports nothing, so the comparison is vacuous"
        );
        assert_eq!(
            simulated, interpreted,
            "{name}: bluesim and the interpreter disagree"
        );
    }
}

/// Simulates a specification under one index policy and returns the events.
fn simulate_under(
    name: &str,
    spec: &Spec,
    trace: &[BTreeMap<String, Value>],
    policy: IndexPolicy,
) -> Vec<Event> {
    let settings = Settings {
        index_policy: policy,
        ..Settings::default()
    };
    let mut packages = generate(spec, &settings)
        .unwrap_or_else(|e| panic!("{name} must generate under {policy:?}: {e}"));
    packages.push(testbench(spec, &settings, trace).unwrap());
    simulate(&workspace(name), &packages, "mkMonitorSim")
}

/// An out-of-range subscript means whatever the configured policy says, and the
/// two engines have to say the same thing.
///
/// The corpus above runs at the default policy only, so the other two would
/// otherwise be code paths nothing executes — and they change what a monitor
/// computes, not just how fast it computes it.
#[test]
fn wrapping_and_saturating_a_subscript_simulate_as_the_interpreter_resolves_them() {
    if toolchain().is_none() {
        return;
    }

    // `arrays` subscripts a three-element array at index 7 on purpose.
    let spec = support::arrays();
    let trace = support::no_samples(8);
    let mut answers = Vec::new();

    for policy in [IndexPolicy::Wrap, IndexPolicy::Saturate] {
        let name = format!("arrays_{policy:?}").to_lowercase();
        let simulated = simulate_under(&name, &spec, &trace, policy);
        assert_eq!(
            simulated,
            support::interpret_with_policy(&spec, &trace, policy),
            "bluesim and the interpreter disagree under {policy:?}"
        );
        answers.push(simulated);
    }

    assert_ne!(
        answers[0], answers[1],
        "wrapping and saturating an out-of-range subscript produced the same trace, \
         so this test cannot tell the policies apart"
    );
}

/// `Assume` emits the subscript unguarded, so the specification owes the
/// obligation and both engines take it as given.
///
/// Worth knowing, and the reason this needs its own specification: Bluespec
/// checks a *constant* subscript during elaboration, so under `Assume` a
/// specification that breaks the bargain visibly is refused by `bsc` rather
/// than compiled into something unspecified. The interpreter refuses it too, as
/// `Error::IndexOutOfRange`. The two agree even about that — but neither
/// produces a trace, so it is the in-range case that has something to compare.
#[test]
fn an_assumed_subscript_simulates_as_the_interpreter_resolves_it() {
    if toolchain().is_none() {
        return;
    }

    let spec = support::bounded_arrays();
    let trace = support::no_samples(9);
    let simulated = simulate_under("arrays_assume", &spec, &trace, IndexPolicy::Assume);
    assert_eq!(
        simulated,
        support::interpret_with_policy(&spec, &trace, IndexPolicy::Assume),
        "bluesim and the interpreter disagree under Assume"
    );
}

/// What gives the corpus suite its teeth.
///
/// The comparison only means something if a broken monitor fails it. Here one
/// is broken on purpose — the rotating index is frozen, so every buffer read
/// after the first step comes from the wrong slot — and the test asserts the
/// disagreement rather than the agreement.
///
/// The mutation is textual, so it also checks that it applied: a code generator
/// that stopped emitting an index advance would otherwise quietly turn this
/// into a test of nothing.
#[test]
fn a_frozen_ring_buffer_index_is_caught() {
    if toolchain().is_none() {
        return;
    }

    let spec = support::fib();
    let trace = support::no_samples(12);
    let settings = Settings::default();
    let mut packages = generate(&spec, &settings).unwrap();
    packages.push(testbench(&spec, &settings, &trace).unwrap());

    let monitor = packages
        .iter_mut()
        .find(|p| p.name == "Monitor")
        .expect("the monitor package is named after the settings");
    let advance = monitor
        .source
        .lines()
        .find(|line| line.trim_start().starts_with("s0_idx := if"))
        .expect("a two-deep buffer advances its index")
        .to_string();
    monitor.source = monitor.source.replace(&advance, "        s0_idx := s0_idx");
    assert!(
        !monitor.source.contains(&advance),
        "the mutation did not apply, so this test proves nothing"
    );

    let simulated = simulate(&workspace("fib_frozen_index"), &packages, "mkMonitorSim");
    let interpreted = support::interpret(&spec, &trace);
    assert_ne!(
        simulated, interpreted,
        "a monitor whose ring-buffer index never advances agreed with the interpreter, \
         so the comparison cannot detect a wrong buffer read"
    );
}

/// The other half of the same argument, for the phase boundary.
///
/// `lag`'s second stream must see the first as it was at the start of the step.
/// Here the commit is made to write the *new* value into the register the
/// second stream reads, which is the classic phase-3/4 swap, and the
/// disagreement is asserted.
#[test]
fn a_stream_reading_a_committed_value_is_caught() {
    if toolchain().is_none() {
        return;
    }

    let spec = support::lag();
    let trace = support::no_samples(8);
    let settings = Settings::default();
    let mut packages = generate(&spec, &settings).unwrap();
    packages.push(testbench(&spec, &settings, &trace).unwrap());

    // `follower` is stream 1, and copies stream 0's current value. Committing
    // stream 0's *next* value into it instead is what a generator that merged
    // phases 3 and 4 would compute.
    let monitor = packages.iter_mut().find(|p| p.name == "Monitor").unwrap();
    let commit = monitor
        .source
        .lines()
        .find(|line| line.trim_start().starts_with("s1_0 := "))
        .expect("the follower commits a value")
        .to_string();
    let next_of_leader = monitor
        .source
        .lines()
        .find(|line| line.trim_start().starts_with("s0_0 := "))
        .expect("the leader commits a value")
        .trim_start()
        .trim_start_matches("s0_0 := ")
        .to_string();
    monitor.source = monitor
        .source
        .replace(&commit, &format!("        s1_0 := {next_of_leader}"));

    let simulated = simulate(&workspace("lag_merged_phases"), &packages, "mkMonitorSim");
    let interpreted = support::interpret(&spec, &trace);
    assert_ne!(
        simulated, interpreted,
        "a monitor whose follower reads a committed value agreed with the interpreter, \
         so the comparison cannot detect a merged compute and commit phase"
    );
}
