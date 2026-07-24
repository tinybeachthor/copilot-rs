# copilot-rs

[![CI](https://github.com/tinybeachthor/copilot-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/tinybeachthor/copilot-rs/actions/workflows/ci.yml)

A Rust implementation of [Copilot](https://copilot-language.github.io/), the stream language for
hard-realtime runtime monitors.

You write a specification as a set of mutually recursive infinite streams. You get a `#![no_std]`
Rust monitor whose per-step time and total memory are fixed by the specification rather than by the
data it sees — and a proof that the monitor means what the specification says.

```rust
use copilot::copilot;

let spec = copilot! {
    extern temperature: f32;

    let celsius = temperature * 0.5 - 30.0;
    stream heating: bool = [false] ++
        (celsius < 18.0).mux(true, (celsius > 21.0).mux(false, heating));

    observe celsius;
    trigger heat_on(celsius) when celsius < 18.0 && !heating;
    property never_both = !(celsius < 18.0 && celsius > 21.0);
}?;
```

`stream heating: bool = [false] ++ …` reads as "`heating` starts as `false`, and thereafter is
whatever the right-hand side says". The buffer on the left is the stream's entire memory; a stream
that buffers *n* values can look *n−1* steps ahead of itself with `drop`, and no further. That is why
memory is bounded: there is nowhere for an unbounded past to live.

## The three objectives, and what enforces each

Copilot's design objectives are realtime, constant memory, and verifiable. Each one here has an
artifact that fails CI when it is violated, rather than a claim in a document.

**Realtime.** The IR has no recursion, no unbounded loops, and no allocation. `cost(&spec)` reports
the per-step operation count, broken down by how expensive each operation actually is on an embedded
target. Trigger arguments are evaluated whether or not their guard fires, so a step's timing does not
depend on its data.

**Constant memory.** `resources(&spec)` reports the exact static footprint in bytes. The generated
state is `#[repr(C)]` specifically so that figure is falsifiable — a test asserts it equals
`size_of::<Monitor>()` for every specification in the corpus, which `repr(Rust)` would be free to
invalidate by reordering fields.

**Verifiable.** Three independent layers, described below.

## The generated monitor

```rust
let source = copilot_rust::generate(&spec, &Settings::default())?;
```

produces a crate that is `#![no_std]`, `#![forbid(unsafe_code)]`, allocation-free, loop-free, and
compiles clean under `-D warnings` with no lint suppressions of its own:

```rust
#[repr(C)]
pub struct Monitor {
    s0: [bool; 1],
    s1: [u32; 2],
    s1_idx: u32,
}

impl Monitor {
    pub fn step<E: Env, H: Handler>(&mut self, env: &mut E, handler: &mut H) { … }
}
```

External variables arrive through an `Env` trait and results leave through a `Handler` trait, so the
monitor is dependency-injected and testable without a target board. A step is strictly four phases:

1. **Sample** every external variable exactly once, so two reads within a step cannot disagree.
2. **Observe and fire**, from the state the previous step committed.
3. **Compute** each stream's next value into a temporary. Nothing is written back yet.
4. **Commit** the temporaries and advance the ring indices.

Getting 3 and 4 backwards is *the* classic bug in this kind of code. It is exactly what layer 3
below rules out.

## Verification

**Layer 1 — differential testing.** Every specification is run through a constant-memory reference
interpreter and through the generated Rust, and the observer values and trigger call sequences must
agree. Inputs come from a hand-picked corpus, from `proptest`, and from a random *well-typed*
specification generator that compiles each monitor with `rustc` and runs it.

**Layer 2 — SMT k-induction.** `property` claims are lowered to a transition system in SMT-LIB2 and
discharged with Z3 or cvc5 over a pipe, so nothing links against a solver. Integers become bitvectors,
matching the wrapping semantics exactly. A refuted property comes back as a trace that the
interpreter reproduces at the same step, not as a model dump. Where an encoding is an approximation —
floats as reals, for instance — the result carries a `Caveat` and `is_conclusive` is false, so an
approximation cannot be mistaken for a proof.

**Layer 3 — Kani bisimulation.** For a given specification, a harness proves

```text
abstract_state(step(m)) == ir_step(abstract_state(m))
```

for *every* well-formed monitor state and *every* input — not up to a bound, because `step` is
loop-free by construction and CBMC's unrolling is therefore exact. The reference `ir_step` is
produced by a deliberately different code generator, in a crate forbidden from depending on
`copilot-rust`; sharing that lowering would prove only that the generator equals itself. A dependency
test keeps that honest. See [docs/bisimulation.md](docs/bisimulation.md).

Each layer carries negative tests that assert a *failure*, because a harness nobody has tried to fool
is not evidence: a swapped phase and a corrupted commit are both real, passing tests that require
Kani to find the counterexample.

## Crates

| Crate | |
|---|---|
| `copilot` | Facade: the language, the interpreter, and the analyses |
| `copilot-core` | The IR — types, expressions, `Spec`, typechecker, `resources`, `cost`. No dependencies, deliberately |
| `copilot-lang` | Builder frontend: phantom-typed `Stream<T>` handles and operators |
| `copilot-macro` | `copilot!` and `#[derive(CopilotStruct)]` |
| `copilot-interp` | Constant-memory reference interpreter |
| `copilot-rust` | `no_std` Rust code generator |
| `copilot-libs` | PTLTL, LTL, MTL, clocks, majority voting, state machines |
| `copilot-theorem` | SMT-LIB2 lowering and the k-induction driver |
| `copilot-verifier` | Kani bisimulation harness generation |
| `copilot-gen` | Random well-typed specification generation, for the differential suites |

Not published to crates.io — the `copilot*` names are contested, and the prefix is still undecided.

## Trying it

```bash
cargo run -p copilot --example heater
```

prints a specification's footprint and per-step cost, then drives it over a trace:

```
specification
  streams        1
  triggers       2
  state          1 bytes (align 1)
  work per step  18 operations (49 without sharing)

step   celsius  heating  triggers
   0     21.0°      off
   1     19.0°      off
   2     17.0°      off  heat_on
   3     15.0°       on
```

The whole suite is 178 tests:

```bash
cargo test --workspace
```

The optional layers skip cleanly when their tool is absent, so this is green on a bare checkout.
To actually run them, install Z3 or cvc5 for layer 2 and `cargo-kani` for layer 3; CI sets
`COPILOT_REQUIRE_SOLVER` and `COPILOT_REQUIRE_KANI` so that a missing tool there is a failure rather
than a silent skip. A verification suite that skips is indistinguishable from one that passes.

## Documentation

- [docs/macro.md](docs/macro.md) — the `copilot!` macro: grammar, scoping, expression translation
- [docs/semantics.md](docs/semantics.md) — denotational and operational semantics of the IR, and the
  correspondence between them
- [docs/bisimulation.md](docs/bisimulation.md) — the layer-3 proof argument, and its limits
- [docs/deviations.md](docs/deviations.md) — where this deliberately differs from Haskell Copilot,
  and why
- [PLAN.md](PLAN.md) — the implementation plan and milestone status

## Differences from Haskell Copilot

Fully recorded in [docs/deviations.md](docs/deviations.md); the ones that change what you can write:

- **Arithmetic is total.** Integers wrap, division and remainder by zero are zero, and
  over-wide shifts are zero. Upstream's C99 backend inherits C's undefined behaviour here; defining
  it makes the interpreter, the generated code, and the SMT encoding able to agree.
- **Equality is scalar-only.** Comparing whole arrays or structs compiles to a fully unrolled
  element-wise walk, which the bisimulation proof would have to carry.
- **No C99 backend.** The `no_std` Rust backend is the flagship, with Bluespec planned.
- **Sharing is structural.** An arena with hash-consing replaces upstream's `data-reify` /
  `StableName` observation, which is `unsafePerformIO`-based and heuristic. This removes the single
  unsafest part of upstream — reusing a handle *is* sharing, deterministically.

## Status

M0–M6 are complete: the IR, the builder frontend, the interpreter, the `no_std` backend, the
libraries, the SMT prover, the Kani harnesses, and the `copilot!` macro. M7, a Bluespec backend,
is the remaining milestone. See [PLAN.md](PLAN.md).

## License

[BSD-3-Clause](LICENSE), matching upstream Copilot.
