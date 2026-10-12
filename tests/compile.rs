//! Runs `mago compile` on a project that mixes PHP# and PHP files, with a PHP# package in `vendor/`.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_lean::PACKAGE_FOLDER;

const ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return extra + 1;\n    }\n}\n";
const BROKEN_ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return \"one\";\n    }\n}\n";
const MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return 100;\n    }\n}\n";
const BROKEN_MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return \"one\";\n    }\n}\n";
const LEGACY: &str = "<?php\n\nnamespace App;\n\nfinal class Legacy {}\n";
const MONEY_PATH: &str = "vendor/acme/money/src/Money.sharp";

/// A project with `app/Order.sharp`, `app/Legacy.php` and the package file `vendor/acme/money/src/Money.sharp`.
fn project(order: &str, money: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n",
    );
    write(directory.path(), "composer.lock", "{}\n");
    write(directory.path(), "app/Order.sharp", order);
    write(directory.path(), "app/Legacy.php", LEGACY);
    write(directory.path(), MONEY_PATH, money);
    directory
}

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn command(root: &Path, colors: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", colors, "compile"])
        .env("MAGO_LOG", "info")
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root);
    command
}

fn compile(root: &Path) -> Output {
    command(root, "never").output().unwrap()
}

/// Compiles `root`, and fails the test with the printed output unless the compile succeeds.
fn compile_succeeds(root: &Path) {
    let output = compile(root);
    assert!(output.status.success(), "{}", printed(&output));
}

fn printed(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// Every file under `folder`, relative to it, in path order.
fn files_under(folder: &Path) -> Vec<PathBuf> {
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

fn compiled(root: &Path, name: &str) -> PathBuf {
    root.join(".sharp").join(name).with_extension("sharpc")
}

#[test]
fn compile_writes_each_accepted_file_into_the_sharp_folder_at_its_source_path() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let vendor = files_under(&root.join("vendor"));

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        files_under(&root.join(".sharp")),
        [Path::new("app/Order.sharpc"), Path::new("vendor/acme/money/src/Money.sharpc")]
    );
    for (name, source) in [("app/Order.sharp", ORDER), (MONEY_PATH, MONEY)] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        let header = header(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(header.source_hash, source_hash(source.as_bytes()), "{name}");
    }
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
    assert!(printed(&output).contains("Compiled 2 PHP# files into .sharp/."), "{}", printed(&output));
}

#[test]
fn compile_refuses_a_file_with_an_error_writes_the_others_and_deletes_its_old_compiled_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    write(root, "app/Order.sharp", BROKEN_ORDER);
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(!compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, MONEY_PATH).exists());
    assert!(printed(&output).contains("app/Order.sharp"), "{}", printed(&output));
    assert!(printed(&output).contains("1 PHP# file was refused"), "{}", printed(&output));
}

#[test]
fn compile_refuses_a_package_file_with_an_error_and_names_its_package() {
    let directory = project(ORDER, BROKEN_MONEY);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(!compiled(root, MONEY_PATH).exists());
    assert!(compiled(root, "app/Order.sharp").exists());
    assert!(printed(&output).contains("Composer package acme/money"), "{}", printed(&output));
}

#[test]
fn compile_deletes_the_compiled_file_of_a_renamed_or_removed_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::rename(root.join("app/Order.sharp"), root.join("app/Invoice.sharp")).unwrap();
    compile_succeeds(root);
    assert!(!compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, "app/Invoice.sharp").exists());

    std::fs::remove_file(root.join("app/Invoice.sharp")).unwrap();
    compile_succeeds(root);
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("vendor/acme/money/src/Money.sharpc")]);
}

#[test]
fn compile_prints_no_color_under_colors_never_even_when_force_color_is_set() {
    let directory = project(BROKEN_ORDER, MONEY);
    let output = command(directory.path(), "never")
        .env("FORCE_COLOR", "1")
        .env("MAGO_REPORTING_FORMAT", "rich")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("app/Order.sharp"), "{}", printed(&output));
    assert!(!printed(&output).contains("\x1b["), "{}", printed(&output));
}

#[test]
fn compile_prints_color_under_colors_always_even_when_no_color_is_set() {
    let directory = project(BROKEN_ORDER, MONEY);
    let output = command(directory.path(), "always")
        .env_remove("FORCE_COLOR")
        .env("NO_COLOR", "1")
        .env("MAGO_REPORTING_FORMAT", "rich")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(String::from_utf8_lossy(&output.stdout).contains("\x1b["), "{}", printed(&output));
}

#[test]
fn compile_writes_no_sharp_folder_in_a_project_without_php_sharp_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n");
    write(root, "app/Legacy.php", LEGACY);

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!root.join(".sharp").exists());
}

#[test]
fn compile_reports_a_sharp_folder_inside_a_package() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "vendor/acme/money/.sharp/src/Money.sharpc", "stale");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Delete vendor/acme/money/.sharp."), "{}", printed(&output));
    assert!(root.join("vendor/acme/money/.sharp/src/Money.sharpc").exists(), "nothing under vendor/ changes");
}

#[cfg(unix)]
#[test]
fn compile_reports_a_link_to_a_file_outside_the_project_and_writes_no_compiled_file_for_it() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), "Money.sharp", MONEY);
    std::fs::remove_file(root.join(MONEY_PATH)).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("Money.sharp"), root.join(MONEY_PATH)).unwrap();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("vendor/acme/money/src/Money.sharp is a link to"), "{}", printed(&output));
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("app/Order.sharpc")]);
}

#[test]
fn compile_replaces_a_compiled_file_by_rename_so_no_reader_sees_a_partial_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let before = std::fs::read(compiled(root, "app/Order.sharp")).unwrap();
    // On Windows, `File::open` shares the file for deletion, which lets a rename replace it while it is open.
    let mut open = std::fs::File::open(compiled(root, "app/Order.sharp")).unwrap();

    write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    compile_succeeds(root);

    let mut still_open = Vec::new();
    std::io::Read::read_to_end(&mut open, &mut still_open).unwrap();
    assert_eq!(still_open, before, "the old compiled file stays whole for a reader that opened it");
    assert_ne!(std::fs::read(compiled(root, "app/Order.sharp")).unwrap(), before);
    assert_eq!(files_under(&root.join(".sharp/app")), [Path::new("Order.sharpc")], "no temporary file is left");
}

/// Spec section 28.1's `app/Shared/Money.sharp`, whose law holds over its pure `add`.
const LAWFUL_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n";

/// The proof `mago compile` creates for [`LAWFUL_MONEY`].
const MONEY_PROOF: &str = "import Code.App.Shared.Money\nopen Sharp\n\ntheorem addKeepsCurrency : App.Shared.Money.addKeepsCurrency := by\n  simp [App.Shared.Money.addKeepsCurrency, App.Shared.Money.add]\n";

/// A project whose `composer.json` maps `App\` to `app/`, holding `files`.
fn lawful_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(
        root,
        "composer.json",
        "{\n    \"autoload\": {\n        \"psr-4\": {\n            \"App\\\\\": \"app/\"\n        }\n    }\n}\n",
    );
    write(root, "composer.lock", "{}\n");
    for (name, contents) in files {
        write(root, name, contents);
    }
    directory
}

/// `mago compile` with each issue on one line, as `file:line:column:level - code: message`.
fn compile_lines(root: &Path) -> Output {
    command(root, "never").env("MAGO_REPORTING_FORMAT", "emacs").output().unwrap()
}

/// A cache folder whose `mago` is a file, so Mago cannot install Lean into it.
fn unwritable_cache() -> PathBuf {
    let cache = Path::new(env!("CARGO_TARGET_TMPDIR")).join("unwritable-cache");
    write(&cache, "mago", "");
    cache
}

/// `mago compile` with no `lake`, nor any other program, on the `PATH`, and a cache folder Mago cannot install Lean
/// into.
fn compile_without_lean(root: &Path) -> Output {
    let empty = tempfile::tempdir().unwrap();
    command(root, "never")
        .env("MAGO_REPORTING_FORMAT", "emacs")
        .env("PATH", empty.path())
        .env("XDG_CACHE_HOME", unwritable_cache())
        .output()
        .unwrap()
}

fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap()
}

/// The folders in `cache`'s `mago/lean/`: the library builds and any staging folder, without `build.lock`.
fn library_builds(cache: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(cache.join("mago/lean")).unwrap();
    entries.map(|entry| entry.unwrap().path()).filter(|path| path.is_dir()).collect()
}

#[test]
fn compile_proves_a_law_with_the_proof_it_creates_and_writes_the_compiled_file() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(read(root, "app/Shared/Money.lean"), MONEY_PROOF);
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert!(root.join(".sharp").join(PACKAGE_FOLDER).is_dir(), "the compile keeps the Lake package's folder");
    assert!(root.join(".sharp/.lean/Code/App/Shared/Money.lean").exists(), "the Lake package stays in .sharp/.lean/");
}

#[test]
fn compiles_that_start_together_on_an_empty_cache_each_prove_their_law_and_leave_one_library_build() {
    let cache = tempfile::tempdir().unwrap();
    let projects: Vec<_> = (0..4).map(|_| lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)])).collect();

    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let compiles: Vec<_> = projects
            .iter()
            .map(|project| {
                scope.spawn(|| command(project.path(), "never").env("XDG_CACHE_HOME", cache.path()).output().unwrap())
            })
            .collect();
        compiles.into_iter().map(|compile| compile.join().unwrap()).collect()
    });

    for (project, output) in projects.iter().zip(&outputs) {
        assert!(output.status.success(), "{}", printed(output));
        assert_eq!(read(project.path(), "app/Shared/Money.lean"), MONEY_PROOF);
    }
    assert_eq!(library_builds(cache.path()).len(), 1, "one library build is left");
}

#[test]
fn compile_rebuilds_a_library_build_that_lost_its_lakefile_or_its_runner_and_proves_its_law() {
    for lost in ["lakefile.toml", ".lake/build/bin"] {
        let cache = tempfile::tempdir().unwrap();
        let compile_in_cache = |project: &tempfile::TempDir| {
            command(project.path(), "never").env("XDG_CACHE_HOME", cache.path()).output().unwrap()
        };
        let output = compile_in_cache(&lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]));
        assert!(output.status.success(), "{}", printed(&output));
        let lost_path = library_builds(cache.path()).remove(0).join(lost);
        if lost_path.is_dir() {
            std::fs::remove_dir_all(&lost_path).unwrap();
        } else {
            std::fs::remove_file(&lost_path).unwrap();
        }

        let project = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
        let output = compile_in_cache(&project);

        assert!(output.status.success(), "{lost}: {}", printed(&output));
        assert_eq!(read(project.path(), "app/Shared/Money.lean"), MONEY_PROOF);
        assert!(lost_path.exists(), "the library build has {lost} again");
        assert_eq!(library_builds(cache.path()).len(), 1, "one library build is left");
    }
}

