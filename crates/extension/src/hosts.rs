//! Runs the extension hosts inside mago, on the PHP# engine linked into the binary.
//!
//! Each thread that asks a host something owns one interpreter, which loads mago's own SDK, then every host's file once, and then serves that
//! thread's requests as calls. All hosts share each interpreter, as a PHP analyzer loads every extension into one
//! process.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::CString;
use std::fmt::Display;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use foldhash::HashMap;
use mago_engine::Answer;
use mago_engine::Engine;
use mago_engine::EngineError;
use mago_engine::Frame;
use mago_engine::Value;

use crate::WorkerError;
use crate::reduction;

include!(concat!(env!("OUT_DIR"), "/sdk.rs"));

/// The SDK's entry that serves one request with a host's worker, as `[true, response]` or `[false, message]`. A
/// request and its response are both a list of values, or both a string of bytes.
const HANDLE: &str = "Mago\\Sdk\\Internal\\Hosts::handle";
/// The SDK's entry that runs a host's file, which registers the host's worker.
const LOAD: &str = "Mago\\Sdk\\Internal\\Hosts::load";

/// The class of the worker a host's file runs, whose code calls each hook.
const WORKER: &str = "Mago\\Sdk\\Worker";
/// The SDK's entry that finds the host whose file declares a class-like.
const DECLARER: &str = "Mago\\Sdk\\Internal\\Hosts::declarer";
/// The longest an interpreter may take to move to the configuration's directory.
const CHDIR: Duration = Duration::from_secs(1);

static NEXT_HOSTS: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// The calling thread's interpreter of each [`Hosts`], by its id: what it has loaded and replayed.
    static PREPARED: RefCell<HashMap<u64, Prepared>> = RefCell::default();
}

/// One host mago runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFile {
    /// The host's name, as `[extension-hosts.<name>]` gives it.
    pub name: String,
    /// The PHP file that registers the host's worker.
    pub file: PathBuf,
    /// The CPU time one unit of a request may use with no progress. Ten times as much wall time with no progress ends
    /// it too, however it spends it.
    pub deadline: Duration,
    /// The environment variables the host's worker reads.
    pub environment: BTreeMap<String, String>,
    /// The PHP settings the host's worker runs with, by name.
    pub ini: BTreeMap<String, String>,
}

/// Answers the codebase queries a worker asks while it serves a request.
pub trait WorkerRequestHandler {
    /// Answers one query.
    ///
    /// # Errors
    ///
    /// Returns why the query failed, which the worker receives as an exception.
    fn handle(&mut self, query: &[Value]) -> Result<Vec<Value>, Vec<u8>>;
}

impl<F> WorkerRequestHandler for F
where
    F: FnMut(&[Value]) -> Result<Vec<Value>, Vec<u8>>,
{
    fn handle(&mut self, query: &[Value]) -> Result<Vec<Value>, Vec<u8>> {
        self(query)
    }
}

/// A request one host received for every interpreter: each interpreter that starts later receives it too, in order.
#[derive(Debug)]
struct Bootstrap {
    sequence: u64,
    host: usize,
    group: Option<u64>,
    request: Vec<Value>,
}

/// What one thread's interpreter has done: its id, the last bootstrap it received, and how many reloads it checked.
#[derive(Debug, Clone, Copy)]
struct Prepared {
    interpreter: u64,
    replayed: u64,
    reloads: usize,
}

/// Every extension host mago runs, in the engine they share.
#[derive(Debug)]
pub struct Hosts {
    id: u64,
    engine: Engine,
    files: Vec<HostFile>,
    directory: PathBuf,
    bootstraps: Mutex<Vec<Bootstrap>>,
    next_bootstrap: AtomicU64,
    reloads: Mutex<Vec<Vec<u8>>>,
    reducing: Vec<AtomicBool>,
    /// Whether a thread of a rayon pool started an interpreter, so the reduction and the stop reach that pool's threads.
    /// While none has, they run on the calling thread alone: a rayon broadcast waits for every thread of the pool, and
    /// the analyzer's initialization thread, which a pool thread joins, stops the hosts when they fail to load.
    pooled: AtomicBool,
}

/// One extension host, which the analyzer and the linter send their requests to.
#[derive(Debug, Clone)]
pub struct Host {
    hosts: Arc<Hosts>,
    index: usize,
}

