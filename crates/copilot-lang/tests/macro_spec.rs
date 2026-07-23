//! `copilot!` is sugar, and this is what makes that claim checkable.
//!
//! The macro must not be a second frontend with its own opinions. Every test
//! here pins it to the builder: the same specification written both ways
//! produces a literally equal `Spec` — same arena, same expression ids, same
//! streams in the same order — not merely one that behaves alike.

use copilot_lang::{Builder, Spec, args, copilot};

/// The heating system from the Copilot homepage, in macro form.
fn heater_macro() -> Spec {
    copilot! {
        extern temperature: f32;

        let celsius = temperature * 0.5 - 30.0;

        stream heating: bool = [false] ++
            (celsius < 18.0).mux(true, (celsius > 21.0).mux(false, heating));

        observe celsius;
        observe heating;

        trigger heat_on(celsius) when celsius < 18.0 && !heating;
        trigger heat_off(celsius) when celsius > 21.0 && heating;
    }
    .unwrap()
}

/// The same specification, written against the builder in the order the macro
/// emits: externs, stream declarations, `let` bindings, stream definitions,
/// then outputs.
fn heater_builder() -> Spec {
    let b = Builder::new();

    let temperature = b.extern_::<f32>("temperature");

    let heating_pending = b.declare::<bool>(&[false]);
    let heating = heating_pending.stream();

    let celsius = temperature * b.lit(0.5) - b.lit(30.0);

    heating_pending.define(celsius.lt(b.lit(18.0)).mux(
        b.lit(true),
        celsius.gt(b.lit(21.0)).mux(b.lit(false), heating),
    ));

    b.observe("celsius", celsius);
    b.observe("heating", heating);
    b.trigger(
        "heat_on",
        celsius.lt(b.lit(18.0)).and(!heating),
        args![celsius],
    );
    b.trigger(
        "heat_off",
        celsius.gt(b.lit(21.0)).and(heating),
        args![celsius],
    );

    b.finish().unwrap()
}

/// The milestone's criterion: the macro desugars to the builder exactly.
#[test]
fn the_heater_desugars_to_the_same_spec() {
    assert_eq!(
        heater_macro(),
        heater_builder(),
        "the macro must expand to builder calls, not to a second frontend"
    );
}

/// Equality above is only meaningful if it can fail. A specification that
/// differs in one constant must compare unequal, or the assertion is vacuous.
#[test]
fn spec_equality_can_actually_fail() {
    let b = Builder::new();
    let temperature = b.extern_::<f32>("temperature");
    let heating_pending = b.declare::<bool>(&[false]);
    let heating = heating_pending.stream();
    // 0.6 rather than 0.5.
    let celsius = temperature * b.lit(0.6) - b.lit(30.0);
    heating_pending.define(celsius.lt(b.lit(18.0)).mux(
        b.lit(true),
        celsius.gt(b.lit(21.0)).mux(b.lit(false), heating),
    ));
    b.observe("celsius", celsius);
    b.observe("heating", heating);
    b.trigger(
        "heat_on",
        celsius.lt(b.lit(18.0)).and(!heating),
        args![celsius],
    );
    b.trigger(
        "heat_off",
        celsius.gt(b.lit(21.0)).and(heating),
        args![celsius],
    );

    assert_ne!(heater_macro(), b.finish().unwrap());
}

#[test]
fn a_self_referential_stream_desugars_to_the_same_spec() {
    let from_macro = copilot! {
        stream counter: u64 = [0] ++ counter + 1;
        stream fib: u64 = [1, 1] ++ fib.drop(1) + fib;
        observe counter;
        observe fib;
    }
    .unwrap();

    let b = Builder::new();
    let counter = b.declare::<u64>(&[0]);
    let fib = b.declare::<u64>(&[1, 1]);
    let (c, f) = (counter.stream(), fib.stream());
    counter.define(c + b.lit(1));
    fib.define(f.drop(1) + f);
    b.observe("counter", c);
    b.observe("fib", f);

    assert_eq!(from_macro, b.finish().unwrap());
}

