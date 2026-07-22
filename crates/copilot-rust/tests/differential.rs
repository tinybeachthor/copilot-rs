//! Differential tests: every corpus monitor is compiled and run alongside the
//! interpreter, and the two must report identical events.
//!
//! This is the test that gives the code generator meaning. The interpreter is a
//! genuine constant-memory implementation of the same ring buffers, evaluated
//! by walking the IR rather than by compiling it, so a disagreement is a real
//! bug in one of them rather than an artefact of comparing different models.
//!
//! The generated code here is the checked-in golden output, compiled as part of
//! this test binary, so the whole suite runs on every commit without a `rustc`
//! subprocess. `proptest` supplies the inputs; the specifications themselves are
//! hand-written, chosen to cover what a code generator can get wrong that a type
//! checker would not catch.

mod support;

use copilot_core::Value::{self, Bool, Double, Int32, Word8, Word32, Word64};
use copilot_interp::Samples;
use proptest::prelude::*;
use support::{Event, Recorder, interpret, no_samples};

/// Routes the transcendentals to the standard library.
///
/// Generated monitors call `libm` because `core` has no `sqrt`. The real `libm`
/// and `std` agree bit for bit on the exactly-rounded operations — `sqrt`,
/// `ceil`, `floor` — but not on the transcendentals, where they may differ in
/// the last place. Pointing both engines at one implementation keeps this test
/// about the code generator rather than about two libms.
///
/// The consequence is that this test checks transcendental *lowering* — that
/// the right function is called with the right arguments in the right order —
/// and not that `libm` matches `std` numerically. It does not, and
/// `docs/semantics.md` says so.
#[allow(dead_code)]
mod libm {
    pub fn sqrt(x: f64) -> f64 {
        x.sqrt()
    }
    pub fn floor(x: f64) -> f64 {
        x.floor()
    }
    pub fn ceil(x: f64) -> f64 {
        x.ceil()
    }
    pub fn sin(x: f64) -> f64 {
        x.sin()
    }
    pub fn exp(x: f64) -> f64 {
        x.exp()
    }
    pub fn log(x: f64) -> f64 {
        x.ln()
    }
    pub fn pow(x: f64, y: f64) -> f64 {
        x.powf(y)
    }
    pub fn atan2(y: f64, x: f64) -> f64 {
        y.atan2(x)
    }

    // The `f` suffix is `libm`'s convention for the single-precision entry
    // points, which is what the generator emits for `Type::Float`.
    pub fn sqrtf(x: f32) -> f32 {
        x.sqrt()
    }
    pub fn floorf(x: f32) -> f32 {
        x.floor()
    }
    pub fn ceilf(x: f32) -> f32 {
        x.ceil()
    }
    pub fn sinf(x: f32) -> f32 {
        x.sin()
    }
    pub fn expf(x: f32) -> f32 {
        x.exp()
    }
    pub fn logf(x: f32) -> f32 {
        x.ln()
    }
    pub fn powf(x: f32, y: f32) -> f32 {
        x.powf(y)
    }
    pub fn atan2f(y: f32, x: f32) -> f32 {
        y.atan2(x)
    }
}

macro_rules! generated {
    ($module:ident, $file:literal) => {
        #[allow(unused_imports, dead_code)]
        mod $module {
            use super::libm;
            include!($file);
        }
    };
}

/// Implements the observer methods of a generated `Handler`.
///
/// Each line names the generated method, the observer's name in the spec, the
/// Rust type the method receives, and the `Value` constructor that lifts it
/// back into something comparable with the interpreter's output.
macro_rules! observers {
    ($($method:ident => $name:literal : $ty:ty = $lift:expr;)*) => {
        $(
            fn $method(&mut self, value: $ty) {
                let lift: fn($ty) -> Value = $lift;
                self.0.observed($name, lift(value));
            }
        )*
    };
}

