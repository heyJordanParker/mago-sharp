//! Runs `mago compile` on a project that mixes PHP# and PHP files, with a PHP# package in `vendor/`.

use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;

use mago_database::clock::poll_until;
use mago_database::process;
use mago_sharp_bridge::unit::GENERATION_LOCK;
use mago_sharp_bridge::unit::GENERATION_PREFIX;
use mago_sharp_bridge::unit::Library;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::inputs_changed;
use mago_sharp_bridge::unit::release_checker;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_bridge::unit::stamp;

use crate::common;
use crate::common::Watch;
use crate::common::compiling::LAWFUL_MONEY;
use crate::common::compiling::LEGACY;
use crate::common::compiling::MONEY;
use crate::common::compiling::MONEY_PATH;
use crate::common::compiling::ORDER;
use crate::common::compiling::SHOP_MODULE;
use crate::common::compiling::analyze;
#[cfg(unix)]
use crate::common::compiling::cache_with_runner;
use crate::common::compiling::command;
use crate::common::compiling::compile;
use crate::common::compiling::compile_lines;
use crate::common::compiling::compile_succeeds;
use crate::common::compiling::compile_without_lean;
use crate::common::compiling::compiled;
#[cfg(unix)]
use crate::common::compiling::counting_cache;
use crate::common::compiling::files_under;
use crate::common::compiling::issue_lines;
use crate::common::compiling::lawful_project;
use crate::common::compiling::library_project;
use crate::common::compiling::money_and_wallet;
use crate::common::compiling::printed;
use crate::common::compiling::project;
use crate::common::compiling::read;
use crate::common::compiling::release_library;
use crate::common::compiling::source_lean_files;
use crate::common::compiling::unwritable_cache;
use crate::common::compiling::write;

const BROKEN_ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return \"one\";\n    }\n}\n";
const BROKEN_MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return \"one\";\n    }\n}\n";

#[test]
fn compile_writes_each_accepted_file_into_the_sharp_folder_at_its_source_path() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let vendor = files_under(&root.join("vendor"));

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(
        files_under(&root.join(".sharp")),
        [
            Path::new("analysis.bin"),
            Path::new("autoload.php"),
            Path::new("compiled.bin"),
            Path::new("current/app/Order.sharpc"),
            Path::new("current/generation.lock"),
            Path::new("current/listeners.php"),
            Path::new("current/vendor/acme/money/src/Money.sharpc"),
            Path::new("inputs.json"),
            Path::new("write.lock")
        ]
    );
    for (name, source) in [("app/Order.sharp", ORDER), (MONEY_PATH, MONEY)] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        let header = header(&bytes).unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(header.source_hash, source_hash(source.as_bytes()), "{name}");
    }
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
    assert!(printed(&output).contains("Compiled 2 PHP# files into .sharp/."), "{}", printed(&output));
}

/// Spec section 27: `.sharp/autoload.php`, which a program loads before it builds its class, requires the workspace's
/// Composer autoloader.
#[test]
fn compile_writes_an_autoload_script_that_requires_the_composer_autoloader() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();

    compile_succeeds(root);

    assert_eq!(
        read(root, ".sharp/autoload.php"),
        "<?php\n\n// mago compile writes this file, the project's Composer autoloader. It rewrites it on every compile.\n\nrequire_once __DIR__ . '/../vendor/autoload.php';\n"
    );
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
        &format!("{library}/library/Sharp/Math.sharp"),
        "namespace Sharp;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
    );
    release_library(&root.join(library));
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    write(
        root,
        "vendor/composer/installed.json",
        &format!(
            "{{\"packages\": [{{\"name\": \"heyjordanparker/php-sharp-composer\", \"version\": \"{version}\"}}]}}\n"
        ),
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
            version: version.into_bytes(),
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

/// mago reads the standard library of its own release, as the `Dispatcher` it marks, so the compile refuses a project
/// whose installed library is another release, naming both. A Composer dev version names no release, so it compiles.
#[test]
fn compile_refuses_a_standard_library_of_another_release() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let library = "vendor/heyjordanparker/php-sharp-composer";
    write(root, &format!("{library}/composer.json"), "{\"name\": \"heyjordanparker/php-sharp-composer\"}\n");
    write(
        root,
        &format!("{library}/library/Sharp/Math.sharp"),
        "namespace Sharp;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
    );
    release_library(&root.join(library));
    let install = |version: &str| {
        write(
            root,
            "vendor/composer/installed.json",
            &format!(
                "{{\"packages\": [{{\"name\": \"heyjordanparker/php-sharp-composer\", \"version\": \"{version}\"}}]}}\n"
            ),
        );
    };
    let mago = env!("CARGO_PKG_VERSION");

    install("v0.5.9");
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "This project's PHP# standard library is 0.5.9, and mago is {mago}. Move mago and the standard library to the \
             same release: require heyjordanparker/php-sharp-composer {mago}, or heyjordanparker/mago-sharp 0.5.9."
        )),
        "{}",
        printed(&output)
    );
    assert!(!compiled(root, "app/Order.sharp").exists());

    for version in [format!("v{mago}"), mago.to_owned(), "dev-master".to_owned(), "0.6.x-dev".to_owned()] {
        install(&version);
        compile_succeeds(root);
    }
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

    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .current_dir(root),
    )
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
/// hosts' compile lives outside the project, and `mago analyze` compiles the project's `.sharp` folder from the same
/// lowering, as `mago --no-extensions compile` writes it.
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );

    compile_succeeds(root);
    let compiled_by_compile = std::fs::read(compiled(root, "app/Word.sharp")).unwrap();
    write(root, "app/Word.sharp", &word("second"));
    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\nsecond\n");
    let compiled_by_analyze = std::fs::read(compiled(root, "app/Word.sharp")).unwrap();
    assert_ne!(compiled_by_analyze, compiled_by_compile, "analyze compiled the edit into the `.sharp` folder");
    let without_hosts = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "--no-extensions", "compile"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();
    assert!(without_hosts.status.success(), "{}", printed(&without_hosts));
    assert_eq!(std::fs::read(compiled(root, "app/Word.sharp")).unwrap(), compiled_by_analyze);
}

/// An extension host whose file constructs the standard library's `SystemFiles` as it loads, through an autoloader that
/// maps `Sharp\` to the installed library and `App\` to `app/`, as a host that builds the project's services at its
/// start does.
const HOST_CONSTRUCTING_SYSTEM_FILES: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

spl_autoload_register(static function (string $class): void {
    $folders = ['Sharp\\' => '/vendor/heyjordanparker/php-sharp-composer/library/Sharp/', 'App\\' => '/app/'];
    foreach ($folders as $prefix => $folder) {
        if (str_starts_with($class, $prefix)) {
            require dirname(__DIR__) . $folder . str_replace('\\', '/', substr($class, strlen($prefix))) . '.sharp';
        }
    }
});

new Sharp\IO\SystemFiles();

(new Worker(new Extension('files', 'Files', '0.0.0')))->run();
"#;

/// A project that installs the standard library, with the host [`HOST_CONSTRUCTING_SYSTEM_FILES`].
fn project_with_a_host_constructing_system_files() -> tempfile::TempDir {
    let directory = library_project(&[("app/Order.sharp", ORDER), ("tools/host.php", HOST_CONSTRUCTING_SYSTEM_FILES)]);
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.files]\nfile = \"tools/host.php\"\n",
    );
    directory
}

/// The standard library's compiled `SystemFiles`, in the `.sharp` folder of the package the project installs.
const SYSTEM_FILES: &str =
    "vendor/heyjordanparker/php-sharp-composer/.sharp/current/library/Sharp/IO/SystemFiles.sharpc";

/// A host loads the standard library's classes from the `.sharp` folder of its package, so a host that constructs one
/// as it loads starts on a tree `mago --no-extensions compile` compiled.
#[test]
fn a_host_that_constructs_a_standard_library_class_starts_on_a_tree_compiled_without_hosts() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host compile test") {
        return;
    }
    let directory = project_with_a_host_constructing_system_files();
    let root = directory.path();
    let without_hosts = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "--no-extensions", "compile"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();
    assert!(without_hosts.status.success(), "{}", printed(&without_hosts));
    assert!(compiled(root, "app/Order.sharp").is_file(), "the compile without hosts wrote app/Order.sharp");

    compile_succeeds(root);
}

/// A host that constructs a standard library class as it loads starts on a tree no compile ran on, in one
/// `mago compile`, and the next compile of that tree writes nothing.
#[test]
fn a_host_that_constructs_a_standard_library_class_starts_on_a_clean_tree_and_the_next_compile_writes_nothing() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host compile test") {
        return;
    }
    let directory = project_with_a_host_constructing_system_files();
    let root = directory.path();

    let written_at = || {
        [compiled(root, "app/Order.sharp"), root.join(SYSTEM_FILES)]
            .map(|path| {
                std::fs::metadata(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display())).modified()
            })
            .map(Result::unwrap)
    };

    compile_succeeds(root);
    let written = written_at();
    compile_succeeds(root);

    assert_eq!(written_at(), written, "the compile wrote nothing");
}

/// An extension host whose file asks the project's PHP# rule `App\Rules\Naming` for its message as it loads, through
/// an autoloader that maps `Sharp\` to the installed library and `App\` to `app/`, and appends that message to
/// `host.log` before each analysis, as a host that builds its rules at its start runs the rules' code.
const HOST_ASKING_A_RULE_AS_IT_LOADS: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

spl_autoload_register(static function (string $class): void {
    $folders = ['Sharp\\' => '/vendor/heyjordanparker/php-sharp-composer/library/Sharp/', 'App\\' => '/app/'];
    foreach ($folders as $prefix => $folder) {
        if (str_starts_with($class, $prefix)) {
            require dirname(__DIR__) . $folder . str_replace('\\', '/', substr($class, strlen($prefix))) . '.sharp';
        }
    }
});

final class LogsTheRule implements Plugin, BeforeAnalysisHook
{
    public function __construct(private readonly string $message) {}

    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('logs-the-rule', 'Logs the rule', 'Logs the message of App\Rules\Naming.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        file_put_contents('host.log', "{$this->message}\n", FILE_APPEND);
    }
}

(new Worker(new Extension('rules', 'Rules', '0.0.0', analyzerPlugins: [new LogsTheRule(App\Rules\Naming::message())])))->run();
"#;

/// The rule [`HOST_ASKING_A_RULE_AS_IT_LOADS`] asks, whose message is `message`.
fn naming(message: &str) -> String {
    format!(
        "namespace App.Rules;\n\npublic class Naming\n{{\n    public static string message() => \"{message}\";\n}}\n"
    )
}

/// A project that installs the standard library, with the rule [`naming`] of the message `first` and the host
/// [`HOST_ASKING_A_RULE_AS_IT_LOADS`].
fn project_with_a_host_asking_a_rule() -> tempfile::TempDir {
    let directory = library_project(&[
        ("app/Rules/Naming.sharp", &naming("first")),
        ("tools/host.php", HOST_ASKING_A_RULE_AS_IT_LOADS),
    ]);
    write(
        directory.path(),
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.rules]\nfile = \"tools/host.php\"\n",
    );
    directory
}

/// `mago analyze` of the project at `root`, with mago's cache in `cache`.
fn analyze_with_cache(root: &Path, cache: &Path) -> Output {
    process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("XDG_CACHE_HOME", cache)
            .current_dir(root),
    )
    .unwrap()
}

/// A host's file that asks a rule of the project as it loads starts on the rule as it is now on every run, the first
/// run after an edit of the rule included: the host's file names the rule, so the rule compiles before the hosts start.
#[test]
fn a_host_starts_on_the_edited_rule_it_asks_as_it_loads_on_the_first_analyze_after_the_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the edited rule host test") {
        return;
    }
    let directory = project_with_a_host_asking_a_rule();
    let root = directory.path();
    let cache = tempfile::tempdir().unwrap();

    let first = analyze_with_cache(root, cache.path());
    write(root, "app/Rules/Naming.sharp", &naming("second"));
    let edited = analyze_with_cache(root, cache.path());

    assert!(edited.status.success(), "{}", printed(&edited));
    assert!(first.status.success(), "{}", printed(&first));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\nsecond\n");
}

/// A host's file that finds its rules as it loads by scanning `app/Rules/`, which the PHP expression `rules` gives,
/// building each rule's class from its file's name, as a host that discovers its rules does, and asks each for its
/// message. The PHP expression `root` gives the project's root.
fn host_scanning(rules: &str, root: &str) -> String {
    HOST_ASKING_A_RULE_AS_IT_LOADS.replace("dirname(__DIR__) . $folder", &format!("{root} . $folder")).replace(
        "new LogsTheRule(App\\Rules\\Naming::message())",
        &format!(
            "new LogsTheRule(implode(',', array_map(static function (string $file): string {{
    $class = 'App\\\\Rules\\\\' . basename($file, '.sharp');

    return $class::message();
}}, glob({rules} . '/*.sharp'))))"
        ),
    )
}

/// A project with the rules `Naming` and `Spacing` in `app/Rules/` and the host [`host_scanning`] them, whose file is
/// `host`, which the project's own files leave out.
fn project_with_a_host_scanning(host: &str, rules: &str, root: &str) -> tempfile::TempDir {
    let directory = library_project(&[
        ("app/Rules/Naming.sharp", &naming("first")),
        ("app/Rules/Spacing.sharp", &naming("second").replace("Naming", "Spacing")),
        (host, &host_scanning(rules, root)),
    ]);
    write(
        directory.path(),
        "mago.toml",
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\nexcludes = [\"{host}\"]\n\n[extension-hosts.rules]\nfile = \"{host}\"\n"
        ),
    );
    directory
}

/// A host's code is the folder that holds its file, as a package is its folder, so every PHP# file under it compiles
/// before the hosts start: a host that finds its rules by scanning a folder under its own, by names its code never
/// writes, starts on every rule on a clean tree.
#[test]
fn a_host_that_finds_its_rules_by_scanning_a_folder_under_its_own_gets_every_rule_s_result_on_a_clean_tree() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scanning host test") {
        return;
    }
    let directory = project_with_a_host_scanning("app/Rules/host.php", "__DIR__", "dirname(__DIR__, 2)");
    let root = directory.path();
    let cache = tempfile::tempdir().unwrap();

    let analyzed = analyze_with_cache(root, cache.path());

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first,second\n");
}

/// A class a host loads as it starts from outside its folder, which nothing the host's code reaches names, compiles
/// only once the host started, so loading it fails with an error that names the class, the host and the rule, and
/// never asks for a longer wait.
#[test]
fn a_host_that_loads_a_class_outside_its_folder_that_nothing_names_fails_naming_the_class_the_host_and_the_rule() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the outside class host test") {
        return;
    }
    let directory =
        project_with_a_host_scanning("tools/host.php", "dirname(__DIR__) . '/app/Rules'", "dirname(__DIR__)");
    let root = directory.path();
    let cache = tempfile::tempdir().unwrap();

    let analyzed = analyze_with_cache(root, cache.path());

    assert!(!analyzed.status.success(), "{}", printed(&analyzed));
    assert!(
        printed(&analyzed).contains(
            "extension host `rules` could not load: `App.Rules.Naming` isn't in extension host `rules`'s folder \
             `tools/` or named from it, so it isn't compiled before the host starts. Move it under `tools/` or name it \
             from the host's code."
        ),
        "{}",
        printed(&analyzed)
    );
}

/// A compile error in a file no host reaches never stops the hosts: on a clean tree, `mago analyze` reports that
/// file's own error, and the host starts on the rule it asks as it loads and runs its hook.
#[test]
fn a_compile_error_in_a_file_no_host_reaches_reports_that_file_and_every_host_runs() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the unreached error host test") {
        return;
    }
    let directory = project_with_a_host_asking_a_rule();
    let root = directory.path();
    let broken = "namespace App;\n\npublic class Broken\n{\n    public int count() => \"none\";\n}\n";
    write(root, "app/Broken.sharp", broken);
    let cache = tempfile::tempdir().unwrap();

    let analyzed = analyze_with_cache(root, cache.path());

    assert_eq!(analyzed.status.code(), Some(1), "{}", printed(&analyzed));
    assert!(printed(&analyzed).contains("app/Broken.sharp"), "{}", printed(&analyzed));
    assert!(!printed(&analyzed).contains("extension host"), "{}", printed(&analyzed));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\n");
}

/// A rule a host asks as it loads, whose code isn't PHP#, so it doesn't lower, is refused in the hosts' folder by its
/// language errors before the hosts start, spec section 27, so the first compile with an empty cache names the rule's
/// errors, as every later compile does, and never says the rule isn't compiled yet.
#[test]
fn a_host_that_loads_a_refused_rule_gets_its_errors_on_the_first_compile_with_an_empty_cache() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the refused rule host test") {
        return;
    }
    let directory = project_with_a_host_asking_a_rule();
    let root = directory.path();
    let refused = "namespace App.Rules;\n\npublic class Naming\n{\n    public static string message()\n    {\n        nope = 1 + 2;\n        return \"x\";\n    }\n}\n";
    write(root, "app/Rules/Naming.sharp", refused);
    let cache = tempfile::tempdir().unwrap();

    let first = process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
    let second = process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();

    assert!(!printed(&first).contains("isn't compiled for the extension hosts yet"), "{}", printed(&first));
    assert!(printed(&second).contains("app/Rules/Naming.sharp has"), "{}", printed(&second));
    assert!(printed(&first).contains("app/Rules/Naming.sharp has"), "{}", printed(&first));
}

