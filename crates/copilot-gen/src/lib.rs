//! Random well-typed specifications, for differential testing.
//!
//! Builds directly on [`copilot_core::Arena`] rather than through the typed
//! frontend, because generation is a dynamically typed activity: the generator
//! picks a type and then produces an expression of it. A useful side effect is
//! that everything here goes through the same door a macro frontend or a
//! deserialized specification would, so [`copilot_core::validate`] is exercised
//! rather than bypassed.
//!
//! Generation depends on nothing but a seed, so a failure is reproducible from
//! the seed printed alongside it.
//!
//! ```
//! use copilot_gen::{Config, Rng, spec, trace};
//!
//! let mut rng = Rng::new(20260722);
//! let generated = spec(&mut rng, &Config::default());
//! let inputs = trace(&mut rng, &generated, 8);
//!
//! assert!(copilot_core::validate(&generated).is_ok());
//! assert_eq!(inputs.len(), 8);
//! ```

mod rng;

pub use rng::Rng;

use copilot_core::{Arena, ExprId, Op1, Op2, Op3, Spec, StreamId, Type, Value};
use copilot_interp::Samples;

/// What kind of specifications to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// How many streams.
    pub streams: usize,
    /// How many external variables.
    pub externs: usize,
    /// Deepest expression nesting.
    pub depth: usize,
    /// Largest stream buffer.
    pub max_buffer: usize,
    /// How many extra expressions to observe, beyond the streams themselves.
    ///
    /// Observing only stream values leaves most of the language invisible: a
    /// comparison produces a `Bool`, so unless a stream happens to have that
    /// type, a mis-encoded signed comparison never reaches anything the test
    /// can see. These are drawn at every type, which is what makes a
    /// differential comparison able to localise a fault rather than merely
    /// notice one.
    pub extra_observers: usize,
    /// Whether to generate floating-point types.
    ///
    /// Off by default. The SMT encoding approximates floats unless it is asked
    /// for IEEE, so including them in a comparison against the interpreter
    /// would report disagreements that are the approximation working as
    /// documented rather than a bug.
    pub floats: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            streams: 3,
            externs: 3,
            depth: 4,
            max_buffer: 3,
            extra_observers: 6,
            floats: false,
        }
    }
}

const INTEGERS: &[Type] = &[
    Type::Int8,
    Type::Int16,
    Type::Int32,
    Type::Int64,
    Type::Word8,
    Type::Word16,
    Type::Word32,
    Type::Word64,
];

/// A random specification, with one observer per stream.
///
/// Every stream is observed, so a comparison between two engines sees all of
/// the generated state rather than whatever happens to reach a trigger.
pub fn spec(rng: &mut Rng, config: &Config) -> Spec {
    let mut generator = Generator {
        arena: Arena::new(),
        streams: Vec::new(),
        externs: Vec::new(),
        config: config.clone(),
    };

    let types = generator.type_pool();

    for _ in 0..config.externs.max(1) {
        let ty = rng.pick(&types).clone();
        let name = format!("e{}", generator.externs.len());
        generator
            .arena
            .extern_var(&name, ty.clone())
            .expect("a fresh name at a fixed type is always accepted");
        generator.externs.push((name, ty));
    }

    for _ in 0..config.streams.max(1) {
        let ty = rng.pick(&types).clone();
        let buffer_len = rng.between(1, config.max_buffer.max(1));
        let id = generator
            .arena
            .declare_stream(ty.clone(), buffer_len)
            .expect("a positive buffer length is always accepted");
        generator.streams.push((id, ty, buffer_len));
    }

    // Transition expressions are built after every stream is declared, so a
    // stream can read any other — including itself.
    let bodies: Vec<ExprId> = generator
        .streams
        .clone()
        .into_iter()
        .map(|(_, ty, _)| generator.expr(rng, &ty, config.depth))
        .collect();

    let streams = generator.streams.clone();
    let mut spec = Spec::new(std::mem::take(&mut generator.arena));
    for ((id, ty, buffer_len), body) in streams.iter().zip(bodies) {
        let buffer: Vec<Value> = (0..*buffer_len).map(|_| value(rng, ty)).collect();
        spec.define_stream(*id, buffer, body)
            .expect("the body was built at the stream's own type");
    }
    for (index, (id, _, _)) in streams.iter().enumerate() {
        let read = spec
            .arena
            .drop_(0, *id)
            .expect("every stream buffers at least one value");
        spec.observe(format!("s{index}"), read)
            .expect("observer names are fresh");
    }

    // Extra observers over freshly generated expressions, so operators whose
    // result type no stream happens to have are still watched.
    let mut generator = Generator {
        arena: std::mem::take(&mut spec.arena),
        streams,
        externs: generator.externs,
        config: config.clone(),
    };
    let mut extra = Vec::new();
    for _ in 0..config.extra_observers {
        let ty = rng.pick(&types).clone();
        extra.push(generator.expr(rng, &ty, config.depth));
    }
    spec.arena = generator.arena;
    for (index, expr) in extra.into_iter().enumerate() {
        spec.observe(format!("o{index}"), expr)
            .expect("observer names are fresh");
    }

    spec
}

