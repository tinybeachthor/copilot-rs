//! Rust operator syntax over streams.
//!
//! `a + b` has nowhere to return a `Result`, which is why the builder defers
//! errors. It never has to here: every impl below is bounded by a marker trait
//! from [`crate::classes`], so the operator it selects is well-typed for the
//! operand type by construction.
//!
//! Three operators mean different things at different types, and the impl
//! chooses on the operand's IR type rather than exposing two methods:
//!
//! - `/` is integer division on integers and floating division on floats.
//! - `!` is logical negation on `bool` and bitwise complement on integers.
//! - `&`, `|`, `^` are the logical connectives on `bool` — with `^` as
//!   inequality — and bitwise on integers.

use crate::classes::{Bits, Floating, Integral, Numeric};
use crate::stream::{Stream, zero};
use copilot_core::{Op1, Op2, Type, Typed};
use std::ops::{Add, BitAnd, BitOr, BitXor, Div, Mul, Neg, Not, Rem, Shl, Shr, Sub};

macro_rules! binary_op {
    ($trait:ident :: $method:ident, $bound:ident, $select:expr) => {
        impl<'a, T: Typed + $bound> $trait for Stream<'a, T> {
            type Output = Stream<'a, T>;

            fn $method(self, rhs: Stream<'a, T>) -> Stream<'a, T> {
                let select: fn(Type) -> Op2 = $select;
                self.binary(select(T::ty()), rhs.expr())
            }
        }
    };
}

binary_op!(Add::add, Numeric, |ty| Op2::Add(ty));
binary_op!(Sub::sub, Numeric, |ty| Op2::Sub(ty));
binary_op!(Mul::mul, Numeric, |ty| Op2::Mul(ty));
binary_op!(Rem::rem, Integral, |ty| Op2::Mod(ty));

/// Integer division on integers, floating division on floats.
///
/// Division by zero is zero rather than a trap; see [`copilot_core::div`].
impl<'a, T: Typed + Numeric> Div for Stream<'a, T> {
    type Output = Stream<'a, T>;

    fn div(self, rhs: Stream<'a, T>) -> Stream<'a, T> {
        let ty = T::ty();
        let op = if ty.is_floating() {
            Op2::Fdiv(ty)
        } else {
            Op2::Div(ty)
        };
        self.binary(op, rhs.expr())
    }
}

impl<'a, T: Typed + Numeric> Neg for Stream<'a, T> {
    type Output = Stream<'a, T>;

    /// Negation is `0 - self`: the IR has no negation operator, matching
    /// upstream, where `negate` comes from `Num` rather than from the operator
    /// GADT. On unsigned types it wraps.
    fn neg(self) -> Stream<'a, T> {
        let ty = T::ty();
        let origin = match self
            .builder()
            .build(|arena| arena.constant(ty.clone(), zero(&ty)))
        {
            Ok(expr) => Stream::<'a, T>::new(self.builder(), expr),
            Err(e) => return self.builder().poisoned(e),
        };
        origin.binary(Op2::Sub(ty), self.expr())
    }
}

impl<'a, T: Typed + Bits> Not for Stream<'a, T> {
    type Output = Stream<'a, T>;

    fn not(self) -> Stream<'a, T> {
        let ty = T::ty();
        let op = if ty == Type::Bool {
            Op1::Not
        } else {
            Op1::BwNot(ty)
        };
        self.unary(op)
    }
}

