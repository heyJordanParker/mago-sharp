//! The Lean runtime library and the `sharp-lean` runner, built once per Mago build in Mago's cache folder, and the
//! `lake` that builds them: the one on the `PATH`, else the one of the elan Mago installs in its cache folder.

use std::fmt::Write;
use std::fs;
use std::io;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::Once;
#[cfg(unix)]
use std::sync::PoisonError;
#[cfg(unix)]
use std::sync::atomic::AtomicI32;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mago_build_id::BUILD_ID;
use sha2::Digest;
use sha2::Sha256;

/// The library's Lean version, which `.sharp/.lean/lean-toolchain` pins too.
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// The Lean namespace that holds every module of the library.
pub(crate) const NAMESPACE: &str = "Sharp";

/// The library's Lake package, as `crates/sharp-lean/lean/` holds it.
const PACKAGE: [(&str, &str); 8] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Lean/Int.lean", include_str!("../lean/Sharp/Lean/Int.lean")),
    ("Sharp/Lean/Interface.lean", include_str!("../lean/Sharp/Lean/Interface.lean")),
    ("Sharp/Lean/Law.lean", include_str!("../lean/Sharp/Lean/Law.lean")),
    ("Sharp/Lean/String.lean", include_str!("../lean/Sharp/Lean/String.lean")),
    ("Sharp/Lean/Facts.lean", include_str!("../lean/Sharp/Lean/Facts.lean")),
    ("Sharp/Lean/Runner.lean", include_str!("../lean/Sharp/Lean/Runner.lean")),
];

/// The elan release Mago installs when no `lake` is on the `PATH`.
const ELAN_VERSION: &str = "4.2.4";

/// The largest elan archive Mago downloads. The pinned ones are about 5 MB.
const ELAN_LIMIT: u64 = 64 * 1024 * 1024;

/// The processor seconds each Lean process Mago starts may use: far above any real proof or rule, so that ordinary load
/// never reaches it, and a proof or rule that never ends stops instead of holding the compile.
const PROCESSOR_SECONDS: u64 = 120;

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

    /// A command that runs this `lake`. On Unix it may use `processor_seconds()`, as may each Lean process it starts,
    /// which inherits the limit.
    pub(crate) fn command(&self) -> Command {
        self.command_within(processor_seconds())
    }

    /// A command that runs this `lake`, which on Unix may use `seconds`, kept under the hard limit Mago inherits.
    pub(crate) fn command_within(&self, seconds: u64) -> Command {
        let mut command = Command::new(&self.program);
        if let Some(home) = &self.home {
            command.env("ELAN_HOME", home);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            // SAFETY: `getrlimit` and `setrlimit` are async-signal-safe, and the closure allocates nothing.
            unsafe { command.pre_exec(move || limit_processor_time(seconds)) };
        }
        #[cfg(not(unix))]
        let _ = seconds;

        command
    }
}

/// The processor seconds each Lean process may use: `PROCESSOR_SECONDS`, or the lower limit Mago itself runs under, such
/// as one `ulimit -t` set.
pub(crate) fn processor_seconds() -> u64 {
    #[cfg(unix)]
    {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: `limit` is a valid `rlimit` for `getrlimit` to fill.
        if unsafe { libc::getrlimit(libc::RLIMIT_CPU, &raw mut limit) } == 0 {
            return processor_seconds_within(&limit);
        }
    }

    PROCESSOR_SECONDS
}

/// The processor seconds each Lean process may use under the inherited processor-time `limit`: `PROCESSOR_SECONDS`, or
/// its soft limit when lower, kept a second under a finite hard limit.
#[cfg(unix)]
fn processor_seconds_within(limit: &libc::rlimit) -> u64 {
    PROCESSOR_SECONDS.min(limit.rlim_cur).min(below_hard(limit))
}

/// The highest soft limit under `limit` that a process reaches as `SIGXCPU`: a second under a finite hard limit, since
/// Linux sends `SIGKILL`, which the out-of-memory killer sends too, once the soft limit meets the hard one.
#[cfg(unix)]
fn below_hard(limit: &libc::rlimit) -> u64 {
    if limit.rlim_max == libc::RLIM_INFINITY { u64::MAX } else { limit.rlim_max.saturating_sub(1) }
}

