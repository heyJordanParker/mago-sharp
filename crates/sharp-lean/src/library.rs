//! The Lean runtime library and the `sharp-lean` runner, built in Mago's cache folder once per library source and Lean
//! version, and the Lean a law runs on, found one way: the Lean Mago installed in its cache folder, else elan's
//! toolchain of the pinned version, else the Lean Mago downloads into its cache folder.

use std::fmt::Write;
use std::fs;
use std::io;
use std::io::Read;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Output;
use std::process::Stdio;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use mago_database::error::DatabaseError;
use mago_database::error::IOAction;
use mago_database::lock;
use mago_database::process;
use mago_extension::ProcessGroup;
use sha2::Digest;
use sha2::Sha256;

/// The library's Lean version, which `.sharp/.lean/lean-toolchain` pins too.
pub(crate) const TOOLCHAIN: &str = include_str!("../lean/lean-toolchain");

/// The library's Lake package, as `crates/sharp-lean/lean/` holds it.
const PACKAGE: [(&str, &str); 9] = [
    ("lakefile.toml", include_str!("../lean/lakefile.toml")),
    ("lean-toolchain", TOOLCHAIN),
    ("Sharp/Lean/Int.lean", include_str!("../lean/Sharp/Lean/Int.lean")),
    ("Sharp/Lean/Interface.lean", include_str!("../lean/Sharp/Lean/Interface.lean")),
    ("Sharp/Lean/Law.lean", include_str!("../lean/Sharp/Lean/Law.lean")),
    ("Sharp/Lean/Map.lean", include_str!("../lean/Sharp/Lean/Map.lean")),
    ("Sharp/Lean/Proof.lean", include_str!("../lean/Sharp/Lean/Proof.lean")),
    ("Sharp/Lean/String.lean", include_str!("../lean/Sharp/Lean/String.lean")),
    ("Sharp/Lean/Runner.lean", include_str!("../lean/Sharp/Lean/Runner.lean")),
];

/// How far the download and the unzip of Lean's archive each read ahead of the step after them: `AHEAD` chunks of
/// `CHUNK` bytes.
const AHEAD: usize = 32;
const CHUNK: u64 = 1024 * 1024;

/// The processor seconds each `lake` process Mago starts may use: far above any real build, so that ordinary load never
/// reaches it, and a build that never ends stops instead of holding the compile. The runner has none: each theorem it
/// proves has a time budget of its own.
const PROCESSOR_SECONDS: u64 = 120;

/// The file in a library's build that names the folder of the Lean that built it, which the runner reads Lean's own
/// modules from.
const SYSROOT: &str = "sysroot";

/// The law checker's build for the platform Mago is built for, the zstd archive of its `.lake/build` that
/// [`write_checker`] writes and the release build embeds, or none in a build that embeds none, which builds the checker
/// with `lake`.
const CHECKER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/checker.tar.zst"));

/// Lean's release archive for one platform: its name, which is also the name of the one folder it holds, its SHA-256 and
/// its size in bytes.
struct Archive {
    name: &'static str,
    sha256: &'static str,
    bytes: u64,
}

/// Lean's archive for the platform Mago is built for, as `gh release view v4.34.1 --repo leanprover/lean4 --json assets`
/// lists it, or `None` where Mago installs no Lean.
const ARCHIVE: Option<Archive> = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    Some(Archive {
        name: "lean-4.34.1-darwin_aarch64",
        sha256: "65f22a4f047738ec742667b3247e836a86ebf06eabc12655c3dee00637e37866",
        bytes: 562_038_070,
    })
} else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
    Some(Archive {
        name: "lean-4.34.1-darwin",
        sha256: "c2fee70250444e46c1106a27bddfef948d1e563eef431eaac05bb946b2f73d3a",
        bytes: 564_348_102,
    })
} else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "x86_64")) {
    Some(Archive {
        name: "lean-4.34.1-linux",
        sha256: "47bf4bbd78f70c2e9670598ab7124d92b6efb7330ff33e5fbb4030f6fd72e4e4",
        bytes: 580_432_872,
    })
} else if cfg!(all(target_os = "linux", target_env = "gnu", target_arch = "aarch64")) {
    Some(Archive {
        name: "lean-4.34.1-linux_aarch64",
        sha256: "fdb974c2cdb4627e090d5d4007b913e09d13c4868720fb5594e22808b3de9e37",
        bytes: 581_946_052,
    })
} else {
    None
};

/// The `lake` of the Lean a law runs on.
#[derive(Debug)]
pub(crate) struct Lake {
    /// The Lean's folder, which holds `bin/lake` and Lean's own modules in `lib/lean`.
    lean: PathBuf,
}

impl Lake {
    /// The `lake` program in `bin` in the Lean's folder.
    fn program(&self) -> PathBuf {
        self.lean.join("bin").join(format!("lake{}", std::env::consts::EXE_SUFFIX))
    }