#[test]
fn compile_deletes_the_staging_folder_a_dead_build_left_and_proves_its_law() {
    let cache = tempfile::tempdir().unwrap();
    let stale = cache.path().join("mago/lean/0123456789abcdef0123456789abcdef.staging");
    std::fs::create_dir_all(&stale).unwrap();
    let project = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);

    let output = command(project.path(), "never").env("XDG_CACHE_HOME", cache.path()).output().unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(read(project.path(), "app/Shared/Money.lean"), MONEY_PROOF);
    assert!(!stale.exists(), "the staging folder a dead build left is deleted");
    assert_eq!(library_builds(cache.path()).len(), 1, "one library build is left");
}

#[test]
fn compiles_that_start_together_on_a_library_build_that_lost_its_lakefile_each_prove_their_law_and_leave_one_build() {
    let cache = tempfile::tempdir().unwrap();
    let compile_in_cache =
        |project: &Path| command(project, "never").env("XDG_CACHE_HOME", cache.path()).output().unwrap();
    let output = compile_in_cache(lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]).path());
    assert!(output.status.success(), "{}", printed(&output));
    let library = library_builds(cache.path()).remove(0);
    std::fs::remove_file(library.join("lakefile.toml")).unwrap();
    let projects: Vec<_> = (0..4).map(|_| lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)])).collect();

    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let compiles: Vec<_> =
            projects.iter().map(|project| scope.spawn(|| compile_in_cache(project.path()))).collect();
        compiles.into_iter().map(|compile| compile.join().unwrap()).collect()
    });

    for (project, output) in projects.iter().zip(&outputs) {
        assert!(output.status.success(), "{}", printed(output));
        assert_eq!(read(project.path(), "app/Shared/Money.lean"), MONEY_PROOF);
    }
    assert!(library.join("lakefile.toml").is_file(), "the library build has its lakefile again");
    assert_eq!(library_builds(cache.path()), [library], "one library build is left, with no staging folder");
}

#[test]
fn a_second_compile_leaves_the_proof_byte_identical_and_starts_no_lean_process() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);
    let before = std::fs::read(compiled(root, "app/Shared/Money.sharp")).unwrap();

    let output = compile_without_lean(root);

    assert!(output.status.success(), "the compile needed lake: {}", printed(&output));
    assert_eq!(read(root, "app/Shared/Money.lean"), MONEY_PROOF);
    assert_eq!(std::fs::read(compiled(root, "app/Shared/Money.sharp")).unwrap(), before);
}

#[test]
fn a_second_compile_of_the_whole_workspace_reads_no_source_through_the_sharp_folder() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\".\"]\n");
    compile_succeeds(root);

    let output = compile_lines(root);

    assert!(output.status.success(), "{}", printed(&output));
}

#[test]
fn compile_proves_a_law_in_a_project_that_installs_the_standard_library() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        (
            "vendor/heyjordanparker/php-sharp-composer/composer.json",
            "{\n    \"name\": \"heyjordanparker/php-sharp-composer\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"Sharp\\\\\": \"library/Sharp/\"\n        }\n    }\n}\n",
        ),
        (
            "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Math/Math.sharp",
            "namespace Sharp.Math;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(read(root, "app/Shared/Money.lean"), MONEY_PROOF);
}

#[test]
fn compile_refuses_a_law_a_changed_method_breaks_on_the_law_line_and_writes_no_compiled_file() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY
            .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency"),
    );
    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Money.sharp:8:5:error - unproven-law: Law addKeepsCurrency is not proven: Lean rejected its proof in app/Shared/Money.lean line 4: unsolved goals"),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(read(root, "app/Shared/Money.lean"), MONEY_PROOF, "an existing proof is never rewritten");
}

/// `Money` with a second class `Wallet` of the same shape, each with its law in its own file.
fn money_and_wallet() -> tempfile::TempDir {
    lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        ("app/Shared/Wallet.sharp", &LAWFUL_MONEY.replace("Money", "Wallet")),
    ])
}

#[test]
fn compile_proves_two_stale_law_files_and_refuses_each_one_whose_method_breaks_its_law() {
    let directory = money_and_wallet();
    let root = directory.path();
    compile_succeeds(root);
    assert_eq!(read(root, "app/Shared/Money.lean"), MONEY_PROOF);
    assert_eq!(read(root, "app/Shared/Wallet.lean"), MONEY_PROOF.replace("Money", "Wallet"));

    for class in ["Money", "Wallet"] {
        write(
            root,
            &format!("app/Shared/{class}.sharp"),
            &LAWFUL_MONEY
                .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency")
                .replace("Money", class),
        );
    }
    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for class in ["Money", "Wallet"] {
        assert!(
            printed(&output).contains(&format!("app/Shared/{class}.sharp:8:5:error - unproven-law: Law addKeepsCurrency is not proven: Lean rejected its proof in app/Shared/{class}.lean line 4: unsolved goals")),
            "{class}: {}",
            printed(&output)
        );
        assert!(!compiled(root, &format!("app/Shared/{class}.sharp")).exists(), "{class}");
    }
}

/// Puts a `lake` first on the `PATH` that runs the real one, except that each `sharp-lean` runner it starts first
/// waits up to 30 seconds for a second runner to start. A runner that waited in vain leaves `alone` in `folder`.
#[cfg(unix)]
fn rendezvous_lake(folder: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH").unwrap();
    let real =
        std::env::split_paths(&path).map(|directory| directory.join("lake")).find(|lake| lake.is_file()).unwrap();
    let started = folder.join("started");
    std::fs::create_dir_all(&started).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$2\" in\n  */sharp-lean)\n    touch '{started}/'$$\n    for _ in $(seq 300); do\n      [ \"$(ls '{started}' | wc -l)\" -ge 2 ] && break\n      sleep 0.1\n    done\n    [ \"$(ls '{started}' | wc -l)\" -ge 2 ] || touch '{alone}'\n    ;;\nesac\nexec '{real}' \"$@\"\n",
        started = started.display(),
        alone = folder.join("alone").display(),
        real = real.display(),
    );
    let lake = folder.join("bin/lake");
    write(folder, "bin/lake", &script);
    std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).unwrap();

    std::env::join_paths(std::iter::once(folder.join("bin")).chain(std::env::split_paths(&path))).unwrap()
}

#[cfg(unix)]
#[test]
fn compile_runs_the_runners_of_two_stale_law_files_at_the_same_time() {
    let directory = money_and_wallet();
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output =
        command(root, "never").env("PATH", rendezvous_lake(lake.path())).env("MAGO_THREADS", "2").output().unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!lake.path().join("alone").exists(), "the second runner started only after the first one ended");
    assert_eq!(read(root, "app/Shared/Wallet.lean"), MONEY_PROOF.replace("Money", "Wallet"));
}

/// A `lake` on the `PATH` builds the Lean code, so CI's and a user's own elan keep working, and Mago installs no Lean.
#[cfg(unix)]
#[test]
fn compile_runs_the_lake_on_the_path_and_installs_no_lean() {
    use std::os::unix::fs::PermissionsExt;

    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let lake = tempfile::tempdir().unwrap();
    write(lake.path(), "lake", "#!/bin/sh\necho \"the lake on the PATH ran $*\"\n[ \"$1\" = --version ]\n");
    std::fs::set_permissions(lake.path().join("lake"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = tempfile::tempdir().unwrap();

    let output = command(directory.path(), "never")
        .env("PATH", lake.path())
        .env("XDG_CACHE_HOME", cache.path())
        .output()
        .unwrap();

    assert!(printed(&output).contains("the lake on the PATH ran build"), "{}", printed(&output));
    assert!(!cache.path().join("mago/elan").exists(), "{}", printed(&output));
}

#[test]
fn compile_refuses_a_gap_every_time_and_never_leaves_a_current_compiled_file() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        (
            "app/Shared/Money.lean",
            &MONEY_PROOF.replace("simp [App.Shared.Money.addKeepsCurrency, App.Shared.Money.add]", "sorry"),
        ),
    ]);
    let root = directory.path();

    for run in ["first", "second"] {
        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{run}: {}", printed(&output));
        assert!(
            printed(&output).contains("app/Shared/Money.sharp:8:5:error - unproven-law: Law addKeepsCurrency is not proven: app/Shared/Money.lean line 4 is a gap (sorry). Write the proof there, or change the law."),
            "{run}: {}",
            printed(&output)
        );
        assert!(!compiled(root, "app/Shared/Money.sharp").exists(), "{run}");
    }
}

#[test]
fn compile_refuses_a_proof_that_uses_native_decide() {
    let directory = lawful_project(&[
        (
            "app/Shared/Limits.sharp",
            "namespace App.Shared;\n\npublic class Limits\n{\n    public static int double(int a) => a + a;\n\n    law doubleOfTwo() => Limits.double(2) == 4;\n}\n",
        ),
        (
            "app/Shared/Limits.lean",
            "import Code.App.Shared.Limits\nopen Sharp\n\ntheorem doubleOfTwo : App.Shared.Limits.doubleOfTwo := by\n  unfold App.Shared.Limits.doubleOfTwo\n  native_decide\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Limits.sharp:7:5:error - unproven-law: Law doubleOfTwo is not proven: its proof in app/Shared/Limits.lean uses native_decide, which trusts compiled code instead of Lean's kernel. Use decide."),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Limits.sharp").exists());
}

/// Spec section 28.1: a proof whose law was deleted is a compile error, whether the law, its whole class or its whole
/// module was deleted. It sits on the class or module name, or at the start of a file that states neither any more.
#[test]
fn compile_reports_the_proof_of_a_deleted_law() {
    const MONEY_DELETED: &str = "app/Shared/Money.lean line 4 proves App.Shared.Money.addKeepsCurrency, which Money.sharp no longer states. Delete the proof, or restore the law.";
    const MODULE_DELETED: &str = "app/Tenant/Store/Module.lean line 4 proves App.Tenant.Store.Module.receiptMatchesRefunds, which Module.sharp no longer states. Delete the proof, or restore the law.";
    let lawless_money =
        LAWFUL_MONEY.replace("\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n", "");
    let refundable = "refunded < paid ? refunded : paid";

    for (directory, source, reduced, position, deleted) in [
        (
            lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]),
            "app/Shared/Money.sharp",
            lawless_money.as_str(),
            "3:14",
            MONEY_DELETED,
        ),
        (
            lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]),
            "app/Shared/Money.sharp",
            "namespace App.Shared;\n",
            "1:1",
            MONEY_DELETED,
        ),
        (
            store(refundable, refundable),
            "app/Tenant/Store/Module.sharp",
            "namespace App.Tenant.Store;\n\nmodule\n{\n}\n",
            "3:1",
            MODULE_DELETED,
        ),
        (
            store(refundable, refundable),
            "app/Tenant/Store/Module.sharp",
            "namespace App.Tenant.Store;\n",
            "1:1",
            MODULE_DELETED,
        ),
    ] {
        let root = directory.path();
        compile_succeeds(root);

        write(root, source, reduced);
        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{reduced}: {}", printed(&output));
        assert!(
            printed(&output).contains(&format!("{source}:{position}:error - non-existent-law: {deleted}")),
            "{reduced}: {}",
            printed(&output)
        );
        assert!(!compiled(root, source).exists(), "{reduced}");
    }
}

