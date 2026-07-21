# copilot-rs — a Rust port of the Copilot runtime-verification language

## Context

[Copilot](https://copilot-language.github.io/) is a Haskell eDSL for writing runtime monitors for
hard-realtime embedded systems (used by NASA Langley for UAS flight monitoring). A spec is a set of
mutually-recursive infinite streams; the compiler emits a monitor that runs in **constant time and
constant memory**, and the spec itself is **verifiable** by SMT.

This repo starts empty — everything below is new code.

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

Not aspirations — each one gets an artifact that fails CI when violated.

**Realtime (constant time).** The IR has no recursion, no unbounded loops, no allocation. Array
indexing is the only variable-cost-looking op and is `O(1)` by construction. `Spec::cost()` returns
a per-step operation count broken down by type; a golden test pins it, so a spec change that
inflates WCET shows up as a diff.

**Constant memory.** `Spec::resources()` computes exact static footprint:
`Σ_streams (buffer_len × sizeof(ty)) + Σ index words + max temporaries`. The generated Rust is
`#![no_std]` with no `alloc` dependency and no `unsafe`, so the footprint is the whole story. A test
asserts the reported number equals `size_of::<Monitor>()` for every example.

**Verifiable.** Three layers, detailed in [Verification](#verification-three-layers).

---

## Workspace layout

```
copilot-rs/
  Cargo.toml                 # workspace
  crates/
    copilot-core/            # IR: types, values, ops, arena, Spec, typechecker, analyses
    copilot-lang/            # builder frontend: Stream<T>, operators, externs, triggers
    copilot-macro/           # #[derive(CopilotStruct)]  (+ copilot! sugar in M6)
    copilot-interp/          # constant-memory reference evaluator
    copilot-rust/            # no_std Rust codegen backend
    copilot-bluespec/        # Bluespec (.bs) codegen backend
    copilot-libs/            # PTLTL, LTL, MTL, clocks, voting, state machines
    copilot-theorem/         # SMT-LIB2 lowering + k-induction driver (z3 / cvc5)
    copilot-verifier/        # Kani bisimulation harness generation
    copilot/                 # facade crate + examples
  docs/
    semantics.md             # denotational semantics of the IR
    bisimulation.md          # the proof argument for layer 3
    deviations.md            # where we deliberately differ from Haskell Copilot
```

Crate names on crates.io are likely contested; publish as `copilot-rs-core` etc. with `[lib] name`
kept short. Decide before M2, it only costs a `package.name` line.

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

Expressions live in a hash-consed arena; `ExprId` is a `u32` index, so the IR is `Clone`, `Send`,
serializable, and cycle-free without `Rc`.

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
  unconstructible, but the macro path and any deserialized `Spec` need this. It is also the
  precondition every backend and proof assumes.
- `wellformed(&Spec)` — every `Drop { idx, stream }` satisfies `idx < buffer.len()`; no empty
  buffers; no zero-length arrays or empty structs (upstream rejects both); no `Exists` reaching a
  backend.
- `resources(&Spec) -> Footprint`, `cost(&Spec) -> OpCounts`.
- `order(&Spec) -> Vec<StreamId>` — not a dependency topo-sort (streams are mutually recursive by
  design); an evaluation order for the *temporaries* phase.

### Two semantic decisions to make now, because all three layers must agree

1. **Integer overflow = wrapping.** Upstream C99 inherits C's implementation-defined/UB behaviour.
   We define it: IR arithmetic is wrapping, the interpreter wraps, codegen emits `wrapping_add` etc.,
   SMT models it with `BitVec`. Total, panic-free, identical in debug and release. Record in
   `docs/deviations.md`.
2. **Array index policy.** `Index` takes a runtime `Word32`. Codegen flag
   `IndexPolicy::{ Wrap, Saturate, Assume }`, default `Wrap` (`a[(i as usize) % N]`) — constant time,
   no panic, no UB. `Assume` emits a Kani `assume(i < N)` plus a bounds-checked get, for users who
   want the obligation surfaced as a proof goal instead. Interpreter and SMT lowering follow the
   same flag.

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
let fib  = b.stream([1u64, 1], |s| s.drop(1) + s);     // [1,1] ++ (drop 1 fib + fib)

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

pub struct Monitor { s0: [u64; 1], s0_idx: usize, s1: [u64; 2], s1_idx: usize }

impl Monitor {
    pub const fn new() -> Self { /* buffers from Stream::buffer */ }
    pub fn step<E: Env, T: Triggers>(&mut self, env: &mut E, tr: &mut T) { .. }
}
```

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
`BluespecSettings { output_directory }`. Buffers → `Vector#(n, Reg#(t))`, the step → a rule or an
`Action` method, externs → `ActionValue` methods on an interface, triggers → `Action` methods.
Validate with `bsc` + `bluesim` in CI when the toolchain is present, golden files otherwise.

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
- Generated Rust is `include!`-ed into a test crate at build time and executed in-process — fast
  enough to run per-commit, unlike shelling out to a C compiler.
- `insta` snapshots of generated Rust and Bluespec so codegen churn is visible in review.
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

Scaling: emit one harness per stream group plus one whole-step harness, so large specs can be split
across `--harness` invocations.

---

## Milestones

| # | Deliverable | Done when |
|---|---|---|
| M0 | Workspace, `copilot-core` IR, typechecker, `wellformed`, `resources`, `cost` | Hand-built `Spec` typechecks; footprint test passes |
| M1 | `copilot-lang` builder, `copilot-interp`, heater example | Heater spec runs in the interpreter, matches hand-computed trace |
| M2 | `copilot-rust` backend, `#[derive(CopilotStruct)]`, arrays, layer-1 testing | `proptest` differential green; `size_of::<Monitor>()` matches `resources()` |
| M3 | `copilot-libs` (PTLTL, LTL, MTL, clocks, voting, FSM) | Upstream tutorial examples reproduce |
| M4 | `copilot-theorem` SMT + k-induction | Proves the bounded-counter property; produces a replayable counterexample on a false one |
| M5 | `copilot-verifier` Kani harnesses + `docs/bisimulation.md` | `cargo kani` green on heater + fib; a deliberately broken codegen (phases 3/4 swapped) is caught |
| M6 | `copilot!` proc-macro sugar over the builder | Heater spec expressible in macro form, desugars to identical `Spec` |
| M7 | `copilot-bluespec` | `bsc` compiles output; bluesim trace matches interpreter |

M0–M2 is the load-bearing core; M3–M7 are independently shippable and can be reordered.

---

## Verification of this work

```bash
cargo test --workspace                       # unit + typechecker + golden (insta)
cargo test -p copilot-rust --features differential   # interpreter vs generated Rust, proptest
cargo run -p copilot --example heater        # prints generated no_std monitor
cargo test -p copilot --test examples        # tutorial examples end-to-end
cargo run -p copilot-theorem --bin prove -- examples/heater.rs   # needs z3 or cvc5 on PATH
cargo kani -p copilot-verifier               # needs kani; layer-3 bisimulation
bsc -sim -p crates/copilot-bluespec/out ...  # M7 only, when bsc present
```

Negative tests that must exist, because they are what prove the harness has teeth:

- Swap phases 3 and 4 in the Rust codegen → layer-1 differential **and** `cargo kani` both fail.
- Change a buffer size without updating the spec → `resources()` test fails.
- Assert a false property → k-induction returns a counterexample that the interpreter reproduces.

---

## Risks and open items

- **crates.io naming** — `copilot*` likely taken; settle on a prefix before M2.
- **Kani scale** — large specs may blow up CBMC. Mitigated by per-stream harness splitting; if it
  still bites, fall back to proving the hot streams and documenting the residual.
- **SMT floats** — the `Real`-vs-`FloatingPoint` fork above is a soundness caveat that must be
  surfaced in the tool's own output, not just the docs.
- **Bluespec toolchain in CI** — `bsc` is open source but heavy; gate M7 tests behind a feature.
- **Struct/array frontend ergonomics** — the derive macro is the least certain piece of the
  frontend; if it drags, ship M2 with scalars and arrays only and defer structs.
