//! Golden tests: the generated Bluespec for each corpus specification is
//! checked in, so any change to the code generator shows up as a reviewable
//! diff rather than as a silent change in what a monitor does.
//!
//! Run `UPDATE_GOLDEN=1 cargo test -p copilot-bluespec --test golden` to rewrite
//! them after an intended change.

mod support;

use copilot_bluespec::{Settings, generate, testbench};
use std::path::PathBuf;

fn golden_dir(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

#[test]
fn generated_source_matches_the_checked_in_copy() {
    let updating = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut stale = Vec::new();

    for (name, spec, trace) in support::all() {
        let settings = Settings::default();
        let mut packages =
            generate(&spec, &settings).unwrap_or_else(|e| panic!("{name} must generate: {e}"));
        packages.push(
            testbench(&spec, &settings, &trace)
                .unwrap_or_else(|e| panic!("{name} must produce a testbench: {e}")),
        );

        let directory = golden_dir(name);
        if updating {
            let _ = std::fs::remove_dir_all(&directory);
            std::fs::create_dir_all(&directory).expect("golden directory must be writable");
        }

        for package in &packages {
            let path = directory.join(package.file_name());
            if updating {
                std::fs::write(&path, &package.source).expect("golden file must be writable");
                continue;
            }
            let checked_in = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("missing golden file {}", path.display()));
            if checked_in != package.source {
                stale.push(format!("{name}/{}", package.file_name()));
            }
        }
    }

    assert!(
        stale.is_empty(),
        "generated Bluespec no longer matches the checked-in copy for: {}\n\
         Re-run with UPDATE_GOLDEN=1 to accept the change, then review the diff.",
        stale.join(", ")
    );
}

/// The generated monitor states its state in registers, and says what that
/// figure is not.
///
/// The Rust backend's footprint is checked against `size_of::<Monitor>()`; this
/// one is checked against nothing, because `bsc` reports area in a form nothing
/// parses. The disclaimer is therefore part of the claim rather than a courtesy,
/// and this test is what keeps it from being dropped in an edit.
#[test]
fn generated_source_states_its_state_without_overclaiming() {
    for (name, spec, _) in support::all() {
        let packages = generate(&spec, &Settings::default()).unwrap();
        let monitor = packages
            .last()
            .expect("a monitor package is always emitted");

        // One register per buffer slot, plus one index per buffer deeper than
        // one element -- exactly the state `mkMonitor` declares.
        let registers: usize = spec
            .streams
            .iter()
            .map(|s| s.buffer.len() + usize::from(s.needs_index()))
            .sum();
        let claim = format!("across {registers} register");
        assert!(
            monitor.source.contains(&claim),
            "{name}: generated source does not state `{claim}`"
        );
        assert!(
            monitor
                .source
                .contains("It is not the area\n-- bsc synthesises"),
            "{name}: generated source claims a footprint without saying what is unchecked"
        );
    }
}

/// A specification with no streams is a monitor with no state, and it must
/// still generate.
///
/// `operators` is exactly that — every observer reads external variables — and
/// it is the entry that caught an over-strict assertion here. A stateless
/// monitor is a perfectly ordinary thing to write.
#[test]
fn a_stateless_monitor_states_that_it_holds_nothing() {
    let spec = support::operators();
    assert!(spec.streams.is_empty(), "operators buffers nothing");

    let packages = generate(&spec, &Settings::default()).unwrap();
    let monitor = packages.last().unwrap();
    assert!(
        monitor.source.contains("State: 0 bits across 0 registers"),
        "a stateless monitor should say so"
    );
}

/// Every package must be named after the file it has to live in, since that is
/// how Bluespec finds it.
#[test]
fn every_package_declares_the_name_of_its_own_file() {
    for (name, spec, _) in support::all() {
        for package in generate(&spec, &Settings::default()).unwrap() {
            assert!(
                package
                    .source
                    .contains(&format!("package {} where", package.name)),
                "{name}: {} does not declare itself",
                package.file_name()
            );
            assert_eq!(package.file_name(), format!("{}.bs", package.name));
        }
    }
}

