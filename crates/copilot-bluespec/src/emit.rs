//! Assembling a monitor's packages from a specification.

use crate::expr::{self, Lowering};
use crate::render;
use crate::{Error, Package, Result, Settings};
use copilot_core::{Spec, StructType, Type};
use std::collections::BTreeMap;
use std::fmt::Write;

/// Every package a monitor needs, in an order that compiles left to right.
pub fn packages(spec: &Spec, settings: &Settings) -> Result<Vec<Package>> {
    copilot_core::validate(spec)?;
    reject_floats(spec)?;
    check_names(spec, settings)?;

    let structs = collect_structs(spec)?;
    let mut packages = Vec::new();
    if !structs.is_empty() {
        packages.push(types_package(settings, &structs));
    }
    packages.push(interface_package(spec, settings, &structs));
    packages.push(monitor_package(spec, settings, &structs));
    Ok(packages)
}

/// Whether the specification mentions an array anywhere, and so needs `Vector`.
fn uses_vector(spec: &Spec) -> bool {
    let is_array = |ty: &Type| matches!(ty, Type::Array { .. });
    spec.streams
        .iter()
        .any(|s| s.buffer.len() > 1 || is_array(&s.ty))
        || spec.arena.externs().iter().any(|(_, ty)| is_array(ty))
        || (0..spec.arena.len())
            .any(|id| is_array(spec.arena.ty_of(copilot_core::ExprId(id as u32))))
}

// ---------------------------------------------------------------------------
// The struct package.
// ---------------------------------------------------------------------------

fn types_package(settings: &Settings, structs: &Structs) -> Package {
    let name = settings.types_package();
    let mut out = banner();
    let _ = writeln!(out, "package {name} where");
    let _ = writeln!(out);
    if structs.values().any(|s| {
        s.fields
            .iter()
            .any(|(_, t)| matches!(t, Type::Array { .. }))
    }) {
        let _ = writeln!(out, "import Vector");
        let _ = writeln!(out);
    }

    for definition in structs.values() {
        let _ = writeln!(out, "struct {} =", definition.name);
        for (field, ty) in &definition.fields {
            let _ = writeln!(out, "    {field} :: {}", render::ty(ty));
        }
        // `Bits` is what lets a value of this type live in a register, so it is
        // not optional; `Eq` and `FShow` cost nothing and make a struct usable
        // in a testbench.
        let _ = writeln!(out, "  deriving (Eq, Bits, FShow)");
        let _ = writeln!(out);
    }

    Package { name, source: out }
}

// ---------------------------------------------------------------------------
// The interface package.
// ---------------------------------------------------------------------------

fn interface_package(spec: &Spec, settings: &Settings, structs: &Structs) -> Package {
    let name = settings.interface_package();
    let mut out = banner();
    let _ = writeln!(out, "package {name} where");
    let _ = writeln!(out);
    if uses_vector(spec) {
        let _ = writeln!(out, "import Vector");
    }
    if !structs.is_empty() {
        let _ = writeln!(out, "import {}", settings.types_package());
    }
    if uses_vector(spec) || !structs.is_empty() {
        let _ = writeln!(out);
    }

    let _ = writeln!(
        out,
        "-- | Everything the monitor reads and everything it reports."
    );
    let _ = writeln!(out, "--");
    let _ = writeln!(
        out,
        "-- Each external variable is read exactly once per step, before anything is"
    );
    let _ = writeln!(
        out,
        "-- evaluated, so two reads of one variable within a step cannot disagree."
    );
    let _ = writeln!(out, "interface {} =", settings.interface());

    if spec.arena.externs().is_empty() && spec.observers.is_empty() && spec.triggers.is_empty() {
        // A specification that reads nothing and reports nothing still has to
        // name an interface type, and Bluespec has no empty layout block.
        let _ = writeln!(out, "    {{ }}");
        return Package { name, source: out };
    }

    for (variable, ty) in spec.arena.externs() {
        let _ = writeln!(out, "    {variable} :: ActionValue {}", render::ty_atom(ty));
    }
    for observer in &spec.observers {
        let _ = writeln!(
            out,
            "    observe_{} :: {} -> Action",
            observer.name,
            render::ty(&observer.ty)
        );
    }
    for trigger in &spec.triggers {
        let mut signature: Vec<String> = trigger.args.iter().map(|a| render::ty(&a.ty)).collect();
        signature.push("Action".into());
        let _ = writeln!(out, "    {} :: {}", trigger.name, signature.join(" -> "));
    }

    Package { name, source: out }
}

// ---------------------------------------------------------------------------
// The monitor package.
// ---------------------------------------------------------------------------

