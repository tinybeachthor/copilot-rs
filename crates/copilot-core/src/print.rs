//! A human-readable rendering of a [`Spec`].
//!
//! Three callers motivate this, and all three constrain the format the same
//! way: it must show what was *written*, not what the arena stores.
//!
//! - **Reading what `copilot!` produced.** The macro desugars invisibly and
//!   hash-consing rewrites structure, so the arena alone does not look like
//!   the source that built it.
//! - **Counterexamples.** `copilot-theorem` replays a failing trace through the
//!   interpreter; printing the property beside it turns a list of `Value`s
//!   into something a person reads against the claim that failed.
//! - **Review.** The golden `.rs` and `.bs` files show what a *backend* did;
//!   nothing shows what the frontend built.
//!
//! # Format
//!
//! `Drop { idx, stream }` prints as `drop idx stream` (`stream`'s `Display`
//! already renders `s0`, `s1`, ...). Struct and array literals print as
//! [`Value`]'s own `Display` already writes them — `Point { x: 1, y: 2 }`,
//! `[0, 1, 2]` — so a literal reads exactly as it would have been written.
//!
//! Hash-consing means one arena node can be reachable from many places — the
//! same subexpression read by a stream, an observer, and a trigger guard is
//! one [`ExprId`], not three copies. Printing it three times would hide that
//! sharing and, for a large spec, would blow up the size of the printed
//! text relative to the arena that produced it. So a node used more than
//! once is named — `let t7 = ...;` — and printed once; everywhere else it is
//! referenced by name. A [`Node::Label`] is named after the label instead of
//! a synthetic `t`-number, since that name was chosen by whoever wrote the
//! spec. A [`Node::Local`] is always named too, regardless of how many times
//! it is used — it exists only because a frontend asked for a binding to
//! appear, so the printer honours that even when hash-consing alone would
//! not have shared the node.
//!
//! # Non-goal
//!
//! Round-tripping through a parser is explicitly not attempted. That would be
//! a second frontend to keep in sync with the builder and the `copilot!`
//! macro, and the plan already has one frontend too many for that. This
//! module only writes; nothing reads it back.

use crate::expr::{ExprId, Node, VarId};
use crate::op::{Op1, Op2, Op3};
use crate::spec::{Prop, Spec};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let roots = self.all_roots();
        Printer::new(self, &roots).write(f)
    }
}

/// Renders one expression, choosing names fresh from its own subtree.
///
/// Independent of [`Spec`]'s own [`Display`](fmt::Display) impl: the same
/// subexpression may be named differently here than it would be inside a
/// whole specification's printout, because naming only looks at what `id`
/// itself reaches. Meant for showing one property or one counterexample's
/// claim on its own, not as a substitute for printing the whole spec.
pub fn format_expr(spec: &Spec, id: ExprId) -> String {
    let printer = Printer::new(spec, &[id]);
    let mut out = String::new();
    for (&named_id, name) in &printer.named {
        out.push_str(&format!(
            "let {name} = {};\n",
            printer.render_node(named_id)
        ));
    }
    out.push_str(&printer.render(id));
    out
}

/// Which expressions get a name, and what each is called.
///
/// Kept as a `BTreeMap` rather than a `HashMap`: iterating it visits names in
/// `ExprId` order, which is arena interning order, which is topological order
/// — every named expression's own definition only ever refers to names that
/// come earlier in that order, so printing the map in key order is exactly
/// printing each `let` after everything it depends on.
struct Printer<'a> {
    spec: &'a Spec,
    named: BTreeMap<ExprId, String>,
    var_bindings: HashMap<VarId, ExprId>,
}

