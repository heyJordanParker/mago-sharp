//! An identifier for the build that produced this binary.
//!
//! [`BUILD_ID`] is a hash of every workspace Rust source, every `Cargo.toml`, `Cargo.lock`, the root
//! `build.rs`, the prelude stubs and the Lean runtime library it embeds, and the rustc version,
//! profile, and target. Two binaries built from the same inputs carry the same ID, and any change
//! to one of them changes it.

/// The identifier of the build that produced this binary.
#[allow(clippy::unreadable_literal)]
pub const BUILD_ID: u128 = include!(concat!(env!("OUT_DIR"), "/build_id.rs"));

#[cfg(test)]
mod digest;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::Path;

    use super::*;

    fn write(root: &Path, name: &str, contents: &str) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "Cargo.toml", "[workspace]\n");
        write(root.path(), "Cargo.lock", "version = 4\n");
        write(root.path(), "build.rs", "fn main() {}\n");
        write(root.path(), "crates/a/Cargo.toml", "[package]\n");
        write(root.path(), "crates/a/src/lib.rs", "pub fn a() {}\n");
        write(root.path(), "crates/prelude/assets/core.php", "<?php function strlen(string $s): int {}\n");
        write(root.path(), "crates/sharp-lean/lean/lakefile.toml", "name = \"sharp\"\n");
        write(root.path(), "crates/sharp-lean/lean/Sharp/Lean/Int.lean", "namespace Sharp\n");
        write(root.path(), "src/main.rs", "fn main() {}\n");
        root
    }

    #[test]
    fn the_embedded_id_is_a_digest() {
        assert_ne!(BUILD_ID, 0);
    }

    #[test]
    fn the_digest_changes_with_every_source_manifest_and_toolchain_input() {
        let root = workspace();
        let base = digest::digest(root.path(), "rustc 1.97.0");
        assert_eq!(base, digest::digest(root.path(), "rustc 1.97.0"));
        assert_ne!(base, digest::digest(root.path(), "rustc 1.98.0"));

        for (name, contents) in [
            ("crates/a/src/lib.rs", "pub fn a() { }\n"),
            ("src/main.rs", "fn main() { }\n"),
            ("crates/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("Cargo.lock", "version = 3\n"),
            ("Cargo.toml", "[workspace]\nmembers = []\n"),
            ("build.rs", "fn main() { }\n"),
            ("crates/prelude/assets/core.php", "<?php function strlen(string $string): int {}\n"),
            ("crates/sharp-lean/lean/lakefile.toml", "name = \"runtime\"\n"),
            ("crates/sharp-lean/lean/Sharp/Lean/Int.lean", "namespace Sharp.Int\n"),
            ("crates/sharp-lean/lean/lean-toolchain", "leanprover/lean4:v4.34.1\n"),
        ] {
            let root = workspace();
            write(root.path(), name, contents);
            assert_ne!(base, digest::digest(root.path(), "rustc 1.97.0"), "{name}");
        }
    }

    #[test]
    fn the_digest_ignores_files_that_do_not_build_the_binary() {
        let root = workspace();
        let base = digest::digest(root.path(), "rustc 1.97.0");

        write(root.path(), "crates/a/README.md", "notes\n");
        write(root.path(), "crates/a/target/debug/build.rs", "fn main() {}\n");
        write(root.path(), "docs/guide.rs", "fn guide() {}\n");
        write(root.path(), "crates/a/tests/cases/fixture.php", "<?php\n");
        write(root.path(), "crates/sharp-lean/lean/.lake/build/lib/lean/Sharp/Lean/Int.olean", "built\n");

        assert_eq!(base, digest::digest(root.path(), "rustc 1.97.0"));
    }

    #[test]
    #[cfg(unix)]
    #[should_panic(expected = "broken.rs")]
    fn the_digest_names_a_source_it_cannot_read() {
        let root = workspace();
        std::os::unix::fs::symlink(root.path().join("missing.rs"), root.path().join("crates/a/src/broken.rs")).unwrap();

        let _ = digest::digest(root.path(), "rustc 1.97.0");
    }
}