/// Sets the soft processor-time limit of the process about to start to `seconds`, kept under its hard limit, and its
/// core-file limit to none, so a Lean stopped that way leaves no core file of gigabytes in the project.
#[cfg(unix)]
fn limit_processor_time(seconds: u64) -> io::Result<()> {
    for (resource, soft) in [(libc::RLIMIT_CPU, seconds), (libc::RLIMIT_CORE, 0)] {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        // SAFETY: `limit` is a valid `rlimit` for `getrlimit` to fill and `setrlimit` to read.
        if unsafe { libc::getrlimit(resource, &raw mut limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
        limit.rlim_cur = soft.min(below_hard(&limit));
        // SAFETY: as above.
        if unsafe { libc::setrlimit(resource, &raw const limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(())
}

/// Whether a Lean process stopped at `processor_seconds()`: `SIGXCPU` ended it, or ended the program `lake env` ran,
/// which `lake env` reports as a `stopped_code`.
pub(crate) fn stopped(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        status.signal() == Some(libc::SIGXCPU) || status.code().is_some_and(stopped_code)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

/// Whether `code` is the exit status Lean reports for a process `SIGXCPU` ended: 128 plus the signal.
pub(crate) fn stopped_code(code: i32) -> bool {
    #[cfg(unix)]
    {
        code == 128 + libc::SIGXCPU
    }
    #[cfg(not(unix))]
    {
        let _ = code;
        false
    }
}

/// How Mago ended Lean work that did not finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    /// It ran past `processor_seconds()`.
    Processor,
    /// It went `deadline()` without finishing: it waited, since work that computes reaches the processor limit first.
    Waited,
}

/// The wall-clock time Lean work may go without finishing: four times `processor_seconds()`.
pub(crate) fn deadline() -> Duration {
    Duration::from_secs(processor_seconds().saturating_mul(4))
}

/// The process group `output_before_deadline` waits on, or 0. It is not Mago's own group, so a terminal's Ctrl-C reaches
/// it only through `forward`.
#[cfg(unix)]
static WAITED_ON: AtomicI32 = AtomicI32::new(0);

/// What `WAITED_ON` holds while `command.spawn()` starts the group.
#[cfg(unix)]
const STARTING: libc::pid_t = -1;

/// The signal `forward` received, which `output_before_deadline` passes back to it once the group it started is known:
/// a signal can reach any of Mago's threads while one of them spawns.
#[cfg(unix)]
static PENDING: AtomicI32 = AtomicI32::new(0);

/// Keeps `output_before_deadline` to one group at a time, the one `WAITED_ON` holds.
#[cfg(unix)]
static ONE_GROUP: Mutex<()> = Mutex::new(());

/// Runs `command` in a process group of its own and returns how it exited and what it wrote, and whether it wrote
/// nothing for `deadline()`, so Mago ended the group. The deadline counts from the last thing it wrote, so a
/// `lake build` that reports each module it builds never reaches it however many modules it builds, while a Lean that
/// waits does. A signal that ends Mago ends the group too, as it did while the group was Mago's own.
pub(crate) fn output_before_deadline(command: &mut Command) -> io::Result<(Output, bool)> {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    let _one = {
        std::os::unix::process::CommandExt::process_group(command, 0);
        forward_signals();
        ONE_GROUP.lock().unwrap_or_else(PoisonError::into_inner)
    };
    #[cfg(unix)]
    WAITED_ON.store(STARTING, Ordering::SeqCst);
    let spawned = command.spawn();
    // `forward` writes `PENDING` before it reads `WAITED_ON`, and this thread writes `WAITED_ON` before it reads
    // `PENDING`, so at least one of them ends the group for a signal that arrived during the spawn.
    #[cfg(unix)]
    {
        let group = spawned.as_ref().ok().and_then(|child| libc::pid_t::try_from(child.id()).ok()).unwrap_or(0);
        WAITED_ON.store(group, Ordering::SeqCst);
        let signal = PENDING.load(Ordering::SeqCst);
        if signal != 0 {
            forward(signal);
        }
    }
    let mut child = spawned?;
    let output = wait_before_deadline(&mut child);
    #[cfg(unix)]
    WAITED_ON.store(0, Ordering::SeqCst);

    output
}

/// On each signal that ends Mago from a terminal or `kill`, which Mago does not ignore, ends the group `WAITED_ON`
/// holds, then ends Mago with it as before.
#[cfg(unix)]
fn forward_signals() {
    static FORWARDING: Once = Once::new();
    FORWARDING.call_once(|| {
        for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP, libc::SIGTERM] {
            // SAFETY: `sigaction` is plain data, which zeroes make valid.
            let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
            // SAFETY: `sigaction` only reads the handler of `signal` into `previous`.
            if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut previous) } != 0
                || previous.sa_sigaction != libc::SIG_DFL
            {
                continue;
            }
            // SAFETY: as for `previous`.
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            // `sigaction` takes the handler as its address.
            #[allow(clippy::fn_to_numeric_cast_any)]
            let handler = forward as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_sigaction = handler;
            action.sa_flags = libc::SA_RESETHAND;
            // SAFETY: `action` is a valid `sigaction`, and `forward` is async-signal-safe.
            unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) };
        }
    });
}

