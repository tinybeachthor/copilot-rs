//! The bisimulation proofs themselves.
//!
//! These run `cargo kani`, so they need it installed and are slow — each spec
//! is a fresh compile plus a CBMC solve. They skip cleanly when Kani is absent,
//! so `cargo test --workspace` stays green on a machine without it; run them
//! deliberately with `cargo test -p copilot-verifier --test kani`.

mod support;

use copilot_rust::{Settings as RustSettings, generate};
use copilot_verifier::{Settings, generate_harness};
use support::{Verdict, corpus, run_kani, skip_without_kani};

fn harness_for(spec: &copilot_core::Spec) -> String {
    let monitor = generate(spec, &RustSettings::default()).expect("monitor must generate");
    generate_harness(spec, &monitor, &Settings::default()).expect("harness must generate")
}

/// Every corpus monitor bisimulates its reference, for all states and inputs.
///
/// This is the milestone's headline: the ring-buffer implementation is proved
/// equivalent to the shifting-window semantics, exhaustively, with no unwinding
/// bound — `step` is loop-free, so CBMC's unrolling is exact.
#[test]
fn every_corpus_monitor_is_verified() {
    if skip_without_kani() {
        return;
    }

    for (name, spec) in corpus() {
        let verdict = run_kani(name, &harness_for(&spec));
        assert_eq!(verdict, Verdict::Verified, "`{name}` failed to verify");
    }
}

/// The proof has teeth: a monitor with its commit corrupted is refuted.
///
/// The corruption is the state-management bug the whole harness exists to
/// catch — the index is advanced but the new value is written to the slot it
/// *becomes* rather than the one it *was*, so the ring buffer no longer holds
/// what the shifting window says it should. If Kani passed this, the proof would
/// be worthless.
#[test]
fn a_corrupted_commit_is_refuted() {
    if skip_without_kani() {
        return;
    }

    let spec = support::fib();
    let monitor = generate(&spec, &RustSettings::default()).unwrap();

    // fib buffers two values, so its commit is
    //   self.s0[self.s0_idx as usize] = e_next;
    //   self.s0_idx = (self.s0_idx + 1) % S0_LEN;
    // Writing to the post-advance slot instead is a plausible off-by-one that a
    // type checker and most traces would miss.
    let broken = monitor.replace(
        "self.s0[self.s0_idx as usize] =",
        "self.s0[((self.s0_idx + 1) % S0_LEN) as usize] =",
    );
    assert_ne!(
        broken, monitor,
        "the corruption must actually change the monitor"
    );

    let harness = generate_harness(&spec, &broken, &Settings::default()).unwrap();
    assert_eq!(
        run_kani("fib-broken", &harness),
        Verdict::Refuted,
        "Kani must catch a monitor that commits to the wrong slot"
    );
}

/// The classic phase bug: committing a stream before the streams that read it
/// have been computed. `lag`'s follower reads the leader, so if the leader is
/// committed first the follower sees its next value and the lag vanishes.
#[test]
fn a_phase_swap_is_refuted() {
    if skip_without_kani() {
        return;
    }

    let spec = support::lag();
    let monitor = generate(&spec, &RustSettings::default()).unwrap();

    // The leader is stream 0, the follower stream 1, each a single-element
    // buffer. The correct step computes both next values, then commits both.
    // Moving the leader's commit before the follower reads it is the phase-3/4
    // swap. The follower's body is `e_leader`; make the leader commit first by
    // rewriting the follower's read of the leader buffer to the post-commit
    // value.
    //
    // Concretely: `lag`'s follower body lowers to a read of `self.s0[0]`
    // (the leader). Committing the leader first means the follower would read
    // the *new* leader value. Simulate that by pointing the follower's stored
    // next value at the leader's freshly computed one.
    let leader_next = monitor
        .lines()
        .find_map(|line| {
            // The leader's committed value: `self.s0[0] = eN;`
            line.trim()
                .strip_prefix("self.s0[0] = ")
                .and_then(|rest| rest.strip_suffix(';'))
        })
        .expect("the leader commits a single-element buffer")
        .to_string();
    let follower_commit = monitor
        .lines()
        .find(|line| line.trim().starts_with("self.s1[0] = "))
        .expect("the follower commits a single-element buffer")
        .to_string();
    let broken = monitor.replace(
        follower_commit.trim(),
        &format!("self.s1[0] = {leader_next};"),
    );
    assert_ne!(broken, monitor, "the corruption must change the monitor");

    let harness = generate_harness(&spec, &broken, &Settings::default()).unwrap();
    assert_eq!(
        run_kani("lag-broken", &harness),
        Verdict::Refuted,
        "Kani must catch a follower that reads the leader's committed value"
    );
}
