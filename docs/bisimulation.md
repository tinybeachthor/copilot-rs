# The bisimulation proof

This is the argument behind `copilot-verifier` (M5): why a passing `cargo kani`
run means a generated monitor computes exactly what its specification says, for
every reachable state and every input — not merely for the traces a test tried.

It is written to be read alongside the harness the crate generates; the harness
is the formal object, this is the reasoning that makes one step of it enough.

## What the two sides are

A specification denotes, for each stream `s` buffering `n` values, an infinite
sequence `s(0), s(1), …`. Two programs compute it.

**The monitor** (`copilot-rust`) keeps a ring buffer `b` of length `n` and a
rotating index `p`, with the invariant

> `b[(p + i) mod n]` holds `s(t + i)`, for `0 ≤ i < n`.

It reads `drop i s` as `b[(p + i) mod n]`, and commits a step by overwriting the
slot holding the now-expired `s(t)` and advancing the index:

```text
b[p] := next;   p := (p + 1) mod n
```

**The reference** (`copilot-verifier`, `ir_step`) keeps an explicit vector `v`
of length `n` in time order, with the invariant

> `v[i]` holds `s(t + i)`.

It reads `drop i s` as `v[i]` — no index, no modular arithmetic — and commits by
shifting:

```text
v := [v[1], …, v[n-1], next]
```

These are different data structures with different code. That is deliberate: a
reference sharing the monitor's lowering would prove only that the generator
agrees with itself. `copilot-verifier` does not depend on `copilot-rust`, and
`tests/independence.rs` enforces it.

## The representation function

Let `R` map a monitor state to a reference state by reading the ring buffer in
time order:

```text
R(b, p).v[i] = b[(p + i) mod n]      for 0 ≤ i < n
```

This is `abstract_state` in the generated harness. It is total on states
satisfying the representation invariant `p < n`, which the harness assumes with
`kani::assume`.

## The one-step theorem

The harness proves, for an arbitrary state satisfying the invariant and
arbitrary external inputs `e`:

```text
R(step(m, e))  =  ir_step(R(m), e)          (states agree)
outputs(step, m, e)  =  outputs(ir_step, R(m), e)   (observers and triggers agree)
```

where `outputs` is the observer values and the fired triggers with their
arguments.

The states-agree half is what makes the ring buffer trustworthy. Working it out
for `n = 2`, index `p`, computed next value `x`:

- **Monitor.** Writes `x` to slot `p`, advances to `p' = (p+1) mod 2`. Reading
  back in time order: `R(step).v = [b'[p'], b'[(p'+1) mod 2]]`. Slot `p` now
  holds `x`; the other slot is unchanged. So this is `[old b[(p+1) mod 2], x]`.
- **Reference.** `ir_step(R(m)).v = [R(m).v[1], x] = [b[(p+1) mod 2], x]`.

Equal. The drops agree by construction of `R` — `R(m).v[i] = b[(p+i) mod n]` is
exactly what the monitor reads for `drop i` — so both sides compute the same `x`
from the same inputs, and the outputs agree for the same reason. CBMC checks
this for **all** `b`, `p < n`, and `e` at once; the sketch above is only to show
a human why it holds.

## From one step to all traces

One-step bisimulation lifts to whole traces by induction on the step count.

- **Base.** At startup the monitor's buffer is the stream's initial values with
  `p = 0`, so `R(m₀)` is those values in order — the reference's initial state.
  The invariant `p < n` holds.
- **Step.** Assume `R(mₜ)` is the reference state at time `t` and the invariant
  holds. The theorem gives `R(step(mₜ)) = ir_step(R(mₜ))` — the states still
  correspond at `t+1` — and the outputs at `t` agree. The monitor's commit sets
  `p' = (p+1) mod n < n`, so the invariant is preserved.

By induction the monitor and the reference produce identical output on every
trace. This induction is the paragraph above, not a machine obligation: Kani
proves the single step, and the lifting is ordinary mathematics.

## Why no unwinding bound

CBMC is a bounded model checker: normally it explores loops up to a fixed depth,
and a proof holds only within that bound. Here `step` has no loops. Buffer
lengths are `const`, stream updates are straight-line, and expressions are
trees. CBMC unrolls the fixed structure exactly, so the proof is unbounded in
the only dimension that matters — it quantifies over all states and inputs, with
nothing left unexplored. The state space is finite per step (fixed-width
integers, fixed-size buffers), which is why a solver can discharge it.

## What the reference rests on

The proof establishes `monitor ≡ ir_step`. If `ir_step` were wrong, this would
faithfully prove the monitor equal to something wrong. Two things guard against
it:

1. **Independence.** `ir_step` is a second code generator that cannot call the
   first. A bug would have to be reproduced independently in both to escape.
2. **Differential testing.** `ir_step` is checked against the interpreter — a
   third implementation, sharing nothing with either generator — over random
   specifications in `tests/reference.rs`. So `ir_step ≈ interpreter` by
   testing, `monitor ≡ ir_step` by proof, and the two compose to
   `monitor ≈ interpreter` across the whole state space.

The residual risk is a bug present identically in the monitor lowering, the
reference lowering, *and* the interpreter. The monitor and reference lowerings
are written separately; the interpreter is a different kind of program again;
and the SMT encoding of M4 is a fourth. Each is fallible, but they do not fail
the same way by accident.

## Limits

- **Transcendentals.** `sqrt`, `sin`, and the rest lower to `libm` calls, which
  CBMC cannot see through. A specification using any is refused
  (`Error::Transcendental`) rather than verified against a stubbed function.
- **Floating point.** Plain float arithmetic is supported, but CBMC's bit-level
  float reasoning is slow; the corpus is integer-first for that reason.
- **Scale.** Very large specifications may exceed what CBMC discharges quickly.
  The proof is per-stream-group composable in principle; the current harness
  proves the whole step at once, which is ample for the specifications a
  constant-memory monitor is meant to be.
