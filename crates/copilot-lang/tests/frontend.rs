//! Tests for the builder: what it produces, what it shares, and what it
//! refuses.

use copilot_lang::{Builder, Error, args, cost, resources};

#[test]
fn a_spec_built_by_the_frontend_validates() {
    let b = Builder::new();
    let counter = b.stream([0u64], |s| s + 1u64);
    let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
    let temperature = b.extern_::<f32>("temperature");

    b.observe("fib", fib);
    b.trigger("tick", (counter % 10u64).eq_val(0), args![counter, fib]);
    b.property_forall("fib_is_positive", fib.gt_val(0));
    b.observe("celsius", temperature * 0.5 - 30.0);

    let spec = b.finish().unwrap();
    assert_eq!(spec.streams.len(), 2);
    assert_eq!(spec.triggers.len(), 1);
    assert_eq!(spec.properties.len(), 1);
    assert_eq!(spec.arena.externs().len(), 1);
}

/// Using a handle twice denotes one expression, not two.
///
/// This is the whole reason the frontend needs no sharing-recovery step: the
/// arena interns `celsius` once, and both triggers read the same node. Upstream
/// Copilot reaches the same place through `data-reify` and `StableName`
/// identity.
#[test]
fn reusing_a_handle_shares_the_expression() {
    let b = Builder::new();
    let raw = b.extern_::<f32>("temperature");
    let celsius = raw * 0.5 - 30.0;
    b.trigger("cold", celsius.lt_val(18.0), args![celsius]);
    b.trigger("hot", celsius.gt_val(21.0), args![celsius]);
    let spec = b.finish().unwrap();

    let counts = cost(&spec);
    // extern, 0.5, *, 30.0, -, 18.0, <, 21.0, > — nine nodes, with the four
    // that make up `celsius` reached from four places.
    assert_eq!(counts.nodes_shared, 9);
    assert!(
        counts.nodes_inlined > counts.nodes_shared as u64,
        "expected sharing to save work, got {counts:?}"
    );
}

/// The same expression written twice is also shared, which upstream's
/// pointer-identity approach cannot guarantee.
#[test]
fn structurally_equal_expressions_share_too() {
    let b = Builder::new();
    let x = b.stream([1u32], |s| s + 1u32);
    let first = x * x;
    let second = x * x;
    b.observe("sum", first + second);
    let spec = b.finish().unwrap();

    // drop, 1, +(the stream body), *, + — the two `x * x` collapse to one node.
    assert_eq!(cost(&spec).nodes_shared, 5);
}

#[test]
fn the_frontend_reports_the_footprint_of_what_it_built() {
    let b = Builder::new();
    b.stream([0u64], |s| s + 1u64);
    b.stream([0u8, 0, 0], |s| s + 1u8);
    let spec = b.finish().unwrap();

    let footprint = resources(&spec);
    assert_eq!(footprint.buffer_bytes, 8 + 3);
    // Only the three-deep buffer needs a rotating index.
    assert_eq!(footprint.index_bytes, copilot_lang::INDEX_BYTES);
}

mod drop_semantics {
    use super::*;

    /// `drop` distributes over operators, so it applies to any expression and
    /// not only to a bare stream handle. Both spellings build the same node.
    #[test]
    fn shifting_distributes_over_operators() {
        let b = Builder::new();
        let x = b.stream([1u32, 2], |s| s.drop(1));
        let y = b.stream([3u32, 4], |s| s.drop(1));

        let shifted_sum = (x + y).drop(1);
        let sum_of_shifted = x.drop(1) + y.drop(1);
        assert_eq!(shifted_sum.expr(), sum_of_shifted.expr());

        b.observe("shifted", shifted_sum);
        b.finish().unwrap();
    }

    #[test]
    fn shifting_a_constant_leaves_it_alone() {
        let b = Builder::new();
        let one = b.lit(1u32);
        assert_eq!(one.drop(3).expr(), one.expr());
        let x = b.stream([0u32, 0], |s| s.drop(1));
        b.observe("x", x);
        b.finish().unwrap();
    }

    /// A stream can only be read as far ahead as it buffers.
    #[test]
    fn shifting_past_the_buffer_is_rejected() {
        let b = Builder::new();
        let x = b.stream([1u32, 2], |s| s.drop(2));
        b.observe("x", x);
        assert!(matches!(
            b.finish(),
            Err(Error::Core(copilot_core::Error::DropOutOfRange {
                idx: 2,
                buffer_len: 2,
                ..
            }))
        ));
    }

    /// An external variable has no future samples to look ahead to.
    #[test]
    fn shifting_an_extern_is_rejected() {
        let b = Builder::new();
        let sensor = b.extern_::<i32>("sensor");
        b.observe("ahead", sensor.drop(1));
        assert!(matches!(b.finish(), Err(Error::DropOnExtern(name)) if name == "sensor"));
    }

    /// The error surfaces even when the extern is buried inside an expression
    /// that is otherwise shiftable.
    #[test]
    fn shifting_an_extern_is_rejected_under_an_operator() {
        let b = Builder::new();
        let sensor = b.extern_::<i32>("sensor");
        let stream = b.stream([0i32, 0], |s| s.drop(1));
        b.observe("ahead", (sensor + stream).drop(1));
        assert!(matches!(b.finish(), Err(Error::DropOnExtern(_))));
    }
}

mod rejects {
    use super::*;

    #[test]
    fn one_extern_used_at_two_types() {
        let b = Builder::new();
        let as_float = b.extern_::<f32>("altitude");
        let as_double = b.extern_::<f64>("altitude");
        b.observe("a", as_float);
        b.observe("b", as_double);
        assert!(matches!(
            b.finish(),
            Err(Error::Core(copilot_core::Error::ExternConflict { .. }))
        ));
    }