impl<'a> Printer<'a> {
    fn new(spec: &'a Spec, roots: &[ExprId]) -> Self {
        let arena = &spec.arena;

        // `var_bindings` is a lookup table, not something printed directly,
        // so it is built from the whole arena — a `Var` reached from `roots`
        // must resolve regardless of where its `Local` sits. `named`, by
        // contrast, is exactly what gets a `let` line, so every rule that
        // populates it is restricted to `reached`: `format_expr` scopes a
        // Printer to one small subtree, and a label or binding anywhere else
        // in the spec's arena must not leak a prelude line into that output.
        let mut var_bindings = HashMap::new();
        for (_, node) in arena.nodes() {
            if let Node::Local { var, bound, .. } = node {
                var_bindings.insert(*var, *bound);
            }
        }

        let reached = crate::analysis::reachable(spec, roots);
        let reached_set: HashSet<ExprId> = reached.iter().copied().collect();

        let mut named: BTreeMap<ExprId, String> = BTreeMap::new();
        for (&var, &bound) in &var_bindings {
            if reached_set.contains(&bound) {
                named.insert(bound, format!("v{}", var.0));
            }
        }
        for &id in &reached {
            if let Node::Label(name, _) = arena.node(id) {
                named.insert(id, name.clone());
            }
        }

        // A node used more than once — as an operand somewhere, or as more
        // than one root — is worth naming so it is printed once instead of
        // being duplicated at every use site.
        let mut uses: HashMap<ExprId, usize> = HashMap::new();
        for &root in roots {
            *uses.entry(root).or_default() += 1;
        }
        for &id in &reached {
            arena.node(id).for_each_child(|child| {
                if reached_set.contains(&child) {
                    *uses.entry(child).or_default() += 1;
                }
            });
        }
        for &id in &reached {
            if named.contains_key(&id) {
                continue;
            }
            let composite = matches!(
                arena.node(id),
                Node::Op1(..) | Node::Op2(..) | Node::Op3(..) | Node::Local { .. }
            );
            if composite && uses.get(&id).copied().unwrap_or(0) > 1 {
                named.insert(id, format!("t{}", id.0));
            }
        }

        Printer {
            spec,
            named,
            var_bindings,
        }
    }

    /// An expression's text in a context that needs no enclosing parentheses
    /// of its own — the right-hand side of a `let`, `++`, `observe`, or
    /// `when`.
    fn render(&self, id: ExprId) -> String {
        match self.named.get(&id) {
            Some(name) => name.clone(),
            None => self.render_node(id),
        }
    }

    /// An expression's text as an operand: parenthesized if it is a compound
    /// expression being inlined, bare if it is already a name or a leaf.
    ///
    /// Precedence is not tracked beyond this — every inlined compound operand
    /// is wrapped, whether or not the parentheses turn out to be necessary.
    /// Slightly more verbose than a precedence-aware printer, but never
    /// ambiguous, which matters more for a format nothing parses back.
    fn atom(&self, id: ExprId) -> String {
        if let Some(name) = self.named.get(&id) {
            return name.clone();
        }
        match self.spec.arena.node(id) {
            Node::Const { .. } | Node::Drop { .. } | Node::ExternVar { .. } | Node::Var(_) => {
                self.render_node(id)
            }
            Node::Local { body, .. } => self.atom(*body),
            Node::Label(_, inner) => self.atom(*inner),
            _ => format!("({})", self.render_node(id)),
        }
    }

    /// The formula defining `id`, ignoring whether `id` itself is named — the
    /// text that goes on the right of that node's own `let`, or is inlined
    /// when nothing named it.
    fn render_node(&self, id: ExprId) -> String {
        match self.spec.arena.node(id) {
            Node::Const { value, .. } => value.to_string(),
            Node::Drop { idx, stream } => format!("drop {idx} {stream}"),
            Node::ExternVar { name, .. } => name.clone(),
            Node::Var(var) => match self.var_bindings.get(var) {
                Some(&bound) => self.render(bound),
                // Only reachable from a spec that failed `wellformed`.
                None => format!("v{}", var.0),
            },
            Node::Local { body, .. } => self.render(*body),
            Node::Label(_, inner) => self.render(*inner),
            Node::Op1(op, a) => self.op1(op, *a),
            Node::Op2(op, a, b) => self.op2(op, *a, *b),
            Node::Op3(op, a, b, c) => self.op3(op, *a, *b, *c),
        }
    }

