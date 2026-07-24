//! Lowering a specification to an SMT-LIB transition system.
//!
//! # State
//!
//! A stream buffering `n` values contributes `n` state variables, holding its
//! values at times `t ..= t + n - 1`. Stepping shifts that window along:
//!
//! ```text
//! s_i' = s_{i+1}          for i < n - 1
//! s_{n-1}' = E(state)     where E is the stream's transition expression
//! ```
//!
//! This is deliberately *not* the ring buffer the interpreter and the code
//! generators use. Both denote the same stream, but a shifting window needs no
//! modular index arithmetic, which keeps the encoding in a decidable fragment
//! and keeps the prover from reasoning about an implementation detail. It also
//! means the SMT encoding is an independent derivation of the semantics rather
//! than a transcription of one of the engines — so a disagreement between them
//! is informative.
//!
//! # Integers
//!
//! Bitvectors, which wrap natively, so the encoding inherits exactly the
//! wrapping arithmetic the rest of the project defines. Division and shifting
//! carry the same explicit guards the code generator emits, since SMT-LIB's own
//! answers at zero and past the operand width differ from ours.
//!
//! # Floats
//!
//! Either reals or IEEE floats, by [`FloatEncoding`]. Reals are fast and
//! *unsound in both directions* — they have no NaN, no infinity, no overflow
//! and no rounding — so a result computed under them is reported with a caveat
//! attached rather than as a proof. The operations reals cannot express at all
//! become uninterpreted functions, which is sound for proving and unsound for
//! counterexamples; those are flagged too.

use crate::{Caveat, Error, FloatEncoding, Settings};
use copilot_core::{ExprId, Node, Op1, Op2, Op3, Spec, StreamId, StructType, Type, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

/// The name of a stream's `i`-th buffered value at unrolling step `t`.
pub fn state_var(stream: StreamId, index: usize, step: usize) -> String {
    format!("s{}_{index}_{step}", stream.0)
}

/// The name of an external variable's sample at unrolling step `t`.
pub fn extern_var(name: &str, step: usize) -> String {
    format!("x_{name}_{step}")
}

fn node_var(expr: ExprId, step: usize) -> String {
    format!("n{}_{step}", expr.0)
}

/// A specification lowered to SMT-LIB.
pub struct Encoding<'a> {
    spec: &'a Spec,
    settings: &'a Settings,
    /// Commands emitted so far, in order.
    commands: Vec<String>,
    /// Uninterpreted functions declared, and why.
    approximations: BTreeSet<&'static str>,
    /// Struct types already declared as datatypes.
    datatypes: BTreeSet<String>,
    /// Steps whose node definitions have been emitted.
    defined_steps: BTreeSet<usize>,
    /// Whether any float was encoded at all.
    uses_floats: bool,
}

impl<'a> Encoding<'a> {
    /// Prepares an encoding, declaring struct datatypes and any needed
    /// uninterpreted functions.
    pub fn new(spec: &'a Spec, settings: &'a Settings) -> Result<Self, Error> {
        copilot_core::validate(spec)?;
        let mut encoding = Encoding {
            spec,
            settings,
            commands: Vec::new(),
            approximations: BTreeSet::new(),
            datatypes: BTreeSet::new(),
            defined_steps: BTreeSet::new(),
            uses_floats: false,
        };
        encoding.emit("(set-option :produce-models true)");
        // `ALL` rather than a computed logic name: the encoding may mix
        // bitvectors, arrays, datatypes and reals depending on the spec, and
        // naming that combination correctly for two different solvers is more
        // ways to be wrong than it is worth.
        encoding.emit("(set-logic ALL)");
        encoding.declare_structs()?;
        Ok(encoding)
    }

    /// Takes the commands emitted since the last call.
    pub fn take(&mut self) -> String {
        std::mem::take(&mut self.commands).join("\n")
    }

    /// What makes the result less than a proof.
    pub fn caveats(&self) -> Vec<Caveat> {
        let mut caveats = Vec::new();
        if self.uses_floats && self.settings.floats == FloatEncoding::Reals {
            caveats.push(Caveat::FloatsAsReals);
        }
        if !self.approximations.is_empty() {
            caveats.push(Caveat::Uninterpreted(
                self.approximations.iter().copied().collect(),
            ));
        }
        caveats
    }