/// Ends the group `WAITED_ON` holds with `SIGKILL`, as `end_group` does, so no Lean of it dumps a core on `SIGQUIT`,
/// then raises `signal` again, which `SA_RESETHAND` made end Mago. While the group starts, it only records `signal` in
/// `PENDING`, for the spawning thread to send on.
#[cfg(unix)]
extern "C" fn forward(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
    let group = WAITED_ON.load(Ordering::SeqCst);
    if group == STARTING {
        return;
    }
    if group != 0 {
        // SAFETY: `kill` is async-signal-safe.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    // SAFETY: `raise` is async-signal-safe.
    unsafe { libc::raise(signal) };
}

/// What `child` wrote and how it exited, and whether Mago ended its group at `deadline()`.
fn wait_before_deadline(child: &mut Child) -> io::Result<(Output, bool)> {
    let missing = || io::Error::other("a Lean process has no output stream");
    let (stdout, stderr) = (child.stdout.take().ok_or_else(missing)?, child.stderr.take().ok_or_else(missing)?);
    let (wrote, written) = mpsc::channel();
    let stdout = read_all(stdout, wrote.clone());
    let stderr = read_all(stderr, wrote);
    let waited = loop {
        match written.recv_timeout(deadline()) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break false,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                #[cfg(unix)]
                end_group(child)?;
                #[cfg(not(unix))]
                child.kill()?;
                break true;
            }
        }
    };
    let status = child.wait()?;
    let joined = |reader: thread::JoinHandle<io::Result<Vec<u8>>>| {
        reader.join().map_err(|_| io::Error::other("a reader of a Lean process panicked"))?
    };

    Ok((Output { status, stdout: joined(stdout)?, stderr: joined(stderr)? }, waited))
}

/// Reads `stream` to its end on a thread of its own, and sends on `wrote` each time it reads something.
fn read_all(
    mut stream: impl Read + Send + 'static,
    wrote: mpsc::Sender<()>,
) -> thread::JoinHandle<io::Result<Vec<u8>>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Ok(bytes);
            }
            bytes.extend_from_slice(&chunk[..read]);
            let _ = wrote.send(());
        }
    })
}

/// Ends `child` and every process of its group, such as each `lean` a `lake` started.
#[cfg(unix)]
fn end_group(child: &Child) -> io::Result<()> {
    let group = libc::pid_t::try_from(child.id()).map_err(io::Error::other)?;
    // SAFETY: `killpg` only sends a signal, to the group `child` leads since it started with `process_group(0)`.
    if unsafe { libc::killpg(group, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
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
    let _lock = install_lock(cache)?;

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

/// Runs `command`, the first call to a `lake`, under the lock Mago's own installs take. elan's `lake` installs the
/// pinned Lean on its first call, with no lock of its own, so compiles that start together on a machine without that
/// Lean take turns to install it once instead of breaking it. With no such program on the `PATH`, the call installs
/// nothing and takes no lock, so it fails as not found even where Mago's cache folder can hold no lock.
pub(crate) fn first_call(command: &mut Command) -> io::Result<Output> {
    let program = Path::new(command.get_program()).with_extension(std::env::consts::EXE_EXTENSION);
    let found = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|folder| folder.join(&program).is_file()));
    let _lock = if found { Some(install_lock(&cache_root()?)?) } else { None };

    command.output()
}

/// The exclusive lock on `elan.lock` in `cache`, held across threads and processes alike until it is dropped.
fn install_lock(cache: &Path) -> io::Result<fs::File> {
    fs::create_dir_all(cache)?;
    let lock = fs::File::options().create(true).truncate(false).write(true).open(cache.join("elan.lock"))?;
    lock.lock()?;

    Ok(lock)
}

/// The SHA-256 of `bytes` in lowercase hex, as an archive's pinned `sha256` is written.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::new(), |mut digest, byte| {
        let _ = write!(digest, "{byte:02x}");
        digest
    })
}

