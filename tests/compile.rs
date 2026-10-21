//! Runs `mago compile` on a project that mixes PHP# and PHP files, with a PHP# package in `vendor/`.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;

use mago_sharp_bridge::unit::Library;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::is_stale;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_bridge::unit::stamp;
use mago_sharp_lean::PACKAGE_FOLDER;

mod common;

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
    project_in(directory.path(), order, money);
    directory
}

/// Writes the files of `project` into `root`.
fn project_in(root: &Path, order: &str, money: &str) {
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(root, "composer.lock", "{}\n");
    write(root, "app/Order.sharp", order);
    write(root, "app/Legacy.php", LEGACY);
    write(root, MONEY_PATH, money);
}

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// `mago compile` in `root`, which prints its issues in the medium format wherever it runs: mago would pick another
/// one under GitHub Actions or GitLab CI.
fn command(root: &Path, colors: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
    command
        .args(["--no-version-check", "--colors", colors, "compile"])
        .env("MAGO_LOG", "info")
        .env("MAGO_REPORTING_FORMAT", "medium")
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
        [Path::new("app/Order.sharpc"), Path::new("listeners.php"), Path::new("vendor/acme/money/src/Money.sharpc")]
    );
    for (name, source) in [("app/Order.sharp", ORDER), (MONEY_PATH, MONEY)] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        let header = header(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(header.source_hash, source_hash(source.as_bytes()), "{name}");
    }
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
    assert!(printed(&output).contains("Compiled 2 PHP# files into .sharp/."), "{}", printed(&output));
}

/// Each compiled file names the standard library the project installs: the native fingerprint its package's
/// `native.php` gives and the version Composer's `installed.json` lists, which the engine compares and names. A
/// library with no `native.php` declares no native bodies, and one whose `native.php` gives no fingerprint fails the
/// compile.
#[test]
fn compile_names_the_installed_standard_library_in_each_compiled_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let library = "vendor/heyjordanparker/php-sharp-composer";
    write(root, &format!("{library}/composer.json"), "{\"name\": \"heyjordanparker/php-sharp-composer\"}\n");
    write(
        root,
        &format!("{library}/library/Sharp/Math/Math.sharp"),
        "namespace Sharp.Math;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
    );
    write(
        root,
        "vendor/composer/installed.json",
        "{\"packages\": [{\"name\": \"heyjordanparker/php-sharp-composer\", \"version\": \"v0.3.2\"}]}\n",
    );
    let named = |root: &Path| header(&std::fs::read(compiled(root, "app/Order.sharp")).unwrap()).unwrap().library;

    write(
        root,
        &format!("{library}/native.php"),
        "<?php\n\nreturn [\n    'fingerprint' => '00112233445566778899aabbccddeeff',\n    'bodies' => [],\n];\n",
    );
    compile_succeeds(root);
    assert_eq!(
        named(root),
        Library {
            native_fingerprint: [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff
            ],
            version: b"v0.3.2".to_vec(),
        }
    );

    std::fs::remove_file(root.join(library).join("native.php")).unwrap();
    compile_succeeds(root);
    assert_eq!(named(root), Library::default());

    write(root, &format!("{library}/native.php"), "<?php\n\nreturn [];\n");
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "{library}/native.php gives no native fingerprint. Reinstall heyjordanparker/php-sharp-composer."
        )),
        "{}",
        printed(&output)
    );
}

/// A dangling link named like a source, as Emacs leaves beside a buffer with unsaved changes, is no source: the loader
/// skips it, and so does the listing of the folder of a file whose code reaches an extension, spec section 26.
#[cfg(unix)]
#[test]
fn compile_skips_a_dangling_link_beside_a_file_that_reaches_an_extension() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Shout.sharp",
        "namespace App;\n\npublic static class Shout\n{\n    extension(string text)\n    {\n        public string loud() => text;\n    }\n}\n",
    );
    write(
        root,
        "app/Report.sharp",
        "namespace App;\n\npublic class Report\n{\n    public string text(string name) => name.loud();\n}\n",
    );
    std::os::unix::fs::symlink("dev@laptop.4242:1760000000", root.join("app/.#Report.sharp")).unwrap();

    compile_succeeds(root);
    assert!(compiled(root, "app/Report.sharp").exists());
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

/// A PHP# interface with a default body, and a plain PHP class whose method returns more than the interface allows.
const KEYED: &str = "namespace App;\n\npublic interface Keyed\n{\n    int getKey();\n\n    string label() => `record:${this.getKey()}`;\n}\n";
const RECORD: &str = "<?php\n\nnamespace App;\n\nclass Record\n{\n    public function getKey(): int|string|null\n    {\n        return 7;\n    }\n}\n";

/// The engine refuses a plain PHP class that extends a PHP# struct when it links it, so the compile refuses that class
/// first. A PHP# method that is not `virtual` is closed to every class the compile sees, static or not, so the
/// compile refuses a plain PHP class that replaces it too, though the engine runs it as an ordinary method for the test
/// doubles tests generate. The compile links a class that replaces a `virtual` method.
#[test]
fn compile_refuses_a_plain_php_class_that_extends_a_struct_or_replaces_a_closed_method() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Base.sharp",
        "namespace App;\n\npublic class Base\n{\n    public Base() { }\n\n    public void save() { }\n\n    public virtual string title() => \"base\";\n\n    public static string kind() => \"base\";\n}\n",
    );
    write(root, "app/Point.sharp", "namespace App;\n\npublic struct Point\n{\n    public int x { get; init; }\n}\n");
    write(root, "app/ExtendsStruct.php", "<?php\n\nnamespace App;\n\nclass ExtendsStruct extends Point\n{\n}\n");
    write(
        root,
        "app/StaticToInstance.php",
        "<?php\n\nnamespace App;\n\nclass StaticToInstance extends Base\n{\n    public function kind(): string\n    {\n        return 'instance';\n    }\n}\n",
    );
    write(
        root,
        "app/OverridesClosed.php",
        "<?php\n\nnamespace App;\n\nclass OverridesClosed extends Base\n{\n    public function save(): void\n    {\n    }\n}\n",
    );
    write(
        root,
        "app/OverridesVirtual.php",
        "<?php\n\nnamespace App;\n\nclass OverridesVirtual extends Base\n{\n    public function title(): string\n    {\n        return 'virtual';\n    }\n}\n",
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("`App\\Point` is a struct, which can't be inherited."), "{}", printed(&output));
    assert!(printed(&output).contains("`App\\Base::save()` is not `virtual`"), "{}", printed(&output));
    assert!(printed(&output).contains("`App\\Base::kind()` is not `virtual`"), "{}", printed(&output));
    assert!(!printed(&output).contains("OverridesVirtual"), "{}", printed(&output));
    assert!(printed(&output).contains("3 plain PHP files were refused"), "{}", printed(&output));
    assert!(compiled(root, "app/Base.sharp").exists());
}

/// A plain PHP class that implements a PHP# interface with default bodies needs its `Defaults` trait, what it inherits
/// must fit the interface, and a parameter must take what the interface's parameter erases to, spec section 22, or the
/// engine refuses the class when it links it. The compile refuses each, and still writes every accepted PHP# file.
#[test]
fn compile_refuses_a_plain_php_class_that_does_not_satisfy_the_php_sharp_interface_it_implements() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Keyed.sharp", KEYED);
    write(root, "app/Record.php", RECORD);
    write(root, "app/Bare.php", "<?php\n\nnamespace App;\n\nclass Bare extends Record implements Keyed\n{\n}\n");
    write(root, "app/Box.sharp", "namespace App;\n\npublic interface Box<T>\n{\n    void put(T item);\n}\n");
    let int_box = |parameter: &str| {
        format!(
            "<?php\n\nnamespace App;\n\n/** @implements Box<int> */\nclass IntBox implements Box\n{{\n    public function put({parameter} $item): void\n    {{\n    }}\n}}\n"
        )
    };
    write(root, "app/IntBox.php", &int_box("int"));

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("`App\\Bare` implements `App\\Keyed` without its default bodies"),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains("incompatible-return-type"), "{}", printed(&output));
    assert!(
        printed(&output).contains("Parameter `$item` of `App\\IntBox::put()` must take at least `mixed`"),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains("2 plain PHP files were refused"), "{}", printed(&output));
    assert!(compiled(root, "app/Keyed.sharp").exists());

    write(
        root,
        "app/Bare.php",
        "<?php\n\nnamespace App;\n\nclass Bare extends Record implements Keyed\n{\n    use Keyed\\Defaults;\n\n    public function getKey(): int\n    {\n        return 7;\n    }\n}\n",
    );
    write(root, "app/IntBox.php", &int_box("mixed"));
    compile_succeeds(root);
}

/// The engine links a plain PHP class against the PHP# types it extends whatever declares each method the class takes:
/// the class, a trait of the project, or a class or trait of a package in `vendor/`, as Eloquent's `Model` gives
/// `unsetRelation()` from its `HasRelationships` trait. Each PHP# call of a PHP# interface's member checks its result,
/// spec section 22, so an untyped method implements a typed member from each of them. A PHP# class's method checks none,
/// so the compile refuses an untyped method under a typed one from each of them, naming its type as PHP declares it.
#[test]
fn compile_takes_an_untyped_method_under_an_interface_member_and_refuses_one_under_a_class_method_wherever_declared() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let untyped = |namespace: &str, name: &str| {
        format!(
            "<?php\n\nnamespace {namespace};\n\ntrait {name}\n{{\n    public function forget($relation)\n    {{\n        return $this;\n    }}\n\n    public function getMorphClass()\n    {{\n        return 'row';\n    }}\n}}\n"
        )
    };
    write(root, "app/ForgetsInApp.php", &untyped("App", "ForgetsInApp"));
    write(
        root,
        "app/AppRow.php",
        "<?php\n\nnamespace App;\n\nclass AppRow implements Relates\n{\n    use ForgetsInApp;\n}\n",
    );
    write(root, "vendor/acme/model/src/ForgetsRelations.php", &untyped("Acme", "ForgetsRelations"));
    write(
        root,
        "vendor/acme/model/src/Model.php",
        "<?php\n\nnamespace Acme;\n\nabstract class Model\n{\n    use ForgetsRelations;\n}\n",
    );
    write(
        root,
        "app/VendorRow.php",
        "<?php\n\nnamespace App;\n\nclass VendorRow extends \\Acme\\Model implements Relates\n{\n}\n",
    );
    write(
        root,
        "app/Relates.sharp",
        "namespace App;\n\npublic interface Relates\n{\n    Self forget(string relation);\n\n    string getMorphClass();\n}\n",
    );

    compile_succeeds(root);

    write(
        root,
        "app/Record.sharp",
        "namespace App;\n\npublic abstract class Record\n{\n    public abstract Self forget(string relation);\n}\n",
    );
    write(
        root,
        "app/AppRecord.php",
        "<?php\n\nnamespace App;\n\nclass AppRecord extends Record\n{\n    use ForgetsInApp;\n}\n",
    );
    write(
        root,
        "app/VendorRecord.php",
        "<?php\n\nnamespace App;\n\nclass VendorRecord extends Record\n{\n    use \\Acme\\ForgetsRelations;\n}\n",
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for class in ["AppRecord", "VendorRecord"] {
        assert!(
            printed(&output).contains(&format!(
                "`App\\{class}::forget()` must declare a return type compatible with `static` declared by `App\\Record::forget()`"
            )),
            "{}",
            printed(&output)
        );
    }
    assert!(printed(&output).contains("2 plain PHP files were refused"), "{}", printed(&output));
}

/// The engine erases a PHP# class's type parameters and reads no docblock, so it links a plain PHP class that implements
/// a generic PHP# interface without the type argument an `@implements` tag would give it. The compile links it too, and
/// the analysis keeps its error on the missing type argument.
#[test]
fn compile_links_a_plain_php_class_that_gives_a_php_sharp_interface_no_type_argument() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Morphs.sharp", "namespace App;\n\npublic interface Morphs<T>\n{\n    T morph();\n}\n");
    write(
        root,
        "app/Bare.php",
        "<?php\n\nnamespace App;\n\nclass Bare implements Morphs\n{\n    public function morph()\n    {\n        return 'row';\n    }\n}\n",
    );

    compile_succeeds(root);
}

/// The compile checks a plain PHP file only where its class meets a PHP# type, as kotlinc checks a Java type only
/// where it meets a Kotlin declaration. Every other error there stays `mago analyze`'s, as in a plain PHP project.
#[test]
fn compile_leaves_every_other_error_in_a_plain_php_file_to_mago_analyze() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Keyed.sharp", KEYED);
    write(
        root,
        "app/Sorting.php",
        "<?php\n\nnamespace App;\n\nfunction sorted(): array\n{\n    ksort($prices);\n\n    return $prices;\n}\n",
    );

    let analyzed = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze"])
        .current_dir(root)
        .output()
        .unwrap();

    assert_eq!(analyzed.status.code(), Some(1), "{}", printed(&analyzed));
    assert!(printed(&analyzed).contains("app/Sorting.php"), "{}", printed(&analyzed));
    compile_succeeds(root);
}

/// An extension host that reads the compiled file of `app/Word.sharp` its engine would load, from the folder mago names
/// in `SHARP_COMPILED`, as its hooks run, and appends which word the file holds to `host.log`, as a host that loads a
/// project class runs that file.
const HOST_READING_THE_COMPILED_FILE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class ReadsTheCompiledFile implements Plugin, BeforeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('reads-the-compiled-file', 'Reads the compiled file', 'Reads app/Word.sharp compiled.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $compiled = @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');
        $seen = match (true) {
            $compiled === false => 'nothing',
            str_contains($compiled, 'second') => 'second',
            str_contains($compiled, 'first') => 'first',
        };
        file_put_contents('host.log', "{$seen}\n", FILE_APPEND);
    }
}

(new Worker(new Extension('reader', 'Reader', '0.0.0', analyzerPlugins: [new ReadsTheCompiledFile()])))->run();
"#;

/// A Roslyn source generator sees the compilation's symbols, and a Kotlin compiler plugin sees the module's own
/// classes, so a host sees each PHP# file compiled from its current source, on a fresh tree and after an edit. The
/// hosts' compile lives outside the project, so `mago analyze` leaves the project's `.sharp` folder as `mago compile`
/// wrote it.
#[test]
fn a_host_sees_each_php_sharp_file_compiled_from_its_current_source() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let word = |word: &str| {
        format!("namespace App;\n\npublic class Word\n{{\n    public static string text() => \"{word}\";\n}}\n")
    };
    write(root, "app/Word.sharp", &word("first"));
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    compile_succeeds(root);
    let compiled_by_compile = std::fs::read(compiled(root, "app/Word.sharp")).unwrap();
    write(root, "app/Word.sharp", &word("second"));
    let analyzed = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze"])
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root)
        .output()
        .unwrap();

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\nsecond\n");
    assert_eq!(std::fs::read(compiled(root, "app/Word.sharp")).unwrap(), compiled_by_compile);
}

/// An extension host whose linter rule appends the word the compiled `app/Word.sharp` holds to `host.log` as it lints
/// a class, as a rule that instantiates a project's attributes loads that file.
const LINT_HOST_READING_THE_COMPILED_FILE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Extension;
use Mago\Sdk\Linter\LintContext;
use Mago\Sdk\Linter\Rule;
use Mago\Sdk\Linter\RuleDefinition;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;

require $argv[1];

final class ReadsTheCompiledFile implements Rule
{
    public function getDefinition(): RuleDefinition
    {
        return new RuleDefinition(
            code: 'reader/reads-the-compiled-file',
            name: 'Reads the compiled file',
            description: 'Reads app/Word.sharp compiled.',
            defaultLevel: Level::Warning,
            defaultEnabled: true,
            targets: [NodeKind::Class_],
        );
    }

