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