/// A file whose law needs proving, or that a structure rule judges, needs Lean; with no `lake` and no Lean Mago can
/// install, each such file is refused with `missing-lean`, which names what failed, and every other file compiles.
#[test]
fn compile_reports_missing_lean_on_each_file_that_needs_lean_and_whose_compiled_file_is_not_current() {
    let missing = if cfg!(target_env = "musl") {
        "error - missing-lean: Laws and structure rules need Lean 4.34.1, and no lake is on the PATH. This musl build of Mago can't install Lean, because Lean has no musl build. On glibc Linux, Mago's glibc build installs Lean itself.".to_owned()
    } else {
        format!(
            "error - missing-lean: Laws and structure rules need Lean 4.34.1, and no lake is on the PATH. Mago could not install Lean into {}: File exists (os error 17). Run vendor/bin/mago compile again to retry.",
            unwritable_cache().join("mago/elan").display()
        )
    };
    let shared = rules("App.Shared", "@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant");
    for (files, refused) in [
        (
            vec![("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Order.sharp", ORDER)],
            vec!["app/Shared/Money.sharp:3:14"],
        ),
        (
            vec![
                ("app/Shared/Module.lean", shared.as_str()),
                ("app/Shared/Ledger.sharp", "namespace App.Shared;\n\npublic class Ledger { }\n"),
                ("app/Order.sharp", ORDER),
            ],
            vec!["app/Shared/Ledger.sharp:1:11"],
        ),
    ] {
        let directory = lawful_project(&files);
        let root = directory.path();

        let output = compile_without_lean(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        let expected: Vec<String> = refused.iter().map(|place| format!("{place}:{missing}")).collect();
        assert_eq!(issue_lines(&output), expected, "{}", printed(&output));
        assert!(compiled(root, "app/Order.sharp").exists());
    }
}

#[test]
fn compile_needs_no_lean_for_a_project_with_no_law_and_no_proof_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!root.join(".sharp/.lean").exists());
}

#[test]
fn an_edit_to_a_file_no_law_reaches_starts_no_lean_process() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Order.sharp", ORDER)]);
    let root = directory.path();
    compile_succeeds(root);

    write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    let output = compile_without_lean(root);

    assert!(output.status.success(), "the compile needed lake: {}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

#[test]
fn compile_deletes_the_lean_package_once_no_law_and_no_proof_file_remain() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::remove_file(root.join("app/Shared/Money.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Shared/Money.lean")).unwrap();
    write(root, "app/Order.sharp", ORDER);
    let output = compile_without_lean(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!root.join(".sharp/.lean").exists());
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("app/Order.sharpc")]);
}

#[test]
fn compile_appends_a_proof_for_a_new_law_and_keeps_every_existing_proof() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY.replace(
            "a.add(b).currency == a.currency;\n",
            "a.add(b).currency == a.currency;\n    law addKeepsOtherCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n",
        ),
    );
    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        read(root, "app/Shared/Money.lean"),
        format!(
            "{MONEY_PROOF}\ntheorem addKeepsOtherCurrency : App.Shared.Money.addKeepsOtherCurrency := by\n  simp [App.Shared.Money.addKeepsOtherCurrency, App.Shared.Money.add]\n"
        )
    );
}

#[test]
fn compile_proves_a_law_whose_calls_go_from_one_file_to_another_and_back_with_no_import_cycle() {
    let directory = lawful_project(&[
        (
            "app/Shared/Alpha.sharp",
            "namespace App.Shared;\n\npublic class Alpha\n{\n    public static int start(int a) => Beta.middle(a);\n    public static int finish(int a) => a;\n\n    law roundTrip(int a) => Alpha.start(a) == a;\n}\n",
        ),
        (
            "app/Shared/Beta.sharp",
            "namespace App.Shared;\n\npublic class Beta\n{\n    public static int middle(int a) => Alpha.finish(a);\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Alpha.sharp").exists());
    assert_eq!(
        read(root, ".sharp/.lean/Code/App/Shared/Beta.lean"),
        "-- Generated by mago compile. Do not edit.\nimport Code.App.Shared.Alpha\n"
    );
    assert!(!read(root, ".sharp/.lean/Code/App/Shared/Alpha.lean").contains("import Code."));
}

#[test]
fn compile_proves_spec_section_28_1_s_state_machine_law_over_an_enum_match() {
    let directory = lawful_project(&[
        (
            "app/Shop/Payment.sharp",
            "namespace App.Shop;\n\npublic enum Payment\n{\n    case Captured;\n    case Returned;\n}\n",
        ),
        (
            "app/Shop/Status.sharp",
            "namespace App.Shop;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n    case Paid = \"paid\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => match (this) {\n        Status.Open => e == Payment.Captured ? Status.Paid : Status.Open,\n        Status.Paid => e == Payment.Returned ? Status.Refunded : Status.Paid,\n        Status.Refunded => Status.Refunded,\n    };\n\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        read(root, "app/Shop/Status.lean").contains("theorem refundedIsFinal : App.Shop.Status.refundedIsFinal := by")
    );
    assert!(compiled(root, "app/Shop/Status.sharp").exists());
}

/// Spec section 28's `app/Tenant/Store/Module.sharp`, whose law spans `Receipts` and `Refunds`.
const STORE_MODULE: &str = "namespace App.Tenant.Store;\n\nmodule\n{\n    law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);\n}\n";

/// `Receipts` and `Refunds`, each with the method `STORE_MODULE`'s law reaches, written with `refundable`'s body.
fn store(refundable: &str, remaining: &str) -> tempfile::TempDir {
    lawful_project(&[
        ("app/Tenant/Store/Module.sharp", STORE_MODULE),
        (
            "app/Tenant/Store/Receipts.sharp",
            &format!(
                "namespace App.Tenant.Store;\n\npublic static class Receipts\n{{\n    public static int refundable(int paid, int refunded) => {refundable};\n}}\n"
            ),
        ),
        (
            "app/Tenant/Store/Refunds.sharp",
            &format!(
                "namespace App.Tenant.Store;\n\npublic static class Refunds\n{{\n    public static int remaining(int paid, int refunded) => {remaining};\n}}\n"
            ),
        ),
    ])
}

/// Spec section 28.1: `app/Tenant/Store/Module.lean` proves the module law, created with the proof `simp` found.
#[test]
fn compile_proves_spec_section_28_s_module_law_with_the_proof_it_creates_in_module_lean() {
    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? refunded : paid");
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        read(root, "app/Tenant/Store/Module.lean"),
        "import Code.App.Tenant.Store.Module\nopen Sharp\n\ntheorem receiptMatchesRefunds : App.Tenant.Store.Module.receiptMatchesRefunds := by\n  simp [App.Tenant.Store.Module.receiptMatchesRefunds, App.Tenant.Store.Receipts.refundable, App.Tenant.Store.Refunds.remaining]\n"
    );
    assert!(compiled(root, "app/Tenant/Store/Module.sharp").exists());
}

/// A module law that does not hold refuses `Module.sharp` and names the law, as a class law refuses its class's file,
/// and the classes it reaches still compile.
#[test]
fn compile_refuses_module_sharp_when_its_law_does_not_hold() {
    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? paid : refunded");
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Tenant/Store/Module.sharp:5:5:error - unproven-law: Law receiptMatchesRefunds is not proven: app/Tenant/Store/Module.lean line 6 is a gap (sorry). Write the proof there, or change the law."),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Tenant/Store/Module.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Refunds.sharp").exists());
}

/// `app/Shop/Module.sharp`, the folder's module, with one law.
const SHOP_MODULE: &str = "namespace App.Shop;\n\nmodule\n{\n    law same(int a) => a == a;\n}\n";

/// Spec section 32: a namespace's module is the one its folder's `Module.sharp` declares. A `module` in another file
/// of the folder is refused on that file alone, and `Module.sharp` still compiles and proves its law.
#[test]
fn compile_refuses_a_module_outside_module_sharp_on_its_own_file_and_proves_module_sharp() {
    for (other, code, refusal) in [
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule : App.Shop\n{\n}\n\npublic class CheckoutTest { }\n",
            "app/Shop/CheckoutTest.sharp:3:8:error - parse: A module that overrides another is not supported yet in PHP#.",
        ),
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/CheckoutTest.sharp:3:1:error - semantics: A module is declared in its folder's Module.sharp.",
        ),
        (
            "app/Shop/Module.Production.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/Module.Production.sharp:3:1:error - semantics: An environment module is not supported yet in PHP#.",
        ),
    ] {
        let directory = lawful_project(&[("app/Shop/Module.sharp", SHOP_MODULE), (other, code)]);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{other}: {}", printed(&output));
        assert!(printed(&output).contains(refusal), "{other}: {}", printed(&output));
        assert!(!printed(&output).contains("app/Shop/Module.sharp"), "{other}: {}", printed(&output));
        assert!(compiled(root, "app/Shop/Module.sharp").exists(), "{other}");
    }
}