impl Hosts {
    /// Starts the engine for `hosts`, whose interpreters each start in `directory`, the configuration's directory, and
    /// writes mago's own SDK to `cache`, where each interpreter loads it from.
    ///
    /// # Errors
    ///
    /// Returns an error when two hosts set one environment variable or one PHP setting to different values, the SDK
    /// can't be written, or the engine can't start.
    pub fn new(hosts: Vec<HostFile>, directory: PathBuf, cache: &Path) -> Result<Arc<Self>, WorkerError> {
        let id = NEXT_HOSTS.fetch_add(1, Ordering::Relaxed);
        let variables = shared(&hosts, Shared::Variable)?;
        let settings = shared(&hosts, Shared::Setting)?;
        let preload = write_sdk(cache, id).map_err(WorkerError::Sdk)?;
        let engine = Engine::new(vec![preload], variables, settings).map_err(WorkerError::Start)?;
        let reducing = hosts.iter().map(|_| AtomicBool::new(false)).collect();

        Ok(Arc::new(Self {
            id,
            engine,
            files: hosts,
            directory,
            bootstraps: Mutex::default(),
            next_bootstrap: AtomicU64::new(1),
            reloads: Mutex::default(),
            reducing,
            pooled: AtomicBool::new(false),
        }))
    }

    /// Each host, in the order `new` was given them.
    #[must_use]
    pub fn hosts(self: &Arc<Self>) -> Vec<Host> {
        (0..self.files.len()).map(|index| Host { hosts: Arc::clone(self), index }).collect()
    }

    /// Makes the calling thread's interpreter ready for a request: starts one that loads every host and receives every
    /// bootstrap, or brings the one that runs up to date, starting it again when it loaded a class that changed.
    fn prepare(&self) -> Result<(), WorkerError> {
        let current = self.engine.interpreter();
        let prepared = PREPARED
            .with_borrow(|prepared| prepared.get(&self.id).copied())
            .filter(|prepared| Some(prepared.interpreter) == current);
        match prepared {
            Some(prepared) if !self.reloaded(prepared)? => self.replay(prepared.replayed),
            _ => self.start(),
        }
    }

    /// Starts the calling thread's interpreter in the configuration's directory, loads every host's file in it, and
    /// gives it every bootstrap.
    fn start(&self) -> Result<(), WorkerError> {
        self.engine.call("chdir", &[path_value(&self.directory)], CHDIR, &mut refuse).map_err(WorkerError::Start)?;
        for (index, host) in self.files.iter().enumerate() {
            let arguments = [Value::String(host.name.clone().into_bytes()), path_value(&host.file)];
            if let Err(error) = self.engine.call(LOAD, &arguments, host.deadline, &mut refuse) {
                let message = error.to_string();
                return Err(match redeclared(&message) {
                    Some(class) => self.clash(index, class),
                    None => {
                        let thrown = match error {
                            EngineError::Threw(thrown) => Some(Box::new(thrown)),
                            _ => None,
                        };

                        WorkerError::Load { host: host.name.clone(), message, thrown }
                    }
                });
            }
        }

        let interpreter = self.engine.interpreter().unwrap_or_default();
        if rayon::current_thread_index().is_some() {
            self.pooled.store(true, Ordering::Release);
        }
        PREPARED.with_borrow_mut(|prepared| {
            prepared.insert(self.id, Prepared { interpreter, replayed: 0, reloads: lock(&self.reloads).len() })
        });

        self.replay(0)
    }

    /// Gives the calling thread's interpreter each bootstrap after `replayed`, in order.
    fn replay(&self, replayed: u64) -> Result<(), WorkerError> {
        let pending: Vec<(u64, usize, Vec<Value>)> = lock(&self.bootstraps)
            .iter()
            .filter(|bootstrap| bootstrap.sequence > replayed)
            .map(|bootstrap| (bootstrap.sequence, bootstrap.host, bootstrap.request.clone()))
            .collect();
        for (sequence, host, request) in pending {
            self.handle(host, request, None, &mut no_queries)?;
            self.mark_replayed(sequence);
        }

        Ok(())
    }

