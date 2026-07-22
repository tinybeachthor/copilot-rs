//! Errors produced while building or validating a specification.

use crate::expr::{ExprId, StreamId, VarId};
use crate::ty::Type;
use std::fmt;

/// Result alias for fallible IR construction and validation.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can be wrong with a specification.
///
/// These are all *compile-time* errors — they are raised while a spec is being
/// built or validated, never while a generated monitor is running. A monitor
/// that passes [`crate::validate`] has no failure modes left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An operand did not belong to the class of types the operator accepts,
    /// e.g. `Mod` applied to a `Float`.
    OperandClass {
        /// Operator name, as it appears in the IR.
        op: &'static str,
        /// Zero-based operand position.
        position: usize,
        /// Prose description of the accepted class, e.g. "an integral type".
        expected: &'static str,
        /// The type actually supplied.
        found: Type,
    },

    /// An operand had the wrong concrete type.
    OperandType {
        /// Operator name, as it appears in the IR.
        op: &'static str,
        /// Zero-based operand position.
        position: usize,
        /// The type the operator requires here.
        expected: Type,
        /// The type actually supplied.
        found: Type,
    },

    /// The type tag an operator carries disagrees with its operand's type.
    ///
    /// Operators carry an explicit type tag (mirroring the type index on
    /// upstream Copilot's GADT) so that backends never have to re-infer types.
    /// A drifted tag means the IR was built by hand or deserialized incorrectly.
    OpTag {
        /// Operator name, as it appears in the IR.
        op: &'static str,
        /// The tag carried by the operator.
        tag: Type,
        /// The type of the operand it disagrees with.
        operand: Type,
    },

    /// A type mismatch outside of operator application.
    Mismatch {
        /// Where the mismatch was found, e.g. `stream 3 buffer element 0`.
        context: String,
        /// The required type.
        expected: Type,
        /// The type actually found.
        found: Type,
    },

    /// A constant's value does not inhabit its declared type.
    BadConstant {
        /// The declared type.
        ty: Type,
    },

    /// Reference to a stream that was never declared.
    UnknownStream(StreamId),

    /// Reference to a local variable that is not in scope.
    UnknownVar(VarId),

    /// Field access on a struct that has no such field.
    UnknownField {
        /// Name of the struct type.
        struct_name: String,
        /// The field that was requested.
        field: String,
    },

    /// `drop i s` where `i` reaches past the end of `s`'s buffer.
    ///
    /// A stream buffered with `n` initial values can only be looked ahead
    /// `n - 1` steps; anything further is not yet determined.
    DropOutOfRange {
        /// The stream being dropped.
        stream: StreamId,
        /// The drop index.
        idx: u32,
        /// The stream's buffer length.
        buffer_len: usize,
    },

    /// A stream was declared with no initial values, so it has no base case.
    EmptyBuffer(StreamId),

    /// A stream's buffer is not the length it was declared with.
    BufferLength {
        /// The stream.
        stream: StreamId,
        /// The declared length.
        expected: usize,
        /// The length actually supplied.
        found: usize,
    },

    /// A zero-length array type. Rejected, as upstream Copilot rejects it.
    ZeroLengthArray,

    /// A struct type with no fields. Rejected, as upstream Copilot rejects it.
    EmptyStruct(String),

    /// The same external variable name was used at two different types.
    ExternConflict {
        /// The extern's name.
        name: String,
        /// The type it was first declared at.
        first: Type,
        /// The conflicting later type.
        second: Type,
    },

    /// Two triggers, observers, or externs share a name; generated code would
    /// collide.
    DuplicateName {
        /// What kind of entity, e.g. `"trigger"`.
        kind: &'static str,
        /// The repeated name.
        name: String,
    },

    /// A name that cannot be used as an identifier in generated code.
    BadName {
        /// What kind of entity, e.g. `"trigger"`.
        kind: &'static str,
        /// The offending name.
        name: String,
    },

    /// A trigger guard that is not a boolean.
    NonBoolGuard {
        /// The trigger's name.
        trigger: String,
        /// The guard's actual type.
        found: Type,
    },

    /// Stream IDs are not the canonical `0..n` sequence matching their position.
    StreamIdMismatch {
        /// Index in [`crate::Spec::streams`].
        position: usize,
        /// The ID stored on the stream at that position.
        found: StreamId,
    },

    /// A node references a child with a higher ID than its own.
    ///
    /// The arena interns children before parents, so IDs increase towards the
    /// root. A violation means the arena is not the one that built the spec.
    NonMonotonicArena {
        /// The offending parent.
        parent: ExprId,
        /// The child it references.
        child: ExprId,
    },

    /// A node's cached type disagrees with the type recomputed from its
    /// children. Indicates a corrupted or hand-edited arena.
    TypeDrift {
        /// The offending expression.
        expr: ExprId,
        /// The type cached in the arena.
        cached: Type,
        /// The type recomputed by the typechecker.
        recomputed: Type,
    },

    /// An existentially quantified property reached something that only handles
    /// universal ones. Matches upstream's `UnexpectedExistentialProposition`.
    ExistentialProperty(String),

    /// An array was subscripted out of range under [`crate::IndexPolicy::Assume`],
    /// which defines no behaviour there.
    IndexOutOfRange {
        /// The index used.
        index: u32,
        /// The array's length.
        len: usize,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::OperandClass {
                op,
                position,
                expected,
                found,
            } => write!(
                f,
                "{op}: operand {position} must be {expected}, found {found}"
            ),
            Error::OperandType {
                op,
                position,
                expected,
                found,
            } => write!(
                f,
                "{op}: operand {position} must have type {expected}, found {found}"
            ),
            Error::OpTag { op, tag, operand } => write!(
                f,
                "{op}: type tag {tag} disagrees with operand type {operand}"
            ),
            Error::Mismatch {
                context,
                expected,
                found,
            } => {
                write!(f, "{context}: expected {expected}, found {found}")
            }
            Error::BadConstant { ty } => write!(f, "constant value does not inhabit type {ty}"),
            Error::UnknownStream(id) => write!(f, "reference to undeclared stream {id}"),
            Error::UnknownVar(id) => {
                write!(f, "reference to local variable {id} that is not in scope")
            }
            Error::UnknownField { struct_name, field } => {
                write!(f, "struct {struct_name} has no field `{field}`")
            }
            Error::DropOutOfRange {
                stream,
                idx,
                buffer_len,
            } => write!(
                f,
                "drop {idx} on stream {stream}: buffered {buffer_len} value(s), so the largest \
                 valid drop index is {}",
                buffer_len.saturating_sub(1)
            ),
            Error::EmptyBuffer(id) => {
                write!(
                    f,
                    "stream {id} has no initial values, so it is not well founded"
                )
            }
            Error::BufferLength {
                stream,
                expected,
                found,
            } => write!(
                f,
                "stream {stream} was declared to buffer {expected} value(s) but {found} were given"
            ),
            Error::ZeroLengthArray => write!(f, "zero-length arrays are not supported"),
            Error::EmptyStruct(name) => write!(f, "struct {name} has no fields"),
            Error::ExternConflict {
                name,
                first,
                second,
            } => write!(
                f,
                "external variable `{name}` used at two types: {first} and {second}"
            ),
            Error::DuplicateName { kind, name } => write!(f, "duplicate {kind} name `{name}`"),
            Error::BadName { kind, name } => write!(
                f,
                "{kind} name `{name}` is not a valid identifier for generated code"
            ),
            Error::NonBoolGuard { trigger, found } => write!(
                f,
                "guard of trigger `{trigger}` must be Bool, found {found}"
            ),
            Error::StreamIdMismatch { position, found } => write!(
                f,
                "stream at position {position} has ID {found}; IDs must match position"
            ),
            Error::NonMonotonicArena { parent, child } => write!(
                f,
                "expression {parent} references child {child}, which is not strictly earlier"
            ),
            Error::TypeDrift {
                expr,
                cached,
                recomputed,
            } => write!(
                f,
                "expression {expr}: cached type {cached} disagrees with recomputed type {recomputed}"
            ),
            Error::ExistentialProperty(name) => write!(
                f,
                "property `{name}` is existentially quantified, which is not supported here"
            ),
            Error::IndexOutOfRange { index, len } => write!(
                f,
                "index {index} is out of range for an array of {len}, and IndexPolicy::Assume \
                 defines no behaviour there"
            ),
        }
    }
}

impl std::error::Error for Error {}
