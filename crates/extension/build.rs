#![allow(clippy::expect_used)]
#![allow(clippy::use_debug, reason = "Debug formatting writes each string as a Rust literal of the generated source")]

use std::fmt::Write as _;
use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;
use std::path::Path;
use std::path::PathBuf;

/// Embeds the extension SDK, composer/src/Sdk, into the crate, so mago gives each interpreter its own SDK before a
/// host's file runs, and a preload script that loads every class of it. The SDK's files go by their path under its
/// folder, and a hash of them names the folder mago writes them to.
fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let sdk = Path::new(&manifest).join("../../composer/src/Sdk");
    println!("cargo:rerun-if-changed={}", sdk.display());

    let mut files = Vec::new();
    collect(&sdk, &mut files);
    files.sort();

    let mut hasher = DefaultHasher::new();
    let mut source = String::from("/// The SDK's files, each by its path under the folder mago writes them to.\n");
    source.push_str("const SDK_FILES: &[(&str, &[u8])] = &[\n");
    let mut classes = String::new();
    for file in &files {
        let relative = file.strip_prefix(&sdk).expect("each file is under the SDK's folder");
        let path = format!("Sdk/{}", relative.to_string_lossy().replace('\\', "/"));
        let contents = std::fs::read(file).expect("the SDK's files read");
        path.hash(&mut hasher);
        hasher.write(&contents);
        let file = file.canonicalize().expect("the SDK's files have a canonical path");
        writeln!(source, "    ({path:?}, include_bytes!({:?})),", file.display().to_string()).expect("a string grows");
        let class = format!("Mago\\{}", path.trim_end_matches(".php").replace('/', "\\"));
        writeln!(classes, "    {class:?},").expect("a string grows");
    }
    source.push_str("];\n\n");

    let preload = format!(
        "<?php\n\ndeclare(strict_types=1);\n\n// The standard streams a php process gives a host's file, which may write its log to STDERR, as the CLI\n// defines them.\nif (!defined('STDIN')) {{\n    define('STDIN', fopen('php://stdin', 'rb'));\n    define('STDOUT', fopen('php://stdout', 'wb'));\n    define('STDERR', fopen('php://stderr', 'wb'));\n}}\n\n// mago's own extension SDK, which it loads into each interpreter before a host's file runs, so the\n// copy in the host's vendor folder never loads.\nspl_autoload_register(\n    static function (string $class): void {{\n        $file = __DIR__ . '/' . strtr(substr($class, 5), '\\\\', '/') . '.php';\n        if (str_starts_with($class, 'Mago\\\\Sdk\\\\') && is_file($file)) {{\n            require $file;\n        }}\n    }},\n    true,\n    true,\n);\n\nforeach ([\n{classes}] as $class) {{\n    class_exists($class) || interface_exists($class) || trait_exists($class) || enum_exists($class);\n}}\n"
    );
    preload.hash(&mut hasher);
    let hash = format!("{:016x}", hasher.finish());
    writeln!(source, "/// The script that loads every class of the SDK.\nconst SDK_PRELOAD: &str = {preload:?};\n")
        .expect("a string grows");
    writeln!(
        source,
        "/// A hash of the SDK, which names the folder mago writes it to.\nconst SDK_HASH: u64 = 0x{}_{}_{}_{};",
        &hash[..4],
        &hash[4..8],
        &hash[8..12],
        &hash[12..]
    )
    .expect("a string grows");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    std::fs::write(out.join("sdk.rs"), source).expect("the SDK's index writes");
}

/// Every `.php` file under `folder`.
fn collect(folder: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(folder).expect("the SDK's folders read") {
        let path = entry.expect("the SDK's folder entries read").path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "php") {
            files.push(path);
        }
    }
}
