//! Bluespec code generator for copilot-rs specifications.
//!
//! Turns a [`Spec`] into Bluespec Classic (`.bs`) packages describing a
//! synthesisable monitor: one step per clock cycle, one rule, no state beyond
//! the stream buffers the specification asks for.
//!
//! ```
//! use copilot_lang::Builder;
//! use copilot_bluespec::{Settings, generate};
//!
//! let b = Builder::new();
//! let counter = b.stream([0u64], |s| s + 1u64);
//! b.trigger("rollover", counter.eq_val(u64::MAX), copilot_lang::args![]);
//! let spec = b.finish()?;
//!
//! let packages = generate(&spec, &Settings::default())?;
//! assert_eq!(packages.last().unwrap().name, "Monitor");
//! assert!(packages.last().unwrap().source.contains("mkMonitor ifc ="));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # What is generated
//!
//! Up to three packages, named after [`Settings::name`] and emitted in an order
//! that compiles left to right:
//!
//! - `<Name>Types` — a `struct` per struct type, deriving `Bits` so it can live
//!   in a register. Omitted when the specification has none.
//! - `<Name>Ifc` — the interface the monitor is given: an `ActionValue` method
//!   per external variable, an `Action` method per trigger, and an
//!   `observe_<name>` method per observer.
//! - `<Name>` — `mk<Name>`, the monitor itself.
//!
//! # The shape of a step
//!
//! `mk<Name>` holds one register per buffer slot, plus a rotating index for
//! each buffer deeper than one element, and a single rule that fires every
//! cycle. The rule's body follows the four phases in `docs/semantics.md`, but
//! two of them come for free here: registers change at the clock edge, so
//! everything the rule reads is the state as it stood at the start of the step,
//! and the compute/commit split that generated Rust has to be careful about is
//! a property of the hardware rather than of the emitted code.
//!
//! Because the rule fires unconditionally and holds no loop, a step takes
//! exactly one cycle regardless of the data — the same constant-time claim the
//! Rust backend makes, in the form hardware states it.
//!
//! # Floating point
//!
//! Specifications mentioning `Float` or `Double` are refused; see
//! [`Error::UnsupportedType`].

mod emit;
mod expr;
mod render;
mod sim;

use copilot_core::{IndexPolicy, Spec, Type};
use std::fmt;
use std::path::{Path, PathBuf};

pub use sim::testbench;

/// Result alias for code generation.
pub type Result<T> = std::result::Result<T, Error>;

