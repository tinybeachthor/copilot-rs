//! M0 acceptance tests: hand-built specs typecheck, and the footprint and cost
//! analyses report numbers that can be checked against something independent.

use copilot_core::{
    Arena, Error, Op1, Op2, Op3, OpClass, Prop, Spec, StreamId, Type, Typed, Value, cost, resources,
};

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

/// `fib = [1,1] ++ (drop 1 fib + fib)`
fn fib() -> Spec {
    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Word64, 2).unwrap();
    let here = arena.drop_(0, id).unwrap();
    let ahead = arena.drop_(1, id).unwrap();
    let next = arena.op2(Op2::Add(Type::Word64), ahead, here).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Word64(1), Value::Word64(1)], next)
        .unwrap();
    spec
}

/// The heating system from the Copilot homepage: convert a raw sensor reading
/// and trigger with hysteresis.
fn heater() -> Spec {
    let mut arena = Arena::new();
    let f = Type::Float;

    let raw = arena.extern_var("temperature", f.clone()).unwrap();
    let scale = arena.constant(f.clone(), 0.5f32.lift()).unwrap();
    let offset = arena.constant(f.clone(), (-30.0f32).lift()).unwrap();
    let scaled = arena.op2(Op2::Mul(f.clone()), raw, scale).unwrap();
    let celsius = arena.op2(Op2::Add(f.clone()), scaled, offset).unwrap();

    let low = arena.constant(f.clone(), 18.0f32.lift()).unwrap();
    let high = arena.constant(f.clone(), 21.0f32.lift()).unwrap();
    let too_cold = arena.op2(Op2::Lt(f.clone()), celsius, low).unwrap();
    let too_hot = arena.op2(Op2::Gt(f.clone()), celsius, high).unwrap();

    // A one-bit stream remembering whether the heater is on, so the triggers
    // only fire on a transition.
    let on = arena.declare_stream(Type::Bool, 1).unwrap();
    let was_on = arena.drop_(0, on).unwrap();
    let stays_on = arena.op1(Op1::Not, too_hot).unwrap();
    let next_on = arena
        .op3(Op3::Mux(Type::Bool), was_on, stays_on, too_cold)
        .unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(on, vec![Value::Bool(false)], next_on)
        .unwrap();
    spec.observe("celsius", celsius).unwrap();
    spec.trigger("heat_on", too_cold, [celsius]).unwrap();
    spec.trigger("heat_off", too_hot, [celsius]).unwrap();
    spec
}

#[test]
fn examples_validate() {
    for spec in [counter(), fib(), heater()] {
        spec.validate().expect("spec should validate");
    }
}

#[test]
fn counter_footprint_is_one_word() {
    let spec = counter();
    let footprint = resources(&spec);

    assert_eq!(footprint.buffer_bytes, 8);
    // A single-element buffer is always read and written at slot 0.
    assert_eq!(footprint.index_bytes, 0);
    assert_eq!(footprint.state_bytes, 8);
    assert_eq!(footprint.state_align, 8);
}

#[test]
fn fib_buffers_two_values_and_needs_an_index() {
    let spec = fib();
    let footprint = resources(&spec);

    assert_eq!(footprint.per_stream[0].buffer_len, 2);
    assert_eq!(footprint.buffer_bytes, 16);
    assert_eq!(footprint.index_bytes, copilot_core::INDEX_BYTES);
    // [u64; 2] then a u32 index, padded out to the u64 alignment.
    assert_eq!(footprint.state_bytes, 24);
}

