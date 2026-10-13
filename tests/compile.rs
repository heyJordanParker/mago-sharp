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

/// `mago compile` with no `lake`, nor any other program, on the `PATH`.
fn compile_without_lean(root: &Path) -> Output {
    let empty = tempfile::tempdir().unwrap();
    command(root, "never").env("MAGO_REPORTING_FORMAT", "emacs").env("PATH", empty.path()).output().unwrap()
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

#[test]
fn compile_reports_the_proof_of_a_deleted_law_on_the_class_name() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &LAWFUL_MONEY.replace("\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n", ""),
    );
    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Money.sharp:3:14:error - non-existent-law: app/Shared/Money.lean line 4 proves App.Shared.Money.addKeepsCurrency, which Money.sharp no longer states. Delete the proof, or restore the law."),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Money.sharp").exists());
}

#[test]
fn compile_reports_missing_lean_on_a_law_file_whose_compiled_file_is_not_current() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Order.sharp", ORDER)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Shared/Money.sharp:3:14:error - missing-lean: Laws and structure rules need Lean 4.34.1, and lake was not found. Install elan from https://lean-lang.org/install, then run vendor/bin/mago compile again."),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    assert!(compiled(root, "app/Order.sharp").exists());
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
