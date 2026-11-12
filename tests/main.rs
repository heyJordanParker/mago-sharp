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

use std::fs;
use std::path::Path;

/// A package that sets `autotests = false` runs only the test files its `tests/main.rs` declares, so a file it forgets
/// compiles and runs nothing. Each file Cargo would have taken as a test, a `tests/*.rs` or a `tests/*/main.rs`, is a
/// module of `tests/main.rs`, or a `[[test]]` of its own in the package's `Cargo.toml`, as the suite is.
#[test]
fn every_integration_test_file_is_a_module_of_its_crates_main() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = fs::read_dir(workspace.join("crates")).unwrap().map(|entry| entry.unwrap().path());
    let mut forgotten = Vec::new();

    for package in std::iter::once(workspace.to_path_buf()).chain(crates).chain([workspace.join("docs")]) {
        let Ok(manifest) = fs::read_to_string(package.join("Cargo.toml")) else {
            continue;
        };
        if !manifest.contains("autotests = false") {
            continue;
        }

        let main = fs::read_to_string(package.join("tests/main.rs")).unwrap();
        for entry in fs::read_dir(package.join("tests")).unwrap() {
            let path = entry.unwrap().path();
            let file = path.file_name().unwrap().to_str().unwrap();
            let (name, source) = if path.is_dir() {
                if !path.join("main.rs").is_file() {
                    continue;
                }
                (file, format!("{file}/main.rs"))
            } else if let Some(name) = file.strip_suffix(".rs") {
                (name, file.to_owned())
            } else {
                continue;
            };
            if source == "main.rs" {
                continue;
            }

            let declared = main.lines().map(str::trim).any(|line| {
                line == format!("mod {name};")
                    || line == format!("pub mod {name};")
                    || line == format!("#[path = \"{source}\"]")
            });
            let own_target = manifest.contains(&format!("[[test]]\nname = \"{name}\""));
            if !declared && !own_target {
                forgotten.push(path.strip_prefix(workspace).unwrap().display().to_string());
            }
        }
    }

    assert!(
        forgotten.is_empty(),
        "No test runs these files. Declare each as a module of its tests/main.rs: {forgotten:?}"
    );
}