generated!(gen_counter, "golden/counter.rs");
generated!(gen_fib, "golden/fib.rs");
generated!(gen_lag, "golden/lag.rs");
generated!(gen_heater, "golden/heater.rs");
generated!(gen_total_ops, "golden/total_ops.rs");
generated!(gen_arrays, "golden/arrays.rs");
generated!(gen_maths, "golden/maths.rs");
generated!(gen_structs, "golden/structs.rs");
generated!(gen_operators, "golden/operators.rs");

/// An environment for a monitor that reads nothing.
struct NoEnv;

/// The exit criterion for this milestone: a generated monitor occupies exactly
/// the memory the analysis reports, for every specification in the corpus.
///
/// `resources` lays the state out under `repr(C)` and the generator emits it
/// that way, so this compares a computed figure against a real compiled type
/// rather than against another computation.
#[test]
fn every_monitor_occupies_exactly_the_reported_footprint() {
    macro_rules! check {
        ($name:literal, $spec:expr, $module:ident) => {{
            let footprint = copilot_core::resources(&$spec);
            assert_eq!(
                size_of::<$module::Monitor>(),
                footprint.state_bytes,
                "{}: size",
                $name
            );
            assert_eq!(
                align_of::<$module::Monitor>(),
                footprint.state_align,
                "{}: alignment",
                $name
            );
        }};
    }

    check!("counter", support::counter(), gen_counter);
    check!("fib", support::fib(), gen_fib);
    check!("lag", support::lag(), gen_lag);
    check!("heater", support::heater(), gen_heater);
    check!("total_ops", support::total_ops(), gen_total_ops);
    check!("arrays", support::arrays(), gen_arrays);
    check!("maths", support::maths(), gen_maths);
    check!("structs", support::structs(), gen_structs);
    check!("operators", support::operators(), gen_operators);
}

// ---------------------------------------------------------------------------
// counter
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CounterHandler(Recorder);

impl gen_counter::Handler for CounterHandler {
    fn every_third(&mut self, arg0: u64) {
        self.0.fired("every_third", vec![Word64(arg0)]);
    }
    fn observe_counter(&mut self, value: u64) {
        self.0.observed("counter", Word64(value));
    }
}

impl gen_counter::Env for NoEnv {}

#[test]
fn counter_agrees() {
    let spec = support::counter();
    let samples = no_samples(12);
    let expected = interpret(&spec, &samples, &["counter", "every_third"]);

    let mut monitor = gen_counter::Monitor::new();
    let mut handler = CounterHandler::default();
    for step in &expected {
        monitor.step(&mut NoEnv, &mut handler);
        assert_eq!(&handler.0.take(), step);
    }
}

// ---------------------------------------------------------------------------
// fib
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FibHandler(Recorder);

impl gen_fib::Handler for FibHandler {
    fn observe_fib(&mut self, value: u64) {
        self.0.observed("fib", Word64(value));
    }
}

impl gen_fib::Env for NoEnv {}

#[test]
fn fib_agrees() {
    let spec = support::fib();
    let samples = no_samples(20);
    let expected = interpret(&spec, &samples, &["fib"]);

    let mut monitor = gen_fib::Monitor::new();
    let mut handler = FibHandler::default();
    for step in &expected {
        monitor.step(&mut NoEnv, &mut handler);
        assert_eq!(&handler.0.take(), step);
    }
}

// ---------------------------------------------------------------------------
// lag — the phase-separation case
// ---------------------------------------------------------------------------

#[derive(Default)]
struct LagHandler(Recorder);

impl gen_lag::Handler for LagHandler {
    fn observe_leader(&mut self, value: u32) {
        self.0.observed("leader", Word32(value));
    }
    fn observe_follower(&mut self, value: u32) {
        self.0.observed("follower", Word32(value));
    }
}

impl gen_lag::Env for NoEnv {}

