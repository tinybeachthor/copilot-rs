//! Builds the rate-limiter monitor at compile time and writes it into
//! `OUT_DIR`, so `src/main.rs` can `include!` the generated `Env` / `Handler`
//! / `Monitor` and drive it directly — the spec below is the only place the
//! rate-limiting logic is written down.

use copilot_lang::{Builder, args};
use copilot_rust::{Settings, generate};
use std::env;
use std::fs;
use std::path::PathBuf;

/// Bucket capacity, in requests.
const CAPACITY: f32 = 20.0;
/// Refill rate, in tokens per millisecond — 2 requests/second sustained.
const REFILL_PER_MS: f32 = 2.0 / 1000.0;
/// Tokens one request costs.
const COST: f32 = 1.0;

/// A token bucket: refill by elapsed time, then admit or reject one request.
///
/// `tokens` denotes the bucket level *before* this request, so the guard and
/// the transition can both read it — that is what [`Builder::declare`] is
/// for. Refill and consumption are folded into a single committed value:
/// `mux` picks between "consume a token" and "stay put" so the rejection
/// path costs the same as the admission path, which is what keeps a step's
/// timing independent of whether it fires.
fn spec() -> Result<copilot_lang::Spec, Box<dyn std::error::Error>> {
    let b = Builder::new();

    let elapsed_ms = b.extern_::<f32>("elapsed_ms");

    let pending = b.declare(&[CAPACITY]);
    let tokens = pending.stream();

    let refilled_raw = tokens + elapsed_ms * REFILL_PER_MS;
    let over_capacity = refilled_raw.gt_val(CAPACITY);
    let refilled = over_capacity.mux(b.lit(CAPACITY), refilled_raw);
    let allowed = refilled.ge_val(COST);
    let next = allowed.mux(refilled - COST, refilled);
    pending.define(next);

    b.observe("remaining", next);
    b.trigger("reject", !allowed, args![refilled]);

    // The bucket can never leave [0, CAPACITY] — provable by
    // `cargo test -p copilot-theorem`, not checked at build time here.
    b.property_forall("bounded", tokens.ge_val(0.0) & tokens.le_val(CAPACITY));

    Ok(b.finish()?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec = spec()?;
    let source = generate(&spec, &Settings::default())?;

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    fs::write(out_dir.join("monitor.rs"), source)?;

    println!("cargo::rerun-if-changed=build.rs");
    Ok(())
}