/// Downloads elan's archive from `url`, checks its SHA-256, and runs its `elan-init` with `elan.staging/` as elan's
/// home, then renames that folder to `home`, so a dead install leaves no `home` behind.
fn install_elan(url: &str, archive: &Archive, home: &Path) -> io::Result<()> {
    let bytes = ureq::get(url)
        .header("User-Agent", concat!("mago/", env!("CARGO_PKG_VERSION")))
        .call()
        .and_then(|response| response.into_body().with_config().limit(ELAN_LIMIT).read_to_vec())
        .map_err(|error| io::Error::other(format!("downloading {url} failed: {error}")))?;
    let digest = sha256_hex(&bytes);
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
    let output = run_unpacked(
        Command::new(unpacked.path().join("elan-init"))
            .args(["-y", "--no-modify-path", "--default-toolchain", "none"])
            .env("ELAN_HOME", &staging),
    )?;
    if !output.status.success() {
        return Err(io::Error::other(format!("elan-init failed: {}", String::from_utf8_lossy(&output.stderr).trim())));
    }

    fs::rename(&staging, home)
}

/// Runs `command`, whose program was just written, and runs it again while Linux refuses it as "Text file busy", for up
/// to a second. A process another thread forks while the program is still open for writing holds that handle until it
/// starts its own program, and Linux refuses to start a file open for writing.
fn run_unpacked(command: &mut Command) -> io::Result<Output> {
    for _ in 0..50 {
        match command.output() {
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(Duration::from_millis(20))
            }
            output => return output,
        }
    }

    command.output()
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
#[allow(clippy::panic_in_result_fn)]
mod tests {
    use std::io::BufRead;
    use std::io::BufReader;
    use std::io::Write as _;
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    /// Each process a `Lake` starts, `lake` itself or anything run in its place, runs under the processor-time limit,
    /// which the Lean processes it starts inherit.
    #[test]
    fn each_process_a_lake_starts_runs_under_the_processor_time_limit() -> io::Result<()> {
        let lake = Lake { program: PathBuf::from("/bin/sh"), home: None };

        let output = lake.command().args(["-c", "ulimit -t; ulimit -c"]).output()?;
        let within = lake.command_within(360).args(["-c", "ulimit -t"]).output()?;

        assert_eq!(String::from_utf8_lossy(&output.stdout), "120\n0\n");
        assert_eq!(String::from_utf8_lossy(&within.stdout), "360\n");

        Ok(())
    }

