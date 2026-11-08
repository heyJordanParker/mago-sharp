//! Every integration test of the analyzer, in one test binary: each file of `tests/` is a module.

#[path = "mod.rs"]
mod cases;
mod framework;
// `framework` and `sharp` both populate their prelude with it, and clippy refuses a file loaded as two modules.
#[path = "../../../tests/support/prelude_population.rs"]
mod prelude_population;
mod sharp;
