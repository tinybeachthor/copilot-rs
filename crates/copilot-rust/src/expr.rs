//! Lowering IR expressions to Rust.
//!
//! Every reachable node becomes one `let` binding, in arena order. Because the
//! arena interns children before parents, that order is already topological: a
//! node's operands are always bound by the time it is emitted, with no
//! recursion and no scheduling pass.
//!
//! Binding each node exactly once is also what makes the generated code cost
//! what [`copilot_core::cost`] says it does. A backend that inlined shared
//! subexpressions would do `nodes_inlined` work instead of `nodes_shared`, and
//! the reported figure would be fiction.

use crate::render;
use copilot_core::{ExprId, IndexPolicy, Node, Op1, Op2, Op3, Spec, StreamId, Type, VarId};
use std::collections::HashMap;

/// Names the binding holding an expression's value.
pub fn binding(id: ExprId) -> String {
    format!("e{}", id.0)
}

/// Names the local holding an external variable's sample for this step.
pub fn sample(name: &str) -> String {
    format!("x_{name}")
}

/// Names the constant holding a stream's buffer length.
pub fn buffer_len(stream: StreamId) -> String {
    format!("S{}_LEN", stream.0)
}

/// Names a stream's buffer field.
pub fn buffer(stream: StreamId) -> String {
    format!("s{}", stream.0)
}

/// Names a stream's rotating index field.
pub fn index(stream: StreamId) -> String {
    format!("s{}_idx", stream.0)
}

/// Context for lowering one specification.
pub struct Lowering<'a> {
    spec: &'a Spec,
    index_policy: IndexPolicy,
    math: &'a str,
    /// What each local variable is bound to.
    ///
    /// `Local` is erased rather than emitted as a nested Rust `let`: since the
    /// language is pure, substituting a variable by its definition is exact,
    /// and the definition is already bound once by the flat emission above. A
    /// nested binding would instead have to be emitted *inside* the expression
    /// that uses it, which the flat arena order cannot express — a `Var` node
    /// is interned before the `Local` that binds it.
    var_bindings: HashMap<VarId, ExprId>,
}

impl<'a> Lowering<'a> {
    /// Prepares to lower a specification.
    pub fn new(spec: &'a Spec, index_policy: IndexPolicy, math: &'a str) -> Self {
        let mut var_bindings = HashMap::new();
        for (_, node) in spec.arena.nodes() {
            if let Node::Local { var, bound, .. } = node {
                var_bindings.insert(*var, *bound);
            }
        }
        Lowering {
            spec,
            index_policy,
            math,
            var_bindings,
        }
    }

    /// The Rust expression computing `id` from its operands' bindings.
    pub fn node(&self, id: ExprId) -> String {
        let arena = &self.spec.arena;
        let operand = |child: ExprId| binding(child);

        match arena.node(id) {
            Node::Const { value, .. } => render::value(value),

            // The ring-buffer invariant made concrete: slot `(p + i) % n` holds
            // the value `i` steps ahead. A single-element buffer is always read
            // at slot 0, so it carries no index at all.
            Node::Drop { idx, stream } => {
                let decl = &arena.stream_decls()[stream.index()];
                if decl.buffer_len == 1 {
                    format!("self.{}[0]", buffer(*stream))
                } else if *idx == 0 {
                    // The rotating index is always in range, so reading the
                    // current value needs no arithmetic at all.
                    format!("self.{}[self.{} as usize]", buffer(*stream), index(*stream))
                } else {
                    format!(
                        "self.{}[((self.{} + {idx}) % {}) as usize]",
                        buffer(*stream),
                        index(*stream),
                        buffer_len(*stream)
                    )
                }
            }

            Node::ExternVar { name, .. } => sample(name),

            Node::Var(var) => match self.var_bindings.get(var) {
                Some(bound) => binding(*bound),
                None => unreachable!("copilot-rust: {var} is unbound in a validated spec"),
            },
            Node::Local { body, .. } => binding(*body),
            Node::Label(_, a) => binding(*a),

            Node::Op1(op, a) => self.op1(op, &operand(*a)),
            Node::Op2(op, a, b) => self.op2(op, &operand(*a), &operand(*b)),
            Node::Op3(op, a, b, c) => self.op3(op, &operand(*a), &operand(*b), &operand(*c)),
        }
    }

