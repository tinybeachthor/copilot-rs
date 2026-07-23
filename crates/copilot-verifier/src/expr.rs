//! Lowering IR expressions to Rust — the reference implementation.
//!
//! This is a second, independent lowering of the same IR that `copilot-rust`
//! compiles. It agrees with that one on the *meaning* of every operator, which
//! it must — wrapping arithmetic, division by zero being zero, shifts past the
//! operand width being zero are semantic contracts, not implementation choices,
//! and if the two lowerings disagreed about them the bisimulation would fail
//! for a reason that is not a bug.
//!
//! Where it differs, deliberately and structurally, is **state**. The monitor
//! stores a stream as a ring buffer with a rotating index and reads `drop i` as
//! `self.s[(idx + i) % n]`. This reference stores a stream as an explicit
//! vector in time order and reads `drop i` as `state.s[i]` — no ring, no index,
//! no modular arithmetic. That is the difference the harness proves the ring
//! buffer correctly implements, over every state and every input at once.

use crate::render;
use copilot_core::{ExprId, IndexPolicy, Node, Op1, Op2, Op3, Spec, StreamId, Type, VarId};
use std::collections::HashMap;

/// Names the binding holding an expression's value.
pub fn binding(id: ExprId) -> String {
    format!("r{}", id.0)
}

/// Names the local holding an external variable's sample.
pub fn sample(name: &str) -> String {
    format!("x_{name}")
}

/// Lowers expressions against an explicit, time-ordered state vector.
pub struct Reference<'a> {
    spec: &'a Spec,
    index_policy: IndexPolicy,
    var_bindings: HashMap<VarId, ExprId>,
}

impl<'a> Reference<'a> {
    /// Prepares to lower a specification's expressions.
    pub fn new(spec: &'a Spec, index_policy: IndexPolicy) -> Self {
        let mut var_bindings = HashMap::new();
        for (_, node) in spec.arena.nodes() {
            if let Node::Local { var, bound, .. } = node {
                var_bindings.insert(*var, *bound);
            }
        }
        Reference {
            spec,
            index_policy,
            var_bindings,
        }
    }

    /// The Rust expression computing `id`, reading state from `state`.
    pub fn node(&self, id: ExprId, state: &str) -> String {
        let arena = &self.spec.arena;
        let operand = binding;

        match arena.node(id) {
            Node::Const { value, .. } => render::value(value),

            // Direct index into the time-ordered vector. This is the whole
            // structural difference from the monitor: no `(idx + i) % n`.
            Node::Drop { idx, stream } => format!("{state}.{}[{idx}]", stream_field(*stream)),

            Node::ExternVar { name, .. } => sample(name),

            Node::Var(var) => match self.var_bindings.get(var) {
                Some(bound) => binding(*bound),
                None => unreachable!("copilot-verifier: {var} is unbound in a validated spec"),
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
            Op2::And => format!("{a} & {b}"),
            Op2::Or => format!("{a} | {b}"),

            Op2::Add(ty) if ty.is_floating() => format!("{a} + {b}"),
            Op2::Sub(ty) if ty.is_floating() => format!("{a} - {b}"),
            Op2::Mul(ty) if ty.is_floating() => format!("{a} * {b}"),
            Op2::Add(_) => format!("{a}.wrapping_add({b})"),
            Op2::Sub(_) => format!("{a}.wrapping_sub({b})"),
            Op2::Mul(_) => format!("{a}.wrapping_mul({b})"),

            Op2::Div(ty) => self.guarded_division(ty, a, b, "wrapping_div"),
            Op2::Mod(ty) => self.guarded_division(ty, a, b, "wrapping_rem"),
            Op2::Fdiv(_) => format!("{a} / {b}"),

            Op2::Pow(ty) => self.call(ty, "pow", &[a, b]),
            Op2::Logb(ty) => format!(
                "{} / {}",
                self.call(ty, "log", &[a]),
                self.call(ty, "log", &[b])
            ),
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
            Op3::Mux(_) => format!("if {a} {{ {b} }} else {{ {c} }}"),
            Op3::UpdateArray(array) => format!(
                "{{ let mut t = {a}; t[{}] = {c}; t }}",
                self.subscript(array, b)
            ),
        }
    }

    fn guarded_division(&self, ty: &Type, a: &str, b: &str, method: &str) -> String {
        let zero = format!("0{}", render::ty(ty));
        format!("if {b} == 0 {{ {zero} }} else {{ {a}.{method}({b}) }}")
    }

    fn guarded_shift(&self, val: &Type, amount: &Type, a: &str, b: &str, method: &str) -> String {
        let width = render::bit_width(val);
        let zero = format!("0{}", render::ty(val));
        let widened = cast_to(b, amount, "u64");
        let narrowed = cast_to(b, amount, "u32");
        format!("if {widened} >= {width} {{ {zero} }} else {{ {a}.{method}({narrowed}) }}")
    }

    fn subscript(&self, array: &Type, index: &str) -> String {
        let len = match array {
            Type::Array { len, .. } => *len,
            other => panic!("copilot-verifier: {other} is not an array"),
        };
        match self.index_policy {
            IndexPolicy::Wrap => format!("({index} as usize) % {len}"),
            IndexPolicy::Saturate => format!(
                "if ({index} as usize) < {len} {{ {index} as usize }} else {{ {} }}",
                len - 1
            ),
            IndexPolicy::Assume => format!("{index} as usize"),
        }
    }

    fn call(&self, ty: &Type, function: &str, args: &[&str]) -> String {
        let suffix = if *ty == Type::Float { "f" } else { "" };
        format!("libm::{function}{suffix}({})", args.join(", "))
    }
}

/// The field name for a stream in the reference `State`.
pub fn stream_field(stream: StreamId) -> String {
    format!("s{}", stream.0)
}

fn cast_to(operand: &str, from: &Type, target: &str) -> String {
    if render::ty(from) == target {
        operand.to_string()
    } else {
        format!("{operand} as {target}")
    }
}