fn monitor_package(spec: &Spec, settings: &Settings, structs: &Structs) -> Package {
    let name = settings.name.clone();
    let counts = copilot_core::cost(spec);
    let state = register_bits(spec);

    let mut out = banner();
    let _ = writeln!(out, "--");
    let _ = writeln!(
        out,
        "-- State: {} bits across {} register{}. Work per step: {} operations, all",
        state.bits,
        state.registers,
        if state.registers == 1 { "" } else { "s" },
        counts.nodes_shared
    );
    let _ = writeln!(
        out,
        "-- combinational, so a step is one cycle whatever the data is."
    );
    let _ = writeln!(out, "--");
    // The Rust backend's footprint is checked against `size_of::<Monitor>()`.
    // There is no equivalent here: `bsc` reports area in a form nothing parses,
    // so the figure above is a statement about the specification, not about what
    // was synthesised from it. Saying so in the file beats reprinting a number
    // that reads as if it had been verified.
    let _ = writeln!(
        out,
        "-- The bit count is what this specification's buffers hold. It is not the area"
    );
    let _ = writeln!(
        out,
        "-- bsc synthesises, and nothing in copilot-rs checks it against one."
    );
    let _ = writeln!(out, "package {name} where");
    let _ = writeln!(out);
    if uses_vector(spec) {
        let _ = writeln!(out, "import Vector");
        let _ = writeln!(out);
    }
    if !structs.is_empty() {
        let _ = writeln!(out, "import {}", settings.types_package());
    }
    let _ = writeln!(out, "import {}", settings.interface_package());
    let _ = writeln!(out);

    write_module(&mut out, spec, settings);
    Package { name, source: out }
}

fn write_module(out: &mut String, spec: &Spec, settings: &Settings) {
    let module = settings.module();
    let interface = settings.interface();

    let _ = writeln!(out, "-- | One step of the monitor per clock cycle.");
    let _ = writeln!(out, "--");
    let _ = writeln!(
        out,
        "-- The rule below is the whole monitor: it fires unconditionally, so a step"
    );
    let _ = writeln!(
        out,
        "-- takes one cycle whatever the data is. Registers change at the clock edge"
    );
    let _ = writeln!(
        out,
        "-- and the rule reads them as they were before it, which is what keeps the"
    );
    let _ = writeln!(
        out,
        "-- compute and commit phases apart without any sequencing."
    );
    let _ = writeln!(out, "{module} :: (IsModule m c) => {interface} -> m Empty");
    let _ = writeln!(out, "{module} ifc =");
    let _ = writeln!(out, "  module");

    write_state(out, spec);
    write_step(out, spec, settings);
}

fn write_state(out: &mut String, spec: &Spec) {
    for stream in &spec.streams {
        let _ = writeln!(
            out,
            "    -- Stream {}: {} value{} of {}.",
            stream.id.index(),
            stream.buffer.len(),
            if stream.buffer.len() == 1 { "" } else { "s" },
            render::ty(&stream.ty)
        );
        for (position, initial) in stream.buffer.iter().enumerate() {
            let _ = writeln!(
                out,
                "    {} :: Reg {} <- mkReg {}",
                expr::slot(stream.id, position),
                render::ty_atom(&stream.ty),
                render::value_atom(initial)
            );
        }
        if stream.needs_index() {
            let _ = writeln!(
                out,
                "    {} :: Reg (UInt 32) <- mkReg 0",
                expr::index(stream.id)
            );
        }
        let _ = writeln!(out);
    }

    // Buffers deeper than one element are read at a slot chosen at run time,
    // which needs the registers gathered into something selectable.
    let deep: Vec<_> = spec.streams.iter().filter(|s| s.needs_index()).collect();
    if !deep.is_empty() {
        for stream in deep {
            let slots: Vec<String> = (0..stream.buffer.len())
                .map(|position| expr::slot(stream.id, position))
                .collect();
            let mut chain = String::from("nil");
            for slot in slots.iter().rev() {
                chain = if chain == "nil" {
                    format!("cons {slot} nil")
                } else {
                    format!("cons {slot} ({chain})")
                };
            }
            let _ = writeln!(
                out,
                "    let {} :: Vector {} (Reg {})",
                expr::vector(stream.id),
                stream.buffer.len(),
                render::ty_atom(&stream.ty)
            );
            let _ = writeln!(out, "        {} = {chain}", expr::vector(stream.id));
        }
        let _ = writeln!(out);
    }
}

