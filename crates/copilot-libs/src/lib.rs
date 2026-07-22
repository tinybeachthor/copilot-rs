//! Reusable specification patterns for copilot-rs.
//!
//! A port of upstream Copilot's `copilot-libraries`: temporal logic, clocks,
//! majority voting, and state machines, written as ordinary Rust functions over
//! [`Stream`](copilot_lang::Stream) handles.
//!
//! ```
//! use copilot_lang::{Builder, args};
//! use copilot_libs::ptltl;
//!
//! let b = Builder::new();
//! let armed = b.extern_::<bool>("armed");
//! let launched = b.extern_::<bool>("launched");
//!
//! // Launching is only allowed once the system has been armed continuously.
//! let legal = ptltl::always_been(armed);
//! b.trigger("illegal_launch", launched & !legal, args![]);
//! # b.finish().unwrap();
//! ```
//!
//! # Everything here is still constant-memory
//!
//! These are combinators, not a runtime: each one expands into buffered streams
//! and expressions while the specification is built, so a monitor using them
//! costs exactly what [`copilot_lang::resources`] and [`copilot_lang::cost`]
//! report for the specification it produced. Nothing here allocates, loops, or
//! grows with the length of the trace.
//!
//! What differs is *how much* they expand. The past-time operators in
//! [`ptltl`] each add one bit of state and a couple of operations. The bounded
//! future operators in [`ltl`] and the metric operators in [`mtl`] unroll their
//! window, so their cost is proportional to it — check
//! [`copilot_lang::cost`] on the finished specification rather than assuming.
//!
//! # Past and future
//!
//! A monitor sees the past. The future-time operators reach forward by reading
//! ahead in a buffer, which only works for a stream that was buffered that
//! deeply, and never for one read straight from an external variable. See
//! [`ltl`] for what that means in practice.

pub mod clocks;
pub mod ltl;
pub mod mtl;
pub mod ptltl;
pub mod state_machine;
pub mod utils;

pub mod voting;

pub use utils::ClockType;
