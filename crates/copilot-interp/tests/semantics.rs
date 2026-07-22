//! Tests that pin the executable semantics: the ring-buffer invariant, the
//! four-phase step order, and the total behaviour of the operations that would
//! otherwise be partial.

use copilot_core::Value::{self, Bool, Double, Float, Int32, Word8, Word32, Word64};
use copilot_interp::{IndexPolicy, Monitor, Samples};
use copilot_lang::{Builder, Spec, args};

/// Runs a spec with no external variables and returns one observer's trace.
fn trace(spec: &Spec, observer: &str, steps: usize) -> Vec<Value> {
    let mut monitor = Monitor::new(spec).unwrap();
    let mut env = Samples::none();
    (0..steps)
        .map(|_| {
            monitor
                .step(&mut env)
                .unwrap()
                .observer(observer)
                .unwrap()
                .clone()
        })
        .collect()
}

#[test]
fn a_counter_counts() {
    let b = Builder::new();
    let counter = b.stream([0u64], |s| s + 1u64);
    b.observe("counter", counter);
    let spec = b.finish().unwrap();

    assert_eq!(
        trace(&spec, "counter", 5),
        [Word64(0), Word64(1), Word64(2), Word64(3), Word64(4)]
    );
}

/// Exercises the ring buffer at a depth greater than one, where the rotating
/// index actually rotates and `drop 1` has to find the right slot.
#[test]
fn fibonacci_needs_two_buffered_values() {
    let b = Builder::new();
    let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
    b.observe("fib", fib);
    let spec = b.finish().unwrap();

    assert_eq!(
        trace(&spec, "fib", 8),
        [1, 1, 2, 3, 5, 8, 13, 21].map(Word64)
    );
}

/// The discriminating test for phase separation.
///
/// `follower` reads `leader` and must see the value `leader` had at the *start*
/// of the step, so it lags by one. A monitor that committed each stream as it
/// computed it — merging phases 3 and 4 — would let `follower` see `leader`'s
/// new value and produce `0, 1, 2, 3` with no lag at all. That is a different
/// specification, and this is the test that catches it.
#[test]
fn streams_read_the_state_from_the_start_of_the_step() {
    let b = Builder::new();
    let leader = b.stream([0u32], |s| s + 1u32);
    let follower = b.stream([0u32], |_| leader);
    b.observe("leader", leader);
    b.observe("follower", follower);
    let spec = b.finish().unwrap();

    assert_eq!(trace(&spec, "leader", 5), [0, 1, 2, 3, 4].map(Word32));
    assert_eq!(trace(&spec, "follower", 5), [0, 0, 1, 2, 3].map(Word32));
}

#[test]
fn arithmetic_wraps_rather_than_trapping() {
    let b = Builder::new();
    let counter = b.stream([253u8], |s| s + 1u8);
    b.observe("counter", counter);
    let spec = b.finish().unwrap();

    assert_eq!(trace(&spec, "counter", 5), [253, 254, 255, 0, 1].map(Word8));
}

#[test]
fn division_by_zero_is_zero() {
    let b = Builder::new();
    // Counts 2, 1, 0, then divides by it: the third step divides by zero.
    let down = b.stream([2i32], |s| s - 1i32);
    let quotient = b.lit(100i32) / down;
    b.observe("quotient", quotient);
    b.observe("remainder", b.lit(100i32) % down);
    let spec = b.finish().unwrap();

    let mut monitor = Monitor::new(&spec).unwrap();
    let mut env = Samples::none();
    let observations = monitor.run(&mut env, 3).unwrap();

    assert_eq!(observations[0].observer("quotient"), Some(&Int32(50)));
    assert_eq!(observations[1].observer("quotient"), Some(&Int32(100)));
    assert_eq!(observations[2].observer("quotient"), Some(&Int32(0)));
    assert_eq!(observations[2].observer("remainder"), Some(&Int32(0)));
}