/// A plain PHP class named like a PHP# module keeps its name, so Mago's PHP behavior does not change, and the module's
/// `Module.sharp` is refused as a second definition of the name.
#[test]
fn compile_refuses_module_sharp_when_a_plain_php_class_holds_its_name() {
    let directory = lawful_project(&[
        ("app/Shop/Module.sharp", SHOP_MODULE),
        (
            "app/Shop/Module.php",
            "<?php\n\ndeclare(strict_types=1);\n\nnamespace App\\Shop;\n\nfinal class Module\n{\n    public function one(): int\n    {\n        return 1;\n    }\n}\n",
        ),
        (
            "app/Shop/Uses.php",
            "<?php\n\ndeclare(strict_types=1);\n\nnamespace App\\Shop;\n\nfinal class Uses\n{\n    public function run(): int\n    {\n        return (new Module())->one();\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shop/Module.sharp:3:1:error - duplicate-definition: "),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shop/Module.sharp").exists());
}

/// Spec section 23: "`Store.sharp` beside a `Store/` folder is a compile error, because both would be
/// `App.Tenant.Store`." The class is refused on its name with one message, whether or not it states a law that Lean
/// would declare under the same name as the namespace.
#[test]
fn compile_refuses_a_class_beside_a_folder_of_its_name_with_or_without_a_law() {
    for store in [
        "namespace App.Tenant;\n\npublic class Store\n{\n}\n",
        "namespace App.Tenant;\n\npublic class Store\n{\n    law same(int a) => a == a;\n}\n",
    ] {
        let directory =
            lawful_project(&[("app/Tenant/Store.sharp", store), ("app/Tenant/Store/Receipts.sharp", RECEIPTS)]);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(
            issue_lines(&output),
            [
                "app/Tenant/Store.sharp:3:14:error - name-already-in-use: `App.Tenant.Store` is both a class and a namespace: rename the class or the folder."
            ],
            "{store}"
        );
        assert!(!compiled(root, "app/Tenant/Store.sharp").exists(), "{store}");
        assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists(), "{store}");
    }
}

/// `app/Shared/Ledger.sharp`, whose line 3 imports a class of `App.Tenant`.
const LEDGER: &str = "namespace App.Shared;\n\nimport App.Tenant.Store.Receipts;\n\npublic class Ledger\n{\n    public static int refundable(int paid) => Receipts.refundable(paid, 0);\n}\n";

/// `app/Tenant/Store/Receipts.sharp`, which imports nothing.
const RECEIPTS: &str = "namespace App.Tenant.Store;\n\npublic class Receipts\n{\n    public static int refundable(int paid, int refunded) => paid - refunded;\n}\n";

/// `app/Tenant/Store/StoreService.sharp`, whose line 3 imports `App.Shared.Ledger`.
const STORE_SERVICE: &str = "namespace App.Tenant.Store;\n\nimport App.Shared.Ledger;\n\npublic class StoreService\n{\n    public int refundable(int paid) => Ledger.refundable(paid);\n}\n";

/// The `Module.lean` of the folder of `namespace`, holding `rules` after the lines every Lean file beside PHP# code
/// starts with.
fn rules(namespace: &str, rules: &str) -> String {
    format!("import Code.{namespace}.Module\nopen Sharp\n\n{rules}\n")
}

/// Section 28.1's imports-only rule, which names two namespaces.
const IMPORTS_ONLY: &str = "@[rule] def importsOnly : Rule := .importsOnly [App.Tenant.Community, App.Shared.Schema]";

/// The design note's `p3` without `app/Module.lean`: `Ledger.sharp`, `Receipts.sharp` and `StoreService.sharp`, with
/// "Shared never reaches Tenant" in `app/Shared/Module.lean` and section 28.1's imports-only rule in
/// `app/Tenant/Store/Module.lean`, beside a class in each namespace that rule names, and then `files`, which may
/// replace them.
fn structured(files: &[(&str, &str)]) -> tempfile::TempDir {
    let shared = rules("App.Shared", "@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant");
    let store = rules("App.Tenant.Store", IMPORTS_ONLY);
    let mut all = vec![
        ("app/Shared/Ledger.sharp", LEDGER),
        ("app/Shared/Module.lean", shared.as_str()),
        ("app/Shared/Schema/Entity.sharp", "namespace App.Shared.Schema;\n\npublic class Entity { }\n"),
        ("app/Tenant/Community/Forum.sharp", "namespace App.Tenant.Community;\n\npublic class Forum { }\n"),
        ("app/Tenant/Store/Module.lean", store.as_str()),
        ("app/Tenant/Store/Receipts.sharp", RECEIPTS),
        ("app/Tenant/Store/StoreService.sharp", STORE_SERVICE),
    ];
    all.extend_from_slice(files);

    lawful_project(&all)
}

/// Each issue line `mago compile` printed, in its order.
fn issue_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(":error - "))
        .map(str::to_owned)
        .collect()
}

/// Spec section 28.1: "A failed rule is reported on the code that breaks it, such as the offending `import` line", and
/// section 27: "A broken structure rule stops the file from running". Each row is a fact a rule judges: an import, an
/// import on a cycle of the namespace import graph, a name reference and a declaration.
#[test]
fn compile_refuses_each_file_that_breaks_a_structure_rule_on_the_code_that_breaks_it() {
    let cycles = rules("App", "@[rule] def noImportCycles : Rule := .noImportCycles");
    let references = rules(
        "App.Shared",
        "@[rule] def neverCallsTenant : Rule := .each fun _ fact => match fact with\n  | .reference r => under r.name App.Tenant\n  | _ => false",
    );
    let declarations = rules(
        "App.Tenant.Store",
        "@[rule] def noServices : Rule := .each fun _ fact => match fact with\n  | .declaration d => d.name.endsWith \"Service\"\n  | _ => false",
    );
    let fine = rules("App.Shared", "");
    let open = rules("App.Tenant.Store", "");
    for (files, refused, accepted) in [
        (
            vec![],
            vec![
                "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesTenant in app/Shared/Module.lean.",
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule importsOnly in app/Tenant/Store/Module.lean.",
            ],
            "app/Tenant/Store/Receipts.sharp",
        ),
        (
            vec![
                ("app/Module.lean", cycles.as_str()),
                ("app/Shared/Module.lean", fine.as_str()),
                ("app/Tenant/Store/Module.lean", open.as_str()),
            ],
            vec![
                "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule noImportCycles in app/Module.lean.",
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule noImportCycles in app/Module.lean.",
            ],
            "app/Tenant/Store/Receipts.sharp",
        ),
        (
            vec![("app/Shared/Module.lean", references.as_str()), ("app/Tenant/Store/Module.lean", open.as_str())],
            vec![
                "app/Shared/Ledger.sharp:7:47:error - broken-rule: This reference to App.Tenant.Store.Receipts breaks the structure rule neverCallsTenant in app/Shared/Module.lean.",
            ],
            "app/Tenant/Store/StoreService.sharp",
        ),
        (
            vec![("app/Shared/Module.lean", fine.as_str()), ("app/Tenant/Store/Module.lean", declarations.as_str())],
            vec![
                "app/Tenant/Store/StoreService.sharp:5:14:error - broken-rule: This declaration of App.Tenant.Store.StoreService breaks the structure rule noServices in app/Tenant/Store/Module.lean.",
            ],
            "app/Shared/Ledger.sharp",
        ),
    ] {
        let directory = structured(&files);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert_eq!(issue_lines(&output), refused, "{}", printed(&output));
        for line in &refused {
            let name = line.split(':').next().unwrap();
            assert!(!compiled(root, name).exists(), "{name}");
        }
        assert!(compiled(root, accepted).exists(), "{accepted}");
    }
}

/// What a rule allows compiles. `Rule.importsOnly` allows the rule's own namespace and the standard library, and a
/// namespace holds only the names under it: `App.TenantDirectory` is not inside `App.Tenant`. Each row is an import
/// section 28.1's rules allow, and the project compiles.
#[test]
fn a_structure_rule_accepts_every_import_it_allows() {
    let fine = rules("App.Shared", "");
    let service =
        |import: &str| format!("namespace App.Tenant.Store;\n\nimport {import};\n\npublic class StoreService {{ }}\n");
    let own = service("App.Tenant.Store.Receipts");
    let library = service("Sharp.Json.Json");
    for files in [
        vec![("app/Shared/Module.lean", fine.as_str()), ("app/Tenant/Store/StoreService.sharp", own.as_str())],
        vec![("app/Shared/Module.lean", fine.as_str()), ("app/Tenant/Store/StoreService.sharp", library.as_str())],
        vec![
            (
                "app/Shared/Ledger.sharp",
                "namespace App.Shared;\n\nimport App.TenantDirectory.Listing;\n\npublic class Ledger { }\n",
            ),
            ("app/TenantDirectory/Listing.sharp", "namespace App.TenantDirectory;\n\npublic class Listing { }\n"),
            ("app/Tenant/Store/StoreService.sharp", "namespace App.Tenant.Store;\n\npublic class StoreService { }\n"),
        ],
    ] {
        let directory = structured(&files);
        let root = directory.path();

        let output = compile_lines(root);

        assert!(output.status.success(), "{files:?}: {}", printed(&output));
        assert!(compiled(root, "app/Tenant/Store/StoreService.sharp").exists(), "{files:?}");
    }
}

/// `app/Shared/Ledger.sharp` breaks "Shared never reaches Tenant" on its import line.
const LEDGER_BREAKS: &str = "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesTenant in app/Shared/Module.lean.";

/// The issue on each `.sharp` file of `App.Tenant.Store` when Lean rejects `app/Tenant/Store/Module.lean` at `place`
/// with `message`.
fn store_rejected(file: &str, place: &str, message: &str) -> String {
    format!(
        "app/Tenant/Store/{file}.sharp:1:11:error - broken-rule: This file is in App.Tenant.Store, whose structure rules app/Tenant/Store/Module.lean holds, and Lean rejected app/Tenant/Store/Module.lean {place}: {message}."
    )
}

