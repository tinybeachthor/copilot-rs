//! The reference `ir_step` agrees with the interpreter.
//!
//! The Kani proof shows the monitor equals `ir_step`. That is only worth having
//! if `ir_step` is itself right — otherwise the proof establishes that the
//! monitor equals a wrong reference. And the residual risk is specific: the
//! monitor and the reference share the *shape* of their operator lowerings, so
//! a bug in an operator that both made identically would pass the proof.
//!
//! This closes that gap by checking `ir_step` against a third implementation
//! that shares nothing with either code generator — the interpreter, which
//! walks the IR directly. `ir_step ≈ interpreter` here, `monitor ≡ ir_step` in
//! Kani, and the two compose to `monitor ≈ interpreter` over the whole state
//! space.
//!
//! `ir_step` is generated Rust, so it has to be compiled to be run. Every case
//! goes into one program, in its own module, and is compiled once.

mod support;

use copilot_core::{Spec, Type, Value};
use copilot_gen::{Config, Rng, spec, trace};
use copilot_interp::{Monitor, Samples};
use copilot_rust::{Settings as RustSettings, generate};
use copilot_verifier::{Settings, generate_harness};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

struct Case {
    spec: Spec,
    samples: Vec<Samples>,
}

fn rust_ty(ty: &Type) -> &'static str {
    match ty {
        Type::Bool => "bool",
        Type::Int8 => "i8",
        Type::Int16 => "i16",
        Type::Int32 => "i32",
        Type::Int64 => "i64",
        Type::Word8 => "u8",
        Type::Word16 => "u16",
        Type::Word32 => "u32",
        Type::Word64 => "u64",
        other => panic!("the generator does not produce {other} here"),
    }
}

fn literal(value: &Value) -> String {
    match value {
        Value::Bool(v) => v.to_string(),
        Value::Int8(v) if *v == i8::MIN => "i8::MIN".into(),
        Value::Int16(v) if *v == i16::MIN => "i16::MIN".into(),
        Value::Int32(v) if *v == i32::MIN => "i32::MIN".into(),
        Value::Int64(v) if *v == i64::MIN => "i64::MIN".into(),
        Value::Int8(v) => format!("{v}i8"),
        Value::Int16(v) => format!("{v}i16"),
        Value::Int32(v) => format!("{v}i32"),
        Value::Int64(v) => format!("{v}i64"),
        Value::Word8(v) => format!("{v}u8"),
        Value::Word16(v) => format!("{v}u16"),
        Value::Word32(v) => format!("{v}u32"),
        Value::Word64(v) => format!("{v}u64"),
        other => panic!("the generator does not produce {other:?} here"),
    }
}

fn render(value: &Value) -> String {
    match value {
        Value::Bool(v) => v.to_string(),
        Value::Int8(v) => v.to_string(),
        Value::Int16(v) => v.to_string(),
        Value::Int32(v) => v.to_string(),
        Value::Int64(v) => v.to_string(),
        Value::Word8(v) => v.to_string(),
        Value::Word16(v) => v.to_string(),
        Value::Word32(v) => v.to_string(),
        Value::Word64(v) => v.to_string(),
        other => panic!("the generator does not produce {other:?} here"),
    }
}

/// The reference harness for one case, wrapped in a module so many can share a
/// program. The generated `#![allow]` line is dropped — an inner attribute
/// cannot follow the module's own — and replaced with one covering the module.
fn module(index: usize, harness: &str) -> String {
    let body: String = harness
        .lines()
        .filter(|line| !line.trim_start().starts_with("#!["))
        .map(|line| format!("    {line}\n"))
        .collect();
    format!("mod m{index} {{\n    #![allow(warnings)]\n{body}}}\n")
}