/// If the generator committed each stream as it computed it, `follower` would
/// see `leader`'s new value and the lag would vanish. Both the comparison and
/// the explicit expectation below would fail.
#[test]
fn lag_agrees_and_actually_lags() {
    let spec = support::lag();
    let samples = no_samples(6);
    let expected = interpret(&spec, &samples, &["leader", "follower"]);

    let mut monitor = gen_lag::Monitor::new();
    let mut handler = LagHandler::default();
    let mut followers = Vec::new();
    for step in &expected {
        monitor.step(&mut NoEnv, &mut handler);
        let events = handler.0.take();
        assert_eq!(&events, step);
        followers.push(events[1].clone());
    }

    let expected_lag: Vec<Event> = [0u32, 0, 1, 2, 3, 4]
        .into_iter()
        .map(|v| Event::Observed("follower", Word32(v)))
        .collect();
    assert_eq!(followers, expected_lag);
}

// ---------------------------------------------------------------------------
// heater
// ---------------------------------------------------------------------------

#[derive(Default)]
struct HeaterHandler(Recorder);

impl gen_heater::Handler for HeaterHandler {
    fn heat_on(&mut self, arg0: f32) {
        self.0.fired("heat_on", vec![Value::Float(arg0)]);
    }
    fn heat_off(&mut self, arg0: f32) {
        self.0.fired("heat_off", vec![Value::Float(arg0)]);
    }
    fn observe_celsius(&mut self, value: f32) {
        self.0.observed("celsius", Value::Float(value));
    }
    fn observe_heating(&mut self, value: bool) {
        self.0.observed("heating", Bool(value));
    }
}

struct HeaterEnv(f32);

impl gen_heater::Env for HeaterEnv {
    fn temperature(&mut self) -> f32 {
        self.0
    }
}

proptest! {
    /// Arbitrary `f32` includes NaN and both infinities, which is where the
    /// comparison operators are most likely to diverge: every comparison
    /// against NaN must be false, including `<=` and `>=`.
    #[test]
    fn heater_agrees(readings in prop::collection::vec(any::<f32>(), 1..24)) {
        let spec = support::heater();
        let samples: Vec<Samples> = readings
            .iter()
            .map(|&t| Samples::none().with("temperature", Value::Float(t)))
            .collect();
        let expected = interpret(&spec, &samples, &["celsius", "heating", "heat_on", "heat_off"]);

        let mut monitor = gen_heater::Monitor::new();
        let mut handler = HeaterHandler::default();
        for (&reading, step) in readings.iter().zip(&expected) {
            monitor.step(&mut HeaterEnv(reading), &mut handler);
            prop_assert_eq!(&handler.0.take(), step);
        }
    }
}

// ---------------------------------------------------------------------------
// total_ops
// ---------------------------------------------------------------------------

#[derive(Default)]
struct TotalOpsHandler(Recorder);

impl gen_total_ops::Handler for TotalOpsHandler {
    fn observe_byte(&mut self, value: u8) {
        self.0.observed("byte", Word8(value));
    }
    fn observe_quotient(&mut self, value: i32) {
        self.0.observed("quotient", Int32(value));
    }
    fn observe_remainder(&mut self, value: i32) {
        self.0.observed("remainder", Int32(value));
    }
    fn observe_shifted(&mut self, value: u32) {
        self.0.observed("shifted", Word32(value));
    }
    fn observe_negated(&mut self, value: i32) {
        self.0.observed("negated", Int32(value));
    }
    fn observe_magnitude(&mut self, value: i32) {
        self.0.observed("magnitude", Int32(value));
    }
    fn observe_sign(&mut self, value: i32) {
        self.0.observed("sign", Int32(value));
    }
}

struct ShiftEnv(u32);

impl gen_total_ops::Env for ShiftEnv {
    fn shift(&mut self) -> u32 {
        self.0
    }
}

