//! Specifications: streams, observers, triggers, and properties.

use crate::error::{Error, Result};
use crate::expr::{Arena, ExprId, StreamId};
use crate::ty::{Type, Value};

/// A stream definition: `buffer ++ expr`.
///
/// The buffer holds the stream's first `n` values. At any step it holds the
/// values for times `t ..= t + n - 1`, so `drop i` for `i < n` is a buffer
/// read, and `expr` computes the value at `t + n`. That is the whole reason a
/// monitor's memory is constant: the buffer length is fixed by the spec, and
/// nothing can reach past it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stream {
    /// The stream's ID, equal to its position in [`Spec::streams`].
    pub id: StreamId,
    /// Element type.
    pub ty: Type,
    /// Initial values, oldest first. Never empty.
    pub buffer: Vec<Value>,
    /// Transition expression, giving the value `buffer.len()` steps ahead.
    pub expr: ExprId,
}

impl Stream {
    /// Whether generated code needs a rotating index for this stream.
    ///
    /// A single-element buffer is always written and read at slot 0, so it
    /// needs no index. [`crate::resources`] and the backends must agree on
    /// this, or the reported footprint stops matching the emitted state.
    pub fn needs_index(&self) -> bool {
        self.buffer.len() > 1
    }
}

/// A stream sampled at every step for observation, without side effects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observer {
    /// Name, used as an identifier in generated code.
    pub name: String,
    /// Observed type.
    pub ty: Type,
    /// The expression sampled.
    pub expr: ExprId,
}

/// One argument passed to a trigger when it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    /// The argument's type.
    pub ty: Type,
    /// The expression computing it.
    pub expr: ExprId,
}

/// A handler invoked whenever its guard holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// Name of the handler to call.
    pub name: String,
    /// Boolean guard; the handler fires on every step this holds.
    pub guard: ExprId,
    /// Arguments, evaluated only when the guard holds.
    pub args: Vec<Arg>,
}

/// A property's quantification over time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prop {
    /// Holds at every step.
    Forall(ExprId),
    /// Holds at some step.
    Exists(ExprId),
}

impl Prop {
    /// The underlying boolean expression.
    pub fn expr(&self) -> ExprId {
        match self {
            Prop::Forall(e) | Prop::Exists(e) => *e,
        }
    }
}

/// A named property, for the theorem prover rather than the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// The property's name.
    pub name: String,
    /// What is claimed.
    pub prop: Prop,
}

/// A complete specification.
///
/// Build one by declaring streams on the [`Arena`], building their expressions,
/// then installing them here. Nothing is trusted until [`Spec::validate`]
/// succeeds — the constructors below derive types from the arena, and the
/// validators re-derive them independently, so a disagreement is caught rather
/// than propagated into a backend.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spec {
    /// The expressions this spec is built from.
    pub arena: Arena,
    /// Stream definitions, indexed by [`StreamId`].
    pub streams: Vec<Stream>,
    /// Observers, in declaration order.
    pub observers: Vec<Observer>,
    /// Triggers, in declaration order. Guards are evaluated in this order.
    pub triggers: Vec<Trigger>,
    /// Properties, in declaration order.
    pub properties: Vec<Property>,
}

impl Spec {
    /// A spec over the given arena, with nothing defined yet.
    pub fn new(arena: Arena) -> Self {
        Spec {
            arena,
            ..Default::default()
        }
    }

    /// Installs a stream's initial values and transition expression.
    ///
    /// Streams must be defined in declaration order, so that
    /// `streams[i].id == StreamId(i)` holds by construction and backends can
    /// index either way round.
    pub fn define_stream(&mut self, id: StreamId, buffer: Vec<Value>, expr: ExprId) -> Result<()> {
        if id.index() != self.streams.len() {
            return Err(Error::StreamIdMismatch {
                position: self.streams.len(),
                found: id,
            });
        }
        let decl = self.arena.stream_decl(id)?.clone();
        if buffer.len() != decl.buffer_len {
            return Err(Error::BufferLength {
                stream: id,
                expected: decl.buffer_len,
                found: buffer.len(),
            });
        }
        for (i, value) in buffer.iter().enumerate() {
            if !value.matches(&decl.ty) {
                return Err(Error::Mismatch {
                    context: format!("stream {id} initial value {i}"),
                    expected: decl.ty.clone(),
                    found: decl.ty.clone(),
                });
            }
        }
        let expr_ty = self.arena.ty_of(expr).clone();
        if expr_ty != decl.ty {
            return Err(Error::Mismatch {
                context: format!("stream {id} transition expression"),
                expected: decl.ty,
                found: expr_ty,
            });
        }
        self.streams.push(Stream {
            id,
            ty: decl.ty,
            buffer,
            expr,
        });
        Ok(())
    }

    /// Adds an observer.
    pub fn observe(&mut self, name: impl Into<String>, expr: ExprId) -> Result<()> {
        let ty = self.arena.ty_of(expr).clone();
        self.observers.push(Observer {
            name: name.into(),
            ty,
            expr,
        });
        Ok(())
    }

    /// Adds a trigger.
    pub fn trigger(
        &mut self,
        name: impl Into<String>,
        guard: ExprId,
        args: impl IntoIterator<Item = ExprId>,
    ) -> Result<()> {
        let name = name.into();
        let guard_ty = self.arena.ty_of(guard).clone();
        if guard_ty != Type::Bool {
            return Err(Error::NonBoolGuard {
                trigger: name,
                found: guard_ty,
            });
        }
        let args = args
            .into_iter()
            .map(|expr| Arg {
                ty: self.arena.ty_of(expr).clone(),
                expr,
            })
            .collect();
        self.triggers.push(Trigger { name, guard, args });
        Ok(())
    }

    /// Adds a property.
    pub fn property(&mut self, name: impl Into<String>, prop: Prop) -> Result<()> {
        let name = name.into();
        let ty = self.arena.ty_of(prop.expr()).clone();
        if ty != Type::Bool {
            return Err(Error::Mismatch {
                context: format!("property `{name}`"),
                expected: Type::Bool,
                found: ty,
            });
        }
        self.properties.push(Property { name, prop });
        Ok(())
    }

    /// Expressions evaluated on every step of the monitor.
    ///
    /// Properties are excluded: they are claims about the spec, discharged
    /// ahead of time by the theorem prover, and cost the running monitor
    /// nothing.
    pub fn runtime_roots(&self) -> Vec<ExprId> {
        let mut roots = Vec::new();
        roots.extend(self.streams.iter().map(|s| s.expr));
        roots.extend(self.observers.iter().map(|o| o.expr));
        for trigger in &self.triggers {
            roots.push(trigger.guard);
            roots.extend(trigger.args.iter().map(|a| a.expr));
        }
        roots
    }

    /// Every expression the spec refers to, including properties.
    pub fn all_roots(&self) -> Vec<ExprId> {
        let mut roots = self.runtime_roots();
        roots.extend(self.properties.iter().map(|p| p.prop.expr()));
        roots
    }

    /// Rejects existentially quantified properties.
    ///
    /// Called by anything that can only handle universal ones — which is every
    /// backend, and the k-induction prover. Mirrors upstream's
    /// `UnexpectedExistentialProposition`.
    pub fn require_universal(&self) -> Result<()> {
        for property in &self.properties {
            if matches!(property.prop, Prop::Exists(_)) {
                return Err(Error::ExistentialProperty(property.name.clone()));
            }
        }
        Ok(())
    }

    /// Runs every structural and type check. See [`crate::validate`].
    pub fn validate(&self) -> Result<()> {
        crate::validate(self)
    }
}