/// A project whose standard library is another release than mago's, as a project halfway through an upgrade is, starts
/// no extension host, so every compile and analyze with an empty cache, and every one after, names the two releases,
/// and none says a host's file isn't compiled yet.
#[test]
fn a_project_with_a_host_and_a_standard_library_of_another_release_is_told_to_match_the_releases() {
    let directory = project_with_a_host_asking_a_rule();
    let root = directory.path();
    write(
        root,
        "vendor/composer/installed.json",
        "{\"packages\": [{\"name\": \"heyjordanparker/php-sharp-composer\", \"version\": \"v0.5.9\"}]}\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let mago = env!("CARGO_PKG_VERSION");
    let refusal = format!("This project's PHP# standard library is 0.5.9, and mago is {mago}.");

    let first = process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
    let second = process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
    let analyzed = analyze_with_cache(root, cache.path());

    for output in [&first, &second, &analyzed] {
        assert_eq!(output.status.code(), Some(1), "{}", printed(output));
        assert!(printed(output).contains(&refusal), "{}", printed(output));
        assert!(!printed(output).contains("isn't compiled for the extension hosts yet"), "{}", printed(output));
    }
}

/// The `extern` lines of a file nothing names apply to every file, as an analysis of the whole project applies them, so
/// a rule a host asks as it loads that calls a Composer `files` helper an `extern` line declares compiles before the
/// hosts start, and the first compile with the host passes.
#[test]
fn a_host_starts_on_a_rule_that_calls_an_extern_declared_helper_on_the_first_compile() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the extern helper host test") {
        return;
    }
    let host = HOST_ASKING_A_RULE_AS_IT_LOADS.replace(
        "spl_autoload_register(",
        "require dirname(__DIR__) . '/vendor/acme/framework/helpers.php';\n\nspl_autoload_register(",
    );
    let rule = "namespace App.Rules;\n\nimport Framework;\n\npublic class Naming\n{\n    public static string message() => Framework.config(\"app.name\") as string ?? \"none\";\n}\n";
    let directory = library_project(&[
        ("app/Rules/Naming.sharp", rule),
        (
            "app/Stubs/Framework.sharp",
            "namespace App.Stubs;\n\nimport Framework;\n\nextern namespace Framework for config;\n\nextern Framework.config uses Environment;\n",
        ),
        ("tools/host.php", &host),
    ]);
    let root = directory.path();
    let maps = "<?php\n\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n";
    write(
        root,
        "vendor/acme/framework/helpers.php",
        "<?php\n\nfunction config(string $key): mixed\n{\n    return $key === 'app.name' ? 'first' : null;\n}\n",
    );
    write(root, "vendor/composer/autoload_psr4.php", &format!("{maps});\n"));
    write(
        root,
        "vendor/composer/autoload_files.php",
        &format!("{maps}    'framework' => $vendorDir . '/acme/framework/helpers.php',\n);\n"),
    );
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.rules]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();

    let compiled = process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();

    assert!(!printed(&compiled).contains("isn't compiled for the extension hosts yet"), "{}", printed(&compiled));
    assert!(compiled.status.success(), "{}", printed(&compiled));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\n");
}

/// `mago analyze` compiles the project's `.sharp` folder and the hosts' folder from one analysis without the hosts and
/// one lowering before it analyzes with the hosts, so on a fresh tree its log holds one compile, and the host and the
/// `.sharp` folder each get every file.
#[test]
fn analyze_compiles_the_sharp_folder_and_the_hosts_folder_from_one_lowering() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the one lowering test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Word.sharp",
        "namespace App;\n\npublic class Word\n{\n    public static string text() => \"first\";\n}\n",
    );
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();

    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("MAGO_LOG", "debug")
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(root),
    )
    .unwrap();

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert_eq!(
        printed(&analyzed).matches("Kept the compiled file of 0 PHP# files").count(),
        1,
        "{}",
        printed(&analyzed)
    );
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\n");
    for name in ["app/Order.sharp", MONEY_PATH, "app/Word.sharp"] {
        assert!(compiled(root, name).exists(), "{name}");
    }
}

/// An extension host of two rules: `ReturnsAtOnce`, registered first, and `SleepsOnEachFile`, which sleeps 40 ms on
/// each file it sees.
const HOST_OF_A_SLOW_RULE_AND_A_QUICK_ONE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class ReturnsAtOnce implements Plugin, AfterFileAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('returns-at-once', 'Returns at once', 'Does nothing on each file.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerAfterFileAnalysisHook($this);
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function afterFileAnalysis(AfterFileAnalysisContext $context): void {}
}

final class SleepsOnEachFile implements Plugin, AfterFileAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('sleeps-on-each-file', 'Sleeps on each file', 'Sleeps 40 ms on each file.');
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
        usleep(40_000);
    }
}

(new Worker(new Extension('timed', 'Timed', '0.0.0', analyzerPlugins: [new ReturnsAtOnce(), new SleepsOnEachFile()])))->run();
"#;

/// With `MAGO_LOG=debug`, `mago analyze` logs how long each host rule took in the round, slowest first, then the
/// slowest rule's slowest files, so a project finds a slow rule without a profiler. At `info`, the level a release
/// build logs at by default, it logs none of it.
#[test]
fn analyze_logs_each_host_rule_s_time_slowest_first_at_the_debug_level() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host rule times test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "tools/host.php", HOST_OF_A_SLOW_RULE_AND_A_QUICK_ONE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.timed]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let analyze = |level: &str| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("MAGO_LOG", level)
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };

    let debug = analyze("debug");

    assert!(debug.status.success(), "{}", printed(&debug));
    let log = printed(&debug);
    let sleeping = host_rule_took(&log, "SleepsOnEachFile::afterFileAnalysis").unwrap_or_else(|| panic!("{log}"));
    let quick = host_rule_took(&log, "ReturnsAtOnce::afterFileAnalysis").unwrap_or_else(|| panic!("{log}"));
    let (_, seconds, files) = sleeping;
    assert!(sleeping.0 < quick.0 && files > 0 && seconds >= 0.04 * f64::from(files), "{log}");
    let slowest_files = log
        .lines()
        .filter(|line| line.contains("Host rule SleepsOnEachFile::afterFileAnalysis took ") && line.contains(" s on "));
    assert_eq!(slowest_files.count(), files.min(5) as usize, "{log}");

    let info = analyze("info");

    assert!(info.status.success(), "{}", printed(&info));
    assert!(!printed(&info).contains("Host rule"), "{}", printed(&info));
}

/// Where `rule`'s round line stands among the "Host rule" lines of `log`, how many seconds it took and over how many
/// files, as "Host rule SleepsOnEachFile::afterFileAnalysis took 0.08 s over 2 files." gives them.
fn host_rule_took(log: &str, rule: &str) -> Option<(usize, f64, u32)> {
    let prefix = format!("{rule} took ");
    log.lines().filter_map(|line| line.split_once("Host rule ").map(|(_, line)| line)).enumerate().find_map(
        |(place, line)| {
            let (seconds, files) = line.strip_prefix(&prefix)?.split_once(" s over ")?;
            Some((place, seconds.parse().ok()?, files.split_once(' ')?.0.parse().ok()?))
        },
    )
}

/// An extension host of two return-type providers for the project's `App\measure`: `ReturnsNoType`, registered first,
/// and `SleepsOnEachCall`, which sleeps 40 ms on each call it is asked about. Neither gives a type, so mago asks both.
const HOST_OF_A_SLOW_PROVIDER_AND_A_QUICK_ONE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\FunctionReturnTypeProvider;
use Mago\Sdk\Analyzer\FunctionTarget;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class ReturnsNoType implements Plugin, FunctionReturnTypeProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('returns-no-type', 'Returns no type', 'Gives no type for App\\measure.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerFunctionReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [FunctionTarget::exact('App\\measure')];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        return null;
    }
}

final class SleepsOnEachCall implements Plugin, FunctionReturnTypeProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('sleeps-on-each-call', 'Sleeps on each call', 'Sleeps 40 ms on each call.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerFunctionReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [FunctionTarget::exact('App\\measure')];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        usleep(40_000);

        return null;
    }
}

(new Worker(new Extension('typed', 'Typed', '0.0.0', analyzerPlugins: [new ReturnsNoType(), new SleepsOnEachCall()])))->run();
"#;

/// A host's providers run in the round as its hooks do, so with `MAGO_LOG=debug` a slow return-type provider's line
/// comes before a quick one's, and at `info` neither prints.
#[test]
fn analyze_logs_a_slow_host_provider_before_a_quick_one_at_the_debug_level() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host provider times test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Measures.php",
        "<?php\n\nnamespace App;\n\nfunction measure(string $text): int\n{\n    return \\strlen($text);\n}\n\nfunction total(): int\n{\n    return measure('a') + measure('bb');\n}\n",
    );
    write(root, "tools/host.php", HOST_OF_A_SLOW_PROVIDER_AND_A_QUICK_ONE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.typed]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let analyze = |level: &str| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("MAGO_LOG", level)
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };

    let debug = analyze("debug");

    assert!(debug.status.success(), "{}", printed(&debug));
    let log = printed(&debug);
    let slow = host_rule_took(&log, "SleepsOnEachCall::getReturnType").unwrap_or_else(|| panic!("{log}"));
    let quick = host_rule_took(&log, "ReturnsNoType::getReturnType").unwrap_or_else(|| panic!("{log}"));
    assert!(slow.0 < quick.0 && slow.1 >= 0.04, "{log}");

    let info = analyze("info");

    assert!(info.status.success(), "{}", printed(&info));
    assert!(!printed(&info).contains("Host rule"), "{}", printed(&info));
}

/// An editor's `mago analyze` and a terminal's start together after an edit, and each compiles the project into the one
/// hosts' folder. One compiles while the others wait and then reuse its tree, so each run passes and each host reads
/// the file compiled from the current source. Each round edits every source first, so every run finds the folder
/// stale, and 60 more files keep each compile writing long enough for three runs to meet in the folder.
#[test]
fn analyses_that_start_together_after_an_edit_each_give_their_hosts_the_current_tree() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the hosts compile race test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let analyze = || {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };

    let class = |namespace: &str, class: &str, word: &str| {
        format!(
            "namespace {namespace};\n\npublic class {class}\n{{\n    public static string text() => \"{word}\";\n}}\n"
        )
    };

    for round in 0..10 {
        let word = if round % 2 == 0 { "first" } else { "second" };
        write(root, "app/Word.sharp", &class("App", "Word", word));
        for file in 0..60 {
            write(root, &format!("app/Words/Word{file}.sharp"), &class("App.Words", &format!("Word{file}"), word));
        }
        let _ = std::fs::remove_file(root.join("host.log"));

        let outputs: Vec<Output> = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..3).map(|_| scope.spawn(analyze)).collect();
            runs.into_iter().map(|run| run.join().unwrap()).collect()
        });

        for output in &outputs {
            assert!(output.status.success(), "round {round}: {}", printed(output));
        }
        assert_eq!(
            std::fs::read_to_string(root.join("host.log")).unwrap(),
            format!("{word}\n{word}\n{word}\n"),
            "round {round}"
        );
    }
}

/// An extension host that reads the word the compiled `app/Word.sharp` holds as its analysis starts, and again once it
/// is let go, and appends both to `host.log`. The first such host to start makes the folder `held` and waits there
/// until the file `hold` is gone, as a host whose hooks take long reads its classes over time.
const HOST_READING_THE_COMPILED_FILE_TWICE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class ReadsTheCompiledFileTwice implements Plugin, BeforeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('reads-twice', 'Reads twice', 'Reads app/Word.sharp compiled, twice.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        $first = $this->word();
        if (@mkdir('held')) {
            while (file_exists('hold')) {
                usleep(10000);
            }
        }
        file_put_contents('host.log', "{$first} {$this->word()}\n", FILE_APPEND);
    }

    private function word(): string
    {
        $compiled = (string) @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');

        return str_contains($compiled, 'second') ? 'second' : (str_contains($compiled, 'first') ? 'first' : 'nothing');
    }
}

(new Worker(new Extension('reader', 'Reader', '0.0.0', analyzerPlugins: [new ReadsTheCompiledFileTwice()])))->run();
"#;

/// A run's hosts read the tree the run started them on until the run ends. An edit while they read makes the next run
/// compile again, and that run waits for them before it rewrites the folder, so the first run's hosts read the first
/// word both times and the next run's hosts read the second. The test lets the first run's host go once the next run's
/// host logged, or 10 seconds on, far past the second a compile of this project takes.
#[test]
fn a_later_run_rewrites_the_hosts_folder_only_once_the_hosts_reading_it_are_done() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the hosts folder reader test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let word = |word: &str| {
        format!("namespace App;\n\npublic class Word\n{{\n    public static string text() => \"{word}\";\n}}\n")
    };
    write(root, "app/Word.sharp", &word("first"));
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE_TWICE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );
    write(root, "hold", "");
    let cache = tempfile::tempdir().unwrap();
    let analyze = || {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    let wait_until = |done: &dyn Fn() -> bool, seconds: u64| {
        poll_until(std::time::Duration::from_secs(seconds), done);
    };

    let (first, next) = std::thread::scope(|scope| {
        let first = scope.spawn(analyze);
        wait_until(&|| root.join("held").is_dir(), 120);
        write(root, "app/Word.sharp", &word("second"));
        let next = scope.spawn(analyze);
        wait_until(&|| !log().is_empty(), 10);
        std::fs::remove_file(root.join("hold")).unwrap();
        (first.join().unwrap(), next.join().unwrap())
    });

    assert!(first.status.success(), "{}", printed(&first));
    assert!(next.status.success(), "{}", printed(&next));
    let mut reads: Vec<String> = log().lines().map(str::to_owned).collect();
    reads.sort();
    assert_eq!(reads, ["first first", "second second"]);
}

/// A host names none of what it reads, so a compile with a host enabled runs every time.
#[test]
fn a_compile_with_a_host_enabled_runs_every_time() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the compile with a host test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Word.sharp",
        "namespace App;\n\npublic class Word\n{\n    public static string text() => \"first\";\n}\n",
    );
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );
    compile_succeeds(root);
    compile_succeeds(root);

    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\nfirst\n", "each compile ran its host");
}

/// An extension host that deletes `app/Gone.sharp` as its analysis starts, once the compile has read the file, as an
/// editor deletes a file while a compile runs.
const HOST_DELETING_A_SOURCE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class DeletesASource implements Plugin, BeforeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('deletes-a-source', 'Deletes a source', 'Deletes app/Gone.sharp.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerBeforeAnalysisHook($this);
    }

    public function beforeAnalysis(BeforeAnalysisContext $context): void
    {
        @unlink('app/Gone.sharp');
    }
}

(new Worker(new Extension('deleter', 'Deleter', '0.0.0', analyzerPlugins: [new DeletesASource()])))->run();
"#;

/// A source deleted after the compile read it is a stale input, which fails nothing, and the next compile leaves the
/// project as a compile of the project without the source does.
#[test]
fn a_source_deleted_while_the_compile_runs_fails_nothing_and_the_next_compile_leaves_it_out() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the deleted source compile test") {
        return;
    }
    let with_host = |root: &Path| {
        write(root, "tools/host.php", HOST_DELETING_A_SOURCE);
        write(
            root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.deleter]\nfile = \"tools/host.php\"\n",
        );
    };
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    with_host(root);
    write(root, "app/Gone.sharp", "namespace App;\n\npublic class Gone\n{\n}\n");

    compile_succeeds(root);
    assert!(!root.join("app/Gone.sharp").exists(), "the host deleted the source");
    compile_succeeds(root);

    let without = project(ORDER, MONEY);
    with_host(without.path());
    compile_succeeds(without.path());
    assert_eq!(files_under(&root.join(".sharp")), files_under(&without.path().join(".sharp")));
    assert_eq!(files_under(&root.join("app")), files_under(&without.path().join("app")));
}

/// The analysis scans the project's files on its worker threads, whose log the hosts' compile doesn't silence, so a
/// plain PHP file that doesn't parse warns there. Here 64 such files, so the warnings reach the worker threads whichever
/// thread scans which file. The warnings are the next run's to report, so the hosts' compile keeps its record and has
/// nothing to do the next time.
#[cfg(unix)]
#[test]
fn the_hosts_compile_keeps_its_record_whatever_its_worker_threads_log() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the hosts compile record test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );
    for file in 0..64 {
        write(root, &format!("vendor/acme/torn/src/Torn{file}.php"), "<?php\n\nfunction torn( {\n");
    }
    let cache = tempfile::tempdir().unwrap();
    let compile = || process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
    let output = compile();
    assert!(output.status.success(), "{}", printed(&output));
    assert!(printed(&output).contains("parsing error(s) in 'vendor/acme/torn/src/Torn"), "{}", printed(&output));
    let hosts = cache.path().join("mago/hosts");
    let folder = std::fs::read_dir(&hosts).unwrap().next().expect("the hosts' compile wrote its folder").unwrap();
    let record = folder.path().join("inputs.json");
    assert!(record.exists(), "the hosts' compile kept its record");
    let recorded = std::fs::metadata(&record).unwrap().modified().unwrap();

    let again = compile();

    assert!(again.status.success(), "{}", printed(&again));
    assert_eq!(
        std::fs::metadata(&record).unwrap().modified().unwrap(),
        recorded,
        "the hosts' compile had nothing to do"
    );
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
    );

    let linted = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "lint"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();

    assert!(linted.status.success(), "{}", printed(&linted));
    let log = std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    assert!(!log.is_empty() && log.lines().all(|line| line == "first"), "{log}{}", printed(&linted));
    assert!(!root.join(".sharp").exists());
}

/// An extension host of two linter rules on each class: `LintsAtOnce`, registered first, and `SleepsOnEachClass`, which
/// sleeps 40 ms on each class it lints.
const LINT_HOST_OF_A_SLOW_RULE_AND_A_QUICK_ONE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Extension;
use Mago\Sdk\Linter\LintContext;
use Mago\Sdk\Linter\Rule;
use Mago\Sdk\Linter\RuleDefinition;
use Mago\Sdk\Reporting\Level;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;

final class LintsAtOnce implements Rule
{
    public function getDefinition(): RuleDefinition
    {
        return new RuleDefinition(
            code: 'timed/lints-at-once',
            name: 'Lints at once',
            description: 'Does nothing on each class.',
            defaultLevel: Level::Warning,
            defaultEnabled: true,
            targets: [NodeKind::Class_],
        );
    }

    public function lint(LintContext $context): void {}
}

final class SleepsOnEachClass implements Rule
{
    public function getDefinition(): RuleDefinition
    {
        return new RuleDefinition(
            code: 'timed/sleeps-on-each-class',
            name: 'Sleeps on each class',
            description: 'Sleeps 40 ms on each class.',
            defaultLevel: Level::Warning,
            defaultEnabled: true,
            targets: [NodeKind::Class_],
        );
    }

    public function lint(LintContext $context): void
    {
        usleep(40_000);
    }
}

(new Worker(new Extension('timed', 'Timed', '0.0.0', linterRules: [new LintsAtOnce(), new SleepsOnEachClass()])))->run();
"#;

/// With `MAGO_LOG=debug`, `mago lint` logs how long each host linter rule took when it ends, slowest first, as a round
/// of `mago analyze` logs its rules, and at `info` it logs none of it.
#[test]
fn lint_logs_a_slow_host_linter_rule_before_a_quick_one_at_the_debug_level() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host linter rule times test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Plain.php", "<?php\n\nnamespace App;\n\nfinal class Plain {}\n");
    write(root, "tools/host.php", LINT_HOST_OF_A_SLOW_RULE_AND_A_QUICK_ONE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.timed]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let lint = |level: &str| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "lint"])
                .env("MAGO_LOG", level)
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };

    let debug = lint("debug");

    assert!(debug.status.success(), "{}", printed(&debug));
    let log = printed(&debug);
    let slow = host_rule_took(&log, "SleepsOnEachClass::lint").unwrap_or_else(|| panic!("{log}"));
    let quick = host_rule_took(&log, "LintsAtOnce::lint").unwrap_or_else(|| panic!("{log}"));
    assert!(slow.0 < quick.0 && slow.2 > 0 && slow.1 >= 0.04 * f64::from(slow.2), "{log}");

    let info = lint("info");

    assert!(info.status.success(), "{}", printed(&info));
    assert!(!printed(&info).contains("Host rule"), "{}", printed(&info));
}

