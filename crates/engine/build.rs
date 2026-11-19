#![allow(clippy::expect_used)]

use std::path::Path;
use std::path::PathBuf;

/// Compiles the C shim against the installed engine's headers and links it with the engine's archives. A build that
/// never links, as `cargo check` and `cargo clippy` are, compiles with no engine installed: the link directives name the
/// archives either way, so only a link of mago needs them, and it fails naming the archive it can't find.
fn main() {
    let engine = engine_folder();
    let include = engine.join("include/php");
    let library = engine.join("lib");
    let missing: Vec<&str> =
        ["libphp.a", "libsharp.a", "system-libs"].into_iter().filter(|archive| !library.join(archive).is_file()).collect();

    println!("cargo:rerun-if-env-changed=MAGO_ENGINE");
    println!("cargo:rerun-if-changed=src/engine.c");
    for archive in ["libphp.a", "libsharp.a", "system-libs"] {
        println!("cargo:rerun-if-changed={}", library.join(archive).display());
    }
    println!("cargo:rustc-link-search=native={}", library.display());
    if missing.is_empty() {
        cc::Build::new()
            .file("src/engine.c")
            .includes(["", "main", "Zend", "TSRM", "ext"].map(|folder| include.join(folder)))
            .define("ZEND_ENABLE_STATIC_TSRMLS_CACHE", "1")
            .warnings_into_errors(true)
            .compile("mago_engine");
    } else {
        println!(
            "cargo:warning={} holds no {} of the PHP# engine, so a build that links mago fails: sharp/bin/build-mago --engine builds it",
            library.display(),
            missing.join(" or ")
        );
        println!("cargo:rustc-link-lib=static=mago_engine");
    }

    println!("cargo:rustc-link-lib=static=php");
    println!("cargo:rustc-link-lib=static=sharp");
    // libphp.a holds every library the engine's extensions need, and system-libs names those the system provides.
    if let Ok(system) = std::fs::read_to_string(library.join("system-libs")) {
        for name in system.lines().filter(|line| !line.is_empty()) {
            println!("cargo:rustc-link-lib={name}");
        }
    }
}

/// The installed engine `MAGO_ENGINE` names, or else the one `sharp/bin/build-mago` installs for this platform:
/// sharp/build/<os>-<arch>-engine/install, named as `uname -s` and `uname -m` name them.
fn engine_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("MAGO_ENGINE") {
        return PathBuf::from(folder);
    }

    let os = std::env::var("CARGO_CFG_TARGET_OS").expect("cargo sets CARGO_CFG_TARGET_OS");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").expect("cargo sets CARGO_CFG_TARGET_ARCH");
    let platform = match (os.as_str(), arch.as_str()) {
        ("macos", "aarch64") => "darwin-arm64".to_owned(),
        ("macos", arch) => format!("darwin-{arch}"),
        (os, arch) => format!("{os}-{arch}"),
    };
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");

    Path::new(&manifest).join("../../../build").join(format!("{platform}-engine")).join("install")
}
