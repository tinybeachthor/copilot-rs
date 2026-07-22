//! A small S-expression reader, and decoding of solver answers into values.
//!
//! Only enough to read what `get-value` returns. Solvers disagree about how
//! they print things — z3 answers a bitvector as `#x0f` where cvc5 says
//! `#b00001111` — so the decoding below accepts every spelling either of them
//! produces rather than assuming one.

use crate::Error;
use copilot_core::{Type, Value};

/// A parsed S-expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sexpr {
    /// A bare token.
    Atom(String),
    /// A parenthesised list.
    List(Vec<Sexpr>),
}

impl Sexpr {
    /// The token, if this is an atom.
    pub fn atom(&self) -> Option<&str> {
        match self {
            Sexpr::Atom(s) => Some(s),
            Sexpr::List(_) => None,
        }
    }

    /// The elements, if this is a list.
    pub fn list(&self) -> Option<&[Sexpr]> {
        match self {
            Sexpr::List(items) => Some(items),
            Sexpr::Atom(_) => None,
        }
    }
}

/// Parses one S-expression, ignoring anything after it.
pub fn parse(input: &str) -> Result<Sexpr, Error> {
    let mut tokens = Lexer { input, position: 0 };
    let parsed = tokens.sexpr()?;
    Ok(parsed)
}

struct Lexer<'a> {
    input: &'a str,
    position: usize,
}