    public function lint(LintContext $context): void
    {
        $compiled = (string) @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');
        file_put_contents('host.log', (str_contains($compiled, 'first') ? 'first' : 'nothing') . "\n", FILE_APPEND);
    }
}

(new Worker(new Extension('reader', 'Reader', '0.0.0', linterRules: [new ReadsTheCompiledFile()])))->run();
"#;

/// `mago lint` starts its hosts as `mago analyze` does, so a linter host loads the project's classes on a fresh tree.
#[test]
fn a_linter_host_sees_each_php_sharp_file_compiled_on_a_fresh_tree() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the linter host compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Word.sharp",
        "namespace App;\n\npublic class Word\n{\n    public static string text() => \"first\";\n}\n",
    );
    write(root, "app/Plain.php", "<?php\n\nnamespace App;\n\nfinal class Plain {}\n");
    write(root, "tools/host.php", LINT_HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    let linted = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "lint"])
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root)
        .output()
        .unwrap();

    assert!(linted.status.success(), "{}", printed(&linted));
    let log = std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    assert!(!log.is_empty() && log.lines().all(|line| line == "first"), "{log}{}", printed(&linted));
    assert!(!root.join(".sharp").exists());
}

/// An extension host that appends its process id and the word the compiled `app/Word.sharp` holds to `host.log` as
/// each analysis starts.
const HOST_LOGGING_ITS_PROCESS: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class LogsItsProcess implements Plugin, BeforeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('logs-its-process', 'Logs its process', 'Logs its process id.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $compiled = (string) @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');
        $word = str_contains($compiled, 'second') ? 'second' : (str_contains($compiled, 'first') ? 'first' : 'nothing');
        file_put_contents('host.log', getmypid() . " {$word}\n", FILE_APPEND);
    }
}

(new Worker(new Extension('logger', 'Logger', '0.0.0', analyzerPlugins: [new LogsItsProcess()])))->run();
"#;

/// A host's worker keeps each class it loaded, as PHP declares a class once, so `mago analyze --watch` starts the
/// hosts' workers again when a PHP# file they load changes, as a dev server restarts a worker when a module it loaded
/// changes.
#[test]
fn a_watch_starts_the_hosts_again_when_a_php_sharp_file_changes() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host watch test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let word = |word: &str| {
        format!("namespace App;\n\npublic class Word\n{{\n    public static string text() => \"{word}\";\n}}\n")
    };
    write(root, "app/Word.sharp", &word("first"));
    write(root, "tools/host.php", HOST_LOGGING_ITS_PROCESS);
    write(
        root,
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    let wait_for_lines = |lines: usize| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while log().lines().count() < lines && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };

    let mut watch = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    wait_for_lines(1);
    write(root, "app/Word.sharp", &word("second"));
    wait_for_lines(2);
    watch.kill().unwrap();
    watch.wait().unwrap();

    let log = log();
    let runs: Vec<(&str, &str)> = log.lines().filter_map(|line| line.split_once(' ')).collect();
    assert!(runs.len() >= 2, "{log}");
    assert_eq!(runs[0].1, "first", "{log}");
    assert_eq!(runs[1].1, "second", "{log}");
    assert_ne!(runs[0].0, runs[1].0, "{log}");
}

/// A host that types the project's code, as one that gives a plain PHP helper its return type does, can load the
/// classes only its typing makes correct: the hosts' compile lowers every file that lowers, whatever the analysis
/// without the hosts says of it, as Roslyn hands an analyzer a compilation with errors and `tsc` emits despite them.
/// The host here fails its hook unless the compiled file of `app/Greeting.sharp` is there.
#[test]
fn a_typing_host_loads_a_class_only_its_typing_makes_correct_on_a_fresh_tree() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the typing host compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Greeting.sharp",
        "namespace App;\n\nimport App.Helpers;\n\npublic class Greeting\n{\n    public static string hello() => Helpers.untyped();\n}\n",
    );
    write(
        root,
        "app/Helpers/helpers.php",
        "<?php\n\nnamespace App\\Helpers;\n\nfunction untyped()\n{\n    return 'typed by the host';\n}\n",
    );
    write(root, "tools/host.php", TYPING_HOST);
    write(
        root,
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.typing]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    compile_succeeds(root);
    compile_succeeds(root);
    assert!(compiled(root, "app/Greeting.sharp").exists());
}

/// An extension host whose rule `acme/no-marker` reports an error at each `host-error-here` in a file.
const MARKER_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Reporting\Issue;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\SourceLocation;
use Mago\Sdk\Span;
use Mago\Sdk\Worker;

require $argv[1];

final class MarkerRule implements Plugin, AfterFileAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('acme', 'Acme rules', 'Reports each marker.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterFileAnalysisHook($this);
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function afterFileAnalysis(AfterFileAnalysisContext $context): void
    {
        $contents = $context->analysis->getSourceFile()->contents;
        $found = strpos($contents, 'host-error-here');
        if ($found !== false) {
            $context->report(Level::Error, 'no-marker', Issue::at(
                'The marker `host-error-here` is here.',
                new SourceLocation($context->analysis->file, new Span($found, $found + 15)),
            ));
        }
    }
}

(new Worker(new Extension('acme', 'Acme', '0.0.0', analyzerPlugins: [new MarkerRule()])))->run();
"#;

/// `mago compile` hides what `mago analyze` hides: an error of a host's rule that an `ignore` entry or the baseline
/// targets refuses no file, as `tsc` and `tsc --noEmit` report the same errors.
#[test]
fn compile_hides_the_host_errors_an_ignore_entry_or_the_baseline_hides_from_analyze() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host error suppression compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Marked.sharp",
        "namespace App;\n\npublic class Marked\n{\n    public static string text() => \"host-error-here\";\n}\n",
    );
    write(root, "tools/host.php", MARKER_HOST);
    write(
        root,
        "baseline.toml",
        "variant = \"loose\"\n\n[[issues]]\nfile = \"app/Marked.sharp\"\ncode = \"acme/no-marker\"\nmessage = \"The marker `host-error-here` is here.\"\ncount = 1\n",
    );
    let config = |analyzer: &str| {
        format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[analyzer]\n{analyzer}\n\n[extension-hosts.acme]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        )
    };

    write(root, "mago.toml", &config(""));
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("acme/no-marker"), "{}", printed(&output));

    for hidden in ["ignore = [\"acme/no-marker\"]", "baseline = \"baseline.toml\""] {
        write(root, "mago.toml", &config(hidden));
        let output = compile(root);
        assert!(output.status.success(), "{hidden}: {}", printed(&output));
        assert!(compiled(root, "app/Marked.sharp").exists(), "{hidden}");
    }
}

/// The typing host of [`a_typing_host_loads_a_class_only_its_typing_makes_correct_on_a_fresh_tree`]: it gives
/// `App\Helpers\untyped()` the return type `string`, and its hook throws unless the hosts' compile holds
/// `app/Greeting.sharp`, as a host whose loading of a project class fails rethrows.
const TYPING_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\FunctionReturnTypeProvider;
use Mago\Sdk\Analyzer\FunctionTarget;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class TypesHelpers implements FunctionReturnTypeProvider
{
    public function getTargets(): array
    {
        return [FunctionTarget::exact('App\\Helpers\\untyped')];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        return Type::string();
    }
}

final class Typing implements Plugin, BeforeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('typing', 'Typing', 'Types the helpers and loads App\Greeting.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerFunctionReturnTypeProvider(new TypesHelpers());
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        if (!is_file(getenv('SHARP_COMPILED') . '/app/Greeting.sharpc')) {
            throw new RuntimeException('App\Greeting isn\'t compiled for the hosts.');
        }
    }
}

(new Worker(new Extension('typing', 'Typing', '0.0.0', analyzerPlugins: [new Typing()])))->run();
"#;

/// An extension host that gives one plain PHP method its parameters, as a framework's host signs its query builder's
/// methods, so the hosts may sign any method the analysis can't find.
const SIGNING_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\CallableSignatureProvider;
use Mago\Sdk\Analyzer\CallableSignatureProviderContext;
use Mago\Sdk\Analyzer\EffectiveCallableSignature;
use Mago\Sdk\Analyzer\MethodReturnTypeProvider;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

require $argv[1];

final class Signing implements Plugin, MethodReturnTypeProvider, CallableSignatureProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('signing', 'Signing', 'Signs one builder method.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerMethodReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [MethodTarget::exact('App\\Builder', 'where')];
    }

    public function getCallableSignature(CallableSignatureProviderContext $context): ?EffectiveCallableSignature
    {
        return null;
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        return null;
    }
}

(new Worker(new Extension('signing', 'Signing', '0.0.0', analyzerPlugins: [new Signing()])))->run();
"#;

/// A method a `List`, a `Map`, a `Set` or an `Iterable` lacks is named on its type as PHP# writes it, never as the class
/// the analyzer checks the collection's methods against, also when a host may sign methods the analysis can't find.
#[test]
fn a_missing_collection_method_names_the_collection_type_when_a_host_signs_methods() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the collection method message test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Shelf.sharp",
        "namespace App;\n\npublic class Shelf\n{\n    public static void touch(List<int> sizes, Map<string, int> counts, Set<string> tags, Iterable<int> stream)\n    {\n        sizes.values();\n        counts.items();\n        tags.items();\n        stream.items();\n    }\n}\n",
    );
    write(root, "tools/host.php", SIGNING_HOST);
    write(
        root,
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.signing]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for missing in [
        "Method `values` does not exist on `List<int>`.",
        "Method `items` does not exist on `Map<string, int>`.",
        "Method `items` does not exist on `Set<string>`.",
        "Method `items` does not exist on `Iterable<int>`.",
    ] {
        assert!(printed(&output).contains(missing), "{missing}: {}", printed(&output));
    }
    assert!(!printed(&output).contains("Methods"), "{}", printed(&output));
}

#[test]
fn compile_deletes_the_compiled_file_of_a_renamed_or_removed_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    // A file is named like its type, so renaming the file renames the class.
    std::fs::remove_file(root.join("app/Order.sharp")).unwrap();
    std::fs::write(root.join("app/Invoice.sharp"), ORDER.replace("class Order", "class Invoice")).unwrap();
    compile_succeeds(root);
    assert!(!compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, "app/Invoice.sharp").exists());

    std::fs::remove_file(root.join("app/Invoice.sharp")).unwrap();
    compile_succeeds(root);
    assert_eq!(
        files_under(&root.join(".sharp")),
        [Path::new("listeners.php"), Path::new("vendor/acme/money/src/Money.sharpc")]
    );
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
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("app/Order.sharpc"), Path::new("listeners.php")]);
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

/// The PHP file `mago compile` writes beside `app/Order.sharp`, spec section 27.
const ORDER_PHP: &str = "<?php\n\n// mago compile writes this file from Order.sharp, so PHP code that lists or reads *.php files finds its type.\n\ndeclare(strict_types=1);\n\nnamespace App;\n\nif (!\\is_file(__DIR__ . '/Order.sharp')) {\n    return;\n}\n\n\\function_exists('Sharp\\Internal\\requireNative')\n    || throw new \\Error(\n        'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',\n    );\n\nrequire_once __DIR__ . '/Order.sharp';\n\n// The engine declares Order from Order.sharp, and this declaration never runs.\nif (false) {\n    class Order\n    {\n    }\n}\n";

/// Spec section 27: each of the project's PHP# sources that declares a type gets a PHP file of the type's name beside
/// it, so PHP code that finds classes by listing `*.php` files finds it. A module's file declares none, and a package
/// in `vendor/` is never written to.
#[test]
fn compile_writes_a_php_file_beside_each_project_source_that_declares_a_type() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Shop/Module.sharp",
        "namespace App.Shop;\n\nimport App.Order;\n\nmodule\n{\n    transient Order = Order;\n}\n",
    );
    let vendor = files_under(&root.join("vendor"));

    compile_succeeds(root);

    assert_eq!(std::fs::read_to_string(root.join("app/Order.php")).unwrap(), ORDER_PHP);
    assert_eq!(
        files_under(&root.join("app")),
        [Path::new("Legacy.php"), Path::new("Order.php"), Path::new("Order.sharp"), Path::new("Shop/Module.sharp")]
    );
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
}

/// A source in an `autoload-dev` folder gets no PHP file, so a test class keeps one file: PHPUnit loads a test class
/// only from the file it lists, and a project lists its PHP# tests with `suffix="Test.sharp"`.
#[test]
fn compile_writes_no_php_file_beside_a_source_in_an_autoload_dev_folder() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "composer.json",
        "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}, \"autoload-dev\": {\"psr-4\": {\"Tests\\\\\": \"tests/\"}}}\n",
    );
    write(root, "tests/Shop/CheckoutTest.sharp", "namespace Tests.Shop;\n\npublic class CheckoutTest\n{\n}\n");
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"tests\"]\nincludes = [\"vendor\"]\n",
    );

    compile_succeeds(root);

    assert!(compiled(root, "tests/Shop/CheckoutTest.sharp").exists());
    assert_eq!(files_under(&root.join("tests")), [Path::new("Shop/CheckoutTest.sharp")]);
    assert_eq!(std::fs::read_to_string(root.join("app/Order.php")).unwrap(), ORDER_PHP);
}

/// mago reads the PHP# source and never the PHP file beside it, as `tsc` reads `Order.ts` and never the `Order.d.ts`
/// beside it: the class has one declaration, and a project's lint reports nothing on a file nobody edits.
#[test]
fn mago_reads_the_source_and_never_the_php_file_beside_it() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    let mago = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never"])
            .args(arguments)
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .output()
            .unwrap()
    };
    let analyzed = mago(&["analyze", "app"]);
    let linted = mago(&["lint", "app/Order.php"]);

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert!(!printed(&analyzed).contains("duplicate-definition"), "{}", printed(&analyzed));
    assert!(printed(&linted).contains("No files found to lint."), "{}", printed(&linted));
}

/// The PHP file is in mago's own format, so a project's `mago fmt --check` passes on a file nobody edits.
#[test]
fn the_php_file_beside_a_source_passes_fmt_check() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    let output = Command::new(env!("CARGO_BIN_EXE_mago"))
        .args(["--no-version-check", "--colors", "never", "fmt", "--check", "app/Order.php"])
        .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
        .current_dir(root)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", printed(&output));
}

/// A second compile leaves a current PHP file as it is, and puts back one whose text changed.
#[test]
fn compile_keeps_the_php_file_beside_a_source_current() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let written = std::fs::metadata(root.join("app/Order.php")).unwrap().modified().unwrap();

    compile_succeeds(root);
    assert_eq!(std::fs::metadata(root.join("app/Order.php")).unwrap().modified().unwrap(), written);

    write(root, "app/Order.php", &ORDER_PHP.replace("require_once", "require"));
    compile_succeeds(root);
    assert_eq!(std::fs::read_to_string(root.join("app/Order.php")).unwrap(), ORDER_PHP);
}

/// The PHP file goes with its source, and a PHP file someone wrote stays.
#[test]
fn compile_deletes_the_php_file_of_a_renamed_or_removed_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::remove_file(root.join("app/Order.sharp")).unwrap();
    write(root, "app/Invoice.sharp", &ORDER.replace("class Order", "class Invoice"));
    compile_succeeds(root);
    assert_eq!(
        files_under(&root.join("app")),
        [Path::new("Invoice.php"), Path::new("Invoice.sharp"), Path::new("Legacy.php")]
    );

    std::fs::remove_file(root.join("app/Invoice.sharp")).unwrap();
    compile_succeeds(root);
    assert_eq!(files_under(&root.join("app")), [Path::new("Legacy.php")]);
}

