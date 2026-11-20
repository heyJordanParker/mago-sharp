#![allow(clippy::expect_used)]

use std::path::PathBuf;

/// Embeds the law checker's build that `MAGO_CHECKER_ARCHIVE` names, the archive the `checker` example writes, as the
/// release build does, or an empty archive, with which Mago builds the checker with `lake` on the first compile that
/// proves a law.
fn main() {
    println!("cargo:rerun-if-env-changed=MAGO_CHECKER_ARCHIVE");
    let embedded = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR")).join("checker.tar.zst");
    match std::env::var_os("MAGO_CHECKER_ARCHIVE") {
        Some(archive) => {
            println!("cargo:rerun-if-changed={}", PathBuf::from(&archive).display());
            std::fs::copy(&archive, &embedded).expect("MAGO_CHECKER_ARCHIVE names a readable archive");
        }
        None => std::fs::write(&embedded, []).expect("cargo's OUT_DIR is writable"),
    }
}
