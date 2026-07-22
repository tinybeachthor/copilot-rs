//! The heating system from the Copilot homepage, run over a synthetic trace.
//!
//! A raw sensor reading is converted to degrees Celsius, and the heater is
//! latched on below 18 °C and off above 21 °C — the hysteresis being what stops
//! it chattering around a single threshold. Triggers fire on the transitions.
//!
//! ```text
//! cargo run -p copilot --example heater
//! ```

use copilot::{Builder, Monitor, Samples, Spec, Value, args, cost, resources};

fn heater() -> Result<Spec, copilot::Error> {
    let b = Builder::new();

    // The sensor reports a raw count; convert it once and share the result.
    let raw = b.extern_::<f32>("temperature");
    let celsius = (raw * 0.5 - 30.0).label("celsius");

    let too_cold = celsius.lt_val(18.0);
    let too_hot = celsius.gt_val(21.0);

    // Latch: on when it gets cold, off when it gets hot, unchanged in between.
    let heating = b.stream([false], |was_on| {
        too_cold.mux(b.lit(true), too_hot.mux(b.lit(false), was_on))
    });

    b.observe("celsius", celsius);
    b.observe("heating", heating);

    // Fire only on a transition, by comparing the latch against what it is
    // about to become.
    b.trigger("heat_on", too_cold & !heating, args![celsius]);
    b.trigger("heat_off", too_hot & heating, args![celsius]);

    b.property_forall("never_both", !(too_cold & too_hot));

    b.finish()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = heater()?;

    let footprint = resources(&spec);
    let counts = cost(&spec);
    println!("specification");
    println!("  streams        {}", spec.streams.len());
    println!("  triggers       {}", spec.triggers.len());
    println!(
        "  state          {} bytes (align {})",
        footprint.state_bytes, footprint.state_align
    );
    println!(
        "  work per step  {} operations ({} without sharing)",
        counts.nodes_shared, counts.nodes_inlined
    );
    println!();

    // Raw sensor counts: warm, cooling through the low threshold, then heating
    // back up through the high one.
    let readings: [f32; 10] = [
        102.0, 98.0, 94.0, 90.0, 86.0, 92.0, 100.0, 106.0, 110.0, 104.0,
    ];

    let mut monitor = Monitor::new(&spec)?;
    println!(
        "{:>4}  {:>8}  {:>7}  triggers",
        "step", "celsius", "heating"
    );
    for (step, &reading) in readings.iter().enumerate() {
        let mut env = Samples::none().with("temperature", Value::Float(reading));
        let observed = monitor.step(&mut env)?;

        let celsius = match observed.observer("celsius") {
            Some(Value::Float(c)) => *c,
            _ => unreachable!("the spec declares `celsius` as a float observer"),
        };
        let heating = observed.observer("heating") == Some(&Value::Bool(true));
        let fired: Vec<&str> = observed.fired.iter().map(|f| f.name.as_str()).collect();

        println!(
            "{step:>4}  {celsius:>7.1}°  {:>7}  {}",
            if heating { "on" } else { "off" },
            fired.join(", ")
        );
    }

    Ok(())
}