/// A monitor's rule must not be conditional on anything: a step that could be
/// held up by its own data is not a constant-time step.
#[test]
fn the_step_rule_fires_unconditionally() {
    for (name, spec, _) in support::all() {
        let packages = generate(&spec, &Settings::default()).unwrap();
        let monitor = packages.last().unwrap();
        assert!(
            monitor.source.contains("\"step\": when True ==> do"),
            "{name}: the step rule is guarded"
        );
    }
}

mod rejects {
    use super::*;
    use copilot_lang::{Builder, args};

    #[test]
    fn a_specification_mentioning_floats() {
        let b = Builder::new();
        let temperature = b.extern_::<f32>("temperature");
        b.observe("temperature", temperature);
        let spec = b.finish().unwrap();

        assert!(matches!(
            generate(&spec, &Settings::default()),
            Err(copilot_bluespec::Error::UnsupportedType(
                copilot_core::Type::Float
            ))
        ));
    }

    /// A float buried inside a struct is still a float.
    #[derive(Clone, Copy, Debug, PartialEq, copilot_lang::CopilotStruct)]
    #[repr(C)]
    struct Sample {
        value: f64,
    }

    #[test]
    fn a_specification_with_a_float_field() {
        let b = Builder::new();
        let sample = b.extern_::<Sample>("sample");
        b.observe("sample", sample);
        let spec = b.finish().unwrap();

        assert!(matches!(
            generate(&spec, &Settings::default()),
            Err(copilot_bluespec::Error::UnsupportedType(_))
        ));
    }

    #[test]
    fn a_trigger_colliding_with_an_observer_method() {
        let b = Builder::new();
        let flag = b.lit(true);
        b.observe("state", flag);
        b.trigger("observe_state", flag, args![]);
        let spec = b.finish().unwrap();

        assert!(matches!(
            generate(&spec, &Settings::default()),
            Err(copilot_bluespec::Error::NameCollision { .. })
        ));
    }

    /// Bluespec's keywords are not the same as Rust's, so a name a
    /// specification is entitled to use can still be unusable here. Saying so
    /// beats a parse error two packages away.
    #[test]
    fn a_trigger_named_after_a_bluespec_keyword() {
        let b = Builder::new();
        let flag = b.lit(true);
        b.trigger("interface", flag, args![]);
        let spec = b.finish().unwrap();

        assert!(matches!(
            generate(&spec, &Settings::default()),
            Err(copilot_bluespec::Error::InvalidName { .. })
        ));
    }

    #[test]
    fn a_package_name_bluespec_cannot_hold() {
        let b = Builder::new();
        b.observe("flag", b.lit(true));
        let spec = b.finish().unwrap();

        let settings = Settings {
            name: "monitor".into(),
            ..Settings::default()
        };
        assert!(matches!(
            generate(&spec, &settings),
            Err(copilot_bluespec::Error::InvalidName {
                kind: "package",
                ..
            })
        ));
    }

    #[test]
    fn a_testbench_over_an_empty_trace() {
        let b = Builder::new();
        b.observe("flag", b.lit(true));
        let spec = b.finish().unwrap();

        assert!(matches!(
            testbench(&spec, &Settings::default(), &[]),
            Err(copilot_bluespec::Error::EmptyTrace)
        ));
    }

    #[test]
    fn a_testbench_over_a_trace_missing_a_variable() {
        let b = Builder::new();
        let x = b.extern_::<u8>("x");
        b.observe("x", x);
        let spec = b.finish().unwrap();

        assert!(matches!(
            testbench(&spec, &Settings::default(), &support::no_samples(3)),
            Err(copilot_bluespec::Error::MissingSample { step: 0, .. })
        ));
    }
}
