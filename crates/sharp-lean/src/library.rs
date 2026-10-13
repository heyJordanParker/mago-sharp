//! The Lean runtime library and the `sharp-lean` runner, built once per Mago build in Mago's cache folder.

use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use mago_build_id::BUILD_ID;

/// The library's Lean version, which `.sharp/.lean/lean-toolchain` pins too.
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// The library's Lake package, as `crates/sharp-lean/lean/` holds it.
const PACKAGE: [(&str, &str); 6] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Lean/Int.lean", include_str!("../lean/Sharp/Lean/Int.lean")),
    ("Sharp/Lean/Law.lean", include_str!("../lean/Sharp/Lean/Law.lean")),
    ("Sharp/Lean/String.lean", include_str!("../lean/Sharp/Lean/String.lean")),
    ("Sharp/Lean/Runner.lean", include_str!("../lean/Sharp/Lean/Runner.lean")),
];

/// The runner executable in the library's build.
pub(crate) fn runner(library: &Path) -> PathBuf {
    library.join(".lake/build/bin").join(format!("sharp-lean{}", std::env::consts::EXE_SUFFIX))
}

/// Whether `library` holds the runner and every package file with the bytes Mago embeds.
fn is_built(library: &Path) -> bool {
    runner(library).is_file()
        && PACKAGE
            .iter()
            .all(|(path, source)| fs::read(library.join(path)).is_ok_and(|bytes| bytes == source.as_bytes()))
}

/// Writes the library's package to `lean/<build id>.staging` in Mago's cache folder, builds it with Lake and renames it
/// to `lean/<build id>`, once per build ID. Builds take turns under an exclusive lock on `lean/build.lock`, across
/// threads and processes alike. A build that lost its runner or a package file, as a cache cleaner leaves it, is
/// deleted and built again. A finished build deletes every other folder in `lean/`: older builds and any staging
/// folder a dead build left.
pub(crate) fn build() -> io::Result<PathBuf> {
    let builds = cache_root()?.join("lean");
    let library = builds.join(format!("{BUILD_ID:032x}"));
    fs::create_dir_all(&builds)?;
    let lock = fs::File::options().create(true).truncate(false).write(true).open(builds.join("build.lock"))?;
    lock.lock()?;
    if is_built(&library) {
        return Ok(library);
    }

    let staging = library.with_extension("staging");
    for folder in [&library, &staging] {
        match fs::remove_dir_all(folder) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    for (path, source) in PACKAGE {
        let target = staging.join(path);
        fs::create_dir_all(target.parent().unwrap_or(&staging))?;
        fs::write(target, source)?;
    }

    let output = Command::new("lake").arg("build").current_dir(&staging).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "Lake could not build the Lean runtime library in {}:\n{}{}",
            staging.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    fs::rename(&staging, &library).map_err(|error| {
        io::Error::other(format!(
            "Mago could not move the Lean runtime library from {} to {}: {error}",
            staging.display(),
            library.display()
        ))
    })?;

    for entry in fs::read_dir(&builds)? {
        let old = entry?.path();
        if old.is_dir() && old != library {
            fs::remove_dir_all(old)?;
        }
    }

    Ok(library)
}

/// Mago's cache folder: `$XDG_CACHE_HOME/mago`, else `~/Library/Caches/mago` on macOS, `%LOCALAPPDATA%\mago` on
/// Windows and `~/.cache/mago` elsewhere.
fn cache_root() -> io::Result<PathBuf> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|cache| !cache.is_empty()) {
        return Ok(PathBuf::from(cache).join("mago"));
    }
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|local| !local.is_empty())
    {
        return Ok(PathBuf::from(local).join("mago"));
    }

    let home = std::env::home_dir().ok_or_else(|| io::Error::other("Mago's cache folder needs a home folder"))?;

    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/mago") } else { home.join(".cache/mago") })
}