proptest! {
    /// Arbitrary shift amounts reach far past the operand width, where the
    /// result must be zero rather than the amount wrapping modulo 32. The
    /// divisor stream counts down through zero on its own.
    #[test]
    fn total_ops_agrees(shifts in prop::collection::vec(any::<u32>(), 1..16)) {
        let spec = support::total_ops();
        let samples: Vec<Samples> = shifts
            .iter()
            .map(|&s| Samples::none().with("shift", Word32(s)))
            .collect();
        let names = ["byte", "quotient", "remainder", "shifted", "negated", "magnitude", "sign"];
        let expected = interpret(&spec, &samples, &names);

        let mut monitor = gen_total_ops::Monitor::new();
        let mut handler = TotalOpsHandler::default();
        for (&shift, step) in shifts.iter().zip(&expected) {
            monitor.step(&mut ShiftEnv(shift), &mut handler);
            prop_assert_eq!(&handler.0.take(), step);
        }
    }
}

// ---------------------------------------------------------------------------
// arrays
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ArraysHandler(Recorder);

impl gen_arrays::Handler for ArraysHandler {
    fn observe_history(&mut self, value: [u32; 3]) {
        self.0
            .observed("history", Value::Array(value.map(Word32).to_vec()));
    }
    fn observe_oldest(&mut self, value: u32) {
        self.0.observed("oldest", Word32(value));
    }
    fn observe_wrapped(&mut self, value: u32) {
        self.0.observed("wrapped", Word32(value));
    }
}

impl gen_arrays::Env for NoEnv {}

#[test]
fn arrays_agree() {
    let spec = support::arrays();
    let samples = no_samples(10);
    let expected = interpret(&spec, &samples, &["history", "oldest", "wrapped"]);

    let mut monitor = gen_arrays::Monitor::new();
    let mut handler = ArraysHandler::default();
    for step in &expected {
        monitor.step(&mut NoEnv, &mut handler);
        assert_eq!(&handler.0.take(), step);
    }
}

// ---------------------------------------------------------------------------
// maths
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MathsHandler(Recorder);

impl gen_maths::Handler for MathsHandler {
    fn observe_sqrt(&mut self, value: f64) {
        self.0.observed("sqrt", Double(value));
    }
    fn observe_floor(&mut self, value: f64) {
        self.0.observed("floor", Double(value));
    }
    fn observe_ceil(&mut self, value: f64) {
        self.0.observed("ceil", Double(value));
    }
    fn observe_sin(&mut self, value: f64) {
        self.0.observed("sin", Double(value));
    }
    fn observe_exp(&mut self, value: f64) {
        self.0.observed("exp", Double(value));
    }
    fn observe_atan2(&mut self, value: f64) {
        self.0.observed("atan2", Double(value));
    }
    fn observe_log(&mut self, value: f64) {
        self.0.observed("log", Double(value));
    }
    fn observe_powi(&mut self, value: f64) {
        self.0.observed("powi", Double(value));
    }
}

struct MathsEnv(f64);

impl gen_maths::Env for MathsEnv {
    fn x(&mut self) -> f64 {
        self.0
    }
}

proptest! {
    /// Negative and non-finite arguments matter here: `sqrt(-1)` and `log(-1)`
    /// are NaN, and NaN must survive the comparison in `Value`'s bitwise
    /// equality identically on both sides.
    #[test]
    fn maths_agrees(xs in prop::collection::vec(any::<f64>(), 1..16)) {
        let spec = support::maths();
        let samples: Vec<Samples> = xs
            .iter()
            .map(|&x| Samples::none().with("x", Double(x)))
            .collect();
        let names = ["sqrt", "floor", "ceil", "sin", "exp", "atan2", "log", "powi"];
        let expected = interpret(&spec, &samples, &names);

        let mut monitor = gen_maths::Monitor::new();
        let mut handler = MathsHandler::default();
        for (&x, step) in xs.iter().zip(&expected) {
            monitor.step(&mut MathsEnv(x), &mut handler);
            prop_assert_eq!(&handler.0.take(), step);
        }
    }
}

// ---------------------------------------------------------------------------
// structs
// ---------------------------------------------------------------------------

