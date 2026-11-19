//! Runs `mago compile` on a project that mixes PHP# and PHP files, with a PHP# package in `vendor/`.

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::process::Stdio;

use mago_database::process;
use mago_sharp_bridge::unit::Library;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::inputs_changed;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_bridge::unit::stamp;
use mago_sharp_lean::PACKAGE_FOLDER;

use crate::common;

const ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return extra + 1;\n    }\n}\n";
const BROKEN_ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return \"one\";\n    }\n}\n";
const MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return 100;\n    }\n}\n";
const BROKEN_MONEY: &str =
    "namespace Acme;\n\npublic class Money\n{\n    public int cents()\n    {\n        return \"one\";\n    }\n}\n";
const LEGACY: &str = "<?php\n\nnamespace App;\n\nfinal class Legacy {}\n";
const MONEY_PATH: &str = "vendor/acme/money/src/Money.sharp";
const LIBRARY: &str = "{\"type\": \"library\"}\n";

/// A library with `app/Order.sharp`, `app/Legacy.php` and the package file `vendor/acme/money/src/Money.sharp`. It is
/// a library so that spec section 5's strict rules, which run on a project, stay out of the tests about something else.
fn project(order: &str, money: &str) -> tempfile::TempDir {
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
    process::output(&mut command(root, "never")).unwrap()
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
        [
            Path::new("app/Order.sharpc"),
            Path::new("compiled.bin"),
            Path::new("inputs.json"),
            Path::new("listeners.php"),
            Path::new("preload.php"),
            Path::new("vendor/acme/money/src/Money.sharpc")
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

/// Spec section 27: `.sharp/preload.php`, which `opcache.preload` names, requires the workspace's Composer autoloader
/// and then each accepted source that declares a type, the package's too, in path order. A refused source, one that
/// declares no type, as a module's file, and a test in an `autoload-dev` folder stay out, so production never preloads
/// a test.
#[test]
fn compile_writes_a_preload_script_that_requires_the_autoloader_and_every_accepted_type() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Broken.sharp", &BROKEN_ORDER.replace("class Order", "class Broken"));
    write(root, "app/Module.sharp", "module\n{\n}\n");
    write(
        root,
        "composer.json",
        "{\"type\": \"library\", \"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}, \"autoload-dev\": {\"psr-4\": {\"Tests\\\\\": \"tests/\"}}}\n",
    );
    write(root, "tests/Shop/CheckoutTest.sharp", "namespace Tests.Shop;\n\npublic class CheckoutTest\n{\n}\n");
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\", \"tests\"]\nincludes = [\"vendor\"]\n",
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(compiled(root, "tests/Shop/CheckoutTest.sharp").exists());
    assert_eq!(
        read(root, ".sharp/preload.php"),
        "<?php\n\n// mago compile writes this file. It rewrites it on every compile.\n\nrequire_once __DIR__ . '/../vendor/autoload.php';\nrequire_once __DIR__ . '/../app/Order.sharp';\nrequire_once __DIR__ . '/../vendor/acme/money/src/Money.sharp';\n"
    );
}

/// Spec section 27: PHP declares an anonymous class only when its code runs, so `.sharp/preload.php` loads each type
/// an anonymous class names, PHP's own left out, in a file the preload loads: a file Composer's autoloader loads up
/// front, as `illuminate/reflection`'s `helpers.php`, and the file of a type a preloaded type extends. Opcache would
/// warn that it can't preload the class otherwise. A file the preload never loads keeps what its anonymous class names
/// unloaded.
#[test]
fn compile_writes_a_preload_script_that_loads_what_the_anonymous_classes_it_loads_name() {
    let directory = project("namespace App;\n\nimport Acme.Model.Base;\n\npublic class Order : Base\n{\n}\n", MONEY);
    let root = directory.path();
    write(
        root,
        "vendor/composer/autoload_psr4.php",
        "<?php\n\n$vendorDir = dirname(__DIR__);\n\nreturn array(\n    'Acme\\\\Model\\\\' => array($vendorDir . '/acme/model/src'),\n);\n",
    );
    write(
        root,
        "vendor/composer/autoload_files.php",
        "<?php\n\n$vendorDir = dirname(__DIR__);\n\nreturn array(\n    'a1' => $vendorDir . '/acme/model/helpers.php',\n);\n",
    );
    write(
        root,
        "vendor/acme/model/helpers.php",
        "<?php\n\nfunction lazy(): object\n{\n    return new class implements Stringable\n    {\n        use Acme\\Model\\Reflects;\n\n        public function __toString(): string\n        {\n            return '';\n        }\n    };\n}\n",
    );
    write(root, "vendor/acme/model/src/Reflects.php", "<?php\n\nnamespace Acme\\Model;\n\ntrait Reflects\n{\n}\n");
    write(
        root,
        "vendor/acme/model/src/Base.php",
        "<?php\n\nnamespace Acme\\Model;\n\nclass Base\n{\n    public function contract(): Contract\n    {\n        return new class implements Contract {};\n    }\n\n    public function unused(): Unused\n    {\n        return new Unused();\n    }\n}\n",
    );
    write(root, "vendor/acme/model/src/Contract.php", "<?php\n\nnamespace Acme\\Model;\n\ninterface Contract\n{\n}\n");
    write(
        root,
        "vendor/acme/model/src/Unused.php",
        "<?php\n\nnamespace Acme\\Model;\n\nclass Unused\n{\n    public function shown(): Shown\n    {\n        return new class extends Shown {};\n    }\n}\n",
    );
    write(root, "vendor/acme/model/src/Shown.php", "<?php\n\nnamespace Acme\\Model;\n\nclass Shown\n{\n}\n");

    compile_succeeds(root);

    assert_eq!(
        read(root, ".sharp/preload.php"),
        "<?php\n\n// mago compile writes this file. It rewrites it on every compile.\n\nrequire_once __DIR__ . '/../vendor/autoload.php';\nrequire_once __DIR__ . '/../app/Order.sharp';\nrequire_once __DIR__ . '/../vendor/acme/money/src/Money.sharp';\ninterface_exists('Acme\\\\Model\\\\Contract');\ntrait_exists('Acme\\\\Model\\\\Reflects');\n"
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
    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();

    assert!(analyzed.status.success(), "{}", printed(&analyzed));
    assert_eq!(std::fs::read_to_string(root.join("host.log")).unwrap(), "first\nsecond\n");
    assert_eq!(std::fs::read(compiled(root, "app/Word.sharp")).unwrap(), compiled_by_compile);
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
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

require $argv[1];

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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
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

require $argv[1];

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
            &format!(
                "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.deleter]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
                repository.join("vendor/autoload.php").display().to_string()
            ),
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.reader]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );
    for file in 0..64 {
        write(root, &format!("vendor/acme/torn/src/Torn{file}.php"), "<?php\n\nfunction torn( {\n");
    }
    let cache = tempfile::tempdir().unwrap();
    let compile = || process::output(command(root, "never").env("XDG_CACHE_HOME", cache.path())).unwrap();
    // The first run's own compile writes `app/Order.php` after the hosts' compile read the project, so the second run's
    // hosts' compile reads it too.
    for _ in 0..2 {
        let output = compile();
        assert!(output.status.success(), "{}", printed(&output));
        assert!(printed(&output).contains("parsing error(s) in 'vendor/acme/torn/src/Torn"), "{}", printed(&output));
    }
    let hosts = cache.path().join("mago/hosts");
    let folder = std::fs::read_dir(&hosts).unwrap().next().expect("the hosts' compile wrote its folder").unwrap();
    let record = folder.path().join("inputs.json");
    assert!(record.exists(), "the hosts' compile kept its record");
    let recorded = std::fs::metadata(&record).unwrap().modified().unwrap();

    let third = compile();

    assert!(third.status.success(), "{}", printed(&third));
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

/// An extension host that loads `App\Word`, the class of `app/Word.sharp`, and appends its process id and the word the
/// compiled file holds to `host.log` as each analysis starts. The `php` the tests run is plain PHP, which never declares
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
        class_exists('App\Word');
        $compiled = (string) @file_get_contents(getenv('SHARP_COMPILED') . '/app/Word.sharpc');
        $word = str_contains($compiled, 'second') ? 'second' : (str_contains($compiled, 'first') ? 'first' : 'nothing');
        file_put_contents('host.log', getmypid() . " {$word}\n", FILE_APPEND);
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

    // The watch's own log, outside the watched project, so a failure shows what the watch saw.
    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let mut watch = process::spawn(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info,mago=debug,mago_database=debug")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .stderr(watch_log.reopen().unwrap()),
    )
    .unwrap();
    wait_for_lines(1);
    write(root, "app/Word.sharp", &word("second"));
    wait_for_lines(2);
    watch.kill().unwrap();
    watch.wait().unwrap();

    let hosts = log();
    let runs: Vec<(&str, &str)> = hosts.lines().filter_map(|line| line.split_once(' ')).collect();
    let failure = format!("{hosts}\n{}", std::fs::read_to_string(watch_log.path()).unwrap());
    assert!(runs.len() >= 2, "{failure}");
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

require $argv[1];

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
        file_put_contents('host.log', getmypid() . " {$context->analysis->file}\n", FILE_APPEND);
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.rule]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let seen = || std::fs::read_to_string(watch_log.path()).unwrap();
    let within = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        done()
    };

    let mut watch = process::spawn(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(watch_log.reopen().unwrap()),
    )
    .unwrap();
    let started = within(&|| seen().contains("Initial analysis complete"));
    let first = log();
    write(root, "app/Word.sharp", &word("second"));
    let rechecked = within(&|| seen().contains("Analysis complete."));
    watch.kill().unwrap();
    watch.wait().unwrap();

    let failure = format!("{}\n{}", log(), seen());
    assert!(started && rechecked, "{failure}");
    assert!(!seen().contains("Restarting analysis"), "{failure}");
    let after = log();
    let recheck: Vec<&str> = after.strip_prefix(first.as_str()).unwrap_or_default().lines().collect();
    let process = first.lines().next().and_then(|line| line.split_once(' ')).map(|(process, _)| process);
    assert!(process.is_some(), "{failure}");
    let edited = format!("{} app/Word.sharp", process.unwrap_or_default());
    assert_eq!(recheck, [edited.as_str()], "the same worker checks the edited file alone: {failure}");
}

/// A watch reports what the one-shot command reports where an issue points into a vendor file Composer's autoload maps
/// served, and keeps watching: the report renders each file the analysis read, and the watch's own tree holds only
/// the files it watches. 0.6.26 stopped the watch after its first analysis with "Files error: file missing".
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
    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let mut watch =
        process::spawn(analyze(true).stdout(reports.reopen().unwrap()).stderr(watch_log.reopen().unwrap())).unwrap();
    let seen = || std::fs::read_to_string(watch_log.path()).unwrap();
    let within = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        done()
    };
    let started = within(&|| seen().contains("Initial analysis complete") || seen().contains("ERROR"));
    let first = std::fs::read_to_string(reports.path()).unwrap();
    write(root, "app/Order.php", &order(6));
    let rechecked = within(&|| seen().contains("Analysis complete.") || seen().contains("ERROR"));
    let running = watch.try_wait().unwrap().is_none();
    watch.kill().unwrap();
    watch.wait().unwrap();

    let failure = format!("{}{}", std::fs::read_to_string(reports.path()).unwrap(), seen());
    assert!(started && !seen().contains("ERROR"), "{failure}");
    assert_eq!(first, report, "the watch's first report is the one-shot command's: {failure}");
    assert!(rechecked && running, "{failure}");
    let second = std::fs::read_to_string(reports.path()).unwrap().split_off(first.len());
    assert!(second.contains("Rate::of(6)") && second.contains("vendor/acme/rate/src/Rate.php"), "{failure}");
}

