//! The corpus of specifications used by the golden and bluesim tests, and the
//! machinery for comparing a simulated monitor against the interpreter.
//!
//! It mirrors `copilot-rust`'s corpus, minus the floating point: Bluespec has
//! no usable float type, so those specifications are refused rather than
//! compiled, and the shapes they covered — transcendentals aside — are covered
//! here at integer types instead.

#![allow(dead_code)]

use copilot_core::{IndexPolicy, Value};
use copilot_interp::{Monitor, Samples};
use copilot_lang::{Builder, Spec, args};
use std::collections::BTreeMap;

/// One thing a monitor reported during a step.
///
/// Both engines are reduced to this so they can be compared directly: same
/// events, same order. Observers come first, then triggers, each in declaration
/// order — the order `docs/semantics.md` fixes for phase 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An observer's value, flattened to its scalar leaves.
    Observed(String, Vec<i128>),
    /// A trigger fired, with its arguments flattened the same way.
    Fired(String, Vec<i128>),
}

impl Event {
    /// Parses one line of a testbench's output.
    ///
    /// The format is the one `copilot_bluespec::testbench` documents; anything
    /// else is bluesim's own chatter and is skipped by the caller.
    pub fn parse(line: &str) -> Option<Event> {
        let mut fields = line.split_whitespace();
        let kind = fields.next()?;
        let name = fields.next()?.to_string();
        let values: Option<Vec<i128>> = fields.map(|f| f.parse().ok()).collect();
        match kind {
            "OBS" => Some(Event::Observed(name, values?)),
            "FIRE" => Some(Event::Fired(name, values?)),
            _ => None,
        }
    }
}

/// Flattens a value to the scalar leaves a testbench would print, in the same
/// order.
pub fn leaves(value: &Value) -> Vec<i128> {
    match value {
        Value::Bool(v) => vec![i128::from(*v)],
        Value::Int8(v) => vec![i128::from(*v)],
        Value::Int16(v) => vec![i128::from(*v)],
        Value::Int32(v) => vec![i128::from(*v)],
        Value::Int64(v) => vec![i128::from(*v)],
        Value::Word8(v) => vec![i128::from(*v)],
        Value::Word16(v) => vec![i128::from(*v)],
        Value::Word32(v) => vec![i128::from(*v)],
        Value::Word64(v) => vec![i128::from(*v)],
        Value::Array(values) => values.iter().flat_map(leaves).collect(),
        Value::Struct { fields, .. } => fields.iter().flat_map(|(_, v)| leaves(v)).collect(),
        other => panic!("the Bluespec corpus holds no {other:?}"),
    }
}

/// Runs a spec in the interpreter, reducing each step to the events a
/// testbench would print.
pub fn interpret(spec: &Spec, trace: &[BTreeMap<String, Value>]) -> Vec<Event> {
    interpret_with_policy(spec, trace, IndexPolicy::default())
}

/// [`interpret`], under a given out-of-range subscript policy.
pub fn interpret_with_policy(
    spec: &Spec,
    trace: &[BTreeMap<String, Value>],
    policy: IndexPolicy,
) -> Vec<Event> {
    let mut monitor = Monitor::with_policy(spec, policy).expect("corpus specs must validate");
    let mut events = Vec::new();
    for row in trace {
        let mut samples = Samples::none();
        for (name, value) in row {
            samples = samples.with(name, value.clone());
        }
        let observed = monitor.step(&mut samples).expect("step must succeed");
        for (name, value) in observed.observers {
            events.push(Event::Observed(name, leaves(&value)));
        }
        for fired in observed.fired {
            events.push(Event::Fired(
                fired.name,
                fired.args.iter().flat_map(leaves).collect(),
            ));
        }
    }
    events
}

/// A recorded trace: one row of external variables per step.
pub type Trace = Vec<BTreeMap<String, Value>>;

