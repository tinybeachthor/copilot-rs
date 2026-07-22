//! Statically typed handles on expressions in the builder's arena.

use crate::builder::Builder;
use crate::classes::{Bits, Equatable, Floating, Integral, Numeric, Ordered};
use copilot_core::{ExprId, Op1, Op2, Op3, Type, Typed, Value};
use std::marker::PhantomData;

/// A handle on an expression, typed by the Rust type it denotes.
///
/// This is where the static typing upstream Copilot gets from a GADT comes
/// back. The IR underneath is runtime-typed, but a `Stream<T>` can only be
/// combined with operators bounded by the marker traits in
/// [`crate::classes`], so a spec that compiles cannot be ill-typed.
///
/// It is [`Copy`], and copying it *is* sharing: two uses of the same handle
/// denote the same arena node. No sharing-recovery step is needed, which is the
/// machinery upstream needs `data-reify` for.
pub struct Stream<'a, T> {
    expr: ExprId,
    builder: &'a Builder,
    _phantom: PhantomData<T>,
}

impl<T> Clone for Stream<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Stream<'_, T> {}

impl<T> std::fmt::Debug for Stream<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Stream({})", self.expr)
    }
}

impl<'a, T> Stream<'a, T> {
    pub(crate) fn new(builder: &'a Builder, expr: ExprId) -> Self {
        Stream {
            expr,
            builder,
            _phantom: PhantomData,
        }
    }

    /// The underlying expression. Used by [`args!`](crate::args).
    pub fn expr(&self) -> ExprId {
        self.expr
    }

    pub(crate) fn builder(&self) -> &'a Builder {
        self.builder
    }
}

impl<'a, T: Typed> Stream<'a, T> {
    /// This stream shifted `n` steps forward in time: at time `t` it denotes
    /// the original at `t + n`.
    ///
    /// Reading ahead is only possible as far as a stream's buffer reaches, so
    /// `n` is bounded by the number of initial values. Shifting an external
    /// variable is an error at any depth — the environment has not produced its
    /// next sample yet.
    ///
    /// ```
    /// # use copilot_lang::Builder;
    /// let b = Builder::new();
    /// let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
    /// # b.finish().unwrap();
    /// ```
    pub fn drop(self, n: u32) -> Stream<'a, T> {
        match self.builder.shift(self.expr, n) {
            Ok(expr) => Stream::new(self.builder, expr),
            Err(e) => self.builder.poisoned(e),
        }
    }

    /// Annotates the expression with a name, for generated code to carry as a
    /// comment. Semantically the identity.
    pub fn label(self, name: &str) -> Stream<'a, T> {
        self.unary_raw(|arena, e| Ok(arena.label(name, e)))
    }

    /// Applies a unary operator.
    pub(crate) fn unary<U: Typed>(self, op: Op1) -> Stream<'a, U> {
        self.unary_raw(move |arena, e| arena.op1(op, e))
    }

    fn unary_raw<U>(
        self,
        f: impl FnOnce(&mut copilot_core::Arena, ExprId) -> copilot_core::Result<ExprId>,
    ) -> Stream<'a, U> {
        match self.builder.build(|arena| f(arena, self.expr)) {
            Ok(expr) => Stream::new(self.builder, expr),
            Err(e) => self.builder.poisoned(e),
        }
    }

    /// Projects a field out of a struct-typed stream.
    ///
    /// The field name is not checked by the compiler, so prefer the accessors
    /// that `#[derive(CopilotStruct)]` generates; this is what they are built
    /// from. A name that does not exist, or a `U` that is not its type, is
    /// reported by [`Builder::finish`](crate::Builder::finish).
    pub fn field<U: Typed>(self, name: &str) -> Stream<'a, U> {
        self.unary(Op1::GetField {
            struct_ty: T::ty(),
            field: name.to_string(),
        })
    }

    /// The struct with one field replaced.
    ///
    /// Copies the whole struct; [`copilot_core::cost`] reports the bytes moved.
    /// As with [`Stream::field`], prefer the generated accessors.
    pub fn with_field<U: Typed>(self, name: &str, value: Stream<'a, U>) -> Stream<'a, T> {
        self.binary(
            Op2::UpdateField {
                struct_ty: T::ty(),
                field: name.to_string(),
            },
            value.expr(),
        )
    }

    /// Applies a binary operator.
    pub(crate) fn binary<U: Typed>(self, op: Op2, rhs: ExprId) -> Stream<'a, U> {
        match self.builder.build(|arena| arena.op2(op, self.expr, rhs)) {
            Ok(expr) => Stream::new(self.builder, expr),
            Err(e) => self.builder.poisoned(e),
        }
    }
}