/// The contract that makes the constant-memory claim falsifiable: the reported
/// footprint is the size of the `repr(C)` state the Rust backend will emit in
/// M2 — buffer then index, in stream order, index omitted for single-element
/// buffers.
#[test]
fn footprint_matches_the_repr_c_state_it_describes() {
    let mut arena = Arena::new();
    let bytes = arena.declare_stream(Type::Word8, 3).unwrap();
    let words = arena.declare_stream(Type::Word64, 1).unwrap();

    let b = arena.drop_(0, bytes).unwrap();
    let w = arena.drop_(0, words).unwrap();
    let widened = arena
        .op1(
            Op1::Cast {
                from: Type::Word8,
                to: Type::Word64,
            },
            b,
        )
        .unwrap();
    let sum = arena.op2(Op2::Add(Type::Word64), w, widened).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(bytes, vec![Value::Word8(0); 3], b)
        .unwrap();
    spec.define_stream(words, vec![Value::Word64(0)], sum)
        .unwrap();
    spec.validate().unwrap();

    #[repr(C)]
    struct GeneratedState {
        s0: [u8; 3],
        s0_idx: u32,
        s1: [u64; 1],
    }

    let footprint = resources(&spec);
    assert_eq!(footprint.state_bytes, size_of::<GeneratedState>());
    assert_eq!(footprint.state_align, align_of::<GeneratedState>());
    assert_eq!(footprint.buffer_bytes, 11);
    assert_eq!(footprint.index_bytes, 4);
}

#[test]
fn cost_counts_each_shared_node_once() {
    let counts = cost(&counter());

    assert_eq!(counts.nodes_shared, 3); // drop, constant, add
    assert_eq!(counts.nodes_inlined, 3); // nothing is shared here
    assert_eq!(counts.class(OpClass::Load), 1);
    assert_eq!(counts.class(OpClass::Const), 1);
    assert_eq!(counts.class(OpClass::Arith), 1);
    assert_eq!(counts.bytes_copied, 0);
}

/// `x * x + x * x` is three nodes because `x * x` is interned once, but seven
/// if a backend substituted every use. The gap is what hash-consing buys, and
/// it is the reason the frontend needs no sharing-recovery machinery at all.
#[test]
fn hash_consing_shares_equal_subexpressions() {
    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Word32, 1).unwrap();
    let x = arena.drop_(0, id).unwrap();
    let square = arena.op2(Op2::Mul(Type::Word32), x, x).unwrap();
    let again = arena.op2(Op2::Mul(Type::Word32), x, x).unwrap();
    assert_eq!(
        square, again,
        "equal subexpressions must intern to one node"
    );

    let sum = arena.op2(Op2::Add(Type::Word32), square, again).unwrap();
    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Word32(1)], sum).unwrap();
    spec.validate().unwrap();

    let counts = cost(&spec);
    assert_eq!(counts.nodes_shared, 3);
    assert_eq!(counts.nodes_inlined, 7);
}

#[test]
fn properties_cost_the_running_monitor_nothing() {
    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Word64, 1).unwrap();
    let current = arena.drop_(0, id).unwrap();
    let one = arena.constant(Type::Word64, 1u64.lift()).unwrap();
    let next = arena.op2(Op2::Add(Type::Word64), current, one).unwrap();
    let limit = arena.constant(Type::Word64, u64::MAX.lift()).unwrap();
    let bounded = arena.op2(Op2::Le(Type::Word64), current, limit).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Word64(0)], next)
        .unwrap();
    spec.property("bounded", Prop::Forall(bounded)).unwrap();
    spec.validate().unwrap();

    assert_eq!(cost(&spec).nodes_shared, cost(&counter()).nodes_shared);
    spec.require_universal().unwrap();
}

#[test]
fn existential_properties_are_rejected_by_backends() {
    let mut spec = counter();
    let t = spec.arena.constant(Type::Bool, true.lift()).unwrap();
    spec.property("eventually", Prop::Exists(t)).unwrap();

    spec.validate().unwrap(); // the IR itself permits them
    assert!(matches!(
        spec.require_universal(),
        Err(Error::ExistentialProperty(name)) if name == "eventually"
    ));
}

