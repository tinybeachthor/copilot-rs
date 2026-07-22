//! Proves the flagship claim: a generated monitor really does compile without
//! the standard library.
//!
//! Every corpus specification is emitted as a standalone crate root and handed
//! to `rustc` with `-D warnings`, so this checks two things at once — that the
//! generated code needs nothing beyond `core` and a maths library, and that it
//! compiles clean enough to drop into a user's build without noise.

mod support;

use copilot_rust::{Settings, generate_crate};
use std::path::Path;
use std::process::Command;

/// A stand-in for `libm` with the same entry points and no real arithmetic.
///
/// This test is about whether generated code *compiles* under `no_std`, so the
/// bodies are irrelevant; what matters is that the stub is itself `no_std`, so
/// nothing pulls the standard library back in through the side door. The real
/// `libm` is exercised for behaviour by `differential.rs`.
const LIBM_STUB: &str = r#"
#![no_std]
macro_rules! stub {
    ($($name:ident ( $($arg:ident),* ) -> $ty:ty;)*) => {
        $( pub fn $name($($arg: $ty),*) -> $ty { $(let _ = $arg;)* 0 as $ty } )*
    };
}
stub! {
    sqrt(x) -> f64; floor(x) -> f64; ceil(x) -> f64; sin(x) -> f64;
    cos(x) -> f64; tan(x) -> f64; asin(x) -> f64; acos(x) -> f64;
    atan(x) -> f64; sinh(x) -> f64; cosh(x) -> f64; tanh(x) -> f64;
    asinh(x) -> f64; acosh(x) -> f64; atanh(x) -> f64; exp(x) -> f64;
    log(x) -> f64; pow(x, y) -> f64; atan2(y, x) -> f64;
}
stub! {
    sqrtf(x) -> f32; floorf(x) -> f32; ceilf(x) -> f32; sinf(x) -> f32;
    cosf(x) -> f32; tanf(x) -> f32; asinf(x) -> f32; acosf(x) -> f32;
    atanf(x) -> f32; sinhf(x) -> f32; coshf(x) -> f32; tanhf(x) -> f32;
    asinhf(x) -> f32; acoshf(x) -> f32; atanhf(x) -> f32; expf(x) -> f32;
    logf(x) -> f32; powf(x, y) -> f32; atan2f(y, x) -> f32;
}
"#;

fn rustc(args: &[&str]) -> std::process::Output {
    Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
        .args(args)
        .output()
        .expect("rustc must be available; it built this test")
}

fn compile_libm(dir: &Path) -> String {
    let source = dir.join("libm.rs");
    let rlib = dir.join("liblibm.rlib");
    std::fs::write(&source, LIBM_STUB).unwrap();

    let output = rustc(&[
        "--edition",
        "2021",
        "--crate-type",
        "rlib",
        "--crate-name",
        "libm",
        source.to_str().unwrap(),
        "-o",
        rlib.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "the libm stub must compile:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    rlib.to_str().unwrap().to_string()
}

#[test]
fn every_monitor_compiles_without_the_standard_library() {
    let dir = std::env::temp_dir().join(format!("copilot-nostd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let libm = compile_libm(&dir);

    for (name, spec) in support::all() {
        let source = dir.join(format!("{name}.rs"));
        let generated = generate_crate(&spec, &Settings::default()).unwrap();
        assert!(
            generated.starts_with("#![no_std]"),
            "{name}: a standalone crate must declare no_std"
        );
        std::fs::write(&source, &generated).unwrap();

        let output = rustc(&[
            "--edition",
            "2021",
            "--crate-type",
            "rlib",
            "--crate-name",
            name,
            "--extern",
            &format!("libm={libm}"),
            "-D",
            "warnings",
            source.to_str().unwrap(),
            "-o",
            dir.join(format!("lib{name}.rlib")).to_str().unwrap(),
        ]);

        assert!(
            output.status.success(),
            "{name}: generated no_std crate must compile without warnings:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
