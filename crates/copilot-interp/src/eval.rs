//! Expression evaluation.

use copilot_core::{
    Arena, Error, ExprId, IndexPolicy, Node, Op1, Op2, Op3, Result, Type, Value, VarId,
};
use std::cmp::Ordering;
use std::collections::HashMap;

/// What an expression can read: the current buffer contents and this step's
/// external samples.
pub(crate) struct Context<'a> {
    pub arena: &'a Arena,
    /// Ring buffers, indexed by stream.
    pub buffers: &'a [Vec<Value>],
    /// Rotating read positions, indexed by stream.
    pub positions: &'a [usize],
    /// External samples taken once at the start of the step.
    pub samples: &'a HashMap<String, Value>,
    pub index_policy: IndexPolicy,
}

impl Context<'_> {
    /// Evaluates an expression against the current state.
    pub fn eval(&self, expr: ExprId) -> Result<Value> {
        self.eval_with(expr, &mut HashMap::new())
    }

    fn eval_with(&self, expr: ExprId, locals: &mut HashMap<VarId, Value>) -> Result<Value> {
        match self.arena.node(expr) {
            Node::Const { value, .. } => Ok(value.clone()),

            // The ring-buffer invariant, and the only place state is read:
            // slot `(position + idx) % len` holds the value `idx` steps ahead.
            Node::Drop { idx, stream } => {
                let buffer = &self.buffers[stream.index()];
                let slot = (self.positions[stream.index()] + *idx as usize) % buffer.len();
                Ok(buffer[slot].clone())
            }

            Node::ExternVar { name, ty } => {
                self.samples
                    .get(name)
                    .cloned()
                    .ok_or_else(|| Error::Mismatch {
                        context: format!("external variable `{name}` was not sampled this step"),
                        expected: ty.clone(),
                        found: ty.clone(),
                    })
            }

            Node::Var(var) => locals.get(var).cloned().ok_or(Error::UnknownVar(*var)),

            Node::Local { var, bound, body } => {
                let value = self.eval_with(*bound, locals)?;
                let shadowed = locals.insert(*var, value);
                let result = self.eval_with(*body, locals);
                match shadowed {
                    Some(previous) => locals.insert(*var, previous),
                    None => locals.remove(var),
                };
                result
            }

            Node::Op1(op, a) => {
                let a = self.eval_with(*a, locals)?;
                apply1(op, &a)
            }

            Node::Op2(op, a, b) => {
                let a = self.eval_with(*a, locals)?;
                let b = self.eval_with(*b, locals)?;
                apply2(op, &a, &b, self.index_policy)
            }

            Node::Op3(op, a, b, c) => {
                let a = self.eval_with(*a, locals)?;
                let b = self.eval_with(*b, locals)?;
                let c = self.eval_with(*c, locals)?;
                apply3(op, &a, &b, &c, self.index_policy)
            }

            Node::Label(_, a) => self.eval_with(*a, locals),
        }
    }
}

macro_rules! integral {
    ($value:expr, |$x:ident| $body:expr) => {
        match $value {
            Value::Int8($x) => Value::Int8($body),
            Value::Int16($x) => Value::Int16($body),
            Value::Int32($x) => Value::Int32($body),
            Value::Int64($x) => Value::Int64($body),
            Value::Word8($x) => Value::Word8($body),
            Value::Word16($x) => Value::Word16($body),
            Value::Word32($x) => Value::Word32($body),
            Value::Word64($x) => Value::Word64($body),
            other => return Err(unsupported(other)),
        }
    };
}

macro_rules! floating {
    ($value:expr, |$x:ident| $body:expr) => {
        match $value {
            Value::Float($x) => Value::Float($body),
            Value::Double($x) => Value::Double($body),
            other => return Err(unsupported(other)),
        }
    };
}

/// Applies a maths-library function at the operand's own width.
///
/// These go through `libm` rather than the standard library, so that the
/// interpreter computes exactly what a generated `no_std` monitor computes.
/// `std`'s transcendentals are the host platform's, which differ between
/// implementations in the last place and would make the reference
/// implementation's answers depend on the machine it ran on. Routing both
/// through one library removes the discrepancy rather than documenting it.
///
/// The single-precision entry points are used for `Float`, keeping the
/// evaluation width equal to the operands' — see `docs/deviations.md`.
macro_rules! transcendental {
    ($value:expr, $single:path, $double:path) => {
        match $value {
            Value::Float(x) => Value::Float($single(*x)),
            Value::Double(x) => Value::Double($double(*x)),
            other => return Err(unsupported(other)),
        }
    };
}

