#![allow(clippy::expect_used)]

use std::process::Command;

include!("src/digest.rs");

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let root = Path::new(&manifest).join("../..");
    for input in ["crates", "src", "Cargo.lock", "Cargo.toml", "build.rs"] {
        println!("cargo:rerun-if-changed={}", root.join(input).display());
    }

    let rustc = std::env::var("RUSTC").expect("cargo sets RUSTC");
    let version = Command::new(&rustc).arg("-vV").output().expect("rustc runs");
    let toolchain = format!(
        "{}\n{}\n{}",
        String::from_utf8_lossy(&version.stdout),
        std::env::var("PROFILE").expect("cargo sets PROFILE"),
        std::env::var("TARGET").expect("cargo sets TARGET"),
    );

    let output = Path::new(&std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("build_id.rs");
    std::fs::write(output, format!("{:#034x}", digest(&root, &toolchain))).expect("OUT_DIR is writable");
}
