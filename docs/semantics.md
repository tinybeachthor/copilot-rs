# Semantics of the copilot-rs IR

Two descriptions of the same language, and the correspondence between them.

The **denotational** semantics says what a specification *means*: streams are
infinite sequences, and every construct is a function on them. It is the
definition — when an implementation disagrees with it, the implementation is
wrong.

The **operational** semantics says how a monitor *runs*: fixed-size ring buffers
advanced one step at a time. It is what the interpreter does and what the code
generators emit.

The gap between them is the whole engineering problem. The denotational reading
talks about infinite sequences; the operational one runs in bytes fixed at
compile time. The final section states the correspondence that makes the second
a faithful implementation of the first — which is what `copilot-verifier` will
discharge mechanically in M5.

---

## 1. Denotational semantics

Time is `t ∈ ℕ`, counting steps of the monitor. A stream of type `τ` denotes a
total function `ℕ → ⟦τ⟧`.

### Streams

A stream is declared as `initial ++ next`, where `initial` is a non-empty list
of `n` values and `next` is an expression. It denotes:

```
⟦s⟧(t) = initial[t]           for t < n
⟦s⟧(t) = ⟦next⟧(t - n)        for t ≥ n
```

Read the second line as: `next` evaluated at time `t - n` produces the stream's
value `n` steps later. A stream is well founded because `n ≥ 1` — every value is
defined in terms of strictly earlier ones.

### Expressions

```
⟦Const v⟧(t)        = v
⟦Drop i s⟧(t)       = ⟦s⟧(t + i)
⟦ExternVar x⟧(t)    = the environment's sample of x at step t
⟦Op₁ f a⟧(t)        = f(⟦a⟧(t))
⟦Op₂ f a b⟧(t)      = f(⟦a⟧(t), ⟦b⟧(t))
⟦Op₃ f a b c⟧(t)    = f(⟦a⟧(t), ⟦b⟧(t), ⟦c⟧(t))
⟦Local v e b⟧(t)    = ⟦b⟧(t) with v bound to ⟦e⟧(t)
⟦Label _ a⟧(t)      = ⟦a⟧(t)
```

Every operator is applied pointwise at a single time. `Drop` is the sole
exception, and therefore the only construct that moves between times at all.

### Why `Drop` is bounded

`Drop i s` reads `s` at time `t + i` — the *future*. That is well defined
denotationally, but a monitor cannot see the future, which is why the IR
requires `i < n` for a stream buffering `n` values. Under the stream equation
above, `⟦s⟧(t + i)` for `i < n` depends only on values determined at or before
`t`. The restriction is exactly what makes the denotational reading
implementable.

It also explains why `Drop` on an external variable is rejected at any depth:
an extern's samples are supplied one per step, and `⟦x⟧(t + 1)` does not exist
when the monitor is at `t`.

### Specifications

- An **observer** `(name, e)` denotes the sequence `⟦e⟧(0), ⟦e⟧(1), …`.
- A **trigger** `(name, g, args)` fires at every `t` where `⟦g⟧(t)` is true,
  with arguments `⟦argᵢ⟧(t)`. Arguments are only meaningful when it fires.
- A **property** `Forall e` claims `∀t. ⟦e⟧(t)`; `Exists e` claims `∃t. ⟦e⟧(t)`.
  Properties are claims *about* a spec, not part of its behaviour, and cost a
  running monitor nothing.

### Total operations

Two operations would otherwise be partial. Both are given total definitions,
because a monitor that must not trap has no way to signal failure, and because
the interpreter, both code generators, and the SMT encoding must agree on one
answer.

| Operation | Definition |
|---|---|
| Integer `+ - *` | Wrapping. `i8::MAX + 1 = i8::MIN` |
| `abs` | Wrapping. `i8::MIN.abs() = i8::MIN` |
| Integer `/` and `%` by zero | Zero |
| Integer `/` at `MIN / -1` | Wrapping, so `MIN` |
| Shift by ≥ the operand width | Zero |
| Array subscript out of range | Per `IndexPolicy`: wrap (default), saturate, or a proof obligation |
| Float comparison against NaN | False in every direction, including `<=` and `>=` |
| Float arithmetic | IEEE 754, evaluated at the operands' own width |

The last line matters more than it looks. Evaluating an `f32` operation in `f64`
and rounding afterwards is *not* the same computation: it is provably
indistinguishable for `+ - * /`, where the intermediate's 53 bits clear the
`2p + 2 = 50` that single precision needs, but it fails for the transcendentals
— routing `f32::exp` through `f64` changes the result for roughly one argument
in two thousand. Every engine evaluates at the operands' width so that no
implementation has to know which case an operator falls into.

---

## 2. Operational semantics

A monitor's state is, for each stream `s` buffering `n` values:

- a buffer `b_s` of exactly `n` values, and
- a position `p_s ∈ [0, n)`, omitted when `n = 1` because it is always zero.

Nothing else. This is what `copilot_core::resources` reports, and there is no
allocation, no growth, and no dependence on how long the monitor has run.

