# Deviations from Haskell Copilot

Where copilot-rs deliberately differs from [upstream Copilot](https://copilot-language.github.io/),
and why. Anything not listed here is intended to match upstream's behaviour; if it does not, that is
a bug.

Each entry records whether the decision is *implemented* or merely *decided* — a decision that binds
future milestones but has no code behind it yet.

---

## 1. Sharing is structural, not observed

**Implemented** (M0, `copilot-core::Arena`).

Upstream recovers sharing from the Haskell expression graph with `data-reify`, which compares
`StableName` pointer identity under `unsafePerformIO`. That is heuristic — whether two equal thunks
are shared is up to GHC — and it is the only unsafe component in the pipeline.

copilot-rs builds expressions directly into a hash-consed arena. Interning an equal node returns the
existing `ExprId`, so equal subexpressions are shared exactly, deterministically, with no unsafe
code. The frontend needs no sharing-recovery step at all: `Stream<T>` is a `Copy` handle, and using
one twice *is* sharing.

A visible consequence: sharing now depends on structural equality rather than on how the user wrote
the spec. Upstream can fail to share two identical expressions built separately; copilot-rs always
shares them.

## 2. Integer arithmetic wraps

**Implemented** (M0 as an IR-level decision; M1 in the interpreter).

Upstream's C99 backend inherits C's semantics, where signed overflow is undefined behaviour. That is
tolerable when C is the only backend and the generated code is then verified, but it does not
survive having three execution engines plus an SMT encoding that must all agree.

copilot-rs defines it: all integer arithmetic wraps.

- The interpreter wraps.
- Generated Rust uses `wrapping_add`, `wrapping_mul`, and friends, so debug and release builds
  behave identically — a monitor cannot panic in test and silently wrap in the field.
- The SMT encoding uses bitvectors, which wrap natively, so the prover reasons about the same
  semantics the monitor implements.

Specs relying on overflow being impossible should say so as a `Property` and have the prover
discharge it, rather than relying on the arithmetic to trap.

`abs` wraps too, so `i8::MIN.abs()` is `i8::MIN`. Rust's `abs` panics there and C's is undefined;
neither is available to a monitor that must not trap.

## 2a. The other partial operations are total too

**Implemented** (M1, `copilot_core::policy`).

Wrapping arithmetic settled overflow, but three more operations could still fail to denote a value.
All are defined, for the same reason: a monitor cannot signal failure, and four engines — the
interpreter, two code generators, and the SMT encoding — have to agree on exactly one answer.

| Operation | Upstream / host | copilot-rs |
|---|---|---|
| Integer `/` and `%` by zero | UB in C; panic in Rust | **Zero** |
| Shift by ≥ the operand width | UB in C; panic or modulo-width in Rust | **Zero** |
| Float comparison against NaN | False | False, in *every* direction including `<=` and `>=` |

Zero is arbitrary for division, but total. A spec that cares should carry a `Property` stating the
divisor is non-zero and let the prover discharge it. Shifting is defined as saturating to zero
rather than Rust's `wrapping_shl`, which reduces the amount modulo the width and would make
`x << 64` equal `x` — surprising, and not what any spec means.

## 2b. Float operations are evaluated at their operands' width

**Implemented** (M1, `copilot-interp`).

An `f32` operation is computed in `f32`, never in `f64` and rounded afterwards.

For `+`, `-`, `*` and `/` this makes no observable difference: double rounding through a wider
format is innocuous once the intermediate carries `2p + 2` bits, and `f64`'s 53 clears the 50 that
single precision needs. But that is a theorem about those four operations, not a property of the
type, and it fails for the transcendentals — routing `f32::exp` through `f64` changes the result for
roughly one argument in two thousand (measured: 2391 differing results in 5M random arguments).

Evaluating at the operands' own width means no engine has to know which case an operator falls into.
Pinned by `f32_operations_are_evaluated_at_f32` in `crates/copilot-interp/tests/semantics.rs`.

## 3. Equality is restricted to scalars

**Implemented** (M0, `Op2::Eq`/`Op2::Ne` require `Type::is_scalar`).

Upstream allows `==` on any type with an `Eq` instance, including structs and arrays.

Comparing an aggregate compiles to a fully-unrolled element-wise walk. Since the M5 bisimulation
proof depends on a generated `step()` being small and loop-free enough for CBMC to discharge without
an unwinding bound, aggregate comparison is excluded until there is a reason to pay for it. Compare
the fields you care about.

This is the deviation most likely to be reversed. Relaxing it later is cheap; discovering in M5 that
`step()` is too large to verify is not.

## 4. Ring-buffer indices are 32-bit

**Implemented** (M0, `copilot_core::INDEX_BYTES`).

Generated code indexes ring buffers with a `u32` rather than a `usize`. Buffer lengths come from the
spec text and are typically one to three elements, so the range is never the binding constraint,
and fixing the width makes a monitor's reported footprint a single number that does not depend on
the target's pointer width.

A stream buffering exactly one value carries no index at all: it is always read and written at slot
zero.

## 5. Monitor state is `#[repr(C)]`

**Implemented** (M0 computes the footprint this way; M2 emits it that way).

`copilot_core::resources` reports a monitor's exact state size, computed under `repr(C)` with fields
in stream order — buffer, then index, index omitted for single-element buffers.

Generated monitors declare their state exactly that way. `repr(Rust)` is free to reorder fields,
which would make the reported footprint unfalsifiable; with `repr(C)` it can be asserted against
`size_of`, and `every_monitor_occupies_exactly_the_reported_footprint` in
`crates/copilot-rust/tests/differential.rs` does so for every corpus specification against the real
compiled type.

## 6. Array index policy is explicit

**Implemented** (M1 in `copilot_core::IndexPolicy` and the interpreter; M2 in the Rust backend; M7
in the Bluespec backend).

`Op2::Index` takes a runtime `Word32`, so an out-of-range index is possible. Upstream's C backend
emits an unchecked subscript, which is undefined behaviour.

copilot-rs makes the policy explicit, defaulting to `Wrap`:

| Policy | Behaviour out of range |
|---|---|
| `Wrap` (default) | `a[i % N]` — constant time, no branch, no panic |
| `Saturate` | `a[min(i, N - 1)]` — one comparison, stays near the intended element |
| `Assume` | Not defined. Generated code subscripts directly and emits an assumption; the interpreter reports `IndexOutOfRange` rather than agreeing with a monitor whose obligation was never discharged |

Every engine must be configured with the same policy, or the interpreter stops being a valid oracle
for the generated code — which is why `Monitor::with_policy` takes it explicitly.

Bluespec adds two wrinkles, one in the user's favour and one against.

Under `Assume`, a subscript that is a compile-time constant is checked during elaboration, so `bsc`
refuses a specification that visibly breaks its obligation instead of compiling it into something
unspecified. The interpreter refuses the same specification as `IndexOutOfRange`; the two agree even
where neither produces a trace.

Against: on hardware, `Wrap` costs more than it looks. `i % N` is one instruction on a processor and,
for an `N` that is not a power of two, a divider on the critical path in a circuit. There is no exact
escape the way there is for the ring-buffer index — where `p + i` is known to be below `2N`, so a
conditional subtraction does the same job, which is what generated Bluespec emits. A runtime
subscript has no such bound. Users who care about the critical path have two options, both
specification-level: give the array a power-of-two length, or take on the obligation with `Assume`.
The Rust backend is unaffected, and the default stays `Wrap` there and here, because a monitor that
silently reads the wrong element is worse than one that is a few gates deeper.

## 7. Struct fields are named, not selected by function

**Implemented** (M0, `Op1::GetField`, `Op2::UpdateField`).

Upstream's `GetField` carries a selector function `a -> Field s b` and recovers the field name from
a type-level symbol. copilot-rs stores the field name directly, and derives the field's type from
the struct type. Generated code needs the name anyway, and one representation cannot drift from the
other.

## 8. No C99 backend

**Decided.**

Upstream's flagship output is C99. copilot-rs targets `no_std` Rust instead, with Bluespec for
hardware. Dropping C also drops the need for a Crucible-style bisimulation argument over LLVM: the
monitor and its proof harness are both Rust, so Kani can verify the artefact that actually ships.

## 9. `order()` is unnecessary

**Implemented** by omission (M0).

The plan called for an evaluation order over streams in the "compute next" phase. There is none to
compute: `Drop` reads only committed buffer state, so no stream's transition expression depends on
another's within a step, and any order is correct. `copilot_core::reachable` covers what the
analyses actually needed.

## 10. `drop` applies to any expression, by distributing

**Implemented** (M1, `Builder::shift`).

`drop n` denotes a shift forward in time, and shifting distributes over every pointwise operator:
`drop n (a + b)` is `drop n a + drop n b`. The frontend implements it by rewriting the expression,
pushing the shift down to the `Drop` leaves where it becomes a deeper read of a stream's buffer.

So `drop` is available on arbitrary expressions rather than only on stream handles, which is what
lets `Stream<T>` stay a single type instead of splitting into buffered and unbuffered variants. It
bottoms out in two places, both reported by `Builder::finish`:

- **an external variable**, whose next sample does not exist yet, at any depth; and
- **a stream buffered too shallowly** to be read that far ahead.

The rewrite is memoized, because hash-consing means one subexpression can be reached along many
paths and shifting it once per path would be exponential in the depth of the sharing.

## 11. The frontend's errors are deferred, not returned

**Implemented** (M1).

`a + b` has nowhere to put a `Result`, so the builder records the first error and reports it from
`finish()`.

This is affordable because almost nothing in the frontend can fail. The marker traits in
`copilot_lang::classes` stand in for upstream's Haskell class constraints — `Num`, `Integral`,
`Floating`, `Bits`, `Ord` — so every operator is offered only at the types it is defined for, and a
spec that compiles is well-typed. What remains is `drop` misuse and invalid identifiers.

The first error is kept rather than the last: later failures are usually consequences of the first,
and the stand-in handle returned after a failure would otherwise generate a cascade of less
informative ones.

## 12. Struct fields are reached through a generated trait

**Implemented** (M1 for the IR; M2 for the frontend, via `#[derive(CopilotStruct)]`).

Upstream reaches a struct field with a type-level symbol and a selector function. copilot-rs
generates a `<Name>Fields` trait next to the struct, implemented for `Stream<'_, Name>`:

```rust
#[derive(Clone, Copy, CopilotStruct)]
#[repr(C)]
struct Reading { altitude: f32, valid: bool }

let climbing = sensor.altitude().gt_val(1000.0);   // Stream<f32>
let cleared  = sensor.set_altitude(b.lit(0.0));    // Stream<Reading>
```

A trait rather than inherent methods because `Stream` belongs to another crate, and only the
defining crate may add inherent methods to a type. `Stream::field` and `Stream::with_field` take the
field name as a string and are what the generated accessors are built from; they are public, but
they move the field-name check from compile time to `Builder::finish`, so prefer the accessors.

## 13. Trigger arguments are always evaluated

**Implemented** (M2, `copilot-rust`).

The interpreter evaluates a trigger's arguments only when its guard holds. Generated code evaluates
every reachable subexpression up front, guard or no guard, and the `if` merely chooses whether to
call the handler.

Expressions are pure, so this is unobservable — and evaluating unconditionally is the point: it is
what makes a step's timing independent of its data, which is the whole claim behind "hard realtime".
A monitor whose execution time depended on whether an alarm fired would leak its own verdict into
its schedule.

The one visible consequence is under `IndexPolicy::Assume`, where an out-of-range subscript inside a
trigger argument becomes a proof obligation even on steps where the trigger stays silent.

## 14. Generated code carries no lint suppressions

**Implemented** (M2).

Generated Rust is emitted warning-free rather than with a blanket `#[allow]`: unused trait
parameters are named with a leading underscore, no-op casts and `+ 0` are not emitted, and no
redundant parentheses are produced — since every node is bound to its own `let` and every operand is
a bare identifier, precedence can never matter.

`every_monitor_compiles_without_the_standard_library` in `crates/copilot-rust/tests/no_std.rs`
compiles every corpus monitor with `-D warnings` against a `no_std` maths stub, so this stays true.
Generated code lands in someone else's build; it should not be the reason their warning count goes
up, and it should not silence lints on their behalf.

## 15. `since` follows the standard semantics, not upstream's formula

**Implemented** (M3, `copilot_libs::ptltl::since`).

Upstream defines past-time `since` as:

```haskell
since s1 s2 = eventuallyPrev (s2 ==> (alwaysBeen s1))
```

with the documented meaning "is there a time when `s2` holds and after which `s1` continuously
holds?"

The formula does not mean that. An implication is true wherever its antecedent is false, so at any
step where `s2` was false, `s2 ==> _` holds; `eventuallyPrev` then finds that step and the whole
expression is true from there on, whatever `s1` did. Under this definition `since(s1, s2)` is true at
almost every step of almost every trace — including traces where `s2` never holds at all and `s1`
never holds at all.

copilot-rs uses the standard recursion instead:

```text
since(t) = s2(t) || (s1(t) && since(t - 1)),   since(-1) = false
```

which is exactly "there is some `k <= t` with `s2(k)`, and `s1(j)` for every `j` in `(k, t]`" — one
bit of state, like the other past-time operators.

This is the one place where fidelity to upstream and correctness genuinely conflict, and a
temporal operator that silently reports "yes" is the wrong way to be wrong in a runtime monitor for
safety-critical systems. `since_is_false_when_its_trigger_never_occurs` in
`crates/copilot-libs/tests/libs.rs` builds both formulas over one trace and shows them disagreeing
at every step.

## 16. `drop` past a buffer peels the stream's definition

**Implemented** (M3, in `copilot-lang`'s shift; a fix to M1).

A stream buffering `n` values defines its value at `t + n` as its transition expression at `t`.
`drop (n + k) s` is therefore `drop k` of that expression, and only a stream whose definition cannot
supply the value — an external variable, or a stream still being defined — is a real error.

M1 implemented `drop` as index arithmetic alone and rejected anything past the buffer. That made
`[false] ++ p` unshiftable back to `p`, which in turn made every bounded future-time operator in
[`copilot_libs::ltl`] and [`copilot_libs::mtl`] unusable — the whole point of buffering a stream
before reading ahead in it. The rewrite terminates because peeling replaces a shift of `by` with one
of `idx + by - n`, and `idx < n`.

A related trap, worth recording because it is invisible in the Haskell original: these recursions
are written in upstream as lazy definitions where the base case never forces the shifted streams.
Rust evaluates arguments first, so a direct transliteration builds one shift too many — an error
outright on an external variable, and for the past-time metric operators a buffered stream the
monitor would carry and never read. `copilot_libs::mtl` guards each step explicitly.

## 17. The SMT encoding uses a shifting window, not the ring buffer

**Implemented** (M4, `copilot-theorem`).

The interpreter and both code generators store a stream as a ring buffer with a rotating index. The
SMT encoding stores it as a window of `n` state variables holding the stream's values at
`t ..= t + n - 1`, and a step shifts that window along.

Both denote the same stream. The window needs no modular index arithmetic, which keeps the encoding
in a decidable fragment and stops the prover reasoning about an implementation detail — but the more
important reason is that it makes the encoding an *independent* derivation of the semantics rather
than a transcription of an existing engine. A prover that shared its meaning with the thing it
checks would agree with it by construction, including where both are wrong.

That independence is what
`the_encoding_agrees_with_the_interpreter` in `crates/copilot-theorem/tests/encoding.rs` exploits:
random specifications run through both, and any disagreement is a real bug in one of them.

## 18. Results carry caveats, and a caveated result is not a proof

**Implemented** (M4, `copilot_theorem::Caveat`).

Upstream reports a property as proved, disproved, or unknown. copilot-rs adds a fourth thing to the
answer: what was approximated to reach it.

- Floats encoded as reals — the default — have no NaN, no infinity, no overflow and no rounding, so
  a property can hold under them and fail on a real machine.
- Transcendental functions and conversions between integers and floats become uninterpreted
  functions. That is sound for *proving* (a property true of every interpretation is true of the
  real one) and unsound for *refuting*.

Rather than burying this in documentation, every `Proof` carries the caveats that applied and
`Proof::is_conclusive` is false whenever any did. A caller that ignores caveats cannot accidentally
treat an approximation as a guarantee — which is the failure mode that matters for a verification
tool.

`FloatEncoding::Ieee` selects the exact encoding, at a large cost in solving time.

## 19. Induction depth is searched, not fixed

**Implemented** (M4).

Upstream picks `k` as the maximum buffer depth in the specification and answers at that depth.

copilot-rs searches depths upwards instead, because the heuristic is wrong in both directions. A
counterexample that takes more steps to arrive than the deepest buffer is missed entirely — the base
case never unrolls far enough to see it — and a property that becomes inductive one step later is
reported as "not inductive" when it is simply true. `Settings::depth` fixes the depth when that is
what is wanted; `Settings::max_depth` bounds the search.

## 20. The interpreter's transcendentals come from `libm`, not the host

**Implemented** (M4 follow-up, `copilot-interp`).

`sqrt`, `exp`, `sin` and the rest are computed with the [`libm`](https://crates.io/crates/libm)
crate rather than the standard library.

The interpreter is the reference every other engine is compared against, and generated `no_std`
monitors call `libm` because `core` provides none of these. An interpreter calling the platform's
maths library would therefore disagree with the code it is the reference for — in the last place,
on exactly the operations hardest to reason about — and would disagree *differently* on different
machines. Routing both through one library removes the discrepancy instead of documenting it.

This closes a gap M2 could only work around. The code-generation differential tests previously
pointed generated code at a shim forwarding to `std`, which meant they checked that the right
function was called with the right arguments but not that the numbers matched. They now link the
real `libm` on both sides and compare values.

`sqrt`, `ceil` and `floor` are exactly rounded and agree between any two implementations; the
transcendentals are not, which is why the choice had to be made rather than left open.
`transcendentals_follow_libm_rather_than_the_host` in `crates/copilot-interp/tests/semantics.rs`
pins it with an argument where the two libraries genuinely differ.

## 21. Every declared external variable is sampled, read or not

**Implemented** (M2 codegen; made explicit by M4's random specifications).

A specification can declare an external variable that no reachable expression reads. Generated code
still calls its `Env` method once per step, and binds the result to a name nothing looks at.

`Env` is the user's own code, and a read may well have an effect — clearing a status register,
advancing a queue, acknowledging an interrupt. "Each method is called exactly once per step" is a
contract the monitor's environment can rely on, and skipping the unread ones to save a call would
break it silently. The binding is underscored instead, so generated code still compiles clean.

The same reasoning applies to `Local` bindings the Rust backend erases: a binding reachable only as
the bound side of an unused `Local` is emitted and never read, and is underscored rather than
suppressed with an `allow`.

## 22. The bisimulation reference is a second, independent code generator

**Implemented** (M5, `copilot-verifier`).

Upstream Copilot's verifier (`copilot-verifier`) proves the generated C monitor correct by
bisimulation against a Crucible model, discharged with an SMT solver. copilot-rs does the same
against the generated Rust monitor, discharged by Kani (CBMC).

The reference the monitor is proved equal to is deliberately a *different* code generator, in a crate
that does not depend on `copilot-rust`. It lowers each stream to an explicit time-ordered vector —
`drop i` is a plain index, commit is a vector shift — where the monitor uses a ring buffer with a
rotating index. A representation function bridges the two, and Kani proves one step of the monitor
equals one step of the reference for every state and every input at once. `docs/bisimulation.md` is
the full argument, including why trace equivalence follows from the single step and why no unwinding
bound is needed.

Two consequences worth recording:

- **Independence is enforced, not assumed.** `tests/independence.rs` fails if `copilot-rust` ever
  becomes a library dependency of `copilot-verifier`, because a reference sharing the monitor's
  lowering would prove only that the generator equals itself.
- **The reference is itself tested.** `tests/reference.rs` checks it against the interpreter over
  random specifications, so `ir_step ≈ interpreter` by testing and `monitor ≡ ir_step` by proof, and
  the two compose.

Transcendental functions are refused (`Error::Transcendental`): they lower to `libm` calls CBMC
cannot see through. Plain floating-point arithmetic is allowed but slow, so the corpus is
integer-first.

## 23. `cargo test --workspace` runs the proofs when Kani is present

**Implemented** (M5).

The Kani proofs live in `crates/copilot-verifier/tests/kani.rs` and run as ordinary tests — but each
shells out to `cargo kani`, so they need it installed. They skip cleanly when it is absent, printing
a note, so `cargo test --workspace` stays green on a machine without Kani and actually discharges the
proofs on one with it. Run them deliberately with `cargo test -p copilot-verifier --test kani`.

The negative tests are the point of the suite: a monitor whose commit writes to the wrong ring-buffer
slot, and a phase-3/4 swap where one stream reads another's committed value, are both refuted. A
proof harness that cannot fail proves nothing, so these keep it honest.

## 24. `copilot!` is sugar over the builder, and that is checkable

**Implemented** (M6, `copilot_macro::copilot`).

Upstream Copilot's surface is a Haskell monadic DSL; the specification *is* the program. copilot-rs's
primary surface is the builder, with `copilot!` as a declarative layer over it.

The macro adds no semantics of its own — it expands to exactly the builder calls a user would have
written. That is not a claim to take on trust: `Spec` derives `PartialEq`, and
`the_heater_desugars_to_the_same_spec` in `crates/copilot-lang/tests/macro_spec.rs` asserts the same
specification written both ways produces a *literally equal* `Spec` — same arena, same expression
ids, same order. `spec_equality_can_actually_fail` keeps that assertion from being vacuous by
changing one constant and requiring the comparison to fail.

Two things the macro translates, because Rust cannot express them directly:

- **Comparisons and boolean connectives.** `a < b` cannot be `PartialOrd`, since comparing two
  streams yields a *stream* of booleans rather than a `bool`. `< <= > >= == != && ||` become the
  corresponding methods.
- **Literals in operand position.** `celsius < 18.0` needs the `18.0` to be a stream too, so bare
  numeric and boolean literals are lifted where they are operands. They are left alone everywhere
  else, which is what `counter.drop(1)` needs — `drop` is the one method in the API whose argument
  is a build-time quantity rather than a stream. String literals are never lifted, so a field or
  label name passes through.

## 25. Streams can be declared before they are defined

**Implemented** (M6, `Builder::declare` and `Pending`).

`Builder::stream` passes a closure a handle on the stream being defined, which covers
self-reference. It cannot express *mutual* recursion: two streams that read each other need both
handles to exist before either body is built — something upstream gets from Haskell's laziness.

`Builder::declare` returns a `Pending` carrying a usable handle, and `Pending::define` installs the
body later. `stream` is now written in terms of it, and the `copilot!` macro declares every stream in
a block before defining any, so specifications like

```rust
stream ping: bool = [false] ++ !pong;
stream pong: bool = [true]  ++ ping;
```

work. `define` consumes the `Pending`, so a stream cannot be given two bodies, and one left declared
but never defined is reported by `Builder::finish`.

## 26. The Bluespec backend refuses floating point

**Implemented** (M7, `copilot_bluespec::Error::UnsupportedType`).

Upstream's Bluespec backend carries `Float` and `Double` through to Bluespec's `FloatingPoint`
library. copilot-rs rejects a specification that mentions either, at generation time, naming the
type.

Bluespec's floats are a soft-float *library*, not a primitive type, and the gap between that and
what this IR requires is not one of cost:

- there are no transcendental functions at all, so `sqrt`, `exp`, and the rest have nothing to lower
  to;
- `FloatingPoint` values cannot be compared or divided during elaboration — `bsc` reports
  "Unordered comparison of type `FloatingPoint`" and stops — so even `x < 4.0` fails to build.

A backend that accepted float specifications would therefore accept them only as far as the
generator, and hand the user a `bsc` error about a library they never imported. Refusing at the
boundary says which type is the problem, in the terms the specification is written in. It also keeps
this backend's agreement with the interpreter total: every specification it compiles, it compiles to
something that simulates identically, which is the property `crates/copilot-bluespec/tests/bluesim.rs`
checks.

The same reasoning as M5's refusal of transcendentals in the Kani corpus, applied one type earlier.
Integer specifications are unaffected, and a fixed-point thermostat is the corpus entry standing in
for the float-valued heater.

## 27. In hardware the compute and commit phases cannot be swapped

**Implemented by the target** (M7).

Generated Rust has to be careful about phases 3 and 4: computing each stream's next value and
committing it are separate loops precisely so that no stream can read another's *new* value.
Merging them is the classic bug, and the Kani harness exists to rule it out.

Bluespec gets it from the semantics of a rule. Every register the step rule reads holds the value it
had at the clock edge, and every write takes effect at the next one, so the entire rule body — the
subexpression wires, the trigger calls, the commits — sees one consistent snapshot no matter what
order it is written in. There is no ordering to get wrong.

That is a genuine simplification rather than a claim taken on trust: the negative test
`a_stream_reading_a_committed_value_is_caught` builds the swap by hand, by rewriting one commit to
store another stream's next value, and asserts that bluesim then disagrees with the interpreter. The
bug is expressible; the code generator simply has no way to introduce it.

What Bluespec does *not* give away is the ring buffer. `a_frozen_ring_buffer_index_is_caught` freezes
the rotating index and asserts the disagreement, which is the other half of what the Rust backend's
differential test covers.

## 28. Generated Bluespec is one rule, and it always fires

**Implemented** (M7).

The monitor is a module taking an interface and returning `Empty`, with a single rule guarded by
`when True`. A step is therefore exactly one clock cycle, whatever the data — the hardware form of
the constant-time claim, and stronger than the software one, since it is a property `bsc`'s own
scheduler reports rather than one inferred from the absence of loops.

Two consequences worth naming:

- **The module is not a synthesis boundary.** A module with an interface argument cannot be compiled
  separately (`bsc` says so in as many words), so `mkMonitor` is elaborated into whatever module
  instantiates it. That is the same shape upstream uses, and it is what lets an external variable be
  an `ActionValue` method rather than a port whose timing the monitor would have to negotiate.
- **A buffer read is a mux, not a modulo.** The invariant is `b[(p + i) % n]`, but `p` and `i` are
  both below `n`, so generated code emits one conditional subtraction instead. On a processor that
  is a micro-optimisation; on hardware it is the difference between a wire and a divider.

## 29. The Bluespec backend reaches verification layers 1 and 2, not 3

**Implemented by omission** (M7).

Layer 3 is Kani over Rust. There is no CBMC for Bluespec, so a generated monitor gets no
bisimulation proof — and "verifiable" must not spread to this backend by association with the other
one.

What it does get:

- **Layer 1 in full**, and by simulation rather than by inspection: `bsc` compiles every corpus
  monitor, bluesim runs it against a generated testbench, and the printed events must be the
  interpreter's. Two negative tests assert the failure — a frozen ring-buffer index and a stream
  reading a committed value — so the comparison is known to have teeth.
- **Layer 2 for free**, because k-induction proves things about a `Spec`. A property discharged by
  `copilot-theorem` holds of the specification, not of any lowering of it, so it transfers to every
  backend without being re-proved.

What is missing is the step in between: nothing proves that the emitted Bluespec *implements* the
`Spec` for all inputs, only that it agrees with the interpreter on the traces tested. For the Rust
backend that gap is closed by `copilot-verifier`; here it is covered by testing alone.

This is the one respect in which the second backend is weaker than the first, and it is not a
temporary state of the tooling — a bisimulation proof for Bluespec would need a model checker for
Bluespec, which is a different project.

## 30. The Bluespec footprint is stated, not verified

**Implemented** (M7).

`copilot_core::resources` reports a Rust monitor's state size, and
`every_monitor_occupies_exactly_the_reported_footprint` checks it against `size_of::<Monitor>()`.
That is what makes M2's constant-memory claim falsifiable.

There is no equivalent here, for two reasons. `repr(C)` does not apply — Bluespec's derived `Bits`
instances pack, so a `Bool` costs one bit rather than one byte, and `resources`' answer would be the
wrong number reported precisely. And there is nothing to compare against: `bsc` reports area, but not
in a form anything parses, so an automated check would mean scraping a human-readable report.

So the generated monitor reports what it can support: the bits its buffers hold and the number of
registers holding them, both computed from the specification — and it says, in the file, that this is
not the synthesised area and that nothing checks it against one. The figure is still worth printing,
because it is exact about the thing it describes and cannot drift silently; it is simply a weaker
claim than M2's, and generated code that implied otherwise would be the defect.

`generated_source_states_its_state_without_overclaiming` pins both halves — the count and the
disclaimer — so an edit cannot quietly drop the second and leave the first reading as if it had been
verified.

---

## 31. The pretty-printer is a module in `copilot-core`, and it does not parse

**Implemented** (M8).

Upstream keeps `Copilot.PrettyPrint` in its own package, `copilot-prettyprinter`, because it wants the
`pretty` library. copilot-rs has no such reason: every crate here already depends on `copilot-core`,
and `copilot-core` is deliberately dependency-free. So the printer is
`crates/copilot-core/src/print.rs`: a hand-written `Display` for `Spec`, plus `format_expr` for one
expression on its own, not a tenth crate.

The harder decision is what the text looks like, because two things pull against each other. The
arena is hash-consed, so the same subexpression can be reachable from a stream, an observer, and two
trigger arguments at once — printing it four times would hide the sharing `copilot_core::cost` charges
for once, and would make the text's size stop tracking the arena's. So a node used more than once is
named — `let t7 = ...;` — and printed once; a [`Node::Label`] is named after itself rather than a
synthetic number, since a user chose that name on purpose; a [`Node::Local`] is always named,
regardless of use count, because it exists only because a frontend asked for a binding to appear —
`crates/copilot-rust/tests/support/mod.rs::locals` builds exactly this shape by hand to prove a
generator handles a binding nobody reads, and the printer now shows it as a `let` too, unused or not.

Round-tripping through a parser was explicitly not attempted, even though the text looks close enough
to `copilot!`'s surface syntax to tempt it. That would be a second frontend to keep in sync with the
builder and the macro, and the plan already turned down a second frontend once (`copilot!` desugars to
the builder rather than parsing to a fresh IR). Nothing here reads the text back; `format_expr` and
`Display for Spec` only write.

One consequence worth naming because it looks like an omission: operand parenthesization does not
track precedence. Every inlined compound operand is wrapped in parentheses whether or not the
wrapping is necessary — `(drop 0 s0 + 1) * 2`, but also the less necessary `t7.mux(true,
(t9.mux(false, drop 0 s0)))`. A precedence table would read slightly better; it would also be another
thing to get wrong in a format nothing checks by parsing it back. Unambiguous-but-verbose was chosen
over pretty-but-fallible.

## 32. Counterexamples print against the property's own text

**Implemented** (M8).

`copilot-theorem`'s `Outcome::Invalid` carries a `Counterexample` — external inputs only, replayed
through the interpreter rather than trusted from the solver, so a counterexample is corroborated by a
second engine before it is shown to anyone. Before this milestone, the only way to read one was
`Proof`'s `Display`, which says "refuted by a trace of N step(s)" — a fact about the trace's length,
not about what was claimed or what the trace's inputs were.

`Proof::describe(&self, spec: &Spec)` prints the property's own expression via
`copilot_core::format_expr` next to each step's inputs by declared name and [`Value`]'s own `Display`.
Nothing here is a raw solver model: the inputs already went through `sexpr::decode` before reaching
`Counterexample`, and `format_expr` is the same printer §31 added for `copilot-core`, not a second
one. `Display for Proof` is unchanged — it is the one-line summary a test failure prints by default —
and `describe` is the expanded form a person reaches for when the one-liner is not enough.