/// `mago analyze --watch` keeps reporting the issues that point into a vendor file Composer's autoload maps served, round
/// after round, while files are deleted, created, renamed and edited, as agents change a tree. Deleting a class's file
/// analyzes the project from scratch inside the round, and 0.6.27's watch then lost the served files and stopped with
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
    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let mut watch =
        process::spawn(analyze(true).stdout(reports.reopen().unwrap()).stderr(watch_log.reopen().unwrap())).unwrap();
    let seen = || std::fs::read_to_string(watch_log.path()).unwrap();
    let reported = || std::fs::read_to_string(reports.path()).unwrap();
    let within = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        done()
    };
    let started = within(&|| seen().contains("Initial analysis complete") || seen().contains("ERROR"));
    let mut rounds: Vec<(&str, bool, String, String)> = Vec::new();
    let changes: [(&str, &dyn Fn()); 4] = [
        ("delete app/Extra.php", &|| std::fs::remove_file(root.join("app/Extra.php")).unwrap()),
        ("create app/Added.php", &|| write(root, "app/Added.php", &calling("added", 7))),
        ("rename app/Added.php", &|| std::fs::rename(root.join("app/Added.php"), root.join("app/Moved.php")).unwrap()),
        ("edit app/Order.php", &|| write(root, "app/Order.php", &calling("total", 6))),
    ];
    let completed = || seen().matches("Analysis complete.").count();
    for (change, apply) in changes {
        let (before, rounds_before) = (reported().len(), completed());
        apply();
        let done = within(&|| completed() > rounds_before || seen().contains("ERROR"));
        // A change can reach the watch in two batches, as a rename's delete and create, each a round of its own.
        let mut settled = completed();
        loop {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let now = completed();
            if now == settled {
                break;
            }
            settled = now;
        }
        let running = watch.try_wait().unwrap().is_none();
        rounds.push((change, done && running && !seen().contains("ERROR"), reported().split_off(before), once()));
    }
    watch.kill().unwrap();
    watch.wait().unwrap();

    let failure = format!("{}{}", reported(), seen());
    assert!(started, "{failure}");
    for (change, kept_watching, report, cold) in rounds {
        assert!(kept_watching, "the watch kept watching after `{change}`: {failure}");
        assert!(report.contains("vendor/acme/rate/src/Rate.php"), "after `{change}`: {failure}");
        assert!(
            report.ends_with(&cold),
            "after `{change}` the watch reports what the one-shot command reports: {failure}"
        );
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    // The watch's reports and log, outside the watched project, so a failure shows what the watch saw.
    let reports = tempfile::NamedTempFile::new().unwrap();
    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let mut watch = process::spawn(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info")
            .env("MAGO_REPORTING_FORMAT", "medium")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root)
            .stdout(reports.reopen().unwrap())
            .stderr(watch_log.reopen().unwrap()),
    )
    .unwrap();
    let seen = || {
        format!(
            "{}{}",
            std::fs::read_to_string(reports.path()).unwrap(),
            std::fs::read_to_string(watch_log.path()).unwrap()
        )
    };
    let analyses = || seen().matches("nalysis complete").count();
    let within = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        while !done() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        done()
    };
    let missing = "Cannot find type `App.Price`";

    let started = within(&|| analyses() >= 1);
    std::fs::remove_file(root.join("app/Price.sharp")).unwrap();
    let reported = within(&|| seen().contains(missing));
    let running = watch.try_wait().unwrap().is_none();
    let before = analyses();
    write(root, "app/Price.sharp", price);
    let analyzed_again = within(&|| analyses() > before);
    watch.kill().unwrap();
    watch.wait().unwrap();

    let failure = seen();
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.logger]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );
    let cache = tempfile::tempdir().unwrap();
    let log = || std::fs::read_to_string(root.join("host.log")).unwrap_or_default();
    let within = |done: &dyn Fn() -> bool| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            if done() {
                return true;
            }
            if std::time::Instant::now() > deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };

    let watch_log = tempfile::NamedTempFile::new().unwrap();
    let mut watch = process::spawn(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze", "--watch"])
            .env("MAGO_LOG", "info,mago=debug,mago_database=debug")
            .env("XDG_CACHE_HOME", cache.path())
            .current_dir(root)
            .stdout(Stdio::null())
            .stderr(watch_log.reopen().unwrap()),
    )
    .unwrap();
    let watch_id = libc::pid_t::try_from(watch.id()).unwrap();
    // SAFETY: `kill` only sends a signal, to the watch alone.
    let signal = |signal| unsafe { libc::kill(watch_id, signal) } == 0;
    let lock = || {
        let hosts = std::fs::read_dir(cache.path().join("mago/hosts")).ok()?.next()?.ok()?.path();
        std::fs::File::options().write(true).open(hosts.join("compile.lock")).ok()
    };
    let started = within(&|| log().lines().count() == 1);
    // Stopped while it waits for a change, the watch holds no lock of the hosts' folder.
    let idle = started
        && within(&|| {
            let free = signal(libc::SIGSTOP) && lock().is_some_and(|lock| lock.try_lock().is_ok());
            if !free {
                signal(libc::SIGCONT);
            }
            free
        });
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
    let restarted = idle && within(&|| log().lines().count() == 3);
    watch.kill().unwrap();
    watch.wait().unwrap();

    let hosts = log();
    let runs: Vec<(&str, &str)> = hosts.lines().filter_map(|line| line.split_once(' ')).collect();
    let failure = format!("{hosts}\n{}", std::fs::read_to_string(watch_log.path()).unwrap());
    assert!(idle, "the watch never let the hosts' folder go: {failure}");
    let terminal = terminal.unwrap();
    assert!(terminal.status.success(), "{}", printed(&terminal));
    assert!(restarted, "{failure}");
    assert_eq!(runs[0].1, "first", "{failure}");
    assert_eq!(runs[1].1, "second", "the terminal's run: {failure}");
    assert_eq!(runs[2].1, "second", "{failure}");
    assert_ne!(runs[2].0, runs[0].0, "the watch started its hosts again: {failure}");
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

require $argv[1];

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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.acme]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    compile_succeeds(root);
    assert!(compiled(root, "app/Order.sharp").exists());
    assert!(compiled(root, MONEY_PATH).exists());
}

/// A host's rule reports only on the project's own files, never on a Composer package's `.sharp` file the compile checks
/// beside them: its error there is left out, so the package file compiles, while its error on a project file refuses
/// that file.
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\n\n[extension-hosts.acme]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(printed(&output).matches("acme/no-marker").count(), 1, "{}", printed(&output));
    assert!(printed(&output).contains("app/Marked.sharp"), "{}", printed(&output));
    assert!(!printed(&output).contains("Money.sharp"), "{}", printed(&output));
    assert!(compiled(root, MONEY_PATH).exists(), "{}", printed(&output));
    assert!(!compiled(root, "app/Marked.sharp").exists(), "{}", printed(&output));
}

/// A host's rule reports on each file of `includes` outside a Composer package, as on a file under `paths`: the compile
/// lowers the project's `lib/Shop/Report.sharp` too, so its error there refuses that file.
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
        &format!(
            "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\", \"lib\"]\n\n[extension-hosts.acme]\nworkers = 1\ncommand = [\"php\", \"tools/host.php\", {:?}]\n",
            repository.join("vendor/autoload.php").display().to_string()
        ),
    );

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert_eq!(printed(&output).matches("acme/no-marker").count(), 1, "{}", printed(&output));
    assert!(printed(&output).contains("lib/Shop/Report.sharp"), "{}", printed(&output));
    assert!(!compiled(root, "lib/Shop/Report.sharp").exists(), "{}", printed(&output));
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
        [
            Path::new("compiled.bin"),
            Path::new("inputs.json"),
            Path::new("listeners.php"),
            Path::new("preload.php"),
            Path::new("vendor/acme/money/src/Money.sharpc")
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
            Path::new("app/Order.sharpc"),
            Path::new("compiled.bin"),
            Path::new("inputs.json"),
            Path::new("listeners.php"),
            Path::new("preload.php")
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
    assert_eq!(files_under(&root.join(".sharp/app")), [Path::new("Order.sharpc")], "no temporary file is left");
}

/// The PHP file `mago compile` writes beside `app/Order.sharp`, spec section 27.
const ORDER_PHP: &str = r#"<?php

// mago compile writes this file from Order.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

namespace App;

use Error;

use function function_exists;
use function is_file;
use function sprintf;

if (is_file(sprintf('%s/Order.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Order.sharp', __DIR__);
}

if (false) {
    class Order
    {
        // The engine declares Order from Order.sharp, and this declaration never runs.
    }
}
"#;

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
        [
            Path::new(".gitignore"),
            Path::new("Legacy.php"),
            Path::new("Order.php"),
            Path::new("Order.sharp"),
            Path::new("Shop/Module.sharp")
        ]
    );
    assert_eq!(files_under(&root.join("vendor")), vendor, "nothing is written under vendor/");
}

/// The PHP file states the type's kind, an interface's and an enum's included, and a type named after a PHP keyword
/// gets none, which PHP would refuse. A type named `Error` declares the name the file would import, so its file names
/// `\Error` in full instead, and a type in no namespace imports nothing, which PHP would warn of.
#[test]
fn compile_writes_the_php_file_of_each_kind_of_type() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let types = [
        ("Payable", "public interface Payable\n{\n}\n", PAYABLE_PHP),
        ("Status", "public enum Status\n{\n    case Open;\n    case Paid;\n}\n", STATUS_PHP),
        ("Match", "public class Match\n{\n}\n", MATCH_PHP),
        ("Error", "public class Error\n{\n}\n", ERROR_PHP),
    ];
    for (name, declaration, _) in types {
        write(root, &format!("app/Billing/{name}.sharp"), &format!("namespace App.Billing;\n\n{declaration}"));
    }
    write(root, "app/Rootless.sharp", "public class Rootless\n{\n}\n");

    compile_succeeds(root);

    assert_eq!(std::fs::read_to_string(root.join("app/Order.php")).unwrap(), ORDER_PHP);
    for (name, _, php) in types {
        assert_eq!(std::fs::read_to_string(root.join(format!("app/Billing/{name}.php"))).unwrap(), php, "{name}");
    }
    assert_eq!(std::fs::read_to_string(root.join("app/Rootless.php")).unwrap(), ROOTLESS_PHP);
}

const PAYABLE_PHP: &str = r#"<?php

// mago compile writes this file from Payable.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

namespace App\Billing;

use Error;

use function function_exists;
use function is_file;
use function sprintf;

if (is_file(sprintf('%s/Payable.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Payable.sharp', __DIR__);
}

if (false) {
    interface Payable
    {
        // The engine declares Payable from Payable.sharp, and this declaration never runs.
    }
}
"#;

const STATUS_PHP: &str = r#"<?php

// mago compile writes this file from Status.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

namespace App\Billing;

use Error;

use function function_exists;
use function is_file;
use function sprintf;

if (is_file(sprintf('%s/Status.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Status.sharp', __DIR__);
}

if (false) {
    enum Status
    {
        // The engine declares Status from Status.sharp, and this declaration never runs.
    }
}
"#;

const MATCH_PHP: &str = r#"<?php

// mago compile writes this file from Match.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

namespace App\Billing;

use Error;

use function function_exists;
use function is_file;
use function sprintf;

if (is_file(sprintf('%s/Match.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Match.sharp', __DIR__);
}
"#;

const ERROR_PHP: &str = r#"<?php

// mago compile writes this file from Error.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

namespace App\Billing;

use function function_exists;
use function is_file;
use function sprintf;

if (is_file(sprintf('%s/Error.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new \Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Error.sharp', __DIR__);
}

if (false) {
    class Error
    {
        // The engine declares Error from Error.sharp, and this declaration never runs.
    }
}
"#;

const ROOTLESS_PHP: &str = r#"<?php

// mago compile writes this file from Rootless.sharp, so PHP code that finds *.php files finds its type.
// phpcs:ignoreFile

declare(strict_types=1);

if (is_file(sprintf('%s/Rootless.sharp', __DIR__))) {
    function_exists('Sharp\Internal\requireNative')
        || throw new Error(
            'This project needs the PHP# engine, and this is plain PHP. Install php-sharp, or run the ghcr.io/heyjordanparker/php-sharp image.',
        );

    require_once sprintf('%s/Rootless.sharp', __DIR__);
}

if (false) {
    class Rootless
    {
        // The engine declares Rootless from Rootless.sharp, and this declaration never runs.
    }
}
"#;

/// A source in an `autoload-dev` folder gets no PHP file, so a test class keeps one file: PHPUnit loads a test class
/// only from the file it lists, and a project lists its PHP# tests with `suffix="Test.sharp"`.
#[test]
fn compile_writes_no_php_file_beside_a_source_in_an_autoload_dev_folder() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "composer.json",
        "{\"type\": \"library\", \"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}, \"autoload-dev\": {\"psr-4\": {\"Tests\\\\\": \"tests/\"}}}\n",
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
        process::output(
            Command::new(env!("CARGO_BIN_EXE_mago"))
                .args(["--no-version-check", "--colors", "never"])
                .args(arguments)
                .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
                .current_dir(root),
        )
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

    let output = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "fmt", "--check", "app/Order.php"])
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
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
        [Path::new(".gitignore"), Path::new("Invoice.php"), Path::new("Invoice.sharp"), Path::new("Legacy.php")]
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
        [Path::new(".gitignore"), Path::new("Legacy.php"), Path::new("Order.php"), Path::new("Order.sharp")]
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
        "namespace App;\n\nimport Lib.Pace;\n\npublic class Paced\n{\n    public int percent()\n    {\n        return Pace.percent();\n    }\n}\n",
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
        "namespace App;\n\nimport Acme.Rate;\n\npublic class Paced\n{\n    public int percent() => Rate.percent();\n}\n",
    );
    compile_succeeds(root);

    write(root, "vendor/acme/rate/src/Rate.php", &rate("string"));
    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("app/Paced.sharp"), "{}", printed(&output));
    assert!(!compiled(root, "app/Paced.sharp").exists());
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

/// A `Module.sharp` at the root, outside the configured paths, is an input of each compiled file, which the engine
/// reads, so one that appears makes the compile write each compiled file again.
#[test]
fn compile_writes_each_compiled_file_again_once_a_module_file_appears_at_the_root() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let written = compiled_at(root, "app/Order.sharp");

    write(root, "Module.sharp", "module\n{\n}\n");
    compile_succeeds(root);

    assert_ne!(compiled_at(root, "app/Order.sharp"), written, "the compile ran");
}

/// The loader reads which folder `autoload-dev` maps from `composer.json`, so moving a folder there drops the PHP file
/// beside each of its sources.
#[test]
fn compile_deletes_the_php_file_of_a_source_whose_folder_moved_to_autoload_dev() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "composer.json", "{\"type\": \"library\", \"autoload\": {\"psr-4\": {\"App\\\\\": \"app/\"}}}\n");
    compile_succeeds(root);
    assert!(root.join("app/Order.php").exists());

    write(root, "composer.json", "{\"type\": \"library\", \"autoload-dev\": {\"psr-4\": {\"App\\\\\": \"app/\"}}}\n");
    compile_succeeds(root);

    assert!(!root.join("app/Order.php").exists());
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

/// A compile that fails with an error of its own, as a PHP file someone wrote where a source's PHP file goes, prints
/// it again from its record while nothing changed.
#[test]
fn a_compile_after_a_failed_one_with_nothing_changed_prints_each_error_again() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/Order.php", "<?php\n\nnamespace App;\n\nfinal class Order {}\n");
    let failed = compile(root);
    assert_eq!(failed.status.code(), Some(1), "{}", printed(&failed));
    assert!(
        printed(&failed).contains("app/Order.php and app/Order.sharp both name App.Order."),
        "{}",
        printed(&failed)
    );

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

/// A file someone writes over one the compile wrote, while the compile still runs, makes the compile keep no record,
/// as a compile that ran at once does, so the next compile sees the file. `composer.lock` is a named pipe here, which
/// holds the compile at its read of it, after it wrote `app/Order.php`, until the test wrote its own `app/Order.php`.
#[cfg(unix)]
#[test]
fn compile_refuses_a_php_file_someone_wrote_over_its_own_while_it_ran() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    let lock = root.join("composer.lock");
    std::fs::remove_file(&lock).unwrap();
    assert!(process::status(Command::new("mkfifo").arg(&lock)).unwrap().success());
    let running = process::spawn(command(root, "never").stdout(Stdio::piped()).stderr(Stdio::piped())).unwrap();

    // The pipe opens once the compile opens it to read it.
    let mut pipe = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
    assert!(written_by_compile(&root.join("app/Order.php")));
    write(root, "app/Order.php", "<?php\n\nnamespace App;\n\nfinal class Order {}\n");
    pipe.write_all(b"{}\n").unwrap();
    drop(pipe);
    let swapped = running.wait_with_output().unwrap();
    assert!(swapped.status.success(), "{}", printed(&swapped));
    assert!(!root.join(".sharp/inputs.json").exists(), "the compile kept no record");
    std::fs::remove_file(&lock).unwrap();
    write(root, "composer.lock", "{}\n");

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("app/Order.php and app/Order.sharp both name App.Order."),
        "{}",
        printed(&output)
    );
}