/// Comparison. These cannot be `PartialOrd`/`PartialEq` impls: those return
/// `bool`, and a comparison of two streams is itself a stream.
impl<'a, T: Typed + Equatable> Stream<'a, T> {
    /// Equality, as a boolean stream.
    pub fn eq_(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Eq(T::ty()), rhs.expr)
    }

    /// Inequality, as a boolean stream.
    pub fn ne_(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Ne(T::ty()), rhs.expr)
    }
}

impl<'a, T: Typed + Ordered> Stream<'a, T> {
    /// Strictly less than.
    pub fn lt(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Lt(T::ty()), rhs.expr)
    }

    /// Less than or equal.
    pub fn le(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Le(T::ty()), rhs.expr)
    }

    /// Strictly greater than.
    pub fn gt(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Gt(T::ty()), rhs.expr)
    }

    /// Greater than or equal.
    pub fn ge(self, rhs: Stream<'a, T>) -> Stream<'a, bool> {
        self.binary(Op2::Ge(T::ty()), rhs.expr)
    }
}

impl<'a> Stream<'a, bool> {
    /// Conjunction. Same as `&`.
    pub fn and(self, rhs: Stream<'a, bool>) -> Stream<'a, bool> {
        self.binary(Op2::And, rhs.expr)
    }

    /// Disjunction. Same as `|`.
    pub fn or(self, rhs: Stream<'a, bool>) -> Stream<'a, bool> {
        self.binary(Op2::Or, rhs.expr)
    }

    /// Implication: `!self | rhs`.
    pub fn implies(self, rhs: Stream<'a, bool>) -> Stream<'a, bool> {
        let negated: Stream<'a, bool> = self.unary(Op1::Not);
        negated.or(rhs)
    }

    /// Branchless selection between two streams.
    ///
    /// Both branches are evaluated, which is what keeps a step's cost
    /// independent of its data.
    pub fn mux<T: Typed>(self, on_true: Stream<'a, T>, on_false: Stream<'a, T>) -> Stream<'a, T> {
        match self
            .builder
            .build(|arena| arena.op3(Op3::Mux(T::ty()), self.expr, on_true.expr, on_false.expr))
        {
            Ok(expr) => Stream::new(self.builder, expr),
            Err(e) => self.builder.poisoned(e),
        }
    }
}

impl<'a, T: Typed + Numeric> Stream<'a, T> {
    /// Absolute value.
    pub fn abs(self) -> Stream<'a, T> {
        self.unary(Op1::Abs(T::ty()))
    }

    /// Sign, as -1, 0, or 1.
    pub fn signum(self) -> Stream<'a, T> {
        self.unary(Op1::Sign(T::ty()))
    }
}

impl<'a, T: Typed + Integral> Stream<'a, T> {
    /// Converts to another numeric type, with `as` semantics.
    pub fn cast<U: Typed + Numeric>(self) -> Stream<'a, U> {
        self.unary(Op1::Cast {
            from: T::ty(),
            to: U::ty(),
        })
    }
}

macro_rules! float_unary {
    ($($method:ident => $op:ident, $doc:literal;)*) => {
        $(
            #[doc = $doc]
            pub fn $method(self) -> Stream<'a, T> {
                self.unary(Op1::$op(T::ty()))
            }
        )*
    };
}