    fn op1(&self, op: &Op1, a: ExprId) -> String {
        let a = self.atom(a);
        match op {
            Op1::Not | Op1::BwNot(_) => format!("!{a}"),
            Op1::Abs(_) => format!("{a}.abs()"),
            Op1::Sign(_) => format!("{a}.signum()"),
            Op1::Recip(_) => format!("{a}.recip()"),
            Op1::Exp(_) => format!("{a}.exp()"),
            Op1::Sqrt(_) => format!("{a}.sqrt()"),
            Op1::Log(_) => format!("{a}.ln()"),
            Op1::Sin(_) => format!("{a}.sin()"),
            Op1::Tan(_) => format!("{a}.tan()"),
            Op1::Cos(_) => format!("{a}.cos()"),
            Op1::Asin(_) => format!("{a}.asin()"),
            Op1::Atan(_) => format!("{a}.atan()"),
            Op1::Acos(_) => format!("{a}.acos()"),
            Op1::Sinh(_) => format!("{a}.sinh()"),
            Op1::Tanh(_) => format!("{a}.tanh()"),
            Op1::Cosh(_) => format!("{a}.cosh()"),
            Op1::Asinh(_) => format!("{a}.asinh()"),
            Op1::Atanh(_) => format!("{a}.atanh()"),
            Op1::Acosh(_) => format!("{a}.acosh()"),
            Op1::Ceiling(_) => format!("{a}.ceil()"),
            Op1::Floor(_) => format!("{a}.floor()"),
            Op1::Cast { to, .. } => format!("{a} as {to}"),
            Op1::GetField { field, .. } => format!("{a}.{field}"),
        }
    }

    fn op2(&self, op: &Op2, a: ExprId, b: ExprId) -> String {
        let a = self.atom(a);
        let b = self.atom(b);
        match op {
            Op2::And => format!("{a} && {b}"),
            Op2::Or => format!("{a} || {b}"),
            Op2::Add(_) => format!("{a} + {b}"),
            Op2::Sub(_) => format!("{a} - {b}"),
            Op2::Mul(_) => format!("{a} * {b}"),
            Op2::Mod(_) => format!("{a} % {b}"),
            // `Div` and `Fdiv` are the same source syntax at two different
            // types; the type tag, not the token, is what tells them apart.
            Op2::Div(_) | Op2::Fdiv(_) => format!("{a} / {b}"),
            Op2::Pow(_) => format!("{a}.powf({b})"),
            Op2::Logb(_) => format!("{a}.log({b})"),
            Op2::Atan2(_) => format!("{a}.atan2({b})"),
            Op2::Eq(_) => format!("{a} == {b}"),
            Op2::Ne(_) => format!("{a} != {b}"),
            Op2::Le(_) => format!("{a} <= {b}"),
            Op2::Ge(_) => format!("{a} >= {b}"),
            Op2::Lt(_) => format!("{a} < {b}"),
            Op2::Gt(_) => format!("{a} > {b}"),
            Op2::BwAnd(_) => format!("{a} & {b}"),
            Op2::BwOr(_) => format!("{a} | {b}"),
            Op2::BwXor(_) => format!("{a} ^ {b}"),
            Op2::BwShiftL { .. } => format!("{a} << {b}"),
            Op2::BwShiftR { .. } => format!("{a} >> {b}"),
            Op2::Index(_) => format!("{a}[{b}]"),
            Op2::UpdateField { field, .. } => format!("{a}.with_field(\"{field}\", {b})"),
        }
    }

    fn op3(&self, op: &Op3, a: ExprId, b: ExprId, c: ExprId) -> String {
        let a = self.atom(a);
        let b = self.atom(b);
        let c = self.atom(c);
        match op {
            Op3::Mux(_) => format!("{a}.mux({b}, {c})"),
            Op3::UpdateArray(_) => format!("{a}.update({b}, {c})"),
        }
    }

    fn write(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut sections = Vec::new();

        let externs: String = self
            .spec
            .arena
            .externs()
            .iter()
            .map(|(name, ty)| format!("extern {name}: {ty};\n"))
            .collect();
        if !externs.is_empty() {
            sections.push(externs);
        }

        let lets: String = self
            .named
            .iter()
            .map(|(&id, name)| format!("let {name} = {};\n", self.render_node(id)))
            .collect();
        if !lets.is_empty() {
            sections.push(lets);
        }

        let streams: String = self
            .spec
            .streams
            .iter()
            .map(|stream| {
                let init = stream
                    .buffer
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "stream {}: {} = [{init}] ++ {};\n",
                    stream.id,
                    stream.ty,
                    self.render(stream.expr)
                )
            })
            .collect();
        if !streams.is_empty() {
            sections.push(streams);
        }

