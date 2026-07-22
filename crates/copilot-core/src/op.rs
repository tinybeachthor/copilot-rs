//! Primitive operators and their typing rules.
//!
//! Every operator carries an explicit type tag in the same place upstream
//! Copilot's GADT carries a type index (`Abs :: Num a => Type a -> Op1 a a`).
//! Backends can therefore read an operator's types straight off the node
//! instead of re-inferring them, and [`crate::typecheck`] has something to
//! check the tag *against*.

use crate::error::{Error, Result};
use crate::ty::Type;

/// Unary operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op1 {
    /// Logical negation.
    Not,
    /// Absolute value.
    Abs(Type),
    /// Sign, as -1, 0, or 1.
    Sign(Type),
    /// Reciprocal.
    Recip(Type),
    /// `e^x`.
    Exp(Type),
    /// Square root.
    Sqrt(Type),
    /// Natural logarithm.
    Log(Type),
    /// Sine.
    Sin(Type),
    /// Tangent.
    Tan(Type),
    /// Cosine.
    Cos(Type),
    /// Arcsine.
    Asin(Type),
    /// Arctangent.
    Atan(Type),
    /// Arccosine.
    Acos(Type),
    /// Hyperbolic sine.
    Sinh(Type),
    /// Hyperbolic tangent.
    Tanh(Type),
    /// Hyperbolic cosine.
    Cosh(Type),
    /// Inverse hyperbolic sine.
    Asinh(Type),
    /// Inverse hyperbolic tangent.
    Atanh(Type),
    /// Inverse hyperbolic cosine.
    Acosh(Type),
    /// Round towards positive infinity.
    Ceiling(Type),
    /// Round towards negative infinity.
    Floor(Type),
    /// Bitwise complement.
    BwNot(Type),
    /// Numeric conversion.
    Cast {
        /// Source type; must be integral.
        from: Type,
        /// Target type; must be numeric.
        to: Type,
    },
    /// Struct field projection.
    GetField {
        /// The struct type being projected.
        struct_ty: Type,
        /// Field name.
        field: String,
    },
}

/// Binary operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op2 {
    /// Logical conjunction.
    And,
    /// Logical disjunction.
    Or,
    /// Addition. Wraps on overflow; see the crate docs.
    Add(Type),
    /// Subtraction. Wraps on overflow.
    Sub(Type),
    /// Multiplication. Wraps on overflow.
    Mul(Type),
    /// Integer remainder.
    Mod(Type),
    /// Integer division.
    Div(Type),
    /// Floating-point division.
    Fdiv(Type),
    /// Exponentiation.
    Pow(Type),
    /// Logarithm in a given base.
    Logb(Type),
    /// Two-argument arctangent.
    Atan2(Type),
    /// Equality.
    Eq(Type),
    /// Inequality.
    Ne(Type),
    /// Less than or equal.
    Le(Type),
    /// Greater than or equal.
    Ge(Type),
    /// Less than.
    Lt(Type),
    /// Greater than.
    Gt(Type),
    /// Bitwise and.
    BwAnd(Type),
    /// Bitwise or.
    BwOr(Type),
    /// Bitwise exclusive or.
    BwXor(Type),
    /// Left shift.
    BwShiftL {
        /// Type of the value being shifted.
        val: Type,
        /// Type of the shift amount.
        amount: Type,
    },
    /// Right shift.
    BwShiftR {
        /// Type of the value being shifted.
        val: Type,
        /// Type of the shift amount.
        amount: Type,
    },
    /// Array subscript, by a `Word32` index.
    Index(Type),
    /// Functional struct update, returning a struct with one field replaced.
    UpdateField {
        /// The struct type.
        struct_ty: Type,
        /// Field name.
        field: String,
    },
}

/// Ternary operators.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Op3 {
    /// Branchless conditional: `Mux(c, t, f)`.
    Mux(Type),
    /// Functional array update, returning an array with one element replaced.
    UpdateArray(Type),
}

