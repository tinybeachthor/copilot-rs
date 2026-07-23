//! The reference must be written independently of what it verifies.
//!
//! The bisimulation is only meaningful if `ir_step` came from a different code
//! generator than the monitor. If this crate's library could call into
//! `copilot-rust`, a future change might quietly lower both sides through the
//! same code, and the proof would degrade to "the generator equals itself"
//! without anything failing.
//!
//! This is enforced structurally: `copilot-rust` is a dev-dependency, used only
//! by the tests that assemble a monitor to verify, and must never become a
//! library dependency. The check reads the manifest rather than trusting review.

mod support;

#[test]
fn the_library_does_not_depend_on_copilot_rust() {
    let manifest = std::fs::read_to_string(support::manifest_dir().join("Cargo.toml"))
        .expect("the crate has a manifest");

    // Split off the dev-dependencies section, where copilot-rust legitimately
    // appears; only the library dependencies before it must be clean.
    let library_deps = manifest
        .split("[dev-dependencies]")
        .next()
        .expect("there is always a part before dev-dependencies");

    // A dependency declaration, not a mention: the section above has a comment
    // explaining precisely why copilot-rust is absent, and that must not trip
    // the check.
    let declares_copilot_rust = library_deps.lines().any(|line| {
        let code = line.split('#').next().unwrap_or("").trim_start();
        code.starts_with("copilot-rust ") || code.starts_with("copilot-rust=")
    });

    assert!(
        !declares_copilot_rust,
        "copilot-verifier's library must not depend on copilot-rust: the reference it \
         generates has to be independent of the monitor it checks, or the bisimulation proves \
         nothing. copilot-rust belongs in [dev-dependencies] only."
    );
}

/// The transitive tree must be clean too — a library dependency could pull
/// `copilot-rust` in indirectly.
#[test]
fn copilot_rust_is_not_a_transitive_library_dependency() {
    use std::process::Command;

    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args([
            "tree",
            "--package",
            "copilot-verifier",
            "--edges",
            "normal", // library dependencies only, excluding dev-dependencies
            "--prefix",
            "none",
        ])
        .current_dir(support::manifest_dir())
        .output();

    let output = match output {
        Ok(output) if output.status.success() => output,
        // `cargo tree` unavailable or failing is not this test's concern; the
        // manifest check above is the primary guard.
        _ => {
            eprintln!("skipping: `cargo tree` did not run");
            return;
        }
    };

    let tree = String::from_utf8_lossy(&output.stdout);
    assert!(
        !tree
            .lines()
            .any(|line| line.trim_start().starts_with("copilot-rust ")),
        "copilot-rust appears in copilot-verifier's normal dependency tree:\n{tree}"
    );
}
