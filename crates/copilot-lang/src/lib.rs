//! Builder frontend for copilot-rs.
//!
//! Specifications are written against a [`Builder`], which hands out
//! [`Stream<T>`] handles into an expression arena. The handles are `Copy` and
//! phantom-typed, so ordinary Rust operators work on them and the compiler
//! checks the spec's types:
//!
//! ```
//! use copilot_lang::{args, Builder};
//!
//! let b = Builder::new();
//!
//! // A raw sensor reading, converted to degrees Celsius.
//! let raw = b.extern_::<f32>("temperature");
//! let celsius = raw * 0.5 - 30.0;
//!
//! // Fire on either side of the comfortable range.
//! b.trigger("heat_on", celsius.lt_val(18.0), args![celsius]);
//! b.trigger("heat_off", celsius.gt_val(21.0), args![celsius]);
//!
//! let spec = b.finish()?;
//! # Ok::<(), copilot_lang::Error>(())
//! ```
//!
//! # Sharing
//!
//! Using a handle twice denotes one expression, not two. `celsius` above is
//! built once and read by both triggers; the arena interns it once and both
//! read the same node. This is what upstream Copilot needs `data-reify` and
//! `StableName` identity for, and here it falls out of handles being values.
//!
//! # Recursion
//!
//! A stream is `initial ++ next`, where `next` may refer to the stream itself.
//! [`Builder::stream`] reserves the stream's identity, passes a handle on it to
//! a closure, and installs whatever the closure returns:
//!
//! ```
//! # use copilot_lang::Builder;
//! let b = Builder::new();
//! let counter = b.stream([0u64], |s| s + 1u64);
//! let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
//! # b.finish().unwrap();
//! ```
//!
//! `s.drop(n)` reads `n` steps ahead, which a stream can answer as far as its
//! buffer reaches — `fib` buffers two values, so `drop(1)` is available and
//! `drop(2)` is not.
//!
//! # Structs
//!
//! `#[derive(CopilotStruct)]` makes a Rust struct usable as a stream type. It
//! generates the [`Typed`] implementation and a `<Name>Fields` trait giving
//! type-checked access to the fields of a stream:
//!
//! ```
//! use copilot_lang::{Builder, CopilotStruct, args};
//!
//! #[derive(Clone, Copy, CopilotStruct)]
//! #[repr(C)]
//! struct Reading {
//!     altitude: f32,
//!     valid: bool,
//! }
//!
//! let b = Builder::new();
//! let sensor = b.extern_::<Reading>("sensor");
//!
//! let climbing = sensor.altitude().gt_val(1000.0);   // Stream<bool>
//! let cleared = sensor.set_altitude(b.lit(0.0));     // Stream<Reading>
//!
//! b.observe("cleared", cleared);
//! b.trigger("high", climbing, args![sensor.altitude()]);
//! # b.finish().unwrap();
//! ```
//!
//! The accessors come through a trait rather than as inherent methods because
//! [`Stream`] belongs to this crate, and only a type's own crate may add
//! inherent methods to it.
//!
//! # Errors
//!
//! Almost nothing here can fail. The marker traits in [`classes`] admit an
//! operator only at the types it is defined for, so a spec that compiles is
//! well-typed. What remains — looking ahead of an external variable, or past
//! the end of a buffer — is recorded and reported by [`Builder::finish`],
//! because `a + b` has nowhere to return a `Result`.

pub mod classes;

mod builder;
mod error;
mod ops;
mod stream;

pub use builder::Builder;
pub use copilot_macro::CopilotStruct;
pub use error::{Error, Result};
pub use stream::Stream;

// Re-exported so a spec only needs to depend on this crate.
pub use copilot_core::{INDEX_BYTES, IndexPolicy, Prop, Spec, Type, Typed, Value, cost, resources};