/// A corpus entry: a name, a specification, and the trace to drive it over.
pub type Entry = (&'static str, Spec, Trace);

/// A trace for a spec with no external variables.
pub fn no_samples(steps: usize) -> Trace {
    vec![BTreeMap::new(); steps]
}

/// Builds a trace from per-variable columns.
pub fn trace(columns: &[(&str, Vec<Value>)]) -> Trace {
    let steps = columns.iter().map(|(_, v)| v.len()).min().unwrap_or(0);
    (0..steps)
        .map(|step| {
            columns
                .iter()
                .map(|(name, values)| (name.to_string(), values[step].clone()))
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// The corpus.
// ---------------------------------------------------------------------------

/// A single-element buffer, which carries no rotating index at all.
pub fn counter() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u64], |s| s + 1u64);
    b.observe("counter", counter);
    b.trigger("every_third", (counter % 3u64).eq_val(0), args![counter]);
    b.finish().unwrap()
}

/// A two-deep buffer, where the index actually rotates and `drop 1` has to find
/// the right slot.
pub fn fib() -> Spec {
    let b = Builder::new();
    let fib = b.stream([1u64, 1], |s| s.after(1) + s);
    b.observe("fib", fib);
    b.finish().unwrap()
}

/// A three-deep buffer, so that the conditional subtraction standing in for the
/// modulo has a case where it actually subtracts.
pub fn window() -> Spec {
    let b = Builder::new();
    let ticks = b.stream([0u32], |s| s + 1u32);
    let history = b.stream([0u32, 0, 0], |_| ticks);
    b.observe("now", history);
    b.observe("one_ago", history.after(1));
    b.observe("two_ago", history.after(2));
    b.finish().unwrap()
}

/// Two streams, one reading the other.
///
/// `follower` must see `leader` as it was at the start of the step, so it lags
/// by one. A generator that committed each stream as it computed it would
/// produce no lag, and this is the case that catches it.
pub fn lag() -> Spec {
    let b = Builder::new();
    let leader = b.stream([0u32], |s| s + 1u32);
    let follower = b.stream([0u32], |_| leader);
    b.observe("leader", leader);
    b.observe("follower", follower);
    b.finish().unwrap()
}

/// External variables, selection, and a latch: the heater, at the fixed-point
/// temperatures a monitor on hardware would actually read.
pub fn thermostat() -> Spec {
    let b = Builder::new();
    let raw = b.extern_::<i16>("temperature");
    // Tenths of a degree, as an ADC would report them.
    let too_cold = raw.lt_val(180);
    let too_hot = raw.gt_val(210);
    let heating = b.stream([false], |was_on| {
        too_cold.mux(b.lit(true), too_hot.mux(b.lit(false), was_on))
    });
    b.observe("temperature", raw);
    b.observe("heating", heating);
    b.trigger("heat_on", too_cold & !heating, args![raw]);
    b.trigger("heat_off", too_hot & heating, args![raw]);
    b.finish().unwrap()
}

/// The operations whose behaviour is defined rather than inherited: wrapping
/// arithmetic, division by zero, and over-width shifts.
pub fn total_ops() -> Spec {
    let b = Builder::new();
    let byte = b.stream([250u8], |s| s + 1u8);
    // Counts down through zero, so the division sees a zero divisor.
    let divisor = b.stream([3i32], |s| s - 1i32);
    let shift = b.extern_::<u32>("shift");

    b.observe("byte", byte);
    b.observe("quotient", b.lit(100i32) / divisor);
    b.observe("remainder", b.lit(100i32) % divisor);
    b.observe("shifted", b.lit(1u32).shift_left(shift));
    b.observe("negated", -divisor);
    b.observe("magnitude", divisor.abs());
    b.observe("sign", divisor.signum());
    b.finish().unwrap()
}

/// Array subscript and whole-array update.
pub fn arrays() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u32], |s| s + 1u32);
    let history = b.stream([[0u32; 3]], |s| s.update(counter % 3u32, counter));
    b.observe("history", history);
    b.observe("oldest", history.index(b.lit(0u32)));
    // Deliberately out of range, to exercise the index policy.
    b.observe("wrapped", history.index(b.lit(7u32)));
    b.finish().unwrap()
}

/// An array subscript that is always in range.
///
/// Not part of the corpus: it exists for `IndexPolicy::Assume`, which is the
/// one policy that makes staying in range the specification's obligation rather
/// than the backend's. `arrays` above deliberately breaks it, so it cannot be
/// the specification that tests it.
pub fn bounded_arrays() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u32], |s| s + 1u32);
    let history = b.stream([[0u32; 4]], |s| s.update(counter % 4u32, counter));
    b.observe("history", history);
    b.observe("current", history.index(counter % 4u32));
    b.finish().unwrap()
}

/// A struct-typed stream, projected and updated field by field.
#[derive(Clone, Copy, Debug, PartialEq, copilot_lang::CopilotStruct)]
#[repr(C)]
pub struct Reading {
    pub altitude: i32,
    pub samples: u16,
    pub valid: bool,
}

