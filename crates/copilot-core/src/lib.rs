//! Core IR for copilot-rs.
//!
//! A Copilot specification is a set of mutually recursive infinite streams,
//! together with the triggers, observers, and properties that monitor them.
//! This crate defines that IR, checks it, and measures it. It knows nothing
//! about how a monitor is executed — the interpreter, the code generators, and
//! the verifier all consume what is defined here, and none of them re-derives
//! its meaning.
//!
//! # Streams
//!
//! A stream is written `buffer ++ expr`: a non-empty list of initial values
//! followed by an expression giving every value after them. The classic
//! examples:
//!
//! ```text
//! counter = [0]   ++ (counter + 1)
//! fib     = [1,1] ++ (drop 1 fib + fib)
//! ```
//!
//! `drop i s` is `s` shifted forward by `i`, so at time `t` it denotes
//! `s(t + i)`. A stream buffering `n` values can be dropped by at most `n - 1`,
//! and its transition expression denotes the value at `t + n`. Everything a
//! monitor can remember is therefore fixed by the spec's text.
//!
//! # Buffer representation
//!
//! Every execution engine — interpreter, generated Rust, generated Bluespec —
//! stores a stream as a ring buffer `b` of length `n` with a rotating index
//! `p`, under one invariant:
//!
//! > `b[(p + i) % n]` holds the stream's value at time `t + i`, for `i < n`.
//!
//! So `drop i` reads `b[(p + i) % n]`, and committing a step writes the new
//! value over the slot holding time `t`, which has just expired:
//!
//! ```text
//! b[p] = next;  p = (p + 1) % n
//! ```
//!
//! This is stated here, once, because the bisimulation proof in
//! `copilot-verifier` is exactly the claim that generated code implements it.
//!
//! # Step order
//!
//! One step of a monitor is four phases, in this order:
//!
//! 1. **Sample** each external variable exactly once.
//! 2. **Fire** triggers whose guards hold, with arguments read from the current
//!    buffers.
//! 3. **Compute** each stream's next value from the current buffers, into
//!    temporaries.
//! 4. **Commit** the temporaries into the buffers.
//!
//! Phases 3 and 4 are separate because every stream reads the state as it was
//! at the start of the step. Merging them — committing each stream as it is
//! computed — would let a stream observe another stream's *next* value, which
//! is a different spec. Because of this separation, no stream's transition
//! expression depends on another's within a step, so phase 3 has no internal
//! ordering constraint at all.
//!
//! # Arithmetic
//!
//! Integer arithmetic wraps. Upstream Copilot's C backend inherits C's
//! undefined behaviour on signed overflow; this IR defines the behaviour
//! instead, so the interpreter, generated code, and the SMT encoding agree on
//! one total semantics, and a monitor cannot panic in debug and silently wrap
//! in release. Generated Rust uses the `wrapping_*` operations; the SMT
//! encoding uses bitvectors.
//!
//! Division and remainder by zero are zero, and array subscript out of range
//! follows a configurable [`IndexPolicy`]. These are the only two partial
//! operations in the language, and both are made total for the same reason:
//! a monitor that must not trap has no way to signal failure.
//!
//! # Building a spec
//!
//! ```
//! use copilot_core::{Arena, Op2, Spec, Type, Typed, Value};
//!
//! let mut arena = Arena::new();
//!
//! // Declare the stream before defining it: `counter` refers to itself.
//! let counter = arena.declare_stream(Type::Word64, 1)?;
//! let current = arena.drop_(0, counter)?;
//! let one = arena.constant(Type::Word64, 1u64.lift())?;
//! let next = arena.op2(Op2::Add(Type::Word64), current, one)?;
//!
//! let mut spec = Spec::new(arena);
//! spec.define_stream(counter, vec![Value::Word64(0)], next)?;
//! spec.validate()?;
//!
//! assert_eq!(copilot_core::resources(&spec).state_bytes, 8);
//! # Ok::<(), copilot_core::Error>(())
//! ```

mod analysis;
mod check;
mod error;
mod expr;
mod op;
mod policy;
mod spec;
mod ty;

pub use analysis::{Footprint, INDEX_BYTES, OpCounts, StreamFootprint, cost, reachable, resources};
pub use check::{typecheck, validate, wellformed};
pub use error::{Error, Result};
pub use expr::{Arena, ExprId, Node, StreamDecl, StreamId, VarId};
pub use op::{Op1, Op2, Op3, OpClass};
pub use policy::{IndexPolicy, div, rem};
pub use spec::{Arg, Observer, Prop, Property, Spec, Stream, Trigger};
pub use ty::{Layout, Type, Typed, Value};
