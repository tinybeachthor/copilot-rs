# The `copilot!` macro

`copilot!` writes a specification as declarations rather than as a sequence of
builder calls. It is *sugar*: it expands to exactly the calls you would have
written by hand, and adds no meaning of its own.

That is a stronger claim than it sounds, and it is checked rather than asserted.
`Spec` implements `PartialEq`, and
[`the_heater_desugars_to_the_same_spec`](../crates/copilot-lang/tests/macro_spec.rs)
builds the same monitor both ways and requires the results to be *equal* — same
arena, same expression ids, same order — with a companion test that changes one
constant and requires the comparison to fail, so the assertion cannot be
vacuous.

Everything the macro can express, the builder can express. If the macro is ever
in the way, drop to [`Builder`](../crates/copilot-lang/src/builder.rs); nothing
is lost but brevity.

## A whole specification

```rust
use copilot_lang::copilot;

let spec = copilot! {
    extern temperature: f32;

    let celsius = temperature * 0.5 - 30.0;

    stream heating: bool = [false] ++
        (celsius < 18.0).mux(true, (celsius > 21.0).mux(false, heating));

    observe celsius;
    observe heating;

    trigger heat_on(celsius)  when celsius < 18.0 && !heating;
    trigger heat_off(celsius) when celsius > 21.0 && heating;

    property never_both = !(celsius < 18.0 && celsius > 21.0);
}?;
```

The block evaluates to `Result<Spec, Error>` — it is an expression, so it can be
returned, bound, or `?`-ed like any other.

## The items

| Form | Meaning |
|---|---|
| `extern name: Ty;` | a value the environment supplies, sampled once per step |
| `stream name: Ty = [a, b] ++ body;` | a buffered stream: the initial values, then `body` |
| `let name = expr;` | a named expression; no buffer, no state |
| `observe name;` | sample the stream of the same name every step |
| `observe name = expr;` | sample `expr` every step, under `name` |
| `trigger name(args..) when guard;` | call a handler on every step `guard` holds |
| `property name = expr;` | a claim for the prover: holds at every step |
| `property exists name = expr;` | a claim that holds at some step |

Names are identifiers, not strings — `observe celsius;` rather than
`b.observe("celsius", celsius)` — and the identifier becomes the name in
generated code.

### `stream` and `++`

`[a, b] ++ body` is upstream Copilot's notation, and means the same thing: the
stream's first values are `a, b`, and every value after them is `body`. The
buffer length is what a monitor's memory costs and what `drop` can reach:

```rust
stream counter: u64 = [0]    ++ counter + 1;      // 0, 1, 2, 3, ..
stream fib:     u64 = [1, 1] ++ fib.drop(1) + fib; // 1, 1, 2, 3, 5, ..
```

`fib.drop(1)` is the stream one step ahead — readable only because `fib`
buffered two values. See `docs/semantics.md` for what `drop` means.

### `trigger`

```rust
trigger heat_on(celsius, counter) when celsius < 18.0;
```

The parenthesised list is the arguments handed to the handler, and the
expression after `when` is the guard. Both may be arbitrary expressions; a
trigger with no arguments is written `trigger alarm() when ..;`.

### `property`

Properties are claims for [`copilot-theorem`](../crates/copilot-theorem) to
discharge ahead of time. They cost the running monitor nothing — they are not
evaluated at runtime at all.

## Expressions

Bodies are ordinary Rust expressions over the names in scope. Arithmetic,
bitwise operators, and method calls work as written, because
[`Stream<T>`](../crates/copilot-lang/src/stream.rs) implements the operator
traits. The macro translates exactly two things.

### Comparisons and boolean connectives

`a < b` cannot be `PartialOrd`: comparing two streams yields a *stream* of
booleans, not a `bool`. So these become method calls:

| Written | Expands to |
|---|---|
| `a < b` `a <= b` `a > b` `a >= b` | `a.lt(b)` `a.le(b)` `a.gt(b)` `a.ge(b)` |
| `a == b` `a != b` | `a.eq_(b)` `a.ne_(b)` |
| `a && b` `a \|\| b` | `a.and(b)` `a.or(b)` |

`&`, `|` and `^` are left alone — they already mean the right thing on streams,
including on booleans.

### Literals in operand position

`celsius < 18.0` needs the `18.0` to be a stream too, so a bare numeric or
boolean literal used as an *operand* is lifted with `Builder::lit`. Literals
anywhere else are left alone, which is what makes this work:

```rust
observe ahead = fib.drop(1);   // `1` stays a plain number
observe large = fib > 100;     // `100` becomes a stream
```

`drop` is the only method in the API whose argument is a build-time quantity —
how far to shift — rather than a value that varies over time, so it is the only
one whose arguments are left unlifted. Everywhere else (`mux`, `index`, the
shifts, the comparisons) a literal argument is lifted, so `p.mux(true, false)`
reads as it should.

