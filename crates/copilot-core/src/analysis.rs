//! Static analyses backing the constant-memory and realtime claims.
//!
//! Both objectives are stated as numbers a test can pin, not as prose:
//! [`resources`] reports exactly how many bytes of state a monitor needs, and
//! [`cost`] reports exactly how much work it does per step. Neither can be
//! satisfied by accident, and a spec change that inflates either shows up as a
//! diff rather than as a missed deadline in the field.

use crate::expr::{ExprId, Node, StreamId};
use crate::op::OpClass;
use crate::spec::Spec;
use crate::ty::{Layout, Type};
use std::collections::BTreeMap;

/// Width of a ring-buffer index in generated code.
///
/// Fixed at 4 bytes rather than following the target's pointer width, so that a
/// monitor's footprint is a single number quotable without knowing the target.
/// Buffer lengths are set by the spec and are typically one to three elements,
/// so the range is never the binding constraint.
pub const INDEX_BYTES: usize = 4;

/// Per-stream contribution to a monitor's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFootprint {
    /// Which stream.
    pub stream: StreamId,
    /// Its element type.
    pub ty: Type,
    /// How many elements it buffers.
    pub buffer_len: usize,
    /// Bytes occupied by the buffer.
    pub buffer_bytes: usize,
    /// Bytes occupied by its rotating index; zero when the buffer holds one
    /// element and no index is needed.
    pub index_bytes: usize,
}

/// A monitor's memory footprint.
///
/// `state_bytes` is computed under `repr(C)` with fields in stream order —
/// buffer then index, index omitted for single-element buffers. Generated
/// monitors must declare their state exactly that way, which is why they are
/// emitted with `#[repr(C)]`: `repr(Rust)` may reorder fields, and a footprint
/// that cannot be checked against `size_of` is not a guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Footprint {
    /// Per-stream breakdown, in stream order.
    pub per_stream: Vec<StreamFootprint>,
    /// Total bytes of ring buffers, before padding.
    pub buffer_bytes: usize,
    /// Total bytes of rotating indices, before padding.
    pub index_bytes: usize,
    /// Size of the monitor's state, including padding.
    pub state_bytes: usize,
    /// Alignment of the monitor's state.
    pub state_align: usize,
    /// Upper bound on transient stack use within one step: one temporary per
    /// stream, plus one sample per external variable.
    ///
    /// An upper bound, not a prediction — the temporaries are ordinary locals
    /// and a compiler will keep most of them in registers.
    pub stack_bytes: usize,
}

/// Computes a spec's memory footprint.
pub fn resources(spec: &Spec) -> Footprint {
    let mut per_stream = Vec::with_capacity(spec.streams.len());
    let mut buffer_bytes = 0;
    let mut index_bytes = 0;
    let mut state = Layout::EMPTY;

    for stream in &spec.streams {
        let buffer_ty = Type::Array {
            elem: Box::new(stream.ty.clone()),
            len: stream.buffer.len(),
        };
        let buffer = buffer_ty.layout();
        state.extend(buffer);

        let index = if stream.needs_index() {
            state.extend(Layout {
                size: INDEX_BYTES,
                align: INDEX_BYTES,
            });
            INDEX_BYTES
        } else {
            0
        };

        buffer_bytes += buffer.size;
        index_bytes += index;
        per_stream.push(StreamFootprint {
            stream: stream.id,
            ty: stream.ty.clone(),
            buffer_len: stream.buffer.len(),
            buffer_bytes: buffer.size,
            index_bytes: index,
        });
    }

    let state = state.pad_to_align();

    let mut stack = Layout::EMPTY;
    for stream in &spec.streams {
        stack.extend(stream.ty.layout());
    }
    for (_, ty) in spec.arena.externs() {
        stack.extend(ty.layout());
    }

    Footprint {
        per_stream,
        buffer_bytes,
        index_bytes,
        state_bytes: state.size,
        state_align: state.align,
        stack_bytes: stack.pad_to_align().size,
    }
}

