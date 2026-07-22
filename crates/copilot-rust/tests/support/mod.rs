//! The corpus of specifications used by both the golden and differential
//! tests, and the machinery for comparing a generated monitor against the
//! interpreter.

#![allow(dead_code)]

use copilot_core::Value;
use copilot_interp::{Monitor, Samples};
use copilot_lang::{Builder, Spec, args};

/// One thing a monitor reported during a step.
///
/// Both engines are reduced to this so they can be compared directly: same
/// events, same order. Observers come first, then triggers, each in
/// declaration order — the order `docs/semantics.md` fixes for phase 2.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// An observer's value.
    Observed(&'static str, Value),
    /// A trigger fired, with its arguments.
    Fired(&'static str, Vec<Value>),
}

/// Collects events from a generated monitor's handler.
#[derive(Debug, Default)]
pub struct Recorder {
    pub events: Vec<Event>,
}

impl Recorder {
    pub fn observed(&mut self, name: &'static str, value: Value) {
        self.events.push(Event::Observed(name, value));
    }

    pub fn fired(&mut self, name: &'static str, args: Vec<Value>) {
        self.events.push(Event::Fired(name, args));
    }

    pub fn take(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}

/// Runs a spec in the interpreter, reducing each step to the same events a
/// generated monitor would report.
///
/// `names` maps observer and trigger names to the `&'static str` the generated
/// side uses, so the two event streams are comparable.
pub fn interpret(spec: &Spec, samples: &[Samples], names: &[&'static str]) -> Vec<Vec<Event>> {
    let intern = |name: &str| -> &'static str {
        names
            .iter()
            .copied()
            .find(|n| *n == name)
            .unwrap_or_else(|| panic!("test harness is missing a name for `{name}`"))
    };

    let mut monitor = Monitor::new(spec).expect("corpus specs must validate");
    samples
        .iter()
        .map(|sample| {
            let observed = monitor
                .step(&mut sample.clone())
                .expect("step must succeed");
            let mut events = Vec::new();
            for (name, value) in observed.observers {
                events.push(Event::Observed(intern(&name), value));
            }
            for fired in observed.fired {
                events.push(Event::Fired(intern(&fired.name), fired.args));
            }
            events
        })
        .collect()
}

/// A spec with no external variables, stepped `count` times.
pub fn no_samples(count: usize) -> Vec<Samples> {
    vec![Samples::none(); count]
}

// ---------------------------------------------------------------------------
// The corpus.
//
// Chosen to cover what the code generator can get wrong that a type checker
// would not catch: ring-buffer indexing, the phase boundary, the operations
// with defined-but-unusual behaviour, and aggregate copies.
// ---------------------------------------------------------------------------

/// A single-element buffer, which carries no rotating index at all.
pub fn counter() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u64], |s| s + 1u64);
    b.observe("counter", counter);
    b.trigger("every_third", (counter % 3u64).eq_val(0), args![counter]);
    b.finish().unwrap()
}

/// A two-deep buffer, where the index actually rotates and `drop 1` has to
/// find the right slot.
pub fn fib() -> Spec {
    let b = Builder::new();
    let fib = b.stream([1u64, 1], |s| s.drop(1) + s);
    b.observe("fib", fib);
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

/// External variables, floats, selection, and a latch.
pub fn heater() -> Spec {
    let b = Builder::new();
    let raw = b.extern_::<f32>("temperature");
    let celsius = raw * 0.5 - 30.0;
    let too_cold = celsius.lt_val(18.0);
    let too_hot = celsius.gt_val(21.0);
    let heating = b.stream([false], |was_on| {
        too_cold.mux(b.lit(true), too_hot.mux(b.lit(false), was_on))
    });
    b.observe("celsius", celsius);
    b.observe("heating", heating);
    b.trigger("heat_on", too_cold & !heating, args![celsius]);
    b.trigger("heat_off", too_hot & heating, args![celsius]);
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

/// The float operations `core` cannot provide, which lower to calls into the
/// maths library.
pub fn maths() -> Spec {
    let b = Builder::new();
    let x = b.extern_::<f64>("x");
    b.observe("sqrt", x.sqrt());
    b.observe("floor", x.floor());
    b.observe("ceil", x.ceil());
    b.observe("sin", x.sin());
    b.observe("exp", x.exp());
    b.observe("atan2", x.atan2(b.lit(2.0f64)));
    b.observe("log", x.log(b.lit(10.0f64)));
    b.observe("powi", x.powi(3.0));
    b.finish().unwrap()
}

/// A struct-typed stream, projected and updated field by field.
#[derive(Clone, Copy, Debug, PartialEq, copilot_lang::CopilotStruct)]
#[repr(C)]
pub struct Reading {
    pub altitude: f32,
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
            altitude: 0.0,
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

/// Every operator the frontend offers, at a representative type.
///
/// The hand-written specs above cover the shapes a generator can get wrong;
/// this one covers the operators themselves, so that a mis-lowered `atan2` or a
/// swapped shift direction is caught by the differential comparison rather than
/// by inspection.
pub fn operators() -> Spec {
    let b = Builder::new();
    let i = b.extern_::<i32>("i");
    let j = b.extern_::<i32>("j");
    let u = b.extern_::<u16>("u");
    // A second operand of each type: `u & u` cannot catch an operand swap, and
    // clippy is right to call it out.
    let v = b.extern_::<u16>("v");
    let f = b.extern_::<f64>("f");
    let h = b.extern_::<f64>("h");
    let g = b.extern_::<f32>("g");
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

    b.observe("cast_widen", i.cast::<i64>());
    b.observe("cast_narrow", i.cast::<u8>());
    b.observe("cast_to_float", i.cast::<f64>());

    b.observe("fdiv", f / b.lit(3.0f64));
    b.observe("recip", f.recip());
    b.observe("f32_add", g + 1.0f32);
    b.observe("f32_sqrt", g.sqrt());
    b.observe("f32_abs", g.abs());
    b.observe("f32_signum", g.signum());

    b.observe("eq", i.eq_(j));
    b.observe("ne", i.ne_(j));
    b.observe("lt", i.lt(j));
    b.observe("le", i.le(j));
    b.observe("gt", i.gt(j));
    b.observe("ge", i.ge(j));
    b.observe("float_eq", f.eq_(h));
    b.observe("float_le", f.le(h));

    b.observe("and", p.and(q));
    b.observe("or", p.or(q));
    b.observe("xor", p ^ q);
    b.observe("not", !p);
    b.observe("implies", p.implies(q));
    b.observe("mux", p.mux(i, j));

    b.finish().unwrap()
}

/// Every corpus entry, by name.
pub fn all() -> Vec<(&'static str, Spec)> {
    vec![
        ("counter", counter()),
        ("fib", fib()),
        ("lag", lag()),
        ("heater", heater()),
        ("total_ops", total_ops()),
        ("arrays", arrays()),
        ("maths", maths()),
        ("structs", structs()),
        ("operators", operators()),
    ]
}