/// An extension host that loads `App\Word`, the class of `app/Word.sharp`, and appends its process id, the id of the
/// `mago` process that started it and the word the compiled file holds to `host.log` as each analysis starts. The `php` the tests run is plain PHP, which never declares
/// a PHP# class, so the worker fails to load it, which counts as loading it.
const HOST_LOGGING_ITS_PROCESS: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\BeforeAnalysisContext;
use Mago\Sdk\Analyzer\BeforeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

define('MAGO_TEST_WORKER', random_int(1, 0xFFFFFFFF));

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
        class_exists('App\Word');
        $compiled = (string) @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');
        $word = str_contains($compiled, 'second') ? 'second' : (str_contains($compiled, 'first') ? 'first' : 'nothing');
        file_put_contents('host.log', MAGO_TEST_WORKER . ' ' . posix_getppid() . " {$word}\n", FILE_APPEND);
    }
}

(new Worker(new Extension('logger', 'Logger', '0.0.0', analyzerPlugins: [new LogsItsProcess()])))->run();
"#;

/// A host's worker keeps each class it loaded, as PHP declares a class once, so `mago analyze --watch` starts a host's
/// worker again when the compiled file of a class it loaded changes.
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nfile = \"tools/host.php\"\n",
    );
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();

    let watch = Watch::start(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info,mago=debug,mago_database=debug")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(Stdio::null()),
    );
    let started = watch.until(|seen| seen.contains("Initial analysis complete"));
    write(root, "app/Word.sharp", &word("second"));
    let rechecked = started && watch.until(|seen| seen.contains("Analysis complete."));
    let seen = watch.stop();

    let hosts = log();
    let runs: Vec<(&str, &str)> = hosts.lines().filter_map(|line| line.rsplit_once(' ')).collect();
    let failure = format!("{hosts}\n{seen}");
    assert!(rechecked && runs.len() >= 2, "{failure}");
    assert_eq!(runs[0].1, "first", "{failure}");
    assert_eq!(runs[1].1, "second", "{failure}");
    assert_ne!(runs[0].0, runs[1].0, "{failure}");
}

/// An extension host whose worker loads only its own rule class, as a framework's rules do, and appends its process id
/// to `host.log` as each file's hooks run.
const HOST_LOADING_ONLY_ITS_RULE: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\AfterFileAnalysisContext;
use Mago\Sdk\Analyzer\AfterFileAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

define('MAGO_TEST_WORKER', random_int(1, 0xFFFFFFFF));

final class LogsEachFile implements Plugin, AfterFileAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('logs-each-file', 'Logs each file', 'Logs its process id for each file.');
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
        file_put_contents('host.log', MAGO_TEST_WORKER . " {$context->analysis->file}\n", FILE_APPEND);
    }
}

(new Worker(new Extension('rule', 'Rule', '0.0.0', analyzerPlugins: [new LogsEachFile()])))->run();
"#;

/// A worker that never loaded an app class keeps running through an edit of one: `mago analyze --watch` checks the
/// edit as any other edit, with no restart of the watch and no new worker, so its hooks run again on the edited file
/// alone, in the same process.
#[test]
fn a_watch_keeps_a_worker_that_never_loaded_the_edited_class() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the worker keeping watch test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let word = |word: &str| {
        format!("namespace App;\n\npublic class Word\n{{\n    public static string text() => \"{word}\";\n}}\n")
    };
    write(root, "app/Word.sharp", &word("first"));
    write(root, "tools/host.php", HOST_LOADING_ONLY_ITS_RULE);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.rule]\nfile = \"tools/host.php\"\n",
    );
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();

    let watch = Watch::start(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(Stdio::null()),
    );
    let started = watch.until(|seen| seen.contains("Initial analysis complete"));
    let first = log();
    write(root, "app/Word.sharp", &word("second"));
    let rechecked = started && watch.until(|seen| seen.contains("Analysis complete."));
    let seen = watch.stop();

    let failure = format!("{}\n{seen}", log());
    assert!(rechecked, "{failure}");
    assert!(!seen.contains("Restarting analysis"), "{failure}");
    let after = log();
    let recheck: Vec<&str> = after.strip_prefix(first.as_str()).unwrap_or_default().lines().collect();
    let process = first.lines().next().and_then(|line| line.split_once(' ')).map(|(process, _)| process);
    assert!(process.is_some(), "{failure}");
    let edited = format!("{} app/Word.sharp", process.unwrap_or_default());
    assert_eq!(recheck, [edited.as_str()], "the same worker checks the edited file alone: {failure}");
}

/// A watch reports what the one-shot command reports where an issue points into a vendor file Composer's autoload maps
/// served, and keeps watching: the report renders each file the analysis read, and the watch's own tree holds only
/// the files it watches. A release stopped the watch after its first analysis with "Files error: file missing".
#[test]
fn a_watch_reports_an_issue_that_points_into_a_vendor_file_the_autoload_maps_served() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(
        root,
        "vendor/composer/autoload_psr4.php",
        "<?php\n\n$vendorDir = dirname(__DIR__);\n\nreturn array(\n    'Acme\\\\Rate\\\\' => array($vendorDir . '/acme/rate/src'),\n);\n",
    );
    write(
        root,
        "vendor/acme/rate/src/Rate.php",
        "<?php\n\nnamespace Acme\\Rate;\n\nfinal class Rate\n{\n    private static function of(int $percent): int\n    {\n        return $percent;\n    }\n}\n",
    );
    let order = |percent: u8| {
        format!(
            "<?php\n\nnamespace App;\n\nuse Acme\\Rate\\Rate;\n\nfunction total(): int\n{{\n    return Rate::of({percent});\n}}\n"
        )
    };
    write(root, "app/Order.php", &order(5));
    let analyze = |watch: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
        command
            .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "rich"])
            .args(watch.then_some("--watch"))
            .env("MAGO_LOG", "info")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root);
        command
    };
    let once = process::output(&mut analyze(false)).unwrap();
    let report = String::from_utf8_lossy(&once.stdout).into_owned();
    assert!(
        report.contains("vendor/acme/rate/src/Rate.php"),
        "the issue points into the vendor file: {}",
        printed(&once)
    );

    let reports = tempfile::NamedTempFile::new().unwrap();
    let mut watch = Watch::start(analyze(true).stdout(reports.reopen().unwrap()));
    let started = watch.until(|seen| seen.contains("Initial analysis complete") || seen.contains("ERROR"));
    let first = std::fs::read_to_string(reports.path()).unwrap();
    write(root, "app/Order.php", &order(6));
    let rechecked = watch.until(|seen| seen.contains("Analysis complete.") || seen.contains("ERROR"));
    let running = watch.running();
    let seen = watch.stop();

    let failure = format!("{}{seen}", std::fs::read_to_string(reports.path()).unwrap());
    assert!(started && !seen.contains("ERROR"), "{failure}");
    assert_eq!(first, report, "the watch's first report is the one-shot command's: {failure}");
    assert!(rechecked && running, "{failure}");
    let second = std::fs::read_to_string(reports.path()).unwrap().split_off(first.len());
    assert!(second.contains("Rate::of(6)") && second.contains("vendor/acme/rate/src/Rate.php"), "{failure}");
}

/// `mago analyze --watch` keeps reporting the issues that point into a vendor file Composer's autoload maps served, round
/// after round, while files are deleted, created, renamed and edited, as agents change a tree. Deleting a class's file
/// analyzes the project from scratch inside the round, and the next release's watch then lost the served files and stopped with
/// "Files error: file missing". Each round's report is the one-shot command's on the same tree.
#[test]
fn a_watch_reports_the_vendor_files_the_autoload_maps_served_through_deletes_creates_renames_and_edits() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n");
    write(
        root,
        "vendor/composer/autoload_psr4.php",
        "<?php\n\n$vendorDir = dirname(__DIR__);\n\nreturn array(\n    'Acme\\\\Rate\\\\' => array($vendorDir . '/acme/rate/src'),\n);\n",
    );
    write(
        root,
        "vendor/acme/rate/src/Rate.php",
        "<?php\n\nnamespace Acme\\Rate;\n\nfinal class Rate\n{\n    private static function of(int $percent): int\n    {\n        return $percent;\n    }\n}\n",
    );
    let calling = |function: &str, percent: u8| {
        format!(
            "<?php\n\nnamespace App;\n\nuse Acme\\Rate\\Rate;\n\nfunction {function}(): int\n{{\n    return Rate::of({percent});\n}}\n"
        )
    };
    write(root, "app/Order.php", &calling("total", 5));
    write(root, "app/Extra.php", "<?php\n\nnamespace App;\n\nfinal class Extra\n{\n}\n");
    let analyze = |watch: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mago"));
        command
            .args(["--no-version-check", "--colors", "never", "analyze", "--reporting-format", "rich"])
            .args(watch.then_some("--watch"))
            .env("MAGO_LOG", "info")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root);
        command
    };
    let once = || String::from_utf8_lossy(&process::output(&mut analyze(false)).unwrap().stdout).into_owned();

    let reports = tempfile::NamedTempFile::new().unwrap();
    let mut watch = Watch::start(analyze(true).stdout(reports.reopen().unwrap()));
    let reported = || std::fs::read_to_string(reports.path()).unwrap();
    let started = watch.until(|seen| seen.contains("Initial analysis complete") || seen.contains("ERROR"));
    let mut rounds: Vec<(&str, bool, String)> = Vec::new();
    let changes: [(&str, &dyn Fn()); 4] = [
        ("delete app/Extra.php", &|| std::fs::remove_file(root.join("app/Extra.php")).unwrap()),
        ("create app/Added.php", &|| write(root, "app/Added.php", &calling("added", 7))),
        ("rename app/Added.php", &|| std::fs::rename(root.join("app/Added.php"), root.join("app/Moved.php")).unwrap()),
        ("edit app/Order.php", &|| write(root, "app/Order.php", &calling("total", 6))),
    ];
    let completed = |seen: &str| seen.matches("Analysis complete.").count();
    for (change, apply) in changes {
        let (before, rounds_before) = (reported().len(), completed(&watch.log()));
        apply();
        // A change can reach the watch in two batches, as a rename's delete and create, each a round of its own, so
        // the watch is done with it at the round that reports what the one-shot command reports on the changed tree.
        let cold = once();
        let caught_up = watch.until(|seen| {
            seen.contains("ERROR") || (completed(seen) > rounds_before && reported()[before..].ends_with(&cold))
        });
        let kept_watching = caught_up && watch.running() && !watch.log().contains("ERROR");
        rounds.push((change, kept_watching, reported().split_off(before)));
    }
    let failure = format!("{}{}", reported(), watch.stop());

    assert!(started, "{failure}");
    for (change, kept_watching, report) in rounds {
        assert!(
            kept_watching,
            "after `{change}` the watch kept watching and reported what the one-shot command reports: {failure}"
        );
        assert!(report.contains("vendor/acme/rate/src/Rate.php"), "after `{change}`: {failure}");
    }
}

/// A batch of changes that holds a deleted file removes it from the watch's tree: `mago analyze --watch`, with a host
/// whose compile reads the tree, reports the error of the file that used the deleted one and keeps watching, so it
/// analyzes the next change too.
#[test]
fn a_watch_whose_changes_hold_a_deleted_file_reports_its_dependents_and_keeps_watching() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the deleted file watch test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let price =
        "namespace App;\n\npublic class Price\n{\n    public int cents()\n    {\n        return 100;\n    }\n}\n";
    write(root, "app/Price.sharp", price);
    write(
        root,
        "app/Cart.sharp",
        "namespace App;\n\npublic class Cart\n{\n    public int total(Price price)\n    {\n        return price.cents();\n    }\n}\n",
    );
    write(root, "tools/host.php", HOST_LOGGING_ITS_PROCESS);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nfile = \"tools/host.php\"\n",
    );

    // The watch's reports, outside the watched project, so a failure shows what the watch saw.
    let reports = tempfile::NamedTempFile::new().unwrap();
    let mut watch = Watch::start(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info")
            .env("MAGO_REPORTING_FORMAT", "medium")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(reports.reopen().unwrap()),
    );
    let report = || std::fs::read_to_string(reports.path()).unwrap();
    let rounds = |seen: &str| seen.matches("Analysis complete.").count();
    let missing = "Cannot find type `App.Price`";

    let started = watch.until(|seen| seen.contains("Initial analysis complete"));
    std::fs::remove_file(root.join("app/Price.sharp")).unwrap();
    let reported = started && watch.until(|seen| rounds(seen) >= 1) && report().contains(missing);
    let running = watch.running();
    write(root, "app/Price.sharp", price);
    let analyzed_again = running && watch.until(|seen| rounds(seen) >= 2);
    let failure = format!("{}{}", report(), watch.stop());

    assert!(started, "{failure}");
    assert!(reported, "{failure}");
    assert!(running, "{failure}");
    assert!(analyzed_again, "{failure}");
}

/// A terminal's run compiles an edit into the hosts' folder while a watch waits for a change, so the watch's own check
/// then finds the folder current. The folder's record differs from the one the watch last held, so the watch starts its
/// hosts' workers again, and they run the new class. The test stops the watch once it waits holding no lock of the
/// folder, so the terminal's run compiles first, and lets it go on once that run is done.
#[cfg(unix)]
#[test]
fn a_watch_starts_the_hosts_again_when_another_run_compiled_an_edit_while_it_waited() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host watch test after another run") {
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();

    let watch = Watch::start(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info,mago=debug,mago_database=debug")
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(root)
            .stdout(Stdio::null()),
    );
    let watch_id = libc::pid_t::try_from(watch.id()).unwrap();
    // SAFETY: `kill` only sends a signal, to the watch alone.
    let signal = |signal| unsafe { libc::kill(watch_id, signal) } == 0;
    let started = watch.until(|seen| seen.contains("Initial analysis complete"));
    // The watch lets the hosts' folder go once it waits for a change, so the exclusive lock is taken then, and the
    // watch is stopped while it waits.
    let idle = started && {
        let hosts = std::fs::read_dir(cache.path().join("mago/hosts")).unwrap().next().unwrap().unwrap().path();
        let lock = std::fs::File::options().write(true).open(hosts.join("compile.lock")).unwrap();
        lock.lock().unwrap();
        signal(libc::SIGSTOP)
    };
    let terminal = idle.then(|| {
        write(root, "app/Word.sharp", &word("second"));
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    });
    signal(libc::SIGCONT);
    let restarted = idle && watch.until(|seen| seen.contains("Analysis complete."));
    let seen = watch.stop();

    let hosts = log();
    let runs: Vec<(&str, &str)> = hosts.lines().filter_map(|line| line.rsplit_once(' ')).collect();
    let failure = format!("{hosts}\n{seen}");
    assert!(idle, "the watch never waited for a change: {failure}");
    let terminal = terminal.unwrap();
    assert!(terminal.status.success(), "{}", printed(&terminal));
    assert!(restarted, "{failure}");
    assert_eq!(
        runs.iter().filter(|(_, word)| *word == "first").count(),
        1,
        "the watch's start ran one host: {failure}"
    );
    assert_eq!(runs[0].1, "first", "{failure}");
    assert_eq!(runs[1].1, "second", "the terminal's run: {failure}");
    assert_eq!(runs[2].1, "second", "{failure}");
    assert_ne!(runs[2].0, runs[0].0, "the watch started its hosts again: {failure}");
}

/// A host that types the project's code, as one that gives a plain PHP helper its return type does, can load the
/// classes only its typing makes correct: the hosts' compile lowers every file that lowers, whatever the analysis
/// without the hosts says of it, as Roslyn hands an analyzer a compilation with errors and `tsc` emits despite them.
/// The host here fails its hook unless the compiled file of `app/Greeting.sharp` is there. The `.sharp` folder holds
/// the compile without the hosts (spec section 27), which refuses `app/Greeting.sharp` for the type the hosts give.
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
        "namespace App;\n\nimport App.Helpers;\n\nextern Helpers.untyped uses none;\n\npublic class Greeting\n{\n    public static string hello() => Helpers.untyped();\n}\n",
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.typing]\nfile = \"tools/host.php\"\n",
    );

    for run in ["fresh", "again"] {
        let output = printed(&compile(root));
        assert!(output.contains("app/Greeting.sharp:9:37: error[mixed-return-statement]"), "{run}: {output}");
        assert!(output.contains("1 PHP# file was refused"), "{run}: {output}");
        assert!(!output.contains("isn't compiled for the hosts"), "{run}: {output}");
    }
    assert!(!compiled(root, "app/Greeting.sharp").exists());
    assert!(compiled(root, "app/Order.sharp").exists());
}

/// An extension host whose one plugin types `App\Box::value()` as a string and declines every other method of
/// `App\Box`, as a framework's host types the few methods it knows.
const BOX_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\MethodReturnTypeProvider;
use Mago\Sdk\Analyzer\MethodTarget;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Analyzer\ReturnTypeProviderContext;
use Mago\Sdk\Analyzer\Type;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class Boxes implements Plugin, MethodReturnTypeProvider
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('boxes', 'Boxes', 'Types the value of a box.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerMethodReturnTypeProvider($this);
    }

    public function getTargets(): array
    {
        return [MethodTarget::allMethods('App\\Box')];
    }

    public function getReturnType(ReturnTypeProviderContext $context): ?Type
    {
        return $context->invocation->name === 'value' ? Type::string() : null;
    }
}

(new Worker(new Extension('boxes', 'Boxes', '0.0.0', analyzerPlugins: [new Boxes()])))->run();
"#;