pub fn structs() -> Spec {
    use ReadingFields as _;

    let b = Builder::new();
    let sensor = b.extern_::<Reading>("sensor");

    // Carries the last reading forward, bumping its sample count.
    let latest = b.stream(
        [Reading {
            altitude: 0,
            samples: 0,
            valid: false,
        }],
        |previous| {
            let bumped = previous.set_samples(previous.samples() + 1u16);
            sensor.valid().mux(sensor, bumped)
        },
    );

    b.observe("latest", latest);
    b.observe("altitude", latest.altitude());
    b.observe("samples", latest.samples());
    b.trigger("lost_signal", !sensor.valid(), args![latest.altitude()]);
    b.finish().unwrap()
}

/// Every integer and boolean operator the frontend offers.
///
/// The hand-written specs above cover the shapes a generator can get wrong;
/// this one covers the operators themselves, so that a swapped shift direction
/// or a mis-lowered cast is caught by the comparison rather than by inspection.
pub fn operators() -> Spec {
    let b = Builder::new();
    let i = b.extern_::<i32>("i");
    let j = b.extern_::<i32>("j");
    let u = b.extern_::<u16>("u");
    // A second operand of each type: `u & u` cannot catch an operand swap.
    let v = b.extern_::<u16>("v");
    let p = b.extern_::<bool>("p");
    let q = b.extern_::<bool>("q");

    b.observe("add", i + j);
    b.observe("sub", i - j);
    b.observe("mul", i * j);
    b.observe("div", i / j);
    b.observe("rem", i % j);
    b.observe("neg", -i);
    b.observe("abs", i.abs());
    b.observe("signum", i.signum());
    b.observe("abs_unsigned", u.abs());
    b.observe("signum_unsigned", u.signum());

    b.observe("bw_not", !u);
    b.observe("bw_and", u & v);
    b.observe("bw_or", u | v);
    b.observe("bw_xor", u ^ v);
    b.observe("shl", u << i);
    b.observe("shr", u >> i);
    b.observe("shr_signed", i >> u.cast::<i32>());

    b.observe("cast_widen", i.cast::<i64>());
    b.observe("cast_narrow", i.cast::<u8>());
    b.observe("cast_unsigned", i.cast::<u32>());
    b.observe("cast_widen_unsigned", u.cast::<i64>());

    b.observe("eq", i.eq_(j));
    b.observe("ne", i.ne_(j));
    b.observe("lt", i.lt(j));
    b.observe("le", i.le(j));
    b.observe("gt", i.gt(j));
    b.observe("ge", i.ge(j));
    b.observe("bool_lt", p.lt(q));
    b.observe("bool_ge", p.ge(q));

    b.observe("and", p.and(q));
    b.observe("or", p.or(q));
    b.observe("xor", p ^ q);
    b.observe("not", !p);
    b.observe("implies", p.implies(q));
    b.observe("mux", p.mux(i, j));

    b.finish().unwrap()
}

/// A specification built out of `copilot-libs` rather than by hand.
pub fn library() -> Spec {
    use copilot_libs::{
        clocks, ptltl,
        state_machine::{Transition, state_machine},
        voting,
    };

    let b = Builder::new();
    let armed = b.extern_::<bool>("armed");
    let fault = b.extern_::<bool>("fault");

    // Three redundant sensors, agreed on by majority vote.
    let sensors: Vec<_> = ["s0", "s1", "s2"]
        .iter()
        .map(|n| b.extern_::<u8>(n))
        .collect();
    let reading = voting::majority(&sensors).unwrap();
    let trustworthy = voting::a_majority(&sensors, reading).unwrap();

    // 0 idle, 1 armed, 2 rejected.
    let mode = state_machine(
        &b,
        0u8,
        0u8,
        2u8,
        !armed & !fault,
        &[
            Transition::new(0, armed, 1),
            Transition::new(1, fault, 2),
            Transition::new(1, !fault, 1),
        ],
    );

    b.observe("reading", reading);
    b.observe("trustworthy", trustworthy);
    b.observe("mode", mode);
    b.observe("was_armed", ptltl::eventually_prev(armed));
    b.observe("clean_run", ptltl::always_been(!fault));
    b.observe("since_armed", ptltl::since(!fault, armed));
    b.observe("tick", clocks::clk(&b, 4, 0).unwrap());
    b.trigger("degraded", !trustworthy, args![reading]);
    b.finish().unwrap()
}

