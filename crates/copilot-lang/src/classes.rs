//! Marker traits restricting operators to the types that admit them.
//!
//! These stand in for the Haskell class constraints on upstream's operator
//! GADT — `Num a`, `Integral a`, `Floating a`, `Bits a`, `Ord a`. Because every
//! operator the frontend offers is bounded by one of them, a spec that compiles
//! is well-typed: [`copilot_core::typecheck`] can only fail on IR that did not
//! come from here.

use copilot_core::Typed;

/// Types admitting `+`, `-`, `*`, and negation.
pub trait Numeric: Typed {}

/// Types admitting integer division, remainder, and casts from.
pub trait Integral: Numeric {}

/// Types admitting `/` and the transcendental functions.
pub trait Floating: Numeric {}

/// Types admitting bitwise operators.
///
/// Includes `bool`, where they mean the logical connectives — `&` on booleans
/// is conjunction, `^` is inequality.
pub trait Bits: Typed {}

/// Types admitting `<`, `<=`, `>`, `>=`.
pub trait Ordered: Typed {}

/// Types admitting `==` and `!=`.
///
/// Scalars only. Aggregate comparison is excluded from the IR; see
/// `docs/deviations.md`.
pub trait Equatable: Typed {}

macro_rules! classify {
    ($trait:ident: $($ty:ty),* $(,)?) => {
        $( impl $trait for $ty {} )*
    };
}

classify!(Numeric: i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);
classify!(Integral: i8, i16, i32, i64, u8, u16, u32, u64);
classify!(Floating: f32, f64);
classify!(Bits: bool, i8, i16, i32, i64, u8, u16, u32, u64);
classify!(Ordered: bool, i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);
classify!(Equatable: bool, i8, i16, i32, i64, u8, u16, u32, u64, f32, f64);
