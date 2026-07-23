//! The macro must be usable by a crate that depends only on the facade.
//!
//! A proc macro cannot see how it was reached, so generated code names a crate
//! path. `::copilot_lang` is the default; a crate that depends on `copilot`
//! instead has no such extern name, and says so with `#![crate(..)]`.

use copilot::copilot;

#[test]
fn a_facade_only_crate_can_use_the_macro() {
    let spec = copilot! {
        #![crate(::copilot)]

        extern temperature: f32;
        let celsius = temperature * 0.5 - 30.0;
        stream heating: bool = [false] ++ (celsius < 18.0).mux(true, heating);
        observe celsius;
        trigger heat_on(celsius) when celsius < 18.0 && !heating;
    }
    .unwrap();

    spec.validate().unwrap();
    assert_eq!(spec.triggers.len(), 1);
    assert_eq!(spec.streams.len(), 1);
}
