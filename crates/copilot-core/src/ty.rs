//! Types and values.
//!
//! Upstream Copilot indexes its expression GADT by a Haskell type. Rust has no
//! GADTs, so the IR is runtime-typed: [`Type`] is an ordinary enum, every node
//! carries its type, and [`crate::typecheck`] re-derives all of them from
//! scratch. Static typing comes back at the frontend, where `Stream<T>` is
//! phantom-typed over a [`Typed`] Rust type.

use crate::error::{Error, Result};
use std::fmt;
use std::hash::{Hash, Hasher};

/// The type of a Copilot expression.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// Boolean.
    Bool,
    /// Signed 8-bit integer.
    Int8,
    /// Signed 16-bit integer.
    Int16,
    /// Signed 32-bit integer.
    Int32,
    /// Signed 64-bit integer.
    Int64,
    /// Unsigned 8-bit integer.
    Word8,
    /// Unsigned 16-bit integer.
    Word16,
    /// Unsigned 32-bit integer.
    Word32,
    /// Unsigned 64-bit integer.
    Word64,
    /// Single-precision float.
    Float,
    /// Double-precision float.
    Double,
    /// Fixed-length array. `len` is always positive; see [`Error::ZeroLengthArray`].
    Array {
        /// Element type.
        elem: Box<Type>,
        /// Number of elements.
        len: usize,
    },
    /// Named record with at least one field, in declaration order.
    ///
    /// Boxed so that `Type` stays 24 bytes rather than 48. A `Type` is stored
    /// on every arena node and carried by every operator, so its size is paid
    /// for by the whole IR, whereas struct types are rare.
    Struct(Box<StructType>),
}

/// The name and fields of a struct type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StructType {
    /// Type name, used verbatim in generated code.
    pub name: String,
    /// Fields, in declaration order.
    pub fields: Vec<(String, Type)>,
}

/// Size and alignment of a type, under `repr(C)` rules.
///
/// Generated monitors declare their state with `#[repr(C)]` precisely so that
/// this computation is exact rather than advisory — `repr(Rust)` is free to
/// reorder fields, which would make the footprint reported by
/// [`crate::resources`] unfalsifiable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Size in bytes, including trailing padding.
    pub size: usize,
    /// Alignment in bytes; always a power of two.
    pub align: usize,
}

impl Layout {
    /// The layout of an aggregate with no fields yet.
    pub const EMPTY: Layout = Layout { size: 0, align: 1 };

    /// Layout of a primitive of the given size, whose alignment equals its size.
    const fn primitive(size: usize) -> Self {
        Layout { size, align: size }
    }

    /// Appends a field to a `repr(C)` aggregate being laid out, returning the
    /// field's offset.
    pub fn extend(&mut self, field: Layout) -> usize {
        let offset = round_up(self.size, field.align);
        self.size = offset + field.size;
        self.align = self.align.max(field.align);
        offset
    }

    /// Rounds the size up to the alignment, as `repr(C)` does at the end of an
    /// aggregate.
    pub fn pad_to_align(mut self) -> Self {
        self.size = round_up(self.size, self.align);
        self
    }
}

const fn round_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

impl Type {
    /// A struct type with the given name and fields.
    pub fn structure(
        name: impl Into<String>,
        fields: impl IntoIterator<Item = (String, Type)>,
    ) -> Type {
        Type::Struct(Box::new(StructType {
            name: name.into(),
            fields: fields.into_iter().collect(),
        }))
    }

    /// An array of `len` elements of the given type.
    pub fn array(elem: Type, len: usize) -> Type {
        Type::Array {
            elem: Box::new(elem),
            len,
        }
    }

    /// The struct's name and fields, for struct types.
    pub fn as_struct(&self) -> Option<&StructType> {
        match self {
            Type::Struct(s) => Some(s),
            _ => None,
        }
    }