#[test]
fn shifting_past_the_operand_width_is_zero() {
    let b = Builder::new();
    let value = b.lit(1u32);
    b.observe("in_range", value.shift_left(b.lit(31u32)));
    b.observe("at_width", value.shift_left(b.lit(32u32)));
    b.observe("beyond", value.shift_left(b.lit(99u32)));
    let spec = b.finish().unwrap();

    let mut monitor = Monitor::new(&spec).unwrap();
    let observed = monitor.step(&mut Samples::none()).unwrap();
    assert_eq!(observed.observer("in_range"), Some(&Word32(1 << 31)));
    assert_eq!(observed.observer("at_width"), Some(&Word32(0)));
    assert_eq!(observed.observer("beyond"), Some(&Word32(0)));
}

mod arrays {
    use super::*;

    /// Builds a spec reading `array[index]` for a constant index.
    fn subscript(index: u32) -> Spec {
        let b = Builder::new();
        let array = b.stream([[10u16, 20, 30]], |s| s);
        b.observe("element", array.index(b.lit(index)));
        b.finish().unwrap()
    }

    #[test]
    fn in_range_subscripts_agree_under_every_policy() {
        let spec = subscript(1);
        for policy in [
            IndexPolicy::Wrap,
            IndexPolicy::Saturate,
            IndexPolicy::Assume,
        ] {
            let mut monitor = Monitor::with_policy(&spec, policy).unwrap();
            let observed = monitor.step(&mut Samples::none()).unwrap();
            assert_eq!(
                observed.observer("element"),
                Some(&Value::Word16(20)),
                "policy {policy:?}"
            );
        }
    }

    #[test]
    fn out_of_range_follows_the_policy() {
        let spec = subscript(4);

        let mut wrapping = Monitor::with_policy(&spec, IndexPolicy::Wrap).unwrap();
        assert_eq!(
            wrapping
                .step(&mut Samples::none())
                .unwrap()
                .observer("element"),
            Some(&Value::Word16(20)) // 4 % 3 == 1
        );

        let mut saturating = Monitor::with_policy(&spec, IndexPolicy::Saturate).unwrap();
        assert_eq!(
            saturating
                .step(&mut Samples::none())
                .unwrap()
                .observer("element"),
            Some(&Value::Word16(30))
        );

        // `Assume` defines no behaviour out of range, so the interpreter refuses
        // rather than agreeing with a monitor whose obligation was never
        // discharged.
        let mut assuming = Monitor::with_policy(&spec, IndexPolicy::Assume).unwrap();
        assert!(matches!(
            assuming.step(&mut Samples::none()),
            Err(copilot_core::Error::IndexOutOfRange { index: 4, len: 3 })
        ));
    }

    #[test]
    fn updating_replaces_one_element() {
        let b = Builder::new();
        // Writes the step counter into a rotating slot of a three-element array.
        let counter = b.stream([0u32], |s| s + 1u32);
        let history = b.stream([[0u32; 3]], |s| s.update(counter % 3u32, counter));
        b.observe("history", history);
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let mut env = Samples::none();
        let observed = monitor.run(&mut env, 4).unwrap();

        let cell = |values: [u32; 3]| Value::Array(values.map(Word32).to_vec());
        assert_eq!(observed[0].observer("history"), Some(&cell([0, 0, 0])));
        assert_eq!(observed[1].observer("history"), Some(&cell([0, 0, 0])));
        assert_eq!(observed[2].observer("history"), Some(&cell([0, 1, 0])));
        assert_eq!(observed[3].observer("history"), Some(&cell([0, 1, 2])));
    }
}

mod floats {
    use super::*;