/// A whole `mago analyze` whose host only answers return types reports what an analysis of every file with the host
/// reports, as `mago analyze app` gives it: the type the host gives a call reaches the file that makes it, and a file
/// whose calls the host declines reports what it reports with no host.
#[test]
fn a_whole_analyze_with_a_return_type_host_reports_what_an_analysis_of_every_file_with_the_host_reports() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the return-type host analyze test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "app/Box.php",
        "<?php\n\nnamespace App;\n\nfinal class Box\n{\n    public function value()\n    {\n        return 1;\n    }\n\n    public function label(): string\n    {\n        return 'box';\n    }\n}\n",
    );
    write(
        root,
        "app/Reader.php",
        "<?php\n\nnamespace App;\n\nfunction read_value(Box $box): int\n{\n    return $box->value();\n}\n",
    );
    write(
        root,
        "app/Labels.php",
        "<?php\n\nnamespace App;\n\nfunction read_label(Box $box): int\n{\n    return $box->label();\n}\n",
    );
    write(root, "tools/host.php", BOX_HOST);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.boxes]\nfile = \"tools/host.php\"\n",
    );

    let whole = analyze(root, &[]);
    let every_file = analyze(root, &["app"]);

    let issues = |output: &Output| String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(issues(&whole), issues(&every_file), "{}", printed(&whole));
    for issue in [
        "app/Reader.php:7:12: error[invalid-return-statement]: Invalid return type for function `App\\read_value`: \
         expected `int`, but found `string`.",
        "app/Labels.php:7:12: error[invalid-return-statement]: Invalid return type for function `App\\read_label`: \
         expected `int`, but found `string`.",
    ] {
        assert!(issues(&whole).contains(issue), "{issue}: {}", printed(&whole));
    }
}

/// With `MAGO_LOG=debug`, a whole `mago analyze` with a host logs how long each phase of its compile and its report took,
/// as "Phase scan took 0.12 s.", from the inputs it finds to the report it prints, a `mago analyze` of named files logs
/// the compile of the hosts' folder that runs before it as `hosts`, and at the default level neither logs a phase.
#[test]
fn analyze_logs_each_phase_s_time_at_the_debug_level() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the phase times test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "tools/host.php", BOX_HOST);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.boxes]\nfile = \"tools/host.php\"\n",
    );
    let cache = tempfile::tempdir().unwrap();
    let analyze = |level: &str| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "analyze"])
                .env("MAGO_LOG", level)
                .env("XDG_CACHE_HOME", cache.path())
                .current_dir(root),
        )
        .unwrap()
    };

    let debug = analyze("debug");

    assert!(debug.status.success(), "{}", printed(&debug));
    let log = printed(&debug);
    for phase in ["inputs", "load", "scan", "populate", "analysis", "lowering", "laws", "write", "hosts", "report"] {
        let prefix = format!("Phase {phase} took ");
        let took =
            log.lines().filter_map(|line| line.split_once(&prefix)?.1.strip_suffix(" s.")?.parse::<f64>().ok()).count();
        assert!(took > 0, "no `Phase {phase} took … s.` line: {log}");
    }
    let named = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "app"])
            .env("MAGO_LOG", "debug")
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(root),
    )
    .unwrap();
    assert!(named.status.success(), "{}", printed(&named));
    assert!(printed(&named).contains("Phase hosts took "), "the hosts' compile logs its time: {}", printed(&named));

    let default = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env_remove("MAGO_LOG")
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(root),
    )
    .unwrap();

    assert!(default.status.success(), "{}", printed(&default));
    assert!(!printed(&default).contains("Phase "), "{}", printed(&default));
}

/// An extension host that declares the class `Stubbed` in a stub, whose `name()` returns a string.
const STUB_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\InitializationContext;
use Mago\Sdk\Analyzer\InitializationHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Worker;

final class Stubs implements Plugin, InitializationHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('stubs', 'Stubs', 'Declares a stub.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerInitializationHook($this);
    }

    public function initialize(InitializationContext $context): void
    {
        $context->addStub('stubbed.php', "<?php\n\nfinal class Stubbed\n{\n    public function name(): string\n    {\n        return 'stub';\n    }\n}\n");
    }
}

(new Worker(new Extension('stubs', 'Stubs', '0.0.0', analyzerPlugins: [new Stubs()])))->run();
"#;

/// A whole `mago analyze` whose host declares a stub or reports from an after-file hook reports what an analysis of
/// every file with the host reports, as `mago analyze app` gives it: a file that reads the stub's class reads it, and
/// the hook reports in each file.
#[test]
fn a_whole_analyze_with_a_stub_host_or_a_hook_host_reports_what_an_analysis_of_every_file_with_the_host_reports() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the stub and hook host analyze test") {
        return;
    }
    for (host, reported) in [
        (
            STUB_HOST,
            "app/Names.php:8:12: error[invalid-return-statement]: Invalid return type for function `App\\stubbed_name`: \
             expected `int`, but found `string`.",
        ),
        (MARKER_HOST, "app/Names.php:5:4: error[acme/no-marker]: The marker `host-error-here` is here."),
    ] {
        let directory = project(ORDER, MONEY);
        let root = directory.path();
        write(
            root,
            "app/Names.php",
            "<?php\n\nnamespace App;\n\n// host-error-here\nfunction stubbed_name(\\Stubbed $stubbed): int\n{\n    return $stubbed->name();\n}\n",
        );
        write(root, "tools/host.php", host);
        write(
            root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.host]\nfile = \"tools/host.php\"\n",
        );

        let whole = analyze(root, &[]);
        let every_file = analyze(root, &["app"]);

        let issues = |output: &Output| String::from_utf8_lossy(&output.stdout).into_owned();
        assert_eq!(issues(&whole), issues(&every_file), "{}", printed(&whole));
        assert!(issues(&whole).contains(reported), "{reported}: {}", printed(&whole));
    }
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
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[analyzer]\n{analyzer}\n\n[extension-hosts.acme]\nfile = \"tools/host.php\"\n"
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

/// An extension host whose node hook fails in any file but a PHP# file, so a compile that runs it anywhere else fails.
const SHARP_ONLY_HOST: &str = r#"<?php

declare(strict_types=1);

use Mago\Sdk\Analyzer\NodeAnalysisContext;
use Mago\Sdk\Analyzer\NodeAnalysisHook;
use Mago\Sdk\Analyzer\Plugin;
use Mago\Sdk\Analyzer\PluginDefinition;
use Mago\Sdk\Analyzer\PluginRegistry;
use Mago\Sdk\Extension;
use Mago\Sdk\Syntax\NodeKind;
use Mago\Sdk\Worker;

final class SharpOnlyRule implements Plugin, NodeAnalysisHook
{
    public function getDefinition(): PluginDefinition
    {
        return new PluginDefinition('acme', 'Acme rules', 'Inspects the classes of PHP# files.');
    }

    public function register(PluginRegistry $registry): void
    {
        $registry->registerNodeAnalysisHook($this);
    }

    public function getRequirements(): array
    {
        return [];
    }

    public function getTargets(): array
    {
        return [NodeKind::Class_];
    }

    public function analyze(NodeAnalysisContext $context): void
    {
        if (!str_ends_with($context->analysis->file, '.sharp')) {
            throw new RuntimeException("The node hook ran in {$context->analysis->file}.");
        }
    }
}

(new Worker(new Extension('acme', 'Acme', '0.0.0', analyzerPlugins: [new SharpOnlyRule()])))->run();
"#;

/// `mago compile` lowers only PHP# files and refuses one only for the errors reported in it, and a node hook reports
/// in the file it inspects, so the compile runs its hosts' node hooks in its PHP# files alone: here the classes of
/// `app/Order.sharp` and the package's `Money.sharp`, and never `app/Legacy.php`'s.
#[test]
fn compile_runs_the_node_hooks_only_in_the_php_sharp_files_it_lowers() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scoped node hooks compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "tools/host.php", SHARP_ONLY_HOST);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.acme]\nfile = \"tools/host.php\"\n",
    );

    compile_succeeds(root);
    assert!(compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, MONEY_PATH).exists());
}

/// A host's rule reports only on the project's own files, never on a Composer package's `.sharp` file the compile checks
/// beside them: its error there is left out, while its error on a project file fails the run. The hosts' rules only
/// report (spec section 27), so both files compile.
#[test]
fn compile_reports_a_hosts_issue_only_on_the_projects_own_files() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host issue scope compile test") {
        return;
    }
    let directory = project(ORDER, &MONEY.replace("return 100;", "return 100; // host-error-here"));
    let root = directory.path();
    write(
        root,
        "app/Marked.sharp",
        "namespace App;\n\npublic class Marked\n{\n    public static string text() => \"host-error-here\";\n}\n",
    );
    write(root, "tools/host.php", MARKER_HOST);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.acme]\nfile = \"tools/host.php\"\n",
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(printed(&output).matches("acme/no-marker").count(), 1, "{}", printed(&output));
    assert!(printed(&output).contains("app/Marked.sharp"), "{}", printed(&output));
    assert!(!printed(&output).contains("Money.sharp"), "{}", printed(&output));
    assert!(compiled(root, MONEY_PATH).exists(), "{}", printed(&output));
    assert!(compiled(root, "app/Marked.sharp").exists(), "{}", printed(&output));
}

/// A host's rule reports on each file of `includes` outside a Composer package, as on a file under `paths`: the compile
/// lowers the project's `lib/Shop/Report.sharp` too, so its error there fails the run, and the file compiles.
#[test]
fn compile_reports_a_hosts_issue_on_a_file_of_an_includes_folder_outside_vendor() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the host issue in an includes folder compile test") {
        return;
    }
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "lib/Shop/Report.sharp",
        "namespace Shop;\n\npublic class Report\n{\n    public static string text() => \"host-error-here\";\n}\n",
    );
    write(root, "tools/host.php", MARKER_HOST);
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\", \"lib\"]\n\n[extension-hosts.acme]\nfile = \"tools/host.php\"\n",
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(printed(&output).matches("acme/no-marker").count(), 1, "{}", printed(&output));
    assert!(printed(&output).contains("lib/Shop/Report.sharp"), "{}", printed(&output));
    assert!(compiled(root, "lib/Shop/Report.sharp").exists(), "{}", printed(&output));
    assert!(compiled(root, "app/Order.sharp").exists(), "{}", printed(&output));
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
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.signing]\nfile = \"tools/host.php\"\n",
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
        [
            Path::new("analysis.bin"),
            Path::new("autoload.php"),
            Path::new("compiled.bin"),
            Path::new("current/generation.lock"),
            Path::new("current/listeners.php"),
            Path::new("current/vendor/acme/money/src/Money.sharpc"),
            Path::new("inputs.json"),
            Path::new("write.lock")
        ]
    );
}

#[test]
fn compile_prints_no_color_under_colors_never_even_when_force_color_is_set() {
    let directory = project(BROKEN_ORDER, MONEY);
    let output = process::output(
        command(directory.path(), "never").env("FORCE_COLOR", "1").env("MAGO_REPORTING_FORMAT", "rich"),
    )
    .unwrap();

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("app/Order.sharp"), "{}", printed(&output));
    assert!(!printed(&output).contains("\x1b["), "{}", printed(&output));
}

#[test]
fn compile_prints_color_under_colors_always_even_when_no_color_is_set() {
    let directory = project(BROKEN_ORDER, MONEY);
    let output = process::output(
        command(directory.path(), "always")
            .env_remove("FORCE_COLOR")
            .env("NO_COLOR", "1")
            .env("MAGO_REPORTING_FORMAT", "rich"),
    )
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
    assert_eq!(
        files_under(&root.join(".sharp")),
        [
            Path::new("analysis.bin"),
            Path::new("autoload.php"),
            Path::new("compiled.bin"),
            Path::new("current/app/Order.sharpc"),
            Path::new("current/generation.lock"),
            Path::new("current/listeners.php"),
            Path::new("inputs.json"),
            Path::new("write.lock")
        ]
    );
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
    assert_eq!(files_under(&root.join(".sharp/current/app")), [Path::new("Order.sharpc")], "no temporary file is left");
}

/// A compile writes a new generation of `.sharp` and switches `.sharp/current` to it: each compiled file that didn't
/// change is a hard link to the last generation's, and only a changed one is written. A generation whose lock file a
/// process holds shared stays, and the first compile after its lock is let go deletes it.
#[cfg(unix)]
#[test]
fn compile_links_each_unchanged_file_into_a_new_generation_and_deletes_the_old_one_once_no_process_holds_it() {
    use std::os::unix::fs::MetadataExt;

    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let old = std::fs::canonicalize(root.join(".sharp/current")).unwrap();
    let inode = |path: &Path| std::fs::metadata(path).unwrap().ino();
    let money = inode(&old.join("vendor/acme/money/src/Money.sharpc"));
    let order = inode(&old.join("app/Order.sharpc"));
    let held = std::fs::File::open(old.join(GENERATION_LOCK)).unwrap();
    held.lock_shared().unwrap();

    write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    compile_succeeds(root);

    assert_ne!(std::fs::canonicalize(root.join(".sharp/current")).unwrap(), old, "the compile switched the link");
    assert_eq!(inode(&compiled(root, MONEY_PATH)), money, "the unchanged file is the last generation's");
    assert_ne!(inode(&compiled(root, "app/Order.sharp")), order, "the changed file is written anew");
    assert!(old.is_dir(), "a generation a process holds stays");

    drop(held);
    compile_succeeds(root);
    assert!(!old.exists(), "the compile after the lock is let go deletes the old generation");
}

/// A PHP# project's tree holds only what its authors wrote: the compile writes every file of its own in the `.sharp`
/// folder, and no PHP file and no `.gitignore` beside a source, in the project's folders or in `vendor/`.
#[test]
fn compile_writes_nothing_beside_a_source() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    std::fs::remove_file(root.join("app/Legacy.php")).unwrap();
    write(root, "app/Shop/StoreService.sharp", "namespace App.Shop;\n\npublic class StoreService\n{\n}\n");
    let vendor = files_under(&root.join("vendor"));

    compile_succeeds(root);

    assert!(compiled(root, "app/Shop/StoreService.sharp").exists());
    assert_eq!(files_under(&root.join("app")), [Path::new("Order.sharp"), Path::new("Shop/StoreService.sharp")]);
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
}

/// A PHP file and a PHP# source that declare one class are two declarations of it, which the analysis refuses as it
/// refuses any two.
#[test]
fn analyze_refuses_a_php_file_and_a_source_that_declare_one_class() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Shop/StoreService.sharp", "namespace App.Shop;\n\npublic class StoreService\n{\n}\n");
    write(root, "app/Shop/StoreService.php", "<?php\n\nnamespace App\\Shop;\n\nfinal class StoreService\n{\n}\n");

    let output = analyze(root, &[]);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[duplicate-definition]: Class `App\\Shop\\StoreService` is already defined in `app/Shop/StoreService.sharp`."
        ),
        "{}",
        printed(&output)
    );
}

/// Composer's loader finds a class's `.php` file before the PHP# loader finds its `.sharp` file, so a plain PHP class
/// that takes a PHP# class's name is what a request would run. It meets that PHP# type, and the compile refuses its file
/// with the analysis's duplicate-definition error, and still compiles the `.sharp` file.
#[test]
fn compile_refuses_a_plain_php_class_that_takes_a_php_sharp_class_s_name() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Shop/StoreService.sharp", "namespace App.Shop;\n\npublic class StoreService\n{\n}\n");
    write(root, "app/Shop/StoreService.php", "<?php\n\nnamespace App\\Shop;\n\nfinal class StoreService\n{\n}\n");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shop/StoreService.php:5:1: error[duplicate-definition]: Class `App\\Shop\\StoreService` is already defined in `app/Shop/StoreService.sharp`."
        ),
        "{}",
        printed(&output)
    );
    assert!(printed(&output).contains("1 plain PHP file was refused"), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/StoreService.sharp").exists());
}

/// The compiled files get the mode any new file gets, `0644` under the usual umask, as the source itself got, so a
/// server that runs PHP as another user, as PHP-FPM's pool user, reads them.
#[cfg(unix)]
#[test]
fn compile_writes_files_with_the_mode_of_a_new_file() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let source = mode(&root.join("app/Order.sharp"));
    for written in [compiled(root, "app/Order.sharp"), root.join(".sharp/current/listeners.php")] {
        assert_eq!(mode(&written), source, "{}", written.display());
    }
}

/// A compile leaves every compiled file current, as the engine checks it, a file whose namespace holds a class named
/// like a default class included: its compiled file stamps the PHP file that class's name could name as an input.
#[test]
fn every_compiled_file_is_current_after_a_compile() {
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

    for name in ["app/Environment.sharp", "app/Order.sharp", "app/Report.sharp", MONEY_PATH] {
        let bytes = std::fs::read(compiled(root, name)).unwrap();
        assert!(!inputs_changed(&std::fs::canonicalize(root).unwrap(), &bytes).unwrap(), "{name}");
    }
}

/// A compiled file goes stale when a file it reads changes, and stays current when a file it never reads changes:
/// here the files of an included folder outside `vendor/` that declare class aliases apart from their classes, as
/// WordPress's bundled libraries declare theirs. `Lib\Pace` is an alias of `Lib\Rapid`, an alias of `Lib\Rate`.
#[test]
fn a_compiled_file_goes_stale_only_when_a_file_it_reads_changes() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\", \"public/wp\"]\n",
    );
    write(
        root,
        "public/wp/Rate.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Rate\n{\n    public static function percent(): int\n    {\n        return 5;\n    }\n}\n",
    );
    write(root, "public/wp/rapid.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'Lib\\\\Rapid');\n");
    write(root, "public/wp/aliases.php", "<?php\n\nclass_alias('Lib\\\\Rapid', 'Lib\\\\Pace');\n");
    write(
        root,
        "app/Paced.sharp",
        "namespace App;\n\nimport Lib.Pace;\nimport Lib.Rate;\n\nextern Rate uses none;\n\npublic class Paced\n{\n    public int percent()\n    {\n        return Pace.percent();\n    }\n}\n",
    );
    let root = std::fs::canonicalize(root).unwrap();
    let stale = |name: &str| inputs_changed(&root, &std::fs::read(compiled(&root, name)).unwrap()).unwrap();

    for alias_file in ["public/wp/aliases.php", "public/wp/rapid.php"] {
        compile_succeeds(&root);
        assert!(!stale("app/Order.sharp") && !stale("app/Paced.sharp"));

        let aliases = std::fs::read_to_string(root.join(alias_file)).unwrap();
        write(&root, alias_file, &format!("{aliases}\n// An edit.\n"));

        assert!(stale("app/Paced.sharp"), "app/Paced.sharp reads the alias {alias_file} declares");
        assert!(!stale("app/Order.sharp"), "app/Order.sharp reads nothing of {alias_file}");
    }
}

/// The modification time of the compiled file of `name`, which every compile that runs writes anew.
fn compiled_at(root: &Path, name: &str) -> std::time::SystemTime {
    std::fs::metadata(compiled(root, name)).unwrap().modified().unwrap()
}

