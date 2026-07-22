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

**Decided** (M0 computes the footprint this way); **binds** the M2 Rust backend.

`copilot_core::resources` reports a monitor's exact state size, computed under `repr(C)` with fields
in stream order — buffer, then index, index omitted for single-element buffers.

Generated monitors must declare their state exactly that way. `repr(Rust)` is free to reorder
fields, which would make the reported footprint unfalsifiable; with `repr(C)` it can be asserted
against `size_of`, and M0's test suite already does so against a hand-written struct.

## 6. Array index policy is explicit

**Implemented** (M1, `copilot_core::IndexPolicy`, honoured by the interpreter; backends follow in
M2).

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