macro_rules! bitwise_op {
    ($trait:ident :: $method:ident, $on_bool:expr, $on_bits:expr) => {
        impl<'a, T: Typed + Bits> $trait for Stream<'a, T> {
            type Output = Stream<'a, T>;

            fn $method(self, rhs: Stream<'a, T>) -> Stream<'a, T> {
                let ty = T::ty();
                let on_bool: fn() -> Op2 = $on_bool;
                let on_bits: fn(Type) -> Op2 = $on_bits;
                let op = if ty == Type::Bool {
                    on_bool()
                } else {
                    on_bits(ty)
                };
                self.binary(op, rhs.expr())
            }
        }
    };
}

bitwise_op!(BitAnd::bitand, || Op2::And, |ty| Op2::BwAnd(ty));
bitwise_op!(BitOr::bitor, || Op2::Or, |ty| Op2::BwOr(ty));
// Exclusive or on booleans is inequality.
bitwise_op!(BitXor::bitxor, || Op2::Ne(Type::Bool), |ty| Op2::BwXor(ty));

impl<'a, T: Typed + Bits, U: Typed + Integral> Shl<Stream<'a, U>> for Stream<'a, T> {
    type Output = Stream<'a, T>;

    fn shl(self, amount: Stream<'a, U>) -> Stream<'a, T> {
        self.shift_left(amount)
    }
}

impl<'a, T: Typed + Bits, U: Typed + Integral> Shr<Stream<'a, U>> for Stream<'a, T> {
    type Output = Stream<'a, T>;

    fn shr(self, amount: Stream<'a, U>) -> Stream<'a, T> {
        self.shift_right(amount)
    }
}

/// Operators against plain Rust values, so a spec can say `s + 1` rather than
/// naming a literal first.
///
/// Written out per concrete type rather than as a blanket
/// `impl<T: Numeric> Add<T> for Stream<'_, T>`: coherence cannot rule out a
/// future `impl Numeric for Stream<..>`, so the blanket form is rejected as
/// possibly overlapping the stream-on-stream impls. Naming the types settles
/// it, and it buys the reversed direction — `1 + s` — which no blanket impl
/// could provide.
macro_rules! scalar_ops {
    ($($ty:ty),* $(,)?) => {
        $(
            scalar_op!($ty, Add::add);
            scalar_op!($ty, Sub::sub);
            scalar_op!($ty, Mul::mul);
            scalar_op!($ty, Div::div);
        )*
    };
}

macro_rules! scalar_op {
    ($ty:ty, $trait:ident :: $method:ident) => {
        impl<'a> $trait<$ty> for Stream<'a, $ty> {
            type Output = Stream<'a, $ty>;

            fn $method(self, rhs: $ty) -> Stream<'a, $ty> {
                let rhs = self.builder().lit(rhs);
                $trait::$method(self, rhs)
            }
        }

        impl<'a> $trait<Stream<'a, $ty>> for $ty {
            type Output = Stream<'a, $ty>;

            fn $method(self, rhs: Stream<'a, $ty>) -> Stream<'a, $ty> {
                let lhs = rhs.builder().lit(self);
                $trait::$method(lhs, rhs)
            }
        }
    };
}

scalar_ops!(i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);

macro_rules! scalar_rem {
    ($($ty:ty),* $(,)?) => {
        $( scalar_op!($ty, Rem::rem); )*
    };
}

scalar_rem!(i8, i16, i32, i64, u8, u16, u32, u64);

/// Comparisons against plain Rust values.
macro_rules! scalar_compare {
    ($($ty:ty),* $(,)?) => {
        $(
            impl<'a> Stream<'a, $ty> {
                /// Equality against a constant.
                pub fn eq_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.eq_(rhs)
                }

                /// Inequality against a constant.
                pub fn ne_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.ne_(rhs)
                }

                /// Strictly less than a constant.
                pub fn lt_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.lt(rhs)
                }

                /// Less than or equal to a constant.
                pub fn le_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.le(rhs)
                }

                /// Strictly greater than a constant.
                pub fn gt_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.gt(rhs)
                }

                /// Greater than or equal to a constant.
                pub fn ge_val(self, rhs: $ty) -> Stream<'a, bool> {
                    let rhs = self.builder().lit(rhs);
                    self.ge(rhs)
                }
            }
        )*
    };
}

scalar_compare!(i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);

impl<'a, T: Typed + Floating> Stream<'a, T> {
    /// Raises this stream to a constant power.
    pub fn powi(self, exponent: T) -> Stream<'a, T> {
        let exponent = self.builder().lit(exponent);
        self.powf(exponent)
    }
}