/// Streams may read each other, not only themselves — the reason the macro
/// declares every stream before defining any.
#[test]
fn mutually_recursive_streams_are_expressible() {
    let spec = copilot! {
        stream ping: bool = [false] ++ !pong;
        stream pong: bool = [true] ++ ping;
        observe ping;
        observe pong;
    }
    .unwrap();

    let mut monitor = copilot_interp::Monitor::new(&spec).unwrap();
    let mut seen = Vec::new();
    for _ in 0..5 {
        let step = monitor.step(&mut copilot_interp::Samples::none()).unwrap();
        seen.push((
            step.observers[0].1 == copilot_lang::Value::Bool(true),
            step.observers[1].1 == copilot_lang::Value::Bool(true),
        ));
    }

    // ping starts false and becomes !pong(previous); pong follows ping.
    assert_eq!(
        seen,
        [
            (false, true),
            (false, false),
            (true, false),
            (true, true),
            (false, true)
        ]
    );
}

#[test]
fn properties_and_the_existential_form_are_expressible() {
    let spec = copilot! {
        stream counter: u8 = [0] ++ (counter + 1) % 10;
        observe counter;
        property below_ten = counter < 10;
        property exists reaches_nine = counter == 9;
    }
    .unwrap();

    assert_eq!(spec.properties.len(), 2);
    assert!(matches!(
        spec.properties[0].prop,
        copilot_lang::Prop::Forall(_)
    ));
    assert!(matches!(
        spec.properties[1].prop,
        copilot_lang::Prop::Exists(_)
    ));
}

/// A literal is lifted where an operand is wanted and left alone where a plain
/// number is — the distinction the rewriter has to get right.
#[test]
fn literals_are_lifted_only_in_operand_position() {
    let spec = copilot! {
        stream fib: u64 = [1, 1] ++ fib.drop(1) + fib;
        // `drop(1)` takes a plain number; `> 100` takes a stream.
        observe ahead = fib.drop(1);
        observe large = fib > 100;
    }
    .unwrap();

    spec.validate().unwrap();
    assert_eq!(spec.observers[1].ty, copilot_lang::Type::Bool);
}

/// An observer can name an expression rather than repeating it.
#[test]
fn a_named_observer_takes_an_expression() {
    let from_macro = copilot! {
        extern raw: i32;
        observe doubled = raw * 2;
    }
    .unwrap();

    let b = Builder::new();
    let raw = b.extern_::<i32>("raw");
    b.observe("doubled", raw * b.lit(2));

    assert_eq!(from_macro, b.finish().unwrap());
}

/// Builder errors survive the macro rather than being swallowed.
#[test]
fn errors_reach_the_caller() {
    let result = copilot! {
        extern raw: u32;
        // Reading an external variable's future is not possible.
        observe ahead = raw.drop(1);
    };
    assert!(matches!(result, Err(copilot_lang::Error::DropOnExtern(_))));
}

/// What may refer to what, pinned so `docs/macro.md` states facts.
///
/// The expansion emits externs, then every stream declaration, then every
/// `let`, then every stream body, then the outputs. Two consequences follow,
/// and neither is obvious from reading a block top to bottom.
mod scoping {
    use super::*;

    /// Every `let` is emitted before any stream body, so a body can use one
    /// that appears further down the block.
    #[test]
    fn a_stream_body_may_use_a_later_let() {
        let spec = copilot! {
            extern raw: i32;
            stream held: i32 = [0] ++ scaled;
            let scaled = raw * 2;
            observe held;
        }
        .unwrap();
        spec.validate().unwrap();
    }

    /// Every stream is declared before any `let`, so a binding can use a stream
    /// that appears further down.
    #[test]
    fn a_let_may_use_a_later_stream() {
        let spec = copilot! {
            let doubled = counter * 2;
            stream counter: u32 = [0] ++ counter + 1;
            observe doubled;
        }
        .unwrap();
        spec.validate().unwrap();
    }

    // A `let` may *not* use a later `let`: bindings keep their source order, so
    // that is an ordinary "cannot find value" error. There is no test for it
    // here because it does not compile, which is the point.
}