    fn emit(&mut self, command: impl Into<String>) {
        self.commands.push(command.into());
    }

    // -- sorts ------------------------------------------------------------

    fn sort(&mut self, ty: &Type) -> String {
        match ty {
            Type::Bool => "Bool".into(),
            Type::Int8 | Type::Word8 => "(_ BitVec 8)".into(),
            Type::Int16 | Type::Word16 => "(_ BitVec 16)".into(),
            Type::Int32 | Type::Word32 => "(_ BitVec 32)".into(),
            Type::Int64 | Type::Word64 => "(_ BitVec 64)".into(),
            Type::Float | Type::Double => {
                self.uses_floats = true;
                match (self.settings.floats, ty) {
                    (FloatEncoding::Reals, _) => "Real".into(),
                    (FloatEncoding::Ieee, Type::Float) => "Float32".into(),
                    (FloatEncoding::Ieee, _) => "Float64".into(),
                }
            }
            // Indexed by a 32-bit vector, matching `Op2::Index`'s index type.
            Type::Array { elem, .. } => {
                let elem = self.sort(elem);
                format!("(Array (_ BitVec 32) {elem})")
            }
            Type::Struct(definition) => definition.name.clone(),
        }
    }

    fn declare_structs(&mut self) -> Result<(), Error> {
        let mut found: BTreeMap<String, StructType> = BTreeMap::new();
        for id in 0..self.spec.arena.len() {
            collect_structs(self.spec.arena.ty_of(ExprId(id as u32)), &mut found);
        }
        for (_, ty) in self.spec.arena.externs() {
            collect_structs(ty, &mut found);
        }
        for stream in &self.spec.streams {
            collect_structs(&stream.ty, &mut found);
        }

        for definition in found.values() {
            if !self.datatypes.insert(definition.name.clone()) {
                continue;
            }
            let name = &definition.name;
            let fields: Vec<String> = definition
                .fields
                .iter()
                .map(|(field, ty)| {
                    let sort = self.sort(ty);
                    format!("({name}-{field} {sort})")
                })
                .collect();
            self.emit(format!(
                "(declare-datatypes (({name} 0)) ((({name}-mk {})))",
                fields.join(" ")
            ));
            // The extra paren closes `declare-datatypes`; kept separate so the
            // nesting above stays readable.
            let last = self.commands.len() - 1;
            self.commands[last].push(')');
        }
        Ok(())
    }

    // -- values -----------------------------------------------------------

    fn literal(&mut self, ty: &Type, value: &Value) -> String {
        match value {
            Value::Bool(v) => v.to_string(),
            Value::Int8(v) => bits(*v as u8 as u128, 8),
            Value::Int16(v) => bits(*v as u16 as u128, 16),
            Value::Int32(v) => bits(*v as u32 as u128, 32),
            Value::Int64(v) => bits(*v as u64 as u128, 64),
            Value::Word8(v) => bits(*v as u128, 8),
            Value::Word16(v) => bits(*v as u128, 16),
            Value::Word32(v) => bits(*v as u128, 32),
            Value::Word64(v) => bits(*v as u128, 64),
            Value::Float(v) => self.float_literal(*v as f64, ty),
            Value::Double(v) => self.float_literal(*v, ty),
            Value::Array(values) => {
                let elem = ty.elem().expect("an array value has an array type").clone();
                let sort = self.sort(ty);
                // Built by storing each element into an arbitrary base array.
                // Elements outside the declared length are never read: every
                // subscript is resolved into range first.
                let mut term = format!("((as const {sort}) {})", {
                    let zero = zero_of(&elem);
                    self.literal(&elem, &zero)
                });
                for (index, value) in values.iter().enumerate() {
                    let value = self.literal(&elem, value);
                    term = format!("(store {term} {} {value})", bits(index as u128, 32));
                }
                term
            }
            Value::Struct { name, fields } => {
                let field_terms: Vec<String> = fields
                    .iter()
                    .map(|(field, value)| {
                        let field_ty = ty
                            .field(field)
                            .expect("a struct value has a struct type")
                            .clone();
                        self.literal(&field_ty, value)
                    })
                    .collect();
                format!("({name}-mk {})", field_terms.join(" "))
            }
        }
    }

