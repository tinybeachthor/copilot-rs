//! Semantics for the two partial operations.
//!
//! Array subscript and integer division are the only places an IR expression
//! could fail to denote a value. A hard-realtime monitor cannot trap, and the
//! interpreter, the code generators, and the SMT encoding have to agree on
//! exactly one answer, so both are given total semantics here rather than left
//! to whatever the target language does.

use crate::ty::Value;

/// What an array subscript does when the index is out of range.
///
/// Upstream Copilot's C backend emits an unchecked subscript, which is
/// undefined behaviour. Every option here is total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IndexPolicy {
    /// Wrap: `a[i % N]`. The default — constant time, no branch, no panic.
    #[default]
    Wrap,
    /// Clamp: `a[min(i, N - 1)]`. Costs a comparison, but keeps a
    /// nearly-in-range index near the element it was aiming at.
    Saturate,
    /// Treat in-range-ness as a proof obligation rather than defining
    /// behaviour outside it.
    ///
    /// Generated code subscripts directly and emits an assumption for the
    /// verifier to discharge; the interpreter reports
    /// [`crate::Error::IndexOutOfRange`] instead of silently agreeing with a
    /// monitor whose obligation was never discharged.
    Assume,
}

impl IndexPolicy {
    /// Resolves an index against an array length, or `None` when the policy is
    /// [`IndexPolicy::Assume`] and the index is out of range.
    pub fn resolve(self, index: u32, len: usize) -> Option<usize> {
        let index = index as usize;
        match self {
            IndexPolicy::Wrap => Some(index % len),
            IndexPolicy::Saturate => Some(index.min(len - 1)),
            IndexPolicy::Assume => (index < len).then_some(index),
        }
    }
}

/// Integer division and remainder, defined at zero.
///
/// `n / 0` and `n % 0` are both zero. C leaves them undefined and Rust panics;
/// neither is available to a monitor that must not trap and must behave the
/// same in debug and release. Zero is arbitrary but total, and a spec that
/// cares should carry a `Property` saying the divisor is non-zero and have the
/// prover discharge it.
///
/// Division also wraps, so `i64::MIN / -1` is `i64::MIN` rather than an
/// overflow trap.
pub fn div(numerator: &Value, denominator: &Value) -> Value {
    integer_binop(
        numerator,
        denominator,
        i128::wrapping_div,
        u128::wrapping_div,
    )
}

/// Integer remainder. See [`div`] for behaviour at zero.
pub fn rem(numerator: &Value, denominator: &Value) -> Value {
    integer_binop(
        numerator,
        denominator,
        i128::wrapping_rem,
        u128::wrapping_rem,
    )
}

/// Applies a division-like operation, short-circuiting a zero divisor to zero.
///
/// Widening to 128 bits keeps one implementation for all eight integer types;
/// every operand round-trips exactly, and the result is narrowed back by the
/// same constructor it came from.
fn integer_binop(
    numerator: &Value,
    denominator: &Value,
    signed: fn(i128, i128) -> i128,
    unsigned: fn(u128, u128) -> u128,
) -> Value {
    macro_rules! dispatch {
        ($($variant:ident($ty:ty) => $op:ident as $wide:ty),* $(,)?) => {
            match (numerator, denominator) {
                $(
                    (Value::$variant(n), Value::$variant(d)) => Value::$variant(if *d == 0 {
                        0
                    } else {
                        $op(*n as $wide, *d as $wide) as $ty
                    }),
                )*
                _ => panic!("copilot-core: division on non-integer or mismatched values"),
            }
        };
    }

    dispatch! {
        Int8(i8) => signed as i128,
        Int16(i16) => signed as i128,
        Int32(i32) => signed as i128,
        Int64(i64) => signed as i128,
        Word8(u8) => unsigned as u128,
        Word16(u16) => unsigned as u128,
        Word32(u32) => unsigned as u128,
        Word64(u64) => unsigned as u128,
    }
}