/// Coarse cost category, used by [`crate::cost`] to report where a monitor's
/// per-step work goes.
///
/// The split is by execution cost on a typical embedded target, not by
/// syntax — [`OpClass::Transcendental`] is broken out because a `sin` call can
/// cost more than the rest of a monitor put together, and
/// [`OpClass::Division`] because integer division is the one multi-cycle
/// arithmetic instruction on most cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpClass {
    /// A literal.
    Const,
    /// A read from a stream buffer.
    Load,
    /// A sample of an external variable.
    Extern,
    /// A local binding or a reference to one.
    Binding,
    /// Boolean connectives.
    Logic,
    /// Addition, subtraction, multiplication, negation, absolute value.
    Arith,
    /// Integer and floating-point division and remainder.
    Division,
    /// Comparisons.
    Compare,
    /// Bitwise operations and shifts.
    Bitwise,
    /// Numeric conversion.
    Cast,
    /// Transcendental and root functions.
    Transcendental,
    /// Array subscript.
    ArrayIndex,
    /// Whole-array copy with one element replaced.
    ArrayUpdate,
    /// Struct field projection.
    FieldGet,
    /// Whole-struct copy with one field replaced.
    FieldUpdate,
    /// Conditional selection.
    Select,
    /// Carries no cost: an annotation that generated code drops.
    Nop,
}

impl Op1 {
    /// Name as it appears in diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Op1::Not => "not",
            Op1::Abs(_) => "abs",
            Op1::Sign(_) => "signum",
            Op1::Recip(_) => "recip",
            Op1::Exp(_) => "exp",
            Op1::Sqrt(_) => "sqrt",
            Op1::Log(_) => "log",
            Op1::Sin(_) => "sin",
            Op1::Tan(_) => "tan",
            Op1::Cos(_) => "cos",
            Op1::Asin(_) => "asin",
            Op1::Atan(_) => "atan",
            Op1::Acos(_) => "acos",
            Op1::Sinh(_) => "sinh",
            Op1::Tanh(_) => "tanh",
            Op1::Cosh(_) => "cosh",
            Op1::Asinh(_) => "asinh",
            Op1::Atanh(_) => "atanh",
            Op1::Acosh(_) => "acosh",
            Op1::Ceiling(_) => "ceiling",
            Op1::Floor(_) => "floor",
            Op1::BwNot(_) => "complement",
            Op1::Cast { .. } => "cast",
            Op1::GetField { .. } => "get_field",
        }
    }

    /// Cost category.
    pub fn class(&self) -> OpClass {
        match self {
            Op1::Not => OpClass::Logic,
            Op1::Abs(_) | Op1::Sign(_) | Op1::Ceiling(_) | Op1::Floor(_) => OpClass::Arith,
            Op1::Recip(_) => OpClass::Division,
            Op1::BwNot(_) => OpClass::Bitwise,
            Op1::Cast { .. } => OpClass::Cast,
            Op1::GetField { .. } => OpClass::FieldGet,
            _ => OpClass::Transcendental,
        }
    }

    /// Result type for an application to `arg`, or an error explaining why the
    /// application is ill-typed.
    pub fn result_ty(&self, arg: &Type) -> Result<Type> {
        match self {
            Op1::Not => {
                expect_ty(self.name(), 0, &Type::Bool, arg)?;
                Ok(Type::Bool)
            }
            Op1::Abs(tag) | Op1::Sign(tag) => {
                same_as_tag(self.name(), tag, arg)?;
                expect_class(self.name(), 0, "a numeric type", Type::is_numeric, arg)?;
                Ok(tag.clone())
            }
            Op1::Recip(tag)
            | Op1::Exp(tag)
            | Op1::Sqrt(tag)
            | Op1::Log(tag)
            | Op1::Sin(tag)
            | Op1::Tan(tag)
            | Op1::Cos(tag)
            | Op1::Asin(tag)
            | Op1::Atan(tag)
            | Op1::Acos(tag)
            | Op1::Sinh(tag)
            | Op1::Tanh(tag)
            | Op1::Cosh(tag)
            | Op1::Asinh(tag)
            | Op1::Atanh(tag)
            | Op1::Acosh(tag)
            | Op1::Ceiling(tag)
            | Op1::Floor(tag) => {
                same_as_tag(self.name(), tag, arg)?;
                expect_class(
                    self.name(),
                    0,
                    "a floating-point type",
                    Type::is_floating,
                    arg,
                )?;
                Ok(tag.clone())
            }
            Op1::BwNot(tag) => {
                same_as_tag(self.name(), tag, arg)?;
                expect_class(self.name(), 0, "an integral type", Type::is_integral, arg)?;
                Ok(tag.clone())
            }
            Op1::Cast { from, to } => {
                same_as_tag(self.name(), from, arg)?;
                expect_class(self.name(), 0, "an integral type", Type::is_integral, arg)?;
                if !to.is_numeric() {
                    return Err(Error::OperandClass {
                        op: self.name(),
                        position: 0,
                        expected: "a cast to a numeric type",
                        found: to.clone(),
                    });
                }
                Ok(to.clone())
            }
            Op1::GetField { struct_ty, field } => {
                same_as_tag(self.name(), struct_ty, arg)?;
                field_ty(struct_ty, field).cloned()
            }
        }
    }
}