impl Lexer<'_> {
    fn skip_trivia(&mut self) {
        let bytes = self.input.as_bytes();
        while self.position < bytes.len() {
            match bytes[self.position] {
                b' ' | b'\t' | b'\r' | b'\n' => self.position += 1,
                b';' => {
                    while self.position < bytes.len() && bytes[self.position] != b'\n' {
                        self.position += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn sexpr(&mut self) -> Result<Sexpr, Error> {
        self.skip_trivia();
        let bytes = self.input.as_bytes();
        if self.position >= bytes.len() {
            return Err(Error::Protocol("unexpected end of solver output".into()));
        }

        if bytes[self.position] == b'(' {
            self.position += 1;
            let mut items = Vec::new();
            loop {
                self.skip_trivia();
                let bytes = self.input.as_bytes();
                if self.position >= bytes.len() {
                    return Err(Error::Protocol("unclosed list in solver output".into()));
                }
                if bytes[self.position] == b')' {
                    self.position += 1;
                    return Ok(Sexpr::List(items));
                }
                items.push(self.sexpr()?);
            }
        }

        // A quoted symbol, `|like this|`, which solvers use for names holding
        // characters that would otherwise need escaping.
        if bytes[self.position] == b'|' {
            let start = self.position;
            self.position += 1;
            while self.position < bytes.len() && bytes[self.position] != b'|' {
                self.position += 1;
            }
            self.position += 1;
            return Ok(Sexpr::Atom(self.input[start..self.position].to_string()));
        }

        let start = self.position;
        while self.position < bytes.len()
            && !matches!(
                bytes[self.position],
                b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')'
            )
        {
            self.position += 1;
        }
        if start == self.position {
            return Err(Error::Protocol(format!(
                "unexpected character in solver output at byte {start}"
            )));
        }
        Ok(Sexpr::Atom(self.input[start..self.position].to_string()))
    }
}

/// Decodes a scalar answer into a value of the given type.
///
/// Aggregates never reach here: a struct or array is read one leaf at a time,
/// by asking the solver for a selector or `select` applied to it, so only
/// scalars are ever decoded.
pub fn decode(ty: &Type, sexpr: &Sexpr) -> Result<Value, Error> {
    match ty {
        Type::Bool => match sexpr.atom() {
            Some("true") => Ok(Value::Bool(true)),
            Some("false") => Ok(Value::Bool(false)),
            _ => Err(unexpected(ty, sexpr)),
        },
        ty if ty.is_integral() => {
            let bits = bitvector(sexpr).ok_or_else(|| unexpected(ty, sexpr))?;
            Ok(integer(ty, bits))
        }
        Type::Float => Ok(Value::Float(
            real(sexpr).ok_or_else(|| unexpected(ty, sexpr))? as f32,
        )),
        Type::Double => Ok(Value::Double(
            real(sexpr).ok_or_else(|| unexpected(ty, sexpr))?,
        )),
        _ => Err(Error::Protocol(format!(
            "cannot decode a value of type {ty} directly"
        ))),
    }
}

fn unexpected(ty: &Type, sexpr: &Sexpr) -> Error {
    Error::Protocol(format!("solver returned {sexpr:?}, which is not a {ty}"))
}

/// Reads a bitvector in any of the spellings the solvers use.
fn bitvector(sexpr: &Sexpr) -> Option<u128> {
    match sexpr {
        Sexpr::Atom(token) => {
            if let Some(bits) = token.strip_prefix("#b") {
                u128::from_str_radix(bits, 2).ok()
            } else if let Some(digits) = token.strip_prefix("#x") {
                u128::from_str_radix(digits, 16).ok()
            } else {
                None
            }
        }
        // `(_ bv5 8)`, the indexed-identifier spelling.
        Sexpr::List(items) => match items.as_slice() {
            [Sexpr::Atom(underscore), Sexpr::Atom(literal), _] if underscore == "_" => {
                literal.strip_prefix("bv")?.parse().ok()
            }
            _ => None,
        },
    }
}

/// Narrows a bit pattern into the value its type denotes.
fn integer(ty: &Type, bits: u128) -> Value {
    match ty {
        Type::Int8 => Value::Int8(bits as i8),
        Type::Int16 => Value::Int16(bits as i16),
        Type::Int32 => Value::Int32(bits as i32),
        Type::Int64 => Value::Int64(bits as i64),
        Type::Word8 => Value::Word8(bits as u8),
        Type::Word16 => Value::Word16(bits as u16),
        Type::Word32 => Value::Word32(bits as u32),
        Type::Word64 => Value::Word64(bits as u64),
        other => unreachable!("copilot-theorem: {other} is not an integer type"),
    }
}

/// Reads a rational or floating-point answer as an `f64`.
fn real(sexpr: &Sexpr) -> Option<f64> {
    match sexpr {
        Sexpr::Atom(token) => token.parse().ok(),
        Sexpr::List(items) => match items.as_slice() {
            // `(- 1.5)`
            [Sexpr::Atom(op), value] if op == "-" => Some(-real(value)?),
            // `(/ 1 2)`, how solvers print a rational.
            [Sexpr::Atom(op), numerator, denominator] if op == "/" => {
                Some(real(numerator)? / real(denominator)?)
            }
            // `(fp #b0 #b10000000 #b0000...)`, an IEEE value in three fields.
            [Sexpr::Atom(op), sign, exponent, significand] if op == "fp" => {
                let sign = bitvector(sign)?;
                let exponent_bits = bit_width(exponent)?;
                let significand_bits = bit_width(significand)?;
                let exponent = bitvector(exponent)?;
                let significand = bitvector(significand)?;
                let bits = (sign << (exponent_bits + significand_bits))
                    | (exponent << significand_bits)
                    | significand;
                match exponent_bits + significand_bits + 1 {
                    32 => Some(f32::from_bits(bits as u32) as f64),
                    64 => Some(f64::from_bits(bits as u64)),
                    _ => None,
                }
            }
            // `(_ +zero 8 24)`, `(_ NaN 8 24)`, `(_ +oo 8 24)`.
            [Sexpr::Atom(underscore), Sexpr::Atom(name), ..] if underscore == "_" => {
                match name.as_str() {
                    "+zero" => Some(0.0),
                    "-zero" => Some(-0.0),
                    "+oo" => Some(f64::INFINITY),
                    "-oo" => Some(f64::NEG_INFINITY),
                    "NaN" => Some(f64::NAN),
                    _ => None,
                }
            }
            _ => None,
        },
    }
}

/// How many bits a literal spells out, needed to reassemble an `fp` triple.
fn bit_width(sexpr: &Sexpr) -> Option<u32> {
    let token = sexpr.atom()?;
    if let Some(bits) = token.strip_prefix("#b") {
        Some(bits.len() as u32)
    } else {
        token
            .strip_prefix("#x")
            .map(|digits| digits.len() as u32 * 4)
    }
}