/// A monitor's per-step workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpCounts {
    /// Distinct arena nodes reachable per step — the number of operations
    /// executed when generated code binds every shared subexpression once.
    pub nodes_shared: usize,
    /// Total nodes if every shared subexpression were instead substituted at
    /// each use. The gap between this and `nodes_shared` is what hash-consing
    /// buys.
    pub nodes_inlined: u64,
    /// Breakdown of `nodes_shared` by cost category.
    pub by_class: BTreeMap<OpClass, usize>,
    /// Bytes copied by whole-aggregate updates. The dominant term for
    /// array-heavy specs, and invisible in a plain node count.
    pub bytes_copied: usize,
}

impl OpCounts {
    /// How many operations of the given class run per step.
    pub fn class(&self, class: OpClass) -> usize {
        self.by_class.get(&class).copied().unwrap_or(0)
    }
}

/// Computes a spec's per-step workload.
///
/// Counts only [`Spec::runtime_roots`]: properties are discharged ahead of time
/// by the prover and cost the running monitor nothing.
pub fn cost(spec: &Spec) -> OpCounts {
    let arena = &spec.arena;
    let reached = reachable(spec, &spec.runtime_roots());

    let mut by_class: BTreeMap<OpClass, usize> = BTreeMap::new();
    let mut bytes_copied = 0;
    for &id in &reached {
        let node = arena.node(id);
        *by_class.entry(class_of(node)).or_default() += 1;
        if matches!(node, Node::Op2(crate::op::Op2::UpdateField { .. }, ..))
            || matches!(node, Node::Op3(crate::op::Op3::UpdateArray(_), ..))
        {
            bytes_copied += arena.ty_of(id).layout().size;
        }
    }

    // Tree sizes, computed in the same forward pass the arena's ordering
    // permits: a node's children are always already sized.
    let mut tree_size: Vec<u64> = Vec::with_capacity(arena.len());
    for (_, node) in arena.nodes() {
        let mut size: u64 = 1;
        node.for_each_child(|c| size = size.saturating_add(tree_size[c.index()]));
        tree_size.push(size);
    }
    let mut roots = spec.runtime_roots();
    roots.sort_unstable();
    roots.dedup();
    let nodes_inlined = roots
        .iter()
        .fold(0u64, |acc, r| acc.saturating_add(tree_size[r.index()]));

    OpCounts {
        nodes_shared: reached.len(),
        nodes_inlined,
        by_class,
        bytes_copied,
    }
}

/// Every expression reachable from the given roots.
///
/// A single backward sweep, exploiting the arena's ordering: a node's children
/// have smaller IDs, so visiting IDs in descending order reaches every child
/// after its parent has been marked. No recursion and no work list.
pub fn reachable(spec: &Spec, roots: &[ExprId]) -> Vec<ExprId> {
    let arena = &spec.arena;
    let mut marked = vec![false; arena.len()];
    for root in roots {
        marked[root.index()] = true;
    }
    for i in (0..arena.len()).rev() {
        if marked[i] {
            arena
                .node(ExprId(i as u32))
                .for_each_child(|c| marked[c.index()] = true);
        }
    }
    marked
        .iter()
        .enumerate()
        .filter(|&(_, &m)| m)
        .map(|(i, _)| ExprId(i as u32))
        .collect()
}

fn class_of(node: &Node) -> OpClass {
    match node {
        Node::Const { .. } => OpClass::Const,
        Node::Drop { .. } => OpClass::Load,
        Node::ExternVar { .. } => OpClass::Extern,
        Node::Local { .. } | Node::Var(_) => OpClass::Binding,
        Node::Label(..) => OpClass::Nop,
        Node::Op1(op, _) => op.class(),
        Node::Op2(op, ..) => op.class(),
        Node::Op3(op, ..) => op.class(),
    }
}