/// Builds the `Value` the interpreter produces for a `Reading`.
///
/// Field order matters: the IR stores struct fields in declaration order, and
/// two values with the fields permuted are not equal.
fn reading(altitude: f32, samples: u16, valid: bool) -> Value {
    Value::Struct {
        name: "Reading".into(),
        fields: vec![
            ("altitude".into(), Value::Float(altitude)),
            ("samples".into(), Value::Word16(samples)),
            ("valid".into(), Bool(valid)),
        ],
    }
}

#[derive(Default)]
struct StructsHandler(Recorder);

impl gen_structs::Handler for StructsHandler {
    fn lost_signal(&mut self, arg0: f32) {
        self.0.fired("lost_signal", vec![Value::Float(arg0)]);
    }

    fn observe_latest(&mut self, value: gen_structs::Reading) {
        self.0.observed(
            "latest",
            reading(value.altitude, value.samples, value.valid),
        );
    }

    observers! {
        observe_altitude => "altitude": f32 = Value::Float;
        observe_samples => "samples": u16 = Value::Word16;
    }
}

struct StructsEnv(gen_structs::Reading);

impl gen_structs::Env for StructsEnv {
    fn sensor(&mut self) -> gen_structs::Reading {
        self.0
    }
}

proptest! {
    /// Exercises field projection, whole-struct update, and selection between
    /// two structs. `f32` is arbitrary, so NaN altitudes flow through the
    /// struct copy and out again unchanged.
    #[test]
    fn structs_agree(
        readings in prop::collection::vec(
            (any::<f32>(), any::<u16>(), any::<bool>()),
            1..16,
        ),
    ) {
        let spec = support::structs();
        let samples: Vec<Samples> = readings
            .iter()
            .map(|&(altitude, count, valid)| {
                Samples::none().with("sensor", reading(altitude, count, valid))
            })
            .collect();
        let names = ["latest", "altitude", "samples", "lost_signal"];
        let expected = interpret(&spec, &samples, &names);

        let mut monitor = gen_structs::Monitor::new();
        let mut handler = StructsHandler::default();
        for (&(altitude, count, valid), step) in readings.iter().zip(&expected) {
            let value = gen_structs::Reading { altitude, samples: count, valid };
            monitor.step(&mut StructsEnv(value), &mut handler);
            prop_assert_eq!(&handler.0.take(), step);
        }
    }
}

// ---------------------------------------------------------------------------
// operators
// ---------------------------------------------------------------------------

#[derive(Default)]
struct OperatorsHandler(Recorder);

impl gen_operators::Handler for OperatorsHandler {
    observers! {
        observe_add => "add": i32 = Int32;
        observe_sub => "sub": i32 = Int32;
        observe_mul => "mul": i32 = Int32;
        observe_div => "div": i32 = Int32;
        observe_rem => "rem": i32 = Int32;
        observe_neg => "neg": i32 = Int32;
        observe_abs => "abs": i32 = Int32;
        observe_signum => "signum": i32 = Int32;
        observe_abs_unsigned => "abs_unsigned": u16 = Value::Word16;
        observe_signum_unsigned => "signum_unsigned": u16 = Value::Word16;

        observe_bw_not => "bw_not": u16 = Value::Word16;
        observe_bw_and => "bw_and": u16 = Value::Word16;
        observe_bw_or => "bw_or": u16 = Value::Word16;
        observe_bw_xor => "bw_xor": u16 = Value::Word16;
        observe_shl => "shl": u16 = Value::Word16;
        observe_shr => "shr": u16 = Value::Word16;

        observe_cast_widen => "cast_widen": i64 = Value::Int64;
        observe_cast_narrow => "cast_narrow": u8 = Word8;
        observe_cast_to_float => "cast_to_float": f64 = Double;

        observe_fdiv => "fdiv": f64 = Double;
        observe_recip => "recip": f64 = Double;
        observe_f32_add => "f32_add": f32 = Value::Float;
        observe_f32_sqrt => "f32_sqrt": f32 = Value::Float;
        observe_f32_abs => "f32_abs": f32 = Value::Float;
        observe_f32_signum => "f32_signum": f32 = Value::Float;

        observe_eq => "eq": bool = Bool;
        observe_ne => "ne": bool = Bool;
        observe_lt => "lt": bool = Bool;
        observe_le => "le": bool = Bool;
        observe_gt => "gt": bool = Bool;
        observe_ge => "ge": bool = Bool;
        observe_float_eq => "float_eq": bool = Bool;
        observe_float_le => "float_le": bool = Bool;

        observe_and => "and": bool = Bool;
        observe_or => "or": bool = Bool;
        observe_xor => "xor": bool = Bool;
        observe_not => "not": bool = Bool;
        observe_implies => "implies": bool = Bool;
        observe_mux => "mux": i32 = Int32;
    }
}