    /// True for the signed and unsigned integer types.
    ///
    /// These are exactly the types admitting `Div`, `Mod`, and the bitwise
    /// operators — upstream's `Integral` and `Bits` classes coincide here.
    pub fn is_integral(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::Word8
                | Type::Word16
                | Type::Word32
                | Type::Word64
        )
    }

    /// True for `Float` and `Double`.
    ///
    /// Stands in for upstream's `Fractional`, `Floating`, `RealFrac`, and
    /// `RealFloat`, which all collapse to these two types.
    pub fn is_floating(&self) -> bool {
        matches!(self, Type::Float | Type::Double)
    }

    /// True for integral and floating types.
    pub fn is_numeric(&self) -> bool {
        self.is_integral() || self.is_floating()
    }

    /// True for the signed integer types and the floats.
    pub fn is_signed(&self) -> bool {
        matches!(self, Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64) || self.is_floating()
    }

    /// True for types that are totally ordered, and so admit `<`, `<=`, `>`, `>=`.
    pub fn is_ordered(&self) -> bool {
        self.is_numeric() || matches!(self, Type::Bool)
    }

    /// True for types held in a single machine word — everything but arrays and
    /// structs.
    ///
    /// Equality is restricted to these. Aggregate comparison would be a
    /// fully-unrolled element-wise walk; keeping it out of the IR is what lets
    /// a generated `step()` stay small enough for the bisimulation proof to run
    /// without an unwinding bound.
    pub fn is_scalar(&self) -> bool {
        !matches!(self, Type::Array { .. } | Type::Struct(_))
    }

    /// Element type, for arrays.
    pub fn elem(&self) -> Option<&Type> {
        match self {
            Type::Array { elem, .. } => Some(elem),
            _ => None,
        }
    }

    /// Type of the named field, for structs.
    pub fn field(&self, name: &str) -> Option<&Type> {
        match self {
            Type::Struct(s) => s.fields.iter().find(|(n, _)| n == name).map(|(_, t)| t),
            _ => None,
        }
    }

    /// Size and alignment under `repr(C)`.
    pub fn layout(&self) -> Layout {
        match self {
            Type::Bool | Type::Int8 | Type::Word8 => Layout::primitive(1),
            Type::Int16 | Type::Word16 => Layout::primitive(2),
            Type::Int32 | Type::Word32 | Type::Float => Layout::primitive(4),
            Type::Int64 | Type::Word64 | Type::Double => Layout::primitive(8),
            Type::Array { elem, len } => {
                let elem = elem.layout();
                Layout {
                    size: elem.size * len,
                    align: elem.align,
                }
            }
            Type::Struct(s) => {
                let mut layout = Layout::EMPTY;
                for (_, ty) in &s.fields {
                    layout.extend(ty.layout());
                }
                layout.pad_to_align()
            }
        }
    }

    /// Rejects types that no backend can represent: zero-length arrays and
    /// fieldless structs, both of which upstream Copilot also rejects.
    pub fn validate(&self) -> Result<()> {
        match self {
            Type::Array { elem, len } => {
                if *len == 0 {
                    return Err(Error::ZeroLengthArray);
                }
                elem.validate()
            }
            Type::Struct(s) => {
                if s.fields.is_empty() {
                    return Err(Error::EmptyStruct(s.name.clone()));
                }
                for (_, ty) in &s.fields {
                    ty.validate()?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Bool => f.write_str("Bool"),
            Type::Int8 => f.write_str("Int8"),
            Type::Int16 => f.write_str("Int16"),
            Type::Int32 => f.write_str("Int32"),
            Type::Int64 => f.write_str("Int64"),
            Type::Word8 => f.write_str("Word8"),
            Type::Word16 => f.write_str("Word16"),
            Type::Word32 => f.write_str("Word32"),
            Type::Word64 => f.write_str("Word64"),
            Type::Float => f.write_str("Float"),
            Type::Double => f.write_str("Double"),
            Type::Array { elem, len } => write!(f, "[{elem}; {len}]"),
            Type::Struct(s) => f.write_str(&s.name),
        }
    }
}

/// A constant inhabiting a [`Type`].
///
/// Floats compare and hash bitwise, not numerically. Values are only ever
/// compared for *structural identity* — to decide whether two constants can
/// share an arena slot — and bitwise comparison is the only total answer there.
/// It also keeps `NaN` usable as an initial value, which numeric equality would
/// not.
#[derive(Debug, Clone)]
pub enum Value {
    /// Boolean.
    Bool(bool),
    /// Signed 8-bit integer.
    Int8(i8),
    /// Signed 16-bit integer.
    Int16(i16),
    /// Signed 32-bit integer.
    Int32(i32),
    /// Signed 64-bit integer.
    Int64(i64),
    /// Unsigned 8-bit integer.
    Word8(u8),
    /// Unsigned 16-bit integer.
    Word16(u16),
    /// Unsigned 32-bit integer.
    Word32(u32),
    /// Unsigned 64-bit integer.
    Word64(u64),
    /// Single-precision float.
    Float(f32),
    /// Double-precision float.
    Double(f64),
    /// Array literal.
    Array(Vec<Value>),
    /// Struct literal, with fields in declaration order.
    Struct {
        /// Type name.
        name: String,
        /// Fields, in declaration order.
        fields: Vec<(String, Value)>,
    },
}

impl Value {
    /// Whether this value inhabits `ty`.
    ///
    /// There is deliberately no `Value::ty()`: an empty array literal has no
    /// recoverable element type, so inferring a type from a value is partial
    /// whereas checking against one is total. Constants therefore always carry
    /// an explicit type, mirroring upstream's `Const :: Type a -> a -> Expr a`.
    pub fn matches(&self, ty: &Type) -> bool {
        match (self, ty) {
            (Value::Bool(_), Type::Bool)
            | (Value::Int8(_), Type::Int8)
            | (Value::Int16(_), Type::Int16)
            | (Value::Int32(_), Type::Int32)
            | (Value::Int64(_), Type::Int64)
            | (Value::Word8(_), Type::Word8)
            | (Value::Word16(_), Type::Word16)
            | (Value::Word32(_), Type::Word32)
            | (Value::Word64(_), Type::Word64)
            | (Value::Float(_), Type::Float)
            | (Value::Double(_), Type::Double) => true,
            (Value::Array(values), Type::Array { elem, len }) => {
                values.len() == *len && values.iter().all(|v| v.matches(elem))
            }
            (
                Value::Struct {
                    name,
                    fields: values,
                },
                Type::Struct(ty),
            ) => {
                let (ty_name, ty_fields) = (&ty.name, &ty.fields);
                name == ty_name
                    && values.len() == ty_fields.len()
                    && values
                        .iter()
                        .zip(ty_fields)
                        .all(|((vn, v), (tn, t))| vn == tn && v.matches(t))
            }
            _ => false,
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int8(a), Value::Int8(b)) => a == b,
            (Value::Int16(a), Value::Int16(b)) => a == b,
            (Value::Int32(a), Value::Int32(b)) => a == b,
            (Value::Int64(a), Value::Int64(b)) => a == b,
            (Value::Word8(a), Value::Word8(b)) => a == b,
            (Value::Word16(a), Value::Word16(b)) => a == b,
            (Value::Word32(a), Value::Word32(b)) => a == b,
            (Value::Word64(a), Value::Word64(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Double(a), Value::Double(b)) => a.to_bits() == b.to_bits(),
            (Value::Array(a), Value::Array(b)) => a == b,
            (
                Value::Struct {
                    name: an,
                    fields: af,
                },
                Value::Struct {
                    name: bn,
                    fields: bf,
                },
            ) => an == bn && af == bf,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Hash for Value {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Value::Bool(v) => v.hash(state),
            Value::Int8(v) => v.hash(state),
            Value::Int16(v) => v.hash(state),
            Value::Int32(v) => v.hash(state),
            Value::Int64(v) => v.hash(state),
            Value::Word8(v) => v.hash(state),
            Value::Word16(v) => v.hash(state),
            Value::Word32(v) => v.hash(state),
            Value::Word64(v) => v.hash(state),
            Value::Float(v) => v.to_bits().hash(state),
            Value::Double(v) => v.to_bits().hash(state),
            Value::Array(v) => v.hash(state),
            Value::Struct { name, fields } => {
                name.hash(state);
                fields.hash(state);
            }
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Bool(v) => write!(f, "{v}"),
            Value::Int8(v) => write!(f, "{v}"),
            Value::Int16(v) => write!(f, "{v}"),
            Value::Int32(v) => write!(f, "{v}"),
            Value::Int64(v) => write!(f, "{v}"),
            Value::Word8(v) => write!(f, "{v}"),
            Value::Word16(v) => write!(f, "{v}"),
            Value::Word32(v) => write!(f, "{v}"),
            Value::Word64(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v:?}"),
            Value::Double(v) => write!(f, "{v:?}"),
            Value::Array(values) => {
                f.write_str("[")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::Struct { name, fields } => {
                write!(f, "{name} {{ ")?;
                for (i, (n, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{n}: {v}")?;
                }
                f.write_str(" }")
            }
        }
    }
}

/// A Rust type that can appear in a specification.
///
/// This is the bridge that gives the frontend static typing over a
/// runtime-typed IR: `Stream<T>` is phantom-typed over a `T: Typed`, so an
/// ill-typed spec fails to compile rather than failing [`crate::typecheck`].
pub trait Typed: Copy + 'static {
    /// The IR type corresponding to `Self`.
    fn ty() -> Type;

    /// Reifies a Rust value as an IR constant.
    fn lift(self) -> Value;
}

macro_rules! impl_typed {
    ($($rust:ty => $ty:ident / $value:ident),* $(,)?) => {
        $(
            impl Typed for $rust {
                fn ty() -> Type { Type::$ty }
                fn lift(self) -> Value { Value::$value(self) }
            }
        )*
    };
}

impl_typed! {
    bool => Bool   / Bool,
    i8   => Int8   / Int8,
    i16  => Int16  / Int16,
    i32  => Int32  / Int32,
    i64  => Int64  / Int64,
    u8   => Word8  / Word8,
    u16  => Word16 / Word16,
    u32  => Word32 / Word32,
    u64  => Word64 / Word64,
    f32  => Float  / Float,
    f64  => Double / Double,
}

impl<T: Typed, const N: usize> Typed for [T; N] {
    fn ty() -> Type {
        Type::array(T::ty(), N)
    }

    fn lift(self) -> Value {
        Value::Array(self.into_iter().map(Typed::lift).collect())
    }
}