    /// A float literal, written exactly.
    ///
    /// Never via decimal formatting: Rust prints small magnitudes in scientific
    /// notation, which SMT-LIB does not accept as a numeral at all, and even
    /// where it parses a decimal is only as exact as the digits written. Both
    /// forms below are built from the value's bits, so every finite float
    /// survives the trip precisely.
    fn float_literal(&mut self, value: f64, ty: &Type) -> String {
        self.uses_floats = true;
        match self.settings.floats {
            FloatEncoding::Ieee => {
                // `(fp sign exponent significand)` covers every case a float
                // has, subnormals, infinities and NaN included, with no special
                // handling for any of them.
                if *ty == Type::Float {
                    let pattern = (value as f32).to_bits();
                    format!(
                        "(fp {} {} {})",
                        bits(u128::from(pattern >> 31), 1),
                        bits(u128::from((pattern >> 23) & 0xFF), 8),
                        bits(u128::from(pattern & 0x007F_FFFF), 23)
                    )
                } else {
                    let pattern = value.to_bits();
                    format!(
                        "(fp {} {} {})",
                        bits(u128::from(pattern >> 63), 1),
                        bits(u128::from((pattern >> 52) & 0x7FF), 11),
                        bits(u128::from(pattern & 0x000F_FFFF_FFFF_FFFF), 52)
                    )
                }
            }
            FloatEncoding::Reals => {
                if !value.is_finite() {
                    // Reals have no infinity or NaN, so there is nothing to
                    // write. The whole result is already caveated; zero keeps
                    // the script valid and the caveat says why it is not a
                    // proof.
                    self.approximations.insert("non-finite float literal");
                    return "0.0".into();
                }
                let (negative, mantissa, exponent) = decompose(value);
                let magnitude = match exponent.cmp(&0) {
                    std::cmp::Ordering::Equal => format!("{mantissa}.0"),
                    std::cmp::Ordering::Greater => {
                        format!("(* {mantissa}.0 {})", power_of_two(exponent as u32))
                    }
                    std::cmp::Ordering::Less => {
                        format!("(/ {mantissa}.0 {})", power_of_two(-exponent as u32))
                    }
                };
                if negative {
                    format!("(- {magnitude})")
                } else {
                    magnitude
                }
            }
        }
    }

    // -- the transition system --------------------------------------------

    /// Declares the state and external variables for one unrolling step.
    pub fn declare_step(&mut self, step: usize) {
        for stream in &self.spec.streams {
            for index in 0..stream.buffer.len() {
                let sort = self.sort(&stream.ty);
                self.emit(format!(
                    "(declare-const {} {sort})",
                    state_var(stream.id, index, step)
                ));
            }
        }
        for (name, ty) in self.spec.arena.externs().to_vec() {
            let sort = self.sort(&ty);
            self.emit(format!(
                "(declare-const {} {sort})",
                extern_var(&name, step)
            ));
        }
    }

    /// Constrains step 0 to the specification's initial state.
    pub fn assert_initial(&mut self) {
        for stream in self.spec.streams.clone() {
            for (index, value) in stream.buffer.iter().enumerate() {
                let literal = self.literal(&stream.ty, value);
                self.emit(format!(
                    "(assert (= {} {literal}))",
                    state_var(stream.id, index, 0)
                ));
            }
        }
    }

    /// Relates step `from` to step `from + 1`.
    pub fn assert_transition(&mut self, from: usize) -> Result<(), Error> {
        self.define_nodes(from)?;
        for stream in self.spec.streams.clone() {
            let depth = stream.buffer.len();
            for index in 0..depth - 1 {
                self.emit(format!(
                    "(assert (= {} {}))",
                    state_var(stream.id, index, from + 1),
                    state_var(stream.id, index + 1, from)
                ));
            }
            self.emit(format!(
                "(assert (= {} {}))",
                state_var(stream.id, depth - 1, from + 1),
                node_var(stream.expr, from)
            ));
        }
        Ok(())
    }

    /// The term denoting an expression at the given step.
    pub fn term_at(&mut self, expr: ExprId, step: usize) -> Result<String, Error> {
        self.define_nodes(step)?;
        Ok(node_var(expr, step))
    }