#[test]
fn local_bindings_typecheck_and_stay_in_scope() {
    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Int32, 1).unwrap();
    let x = arena.drop_(0, id).unwrap();

    let var = arena.declare_local(Type::Int32);
    let reference = arena.var(var).unwrap();
    let doubled = arena
        .op2(Op2::Add(Type::Int32), reference, reference)
        .unwrap();
    let bound = arena.local(var, x, doubled).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Int32(1)], bound)
        .unwrap();
    spec.validate().unwrap();
}

#[test]
fn a_variable_outside_its_binder_is_rejected() {
    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Int32, 1).unwrap();
    let var = arena.declare_local(Type::Int32);
    let escaped = arena.var(var).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Int32(0)], escaped)
        .unwrap();

    assert!(matches!(spec.validate(), Err(Error::UnknownVar(_))));
}

#[test]
fn structs_project_and_update() {
    let point = Type::Struct {
        name: "Point".into(),
        fields: vec![("x".into(), Type::Int32), ("y".into(), Type::Int32)],
    };
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
                struct_ty: point.clone(),
                field: "x".into(),
            },
            current,
        )
        .unwrap();
    let one = arena.constant(Type::Int32, 1i32.lift()).unwrap();
    let next_x = arena.op2(Op2::Add(Type::Int32), x, one).unwrap();
    let moved = arena
        .op2(
            Op2::UpdateField {
                struct_ty: point.clone(),
                field: "x".into(),
            },
            current,
            next_x,
        )
        .unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![origin], moved).unwrap();
    spec.validate().unwrap();

    // The whole struct is copied to replace one field; a plain node count would
    // miss that.
    assert_eq!(cost(&spec).bytes_copied, 8);

    let missing = spec.arena.op1(
        Op1::GetField {
            struct_ty: point,
            field: "z".into(),
        },
        current,
    );
    assert!(matches!(missing, Err(Error::UnknownField { .. })));
}

#[test]
fn arrays_index_and_update() {
    let arr = Type::Array {
        elem: Box::new(Type::Word16),
        len: 4,
    };

    let mut arena = Arena::new();
    let id = arena.declare_stream(arr.clone(), 1).unwrap();
    let current = arena.drop_(0, id).unwrap();
    let zero = arena.constant(Type::Word32, 0u32.lift()).unwrap();
    let head = arena.op2(Op2::Index(arr.clone()), current, zero).unwrap();
    let next = arena
        .op3(Op3::UpdateArray(arr.clone()), current, zero, head)
        .unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![Value::Array(vec![Value::Word16(0); 4])], next)
        .unwrap();
    spec.validate().unwrap();

    assert_eq!(resources(&spec).buffer_bytes, 8);
    assert_eq!(cost(&spec).bytes_copied, 8);
}

mod rejects {
    use super::*;