    /// A command that runs this `lake`. On Unix it may use `processor_seconds()`, as may each Lean process it starts,
    /// which inherits the limit.
    pub(crate) fn command(&self) -> Command {
        let mut command = Command::new(self.program());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;

            let seconds = processor_seconds();
            // SAFETY: `getrlimit` and `setrlimit` are async-signal-safe, and the closure allocates nothing.
            unsafe { command.pre_exec(move || limit_processor_time(seconds)) };
        }

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
pub(crate) fn limit_processor_time(seconds: u64) -> io::Result<()> {
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
fn stopped_code(code: i32) -> bool {
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
    /// It went `deadline()` without writing and without using processor time: it waited.
    Waited,
}

/// The wall-clock time Lean work may go without writing and without using processor time: four times
/// `processor_seconds()`.
pub(crate) fn deadline() -> Duration {
    Duration::from_secs(processor_seconds().saturating_mul(4))
}

/// Runs `command` in a process group of its own with `input` on its standard input, and returns how it exited and what
/// it wrote, and whether it wrote nothing and its group used no processor time for `deadline()`, so Mago ended the
/// group. The deadline counts from the last thing it wrote or the last processor time a process of its group used, so
/// a runner that computes never
/// reaches it however little of a core a busy machine gives it, and heartbeats alone bound its proofs, while a Lean
/// that waits does. A signal that ends Mago ends the group too, as it did while the group was Mago's own.
/// An error writing `input` counts only when the command succeeded, since one that fails before it reads its input is
/// reported by how it exited.
pub(crate) fn output_before_deadline(command: &mut Command, input: &[u8]) -> io::Result<(Output, bool)> {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut group = ProcessGroup::spawn(command)?;
    // A thread writes the input, so a command that never reads it cannot keep Mago from its deadline.
    let mut stdin =
        group.leader().stdin.take().ok_or_else(|| io::Error::other("a law checker process has no input stream"))?;
    let input = input.to_vec();
    let writer = thread::spawn(move || {
        mago_extension::process_group::block_sigpipe(&stdin);
        stdin.write_all(&input)
    });
    let output = wait_before_deadline(&mut group, deadline(), ProcessGroup::end)?;
    // Only a command that succeeded must have read its input. The writer to one that failed ends on its own, or never,
    // when a process of the group outlived its end and never reads.
    if output.0.status.success() {
        writer.join().map_err(|_| io::Error::other("a writer of a law checker process panicked"))??;
    }

    Ok(output)
}

/// What `group`'s leader wrote and how it exited, and whether `end` ended the group once its leader wrote nothing and
/// the group used no processor time for `deadline`. A process of the group that outlives the end, as one the kernel holds on a
/// stalled disk, keeps the group's pipes open, so the output is what the group wrote before the end, and a leader that
/// still runs counts as ended by SIGKILL, since waiting for it could wait forever.
fn wait_before_deadline(
    group: &mut ProcessGroup,
    deadline: Duration,
    mut end: impl FnMut(&mut ProcessGroup) -> io::Result<()>,
) -> io::Result<(Output, bool)> {
    let missing = || io::Error::other("a law checker process has no output stream");
    let leader = group.leader();
    let id = leader.id();
    let (stdout, stderr) = (leader.stdout.take().ok_or_else(missing)?, leader.stderr.take().ok_or_else(missing)?);
    let (wrote, written) = mpsc::channel();
    read_all(stdout, 0, wrote.clone());
    read_all(stderr, 1, wrote);
    let mut streams = [Vec::new(), Vec::new()];
    let mut open = streams.len();
    let mut waited = false;
    let mut used = processor_time(id);
    while open > 0 {
        match written.recv_timeout(deadline) {
            Ok((_, Ok(chunk))) if chunk.is_empty() => open -= 1,
            Ok((stream, Ok(chunk))) => {
                streams[stream].extend_from_slice(&chunk);
                used = processor_time(id);
            }
            Ok((_, Err(error))) => return Err(error),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("a reader of a law checker process panicked"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = processor_time(id);
                if now > used {
                    used = now;
                    continue;
                }
                waited = true;
                match end(group) {
                    Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                        tracing::warn!("The law checker's {error}, so Mago stops waiting for it.");
                        let status = group.leader().try_wait()?.unwrap_or_else(killed);
                        let [stdout, stderr] = streams;

                        return Ok((Output { status, stdout, stderr }, true));
                    }
                    ended => ended?,
                }
            }
        }
    }
    let status = group.leader().wait()?;
    let [stdout, stderr] = streams;

    Ok((Output { status, stdout, stderr }, waited))
}

/// The status of a process that SIGKILL ended.
fn killed() -> ExitStatus {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::from_raw(libc::SIGKILL)
    }
    #[cfg(windows)]
    {
        std::os::windows::process::ExitStatusExt::from_raw(1)
    }
}

/// The processor time the process group `group` leads has used, in its platform's own units, which only compare with
/// each other: each process of the group's own and that of the children it waited for, so the time of a process that
/// ended stays in its parent's. Mago ends the whole group at the deadline, so the group computes while any of its
/// processes does: the leader alone counted a runner's shell as waiting while the program it ran computed, and stopped
/// it at the deadline. It reads the user and system clock ticks of `/proc/<id>/stat` on Linux, and the user and system
/// time of `proc_pid_rusage` on macOS. None where it can't be read, so a group that wrote nothing for the deadline
/// counts as waiting.
fn processor_time(group: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let group = group.to_string();
        let mut used = 0;
        for entry in fs::read_dir("/proc").ok()?.flatten() {
            if !entry.file_name().as_encoded_bytes().iter().all(u8::is_ascii_digit) {
                continue;
            }
            // A process that ended since the listing has no stat.
            let Ok(stat) = fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            // The fields after the parenthesized command name, which may hold spaces: the state first, so the process
            // group is the 3rd, and the user, system, waited-for children's user and children's system times the 12th
            // to the 15th.
            let Some((_, after)) = stat.rsplit_once(')') else {
                continue;
            };
            let fields: Vec<&str> = after.split_whitespace().collect();
            if fields.get(2) == Some(&group.as_str()) {
                used += fields
                    .get(11..15)
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|field| field.parse::<u64>().ok())
                    .sum::<u64>();
            }
        }

        Some(used)
    }
    #[cfg(target_os = "macos")]
    {
        // `<sys/proc_info.h>`'s list of the processes of one process group, which libc doesn't name.
        const PROC_PGRP_ONLY: u32 = 2;
        let mut ids: Vec<libc::c_int> = vec![0; 64];
        let listed = loop {
            let size = libc::c_int::try_from(std::mem::size_of_val(ids.as_slice())).ok()?;
            // SAFETY: `ids` is a valid buffer of `size` bytes for `proc_listpids` to fill.
            let filled = unsafe { libc::proc_listpids(PROC_PGRP_ONLY, group, ids.as_mut_ptr().cast(), size) };
            let listed = usize::try_from(filled).ok()? / std::mem::size_of::<libc::c_int>();
            if listed < ids.len() {
                break listed;
            }
            ids.resize(ids.len() * 2, 0);
        };
        let mut used = 0;
        for &id in &ids[..listed] {
            // SAFETY: `rusage_info_v2` is plain data, which zeroes make valid.
            let mut usage: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
            // SAFETY: `usage` is a valid `rusage_info_v2` for `proc_pid_rusage` to fill at `RUSAGE_INFO_V2`.
            // A process that ended since the listing fails, and counts no more.
            if unsafe { libc::proc_pid_rusage(id, libc::RUSAGE_INFO_V2, (&raw mut usage).cast()) } == 0 {
                used +=
                    usage.ri_user_time + usage.ri_system_time + usage.ri_child_user_time + usage.ri_child_system_time;
            }
        }

        Some(used)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = group;
        None
    }
}

/// Reads `stream` on a thread of its own and sends on `wrote` each chunk it reads with `index`, then an empty chunk
/// at its end, or the error that ends it.
fn read_all(mut stream: impl Read + Send + 'static, index: usize, wrote: mpsc::Sender<(usize, io::Result<Vec<u8>>)>) {
    thread::spawn(move || {
        let mut chunk = [0; 8192];
        loop {
            let read = stream.read(&mut chunk).map(|read| chunk[..read].to_vec());
            let ended = !read.as_ref().is_ok_and(|read| !read.is_empty());
            let _ = wrote.send((index, read));
            if ended {
                return;
            }
        }
    });
}

/// The runner executable in the library's build.
pub(crate) fn runner(library: &Path) -> PathBuf {
    library.join(".lake/build/bin").join(format!("sharp-lean{}", std::env::consts::EXE_SUFFIX))
}

