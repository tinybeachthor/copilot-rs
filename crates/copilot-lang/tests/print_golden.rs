//! Golden tests for `Spec`'s `Display`, from `copilot-core`'s printer
//! (M8): the printed form of a handful of representative builder-built
//! specifications is checked in, so a change to the printer shows up as a
//! reviewable diff rather than as a silent change in what the text says.
//!
//! Run `UPDATE_GOLDEN=1 cargo test -p copilot-lang --test print_golden` to
//! rewrite them after an intended change.

use copilot_lang::{Builder, Prop, Spec, args};
use std::path::PathBuf;

/// External variables, a labelled shared subexpression, a latch, and
/// hysteresis triggers — the same shape as the code generators' own `heater`
/// corpus entry, printed instead of compiled.
fn heater() -> Spec {
    let b = Builder::new();
    let raw = b.extern_::<f32>("temperature");
    let celsius = (raw * 0.5 - 30.0).label("celsius");
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

/// A property alongside the stream it talks about, and both quantifier
/// forms, so the printer's property section is exercised too.
fn bounded_counter() -> Spec {
    let b = Builder::new();
    let counter = b.stream([0u8], |s| (s + 1u8) % 10u8);
    b.observe("counter", counter);
    b.property_forall("stays_below_ten", counter.lt_val(10));
    b.property_exists("reaches_nine", counter.eq_val(9));
    b.finish().unwrap()
}

/// A struct-typed extern, projected and rebuilt with one field replaced, so
/// literal and field-access rendering are exercised outside `copilot-core`'s
/// own unit tests.
fn structs() -> Spec {
    use copilot_lang::CopilotStruct;

    #[derive(Clone, Copy, Debug, PartialEq, CopilotStruct)]
    #[repr(C)]
    struct Reading {
        altitude: f32,
        valid: bool,
    }

    let b = Builder::new();
    let sensor = b.extern_::<Reading>("sensor");
    let latest = b.stream(
        [Reading {
            altitude: 0.0,
            valid: false,
        }],
        |previous| sensor.valid().mux(sensor, previous),
    );
    b.observe("altitude", latest.altitude());
    b.finish().unwrap()
}

fn corpus() -> Vec<(&'static str, Spec)> {
    vec![
        ("heater", heater()),
        ("bounded_counter", bounded_counter()),
        ("structs", structs()),
    ]
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden_print")
        .join(format!("{name}.txt"))
}

#[test]
fn printed_source_matches_the_checked_in_copy() {
    let updating = std::env::var_os("UPDATE_GOLDEN").is_some();
    let mut stale = Vec::new();

    for (name, spec) in corpus() {
        let printed = spec.to_string();
        let path = golden_path(name);

        if updating {
            std::fs::write(&path, &printed).expect("golden file must be writable");
            continue;
        }

        let checked_in = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden file {}", path.display()));
        if checked_in != printed {
            stale.push(name);
        }
    }

    assert!(
        stale.is_empty(),
        "printed source no longer matches the checked-in copy for: {}\n\
         Re-run with UPDATE_GOLDEN=1 to accept the change, then review the diff.",
        stale.join(", ")
    );
}

/// The printed form is meant to be checked against the source that built it,
/// not merely to exist — so pin a few load-bearing fragments directly,
/// independent of the golden file above.
#[test]
fn the_printed_heater_reads_like_the_source_that_built_it() {
    let text = heater().to_string();
    assert!(text.contains("extern temperature: Float;"));
    // The label survives into the printed name, so a `let` bound with
    // `.label("celsius")` is legible without cross-referencing the arena.
    assert!(text.contains("let celsius = "));
    // Read from three places (two observers... one observer and two trigger
    // guards/args), `celsius` is named once and referenced by name, not
    // re-derived at each site.
    assert!(text.contains("observe celsius = celsius;"));
    assert!(text.contains("trigger heat_on(celsius) when "));
    assert!(text.contains("trigger heat_off(celsius) when "));
    assert!(text.contains("drop 0 s0"));
}

#[test]
fn both_quantifiers_are_distinguished_in_print() {
    let text = bounded_counter().to_string();
    assert!(text.contains("property stays_below_ten = "));
    assert!(text.contains("property exists reaches_nine = "));
    assert!(!matches!(
        bounded_counter().properties[0].prop,
        Prop::Exists(_)
    ));
}
