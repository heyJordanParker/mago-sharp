//! A project that mixes PHP# and PHP files, with a PHP# package in `vendor/`, and `mago compile` on it, as the tests of
//! `tests/compile.rs` and `tests/laws.rs` both run it.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_database::process;

pub const ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return extra + 1;\n    }\n}\n";
pub const MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return 100;\n    }\n}\n";
pub const LEGACY: &str = "<?php\n\nnamespace App;\n\nfinal class Legacy {}\n";
pub const MONEY_PATH: &str = "vendor/acme/money/src/Money.sharp";
pub const LIBRARY: &str = "{\"type\": \"library\"}\n";

/// A library with `app/Order.sharp`, `app/Legacy.php` and the package file `vendor/acme/money/src/Money.sharp`. It is
/// a library so that spec section 5's strict rules, which run on a project, stay out of the tests about something else.
pub fn project(order: &str, money: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(root, "composer.json", LIBRARY);
    write(root, "composer.lock", "{}\n");
    write(root, "app/Order.sharp", order);
    write(root, "app/Legacy.php", LEGACY);
    write(root, MONEY_PATH, money);
    directory
}

pub fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// `mago compile` in `root`, which prints its issues in the medium format wherever it runs: mago would pick another
/// one under GitHub Actions or GitLab CI.
pub fn command(root: &Path, colors: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", colors, "compile"])
        .env("MAGO_LOG", "info")
        .env("MAGO_REPORTING_FORMAT", "medium")
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root);
    command
}

pub fn compile(root: &Path) -> Output {
    process::output(&mut command(root, "never")).unwrap()
}

/// Compiles the standard library's package at `package`, its `library/` folder, as PHP# compiles the library at its
/// release, so a project that installs it there reads its compiled files and codex.
pub fn release_library(package: &Path) {
    write(package, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"library\"]\n");
    compile_succeeds(package);
}

/// Compiles `root`, and fails the test with the printed output unless the compile succeeds.
pub fn compile_succeeds(root: &Path) {
    let output = compile(root);
    assert!(output.status.success(), "{}", printed(&output));
}

pub fn printed(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Every file under `folder`, relative to it, in path order.
pub fn files_under(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(next) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                folders.push(path);
            } else {
                files.push(path.strip_prefix(folder).unwrap().to_path_buf());
            }
        }
    }
    files.sort();
    files
}

pub fn compiled(root: &Path, name: &str) -> PathBuf {
    root.join(".sharp").join(name).with_extension("sharpc")
}

pub fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap()
}

/// Spec section 28.1's `app/Shared/Money.sharp`, whose law holds over its pure `add`.
pub const LAWFUL_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n";

/// `app/Shop/Module.sharp`, the folder's module, with one law.
pub const SHOP_MODULE: &str = "namespace App.Shop;\n\nmodule\n{\n    law same(int a) => a == a;\n}\n";

/// The `.lean` files of the project outside `.sharp/`. A law is proven from its `.sharp` file alone, so the compile
/// writes none.
pub fn source_lean_files(root: &Path) -> Vec<PathBuf> {
    files_under(root)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "lean") && !path.starts_with(".sharp"))
        .collect()
}

/// A library whose `composer.json` maps `App\` to `app/`, holding `files`.
pub fn lawful_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(
        root,
        "composer.json",
        "{\n    \"type\": \"library\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"App\\\\\": \"app/\"\n        }\n    }\n}\n",
    );
    write(root, "composer.lock", "{}\n");
    for (name, contents) in files {
        write(root, name, contents);
    }
    directory
}

/// A project that installs the whole standard library, as every project does, holding `files`.
///
/// The library comes with its sources, and its compiled files and codex, which `sharp/bin/build-mago` compiles into
/// `sharp/composer/.sharp` as a release does.
pub fn library_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = lawful_project(files);
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../composer");
    write(
        directory.path(),
        "vendor/heyjordanparker/php-sharp-composer/composer.json",
        "{\"name\": \"heyjordanparker/php-sharp-composer\", \"autoload\": {\"psr-4\": {\"Sharp\\\\\": \"library/Sharp/\"}}}\n",
    );
    for file in files_under(&package.join("library")) {
        let source = std::fs::read_to_string(package.join("library").join(&file)).unwrap();
        write(
            directory.path(),
            &format!("vendor/heyjordanparker/php-sharp-composer/library/{}", file.display()),
            &source,
        );
    }
    let compiled = package.join(".sharp");
    for file in files_under(&compiled) {
        if file.extension().is_some_and(|extension| extension == "sharpc") || file.ends_with("library.codex") {
            let target = directory.path().join("vendor/heyjordanparker/php-sharp-composer/.sharp").join(&file);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(compiled.join(&file), target).unwrap();
        }
    }

    directory
}