String literals are never lifted, so a field or label name passes through
untouched.

### What is not translated

Everything else. Method calls, paths, casts, field access and free calls are
descended into — so a comparison hidden inside `(a < b).mux(x, y)` is still
translated — but otherwise emitted as written. The macro has no expression
language of its own; it is Rust, with the two adjustments above.

## Scoping

The expansion emits, in this order:

1. `Builder::new()`
2. every `extern`
3. every `stream` **declaration** — the handle, not yet the body
4. every `let`, in source order
5. every `stream` **body**
6. every `observe`, `trigger` and `property`, in source order
7. `Builder::finish()`

Two consequences follow, and neither is obvious from reading a block top to
bottom:

- **A stream body may use a `let` that appears further down**, because all
  bindings are emitted before any body.
- **A `let` may use a stream that appears further down**, because all streams
  are declared before any binding.
- **A `let` may *not* use a later `let`**: bindings keep their source order.
  That is an ordinary `cannot find value` error, which is the diagnostic you
  want.

These are pinned by the `scoping` tests in
[`macro_spec.rs`](../crates/copilot-lang/tests/macro_spec.rs).

### Mutual recursion

Because every stream is declared before any body is built, streams may read each
other and not only themselves:

```rust
stream ping: bool = [false] ++ !pong;
stream pong: bool = [true]  ++ ping;
```

Upstream Copilot gets this from Haskell's laziness. Here it comes from
[`Builder::declare`](../crates/copilot-lang/src/builder.rs), which hands back a
usable handle and a `Pending` whose `define` installs the body later. That API
is public, so the same is expressible without the macro:

```rust
let ping = b.declare(&[false]);
let pong = b.declare(&[true]);
let (p, q) = (ping.stream(), pong.stream());
ping.define(!q);
pong.define(p);
```

`define` consumes the `Pending`, so a stream cannot be given two bodies, and one
left declared but never defined is reported by `finish`.

## Errors

The macro reports two kinds of problem in two different ways, and the difference
matters when reading a message.

**Syntax errors** are the macro's own, and point at the offending token:

```text
error: expected one of: `extern`, `let`, `stream`, `observe`, `trigger`, `property`
```

**Everything else is an ordinary Rust error** at the expansion. A type mismatch
between a stream and its body, an unknown name, a comparison between differently
typed streams — all of these surface as the compiler's own diagnostics, because
the expansion is ordinary code. That is a deliberate consequence of the macro
being thin: it means the type checker is doing the work, not a hand-rolled
checker with worse messages.

**Specification errors** — the ones the type system cannot catch — come back in
the `Result`, exactly as they do from `Builder::finish`:

```rust
let result = copilot! {
    extern raw: u32;
    observe ahead = raw.drop(1);   // an external variable has no future
};
assert!(matches!(result, Err(copilot_lang::Error::DropOnExtern(_))));
```

## The crate path

Generated code has to name the crate it calls into, and a procedural macro
cannot see how it was reached. The default is `::copilot_lang`.

A crate that depends on the `copilot` facade instead has no `copilot_lang`
extern name, and says so:

```rust
copilot! {
    #![crate(::copilot)]

    extern temperature: f32;
    // ..
}
```

## How it works

The implementation is [`crates/copilot-macro/src/spec.rs`](../crates/copilot-macro/src/spec.rs),
and it is deliberately small — roughly a parser, a rewriter, and an emitter.

**Parsing.** Each item is a `syn` parse: custom keywords for `stream`,
`observe`, `trigger`, `property`, `when` and `exists`, and Rust's own `extern`
and `let`. Bodies are parsed as `syn::Expr`, so the macro inherits Rust's
expression grammar and precedence rather than inventing its own. `++` is two
`+` tokens to Rust's lexer, parsed explicitly after the bracketed initial
values.

**Rewriting.** A recursive walk over the `Expr` tree, which touches only the
binary operators listed above and lifts literals in operand position. It
descends into method calls, free calls, field access, casts and parentheses so
that a comparison nested inside one is still found; everything else is emitted
unchanged.

**Emitting.** The item list becomes the five groups above, in that order. There
is no cleverness here — the output is the builder program, which is precisely
what makes it testable against a hand-written one.

## When not to use it

The macro is a convenience, and there are jobs it is the wrong tool for:

- **Generating specifications programmatically.** Building a monitor from a
  configuration file, or over a list of sensors known at runtime, wants the
  builder — `copilot!` is fixed at compile time by construction.
- **The combinator libraries.** [`copilot-libs`](../crates/copilot-libs) is
  plain functions over `Stream`, so they compose naturally in builder code.
  They can be called from inside a `copilot!` block, but a specification built
  mostly from `ptltl::since` and `voting::majority` reads better without the
  declaration syntax around it.
- **Anything needing an expression the rewriter would mangle.** Nothing in the
  current rules is known to mangle correct code, but the builder is always
  available and always exact.