/// A specification containing a `Local` binding whose body ignores it.
///
/// The typed frontend never produces `Local` — hash-consing already shares
/// equal subexpressions — but the IR has it, and a macro frontend or an
/// optimisation pass could. It is built here directly on the arena, which is
/// the same door those would come through.
pub fn locals() -> Spec {
    use copilot_core::{Arena, Op2, Type, Typed};

    let mut arena = Arena::new();
    let id = arena.declare_stream(Type::Word32, 1).unwrap();
    let current = arena.drop_(0, id).unwrap();
    let one = arena.constant(Type::Word32, 1u32.lift()).unwrap();

    // let v = current + 1 in (current + 1) -- the variable is used.
    let used_bound = arena.op2(Op2::Add(Type::Word32), current, one).unwrap();
    let used_var = arena.declare_local(Type::Word32);
    let reference = arena.var(used_var).unwrap();
    let used = arena.local(used_var, used_bound, reference).unwrap();

    // let w = current * 7 in 0 -- the variable is not used, so the binding is
    // reachable but nothing reads it.
    let seven = arena.constant(Type::Word32, 7u32.lift()).unwrap();
    let ignored_bound = arena.op2(Op2::Mul(Type::Word32), current, seven).unwrap();
    let ignored_var = arena.declare_local(Type::Word32);
    let zero = arena.constant(Type::Word32, 0u32.lift()).unwrap();
    let ignored = arena.local(ignored_var, ignored_bound, zero).unwrap();

    let next = arena.op2(Op2::Add(Type::Word32), used, ignored).unwrap();

    let mut spec = Spec::new(arena);
    spec.define_stream(id, vec![copilot_core::Value::Word32(0)], next)
        .unwrap();
    spec.observe("counter", current).unwrap();
    spec.validate().unwrap();
    spec
}

/// Every corpus entry, with the trace to drive it over.
pub fn all() -> Vec<Entry> {
    let steps = 12;
    let words =
        |f: fn(usize) -> u32| -> Vec<Value> { (0..steps).map(|s| Value::Word32(f(s))).collect() };
    let ints =
        |f: fn(usize) -> i32| -> Vec<Value> { (0..steps).map(|s| Value::Int32(f(s))).collect() };
    let bools =
        |f: fn(usize) -> bool| -> Vec<Value> { (0..steps).map(|s| Value::Bool(f(s))).collect() };

    vec![
        ("counter", counter(), no_samples(steps)),
        ("fib", fib(), no_samples(steps)),
        ("window", window(), no_samples(steps)),
        ("lag", lag(), no_samples(steps)),
        (
            "thermostat",
            thermostat(),
            trace(&[(
                "temperature",
                (0..steps)
                    .map(|s| Value::Int16(150 + (s as i16) * 12))
                    .collect(),
            )]),
        ),
        (
            "total_ops",
            total_ops(),
            // Runs past the operand width, so the guard has something to catch.
            trace(&[("shift", words(|s| (s as u32) * 5))]),
        ),
        ("arrays", arrays(), no_samples(steps)),
        (
            "structs",
            structs(),
            trace(&[(
                "sensor",
                (0..steps)
                    .map(|s| Value::Struct {
                        name: "Reading".into(),
                        fields: vec![
                            ("altitude".into(), Value::Int32(1000 - (s as i32) * 250)),
                            ("samples".into(), Value::Word16(s as u16)),
                            ("valid".into(), Value::Bool(s % 3 != 2)),
                        ],
                    })
                    .collect(),
            )]),
        ),
        (
            "operators",
            operators(),
            trace(&[
                ("i", ints(|s| i32::MIN / 3 + (s as i32) * 7 - 4)),
                ("j", ints(|s| (s as i32) - 3)),
                (
                    "u",
                    (0..steps)
                        .map(|s| Value::Word16(40000u16.wrapping_add(s as u16 * 3000)))
                        .collect(),
                ),
                (
                    "v",
                    (0..steps).map(|s| Value::Word16(s as u16 * 11)).collect(),
                ),
                ("p", bools(|s| s % 2 == 0)),
                ("q", bools(|s| s % 3 == 0)),
            ]),
        ),
        (
            "library",
            library(),
            trace(&[
                ("armed", bools(|s| s >= 2)),
                ("fault", bools(|s| s == 7)),
                (
                    "s0",
                    (0..steps).map(|s| Value::Word8(s as u8 % 4)).collect(),
                ),
                (
                    "s1",
                    (0..steps).map(|s| Value::Word8(s as u8 % 4)).collect(),
                ),
                (
                    "s2",
                    (0..steps)
                        .map(|s| Value::Word8((s as u8 + 1) % 4))
                        .collect(),
                ),
            ]),
        ),
        ("locals", locals(), no_samples(steps)),
    ]
}
