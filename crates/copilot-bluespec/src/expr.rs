//! Lowering IR expressions to Bluespec.
//!
//! Every reachable node becomes one `let` binding, in arena order. Because the
//! arena interns children before parents, that order is already topological: a
//! node's operands are always bound by the time it is emitted, with no
//! recursion and no scheduling pass. This mirrors `copilot-rust` exactly, and
//! for the same reason — a binding per node is what makes the emitted circuit
//! share what [`copilot_core::cost`] says it shares.
//!
//! Bluespec is a hardware description language, so a "binding" is a named wire
//! rather than a store. Nothing is sequenced by it: the whole body of the step
//! rule is one combinational block whose inputs are the registers as they stood
//! at the clock edge.

use crate::render;
use copilot_core::{ExprId, IndexPolicy, Node, Op1, Op2, Op3, Spec, StreamId, Type, VarId};
use std::collections::{HashMap, HashSet};

/// Names the binding holding an expression's value.
pub fn binding(id: ExprId) -> String {
    format!("e{}", id.0)
}

/// Names the wire holding an external variable's sample for this step.
pub fn sample(name: &str) -> String {
    format!("x_{name}")
}

/// Names one register of a stream's ring buffer.
pub fn slot(stream: StreamId, index: usize) -> String {
    format!("s{}_{index}", stream.0)
}

/// Names the vector gathering a stream's buffer registers.
///
/// Only buffers deeper than one element have one: `drop i` on a deeper buffer
/// reads a slot chosen at run time, which needs a vector to select over.
pub fn vector(stream: StreamId) -> String {
    format!("s{}", stream.0)
}

/// Names a stream's rotating index register.
pub fn index(stream: StreamId) -> String {
    format!("s{}_idx", stream.0)
}

/// Context for lowering one specification.
pub struct Lowering<'a> {
    spec: &'a Spec,
    index_policy: IndexPolicy,
    /// What each local variable is bound to.
    ///
    /// `Local` is erased by substitution rather than emitted as a nested
    /// binding, exactly as in `copilot-rust`: the language is pure, so
    /// substituting a variable by its definition is exact, and the definition
    /// is already bound once by the flat emission above.
    var_bindings: HashMap<VarId, ExprId>,
    /// Bindings some emitted expression actually reads.
    ///
    /// Not the same as the arena's child relation. `Local` is erased by
    /// substitution, so a `Local` node emits only its body, and its *bound*
    /// expression is read only where a `Var` refers to it. A binding reachable
    /// solely as the bound side of a `Local` nobody uses is therefore emitted
    /// and never read.
    referenced: HashSet<ExprId>,
}

impl<'a> Lowering<'a> {
    /// Prepares to lower a specification.
    pub fn new(spec: &'a Spec, index_policy: IndexPolicy) -> Self {
        let mut var_bindings = HashMap::new();
        for (_, node) in spec.arena.nodes() {
            if let Node::Local { var, bound, .. } = node {
                var_bindings.insert(*var, *bound);
            }
        }
        let mut lowering = Lowering {
            spec,
            index_policy,
            var_bindings,
            referenced: HashSet::new(),
        };
        lowering.referenced = lowering.collect_referenced();
        lowering
    }

    /// Every binding the emitted code reads, from any root or any other node.
    fn collect_referenced(&self) -> HashSet<ExprId> {
        let mut referenced: HashSet<ExprId> = self.spec.runtime_roots().into_iter().collect();
        for id in copilot_core::reachable(self.spec, &self.spec.runtime_roots()) {
            match self.spec.arena.node(id) {
                Node::Var(var) => {
                    if let Some(bound) = self.var_bindings.get(var) {
                        referenced.insert(*bound);
                    }
                }
                // Erased: only the body is emitted, so only the body is read.
                Node::Local { body, .. } => {
                    referenced.insert(*body);
                }
                Node::Label(_, a) => {
                    referenced.insert(*a);
                }
                Node::Op1(_, a) => {
                    referenced.insert(*a);
                }
                Node::Op2(_, a, b) => {
                    referenced.insert(*a);
                    referenced.insert(*b);
                }
                Node::Op3(_, a, b, c) => {
                    referenced.insert(*a);
                    referenced.insert(*b);
                    referenced.insert(*c);
                }
                Node::Const { .. } | Node::Drop { .. } | Node::ExternVar { .. } => {}
            }
        }
        referenced
    }