/// A command that runs the runner of `library` with the search path `lake env` gives it: the library's build and the
/// Lean that built it, which [`build_in`] records, so no `lake` starts for a compile. On Windows the runner links Lean's
/// shared libraries from that Lean's `bin` folder.
pub(crate) fn runner_command(library: &Path) -> io::Result<Command> {
    let recorded = library.join(SYSROOT);
    let sysroot =
        fs::read_to_string(&recorded).map_err(|error| DatabaseError::io(IOAction::ReadFile, &recorded, error))?;
    let sysroot = PathBuf::from(sysroot.trim());
    let mut command = Command::new(runner(library));
    command.env("LEAN_SYSROOT", &sysroot).env("LEAN_PATH", library.join(".lake/build/lib/lean"));
    if cfg!(windows) {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let folders = std::iter::once(sysroot.join("bin")).chain(std::env::split_paths(&path));
        command.env("PATH", std::env::join_paths(folders).map_err(io::Error::other)?);
    }

    Ok(command)
}

/// Records in `staging` the folder of the Lean whose `lake` built it.
fn record_sysroot(staging: &Path, lake: &Lake) -> io::Result<()> {
    let recorded = staging.join(SYSROOT);

    fs::write(&recorded, lake.lean.as_os_str().as_encoded_bytes())
        .map_err(|error| DatabaseError::io(IOAction::WriteFile, &recorded, error).into())
}

/// Whether `library` holds the runner and every package file with the bytes Mago embeds, and names the folder of the
/// Lean that built it, which still holds Lean's modules.
fn is_built(library: &Path) -> bool {
    runner(library).is_file()
        && fs::read_to_string(library.join(SYSROOT))
            .is_ok_and(|sysroot| Path::new(sysroot.trim()).join("lib/lean").is_dir())
        && PACKAGE
            .iter()
            .all(|(path, source)| fs::read(library.join(path)).is_ok_and(|bytes| bytes == source.as_bytes()))
}

/// The error of a build of the law checker that failed for `failure`, the step that failed and its cause.
fn unbuilt(failure: impl std::fmt::Display) -> io::Error {
    io::Error::other(format!("the law checker could not be built in Mago's cache folder: {failure}"))
}

/// The lines of `printed` that state an error, as `lake build` starts each with `error`, or all of `printed` when none
/// does.
fn error_lines(printed: &str) -> String {
    let errors: Vec<&str> = printed.lines().filter(|line| line.trim_start().starts_with("error")).collect();

    if errors.is_empty() { printed.trim_end().to_owned() } else { errors.join("\n") }
}

/// The name of the library's build: a hash of each file of `package`, the whole of what the build reads, and of
/// `checker`, the build Mago embeds, when it embeds one. Its `lean-toolchain` pins the Lean version, so a build of
/// another Lean or of other sources has another name, and a Mago that embeds its checker runs no other.
fn key(package: &[(&str, &str)], checker: &[u8]) -> String {
    let mut hasher = Sha256::new();
    for (path, source) in package {
        hasher.update(format!("{path}\0{}\0", source.len()));
        hasher.update(source);
    }
    hasher.update(checker);

    sha256_hex(&hasher.finalize())[..32].to_owned()
}

/// The hash of each file of the library's package, its `lean-toolchain` among them: all of the library a law's proof
/// reads, and so all a law's key needs of it.
pub(crate) fn sources() -> String {
    key(&PACKAGE, &[])
}

