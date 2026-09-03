//! Builds the rate-limiter monitor at compile time and writes it into
//! `OUT_DIR`, so `src/main.rs` can `include!` the generated `Env` / `Handler`
//! / `Monitor` and drive it directly — the spec below is the only place the
//! rate-limiting logic is written down.

use copilot_lang::copilot;
use copilot_rust::{Settings, generate};
use std::env;
use std::fs;
use std::path::PathBuf;

/// A token bucket: refill by elapsed time, then admit or reject one request.
///
/// Burst of 20 requests, refilling at 2/s sustained, one token per request.
///
/// `tokens` denotes the bucket level *before* this request. Every `copilot!`
/// stream is declared before any body is built, so `tokens` is readable both
/// in its own transition (`next`, below) and in the trigger guard — the same
/// thing [`Builder::declare`](copilot_lang::Builder::declare) gives directly.
/// Symmetrically, a stream body may use a `let` that appears further down —
/// `tokens`'s body is `next`, defined last — because every binding is built
/// before any stream body (`docs/macro.md`, "Scoping").
fn spec() -> Result<copilot_lang::Spec, Box<dyn std::error::Error>> {
    Ok(copilot! {
        extern elapsed_ms: f32;

        stream tokens: f32 = [20.0] ++ next;

        let refilled_raw = tokens + elapsed_ms * 0.002;
        let over_capacity = refilled_raw > 20.0;
        let refilled = over_capacity.mux(20.0, refilled_raw);
        let allowed = refilled >= 1.0;
        let next = allowed.mux(refilled - 1.0, refilled);

        observe remaining = next;
        trigger reject(refilled) when !allowed;

        // The bucket can never leave [0, 20] — provable by
        // `cargo test -p copilot-theorem`, not checked at build time here.
        property bounded = tokens >= 0.0 && tokens <= 20.0;
    }?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = spec()?;
    let source = generate(&spec, &Settings::default())?;

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fs::write(out_dir.join("monitor.rs"), source)?;

    println!("cargo::rerun-if-changed=build.rs");
    Ok(())
}
