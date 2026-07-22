//! Validation.
//!
//! Two passes, deliberately independent of the code that builds specs:
//!
//! - [`wellformed`] checks structure — buffers, names, IDs, and the agreement
//!   between a spec's redundant records of a type and the arena's.
//! - [`typecheck`] re-derives every expression's type from scratch and compares
//!   it against the type the arena cached when the node was interned.
//!
//! Building an ill-typed node through [`Arena`] is already impossible, so on a
//! spec produced by the frontend these passes should never fail. They exist for
//! everything else: hand-built IR, deserialized IR, IR produced by a future
//! macro frontend, and IR mutated by an optimization pass. They are also what a
//! backend and the verifier are entitled to assume, so it matters that they do
//! not simply re-run the construction logic and agree with it by construction.

use crate::error::{Error, Result};
use crate::expr::{Arena, ExprId, Node, StreamId, VarId};
use crate::spec::Spec;
use crate::ty::Type;
use std::collections::{BTreeSet, HashSet};

/// Runs [`wellformed`] then [`typecheck`].
pub fn validate(spec: &Spec) -> Result<()> {
    wellformed(spec)?;
    typecheck(spec)
}

/// Checks a spec's structure, independently of its types.
pub fn wellformed(spec: &Spec) -> Result<()> {
    let arena = &spec.arena;

    if spec.streams.len() != arena.stream_decls().len() {
        // A stream was declared on the arena but never given a definition, so
        // some expression reads a stream that never advances.
        return Err(Error::UnknownStream(StreamId(spec.streams.len() as u32)));
    }

    for (position, stream) in spec.streams.iter().enumerate() {
        if stream.id.index() != position {
            return Err(Error::StreamIdMismatch {
                position,
                found: stream.id,
            });
        }
        let decl = arena.stream_decl(stream.id)?;
        if stream.buffer.is_empty() {
            return Err(Error::EmptyBuffer(stream.id));
        }
        if stream.buffer.len() != decl.buffer_len {
            return Err(Error::BufferLength {
                stream: stream.id,
                expected: decl.buffer_len,
                found: stream.buffer.len(),
            });
        }
        if stream.ty != decl.ty {
            return Err(Error::Mismatch {
                context: format!("stream {} type", stream.id),
                expected: decl.ty.clone(),
                found: stream.ty.clone(),
            });
        }
        stream.ty.validate()?;
        for (i, value) in stream.buffer.iter().enumerate() {
            if !value.matches(&stream.ty) {
                return Err(Error::Mismatch {
                    context: format!("stream {} initial value {i}", stream.id),
                    expected: stream.ty.clone(),
                    found: stream.ty.clone(),
                });
            }
        }
    }

    let mut names = HashSet::new();
    for observer in &spec.observers {
        check_ident("observer", &observer.name)?;
        if !names.insert(("observer", observer.name.as_str())) {
            return Err(Error::DuplicateName {
                kind: "observer",
                name: observer.name.clone(),
            });
        }
        observer.ty.validate()?;
    }
    for trigger in &spec.triggers {
        check_ident("trigger", &trigger.name)?;
        if !names.insert(("trigger", trigger.name.as_str())) {
            return Err(Error::DuplicateName {
                kind: "trigger",
                name: trigger.name.clone(),
            });
        }
        for arg in &trigger.args {
            arg.ty.validate()?;
        }
    }
    for property in &spec.properties {
        check_ident("property", &property.name)?;
        if !names.insert(("property", property.name.as_str())) {
            return Err(Error::DuplicateName {
                kind: "property",
                name: property.name.clone(),
            });
        }
    }
    for (name, ty) in arena.externs() {
        check_ident("extern", name)?;
        ty.validate()?;
    }

    Ok(())
}

