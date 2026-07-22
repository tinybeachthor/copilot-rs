//! The specification builder.

use crate::error::{Error, Result};
use crate::stream::Stream;
use copilot_core::{Arena, ExprId, Node, Prop, Spec, StreamId, Type, Typed, Value};
use std::cell::RefCell;
use std::collections::HashMap;

/// Accumulates a specification.
///
/// Every method takes `&self`: expression handles borrow the builder, and they
/// have to stay usable while more of the spec is being declared. Mutation goes
/// through a [`RefCell`], which costs a borrow flag at spec-construction time
/// and nothing at all at monitor runtime.
#[derive(Debug, Default)]
pub struct Builder {
    inner: RefCell<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    arena: Arena,
    /// Stream definitions, indexed by [`StreamId`]. Sparse while building:
    /// a stream declared by a nested `stream()` call is defined before the
    /// outer one that contains it.
    definitions: Vec<Option<Definition>>,
    observers: Vec<(String, ExprId)>,
    triggers: Vec<(String, ExprId, Vec<ExprId>)>,
    properties: Vec<(String, Prop)>,
    /// The first error, if any. See [`Builder`]'s note on deferred errors.
    error: Option<Error>,
}

#[derive(Debug)]
struct Definition {
    buffer: Vec<Value>,
    expr: ExprId,
}