fn apply1(op: &Op1, a: &Value) -> Result<Value> {
    Ok(match op {
        Op1::Not => match a {
            Value::Bool(x) => Value::Bool(!x),
            other => return Err(unsupported(other)),
        },
        Op1::Abs(_) => match a {
            // Wrapping keeps `abs` total: `i8::MIN.abs()` is `i8::MIN`, matching
            // the arithmetic everywhere else rather than trapping.
            Value::Int8(x) => Value::Int8(x.wrapping_abs()),
            Value::Int16(x) => Value::Int16(x.wrapping_abs()),
            Value::Int32(x) => Value::Int32(x.wrapping_abs()),
            Value::Int64(x) => Value::Int64(x.wrapping_abs()),
            Value::Word8(_) | Value::Word16(_) | Value::Word32(_) | Value::Word64(_) => a.clone(),
            Value::Float(x) => Value::Float(x.abs()),
            Value::Double(x) => Value::Double(x.abs()),
            other => return Err(unsupported(other)),
        },
        // The unsigned types have no `signum`; there, sign is just "non-zero".
        Op1::Sign(_) => match a {
            Value::Int8(x) => Value::Int8(x.signum()),
            Value::Int16(x) => Value::Int16(x.signum()),
            Value::Int32(x) => Value::Int32(x.signum()),
            Value::Int64(x) => Value::Int64(x.signum()),
            Value::Word8(x) => Value::Word8((*x != 0).into()),
            Value::Word16(x) => Value::Word16((*x != 0).into()),
            Value::Word32(x) => Value::Word32((*x != 0).into()),
            Value::Word64(x) => Value::Word64((*x != 0).into()),
            Value::Float(x) => Value::Float(x.signum()),
            Value::Double(x) => Value::Double(x.signum()),
            other => return Err(unsupported(other)),
        },
        Op1::Recip(_) => floating!(a, |x| x.recip()),
        Op1::Exp(_) => transcendental!(a, libm::expf, libm::exp),
        Op1::Sqrt(_) => transcendental!(a, libm::sqrtf, libm::sqrt),
        Op1::Log(_) => transcendental!(a, libm::logf, libm::log),
        Op1::Sin(_) => transcendental!(a, libm::sinf, libm::sin),
        Op1::Cos(_) => transcendental!(a, libm::cosf, libm::cos),
        Op1::Tan(_) => transcendental!(a, libm::tanf, libm::tan),
        Op1::Asin(_) => transcendental!(a, libm::asinf, libm::asin),
        Op1::Acos(_) => transcendental!(a, libm::acosf, libm::acos),
        Op1::Atan(_) => transcendental!(a, libm::atanf, libm::atan),
        Op1::Sinh(_) => transcendental!(a, libm::sinhf, libm::sinh),
        Op1::Cosh(_) => transcendental!(a, libm::coshf, libm::cosh),
        Op1::Tanh(_) => transcendental!(a, libm::tanhf, libm::tanh),
        Op1::Asinh(_) => transcendental!(a, libm::asinhf, libm::asinh),
        Op1::Acosh(_) => transcendental!(a, libm::acoshf, libm::acosh),
        Op1::Atanh(_) => transcendental!(a, libm::atanhf, libm::atanh),
        Op1::Ceiling(_) => transcendental!(a, libm::ceilf, libm::ceil),
        Op1::Floor(_) => transcendental!(a, libm::floorf, libm::floor),
        Op1::BwNot(_) => integral!(a, |x| !x),
        Op1::Cast { to, .. } => cast(a, to)?,
        Op1::GetField { field, .. } => match a {
            Value::Struct { name, fields } => fields
                .iter()
                .find(|(n, _)| n == field)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| Error::UnknownField {
                    struct_name: name.clone(),
                    field: field.clone(),
                })?,
            other => return Err(unsupported(other)),
        },
    })
}