/// A compile with nothing to do analyzes nothing and writes nothing, and exits and prints as the compile before it did.
#[test]
fn a_compile_with_nothing_to_do_writes_nothing_and_prints_what_the_last_compile_printed() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let full = compile(root);
    assert!(full.status.success(), "{}", printed(&full));
    let written = compiled_at(root, "app/Order.sharp");

    let nothing_to_do = compile(root);

    assert_eq!(nothing_to_do.status.code(), full.status.code());
    assert_eq!(nothing_to_do.stdout, full.stdout);
    assert_eq!(logged(&nothing_to_do), logged(&full));
    assert_eq!(compiled_at(root, "app/Order.sharp"), written, "the compile wrote nothing");
}

/// The number of threads changes nothing a compile writes, so runs with their own `--threads`, as an editor's and a
/// terminal's, share one compile: the second has nothing to do.
#[test]
fn compiles_with_different_threads_share_one_compile() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let compile_on = |threads: &str| {
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "--threads", threads, "compile"])
                .env("MAGO_REPORTING_FORMAT", "medium")
                .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
                .current_dir(root),
        )
        .unwrap()
    };
    let full = compile_on("1");
    assert!(full.status.success(), "{}", printed(&full));
    let written = compiled_at(root, "app/Order.sharp");

    let shared = compile_on("2");

    assert!(shared.status.success(), "{}", printed(&shared));
    assert_eq!(compiled_at(root, "app/Order.sharp"), written, "the compile on 2 threads wrote nothing");
}

/// Compiles of one tree that start together take turns on the `.sharp` folder's write lock, so none deletes a file
/// another is writing, each passes, and the folder holds what one compile writes. Each round edits every source first,
/// so every run finds the folder stale, and 60 more files keep each compile writing long enough for three runs to meet
/// in the folder.
#[test]
fn compiles_that_start_together_take_turns_on_the_sharp_folder() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let class = |class: &str, word: &str| {
        format!(
            "namespace App.Words;\n\npublic class {class}\n{{\n    public static string text() => \"{word}\";\n}}\n"
        )
    };

    for round in 0..10 {
        let word = if round % 2 == 0 { "first" } else { "second" };
        for file in 0..60 {
            write(root, &format!("app/Words/Word{file}.sharp"), &class(&format!("Word{file}"), word));
        }

        let outputs: Vec<Output> = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..3).map(|_| scope.spawn(|| compile(root))).collect();
            runs.into_iter().map(|run| run.join().unwrap()).collect()
        });

        for output in &outputs {
            assert!(output.status.success(), "round {round}: {}", printed(output));
        }
    }
    let together = files_under(&root.join(".sharp"));
    std::fs::remove_dir_all(root.join(".sharp")).unwrap();
    compile_succeeds(root);

    assert_eq!(together, files_under(&root.join(".sharp")));
}

/// A compile that starts while another run holds the `.sharp` folder's write lock writes nothing until the lock is let
/// go, and then compiles.
#[test]
fn a_compile_takes_its_turn_after_the_run_that_holds_the_write_lock() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let held = mago_database::lock::exclusive(&root.join(".sharp/write.lock")).unwrap();

    let output = std::thread::scope(|scope| {
        let run = scope.spawn(|| compile(root));
        // A compile of a copy of the project, run meanwhile, takes as long as one that took no turn would.
        let copy = project(ORDER, MONEY);
        compile_succeeds(copy.path());
        assert!(!compiled(root, "app/Order.sharp").exists(), "the compile wrote while the lock was held");
        drop(held);
        run.join().unwrap()
    });

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Order.sharp").exists());
}

/// Two compiles of one tree write the same record of the compiled files, byte for byte, on one thread and on several:
/// each list the record keeps, such as what a file read of the classes of its namespace, keeps one order.
#[test]
fn compiles_of_one_tree_write_the_same_record_on_any_number_of_threads() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let items: Vec<String> = (0..12).map(|item| format!("Item{item}.value()")).collect();
    for item in 0..12 {
        write(
            root,
            &format!("app/Item{item}.sharp"),
            &format!("namespace App;\n\npublic class Item{item}\n{{\n    public static int value() => {item};\n}}\n"),
        );
    }
    write(
        root,
        "app/Catalog.sharp",
        &format!(
            "namespace App;\n\npublic class Catalog\n{{\n    public static int total() => {};\n}}\n",
            items.join(" + ")
        ),
    );
    let record_on = |threads: &str| {
        let output = process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never", "--threads", threads, "compile"])
                .env("MAGO_REPORTING_FORMAT", "medium")
                .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
                .current_dir(root),
        )
        .unwrap();
        assert!(output.status.success(), "{}", printed(&output));

        let record = std::fs::read(root.join(".sharp/compiled.bin")).unwrap();
        std::fs::remove_dir_all(root.join(".sharp")).unwrap();

        record
    };

    let on_one = record_on("1");

    assert_eq!(record_on("4"), on_one, "the compile on 4 threads wrote another record");
    assert_eq!(record_on("4"), on_one, "the second compile on 4 threads wrote another record");
}

/// Each message `output` logged, without the time, level, thread and module a debug build's log puts before it.
fn logged(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(|line| line.split_once(": ").map_or(line, |(_, message)| message).to_owned())
        .collect()
}

/// `Acme\Rate` in `vendor/`, whose `percent()` returns `returned`.
fn rate(returned: &str) -> String {
    format!(
        "<?php\n\nnamespace Acme;\n\nfinal class Rate\n{{\n    public static function percent(): {returned}\n    {{\n        return 5;\n    }}\n}}\n"
    )
}

/// A `vendor/` file is no input of the compiled file of a PHP# file that calls it, since `composer.lock` stands for
/// it, so the compiled file stays current after an edit of it the PHP# file no longer compiles against.
#[test]
fn compile_refuses_a_php_sharp_file_after_an_edit_of_the_vendor_file_it_calls() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "vendor/acme/rate/src/Rate.php", &rate("int"));
    write(
        root,
        "app/Paced.sharp",
        "namespace App;\n\nimport Acme.Rate;\n\nextern Rate uses none;\n\npublic class Paced\n{\n    public int percent() => Rate.percent();\n}\n",
    );
    compile_succeeds(root);

    write(root, "vendor/acme/rate/src/Rate.php", &rate("string"));
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("app/Paced.sharp"), "{}", printed(&output));
    assert!(!compiled(root, "app/Paced.sharp").exists());
}

/// Spec sections 1 and 14.3: an `extern` property of a database entity takes an attribute whose argument is a `new`
/// of constants, as any property does, and its file compiles, whatever the `new` holds: a template that reads a class
/// constant, an enum case's `value`, a map literal or a plain string, two properties of one shape in one file included.
#[test]
fn an_extern_property_whose_attribute_holds_a_new_of_constants_compiles() {
    let directory = library_project(&[
        (
            "app/DatabaseEntity.php",
            "<?php\n\nnamespace App;\n\nabstract class DatabaseEntity\n{\n    private array $stored = [];\n\n    public function __get(string $name): mixed { return $this->stored[$name] ?? null; }\n    public function __set(string $name, mixed $value): void { $this->stored[$name] = $value; }\n}\n",
        ),
        (
            "app/Designer/Config.sharp",
            "namespace App.Designer;\n\npublic final class Config\n{\n    public Config(string? help = null, string? @default = null, Map<string, string>? placeholder = null) { }\n}\n",
        ),
        (
            "app/Designer/Field.sharp",
            "namespace App.Designer;\n\n[Attribute]\npublic final class Field\n{\n    public Field(string label, Config? config = null) { }\n}\n",
        ),
        (
            "app/Designer/Visibility.sharp",
            "namespace App.Designer;\n\npublic enum Visibility : string { case Public = \"public\"; }\n",
        ),
        (
            "app/Designer/Sheet.sharp",
            "namespace App.Designer;\n\nimport App.DatabaseEntity;\n\npublic class Sheet : DatabaseEntity\n{\n    public const string LAYERS = \"reset, theme\";\n\n    [Field(label: \"Layer\", config: new Config(help: `Serves into ${Sheet.LAYERS}.`))]\n    public extern string? layer { get; set; }\n\n    [Field(label: \"Who\", config: new Config(@default: Visibility.Public.value))]\n    public extern Visibility visibility { get; set; }\n\n    [Field(label: \"Audience\", config: new Config(@default: Visibility.Public.value))]\n    public extern Visibility audience { get; set; }\n\n    [Field(label: \"Headers\", config: new Config(placeholder: [\"name\": \"Authorization\", \"value\": \"Bearer key\"]))]\n    public extern Map<string, string> headers { get; set; }\n\n    [Field(label: \"Plain\", config: new Config(help: \"x\"))]\n    public extern string? plain { get; set; }\n}\n",
        ),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(compiled(root, "app/Designer/Sheet.sharp").exists());
}

/// A plain PHP class that implements a PHP# interface is no input of any compiled file, so every compiled file stays
/// current when the class changes or a new one appears, though the compile refuses it.
#[test]
fn compile_refuses_a_plain_php_class_that_changed_or_appeared_since_the_last_compile() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let bare = |returned: &str| {
        format!(
            "<?php\n\nnamespace App;\n\nclass Bare implements Keyed\n{{\n    use Keyed\\Defaults;\n\n    public function getKey(): {returned}\n    {{\n        return 7;\n    }}\n}}\n"
        )
    };
    write(root, "app/Keyed.sharp", KEYED);
    write(root, "app/Bare.php", &bare("int"));
    compile_succeeds(root);

    write(root, "app/Bare.php", &bare("int|string|null"));
    let changed = compile(root);
    write(root, "app/Bare.php", &bare("int"));
    compile_succeeds(root);
    write(root, "app/Other.php", "<?php\n\nnamespace App;\n\nclass Other implements Keyed\n{\n}\n");
    let appeared = compile(root);

    assert_eq!(changed.status.code(), Some(1), "{}", printed(&changed));
    assert!(printed(&changed).contains("1 plain PHP file was refused"), "{}", printed(&changed));
    assert_eq!(appeared.status.code(), Some(1), "{}", printed(&appeared));
    assert!(
        printed(&appeared).contains("`App\\Other` implements `App\\Keyed` without its default bodies"),
        "{}",
        printed(&appeared)
    );
}

/// Spec section 32: the engine reads a class's modules by the namespaces they serve, as the listener table records
/// them, and a `Module.sharp` at the root, outside the configured paths, serves none, so one that appears leaves each
/// compiled file current.
#[test]
fn a_module_file_that_appears_at_the_root_outside_the_paths_leaves_each_compiled_file_current() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let written = compiled_at(root, "app/Order.sharp");

    write(root, "Module.sharp", "module\n{\n}\n");
    compile_succeeds(root);

    assert_eq!(compiled_at(root, "app/Order.sharp"), written, "the compile wrote nothing");
}

/// The deprecated `perform-heuristic-checks`, which serde never writes back, changes the analysis, so a change of it
/// makes the compile run and write its record again.
#[test]
fn compile_runs_again_after_a_change_of_perform_heuristic_checks() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let configured = |checks: bool| {
        format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[analyzer]\nperform-heuristic-checks = {checks}\n"
        )
    };
    let record = || std::fs::read(root.join(".sharp/inputs.json")).unwrap();
    write(root, "mago.toml", &configured(true));
    compile_succeeds(root);
    let written = record();

    write(root, "mago.toml", &configured(false));
    compile_succeeds(root);

    assert_ne!(record(), written);
}

/// Another build of the checker lowers to other code, so a record another build wrote makes the compile run and
/// write the record of this build.
#[test]
fn compile_runs_again_after_another_build_of_the_checker_compiled() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let record = root.join(".sharp/inputs.json");
    let inputs = || serde_json::from_slice::<serde_json::Value>(&std::fs::read(&record).unwrap()).unwrap();
    let mut written = inputs();
    let build = written["build"].clone();
    written["build"][0] = serde_json::json!(build[0].as_u64().unwrap() ^ 1);
    std::fs::write(&record, serde_json::to_vec(&written).unwrap()).unwrap();

    compile_succeeds(root);

    assert_eq!(inputs()["build"], build);
}

#[test]
fn compile_writes_a_deleted_compiled_file_again() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::remove_file(compiled(root, "app/Order.sharp")).unwrap();
    compile_succeeds(root);

    assert!(compiled(root, "app/Order.sharp").exists());
}

#[test]
fn compile_reports_a_sharp_folder_that_appeared_inside_a_package() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);

    std::fs::create_dir_all(root.join("vendor/acme/money/.sharp")).unwrap();
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Delete vendor/acme/money/.sharp."), "{}", printed(&output));
}

/// A rewrite of the same size whose modification time is set back, as `touch -d` sets it, still changes the file's
/// status change time, so the compile reads the new source.
#[test]
fn compile_reads_a_source_rewritten_at_the_same_size_with_its_modification_time_set_back() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let source = root.join("app/Order.sharp");
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let rewritten = ORDER.replace("extra + 1", "extra + 2");
    assert_eq!(rewritten.len(), ORDER.len());

    write(root, "app/Order.sharp", &rewritten);
    std::fs::File::options().write(true).open(&source).unwrap().set_modified(modified).unwrap();
    assert_eq!(std::fs::metadata(&source).unwrap().modified().unwrap(), modified);
    compile_succeeds(root);

    let header = header(&std::fs::read(compiled(root, "app/Order.sharp")).unwrap()).unwrap();
    assert_eq!(header.source_hash, source_hash(rewritten.as_bytes()));
}

/// A refused compile keeps its record and what it printed, so a compile with every input as it was prints the same
/// refusal and fails again without compiling, and the first changed input compiles.
#[test]
fn a_compile_after_a_refused_one_with_nothing_changed_refuses_the_file_again_without_compiling() {
    let directory = project(BROKEN_ORDER, MONEY);
    let root = directory.path();
    let refused = compile(root);
    assert_eq!(refused.status.code(), Some(1), "{}", printed(&refused));
    let recorded = || std::fs::metadata(root.join(".sharp/inputs.json")).unwrap().modified().unwrap();
    let record = recorded();
    let money = compiled_at(root, MONEY_PATH);

    let again = compile(root);

    assert_eq!(again.status.code(), Some(1), "{}", printed(&again));
    assert_eq!(again.stdout, refused.stdout);
    assert_eq!(logged(&again), logged(&refused));
    assert!(printed(&again).contains("1 PHP# file was refused"), "{}", printed(&again));
    assert_eq!(compiled_at(root, MONEY_PATH), money, "the compile wrote nothing");
    assert_eq!(recorded(), record, "the compile kept the record");

    write(root, "app/Order.sharp", ORDER);
    compile_succeeds(root);
    assert!(compiled(root, "app/Order.sharp").exists());
}

/// A compile that fails with an error of its own, as a source that links to a file outside the project, prints it
/// again from its record while nothing changed.
#[cfg(unix)]
#[test]
fn a_compile_after_a_failed_one_with_nothing_changed_prints_each_error_again() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), "Money.sharp", MONEY);
    std::fs::remove_file(root.join(MONEY_PATH)).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("Money.sharp"), root.join(MONEY_PATH)).unwrap();
    let failed = compile(root);
    assert_eq!(failed.status.code(), Some(1), "{}", printed(&failed));
    assert!(printed(&failed).contains("vendor/acme/money/src/Money.sharp is a link to"), "{}", printed(&failed));

    let again = compile(root);

    assert_eq!(again.status.code(), Some(1), "{}", printed(&again));
    assert_eq!(logged(&again), logged(&failed));
    assert!(root.join(".sharp/inputs.json").exists(), "the compile kept the record");
}

/// The engine holds the lock of `.sharp/compile.lock` while it compiles on demand, so the compile keeps that file, as
/// a compile that deleted it would leave the next request a lock on another file.
#[test]
fn compile_keeps_the_engine_s_compile_lock() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, ".sharp/compile.lock", "");

    compile_succeeds(root);

    assert!(root.join(".sharp/compile.lock").exists());
}

/// The walk follows a link as the loader does, and the compile writes the compiled file at the source's real path, so
/// a link someone points at an identical copy elsewhere, as `ln -sfn` does, makes the compile run, though no file's
/// bytes changed.
#[cfg(unix)]
#[test]
fn compile_writes_the_compiled_file_at_the_new_target_of_a_link_to_an_identical_copy() {
    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    write(&root, "shared/one/Module.sharp", "module\n{\n}\n");
    write(&root, "shared/two/Module.sharp", "module\n{\n}\n");
    let link = |target: &str| {
        let linked =
            process::status(Command::new("ln").args(["-sfn", target, "app/Shared"]).current_dir(&root)).unwrap();
        assert!(linked.success());
    };
    link("../shared/one");
    compile_succeeds(&root);
    assert!(compiled(&root, "shared/one/Module.sharp").exists());

    link("../shared/two");
    compile_succeeds(&root);

    assert!(compiled(&root, "shared/two/Module.sharp").exists());
    assert!(!compiled(&root, "shared/one/Module.sharp").exists());
}

/// A compile that warns keeps no record, so the next compile prints the warning again, until the cause is gone. Here a
/// link back to its own folder, which the loader warns of and skips.
#[cfg(unix)]
#[test]
fn compile_warns_again_until_the_cause_of_its_warning_is_gone() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    std::os::unix::fs::symlink(".", root.join("app/Loop")).unwrap();
    let first = compile(root);
    assert!(first.status.success(), "{}", printed(&first));
    assert!(printed(&first).contains("Skipping symlink loop at"), "{}", printed(&first));

    let second = compile(root);

    assert!(second.status.success(), "{}", printed(&second));
    assert_eq!(logged(&second), logged(&first));
    std::fs::remove_file(root.join("app/Loop")).unwrap();
    compile_succeeds(root);
    assert!(root.join(".sharp/inputs.json").exists(), "a compile with no warning keeps its record");
}

/// A `.sharp` folder the compile can't open fails the compile with the path it failed on and what the compile was
/// doing, here the write lock the compile takes before it reads the folder. Root opens a folder of mode 000, so the test
/// has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_path_in_the_sharp_folder_it_cannot_open() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    let compiled = root.join(".sharp");
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&compiled).is_ok() {
        std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = compile(&root);
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "can't write the file `{}`: Permission denied (os error 13)",
            compiled.join("write.lock").display()
        )),
        "{}",
        printed(&output)
    );
}

/// A folder below `.sharp` the compile can't open fails the compile with the path it failed on there, not `.sharp`.
/// Root opens a folder of mode 000, so the test has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_path_below_the_sharp_folder_it_cannot_open() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    let app = root.join(".sharp/current/app");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&app).is_ok() {
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = compile(&root);
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "can't read the file `{}`: Permission denied (os error 13)",
            app.join("Order.sharpc").display()
        )),
        "{}",
        printed(&output)
    );
}