    fn mark_replayed(&self, sequence: u64) {
        PREPARED.with_borrow_mut(|prepared| {
            if let Some(prepared) = prepared.get_mut(&self.id) {
                prepared.replayed = prepared.replayed.max(sequence);
            }
        });
    }

    /// Asks the calling thread's interpreter whether it loaded a class that changed since it last checked, and starts it
    /// again on its next call when it did. Returns whether it starts again.
    fn reloaded(&self, prepared: Prepared) -> Result<bool, WorkerError> {
        let changed: Vec<Vec<u8>> = lock(&self.reloads)[prepared.reloads..].to_vec();
        if changed.is_empty() {
            return Ok(false);
        }

        let classes: Vec<&[u8]> = changed.iter().map(Vec::as_slice).collect();
        let answer = self.handle(0, reduction::class_loads_request(&classes)?, None, &mut no_queries)?;
        if reduction::decode_class_loads_response(&answer)? {
            tracing::debug!(
                interpreter = prepared.interpreter,
                "Starting an interpreter again: a class it loaded changed."
            );
            self.engine.reset(prepared.interpreter);
            PREPARED.with_borrow_mut(|prepared| prepared.remove(&self.id));
            return Ok(true);
        }

        PREPARED.with_borrow_mut(|held| {
            if let Some(held) = held.get_mut(&self.id) {
                held.reloads = prepared.reloads + changed.len();
            }
        });

        Ok(false)
    }

    /// The error for host `second`, whose file declares `class`, which an earlier host's file declared: a new
    /// interpreter loads the earlier hosts one by one to find which.
    fn clash(&self, second: usize, class: String) -> WorkerError {
        let earlier = Value::List(
            self.files[..second]
                .iter()
                .map(|host| Value::List(vec![Value::String(host.name.clone().into_bytes()), path_value(&host.file)]))
                .collect(),
        );
        let declarer = self.engine.call(
            DECLARER,
            &[earlier, Value::String(class.clone().into_bytes())],
            self.files[second].deadline,
            &mut refuse,
        );
        if let Some(interpreter) = self.engine.interpreter() {
            self.engine.reset(interpreter);
        }

        let first = match declarer {
            Ok(Value::Int(place)) => usize::try_from(place).ok().and_then(|place| self.files.get(place)),
            _ => None,
        };

        WorkerError::Clash {
            class,
            first: first.map_or_else(|| "an earlier host".to_owned(), |host| host.name.clone()),
            second: self.files[second].name.clone(),
        }
    }

    /// Serves `request`, a message of values, with the worker of host `host` in the calling thread's interpreter, which
    /// answers with one, answering its codebase queries with `handler`. `running` names the request for its deadline's
    /// error, unless the worker names the unit it started.
    fn handle(
        &self,
        host: usize,
        request: Vec<Value>,
        running: Option<&dyn Display>,
        handler: &mut dyn WorkerRequestHandler,
    ) -> Result<Vec<Value>, WorkerError> {
        let file = &self.files[host];
        let mut progressed = None;
        let index = i64::try_from(host).unwrap_or(i64::MAX);
        let returned = self.engine.call(
            HANDLE,
            &[Value::Int(index), Value::List(request)],
            file.deadline,
            &mut |asked| match asked.as_slice() {
                [Value::String(question), Value::List(query)] if question == b"query" => {
                    handler.handle(query).map(Value::List).map_err(|error| String::from_utf8_lossy(&error).into())
                }
                [Value::String(question), Value::String(unit)] if question == b"progress" => {
                    progressed = Some(String::from_utf8_lossy(unit).into_owned());
                    self.engine.restart_deadline();
                    Ok(Value::Null)
                }
                _ => Err("mago answers a `query` of one list or a `progress` of one string".to_owned()),
            },
        );

        match returned {
            Ok(Value::List(answer)) => match <[Value; 2]>::try_from(answer) {
                Ok([Value::Bool(true), Value::List(response)]) => Ok(response),
                Ok([Value::Bool(false), Value::String(message)]) => Err(WorkerError::Remote {
                    host: file.name.clone(),
                    message: String::from_utf8_lossy(&message).into(),
                }),
                _ => Err(unanswered(file)),
            },
            Ok(_) => Err(unanswered(file)),
            Err(EngineError::Deadline { duration, clock, frames }) => {
                let running = progressed.or_else(|| running.map(ToString::to_string));
                let running = match (running, started_hook(&frames)) {
                    (Some(running), Some(hook)) => Some(format!("{running}, in the hook `{hook}`")),
                    (None, Some(hook)) => Some(format!("the hook `{hook}`")),
                    (running, None) => running,
                };

                Err(WorkerError::Timeout { host: file.name.clone(), running, duration, clock })
            }
            Err(error) => Err(WorkerError::Failed { host: file.name.clone(), message: error.to_string() }),
        }
    }