/// What `output`'s process wrote to its standard output and its standard error.
fn printed(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// The library's build in `lean/` in Mago's cache folder, as `built_in` finds it, so the compile runs it with no
/// `lake`. `None` sends the compile to `build`, which reports what failed when Mago's cache folder can't be named.
pub(crate) fn built() -> Option<PathBuf> {
    built_in(&cache_root().ok()?.join("lean"), CHECKER)
}

/// The library's build in `builds` when it is built, from `checker` when it holds a build. A build moves into place
/// whole, so a build found here is never half written.
fn built_in(builds: &Path, checker: &[u8]) -> Option<PathBuf> {
    let library = builds.join(key(&PACKAGE, checker));

    is_built(&library).then_some(library)
}

/// Builds the library in `lean/` in Mago's cache folder, from the build Mago embeds when it embeds one, as `build_in`
/// does.
pub(crate) fn build(lake: &Lake) -> io::Result<PathBuf> {
    build_in(&cache_root()?.join("lean"), lake, CHECKER)
}

/// Writes the library's package to `<key>.staging` in `builds`, builds it with `lake`, or unpacks `checker` there when
/// it holds a build, and renames it to `<key>`, once per library source and the Lean version it pins, and per
/// `checker`, which `key` names. Builds take turns under an exclusive lock on `build.lock` in `builds`, across
/// threads and processes alike. A build that lost its runner or a package file, as a
/// cache cleaner leaves it, or whose Lean is gone, as an uninstalled toolchain leaves it, is deleted and built again.
/// A finished build deletes every other folder in `builds` but the Lean Mago installs there: older builds, any staging
/// folder a dead build left, and the Lean an older Mago installed. A build records the folder of the Lean that built
/// it, which the runner starts with.
pub(crate) fn build_in(builds: &Path, lake: &Lake, checker: &[u8]) -> io::Result<PathBuf> {
    let _lock = lock::exclusive(&builds.join("build.lock"))?;
    if let Some(library) = built_in(builds, checker) {
        return Ok(library);
    }

    let library = builds.join(key(&PACKAGE, checker));
    let staging = library.with_extension("staging");
    for folder in [&library, &staging] {
        remove_folder(folder)?;
    }
    for (path, source) in PACKAGE {
        let target = staging.join(path);
        let folder = target.parent().unwrap_or(&staging);
        fs::create_dir_all(folder).map_err(|error| DatabaseError::io(IOAction::CreateFolder, folder, error))?;
        fs::write(&target, source).map_err(|error| DatabaseError::io(IOAction::WriteFile, &target, error))?;
    }

    if checker.is_empty() {
        let output = process::output(lake.command().arg("build").current_dir(&staging))?;
        if !output.status.success() {
            return Err(unbuilt(format!("lake build printed:\n{}", error_lines(&printed(&output)))));
        }
    } else {
        tar::Archive::new(zstd::Decoder::new(checker)?).unpack(&staging).map_err(unbuilt)?;
    }

    record_sysroot(&staging, lake)?;
    fs::rename(&staging, &library).map_err(unbuilt)?;

    let lean = ARCHIVE.as_ref().map(|archive| builds.join(archive.name));
    for entry in fs::read_dir(builds).map_err(|error| DatabaseError::io(IOAction::OpenFolder, builds, error))? {
        let old = entry.map_err(|error| DatabaseError::io(IOAction::WalkFolder, builds, error))?.path();
        if old.is_dir() && old != library && Some(&old) != lean.as_ref() {
            fs::remove_dir_all(&old).map_err(|error| DatabaseError::io(IOAction::DeleteFolder, &old, error))?;
        }
    }

    Ok(library)
}

/// Builds the law checker with the Lean a law runs on, in a folder of its own, and writes the zstd archive of its
/// `.lake/build` to `archive`, which the release build embeds in Mago as its checker.
///
/// # Errors
///
/// Returns the error of finding Lean, of the build, or of writing the archive.
pub fn write_checker(archive: &Path) -> io::Result<()> {
    let lake = match toolchain() {
        Toolchain::Ready(lake) => lake,
        Toolchain::Unsupported => return Err(io::Error::other("Mago pins no Lean for this platform")),
        Toolchain::Failed(failure) => return Err(io::Error::other(failure)),
    };
    let builds = tempfile::tempdir()?;
    let library = build_in(builds.path(), &lake, &[])?;
    let file = fs::File::create(archive).map_err(|error| DatabaseError::io(IOAction::WriteFile, archive, error))?;
    let mut tarball = tar::Builder::new(zstd::Encoder::new(file, 19)?);
    tarball.append_dir_all(".lake/build/bin", library.join(".lake/build/bin"))?;
    tarball.append_dir_all(".lake/build/lib", library.join(".lake/build/lib"))?;
    tarball.into_inner()?.finish()?;

    Ok(())
}

/// What finding the Lean a law runs on came to.
pub(crate) enum Toolchain {
    /// The `lake` of the Lean found or installed.
    Ready(Lake),
    /// Neither Lean is installed, and the Mago build pins no Lean archive for its target.
    Unsupported,
    /// Mago could not install Lean, with what failed.
    Failed(String),
}

/// The Lean a law runs on: [`installed`] in Mago's cache folder and elan's folder, else the Lean Mago downloads now. A
/// Lean of another version, as a `lake` on the `PATH` may run, is never the one.
pub(crate) fn toolchain() -> Toolchain {
    let installed = cache_root().ok().and_then(|cache| installed(&cache, elan_home().as_deref()));
    match installed.map_or_else(install, |lake| Ok(Some(lake))) {
        Ok(Some(lake)) => Toolchain::Ready(lake),
        Ok(None) => Toolchain::Unsupported,
        Err(error) => Toolchain::Failed(error.to_string()),
    }
}

/// The `lake` of the installed Lean a law runs on: the one Mago installed in `lean/` in `cache`, else elan's toolchain
/// of the pinned version in `elan`, the folder elan installs into, as elan names the toolchain. Mago's own Lean moves
/// into place whole, so one found here is never half installed.
fn installed(cache: &Path, elan: Option<&Path>) -> Option<Lake> {
    let own = ARCHIVE.as_ref().map(|archive| cache.join("lean").join(archive.name));
    let toolchain = TOOLCHAIN.trim().replace('/', "--").replace(':', "---");
    let elan = elan.map(|elan| elan.join("toolchains").join(toolchain));

    own.into_iter().chain(elan).map(|lean| Lake { lean }).find(|lake| lake.program().is_file())
}

/// The folder elan installs toolchains into: `$ELAN_HOME`, else `.elan` in the home folder.
fn elan_home() -> Option<PathBuf> {
    std::env::var_os("ELAN_HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".elan")))
}

/// Installs the pinned Lean into `lean/` in Mago's cache folder and returns its `lake`. `None` where Mago pins no Lean
/// archive for the platform it is built for. An error names the step that failed.
fn install() -> io::Result<Option<Lake>> {
    let Some(archive) = &ARCHIVE else {
        return Ok(None);
    };
    let cache = cache_root()?;

    install_from(&url(archive), archive, &cache)
        .map(Some)
        .map_err(|error| io::Error::other(format!("{}: {error}", cache.join("lean").join(archive.name).display())))
}

/// The address of `archive` in the release of the library's Lean version on GitHub.
fn url(archive: &Archive) -> String {
    let release = TOOLCHAIN.trim().rsplit_once(':').map_or(TOOLCHAIN.trim(), |(_, release)| release);

    format!("https://github.com/leanprover/lean4/releases/download/{release}/{}.tar.zst", archive.name)
}

/// How many times an install downloads Lean when each download fails for a cause a new download can pass.
const DOWNLOADS: u32 = 3;

/// The pause before an install downloads Lean again, which grows by itself with each download.
const DOWNLOAD_PAUSE: Duration = Duration::from_secs(1);

/// Installs Lean from the archive at `url` into the folder of its name in `lean/` in `cache`, once, and returns its
/// `lake`. Installs take turns under `install_lock`, across threads and processes alike, and the first one prints what
/// it downloads. The archive unpacks into a staging folder while it downloads, and the staging folder moves into place
/// only when the archive has its pinned size and SHA-256, so a dead or failed install leaves no Lean behind. An install
/// first deletes the staging folder a dead install left. A download the network drops, or the server fails for now, is
/// tried again, up to [`DOWNLOADS`] times in all.
fn install_from(url: &str, archive: &Archive, cache: &Path) -> io::Result<Lake> {
    let _lock = install_lock(cache)?;
    let builds = cache.join("lean");
    let lean = builds.join(archive.name);
    let lake = Lake { lean: lean.clone() };
    if lake.program().is_file() {
        return Ok(lake);
    }

    tracing::info!(
        "Downloading the law checker ({} MB) into Mago's cache folder, once.",
        archive.bytes.div_ceil(1_000_000)
    );
    tracing::debug!("The law checker installs into {}.", lean.display());
    let staging = builds.join(format!("{}.staging", archive.name));
    let mut downloads = 1;
    let unpacked = loop {
        remove_folder(&staging)?;
        match unpack(url, archive, &staging) {
            Ok(()) => break fs::rename(staging.join(archive.name), &lean).and_then(|()| remove_folder(&staging)),
            Err(InstallFailure::Transient(cause)) if downloads < DOWNLOADS => {
                tracing::info!("The law checker's download failed ({cause}), so it downloads again.");
                thread::sleep(DOWNLOAD_PAUSE * downloads);
                downloads += 1;
            }
            Err(InstallFailure::Transient(cause)) => {
                break Err(io::Error::other(format!("downloading {url} failed on each of {DOWNLOADS} tries: {cause}")));
            }
            Err(InstallFailure::Permanent(error)) => break Err(error),
        }
    };
    if let Err(error) = unpacked {
        remove_folder(&staging)?;

        return Err(error);
    }

    Ok(lake)
}

/// What stopped an install's download and unpack.
enum InstallFailure {
    /// The download failed for this cause, which a new download can pass: the network dropped or stalled it, or the
    /// server failed for now.
    Transient(String),
    /// A failure no new download passes, as a refusal of the server, an archive of another size or SHA-256 than its
    /// pin, or a disk that can't hold it, with what failed.
    Permanent(io::Error),
}

impl From<io::Error> for InstallFailure {
    fn from(error: io::Error) -> Self {
        Self::Permanent(error)
    }
}

/// Whether a download that failed with `error` can pass when tried again: the network dropped, stalled or never made
/// the connection, or the server failed for now with a 5xx or 429 status. A body over the pinned size, a refusal such
/// as a 404, and an address or TLS setup that is wrong fail the same way on every try.
fn passes_on_a_retry(error: &ureq::Error) -> bool {
    match error {
        ureq::Error::StatusCode(status) => *status == 429 || *status >= 500,
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::Protocol(_)
        | ureq::Error::BodyStalled => true,
        _ => false,
    }
}

/// Downloads the archive at `url` and unpacks it into `staging`, each step on a thread of its own: the download, which
/// hashes what it reads, the unzip, and the unpack. An archive of another size or SHA-256 than `archive` pins fails. An
/// error names the first step that failed: a step that stops reading stops the steps before it, whose error then names
/// no cause of its own.
fn unpack(url: &str, archive: &Archive, staging: &Path) -> Result<(), InstallFailure> {
    let failed = |cause: String, transient: bool| {
        if transient {
            InstallFailure::Transient(cause)
        } else {
            InstallFailure::Permanent(io::Error::other(format!("downloading {url} failed: {cause}")))
        }
    };
    let body = ureq::get(url)
        .header("User-Agent", "mago")
        .call()
        .map_err(|error| failed(error.to_string(), passes_on_a_retry(&error)))?
        .into_body()
        .into_with_config()
        // ureq refuses any read once its limit is reached, the read that finds the end too.
        .limit(archive.bytes + 1)
        .reader();
    let (downloaded, download) = read_ahead(Hashed { read: body, sha256: Sha256::new() });
    let (unzipped, unzip) = read_ahead(zstd::Decoder::new(downloaded)?);
    let mut tarball = tar::Archive::new(unzipped);
    // The tarball ends in padding, read to its end so the unzip and the download end too.
    let unpacked =
        tarball.unpack(staging).and_then(|()| io::copy(&mut tarball.into_inner(), &mut io::sink()).map(drop));
    let unpacked = match joined(unzip) {
        Err(error) if error.kind() != io::ErrorKind::BrokenPipe => Err(error),
        _ => unpacked,
    }
    .map_err(|error| io::Error::other(format!("{url} can't be unpacked: {}", with_causes(&error))));
    let hashed = match joined(download) {
        Ok(hashed) => hashed,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(unpacked?),
        Err(error) => {
            // A read of the body fails with ureq's own error inside, as a body over the limit does, or with the error
            // of the connection it reads.
            let ureq = error.get_ref().and_then(|inner| inner.downcast_ref::<ureq::Error>());
            return Err(failed(error.to_string(), ureq.is_none_or(passes_on_a_retry)));
        }
    };
    let digest = hex(&hashed.sha256.finalize());
    if digest != archive.sha256 {
        return Err(
            io::Error::other(format!("{url} has the SHA-256 {digest}, not the pinned {}", archive.sha256)).into()
        );
    }

    Ok(unpacked?)
}

/// A reader that hashes what it reads.
struct Hashed<R> {
    read: R,
    sha256: Sha256,
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.read.read(buffer)?;
        self.sha256.update(&buffer[..read]);

        Ok(read)
    }
}

/// Reads `source` to its end on a thread of its own, up to `AHEAD` chunks ahead of the reader it returns. The thread
/// gives `source` back once it read all of it, else the error that stopped it, or a `BrokenPipe` error once the reader
/// was dropped before the end.
fn read_ahead<R>(mut source: R) -> (Ahead, thread::JoinHandle<io::Result<R>>)
where
    R: Read + Send + 'static,
{
    let (chunks, received) = mpsc::sync_channel(AHEAD);
    let thread = thread::spawn(move || {
        loop {
            let mut chunk = Vec::new();
            (&mut source).take(CHUNK).read_to_end(&mut chunk)?;
            if chunk.is_empty() {
                return Ok(source);
            }
            chunks
                .send(chunk)
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the step after it stopped reading"))?;
        }
    });

    (Ahead { received, chunk: Vec::new(), read: 0 }, thread)
}

/// The reader of the chunks a `read_ahead` thread sends, which ends where the thread stops sending.
struct Ahead {
    received: mpsc::Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    read: usize,
}

impl Read for Ahead {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.read == self.chunk.len() {
            let Ok(chunk) = self.received.recv() else {
                return Ok(0);
            };
            (self.chunk, self.read) = (chunk, 0);
        }
        let read = buffer.len().min(self.chunk.len() - self.read);
        buffer[..read].copy_from_slice(&self.chunk[self.read..self.read + read]);
        self.read += read;

        Ok(read)
    }
}