/// A folder of `.sharp` the compile can't create, here the folder of its new generation, is named as the folder, not as
/// a compiled file it would hold. Root writes into a folder of mode 555, so the test has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_folder_of_the_sharp_folder_it_cannot_create() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    write(&root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    let compiled = root.join(".sharp");
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::create_dir(compiled.join("probe")).is_ok() {
        std::fs::remove_dir(compiled.join("probe")).unwrap();
        std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = compile(&root);
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!("can't create the folder `{}", compiled.join(GENERATION_PREFIX).display())),
        "{}",
        printed(&output)
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
        "<?php\n\nreturn [\n    'inputs' => [\n{inputs}    ],\n    'listeners' => [\n{rows}    ],\n    'dispatcher' => {dispatcher},\n    'packages' => [\n    ],\n    'attributes' => [\n    ],\n    'effects' => [\n    ],\n    'tags' => [\n    ],\n    'modules' => [\n    ],\n];\n"
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
        read(root, ".sharp/current/listeners.php"),
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
        read(root, ".sharp/current/listeners.php"),
        listener_table(root, &every_file, "        ['App\\Receipts', 'deliver', ['App\\OrderPaid'], true],\n", "null")
    );

    std::fs::remove_file(root.join("app/OrderNotices.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Receipts.sharp")).unwrap();
    write(root, "app/Shop/Cart.sharp", "namespace App.Shop;\n\npublic class Cart\n{\n}\n");
    compile_succeeds(root);
    assert_eq!(
        read(root, ".sharp/current/listeners.php"),
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
    let table = read(root, ".sharp/current/listeners.php");
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

/// Spec section 15: the dispatcher is one service for the whole app, so the listener table names the namespace of the
/// app's top module that binds it, whose binding serves every `emit`, and none when no app module binds it. A binding
/// below the top one is refused, and the table still names the top one.
#[test]
fn compile_names_the_app_module_that_binds_the_dispatcher() {
    let recorder = "namespace App;\n\nimport Sharp.Dispatcher;\n\npublic class Recorder : Dispatcher\n{\n    public void dispatch(Any emitted, List<Function<void(Any)>> listeners) { }\n}\n";
    let shop = dispatcher_module("App.Shop");
    let [composer, dispatcher] = LIBRARY_DISPATCHER;
    let directory =
        lawful_project(&[composer, dispatcher, ("app/Recorder.sharp", recorder), ("app/Shop/Module.sharp", &shop)]);
    let root = directory.path();
    release_library(&root.join("vendor/heyjordanparker/php-sharp-composer"));

    compile_succeeds(root);
    assert!(read(root, ".sharp/current/listeners.php").contains("    'dispatcher' => 'app\\\\shop',\n"));

    write(root, "app/Module.sharp", &dispatcher_module("App"));
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Only `app/Module.sharp` binds `Dispatcher`"), "{}", printed(&output));
    assert!(read(root, ".sharp/current/listeners.php").contains("    'dispatcher' => 'app',\n"));

    std::fs::remove_file(root.join("app/Shop/Module.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Module.sharp")).unwrap();
    compile_succeeds(root);
    assert!(read(root, ".sharp/current/listeners.php").contains("    'dispatcher' => null,\n"));
}

/// Spec section 32: a class's services come from the modules of its namespace and of each namespace above it, wherever
/// its file is, so the listener table records each base module's file under the namespace it serves, and each
/// environment module's under that namespace and its environment's name, for the engine to read by namespace.
#[test]
fn compile_records_each_module_under_the_namespace_it_serves() {
    let directory = lawful_project(&[
        ("app/Module.sharp", "namespace App;\n\nmodule\n{\n}\n"),
        ("app/Shop/Module.sharp", "namespace App.Shop;\n\nmodule\n{\n}\n"),
        ("app/Shop/Module.Production.sharp", "namespace App.Shop;\n\nmodule : App.Shop\n{\n}\n"),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    let table = read(root, ".sharp/current/listeners.php");
    assert!(
        table.ends_with(
            "    'modules' => [\n        'app' => ['app/Module.sharp'],\n        'app\\\\shop' => ['app/Shop/Module.sharp'],\n        'app\\\\shop Production' => ['app/Shop/Module.Production.sharp'],\n    ],\n];\n"
        ),
        "{table}"
    );
}

/// Spec section 32: in a project that is the standard library's own package, as on a fresh clone of it, only the
/// library's `Sharp` module serves the root namespace, which the library's autoloader reads itself. Each other module
/// serves its own namespace, as in any project, so the listener table records it.
#[test]
fn compile_records_a_module_of_the_standard_library_s_package_under_its_own_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(root, "composer.json", "{\"name\": \"heyjordanparker/php-sharp-composer\"}\n");
    write(root, "mago.toml", "[source]\npaths = [\"src\", \"library\"]\nextensions = [\"sharp\"]\n");
    write(root, "library/Sharp/Module.sharp", "namespace Sharp;\n\nmodule\n{\n}\n");
    write(root, "src/App/Module.sharp", "namespace App;\n\nmodule\n{\n}\n");

    compile_succeeds(root);
    let table = read(root, ".sharp/current/listeners.php");
    assert!(table.ends_with("    'modules' => [\n        'app' => ['src/App/Module.sharp'],\n    ],\n];\n"), "{table}");
}

/// Spec section 32: a program's services come from the modules of its namespace and of each namespace above it,
/// wherever its file is, so an edit to `app/Module.sharp` writes again the compiled program of `App.Testing.Lib` under
/// `tests/lib`, and a file added beside the module writes no other compiled file again.
#[test]
fn an_edit_to_a_module_writes_again_each_program_of_its_namespace_wherever_its_file_is() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"tests\"]\nincludes = [\"vendor\"]\n",
    );
    write(
        root,
        "composer.json",
        "{\"type\": \"library\", \"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}, \"autoload-dev\": {\"psr-4\": {\"Tests\\\\\": \"tests/\"}}}\n",
    );
    write(root, "composer.lock", "{}\n");
    let runtime = |class: &str| format!("namespace App.Shared;\n\npublic class {class} : Runtime\n{{\n}}\n");
    let module = |class: &str| {
        format!(
            "namespace App;\n\nimport App.Shared.Runtime;\nimport App.Shared.{class};\n\nmodule\n{{\n    singleton Runtime = {class};\n}}\n"
        )
    };
    write(root, "app/Shared/Runtime.sharp", "namespace App.Shared;\n\npublic interface Runtime\n{\n}\n");
    write(root, "app/Shared/LiveRuntime.sharp", &runtime("LiveRuntime"));
    write(root, "app/Shared/QuietRuntime.sharp", &runtime("QuietRuntime"));
    write(root, "app/Module.sharp", &module("LiveRuntime"));
    write(
        root,
        "tests/lib/Link.sharp",
        "namespace App.Testing.Lib;\n\nimport App.Shared.Runtime;\n\npublic class Link\n{\n    public Link(private Runtime runtime) { }\n\n    public void main()\n    {\n    }\n}\n",
    );
    compile_succeeds(root);
    let link = compiled_at(root, "tests/lib/Link.sharp");

    write(root, "app/Module.sharp", &module("QuietRuntime"));
    compile_succeeds(root);
    assert_ne!(compiled_at(root, "tests/lib/Link.sharp"), link, "the edit of App's module wrote Link again");

    let names =
        ["tests/lib/Link.sharp", "app/Module.sharp", "app/Shared/Runtime.sharp", "app/Shared/LiveRuntime.sharp"];
    let written: Vec<std::time::SystemTime> = names.iter().map(|name| compiled_at(root, name)).collect();
    write(root, "app/Foo.sharp", "namespace App;\n\npublic class Foo\n{\n}\n");
    compile_succeeds(root);
    for (name, at) in names.iter().zip(written) {
        assert_eq!(compiled_at(root, name), at, "adding app/Foo.sharp wrote {name} again");
    }
}

/// Spec section 29: the listener table names, for each plain PHP class, method and function an `extern` line gives
/// attributes, the file of each such line, which the engine compiles when reflection reads its attributes. A class is
/// keyed by its full name, a method as `class::method` and a function as `function()`, each in lowercase as PHP looks
/// it up, and a function of a namespace by its full name. A line without attributes names nothing.
#[test]
fn compile_lists_the_files_whose_extern_lines_give_a_declaration_attributes() {
    let group = "<?php\n\nnamespace Lib;\n\n#[\\Attribute(\\Attribute::TARGET_ALL | \\Attribute::IS_REPEATABLE)]\nfinal class Group\n{\n    public function __construct(public string $name) {}\n}\n";
    let mailer = "<?php\n\nnamespace Lib;\n\nfinal class Mailer\n{\n    public function sendLater(): void {}\n}\n";
    let functions = "<?php\n\nnamespace Lib;\n\nfunction sentAt(): int\n{\n    return 1;\n}\n";
    let first = "namespace App.Stubs;\n\nimport Lib;\nimport Lib.Group;\nimport Lib.Mailer;\n\n[Group(\"mail\")]\nextern Mailer uses none;\nextern Group uses none;\n[Group(\"later\")]\nextern Mailer.sendLater uses none;\n[Group(\"time\")]\nextern Lib.sentAt uses none;\n\npublic class First\n{\n}\n";
    let directory = lawful_project(&[
        ("vendor/acme/lib/Group.php", group),
        ("vendor/acme/lib/Mailer.php", mailer),
        ("vendor/acme/lib/functions.php", functions),
        ("app/Stubs/First.sharp", first),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(
        read(root, ".sharp/current/listeners.php").contains(
            "    'attributes' => [\n        'lib\\\\mailer' => ['app/Stubs/First.sharp'],\n        'lib\\\\mailer::sendlater' => ['app/Stubs/First.sharp'],\n        'lib\\\\sentat()' => ['app/Stubs/First.sharp'],\n    ],\n    'effects' => [\n"
        ),
        "{}",
        read(root, ".sharp/current/listeners.php")
    );
}

/// Spec section 29: the listener table gives, for each plain PHP class and method an `extern` line declares, the
/// effects its `uses` lists by their dotted full names, none for a line that declares it pure and `null` for one that
/// says `uses any`, keyed as its attributes are, which `Method.createDelegate` holds a plain PHP method to, spec section
/// 25. A function is left out, since reflection gives it no `Method`.
#[test]
fn compile_lists_the_effects_each_extern_line_declares_for_a_class_and_a_method() {
    let mailer = "<?php\n\nnamespace Lib;\n\nfinal class Mailer\n{\n    public function send(): void {}\n\n    public function count(): int\n    {\n        return 0;\n    }\n}\n";
    let functions = "<?php\n\nnamespace Lib;\n\nfunction sentAt(): int\n{\n    return 1;\n}\n";
    let smtp = "namespace App.Mail;\n\npublic effect abstract class Smtp\n{\n}\n";
    let stubs = "namespace App.Stubs;\n\nimport App.Mail.Smtp;\nimport Lib;\nimport Lib.Mailer;\n\nextern Mailer uses none;\nextern Mailer.send uses Smtp;\nextern Mailer.count uses any;\nextern Lib.sentAt uses Smtp;\n\npublic class Stubs\n{\n}\n";
    let directory = lawful_project(&[
        ("vendor/acme/lib/Mailer.php", mailer),
        ("vendor/acme/lib/functions.php", functions),
        ("app/Mail/Smtp.sharp", smtp),
        ("app/Stubs/Stubs.sharp", stubs),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(
        read(root, ".sharp/current/listeners.php").ends_with(
            "    'effects' => [\n        'lib\\\\mailer' => [],\n        'lib\\\\mailer::count' => null,\n        'lib\\\\mailer::send' => ['App.Mail.Smtp'],\n    ],\n    'tags' => [\n    ],\n    'modules' => [\n    ],\n];\n"
        ),
        "{}",
        read(root, ".sharp/current/listeners.php")
    );
}

/// Spec section 29: the listener table lists the folder Composer installs each of the project's packages in, those
/// `composer.lock` lists, dev-only or not, under its `config.vendor-dir`, in which the engine finds a plain PHP
/// method's own package.
#[test]
fn compile_lists_the_install_folder_of_each_package_under_the_vendor_dir() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "composer.json", "{\"type\": \"library\", \"config\": {\"vendor-dir\": \"libs\"}}\n");
    write(
        root,
        "composer.lock",
        "{\"packages\": [{\"name\": \"acme/money\"}], \"packages-dev\": [{\"name\": \"phpunit/phpunit\"}]}\n",
    );

    compile_succeeds(root);

    assert!(
        read(root, ".sharp/current/listeners.php")
            .contains("    'dispatcher' => null,\n    'packages' => [\n        'libs/acme/money/',\n        'libs/phpunit/phpunit/',\n    ],\n    'attributes' => [\n"),
        "{}",
        read(root, ".sharp/current/listeners.php")
    );
}

/// Spec sections 20.2 and 25: a machine that stores as its case's value is listed with the families, under no key,
/// each case under its value, so `withTag` and `tag()` read its cases as they read a `[Tag]` family's variants.
#[test]
fn compile_lists_a_machine_that_stores_as_its_value_with_each_case_under_its_value() {
    let status = "namespace App.Shop;\n\npublic machine JourneyStatus : string\n{\n    case Open = \"open\" { public JourneyStatus.Completed complete() => JourneyStatus.Completed; }\n    final case Completed = \"completed\";\n    public static JourneyStatus.Open enter() => JourneyStatus.Open;\n}\n";
    let directory = lawful_project(&[("app/Shop/JourneyStatus.sharp", status)]);
    let root = directory.path();

    compile_succeeds(root);

    assert!(
        read(root, ".sharp/current/listeners.php").ends_with(
            "    'tags' => [\n        'App\\\\Shop\\\\JourneyStatus' => ['', ['completed' => 'App\\\\Shop\\\\JourneyStatus\\\\Completed', 'open' => 'App\\\\Shop\\\\JourneyStatus\\\\Open']],\n    ],\n    'modules' => [\n    ],\n];\n"
        ),
        "{}",
        read(root, ".sharp/current/listeners.php")
    );
}

/// Spec section 29: a parameter an `extern` line states keeps the effects its function type lists, so a `Map` a loop
/// fills with closures that have those effects passes to the plain PHP constructor, and the `Map` local keeps the value
/// type it is declared with. A closure with an effect the type leaves out is refused at the call, naming the effect.
#[test]
fn an_extern_parameter_takes_the_functions_whose_effects_its_function_type_lists() {
    let extension = "<?php\n\nnamespace Cel;\n\nfinal readonly class CallableExtension\n{\n    /** @param array<non-empty-string, callable> $functions */\n    public function __construct(\n        private array $functions,\n        private bool $idempotent = false,\n    ) {}\n}\n";
    let stubs = "namespace App.Stubs;\n\nimport Cel.CallableExtension;\n\nextern void CallableExtension.__construct(Map<string, Function<Any?(Any?) uses Database, Files, Clock>> functions) uses none;\n";
    let value = "namespace App.Schema;\n\npublic class CelValue\n{\n    public CelValue(public Any? value { get; }) { }\n\n    public static CelValue of(Any? value) => new CelValue(value);\n}\n";
    let function = "namespace App.Schema;\n\npublic abstract class ExpressionFunction\n{\n    public abstract (string|int|float|bool)? invoke(CelValue ...arguments) uses Database, Files, Clock;\n}\n";
    let runtime = "namespace App.Schema;\n\nimport Cel.CallableExtension;\n\npublic class Runtime\n{\n    public Runtime(private Map<string, ExpressionFunction> functions) { }\n\n    public CallableExtension extension()\n    {\n        Map<string, Function<Any?(Any?) uses Database, Files, Clock>> callables = [:];\n        for (const [name, function] of this.functions) {\n            callables[name] = (Any? ...arguments) => function.invoke(...arguments.map((Any? argument) => CelValue.of(argument)));\n        }\n        return new CallableExtension(callables);\n    }\n}\n";
    let directory = library_project(&[
        ("vendor/acme/cel/CallableExtension.php", extension),
        ("app/Stubs/Cel.sharp", stubs),
        ("app/Schema/CelValue.sharp", value),
        ("app/Schema/ExpressionFunction.sharp", function),
        ("app/Schema/Runtime.sharp", runtime),
    ]);
    let root = directory.path();

    compile_succeeds(root);

    let mailing =
        "namespace App.Schema;\n\npublic abstract class Mailing\n{\n    public abstract string? send() uses Mail;\n}\n";
    let outbox = "namespace App.Schema;\n\nimport Cel.CallableExtension;\n\npublic class Outbox\n{\n    public CallableExtension extension(Mailing mailing) => new CallableExtension([\"send\": (Any? ...arguments) => mailing.send()]);\n}\n";
    write(root, "app/Schema/Mailing.sharp", mailing);
    write(root, "app/Schema/Outbox.sharp", outbox);

    let output = compile_lines(root);

    assert_eq!(
        issue_lines(&output),
        [
            "app/Schema/Outbox.sharp:7:82:error - invalid-argument: This lambda calls `Mailing.send`, which has the effect `Mail`, so a `Function<Any?(Any?) uses Database, Files, Clock>` cannot hold it."
        ],
        "{}",
        printed(&output)
    );
}

/// Spec section 7: a call of a method declared `(string|int|float|bool)?` has that type, which PHP# writes, so the call
/// compiles and an error names its type as written, never as a `scalar` PHP# has no name for.
#[test]
fn a_call_of_a_method_that_gives_string_int_float_or_bool_keeps_the_four_types() {
    let function = "namespace App.Schema;\n\npublic abstract class ExpressionFunction\n{\n    public abstract (string|int|float|bool)? invoke();\n}\n";
    let runner = "namespace App.Schema;\n\npublic class Runner\n{\n    public (string|int|float|bool)? run(ExpressionFunction function) => function.invoke();\n}\n";
    let directory =
        library_project(&[("app/Schema/ExpressionFunction.sharp", function), ("app/Schema/Runner.sharp", runner)]);
    let root = directory.path();

    compile_succeeds(root);

    let counter = "namespace App.Schema;\n\npublic class Counter\n{\n    public int count(ExpressionFunction function) => Counter.twice(function.invoke());\n\n    public static int twice(int value) => value * 2;\n}\n";
    write(root, "app/Schema/Counter.sharp", counter);

    let output = compile_lines(root);

    assert_eq!(
        issue_lines(&output),
        [
            "app/Schema/Counter.sharp:5:68:error - possibly-null-argument: Argument #1 of method `Counter.twice` is possibly `null`, but parameter type `int` does not accept it.",
            "app/Schema/Counter.sharp:5:68:error - possibly-invalid-argument: Possible argument type mismatch for argument #1 of `Counter.twice`: expected `int`, but possibly received `(bool|int|float|string)?`.",
        ],
        "{}",
        printed(&output)
    );
}

/// Spec section 29: an `extern` line of a plain PHP trait's member declares the effects of that one body, so a call of
/// it through a PHP# class that takes it from the trait has them. A class that writes the member itself runs its own
/// body, which the line does not cover.
#[test]
fn an_extern_line_of_a_trait_member_covers_each_class_that_takes_the_member_from_the_trait() {
    let dispatchable = "<?php\n\nnamespace Bus;\n\ntrait Dispatchable\n{\n    public static function dispatch(...$arguments): void\n    {\n    }\n}\n";
    let stubs = "namespace App.Stubs;\n\nimport Bus.Dispatchable;\n\nextern Dispatchable.dispatch uses Database;\n";
    let receive =
        "namespace App.Events;\n\nimport Bus.Dispatchable;\n\npublic class ReceiveEvent : Dispatchable\n{\n}\n";
    let send = "namespace App.Events;\n\nimport Bus.Dispatchable;\n\npublic class SendEvent : Dispatchable\n{\n    public static override void dispatch(Any? ...arguments)\n    {\n    }\n}\n";
    let mail =
        "namespace App.Events;\n\npublic interface Mail\n{\n    void receive() uses Database;\n\n    void send();\n}\n";
    let inbox = "namespace App.Events;\n\npublic class Inbox : Mail\n{\n    public void receive()\n    {\n        ReceiveEvent.dispatch(\"payload\");\n    }\n\n    public void send()\n    {\n        SendEvent.dispatch(\"payload\");\n    }\n}\n";
    let directory = library_project(&[
        ("vendor/acme/bus/Dispatchable.php", dispatchable),
        ("app/Stubs/Bus.sharp", stubs),
        ("app/Events/ReceiveEvent.sharp", receive),
        ("app/Events/SendEvent.sharp", send),
        ("app/Events/Mail.sharp", mail),
        ("app/Events/Inbox.sharp", inbox),
    ]);
    let root = directory.path();

    compile_succeeds(root);

    let outbox = "namespace App.Events;\n\npublic class Outbox : Mail\n{\n    public void receive()\n    {\n    }\n\n    public void send()\n    {\n        ReceiveEvent.dispatch(\"payload\");\n    }\n}\n";
    write(root, "app/Events/Outbox.sharp", outbox);

    let output = compile_lines(root);

    assert_eq!(
        issue_lines(&output),
        [
            "app/Events/Outbox.sharp:11:9:error - incompatible-effects: `Outbox.send` calls `ReceiveEvent.dispatch`, which has the effect `Database`, so the pure `App.Events.Mail.send` cannot accept it."
        ],
        "{}",
        printed(&output)
    );
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
        "{\n    \"type\": \"library\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"App\\\\\": \"it's\\\\here/\"\n        }\n    }\n}\n",
    );

    compile_succeeds(root);
    let input = input_row(root, "it's\\here/*.sharp").replace("it's\\here", "it\\'s\\\\here");
    assert!(
        read(root, ".sharp/current/listeners.php").contains(&input),
        "{}",
        read(root, ".sharp/current/listeners.php")
    );
}

/// The folders in `cache`'s `mago/lean/`: the library builds and any staging folder, without `build.lock`.
fn library_builds(cache: &Path) -> Vec<PathBuf> {
    let entries = std::fs::read_dir(cache.join("mago/lean")).unwrap();
    entries.map(|entry| entry.unwrap().path()).filter(|path| path.is_dir()).collect()
}

/// An earlier `mago` wrote the generated code to `.sharp/.lean/Code/` and built it there with Lake, and wrote the
/// `lakefile.toml` and the manifest that `lake env` gave its runner the search path from. The runner reads the code from
/// its request and starts with the search path the library build records, so the compile deletes them all, and a file
/// in them cannot reach a proof.
#[test]
fn compile_deletes_the_generated_code_and_the_lake_package_an_earlier_mago_left_and_proves_its_law() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let root = directory.path();
    write(root, ".sharp/.lean/Code/App/Shared/Money.lean", "this is not Lean\n");
    write(root, ".sharp/.lean/.lake/build/lib/lean/Code/App/Shared/Money.olean", "this is not a build\n");
    write(root, ".sharp/.lean/lakefile.toml", "this is not a lakefile\n");
    write(root, ".sharp/.lean/lake-manifest.json", "this is not a manifest\n");

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    for left in ["Code", ".lake", "lakefile.toml", "lake-manifest.json"] {
        assert!(!root.join(".sharp/.lean").join(left).exists(), "{left}");
    }
}