    /// Single-precision operations must be evaluated in single precision.
    ///
    /// `+`, `-`, `*` and `/` would survive a detour through `f64`, but the
    /// transcendentals do not: this argument is one of the roughly one in two
    /// thousand where `(x as f64).exp() as f32` differs from `x.exp()` in the
    /// last bit. Asserting against the host's own `f32` operation is what
    /// catches a regression to computing at the wrong width.
    #[test]
    fn f32_operations_are_evaluated_at_f32() {
        let x = -38.997597f32;
        assert_ne!(
            x.exp().to_bits(),
            ((x as f64).exp() as f32).to_bits(),
            "chosen argument no longer distinguishes the two evaluation widths"
        );

        let b = Builder::new();
        b.observe("exp", b.lit(x).exp());
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let observed = monitor.step(&mut Samples::none()).unwrap();
        assert_eq!(observed.observer("exp"), Some(&Float(x.exp())));
    }

    /// Every comparison against NaN is false, including `<=` and `>=`.
    #[test]
    fn nan_is_unordered_in_both_directions() {
        let b = Builder::new();
        let nan = b.lit(f64::NAN);
        let one = b.lit(1.0f64);
        b.observe("lt", nan.lt(one));
        b.observe("le", nan.le(one));
        b.observe("gt", nan.gt(one));
        b.observe("ge", nan.ge(one));
        b.observe("eq", nan.eq_(nan));
        b.observe("ne", nan.ne_(nan));
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let observed = monitor.step(&mut Samples::none()).unwrap();
        for name in ["lt", "le", "gt", "ge", "eq"] {
            assert_eq!(observed.observer(name), Some(&Bool(false)), "{name}");
        }
        assert_eq!(observed.observer("ne"), Some(&Bool(true)));
    }

    #[test]
    fn transcendentals_match_the_host() {
        let b = Builder::new();
        let x = b.lit(0.5f64);
        b.observe("sqrt", x.sqrt());
        b.observe("sin", x.sin());
        b.observe("atan2", x.atan2(b.lit(2.0f64)));
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let observed = monitor.step(&mut Samples::none()).unwrap();
        assert_eq!(observed.observer("sqrt"), Some(&Double(0.5f64.sqrt())));
        assert_eq!(observed.observer("sin"), Some(&Double(0.5f64.sin())));
        assert_eq!(observed.observer("atan2"), Some(&Double(0.5f64.atan2(2.0))));
    }
}

mod externs_and_triggers {
    use super::*;

    /// The heating system from the Copilot homepage, driven over a hand-written
    /// temperature trace.
    #[test]
    fn the_heater_fires_on_the_right_steps() {
        let b = Builder::new();
        let celsius = b.extern_::<f32>("temperature");
        b.observe("celsius", celsius);
        b.trigger("heat_on", celsius.lt_val(18.0), args![celsius]);
        b.trigger("heat_off", celsius.gt_val(21.0), args![celsius]);
        let spec = b.finish().unwrap();

        let readings = [17.0f32, 19.0, 22.0, 20.0, 15.5];
        let mut monitor = Monitor::new(&spec).unwrap();

        let fired: Vec<Vec<String>> = readings
            .iter()
            .map(|&t| {
                let mut env = Samples::none().with("temperature", Float(t));
                monitor
                    .step(&mut env)
                    .unwrap()
                    .fired
                    .into_iter()
                    .map(|f| f.name)
                    .collect()
            })
            .collect();

        assert_eq!(
            fired,
            [
                vec!["heat_on".to_string()],
                vec![],
                vec!["heat_off".to_string()],
                vec![],
                vec!["heat_on".to_string()],
            ]
        );
    }

    /// Trigger arguments are evaluated from the state at the start of the step,
    /// like everything else in phase 2.
    #[test]
    fn triggers_carry_their_arguments() {
        let b = Builder::new();
        let counter = b.stream([0u32], |s| s + 1u32);
        let odd = (counter % 2u32).eq_val(1);
        b.trigger("odd_step", odd, args![counter]);
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let mut env = Samples::none();
        let observed = monitor.run(&mut env, 4).unwrap();

        assert!(!observed[0].did_fire("odd_step"));
        assert_eq!(observed[1].fired[0].args, vec![Word32(1)]);
        assert!(!observed[2].did_fire("odd_step"));
        assert_eq!(observed[3].fired[0].args, vec![Word32(3)]);
    }

