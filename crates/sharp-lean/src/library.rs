//! The Lean runtime library and the `sharp-lean` runner, built once per Mago build in Mago's cache folder, and the
//! `lake` that builds them: the one on the `PATH`, else the one of the elan Mago installs in its cache folder.

use std::fmt::Write;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use mago_build_id::BUILD_ID;
use sha2::Digest;
use sha2::Sha256;

/// The library's Lean version, which `.sharp/.lean/lean-toolchain` pins too.
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// The Lean namespace that holds every module of the library.
pub(crate) const NAMESPACE: &str = "Sharp";

/// The library's Lake package, as `crates/sharp-lean/lean/` holds it.
const PACKAGE: [(&str, &str); 7] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Lean/Int.lean", include_str!("../lean/Sharp/Lean/Int.lean")),
    ("Sharp/Lean/Law.lean", include_str!("../lean/Sharp/Lean/Law.lean")),
    ("Sharp/Lean/String.lean", include_str!("../lean/Sharp/Lean/String.lean")),
    ("Sharp/Lean/Facts.lean", include_str!("../lean/Sharp/Lean/Facts.lean")),
    ("Sharp/Lean/Runner.lean", include_str!("../lean/Sharp/Lean/Runner.lean")),
];

/// The elan release Mago installs when no `lake` is on the `PATH`.
const ELAN_VERSION: &str = "4.2.4";

/// The largest elan archive Mago downloads. The pinned ones are about 5 MB.
const ELAN_LIMIT: u64 = 64 * 1024 * 1024;

/// elan's release archive for one platform, and the size of the Lean archive elan then downloads there.
struct Archive {
    target: &'static str,
    sha256: &'static str,
    size: &'static str,
    lean_size: &'static str,
}

/// elan's archive for the platform Mago is built for, as `gh release view v4.2.4 --repo leanprover/elan` lists it, or
/// `None` where elan ships no build.
const ARCHIVE: Option<Archive> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some(Archive {
        target: "aarch64-apple-darwin",
        sha256: "7ad829861392c718dfebde3a83b5c8508df47be02af68894b094b0b3952616e5",
        size: "2.2 MB",
        lean_size: "562 MB",
    })
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some(Archive {
        target: "x86_64-apple-darwin",
        sha256: "8a340b309d8ed2e96f930761fa223b3af57a38f5d253b53ac90293c9516f8cd4",
        size: "2.3 MB",
        lean_size: "564 MB",
    })
} else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64")) {
    Some(Archive {
        target: "x86_64-unknown-linux-gnu",
        sha256: "42b94d4244e8353142c456ec0e4ca6528fd898a6c604d4059f494e706e431f63",
        size: "5.0 MB",
        lean_size: "580 MB",
    })
} else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64")) {
    Some(Archive {
        target: "aarch64-unknown-linux-gnu",
        sha256: "05febd124d84ebf994b2e7479922a5650b1e950c17ae3bd1ddd776b65bb72bf9",
        size: "5.1 MB",
        lean_size: "582 MB",
    })
} else {
    None
};

/// The `lake` Mago runs.
#[derive(Debug)]
pub(crate) struct Lake {
    program: PathBuf,
    /// The `ELAN_HOME` of the elan Mago installed, or `None` for the `lake` on the `PATH`, which keeps its own.
    home: Option<PathBuf>,
}

impl Lake {
    /// The `lake` on the `PATH`.
    pub(crate) fn on_path() -> Self {
        Self { program: PathBuf::from("lake"), home: None }
    }

    /// A command that runs this `lake`.
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(home) = &self.home {
            command.env("ELAN_HOME", home);
        }

        command
    }
}

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

/// Writes the library's package to `lean/<build id>.staging` in Mago's cache folder, builds it with `lake` and renames
/// it to `lean/<build id>`, once per build ID. Builds take turns under an exclusive lock on `lean/build.lock`, across
/// threads and processes alike. A build that lost its runner or a package file, as a cache cleaner leaves it, is
/// deleted and built again. A finished build deletes every other folder in `lean/`: older builds and any staging
/// folder a dead build left.
pub(crate) fn build(lake: &Lake) -> io::Result<PathBuf> {
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
        remove_folder(folder)?;
    }
    for (path, source) in PACKAGE {
        let target = staging.join(path);
        fs::create_dir_all(target.parent().unwrap_or(&staging))?;
        fs::write(target, source)?;
    }

    let output = lake.command().arg("build").current_dir(&staging).output()?;
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

/// Installs the pinned elan into `elan/` in Mago's cache folder, with the library's Lean version, and returns its
/// `lake`. `None` where elan ships no build for the platform Mago is built for. An error names the step that failed.
pub(crate) fn install() -> io::Result<Option<Lake>> {
    let Some(archive) = &ARCHIVE else {
        return Ok(None);
    };
    let url =
        format!("https://github.com/leanprover/elan/releases/download/v{ELAN_VERSION}/elan-{}.tar.gz", archive.target);
    let cache = cache_root()?;

    install_from(&url, archive, &cache).map(Some).map_err(|error| {
        io::Error::other(format!("Mago could not install Lean into {}: {error}", cache.join("elan").display()))
    })
}