/// Re-derives every expression's type and checks it against the arena's cache.
///
/// Runs as a single forward pass. The arena interns children before parents, so
/// by the time a node is reached its children's types are already recomputed —
/// no recursion, no memo table, and the pass doubles as a check that the
/// monotonicity invariant actually holds.
pub fn typecheck(spec: &Spec) -> Result<()> {
    let arena = &spec.arena;
    let mut recomputed: Vec<Type> = Vec::with_capacity(arena.len());

    for (id, node) in arena.nodes() {
        let mut earliest_violation = None;
        node.for_each_child(|child| {
            if child >= id && earliest_violation.is_none() {
                earliest_violation = Some(child);
            }
        });
        if let Some(child) = earliest_violation {
            return Err(Error::NonMonotonicArena { parent: id, child });
        }

        let ty = recompute(arena, node, &recomputed)?;
        let cached = arena.ty_of(id);
        if ty != *cached {
            return Err(Error::TypeDrift {
                expr: id,
                cached: cached.clone(),
                recomputed: ty,
            });
        }
        recomputed.push(ty);
    }

    check_scopes(spec)?;

    // The spec's own record of each type must agree with the arena's. These are
    // the fields backends read, so a drift here would be silently miscompiled.
    for stream in &spec.streams {
        expect(&stream.ty, arena.ty_of(stream.expr), || {
            format!("stream {} transition expression", stream.id)
        })?;
    }
    for observer in &spec.observers {
        expect(&observer.ty, arena.ty_of(observer.expr), || {
            format!("observer `{}`", observer.name)
        })?;
    }
    for trigger in &spec.triggers {
        let guard_ty = arena.ty_of(trigger.guard);
        if *guard_ty != Type::Bool {
            return Err(Error::NonBoolGuard {
                trigger: trigger.name.clone(),
                found: guard_ty.clone(),
            });
        }
        for (i, arg) in trigger.args.iter().enumerate() {
            expect(&arg.ty, arena.ty_of(arg.expr), || {
                format!("trigger `{}` argument {i}", trigger.name)
            })?;
        }
    }
    for property in &spec.properties {
        expect(&Type::Bool, arena.ty_of(property.prop.expr()), || {
            format!("property `{}`", property.name)
        })?;
    }

    Ok(())
}

/// Recomputes one node's type from its children's recomputed types.
fn recompute(arena: &Arena, node: &Node, so_far: &[Type]) -> Result<Type> {
    let child = |id: ExprId| &so_far[id.index()];
    match node {
        Node::Const { ty, value } => {
            ty.validate()?;
            if !value.matches(ty) {
                return Err(Error::BadConstant { ty: ty.clone() });
            }
            Ok(ty.clone())
        }
        Node::Drop { idx, stream } => {
            let decl = arena.stream_decl(*stream)?;
            if *idx as usize >= decl.buffer_len {
                return Err(Error::DropOutOfRange {
                    stream: *stream,
                    idx: *idx,
                    buffer_len: decl.buffer_len,
                });
            }
            Ok(decl.ty.clone())
        }
        Node::ExternVar { name, ty } => {
            ty.validate()?;
            match arena.externs().iter().find(|(n, _)| n == name) {
                Some((_, declared)) if declared != ty => Err(Error::ExternConflict {
                    name: name.clone(),
                    first: declared.clone(),
                    second: ty.clone(),
                }),
                Some(_) => Ok(ty.clone()),
                None => Err(Error::ExternConflict {
                    name: name.clone(),
                    first: ty.clone(),
                    second: ty.clone(),
                }),
            }
        }
        Node::Var(var) => arena.local_ty(*var).cloned(),
        Node::Local { var, bound, body } => {
            let declared = arena.local_ty(*var)?;
            expect(declared, child(*bound), || format!("local binding {var}"))?;
            Ok(child(*body).clone())
        }
        Node::Op1(op, a) => op.result_ty(child(*a)),
        Node::Op2(op, a, b) => op.result_ty(child(*a), child(*b)),
        Node::Op3(op, a, b, c) => op.result_ty(child(*a), child(*b), child(*c)),
        Node::Label(_, a) => Ok(child(*a).clone()),
    }
}

