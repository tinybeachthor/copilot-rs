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

**Implemented** as an IR-level decision (M0, documented in `copilot_core`'s crate docs); enforced by
the interpreter and backends from M1 onwards.

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

**Decided**; not yet implemented (lands with the interpreter and backends in M1–M2).

`Op2::Index` takes a runtime `Word32`, so an out-of-range index is possible. Upstream's C backend
emits an unchecked subscript, which is undefined behaviour.

copilot-rs will make the policy a codegen option, `IndexPolicy::{ Wrap, Saturate, Assume }`,
defaulting to `Wrap` — `a[(i as u32 as usize) % N]`, which is constant time, total, and free of both
panics and UB. `Assume` emits a proof obligation instead, for users who would rather discharge
in-range-ness than define behaviour outside it. The interpreter and the SMT encoding follow whatever
the flag says, so all three layers continue to agree.

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