/// Spec section 28.1: a structure rule names a namespace by its full name, which Lean resolves as any name, so a
/// misspelled namespace is Lean's unknown identifier at its line and column of the `Module.lean`, in the standard
/// library's rules and in a rule the project writes alike, and refuses every file the rules judge. Each row is the
/// rules of `app/Tenant/Store/Module.lean`, whose fourth line is the first rule: real namespaces of the project, of
/// plain PHP in the project and in `vendor/`, and of the standard library judge imports, and each misspelled one is
/// reported at its own place. A name is a namespace only through a class-like of the codebase: a `Namespace` built from
/// a string, or declared in a `Module.lean`, is refused with Lean's whole reason.
#[test]
fn a_structure_rule_names_a_namespace_by_its_full_name_and_a_misspelled_one_is_an_unknown_identifier() {
    let ledgr = "Unknown identifier `App.Shared.Ledgr`";
    let json = "namespace App.Tenant.Store;\n\nimport Sharp.Json.Json;\n\npublic class StoreService { }\n";
    let plain_php = "namespace App.Tenant.Store;\n\nimport App.Legacy.Rate;\nimport Illuminate.Support.Facades.DB;\nimport App.Legacy.fun.Clock;\n\npublic class StoreService { }\n";
    let plain_php_breaks = |line: u32, imported: &str, rule: &str| {
        format!(
            "app/Tenant/Store/StoreService.sharp:{line}:8:error - broken-rule: This import of {imported} breaks the structure rule {rule} in app/Tenant/Store/Module.lean."
        )
    };
    let anonymous = "Invalid `⟨...⟩` notation: Constructor for `Sharp.Namespace` is marked as private";
    let instance = "invalid {...} notation, constructor for `Namespace` is marked as private";
    let declared =
        "`namespace_decl` belongs in the module mago compile generates: a rule names only a namespace of the codebase";
    let quoted = "Application type mismatch: The argument \"App.Shared.Ledgr\" has type String but is expected to have type Namespace in the application Rule.neverImports \"App.Shared.Ledgr\"";
    for (rules_of_store, service, refused) in [
        (
            IMPORTS_ONLY.to_owned(),
            STORE_SERVICE,
            vec![
                LEDGER_BREAKS.to_owned(),
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule importsOnly in app/Tenant/Store/Module.lean.".to_owned(),
            ],
        ),
        (
            format!(
                "{IMPORTS_ONLY}\n@[rule] def neverLedger : Rule := .neverImports App.Shared.Ledgr\n@[rule] def ownLedger : Rule := .imports fun _ i => under i.name App.Shared.Ledgr"
            ),
            STORE_SERVICE,
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 5, column 49", ledgr),
                store_rejected("Receipts", "line 6, column 66", ledgr),
                store_rejected("StoreService", "line 5, column 49", ledgr),
                store_rejected("StoreService", "line 6, column 66", ledgr),
            ],
        ),
        (
            "@[rule] def importsOnly : Rule := .importsOnly [App.Tenant.Communty, App.Shared.Schema]".to_owned(),
            STORE_SERVICE,
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 4, column 49", "Unknown identifier `App.Tenant.Communty`"),
                store_rejected("StoreService", "line 4, column 49", "Unknown identifier `App.Tenant.Communty`"),
            ],
        ),
        (
            "@[rule] def neverJson : Rule := .neverImports Sharp.Json".to_owned(),
            json,
            vec![
                LEDGER_BREAKS.to_owned(),
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of Sharp.Json.Json breaks the structure rule neverJson in app/Tenant/Store/Module.lean.".to_owned(),
            ],
        ),
        (
            "@[rule] def legacy : Rule := .neverImports App.Legacy\n@[rule] def facades : Rule := .neverImports Illuminate.Support.Facades\n@[rule] def keyword : Rule := .neverImports App.Legacy.«fun»".to_owned(),
            plain_php,
            vec![
                LEDGER_BREAKS.to_owned(),
                plain_php_breaks(5, "App.Legacy.fun.Clock", "keyword"),
                plain_php_breaks(4, "Illuminate.Support.Facades.DB", "facades"),
                plain_php_breaks(3, "App.Legacy.Rate", "legacy"),
                plain_php_breaks(5, "App.Legacy.fun.Clock", "legacy"),
            ],
        ),
        (
            "@[rule] def anonymous : Rule := .neverImports ⟨\"App.Shared.Ledgr\"⟩\n@[rule] def structureInstance : Rule := .neverImports { name := \"App.Shared.Ledgr\" }\nnamespace_decl App.Shared.Ledgr".to_owned(),
            STORE_SERVICE,
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 4, column 47", anonymous),
                store_rejected("Receipts", "line 5, column 55", instance),
                store_rejected("Receipts", "line 6, column 1", declared),
                store_rejected("StoreService", "line 4, column 47", anonymous),
                store_rejected("StoreService", "line 5, column 55", instance),
                store_rejected("StoreService", "line 6, column 1", declared),
            ],
        ),
        (
            "@[rule] def quoted : Rule := .neverImports \"App.Shared.Ledgr\"".to_owned(),
            STORE_SERVICE,
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 4, column 44", quoted),
                store_rejected("StoreService", "line 4, column 44", quoted),
            ],
        ),
    ] {
        let store = rules("App.Tenant.Store", &rules_of_store);
        let directory = structured(&[
            ("app/Tenant/Store/Module.lean", store.as_str()),
            ("app/Tenant/Store/StoreService.sharp", service),
            ("app/Legacy/Rate.php", "<?php\n\nnamespace App\\Legacy;\n\nfinal class Rate {}\n"),
            ("app/Legacy/fun/Clock.php", "<?php\n\nnamespace App\\Legacy\\fun;\n\nfinal class Clock {}\n"),
            ("vendor/illuminate/support/Facades/DB.php", "<?php\n\nnamespace Illuminate\\Support\\Facades;\n\nclass DB {}\n"),
            // A namespace Lean already names, as `List`, is declared by none, so the rest of `Code` still builds.
            ("vendor/acme/list/Item.php", "<?php\n\nnamespace List;\n\nclass Item {}\n"),
            (
                "vendor/heyjordanparker/php-sharp-composer/composer.json",
                "{\"name\": \"heyjordanparker/php-sharp-composer\", \"autoload\": {\"psr-4\": {\"Sharp\\\\\": \"library/Sharp/\"}}}\n",
            ),
            (
                "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Json/Json.sharp",
                "namespace Sharp.Json;\n\npublic static class Json\n{\n    public static int one() => 1;\n}\n",
            ),
        ]);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{rules_of_store}: {}", printed(&output));
        assert_eq!(issue_lines(&output), refused, "{rules_of_store}: {}", printed(&output));
    }
}

/// A structure rule runs only on what Lean's kernel checks, and names a namespace only through the constants `Code`
/// declares: any other way to make a `Namespace` would let a misspelled name compile and never fire. So the runner
/// does not run a rule that reaches `sorry`, an axiom beyond Lean's standard ones, an unsafe constant, a constant whose
/// compiled code is not its definition, `Sharp.Rule.breaks` or the constructor of `Namespace`, nor one Lean cannot
/// evaluate. It refuses each file of the rule's namespace, naming the rule, its place and what it reached, and the
/// compile still checks every other file. Each row is the `app/Tenant/Store/Module.lean` of `structured`.
#[test]
fn a_structure_rule_that_reaches_more_than_lean_checks_does_not_run() {
    let ledgr = "\"App.Shared.Ledgr\"";
    let elaborator = "open Lean Elab Term in\nelab \"ns!\" s:str : term => do\n  let some (mk, _) := (← getEnv).constants.toList.find? fun (n, _) => (privateToUserName? n) == some `Sharp.Namespace.mk\n    | throwError \"no constructor\"\n  return mkApp (mkConst mk) (mkStrLit s.getString)\n";
    let reserved = |name: &str| {
        format!(
            "it reaches {name}, which makes a namespace from a string. Name the namespace by its full name, as App.Shared.Schema"
        )
    };
    for (module_lean, rule, place, reason) in [
        (
            rules(
                "App.Tenant.Store",
                &format!(
                    "@[rule] def viaBreaks : Rule := .imports fun _ i => Rule.breaks (.imports fun ns j => under j.name ns) {ledgr} {{}} (.import {{ i with namespace_ := {ledgr} }})"
                ),
            ),
            "viaBreaks",
            "line 4, column 13",
            reserved("Sharp.Rule.breaks"),
        ),
        (
            format!(
                "import Lean\n{}",
                rules(
                    "App.Tenant.Store",
                    &format!("{elaborator}@[rule] def withElab : Rule := .neverImports (ns! {ledgr})")
                )
            ),
            "withElab",
            "line 10, column 13",
            reserved("Sharp.Namespace.mk"),
        ),
        (
            rules(
                "App.Tenant.Store",
                &format!("@[rule] unsafe def withUnsafeCast : Rule := .neverImports (unsafeCast {ledgr})"),
            ),
            "withUnsafeCast",
            "line 4, column 20",
            "it reaches unsafeCast, which is unsafe, so Lean's kernel does not check it".to_owned(),
        ),
        (
            rules(
                "App.Tenant.Store",
                &format!(
                    "axiom same : String = Namespace\n@[rule] def withAxiomCast : Rule := .neverImports (cast same {ledgr})"
                ),
            ),
            "withAxiomCast",
            "line 5, column 13",
            "it uses the axiom same, which Lean's kernel does not check".to_owned(),
        ),
        (
            rules("App.Tenant.Store", "@[rule] def withSorry : Rule := .neverImports sorry"),
            "withSorry",
            "line 4, column 13",
            "it uses sorry. Write the rule out".to_owned(),
        ),
        (
            rules(
                "App.Tenant.Store",
                "def legacy : Namespace := App.Shared.Schema\n@[implemented_by legacy] def schema : Namespace := App.Shared.Schema\n@[rule] def implemented : Rule := .neverImports schema",
            ),
            "implemented",
            "line 6, column 13",
            "it reaches schema, whose compiled code is not the definition Lean checked".to_owned(),
        ),
        (
            rules(
                "App.Tenant.Store",
                "@[extern \"sharp_schema\"] def schema (u : Unit) : Namespace := App.Shared.Schema\n@[rule] def externed : Rule := .neverImports (schema ())",
            ),
            "externed",
            "line 5, column 13",
            "it reaches schema, whose compiled code is not the definition Lean checked".to_owned(),
        ),
        (
            rules(
                "App.Tenant.Store",
                "@[rule] noncomputable def uncomputed : Rule := .neverImports (Classical.choice ⟨App.Shared.Schema⟩)",
            ),
            "uncomputed",
            "line 4, column 27",
            "Lean cannot evaluate it: (interpreter) unknown declaration 'uncomputed'".to_owned(),
        ),
    ] {
        let directory = structured(&[("app/Tenant/Store/Module.lean", module_lean.as_str())]);
        let refused = |file: &str| {
            format!(
                "app/Tenant/Store/{file}.sharp:1:11:error - broken-rule: This file is in App.Tenant.Store, whose structure rules app/Tenant/Store/Module.lean holds, and its rule {rule} at {place} does not run: {reason}."
            )
        };

        let output = compile_lines(directory.path());

        assert_eq!(
            issue_lines(&output),
            [LEDGER_BREAKS.to_owned(), refused("Receipts"), refused("StoreService")],
            "{rule}: {}",
            printed(&output)
        );
    }
}

/// The runner trusts every module under a root Lean or Mago owns: `Init`, `Lean`, `Std`, `Lake`, `Code` and `Sharp`.
/// So a Lean file of the project never builds as one, and a PSR-4 folder under one of them gets no link. Its Lean files
/// cannot be built, which refuses each file that needs one, naming the root, and every other rule still runs. Each row
/// is a project with `App\` in `app/`: the reviewer's helper in a `Code\Evil\` root that reaches `Rule.breaks`, a
/// `Std\Evil\` file that a `Module.lean` imports, and a `Module.lean`, a Lean file nothing imports and a law in a
/// `Code\Evil\` root inside `app/`.
#[test]
fn a_lean_file_under_a_root_lean_or_mago_owns_is_refused_and_every_other_rule_runs() {
    let outside = "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\", \"Code\\\\Evil\\\\\": \"code-evil/\", \"Std\\\\Evil\\\\\": \"std-evil/\"}}}\n";
    let inside = "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\", \"Code\\\\Evil\\\\\": \"app/Evil/\"}}}\n";
    let owned = |owner: &str, root: &str, file: &str| {
        format!(
            "`{owner}` is a name Lean or Mago keeps, so Lean files under the `{root}` root, such as {file}, can't be built. Move them under one of your own namespaces"
        )
    };
    let code = owned("Code", "Code\\Evil\\", "code-evil/Names.lean");
    let std = owned("Std", "Std\\Evil\\", "std-evil/Names.lean");
    let names = "import Sharp.Lean.Facts\nopen Sharp\n\ndef Code.Evil.Names.neverImports (forbidden : String) : Rule :=\n  .imports fun _ i => Rule.breaks (.imports fun ns j => under j.name ns) forbidden {} (.import { i with namespace_ := forbidden })\n";
    let via_code = "import Code.App.Tenant.Store.Module\nimport Code.Evil.Names\nopen Sharp\n\n@[rule] def viaCodeModule : Rule := Code.Evil.Names.neverImports \"App.Shared.Ledgr\"\n@[rule] def viaCodeModuleLegacy : Rule := Code.Evil.Names.neverImports \"App.Shared\"\n";
    let via_std = format!("import Code.App.Tenant.Store.Module\nimport Std.Evil.Names\nopen Sharp\n\n{IMPORTS_ONLY}\n");
    let answer = "def Std.Evil.Names.answer : Nat := 42\n";
    let evil_rules =
        "import Code.Code.Evil.Module\nopen Sharp\n\n@[rule] def importsNothing : Rule := .importsOnly []\n";
    let money = LAWFUL_MONEY.replace("namespace App.Shared;", "namespace Code.Evil;");
    let store_imports = "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule importsOnly in app/Tenant/Store/Module.lean.";
    for (files, refused) in [
        (
            vec![
                ("composer.json", outside),
                ("code-evil/Names.lean", names),
                ("app/Tenant/Store/Module.lean", via_code),
            ],
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 2, column 1", &code),
                store_rejected("StoreService", "line 2, column 1", &code),
            ],
        ),
        (
            vec![
                ("composer.json", outside),
                ("std-evil/Names.lean", answer),
                ("app/Tenant/Store/Module.lean", via_std.as_str()),
            ],
            vec![
                LEDGER_BREAKS.to_owned(),
                store_rejected("Receipts", "line 2, column 1", &std),
                store_rejected("StoreService", "line 2, column 1", &std),
            ],
        ),
        (
            vec![
                ("composer.json", inside),
                ("app/Evil/Thing.sharp", "namespace Code.Evil;\n\npublic class Thing { }\n"),
                ("app/Evil/Module.lean", evil_rules),
                ("app/Evil/Helper.lean", "def Code.Evil.Helper.answer : Nat := 42\n"),
            ],
            vec![
                format!(
                    "app/Evil/Thing.sharp:1:11:error - broken-rule: {}.",
                    owned("Code", "Code\\Evil\\", "app/Evil/Helper.lean")
                ),
                format!(
                    "app/Evil/Thing.sharp:1:11:error - broken-rule: {}.",
                    owned("Code", "Code\\Evil\\", "app/Evil/Module.lean")
                ),
                LEDGER_BREAKS.to_owned(),
                store_imports.to_owned(),
            ],
        ),
        (
            vec![("composer.json", inside), ("app/Evil/Money.sharp", money.as_str())],
            vec![
                format!(
                    "app/Evil/Money.sharp:8:5:error - unproven-law: Law addKeepsCurrency is not proven: {}.",
                    owned("Code", "Code\\Evil\\", "app/Evil/Money.lean")
                ),
                LEDGER_BREAKS.to_owned(),
                store_imports.to_owned(),
            ],
        ),
    ] {
        let directory = structured(&files);

        let output = compile_lines(directory.path());

        assert_eq!(output.status.code(), Some(1), "{files:?}: {}", printed(&output));
        assert_eq!(issue_lines(&output), refused, "{files:?}: {}", printed(&output));
    }
}