    #[test]
    fn two_triggers_with_the_same_name() {
        let b = Builder::new();
        let always = b.lit(true);
        b.trigger("fire", always, args![]);
        b.trigger("fire", always, args![]);
        assert!(matches!(
            b.finish(),
            Err(Error::Core(copilot_core::Error::DuplicateName { .. }))
        ));
    }

    #[test]
    fn a_name_that_is_not_an_identifier() {
        let b = Builder::new();
        let always = b.lit(true);
        b.trigger("heat on!", always, args![]);
        assert!(matches!(
            b.finish(),
            Err(Error::Core(copilot_core::Error::BadName { .. }))
        ));
    }

    /// The first error is the one reported, not the last: later failures are
    /// usually consequences of it.
    #[test]
    fn the_first_error_is_the_one_reported() {
        let b = Builder::new();
        let sensor = b.extern_::<i32>("sensor");
        b.observe("first", sensor.drop(1));
        let other = b.extern_::<i32>("other");
        b.observe("second", other.drop(1));
        assert!(matches!(b.finish(), Err(Error::DropOnExtern(name)) if name == "sensor"));
    }
}

/// Every operator the frontend exposes builds well-typed IR. If any of these
/// selected the wrong `Op`, `finish` would fail its typecheck.
#[test]
fn every_operator_builds_well_typed_ir() {
    let b = Builder::new();

    let i = b.stream([1i32], |s| s + 1i32);
    let u = b.stream([1u32], |s| s + 1u32);
    let f = b.stream([1.0f64], |s| s + 1.0f64);
    let flag = b.stream([true], |s| !s);

    b.observe("neg", -i);
    b.observe("sub", i - 2i32);
    b.observe("mul", 3i32 * i);
    b.observe("div", i / 2i32);
    b.observe("rem", i % 2i32);
    b.observe("abs", i.abs());
    b.observe("signum", i.signum());
    b.observe("cast", i.cast::<f64>());

    b.observe("bw_not", !u);
    b.observe("bw_and", u & u);
    b.observe("bw_or", u | u);
    b.observe("bw_xor", u ^ u);
    b.observe("shl", u << b.lit(2u32));
    b.observe("shr", u >> b.lit(2u32));

    b.observe("fdiv", f / 2.0f64);
    b.observe("recip", f.recip());
    b.observe("sqrt", f.sqrt());
    b.observe("exp", f.exp());
    b.observe("ln", f.ln());
    b.observe("sin", f.sin());
    b.observe("cos", f.cos());
    b.observe("tan", f.tan());
    b.observe("asin", f.asin());
    b.observe("acos", f.acos());
    b.observe("atan", f.atan());
    b.observe("sinh", f.sinh());
    b.observe("cosh", f.cosh());
    b.observe("tanh", f.tanh());
    b.observe("asinh", f.asinh());
    b.observe("acosh", f.acosh());
    b.observe("atanh", f.atanh());
    b.observe("ceil", f.ceil());
    b.observe("floor", f.floor());
    b.observe("powf", f.powi(2.0));
    b.observe("log", f.log(b.lit(10.0f64)));
    b.observe("atan2", f.atan2(b.lit(1.0f64)));

    b.observe("and", flag.and(flag));
    b.observe("or", flag.or(flag));
    b.observe("implies", flag.implies(flag));
    b.observe("xor", flag ^ flag);
    b.observe("mux", flag.mux(i, i));

    b.observe("eq", i.eq_(i));
    b.observe("ne", i.ne_(i));
    b.observe("lt", i.lt(i));
    b.observe("le", i.le(i));
    b.observe("gt", i.gt(i));
    b.observe("ge", i.ge(i));

    let array = b.stream([[0u16; 4]], |s| s);
    b.observe("index", array.index(u));
    b.observe("update", array.update(u, b.lit(7u16)));

    b.observe("labelled", i.label("annotated"));

    b.finish().expect("every operator must build well-typed IR");
}

/// Reading past the end of a buffer is legal whenever the stream's own
/// definition can supply the value.
///
/// A stream buffering `n` values defines its value at `t + n` to be its
/// transition expression at `t`, so `drop n` of it is that expression. Without
/// this, `[false] ++ p` could not be shifted back to `p`, and every bounded
/// future-time operator would be unusable — which is how M3 found it missing.
mod dropping_past_the_buffer {
    use super::*;

    #[test]
    fn peels_one_layer_off_an_appended_stream() {
        let b = Builder::new();
        let raw = b.extern_::<u32>("p");
        let delayed = b.append(&[0u32], raw);

        // `delayed` is [0, p0, p1, ..], so shifting it once recovers `p`
        // exactly — the same arena node, not merely an equal one.
        assert_eq!(delayed.drop(1).expr(), raw.expr());

        b.observe("out", delayed.drop(1));
        b.finish().unwrap();
    }

    #[test]
    fn still_refuses_to_read_an_external_variable_s_future() {
        let b = Builder::new();
        let raw = b.extern_::<u32>("p");
        let delayed = b.append(&[0u32], raw);

        // One shift lands on `p` now; two would need its next sample.
        b.observe("out", delayed.drop(2));
        assert!(matches!(b.finish(), Err(Error::DropOnExtern(name)) if name == "p"));
    }

    /// A stream whose next value would be defined by its own next value.
    #[test]
    fn refuses_a_definition_that_would_need_itself() {
        let b = Builder::new();
        let bad = b.stream([0u32], |s| s.drop(1));
        b.observe("out", bad);
        assert!(b.finish().is_err());
    }
}