impl<'a, T: Typed + Floating> Stream<'a, T> {
    float_unary! {
        recip => Recip, "Reciprocal.";
        exp => Exp, "`e` raised to this power.";
        sqrt => Sqrt, "Square root.";
        ln => Log, "Natural logarithm.";
        sin => Sin, "Sine.";
        cos => Cos, "Cosine.";
        tan => Tan, "Tangent.";
        asin => Asin, "Arcsine.";
        acos => Acos, "Arccosine.";
        atan => Atan, "Arctangent.";
        sinh => Sinh, "Hyperbolic sine.";
        cosh => Cosh, "Hyperbolic cosine.";
        tanh => Tanh, "Hyperbolic tangent.";
        asinh => Asinh, "Inverse hyperbolic sine.";
        acosh => Acosh, "Inverse hyperbolic cosine.";
        atanh => Atanh, "Inverse hyperbolic tangent.";
        ceil => Ceiling, "Rounds towards positive infinity.";
        floor => Floor, "Rounds towards negative infinity.";
    }

    /// Raises this stream to the power of another.
    pub fn powf(self, exponent: Stream<'a, T>) -> Stream<'a, T> {
        self.binary(Op2::Pow(T::ty()), exponent.expr)
    }

    /// Logarithm of this stream in the given base.
    pub fn log(self, base: Stream<'a, T>) -> Stream<'a, T> {
        self.binary(Op2::Logb(T::ty()), base.expr)
    }

    /// Four-quadrant arctangent of `self / x`.
    pub fn atan2(self, x: Stream<'a, T>) -> Stream<'a, T> {
        self.binary(Op2::Atan2(T::ty()), x.expr)
    }
}

impl<'a, T: Typed + Bits> Stream<'a, T> {
    /// Shifts left by `amount`. Same as `<<`.
    pub fn shift_left<U: Typed + Integral>(self, amount: Stream<'a, U>) -> Stream<'a, T> {
        self.binary(
            Op2::BwShiftL {
                val: T::ty(),
                amount: U::ty(),
            },
            amount.expr,
        )
    }

    /// Shifts right by `amount`. Same as `>>`.
    pub fn shift_right<U: Typed + Integral>(self, amount: Stream<'a, U>) -> Stream<'a, T> {
        self.binary(
            Op2::BwShiftR {
                val: T::ty(),
                amount: U::ty(),
            },
            amount.expr,
        )
    }
}

impl<'a, T: Typed, const N: usize> Stream<'a, [T; N]>
where
    [T; N]: Typed,
{
    /// Reads the element at `index`.
    ///
    /// An out-of-range index follows the backend's
    /// [`IndexPolicy`](copilot_core::IndexPolicy), which defaults to wrapping.
    pub fn index(self, index: Stream<'a, u32>) -> Stream<'a, T> {
        self.binary(Op2::Index(<[T; N]>::ty()), index.expr)
    }

    /// The array with the element at `index` replaced.
    ///
    /// Copies the whole array; [`copilot_core::cost`] reports the bytes moved.
    pub fn update(self, index: Stream<'a, u32>, value: Stream<'a, T>) -> Stream<'a, [T; N]> {
        match self.builder.build(|arena| {
            arena.op3(
                Op3::UpdateArray(<[T; N]>::ty()),
                self.expr,
                index.expr,
                value.expr,
            )
        }) {
            Ok(expr) => Stream::new(self.builder, expr),
            Err(e) => self.builder.poisoned(e),
        }
    }
}

/// The additive identity of a numeric type, for negation.
pub(crate) fn zero(ty: &Type) -> Value {
    match ty {
        Type::Int8 => Value::Int8(0),
        Type::Int16 => Value::Int16(0),
        Type::Int32 => Value::Int32(0),
        Type::Int64 => Value::Int64(0),
        Type::Word8 => Value::Word8(0),
        Type::Word16 => Value::Word16(0),
        Type::Word32 => Value::Word32(0),
        Type::Word64 => Value::Word64(0),
        Type::Float => Value::Float(0.0),
        Type::Double => Value::Double(0.0),
        other => panic!("copilot-lang: {other} is not numeric, so it has no zero"),
    }
}