impl Builder {
    /// A builder with nothing declared.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a stream `initial ++ f(self)`.
    ///
    /// The closure receives a handle to the stream being defined, which is what
    /// makes recursive and mutually recursive definitions expressible without
    /// reference cycles: the ID is reserved first, the handle denotes a read of
    /// it, and the resulting expression is installed afterwards.
    ///
    /// ```
    /// # use copilot_lang::Builder;
    /// let b = Builder::new();
    /// let counter = b.stream([0u64], |s| s + 1u64);
    /// let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
    /// # b.finish().unwrap();
    /// ```
    pub fn stream<'a, T: Typed, const N: usize>(
        &'a self,
        initial: [T; N],
        f: impl FnOnce(Stream<'a, T>) -> Stream<'a, T>,
    ) -> Stream<'a, T> {
        self.stream_from(&initial, f)
    }

    /// [`Builder::stream`] with the initial values given as a slice.
    ///
    /// For buffers whose length is not known until the specification is built —
    /// a clock's period, say — where a fixed-size array cannot express it.
    pub fn stream_from<'a, T: Typed>(
        &'a self,
        initial: &[T],
        f: impl FnOnce(Stream<'a, T>) -> Stream<'a, T>,
    ) -> Stream<'a, T> {
        let id = match self
            .inner
            .borrow_mut()
            .declare_stream(T::ty(), initial.len())
        {
            Ok(id) => id,
            Err(e) => return self.poisoned(e),
        };

        let handle = match self.build(|arena| arena.drop_(0, id)) {
            Ok(expr) => Stream::new(self, expr),
            Err(e) => return self.poisoned(e),
        };

        let body = f(handle);
        let buffer = initial.iter().copied().map(Typed::lift).collect();
        self.inner
            .borrow_mut()
            .define_stream(id, buffer, body.expr());
        handle
    }

    /// `initial ++ s`: the values of `initial`, then `s` delayed by that many
    /// steps.
    ///
    /// Upstream Copilot writes this `[a, b] ++ s`. It is the non-recursive case
    /// of [`Builder::stream`] — the stream being defined does not refer to
    /// itself — and it is how the temporal libraries reach into the past.
    ///
    /// ```
    /// # use copilot_lang::Builder;
    /// let b = Builder::new();
    /// let counter = b.stream([0u32], |s| s + 1u32);
    /// let lagging = b.append(&[0u32], counter);   // counter, one step late
    /// # b.finish().unwrap();
    /// ```
    pub fn append<'a, T: Typed>(&'a self, initial: &[T], s: Stream<'a, T>) -> Stream<'a, T> {
        self.stream_from(initial, |_| s)
    }

    /// A stream of the constant `value`.
    pub fn lit<T: Typed>(&self, value: T) -> Stream<'_, T> {
        match self.build(|arena| arena.constant(T::ty(), value.lift())) {
            Ok(expr) => Stream::new(self, expr),
            Err(e) => self.poisoned(e),
        }
    }

    /// A stream sampled from the environment once per step.
    ///
    /// Reading the same name twice yields the same sample within a step. Using
    /// one name at two different types is an error.
    pub fn extern_<T: Typed>(&self, name: &str) -> Stream<'_, T> {
        match self.build(|arena| arena.extern_var(name, T::ty())) {
            Ok(expr) => Stream::new(self, expr),
            Err(e) => self.poisoned(e),
        }
    }

    /// Samples `expr` at every step under the given name.
    pub fn observe<T: Typed>(&self, name: &str, expr: Stream<'_, T>) {
        self.inner
            .borrow_mut()
            .observers
            .push((name.to_string(), expr.expr()));
    }

    /// Calls the handler `name` on every step where `guard` holds.
    ///
    /// Arguments are built with [`args!`](crate::args), which erases their
    /// types so that a heterogeneous list can be passed:
    ///
    /// ```
    /// # use copilot_lang::{args, Builder};
    /// let b = Builder::new();
    /// let temp = b.extern_::<f32>("temperature");
    /// let count = b.stream([0u32], |s| s + 1u32);
    /// b.trigger("too_cold", temp.lt(b.lit(18.0)), args![temp, count]);
    /// # b.finish().unwrap();
    /// ```
    pub fn trigger(&self, name: &str, guard: Stream<'_, bool>, args: Vec<ExprId>) {
        self.inner
            .borrow_mut()
            .triggers
            .push((name.to_string(), guard.expr(), args));
    }

    /// Claims that `expr` holds at every step, for the theorem prover to
    /// discharge. Costs the running monitor nothing.
    pub fn property_forall(&self, name: &str, expr: Stream<'_, bool>) {
        self.inner
            .borrow_mut()
            .properties
            .push((name.to_string(), Prop::Forall(expr.expr())));
    }

    /// Claims that `expr` holds at some step.
    pub fn property_exists(&self, name: &str, expr: Stream<'_, bool>) {
        self.inner
            .borrow_mut()
            .properties
            .push((name.to_string(), Prop::Exists(expr.expr())));
    }

    /// Finishes the spec, reporting the first error encountered.
    ///
    /// Errors are deferred rather than returned from each call so that operator
    /// syntax stays usable: `a + b` has nowhere to put a `Result`. Almost
    /// nothing can fail — the marker traits in [`crate::classes`] make every
    /// operator well-typed by construction — so in practice this reports a
    /// misuse of [`Stream::drop`] or a name that is not a valid identifier.
    pub fn finish(self) -> Result<Spec> {
        let inner = self.inner.into_inner();
        if let Some(error) = inner.error {
            return Err(error);
        }

        let mut spec = Spec::new(inner.arena);
        for (index, definition) in inner.definitions.into_iter().enumerate() {
            let id = StreamId(index as u32);
            let Some(Definition { buffer, expr }) = definition else {
                return Err(Error::Core(copilot_core::Error::UnknownStream(id)));
            };
            spec.define_stream(id, buffer, expr)?;
        }
        for (name, expr) in inner.observers {
            spec.observe(name, expr)?;
        }
        for (name, guard, args) in inner.triggers {
            spec.trigger(name, guard, args)?;
        }
        for (name, prop) in inner.properties {
            spec.property(name, prop)?;
        }

        spec.validate()?;
        Ok(spec)
    }

    /// Runs an arena operation, recording any error and returning it.
    pub(crate) fn build(
        &self,
        f: impl FnOnce(&mut Arena) -> copilot_core::Result<ExprId>,
    ) -> Result<ExprId> {
        f(&mut self.inner.borrow_mut().arena).map_err(Error::Core)
    }

    /// Records an error and returns a handle standing in for the expression
    /// that could not be built.
    ///
    /// The stand-in is a `false` literal. Later operations on it will usually
    /// fail too, but each failure only records an error if none is held yet, so
    /// the first — and most informative — one is what [`Builder::finish`]
    /// reports.
    pub(crate) fn poisoned<T>(&self, error: Error) -> Stream<'_, T> {
        let mut inner = self.inner.borrow_mut();
        inner.error.get_or_insert(error);
        let expr = inner
            .arena
            .constant(Type::Bool, Value::Bool(false))
            .expect("a boolean literal is always well-typed");
        Stream::new(self, expr)
    }

    /// Shifts an expression forward in time by `by` steps.
    ///
    /// `drop i` distributes over every operator — `drop i (a + b)` is
    /// `drop i a + drop i b` — so it is implemented by pushing the shift down
    /// to the `Drop` leaves, where it becomes a deeper read of a stream's
    /// buffer. That is why it applies to arbitrary expressions and not only to
    /// stream handles.
    ///
    /// It bottoms out at an external variable, whose future value is not
    /// available at any depth, and at a stream buffered too shallowly to be
    /// read that far ahead.
    pub(crate) fn shift(&self, expr: ExprId, by: u32) -> Result<ExprId> {
        if by == 0 {
            return Ok(expr);
        }
        let mut inner = self.inner.borrow_mut();
        let Inner {
            arena, definitions, ..
        } = &mut *inner;
        let definitions = &*definitions;
        let mut memo = HashMap::new();
        shift(arena, definitions, expr, by, &mut memo)
    }
}