/// The compile finds the PHP files it wrote on disk, so a project whose analyzer loads only PHP# files deletes them
/// too, the file of a folder whose every source is gone included.
#[test]
fn compile_deletes_the_php_file_of_a_removed_source_whatever_the_analyzer_loads() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\nextensions = [\"sharp\"]\n",
    );
    write(root, "app/Shop/Cart.sharp", "namespace App.Shop;\n\npublic class Cart\n{\n}\n");
    compile_succeeds(root);
    assert!(root.join("app/Order.php").exists() && root.join("app/Shop/Cart.php").exists());

    std::fs::remove_file(root.join("app/Order.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Shop/Cart.sharp")).unwrap();
    compile_succeeds(root);

    assert_eq!(files_under(&root.join("app")), [Path::new("Legacy.php")]);
}

/// The compiled files and the PHP files get the mode any new file gets, `0644` under the usual umask, as the source
/// itself got, so a server that runs PHP as another user, as PHP-FPM's pool user, reads them.
#[cfg(unix)]
#[test]
fn compile_writes_files_with_the_mode_of_a_new_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let source = mode(&root.join("app/Order.sharp"));
    for written in [root.join("app/Order.php"), compiled(root, "app/Order.sharp"), root.join(".sharp/listeners.php")] {
        assert_eq!(mode(&written), source, "{}", written.display());
    }
}

/// A PHP file someone wrote beside a source is what PHP loads for its name, so the compile refuses it, names both
/// files and the class, leaves it as it is, still writes the compiled file, and prints its summary as an error.
#[test]
fn compile_refuses_a_php_file_someone_wrote_beside_a_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Order.php", "<?php\n\nnamespace App;\n\nfinal class OrderLegacy {}\n");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Order.php and app/Order.sharp both name App.Order. Delete app/Order.php, or rename one of them."
        ),
        "{}",
        printed(&output)
    );
    let summary =
        printed(&output).lines().find(|line| line.contains("Compiled 2 PHP# files into .sharp/.")).map(String::from);
    assert!(summary.as_deref().is_some_and(|line| line.contains(" ERROR ")), "{}", printed(&output));
    assert_eq!(
        std::fs::read_to_string(root.join("app/Order.php")).unwrap(),
        "<?php\n\nnamespace App;\n\nfinal class OrderLegacy {}\n"
    );
    assert!(compiled(root, "app/Order.sharp").exists());
}

/// PHP names a class whatever its case, so a PHP file whose name differs from the source's only in case names the same
/// class. The compile refuses it by its exact name on every disk, one that ignores case included, and writes no file
/// of the source's name, which such a disk would write over it.
#[test]
fn compile_refuses_a_php_file_whose_name_differs_from_the_source_only_in_case() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/order.php", "<?php\n\nnamespace App;\n\nfinal class OrderLegacy {}\n");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/order.php and app/Order.sharp both name App.Order. Delete app/order.php, or rename one of them."
        ),
        "{}",
        printed(&output)
    );
    assert_eq!(
        files_under(&root.join("app")),
        [Path::new("Legacy.php"), Path::new("Order.sharp"), Path::new("order.php")]
    );
    assert_eq!(
        std::fs::read_to_string(root.join("app/order.php")).unwrap(),
        "<?php\n\nnamespace App;\n\nfinal class OrderLegacy {}\n"
    );
}

/// A PHP file the compile wrote under another case, as for a source whose class was renamed only in case, gives way to
/// the file of the source's exact name, on a disk that ignores case and on one that does not.
#[test]
fn compile_names_the_php_file_exactly_like_its_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/order.php", &ORDER_PHP.replace("Order.sharp", "order.sharp"));

    compile_succeeds(root);

    assert_eq!(
        files_under(&root.join("app")),
        [Path::new("Legacy.php"), Path::new("Order.php"), Path::new("Order.sharp")]
    );
    assert_eq!(std::fs::read_to_string(root.join("app/Order.php")).unwrap(), ORDER_PHP);
}

/// Writing the PHP files leaves every compiled file current, as the engine checks it, a file whose namespace holds a
/// class named like a default class included: its compiled file stamps that class's PHP file as an input.
#[test]
fn every_compiled_file_is_current_after_the_php_files_are_written() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Environment.sharp",
        "namespace App;\n\npublic class Environment\n{\n    public static string stage() => \"production\";\n}\n",
    );
    write(
        root,
        "app/Report.sharp",
        "namespace App;\n\npublic class Report\n{\n    public int total() => new Order().total(1);\n\n    public static string stage() => Environment.stage();\n}\n",
    );

    compile_succeeds(root);

    assert!(root.join("app/Environment.php").exists() && root.join("app/Report.php").exists());
    for name in ["app/Environment.sharp", "app/Order.sharp", "app/Report.sharp", MONEY_PATH] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        assert!(!is_stale(&std::fs::canonicalize(root).unwrap(), &bytes).unwrap(), "{name}");
    }
}

/// Runs git in `folder`, and fails the test when git fails.
fn git(folder: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .args(["-c", "user.name=PHP#", "-c", "user.email=sharp@example.com", "-c", "init.defaultBranch=main"])
        .args(arguments)
        .current_dir(folder)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {}", printed(&output));

    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The files `git status` lists as untracked in `folder`.
fn untracked(folder: &Path) -> String {
    git(folder, &["status", "--porcelain", "--untracked-files=all"])
}

/// In a git repository, the PHP files are kept out of git through the block `mago compile` owns in
/// `git rev-parse --git-path info/exclude`, the shared file of a worktree included, and every other line stays.
#[test]
fn compile_keeps_the_php_files_out_of_git_in_a_clone_and_in_a_worktree() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    write(&clone, ".git/info/exclude", "# git ls-files --others --exclude-from=.git/info/exclude\n*.log\n");
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);

    compile_succeeds(&clone);
    assert!(clone.join("app/Order.php").exists());
    assert_eq!(
        untracked(&clone),
        "?? .sharp/app/Order.sharpc\n?? .sharp/listeners.php\n?? .sharp/vendor/acme/money/src/Money.sharpc\n"
    );
    let kept = "# git ls-files --others --exclude-from=.git/info/exclude\n*.log\n";
    assert_eq!(
        std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(),
        format!("{kept}{}", excluded(&clone, "/app/Order.php\n"))
    );

    let worktrees = tempfile::tempdir().unwrap();
    let worktree = std::fs::canonicalize(worktrees.path()).unwrap().join("feature");
    git(&clone, &["worktree", "add", "--quiet", worktree.to_str().unwrap()]);
    write(&worktree, "app/Invoice.sharp", &ORDER.replace("class Order", "class Invoice"));
    compile_succeeds(&worktree);
    assert!(worktree.join("app/Invoice.php").exists());
    assert_eq!(
        untracked(&worktree),
        "?? .sharp/app/Invoice.sharpc\n?? .sharp/app/Order.sharpc\n?? .sharp/listeners.php\n?? .sharp/vendor/acme/money/src/Money.sharpc\n?? app/Invoice.sharp\n"
    );

    assert_eq!(
        std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(),
        format!(
            "{kept}{}{}",
            excluded(&clone, "/app/Order.php\n"),
            excluded(&worktree, "/app/Invoice.php\n/app/Order.php\n")
        ),
        "each checkout keeps its own block in the file they share"
    );

    std::fs::remove_file(clone.join("app/Order.sharp")).unwrap();
    compile_succeeds(&clone);
    assert!(!clone.join("app/Order.php").exists(), "a PHP file git ignores is still deleted with its source");
    assert_eq!(
        std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(),
        format!("{kept}{}", excluded(&worktree, "/app/Invoice.php\n/app/Order.php\n")),
        "a checkout with no PHP file left has no block"
    );
}

/// A compile that can't run git, as in a runtime image that ships none, keeps the PHP files out of git all the same, in
/// a clone and in a linked worktree, because it reads `.git` itself to find the `info/exclude` git reads.
#[test]
fn compile_keeps_the_php_files_out_of_git_with_no_git_to_run() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);
    let worktrees = tempfile::tempdir().unwrap();
    let worktree = std::fs::canonicalize(worktrees.path()).unwrap().join("feature");
    git(&clone, &["worktree", "add", "--quiet", worktree.to_str().unwrap()]);

    for checkout in [&clone, &worktree] {
        let output = command(checkout, "never").env("PATH", "").output().unwrap();
        assert!(output.status.success(), "{}", printed(&output));
        assert!(checkout.join("app/Order.php").exists());
        assert_eq!(
            untracked(checkout),
            "?? .sharp/app/Order.sharpc\n?? .sharp/listeners.php\n?? .sharp/vendor/acme/money/src/Money.sharpc\n",
            "{}",
            checkout.display()
        );
    }
}

/// A `.git` file that names a folder this machine doesn't have, as in a container that mounts a linked worktree
/// without its repository, leaves the PHP files where git shows them, so the compile says so and how to fix it.
#[test]
fn compile_warns_when_the_repository_of_a_worktree_is_missing() {
    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    write(&root, ".git", "gitdir: /missing/repository/.git/worktrees/feature\n");

    let output = compile(&root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "{}/.git names /missing/repository/.git/worktrees/feature, which is missing, so git shows app/Order.php as untracked. Make the repository's .git folder reachable here, and run vendor/bin/mago compile again.",
            root.display()
        )),
        "{}",
        printed(&output)
    );
}

/// An `info/exclude` that can't be written, as in a read-only `.git`, leaves the new PHP files where git shows them, so
/// the compile names them and the folder to make writable, which the file is renamed into, and keeps quiet about the
/// temporary file and the files the earlier block keeps out of git.
#[cfg(unix)]
#[test]
fn compile_warns_when_the_exclude_file_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    git(&root, &["init", "--quiet"]);
    compile_succeeds(&root);
    let info = root.join(".git/info");
    std::fs::set_permissions(&info, std::fs::Permissions::from_mode(0o555)).unwrap();
    write(&root, "app/Refund.sharp", "namespace App;\n\npublic class Refund\n{\n}\n");

    let output = compile(&root);
    std::fs::set_permissions(&info, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "{}/exclude can't be replaced, as {} can't be written: permission denied, so git shows app/Refund.php as untracked. Make {} writable, and run vendor/bin/mago compile again.",
            info.display(),
            info.display(),
            info.display()
        )),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains(&format!("{}/.tmp", info.display())), "{}", printed(&output));
}

/// A repository git refuses to read, as a checkout another user owns in a container or a CI job, or a broken `.git`,
/// fails no compile: the block lists every PHP file, as with no git to run, and the compile warns once with git's
/// reason.
#[test]
fn compile_keeps_the_php_files_out_of_git_where_git_refuses_the_repository() {
    let directory = project(ORDER, MONEY);
    let owned = std::fs::canonicalize(directory.path()).unwrap();
    git(&owned, &["init", "--quiet"]);
    let broken = tempfile::tempdir().unwrap();
    let broken = std::fs::canonicalize(broken.path()).unwrap();
    project_in(&broken, ORDER, MONEY);
    std::fs::create_dir(broken.join(".git")).unwrap();

    for (root, owner) in [(&owned, "1"), (&broken, "0")] {
        write(root, ".git/info/exclude", "*.log\n");
        // A CI runner's git config can trust every folder with `safe.directory = *`, so the compile reads none.
        let output = command(root, "never")
            .env("GIT_TEST_ASSUME_DIFFERENT_OWNER", owner)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();

        assert!(output.status.success(), "{}: {}", root.display(), printed(&output));
        assert_eq!(
            std::fs::read_to_string(root.join(".git/info/exclude")).unwrap(),
            format!("*.log\n{}", excluded(root, "/app/Order.php\n")),
            "{}",
            root.display()
        );
        let warning = format!("git can't read the .gitignore files of {}: fatal: ", root.display());
        assert_eq!(printed(&output).matches(&warning).count(), 1, "{}: {}", root.display(), printed(&output));
        assert_eq!(printed(&output).matches("fatal: ").count(), 1, "git's own output stays out: {}", printed(&output));
        assert!(
            printed(&output).contains(". The repository's info/exclude lists every file mago compile writes there."),
            "{}",
            printed(&output)
        );
    }
    assert_eq!(
        untracked(&owned),
        "?? .sharp/app/Order.sharpc\n?? .sharp/listeners.php\n?? .sharp/vendor/acme/money/src/Money.sharpc\n?? app/Legacy.php\n?? app/Order.sharp\n?? composer.lock\n?? mago.toml\n?? vendor/acme/money/src/Money.sharp\n",
        "the owner's git reads the block"
    );
}

/// The block of `info/exclude` that `mago compile` keeps for the workspace `place`, listing `paths`.
fn excluded(place: &Path, paths: &str) -> String {
    format!("# mago compile: the files it writes in {}\n{paths}# mago compile: end\n", place.display())
}

/// A PHP file a `.gitignore` already ignores needs no line of its own, so a compile in a folder git ignores, as a build
/// folder, or of PHP files a `.gitignore` pattern covers, writes no block.
#[test]
fn compile_writes_no_block_for_the_php_files_git_already_ignores() {
    let directory = tempfile::tempdir().unwrap();
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    write(&clone, ".gitignore", "/build/\n/covered/app/*.php\n");
    write(&clone, ".git/info/exclude", "*.log\n");

    for folder in ["build/project", "covered"] {
        let root = clone.join(folder);
        project_in(&root, ORDER, MONEY);
        compile_succeeds(&root);
        assert!(root.join("app/Order.php").exists(), "{folder}");
    }

    assert_eq!(std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(), "*.log\n");
}

/// A block whose folder is gone, or is now ignored, keeps nothing out of git, so the next compile in any checkout of
/// the repository removes it, and keeps every other line.
#[test]
fn compile_removes_the_block_of_a_folder_that_is_gone_or_ignored() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    write(&clone, ".gitignore", "/build/\n");
    let gone = clone.join("old");
    let ignored = clone.join("build/project");
    std::fs::create_dir_all(&ignored).unwrap();
    write(
        &clone,
        ".git/info/exclude",
        &format!(
            "*.log\n{}{}",
            excluded(&gone, "/old/app/Order.php\n"),
            excluded(&ignored, "/build/project/app/Order.php\n")
        ),
    );

    compile_succeeds(&clone);

    assert_eq!(
        std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(),
        format!("*.log\n{}", excluded(&clone, "/app/Order.php\n"))
    );
}

/// `app/OrderNotices.sharp`, whose two listeners run in their place in the class, not in name order.
const NOTICES: &str = "namespace App;\n\npublic class OrderNotices\n{\n    public void send(OrderPaid e) on OrderPaid { }\n\n    public void refresh() on OrderPaid, OrderEvent { }\n}\n";

/// The row of the input at `path` in the listener table, stamped as the engine reads it.
fn input_row(root: &Path, path: &str) -> String {
    let input = stamp(root, path.as_bytes()).unwrap().unwrap();
    let hash: String = input.hash.iter().map(|byte| format!("{byte:02x}")).collect();

    format!("        ['{path}', {}, {}, '{hash}'],\n", input.size, input.mtime_ns)
}

/// The listener table of `inputs`, `rows` and `dispatcher`, the PHP literal of its dispatcher module.
fn listener_table(root: &Path, inputs: &[&str], rows: &str, dispatcher: &str) -> String {
    let inputs: String = inputs.iter().map(|path| input_row(root, path)).collect();

    format!(
        "<?php\n\nreturn [\n    'inputs' => [\n{inputs}    ],\n    'listeners' => [\n{rows}    ],\n    'dispatcher' => {dispatcher},\n];\n"
    )
}

