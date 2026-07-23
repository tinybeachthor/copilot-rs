//! The expression arena.
//!
//! Upstream Copilot recovers sharing from a Haskell expression graph with
//! `data-reify`, which observes `StableName` pointer identity under
//! `unsafePerformIO`. That is the least principled part of the original design:
//! it is heuristic (GHC may or may not have shared two equal thunks) and it is
//! unsafe.
//!
//! Here, expressions are built directly into a hash-consed arena. Sharing is
//! structural and decided by construction: interning the same node twice
//! returns the same [`ExprId`], so common subexpressions are shared exactly
//! when they are equal, deterministically, with no unsafe code.
//!
//! Two invariants hold by construction and are re-checked by
//! [`crate::typecheck`], because a deserialized or hand-built arena could
//! violate them:
//!
//! 1. **Monotonicity.** A node's children always have strictly smaller IDs,
//!    since children are interned first. Analyses can therefore run as a single
//!    forward pass over `0..len` instead of a recursive traversal.
//! 2. **Well-typedness.** Every node's type is computed when it is interned, so
//!    an ill-typed node cannot be constructed at all.

use crate::error::{Error, Result};
use crate::op::{Op1, Op2, Op3};
use crate::ty::{Type, Value};
use std::collections::HashMap;
use std::fmt;

macro_rules! newtype_id {
    ($(#[$meta:meta])* $name:ident, $prefix:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            /// The index as a `usize`.
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

newtype_id!(
    /// Identifies a node in an [`Arena`].
    ExprId,
    "e"
);
newtype_id!(
    /// Identifies a buffered stream.
    StreamId,
    "s"
);
newtype_id!(
    /// Identifies a local binding.
    VarId,
    "v"
);

/// A node in the expression arena.
///
/// Children are [`ExprId`]s rather than boxes, so the IR is a flat `Vec`:
/// cheap to clone, trivially serializable, and free of reference cycles even
/// though the streams it describes are mutually recursive.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    /// A literal.
    Const {
        /// The literal's type.
        ty: Type,
        /// The literal's value.
        value: Value,
    },

    /// `drop idx stream`: the value of `stream` at `idx` steps ahead of now.
    ///
    /// This is the only way an expression reads state. `idx` is bounded by the
    /// stream's buffer length, which is what makes a monitor's memory constant:
    /// a spec cannot ask to look further back than it declared.
    Drop {
        /// How far ahead to look; `0` is the current value.
        idx: u32,
        /// The stream being read.
        stream: StreamId,
    },

    /// A sample of an external variable, supplied by the environment.
    ExternVar {
        /// The variable's name in the environment.
        name: String,
        /// Its type.
        ty: Type,
    },

    /// `let var = bound in body`.
    ///
    /// Hash-consing already shares equal subexpressions, so this exists to let
    /// a frontend *force* a binding where it wants one to appear in generated
    /// code, not to recover sharing.
    Local {
        /// The bound variable.
        var: VarId,
        /// Its definition.
        bound: ExprId,
        /// The expression it scopes over.
        body: ExprId,
    },

    /// A reference to a [`Node::Local`] binding.
    Var(VarId),

    /// Unary operator application.
    Op1(Op1, ExprId),

    /// Binary operator application.
    Op2(Op2, ExprId, ExprId),

    /// Ternary operator application.
    Op3(Op3, ExprId, ExprId, ExprId),

    /// A named annotation, carried through to generated code as a comment.
    /// Semantically the identity.
    Label(String, ExprId),
}

impl Node {
    /// Calls `f` on each child ID, in operand order.
    pub fn for_each_child(&self, mut f: impl FnMut(ExprId)) {
        match self {
            Node::Const { .. } | Node::Drop { .. } | Node::ExternVar { .. } | Node::Var(_) => {}
            Node::Local { bound, body, .. } => {
                f(*bound);
                f(*body);
            }
            Node::Op1(_, a) | Node::Label(_, a) => f(*a),
            Node::Op2(_, a, b) => {
                f(*a);
                f(*b);
            }
            Node::Op3(_, a, b, c) => {
                f(*a);
                f(*b);
                f(*c);
            }
        }
    }

    /// The children, collected into a small vector.
    pub fn children(&self) -> Vec<ExprId> {
        let mut out = Vec::new();
        self.for_each_child(|c| out.push(c));
        out
    }
}

/// A stream's declaration: everything the arena needs to type and bounds-check
/// references to it, known before its transition expression exists.
///
/// Streams are mutually recursive, so a stream must be referable before it is
/// defined. Declaring it separately is what makes that possible without
/// `Rc<RefCell<..>>` cycles: the frontend reserves an ID, builds an expression
/// that reads the stream, and only then installs that expression as the
/// stream's own definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDecl {
    /// The stream's element type.
    pub ty: Type,
    /// How many values it buffers; the largest valid drop index is one less.
    pub buffer_len: usize,
}

/// A hash-consed store of expressions, plus the stream, extern, and local
/// declarations they refer to.
///
/// Comparable, so that two specifications built by different routes — the
/// builder and the `copilot!` macro, say — can be checked for being literally
/// the same IR rather than merely behaving alike.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Arena {
    nodes: Vec<Node>,
    types: Vec<Type>,
    memo: HashMap<Node, ExprId>,
    streams: Vec<StreamDecl>,
    externs: Vec<(String, Type)>,
    locals: Vec<Type>,
}