impl Op2 {
    /// Name as it appears in diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Op2::And => "and",
            Op2::Or => "or",
            Op2::Add(_) => "add",
            Op2::Sub(_) => "sub",
            Op2::Mul(_) => "mul",
            Op2::Mod(_) => "mod",
            Op2::Div(_) => "div",
            Op2::Fdiv(_) => "fdiv",
            Op2::Pow(_) => "pow",
            Op2::Logb(_) => "logb",
            Op2::Atan2(_) => "atan2",
            Op2::Eq(_) => "eq",
            Op2::Ne(_) => "ne",
            Op2::Le(_) => "le",
            Op2::Ge(_) => "ge",
            Op2::Lt(_) => "lt",
            Op2::Gt(_) => "gt",
            Op2::BwAnd(_) => "bw_and",
            Op2::BwOr(_) => "bw_or",
            Op2::BwXor(_) => "bw_xor",
            Op2::BwShiftL { .. } => "bw_shift_l",
            Op2::BwShiftR { .. } => "bw_shift_r",
            Op2::Index(_) => "index",
            Op2::UpdateField { .. } => "update_field",
        }
    }

    /// Cost category.
    pub fn class(&self) -> OpClass {
        match self {
            Op2::And | Op2::Or => OpClass::Logic,
            Op2::Add(_) | Op2::Sub(_) | Op2::Mul(_) => OpClass::Arith,
            Op2::Mod(_) | Op2::Div(_) | Op2::Fdiv(_) => OpClass::Division,
            Op2::Pow(_) | Op2::Logb(_) | Op2::Atan2(_) => OpClass::Transcendental,
            Op2::Eq(_) | Op2::Ne(_) | Op2::Le(_) | Op2::Ge(_) | Op2::Lt(_) | Op2::Gt(_) => {
                OpClass::Compare
            }
            Op2::BwAnd(_)
            | Op2::BwOr(_)
            | Op2::BwXor(_)
            | Op2::BwShiftL { .. }
            | Op2::BwShiftR { .. } => OpClass::Bitwise,
            Op2::Index(_) => OpClass::ArrayIndex,
            Op2::UpdateField { .. } => OpClass::FieldUpdate,
        }
    }

    /// Result type for an application to `a` and `b`.
    pub fn result_ty(&self, a: &Type, b: &Type) -> Result<Type> {
        match self {
            Op2::And | Op2::Or => {
                expect_ty(self.name(), 0, &Type::Bool, a)?;
                expect_ty(self.name(), 1, &Type::Bool, b)?;
                Ok(Type::Bool)
            }
            Op2::Add(tag) | Op2::Sub(tag) | Op2::Mul(tag) => {
                self.homogeneous(tag, a, b, "a numeric type", Type::is_numeric)
            }
            Op2::Mod(tag) | Op2::Div(tag) => {
                self.homogeneous(tag, a, b, "an integral type", Type::is_integral)
            }
            Op2::Fdiv(tag) | Op2::Pow(tag) | Op2::Logb(tag) | Op2::Atan2(tag) => {
                self.homogeneous(tag, a, b, "a floating-point type", Type::is_floating)
            }
            Op2::Eq(tag) | Op2::Ne(tag) => {
                self.homogeneous(tag, a, b, "a scalar type", Type::is_scalar)?;
                Ok(Type::Bool)
            }
            Op2::Le(tag) | Op2::Ge(tag) | Op2::Lt(tag) | Op2::Gt(tag) => {
                self.homogeneous(tag, a, b, "an ordered type", Type::is_ordered)?;
                Ok(Type::Bool)
            }
            Op2::BwAnd(tag) | Op2::BwOr(tag) | Op2::BwXor(tag) => {
                self.homogeneous(tag, a, b, "an integral type", Type::is_integral)
            }
            Op2::BwShiftL { val, amount } | Op2::BwShiftR { val, amount } => {
                same_as_tag(self.name(), val, a)?;
                expect_ty(self.name(), 1, amount, b)?;
                expect_class(self.name(), 0, "an integral type", Type::is_integral, a)?;
                expect_class(self.name(), 1, "an integral type", Type::is_integral, b)?;
                Ok(val.clone())
            }
            Op2::Index(tag) => {
                same_as_tag(self.name(), tag, a)?;
                expect_ty(self.name(), 1, &Type::Word32, b)?;
                tag.elem().cloned().ok_or_else(|| Error::OperandClass {
                    op: self.name(),
                    position: 0,
                    expected: "an array type",
                    found: tag.clone(),
                })
            }
            Op2::UpdateField { struct_ty, field } => {
                same_as_tag(self.name(), struct_ty, a)?;
                let expected = field_ty(struct_ty, field)?;
                expect_ty(self.name(), 1, expected, b)?;
                Ok(struct_ty.clone())
            }
        }
    }

    /// Checks that both operands match the tag and belong to the given class,
    /// then returns the tag as the result type.
    fn homogeneous(
        &self,
        tag: &Type,
        a: &Type,
        b: &Type,
        class: &'static str,
        in_class: fn(&Type) -> bool,
    ) -> Result<Type> {
        same_as_tag(self.name(), tag, a)?;
        expect_ty(self.name(), 1, tag, b)?;
        expect_class(self.name(), 0, class, in_class, a)?;
        Ok(tag.clone())
    }
}