/// Whether the PHP file at `path` is one the compile wrote, which loads its `.sharp` file through the engine.
fn written_by_compile(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|contents| contents.contains("needs the PHP# engine"))
}

/// The `.gitignore` the compile writes beside its PHP files is one of its outputs, so a compile writes it again once
/// it is gone.
#[test]
fn compile_writes_its_gitignore_again_once_it_is_gone() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let gitignore = std::fs::read_to_string(root.join("app/.gitignore")).unwrap();

    std::fs::remove_file(root.join("app/.gitignore")).unwrap();
    compile_succeeds(root);

    assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), gitignore);
}

/// A `.gitignore` someone else wrote beside the compile's PHP files is an input of the compile, so one that stops
/// listing a PHP file makes the compile run and warn of it.
#[test]
fn compile_warns_once_a_gitignore_someone_else_wrote_stops_listing_its_php_file() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(root, "app/.gitignore", "/Order.php\n");
    let listed = compile(root);
    assert!(listed.status.success() && !printed(&listed).contains("doesn't list"), "{}", printed(&listed));

    write(root, "app/.gitignore", "/Other.php\n");
    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(printed(&output).contains("app/.gitignore doesn't list /Order.php"), "{}", printed(&output));
}

/// The walk follows a link as the loader does, and the compile writes the compiled file at the source's real path, so
/// a link someone points at an identical copy elsewhere, as `ln -sfn` does, makes the compile run, though no file's
/// bytes changed. A module file declares no type, so no PHP file the compile wrote sits behind the link.
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

/// Runs git in `folder`, and fails the test when git fails.
fn git(folder: &Path, arguments: &[&str]) -> String {
    let output = process::output(
        Command::new("git")
            .args(["-c", "user.name=PHP#", "-c", "user.email=sharp@example.com", "-c", "init.defaultBranch=main"])
            .args(arguments)
            .current_dir(folder),
    )
    .unwrap();
    assert!(output.status.success(), "git {arguments:?}: {}", printed(&output));

    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The files `git status` lists as untracked in `folder`.
fn untracked(folder: &Path) -> String {
    git(folder, &["status", "--porcelain", "--untracked-files=all"])
}

/// The `.gitignore` `mago compile` writes in a folder where the PHP files it wrote are `files`, one `/<name>` line each.
fn gitignore(files: &str) -> String {
    format!(
        "# mago compile writes this file and the files it lists. It rewrites it on every compile.\n/.gitignore\n{files}"
    )
}

/// Each checkout of a repository has its own working tree, so the `.gitignore` a compile writes there hides the PHP
/// files of that checkout alone. A PHP file someone wrote at the same path in a linked worktree stays where git shows
/// it, so a branch switch there can't overwrite it unseen, as it could while the compile listed its files in the
/// `info/exclude` every checkout shares.
#[test]
fn compile_hides_its_php_files_from_git_in_its_own_checkout_alone() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);
    let worktrees = tempfile::tempdir().unwrap();
    let worktree = std::fs::canonicalize(worktrees.path()).unwrap().join("feature");
    git(&clone, &["worktree", "add", "--quiet", worktree.to_str().unwrap()]);
    write(&worktree, "app/Tenant/Publishable.php", "<?php\n\nnamespace App\\Tenant;\n\ninterface Publishable {}\n");
    write(&clone, "app/Tenant/Publishable.sharp", "namespace App.Tenant;\n\npublic interface Publishable\n{\n}\n");

    compile_succeeds(&clone);

    assert!(clone.join("app/Tenant/Publishable.php").exists());
    assert_eq!(untracked(&worktree), "?? app/Tenant/Publishable.php\n");
}

/// The compile writes a `.gitignore` in each folder that holds a PHP file it wrote, listing itself and each such file,
/// so git shows neither, and still shows a new `.sharp` source as untracked.
#[test]
fn compile_writes_a_gitignore_that_lists_itself_and_each_php_file_it_wrote() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    write(&clone, ".gitignore", "/.sharp/\n");
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);
    write(&clone, "app/Refund.sharp", "namespace App;\n\npublic class Refund\n{\n}\n");

    compile_succeeds(&clone);

    assert_eq!(std::fs::read_to_string(clone.join("app/.gitignore")).unwrap(), gitignore("/Order.php\n/Refund.php\n"));
    assert_eq!(untracked(&clone), "?? app/Refund.sharp\n");
}

/// A `.gitignore` the compile didn't write, here a tracked one, stays as it is. The compile warns of each PHP file it
/// doesn't list, which git shows as untracked, and is quiet once the file lists it.
#[test]
fn compile_leaves_a_gitignore_it_did_not_write_and_names_each_php_file_it_does_not_list() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    write(&clone, ".gitignore", "/.sharp/\n");
    write(&clone, "app/.gitignore", "*.log\n");
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);

    let output = compile(&clone);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(printed(&output).contains(UNLISTED_ORDER), "{}", printed(&output));
    assert_eq!(std::fs::read_to_string(clone.join("app/.gitignore")).unwrap(), "*.log\n");
    assert_eq!(untracked(&clone), "?? app/Order.php\n");

    write(&clone, "app/.gitignore", "*.log\n/Order.php\n");
    let output = compile(&clone);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(!printed(&output).contains("doesn't list"), "{}", printed(&output));
    assert_eq!(std::fs::read_to_string(clone.join("app/.gitignore")).unwrap(), "*.log\n/Order.php\n");
}

/// The warning of a `.gitignore` the compile didn't write and that doesn't list `app/Order.php`.
const UNLISTED_ORDER: &str =
    "app/.gitignore doesn't list /Order.php, so git may show app/Order.php as untracked: add /Order.php to it.";

/// A `.gitignore` the compile didn't write lists a PHP file when a line names it as `/` and its name and no `!` line
/// after that names it again, which git reads as showing the file. A byte order mark before the first line is no part
/// of that line.
#[test]
fn compile_reads_a_gitignore_it_did_not_write_as_git_reads_it() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();

    for (contents, listed) in [
        ("/Order.php\n!/Order.php\n", false),
        ("/Order.php\n!Order.php\n", false),
        ("!/Order.php\n/Order.php\n", true),
        ("\u{feff}/Order.php\n", true),
        ("/Order.php\r\n", true),
    ] {
        write(root, "app/.gitignore", contents);

        let output = compile(root);

        assert!(output.status.success(), "{contents:?}: {}", printed(&output));
        assert_eq!(printed(&output).contains(UNLISTED_ORDER), !listed, "{contents:?}: {}", printed(&output));
        assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), contents);
    }
}

/// A `.gitignore` that starts with the compile's first line but holds a line the compile never writes is someone
/// else's: the compile writes nothing to it, never deletes it, and warns of each PHP file it doesn't list.
#[test]
fn compile_leaves_its_gitignore_once_someone_adds_a_line() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    compile_succeeds(root);
    let edited = gitignore("*.log\n");
    write(root, "app/.gitignore", &edited);

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(printed(&output).contains(UNLISTED_ORDER), "{}", printed(&output));
    assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), edited);

    std::fs::remove_file(root.join("app/Order.sharp")).unwrap();
    compile_succeeds(root);

    assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), edited, "it deletes no file of another");
}

/// Git reads no `.gitignore` that is a link, so the compile counts one as listing nothing: it warns of each PHP file,
/// and writes nothing through the link.
#[cfg(unix)]
#[test]
fn compile_warns_of_a_gitignore_that_is_a_link() {
    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    write(&root, ".gitignore", "/.sharp/\n/ignores\n");
    write(&root, "ignores", "/Order.php\n");
    std::os::unix::fs::symlink(root.join("ignores"), root.join("app/.gitignore")).unwrap();
    git(&root, &["init", "--quiet"]);

    let output = compile(&root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/.gitignore is a link, which git doesn't read, so git may show app/Order.php as untracked: make it a file that lists /Order.php."
        ),
        "{}",
        printed(&output)
    );
    assert!(std::fs::symlink_metadata(root.join("app/.gitignore")).unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(root.join("ignores")).unwrap(), "/Order.php\n");
    assert!(untracked(&root).contains("?? app/Order.php\n"), "{}", untracked(&root));
}

/// A `.gitignore` the compile can't read, as a folder of that name or a file of mode 000, fails no compile: the compile
/// leaves it as it is, and warns which PHP files git may show. Root reads a file of mode 000, so the test has nothing to
/// show there.
#[cfg(unix)]
#[test]
fn compile_warns_of_a_gitignore_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;

    let folder = project(ORDER, MONEY);
    std::fs::create_dir(folder.path().join("app/.gitignore")).unwrap();
    let closed = project(ORDER, MONEY);
    write(closed.path(), "app/.gitignore", "/Order.php\n");
    let file = closed.path().join("app/.gitignore");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut cases = vec![(folder.path(), "is a directory")];
    if std::fs::read(&file).is_err() {
        cases.push((closed.path(), "permission denied"));
    }

    for (root, reason) in cases {
        let output = compile(root);

        assert!(output.status.success(), "{}", printed(&output));
        assert!(
            printed(&output).contains(&format!(
                "app/.gitignore can't be read: {reason}, so git may show app/Order.php as untracked. Make it a file mago compile can read, and run vendor/bin/mago compile again."
            )),
            "{}",
            printed(&output)
        );
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(folder.path().join("app/.gitignore").is_dir());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "/Order.php\n");
}

/// A refused source gets its PHP file and its line in the folder's `.gitignore`, and no compiled file. Once it is
/// deleted, only that `.gitignore` brings the compile back to a folder that holds no other source. The compile deletes
/// the PHP file it wrote there, keeps one someone wrote in another's place, and deletes its `.gitignore`, so git shows
/// that file.
#[test]
fn compile_visits_a_folder_its_gitignore_alone_names() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    write(&clone, ".gitignore", "/.sharp/\n");
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);
    let refused = |name: &str| {
        format!("namespace App.Billing;\n\npublic class {name}\n{{\n    public int owed() => \"one\";\n}}\n")
    };
    write(&clone, "app/Billing/Refund.sharp", &refused("Refund"));
    write(&clone, "app/Billing/Credit.sharp", &refused("Credit"));

    let output = compile(&clone);

    assert!(!output.status.success(), "{}", printed(&output));
    assert_eq!(
        std::fs::read_to_string(clone.join("app/Billing/.gitignore")).unwrap(),
        gitignore("/Credit.php\n/Refund.php\n")
    );
    assert!(!compiled(&clone, "app/Billing/Refund.sharp").exists());

    std::fs::remove_file(clone.join("app/Billing/Refund.sharp")).unwrap();
    std::fs::remove_file(clone.join("app/Billing/Credit.sharp")).unwrap();
    let written = "<?php\n\nnamespace App\\Billing;\n\nfinal class Refund {}\n";
    write(&clone, "app/Billing/Refund.php", written);
    compile_succeeds(&clone);

    assert_eq!(files_under(&clone.join("app/Billing")), [Path::new("Refund.php")]);
    assert_eq!(std::fs::read_to_string(clone.join("app/Billing/Refund.php")).unwrap(), written);
    assert_eq!(untracked(&clone), "?? app/Billing/Refund.php\n");
}

/// The compile deletes its `.gitignore` once the folder holds no PHP file it wrote.
#[test]
fn compile_deletes_its_gitignore_once_the_folder_holds_no_php_file_it_wrote() {
    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), gitignore("/Order.php\n"));

    std::fs::remove_file(root.join("app/Order.sharp")).unwrap();
    compile_succeeds(&root);

    assert_eq!(files_under(&root.join("app")), [Path::new("Legacy.php")]);
}

/// The compile's walk for its `.gitignore` files skips each folder the analysis's walk skips, so a folder the
/// `excludes` name, by a path or by a pattern, is never visited: its `.gitignore` and the PHP file it lists stay as they
/// are.
#[test]
fn compile_never_visits_a_folder_the_excludes_name() {
    let directory = project(ORDER, MONEY);
    let root = directory.path();
    write(
        root,
        "mago.toml",
        "php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\nincludes = [\"vendor\"]\nexcludes = [\"app/Cache\", \"app/Logs/**\"]\n",
    );
    for folder in ["app/Cache", "app/Logs"] {
        write(root, &format!("{folder}/.gitignore"), &gitignore("/Order.php\n"));
        write(root, &format!("{folder}/Order.php"), ORDER_PHP);
    }

    compile_succeeds(root);

    for folder in ["app/Cache", "app/Logs"] {
        assert_eq!(files_under(&root.join(folder)), [Path::new(".gitignore"), Path::new("Order.php")], "{folder}");
        assert_eq!(std::fs::read_to_string(root.join(folder).join(".gitignore")).unwrap(), gitignore("/Order.php\n"));
    }
}

/// A compile that can't run git, as in a runtime image that ships none, keeps the PHP files out of git all the same, in
/// a clone and in a linked worktree, because the `.gitignore` it writes needs no git.
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
        let output = process::output(command(checkout, "never").env("PATH", "")).unwrap();
        assert!(output.status.success(), "{}", printed(&output));
        assert!(checkout.join("app/Order.php").exists());
        assert_eq!(
            untracked(checkout),
            "?? .sharp/app/Order.sharpc\n?? .sharp/compiled.bin\n?? .sharp/inputs.json\n?? .sharp/listeners.php\n?? .sharp/preload.php\n?? .sharp/vendor/acme/money/src/Money.sharpc\n",
            "{}",
            checkout.display()
        );
    }
}

/// The `.gitignore` needs no repository, so a compile writes it where none is reachable: in a folder outside any git
/// repository, and in a dev container that mounts a linked worktree without the repository its `.git` names. Git reads
/// it once the checkout is back where the repository is, and the compile warns of nothing.
#[test]
fn compile_writes_its_gitignore_where_no_git_repository_is_reachable() {
    let outside = project(ORDER, MONEY);
    let mounted = project(ORDER, MONEY);
    write(mounted.path(), ".git", "gitdir: /missing/repository/.git/worktrees/feature\n");

    for root in [outside.path(), mounted.path()] {
        let output = compile(root);

        assert!(output.status.success(), "{}", printed(&output));
        assert!(!printed(&output).contains("WARN"), "{}", printed(&output));
        assert_eq!(std::fs::read_to_string(root.join("app/.gitignore")).unwrap(), gitignore("/Order.php\n"));
    }
}

