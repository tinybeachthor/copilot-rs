//! Golden tests: the generated source for each corpus specification is checked
//! in, so any change to the code generator shows up as a reviewable diff rather
//! than as a silent change in what a monitor does.
//!
//! Run `UPDATE_GOLDEN=1 cargo test -p copilot-rust --test golden` to rewrite
//! them after an intended change.

mod support;

use copilot_rust::{Settings, generate};
use std::path::PathBuf;

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.rs"))
}

#[test]
fn generated_source_matches_the_checked_in_copy() {
    let updating = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut stale = Vec::new();

    for (name, spec) in support::all() {
        let generated = generate(&spec, &Settings::default())
            .unwrap_or_else(|e| panic!("{name} must generate: {e}"));
        let path = golden_path(name);

        if updating {
            std::fs::write(&path, &generated).expect("golden file must be writable");
            continue;
        }

        let checked_in = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden file {}", path.display()));
        if checked_in != generated {
            stale.push(name);
        }
    }

    assert!(
        stale.is_empty(),
        "generated source no longer matches the checked-in copy for: {}\n\
         Re-run with UPDATE_GOLDEN=1 to accept the change, then review the diff.",
        stale.join(", ")
    );
}

/// The generated `Monitor` must be exactly as large as
/// `copilot_core::resources` says, which is what makes the constant-memory
/// claim falsifiable rather than decorative.
///
/// The claim is checked against real compiled types in `differential.rs`, where
/// the generated code is compiled; here it is checked that the generator at
/// least reports the same figure it computes.
#[test]
fn generated_source_states_its_own_footprint() {
    for (name, spec) in support::all() {
        let footprint = copilot_core::resources(&spec);
        let generated = generate(&spec, &Settings::default()).unwrap();
        let claim = format!("State: {} bytes", footprint.state_bytes);
        assert!(
            generated.contains(&claim),
            "{name}: generated source does not state `{claim}`"
        );
    }
}

/// Every monitor must be `repr(C)`. The footprint analysis lays the state out
/// under C rules; `repr(Rust)` may reorder fields, which would make the
/// reported size unfalsifiable.
#[test]
fn monitor_state_is_repr_c() {
    for (name, spec) in support::all() {
        let generated = generate(&spec, &Settings::default()).unwrap();
        let state = generated
            .split("pub struct Monitor")
            .next()
            .unwrap_or_default();
        assert!(
            state.contains("#[repr(C)]"),
            "{name}: Monitor is not declared repr(C)"
        );
    }
}

mod rejects {
    use super::*;
    use copilot_lang::{Builder, args};

    #[test]
    fn a_trigger_colliding_with_an_observer_method() {
        let b = Builder::new();
        let flag = b.lit(true);
        b.observe("state", flag);
        b.trigger("observe_state", flag, args![]);
        let spec = b.finish().unwrap();

        assert!(matches!(
            generate(&spec, &Settings::default()),
            Err(copilot_rust::Error::NameCollision { .. })
        ));
    }
}