impl Arena {
    /// An empty arena.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of interned nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether no nodes have been interned.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The node with the given ID.
    pub fn node(&self, id: ExprId) -> &Node {
        &self.nodes[id.index()]
    }

    /// The cached type of the given expression.
    pub fn ty_of(&self, id: ExprId) -> &Type {
        &self.types[id.index()]
    }

    /// All nodes, in interning order — which is also topological order, parents
    /// last.
    pub fn nodes(&self) -> impl Iterator<Item = (ExprId, &Node)> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (ExprId(i as u32), n))
    }

    /// The declared streams, indexed by [`StreamId`].
    pub fn stream_decls(&self) -> &[StreamDecl] {
        &self.streams
    }

    /// Declaration of the given stream.
    pub fn stream_decl(&self, id: StreamId) -> Result<&StreamDecl> {
        self.streams.get(id.index()).ok_or(Error::UnknownStream(id))
    }

    /// The external variables referenced so far, in declaration order.
    pub fn externs(&self) -> &[(String, Type)] {
        &self.externs
    }

    /// Type of the given local binding.
    pub fn local_ty(&self, var: VarId) -> Result<&Type> {
        self.locals.get(var.index()).ok_or(Error::UnknownVar(var))
    }

    /// How many local bindings have been declared.
    pub fn local_count(&self) -> usize {
        self.locals.len()
    }

    /// Declares a stream, reserving its ID and buffer length.
    ///
    /// Must be called before any expression reads the stream, which for a
    /// recursive definition means before the stream's own body is built.
    pub fn declare_stream(&mut self, ty: Type, buffer_len: usize) -> Result<StreamId> {
        ty.validate()?;
        let id = StreamId(self.streams.len() as u32);
        if buffer_len == 0 {
            return Err(Error::EmptyBuffer(id));
        }
        self.streams.push(StreamDecl { ty, buffer_len });
        Ok(id)
    }

    /// Declares a local binding of the given type, returning its ID.
    ///
    /// The type is that of the expression the variable will be bound to, so
    /// this is called after building the bound expression and before the body
    /// that refers to it.
    pub fn declare_local(&mut self, ty: Type) -> VarId {
        let id = VarId(self.locals.len() as u32);
        self.locals.push(ty);
        id
    }

    /// A literal of the given type.
    pub fn constant(&mut self, ty: Type, value: Value) -> Result<ExprId> {
        ty.validate()?;
        if !value.matches(&ty) {
            return Err(Error::BadConstant { ty });
        }
        Ok(self.intern(
            Node::Const {
                ty: ty.clone(),
                value,
            },
            ty,
        ))
    }

    /// `drop idx stream`.
    pub fn drop_(&mut self, idx: u32, stream: StreamId) -> Result<ExprId> {
        let decl = self.stream_decl(stream)?;
        if idx as usize >= decl.buffer_len {
            return Err(Error::DropOutOfRange {
                stream,
                idx,
                buffer_len: decl.buffer_len,
            });
        }
        let ty = decl.ty.clone();
        Ok(self.intern(Node::Drop { idx, stream }, ty))
    }

    /// A sample of an external variable.
    ///
    /// Repeating a name is how a spec reads the same input twice; repeating it
    /// at a *different* type is an error, since the environment can only supply
    /// one.
    pub fn extern_var(&mut self, name: impl Into<String>, ty: Type) -> Result<ExprId> {
        ty.validate()?;
        let name = name.into();
        match self.externs.iter().find(|(n, _)| *n == name) {
            Some((_, existing)) if *existing != ty => {
                return Err(Error::ExternConflict {
                    name,
                    first: existing.clone(),
                    second: ty,
                });
            }
            Some(_) => {}
            None => self.externs.push((name.clone(), ty.clone())),
        }
        Ok(self.intern(
            Node::ExternVar {
                name,
                ty: ty.clone(),
            },
            ty,
        ))
    }

    /// A reference to a local binding.
    pub fn var(&mut self, var: VarId) -> Result<ExprId> {
        let ty = self.local_ty(var)?.clone();
        Ok(self.intern(Node::Var(var), ty))
    }

    /// `let var = bound in body`.
    pub fn local(&mut self, var: VarId, bound: ExprId, body: ExprId) -> Result<ExprId> {
        let declared = self.local_ty(var)?.clone();
        let bound_ty = self.ty_of(bound).clone();
        if declared != bound_ty {
            return Err(Error::Mismatch {
                context: format!("local binding {var}"),
                expected: declared,
                found: bound_ty,
            });
        }
        let ty = self.ty_of(body).clone();
        Ok(self.intern(Node::Local { var, bound, body }, ty))
    }

    /// Unary operator application.
    pub fn op1(&mut self, op: Op1, a: ExprId) -> Result<ExprId> {
        let ty = op.result_ty(self.ty_of(a))?;
        Ok(self.intern(Node::Op1(op, a), ty))
    }

    /// Binary operator application.
    pub fn op2(&mut self, op: Op2, a: ExprId, b: ExprId) -> Result<ExprId> {
        let ty = op.result_ty(self.ty_of(a), self.ty_of(b))?;
        Ok(self.intern(Node::Op2(op, a, b), ty))
    }

    /// Ternary operator application.
    pub fn op3(&mut self, op: Op3, a: ExprId, b: ExprId, c: ExprId) -> Result<ExprId> {
        let ty = op.result_ty(self.ty_of(a), self.ty_of(b), self.ty_of(c))?;
        Ok(self.intern(Node::Op3(op, a, b, c), ty))
    }

    /// Annotates an expression with a name, without changing its meaning.
    pub fn label(&mut self, name: impl Into<String>, a: ExprId) -> ExprId {
        let ty = self.ty_of(a).clone();
        self.intern(Node::Label(name.into(), a), ty)
    }

    /// Interns a node, reusing an existing ID if an equal node is already
    /// present.
    fn intern(&mut self, node: Node, ty: Type) -> ExprId {
        if let Some(&id) = self.memo.get(&node) {
            return id;
        }
        let id = ExprId(self.nodes.len() as u32);
        self.memo.insert(node.clone(), id);
        self.nodes.push(node);
        self.types.push(ty);
        id
    }
}

/// Deliberate corruption, for testing that validation actually catches it.
///
/// The arena's invariants hold by construction, so the only way to check that
/// [`crate::typecheck`] enforces them is to break one on purpose. That needs
/// access to fields no caller outside this module has — which is the point:
/// nothing but a test can reach them.
#[cfg(test)]
impl Arena {
    /// Replaces a node's cached type without recomputing anything.
    pub(crate) fn corrupt_cached_type(&mut self, id: ExprId, ty: Type) {
        self.types[id.index()] = ty;
    }

    /// Puts a node at a position earlier than one of its children.
    pub(crate) fn corrupt_order(&mut self, a: ExprId, b: ExprId) {
        self.nodes.swap(a.index(), b.index());
        self.types.swap(a.index(), b.index());
    }
}