### The buffer invariant

At every step, with the monitor at time `t`:

> **(INV)** `b_s[(p_s + i) mod n] = ⟦s⟧(t + i)` for all `i < n`.

The buffer holds a sliding window of the stream over `[t, t + n)`, rotated by
`p_s`. Initially `p_s = 0` and `b_s = initial`, so (INV) holds at `t = 0`
directly from the stream equation.

Reading follows from (INV) by definition:

```
Drop i s   ↦   b_s[(p_s + i) mod n]
```

### One step

A step at time `t` runs four phases, in this order:

1. **Sample.** Read each external variable exactly once into `x_1 … x_k`. Once,
   so that two reads of one name within a step cannot disagree.
2. **Fire.** Evaluate observers, then each trigger's guard, and where it holds,
   its arguments — all against `(b, p, x)`, which by (INV) is the state at `t`.
3. **Compute.** For each stream `s`, evaluate its `next` expression against
   `(b, p, x)` into a temporary `v_s`. Nothing is written back.
4. **Commit.** For each stream: `b_s[p_s] ← v_s`, then `p_s ← (p_s + 1) mod n`.

### Why phases 3 and 4 are separate

Every stream must read the state as it was at the *start* of the step. If a
stream were committed as soon as it was computed, a later stream reading it
would see its new value — a different specification, and one nothing in the
denotational semantics describes.

Concretely, for `a = [0] ++ (a + 1)` and `b = [0] ++ a`:

| | correct (separated) | merged phases |
|---|---|---|
| `a` | 0, 1, 2, 3, 4 | 0, 1, 2, 3, 4 |
| `b` | 0, **0**, 1, 2, 3 | 0, **1**, 2, 3, 4 |

`b` must lag `a` by one step. `crates/copilot-interp/tests/semantics.rs` pins
this as `streams_read_the_state_from_the_start_of_the_step`, and M5's proof
obligation rules it out mechanically.

A useful consequence: because phase 3 reads only committed state, no stream's
transition expression depends on another's within a step. Phase 3 has no
internal ordering constraint, and a backend may evaluate streams in any order or
in parallel. This is why the IR needs no dependency sort over streams.

### The commit is well founded

Phase 4 overwrites `b_s[p_s]`, which by (INV) held `⟦s⟧(t)` — the value for the
time the monitor is leaving. Nothing still needed is destroyed: after the step
the monitor is at `t + 1`, and the window it needs is `[t + 1, t + 1 + n)`.

---

## 3. Correspondence

**Claim.** For every validated specification, the operational semantics
implements the denotational one: for every stream `s`, time `t`, and `i < n`,
running `t` steps from the initial state leaves `b_s[(p_s + i) mod n] = ⟦s⟧(t + i)`
— and the observers and triggers reported at step `t` are exactly those the
denotational semantics assigns to `t`.

**Proof sketch.** Induction on `t`, with (INV) as the invariant.

*Base.* At `t = 0`, `p_s = 0` and `b_s = initial`, so `b_s[i] = initial[i] = ⟦s⟧(i)`
for `i < n` by the stream equation.

*Step.* Assume (INV) at `t`. Phase 2 evaluates against a state that, by the
inductive hypothesis, agrees with the denotational semantics at `t`; since every
operator is pointwise and `Drop i` reads exactly `⟦s⟧(t + i)`, observers and
triggers agree at `t`. Phase 3 computes `v_s = ⟦next_s⟧(t) = ⟦s⟧(t + n)` by the
same argument plus the stream equation. Phase 4 writes `v_s` into the slot that
held `⟦s⟧(t)` and advances `p_s`; the remaining slots are untouched, so
re-indexing from the new position gives `b_s[(p_s' + i) mod n] = ⟦s⟧(t + 1 + i)`
for `i < n`. That is (INV) at `t + 1`. ∎

The argument is short, and deliberately so: it turns a statement about all
infinite executions into a statement about **one step**. That is the shape M5
exploits. A generated `step()` is loop-free — buffer lengths are compile-time
constants and stream updates are straight-line — so a model checker can
discharge the one-step obligation for *all* states and *all* inputs, with no
unwinding bound and no bounded-verification caveat. The induction over steps
stays here, on paper, where it costs nothing.

`docs/bisimulation.md` (M5) states the mechanised form.

---

## 4. Deliberately unspecified

- **Trigger side effects.** A trigger calls out to code the spec does not
  describe. Handlers are assumed not to modify anything the monitor reads;
  nothing enforces it.
- **Wall-clock time.** A step is one call to the monitor. Relating steps to
  seconds is the caller's business — a clock is an external variable like any
  other.
- **Float determinism across targets.** IEEE 754 pins `+ - * / sqrt`, but the
  transcendentals are the platform's libm and may differ in the last bit between
  a host and a target. This is a real limitation of comparing a monitor against
  an interpreter running elsewhere, and where it matters, the SMT encoding is
  the arbiter rather than either implementation.