/// Spec section 15: `mago compile` lists every listener of an accepted file in `.sharp/listeners.php`, the table the
/// engine runs them from, by its class's full name, then by its place in its class. A refused file's listeners leave
/// the table, and the table stays once no listener is left. Its inputs are every PHP# file, which an `on` clause may
/// join, and the listing of the folder of each and of each folder above it, which a file or a namespace folder joining
/// or leaving changes, so the engine can tell when the table is out of date.
#[test]
fn compile_writes_every_listener_of_an_accepted_file_in_order() {
    let directory = lawful_project(&[
        ("app/OrderEvent.sharp", "namespace App;\n\npublic interface OrderEvent\n{\n    int orderId { get; }\n}\n"),
        ("app/OrderPaid.sharp", "namespace App;\n\npublic event OrderPaid(int orderId) : OrderEvent;\n"),
        (
            "app/Receipts.sharp",
            "namespace App;\n\npublic class Receipts\n{\n    private void deliver(OrderPaid e) on OrderPaid { }\n}\n",
        ),
        ("app/OrderNotices.sharp", NOTICES),
    ]);
    let root = directory.path();

    let output = compile(root);
    assert!(printed(&output).contains("Compiled 4 PHP# files into .sharp/."), "{}", printed(&output));
    let every_file = [
        "*.sharp",
        "app/*.sharp",
        "app/OrderEvent.sharp",
        "app/OrderNotices.sharp",
        "app/OrderPaid.sharp",
        "app/Receipts.sharp",
    ];
    assert_eq!(
        read(root, ".sharp/listeners.php"),
        listener_table(
            root,
            &every_file,
            "        ['App\\OrderNotices', 'send', ['App\\OrderPaid'], true],\n        ['App\\OrderNotices', 'refresh', ['App\\OrderPaid', 'App\\OrderEvent'], false],\n        ['App\\Receipts', 'deliver', ['App\\OrderPaid'], true],\n",
            "null",
        )
    );

    write(root, "app/OrderNotices.sharp", &NOTICES.replace("public void send", "public static void send"));
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(
        read(root, ".sharp/listeners.php"),
        listener_table(root, &every_file, "        ['App\\Receipts', 'deliver', ['App\\OrderPaid'], true],\n", "null")
    );

    std::fs::remove_file(root.join("app/OrderNotices.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Receipts.sharp")).unwrap();
    write(root, "app/Shop/Cart.sharp", "namespace App.Shop;\n\npublic class Cart\n{\n}\n");
    compile_succeeds(root);
    assert_eq!(
        read(root, ".sharp/listeners.php"),
        listener_table(
            root,
            &[
                "*.sharp",
                "app/*.sharp",
                "app/OrderEvent.sharp",
                "app/OrderPaid.sharp",
                "app/Shop/*.sharp",
                "app/Shop/Cart.sharp"
            ],
            "",
            "null",
        )
    );
}

/// Spec section 15: a listener on a generic event lists the event with its type arguments, as the type text the engine
/// tests an emitted event against, so `on Saved<Order>` runs for a `Saved<Order>` alone, while a listener on an
/// interface the event implements lists the interface's name and runs for every `Saved<…>`.
#[test]
fn compile_writes_a_generic_event_s_listener_with_its_type_arguments() {
    let directory = lawful_project(&[
        ("app/Entity.sharp", "namespace App;\n\npublic class Entity\n{\n}\n"),
        ("app/Order.sharp", "namespace App;\n\npublic class Order : Entity\n{\n}\n"),
        ("app/EntityEvent.sharp", "namespace App;\n\npublic interface EntityEvent\n{\n}\n"),
        ("app/Saved.sharp", "namespace App;\n\npublic event Saved<T : Entity>(T entity) : EntityEvent;\n"),
        (
            "app/Audit.sharp",
            "namespace App;\n\npublic class Audit\n{\n    private void deliver(Saved<Order> e) on Saved<Order> { }\n    private void record(EntityEvent e) on EntityEvent { }\n}\n",
        ),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    let table = read(root, ".sharp/listeners.php");
    assert!(
        table.contains(
            "        ['App\\Audit', 'deliver', ['App.Saved<App.Order>'], true],\n        ['App\\Audit', 'record', ['App\\EntityEvent'], true],\n"
        ),
        "{table}"
    );
}

/// The standard library's `Dispatcher`, at its place in a project that installs the library.
const LIBRARY_DISPATCHER: [(&str, &str); 2] = [
    (
        "vendor/heyjordanparker/php-sharp-composer/composer.json",
        "{\"name\": \"heyjordanparker/php-sharp-composer\", \"autoload\": {\"psr-4\": {\"Sharp\\\\\": \"library/Sharp/\"}}}\n",
    ),
    (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Dispatcher.sharp",
        include_str!("../../composer/library/Sharp/Dispatcher.sharp"),
    ),
];

/// A module of `namespace` that binds the dispatcher to `Recorder`.
fn dispatcher_module(namespace: &str) -> String {
    format!(
        "namespace {namespace};\n\nimport App.Recorder;\nimport Sharp.Dispatcher;\n\nmodule\n{{\n    singleton Dispatcher = Recorder;\n}}\n"
    )
}

/// Spec section 15: the dispatcher is one service for the whole app, so the listener table names the app's top module
/// that binds it, whose binding serves every `emit`, and none when no app module binds it. A binding below the top one
/// is refused, and the table still names the top one.
#[test]
fn compile_names_the_app_module_that_binds_the_dispatcher() {
    let recorder = "namespace App;\n\nimport Sharp.Dispatcher;\n\npublic class Recorder : Dispatcher\n{\n    public void dispatch(Any emitted, List<Function<void(Any)>> listeners) { }\n}\n";
    let shop = dispatcher_module("App.Shop");
    let [composer, dispatcher] = LIBRARY_DISPATCHER;
    let directory =
        lawful_project(&[composer, dispatcher, ("app/Recorder.sharp", recorder), ("app/Shop/Module.sharp", &shop)]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(
        read(root, ".sharp/listeners.php")
            .ends_with("    'dispatcher' => ['app/Shop/Module.sharp', 'app\\\\shop'],\n];\n")
    );

    write(root, "app/Module.sharp", &dispatcher_module("App"));
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Only `app/Module.sharp` binds `Dispatcher`"), "{}", printed(&output));
    assert!(read(root, ".sharp/listeners.php").ends_with("    'dispatcher' => ['app/Module.sharp', 'app'],\n];\n"));

    std::fs::remove_file(root.join("app/Shop/Module.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Module.sharp")).unwrap();
    compile_succeeds(root);
    assert!(read(root, ".sharp/listeners.php").ends_with("    'dispatcher' => null,\n];\n"));
}

/// A path the listener table quotes keeps a `'` and a `\` in its name. The PSR-4 folder that maps `App` carries both,
/// since a folder below it names a namespace, which can take neither.
#[test]
fn compile_quotes_each_input_path_of_the_listener_table() {
    let directory = lawful_project(&[("it's\\here/Cart.sharp", "namespace App;\n\npublic class Cart\n{\n}\n")]);
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"it's\\\\here\"]\nincludes = [\"vendor\"]\n",
    );
    write(
        root,
        "composer.json",
        "{\n    \"autoload\": {\n        \"psr-4\": {\n            \"App\\\\\": \"it's\\\\here/\"\n        }\n    }\n}\n",
    );

    compile_succeeds(root);
    let input = input_row(root, "it's\\here/*.sharp").replace("it's\\here", "it\\'s\\\\here");
    assert!(read(root, ".sharp/listeners.php").contains(&input), "{}", read(root, ".sharp/listeners.php"));
}

/// Spec section 28.1's `app/Shared/Money.sharp`, whose law holds over its pure `add`.
const LAWFUL_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n";

/// The `.lean` files of the project outside `.sharp/`. A law is proven from its `.sharp` file alone, so the compile
/// writes none.
fn source_lean_files(root: &Path) -> Vec<PathBuf> {
    files_under(root)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "lean") && !path.starts_with(".sharp"))
        .collect()
}

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
fn compile_proves_a_law_from_its_sharp_file_alone_and_writes_the_compiled_file() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
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
        assert_eq!(source_lean_files(project.path()), Vec::<PathBuf>::new());
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
        assert_eq!(source_lean_files(project.path()), Vec::<PathBuf>::new());
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
    assert_eq!(source_lean_files(project.path()), Vec::<PathBuf>::new());
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
        assert_eq!(source_lean_files(project.path()), Vec::<PathBuf>::new());
    }
    assert!(library.join("lakefile.toml").is_file(), "the library build has its lakefile again");
    assert_eq!(library_builds(cache.path()), [library], "one library build is left, with no staging folder");
}

#[test]
fn a_second_compile_leaves_the_compiled_file_byte_identical_and_starts_no_lean_process() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);
    let before = std::fs::read(compiled(root, "app/Shared/Money.sharp")).unwrap();

    let output = compile_without_lean(root);

    assert!(output.status.success(), "the compile needed lake: {}", printed(&output));
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
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
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
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Money.sharp:8:5: error[law-unproven]: law addKeepsCurrency is not proven\n = still to show: new Money(a.amount + b.amount, b.currency).currency == a.currency\n"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// Spec section 10: a struct states a law as a class does. Mago proves it, and refuses it once a changed method breaks
/// it.
#[test]
fn a_struct_states_a_law() {
    let money = LAWFUL_MONEY.replace("public class Money", "public struct Money");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();
    compile_succeeds(root);

    write(
        root,
        "app/Shared/Money.sharp",
        &money.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency"),
    );
    let output = compile_lines(root);

    assert!(
        printed(&output)
            .contains("app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"),
        "{}",
        printed(&output)
    );
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
    assert!(compiled(root, "app/Shared/Wallet.sharp").exists());

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
            printed(&output).contains(&format!(
                "app/Shared/{class}.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"
            )),
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
    assert!(compiled(root, "app/Shared/Wallet.sharp").exists());
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

/// Puts a `lake` first on the `PATH` that runs the real one, except that its first `--version`, the call on which elan
/// installs a missing Lean, installs into the empty folder `toolchain` in `folder`, as elan does, with no lock of its
/// own: an install takes a second, and an install that starts while another runs leaves `overlap` in `folder`.
#[cfg(unix)]
fn installing_lake(folder: &Path) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH").unwrap();
    let real =
        std::env::split_paths(&path).map(|directory| directory.join("lake")).find(|lake| lake.is_file()).unwrap();
    let toolchain = folder.join("toolchain");
    std::fs::create_dir_all(&toolchain).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = --version ] && [ ! -e '{toolchain}/installed' ]; then\n  if mkdir '{toolchain}/installing' 2>/dev/null; then\n    sleep 1\n    echo lean >> '{toolchain}/installed'\n    rmdir '{toolchain}/installing'\n  else\n    touch '{overlap}'\n  fi\nfi\nexec '{real}' \"$@\"\n",
        toolchain = toolchain.display(),
        overlap = folder.join("overlap").display(),
        real = real.display(),
    );
    let lake = folder.join("bin/lake");
    write(folder, "bin/lake", &script);
    std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).unwrap();

    std::env::join_paths(std::iter::once(folder.join("bin")).chain(std::env::split_paths(&path))).unwrap()
}

/// elan's `lake` installs a missing Lean on its first call, with no lock of its own, so compiles that start together on
/// a machine without that Lean would break it. Mago makes that call under the lock its own installs take, so the
/// compiles take turns and Lean is installed once.
#[cfg(unix)]
#[test]
fn compiles_that_start_together_install_the_missing_lean_of_the_lake_on_the_path_once() {
    let lake = tempfile::tempdir().unwrap();
    let path = installing_lake(lake.path());
    let projects: Vec<_> = (0..2).map(|_| lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)])).collect();

    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let compiles: Vec<_> = projects
            .iter()
            .map(|project| scope.spawn(|| command(project.path(), "never").env("PATH", &path).output().unwrap()))
            .collect();
        compiles.into_iter().map(|compile| compile.join().unwrap()).collect()
    });

    for output in &outputs {
        assert!(output.status.success(), "{}", printed(output));
    }
    assert!(!lake.path().join("overlap").exists(), "two compiles installed Lean at once");
    assert_eq!(read(lake.path(), "toolchain/installed"), "lean\n");
}

#[test]
fn compile_refuses_an_unproven_law_every_time_and_never_leaves_a_current_compiled_file() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
    let root = directory.path();

    for run in ["first", "second"] {
        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{run}: {}", printed(&output));
        assert!(
            printed(&output)
                .contains("app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven"),
            "{run}: {}",
            printed(&output)
        );
        assert!(!compiled(root, "app/Shared/Money.sharp").exists(), "{run}");
    }
}

/// A file whose law needs proving needs the law checker; with no `lake` and no Lean Mago can install, each such file is
/// refused with `law-checker-unavailable`, a fixed sentence that names no tool and no folder of one, and every other
/// file compiles. The debug log holds what failed.
#[test]
fn compile_reports_an_unavailable_law_checker_on_each_file_that_needs_it_and_whose_compiled_file_is_not_current() {
    let unavailable = if cfg!(target_env = "musl") {
        "error - law-checker-unavailable: Laws need the law checker, and this musl build of the checker can't install it, because the law checker has no musl build. On glibc Linux, the checker's glibc build installs it itself."
    } else {
        "error - law-checker-unavailable: The law checker could not be installed. Run vendor/bin/mago compile again to retry, with MAGO_LOG=debug to see why."
    };
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Order.sharp", ORDER)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(issue_lines(&output), [format!("app/Shared/Money.sharp:8:5:{unavailable}")], "{}", printed(&output));
    assert!(!printed(&output).contains("elan"), "{}", printed(&output));
    assert!(compiled(root, "app/Order.sharp").exists());
}

#[test]
fn compile_needs_no_lean_for_a_project_with_no_law() {
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
fn compile_deletes_the_lean_package_once_no_law_remains() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::remove_file(root.join("app/Shared/Money.sharp")).unwrap();
    write(root, "app/Order.sharp", ORDER);
    let output = compile_without_lean(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!root.join(".sharp/.lean").exists());
    assert_eq!(files_under(&root.join(".sharp")), [Path::new("app/Order.sharpc"), Path::new("listeners.php")]);
}

/// A `Money.lean` a 0.4.2 compile left beside `Money.sharp` is an ordinary unused file: the compile builds none of it,
/// and the law it once proved is proven from `Money.sharp` alone.
#[test]
fn compile_reads_no_lean_file_beside_a_class_and_proves_its_law() {
    let leftover = "import Code.App.Shared.Money\nopen Sharp\n\ntheorem addKeepsCurrency : App.Shared.Money.addKeepsCurrency := by\n  simp [App.Shared.Money.addKeepsCurrency, App.Shared.Money.add]\n";
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Shared/Money.lean", leftover)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(read(root, "app/Shared/Money.lean"), leftover);
}