#[test]
fn compiles_that_start_together_on_an_empty_cache_each_prove_their_law_and_leave_one_library_build() {
    let cache = tempfile::tempdir().unwrap();
    let projects: Vec<_> = (0..4).map(|_| lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)])).collect();

    let outputs: Vec<Output> = std::thread::scope(|scope| {
        let compiles: Vec<_> = projects
            .iter()
            .map(|project| {
                scope.spawn(|| {
                    process::output(command(project.path(), "never").env("XDG_CACHE_HOME", cache.path())).unwrap()
                })
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
            process::output(command(project.path(), "never").env("XDG_CACHE_HOME", cache.path())).unwrap()
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

    let output = process::output(command(project.path(), "never").env("XDG_CACHE_HOME", cache.path())).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(source_lean_files(project.path()), Vec::<PathBuf>::new());
    assert!(!stale.exists(), "the staging folder a dead build left is deleted");
    assert_eq!(library_builds(cache.path()).len(), 1, "one library build is left");
}

#[test]
fn compiles_that_start_together_on_a_library_build_that_lost_its_lakefile_each_prove_their_law_and_leave_one_build() {
    let cache = tempfile::tempdir().unwrap();
    let compile_in_cache =
        |project: &Path| process::output(command(project, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
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

/// One runner proves the laws of every stale file of a compile, so the compile imports Lean once.
#[cfg(unix)]
#[test]
fn compile_proves_the_laws_of_two_stale_files_with_one_runner() {
    let directory = money_and_wallet();
    let root = directory.path();
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert_eq!(std::fs::read_dir(lake.path().join("started")).unwrap().count(), 1, "one runner started");
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
    assert!(compiled(root, "app/Shared/Wallet.sharp").exists());
}

/// A law runs on one Lean, found one way, and a `lake` on the `PATH` is never it: neither a compile that builds the law
/// checker nor one that finds it built calls the `lake` first on the `PATH`, which fails every call.
#[cfg(unix)]
#[test]
fn a_law_never_runs_on_the_lake_on_the_path() {
    use std::os::unix::fs::PermissionsExt;

    let lake = tempfile::tempdir().unwrap();
    let started = lake.path().join("started");
    write(lake.path(), "lake", &format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 1\n", started.display()));
    std::fs::set_permissions(lake.path().join("lake"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::var_os("PATH").unwrap();
    let path =
        std::env::join_paths(std::iter::once(lake.path().to_path_buf()).chain(std::env::split_paths(&path))).unwrap();
    let cache = tempfile::tempdir().unwrap();

    for run in ["builds the law checker", "finds it built"] {
        let project = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
        let output =
            process::output(command(project.path(), "never").env("XDG_CACHE_HOME", cache.path()).env("PATH", &path))
                .unwrap();

        assert!(output.status.success(), "{run}: {}", printed(&output));
        assert!(compiled(project.path(), "app/Shared/Money.sharp").exists(), "{run}");
    }
    assert_eq!(std::fs::read_to_string(&started).unwrap_or_default(), "", "the calls to the lake on the PATH");
}

/// A cache folder the law checker can't create, as one a root container left, is named with what failed.
#[cfg(unix)]
#[test]
fn compile_names_the_cache_folder_the_law_checker_cannot_create() {
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let cache = unwritable_cache();

    let output = process::output(command(directory.path(), "never").env("XDG_CACHE_HOME", &cache)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "The laws could not be checked: can't create the folder `{}`: Not a directory (os error 20).",
            cache.join("mago/lean").display()
        )),
        "{}",
        printed(&output)
    );
}

/// A file whose law needs proving needs the law checker; with no Lean installed and none Mago can install, each such
/// file is refused with `law-checker-unavailable`, which names the step of the install that failed and its cause, and
/// every other file compiles. Here the install can't create Mago's cache folder, which is a file.
#[test]
fn compile_reports_an_unavailable_law_checker_on_each_file_that_needs_it_and_whose_compiled_file_is_not_current() {
    let cache = unwritable_cache().join("mago");
    let (unavailable, cause) = if cfg!(target_env = "musl") {
        (
            "error - law-checker-unavailable: Laws need the law checker, and this musl build of the checker can't install it, because the law checker has no musl build. On glibc Linux, the checker's glibc build installs it itself.".to_owned(),
            String::new(),
        )
    } else {
        (
            format!(
                "error - law-checker-unavailable: The law checker could not be installed: {}",
                cache.join("lean/lean-4.34.1-").display()
            ),
            format!(": can't create the folder `{}`: File exists (os error 17).", cache.display()),
        )
    };
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY), ("app/Order.sharp", ORDER)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    let issues = issue_lines(&output);
    let [issue] = issues.as_slice() else { panic!("one issue: {}", printed(&output)) };
    assert!(issue.starts_with(&format!("app/Shared/Money.sharp:8:5:{unavailable}")), "{}", printed(&output));
    assert!(issue.ends_with(&cause), "{}", printed(&output));
    assert!(!printed(&output).contains("elan"), "{}", printed(&output));
    assert!(compiled(root, "app/Order.sharp").exists());
    assert!(!root.join(".sharp/inputs.json").exists(), "the next compile tries to install the law checker again");
}

/// A project of `kind`, `"project"` or `"library"`, with `app/` under `autoload` and `tests/` under `autoload-dev`,
/// whose `composer.lock` lists `phpunit/phpunit` under `packages-dev` and `acme/report` under `packages`, both
/// installed in `vendor/` with their plain PHP classes, and `files`.
fn dev_package_project(kind: &str, files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"tests\"]\nincludes = [\"vendor\"]\n",
    );
    write(
        root,
        "composer.json",
        &format!(
            "{{\"type\": \"{kind}\", \"autoload\": {{\"psr-4\": {{\"App\\\\\": \"app/\"}}}}, \"autoload-dev\": {{\"psr-4\": {{\"Tests\\\\\": \"tests/\"}}}}}}\n"
        ),
    );
    write(
        root,
        "composer.lock",
        "{\"packages\": [{\"name\": \"acme/report\"}], \"packages-dev\": [{\"name\": \"phpunit/phpunit\"}]}\n",
    );
    write(
        root,
        "vendor/phpunit/phpunit/src/Framework/TestCase.php",
        "<?php\n\nnamespace PHPUnit\\Framework;\n\nabstract class TestCase\n{\n}\n",
    );
    write(
        root,
        "vendor/phpunit/phpunit/src/Framework/Test.php",
        "<?php\n\nnamespace PHPUnit\\Framework;\n\ninterface Test\n{\n}\n",
    );
    write(
        root,
        "vendor/acme/report/src/Report.php",
        "<?php\n\nnamespace Acme\\Report;\n\nabstract class Report\n{\n}\n",
    );
    for (name, contents) in files {
        write(root, name, contents);
    }

    directory
}

/// A test class outside every `autoload-dev` folder extends PHPUnit's `TestCase`, a class of a dev-only package, one
/// `composer.lock` lists under `packages-dev`: a production install leaves the package out, so the class is refused
/// with where tests go, and no `unused-class` names it too.
#[test]
fn production_code_that_extends_a_class_of_a_dev_only_package_is_refused_with_where_tests_go() {
    let directory = dev_package_project(
        "project",
        &[(
            "app/OptInTest.sharp",
            "namespace App;\n\nimport PHPUnit.Framework.TestCase;\n\ninternal class OptInTest : TestCase\n{\n}\n",
        )],
    );

    let output = compile_lines(directory.path());

    assert_eq!(
        issue_lines(&output),
        [
            "app/OptInTest.sharp:5:28:error - production-uses-dev-package: `OptInTest` extends `TestCase`, from phpunit/phpunit, a dev-only package that a production install leaves out. Tests go in a folder composer.json lists under autoload-dev, such as tests/."
        ],
        "{}",
        printed(&output)
    );
}

/// Production code of a library that implements an interface of a dev-only package, or names it as a type, is
/// refused at each name, the header's saying which class implements it.
#[test]
fn production_code_that_implements_or_names_a_class_of_a_dev_only_package_is_refused_at_each_name() {
    let directory = dev_package_project(
        "library",
        &[(
            "app/Probe.sharp",
            "namespace App;\n\nimport PHPUnit.Framework.Test;\n\ninternal class Probe : Test\n{\n    internal void check(Test other)\n    {\n    }\n}\n",
        )],
    );

    let output = compile_lines(directory.path());

    assert_eq!(
        issue_lines(&output),
        [
            "app/Probe.sharp:5:24:error - production-uses-dev-package: `Probe` implements `Test`, from phpunit/phpunit, a dev-only package that a production install leaves out. Tests go in a folder composer.json lists under autoload-dev, such as tests/.",
            "app/Probe.sharp:7:25:error - production-uses-dev-package: Production code uses `Test`, from phpunit/phpunit, a dev-only package that a production install leaves out. Tests go in a folder composer.json lists under autoload-dev, such as tests/.",
        ],
        "{}",
        printed(&output)
    );
}

/// The same test class in `tests/`, a folder `composer.json` lists under `autoload-dev`, compiles, and so does
/// production code that extends a class of a package `composer.lock` lists under `packages`.
#[test]
fn a_test_in_an_autoload_dev_folder_and_production_code_of_a_required_package_compile() {
    let test = dev_package_project(
        "project",
        &[(
            "tests/OptInTest.sharp",
            "namespace Tests;\n\nimport PHPUnit.Framework.TestCase;\n\npublic class OptInTest : TestCase\n{\n}\n",
        )],
    );
    let required = dev_package_project(
        "library",
        &[("app/Sales.sharp", "namespace App;\n\nimport Acme.Report.Report;\n\npublic class Sales : Report\n{\n}\n")],
    );

    for directory in [test, required] {
        let output = compile_lines(directory.path());

        assert!(output.status.success(), "{}", printed(&output));
    }
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
    assert_eq!(
        files_under(&root.join(".sharp")),
        [
            Path::new("autoload.php"),
            Path::new("compiled.bin"),
            Path::new("current/app/Order.sharpc"),
            Path::new("current/generation.lock"),
            Path::new("current/listeners.php"),
            Path::new("write.lock")
        ]
    );
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
    let listed = store_module("App.Shop.Loyalty, App.Shop.Records, Illuminate.Support");
    let misspelled = store_module("App.Shop.Loyalty, App.Shop.Records, Illuminate.Suport");
    // `App.Shop.Loyalty` imports `StoreService` back, and no list allows or refuses a cycle.
    let points = "namespace App.Shop.Loyalty;\n\nimport App.Tenant.Store.StoreService;\n\npublic class Points\n{\n    public Points(private StoreService store) { }\n}\n";
    let ways_out = "Move the code, or declare an interface in one namespace that the other implements.";
    let service = "namespace App.Tenant.Store;\n\nimport App.Shop.Loyalty.Member;\nimport Illuminate.Support.Facades.DB;\nimport App.Shared.Ledger.Entry;\n\npublic class StoreService\n{\n    public StoreService(private Member member, private DB db, private Entry entry) { }\n}\n";
    let refused = "app/Tenant/Store/StoreService.sharp:5:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Shop.Loyalty`, `App.Shop.Records` and `Illuminate.Support`.";
    for (modules, expected) in [
        (vec![("app/Tenant/Store/Module.sharp", listed.as_str())], vec![refused.to_owned()]),
        (
            vec![("app/Tenant/Store/Module.sharp", misspelled.as_str())],
            vec![
                "app/Tenant/Store/Module.sharp:5:49:error - non-existent-use-import: Imported namespace `Illuminate.Suport` does not exist.".to_owned(),
                "app/Tenant/Store/StoreService.sharp:4:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Shop.Loyalty`, `App.Shop.Records` and `Illuminate.Suport`.".to_owned(),
                "app/Tenant/Store/StoreService.sharp:5:8:error - unlisted-import: `App.Tenant.Store` imports only `App.Shop.Loyalty`, `App.Shop.Records` and `Illuminate.Suport`.".to_owned(),
            ],
        ),
        (
            vec![("app/Shop/Loyalty/Points.sharp", points)],
            vec![
                format!("app/Shop/Loyalty/Points.sharp:3:8:error - import-cycle: `App.Shop.Loyalty` and `App.Tenant.Store` import each other (StoreService.sharp imports Member). {ways_out}"),
                format!("app/Tenant/Store/StoreService.sharp:3:8:error - import-cycle: `App.Tenant.Store` and `App.Shop.Loyalty` import each other (Points.sharp imports StoreService). {ways_out}"),
            ],
        ),
    ] {
        let mut files = vec![
            ("app/Shop/Loyalty/Member.sharp", "namespace App.Shop.Loyalty;\n\npublic class Member\n{\n}\n"),
            ("app/Shop/Records/Table.sharp", "namespace App.Shop.Records;\n\npublic class Table\n{\n}\n"),
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

/// `mago compile` with each issue on one line, as `compile_lines` runs it, with the cache folder `cache`, under a
/// processor-time limit of `seconds`, which each Lean process it starts inherits, so a row reaches the limit long before
/// 120 seconds and the deadline long before 480 seconds, and the processor seconds it and every process it waited for
/// used. The limit is soft, as `ulimit -S -t` sets it. The runner runs under it as Mago does, and Mago's own limit on
/// each `lake` process is `library.rs`'s `each_process_a_lake_starts_runs_under_the_processor_time_limit`'s to prove.
#[cfg(unix)]
// `wait4` reaps the compile, which clippy can't see.
#[allow(clippy::zombie_processes)]
fn compile_lines_within(root: &Path, seconds: u64, cache: &Path) -> (Output, f64) {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;
    use std::process::Stdio;

    let mut command = command(root, "never");
    command
        .env("MAGO_REPORTING_FORMAT", "emacs")
        .env("XDG_CACHE_HOME", cache)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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

    let mut child = process::spawn(&mut command).unwrap();
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

/// A cache folder in `folder` whose runner reads the request, reports that it elaborates the generated module of
/// `app/Shared/Money.sharp`, as the runner does, leaves `stalled` in `folder` and runs the shell commands `work`, which
/// stand in for Lean work in that module that never ends. The runner's path names each process it runs `work` in.
#[cfg(unix)]
fn stalling_cache(folder: &Path, work: &str) -> PathBuf {
    cache_with_runner(
        folder,
        &format!(
            "cat > /dev/null\necho 'module 0 Code.App.Shared.Money' >&2\ntouch '{}'\n{work}",
            folder.join("stalled").display()
        ),
    )
}

/// Lean work that runs past the processor-time limit, or waits until the deadline, stops there once instead of holding
/// the compile, and refuses the law whose code it was checking with what stopped it. Each row runs after a compile that
/// built the runtime library. Work that never ends runs under a limit of 2 seconds, and costs the compile less than two
/// limits, which leaves room for the rest of the compile on a slow machine. It spins, and an alarm on the clock makes
/// it write a line each second, as the runner reports each module and check it starts, so the deadline, which counts
/// from the last thing written or the last processor time used, never stops it, however little of a core a busy
/// machine gives it. It spins with no
/// system call, since macOS lets a process that spends its time in system calls run past its limit. It ends with no
/// answer once it has used 10 processor seconds, so a limit that no longer stops it fails the row. Work that waits runs
/// under a limit of 2 seconds, so its deadline is 8 seconds, and it ends with no answer after 60 seconds, so a deadline
/// that no longer stops it fails the row. It uses almost no processor time, so it runs on its own thread meanwhile.
/// Each limit holds the whole compile too, so it stays above the processor seconds the compile itself uses.
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

        let (output, seconds) = compile_lines_within(directory.path(), limit, &stalling_cache(lake.path(), work));

        assert!(lake.path().join("stalled").exists(), "{work}: {}", printed(&output));
        assert_eq!(issue_lines(&output), expected, "{work}, after {seconds} processor seconds: {}", printed(&output));
        assert!(seconds < 2.0 * limit as f64, "{work} used {seconds} processor seconds: {}", printed(&output));
        assert!(!compiled(directory.path(), "app/Shared/Money.sharp").exists(), "{work}");
    };

    std::thread::scope(|scope| {
        let waits = scope.spawn(|| stops(2, "sleep 60", &refused("it did not finish within 8 seconds")));
        stops(
            2,
            "exec perl -e '$| = 1; $SIG{ALRM} = sub { exit if (times)[0] + (times)[1] >= 10; print time, qq(\\n); alarm 1 }; alarm 1; 1 while 1'",
            &refused("it ran past 2 processor seconds, the processor-time limit Mago runs under"),
        );
        waits.join().unwrap();
    });
}

/// Spec section 28: heartbeats alone bound a proof, so a runner that computes past its deadline without writing, as a
/// runner on a busy machine does, is not stopped there, and its law proves. The runner replays the answer the tests'
/// runner gave the same request, after it computes for 12 seconds without writing, with a share of a core small enough
/// to stay under a limit of 2 processor seconds, whose deadline is 8 seconds.
#[cfg(unix)]
#[test]
fn a_runner_that_computes_past_its_deadline_without_writing_finishes() {
    let lake = tempfile::tempdir().unwrap();
    let answer = lake.path().join("answer");
    let recording = cache_with_runner(lake.path(), &format!("\"$real\" \"$@\" | tee '{}'", answer.display()));
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let recorded = process::output(command(directory.path(), "never").env("XDG_CACHE_HOME", recording)).unwrap();
    assert!(recorded.status.success(), "{}", printed(&recorded));
    let busy = tempfile::tempdir().unwrap();
    let cache = cache_with_runner(
        busy.path(),
        &format!(
            "cat > /dev/null\nperl -MTime::HiRes=time,sleep -e 'my $end = time + 12; while (time < $end) {{ my $spin = time + 0.01; 1 while time < $spin; sleep 0.29 }}'\ncat '{}'",
            answer.display()
        ),
    );
    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);

    let (output, _) = compile_lines_within(directory.path(), 2, &cache);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(directory.path(), "app/Shared/Money.sharp").exists());
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

    for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
        let lake = tempfile::tempdir().unwrap();
        let cache = stalling_cache(lake.path(), "while :; do sleep 1; done");
        let script = lake.path().join("cache/mago/lean").display().to_string();
        let leans =
            || String::from_utf8(process::output(Command::new("pgrep").args(["-f", &script])).unwrap().stdout).unwrap();
        let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
        let mut compiling = command(directory.path(), "never");
        compiling.env("XDG_CACHE_HOME", &cache).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0);
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
        let mut compile = process::spawn(&mut compiling).unwrap();
        assert!(
            poll_until(Duration::from_secs(300), || lake.path().join("stalled").exists()),
            "{signal}: the stalled Lean never started"
        );

        // SAFETY: `kill` only sends a signal, to the group the compile leads, as a terminal does.
        assert_eq!(unsafe { libc::kill(-i32::try_from(compile.id()).unwrap(), signal) }, 0);

        assert_eq!(compile.wait().unwrap().signal(), Some(signal));
        poll_until(Duration::from_secs(10), || leans().is_empty());
        let left = leans();
        if !left.is_empty() {
            process::status(Command::new("pkill").args(["-f", &script])).unwrap();
        }
        assert_eq!(left, "", "{signal}: Lean outlived the compile");
    }
}

/// Spec section 23: a call only `__call` or `__callStatic` answers, instance or static, is a missing method in a library
/// and in a project alike, where a release compiled it.
#[test]
fn a_call_only_a_magic_method_answers_is_refused_in_a_library_and_a_project() {
    let bag = "<?php\n\nnamespace Lib;\n\nfinal class Bag\n{\n    /** @param list<mixed> $arguments */\n    public function __call(string $name, array $arguments): mixed\n    {\n        return null;\n    }\n\n    /** @param list<mixed> $arguments */\n    public static function __callStatic(string $name, array $arguments): mixed\n    {\n        return null;\n    }\n}\n";
    let uses = "namespace App;\n\nimport Lib.Bag;\n\nextern Bag uses none;\n\npublic class Uses\n{\n    public static void magic(Bag b)\n    {\n        b.anything(1);\n        Bag.anything(1);\n    }\n}\n";
    for kind in ["library", "project"] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        write(root, "mago.toml", "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"src\"]\n");
        write(
            root,
            "composer.json",
            &format!(
                "{{\"type\": \"{kind}\", \"autoload\": {{\"psr-4\": {{\"App\\\\\": \"app/\", \"Lib\\\\\": \"src/Lib/\"}}}}}}\n"
            ),
        );
        write(root, "composer.lock", "{}\n");
        write(root, "src/Lib/Bag.php", bag);
        write(root, "app/Uses.sharp", uses);

        let output = compile(root);

        assert_eq!(output.status.code(), Some(1), "{kind}: {}", printed(&output));
        assert_eq!(
            printed(&output).matches("Method `Bag.anything` does not exist.").count(),
            2,
            "{kind}: {}",
            printed(&output)
        );
    }
}

/// Spec section 9: a named constructor takes any name but a magic method's, which the engine calls itself. One named
/// `__construct` is refused at its name, and the lowering, which runs on a refused file too, lowers it without a panic.
#[test]
fn a_named_constructor_named_construct_is_refused_and_lowers_without_a_panic() {
    let maker = "namespace App.Shop;\n\npublic class Maker\n{\n    public Maker(public int sides { get; }) { }\n\n    public Maker.__construct(int sides) : this(sides) { }\n}\n";
    let directory = library_project(&[("app/Shop/Maker.sharp", maker)]);

    let output = compile_lines(directory.path());

    assert_eq!(
        issue_lines(&output),
        [
            "app/Shop/Maker.sharp:7:18:error - semantics: A named constructor cannot be named `__construct`: the engine calls `__construct` itself."
        ],
        "{}",
        printed(&output)
    );
}

/// An `Instant` that crosses to a plain PHP date, as a plain PHP library takes one, written as PHP's `date("c")` writes it.
fn stamp_of_an_instant(imports: &str) -> String {
    format!(
        "namespace App;\n\n{imports}import Sharp.Time.Instant;\n\npublic class Stamp\n{{\n    public static string of(Instant at) => at.toPhpDateTime().format(\"c\");\n}}\n"
    )
}

/// `toPhpDateTime()` names PHP, so it lives in `Sharp.Interop`, which a program imports only to work with plain PHP: an
/// `Instant` has it after `import Sharp.Interop.PhpDates;`, and without the import the call is the error any member the
/// type lacks gets.
#[test]
fn an_instant_crosses_to_a_plain_php_date_only_after_the_interop_import() {
    let without = library_project(&[("app/Stamp.sharp", &stamp_of_an_instant(""))]);
    let with = library_project(&[("app/Stamp.sharp", &stamp_of_an_instant("import Sharp.Interop.PhpDates;\n"))]);

    let refused = compile_lines(without.path());
    let compiled = compile_lines(with.path());

    assert_eq!(
        issue_lines(&refused),
        [
            "app/Stamp.sharp:7:47:error - non-existent-method: `toPhpDateTime` is an extension this file does not import. Add `import Sharp.Interop.PhpDates;` to the file."
        ],
        "{}",
        printed(&refused)
    );
    assert!(compiled.status.success(), "{}", printed(&compiled));
}

/// A shop's stock, which reserves a unit and announces it in one transaction, with the announcing step `step`.
fn stock_announcing(step: &str) -> String {
    format!(
        "namespace App.Shop;\n\nimport Sharp.Data.Database;\nimport Sharp.Net.Mail;\nimport Sharp.Net.MailMessage;\n\npublic class Stock\n{{\n    public Stock(private Database database, private Mail mail) {{ }}\n\n    public int reserve(string sku, MailMessage message)\n        => this.database.transaction(() => {{\n            {step}\n            return this.database.execute(\"UPDATE stock SET count = count - 1 WHERE sku = ?\", [sku]);\n        }});\n}}\n"
    )
}

/// Spec section 29: `Database.transaction`'s body may reach the database and emit, whose listeners run once the
/// transaction commits, and nothing else, so mail sent inside it can't go out for a transaction that rolls back.
#[test]
fn a_transaction_s_body_emits_and_reaches_the_database_and_sending_mail_in_it_is_refused() {
    let reserved = ("app/Shop/Reserved.sharp", "namespace App.Shop;\n\npublic event Reserved(string sku);\n");
    let emitting = library_project(&[reserved, ("app/Shop/Stock.sharp", &stock_announcing("emit new Reserved(sku);"))]);
    let mailing = library_project(&[reserved, ("app/Shop/Stock.sharp", &stock_announcing("this.mail.send(message);"))]);

    let compiled = compile_lines(emitting.path());
    let refused = compile_lines(mailing.path());

    assert!(compiled.status.success(), "{}", printed(&compiled));
    assert_eq!(
        issue_lines(&refused),
        [
            "app/Shop/Stock.sharp:12:38:error - invalid-argument: This lambda calls `Mail.send`, which has the effect `Mail`, so a `Function<int() uses Database, Events>` cannot hold it."
        ],
        "{}",
        printed(&refused)
    );
}

/// `app/Cart.sharp`, which calls `Order.total`, so a signature edit of `Order` reaches it.
const CART: &str = "namespace App;\n\npublic class Cart\n{\n    public int sum() => new Order().total(1);\n}\n";

/// The key of each compiled file under the `.sharp` folder of `root`, by its path there. A compiled file's inputs hold
/// their modification times, so its key, not its bytes, tells the code it holds.
fn compiled_keys(root: &Path) -> Vec<(PathBuf, [u8; 16])> {
    let folder = root.join(".sharp");
    files_under(&folder)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "sharpc"))
        .map(|path| {
            let key = header(&std::fs::read(folder.join(&path)).unwrap()).unwrap().key;
            (path, key)
        })
        .collect()
}