    /// Runs the worker reducers of host `host`: collects each interpreter's state, on the thread it runs on, and reduces
    /// it all in the calling thread's interpreter. Returns the issues the reducers reported, as the analyzer protocol
    /// writes them, or none when no extension of the host has a reducer.
    fn reduce(&self, host: usize) -> Result<Vec<Value>, WorkerError> {
        let collect = || -> Option<Result<Vec<Value>, WorkerError>> {
            let current = self.engine.interpreter();
            PREPARED
                .with_borrow(|prepared| prepared.get(&self.id).copied())
                .filter(|p| Some(p.interpreter) == current)?;

            Some(self.handle(host, reduction::collect_request(), None, &mut no_queries))
        };
        let mut collected: Vec<Result<Vec<Value>, WorkerError>> = if self.pooled.load(Ordering::Acquire) {
            rayon::broadcast(|_| collect()).into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        if rayon::current_thread_index().is_none() {
            collected.extend(collect());
        }

        let responses = collected.into_iter().collect::<Result<Vec<_>, _>>()?;
        let mut reducers = false;
        for response in &responses {
            reducers |= reduction::decode_collect_response(response)?;
        }
        if !reducers {
            return Ok(Vec::new());
        }

        self.prepare()?;
        let answer = self.handle(host, reduction::reduce_request(&responses)?, None, &mut no_queries)?;

        reduction::decode_reduce_response(&answer)
    }
}

impl Drop for Hosts {
    /// Runs the worker reducers of each host [`Host::enable_worker_reduction`] armed, as a linter's are, whose issues no
    /// report holds, then ends the interpreter of every thread of the current pool, and the calling thread's, so stopped
    /// hosts keep no interpreter alive.
    fn drop(&mut self) {
        for (host, reducing) in self.reducing.iter().enumerate() {
            if reducing.load(Ordering::Acquire)
                && let Err(error) = self.reduce(host)
            {
                tracing::warn!(host = %self.files[host].name, error = %error, "Extension worker reduction failed.");
            }
        }

        let end = || {
            if let Some(prepared) = PREPARED.with_borrow_mut(|prepared| prepared.remove(&self.id)) {
                self.engine.reset(prepared.interpreter);
            }
        };
        if self.pooled.load(Ordering::Acquire) {
            rayon::broadcast(|_| end());
        }
        if rayon::current_thread_index().is_none() {
            end();
        }
    }
}

impl Host {
    /// The host's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.hosts.files[self.index].name
    }

    /// Sends `request`, a message of values, to the host's worker in every interpreter, this thread's now and each other
    /// one before its next request, and returns this thread's response.
    ///
    /// # Errors
    ///
    /// Returns an error when the interpreter can't be made ready or the worker fails the request.
    pub fn broadcast(&self, request: &[Value]) -> Result<Vec<Value>, WorkerError> {
        Ok(self.broadcast_sequence(None, &[request.to_vec()])?.pop().unwrap_or_default())
    }

    /// Sends `requests` in order as [`Self::broadcast`] sends one, and keeps each as a bootstrap. Those of a `group`
    /// replace the ones an earlier sequence of that group sent, so an interpreter that starts later receives only the
    /// latest.
    ///
    /// # Errors
    ///
    /// Returns an error when the interpreter can't be made ready or the worker fails a request.
    pub fn broadcast_sequence(
        &self,
        group: Option<u64>,
        requests: &[Vec<Value>],
    ) -> Result<Vec<Vec<Value>>, WorkerError> {
        let hosts = &self.hosts;
        hosts.prepare()?;
        let responses = requests
            .iter()
            .map(|request| hosts.handle(self.index, request.clone(), None, &mut no_queries))
            .collect::<Result<Vec<_>, _>>()?;

        let mut bootstraps = lock(&hosts.bootstraps);
        if group.is_some() {
            bootstraps.retain(|bootstrap| bootstrap.host != self.index || bootstrap.group != group);
        }
        for request in requests {
            let sequence = hosts.next_bootstrap.fetch_add(1, Ordering::Relaxed);
            bootstraps.push(Bootstrap { sequence, host: self.index, group, request: request.clone() });
            hosts.mark_replayed(sequence);
        }

        Ok(responses)
    }