#[test]
fn compile_proves_a_law_an_edit_adds_from_the_sharp_file_alone() {
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
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
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

/// A call through the class, as `ShapeMath.doubled(s)`, is that static call already, as in C#, so it compiles and its
/// law is proved the same way.
#[test]
fn compile_proves_a_law_through_an_extension_member_as_the_static_call_it_runs_as() {
    let directory = lawful_project(&[
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\nimport App.Shared.ShapeMath;\n\npublic class Shape\n{\n    public Shape(public int sides { get; }) { }\n\n    public static int doubledOr(Shape? s) => s?.doubled() ?? 0;\n    public static bool flatOr(Shape? s) => s?.isFlat ?? true;\n    public static int direct(Shape s) => ShapeMath.doubled(s) + (ShapeMath.isFlat(s) ? 1 : 0);\n\n    law doubles(Shape s) => s.doubled() == s.sides + s.sides;\n    law flat(Shape s) => s.isFlat == (s.sides < 3);\n    law twice(int n) => n.twice() == n + n;\n    law absent(int unused) => Shape.doubledOr(null) == 0 && Shape.flatOr(null);\n    law throughTheClass(Shape s) => ShapeMath.doubled(s) == s.doubled() && ShapeMath.isFlat(s) == s.isFlat;\n}\n",
        ),
        (
            "app/Shared/ShapeMath.sharp",
            "namespace App.Shared;\n\npublic static class ShapeMath\n{\n    extension(Shape shape)\n    {\n        public int doubled() => shape.sides + shape.sides;\n        public bool isFlat => shape.sides < 3;\n    }\n\n    extension(int n)\n    {\n        public int twice() => n + n;\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    let laws = read(root, ".sharp/.lean/Code/App/Shared/Shape.lean");
    for call in [
        "App.Shared.ShapeMath.doubled s",
        "App.Shared.ShapeMath.isFlat s",
        "App.Shared.ShapeMath.twice n",
        "App.Shared.ShapeMath.doubled (s.getD default)",
        "App.Shared.ShapeMath.isFlat (s.getD default)",
    ] {
        assert!(laws.contains(call), "{call}:\n{laws}");
    }
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());
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
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Shop/Status.sharp").exists());
}

/// Spec section 28's `state-machine-law` example as it stands.
const STATE_MACHINE: &str = "namespace App.Shop;\n\npublic enum Status : string\n{\n    case Open = \"open\";\n    case Paid = \"paid\";\n    case Refunded = \"refunded\";\n\n    public Status after(Payment e) => match (this) {                               // pure: the next status for every status and event\n        Status.Open => Status.Paid,\n        Status.Paid => Status.Refunded,\n        Status.Refunded => Status.Refunded,\n    };\n    law refundedIsFinal(Payment e) => Status.Refunded.after(e) == Status.Refunded;\n}\n";

/// Spec section 28's `state-machine-law` example, whose `Payment` is an interface: the law holds for every class that
/// implements it.
#[test]
fn compile_proves_spec_section_28_s_state_machine_law_over_an_interface() {
    let directory = lawful_project(&[
        ("app/Shop/Payment.sharp", "namespace App.Shop;\n\npublic interface Payment\n{\n}\n"),
        ("app/Shop/Status.sharp", STATE_MACHINE),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Shop/Status.sharp").exists());
}

/// Spec section 28's `state-machine-law` example, called with a class that implements `Payment` as its event: Lean
/// takes the class's value as a `Payment` it knows nothing about, so the law proves for every event.
#[test]
fn compile_proves_spec_section_28_s_state_machine_law_with_a_class_as_its_event() {
    let directory = lawful_project(&[
        ("app/Shop/Payment.sharp", "namespace App.Shop;\n\npublic interface Payment\n{\n}\n"),
        ("app/Shop/Status.sharp", STATE_MACHINE),
        (
            "app/Shop/Card.sharp",
            "namespace App.Shop;\n\npublic class Card : Payment\n{\n    public Card(public int cents { get; }) { }\n\n    law cardRefundIsFinal(int cents) => Status.Refunded.after(new Card(cents)) == Status.Refunded;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Card.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// A law never calls an interface's method, which a law cannot reason about, even when every implementation is
/// pure as spec section 29 holds `Card.amount` to. A call through `?.` is refused too.
#[test]
fn a_law_that_calls_an_interface_method_is_refused() {
    let payment = "namespace App.Shop;\n\npublic interface Payment\n{\n    int amount();\n}\n";
    for (file, source, law, calls) in [
        (
            "app/Shop/Card.sharp",
            "namespace App.Shop;\n\npublic class Card : Payment\n{\n    public Card(public int cents { get; }) { }\n\n    public int amount() => this.cents;\n\n    public static bool same(Payment p) => p.amount() == p.amount();\n\n    law alwaysSame(Payment p) => Card.same(p);\n}\n",
            "alwaysSame reaches the interface method Payment.amount in Card.same",
            ["9:43", "9:57"],
        ),
        (
            "app/Shop/Refund.sharp",
            "namespace App.Shop;\n\npublic class Refund\n{\n    public Refund(public Payment payment { get; }, public Payment? fee { get; }) { }\n\n    public int owed() => this.payment.amount() - (this.fee?.amount() ?? 0);\n\n    law owedIsNumber(Refund r) => r.owed() == r.owed();\n}\n",
            "owedIsNumber reaches the interface method Payment.amount in Refund.owed",
            ["7:26", "7:51"],
        ),
    ] {
        let directory = lawful_project(&[("app/Shop/Payment.sharp", payment), (file, source)]);
        let root = directory.path();
        let expected: Vec<String> = calls
            .iter()
            .map(|at| format!("{file}:{at}:error - unmodeled-construct: Law {law}. Call the method on a value of a class that implements the interface, since a law cannot know which implementation runs."))
            .collect();

        let output = compile_without_lean(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert_eq!(issue_lines(&output), expected, "{}", printed(&output));
    }
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

/// Spec section 28: the checker proves the module law from `Module.sharp` alone, and writes no `Module.lean`.
#[test]
fn compile_proves_spec_section_28_s_module_law_from_module_sharp_alone() {
    let directory = store("refunded < paid ? refunded : paid", "refunded < paid ? refunded : paid");
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
    assert!(compiled(root, "app/Tenant/Store/Module.sharp").exists());
}

/// Spec section 28: a module law's body proves it as a class law's does, calling the laws of the module and of its
/// classes through their names.
#[test]
fn compile_proves_a_module_law_by_its_body() {
    let module = "namespace App.Tenant.Store;\n\nmodule\n{\n    law receiptMatchesRefunds(int paid, int refunded) => Receipts.refundable(paid, refunded) == Refunds.remaining(paid, refunded);\n    law receiptNeverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0 => Refunds.remaining(paid, refunded) <= paid\n    {\n        Module.receiptMatchesRefunds(paid, refunded);\n        Receipts.neverAbovePaid(paid, refunded);\n    }\n}\n";
    let receipts = "namespace App.Tenant.Store;\n\npublic static class Receipts\n{\n    public static int refundable(int paid, int refunded) => refunded < paid ? paid - refunded : 0;\n\n    law neverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0 => Receipts.refundable(paid, refunded) <= paid;\n}\n";
    let refunds = "namespace App.Tenant.Store;\n\npublic static class Refunds\n{\n    public static int remaining(int paid, int refunded) => refunded < paid ? paid - refunded : 0;\n}\n";
    let directory = lawful_project(&[
        ("app/Tenant/Store/Module.sharp", module),
        ("app/Tenant/Store/Receipts.sharp", receipts),
        ("app/Tenant/Store/Refunds.sharp", refunds),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
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
        printed(&output).contains(
            "app/Tenant/Store/Module.sharp:5:5:error - law-unproven: law receiptMatchesRefunds is not proven"
        ),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Tenant/Store/Module.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists());
    assert!(compiled(root, "app/Tenant/Store/Refunds.sharp").exists());
}

/// `app/Shop/Module.sharp`, the folder's module, with one law.
const SHOP_MODULE: &str = "namespace App.Shop;\n\nmodule\n{\n    law same(int a) => a == a;\n}\n";

/// Spec section 32: a namespace's module is the one its folder's `Module.sharp` declares, and only it states laws. A
/// misplaced `module` in another file of the folder is refused on that file alone, and `Module.sharp` still compiles
/// and proves its law.
#[test]
fn compile_refuses_a_module_outside_module_sharp_on_its_own_file_and_proves_module_sharp() {
    for (other, code, refusal) in [
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule : App.Shop\n{\n    law same(int a) => a == a;\n}\n\npublic class CheckoutTest { }\n",
            "app/Shop/CheckoutTest.sharp:5:5:error - semantics: A module that overrides another states no laws: state it in its folder's Module.sharp.",
        ),
        (
            "app/Shop/CheckoutTest.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/CheckoutTest.sharp:3:1:error - semantics: A module is declared in its folder's Module.sharp.",
        ),
        (
            "app/Shop/Module.Production.sharp",
            "namespace App.Shop;\n\nmodule\n{\n}\n",
            "app/Shop/Module.Production.sharp:3:1:error - semantics: An environment module overrides its folder's module: write `module : App.Shop`.",
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

/// `app/Tenant/Store/Receipts.sharp`, which imports nothing.
const RECEIPTS: &str = "namespace App.Tenant.Store;\n\npublic class Receipts\n{\n    public static int refundable(int paid, int refunded) => paid - refunded;\n}\n";

/// Spec section 32's `imports` line: `App.Tenant.Store` imports only from the namespaces its `Module.sharp` lists, so
/// `mago compile` refuses `StoreService.sharp` at its import outside the list and writes every other file. Lists that
/// name each other refuse each module of the cycle at its `imports` line, and so does a listed name that names no
/// namespace. Each row is the `Module.sharp` files beside the classes the store reaches.
#[test]
fn compile_refuses_an_import_outside_the_list_of_its_namespace_at_the_import() {
    let store_module = |listed: &str| format!("namespace App.Tenant.Store;\n\nmodule\n{{\n    imports {listed};\n}}\n");
    let listed = store_module("App.Tenant.Community, App.Shared.Schema, Illuminate.Support");
    let misspelled = store_module("App.Tenant.Community, App.Shared.Schema, Illuminate.Suport");
    let cyclic = store_module("App.Tenant.Community, App.Shared");
    let schema_module = "namespace App.Shared.Schema;\n\nmodule\n{\n    imports App.Tenant.Store;\n}\n";
    let service = "namespace App.Tenant.Store;\n\nimport App.Tenant.Community.Member;\nimport Illuminate.Support.Facades.DB;\nimport App.Shared.Ledger.Entry;\n\npublic class StoreService\n{\n    public StoreService(private Member member, private DB db, private Entry entry) { }\n}\n";
    let refused = "app/Tenant/Store/StoreService.sharp:5:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Tenant.Community`, `App.Shared.Schema` and `Illuminate.Support`.";
    for (modules, expected) in [
        (vec![("app/Tenant/Store/Module.sharp", listed.as_str())], vec![refused.to_owned()]),
        (
            vec![("app/Tenant/Store/Module.sharp", misspelled.as_str())],
            vec![
                "app/Tenant/Store/Module.sharp:5:54:error - non-existent-use-import: Imported namespace `Illuminate.Suport` does not exist.".to_owned(),
                "app/Tenant/Store/StoreService.sharp:4:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Tenant.Community`, `App.Shared.Schema` and `Illuminate.Suport`.".to_owned(),
                "app/Tenant/Store/StoreService.sharp:5:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Tenant.Community`, `App.Shared.Schema` and `Illuminate.Suport`.".to_owned(),
            ],
        ),
        (
            vec![("app/Tenant/Store/Module.sharp", cyclic.as_str()), ("app/Shared/Schema/Module.sharp", schema_module)],
            vec![
                "app/Shared/Schema/Module.sharp:5:13:error - import-cycle: `App.Shared.Schema` and `App.Tenant.Store` import from each other: `App.Shared.Schema` lists `App.Tenant.Store`, and `App.Tenant.Store` lists `App.Shared`.".to_owned(),
                "app/Tenant/Store/Module.sharp:5:35:error - import-cycle: `App.Tenant.Store` and `App.Shared.Schema` import from each other: `App.Tenant.Store` lists `App.Shared`, and `App.Shared.Schema` lists `App.Tenant.Store`.".to_owned(),
                "app/Tenant/Store/StoreService.sharp:4:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Tenant.Community` and `App.Shared`.".to_owned(),
            ],
        ),
    ] {
        let mut files = vec![
            ("app/Tenant/Community/Member.sharp", "namespace App.Tenant.Community;\n\npublic class Member\n{\n}\n"),
            ("app/Shared/Schema/Table.sharp", "namespace App.Shared.Schema;\n\npublic class Table\n{\n}\n"),
            ("app/Shared/Ledger/Entry.sharp", "namespace App.Shared.Ledger;\n\npublic class Entry\n{\n}\n"),
            (
                "vendor/laravel/framework/src/Illuminate/Support/Facades/DB.php",
                "<?php\n\nnamespace Illuminate\\Support\\Facades;\n\nclass DB\n{\n}\n",
            ),
            ("app/Tenant/Store/Receipts.sharp", RECEIPTS),
            ("app/Tenant/Store/StoreService.sharp", service),
        ];
        files.extend(modules.iter().copied());
        let directory = lawful_project(&files);
        let root = directory.path();

        let output = compile_lines(root);

        assert_eq!(output.status.code(), Some(1), "{modules:?}: {}", printed(&output));
        assert_eq!(issue_lines(&output), expected, "{modules:?}: {}", printed(&output));
        assert!(compiled(root, "app/Tenant/Store/Receipts.sharp").exists(), "{modules:?}");
    }
}

/// Each issue line `mago compile` printed, in its order.
fn issue_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(":error - "))
        .map(str::to_owned)
        .collect()
}

/// `mago compile` with each issue on one line, as `compile_lines` runs it, with `path` as its `PATH`, under a
/// processor-time limit of `seconds`, which each Lean process it starts inherits, so a row reaches the limit long before
/// 120 seconds and the deadline long before 480 seconds, and the processor seconds it and every process it waited for
/// used. The limit is soft, as `ulimit -S -t` sets it. Mago's own limit on each Lean process is `library.rs`'s
/// `each_process_a_lake_starts_runs_under_the_processor_time_limit`'s to prove: Mago takes the lower of 120 seconds and
/// this limit, so no compile can tell the two apart.
#[cfg(unix)]
// `wait4` reaps the compile, which clippy can't see.
#[allow(clippy::zombie_processes)]
fn compile_lines_within(root: &Path, seconds: u64, path: &std::ffi::OsStr) -> (Output, f64) {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::process::Stdio;

    let mut command = command(root, "never");
    command.env("MAGO_REPORTING_FORMAT", "emacs").env("PATH", path).stdout(Stdio::piped()).stderr(Stdio::piped());
    // SAFETY: `getrlimit` and `setrlimit` are async-signal-safe, and the closure allocates nothing.
    unsafe {
        command.pre_exec(move || {
            let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            if libc::getrlimit(libc::RLIMIT_CPU, &raw mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = seconds;
            if libc::setrlimit(libc::RLIMIT_CPU, &raw const limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    };

    let mut child = command.spawn().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let mut stdout = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut stdout).unwrap();
    // `wait4` reports what the compile used with every process it waited for, and no other test's processes.
    let pid = i32::try_from(child.id()).unwrap();
    let mut status = 0;
    // SAFETY: `rusage` is plain data, which zeroes make valid.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: `status` and `usage` are valid for `wait4` to fill, and `pid` is a child no one else waits for.
    assert_eq!(unsafe { libc::wait4(pid, &raw mut status, 0, &raw mut usage) }, pid);
    let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1_000_000.0;

    (
        Output { status: ExitStatus::from_raw(status), stdout, stderr: stderr.join().unwrap() },
        seconds(usage.ru_utime) + seconds(usage.ru_stime),
    )
}

/// Puts a `lake` first on the `PATH` that runs the real one, except in a project's `.sharp/.lean` package: there a build
/// fails at once, and `lake env lean`, which elaborates a module that did not build, leaves `stalled` in `folder` and
/// runs the shell commands `work`, which stand in for Lean work that never ends. Its own path, `bin/lake` in `folder`,
/// names each process it runs `work` in.
#[cfg(unix)]
fn stalling_lake(folder: &Path, work: &str) -> std::ffi::OsString {
    use std::os::unix::fs::PermissionsExt;

    let path = std::env::var_os("PATH").unwrap();
    let real =
        std::env::split_paths(&path).map(|directory| directory.join("lake")).find(|lake| lake.is_file()).unwrap();
    let script = format!(
        "#!/bin/sh\ncase \"$PWD\" in\n  */.sharp/.lean)\n    case \"$1 $2\" in\n      \"build \"*) exit 1 ;;\n      \"env lean\")\n        touch '{stalled}'\n        {work}\n        ;;\n    esac\n    ;;\nesac\nexec '{real}' \"$@\"\n",
        stalled = folder.join("stalled").display(),
        real = real.display(),
    );
    let lake = folder.join("bin/lake");
    write(folder, "bin/lake", &script);
    std::fs::set_permissions(&lake, std::fs::Permissions::from_mode(0o755)).unwrap();

    std::env::join_paths(std::iter::once(folder.join("bin")).chain(std::env::split_paths(&path))).unwrap()
}

/// Lean work that runs past the processor-time limit, or waits until the deadline, stops there once instead of holding
/// the compile, and refuses the law whose code it was checking with what stopped it. Each row runs after a compile that
/// built the runtime library. Work that never ends runs under a limit of 30 seconds, and costs the compile less than two
/// limits, which leaves room for the rest of the compile on a slow machine. Work that waits runs under a limit of 10
/// seconds, so its deadline is 40 seconds. It uses almost no processor time, so it runs on its own thread meanwhile.
#[cfg(unix)]
#[test]
fn lean_work_that_never_ends_stops_once_at_its_limit_or_deadline() {
    compile_succeeds(lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]).path());
    let refused = |why: &str| {
        vec![format!(
            "app/Shared/Money.sharp:8:5:error - law-unproven: Law addKeepsCurrency is not proven: {why}. Make the code it reaches end sooner."
        )]
    };
    let stops = |limit: u64, work: &str, expected: &[String]| {
        let lake = tempfile::tempdir().unwrap();
        let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);

        let (output, seconds) = compile_lines_within(directory.path(), limit, &stalling_lake(lake.path(), work));

        assert!(lake.path().join("stalled").exists(), "{work}: {}", printed(&output));
        assert_eq!(issue_lines(&output), expected, "{work}, after {seconds} processor seconds: {}", printed(&output));
        assert!(seconds < 2.0 * limit as f64, "{work} used {seconds} processor seconds: {}", printed(&output));
        assert!(!compiled(directory.path(), "app/Shared/Money.sharp").exists(), "{work}");
    };

    std::thread::scope(|scope| {
        let waits = scope.spawn(|| stops(10, "sleep 4000", &refused("it waited 40 seconds without finishing")));
        stops(
            30,
            "exec awk 'BEGIN { while (1) { } }'",
            &refused("it ran past 30 processor seconds, the limit the checker sets on each check"),
        );
        waits.join().unwrap();
    });
}

/// Each signal that ends a compile from a terminal or `kill` ends the Lean it waits on too, though Mago runs Lake in a
/// process group of its own so it can end Lake's at the deadline: the signal reaches the compile's group, and Mago
/// ends Lake's before it ends. Its rows are Ctrl-C's SIGINT, Ctrl-\'s SIGQUIT, a closed terminal's SIGHUP, and `kill`'s
/// SIGTERM.
#[cfg(unix)]
#[test]
fn a_signal_that_ends_a_compile_ends_the_lean_it_waits_on() {
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Stdio;
    use std::time::Duration;
    use std::time::Instant;

    for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
        let lake = tempfile::tempdir().unwrap();
        let path = stalling_lake(lake.path(), "while :; do sleep 1; done");
        let script = lake.path().join("bin/lake").display().to_string();
        let leans = || String::from_utf8(Command::new("pgrep").args(["-f", &script]).output().unwrap().stdout).unwrap();
        let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
        let mut compiling = command(directory.path(), "never");
        compiling.env("PATH", &path).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
        // SAFETY: `setrlimit` is async-signal-safe, and the closure allocates nothing.
        unsafe {
            compiling.pre_exec(|| {
                // SIGQUIT dumps no core.
                let none = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
                if libc::setrlimit(libc::RLIMIT_CORE, &raw const none) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
        };
        let mut compile = compiling.spawn().unwrap();
        let started = Instant::now();
        while !lake.path().join("stalled").exists() {
            assert!(started.elapsed() < Duration::from_secs(300), "{signal}: the stalled Lean never started");
            std::thread::sleep(Duration::from_millis(100));
        }

        // SAFETY: `kill` only sends a signal, to the group the compile leads, as a terminal does.
        assert_eq!(unsafe { libc::kill(-i32::try_from(compile.id()).unwrap(), signal) }, 0);

        assert_eq!(compile.wait().unwrap().signal(), Some(signal));
        let ended = Instant::now();
        while !leans().is_empty() && ended.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let left = leans();
        if !left.is_empty() {
            Command::new("pkill").args(["-f", &script]).status().unwrap();
        }
        assert_eq!(left, "", "{signal}: Lean outlived the compile");
    }
}

#[test]
fn compile_proves_a_law_over_locals_an_if_a_nullable_and_strings() {
    assert_proven(
        "Label",
        "namespace App.Shared;\n\npublic class Label\n{\n    public static string tag(string? name, bool loud)\n    {\n        let text = name ?? \"item\";\n        if (loud) {\n            text = text + \"!\";\n        }\n        return text;\n    }\n\n    law quietKeepsName(string name) => Label.tag(name, false) == name;\n}\n",
        "law loudKeepsName(string name) => Label.tag(name, true) == name;",
    );
}

/// A package's law is proven from its `.sharp` file as a law of the project is, and the compile writes nothing under
/// `vendor/`.
#[test]
fn compile_proves_a_package_law_and_writes_nothing_under_vendor() {
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

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "vendor/acme/money/src/Money.sharp").exists());
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
        "variant = \"strict\"\n\n[[entries.\"app/Shared/Money.sharp\".issues]]\ncode = \"law-unproven\"\nstart_line = 8\nend_line = 8\n",
    );

    for (suppression, directory) in [
        ("a pragma", workspace("// @mago-expect analysis:law-unproven", "")),
        ("an ignore entry", workspace("", "ignore = [\"law-unproven\"]")),
        ("a baseline entry", baselined),
    ] {
        let output = compile_lines(directory.path());

        assert_eq!(output.status.code(), Some(1), "{suppression}: {}", printed(&output));
        assert!(
            printed(&output).contains("error - law-unproven: law addKeepsCurrency"),
            "{suppression}: {}",
            printed(&output)
        );
        assert!(!compiled(directory.path(), "app/Shared/Money.sharp").exists(), "{suppression}");
    }
}

/// Compiles `source` as `app/Shared/Shape.sharp` and checks that the compile refuses it with the unmodeled-construct
/// error `message`, before any Lean process starts.
fn assert_unmodeled(source: &str, message: &str) {
    assert_unmodeled_beside(source, &[], message);
}

/// [`assert_unmodeled`] with the project's `others` files, each a path and its code, beside `app/Shared/Shape.sharp`.
fn assert_unmodeled_beside(source: &str, others: &[(&str, &str)], message: &str) {
    let files: Vec<(&str, &str)> =
        [("app/Shared/Shape.sharp", source)].into_iter().chain(others.iter().copied()).collect();
    let directory = lawful_project(&files);
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
        "Law ratioBelowOne reaches a float in Shape.ratio. A law cannot reason about float arithmetic, which rounds; use int.",
    );
}

#[test]
fn a_law_reaching_an_overridable_method_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public virtual int sides() => 3;\n\n    law hasSides(Shape s) => s.sides() > 0;\n}\n",
        "Law hasSides reaches a call to the overridable method Shape.sides in Shape.hasSides. A law cannot know which implementation runs.",
    );
}

#[test]
fn a_law_testing_the_identity_of_objects_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law same(Shape a, Shape b) => a === b;\n}\n",
        "Law same reaches the identity test `===` in Shape.same. A law sees an object as its values, so it has no identity to compare.",
    );
}

/// `==` on two Lists runs the elements' class operator `==` on each pair, as the bridge lowers it, so a law over it is
/// refused: Lean's own `==` on Lists would compare every value the objects hold and prove this law, which is false
/// when two amounts match and the currencies differ.
#[test]
fn a_law_comparing_lists_of_objects_a_class_operator_compares_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int amount { get; }, public string currency { get; }) { }\n    public static bool operator ==(Shape a, Shape b) => a.amount == b.amount;\n    public int hash() => this.amount;\n\n    law sameListSameCurrency(Shape a, Shape b) => !([a] == [b]) || a.currency == b.currency;\n}\n",
        "Law sameListSameCurrency reaches the class operator `==` in Shape.sameListSameCurrency. Compare the values the objects hold, such as their properties.",
    );
}

