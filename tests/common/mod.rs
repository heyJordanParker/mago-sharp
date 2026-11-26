#![allow(dead_code)]

pub mod compiling;

use std::borrow::Cow;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;

use mago_database::DatabaseConfiguration;
use mago_database::GlobSettings;
use mago_database::process;
use mago_extension::Host;
use mago_extension::HostFile;
use mago_extension::Hosts;

/// The deadline and the backstop of a host whose test checks neither: a day, longer than any CI job runs, so a loaded
/// machine never reaches it, where `Duration::MAX` would overflow the engine's clock.
pub const NO_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// A `mago analyze --watch` that a test reads the log of as the watch writes it, so the test waits for the watch's own
/// events and never for wall time. Dropping it kills the watch.
pub struct Watch {
    child: Child,
    log: Arc<(Mutex<Log>, Condvar)>,
}

/// What a watch wrote to its stderr so far, and whether it closed it.
#[derive(Default)]
struct Log {
    text: String,
    closed: bool,
}

impl Watch {
    /// Starts the watch `command` runs, with its stderr read line by line.
    pub fn start(command: &mut Command) -> Self {
        let mut child = process::spawn(command.stderr(Stdio::piped())).unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let log = Arc::new((Mutex::new(Log::default()), Condvar::new()));
        let written = Arc::clone(&log);
        std::thread::spawn(move || {
            let (log, wrote) = &*written;
            let mut line = Vec::new();
            while stderr.read_until(b'\n', &mut line).is_ok_and(|read| read > 0) {
                log.lock().unwrap().text.push_str(&String::from_utf8_lossy(&line));
                wrote.notify_all();
                line.clear();
            }
            log.lock().unwrap().closed = true;
            wrote.notify_all();
        });

        Self { child, log }
    }

    /// Waits until `done` holds of the watch's log, and returns whether it did before the watch closed its stderr.
    pub fn until(&self, done: impl Fn(&str) -> bool) -> bool {
        let (log, wrote) = &*self.log;
        let log = wrote.wait_while(log.lock().unwrap(), |log| !log.closed && !done(&log.text)).unwrap();

        done(&log.text)
    }

    /// What the watch logged so far.
    pub fn log(&self) -> String {
        self.log.0.lock().unwrap().text.clone()
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }

    /// Kills the watch and returns what it logged.
    pub fn stop(mut self) -> String {
        self.child.kill().unwrap();
        self.child.wait().unwrap();

        self.log()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if self.child.try_wait().is_ok_and(|status| status.is_none()) {
            self.child.kill().ok();
            self.child.wait().ok();
        }
    }
}

pub fn php_sdk_is_available(repository: &std::path::Path, test: &str) -> bool {
    let available = repository.join("vendor/autoload.php").is_file();
    assert!(
        available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(),
        "vendor dependencies are required for {test}"
    );
    available
}

/// The extension host `acme` whose worker `file` registers, as mago runs it from the repository: in each thread's
/// interpreter of the engine linked into the test binary, with its `deadline` of CPU time, its `backstop` of wall time,
/// and `environment`.
pub fn host(file: &Path, deadline: Duration, backstop: Duration, environment: &[(&str, &str)]) -> Host {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let file = HostFile {
        name: "acme".to_owned(),
        file: file.to_path_buf(),
        deadline,
        backstop,
        environment: environment.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
        ini: Default::default(),
    };
    let sdk = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sdk");

    Hosts::new(vec![file], repository.to_path_buf(), &sdk).expect("the extension host starts").hosts().remove(0)
}

pub fn database_configuration(
    workspace: impl Into<PathBuf>,
    paths: Vec<Cow<'static, [u8]>>,
) -> DatabaseConfiguration<'static> {
    DatabaseConfiguration {
        workspace: Cow::Owned(workspace.into()),
        paths,
        includes: Vec::new(),
        patches: Vec::new(),
        excludes: Vec::new(),
        extensions: vec![Cow::Borrowed(b"php")],
        glob: GlobSettings::default(),
    }
}