impl Inner {
    fn declare_stream(&mut self, ty: Type, buffer_len: usize) -> Result<StreamId> {
        let id = self.arena.declare_stream(ty, buffer_len)?;
        if self.definitions.len() <= id.index() {
            self.definitions.resize_with(id.index() + 1, || None);
        }
        Ok(id)
    }

    fn define_stream(&mut self, id: StreamId, buffer: Vec<Value>, expr: ExprId) {
        self.definitions[id.index()] = Some(Definition { buffer, expr });
    }
}

/// Rebuilds `expr` with every stream read moved `by` steps later.
///
/// Memoized on the way down: hash-consing means a subexpression can be reached
/// along many paths, and shifting it once per path would be exponential in the
/// depth of the sharing. The memo is keyed on the shift amount as well as the
/// expression, because peeling a stream open (below) recurses with a smaller
/// one.
fn shift(
    arena: &mut Arena,
    definitions: &[Option<Definition>],
    expr: ExprId,
    by: u32,
    memo: &mut HashMap<(ExprId, u32), ExprId>,
) -> Result<ExprId> {
    // Shifting by nothing is the identity, externals included. Peeling a stream
    // open can land here with nothing left to shift, and that is exactly the
    // case where reading an external variable is fine: `[false] ++ p` shifted
    // once is `p` at the current step, not `p` in the future.
    if by == 0 {
        return Ok(expr);
    }
    if let Some(&done) = memo.get(&(expr, by)) {
        return Ok(done);
    }

    let shifted = match arena.node(expr).clone() {
        // A literal is the same at every time.
        Node::Const { .. } => expr,

        // Reading ahead within the buffer is just a deeper index. Reading past
        // it is still meaningful: a stream buffering `n` values defines its
        // value at `t + n` as its transition expression evaluated at `t`, so
        // `drop (n + k) s` is `drop k` of that expression. This is what lets
        // `[false] ++ p` be shifted once to recover `p`, which is how the
        // bounded-future operators reach forward at all.
        //
        // The recursion terminates because `idx < n`, so the remaining shift
        // `idx + by - n` is strictly smaller than `by`.
        Node::Drop { idx, stream } => {
            let buffered = arena.stream_decl(stream)?.buffer_len as u32;
            if idx + by < buffered {
                arena.drop_(idx + by, stream)?
            } else {
                let definition = definitions
                    .get(stream.index())
                    .and_then(|d| d.as_ref())
                    // The stream is still being defined, so its transition
                    // expression does not exist yet and cannot be peeled. This
                    // is a stream whose next value depends on its own next
                    // value.
                    .ok_or(Error::Core(copilot_core::Error::DropOutOfRange {
                        stream,
                        idx: idx + by,
                        buffer_len: buffered as usize,
                    }))?;
                shift(
                    arena,
                    definitions,
                    definition.expr,
                    idx + by - buffered,
                    memo,
                )?
            }
        }

        Node::ExternVar { name, .. } => return Err(Error::DropOnExtern(name)),
        Node::Var(_) => expr,
        Node::Local { var, bound, body } => {
            let bound = shift(arena, definitions, bound, by, memo)?;
            let body = shift(arena, definitions, body, by, memo)?;
            arena.local(var, bound, body)?
        }
        Node::Op1(op, a) => {
            let a = shift(arena, definitions, a, by, memo)?;
            arena.op1(op, a)?
        }
        Node::Op2(op, a, b) => {
            let a = shift(arena, definitions, a, by, memo)?;
            let b = shift(arena, definitions, b, by, memo)?;
            arena.op2(op, a, b)?
        }
        Node::Op3(op, a, b, c) => {
            let a = shift(arena, definitions, a, by, memo)?;
            let b = shift(arena, definitions, b, by, memo)?;
            let c = shift(arena, definitions, c, by, memo)?;
            arena.op3(op, a, b, c)?
        }
        Node::Label(name, a) => {
            let a = shift(arena, definitions, a, by, memo)?;
            arena.label(name, a)
        }
    };

    memo.insert((expr, by), shifted);
    Ok(shifted)
}

/// Collects trigger arguments of differing types into one list.
///
/// ```
/// # use copilot_lang::{args, Builder};
/// let b = Builder::new();
/// let flag = b.lit(true);
/// let count = b.lit(3u8);
/// b.trigger("report", flag, args![flag, count]);
/// # b.finish().unwrap();
/// ```
#[macro_export]
macro_rules! args {
    ($($arg:expr),* $(,)?) => {
        ::std::vec![$($crate::Stream::expr(&$arg)),*]
    };
}