/// The namespaces a rule may name are those of the project's `.sharp` files when `mago compile` runs, so a rule that
/// names a namespace an edit deletes is Lean's unknown identifier on the next compile, though no `.sharpc` file the rule
/// judges lists the deleted file as an input. Each row is an edit to a project that compiled.
#[test]
fn a_rule_that_names_a_namespace_an_edit_deletes_is_an_unknown_identifier_on_the_next_compile() {
    let fine = rules("App.Shared", "");
    let plain = "namespace App.Tenant.Store;\n\npublic class StoreService { }\n";
    let renamed = "namespace App.Shared.Schemas;\n\npublic class Entity { }\n";
    let schema = "Unknown identifier `App.Shared.Schema`";
    for (edits, cold) in [
        (vec![("app/Shared/Schema/Entity.sharp", None)], false),
        (vec![("app/Shared/Schema/Entity.sharp", None), ("app/Shared/Schemas/Entity.sharp", Some(renamed))], false),
        (vec![("app/Shared/Schema/Entity.sharp", None)], true),
    ] {
        let directory =
            structured(&[("app/Shared/Module.lean", fine.as_str()), ("app/Tenant/Store/StoreService.sharp", plain)]);
        let root = directory.path();
        compile_succeeds(root);
        for (name, contents) in &edits {
            match contents {
                Some(contents) => write(root, name, contents),
                None => std::fs::remove_file(root.join(name)).unwrap(),
            }
        }
        if cold {
            std::fs::remove_dir_all(root.join(".sharp")).unwrap();
        }

        let output = compile_lines(root);

        assert_eq!(
            issue_lines(&output),
            [
                store_rejected("Receipts", "line 4, column 71", schema),
                store_rejected("StoreService", "line 4, column 71", schema),
            ],
            "{edits:?}, cold {cold}: {}",
            printed(&output)
        );
    }
}

/// Spec section 28.1: "A rule lives with the namespace it constrains. A rule about the whole application goes in
/// `app/Module.lean`." A rule judges only the files under the namespace of its `Module.lean`, which composer.json's
/// PSR-4 map names, as it names a proof file's module: `src/Cart/Module.lean` is `Shop.Cart.Module`.
#[test]
fn a_structure_rule_judges_only_the_files_under_the_namespace_of_its_module_lean() {
    let shared_only = rules("App.Shared", "@[rule] def importsNothing : Rule := .importsOnly []");
    let whole = rules("App", "@[rule] def neverImportsShared : Rule := .neverImports App.Shared");
    let open = rules("App.Tenant.Store", "");
    let fine = rules("App.Shared", "");
    let cart = rules("Shop.Cart", "@[rule] def importsNothing : Rule := .importsOnly []");
    for (files, refused, accepted) in [
        (
            vec![("app/Shared/Module.lean", shared_only.as_str()), ("app/Tenant/Store/Module.lean", open.as_str())],
            "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule importsNothing in app/Shared/Module.lean.",
            "app/Tenant/Store/StoreService.sharp",
        ),
        (
            vec![
                ("app/Module.lean", whole.as_str()),
                ("app/Shared/Module.lean", fine.as_str()),
                ("app/Tenant/Store/Module.lean", open.as_str()),
            ],
            "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule neverImportsShared in app/Module.lean.",
            "app/Shared/Ledger.sharp",
        ),
        (
            vec![
                ("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\", \"Shop\\\\\": \"src/\"}}}\n"),
                ("app/Shared/Module.lean", fine.as_str()),
                ("app/Tenant/Store/Module.lean", open.as_str()),
                ("src/Cart/Module.lean", cart.as_str()),
                (
                    "src/Cart/Basket.sharp",
                    "namespace Shop.Cart;\n\nimport App.Shared.Ledger;\n\npublic class Basket { }\n",
                ),
            ],
            "src/Cart/Basket.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule importsNothing in src/Cart/Module.lean.",
            "app/Tenant/Store/StoreService.sharp",
        ),
    ] {
        let directory = structured(&files);
        let root = directory.path();
        write(
            root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"src\"]\nincludes = [\"vendor\"]\n",
        );

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert_eq!(issue_lines(&output), [refused], "{}", printed(&output));
        assert!(compiled(root, accepted).exists(), "{accepted}");
    }
}

/// A `Module.lean` needs a Lean module name, which the PSR-4 map of composer.json gives it as it gives a proof file one.
/// One in no PSR-4 folder with a namespace refuses each file under its folder, since none of its rules can run.
#[test]
fn a_module_lean_outside_every_psr_4_folder_refuses_each_file_under_its_folder() {
    let shop = rules("Shop", "@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant");
    let cart = "namespace Shop;\n\nimport App.Tenant.Store.Receipts;\n\npublic class Cart\n{\n    public static int refundable(int paid) => Receipts.refundable(paid, 0);\n}\n";
    for composer in [
        "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}}\n",
        "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\", \"\": \"lib/\"}}}\n",
    ] {
        let directory = lawful_project(&[
            ("composer.json", composer),
            ("app/Tenant/Store/Receipts.sharp", RECEIPTS),
            ("lib/Shop/Module.lean", shop.as_str()),
            ("lib/Shop/Cart.sharp", cart),
        ]);
        let root = directory.path();
        write(
            root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"lib\"]\nincludes = [\"vendor\"]\n",
        );

        let output = compile_lines(root);

        assert_eq!(
            issue_lines(&output),
            [
                "lib/Shop/Cart.sharp:1:11:error - broken-rule: lib/Shop/Module.lean is in no PSR-4 folder of composer.json, so its structure rules have no Lean module name and cannot run.",
            ],
            "{composer}: {}",
            printed(&output)
        );
        assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists(), "{composer}");
    }
}

