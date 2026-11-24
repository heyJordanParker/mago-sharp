//! Every integration test of the analyzer, in one test binary: each file of `tests/` is a module.

use std::sync::LazyLock;

use mago_prelude::Prelude;

/// The prelude a user's `mago` decodes, which build.rs writes, so every test analyzes against it.
#[allow(clippy::expect_used, clippy::large_include_file)]
static PRELUDE: LazyLock<Prelude> = LazyLock::new(|| {
    Prelude::decode(include_bytes!(concat!(env!("OUT_DIR"), "/prelude.bin")))
        .expect("build.rs writes a prelude that decodes")
});

#[path = "mod.rs"]
mod cases;
mod framework;
// `framework` and `sharp` both populate their prelude with it, and clippy refuses a file loaded as two modules.
#[path = "../../../tests/support/prelude_population.rs"]
mod prelude_population;
mod sharp;
