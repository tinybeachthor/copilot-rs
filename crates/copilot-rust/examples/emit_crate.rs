//! Writes a standalone `no_std` monitor crate to a directory.
//!
//! ```text
//! cargo run -p copilot-rust --example emit_crate -- /tmp/monitor
//! cargo build --manifest-path /tmp/monitor/Cargo.toml \
//!     --target thumbv7em-none-eabihf
//! ```
//!
//! Used by the `embedded` CI job to check that generated code really does
//! cross-compile to a bare-metal target. The `no_std` test in this crate
//! compiles for the host, which proves the code needs nothing from `std` but
//! not that it builds for a machine that has no `std` to begin with.

use copilot_lang::{Builder, args};
use copilot_rust::{Settings, generate_crate};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: emit_crate <directory>")?;
    let directory = std::path::PathBuf::from(directory);

    // A specification exercising what an embedded target is most likely to
    // stumble on: floats, a maths-library call, a rotating buffer, and an
    // aggregate.
    let b = Builder::new();
    let raw = b.extern_::<f32>("temperature");
    let celsius = raw * 0.5 - 30.0;
    let heating = b.stream([false], |was| {
        celsius
            .lt_val(18.0)
            .mux(b.lit(true), celsius.gt_val(21.0).mux(b.lit(false), was))
    });
    let fib = b.stream([1u32, 1], |s| s.drop(1) + s);
    let history = b.stream([[0u32; 4]], |h| h.update(fib % 4u32, fib));

    b.observe("celsius", celsius);
    b.observe("heating", heating);
    b.observe("root", celsius.sqrt());
    b.observe("history", history);
    b.trigger("alarm", celsius.lt_val(-40.0), args![celsius, fib]);
    let spec = b.finish()?;

    std::fs::create_dir_all(directory.join("src"))?;
    std::fs::write(
        directory.join("Cargo.toml"),
        "[package]\n\
         name = \"monitor\"\n\
         version = \"0.0.0\"\n\
         edition = \"2021\"\n\
         \n\
         [dependencies]\n\
         libm = \"0.2\"\n\
         \n\
         [workspace]\n",
    )?;
    std::fs::write(
        directory.join("src/lib.rs"),
        generate_crate(&spec, &Settings::default())?,
    )?;

    println!("wrote a no_std monitor crate to {}", directory.display());
    Ok(())
}