fn write_step(out: &mut String, spec: &Spec, settings: &Settings) {
    let lowering = Lowering::new(spec, settings.index_policy);
    let reached = copilot_core::reachable(spec, &spec.runtime_roots());

    let _ = writeln!(out, "    rules");
    let _ = writeln!(out, "      \"step\": when True ==> do");

    // Phase 1. Every declared variable is read, including one no expression
    // uses: a read is an `ActionValue`, so it may well have an effect on the
    // user's side, and "read exactly once per step" has to hold for all of them
    // or it is not a contract.
    if !spec.arena.externs().is_empty() {
        let _ = writeln!(
            out,
            "        -- Phase 1: read each external variable exactly once."
        );
        for (variable, _) in spec.arena.externs() {
            let _ = writeln!(out, "        {} <- ifc.{variable}", expr::sample(variable));
        }
        let _ = writeln!(out);
    }

    // Phases 2 and 3 share one block of bindings, because both read the state
    // as it stood at the clock edge.
    let _ = writeln!(
        out,
        "        -- Phases 2 and 3: name every subexpression once. These are wires, not"
    );
    let _ = writeln!(
        out,
        "        -- state, so naming a shared one is what makes it shared hardware."
    );
    for &id in &reached {
        let ty = spec.arena.ty_of(id);
        let name = lowering.declaration(id);
        let _ = writeln!(out, "        let {name} :: {}", render::ty(ty));
        let _ = writeln!(out, "            {name} = {}", lowering.node(id));
    }
    let _ = writeln!(out);

    // Phase 2 output.
    if !spec.observers.is_empty() || !spec.triggers.is_empty() {
        let _ = writeln!(
            out,
            "        -- Phase 2: report observers, then fire triggers."
        );
        for observer in &spec.observers {
            let _ = writeln!(
                out,
                "        ifc.observe_{} {}",
                observer.name,
                expr::binding(observer.expr)
            );
        }
        for trigger in &spec.triggers {
            let args: Vec<String> = trigger
                .args
                .iter()
                .map(|arg| expr::binding(arg.expr))
                .collect();
            let call = format!(
                "ifc.{}{}{}",
                trigger.name,
                if args.is_empty() { "" } else { " " },
                args.join(" ")
            );
            let _ = writeln!(
                out,
                "        if {} then {call} else noAction",
                expr::binding(trigger.guard)
            );
        }
        let _ = writeln!(out);
    }

    // Phase 4.
    let _ = writeln!(
        out,
        "        -- Phase 4: commit. The new value overwrites the slot holding the value"
    );
    let _ = writeln!(out, "        -- that has just expired.");
    for stream in &spec.streams {
        let next = expr::binding(stream.expr);
        if stream.needs_index() {
            let vector = expr::vector(stream.id);
            let index = expr::index(stream.id);
            let len = stream.buffer.len();
            let _ = writeln!(
                out,
                "        writeVReg {vector} (update (readVReg {vector}) {index} {next})"
            );
            let _ = writeln!(
                out,
                "        {index} := if {index} + 1 >= {len} then 0 else {index} + 1"
            );
        } else {
            let _ = writeln!(out, "        {} := {next}", expr::slot(stream.id, 0));
        }
    }
}

fn banner() -> String {
    String::from("-- Generated by copilot-rs. Do not edit.\n")
}

/// What a monitor's state costs in registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    /// Total bits held.
    pub bits: usize,
    /// How many registers hold them.
    pub registers: usize,
}

/// Counts the registers a monitor declares, and the bits they hold.
///
/// Deliberately not [`copilot_core::resources`], which lays state out under
/// `repr(C)` — with padding, and with a `bool` costing a byte. Bluespec's
/// derived `Bits` instances pack, so a `Bool` is one bit and a struct is the sum
/// of its fields, and reporting C's answer for a target that does not use C's
/// layout would be reporting the wrong number precisely.
pub fn register_bits(spec: &Spec) -> State {
    let mut state = State {
        bits: 0,
        registers: 0,
    };
    for stream in &spec.streams {
        state.bits += stream.buffer.len() * packed_bits(&stream.ty);
        state.registers += stream.buffer.len();
        if stream.needs_index() {
            state.bits += copilot_core::INDEX_BYTES * 8;
            state.registers += 1;
        }
    }
    state
}

/// The width of a type under Bluespec's derived `Bits` instance.
fn packed_bits(ty: &Type) -> usize {
    match ty {
        Type::Bool => 1,
        Type::Array { elem, len } => len * packed_bits(elem),
        Type::Struct(definition) => definition.fields.iter().map(|(_, t)| packed_bits(t)).sum(),
        other => render::bit_width(other) as usize,
    }
}

// ---------------------------------------------------------------------------
// Checks.
// ---------------------------------------------------------------------------

type Structs = BTreeMap<String, StructType>;

