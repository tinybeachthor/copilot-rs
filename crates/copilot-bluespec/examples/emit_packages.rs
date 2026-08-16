//! Writes a monitor's Bluespec packages, and a testbench driving them, to a
//! directory.
//!
//! ```text
//! cargo run -p copilot-bluespec --example emit_packages -- /tmp/monitor
//! cd /tmp/monitor
//! bsc -sim -u -g mkMonitorSim MonitorSim.bs
//! bsc -sim -e mkMonitorSim -o sim.out mkMonitorSim.ba
//! ./sim.out
//! ```
//!
//! Used by the `bluespec` CI job to check that generated Bluespec really does
//! compile and simulate with a toolchain nothing else in this repository has
//! touched.

use copilot_bluespec::{Settings, compile, testbench};
use copilot_core::Value;
use copilot_lang::{Builder, args};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: emit_packages <directory>")?;
    let directory = std::path::PathBuf::from(directory);

    // A specification exercising what a hardware backend is most likely to
    // stumble on: a rotating buffer, an array update at a computed index, a
    // struct in a register, and an external variable.
    let b = Builder::new();
    let raw = b.extern_::<i16>("temperature");
    let heating = b.stream([false], |was| {
        raw.lt_val(180)
            .mux(b.lit(true), raw.gt_val(210).mux(b.lit(false), was))
    });
    let fib = b.stream([1u32, 1], |s| s.drop(1) + s);
    let history = b.stream([[0u32; 4]], |h| h.update(fib % 4u32, fib));

    b.observe("heating", heating);
    b.observe("history", history);
    b.trigger("alarm", raw.lt_val(-400), args![raw, fib]);
    let spec = b.finish()?;

    let settings = Settings {
        output_directory: directory.clone(),
        ..Settings::default()
    };
    let mut written = compile(&spec, &settings)?;

    // Twelve steps of a ramp that crosses both thresholds.
    let trace: Vec<BTreeMap<String, Value>> = (0..12)
        .map(|step| BTreeMap::from([("temperature".to_string(), Value::Int16(150 + step * 12))]))
        .collect();
    let simulation = testbench(&spec, &settings, &trace)?;
    let path = directory.join(simulation.file_name());
    std::fs::write(&path, &simulation.source)?;
    written.push(path);

    for path in written {
        println!("wrote {}", path.display());
    }
    Ok(())
}