fn apply2(op: &Op2, a: &Value, b: &Value, policy: IndexPolicy) -> Result<Value> {
    Ok(match op {
        Op2::And => Value::Bool(as_bool(a)? && as_bool(b)?),
        Op2::Or => Value::Bool(as_bool(a)? || as_bool(b)?),

        Op2::Add(_) => arith(a, b, Arith::ADD)?,
        Op2::Sub(_) => arith(a, b, Arith::SUB)?,
        Op2::Mul(_) => arith(a, b, Arith::MUL)?,

        Op2::Div(_) => copilot_core::div(a, b),
        Op2::Mod(_) => copilot_core::rem(a, b),
        Op2::Fdiv(_) => match (a, b) {
            (Value::Float(x), Value::Float(y)) => Value::Float(x / y),
            (Value::Double(x), Value::Double(y)) => Value::Double(x / y),
            _ => return Err(unsupported(a)),
        },
        Op2::Pow(_) => match (a, b) {
            (Value::Float(x), Value::Float(y)) => Value::Float(libm::powf(*x, *y)),
            (Value::Double(x), Value::Double(y)) => Value::Double(libm::pow(*x, *y)),
            _ => return Err(unsupported(a)),
        },
        // `log(x, base)` is `ln x / ln base`, which is how the standard library
        // defines it and what the code generators emit.
        Op2::Logb(_) => match (a, b) {
            (Value::Float(x), Value::Float(base)) => {
                Value::Float(libm::logf(*x) / libm::logf(*base))
            }
            (Value::Double(x), Value::Double(base)) => {
                Value::Double(libm::log(*x) / libm::log(*base))
            }
            _ => return Err(unsupported(a)),
        },
        Op2::Atan2(_) => match (a, b) {
            (Value::Float(y), Value::Float(x)) => Value::Float(libm::atan2f(*y, *x)),
            (Value::Double(y), Value::Double(x)) => Value::Double(libm::atan2(*y, *x)),
            _ => return Err(unsupported(a)),
        },

        // Equality is by value, so `Float(NaN) == Float(NaN)` would hold under
        // the bitwise `PartialEq` the IR uses for interning. Route it through
        // the numeric comparison instead, where NaN is unordered.
        Op2::Eq(_) => Value::Bool(compare(a, b)? == Some(Ordering::Equal)),
        Op2::Ne(_) => Value::Bool(compare(a, b)? != Some(Ordering::Equal)),
        Op2::Lt(_) => Value::Bool(compare(a, b)? == Some(Ordering::Less)),
        Op2::Gt(_) => Value::Bool(compare(a, b)? == Some(Ordering::Greater)),
        Op2::Le(_) => Value::Bool(matches!(
            compare(a, b)?,
            Some(Ordering::Less | Ordering::Equal)
        )),
        Op2::Ge(_) => Value::Bool(matches!(
            compare(a, b)?,
            Some(Ordering::Greater | Ordering::Equal)
        )),

        Op2::BwAnd(_) => bitwise(a, b, |x, y| x & y, |x, y| x & y)?,
        Op2::BwOr(_) => bitwise(a, b, |x, y| x | y, |x, y| x | y)?,
        Op2::BwXor(_) => bitwise(a, b, |x, y| x ^ y, |x, y| x ^ y)?,

        // Shifting by at least the operand's width yields zero rather than
        // wrapping the shift amount, which is what `wrapping_shl` would do.
        Op2::BwShiftL { .. } => shift(a, b, true)?,
        Op2::BwShiftR { .. } => shift(a, b, false)?,

        Op2::Index(_) => match (a, b) {
            (Value::Array(values), Value::Word32(i)) => {
                let slot = policy
                    .resolve(*i, values.len())
                    .ok_or(Error::IndexOutOfRange {
                        index: *i,
                        len: values.len(),
                    })?;
                values[slot].clone()
            }
            _ => return Err(unsupported(a)),
        },

        Op2::UpdateField { field, .. } => match a {
            Value::Struct { name, fields } => {
                let mut fields = fields.clone();
                let slot = fields.iter_mut().find(|(n, _)| n == field).ok_or_else(|| {
                    Error::UnknownField {
                        struct_name: name.clone(),
                        field: field.clone(),
                    }
                })?;
                slot.1 = b.clone();
                Value::Struct {
                    name: name.clone(),
                    fields,
                }
            }
            other => return Err(unsupported(other)),
        },
    })
}

fn apply3(op: &Op3, a: &Value, b: &Value, c: &Value, policy: IndexPolicy) -> Result<Value> {
    Ok(match op {
        // Both branches are already evaluated; selection is what makes a step's
        // cost independent of its data.
        Op3::Mux(_) => {
            if as_bool(a)? {
                b.clone()
            } else {
                c.clone()
            }
        }
        Op3::UpdateArray(_) => match (a, b) {
            (Value::Array(values), Value::Word32(i)) => {
                let mut values = values.clone();
                let slot = policy
                    .resolve(*i, values.len())
                    .ok_or(Error::IndexOutOfRange {
                        index: *i,
                        len: values.len(),
                    })?;
                values[slot] = c.clone();
                Value::Array(values)
            }
            _ => return Err(unsupported(a)),
        },
    })
}