/// The whole program: every case's reference, plus a driver that runs `ir_step`
/// forward from the initial state and prints what it observed.
fn build_program(cases: &[Case]) -> String {
    let mut out = String::new();
    for (index, case) in cases.iter().enumerate() {
        let monitor = generate(&case.spec, &RustSettings::default()).unwrap();
        let harness = generate_harness(&case.spec, &monitor, &Settings::default())
            .unwrap_or_else(|e| panic!("case {index}: {e}"));
        out.push_str(&module(index, &harness));

        for (name, ty) in case.spec.arena.externs() {
            let values: Vec<String> = case
                .samples
                .iter()
                .map(|sample| {
                    let mut sample = sample.clone();
                    let value = copilot_interp::Env::sample(&mut sample, name, ty)
                        .expect("the generator supplies every external variable");
                    literal(&value)
                })
                .collect();
            let _ = writeln!(
                out,
                "const IN{index}_{}: [{}; {}] = [{}];",
                name.to_uppercase(),
                rust_ty(ty),
                values.len(),
                values.join(", ")
            );
        }
    }

    let _ = writeln!(out, "fn main() {{");
    for (index, case) in cases.iter().enumerate() {
        let args: Vec<String> = case
            .spec
            .arena
            .externs()
            .iter()
            .map(|(name, _)| format!("IN{index}_{}[t]", name.to_uppercase()))
            .collect();
        let observers: Vec<String> = case
            .spec
            .observers
            .iter()
            .map(|o| format!("rec.o_{}.unwrap()", o.name))
            .collect();
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "        let mut st = m{index}::State::initial();");
        let _ = writeln!(out, "        for t in 0..{} {{", case.samples.len());
        let _ = writeln!(
            out,
            "            let (next, rec) = m{index}::ir_step(&st{}{});",
            if args.is_empty() { "" } else { ", " },
            args.join(", ")
        );
        let mut line = format!("\"{index} {{t}}");
        let mut printed = Vec::new();
        for _ in &observers {
            line.push_str(" {}");
        }
        line.push('"');
        printed.extend(observers);
        let _ = writeln!(out, "            println!({line}{});", {
            let joined = printed.join(", ");
            if joined.is_empty() {
                String::new()
            } else {
                format!(", {joined}")
            }
        });
        let _ = writeln!(out, "            st = next;");
        let _ = writeln!(out, "        }}");
        let _ = writeln!(out, "    }}");
    }
    let _ = writeln!(out, "}}");
    out
}

/// The interpreter's observations, in the same per-line format the driver
/// prints.
fn interpret(cases: &[Case]) -> Vec<String> {
    let mut lines = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let mut monitor = Monitor::new(&case.spec).expect("generated specs validate");
        for (step, sample) in case.samples.iter().enumerate() {
            let observed = monitor
                .step(&mut sample.clone())
                .expect("a generated spec cannot fail a step");
            let mut line = format!("{index} {step}");
            for (_, value) in &observed.observers {
                line.push(' ');
                line.push_str(&render(value));
            }
            lines.push(line);
        }
    }
    lines
}

fn compile_and_run(dir: &Path, program: &str) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let source = dir.join("reference.rs");
    let binary = dir.join("reference");
    std::fs::write(&source, program).map_err(|e| e.to_string())?;

    let compile = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .args([
            "--edition",
            "2021",
            "-O",
            source.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("could not run rustc: {e}"))?;
    if !compile.status.success() {
        return Err(format!(
            "the reference did not compile:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        ));
    }

    let run = Command::new(&binary)
        .output()
        .map_err(|e| format!("could not run the reference: {e}"))?;
    Ok(String::from_utf8_lossy(&run.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

#[test]
fn the_reference_agrees_with_the_interpreter() {
    let cases: Vec<Case> = (0..24)
        .map(|seed| {
            let mut rng = Rng::new(seed);
            let generated = spec(&mut rng, &Config::default());
            let samples = trace(&mut rng, &generated, 8);
            Case {
                spec: generated,
                samples,
            }
        })
        .collect();

    let expected = interpret(&cases);
    let dir = std::env::temp_dir().join(format!("copilot-reference-{}", std::process::id()));
    let actual = match compile_and_run(&dir, &build_program(&cases)) {
        Ok(lines) => lines,
        Err(report) => {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("{report}");
        }
    };
    let _ = std::fs::remove_dir_all(&dir);

    assert_eq!(
        actual.len(),
        expected.len(),
        "the reference printed {} lines, the interpreter produced {}",
        actual.len(),
        expected.len()
    );
    for (reference, interpreted) in actual.iter().zip(&expected) {
        assert_eq!(
            reference, interpreted,
            "the reference disagrees with the interpreter (leading number is the seed)"
        );
    }
}