/// The design note's ruling: a `Module.lean` that Lean rejects refuses every `.sharp` file of its namespace, with one
/// issue for each of Lean's errors that names the `Module.lean`, the error's line and column, and Lean's whole message,
/// its lines joined by spaces. A `Module.lean` that
/// imports a rejected one names it and its errors instead, and a `Module.sharp` beside a rejected `Module.lean` gets
/// Lean's error as the proof file of its laws.
#[test]
fn a_module_lean_that_lean_rejects_refuses_every_file_of_its_namespace() {
    let mistyped = rules("App.Shared", "@[rule] def broken : Nat := 1");
    let rejected = rules("App.Tenant", "def allowed : List String := 1");
    let importing = "import Code.App.Shared.Module\nimport App.Tenant.Module\nopen Sharp\n\n@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant\n";
    let open = rules("App.Tenant.Store", "");
    for (files, refused) in [
        (
            vec![("app/Module.lean", "this is not Lean at all\n")],
            vec![
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App, whose structure rules app/Module.lean holds, and Lean rejected app/Module.lean line 1, column 1: unexpected identifier; expected command.",
                "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesTenant in app/Shared/Module.lean.",
                "app/Shared/Schema/Entity.sharp:1:11:error - broken-rule: This file is in App, whose structure rules app/Module.lean holds, and Lean rejected app/Module.lean line 1, column 1: unexpected identifier; expected command.",
                "app/Tenant/Community/Forum.sharp:1:11:error - broken-rule: This file is in App, whose structure rules app/Module.lean holds, and Lean rejected app/Module.lean line 1, column 1: unexpected identifier; expected command.",
                "app/Tenant/Store/Receipts.sharp:1:11:error - broken-rule: This file is in App, whose structure rules app/Module.lean holds, and Lean rejected app/Module.lean line 1, column 1: unexpected identifier; expected command.",
                "app/Tenant/Store/StoreService.sharp:1:11:error - broken-rule: This file is in App, whose structure rules app/Module.lean holds, and Lean rejected app/Module.lean line 1, column 1: unexpected identifier; expected command.",
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule importsOnly in app/Tenant/Store/Module.lean.",
            ],
        ),
        (
            vec![("app/Shared/Module.lean", mistyped.as_str()), ("app/Tenant/Store/Module.lean", open.as_str())],
            vec![
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and Lean rejected app/Shared/Module.lean line 4, column 3: `@[rule]` needs a definition of type `Sharp.Rule`.",
                "app/Shared/Schema/Entity.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and Lean rejected app/Shared/Module.lean line 4, column 3: `@[rule]` needs a definition of type `Sharp.Rule`.",
            ],
        ),
        (
            vec![
                ("app/Tenant/Module.lean", rejected.as_str()),
                ("app/Shared/Module.lean", importing),
                ("app/Tenant/Store/Module.lean", open.as_str()),
            ],
            vec![
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and app/Shared/Module.lean imports app/Tenant/Module.lean, which Lean rejected at line 4, column 30: failed to synthesize instance of type class OfNat (List String) 1 numerals are polymorphic in Lean, but the numeral `1` cannot be used in a context where the expected type is List String due to the absence of the instance above Hint: Type class instance resolution failures can be inspected with the `set_option trace.Meta.synthInstance true` command.",
                "app/Shared/Schema/Entity.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and app/Shared/Module.lean imports app/Tenant/Module.lean, which Lean rejected at line 4, column 30: failed to synthesize instance of type class OfNat (List String) 1 numerals are polymorphic in Lean, but the numeral `1` cannot be used in a context where the expected type is List String due to the absence of the instance above Hint: Type class instance resolution failures can be inspected with the `set_option trace.Meta.synthInstance true` command.",
                "app/Tenant/Community/Forum.sharp:1:11:error - broken-rule: This file is in App.Tenant, whose structure rules app/Tenant/Module.lean holds, and Lean rejected app/Tenant/Module.lean line 4, column 30: failed to synthesize instance of type class OfNat (List String) 1 numerals are polymorphic in Lean, but the numeral `1` cannot be used in a context where the expected type is List String due to the absence of the instance above Hint: Type class instance resolution failures can be inspected with the `set_option trace.Meta.synthInstance true` command.",
                "app/Tenant/Store/Receipts.sharp:1:11:error - broken-rule: This file is in App.Tenant, whose structure rules app/Tenant/Module.lean holds, and Lean rejected app/Tenant/Module.lean line 4, column 30: failed to synthesize instance of type class OfNat (List String) 1 numerals are polymorphic in Lean, but the numeral `1` cannot be used in a context where the expected type is List String due to the absence of the instance above Hint: Type class instance resolution failures can be inspected with the `set_option trace.Meta.synthInstance true` command.",
                "app/Tenant/Store/StoreService.sharp:1:11:error - broken-rule: This file is in App.Tenant, whose structure rules app/Tenant/Module.lean holds, and Lean rejected app/Tenant/Module.lean line 4, column 30: failed to synthesize instance of type class OfNat (List String) 1 numerals are polymorphic in Lean, but the numeral `1` cannot be used in a context where the expected type is List String due to the absence of the instance above Hint: Type class instance resolution failures can be inspected with the `set_option trace.Meta.synthInstance true` command.",
            ],
        ),
    ] {
        let directory = structured(&files);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert_eq!(issue_lines(&output), refused, "{}", printed(&output));
    }

    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? refunded : paid");
    let root = directory.path();
    compile_succeeds(root);
    let proof = read(root, "app/Tenant/Store/Module.lean");
    write(root, "app/Tenant/Store/Module.lean", &format!("{proof}\n@[rule] def broken : Nat := 1\n"));

    let output = compile_lines(root);

    assert_eq!(
        issue_lines(&output),
        [
            "app/Tenant/Store/Module.sharp:3:1:error - unproven-law: Lean rejected app/Tenant/Store/Module.lean line 7: `@[rule]` needs a definition of type `Sharp.Rule`",
            "app/Tenant/Store/Receipts.sharp:1:11:error - broken-rule: This file is in App.Tenant.Store, whose structure rules app/Tenant/Store/Module.lean holds, and Lean rejected app/Tenant/Store/Module.lean line 7, column 3: `@[rule]` needs a definition of type `Sharp.Rule`.",
            "app/Tenant/Store/Refunds.sharp:1:11:error - broken-rule: This file is in App.Tenant.Store, whose structure rules app/Tenant/Store/Module.lean holds, and Lean rejected app/Tenant/Store/Module.lean line 7, column 3: `@[rule]` needs a definition of type `Sharp.Rule`.",
        ],
        "{}",
        printed(&output)
    );
}

/// The design note's decision 3: a rule constrains the namespace of the `Module.lean` that holds it, so a `@[rule]` in
/// any other Lean file is a Lean error at the attribute. In a proof file it refuses the file of the proved laws. In a
/// Lean file that nothing imports it refuses each file of its folder's namespace, and in one a `Module.lean` imports
/// each file that `Module.lean`'s rules judge, naming the file and its error.
#[test]
fn a_structure_rule_outside_a_module_lean_is_refused_at_its_attribute() {
    let rule = "@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant";
    let proof = format!("{MONEY_PROOF}\n{rule}\n");
    let stray = format!("import Code.App.Shared.Rules\nopen Sharp\n\n{rule}\n");
    let attribute = "`@[rule]` belongs in a Module.lean: a structure rule constrains the namespace of the Module.lean that holds it";
    for (files, refused) in [
        (
            vec![("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Shared/Money.lean", proof.as_str())],
            format!(
                "app/Shared/Money.sharp:3:14:error - unproven-law: Lean rejected app/Shared/Money.lean line 7: {attribute}"
            ),
        ),
        (
            vec![("app/Shared/Rules.lean", stray.as_str())],
            format!(
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App.Shared, and Lean rejected app/Shared/Rules.lean line 4, column 3: {attribute}."
            ),
        ),
        (
            vec![
                ("app/Shared/Rules.lean", stray.as_str()),
                ("app/Shared/Module.lean", "import Code.App.Shared.Module\nimport App.Shared.Rules\nopen Sharp\n"),
            ],
            format!(
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and app/Shared/Module.lean imports app/Shared/Rules.lean, which Lean rejected at line 4, column 3: {attribute}."
            ),
        ),
    ] {
        let mut all = vec![("app/Shared/Ledger.sharp", LEDGER), ("app/Tenant/Store/Receipts.sharp", RECEIPTS)];
        all.extend_from_slice(&files);
        let directory = lawful_project(&all);

        let output = compile_lines(directory.path());

        assert_eq!(issue_lines(&output), [refused], "{files:?}: {}", printed(&output));
    }
}

/// Spec section 27: "At deploy, `mago compile` checks every rule exactly." Each `.sharpc` lists the `Module.lean` of
/// its folder and of each folder above it and every workspace Lean file they import, so any edit that changes a rule
/// makes the files it judges stale, and the compile checks every rule again over the facts of every file. A Lean file
/// that no Lean file imports is listed by no `.sharpc`, and the compile checks again when one joins or changes, so a
/// `@[rule]` written there is refused at once. Each row is an edit to a project that compiled, and the next compile
/// refuses exactly the files a compile from nothing refuses.
#[test]
fn compile_checks_every_rule_exactly_after_any_edit() {
    let fine = rules("App.Shared", "");
    let open = rules("App.Tenant.Store", "");
    let reaching = |namespace: &str| {
        rules("App.Shared", &format!("@[rule] def neverReachesTenant : Rule := .neverImports {namespace}"))
    };
    let (community, tenant) = (reaching("App.Tenant.Community"), reaching("App.Tenant"));
    let importing = |module: &str| {
        format!(
            "import Code.App.Shared.Module\nimport {module}\nopen Sharp\n\n@[rule] def neverReachesForbidden : Rule := .neverImports forbidden\n"
        )
    };
    let (names, lib) = (importing("App.Lib.Names"), importing("App.Lib.Module"));
    let forbidden = |namespace: &str| format!("def forbidden : Namespace := {namespace}");
    let (lib_community, lib_tenant) =
        (rules("App.Lib", &forbidden("App.Tenant.Community")), rules("App.Lib", &forbidden("App.Tenant")));
    let names_file = |namespace: &str| format!("import Code.App.Lib.Names\nopen Sharp\n\n{}\n", forbidden(namespace));
    let (names_community, names_tenant) = (names_file("App.Tenant.Community"), names_file("App.Tenant"));
    let whole = rules("App", "@[rule] def neverImportsShared : Rule := .neverImports App.Shared");
    let cycles = rules("App", "@[rule] def noImportCycles : Rule := .noImportCycles");
    let store = rules("App.Tenant", "@[rule] def neverReachesStore : Rule := .neverImports App.Tenant.Store");
    let moved = LEDGER.replace("namespace App.Shared;", "namespace App.Tenant;");
    let plain = "namespace App.Tenant.Store;\n\npublic class StoreService { }\n";
    let class = |namespace: &str, name: &str, import: &str| {
        format!("namespace {namespace};\n\n{import}public class {name} {{ }}\n")
    };
    let (x, y, z) = (
        class("App.A", "X", "import App.B.Y;\n\n"),
        class("App.B", "Y", "import App.C.Z;\n\n"),
        class("App.C", "Z", ""),
    );
    let closing = class("App.C", "Z", "import App.A.X;\n\n");
    let forbidding = "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesForbidden in app/Shared/Module.lean.";
    let unreached = |declaration: &str| format!("import Code.App.Shared.Rules\nopen Sharp\n\n{declaration}\n");
    let (helper, straying) = (
        unreached("def helper : Nat := 1"),
        unreached("@[rule] def neverReachesTenant : Rule := .neverImports App.Tenant"),
    );
    let refused_at_attribute = |file: &str| {
        format!(
            "{file}:1:11:error - broken-rule: This file is in App.Shared, and Lean rejected app/Shared/Rules.lean line 4, column 3: `@[rule]` belongs in a Module.lean: a structure rule constrains the namespace of the Module.lean that holds it."
        )
    };
    let attributes =
        [refused_at_attribute("app/Shared/Ledger.sharp"), refused_at_attribute("app/Shared/Schema/Entity.sharp")];
    let attributes: Vec<&str> = attributes.iter().map(String::as_str).collect();
    for (files, edits, refused) in [
        (
            vec![("app/Shared/Module.lean", fine.as_str())],
            vec![("app/Module.lean", Some(whole.as_str()))],
            vec![
                "app/Tenant/Store/StoreService.sharp:3:8:error - broken-rule: This import of App.Shared.Ledger breaks the structure rule neverImportsShared in app/Module.lean.",
            ],
        ),
        (
            vec![("app/Shared/Module.lean", community.as_str())],
            vec![("app/Shared/Module.lean", Some(tenant.as_str()))],
            vec![
                "app/Shared/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesTenant in app/Shared/Module.lean.",
            ],
        ),
        (
            vec![("app/Shared/Module.lean", names.as_str()), ("app/Lib/Names.lean", names_community.as_str())],
            vec![("app/Lib/Names.lean", Some(names_tenant.as_str()))],
            vec![forbidding],
        ),
        (
            vec![("app/Shared/Module.lean", lib.as_str()), ("app/Lib/Module.lean", lib_community.as_str())],
            vec![("app/Lib/Module.lean", Some(lib_tenant.as_str()))],
            vec![forbidding],
        ),
        (
            vec![("app/Shared/Module.lean", lib.as_str()), ("app/Lib/Module.lean", lib_community.as_str())],
            vec![("app/Lib/Module.lean", None)],
            vec![
                "app/Shared/Ledger.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and Lean rejected app/Shared/Module.lean line 2, column 1: bad import App.Lib.Module: app/Lib/Module.lean does not exist.",
                "app/Shared/Schema/Entity.sharp:1:11:error - broken-rule: This file is in App.Shared, whose structure rules app/Shared/Module.lean holds, and Lean rejected app/Shared/Module.lean line 2, column 1: bad import App.Lib.Module: app/Lib/Module.lean does not exist.",
            ],
        ),
        (
            vec![
                ("app/Shared/Module.lean", fine.as_str()),
                ("app/Tenant/Store/StoreService.sharp", plain),
                ("app/Module.lean", cycles.as_str()),
                ("app/A/X.sharp", x.as_str()),
                ("app/B/Y.sharp", y.as_str()),
                ("app/C/Z.sharp", z.as_str()),
            ],
            vec![("app/C/Z.sharp", Some(closing.as_str()))],
            vec![
                "app/A/X.sharp:3:8:error - broken-rule: This import of App.B.Y breaks the structure rule noImportCycles in app/Module.lean.",
                "app/B/Y.sharp:3:8:error - broken-rule: This import of App.C.Z breaks the structure rule noImportCycles in app/Module.lean.",
                "app/C/Z.sharp:3:8:error - broken-rule: This import of App.A.X breaks the structure rule noImportCycles in app/Module.lean.",
            ],
        ),
        (
            vec![
                ("app/Shared/Module.lean", fine.as_str()),
                ("app/Tenant/Store/StoreService.sharp", plain),
                ("app/Tenant/Module.lean", store.as_str()),
            ],
            vec![("app/Shared/Ledger.sharp", None), ("app/Tenant/Ledger.sharp", Some(moved.as_str()))],
            vec![
                "app/Tenant/Ledger.sharp:3:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesStore in app/Tenant/Module.lean.",
            ],
        ),
        (
            vec![("app/Shared/Module.lean", fine.as_str())],
            vec![("app/Shared/Rules.lean", Some(straying.as_str()))],
            attributes.clone(),
        ),
        (
            vec![("app/Shared/Module.lean", fine.as_str()), ("app/Shared/Rules.lean", helper.as_str())],
            vec![("app/Shared/Rules.lean", Some(straying.as_str()))],
            attributes,
        ),
    ] {
        let mut all = vec![("app/Tenant/Store/Module.lean", open.as_str())];
        all.extend_from_slice(&files);
        let directory = structured(&all);
        let root = directory.path();
        compile_succeeds(root);
        for (name, contents) in &edits {
            match contents {
                Some(contents) => write(root, name, contents),
                None => std::fs::remove_file(root.join(name)).unwrap(),
            }
        }

        let output = compile_lines(root);

        assert_eq!(issue_lines(&output), refused, "{edits:?}: {}", printed(&output));
        for line in &refused {
            let name = line.split(':').next().unwrap();
            assert!(!compiled(root, name).exists(), "{edits:?}: {name}");
        }
    }
}

