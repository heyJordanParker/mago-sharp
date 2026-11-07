//! Every integration test of `mago` but the suite, which has a harness of its own, in one test binary: each file of
//! `tests/` is a module.

mod common;
mod compile;
mod convert;
mod cst;
mod extension_host_processes;
mod external_analyzer_entry_points;
mod external_analyzer_invocation;
mod external_analyzer_lifecycle;
mod external_analyzer_refinement;
mod external_analyzer_scan;
mod external_linter;
mod external_worker_reduction;
mod fix;
mod lint_stats;
mod scoped_check;
mod server_engine;
mod sharp;
mod stdin_input;