/// The three wrapping arithmetic operations, at every type they apply to.
///
/// `f32` carries its own operation rather than borrowing the `f64` one. For
/// `+`, `-`, `*` and `/` it would in fact make no difference — double rounding
/// through a wider format is innocuous once the intermediate has `2p + 2` bits,
/// and `f64`'s 53 clears the 50 that `f32` needs — but that is a theorem about
/// these four operations, not a property of the type. It fails for the
/// transcendentals: routing `f32::exp` through `f64` changes the result for
/// roughly one argument in two thousand. Evaluating every operation at its
/// operands' own width means never having to know which case a given operator
/// falls into.
struct Arith {
    signed: fn(i128, i128) -> i128,
    unsigned: fn(u128, u128) -> u128,
    f32: fn(f32, f32) -> f32,
    f64: fn(f64, f64) -> f64,
}

impl Arith {
    const ADD: Arith = Arith {
        signed: i128::wrapping_add,
        unsigned: u128::wrapping_add,
        f32: |x, y| x + y,
        f64: |x, y| x + y,
    };
    const SUB: Arith = Arith {
        signed: i128::wrapping_sub,
        unsigned: u128::wrapping_sub,
        f32: |x, y| x - y,
        f64: |x, y| x - y,
    };
    const MUL: Arith = Arith {
        signed: i128::wrapping_mul,
        unsigned: u128::wrapping_mul,
        f32: |x, y| x * y,
        f64: |x, y| x * y,
    };
}

/// Applies an arithmetic operation, widening integers to 128 bits so that one
/// implementation covers all eight widths, and narrowing back by the same
/// constructor the operands came from.
fn arith(a: &Value, b: &Value, op: Arith) -> Result<Value> {
    let Arith {
        signed,
        unsigned,
        f32,
        f64,
    } = op;
    Ok(match (a, b) {
        (Value::Int8(x), Value::Int8(y)) => Value::Int8(signed(*x as i128, *y as i128) as i8),
        (Value::Int16(x), Value::Int16(y)) => Value::Int16(signed(*x as i128, *y as i128) as i16),
        (Value::Int32(x), Value::Int32(y)) => Value::Int32(signed(*x as i128, *y as i128) as i32),
        (Value::Int64(x), Value::Int64(y)) => Value::Int64(signed(*x as i128, *y as i128) as i64),
        (Value::Word8(x), Value::Word8(y)) => Value::Word8(unsigned(*x as u128, *y as u128) as u8),
        (Value::Word16(x), Value::Word16(y)) => {
            Value::Word16(unsigned(*x as u128, *y as u128) as u16)
        }
        (Value::Word32(x), Value::Word32(y)) => {
            Value::Word32(unsigned(*x as u128, *y as u128) as u32)
        }
        (Value::Word64(x), Value::Word64(y)) => {
            Value::Word64(unsigned(*x as u128, *y as u128) as u64)
        }
        (Value::Float(x), Value::Float(y)) => Value::Float(f32(*x, *y)),
        (Value::Double(x), Value::Double(y)) => Value::Double(f64(*x, *y)),
        _ => return Err(unsupported(a)),
    })
}

fn bitwise(
    a: &Value,
    b: &Value,
    signed: fn(i64, i64) -> i64,
    unsigned: fn(u64, u64) -> u64,
) -> Result<Value> {
    Ok(match (a, b) {
        (Value::Int8(x), Value::Int8(y)) => Value::Int8(signed(*x as i64, *y as i64) as i8),
        (Value::Int16(x), Value::Int16(y)) => Value::Int16(signed(*x as i64, *y as i64) as i16),
        (Value::Int32(x), Value::Int32(y)) => Value::Int32(signed(*x as i64, *y as i64) as i32),
        (Value::Int64(x), Value::Int64(y)) => Value::Int64(signed(*x, *y)),
        (Value::Word8(x), Value::Word8(y)) => Value::Word8(unsigned(*x as u64, *y as u64) as u8),
        (Value::Word16(x), Value::Word16(y)) => {
            Value::Word16(unsigned(*x as u64, *y as u64) as u16)
        }
        (Value::Word32(x), Value::Word32(y)) => {
            Value::Word32(unsigned(*x as u64, *y as u64) as u32)
        }
        (Value::Word64(x), Value::Word64(y)) => Value::Word64(unsigned(*x, *y)),
        _ => return Err(unsupported(a)),
    })
}