/// `Money` with a second class `Wallet` of the same shape, each with its law in its own file.
pub fn money_and_wallet() -> tempfile::TempDir {
    lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        ("app/Shared/Wallet.sharp", &LAWFUL_MONEY.replace("Money", "Wallet")),
    ])
}

/// `mago compile` with each issue on one line, as `file:line:column:level - code: message`.
pub fn compile_lines(root: &Path) -> Output {
    process::output(command(root, "never").env("MAGO_REPORTING_FORMAT", "emacs")).unwrap()
}

/// Each issue line `mago compile` printed, in its order.
pub fn issue_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(":error - "))
        .map(str::to_owned)
        .collect()
}

/// `mago analyze` of `paths` in `root`, of the whole project when it names none, with every issue on one line.
pub fn analyze(root: &Path, paths: &[&str]) -> Output {
    process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .args(paths)
            .env("MAGO_REPORTING_FORMAT", "medium")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap()
}

/// A cache folder whose `mago` is a file, so Mago cannot install Lean into it.
pub fn unwritable_cache() -> PathBuf {
    let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("unwritable-cache");
    write(&cache, "mago", "");
    cache
}

/// `mago compile` with no elan folder, no other program on the `PATH`, and a cache folder Mago cannot install Lean into.
pub fn compile_without_lean(root: &Path) -> Output {
    let empty = tempfile::tempdir().unwrap();
    process::output(
        command(root, "never")
            .env("MAGO_REPORTING_FORMAT", "emacs")
            .env("PATH", empty.path())
            .env("ELAN_HOME", empty.path())
            .env("XDG_CACHE_HOME", unwritable_cache()),
    )
    .unwrap()
}

/// A cache folder in `folder` whose law checker build is the tests' own, with a runner that runs the shell commands
/// `script` in its place, which `$real` names the tests' runner in. Every other file of the build is a link to the
/// tests' build, so a compile with this cache builds nothing.
#[cfg(unix)]
pub fn cache_with_runner(folder: &Path, script: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    compile_succeeds(lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]).path());
    let builds = Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache/mago/lean");
    let runner = Path::new(".lake/build/bin/sharp-lean");
    let built = std::fs::read_dir(&builds)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|build| build.join(runner).is_file())
        .unwrap();
    let cache = folder.join("cache");
    let library = cache.join("mago/lean").join(built.file_name().unwrap());
    let mut parts = runner.ancestors().skip(1).collect::<Vec<_>>();
    parts.reverse();
    for part in parts {
        std::fs::create_dir_all(library.join(part)).unwrap();
        for entry in std::fs::read_dir(built.join(part)).unwrap() {
            let name = Path::new(part).join(entry.unwrap().file_name());
            if !runner.starts_with(&name) {
                std::os::unix::fs::symlink(built.join(&name), library.join(&name)).unwrap();
            }
        }
    }
    let real = built.join(runner);
    std::fs::write(library.join(runner), format!("#!/bin/sh\nreal='{}'\n{script}\n", real.display())).unwrap();
    std::fs::set_permissions(library.join(runner), std::fs::Permissions::from_mode(0o755)).unwrap();

    cache
}

/// A cache folder in `folder` whose runner runs the tests' runner, and leaves a file in `started` in `folder` for each
/// runner it starts, which holds the runner's request.
#[cfg(unix)]
pub fn counting_cache(folder: &Path) -> PathBuf {
    let started = folder.join("started");
    std::fs::create_dir_all(&started).unwrap();

    cache_with_runner(folder, &format!("tee '{}/'$$ | \"$real\" \"$@\"", started.display()))
}
