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

**Implemented** (M1 in `copilot_core::IndexPolicy` and the interpreter; M2 in the Rust backend).

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