    #[test]
    fn dropping_past_the_buffer() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 2).unwrap();
        assert!(arena.drop_(1, id).is_ok());
        assert!(matches!(
            arena.drop_(2, id),
            Err(Error::DropOutOfRange {
                idx: 2,
                buffer_len: 2,
                ..
            })
        ));
    }

    #[test]
    fn a_stream_with_no_initial_values() {
        let mut arena = Arena::new();
        assert!(matches!(
            arena.declare_stream(Type::Word64, 0),
            Err(Error::EmptyBuffer(_))
        ));
    }

    #[test]
    fn a_buffer_that_is_not_the_declared_length() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 2).unwrap();
        let expr = arena.drop_(0, id).unwrap();
        let mut spec = Spec::new(arena);
        assert!(matches!(
            spec.define_stream(id, vec![Value::Word64(0)], expr),
            Err(Error::BufferLength {
                expected: 2,
                found: 1,
                ..
            })
        ));
    }

    #[test]
    fn an_initial_value_of_the_wrong_type() {
        let mut arena = Arena::new();
        let id = arena.declare_stream(Type::Word64, 1).unwrap();
        let expr = arena.drop_(0, id).unwrap();
        let mut spec = Spec::new(arena);
        assert!(spec.define_stream(id, vec![Value::Int64(0)], expr).is_err());
    }

    #[test]
    fn integer_division_of_floats() {
        let mut arena = Arena::new();
        let a = arena.constant(Type::Float, 1.0f32.lift()).unwrap();
        assert!(matches!(
            arena.op2(Op2::Div(Type::Float), a, a),
            Err(Error::OperandClass { op: "div", .. })
        ));
    }

    #[test]
    fn mixing_types_in_one_operator() {
        let mut arena = Arena::new();
        let a = arena.constant(Type::Int32, 1i32.lift()).unwrap();
        let b = arena.constant(Type::Int64, 1i64.lift()).unwrap();
        assert!(matches!(
            arena.op2(Op2::Add(Type::Int32), a, b),
            Err(Error::OperandType {
                op: "add",
                position: 1,
                ..
            })
        ));
    }

    /// An operator's type tag is redundant with its operands, and the
    /// redundancy is checked rather than trusted.
    #[test]
    fn an_operator_tag_that_disagrees_with_its_operand() {
        let mut arena = Arena::new();
        let a = arena.constant(Type::Int32, 1i32.lift()).unwrap();
        assert!(matches!(
            arena.op2(Op2::Add(Type::Int64), a, a),
            Err(Error::OpTag { op: "add", .. })
        ));
    }

    #[test]
    fn equality_on_aggregates() {
        let arr = Type::Array {
            elem: Box::new(Type::Word16),
            len: 2,
        };
        let mut arena = Arena::new();
        let id = arena.declare_stream(arr.clone(), 1).unwrap();
        let current = arena.drop_(0, id).unwrap();
        assert!(matches!(
            arena.op2(Op2::Eq(arr), current, current),
            Err(Error::OperandClass { op: "eq", .. })
        ));
    }

    #[test]
    fn one_extern_used_at_two_types() {
        let mut arena = Arena::new();
        arena.extern_var("altitude", Type::Float).unwrap();
        assert!(matches!(
            arena.extern_var("altitude", Type::Double),
            Err(Error::ExternConflict { .. })
        ));
    }

    #[test]
    fn a_non_boolean_trigger_guard() {
        let mut spec = counter();
        let guard = spec.arena.constant(Type::Word8, 1u8.lift()).unwrap();
        assert!(matches!(
            spec.trigger("fire", guard, []),
            Err(Error::NonBoolGuard { .. })
        ));
    }

    #[test]
    fn two_triggers_with_the_same_name() {
        let mut spec = counter();
        let guard = spec.arena.constant(Type::Bool, true.lift()).unwrap();
        spec.trigger("fire", guard, []).unwrap();
        spec.trigger("fire", guard, []).unwrap();
        assert!(matches!(
            spec.validate(),
            Err(Error::DuplicateName {
                kind: "trigger",
                ..
            })
        ));
    }

    #[test]
    fn a_name_that_is_not_an_identifier() {
        let mut spec = counter();
        let guard = spec.arena.constant(Type::Bool, true.lift()).unwrap();
        spec.trigger("heat on!", guard, []).unwrap();
        assert!(matches!(spec.validate(), Err(Error::BadName { .. })));
    }

    #[test]
    fn zero_length_arrays_and_fieldless_structs() {
        let mut arena = Arena::new();
        let empty_array = Type::Array {
            elem: Box::new(Type::Word8),
            len: 0,
        };
        assert!(matches!(
            arena.declare_stream(empty_array, 1),
            Err(Error::ZeroLengthArray)
        ));

        let empty_struct = Type::Struct {
            name: "Void".into(),
            fields: vec![],
        };
        assert!(matches!(
            arena.declare_stream(empty_struct, 1),
            Err(Error::EmptyStruct(_))
        ));
    }

    #[test]
    fn a_stream_declared_but_never_defined() {
        let mut arena = Arena::new();
        arena.declare_stream(Type::Word64, 1).unwrap();
        let spec = Spec::new(arena);
        assert!(matches!(
            spec.validate(),
            Err(Error::UnknownStream(StreamId(0)))
        ));
    }
}
