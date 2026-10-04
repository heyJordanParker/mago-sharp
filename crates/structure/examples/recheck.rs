//! Measures a full structure check and the re-check after one file changes, over a real project.
//!
//! ```sh
//! cargo run --release -p mago-structure --example recheck -- <workspace> <source path> <file> [--verify]
//! ```
//!
//! `<file>` is changed three ways in turn: a comment line that moves its facts down one line, an
//! import that already has its namespace edge, and an import of a new namespace, which changes the
//! namespace import graph. With `--verify`, each re-check is compared with a full check by a fresh
//! `sharp-rules`.

#![allow(clippy::float_arithmetic)]
#![allow(clippy::print_stdout)]
#![allow(clippy::unwrap_used)]

use std::borrow::Cow;
use std::path::Path;
use std::time::Duration;
use std::time::Instant;

use foldhash::HashMap;
use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_structure::Rules;
use mago_structure::Runner;
use mago_structure::Structure;
use mago_structure::Violation;
use mago_syntax::parser::parse_file;
use walkdir::WalkDir;

fn collect(name: &str, contents: String) -> Vec<u8> {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Owned(name.as_bytes().to_vec()), Cow::Owned(contents.into_bytes()));
    let program = parse_file(&arena, &file);
    let resolved_names = NameResolver::new(&arena).resolve(program);

    Structure::collect(&file, program, &resolved_names).encode().unwrap()
}

fn milliseconds(duration: Duration) -> String {
    format!("{:.2} ms", duration.as_secs_f64() * 1000.0)
}

fn peak_megabytes(who: libc::c_int) -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to writable memory the size of `rusage`.
    unsafe { libc::getrusage(who, usage.as_mut_ptr()) };

    // SAFETY: `rusage` is plain integers, so the zeroed or filled memory is a valid value.
    unsafe { usage.assume_init() }.ru_maxrss as f64 / 1024.0 / 1024.0
}

fn check(runner: &mut Runner, files: &HashMap<String, Vec<u8>>, changed: &[&str]) -> Vec<Violation> {
    runner.check(files, changed, &[]).unwrap().cloned().collect()
}

fn full_check(rules: &Rules, files: &HashMap<String, Vec<u8>>) -> Vec<Violation> {
    let every: Vec<&str> = files.keys().map(String::as_str).collect();

    check(&mut Runner::start(rules).unwrap(), files, &every)
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let [_, workspace, source, edited, ..] = arguments.as_slice() else {
        panic!("usage: recheck <workspace> <source path> <file> [--verify]");
    };
    let workspace = Path::new(workspace);
    let verify = arguments.iter().any(|argument| argument == "--verify");

    let started = Instant::now();
    let mut files: HashMap<String, Vec<u8>> = HashMap::default();
    for entry in WalkDir::new(workspace.join(source)).sort_by_file_name() {
        let path = entry.unwrap().into_path();
        if path.extension().is_some_and(|extension| extension == "php") {
            let name = path.strip_prefix(workspace).unwrap().to_string_lossy().into_owned();
            files.insert(name.clone(), collect(&name, std::fs::read_to_string(&path).unwrap()));
        }
    }
    let bytes: usize = files.values().map(Vec::len).sum();
    println!("collect     {} files, {bytes} bytes of facts, {}", files.len(), milliseconds(started.elapsed()));

    let started = Instant::now();
    let (rules, issues) = Rules::compile(workspace, std::slice::from_ref(source)).unwrap();
    assert!(issues.is_empty(), "{issues:?}");
    println!("compile     {} rules modules, {}", rules.modules().count(), milliseconds(started.elapsed()));

    let started = Instant::now();
    let mut runner = Runner::start(&rules).unwrap();
    let every: Vec<&str> = files.keys().map(String::as_str).collect();
    let violations = runner.check(&files, &every, &[]).unwrap().count();
    println!("full check  {violations} violations, {} (includes sharp-rules start)", milliseconds(started.elapsed()));

    let started = Instant::now();
    let violations = runner.check(&files, &every, &[]).unwrap().count();
    println!("full again  {violations} violations, {}", milliseconds(started.elapsed()));

    let original = std::fs::read_to_string(workspace.join(edited)).unwrap();
    let edits = [
        ("comment line", original.replacen("<?php\n", "<?php\n// edited\n", 1)),
        (
            "known edge",
            original.replacen(";\n", ";\n\nuse Illuminate\\Database\\Eloquent\\Model as StructureProbe;\n", 1),
        ),
        ("new edge", original.replacen(";\n", ";\n\nuse Vendor\\StructureProbe\\Thing;\n", 1)),
    ];
    for (label, contents) in edits {
        let started = Instant::now();
        files.insert(edited.clone(), collect(edited, contents));
        let collected = started.elapsed();
        let count = runner.check(&files, &[edited.as_str()], &[]).unwrap().count();
        println!(
            "re-check    {label:<12} {count} violations, {} ({} collect, {} sharp-rules)",
            milliseconds(started.elapsed()),
            milliseconds(collected),
            milliseconds(started.elapsed() - collected)
        );

        if verify {
            let violations = check(&mut runner, &files, &[]);
            let full = full_check(&rules, &files);
            assert_eq!(violations, full, "the re-check after `{label}` differs from a full check");
            println!("verified    {label:<12} equals a full check by a fresh sharp-rules ({} violations)", full.len());
        }
    }

    drop(runner);
    println!(
        "peak memory mago {:.1} MB, sharp-rules {:.1} MB",
        peak_megabytes(libc::RUSAGE_SELF),
        peak_megabytes(libc::RUSAGE_CHILDREN)
    );
}