    /// Pins an external variable to a concrete value at one step.
    ///
    /// Used to run the encoding forwards over a known trace rather than to
    /// search; see [`crate::evaluate`].
    pub fn assert_extern(&mut self, name: &str, value: &Value, step: usize) -> Result<(), Error> {
        let ty = self
            .spec
            .arena
            .externs()
            .iter()
            .find(|(declared, _)| declared == name)
            .map(|(_, ty)| ty.clone())
            .ok_or_else(|| Error::Unsupported(format!("no external variable named `{name}`")))?;
        let literal = self.literal(&ty, value);
        self.emit(format!("(assert (= {} {literal}))", extern_var(name, step)));
        Ok(())
    }

    /// Emits one definition per reachable expression at this step.
    ///
    /// Sharing the arena's structure into the script matters: a spec whose
    /// expression graph is deeply shared would otherwise be exponentially
    /// larger once written out as a tree, and the solver would see a problem
    /// far bigger than the one that was asked.
    fn define_nodes(&mut self, step: usize) -> Result<(), Error> {
        if !self.defined_steps.insert(step) {
            return Ok(());
        }
        let mut roots = self.spec.runtime_roots();
        roots.extend(self.spec.properties.iter().map(|p| p.prop.expr()));
        let reachable = copilot_core::reachable(self.spec, &roots);

        for id in reachable {
            let ty = self.spec.arena.ty_of(id).clone();
            let sort = self.sort(&ty);
            let body = self.node(id, step)?;
            self.emit(format!(
                "(define-fun {} () {sort} {body})",
                node_var(id, step)
            ));
        }
        Ok(())
    }

    fn node(&mut self, id: ExprId, step: usize) -> Result<String, Error> {
        let node = self.spec.arena.node(id).clone();
        let operand = |child: ExprId| node_var(child, step);

        Ok(match node {
            Node::Const { ty, value } => self.literal(&ty, &value),
            Node::Drop { idx, stream } => state_var(stream, idx as usize, step),
            Node::ExternVar { name, .. } => extern_var(&name, step),
            Node::Var(var) => {
                // `Local` is erased by substitution, exactly as the Rust
                // backend does: the language is pure, so a variable and its
                // definition are interchangeable.
                let bound = self.binding_of(var)?;
                operand(bound)
            }
            Node::Local { body, .. } => operand(body),
            Node::Label(_, a) => operand(a),
            Node::Op1(op, a) => self.op1(&op, &operand(a))?,
            Node::Op2(op, a, b) => self.op2(&op, &operand(a), &operand(b))?,
            Node::Op3(op, a, b, c) => self.op3(&op, &operand(a), &operand(b), &operand(c))?,
        })
    }

    fn binding_of(&self, var: copilot_core::VarId) -> Result<ExprId, Error> {
        for (_, node) in self.spec.arena.nodes() {
            if let Node::Local {
                var: bound,
                bound: definition,
                ..
            } = node
                && *bound == var
            {
                return Ok(*definition);
            }
        }
        Err(Error::Unsupported(format!("{var} is not bound")))
    }

    // -- operators ---------------------------------------------------------