    /// An external variable is sampled once per step, so two reads of the same
    /// name within a step always agree.
    #[test]
    fn an_extern_is_sampled_once_per_step() {
        let b = Builder::new();
        let a = b.extern_::<i32>("sensor");
        let again = b.extern_::<i32>("sensor");
        b.observe("agree", a.eq_(again));
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        let mut env = Samples::none().with("sensor", Int32(7));
        assert_eq!(
            monitor.step(&mut env).unwrap().observer("agree"),
            Some(&Bool(true))
        );
    }

    #[test]
    fn a_missing_sample_is_an_error() {
        let b = Builder::new();
        let sensor = b.extern_::<i32>("sensor");
        b.observe("sensor", sensor);
        let spec = b.finish().unwrap();

        let mut monitor = Monitor::new(&spec).unwrap();
        assert!(monitor.step(&mut Samples::none()).is_err());
    }
}

mod selection {
    use super::*;

    #[test]
    fn mux_selects_without_branching() {
        let b = Builder::new();
        let counter = b.stream([0u32], |s| s + 1u32);
        let even = (counter % 2u32).eq_val(0);
        b.observe("chosen", even.mux(b.lit(100u32), b.lit(200u32)));
        let spec = b.finish().unwrap();

        assert_eq!(trace(&spec, "chosen", 4), [100, 200, 100, 200].map(Word32));
    }

    /// A latch: on until it gets too hot, off until it gets too cold. Needs the
    /// stream to read its own previous value, which is the simplest thing a
    /// stateless expression cannot express.
    #[test]
    fn a_latch_remembers_across_steps() {
        let b = Builder::new();
        let temperature = b.extern_::<f32>("temperature");
        let too_cold = temperature.lt_val(18.0);
        let too_hot = temperature.gt_val(21.0);
        let heating = b.stream([false], |was_on| {
            too_cold.mux(b.lit(true), too_hot.mux(b.lit(false), was_on))
        });
        b.observe("heating", heating);
        let spec = b.finish().unwrap();

        let readings = [20.0f32, 17.0, 19.0, 20.5, 22.0, 20.0];
        let mut monitor = Monitor::new(&spec).unwrap();
        let observed: Vec<_> = readings
            .iter()
            .map(|&t| {
                let mut env = Samples::none().with("temperature", Float(t));
                monitor
                    .step(&mut env)
                    .unwrap()
                    .observer("heating")
                    .unwrap()
                    .clone()
            })
            .collect();

        // Off; cold turns it on; stays on through the comfortable band; hot
        // turns it off; stays off.
        assert_eq!(observed, [false, false, true, true, true, false].map(Bool));
    }
}

/// `drop n` past a stream's buffer evaluates its definition forward, so a
/// counter can be read arbitrarily far ahead.
///
/// The frontend rewrites this while the spec is built; the values below are the
/// check that the rewrite means what it says.
#[test]
fn reading_ahead_past_the_buffer_evaluates_the_definition() {
    let b = Builder::new();
    let counter = b.stream([10u32], |c| c + 1u32);
    b.observe("now", counter);
    b.observe("next", counter.drop(1));
    b.observe("later", counter.drop(3));
    let spec = b.finish().unwrap();

    let mut monitor = Monitor::new(&spec).unwrap();
    for step in 0..4u32 {
        let observed = monitor.step(&mut Samples::none()).unwrap();
        assert_eq!(observed.observers[0].1, Value::Word32(10 + step));
        assert_eq!(observed.observers[1].1, Value::Word32(11 + step));
        assert_eq!(observed.observers[2].1, Value::Word32(13 + step));
    }
}