        let observers: String = self
            .spec
            .observers
            .iter()
            .map(|o| format!("observe {} = {};\n", o.name, self.render(o.expr)))
            .collect();
        if !observers.is_empty() {
            sections.push(observers);
        }

        let triggers: String = self
            .spec
            .triggers
            .iter()
            .map(|t| {
                let args = t
                    .args
                    .iter()
                    .map(|a| self.render(a.expr))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "trigger {}({args}) when {};\n",
                    t.name,
                    self.render(t.guard)
                )
            })
            .collect();
        if !triggers.is_empty() {
            sections.push(triggers);
        }

        let properties: String = self
            .spec
            .properties
            .iter()
            .map(|p| {
                let (quantifier, expr) = match &p.prop {
                    Prop::Forall(e) => ("", *e),
                    Prop::Exists(e) => ("exists ", *e),
                };
                format!("property {quantifier}{} = {};\n", p.name, self.render(expr))
            })
            .collect();
        if !properties.is_empty() {
            sections.push(properties);
        }

        write!(f, "{}", sections.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arena, Op1, Op2, Op3, Type, Typed, Value};

    /// `counter = [0] ++ (counter + 1)`
    fn counter() -> Spec {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let one = arena.constant(Type::Word64, 1u64.lift()).unwrap();
        let next = arena.op2(Op2::Add(Type::Word64), current, one).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word64(0)], next)
            .unwrap();
        spec
    }

    #[test]
    fn a_counter_reads_back_as_a_drop_and_an_add() {
        let text = counter().to_string();
        assert_eq!(text, "stream s0: Word64 = [0] ++ drop 0 s0 + 1;\n");
    }

    /// A subexpression read from three places is named once and referenced by
    /// name everywhere else, rather than being printed three times.
    #[test]
    fn a_shared_subexpression_is_named_once() {
        let mut arena = Arena::new();
        let f = Type::Float;
        let raw = arena.extern_var("temperature", f.clone()).unwrap();
        let scale = arena.constant(f.clone(), 0.5f32.lift()).unwrap();
        let celsius = arena.op2(Op2::Mul(f.clone()), raw, scale).unwrap();
        let low = arena.constant(f.clone(), 18.0f32.lift()).unwrap();
        let too_cold = arena.op2(Op2::Lt(f), celsius, low).unwrap();

        let on = arena.declare_stream(Type::Bool, 1).unwrap();
        let was_on = arena.drop_(0, on).unwrap();
        let next_on = arena
            .op3(Op3::Mux(Type::Bool), too_cold, was_on, was_on)
            .unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(on, vec![Value::Bool(false)], next_on)
            .unwrap();
        spec.observe("celsius", celsius).unwrap();
        spec.trigger("heat_on", too_cold, [celsius]).unwrap();
        spec.validate().unwrap();

        let text = spec.to_string();
        // `celsius` (t2) is read by the observer, the trigger guard's operand,
        // and the trigger's argument; `too_cold` (t4) is read by the trigger
        // guard and the mux. Each is named once in a prelude and referenced
        // by name at every other use, never re-derived.
        assert_eq!(
            text,
            "extern temperature: Float;\n\
             \n\
             let t2 = temperature * 0.5;\n\
             let t4 = t2 < 18.0;\n\
             \n\
             stream s0: Bool = [false] ++ t4.mux(drop 0 s0, drop 0 s0);\n\
             \n\
             observe celsius = t2;\n\
             \n\
             trigger heat_on(t2) when t4;\n"
        );
    }

    /// A [`Node::Label`] is named after the label text, not a synthetic
    /// number, and is shown even when it is read only once.
    #[test]
    fn a_label_is_named_after_itself() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word32, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let labelled = arena.label("doubled", current);
        let two = arena.constant(Type::Word32, 2u32.lift()).unwrap();
        let next = arena.op2(Op2::Mul(Type::Word32), labelled, two).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word32(1)], next)
            .unwrap();
        spec.validate().unwrap();

        let text = spec.to_string();
        assert_eq!(
            text,
            "let doubled = drop 0 s0;\n\
             \n\
             stream s0: Word32 = [1] ++ doubled * 2;\n"
        );
    }

    /// A [`Node::Local`] is named after its variable and shown even though
    /// nothing reads it back — printing exposes exactly the shape
    /// `crates/copilot-rust/tests/support/mod.rs::locals` was written to
    /// exercise: a reachable binding whose variable is never used.
    #[test]
    fn an_unused_local_still_prints_as_a_let() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word32, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let seven = arena.constant(Type::Word32, 7u32.lift()).unwrap();
        let bound = arena.op2(Op2::Mul(Type::Word32), current, seven).unwrap();
        let var = arena.declare_local(Type::Word32);
        let zero = arena.constant(Type::Word32, 0u32.lift()).unwrap();
        let local = arena.local(var, bound, zero).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word32(0)], local)
            .unwrap();
        spec.validate().unwrap();

        let text = spec.to_string();
        assert_eq!(
            text,
            "let v0 = drop 0 s0 * 7;\n\
             \n\
             stream s0: Word32 = [0] ++ 0;\n"
        );
    }

    /// Struct and array literals print as written, reusing [`Value`]'s own
    /// `Display` rather than a second rendering.
    #[test]
    fn struct_and_array_literals_print_as_written() {
        let point = Type::structure(
            "Point",
            [("x".into(), Type::Int32), ("y".into(), Type::Int32)],
        );
        let origin = Value::Struct {
            name: "Point".into(),
            fields: vec![("x".into(), Value::Int32(0)), ("y".into(), Value::Int32(0))],
        };

        let mut arena = Arena::new();
        let id = arena.declare_stream(point.clone(), 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let x = arena
            .op1(
                Op1::GetField {
                    struct_ty: point,
                    field: "x".into(),
                },
                current,
            )
            .unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![origin], current).unwrap();
        spec.observe("x", x).unwrap();
        spec.validate().unwrap();

        assert_eq!(
            spec.to_string(),
            "stream s0: Point = [Point { x: 0, y: 0 }] ++ drop 0 s0;\n\
             \n\
             observe x = drop 0 s0.x;\n"
        );
    }

    /// An inlined compound operand is parenthesized, even where it would not
    /// strictly need to be — unambiguous over pretty, since nothing parses
    /// this back.
    #[test]
    fn compound_operands_are_parenthesized() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word32, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let one = arena.constant(Type::Word32, 1u32.lift()).unwrap();
        let sum = arena.op2(Op2::Add(Type::Word32), current, one).unwrap();
        let two = arena.constant(Type::Word32, 2u32.lift()).unwrap();
        let product = arena.op2(Op2::Mul(Type::Word32), sum, two).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word32(0)], product)
            .unwrap();
        spec.validate().unwrap();

        assert_eq!(
            spec.to_string(),
            "stream s0: Word32 = [0] ++ (drop 0 s0 + 1) * 2;\n"
        );
    }

    /// `format_expr` names only from the given expression's own subtree, so
    /// it stays legible when handed a single property in isolation.
    #[test]
    fn format_expr_names_only_from_its_own_subtree() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let limit = arena.constant(Type::Word64, 100u64.lift()).unwrap();
        let bounded = arena.op2(Op2::Lt(Type::Word64), current, limit).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word64(0)], current)
            .unwrap();
        spec.property("bounded", Prop::Forall(bounded)).unwrap();
        spec.validate().unwrap();

        assert_eq!(format_expr(&spec, bounded), "drop 0 s0 < 100");
    }

    #[test]
    fn a_forall_and_an_exists_property_are_distinguished() {
        let mut spec = counter();
        let limit = spec.arena.constant(Type::Word64, 10u64.lift()).unwrap();
        let current = spec.streams[0].expr;
        let below = spec
            .arena
            .op2(Op2::Lt(Type::Word64), current, limit)
            .unwrap();
        spec.property("below_ten", Prop::Forall(below)).unwrap();
        spec.property("reaches_ten", Prop::Exists(below)).unwrap();
        spec.validate().unwrap();

        let text = spec.to_string();
        assert!(text.contains("property below_ten = "));
        assert!(text.contains("property exists reaches_ten = "));
    }
}
