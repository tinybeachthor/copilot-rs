//! copilot-rs: a stream language for hard-realtime runtime monitors.
//!
//! A Rust implementation of [Copilot](https://copilot-language.github.io/),
//! keeping its three design objectives:
//!
//! - **Realtime.** A monitor's per-step work is fixed by its specification.
//!   [`cost`] reports it, broken down by how expensive each operation actually
//!   is on an embedded target.
//! - **Constant memory.** A monitor's state is fixed by its specification.
//!   [`resources`] reports it exactly, in bytes.
//! - **Verifiable.** Specifications are small enough to reason about
//!   mechanically, and the generated monitor is checked against the
//!   specification rather than trusted.
//!
//! This crate is the front door: it re-exports the language, the interpreter,
//! and the analyses.
//!
//! ```
//! use copilot::{args, Builder, Monitor, Samples, Value};
//!
//! let b = Builder::new();
//!
//! let raw = b.extern_::<f32>("temperature");
//! let celsius = raw * 0.5 - 30.0;
//! b.trigger("heat_on", celsius.lt_val(18.0), args![celsius]);
//!
//! let spec = b.finish()?;
//! let mut monitor = Monitor::new(&spec)?;
//!
//! let mut env = Samples::none().with("temperature", Value::Float(90.0));
//! assert!(monitor.step(&mut env)?.did_fire("heat_on")); // 90 * 0.5 - 30 = 15 °C
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! See `examples/heater.rs` for a spec driven over a full trace.

pub use copilot_lang::{Builder, Error, Result, Stream, args, classes};

pub use copilot_interp::{Env, Fired, Monitor, Observation, Samples};

pub use copilot_core::{
    Footprint, INDEX_BYTES, IndexPolicy, OpCounts, Prop, Spec, Type, Typed, Value, cost, resources,
};