    /// Sends `request`, a message of values, to the host's worker in the calling thread's interpreter, answering its
    /// codebase queries with `handler`. `running` names the request for its deadline's error, unless the worker names
    /// the unit it started.
    ///
    /// # Errors
    ///
    /// Returns an error when the interpreter can't be made ready, the worker fails the request, or the request runs past
    /// its deadline.
    pub fn request(
        &self,
        request: Vec<Value>,
        running: Option<&dyn Display>,
        handler: &mut dyn WorkerRequestHandler,
    ) -> Result<Vec<Value>, WorkerError> {
        self.hosts.prepare()?;
        self.hosts.handle(self.index, request, running, handler)
    }

    /// Takes in that the compiled files of `classes`, each a lowercase class name, changed: each interpreter that loaded
    /// one starts again before its next request, since PHP declares a class once.
    pub fn reload_holding(&self, classes: &[&[u8]]) {
        lock(&self.hosts.reloads).extend(classes.iter().map(|class| class.to_vec()));
    }

    /// Has the host's worker reducers run when mago stops the hosts.
    pub fn enable_worker_reduction(&self) {
        self.hosts.reducing[self.index].store(true, Ordering::Release);
    }

    /// Runs the host's worker reducers now, over what each interpreter gathered since its last collection, and returns
    /// the issues they reported, as the analyzer protocol writes them, or none when no extension of the host has a
    /// reducer.
    ///
    /// # Errors
    ///
    /// Returns an error when an interpreter can't collect, the reducing one can't be made ready, or a reducer fails.
    pub fn reduce(&self) -> Result<Vec<Value>, WorkerError> {
        self.hosts.reduce(self.index)
    }
}

/// What every host sets for each interpreter, which all hosts share: an environment variable, or a PHP setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shared {
    Variable,
    Setting,
}

impl Shared {
    fn of(self, host: &HostFile) -> &BTreeMap<String, String> {
        match self {
            Self::Variable => &host.environment,
            Self::Setting => &host.ini,
        }
    }

    /// The error of the hosts `first` and `second`, which set `name` to different values.
    fn clash(self, name: String, first: String, second: String) -> WorkerError {
        match self {
            Self::Variable => WorkerError::Variable { name, first, second },
            Self::Setting => WorkerError::Setting { name, first, second },
        }
    }
}

impl std::fmt::Display for Shared {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Variable => "environment variable",
            Self::Setting => "PHP setting",
        })
    }
}

/// The `kind` every host sets, by name, which every interpreter reads.
fn shared(hosts: &[HostFile], kind: Shared) -> Result<Vec<(CString, CString)>, WorkerError> {
    let mut set: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for host in hosts {
        for (name, value) in kind.of(host) {
            match set.get(name.as_str()) {
                Some((first, held)) if held != value => {
                    return Err(kind.clash(name.clone(), (*first).to_owned(), host.name.clone()));
                }
                Some(_) => {}
                None => {
                    set.insert(name, (&host.name, value));
                }
            }
        }
    }

    set.into_iter()
        .map(|(name, (host, value))| match (CString::new(name), CString::new(value)) {
            (Ok(name), Ok(value)) => Ok((name, value)),
            _ => Err(WorkerError::Load {
                host: host.to_owned(),
                message: format!("its {kind} `{name}` holds a NUL byte"),
                thrown: None,
            }),
        })
        .collect()
}