    fn op1(&self, op: &Op1, a: &str) -> String {
        match op {
            Op1::Not | Op1::BwNot(_) => format!("!{a}"),

            // `wrapping_abs` keeps `i8::MIN.abs()` at `i8::MIN` rather than
            // panicking, matching the wrapping arithmetic everywhere else.
            // Unsigned values are already their own magnitude.
            Op1::Abs(ty) if ty.is_floating() => format!("{a}.abs()"),
            Op1::Abs(ty) if ty.is_signed() => format!("{a}.wrapping_abs()"),
            Op1::Abs(_) => a.to_string(),

            Op1::Sign(ty) if ty.is_floating() || ty.is_signed() => format!("{a}.signum()"),
            Op1::Sign(ty) => format!("({a} != 0) as {}", render::ty(ty)),

            Op1::Recip(ty) => format!("1.0{} / {a}", render::ty(ty)),

            Op1::Sqrt(ty) => self.call(ty, "sqrt", &[a]),
            Op1::Exp(ty) => self.call(ty, "exp", &[a]),
            Op1::Log(ty) => self.call(ty, "log", &[a]),
            Op1::Sin(ty) => self.call(ty, "sin", &[a]),
            Op1::Cos(ty) => self.call(ty, "cos", &[a]),
            Op1::Tan(ty) => self.call(ty, "tan", &[a]),
            Op1::Asin(ty) => self.call(ty, "asin", &[a]),
            Op1::Acos(ty) => self.call(ty, "acos", &[a]),
            Op1::Atan(ty) => self.call(ty, "atan", &[a]),
            Op1::Sinh(ty) => self.call(ty, "sinh", &[a]),
            Op1::Cosh(ty) => self.call(ty, "cosh", &[a]),
            Op1::Tanh(ty) => self.call(ty, "tanh", &[a]),
            Op1::Asinh(ty) => self.call(ty, "asinh", &[a]),
            Op1::Acosh(ty) => self.call(ty, "acosh", &[a]),
            Op1::Atanh(ty) => self.call(ty, "atanh", &[a]),
            Op1::Ceiling(ty) => self.call(ty, "ceil", &[a]),
            Op1::Floor(ty) => self.call(ty, "floor", &[a]),

            Op1::Cast { to, .. } => format!("{a} as {}", render::ty(to)),
            Op1::GetField { field, .. } => format!("{a}.{field}"),
        }
    }

    fn op2(&self, op: &Op2, a: &str, b: &str) -> String {
        match op {
            // `&` and `|` rather than `&&` and `||`: both operands are already
            // bound, so there is nothing to short-circuit, and avoiding the
            // branch keeps a step's timing independent of its data.
            Op2::And => format!("{a} & {b}"),
            Op2::Or => format!("{a} | {b}"),

            Op2::Add(ty) if ty.is_floating() => format!("{a} + {b}"),
            Op2::Sub(ty) if ty.is_floating() => format!("{a} - {b}"),
            Op2::Mul(ty) if ty.is_floating() => format!("{a} * {b}"),
            Op2::Add(_) => format!("{a}.wrapping_add({b})"),
            Op2::Sub(_) => format!("{a}.wrapping_sub({b})"),
            Op2::Mul(_) => format!("{a}.wrapping_mul({b})"),

            // Division by zero is zero. `wrapping_div` additionally keeps
            // `MIN / -1` at `MIN` instead of trapping.
            Op2::Div(ty) => self.guarded_division(ty, a, b, "wrapping_div"),
            Op2::Mod(ty) => self.guarded_division(ty, a, b, "wrapping_rem"),
            Op2::Fdiv(_) => format!("{a} / {b}"),

            Op2::Pow(ty) => self.call(ty, "pow", &[a, b]),
            Op2::Logb(ty) => {
                // `x.log(base)` is `ln(x) / ln(base)`, which is how the standard
                // library defines it and therefore what the interpreter computes.
                format!(
                    "{} / {}",
                    self.call(ty, "log", &[a]),
                    self.call(ty, "log", &[b])
                )
            }
            Op2::Atan2(ty) => self.call(ty, "atan2", &[a, b]),

            Op2::Eq(_) => format!("{a} == {b}"),
            Op2::Ne(_) => format!("{a} != {b}"),
            Op2::Lt(_) => format!("{a} < {b}"),
            Op2::Le(_) => format!("{a} <= {b}"),
            Op2::Gt(_) => format!("{a} > {b}"),
            Op2::Ge(_) => format!("{a} >= {b}"),

            Op2::BwAnd(_) => format!("{a} & {b}"),
            Op2::BwOr(_) => format!("{a} | {b}"),
            Op2::BwXor(_) => format!("{a} ^ {b}"),
            Op2::BwShiftL { val, amount } => self.guarded_shift(val, amount, a, b, "wrapping_shl"),
            Op2::BwShiftR { val, amount } => self.guarded_shift(val, amount, a, b, "wrapping_shr"),

            Op2::Index(array) => format!("{a}[{}]", self.subscript(array, b)),
            Op2::UpdateField { field, .. } => {
                format!("{{ let mut t = {a}; t.{field} = {b}; t }}")
            }
        }
    }