    fn op1(&mut self, op: &Op1, a: &str) -> Result<String, Error> {
        Ok(match op {
            Op1::Not => format!("(not {a})"),
            Op1::BwNot(ty) if *ty == Type::Bool => format!("(not {a})"),
            Op1::BwNot(_) => format!("(bvnot {a})"),

            Op1::Abs(ty) if ty.is_floating() => match self.settings.floats {
                FloatEncoding::Reals => format!("(ite (< {a} 0.0) (- {a}) {a})"),
                FloatEncoding::Ieee => format!("(fp.abs {a})"),
            },
            // `bvneg` of the most negative value is itself, which is exactly
            // the wrapping `abs` the rest of the project defines.
            Op1::Abs(ty) if ty.is_signed() => {
                format!("(ite (bvslt {a} {}) (bvneg {a}) {a})", zero_bits(ty))
            }
            Op1::Abs(_) => a.to_string(),

            Op1::Sign(ty) if ty.is_floating() => self.uninterpreted("signum", &[ty], ty, &[a]),
            Op1::Sign(ty) if ty.is_signed() => {
                let zero = zero_bits(ty);
                let one = one_bits(ty);
                let minus_one = format!("(bvneg {one})");
                format!("(ite (bvslt {a} {zero}) {minus_one} (ite (= {a} {zero}) {zero} {one}))")
            }
            Op1::Sign(ty) => {
                let zero = zero_bits(ty);
                format!("(ite (= {a} {zero}) {zero} {})", one_bits(ty))
            }

            Op1::Recip(ty) => match self.settings.floats {
                FloatEncoding::Reals => format!("(/ 1.0 {a})"),
                FloatEncoding::Ieee => {
                    let one = self.float_literal(1.0, ty);
                    format!("(fp.div RNE {one} {a})")
                }
            },

            // Square root is exactly rounded, so IEEE mode can express it;
            // everything else transcendental cannot be expressed in either
            // theory and becomes an uninterpreted function.
            Op1::Sqrt(_) if self.settings.floats == FloatEncoding::Ieee => {
                format!("(fp.sqrt RNE {a})")
            }
            Op1::Ceiling(_) if self.settings.floats == FloatEncoding::Ieee => {
                format!("(fp.roundToIntegral RTP {a})")
            }
            Op1::Floor(_) if self.settings.floats == FloatEncoding::Ieee => {
                format!("(fp.roundToIntegral RTN {a})")
            }

            Op1::Sqrt(ty) => self.uninterpreted("sqrt", &[ty], ty, &[a]),
            Op1::Exp(ty) => self.uninterpreted("exp", &[ty], ty, &[a]),
            Op1::Log(ty) => self.uninterpreted("log", &[ty], ty, &[a]),
            Op1::Sin(ty) => self.uninterpreted("sin", &[ty], ty, &[a]),
            Op1::Cos(ty) => self.uninterpreted("cos", &[ty], ty, &[a]),
            Op1::Tan(ty) => self.uninterpreted("tan", &[ty], ty, &[a]),
            Op1::Asin(ty) => self.uninterpreted("asin", &[ty], ty, &[a]),
            Op1::Acos(ty) => self.uninterpreted("acos", &[ty], ty, &[a]),
            Op1::Atan(ty) => self.uninterpreted("atan", &[ty], ty, &[a]),
            Op1::Sinh(ty) => self.uninterpreted("sinh", &[ty], ty, &[a]),
            Op1::Cosh(ty) => self.uninterpreted("cosh", &[ty], ty, &[a]),
            Op1::Tanh(ty) => self.uninterpreted("tanh", &[ty], ty, &[a]),
            Op1::Asinh(ty) => self.uninterpreted("asinh", &[ty], ty, &[a]),
            Op1::Acosh(ty) => self.uninterpreted("acosh", &[ty], ty, &[a]),
            Op1::Atanh(ty) => self.uninterpreted("atanh", &[ty], ty, &[a]),
            Op1::Ceiling(ty) => self.uninterpreted("ceil", &[ty], ty, &[a]),
            Op1::Floor(ty) => self.uninterpreted("floor", &[ty], ty, &[a]),

            Op1::Cast { from, to } => self.cast(from, to, a),
            Op1::GetField { struct_ty, field } => {
                let name = &struct_ty
                    .as_struct()
                    .ok_or_else(|| Error::Unsupported("field access on a non-struct".into()))?
                    .name;
                format!("({name}-{field} {a})")
            }
        })
    }