/// Installs elan from the archive at `url` into `elan/` in `cache`, then the library's Lean version with it, each once.
/// Installs take turns under an exclusive lock on `elan.lock`, across threads and processes alike, and the first one
/// prints what it downloads. Each Lean install first deletes elan's `tmp/`, since elan names each download anew and so
/// never reuses or deletes the download a killed install left there.
fn install_from(url: &str, archive: &Archive, cache: &Path) -> io::Result<Lake> {
    let home = cache.join("elan");
    fs::create_dir_all(cache)?;
    let lock = fs::File::options().create(true).truncate(false).write(true).open(cache.join("elan.lock"))?;
    lock.lock()?;

    let lake = Lake { program: home.join("bin/lake"), home: Some(home.clone()) };
    let has_elan = Path::new(elan(&home).get_program()).is_file();
    if has_elan && has_toolchain(&home)? {
        return Ok(lake);
    }

    let lean = format!("Lean {} ({})", TOOLCHAIN.trim().trim_start_matches("leanprover/lean4:v"), archive.lean_size);
    let downloads = if has_elan { lean } else { format!("elan {ELAN_VERSION} ({}) and {lean}", archive.size) };
    tracing::info!("Downloading {downloads} into {}, once, because no lake is on the PATH.", home.display());
    if !has_elan {
        install_elan(url, archive, &home)?;
    }

    remove_folder(&home.join("tmp"))?;
    let output = elan(&home).args(["toolchain", "install", TOOLCHAIN.trim()]).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "elan could not install {}: {}",
            TOOLCHAIN.trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(lake)
}

/// Downloads elan's archive from `url`, checks its SHA-256, and runs its `elan-init` with `elan.staging/` as elan's
/// home, then renames that folder to `home`, so a dead install leaves no `home` behind.
fn install_elan(url: &str, archive: &Archive, home: &Path) -> io::Result<()> {
    let bytes = ureq::get(url)
        .header("User-Agent", concat!("mago/", env!("CARGO_PKG_VERSION")))
        .call()
        .and_then(|response| response.into_body().with_config().limit(ELAN_LIMIT).read_to_vec())
        .map_err(|error| io::Error::other(format!("downloading {url} failed: {error}")))?;
    let digest = Sha256::digest(&bytes).iter().fold(String::new(), |mut digest, byte| {
        let _ = write!(digest, "{byte:02x}");
        digest
    });
    if digest != archive.sha256 {
        return Err(io::Error::other(format!("{url} has the SHA-256 {digest}, not the pinned {}", archive.sha256)));
    }

    let unpacked = tempfile::tempdir_in(home.parent().unwrap_or(home))?;
    let mut tarball = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
    let mut entry = tarball
        .entries()?
        .filter_map(Result::ok)
        .find(|entry| entry.path().is_ok_and(|path| path == Path::new("elan-init")))
        .ok_or_else(|| io::Error::other(format!("{url} holds no elan-init")))?;
    entry.unpack_in(unpacked.path())?;

    let staging = home.with_extension("staging");
    for folder in [home, &staging] {
        remove_folder(folder)?;
    }
    let output = Command::new(unpacked.path().join("elan-init"))
        .args(["-y", "--no-modify-path", "--default-toolchain", "none"])
        .env("ELAN_HOME", &staging)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!("elan-init failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }

    fs::rename(&staging, home)
}

/// Whether the elan in `home` has the library's Lean version installed.
fn has_toolchain(home: &Path) -> io::Result<bool> {
    let output = elan(home).args(["toolchain", "list"]).output()?;

    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.split_whitespace().next() == Some(TOOLCHAIN.trim())))
}

/// A command that runs the elan in `home`.
fn elan(home: &Path) -> Command {
    let mut command = Command::new(home.join("bin/elan"));
    command.env("ELAN_HOME", home);

    command
}

/// Deletes `folder` and everything in it, if it exists.
fn remove_folder(folder: &Path) -> io::Result<()> {
    match fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
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

#[cfg(all(test, unix))]
mod tests {
    use std::io::BufRead;
    use std::io::BufReader;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    /// An `elan-init` that installs an `elan` whose `toolchain install` appends the toolchain to `$ELAN_HOME/installed`
    /// and whose `toolchain list` prints that file. When the cache holds `broken-init`, it fails after it installed.
    const ELAN_INIT: &str = r#"#!/bin/sh
mkdir -p "$ELAN_HOME/bin"
cat > "$ELAN_HOME/bin/elan" <<'ELAN'
#!/bin/sh
case "$2" in
    list) cat "$ELAN_HOME/installed" 2>/dev/null ;;
    install) echo "$3" >> "$ELAN_HOME/installed" ;;