/// `error` followed by each error that caused it, since tar's errors name the step that failed and leave the cause, such
/// as a full disk, to their source.
fn with_causes(error: &io::Error) -> String {
    let mut printed = error.to_string();
    let mut cause = std::error::Error::source(error);
    while let Some(error) = cause {
        let _ = write!(printed, ": {error}");
        cause = error.source();
    }

    printed
}

/// What `thread` gave back once it ended.
fn joined<T>(thread: thread::JoinHandle<io::Result<T>>) -> io::Result<T> {
    thread.join().map_err(|_| io::Error::other("a thread of the install panicked"))?
}

/// The exclusive lock on `install.lock` in `cache`, held across threads and processes alike until it is dropped.
fn install_lock(cache: &Path) -> io::Result<fs::File> {
    Ok(lock::exclusive(&cache.join("install.lock"))?)
}

/// The SHA-256 of `bytes` in lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// `bytes` in lowercase hex, as an archive's pinned `sha256` is written.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    })
}

/// Deletes `folder` and everything in it, if it exists.
fn remove_folder(folder: &Path) -> io::Result<()> {
    match fs::remove_dir_all(folder) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            Err(DatabaseError::io(IOAction::DeleteFolder, folder, error).into())
        }
        _ => Ok(()),
    }
}