    fn op2(&mut self, op: &Op2, a: &str, b: &str) -> Result<String, Error> {
        let ieee = self.settings.floats == FloatEncoding::Ieee;
        Ok(match op {
            Op2::And => format!("(and {a} {b})"),
            Op2::Or => format!("(or {a} {b})"),

            Op2::Add(ty) if ty.is_floating() => self.float_arith("add", "+", a, b),
            Op2::Sub(ty) if ty.is_floating() => self.float_arith("sub", "-", a, b),
            Op2::Mul(ty) if ty.is_floating() => self.float_arith("mul", "*", a, b),
            Op2::Add(_) => format!("(bvadd {a} {b})"),
            Op2::Sub(_) => format!("(bvsub {a} {b})"),
            Op2::Mul(_) => format!("(bvmul {a} {b})"),

            // SMT-LIB defines division by zero as all-ones, and ours as zero,
            // so the guard is not optional.
            Op2::Div(ty) => {
                let zero = zero_bits(ty);
                let op = if ty.is_signed() { "bvsdiv" } else { "bvudiv" };
                format!("(ite (= {b} {zero}) {zero} ({op} {a} {b}))")
            }
            Op2::Mod(ty) => {
                let zero = zero_bits(ty);
                // `bvsrem` takes its sign from the dividend, like Rust's `%`.
                let op = if ty.is_signed() { "bvsrem" } else { "bvurem" };
                format!("(ite (= {b} {zero}) {zero} ({op} {a} {b}))")
            }
            Op2::Fdiv(_) => self.float_arith("div", "/", a, b),

            Op2::Pow(ty) => self.uninterpreted("pow", &[ty, ty], ty, &[a, b]),
            Op2::Logb(ty) => self.uninterpreted("logb", &[ty, ty], ty, &[a, b]),
            Op2::Atan2(ty) => self.uninterpreted("atan2", &[ty, ty], ty, &[a, b]),

            Op2::Eq(ty) if ty.is_floating() && ieee => format!("(fp.eq {a} {b})"),
            Op2::Ne(ty) if ty.is_floating() && ieee => format!("(not (fp.eq {a} {b}))"),
            Op2::Eq(_) => format!("(= {a} {b})"),
            Op2::Ne(_) => format!("(not (= {a} {b}))"),

            Op2::Lt(ty) => self.compare(ty, "lt", "<", a, b),
            Op2::Le(ty) => self.compare(ty, "leq", "<=", a, b),
            Op2::Gt(ty) => self.compare(ty, "gt", ">", a, b),
            Op2::Ge(ty) => self.compare(ty, "geq", ">=", a, b),

            Op2::BwAnd(ty) if *ty == Type::Bool => format!("(and {a} {b})"),
            Op2::BwOr(ty) if *ty == Type::Bool => format!("(or {a} {b})"),
            Op2::BwXor(ty) if *ty == Type::Bool => format!("(xor {a} {b})"),
            Op2::BwAnd(_) => format!("(bvand {a} {b})"),
            Op2::BwOr(_) => format!("(bvor {a} {b})"),
            Op2::BwXor(_) => format!("(bvxor {a} {b})"),

            Op2::BwShiftL { val, amount } => self.shift("bvshl", val, amount, a, b),
            Op2::BwShiftR { val, amount } => {
                // Logical for unsigned, arithmetic for signed, matching Rust's
                // `>>` on each.
                let op = if val.is_signed() { "bvashr" } else { "bvlshr" };
                self.shift(op, val, amount, a, b)
            }

            Op2::Index(array) => {
                let index = self.subscript(array, b);
                format!("(select {a} {index})")
            }
            Op2::UpdateField { struct_ty, field } => {
                let definition = struct_ty
                    .as_struct()
                    .ok_or_else(|| Error::Unsupported("field update on a non-struct".into()))?;
                let name = definition.name.clone();
                let fields: Vec<String> = definition
                    .fields
                    .iter()
                    .map(|(other, _)| {
                        if other == field {
                            b.to_string()
                        } else {
                            format!("({name}-{other} {a})")
                        }
                    })
                    .collect();
                format!("({name}-mk {})", fields.join(" "))
            }
        })
    }

    fn op3(&mut self, op: &Op3, a: &str, b: &str, c: &str) -> Result<String, Error> {
        Ok(match op {
            Op3::Mux(_) => format!("(ite {a} {b} {c})"),
            Op3::UpdateArray(array) => {
                let index = self.subscript(array, b);
                format!("(store {a} {index} {c})")
            }
        })
    }

    fn float_arith(&mut self, ieee: &str, real: &str, a: &str, b: &str) -> String {
        self.uses_floats = true;
        match self.settings.floats {
            FloatEncoding::Reals => format!("({real} {a} {b})"),
            FloatEncoding::Ieee => format!("(fp.{ieee} RNE {a} {b})"),
        }
    }