#[test]
fn a_law_reaching_a_try_catch_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\nimport DivisionByZeroError;\n\npublic class Shape\n{\n    public static int share(int a)\n    {\n        try {\n            return 10 / a;\n        } catch (DivisionByZeroError e) {\n            return 0;\n        }\n    }\n\n    law shared(int a) => Shape.share(a) >= 0;\n}\n",
        "Law shared reaches a try/catch in Shape.share. Return a value that tells the failure apart, such as `null` or an enum case, in place of the exception.",
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
        "Law one reaches the method Shape.rec in Shape.rec. The checker gives every class a member of that name, so a law cannot reach a member that has it.",
    );
}

#[test]
fn a_law_reaching_a_map_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law sized(Map<string, int> items) => true;\n}\n",
        "Law sized reaches a collection in Shape.sized. Hold the values in a List, and read it with `count()`, `map`, `filter`, a list pattern or a spread.",
    );
}

/// A `List` is Lean's `List`, and its `map`, `filter` and `count` are Lean's `List.map`, `List.filter` and
/// `List.length`, so Mago proves a law over them with the proof it creates.
#[test]
fn compile_proves_a_law_over_map_filter_and_count() {
    assert_proven(
        "Basket",
        "namespace App.Shared;\n\npublic class Basket\n{\n    public static List<bool> flags(List<int> items) => items.map(x => x > 0);\n\n    public static List<int> positives(List<int> items) => items.filter(x => x > 0);\n\n    law flagsKeepCount(List<int> items) => Basket.flags(items).count() == items.count();\n\n    law positivesDropTheRest(int unused) => Basket.positives([1, -2, 3]) == [1, 3];\n}\n",
        "law positivesKeepAll(List<int> items) => Basket.positives(items).count() == items.count();",
    );
}

