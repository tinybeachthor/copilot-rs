# copilot-rs — a Rust port of the Copilot runtime-verification language

## Context

[Copilot](https://copilot-language.github.io/) is a Haskell eDSL for writing runtime monitors for
hard-realtime embedded systems (used by NASA Langley for UAS flight monitoring). A spec is a set of
mutually-recursive infinite streams; the compiler emits a monitor that runs in **constant time and
constant memory**, and the spec itself is **verifiable** by SMT.

**Status: M0–M8 complete.** See the milestone table below. This document is the plan; decisions taken
along the way — and the reasons for them — are recorded in [docs/deviations.md](docs/deviations.md),
which is the living record. Where a sketch below disagrees with the code, the code is right.

The goal is a Rust implementation preserving the three design objectives, not a transliteration.
Two places where Rust is genuinely better than the Haskell original, and one where it is worse:

- **Better — sharing.** Haskell `copilot-language` recovers stream sharing with `data-reify` /
  `StableName`, which is `unsafePerformIO`-based and heuristic. An arena with hash-consing gives
  deterministic, total structural sharing. This removes the single unsafest part of upstream.
- **Better — sizes.** Copilot's `Array n t` needs type-level naturals; Rust const generics give
  `[T; N]` for free, and buffer sizes become `const` in generated code.
- **Worse — GADTs.** Copilot's `Expr a` / `Type a` are GADT-indexed. Rust gets a runtime-typed IR
  plus a phantom-typed frontend handle (`Stream<T>`) plus an IR typechecker, which is the standard
  workaround and is what the verifier will trust anyway.

Decisions taken with the user:

| Question | Decision |
|---|---|
| Frontend | Arena builder as the real API; `copilot!` proc-macro sugar layered on later |
| Backends | Interpreter, `no_std` Rust codegen, Bluespec. **No C99 backend.** |
| Verification | All three layers, including Kani bisimulation of the generated monitor |

---

## How each design objective is mechanized

Not aspirations — each one gets an artifact that fails CI when violated. The jobs that do the failing
are listed in [What CI enforces](#what-ci-enforces).

**Realtime (constant time).** The IR has no recursion, no unbounded loops, no allocation. Array
indexing is the only variable-cost-looking op and is `O(1)` by construction. `Spec::cost()` returns
a per-step operation count broken down by type; a golden test pins it, so a spec change that
inflates WCET shows up as a diff.

The precise claim, since "realtime" invites a stronger reading: a step's work is a function of the
specification alone, never of the data — which is why trigger arguments are evaluated whether or not
their guard fires (`deviations.md` §13). That is what is enforced. What is *not* measured is time.
`cost()` counts operations, weighted by how expensive each class is on an embedded target, and no
benchmark converts that to cycles on any real part. Nothing here is a WCET tool, and the number
should never be quoted as one: it is an exact, comparable proxy whose value is that it cannot drift
silently, not that it predicts nanoseconds. A user needing a WCET bound runs their own analysis on
the generated monitor, which is loop-free and allocation-free precisely so that such a tool has an
easy job.

**Constant memory.** `Spec::resources()` computes exact static footprint:
`Σ_streams (buffer_len × sizeof(ty)) + Σ index words + max temporaries`. The generated Rust is
`#![no_std]` with no `alloc` dependency and no `unsafe`, so the footprint is the whole story. A test
asserts the reported number equals `size_of::<Monitor>()` for every example, and the `embedded` CI
job cross-compiles a generated monitor for `thumbv7em-none-eabihf` — a target with no `std` to fall
back on, which is the claim actually being made.

**Verifiable.** Three layers, detailed in [Verification](#verification-three-layers).

---

## Workspace layout

```
copilot-rs/
  Cargo.toml                 # workspace
  README.md                  # what the project is, and what enforces each objective
  LICENSE                    # BSD-3-Clause, matching upstream Copilot
  CHANGELOG.md               # (M9, not yet created)
  .github/workflows/ci.yml   # lints, tests, MSRV, bare-metal build, Kani proofs, bluesim
  crates/
    copilot-core/            # IR: types, values, ops, arena, Spec, typechecker, analyses
    copilot-lang/            # builder frontend: Stream<T>, operators, externs, triggers
    copilot-macro/           # #[derive(CopilotStruct)]  (+ copilot! sugar in M6)
    copilot-interp/          # constant-memory reference evaluator
    copilot-gen/             # random well-typed Spec generation, for the differential suites
    copilot-rust/            # no_std Rust codegen backend
    copilot-bluespec/        # Bluespec (.bs) codegen backend + bluesim testbench generator
    copilot-libs/            # PTLTL, LTL, MTL, clocks, voting, state machines
    copilot-theorem/         # SMT-LIB2 lowering + k-induction driver (z3 / cvc5)
    copilot-verifier/        # Kani bisimulation harness generation
    copilot/                 # facade crate + examples
  docs/
    semantics.md             # denotational semantics of the IR
    bisimulation.md          # the proof argument for layer 3
    macro.md                 # the copilot! macro: syntax, usage, workings
    deviations.md            # where we deliberately differ from Haskell Copilot
```

`copilot-gen` is not in the original plan. Random specification generation is a dev-dependency of
three suites — `copilot-rust`, `copilot-theorem`, and `copilot-verifier` — so it lives in its own
crate, depending only on `copilot-core`, `copilot-lang`, and `copilot-interp` and on no backend.

Crate names on crates.io are likely contested; publish as `copilot-rs-core` etc. with `[lib] name`
kept short. Still undecided, and no longer urgent — nothing is published, and the cost stays one
`package.name` line per crate. Settle it before the first publish; see Risks.

---

## `copilot-core` — the IR

Mirrors `Copilot.Core.{Expr,Operators,Spec}` with runtime type tags where Haskell has GADT indices.

```rust
pub enum Type {
    Bool,
    Int8, Int16, Int32, Int64,
    Word8, Word16, Word32, Word64,
    Float, Double,
    Array  { elem: Box<Type>, len: usize },
    Struct { name: &'static str, fields: Vec<(&'static str, Type)> },
}

pub trait Typed: Copy + 'static { fn ty() -> Type; fn lift(self) -> Value; }
// impls for bool, i8..i64, u8..u64, f32, f64, [T; N] (const generic), derive for structs
```

Expressions live in a hash-consed arena; `ExprId` is a `u32` index, so the IR is `Clone`, `Send`, and
cycle-free without `Rc` — and is a flat table of plain data, so serializing it would be mechanical.
No `serde` impls exist and nothing has needed them; the property is a consequence of the
representation, not a shipped feature.

```rust
pub enum Node {
    Const(Value),
    Drop      { idx: u32, stream: StreamId },   // the only reference to buffered state
    ExternVar { name: String, ty: Type },
    Local     { var: VarId, bound: ExprId, body: ExprId },
    Var(VarId),
    Op1(Op1, ExprId),
    Op2(Op2, ExprId, ExprId),
    Op3(Op3, ExprId, ExprId, ExprId),
    Label(String, ExprId),
}
```

`Op1` / `Op2` / `Op3` carry the operand `Type` exactly where upstream's GADT does — `Abs(Type)`,
`Cast { from, to }`, `GetField { struct_ty, field_ty, field_idx }`, `Index(Type)`,
`UpdateArray(Type)`, `UpdateField { .. }`, `Mux(Type)` — so the full upstream operator set is
covered (arith, `Fdiv`/`Pow`/`Logb`/`Atan2`, all the `Floating` ops, bitwise + shifts, comparisons,
array index/update, struct get/update, `Mux`).

```rust
pub struct Stream   { id: StreamId, buffer: Vec<Value>, expr: ExprId, ty: Type }
pub struct Trigger  { name: String, guard: ExprId, args: Vec<(ExprId, Type)> }
pub struct Observer { name: String, expr: ExprId, ty: Type }
pub enum   Prop     { Forall(ExprId), Exists(ExprId) }
pub struct Spec     { arena: Arena, streams: Vec<Stream>, observers: Vec<Observer>,
                      triggers: Vec<Trigger>, properties: Vec<Property> }
```

Core passes, all in `copilot-core` so every backend and the verifier share them:

- `typecheck(&Spec) -> Result<(), TypeError>` — the frontend's `Stream<T>` makes ill-typed IR
  unconstructible, but a hand-built `Spec` is not so constrained, and neither is the arena itself: it
  is what catches a drifted cached type or a mis-ordered arena (`check::corruption`). It is also the
  precondition every backend and proof assumes.
- `wellformed(&Spec)` — every `Drop { idx, stream }` satisfies `idx < buffer.len()`; no empty
  buffers; no zero-length arrays or empty structs (upstream rejects both); no `Exists` reaching a
  backend.
- `resources(&Spec) -> Footprint`, `cost(&Spec) -> OpCounts`.
- `reachable(&Spec, roots) -> Vec<ExprId>` — the set of expressions evaluated per step, backing
  `cost`. (Replaces the `order()` this plan originally called for: `Drop` reads only *committed*
  state, so no stream's transition expression depends on another's within a step, and phase 3 has no
  ordering constraint to compute.)

### Semantic decisions all three layers must agree on

1. **Integer overflow = wrapping.** *Decided and recorded (M0).* Upstream C99 inherits C's
   implementation-defined/UB behaviour. We define it: IR arithmetic is wrapping, the interpreter
   wraps, codegen emits `wrapping_add` etc., SMT models it with `BitVec`. Total, panic-free,
   identical in debug and release.
2. **Array index policy.** *Implemented (M1 interpreter, M2 backend).* `Index` takes a
   runtime `Word32`. Codegen flag `IndexPolicy::{ Wrap, Saturate, Assume }`, default `Wrap`
   (`a[(i as usize) % N]`) — constant time, no panic, no UB. `Assume` emits a Kani `assume(i < N)`
   plus a bounds-checked get, for users who want the obligation surfaced as a proof goal instead.
   Interpreter and SMT lowering follow the same flag.
3. **Equality is scalar-only.** *Decided and implemented (M0).* Upstream allows `==` on any `Eq`
   instance including arrays and structs; we reject aggregates, because comparing one compiles to a
   fully-unrolled element-wise walk and M5's proof depends on `step()` staying small. Constrains M3
   (voting compares elements, not whole arrays). Cheap to relax; expensive to discover in M5.

---

## `copilot-lang` — the builder frontend

`Stream<T>` is `{ id: ExprId, _p: PhantomData<T> }`, `Copy`. Sharing = copying a handle. Operator
traits (`Add`/`Sub`/`Mul`/`Not`/`BitAnd`/…) plus inherent methods for comparisons (`.lt()`, `.eq_()`
— can't use `PartialOrd`, it returns `bool`) and `mux`.

Recursion via a closure that receives its own handle, which is how we get `x = [0] ++ (x + 1)`
without cyclic ownership:

```rust
let mut b = Builder::new();

let ctr  = b.stream([0u64],  |s| s + 1u64);            // [0] ++ (ctr + 1)
let fib  = b.stream([1u64, 1], |s| s.after(1) + s);    // [1,1] ++ (after 1 fib + fib)

let temp = b.extern_::<f32>("temperature");
let ctemp = (temp * 9.0 / 5.0) + 32.0;

b.trigger("heaton",  ctemp.lt(18.0), args![ctemp]);
b.trigger("heatoff", ctemp.gt(21.0), args![ctemp]);
b.property_forall("bounded", ctr.lt(u64::MAX));

let spec = b.finish()?;                                 // runs typecheck + wellformed
```

`b.stream(init, f)` reserves the `StreamId` and buffer first, hands the closure a `Stream<T>`
denoting `Drop { idx: 0, stream: id }`, then installs the resulting expression. No unsafe, no
`RefCell` cycles.

Structs via `#[derive(CopilotStruct)]` in `copilot-macro`: generates the `Typed` impl with field
layout, plus typed field accessors on `Stream<MyStruct>` returning `Stream<FieldTy>`. This is the
fiddliest frontend work — schedule it in M2, not M1.

---

## Backends

All three consume `&Spec` after `typecheck`; none re-derives semantics.

### `copilot-interp` (M1)

Reference evaluator with the same ring buffers the codegen uses, so it is a genuine constant-memory
implementation rather than a lazy-list oracle. Drives from a supplied extern trace, yields observer
values and fired triggers per step. This is the oracle for layer-1 testing.

### `copilot-rust` (M2) — the flagship

`#![no_std]`, no `alloc`, `#![forbid(unsafe_code)]`. Externs and triggers become traits so the
monitor is dependency-injected and testable:

```rust
pub trait Env      { fn temperature(&mut self) -> f32; }
pub trait Triggers { fn heaton(&mut self, a0: f32); fn heatoff(&mut self, a0: f32); }

#[repr(C)]                                       // required; see below
pub struct Monitor { s0: [u64; 1], s1: [u64; 2], s1_idx: u32 }

impl Monitor {
    pub const fn new() -> Self { /* buffers from Stream::buffer */ }
    pub fn step<E: Env, T: Triggers>(&mut self, env: &mut E, tr: &mut T) { .. }
}
```

The state layout is a contract with `copilot_core::resources`, which M0 already computes and tests
against a hand-written struct — so M2 must match it, not the other way round:

- `#[repr(C)]`, fields in stream order, buffer then index. `repr(Rust)` may reorder fields, which
  would make the reported footprint unfalsifiable.
- Indices are `u32` (`copilot_core::INDEX_BYTES`), not `usize`, so a footprint does not depend on the
  target's pointer width.
- A stream buffering one value carries **no** index — it is always read and written at slot 0. Note
  `s0` above.

`step()` is strictly four phases, and the phase split is the semantic crux the bisimulation proof
asserts:

1. **Sample** every extern exactly once into a local.
2. **Observe & fire** — compute guards and trigger args from the *current* buffers; call trigger
   methods in spec order.
3. **Compute next** — evaluate each stream's transition expression from the *current* buffers into
   temporaries. Nothing written back yet.
4. **Commit** — write temporaries into buffers, advance indices (`% N`, const `N`).

Getting 3 and 4 backwards is the classic bug; it is exactly what the Kani harness rules out.

### `copilot-bluespec` (M7)

Emits `.bs` (Bluespec Classic), mirroring upstream's naming: module name doubles as file prefix,
`BluespecSettings { output_directory }`. Buffers → `Vector#(n, Reg#(t))`, the step → one rule or one
`Action` method — singular, for reasons below — externs → `ActionValue` methods on an interface,
triggers → `Action` methods.

`copilot-rust` was specified here down to the field order of its state struct. The same is owed to
the second backend, because hardware does not merely re-render the Rust — it changes what each of the
three objectives means. Six things to settle before writing the emitter; check upstream's
`copilot-bluespec` first on each, since it faced all of them.

**The four phases mostly collapse, and that is the thing to verify.** A rule's register writes land at
the end of the cycle and reads inside it see the values from the start, so "evaluate from the current
buffers, then commit" is not a discipline the backend imposes — it is what `Reg` already means.
Phases 3 and 4 are inseparable inside one rule, and the phase-swap bug the Kani harness exists to
catch is not expressible. The obligation moves rather than disappearing: it becomes *everything is
in one rule*, since two rules could be scheduled in either order and the reads in the second would
see the first's writes. Phase 2 is where the real care goes — triggers are `Action` methods, so their
spec order becomes an ordering constraint on actions within the rule, not a statement sequence.

**`cost()` becomes area, not time.** The per-step operation count predicts combinational logic and
critical-path depth. The step is one cycle by construction, so the realtime objective is met
trivially and the question that replaces it — does the emitted logic close timing at the target
clock? — is one only `bsc` can answer. `resources()` still denotes something real (register bits),
but there is no `#[repr(C)]` to make it falsifiable and no `size_of` to compare against; the honest
check is against `bsc`'s own area report. If that cannot be automated, the footprint claim for this
backend is weaker than M2's and the docs must say so rather than reprint a number nothing verifies.

**`%` is a divider.** Index advance and `IndexPolicy::Wrap` both lower to modulo, which is one
instruction in Rust and, for a non-power-of-two modulus, a divider on the critical path in hardware.
For index advance there is an exact escape and it should be taken: `idx == N-1 ? 0 : idx+1`, which is
what a hand-written monitor would do. A runtime `Index` under `Wrap` has no such escape — it needs
either power-of-two array lengths or `IndexPolicy::Assume`, and which one is a user-visible
restriction that belongs in `deviations.md`.

**Floats are out of scope for M7 — and here we are behind upstream, not ahead.** Upstream's
`copilot-bluespec` 4.8 supports them; it depends on `fp-ieee` and `ieee754`, and 4.7.1 specifically
fixed its handling of special float values. So this is not a hardware limitation to discover, it is
work not being done yet. The blocker on our side is narrower than floats in general: nothing in
`FloatingPoint#(e,m)` corresponds to the `libm` transcendentals the Rust backend calls, and the
differential compares numbers, not just which function was called — the same reason M5 refuses
transcendentals rather than verifying them against a stub. Ship M7 restricted to bool and integer
specs, rejecting floats at `generate()` with an error that names the restriction and says it is
temporary; the corpus is already integer-first from M5. Lifting it is a follow-up, and upstream's
implementation is the reference.

**Verification reaches layers 1 and 2, not 3.** Layer 3 is Kani over Rust; there is no CBMC for
Bluespec. The generated hardware gets differential testing against the interpreter, and it inherits
the layer-2 proofs for free because those are statements about the `Spec`, not about any backend —
but there is no bisimulation. This is the one place the second backend is strictly weaker than the
first, and it needs a `deviations.md` entry, or "verifiable" spreads to it by association.

**Testing, concretely.** Golden `.bs` files under `crates/copilot-bluespec/tests/golden/`, rewritten
with `UPDATE_GOLDEN=1`, same as M2. The differential follows `random_specs.rs`, not `differential.rs`:
`.bs` cannot be `include!`-ed, so it is emit → `bsc -sim` → `bluesim` → compare printed events
against the interpreter, batching specs into one toolchain invocation the way the `rustc` harness
does. The testbench takes its trace as a `Vector` of samples indexed by a cycle counter and `$display`s
observers and fired triggers in a format the harness parses. Gate on `bsc` with `COPILOT_REQUIRE_BSC`
in CI, per the Risks entry.

#### As built (M7)

`Settings { name, output_directory, index_policy }`. Three packages — `<Name>Types` (structs, when
there are any), `<Name>Ifc`, `<Name>` — plus `testbench()`, which emits a fourth driving the monitor
over a recorded trace and printing one line per event. Buffers are one `Reg` per slot, gathered into
a `Vector n (Reg t)` only where the read index is dynamic. Answering the six points above in order,
because two of them changed what shipped:

1. **The phases did collapse, and the obligation did move.** Everything is in one rule, so phases 3
   and 4 are inseparable and the swap is not expressible — `deviations.md` §27. That is asserted
   rather than assumed: `a_stream_reading_a_committed_value_is_caught` builds the swap by hand and
   requires bluesim to disagree with the interpreter, which shows the bug is expressible in Bluespec
   and that the *generator* has no way to introduce it. Trigger order within the rule follows spec
   order, and bluesim confirms it, since the comparison is order-sensitive.
2. **The footprint claim was weakened, as this section demanded.** `bsc` has no machine-readable
   area report to compare against, so the emitted header no longer prints a byte count as if it were
   checked: it reports the register bits the buffers hold and says explicitly that this is the
   specification's own state and not the synthesised area. `deviations.md` §30. This is the one place
   where the second backend's constant-memory story is weaker than M2's, and it now says so in the
   generated file rather than only here.
3. **The `%` escape was taken for index advance**, exactly as prescribed: `idx + 1 >= N ? 0 : idx + 1`
   for the advance and one conditional subtraction for a `drop i` read. A runtime `Index` under
   `Wrap` still emits `%`, which is a divider for a non-power-of-two length — recorded as a
   user-visible cost in `deviations.md` §6, with `Assume` and power-of-two lengths as the escapes.
4. **Floats are refused, and the reason turned out to be larger than transcendentals.** The blocker
   is not only that `FloatingPoint#(e,m)` lacks `libm`: `bsc` fails during *elaboration* on `x < 4.0`
   ("Unordered comparison of type `FloatingPoint`") and on float division. So a float spec would not
   merely lose precision, it would not build. The error names the type and the restriction;
   `deviations.md` §26. Still a gap against upstream 4.8, not a design win.
5. **Layers 1 and 2 only**, as predicted, and now written down — `deviations.md` §29.
6. **Testing followed the sketch**, with one departure: each specification gets its own `bsc`
   invocation rather than being batched. Eleven specs cost about 24 seconds, which is cheap enough
   that batching would trade a real diagnostic — the failing spec's own directory, left on disk — for
   nothing. The suite also checks both non-default index policies and that `bsc` emits no warnings on
   generated code.

---

## `copilot-libs` (M3)

Straight ports as combinator functions over `Stream<bool>` / `Stream<T>`, all bounded-past so
constant memory is preserved:

- **PTLTL** — `since`, `previous`, `alwaysBeen`, `eventuallyPrev` (single-bit state each).
- **LTL** — bounded-future over a fixed window `n`, exactly as upstream.
- **MTL** — bounded future/past against an explicit clock stream.
- **Clocks** — `clk period phase`, `tick`.
- **Voting** — Boyer–Moore MJRTY majority + `aMajority` check; needs array streams, so it lands
  after struct/array support.
- **State machines** — the 4.7.x addition: transition-table-driven FSM over an enum-like `Word8`.

---

## Verification (three layers)

### Layer 1 — differential + golden testing (M1–M2, continuous)

- `proptest` strategy generating **well-typed** random `Spec`s (generate against `Type`, not
  post-filter) plus random extern traces. Run interpreter vs generated Rust over N steps and assert
  identical observer values and trigger call sequences.
- The generated Rust for each corpus spec is checked in under `crates/copilot-rust/tests/golden/` and
  `include!`-ed into the test binary, so the differential runs in-process on every commit with no
  `rustc` subprocess. Random specs, which do not exist until test time, do shell out (`random_specs.rs`).
- Those same golden files are the codegen-churn diff — a plain checked-in-output test, rewritten with
  `UPDATE_GOLDEN=1`, rather than a snapshot library. Being ordinary `.rs` files is what lets the
  differential compile them; a snapshot format could not do double duty.
- Examples from the upstream tutorial (heater, engine monitor, voting) as end-to-end cases.

### Layer 2 — `copilot-theorem`: SMT + k-induction (M4)

The `Copilot.Theorem.What4` analogue. Lower `Spec` to a transition system whose state is every
buffer cell, emit SMT-LIB2, and drive `z3` / `cvc5` over stdin — a pipe, not FFI, so there is no
build-time solver dependency.

- **k-induction** with `k = max buffer depth` across involved streams, matching upstream's
  heuristic. Base case + inductive step; sound, incomplete — a failure means "not inductive at this
  k", never "false".
- Counterexample extraction from `get-model`, replayed through the interpreter so the user sees a
  concrete failing trace rather than a model dump.
- `Forall` only; `Exists` rejected at the API boundary, as upstream does.
- Integers → `BitVec 8/16/32/64`, exactly matching the wrapping semantics decided above.
- **Floats are a real fork.** Default to `Real` approximation with a loud warning in the result
  (fast, unsound for overflow/NaN corners); `--fp=ieee` selects SMT `FloatingPoint` for exactness at
  large cost. Upstream has the same tension; making the choice explicit and reported is the
  improvement.

### Layer 3 — `copilot-verifier`: Kani bisimulation (M5)

The `copilot-verifier` analogue, using CBMC-via-Kani instead of Crucible. For a given spec, generate
a harness crate:

```rust
#[kani::proof]
fn step_bisimulates() {
    let mut m = Monitor { s0: kani::any(), s0_idx: kani::any(), .. };
    kani::assume(m.s0_idx < S0_LEN && ..);              // representation invariant
    let pre  = abstract_state(&m);                      // impl state -> IR state
    let ext  = Externs { temperature: kani::any() };
    let mut rec = RecordingTriggers::new();

    m.step(&mut FixedEnv(ext), &mut rec);

    let post = ir_step(pre, ext);                       // independent IR-level reference
    assert_eq!(abstract_state(&m), post.state);
    assert_eq!(rec.calls(), post.triggers);
}
```

Two things make this stronger than generic bounded model checking, and both must be written up in
`docs/bisimulation.md`:

1. **No unwind bound is needed.** `step()` is loop-free by construction — const-generic array sizes,
   straight-line stream updates — so CBMC's unrolling is exact. The harness proves the transition
   relation for *all* states and *all* extern inputs, not up to a bound.
2. **One-step bisimulation lifts to traces.** One-step equivalence + the representation invariant
   being preserved + agreement at the initial state gives full trace equivalence by induction on
   steps. That induction is a short pen-and-paper argument in the doc, not a CBMC obligation.

**The trap to avoid:** `ir_step` must be produced by a structurally *different* lowering than
`copilot-rust` — a direct denotational unfolding of the IR over an explicit state vector, no ring
buffers, no index arithmetic. If both come from the same emitter, the proof only shows the generator
equals itself. Enforce it: `ir_step` generation lives in `copilot-verifier` and is forbidden from
depending on `copilot-rust`, checked by a `cargo deny`-style dependency test.

Scaling: as shipped, one whole-step harness per spec, which is ample for the corpus and keeps the
proof a single obligation. Per-stream-group splitting across `--harness` invocations is the escape
hatch if a real spec ever exceeds what CBMC discharges quickly; not needed yet.

*As built (M5):* transcendentals are refused rather than verified against a stub (`libm` calls CBMC
cannot see through); floats are permitted but slow, so the corpus is integer-first. The independence
rule is enforced by a manifest-and-tree test, and the reference is itself differential-tested against
the interpreter, so `ir_step ≈ interpreter` by testing composes with `monitor ≡ ir_step` by proof.

---

## Parity with upstream

"A Rust port of Copilot" needs a completion criterion, or it stays subjective. Upstream ships as a
family of `copilot-*` packages, so the criterion is the family: every package either has a
counterpart here or an entry in `deviations.md` saying why it never will. Checked against Hackage at
upstream 4.8 (2026-08-12):

| Upstream 4.8 | Here | |
|---|---|---|
| `copilot-core` | `copilot-core` | done, plus the typechecker and analyses upstream has no need for |
| `copilot-language` | `copilot-lang` | done |
| — | `copilot-macro` | ours: `copilot!` and `#[derive(CopilotStruct)]`, doing what Haskell gets from `do` notation and type classes |
| `copilot-libraries` | `copilot-libs` | done (M3) |
| `copilot-interpreter` | `copilot-interp` | done |
| `copilot-theorem` | `copilot-theorem` | done (M4). Upstream drives What4; we emit SMT-LIB2 down a pipe |
| `copilot-verifier` | `copilot-verifier` | done (M5). Upstream is Crucible over LLVM against the C99 output; ours is CBMC-via-Kani against the Rust output. Same bisimulation argument, different machinery |
| `copilot-bluespec` | `copilot-bluespec` | done (M7), and behind in one respect: upstream supports floats, we refuse them — `deviations.md` §26 |
| `copilot-c99` | — | deliberate, `deviations.md` §8 |
| `copilot-prettyprinter` | `copilot-core::print` | done (M8), as a module rather than a package — `deviations.md` §31 |
| `copilot` | `copilot` | done, though the facade re-exports less; see Risks |
| — | `copilot-gen` | ours: random well-typed specs, which upstream has no equivalent of |

One row is a deliberate, permanent gap (`copilot-c99`, `deviations.md` §8); every other package either
has a counterpart here or, as of M8, prints one. `copilot-prettyprinter` was never in this plan — it
went straight from an IR to three backends without noticing that a user who writes a spec has no way
to *look* at one. That mattered more here than upstream, because `copilot!` desugars invisibly and
hash-consing rewrites what the user wrote; M8 closed it.

## Milestones

| # | Status | Deliverable | Done when |
|---|---|---|---|
| M0 | **done** | Workspace, `copilot-core` IR, typechecker, `wellformed`, `resources`, `cost` | Hand-built `Spec` typechecks; footprint test passes |
| M1 | **done** | `copilot-lang` builder, `copilot-interp`, heater example | Heater spec runs in the interpreter, matches hand-computed trace |
| M2 | **done** | `copilot-rust` backend, `#[derive(CopilotStruct)]`, arrays, layer-1 testing | `proptest` differential green; `size_of::<Monitor>()` matches `resources()` |
| M3 | **done** | `copilot-libs` (PTLTL, LTL, MTL, clocks, voting, FSM) | Upstream tutorial examples reproduce |
| M4 | **done** | `copilot-theorem` SMT + k-induction | Proves the bounded-counter property; produces a replayable counterexample on a false one |
| M5 | **done** | `copilot-verifier` Kani harnesses + `docs/bisimulation.md` | `cargo kani` green on the corpus (fib, lag, an integer thermostat, struct and array specs — floats refused, see below); the phase-3/4 swap and a corrupted commit are caught |
| M6 | **done** | `copilot!` proc-macro sugar over the builder | Heater spec expressible in macro form, desugars to identical `Spec` |
| M7 | **done** | `copilot-bluespec` | `bsc` compiles the integer corpus; bluesim events match the interpreter; golden `.bs` checked in; floats refused by name, not miscompiled |
| M8 | **done** | `Spec` pretty-printer, closing the last parity gap | Every corpus spec round-trips to text a reader can check against the source; the `copilot!` desugaring is inspectable; counterexamples print as specs, not model dumps |
| M9 | next | First publish | Names settled, `CHANGELOG.md`, the semver surface written down (see below), docs.rs green |

M0–M2 is the load-bearing core; M3–M8 are independently shippable and can be reordered.

Carried forward, to do before the milestone that depends on it:

- *(nothing outstanding — the four items carried since M0 were cleared after M4)*

Cleared: random specification generation now feeds both the SMT encoding (`copilot-theorem`) and a
`rustc` harness that compiles generated monitors and compares them against the interpreter
(`crates/copilot-rust/tests/random_specs.rs`); `Error::TypeDrift` and `Error::NonMonotonicArena` have
in-crate corruption tests, which M5's soundness argument rests on; `Local` erasure is exercised by a
corpus entry and no longer warns; and the `libm`/`std` split is gone, since the interpreter now uses
`libm` too.

### M8 — printing a `Spec`

Upstream keeps its pretty-printer in its own package because it wants the `pretty` library. We have
no such reason: every crate here already depends on `copilot-core`, and core is deliberately
dependency-free. So this is a module in `copilot-core`, not a tenth crate — a `Display` for `Spec`
plus a way to print one expression, hand-written.

Three callers make it worth doing, and they constrain the format:

- **Reading what `copilot!` produced.** The macro desugars invisibly, `Local` bindings are erased,
  and hash-consing rewrites structure. `docs/macro.md` currently explains the translation; printing
  the result *shows* it, and M6's "desugars to an identical `Spec`" test could assert on text rather
  than on graph equality.
- **Counterexamples.** M4 already replays a failing model through the interpreter so the user sees a
  trace. Printing the spec beside it closes the loop from model dump to something a person reads.
- **Review.** Golden `.rs` and `.bs` files show what the *backends* did; nothing shows what the
  frontend built.

The format should follow the source, not the arena: `Drop` back to `drop n s`, shared subexpressions
named rather than duplicated, and struct and array literals as written. Round-tripping to a parser is
explicitly *not* a goal — that is a second frontend to keep in sync, and the `copilot!` macro already
occupies that niche.

#### As built (M8)

A `Display` impl for `Spec` and a `format_expr(spec, id)` for one expression on its own, both in
`crates/copilot-core/src/print.rs` — a module, not a tenth crate, exactly as sketched. The three
motivating callers all got something concrete:

1. **Reading what `copilot!` produced.** `crates/copilot-lang/tests/macro_spec.rs` gained
   `the_desugared_heater_prints_legibly`, which prints the macro's own `heater_macro()` and asserts
   that the shared `celsius` binding appears once rather than being re-derived at each of its four
   uses. `docs/macro.md`'s "erased" claim about `Local` was prose only before this; it is now
   something a test reads off the printed text.
2. **Counterexamples.** `copilot_theorem::Proof::describe(&self, spec)` prints the failing property's
   own text via `format_expr`, then each step of the counterexample's external inputs by declared
   name and `Value`'s own `Display` — `deviations.md` §32. `Display for Proof` is untouched; `describe`
   is the expanded form.
3. **Review.** Golden `.txt` files under `crates/copilot-lang/tests/golden_print/` (`heater`,
   `bounded_counter`, `structs`), checked in the same way as the `.rs` and `.bs` golden corpora and
   rewritten with `UPDATE_GOLDEN=1`.

Two decisions the sketch above did not settle, resolved while building it — both in `deviations.md`
§31:

- **What gets named.** Hash-consing makes sharing structural, so a node used more than once — by
  actual reference count, not by guesswork — is named `t{id}` and printed once. A `Node::Label` is
  named after itself instead, since a user chose that name; a `Node::Local` is always named,
  regardless of use count, which is what makes the `crates/copilot-rust/tests/support/mod.rs::locals`
  corpus entry (a reachable binding nobody reads) print sensibly rather than needing a special case.
- **Precedence is not tracked.** Every inlined compound operand is parenthesized whether or not the
  parentheses are load-bearing. Slightly more verbose than a precedence table; never ambiguous, which
  is what matters for a format nothing parses back.

Tests live where the printer's callers live, not only in `copilot-core`: unit tests for the naming
rules in `print.rs` itself, golden and content tests in `copilot-lang` (builder specs and the macro's
desugaring), and hand-built-`Proof` tests in `copilot-theorem/tests/describe.rs` that need no solver,
since the rendering has nothing to do with how the `Proof` was produced.

### M9 — publishing

Nothing here is on crates.io, and the plan has never said what publishing would commit us to.

**Names.** `copilot-rs-*` with short `[lib] name`s, so `use copilot_core::…` keeps working. Decide
once, in one commit, across all crates.

**Versions in lockstep**, as upstream does — all its packages sit at 4.8, and its inter-package
bounds are exact ranges. The workspace already shares one `version` and the path dependencies already
carry it, so this costs nothing and avoids a matrix of compatible pairs.

**The semver surface is larger than the Rust API**, and this is the part worth writing down before
the first publish rather than discovering after it. A user depends on three things:

1. The crates' Rust APIs, as usual.
2. **The generated code's interface** — `Env` and `Handler` method names, `Monitor` field names and
   layout, `new`, `step`. Renaming a generated trait method breaks every user's code while changing
   no signature in any of our crates. The golden files are the de facto record of this surface, so a
   golden diff is a semver signal and should be read as one in review.
3. **The semantics in `deviations.md`** — index policy, wrapping, totality. Changing what a monitor
   *computes* is the most breaking change available here and the least visible in a diff.

**MSRV bumps are semver-visible**; the `msrv` job pins the claim, and moving it is a minor bump at
minimum.

**Also needed:** a `CHANGELOG.md` (there is none), and docs.rs, which should be free — `cargo doc
--workspace --no-deps` already runs under `-D warnings` in CI.

---

## Verification of this work

These are the real commands, as the crates are actually laid out.

```bash
cargo test --workspace                       # everything; 196 tests
cargo clippy --workspace --all-targets       # clean, no allows in generated code
cargo run -p copilot --example heater        # footprint + per-step cost, then a driven trace
cargo run -p copilot-rust --example emit_crate -- DIR   # writes a generated monitor crate to DIR
```

Per-layer, when the optional tool is present (each suite skips cleanly otherwise):

```bash
cargo test -p copilot-rust                   # layer 1: interpreter vs generated Rust
                                             #   differential.rs (golden corpus, proptest inputs),
                                             #   random_specs.rs (rustc-compiled random specs),
                                             #   no_std.rs (-D warnings against a libm stub)
cargo test -p copilot-theorem                # layer 2: SMT k-induction; needs z3 or cvc5 on PATH
                                             #   prove.rs (proofs + replayable counterexamples),
                                             #   encoding.rs (encoding vs interpreter, both solvers)
cargo test -p copilot-verifier --test kani   # layer 3: Kani bisimulation; needs cargo-kani
                                             #   also runs under `cargo test --workspace`
cargo test -p copilot-bluespec               # M7: bsc + bluesim vs the interpreter; needs bsc
                                             #   bluesim.rs (simulated trace vs interpreter),
                                             #   golden.rs (checked-in .bs, no toolchain needed)
```

To look at what the Bluespec backend emits, or to run it by hand:

```bash
cargo run -p copilot-bluespec --example emit_packages -- /tmp/monitor
cd /tmp/monitor && bsc -sim -u -g mkMonitorSim MonitorSim.bs \
  && bsc -sim -e mkMonitorSim -o sim.out mkMonitorSim.ba && ./sim.out
```

The negative tests are the point of the suites, because they are what prove the harness has teeth —
each is a real, passing test that asserts the *failure*:

- `copilot-rust`: swap the compute/commit phases, or corrupt a buffer index → the differential
  disagrees with the interpreter. Mutation-checked in the differential's history.
- `copilot-verifier`: `a_corrupted_commit_is_refuted` and `a_phase_swap_is_refuted` → `cargo kani`
  finds a counterexample.
- `copilot-theorem`: a false property → k-induction returns a trace that the interpreter reproduces
  at the same step (`refutes_a_false_property_with_a_replayable_trace`).
- `copilot-core`: a drifted cached type or a mis-ordered arena → `typecheck` catches it
  (`check::corruption`).
- `copilot-bluespec`: freeze a monitor's rotating index, or make one stream read another's committed
  value → bluesim disagrees with the interpreter (`a_frozen_ring_buffer_index_is_caught`,
  `a_stream_reading_a_committed_value_is_caught`).

### What CI enforces

[.github/workflows/ci.yml](.github/workflows/ci.yml) — six jobs, on every push to `main` and every
pull request, with a newer push cancelling the older run. `RUSTFLAGS: -D warnings` applies to this
workspace only, since Cargo caps lints for dependencies. Every cargo invocation is `--locked`, so a
run checks the committed lockfile rather than whatever resolved that morning.

| Job | Runs | Because |
|---|---|---|
| `check` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`, `cargo doc --workspace --no-deps` under `RUSTDOCFLAGS: -D warnings` | Generated code is documented as compiling clean in the *user's* build, and the crates carry `missing_docs = warn`. Both are claims only while this is green. |
| `test` | `cargo test --workspace` with z3 and cvc5 installed, `COPILOT_REQUIRE_SOLVER=1` | The whole suite, layers 1 and 2, against both solvers. Kani skips here and gets its own job. |
| `msrv` | `cargo check --workspace --all-targets` on the toolchain named by `rust-version` | `rust-version = 1.88`: let-chains under edition 2024, used in `copilot-core`, `copilot-rust` and `copilot-theorem`. The job reads the version out of the manifest rather than repeating it, so the two cannot drift. |
| `embedded` | `emit_crate` → `cargo build --target thumbv7em-none-eabihf` | The constant-memory objective, checked on a machine that has no `std` at all rather than on a host that merely went unused; see below. |
| `kani` | `cargo test -p copilot-verifier --test kani`, `COPILOT_REQUIRE_KANI=1` | Layer 3. The `test` job skips the proofs; they run here, with a Kani install of their own, so the rest of the suite reports without waiting on CBMC. |
| `bluespec` | `cargo test -p copilot-bluespec` with a pinned `bsc`, `COPILOT_REQUIRE_BSC=1`, then `emit_packages` → `bsc -sim` → bluesim | Layer 1 for the second backend. The `test` job has no toolchain, so the simulation skips there; here it runs, and the second step exercises the path a user takes rather than the one the harness takes. |

Three decisions in there are load-bearing, and each is the sort of thing that quietly rots:

**A skipped suite must be a failure.** Locally the optional layers skip cleanly, so `cargo test
--workspace` is green on a bare checkout with no solver and no Kani — which is right for a
contributor and wrong for CI. `COPILOT_REQUIRE_SOLVER` and `COPILOT_REQUIRE_KANI` turn the skip path
into an error. A verification suite that skips is indistinguishable from one that passes, and for
this project that is the worst available way to be wrong.

**`no_std` is tested twice, differently.** `no_std.rs` compiles generated code for the *host*
against a `libm` stub, which shows the code needs nothing from `std`. The `embedded` job builds it
for a machine that *has* no `std`. The first can pass on a target where the second fails.

**Solvers and `bsc` are pinned, Kani is not cached.** cvc5 is fixed at 1.3.4 and `bsc` at 2024.07 rather than tracking latest,
because a silently changing solver makes a failure hard to attribute; bumping it is a visible commit.
Kani is installed from scratch every run: `cargo kani setup` writes a bundle whose toolchain is a
symlink into `~/.rustup`, which the job reinstalls, so caching `~/.kani` restores a dangling link and
the failure names nothing that suggests the cache. It takes about fifteen seconds.

---

## Risks and open items

- **crates.io naming** — `copilot*` is likely taken; the crates are unpublished, so this is still
  open. Settle on a prefix (`copilot-rs-*` with short `[lib] name`s) as part of M9.
- **Kani scale** — large specs may blow up CBMC. The current harness proves the whole step at once,
  which is ample for the corpus; per-stream-group splitting is the escape hatch if a real spec bites.
- **Bluespec area is unverified** — *open.* The generated monitor reports the register bits the
  specification's buffers hold, which is a statement about the `Spec` rather than about what `bsc`
  synthesised. M2's footprint claim is checked against `size_of::<Monitor>()`; this one is checked
  against nothing. Closing it means parsing `bsc`'s area output, which is not machine-readable
  today — so the claim is stated narrowly instead. `deviations.md` §30.
- **Facade surface** — the `copilot` facade re-exports the language crates (core, lang, interp) and
  the `copilot!` macro, not the backends or verifier. Those are used as their own crates. Revisit
  only if a "batteries included" story needs them — and settle it before M9, since narrowing a
  facade after publishing is a breaking change and widening one is not.
- **Upstream keeps moving** — the parity table is pinned to 4.8, checked 2026-08-12. 4.7.1 added
  state machines to `copilot-libraries`, which M3 picked up; the next release may add something
  similar. Re-check the table at M9 rather than tracking continuously.

Resolved since the plan was written:

- **SMT floats** — the real-vs-IEEE fork is surfaced in the tool's own output as a `Caveat`, and
  `Proof::is_conclusive` is false whenever one applied (M4).
- **Struct/array frontend ergonomics** — `#[derive(CopilotStruct)]` shipped in M2 with its field
  accessors, and structs and arrays are in the differential, SMT, and Kani corpora.
- **Bluespec toolchain in CI** — `bsc` is gated behind a toolchain check the way the solver and Kani
  suites are, with `COPILOT_REQUIRE_BSC` turning a missing toolchain into a failure in the job that
  installs one (M7). The golden tests need no toolchain, so codegen churn stays visible everywhere.