    fn op3(&self, op: &Op3, a: &str, b: &str, c: &str) -> String {
        match op {
            // Both arms are already bound, so this selects rather than
            // branching around work.
            Op3::Mux(_) => format!("if {a} {{ {b} }} else {{ {c} }}"),
            Op3::UpdateArray(array) => format!(
                "{{ let mut t = {a}; t[{}] = {c}; t }}",
                self.subscript(array, b)
            ),
        }
    }

    /// `if divisor == 0 { 0 } else { .. }`, giving division by zero a value.
    fn guarded_division(&self, ty: &Type, a: &str, b: &str, method: &str) -> String {
        let zero = match ty {
            Type::Int8 => "0i8",
            Type::Int16 => "0i16",
            Type::Int32 => "0i32",
            Type::Int64 => "0i64",
            Type::Word8 => "0u8",
            Type::Word16 => "0u16",
            Type::Word32 => "0u32",
            Type::Word64 => "0u64",
            other => panic!("copilot-rust: {other} does not support integer division"),
        };
        format!("if {b} == 0 {{ {zero} }} else {{ {a}.{method}({b}) }}")
    }

    /// A shift that yields zero once the amount reaches the operand's width.
    ///
    /// `wrapping_shl` reduces the amount modulo the width, which would make
    /// `x << 64` equal `x`. Widening the amount to `u64` before comparing also
    /// handles a negative amount: it becomes enormous, so the guard rejects it,
    /// which is what the interpreter does too.
    fn guarded_shift(&self, val: &Type, amount: &Type, a: &str, b: &str, method: &str) -> String {
        let width = render::bit_width(val);
        let zero = format!("0{}", render::ty(val));
        let widened = cast_to(b, amount, "u64");
        let narrowed = cast_to(b, amount, "u32");
        format!("if {widened} >= {width} {{ {zero} }} else {{ {a}.{method}({narrowed}) }}")
    }

    /// Resolves an array subscript under the configured policy.
    fn subscript(&self, array: &Type, index: &str) -> String {
        let len = match array {
            Type::Array { len, .. } => *len,
            other => panic!("copilot-rust: {other} is not an array"),
        };
        match self.index_policy {
            IndexPolicy::Wrap => format!("({index} as usize) % {len}"),
            IndexPolicy::Saturate => {
                format!(
                    "if ({index} as usize) < {len} {{ {index} as usize }} else {{ {} }}",
                    len - 1
                )
            }
            // The obligation is the caller's; generated code takes it as given.
            IndexPolicy::Assume => format!("{index} as usize"),
        }
    }

    /// A call into the maths library, picking the single- or double-precision
    /// entry point.
    ///
    /// `core` provides only the exactly-rounded operations — `abs`, `signum`,
    /// `copysign` — so everything transcendental goes through `libm`, which is
    /// what makes the generated monitor `no_std`.
    fn call(&self, ty: &Type, function: &str, args: &[&str]) -> String {
        let suffix = if *ty == Type::Float { "f" } else { "" };
        format!("{}::{function}{suffix}({})", self.math, args.join(", "))
    }
}

/// Casts an operand to `target`, unless it is already that type.
///
/// A no-op cast is not wrong, but it is noise in generated source and clippy
/// reports it in the user's own build.
fn cast_to(operand: &str, from: &Type, target: &str) -> String {
    if render::ty(from) == target {
        operand.to_string()
    } else {
        format!("{operand} as {target}")
    }
}