/// Compiles `source` as `app/Shared/<class>.sharp` and checks that the compile proves every law it states with the proof
/// it creates, and that the class with `twin` added, a false law over the same code, is refused as `law-unproven`.
fn assert_proven(class: &str, source: &str, twin: &str) {
    let name = format!("app/Shared/{class}.sharp");
    let Some(end) = source.rfind('}') else {
        panic!("{class} has no closing brace");
    };
    let with_twin = format!("{}    {twin}\n{}", &source[..end], &source[end..]);
    let Some(law) = twin.strip_prefix("law ").and_then(|twin| twin.split('(').next()) else {
        panic!("{twin} is no law");
    };

    for (source, proven) in [(source, true), (with_twin.as_str(), false)] {
        let directory = lawful_project(&[(&name, source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{source}: {}", printed(&output));
        assert_eq!(compiled(root, &name).exists(), proven);
        if !proven {
            assert!(
                printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven")),
                "{}",
                printed(&output)
            );
        }
    }
}

/// Spec section 28.1's `total`: a method that calls itself on a list's rest from a list pattern gets smaller on each call.
#[test]
fn compile_proves_a_law_over_recursion_on_a_list_rest() {
    assert_proven(
        "Money",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n\n    public static int total(List<Money> items) => match (items) {\n        [] => 0,\n        [Money first, ...List<Money> rest] => first.amount + Money.total(rest),\n        default => 0,\n    };\n\n    law totalOfTwo(int unused) => Money.total([new Money(1, \"EUR\"), new Money(2, \"EUR\")]) == 3;\n}\n",
        "law totalOfTwoIsFour(int unused) => Money.total([new Money(1, \"EUR\"), new Money(2, \"EUR\")]) == 4;",
    );
}

/// Spec section 28.1's `times`: `n - 1` where `n <= 0` failed is one step towards the bound 0.
#[test]
fn compile_proves_a_law_over_recursion_on_an_int_one_step_towards_a_bound_a_guard_tested() {
    assert_proven(
        "Money",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n    public Money times(int n) => n <= 0 ? new Money(0, this.currency) : this.add(this.times(n - 1));\n\n    law zeroTimesIsZero(Money m) => m.times(0).amount == 0;\n}\n",
        "law oneTimeIsZero(Money m) => m.times(1).amount == 0;",
    );
}

/// An `if` that returns, a `match` arm's comparison, and a `?:` each show the bound a step moves towards, a literal or a
/// parameter the call passes on unchanged.
#[test]
fn compile_proves_a_law_over_recursion_towards_a_bound_an_if_a_match_or_a_parameter_sets() {
    assert_proven(
        "Steps",
        "namespace App.Shared;\n\npublic class Steps\n{\n    public static int walk(int n)\n    {\n        if (n <= 0) {\n            return 0;\n        }\n        return 1 + Steps.walk(n - 1);\n    }\n\n    public static int fall(int n) => match (n) {\n        <= 0 => 0,\n        default => Steps.fall(n - 1),\n    };\n\n    public static int climb(int i, int top) => i >= top ? 0 : 1 + Steps.climb(i + 1, top);\n\n    law twoSteps(int unused) => Steps.walk(2) == 2;\n    law fallsToZero(int unused) => Steps.fall(2) == 0;\n    law climbsTwo(int unused) => Steps.climb(0, 2) == 2;\n}\n",
        "law climbsThree(int unused) => Steps.climb(0, 2) == 3;",
    );
}

/// Two methods of one class that call each other on `n - 1` under the same bound are one Lean `mutual` block.
#[test]
fn compile_proves_a_law_over_mutual_recursion() {
    assert_proven(
        "Parity",
        "namespace App.Shared;\n\npublic class Parity\n{\n    public static bool isEven(int n) => n <= 0 ? true : Parity.isOdd(n - 1);\n    public static bool isOdd(int n) => n <= 0 ? false : Parity.isEven(n - 1);\n\n    law fourIsEven(int unused) => Parity.isEven(4);\n}\n",
        "law threeIsEven(int unused) => Parity.isEven(3);",
    );
}

/// A list pattern names an element or only its type, and has a rest anywhere: before the last element, or bare.
#[test]
fn compile_proves_a_law_over_list_patterns_with_a_binding_arm_and_a_type_only_arm() {
    assert_proven(
        "Basket",
        "namespace App.Shared;\n\npublic class Basket\n{\n    public Basket(public int amount { get; }) { }\n\n    public static int size(List<Basket> items) => match (items) {\n        [] => 0,\n        [Basket, ...List<Basket> rest] => 1 + Basket.size(rest),\n        default => 0,\n    };\n\n    public static int firstAmount(List<Basket> items) => match (items) {\n        [Basket first, ...] => first.amount,\n        default => 0,\n    };\n\n    public static int lastAmount(List<Basket> items) => match (items) {\n        [..., Basket last] => last.amount,\n        default => 0,\n    };\n\n    public static int backwards(List<Basket> items) => match (items) {\n        [...List<Basket> before, Basket last] => last.amount + Basket.backwards(before),\n        default => 0,\n    };\n\n    law sizeOfTwo(int unused) => Basket.size([new Basket(1), new Basket(2)]) == 2;\n    law firstOfTwo(int unused) => Basket.firstAmount([new Basket(1), new Basket(2)]) == 1;\n    law lastOfTwo(int unused) => Basket.lastAmount([new Basket(1), new Basket(2)]) == 2;\n    law backwardsOfTwo(int unused) => Basket.backwards([new Basket(1), new Basket(2)]) == 3;\n}\n",
        "law lastOfTwoIsFirst(int unused) => Basket.lastAmount([new Basket(1), new Basket(2)]) == 1;",
    );
}

#[test]
fn compile_proves_a_law_over_a_match_statement_with_block_arms_and_a_when_guard() {
    assert_proven(
        "Sign",
        "namespace App.Shared;\n\npublic class Sign\n{\n    public static int of(int n)\n    {\n        match (n) {\n            int m when m > 0 => { return 1; },\n            0 => { return 0; },\n            default => { return -1; },\n        }\n    }\n\n    law positive(int unused) => Sign.of(5) == 1;\n    law zero(int unused) => Sign.of(0) == 0;\n}\n",
        "law negativeIsZero(int unused) => Sign.of(-5) == 0;",
    );
}

/// `is` tests the type a value of a nullable type holds when it is not null, and names it in the `?:` it decides.
#[test]
fn compile_proves_a_law_over_an_is_test() {
    assert_proven(
        "Order",
        "namespace App.Shared;\n\npublic class Order\n{\n    public Order(public int total { get; }) { }\n\n    public static bool known(Order? order) => order is Order;\n    public static int totalOf(Order? order) => order is Order o ? o.total : 0;\n\n    law nullIsUnknown(int unused) => !Order.known(null);\n    law nullTotalIsZero(int unused) => Order.totalOf(null) == 0;\n    law totalIsKept(Order order) => Order.totalOf(order) == order.total;\n}\n",
        "law totalIsZero(Order order) => Order.totalOf(order) == 0;",
    );
}

#[test]
fn compile_proves_a_law_over_a_spread() {
    assert_proven(
        "Joiner",
        "namespace App.Shared;\n\npublic class Joiner\n{\n    public static List<int> joined(List<int> a, List<int> b) => [...a, 0, ...b];\n\n    law joinedKeepsCount(List<int> a, List<int> b) => Joiner.joined(a, b).count() == a.count() + b.count() + 1;\n}\n",
        "law joinedAddsNothing(List<int> a, List<int> b) => Joiner.joined(a, b).count() == a.count() + b.count();",
    );
}

/// `count()` is a value, so a lambda, a `when` guard and a `?:` condition read it.
#[test]
fn compile_proves_a_law_over_a_count_in_a_lambda_a_guard_and_a_condition() {
    assert_proven(
        "Lists",
        "namespace App.Shared;\n\npublic class Lists\n{\n    public static List<List<int>> nonEmpty(List<List<int>> lists) => lists.filter(list => list.count() > 0);\n\n    public static int size(List<int> xs) => match (xs) {\n        [int first, ...] when xs.count() > 2 => first,\n        default => xs.count() > 1 ? 1 : 0,\n    };\n\n    law dropsEmpty(int unused) => Lists.nonEmpty([[1], [2, 3]]).count() == 2;\n    law longFirst(int unused) => Lists.size([5, 6, 7]) == 5;\n    law shortIsOne(int unused) => Lists.size([5, 6]) == 1;\n}\n",
        "law keepsEmpty(int unused) => Lists.nonEmpty([[1], []]).count() == 2;",
    );
}

/// `[]` is the empty List wherever the code expects a List: an argument, a returned value, and a compared one.
#[test]
fn compile_proves_a_law_over_an_empty_list() {
    assert_proven(
        "More",
        "namespace App.Shared;\n\npublic class More\n{\n    public static int sizeOf(List<int> xs) => match (xs) {\n        [] => 0,\n        default => xs.count() > 2 ? 2 : 1,\n    };\n\n    public static List<int> none(int unused) => [];\n\n    law sizeOfEmpty(int unused) => More.sizeOf([]) == 0;\n    law noneIsEmpty(int unused) => More.none(0).count() == 0;\n    law noneIsTheEmptyList(int unused) => More.none(0) == [];\n}\n",
        "law sizeOfEmptyIsOne(int unused) => More.sizeOf([]) == 1;",
    );
}

/// A loop that only counts is the list's length.
#[test]
fn compile_proves_a_law_over_a_counting_loop() {
    assert_proven(
        "Orders",
        "namespace App.Shared;\n\npublic class Orders\n{\n    public static int size(List<int> orders)\n    {\n        let count = 0;\n        for (const order of orders) {\n            count++;\n        }\n        return count;\n    }\n\n    law sizeIsCount(List<int> orders) => Orders.size(orders) == orders.count();\n}\n",
        "law sizeIsOneMore(List<int> orders) => Orders.size(orders) == orders.count() + 1;",
    );
}

/// A loop that adds each element is `List.sum`, so Lean's `List.sum_append` proves that it joins.
#[test]
fn compile_proves_a_law_over_a_summing_loop() {
    assert_proven(
        "Sums",
        "namespace App.Shared;\n\npublic class Sums\n{\n    public static int sumOf(List<int> xs)\n    {\n        let sum = 0;\n        for (const x of xs) {\n            sum += x;\n        }\n        return sum;\n    }\n\n    law sumJoins(List<int> a, List<int> b) => Sums.sumOf([...a, ...b]) == Sums.sumOf(a) + Sums.sumOf(b);\n}\n",
        "law sumIsTheFirst(List<int> a, List<int> b) => Sums.sumOf([...a, ...b]) == Sums.sumOf(a);",
    );
}

/// A loop that counts the elements an `if` tests is `List.countP`, so Lean's `List.countP_le_length` proves it counts no
/// more than the list holds.
#[test]
fn compile_proves_a_law_over_a_counting_loop_under_an_if() {
    assert_proven(
        "Order",
        "namespace App.Shared;\n\npublic class Order\n{\n    public Order(public bool open { get; }) { }\n\n    public static int openCount(List<Order> orders)\n    {\n        let count = 0;\n        for (const o of orders) {\n            if (o.open) {\n                count += 1;\n            }\n        }\n        return count;\n    }\n\n    law countAtMost(List<Order> orders) => Order.openCount(orders) <= orders.count();\n}\n",
        "law countAll(List<Order> orders) => Order.openCount(orders) == orders.count();",
    );
}

/// Spec section 28's `Money`: a law the checker proves alone, a law its body proves by recursion on a smaller int, one
/// proven by recursion on a list's rest, and one under a `when` condition.
const SPECIMEN_MONEY: &str = "namespace App.Shared;\n\nimport Sharp.Math.Math;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n    public Money times(int n) => n <= 0 ? new Money(0, this.currency) : this.add(this.times(n - 1));\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n    law timesKeepsCurrency(Money m, int n) => m.times(n).currency == m.currency\n    {\n        if (n <= 0) {\n            return;\n        }\n        Money.timesKeepsCurrency(m, n - 1);\n        Money.addKeepsCurrency(m, m.times(n - 1));\n    }\n\n    public static Money sum(Money total, List<Money> items) => match (items) {\n        [] => total,\n        [Money first, ...List<Money> rest] => Money.sum(total.add(first), rest),\n        default => total,\n    };\n\n    law sumKeepsCurrency(Money total, List<Money> items) => Money.sum(total, items).currency == total.currency\n    {\n        match (items) {\n            [] => { return; },\n            [Money first, ...List<Money> rest] => {\n                Money.sumKeepsCurrency(total.add(first), rest);\n                Money.addKeepsCurrency(total, first);\n            },\n            default => { return; },\n        }\n    }\n\n    public static int refundable(int paid, int refunded) => Math.max(paid - refunded, 0);\n\n    law neverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0\n        => Money.refundable(paid, refunded) <= paid;\n}\n";

/// A project that installs the whole standard library, as every project does, holding `files`.
fn library_project(files: &[(&str, &str)]) -> tempfile::TempDir {
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

    directory
}

/// The standard library's `Math.max` and `Math.min` on ints are pure and reach laws with no `extern`: the checker
/// knows what they compute, with a spread argument as with one written out, and a claim they break is not proven.
#[test]
fn a_law_reasons_about_math_max_and_math_min_as_the_largest_and_the_smallest_int() {
    let bounds = |law: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Math.Math;\n\npublic class Bounds\n{{\n    public static int clamp(int x, int low, int high) => Math.min(Math.max(x, low), high);\n    public static int largest(int first, List<int> rest) => Math.max(first, ...rest);\n\n    {law}\n}}\n"
        )
    };
    for (law, proven) in [
        ("law clampAtMostHigh(int x, int low, int high) => Bounds.clamp(x, low, high) <= high;", true),
        ("law clampAtLeastLow(int x, int low, int high) when low <= high => Bounds.clamp(x, low, high) >= low;", true),
        ("law largestOfThree(int unused) => Bounds.largest(1, [5, 3]) == 5;", true),
        ("law clampAtMostLow(int x, int low, int high) => Bounds.clamp(x, low, high) <= low;", false),
    ] {
        let directory = library_project(&[("app/Shared/Bounds.sharp", &bounds(law))]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{law}: {}", printed(&output));
        if !proven {
            assert!(
                printed(&output).contains("error[law-unproven]: law clampAtMostLow is not proven"),
                "{}",
                printed(&output)
            );
        }
    }
}

/// Spec section 28: every law of a project with only `.sharp` files is proven, and no `.lean` file is in the project
/// before the compile or after it.
#[test]
fn compile_proves_spec_section_28_s_money_from_its_sharp_file_alone() {
    let directory = library_project(&[("app/Shared/Money.sharp", SPECIMEN_MONEY)]);
    let root = directory.path();
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());
}

/// Spec section 28: a `match` on an int parameter shows its bound as an `if` does: a comparison arm where it holds, and
/// in each arm after it, where it failed. The analyzer and the proof read one walk of the body's paths, so the call of
/// the law itself on `n - 1` after `<= 0` is smaller in both.
#[test]
fn compile_proves_a_law_whose_body_tests_its_bound_with_a_comparison_arm() {
    let money = SPECIMEN_MONEY.replace(
        "        if (n <= 0) {\n            return;\n        }\n        Money.timesKeepsCurrency(m, n - 1);\n        Money.addKeepsCurrency(m, m.times(n - 1));\n",
        "        match (n) {\n            <= 0 => { return; },\n            default => {\n                Money.timesKeepsCurrency(m, n - 1);\n                Money.addKeepsCurrency(m, m.times(n - 1));\n            },\n        }\n",
    );
    assert_ne!(money, SPECIMEN_MONEY);
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// Spec section 28: a case the body leaves open is `law-unproven`, with the case's conditions and the goal still to
/// show in PHP#, and the call of the law itself on a smaller value when the case makes none.
#[test]
fn a_case_a_law_s_body_leaves_open_names_its_conditions_and_its_goal_in_php_sharp() {
    let without_add = SPECIMEN_MONEY.replace("        Money.addKeepsCurrency(m, m.times(n - 1));\n", "");
    let without_calls = without_add.replace("        Money.timesKeepsCurrency(m, n - 1);\n", "");
    let without_sum_calls = SPECIMEN_MONEY.replace(
        "                Money.sumKeepsCurrency(total.add(first), rest);\n                Money.addKeepsCurrency(total, first);\n",
        "",
    );
    for (source, expected) in [
        (
            without_sum_calls.as_str(),
            "app/Shared/Money.sharp:27:5: error[law-unproven]: law sumKeepsCurrency is not proven\n = when items is [Money first, ...List<Money> rest], still to show: Money.sum(total.add(first), rest).currency == total.currency\n = Help: the law for a smaller items is the call Money.sumKeepsCurrency(total, rest)\n",
        ),
        (
            without_add.as_str(),
            "app/Shared/Money.sharp:12:5: error[law-unproven]: law timesKeepsCurrency is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).currency == m.currency\n",
        ),
        (
            without_calls.as_str(),
            "app/Shared/Money.sharp:12:5: error[law-unproven]: law timesKeepsCurrency is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).currency == m.currency\n = Help: the law for a smaller n is the call Money.timesKeepsCurrency(m, n - 1)\n",
        ),
    ] {
        let directory = library_project(&[("app/Shared/Money.sharp", source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(printed(&output).contains(expected), "{}", printed(&output));
        assert!(!compiled(root, "app/Shared/Money.sharp").exists());
    }
}

/// Spec section 28: every goal prints in PHP#. A goal whose `match` the case leaves open, as `Money.sum`'s on a list
/// it knows nothing of, prints as the claim the law writes.
#[test]
fn a_goal_with_a_match_the_case_leaves_open_prints_as_the_law_s_claim() {
    let money = SPECIMEN_MONEY.replace(
        "    public static int refundable",
        "    law sumIsEuro(Money total, List<Money> items) => Money.sum(total, items).currency == \"EUR\";\n\n    public static int refundable",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law sumIsEuro is not proven\n = still to show: Money.sum(total, items).currency == \"EUR\"\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a law with `when` is called only where its condition holds, so a call where it is not known names
/// the condition still to show.
#[test]
fn a_call_of_a_when_law_where_its_condition_is_not_known_names_the_condition_to_show() {
    let money = SPECIMEN_MONEY.replace(
        "        => Money.refundable(paid, refunded) <= paid;\n",
        "        => Money.refundable(paid, refunded) <= paid;\n    law refundOfAnything(int p, int r) => p < 0 || Money.refundable(p, r) <= p\n    {\n        Money.neverAbovePaid(p, r);\n    }\n",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law refundOfAnything is not proven\n = still to show for the call Money.neverAbovePaid(p, r): p >= 0 && r >= 0\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a call of a `when` law where the conditions on the path show each part of its condition proves,
/// with nothing left to show for the call.
#[test]
fn a_call_of_a_when_law_where_each_part_of_its_condition_is_known_proves() {
    let money = SPECIMEN_MONEY.replace(
        "        => Money.refundable(paid, refunded) <= paid;\n",
        "        => Money.refundable(paid, refunded) <= paid;\n    law refundOfKnown(int p, int r) => p < 0 || r < 0 || Money.refundable(p, r) <= p\n    {\n        if (p < 0) {\n            return;\n        }\n        if (r < 0) {\n            return;\n        }\n        Money.neverAbovePaid(p, r);\n    }\n",
    );
    let directory = library_project(&[("app/Shared/Money.sharp", &money)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
}

/// Spec section 28: a law's `when` condition and its body read a nullable parameter as its claim does, so a value the
/// condition or an `if` tests for null is a value.
#[test]
fn compile_proves_a_law_whose_condition_or_body_reads_a_nullable_parameter() {
    let directory = lawful_project(&[(
        "app/Shared/Box.sharp",
        "namespace App.Shared;\n\npublic class Box\n{\n    law somePositive(int? x) when x != null && x + 1 > 0 => true;\n    law anything(int x) => true;\n    law someAnything(int? x) => true\n    {\n        if (x != null) {\n            Box.anything(x);\n        }\n    }\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Box.sharp").exists());
}

/// A law that calls a law of a file with errors is not proven until those errors are fixed, which the error says, as it
/// says of any code of such a file a law reaches.
#[test]
fn a_call_of_a_law_whose_file_has_errors_names_the_errors_to_fix() {
    let money = LAWFUL_MONEY.replace(
        "    law addKeepsCurrency",
        "    public int broken() => \"text\";\n    law same(int x) => x == x;\n\n    law addKeepsCurrency",
    );
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", &money),
        (
            "app/Shared/Ledger.sharp",
            "namespace App.Shared;\n\npublic class Ledger\n{\n    law usesMoney(int x) => true\n    {\n        Money.same(x);\n    }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Ledger.sharp:5:5:error - law-unproven: Law usesMoney is not proven: it reaches Money.same, whose file has errors. Fix them, then run vendor/bin/mago compile again."
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: laws that call each other in a cycle never end, so the call that closes the cycle is the error of
/// a law that calls itself on no smaller value.
#[test]
fn laws_that_call_each_other_in_a_cycle_are_refused() {
    let cycle = "namespace App.Shared;\n\npublic class Count\n{\n    law up(int x) => true\n    {\n        Count.down(x);\n    }\n    law down(int x) => true\n    {\n        Count.up(x);\n    }\n}\n";
    let directory = lawful_project(&[("app/Shared/Count.sharp", cycle)]);
    let root = directory.path();

    let output = compile_lines(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(
        issue_lines(&output),
        [
            "app/Shared/Count.sharp:7:9:error - law-recursion: a law calls itself only on a smaller value",
            "app/Shared/Count.sharp:11:9:error - law-recursion: a law calls itself only on a smaller value",
        ],
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a `match` in a law's body takes every arm a `match` takes, and each arm is a case that knows its
/// pattern and its `when` guard: a constant, a type pattern that names the value, an enum's cases, a comparison and
/// `default`. A case an arm leaves open names its pattern. Spec section 28's `sumKeepsCurrency` covers a list.
#[test]
fn a_law_body_s_match_splits_its_cases_on_every_arm_a_match_takes() {
    let status = "namespace App.Shared;\n\npublic enum Status\n{\n    case Open;\n    case Paid;\n\n    public int weight() => match (this) {\n        Status.Open => 1,\n        Status.Paid => 2,\n    };\n}\n";
    let signs = |arms: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Signs\n{{\n    public static int sign(int n) => n > 5 ? 2 : (n < 0 ? 0 : 1);\n\n    law signBounded(int n) => Signs.sign(n) >= 0 && Signs.sign(n) <= 2\n    {{\n        match (n) {{\n{arms}        }}\n    }}\n    law weighs(Status s) => s.weight() > 0\n    {{\n        match (s) {{\n            Status.Open => {{ return; }},\n            default => {{ return; }},\n        }}\n    }}\n}}\n"
        )
    };
    let proven = signs(
        "            int m when m > 5 => { return; },\n            < 0 => { return; },\n            0 => { return; },\n            default => { return; },\n",
    );
    let directory = lawful_project(&[("app/Shared/Signs.sharp", &proven), ("app/Shared/Status.sharp", status)]);
    let output = compile(directory.path());
    assert!(output.status.success(), "{}", printed(&output));

    let positive = signs("            int m when m > 5 => { return; },\n            default => { return; },\n")
        .replace(
            "law signBounded(int n) => Signs.sign(n) >= 0 && Signs.sign(n) <= 2",
            "law signBounded(int n) => Signs.sign(n) == 2",
        );
    let directory = lawful_project(&[("app/Shared/Signs.sharp", &positive), ("app/Shared/Status.sharp", status)]);
    let output = compile(directory.path());
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(" = when !(n is int m && m > 5), still to show: (n > 5 ? 2 : n < 0 ? 0 : 1) == 2\n"),
        "{}",
        printed(&output)
    );
}

/// A law's proof splits its cases on a value it can name without running code, so a `match` of any other value is
/// refused with what to match instead.
#[test]
fn a_law_body_s_match_on_a_value_that_runs_code_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    law signs(int n) => true\n    {\n        match (n + 1) {\n            0 => { return; },\n            default => { return; },\n        }\n    }\n}\n",
        "Law signs reaches a match on `n + 1` in Shape.signs. Match a parameter of the law, a value a pattern of its body names, or a property of one.",
    );
}

/// A law reaches only `map`, `filter` and `count` of a `List`, so any other method of one is refused by its name.
#[test]
fn a_law_reaching_another_list_method_is_refused_by_name() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static bool known(List<int> items) => items.contains(3);\n\n    law knows(List<int> items) => Shape.known(items);\n}\n",
        "Law knows reaches the List method contains in Shape.known. Hold the values in a List, and read it with `count()`, `map`, `filter`, a list pattern or a spread.",
    );
}

/// `map` and `filter` take a pure function, so a lambda whose body can throw, as `x * 2` throws on overflow, is
/// refused.
#[test]
fn a_law_reaching_a_lambda_that_can_throw_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static List<int> doubled(List<int> items) => items.map(x => x * 2);\n\n    law same(List<int> items) => Shape.doubled(items).count() == items.count();\n}\n",
        "Law same reaches a lambda whose body can throw in Shape.doubled. Pass a lambda only to a List's `map` or `filter`, with a body that cannot throw, and call a PHP# method in place of any other function value.",
    );
}

#[test]
fn a_law_reaching_a_loop_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int sum(int n)\n    {\n        let total = 0;\n        for (let i = 0; i < n; i++) {\n            total += i;\n        }\n        return total;\n    }\n\n    law summed(int n) => Shape.sum(n) >= 0;\n}\n",
        "Law summed reaches a loop in Shape.sum. Write the method as recursion that calls itself on a smaller value.",
    );
}

#[test]
fn a_law_reaching_a_while_loop_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int steps(int n)\n    {\n        let left = n;\n        let steps = 0;\n        while (left > 0) {\n            left -= 1;\n            steps += 1;\n        }\n        return steps;\n    }\n\n    law counted(int n) => Shape.steps(n) >= 0;\n}\n",
        "Law counted reaches a loop in Shape.steps. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// A `for … of` loop is a law's only when it only adds to int locals, so one that adds to a string is refused.
#[test]
fn a_law_reaching_a_loop_that_writes_a_string_local_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static string joined(List<string> names)\n    {\n        let text = \"\";\n        for (const name of names) {\n            text += name;\n        }\n        return text;\n    }\n\n    law joinedText(List<string> names) => Shape.joined(names) == Shape.joined(names);\n}\n",
        "Law joinedText reaches a loop that does more than count or sum in Shape.joined. Write the method as recursion that calls itself on a smaller value.",
    );
}

/// `n + 1` under `n <= 0` moves away from the bound the condition tested, so the recursion never gets smaller.
#[test]
fn a_law_reaching_a_recursion_that_does_not_get_smaller_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int up(int n) => n <= 0 ? 0 : Shape.up(n + 1);\n\n    law reachesZero(int n) => Shape.up(n) == 0;\n}\n",
        "Law reachesZero reaches a recursion that does not get smaller in Shape.up. Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.",
    );
}