impl Op3 {
    /// Name as it appears in diagnostics.
    pub fn name(&self) -> &'static str {
        match self {
            Op3::Mux(_) => "mux",
            Op3::UpdateArray(_) => "update_array",
        }
    }

    /// Cost category.
    pub fn class(&self) -> OpClass {
        match self {
            Op3::Mux(_) => OpClass::Select,
            Op3::UpdateArray(_) => OpClass::ArrayUpdate,
        }
    }

    /// Result type for an application to `a`, `b`, and `c`.
    pub fn result_ty(&self, a: &Type, b: &Type, c: &Type) -> Result<Type> {
        match self {
            Op3::Mux(tag) => {
                expect_ty(self.name(), 0, &Type::Bool, a)?;
                expect_ty(self.name(), 1, tag, b)?;
                expect_ty(self.name(), 2, tag, c)?;
                Ok(tag.clone())
            }
            Op3::UpdateArray(tag) => {
                same_as_tag(self.name(), tag, a)?;
                expect_ty(self.name(), 1, &Type::Word32, b)?;
                let elem = tag.elem().ok_or_else(|| Error::OperandClass {
                    op: self.name(),
                    position: 0,
                    expected: "an array type",
                    found: tag.clone(),
                })?;
                expect_ty(self.name(), 2, elem, c)?;
                Ok(tag.clone())
            }
        }
    }
}

fn expect_ty(op: &'static str, position: usize, expected: &Type, found: &Type) -> Result<()> {
    if expected == found {
        Ok(())
    } else {
        Err(Error::OperandType {
            op,
            position,
            expected: expected.clone(),
            found: found.clone(),
        })
    }
}

fn expect_class(
    op: &'static str,
    position: usize,
    expected: &'static str,
    in_class: fn(&Type) -> bool,
    found: &Type,
) -> Result<()> {
    if in_class(found) {
        Ok(())
    } else {
        Err(Error::OperandClass {
            op,
            position,
            expected,
            found: found.clone(),
        })
    }
}

/// Checks an operator's type tag against the operand it describes.
///
/// Reported as [`Error::OpTag`] rather than a plain mismatch: a disagreement
/// here means the *node* is malformed, not that the user applied an operator to
/// the wrong argument.
fn same_as_tag(op: &'static str, tag: &Type, operand: &Type) -> Result<()> {
    if tag == operand {
        Ok(())
    } else {
        Err(Error::OpTag {
            op,
            tag: tag.clone(),
            operand: operand.clone(),
        })
    }
}

fn field_ty<'a>(struct_ty: &'a Type, field: &str) -> Result<&'a Type> {
    match struct_ty {
        Type::Struct(s) => struct_ty.field(field).ok_or_else(|| Error::UnknownField {
            struct_name: s.name.clone(),
            field: field.to_string(),
        }),
        other => Err(Error::OperandClass {
            op: "field access",
            position: 0,
            expected: "a struct type",
            found: other.clone(),
        }),
    }
}