struct OperatorsEnv {
    i: i32,
    j: i32,
    u: u16,
    v: u16,
    f: f64,
    h: f64,
    g: f32,
    p: bool,
    q: bool,
}

impl gen_operators::Env for OperatorsEnv {
    fn i(&mut self) -> i32 {
        self.i
    }
    fn j(&mut self) -> i32 {
        self.j
    }
    fn u(&mut self) -> u16 {
        self.u
    }
    fn v(&mut self) -> u16 {
        self.v
    }
    fn f(&mut self) -> f64 {
        self.f
    }
    fn h(&mut self) -> f64 {
        self.h
    }
    fn g(&mut self) -> f32 {
        self.g
    }
    fn p(&mut self) -> bool {
        self.p
    }
    fn q(&mut self) -> bool {
        self.q
    }
}

proptest! {
    /// Every operator, on arbitrary inputs.
    ///
    /// The inputs are unconstrained on purpose: `j` reaches zero, so division
    /// and remainder hit their defined-at-zero case; `i` reaches `i32::MIN`,
    /// where `abs` and `MIN / -1` must wrap rather than trap; `i` is also the
    /// shift amount, so it goes negative and past the operand width; and the
    /// floats include NaN and both infinities.
    #[test]
    fn operators_agree(
        inputs in prop::collection::vec(
            (
                any::<i32>(), any::<i32>(),
                any::<u16>(), any::<u16>(),
                any::<f64>(), any::<f64>(),
                any::<f32>(),
                any::<bool>(), any::<bool>(),
            ),
            1..12,
        ),
    ) {
        let spec = support::operators();
        let samples: Vec<Samples> = inputs
            .iter()
            .map(|&(i, j, u, v, f, h, g, p, q)| {
                Samples::none()
                    .with("i", Int32(i))
                    .with("j", Int32(j))
                    .with("u", Value::Word16(u))
                    .with("v", Value::Word16(v))
                    .with("f", Double(f))
                    .with("h", Double(h))
                    .with("g", Value::Float(g))
                    .with("p", Bool(p))
                    .with("q", Bool(q))
            })
            .collect();

        let names = [
            "add", "sub", "mul", "div", "rem", "neg", "abs", "signum",
            "abs_unsigned", "signum_unsigned", "bw_not", "bw_and", "bw_or",
            "bw_xor", "shl", "shr", "cast_widen", "cast_narrow",
            "cast_to_float", "fdiv", "recip", "f32_add", "f32_sqrt", "f32_abs",
            "f32_signum", "eq", "ne", "lt", "le", "gt", "ge", "float_eq",
            "float_le", "and", "or", "xor", "not", "implies", "mux",
        ];
        let expected = interpret(&spec, &samples, &names);

        let mut monitor = gen_operators::Monitor::new();
        let mut handler = OperatorsHandler::default();
        for (&(i, j, u, v, f, h, g, p, q), step) in inputs.iter().zip(&expected) {
            monitor.step(&mut OperatorsEnv { i, j, u, v, f, h, g, p, q }, &mut handler);
            prop_assert_eq!(&handler.0.take(), step);
        }
    }
}