esac
ELAN
chmod +x "$ELAN_HOME/bin/elan"
if [ -e "$ELAN_HOME/../broken-init" ]; then
    echo "elan-init broke" >&2
    exit 1
fi
"#;

    /// An elan archive that holds `ELAN_INIT`, served over HTTP on a local port, and how many requests it answered.
    fn serve_elan() -> (String, Vec<u8>, Arc<AtomicUsize>) {
        let mut tarball = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(ELAN_INIT.len() as u64);
        header.set_mode(0o755);
        tarball.append_data(&mut header, "elan-init", ELAN_INIT.as_bytes()).unwrap();
        let bytes = tarball.into_inner().unwrap().finish().unwrap();

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/elan.tar.gz", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let (served, answered) = (bytes.clone(), Arc::clone(&requests));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                answered.fetch_add(1, Ordering::SeqCst);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len())
                    .unwrap();
                stream.write_all(&served).unwrap();
            }
        });

        (url, bytes, requests)
    }

    fn archive(sha256: &'static str) -> Archive {
        Archive { target: "test", sha256, size: "1 KB", lean_size: "1 MB" }
    }

    fn sha256(bytes: &[u8]) -> &'static str {
        let digest = Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>();

        Box::leak(digest.into_boxed_str())
    }

    fn installed(cache: &Path) -> String {
        fs::read_to_string(cache.join("elan/installed")).unwrap()
    }

    #[test]
    fn installs_that_start_together_download_elan_and_install_lean_once() {
        let (url, bytes, requests) = serve_elan();
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir().unwrap();

        let lakes: Vec<Lake> = std::thread::scope(|scope| {
            let installs: Vec<_> =
                (0..4).map(|_| scope.spawn(|| install_from(&url, &archive, cache.path()).unwrap())).collect();
            installs.into_iter().map(|install| install.join().unwrap()).collect()
        });

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(installed(cache.path()), format!("{}\n", TOOLCHAIN.trim()));
        for lake in lakes {
            assert_eq!(lake.program, cache.path().join("elan/bin/lake"));
            assert_eq!(lake.home.as_deref(), Some(cache.path().join("elan").as_path()));
        }
        assert!(!cache.path().join("elan.staging").exists());
    }

    #[test]
    fn an_install_whose_lean_is_missing_installs_only_lean() {
        let (url, bytes, requests) = serve_elan();
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir().unwrap();
        install_from(&url, &archive, cache.path()).unwrap();
        fs::remove_file(cache.path().join("elan/installed")).unwrap();

        install_from(&url, &archive, cache.path()).unwrap();

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(installed(cache.path()), format!("{}\n", TOOLCHAIN.trim()));
    }

    #[test]
    fn an_install_of_lean_deletes_the_download_a_killed_install_left() {
        let (url, bytes, _) = serve_elan();
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir().unwrap();
        install_from(&url, &archive, cache.path()).unwrap();
        fs::remove_file(cache.path().join("elan/installed")).unwrap();
        fs::create_dir_all(cache.path().join("elan/tmp")).unwrap();
        fs::write(cache.path().join("elan/tmp/jpvym4et00p_dz4p_file"), "half of Lean").unwrap();

        install_from(&url, &archive, cache.path()).unwrap();

        assert!(!cache.path().join("elan/tmp").exists());
        assert_eq!(installed(cache.path()), format!("{}\n", TOOLCHAIN.trim()));
    }

    #[test]
    fn a_failed_install_leaves_no_elan_and_the_next_install_downloads_it_again() {
        let (url, bytes, requests) = serve_elan();
        let digest = sha256(&bytes);
        let other = "0000000000000000000000000000000000000000000000000000000000000000";
        for (failure, pinned, broken_init, error) in [
            (
                "an archive whose SHA-256 is not the pinned one",
                other,
                false,
                format!("{url} has the SHA-256 {digest}, not the pinned {other}"),
            ),
            (
                "an elan-init that fails after it installed",
                digest,
                true,
                "elan-init failed: elan-init broke".to_owned(),
            ),
        ] {
            let cache = tempfile::tempdir().unwrap();
            if broken_init {
                fs::write(cache.path().join("broken-init"), "").unwrap();
            }
            let before = requests.load(Ordering::SeqCst);

            assert_eq!(install_from(&url, &archive(pinned), cache.path()).unwrap_err().to_string(), error, "{failure}");
            assert!(!cache.path().join("elan").exists(), "{failure}");

            if broken_init {
                fs::remove_file(cache.path().join("broken-init")).unwrap();
            }
            install_from(&url, &archive(digest), cache.path()).unwrap();
            assert_eq!(requests.load(Ordering::SeqCst) - before, 2, "{failure}");
            assert_eq!(installed(cache.path()), format!("{}\n", TOOLCHAIN.trim()), "{failure}");
        }
    }
}