    /// Linux ends a process whose soft limit meets its hard limit with `SIGKILL`, which Mago can't tell from the
    /// out-of-memory killer's, so the soft limit stays a second under a finite hard limit, as a `ulimit -t 60` sets.
    #[test]
    fn the_processor_time_limit_stays_a_second_under_a_finite_hard_limit() {
        let limit = |rlim_cur, rlim_max| libc::rlimit { rlim_cur, rlim_max };

        assert_eq!(processor_seconds_within(&limit(60, 60)), 59);
        assert_eq!(processor_seconds_within(&limit(10, libc::RLIM_INFINITY)), 10);
        assert_eq!(processor_seconds_within(&limit(libc::RLIM_INFINITY, 200)), 120);
        assert_eq!(processor_seconds_within(&limit(libc::RLIM_INFINITY, libc::RLIM_INFINITY)), 120);
    }

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
    fn serve_elan() -> io::Result<(String, Vec<u8>, Arc<AtomicUsize>)> {
        let mut tarball = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(ELAN_INIT.len() as u64);
        header.set_mode(0o755);
        tarball.append_data(&mut header, "elan-init", ELAN_INIT.as_bytes())?;
        let bytes = tarball.into_inner()?.finish()?;

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/elan.tar.gz", listener.local_addr()?);
        let requests = Arc::new(AtomicUsize::new(0));
        let (served, answered) = (bytes.clone(), Arc::clone(&requests));
        // A request the server fails to answer fails the install that sent it.
        std::thread::spawn(move || -> io::Result<()> {
            for stream in listener.incoming() {
                let mut stream = stream?;
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line)? > 2 {
                    line.clear();
                }
                answered.fetch_add(1, Ordering::SeqCst);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len())?;
                stream.write_all(&served)?;
            }

            Ok(())
        });

        Ok((url, bytes, requests))
    }

    fn archive(sha256: &'static str) -> Archive {
        Archive { target: "test", sha256, size: "1 KB", lean_size: "1 MB" }
    }

    fn sha256(bytes: &[u8]) -> &'static str {
        Box::leak(sha256_hex(bytes).into_boxed_str())
    }

    fn installed(cache: &Path) -> io::Result<String> {
        fs::read_to_string(cache.join("elan/installed"))
    }

    #[test]
    fn installs_that_start_together_download_elan_and_install_lean_once() -> io::Result<()> {
        let (url, bytes, requests) = serve_elan()?;
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir()?;

        // Every install starts before any is joined, so the four run together.
        let lakes = std::thread::scope(|scope| {
            #[allow(clippy::needless_collect)]
            let installs: Vec<_> =
                std::iter::repeat_with(|| scope.spawn(|| install_from(&url, &archive, cache.path()))).take(4).collect();
            installs
                .into_iter()
                .map(|install| install.join().map_err(|_| io::Error::other("an install panicked"))?)
                .collect::<io::Result<Vec<Lake>>>()
        })?;

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(installed(cache.path())?, format!("{}\n", TOOLCHAIN.trim()));
        for lake in lakes {
            assert_eq!(lake.program, cache.path().join("elan/bin/lake"));
            assert_eq!(lake.home.as_deref(), Some(cache.path().join("elan").as_path()));
        }
        assert!(!cache.path().join("elan.staging").exists());

        Ok(())
    }

    #[test]
    fn an_install_whose_lean_is_missing_installs_only_lean() -> io::Result<()> {
        let (url, bytes, requests) = serve_elan()?;
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir()?;
        install_from(&url, &archive, cache.path())?;
        fs::remove_file(cache.path().join("elan/installed"))?;

        install_from(&url, &archive, cache.path())?;

        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(installed(cache.path())?, format!("{}\n", TOOLCHAIN.trim()));

        Ok(())
    }

    #[test]
    fn an_install_of_lean_deletes_the_download_a_killed_install_left() -> io::Result<()> {
        let (url, bytes, _) = serve_elan()?;
        let archive = archive(sha256(&bytes));
        let cache = tempfile::tempdir()?;
        install_from(&url, &archive, cache.path())?;
        fs::remove_file(cache.path().join("elan/installed"))?;
        fs::create_dir_all(cache.path().join("elan/tmp"))?;
        fs::write(cache.path().join("elan/tmp/jpvym4et00p_dz4p_file"), "half of Lean")?;

        install_from(&url, &archive, cache.path())?;

        assert!(!cache.path().join("elan/tmp").exists());
        assert_eq!(installed(cache.path())?, format!("{}\n", TOOLCHAIN.trim()));

        Ok(())
    }

    #[test]
    fn a_failed_install_leaves_no_elan_and_the_next_install_downloads_it_again() -> io::Result<()> {
        let (url, bytes, requests) = serve_elan()?;
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
            let cache = tempfile::tempdir()?;
            if broken_init {
                fs::write(cache.path().join("broken-init"), "")?;
            }
            let before = requests.load(Ordering::SeqCst);

            let failed = install_from(&url, &archive(pinned), cache.path()).err().map(|failed| failed.to_string());
            assert_eq!(failed, Some(error), "{failure}");
            assert!(!cache.path().join("elan").exists(), "{failure}");

            if broken_init {
                fs::remove_file(cache.path().join("broken-init"))?;
            }
            install_from(&url, &archive(digest), cache.path())?;
            assert_eq!(requests.load(Ordering::SeqCst) - before, 2, "{failure}");
            assert_eq!(installed(cache.path())?, format!("{}\n", TOOLCHAIN.trim()), "{failure}");
        }

        Ok(())
    }
}