/// Mutual recursion ends only when every call among the methods passes a smaller value, so a call on `n` itself is
/// refused.
#[test]
fn a_law_reaching_a_mutual_recursion_that_does_not_get_smaller_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static bool isEven(int n) => n <= 0 ? true : Shape.isOdd(n - 1);\n    public static bool isOdd(int n) => n <= 0 ? false : Shape.isEven(n);\n\n    law fourIsEven(int unused) => Shape.isEven(4);\n}\n",
        "Law fourIsEven reaches a recursion that does not get smaller in Shape.isOdd. Write the method as recursion that calls itself on a smaller value: a list's rest from a list pattern, or an int one step towards a bound a condition tested.",
    );
}

/// A class whose property holds the class is a Lean structure that holds itself, which Lean refuses.
#[test]
fn a_law_reaching_a_type_that_holds_itself_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int value { get; }, public Shape? next { get; }) { }\n\n    law keepsValue(Shape shape) => shape.value == shape.value;\n}\n",
        "Law keepsValue reaches a type that holds itself in Shape. A law reasons only about a class whose properties never hold a value of the class itself.",
    );
}

/// A value pattern tests a List by `==`, which a `when` guard writes, so the refusal says to write it there.
#[test]
fn a_law_reaching_a_value_pattern_on_a_list_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public const List<int> PAIR = [1, 2];\n\n    public static int pair(List<int> xs) => match (xs) {\n        Shape.PAIR => 1,\n        default => 0,\n    };\n\n    law pairHolds(List<int> xs) => Shape.pair(xs) == Shape.pair(xs);\n}\n",
        "Law pairHolds reaches a match on this subject in Shape.pair. Compare the value with `==` in a `when` guard.",
    );
}

/// Each refusal a law reaches names the construct and says, in one PHP# sentence, what to write in its place, or why
/// the value is out of a law's reach.
#[test]
fn each_refusal_a_law_reaches_says_what_to_write_instead() {
    let source = "namespace App.Shared;\n\nextern InvalidArgumentException;\n\npublic class Shape\n{\n    public Shape(public int value { get; }) { }\n    public static bool operator ==(Shape a, Shape b) => a.value == b.value;\n    public int hash() => this.value;\n\n    public static int union(int|string x) => 1;\n    public static int power(int a) => a ** 2;\n    public static int complement(int a) => ~a;\n    public static bool same(Shape a, Shape b) => a == b;\n    public static string template(int a) => `n ${a}`;\n    public static int guard(int a) => match (a) {\n        int m when m + 1 > 2 => 1,\n        default => 0,\n    };\n    public static int defaulted(int a = 1) => a;\n    public static int callsDefault(int a) => Shape.defaulted();\n    public static int named(int a) => Shape.defaulted(a: a);\n    public static int thrown(int a) => a > 0 ? a : throw new InvalidArgumentException(\"no\");\n    public static int compound(int a)\n    {\n        let x = a;\n        x **= 2;\n        return x;\n    }\n    public static int inner(int a)\n    {\n        let x = 0;\n        return (x = a) + 1;\n    }\n\n    law l1(int a) => Shape.union(a) == 1;\n    law l2(int a) => Shape.power(a) == Shape.power(a);\n    law l3(int a) => Shape.complement(a) == Shape.complement(a);\n    law l4(Shape a) => Shape.same(a, a);\n    law l5(int a) => Shape.template(a) == Shape.template(a);\n    law l6(int a) => Shape.guard(a) == Shape.guard(a);\n    law l7(int a) => Shape.callsDefault(a) == 1;\n    law l8(int a) => Shape.named(a) == a;\n    law l9(int a) => Shape.thrown(a) == Shape.thrown(a);\n    law l10(int a) => Shape.compound(a) == Shape.compound(a);\n    law l11(int a) => Shape.inner(a) == Shape.inner(a);\n}\n";
    let directory = lawful_project(&[("app/Shared/Shape.sharp", source)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the type `int|string` in Shape.union. Give it a type a law reasons about: int, bool, string, a List, or a PHP# class, enum or interface, with or without `?`.",
        "Law l2 reaches the operator `**` here in Shape.power. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints and strings, `== !=`, `! && ||` and `??`.",
        "Law l3 reaches the operator `~` in Shape.complement. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints and strings, `== !=`, `! && ||` and `??`.",
        "Law l4 reaches the class operator `==` in Shape.same. Compare the values the objects hold, such as their properties.",
        "Law l5 reaches a string template in Shape.template. Join the strings with `+`.",
        "Law l6 reaches a guard that can throw in Shape.guard. Write the guard without calls or arithmetic, which can throw, and compute such a value in the arm.",
        "Law l7 reaches a default argument in Shape.callsDefault. Pass every argument, the ones with a default value included.",
        "Law l8 reaches a named or spread argument in Shape.named. Pass each argument by its position.",
        "Law l9 reaches a throw in Shape.thrown. Return a value that tells the failure apart, such as `null` or an enum case, in place of the exception.",
        "Law l10 reaches the operator `**=` in Shape.compound. Update an int with `+=`, `-=`, `*=`, `/=` or `%=`, or a string with `+=`.",
        "Law l11 reaches an assignment inside an expression in Shape.inner. Assign the value in a statement of its own, before the expression that reads it.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
    assert!(!printed.contains("A law cannot reason about it."), "{printed}");
}

/// The refusals of code a law cannot read say what to write instead, and name a type parameter as one and PHP's root
/// namespace as `global.`, never as a plain PHP class.
#[test]
fn each_refusal_of_code_a_law_cannot_read_says_what_to_write_instead() {
    let clock = (
        "vendor/lib/clock/Clock.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Clock\n{\n    public static function hour(): int\n    {\n        return 1;\n    }\n}\n",
    );
    let stub = ("app/Stubs/Clock.sharp", "namespace App.Stubs;\n\nimport Lib.Clock;\n\nextern Clock;\n");
    let shape = (
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\nimport Lib.Clock;\n\npublic class Shape\n{\n    public Shape(public int raw { get; }) { }\n    public int doubled => this.raw * 2;\n\n    public static int parsed(string s) => Int.tryParse(s) ?? 0;\n    public static int hour(int a) => Clock.hour();\n    public static int length(string s) => global.strlen(s);\n    public static int kept<T>(T item) => 1;\n\n    law l1(string s) => Shape.parsed(s) == Shape.parsed(s);\n    law l2(int a) => Shape.hour(a) == Shape.hour(a);\n    law l3(string s) => Shape.length(s) == Shape.length(s);\n    law l4(int a) => Shape.kept(a) == 1;\n    law l5(Shape s) => s.doubled == s.doubled;\n}\n",
    );
    let directory = library_project(&[shape, clock, stub]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the standard library class Int in Shape.parsed. Write the code in a PHP# class of the project: of the standard library, a law reaches a List's `count()`, `map` and `filter`, and `Math.max` and `Math.min` on ints.",
        "Law l2 reaches the plain PHP class Clock in Shape.hour. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l3 reaches the function `global.strlen` of PHP's root namespace in Shape.length. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l4 reaches the type parameter T in Shape.kept. Give it a type a law reasons about: int, bool, string, a List, or a PHP# class, enum or interface, with or without `?`.",
        "Law l5 reaches the accessor of Shape.doubled in Shape.l5. Compute the value in a method, and give the property no accessor body.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
    assert!(!printed.contains("the plain PHP class T"), "{printed}");
}

/// A loop that reads the local it adds to, here in an `if`, does more than count or sum.
#[test]
fn a_law_reaching_a_loop_that_reads_a_local_it_adds_to_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int capped(List<int> xs)\n    {\n        let sum = 0;\n        for (const x of xs) {\n            if (sum < 10) {\n                sum += x;\n            }\n        }\n        return sum;\n    }\n\n    law cappedHolds(List<int> xs) => Shape.capped(xs) == Shape.capped(xs);\n}\n",
        "Law cappedHolds reaches a loop that reads a local it adds to in Shape.capped. Write the method as recursion that calls itself on a smaller value.",
    );
}

#[test]
fn a_law_reaching_a_pattern_that_names_a_value_outside_an_if_or_a_conditional_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int total { get; }) { }\n\n    public static bool big(Shape? shape) => shape is Shape found && found.total > 10;\n\n    law nullIsSmall(int unused) => !Shape.big(null);\n}\n",
        "Law nullIsSmall reaches a pattern that names a value outside the condition of an if or a ?: in Shape.big. Write the test as the condition of an if or a ?:, and read the value it names in the branch it decides.",
    );
}

#[test]
fn a_law_reaching_a_list_pattern_on_a_value_that_is_no_list_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int first(Any? value) => match (value) {\n        [int first, ...] => first,\n        default => 0,\n    };\n\n    law firstOfNull(int unused) => Shape.first(null) == 0;\n}\n",
        "Law firstOfNull reaches a list pattern on a value that is no List in Shape.first. It reads types while the code runs.",
    );
}

#[test]
fn a_law_reaching_a_lambda_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int next(int a)\n    {\n        const step = (int x) => x + 1;\n        return step(a);\n    }\n\n    law grows(int a) => Shape.next(a) != a;\n}\n",
        "Law grows reaches a lambda in Shape.next. Pass a lambda only to a List's `map` or `filter`, with a body that cannot throw, and call a PHP# method in place of any other function value.",
    );
}

#[test]
fn a_law_reaching_an_inherited_class_is_refused() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\npublic class Shape : Base\n{\n    law any(Shape s) => true;\n}\n",
        &[("app/Shared/Base.sharp", "namespace App.Shared;\n\npublic class Base\n{\n}\n")],
        "Law any reaches the class Shape, which extends Base in Shape. Declare the members the law reaches in the class itself, in place of inheriting them.",
    );
}

#[test]
fn a_law_reaching_a_named_constructor_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int sides { get; }) { }\n    public Shape.triangle() : this(3) { }\n\n    law threeSides(int unused) => new Shape.triangle().sides == 3;\n}\n",
        "Law threeSides reaches the named constructor Shape.triangle in Shape.threeSides. Write the constructor with promoted properties and no body, and compute the values in a static method that calls `new`.",
    );
}

#[test]
fn a_law_reaching_an_extension_member_lean_cannot_see_is_refused() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\npublic final class Shape\n{\n    public Shape(public int sides { get; }) { }\n\n    law summed(Shape s) => s.sum() >= 0;\n}\n",
        &[(
            "app/Shared/ShapeMath.sharp",
            "namespace App.Shared;\n\npublic static class ShapeMath\n{\n    extension(Shape shape)\n    {\n        public int sum()\n        {\n            let total = 0;\n            for (let i = 0; i < shape.sides; i++) {\n                total += i;\n            }\n            return total;\n        }\n    }\n}\n",
        )],
        "Law summed reaches a loop in ShapeMath.sum. Write the method as recursion that calls itself on a smaller value.",
    );
}
