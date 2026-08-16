//! Rendering IR types and values as Bluespec source.

use copilot_core::{Type, Value};

/// The Bluespec type denoting `ty`.
///
/// Sized integers become `Int n` and `UInt n`, so the width a specification
/// asks for is the width the hardware gets — Bluespec has no promotion and no
/// native word size to inherit one from.
pub fn ty(ty: &Type) -> String {
    match ty {
        Type::Bool => "Bool".into(),
        Type::Int8 => "Int 8".into(),
        Type::Int16 => "Int 16".into(),
        Type::Int32 => "Int 32".into(),
        Type::Int64 => "Int 64".into(),
        Type::Word8 => "UInt 8".into(),
        Type::Word16 => "UInt 16".into(),
        Type::Word32 => "UInt 32".into(),
        Type::Word64 => "UInt 64".into(),
        Type::Array { elem, len } => format!("Vector {len} {}", ty_atom(elem)),
        Type::Struct(s) => s.name.clone(),
        // Rejected by `emit`, which runs before anything is rendered.
        other => panic!("copilot-bluespec: {other} has no Bluespec type"),
    }
}

/// [`ty`], parenthesised unless it is a single token.
///
/// Bluespec type application binds like Haskell's, so `Reg (UInt 8)` needs the
/// parentheses and `Reg Bool` does not.
pub fn ty_atom(t: &Type) -> String {
    let rendered = ty(t);
    if rendered.contains(' ') {
        format!("({rendered})")
    } else {
        rendered
    }
}

/// The width in bits of an integer type, for guarding shifts.
pub fn bit_width(ty: &Type) -> u32 {
    match ty {
        Type::Int8 | Type::Word8 => 8,
        Type::Int16 | Type::Word16 => 16,
        Type::Int32 | Type::Word32 => 32,
        Type::Int64 | Type::Word64 => 64,
        other => panic!("copilot-bluespec: {other} has no bit width"),
    }
}

/// A Bluespec literal for `value`.
///
/// Every literal here is polymorphic in its own type — `0` is a `Literal`, not
/// a `UInt 8` — and takes its type from the signature of whatever it
/// initialises. Generated code annotates every binding and every register, so
/// there is always one in scope.
pub fn value(value: &Value) -> String {
    match value {
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),

        Value::Word8(v) => v.to_string(),
        Value::Word16(v) => v.to_string(),
        Value::Word32(v) => v.to_string(),
        Value::Word64(v) => v.to_string(),

        // Bluespec has no negative literals — `-5` is a subtraction — and the
        // most negative value of a width has no positive counterpart to negate
        // anyway. `minBound` names it; `negate` builds the rest.
        Value::Int8(v) => signed(*v as i128, i8::MIN as i128),
        Value::Int16(v) => signed(*v as i128, i16::MIN as i128),
        Value::Int32(v) => signed(*v as i128, i32::MIN as i128),
        Value::Int64(v) => signed(*v as i128, i64::MIN as i128),

        Value::Array(values) => {
            let mut out = String::new();
            for (i, element) in values.iter().enumerate().rev() {
                let tail = if i + 1 == values.len() {
                    "nil".to_string()
                } else {
                    format!("({out})")
                };
                out = format!("cons {} {tail}", value_atom(element));
            }
            out
        }
        Value::Struct { name, fields } => {
            let assignments: Vec<String> = fields
                .iter()
                .map(|(field, v)| format!("{field} = {}", value_atom(v)))
                .collect();
            format!("{name} {{ {} }}", assignments.join("; "))
        }

        // Rejected by `emit`, which runs before anything is rendered.
        other => panic!("copilot-bluespec: {other} has no Bluespec literal"),
    }
}

/// [`value`], parenthesised unless it is a single token.
pub fn value_atom(v: &Value) -> String {
    let rendered = value(v);
    if rendered.contains(' ') {
        format!("({rendered})")
    } else {
        rendered
    }
}

fn signed(v: i128, min: i128) -> String {
    if v == min {
        "minBound".into()
    } else if v < 0 {
        format!("negate {}", -v)
    } else {
        v.to_string()
    }
}