/// Writes mago's SDK into the folder of `cache` the SDK's hash names, unless it is there, and returns its preload
/// script. A folder is written beside it, named by the process and by `hosts`, the id of the [`Hosts`] that writes
/// it, and renamed into place, so a run that starts meanwhile never reads half of one, and two that start together
/// never write into one.
fn write_sdk(cache: &Path, hosts: u64) -> std::io::Result<PathBuf> {
    let folder = cache.join(format!("{SDK_HASH:016x}"));
    let preload = folder.join("preload.php");
    if preload.is_file() {
        return Ok(preload);
    }

    let partial = cache.join(format!("{SDK_HASH:016x}.{}.{hosts}", std::process::id()));
    for (path, contents) in SDK_FILES {
        let file = partial.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, contents)?;
    }
    std::fs::write(partial.join("preload.php"), SDK_PRELOAD)?;
    if let Err(error) = std::fs::rename(&partial, &folder) {
        std::fs::remove_dir_all(&partial)?;
        if !preload.is_file() {
            return Err(error);
        }
    }

    Ok(preload)
}

/// The class-like or function a fatal error's message says a file declared again, as PHP names it in "Cannot redeclare
/// class App\Order (previously declared in …)" or "Cannot declare class App\Order, because the name is already in use".
fn redeclared(message: &str) -> Option<String> {
    let (_, declared) = message.split_once("Cannot redeclare ").or_else(|| message.split_once("Cannot declare "))?;
    let (_, name) = declared.split_once(' ')?;
    let end = name.find([' ', ',', '('])?;

    Some(name[..end].to_owned())
}

/// The hook the worker started in a call that ran past its deadline in `frames`, innermost first: the class, or the
/// function of no class, of the frame the worker's own code called, which the worker held as it ran the hook.
fn started_hook(frames: &[Frame]) -> Option<&str> {
    frames.windows(2).find(|pair| pair[1].class == WORKER && pair[0].class != WORKER).map(|pair| {
        let hook = &pair[0];
        if hook.class.is_empty() { hook.function.as_str() } else { hook.class.as_str() }
    })
}

fn unanswered(file: &HostFile) -> WorkerError {
    WorkerError::Failed { host: file.name.clone(), message: "its worker answered with no response".to_owned() }
}

fn path_value(path: &Path) -> Value {
    Value::String(path.as_os_str().as_encoded_bytes().to_vec())
}

/// Answers a worker that asks a codebase query during a request that takes none.
fn no_queries(_: &[Value]) -> Result<Vec<Value>, Vec<u8>> {
    Err(b"mago answers no codebase query during this request".to_vec())
}