/// A compile that finds every `.sharpc` current starts no Lean process, as for laws, so a project with structure rules
/// compiles again with no `lake` on the `PATH`.
#[test]
fn a_second_compile_of_a_project_with_structure_rules_starts_no_lean_process() {
    let fine = rules("App.Shared", "");
    let open = rules("App.Tenant.Store", "");
    let directory =
        structured(&[("app/Shared/Module.lean", fine.as_str()), ("app/Tenant/Store/Module.lean", open.as_str())]);
    let root = directory.path();
    compile_succeeds(root);

    let output = compile_without_lean(root);

    assert!(output.status.success(), "the compile needed lake: {}", printed(&output));
    assert!(compiled(root, "app/Shared/Ledger.sharp").exists());
}

/// The runner reads a request of thousands of facts whole, more than one read of its pipe holds, so a rule judges the
/// last fact of a file too.
#[test]
fn compile_checks_a_structure_rule_over_thousands_of_imports() {
    let imports: String = (0..4072).map(|index| format!("import Vendor.Package.Class{index};\n")).collect();
    let ledger =
        LEDGER.replace("import App.Tenant.Store.Receipts;\n", &format!("{imports}import App.Tenant.Store.Receipts;\n"));
    let directory = structured(&[("app/Shared/Ledger.sharp", ledger.as_str())]);
    let root = directory.path();

    let output = compile_lines(root);

    assert!(
        issue_lines(&output).contains(&"app/Shared/Ledger.sharp:4075:8:error - broken-rule: This import of App.Tenant.Store.Receipts breaks the structure rule neverReachesTenant in app/Shared/Module.lean.".to_owned()),
        "{}",
        printed(&output)
    );
}

#[test]
fn compile_proves_a_law_over_locals_an_if_a_nullable_and_strings() {
    let directory = lawful_project(&[(
        "app/Shared/Label.sharp",
        "namespace App.Shared;\n\npublic class Label\n{\n    public static string tag(string? name, bool loud)\n    {\n        let text = name ?? \"item\";\n        if (loud) {\n            text = text + \"!\";\n        }\n        return text;\n    }\n\n    law quietKeepsName(string name) => Label.tag(name, false) == name;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Label.sharp").exists());
}

#[test]
fn compile_refuses_a_package_law_with_no_proof_and_writes_nothing_under_vendor() {
    let directory = lawful_project(&[
        (
            "vendor/acme/money/composer.json",
            "{\n    \"autoload\": {\n        \"psr-4\": {\n            \"Acme\\\\\": \"src/\"\n        }\n    }\n}\n",
        ),
        ("vendor/acme/money/src/Money.sharp", &LAWFUL_MONEY.replace("namespace App.Shared;", "namespace Acme;")),
    ]);
    let root = directory.path();
    let vendor = files_under(&root.join("vendor"));

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("vendor/acme/money/src/Money.sharp:8:5:error - unproven-law: Law addKeepsCurrency has no proof in vendor/acme/money/src/Money.lean. Its package ships no proof for it, so report it to the package's maintainers."),
        "{}",
        printed(&output)
    );
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
}

#[test]
fn compile_refuses_an_unproven_law_whatever_pragma_ignore_entry_or_baseline_entry_targets_it() {
    let broken = |pragma: &str| {
        LAWFUL_MONEY
            .replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency")
            .replace("    law ", &format!("    {pragma}\n    law "))
    };
    let workspace = |pragma: &str, analyzer: &str| {
        let directory = lawful_project(&[("app/Shared/Money.sharp", &broken(pragma))]);
        write(
            directory.path(),
            "mago.toml",
            &format!("php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n\n[analyzer]\n{analyzer}\n"),
        );
        directory
    };
    let baselined = workspace("", "baseline = \"baseline.toml\"");
    write(
        baselined.path(),
        "baseline.toml",
        "variant = \"strict\"\n\n[[entries.\"app/Shared/Money.sharp\".issues]]\ncode = \"unproven-law\"\nstart_line = 8\nend_line = 8\n",
    );

    for (suppression, directory) in [
        ("a pragma", workspace("// @mago-expect analysis:unproven-law", "")),
        ("an ignore entry", workspace("", "ignore = [\"unproven-law\"]")),
        ("a baseline entry", baselined),
    ] {
        let output = compile_lines(directory.path());

        assert_eq!(output.status.code(), Some(1), "{suppression}: {}", printed(&output));
        assert!(
            printed(&output).contains("error - unproven-law: Law addKeepsCurrency"),
            "{suppression}: {}",
            printed(&output)
        );
        assert!(!compiled(directory.path(), "app/Shared/Money.sharp").exists(), "{suppression}");
    }
}

/// Compiles `source` as `app/Shared/Shape.sharp` and checks that the compile refuses it with the unmodeled-construct
/// error `message`, before any Lean process starts.
fn assert_unmodeled(source: &str, message: &str) {
    let directory = lawful_project(&[("app/Shared/Shape.sharp", source)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains(&format!("error - unmodeled-construct: {message}")), "{}", printed(&output));
    assert!(!compiled(root, "app/Shared/Shape.sharp").exists());
}

#[test]
fn a_law_reaching_a_float_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int cents { get; }) { }\n    public float ratio() => this.cents / 2.0;\n\n    law ratioBelowOne(Shape p) => p.ratio() < 100.0;\n}\n",
        "Law ratioBelowOne reaches a float in Shape.ratio. Lean cannot prove facts about float arithmetic; use int.",
    );
}

#[test]
fn a_law_reaching_an_overridable_method_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public virtual int sides() => 3;\n\n    law hasSides(Shape s) => s.sides() > 0;\n}\n",
        "Law hasSides reaches a call to the overridable method Shape.sides in Shape.hasSides. Lean cannot see which implementation runs.",
    );
}

#[test]
fn a_law_testing_the_identity_of_objects_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law same(Shape a, Shape b) => a === b;\n}\n",
        "Law same reaches the identity test `===` in Shape.same. Classes keep no identity in Lean.",
    );
}

#[test]
fn a_law_reaching_a_try_catch_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\nimport DivisionByZeroError;\n\npublic class Shape\n{\n    public static int share(int a)\n    {\n        try {\n            return 10 / a;\n        } catch (DivisionByZeroError e) {\n            return 0;\n        }\n    }\n\n    law shared(int a) => Shape.share(a) >= 0;\n}\n",
        "Law shared reaches a try/catch in Shape.share. An exception is a bug, and a law reasons about values.",
    );
}

#[test]
fn a_law_reaching_a_type_test_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static bool whole(Any? value) => value is int;\n\n    law wholeInt(int a) => Shape.whole(a);\n}\n",
        "Law wholeInt reaches Any in Shape.whole. It reads types while the code runs.",
    );
}

#[test]
fn a_law_reaching_a_member_lean_reserves_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public int rec() => 1;\n\n    law one(Shape s) => s.rec() == 1;\n}\n",
        "Law one reaches the method Shape.rec in Shape.rec. Lean declares a member of that name for every structure and inductive.",
    );
}

#[test]
fn a_law_reaching_a_collection_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law sized(List<int> items) => true;\n}\n",
        "Law sized reaches a collection in Shape.sized. Mago does not translate collections to Lean.",
    );
}

#[test]
fn a_law_reaching_a_loop_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int sum(int n)\n    {\n        let total = 0;\n        for (let i = 0; i < n; i++) {\n            total += i;\n        }\n        return total;\n    }\n\n    law summed(int n) => Shape.sum(n) >= 0;\n}\n",
        "Law summed reaches a loop in Shape.sum. Mago does not translate loops to Lean.",
    );
}

#[test]
fn a_law_reaching_a_recursion_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int down(int n) => n <= 0 ? 0 : Shape.down(n - 1);\n\n    law reachesZero(int n) => Shape.down(n) == 0;\n}\n",
        "Law reachesZero reaches a recursion in Shape.down. Mago does not translate recursion to Lean.",
    );
}

#[test]
fn a_law_reaching_a_lambda_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int next(int a)\n    {\n        const step = (int x) => x + 1;\n        return step(a);\n    }\n\n    law grows(int a) => Shape.next(a) != a;\n}\n",
        "Law grows reaches a lambda in Shape.next. Mago does not translate lambdas to Lean.",
    );
}

#[test]
fn a_law_reaching_an_inherited_class_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Base\n{\n}\n\npublic class Shape : Base\n{\n    law any(Shape s) => true;\n}\n",
        "Law any reaches the class Shape, which extends Base in Shape. Mago does not translate inheritance to Lean.",
    );
}

#[test]
fn a_law_reaching_a_named_constructor_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int sides { get; }) { }\n    public Shape.triangle() : this(3) { }\n\n    law threeSides(int unused) => new Shape.triangle().sides == 3;\n}\n",
        "Law threeSides reaches the named constructor Shape.triangle in Shape.threeSides. Mago does not translate constructor bodies to Lean.",
    );
}