/// Checks that no root expression has a free local variable.
///
/// Computed as free-variable sets in one forward pass rather than by walking
/// each root with a scope stack: with hash-consing a subexpression can be
/// reached along many paths, and a naive scoped walk revisits it once per path.
fn check_scopes(spec: &Spec) -> Result<()> {
    let arena = &spec.arena;
    if arena.local_count() == 0 {
        return Ok(());
    }

    let mut free: Vec<BTreeSet<VarId>> = Vec::with_capacity(arena.len());
    for (_, node) in arena.nodes() {
        let mut set = BTreeSet::new();
        match node {
            Node::Var(var) => {
                set.insert(*var);
            }
            Node::Local { var, bound, body } => {
                set.extend(free[bound.index()].iter().copied());
                set.extend(free[body.index()].iter().copied().filter(|v| v != var));
            }
            other => other.for_each_child(|c| set.extend(free[c.index()].iter().copied())),
        }
        free.push(set);
    }

    for root in spec.all_roots() {
        if let Some(&var) = free[root.index()].iter().next() {
            return Err(Error::UnknownVar(var));
        }
    }
    Ok(())
}

fn expect(expected: &Type, found: &Type, context: impl FnOnce() -> String) -> Result<()> {
    if expected == found {
        Ok(())
    } else {
        Err(Error::Mismatch {
            context: context(),
            expected: expected.clone(),
            found: found.clone(),
        })
    }
}

/// Accepts names that are valid identifiers in every backend language we
/// target: ASCII, not starting with a digit.
fn check_ident(kind: &'static str, name: &str) -> Result<()> {
    let mut chars = name.chars();
    let valid = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(Error::BadName {
            kind,
            name: name.to_string(),
        })
    }
}

#[cfg(test)]
mod corruption {
    //! What validation catches once the arena's invariants are broken.
    //!
    //! Both invariants hold by construction, so these cannot be written as
    //! integration tests: the arena's fields are private, and its constructors
    //! refuse to produce anything invalid. They live here because the verifier's
    //! soundness rests on `typecheck` noticing exactly these two things about
    //! IR it did not build — deserialized, macro-generated, or rewritten by an
    //! optimisation pass.

    use crate::{Arena, ExprId, Op2, Spec, Type, Typed, Value};

    /// `counter = [0] ++ (counter + 1)`, the smallest spec with an operator.
    fn counter() -> Spec {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        let one = arena.constant(Type::Word64, 1u64.lift()).unwrap();
        let next = arena.op2(Op2::Add(Type::Word64), current, one).unwrap();

        let mut spec = Spec::new(arena);
        spec.define_stream(id, vec![Value::Word64(0)], next)
            .unwrap();
        spec.observe("counter", current).unwrap();
        spec
    }

    #[test]
    fn a_cached_type_that_no_longer_matches_is_caught() {
        let mut spec = counter();
        spec.validate()
            .expect("the spec is valid before corruption");

        // Claim the constant `1u64` is an `Int8`. Nothing else changes, so only
        // a pass that re-derives types from scratch can notice.
        spec.arena.corrupt_cached_type(ExprId(1), Type::Int8);

        assert!(
            matches!(
                crate::typecheck(&spec),
                Err(crate::Error::TypeDrift {
                    expr: ExprId(1),
                    ..
                })
            ),
            "typecheck must re-derive types rather than trust the cache"
        );
    }

    #[test]
    fn a_node_placed_before_its_child_is_caught() {
        let mut spec = counter();
        spec.validate()
            .expect("the spec is valid before corruption");

        // Move the addition ahead of the operands it reads. Analyses rely on a
        // node's children having smaller IDs so that one forward pass suffices;
        // without this check they would silently read uninitialised entries.
        spec.arena.corrupt_order(ExprId(0), ExprId(2));

        assert!(
            matches!(
                crate::typecheck(&spec),
                Err(crate::Error::NonMonotonicArena { .. })
            ),
            "typecheck must verify the ordering its own single pass depends on"
        );
    }

    /// The monotonicity check has to run before types are recomputed.
    ///
    /// Recomputing a node's type reads its children's already-computed types by
    /// index. A node placed before its child would index past what has been
    /// filled in, so catching the ordering second would mean panicking first.
    #[test]
    fn the_ordering_check_runs_before_any_type_is_recomputed() {
        let mut spec = counter();
        spec.arena.corrupt_order(ExprId(0), ExprId(2));

        // The point is that this returns rather than panics.
        assert!(crate::typecheck(&spec).is_err());
    }
}