/// Random inputs for a specification's external variables.
pub fn trace(rng: &mut Rng, spec: &Spec, steps: usize) -> Vec<Samples> {
    let externs = spec.arena.externs().to_vec();
    (0..steps)
        .map(|_| {
            externs.iter().fold(Samples::none(), |samples, (name, ty)| {
                samples.with(name, value(rng, ty))
            })
        })
        .collect()
}

/// A random value of the given type.
///
/// Biased towards the values that break things: zero, one, the extremes, and
/// small magnitudes. Uniformly random 64-bit integers almost never divide
/// evenly, sit at a boundary, or make a shift amount land in range.
pub fn value(rng: &mut Rng, ty: &Type) -> Value {
    macro_rules! integer {
        ($variant:ident, $ty:ty) => {{
            let raw = match rng.below(8) {
                0 => 0,
                1 => 1,
                2 => <$ty>::MAX,
                3 => <$ty>::MIN,
                4 => (rng.next_u64() % 8) as $ty,
                5 => <$ty>::MAX.wrapping_sub((rng.next_u64() % 4) as $ty),
                _ => rng.next_u64() as $ty,
            };
            Value::$variant(raw)
        }};
    }

    match ty {
        Type::Bool => Value::Bool(rng.flip()),
        Type::Int8 => integer!(Int8, i8),
        Type::Int16 => integer!(Int16, i16),
        Type::Int32 => integer!(Int32, i32),
        Type::Int64 => integer!(Int64, i64),
        Type::Word8 => integer!(Word8, u8),
        Type::Word16 => integer!(Word16, u16),
        Type::Word32 => integer!(Word32, u32),
        Type::Word64 => integer!(Word64, u64),
        Type::Float => Value::Float(match rng.below(6) {
            0 => 0.0,
            1 => 1.0,
            2 => -1.0,
            3 => f32::MAX,
            4 => f32::MIN_POSITIVE,
            _ => rng.next_u64() as f32 / 1e9,
        }),
        Type::Double => Value::Double(match rng.below(6) {
            0 => 0.0,
            1 => 1.0,
            2 => -1.0,
            3 => f64::MAX,
            4 => f64::MIN_POSITIVE,
            _ => rng.next_u64() as f64 / 1e9,
        }),
        Type::Array { elem, len } => Value::Array((0..*len).map(|_| value(rng, elem)).collect()),
        Type::Struct(definition) => Value::Struct {
            name: definition.name.clone(),
            fields: definition
                .fields
                .iter()
                .map(|(field, ty)| (field.clone(), value(rng, ty)))
                .collect(),
        },
    }
}

struct Generator {
    arena: Arena,
    streams: Vec<(StreamId, Type, usize)>,
    externs: Vec<(String, Type)>,
    config: Config,
}

impl Generator {
    fn type_pool(&self) -> Vec<Type> {
        let mut types = vec![Type::Bool];
        types.extend(INTEGERS.iter().cloned());
        if self.config.floats {
            types.push(Type::Float);
            types.push(Type::Double);
        }
        types
    }

    /// An expression of the given type, at most `depth` deep.
    fn expr(&mut self, rng: &mut Rng, ty: &Type, depth: usize) -> ExprId {
        if depth == 0 {
            return self.leaf(rng, ty);
        }
        // Every branch below is fallible only because the arena typechecks what
        // it is given; a `None` means this generator picked an operand shape the
        // type does not admit, and falling back to a leaf keeps generation total
        // rather than skewing it by retrying.
        let attempt = match rng.below(if ty.is_integral() { 12 } else { 10 }) {
            0 | 1 => Some(self.leaf(rng, ty)),
            2 => self.mux(rng, ty, depth),
            _ if *ty == Type::Bool => self.boolean(rng, depth),
            _ if ty.is_integral() => self.integral(rng, ty, depth),
            _ => Some(self.leaf(rng, ty)),
        };
        attempt.unwrap_or_else(|| self.leaf(rng, ty))
    }