    /// The name to declare a binding under.
    ///
    /// Underscored when nothing reads it, which is how Bluespec — like
    /// Haskell — is told that a definition is deliberately unused. Without it
    /// `bsc` warns in the user's own build about code they did not write.
    pub fn declaration(&self, id: ExprId) -> String {
        if self.referenced.contains(&id) {
            binding(id)
        } else {
            format!("_{}", binding(id))
        }
    }

    /// The Bluespec expression computing `id` from its operands' bindings.
    pub fn node(&self, id: ExprId) -> String {
        let arena = &self.spec.arena;
        let operand = |child: ExprId| binding(child);

        match arena.node(id) {
            Node::Const { value, .. } => render::value(value),

            Node::Drop { idx, stream } => self.read(*idx, *stream),

            Node::ExternVar { name, .. } => sample(name),

            Node::Var(var) => match self.var_bindings.get(var) {
                Some(bound) => binding(*bound),
                None => unreachable!("copilot-bluespec: {var} is unbound in a validated spec"),
            },
            Node::Local { body, .. } => binding(*body),
            Node::Label(_, a) => binding(*a),

            Node::Op1(op, a) => self.op1(op, &operand(*a)),
            Node::Op2(op, a, b) => self.op2(op, &operand(*a), &operand(*b)),
            Node::Op3(op, a, b, c) => self.op3(op, &operand(*a), &operand(*b), &operand(*c)),
        }
    }

    /// Reads the value `idx` steps ahead out of a stream's ring buffer.
    ///
    /// The invariant is the one `copilot-core` states: slot `(p + i) % n` holds
    /// the value `i` steps ahead. Both `p` and `i` are below `n`, so their sum
    /// is below `2n` and one conditional subtraction does what `%` would — a
    /// mux instead of a divider, which on hardware is not a micro-optimisation
    /// but the difference between a wire and a multi-cycle block.
    fn read(&self, idx: u32, stream: StreamId) -> String {
        let decl = &self.spec.arena.stream_decls()[stream.index()];
        let len = decl.buffer_len;
        if len == 1 {
            return slot(stream, 0);
        }
        let position = index(stream);
        if idx == 0 {
            format!("select (readVReg {}) {position}", vector(stream))
        } else {
            format!(
                "select (readVReg {}) (if {position} + {idx} >= {len} then {position} + {idx} - {len} else {position} + {idx})",
                vector(stream)
            )
        }
    }

    fn op1(&self, op: &Op1, a: &str) -> String {
        match op {
            Op1::Not => format!("not {a}"),
            Op1::BwNot(_) => format!("invert {a}"),

            // Wrapping absolute value, so the most negative integer is its own
            // magnitude rather than a trap. Unsigned values already are.
            Op1::Abs(ty) if ty.is_signed() => format!("if {a} < 0 then negate {a} else {a}"),
            Op1::Abs(_) => a.to_string(),

            // Bluespec's `signum` is not defined for `Int`/`UInt`, and spelling
            // it out is what the operation means anyway.
            Op1::Sign(ty) if ty.is_signed() => {
                format!("if {a} > 0 then 1 else (if {a} < 0 then negate 1 else 0)")
            }
            Op1::Sign(_) => format!("if {a} /= 0 then 1 else 0"),

            Op1::Cast { from, to } => self.cast(from, to, a),
            Op1::GetField { field, .. } => format!("{a}.{field}"),

            // Every remaining `Op1` is floating-point, and floats never reach a
            // Bluespec backend; see `Error::UnsupportedType`.
            other => panic!("copilot-bluespec: {} is not supported", other.name()),
        }
    }