/// A compile of the project with `app/Cart.sharp`, then `between` and `edit`, then a compile again, which restores the
/// analysis the first compile left, prints what a full compile of a fresh copy with `edit` prints, and writes compiled
/// files of the same keys.
fn assert_a_compile_after_an_edit_matches_a_full_compile(between: impl Fn(&Path), edit: impl Fn(&Path)) {
    let seeded = project(ORDER, MONEY);
    write(seeded.path(), "app/Cart.sharp", CART);
    compile_succeeds(seeded.path());
    assert!(seeded.path().join(".sharp/analysis.bin").is_file());
    between(seeded.path());
    edit(seeded.path());

    let after = compile_lines(seeded.path());

    let fresh = project(ORDER, MONEY);
    write(fresh.path(), "app/Cart.sharp", CART);
    edit(fresh.path());
    let full = compile_lines(fresh.path());
    assert_eq!(after.status.code(), full.status.code(), "{}", printed(&after));
    assert_eq!(issue_lines(&after), issue_lines(&full), "{}", printed(&after));
    assert_eq!(compiled_keys(seeded.path()), compiled_keys(fresh.path()));
}

/// After a body edit, a compile restores the analysis the last compile left and gives what a full compile gives.
#[test]
fn a_compile_after_a_body_edit_gives_what_a_full_compile_gives() {
    assert_a_compile_after_an_edit_matches_a_full_compile(
        |_| {},
        |root| write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2")),
    );
}

/// A compile with an extension host keeps the analysis the host took over, and after a body edit restores it and
/// reports what a full compile with the host reports, in a project whose package holds a `.sharp` file with an error.
#[test]
fn a_compile_with_an_extension_host_after_a_body_edit_gives_what_a_full_compile_gives() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the compile with a host after an edit") {
        return;
    }
    let hosted = |root: &Path| {
        write(root, "app/Cart.sharp", CART);
        write(root, MONEY_PATH, &MONEY.replace("return 100;", "return \"100\";"));
        write(root, "tools/host.php", HOST_READING_THE_COMPILED_FILE);
        write(
            root,
            "mago.toml",
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nfile = \"tools/host.php\"\n",
        );
    };
    let edit = |root: &Path| write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2"));
    let seeded = project(ORDER, MONEY);
    hosted(seeded.path());
    let first = compile_lines(seeded.path());
    assert!(issue_lines(&first).iter().any(|line| line.contains("Money")), "{}", printed(&first));
    assert!(seeded.path().join(".sharp/analysis.bin").is_file(), "the compile with a host kept no analysis");
    edit(seeded.path());

    let after = compile_lines(seeded.path());

    let fresh = project(ORDER, MONEY);
    hosted(fresh.path());
    edit(fresh.path());
    let full = compile_lines(fresh.path());
    assert_eq!(after.status.code(), full.status.code(), "{}", printed(&after));
    assert_eq!(issue_lines(&after), issue_lines(&full), "{}", printed(&after));
    assert_eq!(compiled_keys(seeded.path()), compiled_keys(fresh.path()));
}

/// After a signature edit that breaks a caller in another file, a compile restores the last analysis, checks the
/// caller again, and refuses it as a full compile does.
#[test]
fn a_compile_after_a_signature_edit_checks_each_caller_again_as_a_full_compile_does() {
    assert_a_compile_after_an_edit_matches_a_full_compile(
        |_| {},
        |root| write(root, "app/Order.sharp", &ORDER.replace("int total(int extra)", "int total(string extra)")),
    );
}

/// After a file is added, and after one is deleted, a compile gives what a full compile gives.
#[test]
fn a_compile_after_a_file_is_added_or_deleted_gives_what_a_full_compile_gives() {
    assert_a_compile_after_an_edit_matches_a_full_compile(
        |_| {},
        |root| {
            write(root, "app/Extra.sharp", "namespace App;\n\npublic class Extra\n{\n    public int one() => 1;\n}\n")
        },
    );
    assert_a_compile_after_an_edit_matches_a_full_compile(
        |_| {},
        |root| std::fs::remove_file(root.join("app/Cart.sharp")).unwrap(),
    );
}

/// An analysis the compile can't read, here one damaged after the compile that wrote it, leaves the next compile to
/// analyze from scratch, which gives what a full compile gives.
#[test]
fn a_compile_after_a_damaged_analysis_analyzes_from_scratch() {
    assert_a_compile_after_an_edit_matches_a_full_compile(
        |root| {
            let path = root.join(".sharp/analysis.bin");
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        },
        |root| write(root, "app/Order.sharp", &ORDER.replace("extra + 1", "extra + 2")),
    );
}

/// The compiled file the engine reads for the source `name` of the project at `root`: the one under the current
/// generation of the `.sharp` folder of the nearest folder above the source that holds one.
fn compiled_as_the_engine_finds_it(root: &Path, name: &str) -> PathBuf {
    let source = root.join(name);
    let folder = source.ancestors().skip(1).find(|folder| folder.join(".sharp").is_dir()).unwrap();

    folder.join(".sharp/current").join(source.strip_prefix(folder).unwrap()).with_extension("sharpc")
}

/// The standard library resolves its names when PHP# releases it, so a plain PHP class a package declares in `Sharp`
/// never changes what the library's code names: `Sharp.Int` throws PHP's `ValueError`, not the package's.
#[test]
fn a_plain_php_sharp_value_error_leaves_the_standard_library_throwing_php_s_value_error() {
    let directory = library_project(&[("app/Order.sharp", ORDER)]);
    let root = directory.path();
    write(
        root,
        "vendor/acme/shadow/ValueError.php",
        "<?php\n\nnamespace Sharp;\n\nclass ValueError extends \\ValueError\n{\n}\n",
    );

    compile_succeeds(root);

    let int = std::fs::read(compiled_as_the_engine_finds_it(
        root,
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Int.sharp",
    ))
    .unwrap();
    let names = |name: &[u8]| int.windows(name.len()).any(|window| window == name);
    assert!(names(b"ValueError"), "Sharp.Int names no ValueError");
    assert!(!names(b"Sharp\\ValueError"), "Sharp.Int throws the package's Sharp\\ValueError");
}

/// A compiled file of the standard library names the release that compiled it, and every `mago` of another release
/// refuses it, since the library's code is that release's code.
#[test]
fn a_standard_library_compiled_file_of_another_release_is_refused() {
    let directory = library_project(&[("app/Order.sharp", ORDER)]);
    let root = directory.path();
    let int = root.join("vendor/heyjordanparker/php-sharp-composer/.sharp/current/library/Sharp/Int.sharpc");
    let mut bytes = std::fs::read(&int).unwrap();
    bytes[24..40].copy_from_slice(&release_checker("0.0.1"));
    std::fs::write(&int, bytes).unwrap();

    let output = compile(root);

    assert!(!output.status.success(), "{}", printed(&output));
    let version = env!("CARGO_PKG_VERSION");
    assert!(
        printed(&output).contains(&format!(
            "{} was compiled by another release of PHP#, and mago is {version}. Reinstall heyjordanparker/php-sharp-composer {version}.",
            int.canonicalize().unwrap().display()
        )),
        "{}",
        printed(&output)
    );
}