/// The block of `info/exclude` that an earlier `mago compile` kept for the workspace `place`, listing `paths`.
fn excluded(place: &Path, paths: &str) -> String {
    format!("# mago compile: the files it writes in {}\n{paths}# mago compile: end\n", place.display())
}

/// A compile removes every block an earlier `mago compile` kept in the `info/exclude` every checkout of a repository
/// shares, whichever checkout it names, here from a linked worktree, and keeps every other line.
#[test]
fn compile_removes_the_blocks_it_kept_in_info_exclude() {
    let directory = project(ORDER, MONEY);
    let clone = std::fs::canonicalize(directory.path()).unwrap();
    git(&clone, &["init", "--quiet"]);
    git(&clone, &["add", "--all"]);
    git(&clone, &["commit", "--quiet", "--message", "project"]);
    let worktrees = tempfile::tempdir().unwrap();
    let worktree = std::fs::canonicalize(worktrees.path()).unwrap().join("feature");
    git(&clone, &["worktree", "add", "--quiet", worktree.to_str().unwrap()]);
    let kept = "# git ls-files --others --exclude-from=.git/info/exclude\n*.log\n";
    write(
        &clone,
        ".git/info/exclude",
        &format!(
            "{}{kept}{}",
            excluded(&clone, "/app/Order.php\n"),
            excluded(&worktrees.path().join("gone"), "/app/Refund.php\n")
        ),
    );

    compile_succeeds(&worktree);

    assert_eq!(std::fs::read_to_string(clone.join(".git/info/exclude")).unwrap(), kept);
}

/// The compile removes its blocks byte for byte: a heading, the `/` lines right after it, and the end line when it
/// comes next. Every other byte stays, a CRLF line end, a byte that is no UTF-8 and a last line with no line end
/// included, and a block torn before its end line loses only its own `/` lines.
#[test]
fn compile_removes_its_blocks_from_info_exclude_and_keeps_every_other_byte() {
    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    git(&root, &["init", "--quiet"]);
    let exclude = root.join(".git/info/exclude");
    let block = "# mago compile: the files it writes in /gone\r\n/app/Order.php\r\n# mago compile: end\r\n";
    let torn = "# mago compile: the files it writes in /torn\n/app/Refund.php\n";
    std::fs::write(&exclude, [b"*.log\r\n", block.as_bytes(), b"\xff\xfe\n", torn.as_bytes(), b"*.tmp"].concat())
        .unwrap();

    compile_succeeds(&root);

    assert_eq!(std::fs::read(&exclude).unwrap(), b"*.log\r\n\xff\xfe\n*.tmp");
}

/// An `info/exclude` that can't be written, as in a read-only `.git`, keeps the blocks an earlier `mago compile` kept,
/// which hide their files in every checkout, so the compile names the folder to make writable, which the file is
/// renamed into, and keeps quiet about the temporary file.
#[cfg(unix)]
#[test]
fn compile_warns_when_its_blocks_in_info_exclude_cannot_be_removed() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    git(&root, &["init", "--quiet"]);
    write(&root, ".git/info/exclude", &excluded(&root, "/app/Order.php\n"));
    let info = root.join(".git/info");
    std::fs::set_permissions(&info, std::fs::Permissions::from_mode(0o555)).unwrap();

    let output = compile(&root);
    std::fs::set_permissions(&info, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "{}/exclude can't be replaced, as {} can't be written: permission denied, so its mago compile blocks still hide their files in every checkout of the repository. Make {} writable, and run vendor/bin/mago compile again.",
            info.display(),
            info.display(),
            info.display()
        )),
        "{}",
        printed(&output)
    );
    assert!(!printed(&output).contains(&format!("{}/.tmp", info.display())), "{}", printed(&output));
}

/// A `.sharp` folder the compile can't open fails the compile with the folder's path and what the compile was doing.
/// Root opens a folder of mode 000, so the test has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_sharp_folder_it_cannot_open() {
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
            "mago: can't open the folder `{}`: Permission denied (os error 13)",
            compiled.display()
        )),
        "{}",
        printed(&output)
    );
}

/// A folder the compile's walk of `.sharp` reaches and can't open is the one the error names, not `.sharp`. Root opens
/// a folder of mode 000, so the test has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_folder_its_walk_of_the_sharp_folder_cannot_open() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    let app = root.join(".sharp/app");
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::read_dir(&app).is_ok() {
        std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = compile(&root);
    std::fs::set_permissions(&app, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output)
            .contains(&format!("mago: can't open the folder `{}`: Permission denied (os error 13)", app.display())),
        "{}",
        printed(&output)
    );
}