    fn op2(&self, op: &Op2, a: &str, b: &str) -> String {
        match op {
            Op2::And => format!("{a} && {b}"),
            Op2::Or => format!("{a} || {b}"),

            // Fixed-width arithmetic in Bluespec wraps, which is the semantics
            // the IR fixes, so these need no guard at all.
            Op2::Add(_) => format!("{a} + {b}"),
            Op2::Sub(_) => format!("{a} - {b}"),
            Op2::Mul(_) => format!("{a} * {b}"),

            Op2::Div(ty) => self.guarded_division(ty, a, b, "/"),
            Op2::Mod(ty) => self.guarded_division(ty, a, b, "%"),

            Op2::Eq(_) => format!("{a} == {b}"),
            Op2::Ne(_) => format!("{a} /= {b}"),

            // `Bool` is ordered in this IR but has no `Ord` instance in
            // Bluespec. Comparing the packed bits is the same order — `False`
            // is 0 — without a special case anywhere else.
            Op2::Lt(ty) => self.compare(ty, a, b, "<"),
            Op2::Le(ty) => self.compare(ty, a, b, "<="),
            Op2::Gt(ty) => self.compare(ty, a, b, ">"),
            Op2::Ge(ty) => self.compare(ty, a, b, ">="),

            Op2::BwAnd(_) => format!("{a} & {b}"),
            Op2::BwOr(_) => format!("{a} | {b}"),
            Op2::BwXor(_) => format!("{a} ^ {b}"),
            Op2::BwShiftL { val, amount } => self.guarded_shift(val, amount, a, b, "<<"),
            Op2::BwShiftR { val, amount } => self.guarded_shift(val, amount, a, b, ">>"),

            Op2::Index(array) => format!("select {a} {}", self.subscript(array, b)),
            Op2::UpdateField { field, .. } => format!("{a} {{ {field} = {b} }}"),

            other => panic!("copilot-bluespec: {} is not supported", other.name()),
        }
    }

    fn op3(&self, op: &Op3, a: &str, b: &str, c: &str) -> String {
        match op {
            Op3::Mux(_) => format!("if {a} then {b} else {c}"),
            Op3::UpdateArray(array) => {
                format!("update {a} {} {c}", self.subscript(array, b))
            }
        }
    }

    /// A numeric conversion, spelled through the bit level.
    ///
    /// `Int`/`UInt` in Bluespec are distinct types with no coercion between
    /// them, so a cast is `pack`, resize, `unpack`. Widening sign-extends when
    /// the *source* is signed and zero-extends otherwise, which is exactly what
    /// the interpreter's `as` does and what the SMT encoding models.
    fn cast(&self, from: &Type, to: &Type, a: &str) -> String {
        let (from_width, to_width) = (render::bit_width(from), render::bit_width(to));
        let bits = if to_width > from_width {
            let extend = if from.is_signed() {
                "signExtend"
            } else {
                "zeroExtend"
            };
            format!("{extend} (pack {a})")
        } else if to_width < from_width {
            format!("truncate (pack {a})")
        } else {
            format!("pack {a}")
        };
        format!("unpack ({bits})")
    }

    /// A comparison, packing booleans so that one operator covers every ordered
    /// type.
    fn compare(&self, ty: &Type, a: &str, b: &str, operator: &str) -> String {
        if *ty == Type::Bool {
            format!("pack {a} {operator} pack {b}")
        } else {
            format!("{a} {operator} {b}")
        }
    }

    /// `if divisor == 0 then 0 else ..`, giving division by zero a value.
    ///
    /// Bluespec's own `/` and `%` agree with the IR everywhere else, including
    /// leaving the most negative integer divided by -1 where it is, so zero is
    /// the only case that needs saying.
    fn guarded_division(&self, ty: &Type, a: &str, b: &str, operator: &str) -> String {
        debug_assert!(ty.is_integral(), "division is integral");
        format!("if {b} == 0 then 0 else {a} {operator} {b}")
    }

    /// A shift that yields zero once the amount reaches the operand's width.
    ///
    /// A shift amount is an ordinary integer of any width, so it can be
    /// negative or larger than the value being shifted. Both cases are zero,
    /// which is what the interpreter computes. The comparison is done in the
    /// amount's own type: widths run from 8 to 64 and every integer type in the
    /// IR holds 64, so it never overflows.
    fn guarded_shift(&self, val: &Type, amount: &Type, a: &str, b: &str, operator: &str) -> String {
        let width = render::bit_width(val);
        let out_of_range = if amount.is_signed() {
            format!("{b} < 0 || {b} >= {width}")
        } else {
            format!("{b} >= {width}")
        };
        format!("if {out_of_range} then 0 else {a} {operator} {b}")
    }

    /// Resolves an array subscript under the configured policy.
    ///
    /// The result is always parenthesised: it lands in argument position, where
    /// Bluespec's application binds tighter than anything inside it.
    fn subscript(&self, array: &Type, index: &str) -> String {
        let len = match array {
            Type::Array { len, .. } => *len,
            other => panic!("copilot-bluespec: {other} is not an array"),
        };
        match self.index_policy {
            IndexPolicy::Wrap => format!("({index} % {len})"),
            IndexPolicy::Saturate => {
                format!("(if {index} < {len} then {index} else {})", len - 1)
            }
            // The obligation is the caller's; generated code takes it as given.
            IndexPolicy::Assume => index.to_string(),
        }
    }
}
