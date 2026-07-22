//! Rendering IR types and values as Rust source.

use copilot_core::{Type, Value};

/// The Rust type denoting `ty`.
pub fn ty(ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::Int8 => "i8".into(),
        Type::Int16 => "i16".into(),
        Type::Int32 => "i32".into(),
        Type::Int64 => "i64".into(),
        Type::Word8 => "u8".into(),
        Type::Word16 => "u16".into(),
        Type::Word32 => "u32".into(),
        Type::Word64 => "u64".into(),
        Type::Float => "f32".into(),
        Type::Double => "f64".into(),
        Type::Array { elem, len } => format!("[{}; {}]", self::ty(elem), len),
        Type::Struct(s) => s.name.clone(),
    }
}

/// The width in bits of an integer type, for guarding shifts.
pub fn bit_width(ty: &Type) -> u32 {
    match ty {
        Type::Int8 | Type::Word8 => 8,
        Type::Int16 | Type::Word16 => 16,
        Type::Int32 | Type::Word32 => 32,
        Type::Int64 | Type::Word64 => 64,
        other => panic!("copilot-rust: {other} has no bit width"),
    }
}

/// A `const`-compatible Rust literal for `value`.
///
/// Every literal here can appear in a `const fn`, which is what lets a
/// generated `Monitor::new` be `const` and its initial state live in read-only
/// memory.
pub fn value(value: &Value) -> String {
    match value {
        Value::Bool(v) => v.to_string(),

        // The most negative integer of each width has no literal form: `-128i8`
        // parses as the negation of `128i8`, which does not fit. Name the
        // constant instead.
        Value::Int8(v) => signed(*v as i128, i8::MIN as i128, "i8"),
        Value::Int16(v) => signed(*v as i128, i16::MIN as i128, "i16"),
        Value::Int32(v) => signed(*v as i128, i32::MIN as i128, "i32"),
        Value::Int64(v) => signed(*v as i128, i64::MIN as i128, "i64"),

        Value::Word8(v) => format!("{v}u8"),
        Value::Word16(v) => format!("{v}u16"),
        Value::Word32(v) => format!("{v}u32"),
        Value::Word64(v) => format!("{v}u64"),

        // Rust's `Debug` for floats emits the shortest string that round-trips,
        // so a finite value survives the trip through source exactly, negative
        // zero included. Infinities and NaN have no literal form at all;
        // `from_bits` is `const`, and it also preserves *which* NaN, since a
        // spec may legitimately start a stream at one.
        Value::Float(v) if v.is_finite() => format!("{v:?}f32"),
        Value::Float(v) => format!("f32::from_bits({:#010x})", v.to_bits()),
        Value::Double(v) if v.is_finite() => format!("{v:?}f64"),
        Value::Double(v) => format!("f64::from_bits({:#018x})", v.to_bits()),

        Value::Array(values) => {
            let elements: Vec<String> = values.iter().map(self::value).collect();
            format!("[{}]", elements.join(", "))
        }
        Value::Struct { name, fields } => {
            let assignments: Vec<String> = fields
                .iter()
                .map(|(field, v)| format!("{field}: {}", self::value(v)))
                .collect();
            format!("{name} {{ {} }}", assignments.join(", "))
        }
    }
}

fn signed(v: i128, min: i128, suffix: &str) -> String {
    if v == min {
        format!("{suffix}::MIN")
    } else {
        format!("{v}{suffix}")
    }
}