/// A folder of `.sharp` the compile can't create is named as the folder, not as the compiled file it would hold. Root
/// writes into a folder of mode 555, so the test has nothing to show there.
#[cfg(unix)]
#[test]
fn compile_names_the_folder_of_the_sharp_folder_it_cannot_create() {
    use std::os::unix::fs::PermissionsExt;

    let directory = project(ORDER, MONEY);
    let root = std::fs::canonicalize(directory.path()).unwrap();
    compile_succeeds(&root);
    let compiled = root.join(".sharp");
    let app = compiled.join("app");
    std::fs::remove_dir_all(&app).unwrap();
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::create_dir(&app).is_ok() {
        std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }

    let output = compile(&root);
    std::fs::set_permissions(&compiled, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output)
            .contains(&format!("mago: can't create the folder `{}`: Permission denied (os error 13)", app.display())),
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
        "<?php\n\nreturn [\n    'inputs' => [\n{inputs}    ],\n    'listeners' => [\n{rows}    ],\n    'dispatcher' => {dispatcher},\n    'attributes' => [\n    ],\n    'effects' => [\n    ],\n    'tags' => [\n    ],\n];\n"
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
            .contains("    'dispatcher' => ['app/Shop/Module.sharp', 'app\\\\shop'],\n    'attributes' => [\n")
    );

    write(root, "app/Module.sharp", &dispatcher_module("App"));
    let output = compile(root);
    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains("Only `app/Module.sharp` binds `Dispatcher`"), "{}", printed(&output));
    assert!(
        read(root, ".sharp/listeners.php")
            .contains("    'dispatcher' => ['app/Module.sharp', 'app'],\n    'attributes' => [\n")
    );

    std::fs::remove_file(root.join("app/Shop/Module.sharp")).unwrap();
    std::fs::remove_file(root.join("app/Module.sharp")).unwrap();
    compile_succeeds(root);
    assert!(read(root, ".sharp/listeners.php").contains("    'dispatcher' => null,\n    'attributes' => [\n"));
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
    let first = "namespace App.Stubs;\n\nimport Lib;\nimport Lib.Group;\nimport Lib.Mailer;\n\n[Group(\"mail\")]\nextern Mailer;\nextern Group;\n[Group(\"later\")]\nextern Mailer.sendLater;\n[Group(\"time\")]\nextern Lib.sentAt;\n\npublic class First\n{\n}\n";
    let directory = lawful_project(&[
        ("vendor/acme/lib/Group.php", group),
        ("vendor/acme/lib/Mailer.php", mailer),
        ("vendor/acme/lib/functions.php", functions),
        ("app/Stubs/First.sharp", first),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(
        read(root, ".sharp/listeners.php").contains(
            "    'attributes' => [\n        'lib\\\\mailer' => ['app/Stubs/First.sharp'],\n        'lib\\\\mailer::sendlater' => ['app/Stubs/First.sharp'],\n        'lib\\\\sentat()' => ['app/Stubs/First.sharp'],\n    ],\n    'effects' => [\n"
        ),
        "{}",
        read(root, ".sharp/listeners.php")
    );
}

/// Spec section 29: the listener table gives, for each plain PHP class and method an `extern` line declares, the
/// effects its `uses` lists by their dotted full names, none for a line that declares it pure, keyed as its attributes
/// are, which `Method.createDelegate` holds a plain PHP method to, spec section 25. A function is left out, since
/// reflection gives it no `Method`.
#[test]
fn compile_lists_the_effects_each_extern_line_declares_for_a_class_and_a_method() {
    let mailer = "<?php\n\nnamespace Lib;\n\nfinal class Mailer\n{\n    public function send(): void {}\n\n    public function count(): int\n    {\n        return 0;\n    }\n}\n";
    let functions = "<?php\n\nnamespace Lib;\n\nfunction sentAt(): int\n{\n    return 1;\n}\n";
    let smtp = "namespace App.Mail;\n\npublic foreign class Smtp\n{\n}\n";
    let stubs = "namespace App.Stubs;\n\nimport App.Mail.Smtp;\nimport Lib;\nimport Lib.Mailer;\n\nextern Mailer;\nextern Mailer.send uses Smtp;\nextern Lib.sentAt uses Smtp;\n\npublic class Stubs\n{\n}\n";
    let directory = lawful_project(&[
        ("vendor/acme/lib/Mailer.php", mailer),
        ("vendor/acme/lib/functions.php", functions),
        ("app/Mail/Smtp.sharp", smtp),
        ("app/Stubs/Stubs.sharp", stubs),
    ]);
    let root = directory.path();

    compile_succeeds(root);
    assert!(
        read(root, ".sharp/listeners.php").ends_with(
            "    'effects' => [\n        'lib\\\\mailer' => [],\n        'lib\\\\mailer::send' => ['App.Mail.Smtp'],\n    ],\n    'tags' => [\n    ],\n];\n"
        ),
        "{}",
        read(root, ".sharp/listeners.php")
    );
}

/// Spec section 29: a parameter an `extern` line states keeps the effects its function type lists, so a `Map` a loop
/// fills with closures that have those effects passes to the plain PHP constructor, and the `Map` local keeps the value
/// type it is declared with. A closure with an effect the type leaves out is refused at the call, naming the effect.
#[test]
fn an_extern_parameter_takes_the_functions_whose_effects_its_function_type_lists() {
    let extension = "<?php\n\nnamespace Cel;\n\nfinal readonly class CallableExtension\n{\n    /** @param array<non-empty-string, callable> $functions */\n    public function __construct(\n        private array $functions,\n        private bool $idempotent = false,\n    ) {}\n}\n";
    let stubs = "namespace App.Stubs;\n\nimport Cel.CallableExtension;\n\nextern void CallableExtension.__construct(Map<string, Function<Any?(Any?) uses Database, Files, Clock>> functions);\n";
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

/// A library whose `composer.json` maps `App\` to `app/`, holding `files`.
fn lawful_project(files: &[(&str, &str)]) -> tempfile::TempDir {
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

/// `mago compile` with each issue on one line, as `file:line:column:level - code: message`.
fn compile_lines(root: &Path) -> Output {
    process::output(command(root, "never").env("MAGO_REPORTING_FORMAT", "emacs")).unwrap()
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
    process::output(
        command(root, "never")
            .env("MAGO_REPORTING_FORMAT", "emacs")
            .env("PATH", empty.path())
            .env("XDG_CACHE_HOME", unwritable_cache()),
    )
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
    assert!(!root.join(".sharp/.lean/Code").exists(), "the runner's request holds the generated code, so no file does");
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

#[test]
fn compile_proves_a_law_in_a_project_that_installs_the_standard_library() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", LAWFUL_MONEY),
        (
            "vendor/heyjordanparker/php-sharp-composer/composer.json",
            "{\n    \"name\": \"heyjordanparker/php-sharp-composer\",\n    \"autoload\": {\n        \"psr-4\": {\n            \"Sharp\\\\\": \"library/Sharp/\"\n        }\n    }\n}\n",
        ),
        (
            "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Math.sharp",
            "namespace Sharp;\n\npublic static class Math\n{\n    public static int twice(int value) => value * 2;\n}\n",
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

/// A cache folder in `folder` whose law checker build is the tests' own, with a runner that runs the shell commands
/// `script` in its place, which `$real` names the tests' runner in. Every other file of the build is a link to the
/// tests' build, so a compile with this cache builds nothing.
#[cfg(unix)]
fn cache_with_runner(folder: &Path, script: &str) -> PathBuf {
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
fn counting_cache(folder: &Path) -> PathBuf {
    let started = folder.join("started");
    std::fs::create_dir_all(&started).unwrap();

    cache_with_runner(folder, &format!("tee '{}/'$$ | \"$real\" \"$@\"", started.display()))
}

/// The generated Lean module of `name` in the request of the one runner `counting_cache` started in `folder`: the
/// request's header and the threads and heartbeats it answers with, the modules an earlier runner stopped in, then each
/// generated module's name and source.
#[cfg(unix)]
fn generated_module(folder: &Path, name: &str) -> String {
    let mut started = std::fs::read_dir(folder.join("started")).unwrap();
    let request = std::fs::read(started.next().unwrap().unwrap().path()).unwrap();
    assert!(started.next().is_none(), "one runner started");
    let mut at = 20;
    let length = |at: &mut usize| {
        let length = u32::from_be_bytes(request[*at..*at + 4].try_into().unwrap());
        *at += 4;
        usize::try_from(length).unwrap()
    };
    let string = |at: &mut usize| {
        let bytes = length(at);
        *at += bytes;
        String::from_utf8(request[*at - bytes..*at].to_vec()).unwrap()
    };
    for _ in 0..length(&mut at) {
        string(&mut at);
    }
    for _ in 0..length(&mut at) {
        let (module, text) = (string(&mut at), string(&mut at));
        if module == name {
            return text;
        }
    }

    panic!("the request holds no module {name}")
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

    // The debug log holds what the failed build printed.
    let output = process::output(
        command(directory.path(), "never")
            .env("PATH", lake.path())
            .env("XDG_CACHE_HOME", cache.path())
            .env("MAGO_LOG", "debug"),
    )
    .unwrap();

    assert!(printed(&output).contains("the lake on the PATH ran build"), "{}", printed(&output));
    assert!(!printed(&output).contains("Downloading the law checker"), "{}", printed(&output));
}

/// A cache folder the law checker can't create, as one a root container left, is named with what failed.
#[cfg(unix)]
#[test]
fn compile_names_the_cache_folder_the_law_checker_cannot_create() {
    use std::os::unix::fs::PermissionsExt;

    let directory = lawful_project(&[("app/Shared/Money.sharp", LAWFUL_MONEY)]);
    let lake = tempfile::tempdir().unwrap();
    write(lake.path(), "lake", "#!/bin/sh\n[ \"$1\" = --version ]\n");
    std::fs::set_permissions(lake.path().join("lake"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let cache = unwritable_cache();

    let output =
        process::output(command(directory.path(), "never").env("PATH", lake.path()).env("XDG_CACHE_HOME", &cache))
            .unwrap();

    assert!(!output.status.success(), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "The laws could not be checked: can't create the folder `{}`: File exists (os error 17).",
            cache.join("mago").display()
        )),
        "{}",
        printed(&output)
    );
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
            .map(|project| {
                scope.spawn(|| process::output(command(project.path(), "never").env("PATH", &path)).unwrap())
            })
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
    assert!(!root.join(".sharp/inputs.json").exists(), "the next compile tries to install the law checker again");
}

/// A law's proof ends once while its key holds, passed or failed: the next compile reports a failed proof from
/// `.sharp/proofs.json`, so it needs no Lean, and an edit no law reaches proves nothing again.
#[test]
fn a_proof_is_not_attempted_again_while_its_law_is_unchanged() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
    let root = directory.path();
    let unproven = "app/Shared/Money.sharp:8:5:error - law-unproven: law addKeepsCurrency is not proven";
    let first = compile_lines(root);
    assert!(printed(&first).contains(unproven), "{}", printed(&first));

    let output = compile_without_lean(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(printed(&output).contains(unproven), "the failed proof is reported again: {}", printed(&output));
    assert!(!printed(&output).contains("law-checker-unavailable"), "{}", printed(&output));

    write(root, "app/Shared/Money.sharp", LAWFUL_MONEY);
    compile_succeeds(root);
    let unread = "    public int pages() => 1;\n\n    law addKeepsCurrency";
    write(root, "app/Shared/Money.sharp", &LAWFUL_MONEY.replace("    law addKeepsCurrency", unread));
    let output = compile_without_lean(root);

    assert!(output.status.success(), "the proof ran again: {}", printed(&output));
    assert!(compiled(root, "app/Shared/Money.sharp").exists());
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
            Path::new("app/Order.sharpc"),
            Path::new("compiled.bin"),
            Path::new("listeners.php"),
            Path::new("preload.php")
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

#[cfg(unix)]
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
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Alpha.sharp").exists());
    let alpha = generated_module(lake.path(), "Code.App.Shared.Alpha");
    assert!(alpha.contains("def App.Shared.Beta.middle"), "the group's module holds Beta's declaration: {alpha}");
    assert!(!alpha.contains("import Code."), "{alpha}");
}

/// A call through the class, as `ShapeMath.doubled(s)`, is that static call already, as in C#, so it compiles and its
/// law is proved the same way.
#[cfg(unix)]
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
    let lake = tempfile::tempdir().unwrap();

    let output = process::output(command(root, "never").env("XDG_CACHE_HOME", counting_cache(lake.path()))).unwrap();

    assert!(output.status.success(), "{}", printed(&output));
    let laws = generated_module(lake.path(), "Code.App.Shared.Shape");
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

/// A law reaches an enum's `cases()`, the list of its cases in the order the enum declares them, so a claim of another
/// order is not proven.
#[test]
fn a_law_reaches_an_enum_s_cases_in_declaration_order() {
    let level = |claim: &str| {
        format!(
            "namespace App.Auth;\n\npublic enum Level\n{{\n    case Low;\n    case Mid;\n    case High;\n\n    public static Level lowest() => match (Level.cases()) {{\n        [Level first, ...List<Level> rest] => first,\n        default => Level.High,\n    }};\n\n    law lowestIsDeclaredFirst(int unused) => {claim};\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Auth/Level.sharp",
        &level("Level.lowest() == Level.Low && Level.cases().count() == 3"),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Level.sharp").exists());

    let directory = library_project(&[("app/Auth/Level.sharp", &level("Level.lowest() == Level.High"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law lowestIsDeclaredFirst is not proven\n = still to show: Level.lowest() == Level.High\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A backed enum `Status` stating `laws`.
fn backed_status(laws: &str) -> String {
    format!(
        "namespace App.Shop;\n\npublic enum Status : string\n{{\n    case Open = \"open\";\n    case Paid = \"paid\";\n\n    {laws}\n}}\n"
    )
}

/// A law reaches a backed enum's `from`, the case whose value matches, which throws `ValueError` for any other value as
/// the engine does, and its `tryFrom`, the case or `null`. A false law over either is not proven.
#[test]
fn a_law_reaches_a_backed_enum_s_from_and_try_from() {
    let directory = lawful_project(&[(
        "app/Shop/Status.sharp",
        &backed_status(
            "law fromValueGivesTheCase(Status s) => Status.from(s.value) == s;\n    law tryFromOfAnUnknownValueIsNull(int unused) => Status.tryFrom(\"lost\") == null;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Status.sharp").exists());

    for (law, claim) in [
        ("fromOpenIsPaid", "Status.from(\"open\") == Status.Paid"),
        ("tryFromOfPaidIsNull", "Status.tryFrom(\"paid\") == null"),
    ] {
        let directory =
            lawful_project(&[("app/Shop/Status.sharp", &backed_status(&format!("law {law}(int unused) => {claim};")))]);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        assert!(
            printed(&output)
                .contains(&format!("error[law-unproven]: law {law} is not proven\n = still to show: {claim}\n")),
            "{}",
            printed(&output)
        );
    }
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

/// `app/Shop/OrderStatus.sharp`, a sealed family of the states `Pending` and `Paid`, whose `Pending` states `laws`.
fn order_states(laws: &str) -> tempfile::TempDir {
    lawful_project(&[
        ("app/Shop/OrderStatus.sharp", "namespace App.Shop;\n\npublic sealed interface OrderStatus\n{\n}\n"),
        (
            "app/Shop/Paid.sharp",
            "namespace App.Shop;\n\npublic class Paid : OrderStatus\n{\n    public Paid(public int paidAt { get; }) { }\n}\n",
        ),
        (
            "app/Shop/Pending.sharp",
            &format!(
                "namespace App.Shop;\n\npublic class Pending : OrderStatus\n{{\n    public static Pending initial() => new Pending();\n    public static OrderStatus start() => new Pending();\n    public Paid pay(int at) => new Paid(at);\n    public OrderStatus settle(int at) => new Paid(at);\n\n    {laws}\n}}\n"
            ),
        ),
    ])
}

/// Spec section 28: a law reads a sealed family as the closed set of its implementers, so a type test over it, as an
/// `is` or a `match`, is a test of which implementer a value is, and an implementer's get-only property reads in the
/// branch the test names it in. A false claim of which state a move enters is not proven, and a test of a class against
/// another class is the checker's own error before any law runs.
#[test]
fn compile_proves_a_law_over_a_sealed_family_s_type_tests_and_its_states_data() {
    let directory = order_states(
        "law startsPending(int unused) => Pending.initial() is Pending && Pending.start() is Pending;\n    law payKeepsTime(Pending p, int at) => p.pay(at).paidAt == at;\n    law settleKeepsTime(Pending p, int at) => p.settle(at) is Paid paid ? paid.paidAt == at : false;\n    law settleIsNoLongerPending(Pending p, int at) => match (p.settle(at)) {\n        Pending pending => false,\n        Paid paid => paid.paidAt == at,\n    };",
    );
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Pending.sharp").exists());
    assert_eq!(source_lean_files(root), Vec::<PathBuf>::new());

    let directory = order_states("law startIsPaid(int unused) => Pending.start() is Paid;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law startIsPaid is not proven\n = still to show: Pending.start() is Paid\n"
        ),
        "{}",
        printed(&output)
    );

    let directory = order_states("law startsPaid(int unused) => Pending.initial() is Paid;");

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shop/Pending.sharp:10:56: error[impossible-type-comparison]: This pattern never matches the value it tests.\n"
        ),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a move whose `?:`, `match` or `??` gives one state in a branch and another in the next gives a value
/// of the sealed interface, an enum's case among them, and a law tells its states apart. A state's property reads where
/// a test narrowed the value to it, and in its own methods.
#[test]
fn compile_proves_a_law_over_moves_whose_branches_give_different_states() {
    let directory = lawful_project(&[
        ("app/Shop/Status.sharp", "namespace App.Shop;\n\npublic sealed interface Status\n{\n}\n"),
        (
            "app/Shop/Open.sharp",
            "namespace App.Shop;\n\npublic enum Open : Status\n{\n    case Fresh;\n    case Reopened(int times);\n}\n",
        ),
        (
            "app/Shop/Closed.sharp",
            "namespace App.Shop;\n\npublic class Closed : Status\n{\n    public Closed(public int at { get; }) { }\n\n    public static Status close(Status status, int at) => status is Open ? new Closed(at) : status;\n    public static Status reopen(Status status) => match (status) {\n        Closed closed => Open.Reopened(closed.at),\n        Open open => open,\n    };\n    public static Status settled(Closed? closed) => closed ?? Open.Fresh;\n    public static int closedAt(Status status) => status is Closed ? status.at : 0;\n    public Closed same() => new Closed(this.at);\n\n    law closeCloses(Status status, int at) => Closed.close(status, at) is Closed;\n    law reopenCounts(int at) => Closed.reopen(new Closed(at)) is Open.Reopened(int times) ? times == at : false;\n    law freshStaysFresh(int at) => Closed.reopen(Open.Fresh) is Open;\n    law unsettledIsFresh(int unused) => Closed.settled(null) is Open;\n    law closeKeepsTime(int at) => Closed.closedAt(Closed.close(Open.Fresh, at)) == at;\n    law sameKeepsTime(Closed closed) => closed.same().at == closed.at;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Closed.sharp").exists());
}

/// Spec section 28: a law over a sealed interface that no class implements proves, as one over any other interface's
/// value does, though the interface has no implementer to tell apart.
#[test]
fn compile_proves_a_law_over_a_sealed_interface_no_class_implements() {
    let directory = lawful_project(&[
        ("app/Shop/Nothing.sharp", "namespace App.Shop;\n\npublic sealed interface Nothing\n{\n}\n"),
        (
            "app/Shop/Uses.sharp",
            "namespace App.Shop;\n\npublic static class Uses\n{\n    public static bool keep(Nothing value) => true;\n\n    law keeps(Nothing value) => Uses.keep(value);\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/Uses.sharp").exists());
}

/// Spec section 28: a law reaches a standard library type from its PHP# source, as it reaches a project type, so a law
/// over one state of a sealed family proves though another state holds an `Instant`.
#[test]
fn compile_proves_a_law_over_a_state_beside_one_that_holds_an_instant() {
    let directory = library_project(&[
        (
            "app/Delivery/DeliveryStatus.sharp",
            "namespace App.Delivery;\n\npublic sealed interface DeliveryStatus\n{\n}\n",
        ),
        (
            "app/Delivery/Pending.sharp",
            "namespace App.Delivery;\n\npublic final class Pending : DeliveryStatus\n{\n    public Pending(public string? error { get; }) { }\n\n    public Pending postpone(string error) => new Pending(error);\n\n    law postponeHoldsTheError(string? held, string error) => new Pending(held).postpone(error).error == error;\n}\n",
        ),
        (
            "app/Delivery/Done.sharp",
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Done : DeliveryStatus\n{\n    internal Done(public Instant doneAt { get; }) { }\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Delivery/Pending.sharp").exists());
}

/// Spec section 28: `<` on two values of a class runs the class's own `<=>`, so a law that compares two `Instant`s
/// proves through `Instant`'s, which compares their seconds, then their nanoseconds. A false order is not proven.
#[test]
fn compile_proves_a_law_that_orders_two_instants_through_their_own_comparison() {
    let schedule = |laws: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Schedule\n{{\n    {laws}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Schedule.sharp",
        &schedule(
            "law earlierSecondIsEarlier(Instant a, Instant b) when a.epochSeconds < b.epochSeconds => a < b;\n    law sameSecondByNanosecond(Instant a, Instant b) when a.epochSeconds == b.epochSeconds && a.nanosecond < b.nanosecond => a < b;",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Schedule.sharp").exists());

    let directory = library_project(&[(
        "app/Shared/Schedule.sharp",
        &schedule("law laterNanosecondIsEarlier(Instant a, Instant b) when a.nanosecond > b.nanosecond => a < b;"),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law laterNanosecondIsEarlier is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// Spec section 28: a standard library member a law can't reach is refused where the law reaches it, with the reason
/// project code gives, as `Instant.tryParse`, which calls plain PHP.
#[test]
fn a_law_reaching_a_standard_library_member_that_calls_plain_php_is_refused_naming_the_call() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic class Shape\n{\n    law parses(string text) => Instant.tryParse(text) != null;\n}\n",
        &[],
        "Law parses reaches the plain PHP function `global.preg_match` in Instant.tryParse. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
    );
}

/// Spec section 28: a call in a law that leaves out an argument passes its parameter's default, as the call does at
/// run time, for a constructor, a static method and a method alike. `==` on two `Instant`s, a struct that declares no
/// `operator ==`, compares each field with the field's own `==`. A claim the default or the fields break is not proven.
#[test]
fn compile_proves_a_law_whose_calls_leave_out_defaulted_arguments() {
    let pending = |laws: &str| {
        format!(
            "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Pending : DeliveryStatus\n{{\n    public Pending(public string? error {{ get; }} = null) {{ }}\n\n    public static Pending enqueued() => new Pending();\n\n    public static Pending failed(string error = \"timeout\") => new Pending(error);\n\n    public Pending postpone(string error = \"retry\") => new Pending(error);\n\n    public Done deliver(Instant at) => new Done(at);\n\n    {laws}\n}}\n"
        )
    };
    let family = |laws: &str| {
        library_project(&[
            (
                "app/Delivery/DeliveryStatus.sharp",
                "namespace App.Delivery;\n\npublic sealed interface DeliveryStatus\n{\n}\n",
            ),
            ("app/Delivery/Pending.sharp", &pending(laws)),
            (
                "app/Delivery/Done.sharp",
                "namespace App.Delivery;\n\nimport Sharp.Time.Instant;\n\npublic final class Done : DeliveryStatus\n{\n    internal Done(public Instant doneAt { get; }) { }\n}\n",
            ),
        ])
    };
    let directory = family(
        "law deliveredAt(Instant at) => new Pending().deliver(at).doneAt == at;\n    law enqueuedHoldsNoError(int unused) => Pending.enqueued().error == null;\n    law failedTimesOut(int unused) => Pending.failed().error == \"timeout\";\n    law postponedRetries(string? held) => new Pending(held).postpone().error == \"retry\";",
    );
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Delivery/Pending.sharp").exists());

    let directory = family(
        "law failedHoldsNothing(int unused) => Pending.failed().error == null;\n    law deliveredAtAnother(Instant at, Instant other) => new Pending().deliver(at).doneAt == other;",
    );

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for law in ["failedHoldsNothing", "deliveredAtAnother"] {
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec section 19: a struct with a field whose `==` a law can't read, as a nullable struct's, keeps the class operator
/// refusal for the `==` it gets with no code.
#[test]
fn a_law_comparing_structs_whose_field_has_no_equality_a_law_reads_is_refused() {
    assert_unmodeled(
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Shape\n{\n    public Shape(public Instant? closedAt { get; }) { }\n\n    law same(Shape shape) => shape == shape;\n}\n",
        "Law same reaches the class operator `==` in Shape.operator ==. Compare the values the objects hold, such as their properties.",
    );
}

/// Spec section 19: `==` on two values of a struct that holds another struct compares that field by its own `==`, and
/// `==` on two values of a class that declares `operator ==` runs it. A claim either breaks is not proven.
#[test]
fn compile_proves_a_law_over_a_struct_that_holds_a_struct_and_over_a_class_s_own_equality() {
    let window = (
        "app/Shared/Window.sharp",
        "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic struct Window\n{\n    public Window(public Instant opened { get; }) { }\n}\n",
    );
    let badge = |laws: &str| {
        format!(
            "namespace App.Shared;\n\nimport Sharp.Time.Instant;\n\npublic final class Badge\n{{\n    public Badge(public int id {{ get; }}, public string label {{ get; }}) {{ }}\n\n    public static bool operator ==(Badge a, Badge b) => a.id == b.id;\n\n    public int hash() => this.id;\n\n    {laws}\n}}\n"
        )
    };
    let badges = badge(
        "law sameWindow(Instant at) => new Window(at) == new Window(at);\n    law sameBadge(int id) => new Badge(id, \"a\") == new Badge(id, \"b\");",
    );
    let directory = library_project(&[window, ("app/Shared/Badge.sharp", &badges)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shared/Badge.sharp").exists());

    let badges = badge(
        "law anyWindow(Instant at, Instant other) => new Window(at) == new Window(other);\n    law differentBadges(int id) => new Badge(id, \"a\") != new Badge(id, \"b\");",
    );
    let directory = library_project(&[window, ("app/Shared/Badge.sharp", &badges)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for law in ["anyWindow", "differentBadges"] {
        assert!(
            printed(&output).contains(&format!("error[law-unproven]: law {law} is not proven\n")),
            "{}",
            printed(&output)
        );
    }
}

/// Spec section 28: a default a law can't read is refused, naming its parameter, only where a call leaves the argument
/// out, so a call that passes it still proves.
#[test]
fn a_law_that_leaves_out_an_argument_whose_default_a_law_cannot_read_is_refused_naming_the_parameter() {
    let shape = |law: &str| {
        format!(
            "namespace App.Shared;\n\npublic class Shape\n{{\n    public static int capped(int limit = 2 ** 3) => limit;\n\n    {law}\n}}\n"
        )
    };
    let directory = library_project(&[(
        "app/Shared/Shape.sharp",
        &shape("law cappedAtTen(int unused) => Shape.capped(10) == 10;"),
    )]);

    let output = compile(directory.path());

    assert!(output.status.success(), "{}", printed(&output));
    assert_unmodeled(
        &shape("law cappedAtMax(int unused) => Shape.capped() >= 0;"),
        "Law cappedAtMax reaches the operator `**` here in the default of `limit` in Shape.capped.",
    );
}

/// Spec section 28: a law reads the fields of an enum's data case, in the branch a pattern or a type test names the
/// case in, as the constructor's fields. A false claim over a field is not proven.
#[test]
fn compile_proves_a_law_over_the_fields_of_an_enum_s_data_case() {
    let result = |laws: &str| {
        format!(
            "namespace App.Shop;\n\npublic enum PaymentResult\n{{\n    case Paid(int amount);\n    case Declined(string reason);\n\n    public static PaymentResult charge(int amount) => amount > 0 ? PaymentResult.Paid(amount) : PaymentResult.Declined(\"empty\");\n\n    {laws}\n}}\n"
        )
    };
    let directory = lawful_project(&[(
        "app/Shop/PaymentResult.sharp",
        &result(
            "law chargeKeepsAmount(int amount) => PaymentResult.charge(amount) is PaymentResult.Paid paid ? paid.amount == amount : amount <= 0;\n    law declinedGivesReason(int amount) => match (PaymentResult.charge(amount)) {\n        PaymentResult.Paid(int paid) => paid > 0,\n        PaymentResult.Declined declined => declined.reason == \"empty\",\n    };",
        ),
    )]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Shop/PaymentResult.sharp").exists());

    let directory = lawful_project(&[(
        "app/Shop/PaymentResult.sharp",
        &result(
            "law chargeDoubles(int amount) => PaymentResult.charge(amount) is PaymentResult.Paid paid ? paid.amount == amount * 2 : true;",
        ),
    )]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains("error[law-unproven]: law chargeDoubles is not proven\n"),
        "{}",
        printed(&output)
    );
}

/// A type test over an interface that is not sealed is refused: its implementers are open, so a law cannot tell them apart.
#[test]
fn a_law_reaching_a_type_test_over_an_open_interface_is_refused() {
    assert_unmodeled_beside(
        "namespace App.Shared;\n\npublic class Shape : Figure\n{\n    public static bool isShape(Figure figure) => figure is Shape;\n\n    law shapeIsShape(Shape s) => Shape.isShape(s);\n}\n",
        &[("app/Shared/Figure.sharp", "namespace App.Shared;\n\npublic interface Figure\n{\n}\n")],
        "Law shapeIsShape reaches a type test of the interface Figure in Shape.isShape. A law tells apart only the implementers of a sealed interface. Declare `Figure` sealed.",
    );
}

/// `mago analyze` refuses a law that reaches a construct the law translation does not model with the issue `mago
/// compile` refuses it with, and proves no law: it writes no Lean package and no record of proofs, beside a law that
/// would prove.
#[test]
fn analyze_refuses_a_law_that_reaches_an_unmodeled_construct_as_compile_does_and_starts_no_lean() {
    let directory = library_project(&[
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\npublic class Shape : Figure\n{\n    public static bool isShape(Figure figure) => figure is Shape;\n\n    law shapeIsShape(Shape s) => Shape.isShape(s);\n    law sameValue(int value) => value == value;\n}\n",
        ),
        ("app/Shared/Figure.sharp", "namespace App.Shared;\n\npublic interface Figure\n{\n}\n"),
    ]);
    let root = directory.path();
    let refusal = "Law shapeIsShape reaches a type test of the interface Figure in Shape.isShape. A law tells apart only the implementers of a sealed interface. Declare `Figure` sealed.";

    let analyzed = process::output(
        Command::new(env!("CARGO_BIN_EXE_mago"))
            .args(["--no-version-check", "--colors", "never", "analyze"])
            .env("MAGO_REPORTING_FORMAT", "medium")
            .env("XDG_CACHE_HOME", Path::new(env!("CARGO_TARGET_TMPDIR")).join("cache"))
            .current_dir(root),
    )
    .unwrap();

    assert_eq!(analyzed.status.code(), Some(1), "{}", printed(&analyzed));
    assert!(
        printed(&analyzed).contains(&format!("app/Shared/Shape.sharp:5:60: error[unmodeled-construct]: {refusal}")),
        "{}",
        printed(&analyzed)
    );
    assert!(!root.join(".sharp").join(PACKAGE_FOLDER).exists(), "{}", printed(&analyzed));
    assert!(!root.join(".sharp/proofs.json").exists(), "{}", printed(&analyzed));

    let compiled = compile_without_lean(root);

    assert!(
        printed(&compiled).contains(&format!("app/Shared/Shape.sharp:5:60:error - unmodeled-construct: {refusal}")),
        "{}",
        printed(&compiled)
    );
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

/// Each issue line `mago compile` printed, in its order.
fn issue_lines(output: &Output) -> Vec<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(":error - "))
        .map(str::to_owned)
        .collect()
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
/// from the last thing written, never stops it, however little of a core a busy machine gives it. It spins with no
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
            process::status(Command::new("pkill").args(["-f", &script])).unwrap();
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
fn compile_refuses_an_unproven_law_whatever_ignore_entry_or_baseline_entry_targets_it() {
    let broken =
        LAWFUL_MONEY.replace("this.amount + other.amount, this.currency", "this.amount + other.amount, other.currency");
    let workspace = |analyzer: &str| {
        let directory = lawful_project(&[("app/Shared/Money.sharp", &broken)]);
        write(
            directory.path(),
            "mago.toml",
            &format!("php-version = \"8.4\"\n\n[source]\npaths = [\"app\"]\n\n[analyzer]\n{analyzer}\n"),
        );
        directory
    };
    let baselined = workspace("baseline = \"baseline.toml\"");
    write(
        baselined.path(),
        "baseline.toml",
        "variant = \"strict\"\n\n[[entries.\"app/Shared/Money.sharp\".issues]]\ncode = \"law-unproven\"\nstart_line = 8\nend_line = 8\n",
    );

    for (suppression, directory) in
        [("an ignore entry", workspace("ignore = [\"law-unproven\"]")), ("a baseline entry", baselined)]
    {
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

/// Compiles `source` as `app/Shared/Shape.sharp`, in a project that installs the standard library and so the members of
/// a `List`, and checks that the compile refuses it with the unmodeled-construct error `message`, before any Lean
/// process starts.
fn assert_unmodeled(source: &str, message: &str) {
    assert_unmodeled_beside(source, &[], message);
}

/// [`assert_unmodeled`] with the project's `others` files, each a path and its code, beside `app/Shared/Shape.sharp`.
fn assert_unmodeled_beside(source: &str, others: &[(&str, &str)], message: &str) {
    let files: Vec<(&str, &str)> =
        [("app/Shared/Shape.sharp", source)].into_iter().chain(others.iter().copied()).collect();
    let directory = library_project(&files);
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

/// A law reaches an instance method no subclass can override: a private one, and any method of an enum. A `virtual`
/// method of a class a subclass can extend is refused, since a law cannot know which implementation runs, and so is a
/// call through `Self`, even of an enum's private method.
#[test]
fn a_law_reaches_a_method_only_when_no_subclass_can_override_it() {
    let directory = lawful_project(&[
        (
            "app/Auth/Level.sharp",
            "namespace App.Auth;\n\npublic enum Level\n{\n    case Low;\n    case High;\n\n    private int rank() => match (this) {\n        Level.Low => 1,\n        Level.High => 2,\n    };\n    public bool meets(Level @required) => this.rank() >= @required.rank();\n\n    law meetsItself(Level a) => a.meets(a);\n}\n",
        ),
        (
            "app/Shared/Shape.sharp",
            "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int corners { get; }) { }\n    private int sides() => this.corners + 1;\n\n    law hasSides(Shape s) => s.corners < 0 || s.sides() > 0;\n}\n",
        ),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Level.sharp").exists());
    assert!(compiled(root, "app/Shared/Shape.sharp").exists());

    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public virtual int sides() => 3;\n\n    law hasSides(Shape s) => s.sides() > 0;\n}\n",
        "Law hasSides reaches a call to the overridable method Shape.sides in Shape.hasSides. A law cannot know which implementation runs.",
    );
    assert_unmodeled(
        "namespace App.Shared;\n\npublic enum Shape\n{\n    case Low;\n    case High;\n\n    private int rank() => match (this) {\n        Shape.Low => 1,\n        Shape.High => 2,\n    };\n    public int twice() => Self.rank() * 2;\n\n    law twiceIsPositive(Shape s) => s.twice() > 0;\n}\n",
        "Law twiceIsPositive reaches a call through `Self` in Shape.twice. A law cannot know which implementation runs.",
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
        "Law sized reaches a collection in Shape.sized. Hold the values in a List, and read it with `count()`, an index, `map`, `filter`, a list pattern or a spread.",
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

/// `App.Jobs.Retry`, an enum whose `backoff()` gives each case's growing list of delays, of 0 to 9 entries,
/// with `law`.
fn retry(law: &str) -> String {
    format!(
        "namespace App.Jobs;\n\npublic enum Retry : string\n{{\n    case None = \"none\";\n    case Fixed = \"fixed\";\n    case Linear = \"linear\";\n    case Patient = \"patient\";\n    case Exponential = \"exponential\";\n    case ExponentialRamp = \"exponential-ramp\";\n\n    public List<int> backoff() => match (this) {{\n        Retry.None => [],\n        Retry.Fixed => [5, 5, 5],\n        Retry.Linear => [10, 20, 30],\n        Retry.Patient => [1, 2, 3, 5, 8, 13, 21, 34, 55],\n        Retry.Exponential => [1, 2, 4, 8, 16, 32, 64],\n        Retry.ExponentialRamp => [30, 60, 300, 900, 1800, 3600, 7200],\n    }};\n\n    {law}\n}}\n"
    )
}

/// A `List` read `x[i]` is the element at `i`, and throws `OutOfRangeException` at an index outside the list, as the
/// engine does, and a throw does not break a law. So a law proves through its own guards, and a law without them proves
/// where the read is in range, at an index the law leaves open, over every list an enum's method returns.
#[test]
fn compile_proves_a_law_over_a_list_index_read() {
    let laws = "law backoffNeverShrinks(Retry retry, int index) => index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1];\n    law rampNeverShrinks(int index) => index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1];\n    law rampStartsAtThirty(int unused) => Retry.ExponentialRamp.backoff()[0] == 30;\n    law backoffWaits(Retry retry, int index) => retry.backoff()[index] >= 1;";
    let directory = library_project(&[("app/Jobs/Retry.sharp", &retry(laws))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Jobs/Retry.sharp").exists());
}

/// A law an index read breaks, as a ramp of delays that shrinks from 30 to 20, is not proven, and the goal still to show
/// names the reads, and a `Map` read by its key is still refused.
#[test]
fn a_false_law_over_a_list_index_read_is_not_proven_and_a_map_key_read_is_refused() {
    let shrinking = retry(
        "law rampNeverShrinks(int index) => index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1];",
    )
    .replace("[30, 60, 300", "[30, 20, 300");
    let directory = library_project(&[("app/Jobs/Retry.sharp", &shrinking)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "error[law-unproven]: law rampNeverShrinks is not proven\n = still to show: index < 0 || index + 1 >= Retry.ExponentialRamp.backoff().count() || Retry.ExponentialRamp.backoff()[index] <= Retry.ExponentialRamp.backoff()[index + 1]\n"
        ),
        "{}",
        printed(&output)
    );

    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public static int limit(Map<string, int> limits) => limits[\"orders\"] ?? 0;\n\n    law limited(int unused) => Shape.limit([\"orders\": 3]) == 3;\n}\n",
        "Law limited reaches a collection in Shape.limit.",
    );
}

/// A law without a body over an enum it takes that proves case by case reports each case it does not prove on its own,
/// with the case it is: over a ramp of delays that shrinks from 30 to 20, only `Retry.ExponentialRamp`, and with
/// `Retry.Linear`'s delays shrinking from 20 to 15 too, both.
#[test]
fn a_law_without_a_body_reports_each_enum_case_it_does_not_prove() {
    let claim =
        "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";
    let ramp = retry(&format!("law backoffNeverShrinks(Retry retry, int index) => {claim};"))
        .replace("[30, 60, 300", "[30, 20, 300");
    let linear = ramp.replace("[10, 20, 30]", "[10, 20, 15]");
    for (source, cases) in
        [(ramp, vec!["Retry.ExponentialRamp"]), (linear, vec!["Retry.Linear", "Retry.ExponentialRamp"])]
    {
        let directory = library_project(&[("app/Jobs/Retry.sharp", &source)]);

        let output = compile(directory.path());

        assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
        for case in &cases {
            assert!(
                printed(&output).contains(&format!(
                    "error[law-unproven]: law backoffNeverShrinks is not proven\n = when retry is {case}, still to show: {claim}\n"
                )),
                "{}",
                printed(&output)
            );
        }
        assert_eq!(
            printed(&output).matches("law backoffNeverShrinks is not proven").count(),
            cases.len(),
            "{}",
            printed(&output)
        );
    }
}

/// A case whose proof runs out of its time budget is not proven, as any case left open is: it is refused as
/// `law-unproven`, names the case, and says the proof ran out of time. The case after it still runs, and is reported on
/// its own. The slow case's claim has 18 `||` choices for `omega` to split on, 2^18 cases in all.
#[test]
fn a_law_whose_proof_runs_out_of_time_is_not_proven_and_its_other_cases_still_run() {
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r"];
    let bits = names.map(|name| format!("({name} == 0 || {name} == 1)")).join(" && ");
    let claim = format!("slow ? !({bits}) || {} <= 18 : a < a", names.join(" + "));
    let parameters = names.map(|name| format!("int {name}")).join(", ");
    let source = format!(
        "namespace App.Shared;\n\npublic class Steps\n{{\n    law bits(bool slow, {parameters})\n        => {claim}\n    {{\n        if (slow) {{\n            return;\n        }}\n        return;\n    }}\n}}\n"
    );
    let directory = lawful_project(&[("app/Shared/Steps.sharp", &source)]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for refused in [
        format!(
            "error[law-unproven]: law bits is not proven\n = when slow, still to show: {claim}\n = the proof ran out of time before it finished\n"
        ),
        "error[law-unproven]: law bits is not proven\n = when !slow, still to show: a < a\nerror".to_owned(),
    ] {
        assert!(printed(&output).contains(&refused), "{}", printed(&output));
    }
    assert!(!printed(&output).contains("bug in the PHP# checker"), "{}", printed(&output));
}

/// A law body's `match` with an arm for every case of an enum is exhaustive, as the same `match` in a method is, and
/// splits the law's proof on the cases.
#[test]
fn compile_proves_a_law_whose_body_matches_every_case_of_an_enum() {
    assert_backoff_split_on("Retry.ExponentialRamp", "retry is Retry.ExponentialRamp");
}

/// Checks that `backoffNeverShrinks`, with a body that matches `retry` on each case and then `last`, proves, and that
/// over a ramp of delays that shrinks from 30 to 20 it is refused in the arm `when` names.
fn assert_backoff_split_on(last: &str, when: &str) {
    let claim =
        "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";
    let law = format!(
        "law backoffNeverShrinks(Retry retry, int index) => {claim}\n    {{\n        match (retry) {{\n            Retry.None => {{ return; }},\n            Retry.Fixed => {{ return; }},\n            Retry.Linear => {{ return; }},\n            Retry.Patient => {{ return; }},\n            Retry.Exponential => {{ return; }},\n            {last} => {{ return; }},\n        }}\n    }}"
    );
    for (ramp, proven) in [("[30, 60, 300", true), ("[30, 20, 300", false)] {
        let source = retry(&law).replace("[30, 60, 300", ramp);
        let directory = library_project(&[("app/Jobs/Retry.sharp", &source)]);
        let root = directory.path();

        let output = compile(root);

        assert_eq!(output.status.success(), proven, "{ramp}: {}", printed(&output));
        assert_eq!(compiled(root, "app/Jobs/Retry.sharp").exists(), proven);
        if !proven {
            assert!(
                printed(&output).contains(&format!(
                    "error[law-unproven]: law backoffNeverShrinks is not proven\n = when {when}, still to show: {claim}\n"
                )),
                "{}",
                printed(&output)
            );
        }
    }
}

/// A law body's `match` arm that joins an enum's cases with `or` is a case for each case it joins, as a `match` with an
/// arm for each case is.
#[test]
fn compile_proves_a_law_whose_body_matches_cases_joined_with_or() {
    let directory = library_project(&[("app/Jobs/Retry.sharp", &joined_retry("[30, 60, 300]"))]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Jobs/Retry.sharp").exists());
}

/// A law whose `or` arm is false for one of the cases it joins, as over a ramp of delays that shrinks from 30 to 20, is
/// not proven in that case, and the error names that case alone.
#[test]
fn a_law_whose_body_joins_cases_with_or_is_refused_in_the_case_it_does_not_prove() {
    let directory = library_project(&[("app/Jobs/Retry.sharp", &joined_retry("[30, 20, 300]"))]);

    let output = compile(directory.path());

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(&format!(
            "error[law-unproven]: law backoffNeverShrinks is not proven\n = when retry is Retry.ExponentialRamp, still to show: {JOINED_CLAIM}\n"
        )),
        "{}",
        printed(&output)
    );
    assert_eq!(printed(&output).matches("error[law-unproven]").count(), 1, "{}", printed(&output));
}

/// The claim of `joined_retry`'s law.
const JOINED_CLAIM: &str =
    "index < 0 || index + 1 >= retry.backoff().count() || retry.backoff()[index] <= retry.backoff()[index + 1]";

/// `App.Jobs.Retry`, an enum of three cases whose `ExponentialRamp` delays are `ramp`, with the law that its delays
/// never shrink, proven by a `match` whose second arm joins `Retry.Exponential` and `Retry.ExponentialRamp` with `or`.
fn joined_retry(ramp: &str) -> String {
    format!(
        "namespace App.Jobs;\n\npublic enum Retry : string\n{{\n    case None = \"none\";\n    case Exponential = \"exponential\";\n    case ExponentialRamp = \"exponential-ramp\";\n\n    public List<int> backoff() => match (this) {{\n        Retry.None => [],\n        Retry.Exponential => [1, 2, 4, 8],\n        Retry.ExponentialRamp => {ramp},\n    }};\n\n    law backoffNeverShrinks(Retry retry, int index) => {JOINED_CLAIM}\n    {{\n        match (retry) {{\n            Retry.None => {{ return; }},\n            Retry.Exponential or Retry.ExponentialRamp => {{ return; }},\n        }}\n    }}\n}}\n"
    )
}

/// Compiles `source` as `app/Shared/<class>.sharp`, in a project that installs the standard library and so the members
/// of a `List`, and checks that the compile proves every law it states with the proof
/// it creates, and that the class with `twin` added, a false law over the same code, is refused as `law-unproven`.
fn assert_proven(class: &str, source: &str, twin: &str) {
    let name = format!("app/Shared/{class}.sharp");
    let with_twin = with_twin(source, twin);
    let Some(law) = twin.strip_prefix("law ").and_then(|twin| twin.split('(').next()) else {
        panic!("{twin} is no law");
    };

    for (source, proven) in [(source, true), (with_twin.as_str(), false)] {
        let directory = library_project(&[(&name, source)]);
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

/// `source` with `twin`, a law, stated last in its class.
fn with_twin(source: &str, twin: &str) -> String {
    let Some(end) = source.rfind('}') else {
        panic!("{source} has no closing brace");
    };

    format!("{}    {twin}\n{}", &source[..end], &source[end..])
}

/// Spec section 28.1's `total`: a method that calls itself on a list's rest from a list pattern gets smaller on each call.
#[test]
fn compile_proves_a_law_over_recursion_on_a_list_rest() {
    assert_proven(
        "Money",
        "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n\n    public static int total(List<Money> items) => match (items) {\n        [] => 0,\n        [Money first, ...List<Money> rest] => first.amount + Money.total(rest),\n    };\n\n    law totalOfTwo(int unused) => Money.total([new Money(1, \"EUR\"), new Money(2, \"EUR\")]) == 3;\n}\n",
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
        "namespace App.Shared;\n\npublic class Basket\n{\n    public Basket(public int amount { get; }) { }\n\n    public static int size(List<Basket> items) => match (items) {\n        [] => 0,\n        [Basket, ...List<Basket> rest] => 1 + Basket.size(rest),\n    };\n\n    public static int firstAmount(List<Basket> items) => match (items) {\n        [Basket first, ...] => first.amount,\n        default => 0,\n    };\n\n    public static int lastAmount(List<Basket> items) => match (items) {\n        [..., Basket last] => last.amount,\n        default => 0,\n    };\n\n    public static int backwards(List<Basket> items) => match (items) {\n        [...List<Basket> before, Basket last] => last.amount + Basket.backwards(before),\n        default => 0,\n    };\n\n    law sizeOfTwo(int unused) => Basket.size([new Basket(1), new Basket(2)]) == 2;\n    law firstOfTwo(int unused) => Basket.firstAmount([new Basket(1), new Basket(2)]) == 1;\n    law lastOfTwo(int unused) => Basket.lastAmount([new Basket(1), new Basket(2)]) == 2;\n    law backwardsOfTwo(int unused) => Basket.backwards([new Basket(1), new Basket(2)]) == 3;\n}\n",
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

/// A recursive sum over a list's rest, and a law its body proves by calling itself on that rest, as the laws design
/// writes them: each `match` handles every length of the list, so neither has `default`.
const SUMMED_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n\n    public static int total(List<Money> items) => match (items) {\n        [] => 0,\n        [Money first, ...List<Money> rest] => first.amount + Money.total(rest),\n    };\n\n    law totalJoins(List<Money> a, List<Money> b) => Money.total([...a, ...b]) == Money.total(a) + Money.total(b)\n    {\n        match (a) {\n            [] => { return; },\n            [Money, ...List<Money> rest] => { Money.totalJoins(rest, b); },\n        }\n    }\n}\n";

const TOTAL_JOINS_FIRST: &str = "law totalJoinsFirst(List<Money> a, List<Money> b) => Money.total([...a, ...b]) == Money.total(a)\n    {\n        match (a) {\n            [] => { return; },\n            [Money, ...List<Money> rest] => { Money.totalJoinsFirst(rest, b); },\n        }\n    }";

/// Three recursive sums over a list, one of two fields of each element, and a law its body proves by calling itself on
/// the list's rest.
const LINES: &str = "namespace App.Shared;\n\npublic class Line\n{\n    public Line(public int net { get; }, public int tax { get; }) { }\n\n    public static int nets(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.net + Line.nets(rest),\n        default => 0,\n    };\n\n    public static int taxes(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.tax + Line.taxes(rest),\n        default => 0,\n    };\n\n    public static int gross(List<Line> lines) => match (lines) {\n        [] => 0,\n        [Line first, ...List<Line> rest] => first.net + first.tax + Line.gross(rest),\n        default => 0,\n    };\n\n    law grossIsNetsAndTaxes(List<Line> lines) => Line.gross(lines) == Line.nets(lines) + Line.taxes(lines)\n    {\n        match (lines) {\n            [] => { return; },\n            [Line, ...List<Line> rest] => { Line.grossIsNetsAndTaxes(rest); },\n            default => { return; },\n        }\n    }\n}\n";

const GROSS_IS_NETS: &str = "law grossIsNets(List<Line> lines) => Line.gross(lines) == Line.nets(lines)\n    {\n        match (lines) {\n            [] => { return; },\n            [Line, ...List<Line> rest] => { Line.grossIsNets(rest); },\n            default => { return; },\n        }\n    }";

/// The law for the list's rest holds wherever `total` runs, since a claim computes ints exactly.
#[test]
fn compile_proves_by_recursion_that_the_sum_of_a_joined_list_is_the_sum_of_its_parts() {
    assert_proven("Money", SUMMED_MONEY, TOTAL_JOINS_FIRST);
}

/// A claim computes ints exactly, so a claim true only because its own `+` throws at `PHP_INT_MAX` is not proven.
#[test]
fn a_claim_true_only_because_its_own_arithmetic_overflows_is_not_proven() {
    let directory = lawful_project(&[(
        "app/Shared/Bound.sharp",
        "namespace App.Shared;\n\npublic class Bound\n{\n    law fits(int x) => x + 1 <= 9223372036854775807;\n}\n",
    )]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    assert!(
        printed(&output).contains(
            "app/Shared/Bound.sharp:5:5: error[law-unproven]: law fits is not proven\n = still to show: x + 1 <= 9223372036854775807\n"
        ),
        "{}",
        printed(&output)
    );
}

/// A nullable int is a value or null, so a claim compares it as code does, with no exact `Int` to read.
#[test]
fn compile_proves_a_law_that_compares_nullable_ints() {
    assert_proven(
        "Box",
        "namespace App.Shared;\n\npublic class Box\n{\n    public static int? keep(int? x) => x;\n\n    law keepKeeps(int? x) => Box.keep(x) == x;\n}\n",
        "law keepIsNull(int? x) => Box.keep(x) == null;",
    );
}

/// A sum of two fields of each element is the sum of their two sums, which the law for the list's rest proves.
#[test]
fn compile_proves_by_recursion_that_a_sum_of_two_fields_is_the_sum_of_their_sums() {
    assert_proven("Line", LINES, GROSS_IS_NETS);
}

/// A case a sum law leaves open prints its goal in PHP#: the claim's exact ints as the code writes them, each value a
/// step binds where it is used, though two steps bind theirs under one name, and an element the arm leaves unnamed by
/// its place in the list, as `lines[0]`.
#[test]
fn a_case_a_sum_law_leaves_open_names_its_goal_in_php_sharp() {
    let directory = lawful_project(&[
        ("app/Shared/Money.sharp", &with_twin(SUMMED_MONEY, TOTAL_JOINS_FIRST)),
        ("app/Shared/Line.sharp", &with_twin(LINES, GROSS_IS_NETS)),
    ]);
    let root = directory.path();

    let output = compile(root);

    assert_eq!(output.status.code(), Some(1), "{}", printed(&output));
    for expected in [
        "app/Shared/Money.sharp:19:5: error[law-unproven]: law totalJoinsFirst is not proven\n = when a is [], still to show: Money.total([...a, ...b]) == Money.total(a)\n",
        "app/Shared/Line.sharp:33:5: error[law-unproven]: law grossIsNets is not proven\n = when lines is [Line, ...List<Line> rest], still to show: lines[0].net + lines[0].tax + Line.gross(rest) == lines[0].net + Line.nets(rest)\n",
    ] {
        assert!(printed(&output).contains(expected), "{}", printed(&output));
    }
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
const SPECIMEN_MONEY: &str = "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(this.amount + other.amount, this.currency);\n    public Money times(int n) => n <= 0 ? new Money(0, this.currency) : this.add(this.times(n - 1));\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n    law timesKeepsCurrency(Money m, int n) => m.times(n).currency == m.currency\n    {\n        if (n <= 0) {\n            return;\n        }\n        Money.timesKeepsCurrency(m, n - 1);\n        Money.addKeepsCurrency(m, m.times(n - 1));\n    }\n\n    public static Money sum(Money total, List<Money> items) => match (items) {\n        [] => total,\n        [Money first, ...List<Money> rest] => Money.sum(total.add(first), rest),\n    };\n\n    law sumKeepsCurrency(Money total, List<Money> items) => Money.sum(total, items).currency == total.currency\n    {\n        match (items) {\n            [] => { return; },\n            [Money first, ...List<Money> rest] => {\n                Money.sumKeepsCurrency(total.add(first), rest);\n                Money.addKeepsCurrency(total, first);\n            },\n        }\n    }\n\n    public static int refundable(int paid, int refunded) => Math.max(paid - refunded, 0);\n\n    law neverAbovePaid(int paid, int refunded) when paid >= 0 && refunded >= 0\n        => Money.refundable(paid, refunded) <= paid;\n}\n";

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
            "namespace App.Shared;\n\npublic class Bounds\n{{\n    public static int clamp(int x, int low, int high) => Math.min(Math.max(x, low), high);\n    public static int largest(int first, List<int> rest) => Math.max(first, ...rest);\n\n    {law}\n}}\n"
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
            "app/Shared/Money.sharp:24:5: error[law-unproven]: law sumKeepsCurrency is not proven\n = when items is [Money first, ...List<Money> rest], still to show: Money.sum(total.add(first), rest).currency == total.currency\n = Help: the law for a smaller items is the call Money.sumKeepsCurrency(total, rest)\n",
        ),
        (
            without_add.as_str(),
            "app/Shared/Money.sharp:10:5: error[law-unproven]: law timesKeepsCurrency is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).currency == m.currency\n",
        ),
        (
            without_calls.as_str(),
            "app/Shared/Money.sharp:10:5: error[law-unproven]: law timesKeepsCurrency is not proven\n = when n > 0, still to show: m.add(m.times(n - 1)).currency == m.currency\n = Help: the law for a smaller n is the call Money.timesKeepsCurrency(m, n - 1)\n",
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
            "namespace App.Shared;\n\npublic class Signs\n{{\n    public static int sign(int n) => n > 5 ? 2 : (n < 0 ? 0 : 1);\n\n    law signBounded(int n) => Signs.sign(n) >= 0 && Signs.sign(n) <= 2\n    {{\n        match (n) {{\n{arms}        }}\n    }}\n    law weighs(Status s) => s.weight() > 0\n    {{\n        match (s) {{\n            Status.Open => {{ return; }},\n            Status.Paid => {{ return; }},\n        }}\n    }}\n}}\n"
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

/// Spec section 28: an `if` in a law's body that compares a value with an enum's case is a case that knows the case,
/// with the case on either side of `==`, `!=` in the `else` branch, and a local that holds a parameter, as a `match`
/// arm is. The case reads the code each call of the claim reaches, as `weight` through `level` and `rank` through
/// `meets`, as a law with no body does.
#[test]
fn a_law_body_s_if_splits_its_cases_on_an_enum_s_cases() {
    let role = "namespace App.Auth;\n\npublic enum Role\n{\n    case Owner;\n    case Admin;\n    case Member;\n\n    public int level() => Role.weight(this);\n    private static int weight(Role role) => match (role) {\n        Role.Owner => 3,\n        Role.Admin => 2,\n        Role.Member => 1,\n    };\n    public static Role fromLevel(int level) => match (level) {\n        3 => Role.Owner,\n        2 => Role.Admin,\n        default => Role.Member,\n    };\n\n    law fromLevelUndoesLevel(Role role) => Role.fromLevel(role.level()) == role\n    {\n        if (role == Role.Owner) {\n            return;\n        }\n        if (role == Role.Admin) {\n            return;\n        }\n        if (role == Role.Member) {\n            return;\n        }\n    }\n    law fromLevelUndoesLevelOfEitherSide(Role role) => Role.fromLevel(role.level()) == role\n    {\n        const same = role;\n        if (Role.Owner == same) {\n            return;\n        }\n        if (role != Role.Admin) {\n            return;\n        } else {\n            return;\n        }\n    }\n    law fromLevelUndoesLevelByMatch(Role role) => Role.fromLevel(role.level()) == role\n    {\n        match (role) {\n            Role.Owner => { return; },\n            Role.Admin => { return; },\n            Role.Member => { return; },\n        }\n    }\n}\n";
    let level = "namespace App.Auth;\n\npublic enum Level\n{\n    case Low;\n    case Mid;\n    case High;\n\n    private static int rank(Level level) => match (level) {\n        Level.Low => 1,\n        Level.Mid => 2,\n        Level.High => 3,\n    };\n    public bool meets(Level @required) => Level.rank(this) >= Level.rank(@required);\n\n    law sameWhenEachMeetsTheOther(Level a, Level b) => !(a.meets(b) && b.meets(a)) || a == b\n    {\n        if (b == Level.Low) {\n            return;\n        }\n        if (b == Level.Mid) {\n            return;\n        }\n        if (b == Level.High) {\n            return;\n        }\n    }\n}\n";
    let directory = lawful_project(&[("app/Auth/Role.sharp", role), ("app/Auth/Level.sharp", level)]);
    let root = directory.path();

    let output = compile(root);

    assert!(output.status.success(), "{}", printed(&output));
    assert!(compiled(root, "app/Auth/Role.sharp").exists());
    assert!(compiled(root, "app/Auth/Level.sharp").exists());
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
        "Law knows reaches the List method contains in Shape.known. Hold the values in a List, and read it with `count()`, an index, `map`, `filter`, a list pattern or a spread.",
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
    let source = "namespace App.Shared;\n\nextern InvalidArgumentException;\n\npublic class Shape\n{\n    public Shape(public int value { get; }) { }\n    public static bool operator ==(Shape a, Shape b) => a.value == b.value;\n    public int hash() => this.value;\n\n    public static int union(int|string x) => 1;\n    public static int power(int a) => a ** 2;\n    public static int complement(int a) => ~a;\n    public static string template(int a) => `n ${a}`;\n    public static int guard(int a) => match (a) {\n        int m when m + 1 > 2 => 1,\n        default => 0,\n    };\n    public static int defaulted(int a = 1) => a;\n    public static int named(int a) => Shape.defaulted(a: a);\n    public static int thrown(int a) => a > 0 ? a : throw new InvalidArgumentException(\"no\");\n    public static int compound(int a)\n    {\n        let x = a;\n        x **= 2;\n        return x;\n    }\n    public static int inner(int a)\n    {\n        let x = 0;\n        return (x = a) + 1;\n    }\n\n    law l1(int a) => Shape.union(a) == 1;\n    law l2(int a) => Shape.power(a) == Shape.power(a);\n    law l3(int a) => Shape.complement(a) == Shape.complement(a);\n    law l4(int a) => Shape.template(a) == Shape.template(a);\n    law l5(int a) => Shape.guard(a) == Shape.guard(a);\n    law l6(int a) => Shape.named(a) == a;\n    law l7(int a) => Shape.thrown(a) == Shape.thrown(a);\n    law l8(int a) => Shape.compound(a) == Shape.compound(a);\n    law l9(int a) => Shape.inner(a) == Shape.inner(a);\n}\n";
    let directory = lawful_project(&[("app/Shared/Shape.sharp", source)]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the type `int|string` in Shape.union. Give it a type a law reasons about: int, bool, string, a List, or a PHP# class, enum or interface, with or without `?`.",
        "Law l2 reaches the operator `**` here in Shape.power. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints, on strings and on two values of a class that declares `<=>`, `<=>` on ints and on those values, `== !=`, `! && ||` and `??`.",
        "Law l3 reaches the operator `~` in Shape.complement. Write it with the operators a law reaches: `+ - * / %` on ints, `+` on strings, `< <= > >=` on ints, on strings and on two values of a class that declares `<=>`, `<=>` on ints and on those values, `== !=`, `! && ||` and `??`.",
        "Law l4 reaches a string template in Shape.template. Join the strings with `+`.",
        "Law l5 reaches a guard that can throw in Shape.guard. Write the guard without calls or arithmetic, which can throw, and compute such a value in the arm.",
        "Law l6 reaches a named or spread argument in Shape.named. Pass each argument by its position.",
        "Law l7 reaches a throw in Shape.thrown. Return a value that tells the failure apart, such as `null` or an enum case, in place of the exception.",
        "Law l8 reaches the operator `**=` in Shape.compound. Update an int with `+=`, `-=`, `*=`, `/=` or `%=`, or a string with `+=`.",
        "Law l9 reaches an assignment inside an expression in Shape.inner. Assign the value in a statement of its own, before the expression that reads it.",
    ] {
        assert!(printed.contains(&format!("error - unmodeled-construct: {message}")), "{message}\n{printed}");
    }
    assert!(!printed.contains("A law cannot reason about it."), "{printed}");
}

/// The refusals of code a law cannot read say what to write instead, and name a type parameter as one and a function a
/// library declares in PHP's root namespace through the namespace of its classes, never as a plain PHP class.
#[test]
fn each_refusal_of_code_a_law_cannot_read_says_what_to_write_instead() {
    let clock = (
        "vendor/lib/clock/Clock.php",
        "<?php\n\nnamespace Lib;\n\nfinal class Clock\n{\n    public static function hour(): int\n    {\n        return 1;\n    }\n}\n",
    );
    let helpers = (
        "vendor/lib/clock/helpers.php",
        "<?php\n\nfunction measure(string $text): int\n{\n    return strlen($text);\n}\n",
    );
    let stub = (
        "app/Stubs/Clock.sharp",
        "namespace App.Stubs;\n\nimport Lib;\nimport Lib.Clock;\n\nextern namespace Lib for measure;\nextern Clock;\nextern Lib.measure;\n",
    );
    let shape = (
        "app/Shared/Shape.sharp",
        "namespace App.Shared;\n\nimport Lib;\nimport Lib.Clock;\n\npublic class Shape\n{\n    public Shape(public int raw { get; }) { }\n    public int doubled => this.raw * 2;\n\n    public static int hour(int a) => Clock.hour();\n    public static int length(string s) => Lib.measure(s);\n    public static int kept<T>(T item) => 1;\n\n    law l1(int a) => Shape.hour(a) == Shape.hour(a);\n    law l2(string s) => Shape.length(s) == Shape.length(s);\n    law l3(int a) => Shape.kept(a) == 1;\n    law l4(Shape s) => s.doubled == s.doubled;\n}\n",
    );
    let directory = library_project(&[shape, clock, helpers, stub]);
    let root = directory.path();

    let output = compile_without_lean(root);

    let printed = printed(&output);
    assert_eq!(output.status.code(), Some(1), "{printed}");
    for message in [
        "Law l1 reaches the plain PHP class Clock in Shape.hour. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l2 reaches the plain PHP function `Lib.measure` in Shape.length. Write the code the law reaches in PHP#, or pass the value plain PHP computes to the law as a parameter.",
        "Law l3 reaches the type parameter T in Shape.kept. Give it a type a law reasons about: int, bool, string, a List, or a PHP# class, enum or interface, with or without `?`.",
        "Law l4 reaches the accessor of Shape.doubled in Shape.l4. Compute the value in a method, and give the property no accessor body.",
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

/// PHP finds a property by its exact name, so a law knows a stored `raw` and an accessor `Raw` apart.
#[test]
fn a_law_knows_a_stored_field_from_an_accessor_whose_name_differs_only_in_case() {
    assert_unmodeled(
        "namespace App.Shared;\n\npublic class Shape\n{\n    public Shape(public int raw { get; }) { }\n    public int Raw => this.raw * 2;\n\n    law doubledHolds(Shape s) => s.Raw == s.Raw;\n}\n",
        "Law doubledHolds reaches the accessor of Shape.Raw in Shape.doubledHolds. Compute the value in a method, and give the property no accessor body.",
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