/// Answers a worker that asks something where mago asks nothing.
fn refuse(asked: Vec<Value>) -> Answer {
    Err(format!("mago answers nothing here, and the worker asked {asked:?}"))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The hosts of `files`, each a host's name and the PHP after its `<?php`, written into a folder of their own.
    fn hosts_of(folder: &str, files: &[(&str, &str)]) -> Arc<Hosts> {
        let directory = std::env::temp_dir().join(format!("mago-hosts-{folder}-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let hosts = files
            .iter()
            .map(|(name, source)| {
                let file = directory.join(format!("{name}.php"));
                std::fs::write(&file, format!("<?php\n\n{source}\n")).unwrap();
                HostFile {
                    name: (*name).to_owned(),
                    file,
                    deadline: Duration::from_secs(5),
                    environment: BTreeMap::new(),
                    ini: BTreeMap::new(),
                }
            })
            .collect();

        Hosts::new(hosts, directory.clone(), &directory.join("sdk")).unwrap()
    }

    const WORKER: &str = "(new Mago\\Sdk\\Worker(new Mago\\Sdk\\Extension('acme/tools', 'Tools', '1.0.0')))->run();";

    #[test]
    fn two_hosts_that_declare_one_class_are_refused_naming_both_and_the_class() {
        let declaring = format!("final class Shared {{}}\n\n{WORKER}");
        let hosts = hosts_of("clash", &[("routes", &declaring), ("models", &declaring)]);

        let error = hosts.hosts()[0].broadcast(&[Value::String(b"describe".to_vec())]).unwrap_err();

        assert_eq!(
            error.to_string(),
            "extension hosts `routes` and `models` both declare `Shared`, which PHP declares once: give one of them another name"
        );
    }

    /// A host's file runs with the PHP settings of its configuration, as `php -d` set them for its worker process: its
    /// `memory_limit`, and its `error_log`, the file PHP logs to, from the configuration's directory.
    #[test]
    fn a_host_runs_with_its_php_settings_and_logs_to_its_error_log() {
        let directory = std::env::temp_dir().join(format!("mago-hosts-ini-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let log = directory.join("host-errors.log");
        let _ = std::fs::remove_file(&log);
        let file = directory.join("reports.php");
        std::fs::write(
            &file,
            format!("<?php\n\nerror_log('memory_limit is ' . ini_get('memory_limit'));\n\n{WORKER}\n"),
        )
        .unwrap();
        let host = HostFile {
            name: "reports".to_owned(),
            file,
            deadline: Duration::from_secs(5),
            environment: BTreeMap::new(),
            ini: BTreeMap::from([
                ("memory_limit".to_owned(), "2G".to_owned()),
                ("error_log".to_owned(), "host-errors.log".to_owned()),
            ]),
        };
        let hosts = Hosts::new(vec![host], directory.clone(), &directory.join("sdk")).unwrap();

        let served = hosts.hosts()[0].broadcast(&[Value::String(b"describe".to_vec())]);

        assert!(matches!(served, Err(WorkerError::Remote { .. })), "the host loaded and its worker served: {served:?}");
        let logged = std::fs::read_to_string(&log).unwrap();
        assert!(logged.ends_with("] memory_limit is 2G\n"), "{logged}");
    }

    #[test]
    fn a_host_whose_file_registers_no_worker_is_refused_naming_it() {
        let hosts = hosts_of("silent", &[("routes", "final class Quiet {}")]);

        let error = hosts.hosts()[0].broadcast(&[Value::String(b"describe".to_vec())]).unwrap_err().to_string();

        assert!(error.starts_with("extension host `routes` could not load: "), "{error}");
        assert!(error.contains("must call Mago\\Sdk\\Worker::run() once"), "{error}");
    }

    #[test]
    fn a_fatal_error_names_the_class_like_or_function_a_file_declared_again() {
        let declared =
            "PHP Fatal error:  Cannot declare class App\\Order, because the name is already in use in /b.php";
        let redeclared_class =
            "Fatal error: Cannot redeclare class App\\Order (previously declared in /a.php:3) in /b.php";
        let redeclared_function = "Fatal error: Cannot redeclare function route() (previously declared in /a.php:3)";

        assert_eq!(redeclared(declared).as_deref(), Some("App\\Order"));
        assert_eq!(redeclared(redeclared_class).as_deref(), Some("App\\Order"));
        assert_eq!(redeclared(redeclared_function).as_deref(), Some("route"));
        assert_eq!(redeclared("PHP Fatal error:  Allowed memory size exhausted"), None);
    }

    /// The host `name`, which sets the environment variable `APP_ENV` and the PHP setting `memory_limit` to `value`.
    fn setting(name: &str, value: &str) -> HostFile {
        HostFile {
            name: name.to_owned(),
            file: PathBuf::from(format!("{name}.php")),
            deadline: Duration::from_secs(1),
            environment: BTreeMap::from([("APP_ENV".to_owned(), value.to_owned())]),
            ini: BTreeMap::from([("memory_limit".to_owned(), value.to_owned())]),
        }
    }

    #[test]
    fn two_hosts_that_set_one_variable_to_different_values_are_refused() {
        let same = shared(&[setting("routes", "2G"), setting("models", "2G")], Shared::Variable);
        let different = shared(&[setting("routes", "2G"), setting("models", "1G")], Shared::Variable);

        assert_eq!(same.map(|variables| variables.len()).ok(), Some(1));
        assert_eq!(
            different.err().map(|error| error.to_string()).as_deref(),
            Some(
                "extension hosts `routes` and `models` set the environment variable `APP_ENV` to different values, which their shared interpreters can't both read"
            )
        );
    }

    #[test]
    fn two_hosts_that_set_one_php_setting_to_different_values_are_refused() {
        let same = shared(&[setting("routes", "2G"), setting("models", "2G")], Shared::Setting);
        let different = shared(&[setting("routes", "2G"), setting("models", "1G")], Shared::Setting);

        assert_eq!(same.map(|settings| settings.len()).ok(), Some(1));
        assert_eq!(
            different.err().map(|error| error.to_string()).as_deref(),
            Some(
                "extension hosts `routes` and `models` set the PHP setting `memory_limit` to different values, which their shared interpreters can't both run with"
            )
        );
    }
}