/// Mago's cache folder: `$XDG_CACHE_HOME/mago`, else `~/Library/Caches/mago` on macOS, `%LOCALAPPDATA%\mago` on
/// Windows and `~/.cache/mago` elsewhere.
///
/// # Errors
///
/// Returns an error when no environment variable names the cache and the user has no home folder.
pub fn cache_root() -> io::Result<PathBuf> {
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|cache| !cache.is_empty()) {
        return Ok(PathBuf::from(cache).join("mago"));
    }
    if cfg!(windows)
        && let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|local| !local.is_empty())
    {
        return Ok(PathBuf::from(local).join("mago"));
    }

    let home =
        std::env::home_dir().ok_or_else(|| io::Error::other("The checker's cache folder needs a home folder"))?;

    Ok(if cfg!(target_os = "macos") { home.join("Library/Caches/mago") } else { home.join(".cache/mago") })
}

#[cfg(all(test, unix))]
#[allow(clippy::panic_in_result_fn)]
mod tests {
    use std::io::BufRead;
    use std::io::BufReader;
    use std::net::TcpListener;
    use std::net::TcpStream;
    use std::ops::RangeBounds;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    use super::*;

    /// Each process a `Lake` starts, `lake` itself or anything run in its place, runs under the processor-time limit,
    /// which the Lean processes it starts inherit.
    #[test]
    fn each_process_a_lake_starts_runs_under_the_processor_time_limit() -> io::Result<()> {
        let lean = tempfile::tempdir()?;
        let lake = lake_in(lean.path(), "#!/bin/sh\nulimit -t; ulimit -c\n")?;

        let output = process::output(&mut lake.command())?;

        assert_eq!(String::from_utf8_lossy(&output.stdout), "120\n0\n");

        Ok(())
    }

    /// A law checker process that outlives Mago's end at the deadline, as one the kernel holds on a stalled disk, keeps
    /// the group's pipes open. The wait still returns what the group wrote before the deadline, as a check the deadline
    /// stopped, with the leader ended by SIGKILL, where the compile failed with the end's error.
    #[test]
    fn a_group_that_outlives_its_end_at_the_deadline_gives_what_it_wrote_as_a_stopped_check() -> io::Result<()> {
        use std::os::unix::process::ExitStatusExt;

        let mut command = Command::new("sh");
        command.args(["-c", "echo 'module 0 Code.App.Shared.Money' >&2; echo answer; exec sleep 2147483647"]);
        let mut group = ProcessGroup::spawn(command.stdout(Stdio::piped()).stderr(Stdio::piped()))?;
        let outlived = |_: &mut ProcessGroup| {
            Err(io::Error::new(io::ErrorKind::TimedOut, "process group 4321 still ran 1000 ms after SIGKILL"))
        };

        let waited = wait_before_deadline(&mut group, Duration::from_millis(200), outlived);
        drop(group);
        let (output, waited) = waited?;

        assert_eq!(
            (output.status.signal(), output.stdout.as_slice(), output.stderr.as_slice(), waited),
            (Some(libc::SIGKILL), b"answer\n".as_slice(), b"module 0 Code.App.Shared.Money\n".as_slice(), true)
        );

        Ok(())
    }

    /// A build is named by every file of the library's package, its `lean-toolchain` among them, so a build of another
    /// Lean or of other sources is never used, and nothing else, such as Mago's own Rust code, names another build.
    #[test]
    fn the_library_build_is_named_by_each_file_of_its_package_and_so_by_its_lean_version() {
        let base = sources();
        assert_eq!(base, key(&PACKAGE, &[]));
        assert_eq!(base.len(), 32);
        let other_lean = PACKAGE
            .map(|(path, source)| (path, if path == "lean-toolchain" { "leanprover/lean4:v4.35.0\n" } else { source }));
        assert_ne!(base, key(&other_lean, &[]));

        for (index, &(path, source)) in PACKAGE.iter().enumerate() {
            let edited = format!("{source}\n");
            let mut package = PACKAGE;
            package[index] = (path, edited.as_str());
            assert_ne!(base, key(&package, &[]), "{path}");
        }
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

    /// The `lake` of the archive `lean_archive` makes, which prints `lake`.
    const LAKE: &str = "#!/bin/sh\necho lake\n";

    /// The `lake` of a Lean in `lean` whose `bin/lake` runs `script`.
    fn lake_in(lean: &Path, script: &str) -> io::Result<Lake> {
        use std::os::unix::fs::PermissionsExt;

        let lake = Lake { lean: lean.to_path_buf() };
        fs::create_dir_all(lean.join("bin"))?;
        fs::write(lake.program(), script)?;
        fs::set_permissions(lake.program(), fs::Permissions::from_mode(0o755))?;

        Ok(lake)
    }

    /// A Lean archive as Lean's releases make one, a zstd stream of a tarball whose one folder, `lean-test`, holds
    /// `bin/lake` and `bin/lean`, a link to it.
    fn lean_archive() -> io::Result<Vec<u8>> {
        let mut tarball = tar::Builder::new(Vec::new());
        let mut lake = tar::Header::new_gnu();
        lake.set_size(LAKE.len() as u64);
        lake.set_mode(0o755);
        tarball.append_data(&mut lake, "lean-test/bin/lake", LAKE.as_bytes())?;
        let mut lean = tar::Header::new_gnu();
        lean.set_entry_type(tar::EntryType::Symlink);
        lean.set_size(0);
        tarball.append_link(&mut lean, "lean-test/bin/lean", "lake")?;

        zstd::encode_all(tarball.into_inner()?.as_slice(), 0)
    }

    /// `served` over HTTP on a local port, and how many requests it answered.
    fn serve(served: Vec<u8>) -> io::Result<(String, Arc<AtomicUsize>)> {
        serve_dropping(served, 0..0)
    }

    /// `served` over HTTP on a local port, where the connection of each response whose number `dropped` holds drops
    /// halfway through its body, as a network that drops a download does, and how many requests it answered.
    fn serve_dropping(
        served: Vec<u8>,
        dropped: impl RangeBounds<usize> + Send + 'static,
    ) -> io::Result<(String, Arc<AtomicUsize>)> {
        listen(move |request, stream| {
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", served.len())?;
            let sent = if dropped.contains(&request) { served.len() / 2 } else { served.len() };
            stream.write_all(&served[..sent])
        })
    }

    /// A local port that answers each request with `status` and no body, and how many requests it answered.
    fn serve_status(status: &'static str) -> io::Result<(String, Arc<AtomicUsize>)> {
        listen(move |_, stream| write!(stream, "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"))
    }

    /// A local port whose server reads each request and answers it with `answer`, given the request's number from 0,
    /// and how many requests it answered. A request the server fails to answer fails the install that sent it.
    fn listen(
        answer: impl Fn(usize, &mut TcpStream) -> io::Result<()> + Send + 'static,
    ) -> io::Result<(String, Arc<AtomicUsize>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://{}/lean-test.tar.zst", listener.local_addr()?);
        let requests = Arc::new(AtomicUsize::new(0));
        let answered = Arc::clone(&requests);
        std::thread::spawn(move || -> io::Result<()> {
            for stream in listener.incoming() {
                let mut stream = stream?;
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                while reader.read_line(&mut line)? > 2 {
                    line.clear();
                }
                answer(answered.fetch_add(1, Ordering::SeqCst), &mut stream)?;
            }

            Ok(())
        });

        Ok((url, requests))
    }

    /// The archive `lean-test` pinned to the SHA-256 and the size of `bytes`.
    fn archive(bytes: &[u8]) -> Archive {
        Archive { name: "lean-test", sha256: sha256(bytes), bytes: bytes.len() as u64 }
    }

    fn sha256(bytes: &[u8]) -> &'static str {
        Box::leak(sha256_hex(bytes).into_boxed_str())
    }

