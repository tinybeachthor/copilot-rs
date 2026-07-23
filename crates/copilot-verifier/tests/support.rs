//! Shared corpus and Kani driver for the verifier's tests.

#![allow(dead_code)]

use copilot_lang::{Builder, Spec, args};
use std::path::PathBuf;
use std::process::Command;

/// A single-element buffer: no rotating index, the simplest commit.
pub fn counter() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u8], |s| (s + 1u8) % 10u8);
    b.observe("counter", counter);
    b.trigger("wrapped", counter.eq_val(9), args![counter]);
    b.finish().unwrap()
}

/// A two-deep rotating buffer: the ring index actually turns, so this is the
/// case where getting the commit slot wrong would show.
pub fn fib() -> Spec {
    let b = Builder::new();
    let fib = b.stream([1u32, 1], |s| s.drop(1) + s);
    b.observe("fib", fib);
    b.finish().unwrap()
}

/// Two streams, one reading the other. The follower must see the leader as it
/// was at the start of the step — the phase boundary the swap bug breaks.
pub fn lag() -> Spec {
    let b = Builder::new();
    let leader = b.stream([0u32], |s| s + 1u32);
    let follower = b.stream([0u32], |_| leader);
    b.observe("leader", leader);
    b.observe("follower", follower);
    b.finish().unwrap()
}

/// External inputs, a latch, and triggers on both edges — integer only, so CBMC
/// stays fast.
pub fn thermostat() -> Spec {
    let b = Builder::new();
    let temp = b.extern_::<i16>("temp");
    let heating = b.stream([false], |was| {
        temp.lt_val(18)
            .mux(b.lit(true), temp.gt_val(21).mux(b.lit(false), was))
    });
    b.observe("heating", heating);
    b.trigger("heat_on", temp.lt_val(18) & !heating, args![temp]);
    b.trigger("heat_off", temp.gt_val(21) & heating, args![temp]);
    b.finish().unwrap()
}

/// A struct-typed stream, so the harness must derive `Arbitrary` for it.
pub fn structs() -> Spec {
    use copilot_lang::CopilotStruct;

    #[derive(Clone, Copy, Debug, PartialEq, CopilotStruct)]
    #[repr(C)]
    struct Pair {
        lo: u16,
        hi: u16,
    }

    let b = Builder::new();
    let pair = b.stream([Pair { lo: 0, hi: 0 }], |p| {
        p.set_lo(p.lo() + 1u16).set_hi(p.hi() + p.lo())
    });
    b.observe("hi", pair.hi());
    b.finish().unwrap()
}

/// An array-typed stream with subscript and update, exercising aggregate state.
pub fn arrays() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u32], |s| s + 1u32);
    let history = b.stream([[0u32; 3]], |s| s.update(counter % 3u32, counter));
    b.observe("oldest", history.index(b.lit(0u32)));
    b.finish().unwrap()
}

/// Every corpus spec that is meant to verify.
pub fn corpus() -> Vec<(&'static str, Spec)> {
    vec![
        ("counter", counter()),
        ("fib", fib()),
        ("lag", lag()),
        ("thermostat", thermostat()),
        ("structs", structs()),
        ("arrays", arrays()),
    ]
}

/// Whether Kani is installed. The Kani tests skip rather than fail without it.
pub fn kani_available() -> bool {
    Command::new("cargo")
        .args(["kani", "--version"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The outcome of running Kani on a harness.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Every check passed.
    Verified,
    /// At least one check failed — a counterexample exists.
    Refuted,
}

/// Compiles a harness into a throwaway crate and runs `cargo kani` on it.
///
/// Each call gets its own crate directory, so runs cannot collide over a target
/// directory or a lock file.
pub fn run_kani(name: &str, harness: &str) -> Verdict {
    let dir = std::env::temp_dir().join(format!("copilot-kani-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();

    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"harness\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\
         [lib]\npath = \"src/lib.rs\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/lib.rs"), harness).unwrap();

    let output = Command::new("cargo")
        .args(["kani"])
        .current_dir(&dir)
        .output()
        .expect("cargo kani must run");
    let stdout = String::from_utf8_lossy(&output.stdout);

    let verdict = if stdout.contains("VERIFICATION:- SUCCESSFUL")
        || stdout.contains("VERIFICATION SUCCESSFUL")
    {
        Verdict::Verified
    } else if stdout.contains("VERIFICATION:- FAILED") || stdout.contains("VERIFICATION FAILED") {
        Verdict::Refuted
    } else {
        panic!(
            "could not tell what Kani decided for `{name}`:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let _ = std::fs::remove_dir_all(&dir);
    verdict
}

/// Where the manifest lives, for tests that need a path into the crate.
pub fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