    fn compare(&mut self, ty: &Type, ieee: &str, real: &str, a: &str, b: &str) -> String {
        if ty.is_floating() {
            self.uses_floats = true;
            return match self.settings.floats {
                FloatEncoding::Reals => format!("({real} {a} {b})"),
                FloatEncoding::Ieee => format!("(fp.{ieee} {a} {b})"),
            };
        }
        if *ty == Type::Bool {
            // `false < true`, matching the ordering the interpreter uses.
            return match real {
                "<" => format!("(and (not {a}) {b})"),
                "<=" => format!("(or (not {a}) {b})"),
                ">" => format!("(and {a} (not {b}))"),
                _ => format!("(or {a} (not {b}))"),
            };
        }
        let op = match (ty.is_signed(), real) {
            (true, "<") => "bvslt",
            (true, "<=") => "bvsle",
            (true, ">") => "bvsgt",
            (true, _) => "bvsge",
            (false, "<") => "bvult",
            (false, "<=") => "bvule",
            (false, ">") => "bvugt",
            (false, _) => "bvuge",
        };
        format!("({op} {a} {b})")
    }

    /// A shift that yields zero once the amount reaches the operand's width.
    fn shift(&mut self, op: &str, val: &Type, amount: &Type, a: &str, b: &str) -> String {
        let width = bit_width(val);
        let amount_width = bit_width(amount);
        // Widen the amount to 64 bits the same way `as u64` does, so a negative
        // amount becomes enormous and the guard rejects it — exactly what the
        // interpreter and the code generator do.
        let widened = resize(b, amount_width, 64, amount.is_signed());
        let resized = resize(b, amount_width, width, amount.is_signed());
        format!(
            "(ite (bvuge {widened} {}) {} ({op} {a} {resized}))",
            bits(width as u128, 64),
            zero_bits(val)
        )
    }

    /// Resolves an array subscript under the configured index policy.
    fn subscript(&mut self, array: &Type, index: &str) -> String {
        let len = match array {
            Type::Array { len, .. } => *len as u128,
            _ => unreachable!("copilot-theorem: subscript of a non-array"),
        };
        match self.settings.index_policy {
            copilot_core::IndexPolicy::Wrap => {
                format!("(bvurem {index} {})", bits(len, 32))
            }
            copilot_core::IndexPolicy::Saturate => format!(
                "(ite (bvult {index} {}) {index} {})",
                bits(len, 32),
                bits(len - 1, 32)
            ),
            copilot_core::IndexPolicy::Assume => index.to_string(),
        }
    }

    fn cast(&mut self, from: &Type, to: &Type, a: &str) -> String {
        if from == to {
            return a.to_string();
        }
        if to.is_floating() || from.is_floating() {
            // Converting between a bitvector and a float needs the theory
            // combination the two solvers spell differently; an uninterpreted
            // function is sound for proving and honest about the rest.
            return self.uninterpreted("cast", &[from], to, &[a]);
        }
        let source = bit_width(from);
        let target = bit_width(to);
        resize(a, source, target, from.is_signed())
    }

    /// Declares an uninterpreted function and applies it.
    ///
    /// Sound for proving: a property that holds for every interpretation holds
    /// for the real one. Not sound for refuting — a counterexample may pick an
    /// interpretation the actual function does not have — which is why this
    /// shows up as a caveat on the result.
    fn uninterpreted(
        &mut self,
        name: &'static str,
        arguments: &[&Type],
        result: &Type,
        terms: &[&str],
    ) -> String {
        let mut symbol = format!("uf_{name}");
        for ty in arguments {
            let _ = write!(symbol, "_{}", type_tag(ty));
        }
        let _ = write!(symbol, "_to_{}", type_tag(result));

        if self.approximations.insert(name) || !self.datatypes.contains(&symbol) {
            self.datatypes.insert(symbol.clone());
            let argument_sorts: Vec<String> = arguments.iter().map(|ty| self.sort(ty)).collect();
            let result_sort = self.sort(result);
            self.emit(format!(
                "(declare-fun {symbol} ({}) {result_sort})",
                argument_sorts.join(" ")
            ));
        }
        format!("({symbol} {})", terms.join(" "))
    }
}

fn collect_structs(ty: &Type, found: &mut BTreeMap<String, StructType>) {
    match ty {
        Type::Array { elem, .. } => collect_structs(elem, found),
        Type::Struct(definition) => {
            for (_, field) in &definition.fields {
                collect_structs(field, found);
            }
            found.insert(definition.name.clone(), (**definition).clone());
        }
        _ => {}
    }
}