/// Something that prevents a specification from being compiled to Bluespec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The specification is not valid.
    Core(copilot_core::Error),

    /// The specification mentions a type this backend does not yet carry.
    ///
    /// In practice this is always `Float` or `Double`, and it is a gap rather
    /// than a design win: upstream's `copilot-bluespec` supports both.
    ///
    /// Bluespec's floats are a soft-float library rather than a primitive type.
    /// It offers no transcendental functions at all, so `sqrt` and its
    /// neighbours have nothing to lower to — and, more immediately, its
    /// `FloatingPoint` values cannot be compared or divided during elaboration,
    /// so `bsc` rejects an expression as ordinary as `x < 4.0`. A specification
    /// carried this far would therefore not merely lose precision; it would not
    /// build. Refusing it here names the type at the point the user can act on
    /// it, and keeps this backend's agreement with the interpreter total rather
    /// than partial.
    ///
    /// Lifting the restriction is follow-up work, with upstream as the
    /// reference.
    UnsupportedType(Type),

    /// Two different struct types share a name, so they would emit two
    /// conflicting definitions in one package.
    ConflictingStruct(String),

    /// A trigger is named `observe_<x>` for an observer `x`, so the two would
    /// emit the same interface method.
    NameCollision {
        /// The trigger's name.
        trigger: String,
        /// The observer whose generated method it collides with.
        observer: String,
    },

    /// A name the specification supplies is not usable as a Bluespec
    /// identifier.
    InvalidName {
        /// The offending name.
        name: String,
        /// What it names: a trigger, an observer, a struct field, and so on.
        kind: &'static str,
        /// Why it cannot be used.
        reason: &'static str,
    },

    /// A testbench was asked for over a trace that does not cover an external
    /// variable the specification reads.
    MissingSample {
        /// The variable with no value.
        variable: String,
        /// The step at which it was missing.
        step: usize,
    },

    /// A testbench was asked for over a trace with no steps in it.
    EmptyTrace,

    /// A generated package could not be written.
    Io {
        /// The file being written.
        path: PathBuf,
        /// What the operating system said.
        message: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Core(e) => e.fmt(f),
            Error::UnsupportedType(ty) => write!(
                f,
                "the Bluespec backend does not yet support `{ty}`: Bluespec's floating point is a \
                 library rather than a primitive type, offers no transcendental functions, and \
                 cannot even be compared or divided during elaboration, so a monitor using it \
                 would not build. Use an integer or fixed-point representation, or the Rust \
                 backend, which supports floats fully"
            ),
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
            Error::InvalidName { name, kind, reason } => {
                write!(f, "{kind} name `{name}` {reason}")
            }
            Error::MissingSample { variable, step } => write!(
                f,
                "the trace has no value for external variable `{variable}` at step {step}"
            ),
            Error::EmptyTrace => write!(f, "a testbench needs a trace of at least one step"),
            Error::Io { path, message } => {
                write!(f, "could not write {}: {message}", path.display())
            }
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

/// One generated Bluespec package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// The package's name, which is also its file's stem.
    pub name: String,
    /// Its source.
    pub source: String,
}

impl Package {
    /// The file this package must be written to.
    ///
    /// Bluespec finds a package by looking for a file named after it, so this
    /// is a requirement rather than a convention.
    pub fn file_name(&self) -> String {
        format!("{}.bs", self.name)
    }
}

/// How to name and lower a monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Name of the generated monitor package.
    ///
    /// Doubles as the file prefix, as Bluespec requires, and as the stem of
    /// every other generated name: `mk<Name>` for the module, `<Name>Ifc` for
    /// the interface, `<Name>Types` for the struct definitions.
    pub name: String,
    /// Where [`compile`] writes the generated packages.
    pub output_directory: PathBuf,
    /// What an out-of-range array subscript does.
    ///
    /// Must match the policy the interpreter is configured with, or the two
    /// stop agreeing.
    pub index_policy: IndexPolicy,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            name: "Monitor".into(),
            output_directory: PathBuf::from("."),
            index_policy: IndexPolicy::default(),
        }
    }
}

impl Settings {
    /// The package holding the struct definitions.
    fn types_package(&self) -> String {
        format!("{}Types", self.name)
    }

    /// The package holding the interface.
    fn interface_package(&self) -> String {
        format!("{}Ifc", self.name)
    }

    /// The interface type, which shares its package's name.
    fn interface(&self) -> String {
        self.interface_package()
    }

    /// The module constructor.
    fn module(&self) -> String {
        format!("mk{}", self.name)
    }
}

/// Generates a monitor's packages, in an order that compiles left to right.
pub fn generate(spec: &Spec, settings: &Settings) -> Result<Vec<Package>> {
    emit::packages(spec, settings)
}

/// Generates a monitor and writes it to [`Settings::output_directory`],
/// returning the files written.
pub fn compile(spec: &Spec, settings: &Settings) -> Result<Vec<PathBuf>> {
    let packages = generate(spec, settings)?;
    std::fs::create_dir_all(&settings.output_directory).map_err(|e| Error::Io {
        path: settings.output_directory.clone(),
        message: e.to_string(),
    })?;
    packages
        .iter()
        .map(|package| write(&settings.output_directory, package))
        .collect()
}

fn write(directory: &Path, package: &Package) -> Result<PathBuf> {
    let path = directory.join(package.file_name());
    std::fs::write(&path, &package.source).map_err(|e| Error::Io {
        path: path.clone(),
        message: e.to_string(),
    })?;
    Ok(path)
}
