//! The stepping interpreter.

use crate::eval::Context;
use copilot_core::{Error, IndexPolicy, Result, Spec, Type, Value};
use std::collections::HashMap;

/// Supplies external variables, one sample per step.
pub trait Env {
    /// The value of `name` for this step, or `None` if the environment cannot
    /// supply it.
    fn sample(&mut self, name: &str, ty: &Type) -> Option<Value>;
}

/// A fixed set of samples, for specs driven from a recorded trace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Samples(pub HashMap<String, Value>);

impl Samples {
    /// An empty sample set, for specs with no external variables.
    pub fn none() -> Self {
        Samples::default()
    }

    /// Adds a sample.
    pub fn with(mut self, name: &str, value: Value) -> Self {
        self.0.insert(name.to_string(), value);
        self
    }
}

impl Env for Samples {
    fn sample(&mut self, name: &str, _ty: &Type) -> Option<Value> {
        self.0.get(name).cloned()
    }
}

/// A trigger that fired, with the arguments it was passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    /// The handler's name.
    pub name: String,
    /// Its arguments, in declaration order.
    pub args: Vec<Value>,
}

/// What one step produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    /// Observer values, in declaration order.
    pub observers: Vec<(String, Value)>,
    /// Triggers that fired, in declaration order.
    pub fired: Vec<Fired>,
}

impl Observation {
    /// The value of the named observer, if the spec has one.
    pub fn observer(&self, name: &str) -> Option<&Value> {
        self.observers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v)
    }

    /// Whether the named trigger fired this step.
    pub fn did_fire(&self, name: &str) -> bool {
        self.fired.iter().any(|f| f.name == name)
    }
}

/// A running specification.
///
/// Holds exactly the state [`copilot_core::resources`] reports and nothing
/// else: one ring buffer and one rotating index per stream. It is a genuine
/// constant-memory implementation rather than a lazy-list oracle, which is what
/// makes it a meaningful reference for the code generators — the same
/// representation, evaluated a different way.
#[derive(Debug, Clone)]
pub struct Monitor<'a> {
    spec: &'a Spec,
    buffers: Vec<Vec<Value>>,
    positions: Vec<usize>,
    index_policy: IndexPolicy,
    step: u64,
}

impl<'a> Monitor<'a> {
    /// Starts a monitor on a validated spec.
    pub fn new(spec: &'a Spec) -> Result<Self> {
        Self::with_policy(spec, IndexPolicy::default())
    }

    /// Starts a monitor with a given out-of-range subscript policy.
    ///
    /// Must match the policy the code generator is configured with, or the
    /// interpreter stops being a valid oracle for it.
    pub fn with_policy(spec: &'a Spec, index_policy: IndexPolicy) -> Result<Self> {
        spec.validate()?;
        Ok(Monitor {
            buffers: spec.streams.iter().map(|s| s.buffer.clone()).collect(),
            positions: vec![0; spec.streams.len()],
            spec,
            index_policy,
            step: 0,
        })
    }

    /// How many steps have been taken.
    pub fn steps_taken(&self) -> u64 {
        self.step
    }

    /// The current value of a stream, as `drop 0` would read it.
    pub fn peek(&self, stream: usize) -> &Value {
        let buffer = &self.buffers[stream];
        &buffer[self.positions[stream] % buffer.len()]
    }

    /// Advances one step.
    ///
    /// The four phases run in the order the crate documents, and the separation
    /// of the last two is the whole point: every stream's next value is
    /// computed from the state as it was at the start of the step, so no stream
    /// can observe another's update. Committing each stream as it is computed
    /// would silently be a different specification.
    pub fn step(&mut self, env: &mut impl Env) -> Result<Observation> {
        // 1. Sample each external variable exactly once.
        let mut samples = HashMap::new();
        for (name, ty) in self.spec.arena.externs() {
            let value = env.sample(name, ty).ok_or_else(|| Error::Mismatch {
                context: format!("environment did not supply external variable `{name}`"),
                expected: ty.clone(),
                found: ty.clone(),
            })?;
            if !value.matches(ty) {
                return Err(Error::Mismatch {
                    context: format!("external variable `{name}`"),
                    expected: ty.clone(),
                    found: ty.clone(),
                });
            }
            samples.insert(name.clone(), value);
        }

        let context = Context {
            arena: &self.spec.arena,
            buffers: &self.buffers,
            positions: &self.positions,
            samples: &samples,
            index_policy: self.index_policy,
        };

        // 2. Observe, and fire triggers whose guards hold.
        let mut observation = Observation::default();
        for observer in &self.spec.observers {
            observation
                .observers
                .push((observer.name.clone(), context.eval(observer.expr)?));
        }
        for trigger in &self.spec.triggers {
            if context.eval(trigger.guard)? != Value::Bool(true) {
                continue;
            }
            let args = trigger
                .args
                .iter()
                .map(|arg| context.eval(arg.expr))
                .collect::<Result<Vec<_>>>()?;
            observation.fired.push(Fired {
                name: trigger.name.clone(),
                args,
            });
        }

        // 3. Compute every stream's next value, still reading the old state.
        let next = self
            .spec
            .streams
            .iter()
            .map(|stream| context.eval(stream.expr))
            .collect::<Result<Vec<_>>>()?;

        // 4. Commit. The new value overwrites the slot holding the value that
        //    has just expired, and the read position moves on.
        for (index, value) in next.into_iter().enumerate() {
            let buffer = &mut self.buffers[index];
            let position = self.positions[index];
            buffer[position] = value;
            self.positions[index] = (position + 1) % buffer.len();
        }

        self.step += 1;
        Ok(observation)
    }

    /// Runs `env` for as many steps as it supplies samples for, collecting
    /// every observation.
    pub fn run(&mut self, env: &mut impl Env, steps: usize) -> Result<Vec<Observation>> {
        (0..steps).map(|_| self.step(env)).collect()
    }
}