    fn leaf(&mut self, rng: &mut Rng, ty: &Type) -> ExprId {
        let readable: Vec<(StreamId, usize)> = self
            .streams
            .iter()
            .filter(|(_, stream_ty, _)| stream_ty == ty)
            .map(|(id, _, len)| (*id, *len))
            .collect();
        let externs: Vec<String> = self
            .externs
            .iter()
            .filter(|(_, extern_ty)| extern_ty == ty)
            .map(|(name, _)| name.clone())
            .collect();

        let choice = rng.below(3);
        if choice == 0 && !readable.is_empty() {
            let (id, len) = *rng.pick(&readable);
            let idx = rng.below(len) as u32;
            return self
                .arena
                .drop_(idx, id)
                .expect("the index was drawn below the buffer length");
        }
        if choice == 1 && !externs.is_empty() {
            let name = rng.pick(&externs).clone();
            return self
                .arena
                .extern_var(&name, ty.clone())
                .expect("the name was declared at this type");
        }
        let literal = value(rng, ty);
        self.arena
            .constant(ty.clone(), literal)
            .expect("the value was generated at this type")
    }

    fn mux(&mut self, rng: &mut Rng, ty: &Type, depth: usize) -> Option<ExprId> {
        let condition = self.expr(rng, &Type::Bool, depth - 1);
        let on_true = self.expr(rng, ty, depth - 1);
        let on_false = self.expr(rng, ty, depth - 1);
        self.arena
            .op3(Op3::Mux(ty.clone()), condition, on_true, on_false)
            .ok()
    }

    fn boolean(&mut self, rng: &mut Rng, depth: usize) -> Option<ExprId> {
        match rng.below(10) {
            0 => {
                let a = self.expr(rng, &Type::Bool, depth - 1);
                self.arena.op1(Op1::Not, a).ok()
            }
            1 | 2 => {
                let a = self.expr(rng, &Type::Bool, depth - 1);
                let b = self.expr(rng, &Type::Bool, depth - 1);
                let op = if rng.flip() { Op2::And } else { Op2::Or };
                self.arena.op2(op, a, b).ok()
            }
            _ => {
                // A comparison, which is where the signed and unsigned
                // encodings differ and so the most valuable thing to generate.
                let operand = rng.pick(INTEGERS).clone();
                let a = self.expr(rng, &operand, depth - 1);
                let b = self.expr(rng, &operand, depth - 1);
                let op = match rng.below(6) {
                    0 => Op2::Eq(operand),
                    1 => Op2::Ne(operand),
                    2 => Op2::Lt(operand),
                    3 => Op2::Le(operand),
                    4 => Op2::Gt(operand),
                    _ => Op2::Ge(operand),
                };
                self.arena.op2(op, a, b).ok()
            }
        }
    }

    fn integral(&mut self, rng: &mut Rng, ty: &Type, depth: usize) -> Option<ExprId> {
        match rng.below(14) {
            0 => {
                let a = self.expr(rng, ty, depth - 1);
                self.arena.op1(Op1::Abs(ty.clone()), a).ok()
            }
            1 => {
                let a = self.expr(rng, ty, depth - 1);
                self.arena.op1(Op1::Sign(ty.clone()), a).ok()
            }
            2 => {
                let a = self.expr(rng, ty, depth - 1);
                self.arena.op1(Op1::BwNot(ty.clone()), a).ok()
            }
            3 => {
                // A cast, which is where widening has to agree about sign
                // extension and narrowing about truncation.
                let from = rng.pick(INTEGERS).clone();
                let a = self.expr(rng, &from, depth - 1);
                self.arena
                    .op1(
                        Op1::Cast {
                            from,
                            to: ty.clone(),
                        },
                        a,
                    )
                    .ok()
            }
            4 | 5 => {
                // Shifts, whose behaviour past the operand width is defined
                // rather than inherited.
                let amount = rng.pick(INTEGERS).clone();
                let a = self.expr(rng, ty, depth - 1);
                let b = self.expr(rng, &amount, depth - 1);
                let op = if rng.flip() {
                    Op2::BwShiftL {
                        val: ty.clone(),
                        amount,
                    }
                } else {
                    Op2::BwShiftR {
                        val: ty.clone(),
                        amount,
                    }
                };
                self.arena.op2(op, a, b).ok()
            }
            other => {
                let a = self.expr(rng, ty, depth - 1);
                let b = self.expr(rng, ty, depth - 1);
                let op = match other {
                    6 => Op2::Add(ty.clone()),
                    7 => Op2::Sub(ty.clone()),
                    8 => Op2::Mul(ty.clone()),
                    // Division and remainder, whose divisor reaches zero often
                    // because the value generator favours it.
                    9 => Op2::Div(ty.clone()),
                    10 => Op2::Mod(ty.clone()),
                    11 => Op2::BwAnd(ty.clone()),
                    12 => Op2::BwOr(ty.clone()),
                    _ => Op2::BwXor(ty.clone()),
                };
                self.arena.op2(op, a, b).ok()
            }
        }
    }
}