    /// What the `bin/lean` installed in `cache`, a link to `bin/lake`, prints.
    fn lean_in(cache: &Path) -> io::Result<String> {
        let output = process::output(&mut Command::new(cache.join("lean/lean-test/bin/lean")))?;

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Each Lean archive Mago pins is of the Lean version the library's package pins, at its release on GitHub, so a
    /// new Lean version fails here until its archives are pinned.
    #[test]
    fn the_pinned_lean_archive_is_the_release_of_the_library_s_lean_version() {
        if let Some(archive) = &ARCHIVE {
            assert!(archive.name.starts_with("lean-4.34.1-"), "{}", archive.name);
            assert_eq!(
                url(archive),
                format!("https://github.com/leanprover/lean4/releases/download/v4.34.1/{}.tar.zst", archive.name)
            );
        }
    }

    #[test]
    fn installs_that_start_together_download_lean_once() -> io::Result<()> {
        let bytes = lean_archive()?;
        let archive = archive(&bytes);
        let (url, requests) = serve(bytes)?;
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
        assert_eq!(lean_in(cache.path())?, "lake\n");
        for lake in lakes {
            assert_eq!(lake.lean, cache.path().join("lean/lean-test"));
        }
        assert!(!cache.path().join("lean/lean-test.staging").exists());

        Ok(())
    }

    /// Installs that start together on an empty cache, as the compiles of `sharp/bin/spec` do, each get Lean when the
    /// network drops the first download: the install that took the lock downloads again, and the others use its Lean.
    #[test]
    fn installs_that_start_together_each_get_lean_when_the_first_download_drops() -> io::Result<()> {
        let bytes = lean_archive()?;
        let archive = archive(&bytes);
        let (url, requests) = serve_dropping(bytes, 0..1)?;
        let cache = tempfile::tempdir()?;

        let installed: Vec<Result<PathBuf, String>> = std::thread::scope(|scope| {
            #[allow(clippy::needless_collect)]
            let installs: Vec<_> =
                std::iter::repeat_with(|| scope.spawn(|| install_from(&url, &archive, cache.path()))).take(4).collect();
            installs
                .into_iter()
                .map(|install| match install.join() {
                    Ok(installed) => installed.map(|lake| lake.lean).map_err(|error| error.to_string()),
                    Err(_) => Err("an install panicked".to_owned()),
                })
                .collect()
        });

        assert_eq!(installed, vec![Ok(cache.path().join("lean/lean-test")); 4]);
        assert_eq!(requests.load(Ordering::SeqCst), 2, "the dropped download and the one after it");
        assert_eq!(lean_in(cache.path())?, "lake\n");
        assert!(!cache.path().join("lean/lean-test.staging").exists());

        Ok(())
    }

    /// A download that drops on every try fails the install once its three tries are spent, and a download the server
    /// refuses is not tried again. Each error names the step and its cause, and leaves no Lean and no staging folder.
    #[test]
    fn a_download_that_drops_every_time_or_is_refused_fails_the_install_with_its_cause() -> io::Result<()> {
        let bytes = lean_archive()?;
        let archive = archive(&bytes);
        for (failure, (url, requests), tries, cause) in [
            (
                "a download that drops on every try",
                serve_dropping(bytes, ..)?,
                3,
                "failed on each of 3 tries: Peer disconnected",
            ),
            ("a download the server refuses", serve_status("404 Not Found")?, 1, "failed: http status: 404"),
        ] {
            let cache = tempfile::tempdir()?;

            let failed = install_from(&url, &archive, cache.path()).err().map(|failed| failed.to_string());

            assert_eq!(failed, Some(format!("downloading {url} {cause}")), "{failure}");
            assert_eq!(requests.load(Ordering::SeqCst), tries, "{failure}");
            assert!(!cache.path().join("lean/lean-test").exists(), "{failure}");
            assert!(!cache.path().join("lean/lean-test.staging").exists(), "{failure}");
        }

        Ok(())
    }

    #[test]
    fn an_install_deletes_the_staging_folder_a_killed_install_left() -> io::Result<()> {
        let bytes = lean_archive()?;
        let archive = archive(&bytes);
        let (url, _) = serve(bytes)?;
        let cache = tempfile::tempdir()?;
        fs::create_dir_all(cache.path().join("lean/lean-test.staging/lean-test/bin"))?;
        fs::write(cache.path().join("lean/lean-test.staging/lean-test/bin/half"), "half of Lean")?;

        install_from(&url, &archive, cache.path())?;

        assert!(!cache.path().join("lean/lean-test.staging").exists());
        assert!(!cache.path().join("lean/lean-test/bin/half").exists());
        assert_eq!(lean_in(cache.path())?, "lake\n");

        Ok(())
    }

    #[test]
    fn a_failed_install_leaves_no_lean_and_the_next_install_downloads_it_again() -> io::Result<()> {
        let bytes = lean_archive()?;
        let digest = sha256(&bytes);
        let other = "0000000000000000000000000000000000000000000000000000000000000000";
        let size = bytes.len() as u64;
        let not_zstd = b"half of a tarball".to_vec();
        for (failure, served, pinned, error) in [
            (
                "an archive whose SHA-256 is not the pinned one",
                bytes.clone(),
                Archive { name: "lean-test", sha256: other, bytes: size },
                format!("has the SHA-256 {digest}, not the pinned {other}"),
            ),
            (
                "an archive longer than its pinned size",
                bytes.clone(),
                Archive { name: "lean-test", sha256: digest, bytes: size - 1 },
                format!("failed: the response body is larger than request limit: {size}"),
            ),
            (
                "an archive that is no zstd stream",
                not_zstd.clone(),
                archive(&not_zstd),
                "can't be unpacked: Unknown frame descriptor".to_owned(),
            ),
        ] {
            let (url, _) = serve(served)?;
            let cache = tempfile::tempdir()?;

            let failed = install_from(&url, &pinned, cache.path()).err().map(|failed| failed.to_string());

            assert!(failed.as_deref().is_some_and(|failed| failed.contains(&error)), "{failure}: {failed:?}");
            assert!(!cache.path().join("lean/lean-test").exists(), "{failure}");
            assert!(!cache.path().join("lean/lean-test.staging").exists(), "{failure}");

            let (url, requests) = serve(bytes.clone())?;
            install_from(&url, &archive(&bytes), cache.path())?;
            assert_eq!(requests.load(Ordering::SeqCst), 1, "{failure}");
            assert_eq!(lean_in(cache.path())?, "lake\n", "{failure}");
        }

        Ok(())
    }

    /// A law runs on one Lean, found one way: the Lean Mago installed in its cache folder, else elan's toolchain of the
    /// pinned version. elan's toolchain of another version is never one of them.
    #[test]
    fn a_law_runs_on_mago_s_own_lean_else_on_elan_s_toolchain_of_the_pinned_version() -> io::Result<()> {
        let cache = tempfile::tempdir()?;
        let elan = tempfile::tempdir()?;
        let pinned = elan.path().join("toolchains/leanprover--lean4---v4.34.1");
        lake_in(&elan.path().join("toolchains/leanprover--lean4---v4.35.0"), LAKE)?;

        assert!(installed(cache.path(), Some(elan.path())).is_none());

        lake_in(&pinned, LAKE)?;
        assert_eq!(installed(cache.path(), Some(elan.path())).map(|lake| lake.lean), Some(pinned));

        if let Some(archive) = &ARCHIVE {
            let own = cache.path().join("lean").join(archive.name);
            lake_in(&own, LAKE)?;
            assert_eq!(installed(cache.path(), Some(elan.path())).map(|lake| lake.lean), Some(own.clone()));
            assert_eq!(installed(cache.path(), None).map(|lake| lake.lean), Some(own));
        }

        Ok(())
    }

    /// A build that `lake build` fails names its cause: the error lines `lake build` printed, without its progress.
    #[test]
    fn a_build_that_lake_build_fails_names_the_error_lines_it_printed() -> io::Result<()> {
        let builds = tempfile::tempdir()?;
        let lean = tempfile::tempdir()?;
        let lake = lake_in(
            lean.path(),
            "#!/bin/sh\necho '[2/5] Built Sharp.Lean.Prelude'\necho 'error: ./Sharp/Lean/Laws.lean:3:8: unknown identifier'\necho 'error: build failed' >&2\nexit 1\n",
        )?;

        let failed = build_in(builds.path(), &lake, &[]).err().map(|error| error.to_string());

        assert_eq!(
            failed.as_deref(),
            Some(
                "the law checker could not be built in Mago's cache folder: lake build printed:\nerror: ./Sharp/Lean/Laws.lean:3:8: unknown identifier\nerror: build failed"
            )
        );

        Ok(())
    }

    /// A finished build deletes the older builds beside it, and keeps the Lean Mago installed there.
    #[test]
    fn a_build_deletes_the_older_builds_and_keeps_the_lean_mago_installed() -> io::Result<()> {
        let Some(archive) = &ARCHIVE else {
            return Ok(());
        };
        let builds = tempfile::tempdir()?;
        let lean = tempfile::tempdir()?;
        let lake = lake_in(lean.path(), "#!/bin/sh\nmkdir -p .lake/build/bin && touch .lake/build/bin/sharp-lean\n")?;
        fs::create_dir_all(builds.path().join(archive.name).join("bin"))?;
        fs::create_dir_all(builds.path().join("0123456789abcdef0123456789abcdef"))?;

        let library = build_in(builds.path(), &lake, &[])?;

        assert!(runner(&library).is_file());
        assert!(builds.path().join(archive.name).join("bin").is_dir());
        assert!(!builds.path().join("0123456789abcdef0123456789abcdef").exists());

        Ok(())
    }

    /// A build's `.olean` files load only beside the modules of the Lean that wrote them, so a build whose Lean is gone,
    /// as an uninstalled toolchain leaves it, is built again by the Lean there is, which it then records.
    #[test]
    fn a_build_whose_lean_is_gone_is_built_again_by_the_lean_there_is() -> io::Result<()> {
        let builds = tempfile::tempdir()?;
        let lean = tempfile::tempdir()?;
        fs::create_dir_all(lean.path().join("lib/lean"))?;
        let lake =
            lake_in(lean.path(), "#!/bin/sh\nmkdir -p .lake/build/bin && echo rebuilt > .lake/build/bin/sharp-lean\n")?;
        let library = builds.path().join(key(&PACKAGE, &[]));
        for (path, source) in PACKAGE {
            fs::create_dir_all(library.join(path).parent().unwrap_or(&library))?;
            fs::write(library.join(path), source)?;
        }
        fs::create_dir_all(library.join(".lake/build/bin"))?;
        fs::write(runner(&library), "built by a Lean that is gone")?;
        fs::write(library.join(SYSROOT), builds.path().join("gone").display().to_string())?;

        assert_eq!(built_in(builds.path(), &[]), None);
        let built = build_in(builds.path(), &lake, &[])?;

        assert_eq!(built, library);
        assert_eq!(fs::read_to_string(runner(&library))?, "rebuilt\n");
        assert_eq!(fs::read_to_string(library.join(SYSROOT))?.trim(), lean.path().display().to_string());
        assert_eq!(built_in(builds.path(), &[]), Some(library));

        Ok(())
    }

    /// A Mago that embeds its checker unpacks it on the first build that proves a law, under the build lock, and runs
    /// no `lake`. The build is named by the checker too, so a Mago never runs another Mago's embedded checker.
    #[cfg(unix)]
    #[test]
    fn a_build_unpacks_the_embedded_checker_and_runs_no_lake() -> io::Result<()> {
        let lean = tempfile::tempdir()?;
        let lake = lake_in(lean.path(), "#!/bin/sh\nexit 1\n")?;
        fs::create_dir_all(lean.path().join("lib/lean"))?;
        let mut checker = Vec::new();
        let mut tarball = tar::Builder::new(zstd::Encoder::new(&mut checker, 3)?);
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o755);
        tarball.append_data(&mut header, ".lake/build/bin/sharp-lean", &b"runner"[..])?;
        tarball.into_inner()?.finish()?;
        let builds = tempfile::tempdir()?;

        let library = build_in(builds.path(), &lake, &checker)?;

        assert_eq!(library, builds.path().join(key(&PACKAGE, &checker)));
        assert_ne!(key(&PACKAGE, &checker), key(&PACKAGE, &[]), "the build is named by the checker it embeds");
        assert_eq!(fs::read(runner(&library))?, b"runner");
        assert_eq!(built_in(builds.path(), &checker), Some(library));

        Ok(())
    }
}