/// A bitvector literal, written in binary so both solvers read it the same way.
fn bits(value: u128, width: u32) -> String {
    let mut digits = String::with_capacity(width as usize + 2);
    digits.push_str("#b");
    for position in (0..width).rev() {
        digits.push(if value >> position & 1 == 1 { '1' } else { '0' });
    }
    digits
}

fn bit_width(ty: &Type) -> u32 {
    match ty {
        Type::Int8 | Type::Word8 => 8,
        Type::Int16 | Type::Word16 => 16,
        Type::Int32 | Type::Word32 => 32,
        Type::Int64 | Type::Word64 => 64,
        other => unreachable!("copilot-theorem: {other} has no bit width"),
    }
}

fn zero_bits(ty: &Type) -> String {
    bits(0, bit_width(ty))
}

fn one_bits(ty: &Type) -> String {
    bits(1, bit_width(ty))
}

fn zero_of(ty: &Type) -> Value {
    match ty {
        Type::Bool => Value::Bool(false),
        Type::Int8 => Value::Int8(0),
        Type::Int16 => Value::Int16(0),
        Type::Int32 => Value::Int32(0),
        Type::Int64 => Value::Int64(0),
        Type::Word8 => Value::Word8(0),
        Type::Word16 => Value::Word16(0),
        Type::Word32 => Value::Word32(0),
        Type::Word64 => Value::Word64(0),
        Type::Float => Value::Float(0.0),
        Type::Double => Value::Double(0.0),
        Type::Array { elem, len } => Value::Array(vec![zero_of(elem); *len]),
        Type::Struct(definition) => Value::Struct {
            name: definition.name.clone(),
            fields: definition
                .fields
                .iter()
                .map(|(field, ty)| (field.clone(), zero_of(ty)))
                .collect(),
        },
    }
}

/// Widens or narrows a bitvector term, the way Rust's `as` does.
fn resize(term: &str, from: u32, to: u32, signed: bool) -> String {
    match to.cmp(&from) {
        std::cmp::Ordering::Equal => term.to_string(),
        std::cmp::Ordering::Less => format!("((_ extract {} 0) {term})", to - 1),
        std::cmp::Ordering::Greater => {
            let extend = if signed { "sign_extend" } else { "zero_extend" };
            format!("((_ {extend} {}) {term})", to - from)
        }
    }
}

fn type_tag(ty: &Type) -> String {
    match ty {
        Type::Array { elem, len } => format!("arr{len}_{}", type_tag(elem)),
        Type::Struct(definition) => definition.name.clone(),
        other => other.to_string().to_lowercase(),
    }
}

/// Splits a float into `± mantissa * 2^exponent`, exactly.
///
/// Trailing zeros are stripped from the mantissa so the exponent stays as close
/// to zero as the value allows, which keeps the emitted literal short.
fn decompose(value: f64) -> (bool, u64, i32) {
    let bits = value.to_bits();
    let negative = bits >> 63 == 1;
    let biased = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & 0x000F_FFFF_FFFF_FFFF;

    let (mut mantissa, mut exponent) = if biased == 0 {
        // Subnormal: no implicit leading one.
        (fraction, -1074)
    } else {
        (fraction | (1 << 52), biased - 1075)
    };
    while mantissa != 0 && mantissa % 2 == 0 {
        mantissa /= 2;
        exponent += 1;
    }
    if mantissa == 0 {
        exponent = 0;
    }
    (negative, mantissa, exponent)
}

/// `2^exponent` as a real literal, for any exponent a float can carry.
///
/// Written as a product of factors that each fit in a `u128`, since `2^1074` —
/// the smallest subnormal's denominator — does not, and SMT-LIB has no reliable
/// exponentiation on reals to fall back on.
fn power_of_two(exponent: u32) -> String {
    const CHUNK: u32 = 100;
    let mut factors = Vec::new();
    let mut remaining = exponent;
    while remaining > CHUNK {
        factors.push(format!("{}.0", 1u128 << CHUNK));
        remaining -= CHUNK;
    }
    factors.push(format!("{}.0", 1u128 << remaining));
    if factors.len() == 1 {
        factors.pop().expect("just pushed one")
    } else {
        format!("(* {})", factors.join(" "))
    }
}