/// Shifts `a` by `b`, yielding zero once the amount reaches the operand width.
///
/// Rust's `<<` panics on over-shift and `wrapping_shl` reduces the amount
/// modulo the width, which would make `x << 64` equal `x`. Saturating to zero
/// is the behaviour a monitor can rely on without a proof obligation.
fn shift(a: &Value, b: &Value, left: bool) -> Result<Value> {
    let amount = to_u128(b)?;
    macro_rules! do_shift {
        ($($variant:ident($ty:ty)),* $(,)?) => {
            match a {
                $(
                    Value::$variant(x) => {
                        let width = <$ty>::BITS as u128;
                        Value::$variant(if amount >= width {
                            0
                        } else if left {
                            x.wrapping_shl(amount as u32)
                        } else {
                            x.wrapping_shr(amount as u32)
                        })
                    }
                )*
                other => return Err(unsupported(other)),
            }
        };
    }
    Ok(do_shift!(
        Int8(i8),
        Int16(i16),
        Int32(i32),
        Int64(i64),
        Word8(u8),
        Word16(u16),
        Word32(u32),
        Word64(u64),
    ))
}

fn cast(a: &Value, to: &Type) -> Result<Value> {
    let wide = to_i128(a)?;
    Ok(match to {
        Type::Int8 => Value::Int8(wide as i8),
        Type::Int16 => Value::Int16(wide as i16),
        Type::Int32 => Value::Int32(wide as i32),
        Type::Int64 => Value::Int64(wide as i64),
        Type::Word8 => Value::Word8(wide as u8),
        Type::Word16 => Value::Word16(wide as u16),
        Type::Word32 => Value::Word32(wide as u32),
        Type::Word64 => Value::Word64(wide as u64),
        Type::Float => Value::Float(wide as f32),
        Type::Double => Value::Double(wide as f64),
        other => {
            return Err(Error::Mismatch {
                context: "cast target".into(),
                expected: Type::Word64,
                found: other.clone(),
            });
        }
    })
}

/// Compares two scalars, reporting `None` when they are unordered.
///
/// Only NaN is unordered, and every comparison against it must be false —
/// including `<=` and `>=`, which is why this returns an `Option` rather than
/// collapsing NaN to `Equal`.
fn compare(a: &Value, b: &Value) -> Result<Option<Ordering>> {
    Ok(match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        (Value::Int8(x), Value::Int8(y)) => Some(x.cmp(y)),
        (Value::Int16(x), Value::Int16(y)) => Some(x.cmp(y)),
        (Value::Int32(x), Value::Int32(y)) => Some(x.cmp(y)),
        (Value::Int64(x), Value::Int64(y)) => Some(x.cmp(y)),
        (Value::Word8(x), Value::Word8(y)) => Some(x.cmp(y)),
        (Value::Word16(x), Value::Word16(y)) => Some(x.cmp(y)),
        (Value::Word32(x), Value::Word32(y)) => Some(x.cmp(y)),
        (Value::Word64(x), Value::Word64(y)) => Some(x.cmp(y)),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y),
        (Value::Double(x), Value::Double(y)) => x.partial_cmp(y),
        _ => return Err(unsupported(a)),
    })
}

fn as_bool(value: &Value) -> Result<bool> {
    match value {
        Value::Bool(b) => Ok(*b),
        other => Err(unsupported(other)),
    }
}

fn to_i128(value: &Value) -> Result<i128> {
    Ok(match value {
        Value::Int8(x) => *x as i128,
        Value::Int16(x) => *x as i128,
        Value::Int32(x) => *x as i128,
        Value::Int64(x) => *x as i128,
        Value::Word8(x) => *x as i128,
        Value::Word16(x) => *x as i128,
        Value::Word32(x) => *x as i128,
        Value::Word64(x) => *x as i128,
        other => return Err(unsupported(other)),
    })
}

fn to_u128(value: &Value) -> Result<u128> {
    Ok(to_i128(value)? as u128)
}

/// A value reached an operator that does not accept its type.
///
/// Unreachable on a validated spec — [`copilot_core::typecheck`] rules these
/// out — so it reports the type rather than trying to explain the operator.
fn unsupported(value: &Value) -> Error {
    Error::Mismatch {
        context: format!("operator applied to unsupported value `{value}`"),
        expected: Type::Bool,
        found: Type::Bool,
    }
}
