//! Reference interpreter for copilot-rs specifications.
//!
//! Runs a [`Spec`] directly, in the same constant memory a
//! generated monitor would use: one ring buffer and one rotating index per
//! stream, exactly what [`copilot_core::resources`] reports. That is what makes
//! it a useful oracle — it is the same representation as the code generators
//! emit, evaluated by walking the IR rather than by compiling it, so a
//! disagreement between the two is a real bug in one of them rather than an
//! artefact of comparing different models.
//!
//! ```
//! use copilot_interp::{Monitor, Samples};
//! use copilot_lang::Builder;
//!
//! let b = Builder::new();
//! let counter = b.stream([0u64], |s| s + 1u64);
//! b.observe("counter", counter);
//! let spec = b.finish()?;
//!
//! let mut monitor = Monitor::new(&spec)?;
//! let mut env = Samples::none();
//! let trace: Vec<_> = (0..4)
//!     .map(|_| monitor.step(&mut env).unwrap())
//!     .map(|o| o.observer("counter").unwrap().clone())
//!     .collect();
//!
//! use copilot_core::Value::Word64;
//! assert_eq!(trace, [Word64(0), Word64(1), Word64(2), Word64(3)]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod eval;
mod monitor;

pub use monitor::{Env, Fired, Monitor, Observation, Samples};

pub use copilot_core::{Error, IndexPolicy, Result, Spec, Type, Value};