/// Every struct type the specification mentions, keyed by name.
fn collect_structs(spec: &Spec) -> Result<Structs> {
    let mut found = Structs::new();

    let visit = |ty: &Type, found: &mut Structs| -> Result<()> {
        let mut stack = vec![ty.clone()];
        while let Some(ty) = stack.pop() {
            match ty {
                Type::Array { elem, .. } => stack.push(*elem),
                Type::Struct(definition) => {
                    stack.extend(definition.fields.iter().map(|(_, t)| t.clone()));
                    // Two different structs sharing a name would emit two
                    // conflicting definitions in one package.
                    if let Some(existing) = found.get(&definition.name)
                        && *existing != *definition
                    {
                        return Err(Error::ConflictingStruct(definition.name.clone()));
                    }
                    found.insert(definition.name.clone(), *definition);
                }
                _ => {}
            }
        }
        Ok(())
    };

    for id in 0..spec.arena.len() {
        visit(
            spec.arena.ty_of(copilot_core::ExprId(id as u32)),
            &mut found,
        )?;
    }
    for (_, ty) in spec.arena.externs() {
        visit(ty, &mut found)?;
    }
    for stream in &spec.streams {
        visit(&stream.ty, &mut found)?;
    }

    Ok(found)
}

/// Rejects a specification that mentions a floating-point type anywhere.
///
/// See `Error::UnsupportedType` for why this backend cannot carry them.
fn reject_floats(spec: &Spec) -> Result<()> {
    fn check(ty: &Type) -> Result<()> {
        match ty {
            Type::Float | Type::Double => Err(Error::UnsupportedType(ty.clone())),
            Type::Array { elem, .. } => check(elem),
            Type::Struct(definition) => definition.fields.iter().try_for_each(|(_, t)| check(t)),
            _ => Ok(()),
        }
    }

    for id in 0..spec.arena.len() {
        check(spec.arena.ty_of(copilot_core::ExprId(id as u32)))?;
    }
    for (_, ty) in spec.arena.externs() {
        check(ty)?;
    }
    for stream in &spec.streams {
        check(&stream.ty)?;
    }
    Ok(())
}

/// Every name a specification contributes to generated Bluespec.
fn check_names(spec: &Spec, settings: &Settings) -> Result<()> {
    upper_identifier(&settings.name, "package")?;

    for (variable, _) in spec.arena.externs() {
        lower_identifier(variable, "external variable")?;
    }
    for observer in &spec.observers {
        lower_identifier(&observer.name, "observer")?;
    }
    for trigger in &spec.triggers {
        lower_identifier(&trigger.name, "trigger")?;
        for observer in &spec.observers {
            if trigger.name == format!("observe_{}", observer.name) {
                return Err(Error::NameCollision {
                    trigger: trigger.name.clone(),
                    observer: observer.name.clone(),
                });
            }
        }
    }
    for definition in collect_structs(spec)?.values() {
        upper_identifier(&definition.name, "struct")?;
        for (field, _) in &definition.fields {
            lower_identifier(field, "struct field")?;
        }
    }
    Ok(())
}

/// Bluespec Classic's keywords.
///
/// A specification is free to call a trigger `when`; Bluespec is not, and the
/// error it produces two files later says nothing about where the name came
/// from. Refusing it here does.
const KEYWORDS: &[&str] = &[
    "action",
    "actionvalue",
    "case",
    "class",
    "clock",
    "data",
    "default",
    "deriving",
    "do",
    "else",
    "enum",
    "export",
    "foreign",
    "if",
    "import",
    "in",
    "infix",
    "infixl",
    "infixr",
    "instance",
    "interface",
    "let",
    "letseq",
    "match",
    "module",
    "package",
    "primitive",
    "qualified",
    "reset",
    "return",
    "rules",
    "signature",
    "struct",
    "then",
    "type",
    "verilog",
    "when",
    "where",
    "while",
];

fn lower_identifier(name: &str, kind: &'static str) -> Result<()> {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(Error::InvalidName {
            name: name.to_string(),
            kind,
            reason: "must start with a lower-case letter and hold only letters, digits, and \
                     underscores",
        });
    }
    reserved(name, kind)
}

fn upper_identifier(name: &str, kind: &'static str) -> Result<()> {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(Error::InvalidName {
            name: name.to_string(),
            kind,
            reason: "must start with an upper-case letter and hold only letters, digits, and \
                     underscores",
        });
    }
    reserved(name, kind)
}

fn reserved(name: &str, kind: &'static str) -> Result<()> {
    if KEYWORDS.contains(&name) {
        return Err(Error::InvalidName {
            name: name.to_string(),
            kind,
            reason: "is a Bluespec keyword",
        });
    }
    Ok(())
}
