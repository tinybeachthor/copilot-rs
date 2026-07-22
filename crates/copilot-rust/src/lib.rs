//! Rust code generator for copilot-rs specifications.
//!
//! Turns a [`Spec`] into a `no_std` Rust monitor: no allocation, no `unsafe`,
//! no loops, and a step function whose cost and memory are fixed by the
//! specification rather than by the data it sees.
//!
//! ```
//! use copilot_lang::Builder;
//! use copilot_rust::{Settings, generate};
//!
//! let b = Builder::new();
//! let counter = b.stream([0u64], |s| s + 1u64);
//! b.trigger("rollover", counter.eq_val(u64::MAX), copilot_lang::args![]);
//! let spec = b.finish()?;
//!
//! let source = generate(&spec, &Settings::default())?;
//! assert!(source.contains("pub struct Monitor"));
//! assert!(source.contains("pub fn step<E: Env, H: Handler>"));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is generated
//!
//! - A `#[repr(C)]` state struct holding one ring buffer per stream, plus a
//!   rotating index for each buffer deeper than one element. `repr(C)` is
//!   required, not cosmetic: [`copilot_core::resources`] reports this type's
//!   exact size, and `repr(Rust)` could reorder the fields and leave that
//!   figure unfalsifiable.
//! - An `Env` trait supplying external variables, and a `Handler` trait
//!   receiving triggers and observers.
//! - A `const fn new()` and a `step` function.
//!
//! # The shape of a step
//!
//! `step` follows the four phases in `docs/semantics.md`, with one departure
//! worth naming: every reachable subexpression is bound once, up front,
//! including trigger arguments whose guard turns out to be false. Expressions
//! are pure, so computing them is unobservable — and computing them
//! unconditionally is what makes a step's timing independent of its data.
//!
//! Binding each node exactly once is also what makes the generated code cost
//! what [`copilot_core::cost`] reports. Inlining shared subexpressions instead
//! would do `nodes_inlined` work, and the reported figure would be fiction.
//!
//! # Maths
//!
//! `core` provides the exactly-rounded float operations — `abs`, `signum`,
//! `copysign` — but not `sqrt` or anything transcendental. Those lower to calls
//! into [`Settings::math`], `libm` by default, which the generated crate must
//! depend on.
//!
//! `sqrt`, `ceil` and `floor` are exactly rounded, so `libm` and `std` agree on
//! them bit for bit. The transcendentals are not, and may differ in the last
//! place between implementations. That is a real limitation on comparing a
//! generated monitor against an interpreter running elsewhere; see
//! `docs/semantics.md`.

mod emit;
mod expr;
mod render;

use copilot_core::{IndexPolicy, Spec};
use std::fmt;

/// Result alias for code generation.
pub type Result<T> = std::result::Result<T, Error>;

/// Something that prevents a specification from being compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The specification is not valid.
    Core(copilot_core::Error),

    /// Two different struct types share a name, so they would emit two
    /// conflicting Rust definitions.
    ConflictingStruct(String),

    /// A trigger is named `observe_<x>` for an observer `x`, so the two would
    /// emit the same trait method.
    NameCollision {
        /// The trigger's name.
        trigger: String,
        /// The observer whose generated method it collides with.
        observer: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Core(e) => e.fmt(f),
            Error::ConflictingStruct(name) => write!(
                f,
                "two different struct types are both named `{name}`, which would generate two \
                 conflicting definitions"
            ),
            Error::NameCollision { trigger, observer } => write!(
                f,
                "trigger `{trigger}` collides with the method generated for observer \
                 `{observer}`; rename one of them"
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Core(e) => Some(e),
            _ => None,
        }
    }
}

impl From<copilot_core::Error> for Error {
    fn from(e: copilot_core::Error) -> Self {
        Error::Core(e)
    }
}

/// How to name and lower a monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Name of the generated state struct.
    pub monitor: String,
    /// Name of the generated step function.
    pub step: String,
    /// Name of the trait supplying external variables.
    pub env_trait: String,
    /// Name of the trait receiving triggers and observers.
    pub handler_trait: String,
    /// Path to the maths library providing `sqrt` and the transcendentals.
    ///
    /// `libm` by default. A test harness can point this at a `std`-backed shim
    /// so that generated code and an interpreter compute transcendentals
    /// identically, rather than differing in the last place.
    pub math: String,
    /// What an out-of-range array subscript does.
    ///
    /// Must match the policy the interpreter is configured with, or the two
    /// stop agreeing.
    pub index_policy: IndexPolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            monitor: "Monitor".into(),
            step: "step".into(),
            env_trait: "Env".into(),
            handler_trait: "Handler".into(),
            math: "libm".into(),
            index_policy: IndexPolicy::default(),
        }
    }
}

/// Generates a monitor as a sequence of items, with no crate-level attributes.
///
/// Suitable for `include!`ing into an existing module. Use
/// [`generate_crate`] for a standalone `no_std` crate.
pub fn generate(spec: &Spec, settings: &Settings) -> Result<String> {
    emit::items(spec, settings)
}

/// Generates a monitor as a complete `no_std` crate root.
pub fn generate_crate(spec: &Spec, settings: &Settings) -> Result<String> {
    let mut out = String::from("#![no_std]\n#![forbid(unsafe_code)]\n\n");
    out.push_str(&emit::items(spec, settings)?);
    Ok(out)
}
