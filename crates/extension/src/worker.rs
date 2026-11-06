use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::fmt::Display;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
#[cfg(windows)]
use std::net::Ipv4Addr;
#[cfg(windows)]
use std::net::TcpListener;
#[cfg(any(windows, test))]
use std::net::TcpStream;
#[cfg(windows)]
use std::process::Child;
use std::process::ChildStderr;
#[cfg(not(windows))]
use std::process::ChildStdin;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;
use std::time::Instant;

use crate::command::WorkerCommand;
use crate::error::ProtocolError;
use crate::error::WorkerError;
use crate::pool::WorkerPoolOptions;
use crate::process_group::ProcessGroup;
use crate::protocol::Frame;
use crate::protocol::FrameFlags;
use crate::protocol::FrameKind;

const STATE_RUNNING: u8 = 0;
const STATE_STOPPING: u8 = 1;
const STATE_STOPPED: u8 = 2;
const STATE_FAILED: u8 = 3;
const IO_THREAD_STACK_SIZE: usize = 256 * 1024;
const INITIAL_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_micros(50);
const MAXIMUM_SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(1);
/// The longest a failed worker's error waits for the stderr reader to read the rest of the worker's stderr. A dead
/// worker's stderr ends when it exits, so only a process it started that still holds its stderr reaches this. Eight PHP
/// workers that died at startup on a loaded Mac waited from 5 µs to 179 µs.
const STDERR_END_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const INPUT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(any(windows, test))]
const INPUT_AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(2);
#[cfg(windows)]
const INPUT_ADDRESS_ENVIRONMENT_VARIABLE: &str = "MAGO_EXTENSION_INPUT_ADDRESS";
#[cfg(windows)]
const INPUT_TOKEN_ENVIRONMENT_VARIABLE: &str = "MAGO_EXTENSION_INPUT_TOKEN";

#[derive(Debug, Default)]
struct WorkerTelemetry {
    frames_sent: AtomicU64,
    frames_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    requests: AtomicU64,
    request_ns: AtomicU64,
    successful_responses: AtomicU64,
    remote_errors: AtomicU64,
    nested_requests: AtomicU64,
    nested_handler_ns: AtomicU64,
    notifications: AtomicU64,
    cancellations: AtomicU64,
    timeouts: AtomicU64,
    failures: AtomicU64,
    stderr_bytes: AtomicU64,
    forced_shutdowns: AtomicU64,
}

/// Handles a nested request sent by an extension worker while Mago is waiting
/// for the worker's outer response.
///
/// The successful or error bytes are returned to the worker as an opaque
/// response payload.
pub trait WorkerRequestHandler {
    /// Handles one nested worker request.
    ///
    /// # Errors
    ///
    /// Returns an opaque binary error payload to send back to the worker.
    fn handle(&mut self, request: &Frame) -> Result<Vec<u8>, Vec<u8>>;
}

impl<F> WorkerRequestHandler for F
where
    F: FnMut(&Frame) -> Result<Vec<u8>, Vec<u8>>,
{
    fn handle(&mut self, request: &Frame) -> Result<Vec<u8>, Vec<u8>> {
        self(request)
    }
}

enum WorkerStdin {
    #[cfg(not(windows))]
    Process(ChildStdin),
    #[cfg(windows)]
    Socket(TcpStream),
    #[cfg(test)]
    Test(std::net::TcpStream),
}

impl Write for WorkerStdin {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(not(windows))]
            Self::Process(stdin) => stdin.write(buffer),
            #[cfg(windows)]
            Self::Socket(stream) => stream.write(buffer),
            #[cfg(test)]
            Self::Test(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(not(windows))]
            Self::Process(stdin) => stdin.flush(),
            #[cfg(windows)]
            Self::Socket(stream) => stream.flush(),
            #[cfg(test)]
            Self::Test(stream) => stream.flush(),
        }
    }
}

/// Why Mago did not queue a frame for the worker.
enum WriteFailure {
    /// The worker failed or shut down, so Mago no longer writes to it.
    Stopped,
    Protocol(ProtocolError),
}

enum PendingEvent {
    Frame(Frame),
    Started,
    /// The worker failed or shut down, so it will never answer.
    Stopped,
}

struct StderrTail {
    bytes: VecDeque<u8>,
    capacity: usize,
    /// Whether the stderr reader has read the worker's stderr to its end, so the tail holds all the worker wrote.
    ended: bool,
}

impl StderrTail {
    fn new(capacity: usize) -> Self {
        Self { bytes: VecDeque::with_capacity(capacity.min(4096)), capacity, ended: false }
    }

    fn extend(&mut self, bytes: &[u8]) {
        if self.capacity == 0 {
            return;
        }

        if bytes.len() >= self.capacity {
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - self.capacity..]);
            return;
        }

        let excess = self.bytes.len().saturating_add(bytes.len()).saturating_sub(self.capacity);
        self.bytes.drain(..excess);
        self.bytes.extend(bytes);
    }

    fn display(&self) -> String {
        let bytes: Vec<_> = self.bytes.iter().copied().collect();
        String::from_utf8_lossy(&bytes).trim().to_owned()
    }
}

/// The failure `fail` builds for a worker, which it records as the worker's failure when it drops. A panic while `fail`
/// builds it still records what it has, so no request waits for the failure forever.
struct FailureRecord<'worker> {
    inner: &'worker WorkerInner,
    message: String,
}

impl Drop for FailureRecord<'_> {
    fn drop(&mut self) {
        let _result = self.inner.failure.set(std::mem::take(&mut self.message));
    }
}

struct WorkerInner {
    id: usize,
    /// The worker's host in the configuration, which every error of the worker names.
    host: String,
    state: AtomicU8,
    next_request_id: AtomicU64,
    in_flight: AtomicUsize,
    maximum_payload_size: usize,
    /// The frames the stdin writer writes to the worker, in the order they are queued. `None` once Mago stops writing
    /// to the worker.
    writer: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    /// The worker's process, with every process it started.
    child: Mutex<Option<ProcessGroup>>,
    pending: Mutex<BTreeMap<u64, mpsc::Sender<PendingEvent>>>,
    stderr: Mutex<StderrTail>,
    /// Notified when the stderr reader reaches the end of the worker's stderr.
    stderr_ended: Condvar,
    /// Why the worker failed, with the tail of its stderr. `fail` sets it once, after the worker's state turns failed.
    failure: OnceLock<String>,
    trace_enabled: bool,
    trace_summary_emitted: AtomicBool,
    telemetry: WorkerTelemetry,
    started_at: Option<Instant>,
    /// The request the worker last sent a progress frame for, when, and what it said the request started then.
    progress: Mutex<Option<(u64, Instant, String)>>,
}

impl WorkerInner {
    /// The later of `started_at` and the last progress of request `request_id`. The reader can route a request's
    /// first progress before the request's own thread wakes to record `started_at`, so the progress still counts.
    fn progressed_at(&self, request_id: u64, started_at: Instant) -> Instant {
        match &*lock(&self.progress) {
            Some((request, at, _)) if *request == request_id => (*at).max(started_at),
            _ => started_at,
        }
    }

    /// What the worker last said request `request_id` started, if it said anything.
    fn running(&self, request_id: u64) -> Option<String> {
        match &*lock(&self.progress) {
            Some((request, _, running)) if *request == request_id => Some(running.clone()),
            _ => None,
        }
    }

    fn send(&self, frame: &Frame) -> Result<(), WorkerError> {
        let written = self.write(lock(&self.writer).as_ref(), frame);
        self.finish_write(written)
    }

    /// Sends a top-level request under the next identifier and registers its response channel. Returns the
    /// identifier, and whether the worker starts the request at once: a worker serves requests in the order they
    /// arrive, so a request waits while one sent before it is still pending.
    fn send_request(&self, payload: Vec<u8>, sender: mpsc::Sender<PendingEvent>) -> Result<(u64, bool), WorkerError> {
        let (request_id, starts, written) = {
            let writer = lock(&self.writer);
            let request_id = self.next_request_id();
            let starts = {
                let mut pending = lock(&self.pending);
                pending.insert(request_id, sender);
                pending.len() == 1
            };
            let written = self.write(writer.as_ref(), &Frame::request(request_id, payload));
            (request_id, starts, written)
        };

        self.finish_write(written).inspect_err(|_| {
            self.take_pending(request_id);
        })?;

        Ok((request_id, starts))
    }

    /// Removes a request the worker no longer serves, and tells the request after it that the worker starts it now.
    fn take_pending(&self, request_id: u64) -> Option<mpsc::Sender<PendingEvent>> {
        let mut pending = lock(&self.pending);
        let sender = pending.remove(&request_id)?;
        if let Some((next, next_sender)) = pending.first_key_value()
            && *next > request_id
        {
            let _result = next_sender.send(PendingEvent::Started);
        }

        Some(sender)
    }

    fn next_request_id(&self) -> u64 {
        loop {
            let id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }

    /// Queues `frame` for the stdin writer under the writer's lock, so frames reach the worker in the order Mago
    /// numbered them, and never waits for the worker to read them. It fails with why the frame will not go out, and
    /// `finish_write` turns that into the error once the lock is released, as the error of a failed worker waits for
    /// `fail` to record the failure.
    fn write(&self, writer: Option<&mpsc::Sender<Vec<u8>>>, frame: &Frame) -> Result<(), WriteFailure> {
        if self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(WriteFailure::Stopped);
        }

        let writer = writer.ok_or(WriteFailure::Stopped)?;
        let mut bytes = Vec::new();
        frame.write_to(&mut bytes, self.maximum_payload_size).map_err(WriteFailure::Protocol)?;
        // The stdin writer stops at a failed write, which fails the worker first.
        writer.send(bytes).map_err(|_| WriteFailure::Stopped)
    }

    fn finish_write(&self, written: Result<(), WriteFailure>) -> Result<(), WorkerError> {
        match written {
            Ok(()) => Ok(()),
            Err(WriteFailure::Stopped) => Err(self.disconnected("worker is not running")),
            Err(WriteFailure::Protocol(source)) => {
                let error = WorkerError::Protocol { worker: self.id, source };
                self.fail(error.to_string());
                Err(error)
            }
        }
    }

    fn route(&self, frame: Frame) {
        if self.trace_enabled {
            self.telemetry.frames_received.fetch_add(1, Ordering::Relaxed);
            self.telemetry
                .bytes_received
                .fetch_add((crate::protocol::FRAME_HEADER_LENGTH + frame.payload.len()) as u64, Ordering::Relaxed);
        }
        if frame.kind == FrameKind::Shutdown {
            tracing::trace!(worker = self.id, "Extension worker requested host-side shutdown.");
            self.fail("worker requested shutdown");
            return;
        }

        if frame.kind == FrameKind::Notification {
            *lock(&self.progress) =
                Some((frame.parent_id, Instant::now(), String::from_utf8_lossy(&frame.payload).into_owned()));
        }

        let pending_id = match frame.kind {
            FrameKind::Response | FrameKind::Cancel => frame.id,
            FrameKind::Request | FrameKind::Notification => frame.parent_id,
            FrameKind::Shutdown => return,
        };
        if pending_id == 0 {
            tracing::warn!(worker = self.id, kind = ?frame.kind, "Ignoring unassociated extension worker frame.");
            return;
        }

        let sender = if frame.kind == FrameKind::Response {
            self.take_pending(pending_id)
        } else {
            lock(&self.pending).get(&pending_id).cloned()
        };

        if let Some(sender) = sender {
            let _result = sender.send(PendingEvent::Frame(frame));
        } else {
            tracing::debug!(worker = self.id, request = pending_id, "Ignoring late extension worker frame.");
        }
    }

    /// Stops the worker for `reason`, and records the reason with the tail of the worker's stderr as its failure, which
    /// every request the worker fails from then on reads.
    fn fail(&self, reason: impl Into<String>) {
        if self.state.compare_exchange(STATE_RUNNING, STATE_FAILED, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }

        let mut failure = FailureRecord { inner: self, message: reason.into() };
        if self.trace_enabled {
            self.telemetry.failures.fetch_add(1, Ordering::Relaxed);
        }

        // Closing stdin first lets a process the worker started, which reads stdin to its end, exit and close stderr. The
        // stdin writer closes it once it has written the frames queued before.
        lock(&self.writer).take();
        if let Some(group) = lock(&self.child).as_mut() {
            let _result = group.leader().kill();
        }

        let stderr_wait_start = Instant::now();
        let (stderr, ended) = self.stderr_tail();
        let stderr_wait = stderr_wait_start.elapsed();
        // What the worker started and still runs, such as the worker a wrapper forked, can hold the pipe the stdin
        // writer is stuck on, and the worker's stdout and stderr, so the failure ends it too.
        if let Some(group) = lock(&self.child).as_mut() {
            let _result = group.end();
        }
        if !ended {
            failure.message =
                format!("{}; stderr still open after {} ms", failure.message, STDERR_END_TIMEOUT.as_millis());
        }
        if !stderr.is_empty() {
            failure.message.push_str("; stderr: ");
            failure.message.push_str(&stderr);
        }

        tracing::trace!(worker = self.id, reason = %failure.message, ?stderr_wait, "Extension worker failed.");
        drop(failure);
        let pending = std::mem::take(&mut *lock(&self.pending));
        for sender in pending.into_values() {
            let _result = sender.send(PendingEvent::Stopped);
        }
    }

    /// Marks the worker's stderr read to its end, so a failure stops waiting for more of it.
    fn end_stderr(&self) {
        lock(&self.stderr).ended = true;
        self.stderr_ended.notify_all();
    }

    /// The tail of the worker's stderr, and whether it holds all the worker wrote. A worker that dies writes why just
    /// before it exits, and the stdout reader can see it exit before the stderr reader has read that, so this waits up
    /// to `STDERR_END_TIMEOUT` for the stderr reader to reach the end.
    fn stderr_tail(&self) -> (String, bool) {
        let (tail, _) = self
            .stderr_ended
            .wait_timeout_while(lock(&self.stderr), STDERR_END_TIMEOUT, |tail| !tail.ended)
            .unwrap_or_else(PoisonError::into_inner);

        (tail.display(), tail.ended)
    }

    /// The error for a request the worker can't serve. A worker that failed says why it failed, which `fail` records
    /// just after the worker's state turns failed. A worker that Mago stopped says `message`.
    fn disconnected(&self, message: impl Into<String>) -> WorkerError {
        let failure = if self.state.load(Ordering::Acquire) == STATE_FAILED {
            Some(self.failure.wait())
        } else {
            self.failure.get()
        };

        WorkerError::Disconnected {
            host: self.host.clone(),
            worker: self.id,
            message: failure.map_or_else(|| message.into(), Clone::clone),
        }
    }

    fn emit_trace_summary(&self) {
        if !self.trace_enabled || self.trace_summary_emitted.swap(true, Ordering::AcqRel) {
            return;
        }

        let requests = self.telemetry.requests.load(Ordering::Relaxed);
        let request_ns = self.telemetry.request_ns.load(Ordering::Relaxed);
        let nested_requests = self.telemetry.nested_requests.load(Ordering::Relaxed);
        let nested_ns = self.telemetry.nested_handler_ns.load(Ordering::Relaxed);
        tracing::trace!(
            worker = self.id,
            requests,
            successful_responses = self.telemetry.successful_responses.load(Ordering::Relaxed),
            remote_errors = self.telemetry.remote_errors.load(Ordering::Relaxed),
            timeouts = self.telemetry.timeouts.load(Ordering::Relaxed),
            cancellations = self.telemetry.cancellations.load(Ordering::Relaxed),
            total_request_ms = nanos_millis(request_ns),
            average_request_micros = average_micros(request_ns, requests),
            "Extension worker request summary."
        );
        tracing::trace!(
            worker = self.id,
            frames_sent = self.telemetry.frames_sent.load(Ordering::Relaxed),
            frames_received = self.telemetry.frames_received.load(Ordering::Relaxed),
            bytes_sent = self.telemetry.bytes_sent.load(Ordering::Relaxed),
            bytes_received = self.telemetry.bytes_received.load(Ordering::Relaxed),
            notifications = self.telemetry.notifications.load(Ordering::Relaxed),
            "Extension worker protocol summary."
        );
        tracing::trace!(
            worker = self.id,
            nested_requests,
            nested_handler_ms = nanos_millis(nested_ns),
            average_nested_handler_micros = average_micros(nested_ns, nested_requests),
            failures = self.telemetry.failures.load(Ordering::Relaxed),
            stderr_bytes = self.telemetry.stderr_bytes.load(Ordering::Relaxed),
            forced_shutdowns = self.telemetry.forced_shutdowns.load(Ordering::Relaxed),
            lifetime = ?self.started_at.map(|start| start.elapsed()).unwrap_or_default(),
            "Extension worker lifecycle summary."
        );
    }
}

struct WorkerRequestTrace<'worker> {
    inner: &'worker WorkerInner,
    started_at: Instant,
}

impl Drop for WorkerRequestTrace<'_> {
    fn drop(&mut self) {
        self.inner.telemetry.requests.fetch_add(1, Ordering::Relaxed);
        self.inner.telemetry.request_ns.fetch_add(duration_nanos(self.started_at.elapsed()), Ordering::Relaxed);
    }
}

pub(crate) struct Worker {
    inner: Arc<WorkerInner>,
    request_timeout: Duration,
    shutdown_timeout: Duration,
    reader_thread: Mutex<Option<JoinHandle<()>>>,
    stderr_thread: Mutex<Option<JoinHandle<()>>>,
    writer_thread: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    pub fn spawn(id: usize, command: &WorkerCommand, options: &WorkerPoolOptions) -> Result<Arc<Self>, WorkerError> {
        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        let started_at = trace_enabled.then(Instant::now);
        tracing::trace!(
            worker = id,
            program = ?command.program(),
            arguments = command.arguments().len(),
            working_directory = ?command.current_directory(),
            "Spawning extension worker process."
        );
        let mut process = command.build();
        #[cfg(windows)]
        let (input_listener, input_address, input_token) = prepare_windows_input()
            .map_err(|source| WorkerError::Spawn { program: command.program().to_owned(), source })?;
        #[cfg(windows)]
        process
            .env(INPUT_ADDRESS_ENVIRONMENT_VARIABLE, input_address)
            .env(INPUT_TOKEN_ENVIRONMENT_VARIABLE, &input_token);

        let mut group = ProcessGroup::spawn(&mut process)
            .map_err(|source| WorkerError::Spawn { program: command.program().to_owned(), source })?;

        let process_stdin = take_pipe(id, "stdin", group.leader().stdin.take())?;
        let stdout = take_pipe(id, "stdout", group.leader().stdout.take())?;
        let stderr = take_pipe(id, "stderr", group.leader().stderr.take())?;
        let process_id = group.leader().id();
        #[cfg(windows)]
        let stdin = match accept_windows_input(&input_listener, &input_token, group.leader()) {
            Ok(stream) => {
                drop(process_stdin);
                WorkerStdin::Socket(stream)
            }
            Err(source) => return Err(WorkerError::Spawn { program: command.program().to_owned(), source }),
        };
        #[cfg(not(windows))]
        let stdin = WorkerStdin::Process(process_stdin);

        let (writer, queued) = mpsc::channel();
        let inner = Arc::new(WorkerInner {
            id,
            host: options.host.clone(),
            state: AtomicU8::new(STATE_RUNNING),
            next_request_id: AtomicU64::new(1),
            in_flight: AtomicUsize::new(0),
            maximum_payload_size: options.maximum_payload_size,
            writer: Mutex::new(Some(writer)),
            child: Mutex::new(Some(group)),
            pending: Mutex::new(BTreeMap::new()),
            stderr: Mutex::new(StderrTail::new(options.stderr_tail_size)),
            stderr_ended: Condvar::new(),
            failure: OnceLock::new(),
            trace_enabled,
            trace_summary_emitted: AtomicBool::new(false),
            telemetry: WorkerTelemetry::default(),
            started_at,
            progress: Mutex::new(None),
        });
        tracing::trace!(worker = id, process_id, "Extension worker process spawned.");

        let reader_inner = Arc::clone(&inner);
        let reader_thread = std::thread::Builder::new()
            .name(format!("mago-extension-worker-{id}-stdout"))
            .stack_size(IO_THREAD_STACK_SIZE)
            .spawn(move || read_worker_stream(&reader_inner, stdout))
            .map_err(|source| {
                // No stderr reader runs, so none will reach the end of the worker's stderr.
                inner.end_stderr();
                inner.fail("failed to start stdout reader thread");
                reap_child(&inner);
                WorkerError::ThreadSpawn { worker: id, role: "stdout reader", source }
            })?;

        let stderr_inner = Arc::clone(&inner);
        let stderr_thread = match std::thread::Builder::new()
            .name(format!("mago-extension-worker-{id}-stderr"))
            .stack_size(IO_THREAD_STACK_SIZE)
            .spawn(move || read_worker_stderr(&stderr_inner, stderr))
        {
            Ok(thread) => thread,
            Err(source) => {
                inner.end_stderr();
                inner.fail("failed to start stderr reader thread");
                reap_child(&inner);
                let _result = reader_thread.join();
                return Err(WorkerError::ThreadSpawn { worker: id, role: "stderr reader", source });
            }
        };

        let writer_inner = Arc::clone(&inner);
        let writer_thread = match std::thread::Builder::new()
            .name(format!("mago-extension-worker-{id}-stdin"))
            .stack_size(IO_THREAD_STACK_SIZE)
            .spawn(move || write_worker_stdin(&writer_inner, stdin, queued))
        {
            Ok(thread) => thread,
            Err(source) => {
                inner.fail("failed to start stdin writer thread");
                reap_child(&inner);
                let _result = reader_thread.join();
                let _result = stderr_thread.join();
                return Err(WorkerError::ThreadSpawn { worker: id, role: "stdin writer", source });
            }
        };

        let worker = Arc::new(Self {
            inner,
            request_timeout: options.request_timeout,
            shutdown_timeout: options.shutdown_timeout,
            reader_thread: Mutex::new(Some(reader_thread)),
            stderr_thread: Mutex::new(Some(stderr_thread)),
            writer_thread: Mutex::new(Some(writer_thread)),
        });
        if let Some(start) = started_at {
            tracing::trace!(worker = id, elapsed = ?start.elapsed(), "Extension worker I/O threads started.");
        }

        Ok(worker)
    }

    pub fn id(&self) -> usize {
        self.inner.id
    }

    pub fn is_running(&self) -> bool {
        self.inner.state.load(Ordering::Acquire) == STATE_RUNNING
    }

    pub fn in_flight(&self) -> usize {
        self.inner.in_flight.load(Ordering::Relaxed)
    }

    /// Sends `payload` and waits for its response. `running` names what the request runs, for its deadline's error
    /// when the worker names nothing it started.
    pub fn request(&self, payload: Vec<u8>, running: Option<&dyn Display>) -> Result<Vec<u8>, WorkerError> {
        self.request_inner::<fn(&Frame) -> Result<Vec<u8>, Vec<u8>>>(payload, running, None)
    }

    /// Sends `payload`, as [`Self::request`] does, and answers the worker's nested requests with `handler`.
    pub fn request_with_handler<H>(
        &self,
        payload: Vec<u8>,
        running: Option<&dyn Display>,
        handler: &mut H,
    ) -> Result<Vec<u8>, WorkerError>
    where
        H: WorkerRequestHandler,
    {
        self.request_inner(payload, running, Some(handler))
    }

    fn request_inner<H>(
        &self,
        payload: Vec<u8>,
        running: Option<&dyn Display>,
        mut handler: Option<&mut H>,
    ) -> Result<Vec<u8>, WorkerError>
    where
        H: WorkerRequestHandler,
    {
        if !self.is_running() {
            return Err(self.inner.disconnected("worker is not running"));
        }

        let _trace =
            self.inner.trace_enabled.then(|| WorkerRequestTrace { inner: &self.inner, started_at: Instant::now() });
        let (sender, receiver) = mpsc::channel();
        let (request_id, starts) = self.inner.send_request(payload, sender)?;

        // The deadline is the longest one unit of a request may run, as Jest's `testTimeout` bounds each test rather
        // than the run. It runs from the request's start, not while the request waits behind earlier ones, and again
        // from each progress frame the worker sends for the request as it starts one of its units: a file's hooks in
        // an after-file batch, one plugin's initialization, before-analysis or after-analysis hooks, a codebase-scan
        // hook, or an extension's worker reducer. Nested requests are not progress, so a hook that keeps asking Mago
        // for metadata and never finishes its file fails.
        let mut started_at = starts.then(Instant::now);
        loop {
            let event = match started_at {
                Some(started_at) => {
                    let progressed_at = self.inner.progressed_at(request_id, started_at);
                    let Some(remaining) = self.request_timeout.checked_sub(progressed_at.elapsed()) else {
                        return Err(self.timeout(request_id, running));
                    };
                    receiver.recv_timeout(remaining)
                }
                None => receiver.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
            };

            match event {
                Ok(PendingEvent::Started) => {
                    started_at.get_or_insert_with(Instant::now);
                }
                Ok(PendingEvent::Frame(frame)) => match frame.kind {
                    FrameKind::Response => {
                        if frame.flags.contains(FrameFlags::ERROR) {
                            if self.inner.trace_enabled {
                                self.inner.telemetry.remote_errors.fetch_add(1, Ordering::Relaxed);
                            }
                            return Err(WorkerError::Remote {
                                host: self.inner.host.clone(),
                                worker: self.id(),
                                request: request_id,
                                payload: frame.payload,
                            });
                        }

                        if self.inner.trace_enabled {
                            self.inner.telemetry.successful_responses.fetch_add(1, Ordering::Relaxed);
                        }
                        return Ok(frame.payload);
                    }
                    FrameKind::Request => {
                        if self.inner.trace_enabled {
                            self.inner.telemetry.nested_requests.fetch_add(1, Ordering::Relaxed);
                        }
                        let Some(handler) = handler.as_deref_mut() else {
                            let response = Frame::error(
                                frame.id,
                                request_id,
                                b"Mago did not install a handler for nested worker requests".to_vec(),
                            );
                            let _result = self.inner.send(&response);
                            self.inner.take_pending(request_id);
                            return Err(WorkerError::UnexpectedRequest { worker: self.id(), request: frame.id });
                        };

                        let handler_start = self.inner.trace_enabled.then(Instant::now);
                        let response = match handler.handle(&frame) {
                            Ok(payload) => Frame::response(frame.id, request_id, payload),
                            Err(payload) => Frame::error(frame.id, request_id, payload),
                        };
                        if let Some(start) = handler_start {
                            self.inner
                                .telemetry
                                .nested_handler_ns
                                .fetch_add(duration_nanos(start.elapsed()), Ordering::Relaxed);
                        }
                        self.inner.send(&response)?;
                    }
                    FrameKind::Notification => {
                        if self.inner.trace_enabled {
                            self.inner.telemetry.notifications.fetch_add(1, Ordering::Relaxed);
                        }
                        tracing::trace!(worker = self.id(), request = request_id, "Worker notification received.");
                    }
                    FrameKind::Cancel => {
                        if self.inner.trace_enabled {
                            self.inner.telemetry.cancellations.fetch_add(1, Ordering::Relaxed);
                        }
                        self.inner.take_pending(request_id);
                        return Err(WorkerError::Cancelled { worker: self.id(), request: request_id });
                    }
                    FrameKind::Shutdown => {
                        self.inner.fail("worker requested shutdown");
                    }
                },
                Ok(PendingEvent::Stopped) => {
                    return Err(self.inner.disconnected("worker shut down"));
                }
                // The worker may have made progress on another request in the meantime, which the next pass reads.
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.inner.take_pending(request_id);
                    return Err(self.inner.disconnected("response channel closed"));
                }
            }
        }
    }

    /// Fails request `request_id`, which made no progress within the deadline. The error names the unit the worker
    /// last said the request started, or else what Mago named the request, `running`, and so does the failure every
    /// other request of the worker reads. The request's number is protocol detail, so only the trace log names it.
    fn timeout(&self, request_id: u64, running: Option<&dyn Display>) -> WorkerError {
        if self.inner.trace_enabled {
            self.inner.telemetry.timeouts.fetch_add(1, Ordering::Relaxed);
        }
        let running = self.inner.running(request_id).or_else(|| running.map(ToString::to_string));
        tracing::trace!(worker = self.id(), request = request_id, running, timeout = ?self.request_timeout, "Extension worker request timed out.");
        self.inner.take_pending(request_id);
        let _result = self.inner.send(&Frame::cancel(request_id));
        let mut reason = format!("made no progress for {} ms", self.request_timeout.as_millis());
        if let Some(running) = &running {
            reason.push_str(" on ");
            reason.push_str(running);
        }
        self.inner.fail(reason);

        WorkerError::Timeout {
            host: self.inner.host.clone(),
            worker: self.id(),
            running,
            duration: self.request_timeout,
        }
    }

    pub fn reserve(self: &Arc<Self>) -> WorkerReservation {
        self.inner.in_flight.fetch_add(1, Ordering::Relaxed);
        WorkerReservation { worker: Arc::clone(self) }
    }

    pub fn shutdown(&self) {
        self.begin_shutdown();
        self.finish_shutdown(Instant::now(), self.shutdown_timeout);
    }

    pub fn begin_shutdown(&self) {
        let mut state = self.inner.state.load(Ordering::Acquire);
        loop {
            match state {
                STATE_RUNNING | STATE_FAILED => {
                    // A failed worker records why before it stops, so a request it fails while it stops reads why.
                    if state == STATE_FAILED {
                        self.inner.failure.wait();
                    }
                    match self.inner.state.compare_exchange(state, STATE_STOPPING, Ordering::AcqRel, Ordering::Acquire)
                    {
                        Ok(_) => break,
                        Err(actual) => state = actual,
                    }
                }
                STATE_STOPPING | STATE_STOPPED => return,
                _ => return,
            }
        }

        tracing::trace!(worker = self.id(), previous_state = state, "Beginning extension worker shutdown.");

        // The stdin writer writes the shutdown frame after the frames queued before it, then closes stdin.
        if let Some(writer) = lock(&self.inner.writer).take()
            && state == STATE_RUNNING
        {
            let mut shutdown = Vec::new();
            let _result = Frame::shutdown().write_to(&mut shutdown, self.inner.maximum_payload_size);
            let _result = writer.send(shutdown);
        }
    }

    pub fn finish_shutdown(&self, started_at: Instant, timeout: Duration) {
        if self.inner.state.load(Ordering::Acquire) == STATE_STOPPED {
            self.join_threads();
            self.inner.emit_trace_summary();
            return;
        }

        let trace_start = self.inner.trace_enabled.then(Instant::now);
        let mut poll_interval = INITIAL_SHUTDOWN_POLL_INTERVAL;
        let exited_gracefully = loop {
            let exited = lock(&self.inner.child).as_ref().is_none_or(ProcessGroup::leader_exited);
            let elapsed = started_at.elapsed();
            if exited || elapsed >= timeout {
                break exited;
            }

            std::thread::sleep(poll_interval.min(timeout.saturating_sub(elapsed)));
            poll_interval = (poll_interval * 2).min(MAXIMUM_SHUTDOWN_POLL_INTERVAL);
        };

        let child = lock(&self.inner.child).take();
        if let Some(group) = child {
            if !group.leader_exited() {
                if self.inner.trace_enabled {
                    self.inner.telemetry.forced_shutdowns.fetch_add(1, Ordering::Relaxed);
                }
                tracing::trace!(worker = self.id(), "Killing extension worker after shutdown grace period.");
            }
            // Dropping the group ends it, whether its leader still runs past the grace period or exited and left a
            // process it started running, which would keep the worker's pipes open, and then reaps the leader.
            drop(group);
        }

        self.inner.state.store(STATE_STOPPED, Ordering::Release);
        let pending = std::mem::take(&mut *lock(&self.inner.pending));
        for sender in pending.into_values() {
            let _result = sender.send(PendingEvent::Stopped);
        }

        self.join_threads();
        if let Some(start) = trace_start {
            tracing::trace!(
                worker = self.id(),
                graceful = exited_gracefully,
                elapsed = ?start.elapsed(),
                "Extension worker shutdown completed."
            );
        }
        self.inner.emit_trace_summary();
    }

    #[cfg(test)]
    fn from_streams(
        id: usize,
        reader: impl Read + Send + 'static,
        writer: std::net::TcpStream,
        options: &WorkerPoolOptions,
    ) -> Arc<Self> {
        let (frames, queued) = mpsc::channel();
        let inner = Arc::new(WorkerInner {
            id,
            host: options.host.clone(),
            state: AtomicU8::new(STATE_RUNNING),
            next_request_id: AtomicU64::new(1),
            in_flight: AtomicUsize::new(0),
            maximum_payload_size: options.maximum_payload_size,
            writer: Mutex::new(Some(frames)),
            child: Mutex::new(None),
            pending: Mutex::new(BTreeMap::new()),
            // A peer on streams has no stderr.
            stderr: Mutex::new(StderrTail { ended: true, ..StderrTail::new(options.stderr_tail_size) }),
            stderr_ended: Condvar::new(),
            failure: OnceLock::new(),
            trace_enabled: tracing::enabled!(tracing::Level::TRACE),
            trace_summary_emitted: AtomicBool::new(false),
            telemetry: WorkerTelemetry::default(),
            started_at: tracing::enabled!(tracing::Level::TRACE).then(Instant::now),
            progress: Mutex::new(None),
        });
        let reader_inner = Arc::clone(&inner);
        let reader_thread = std::thread::spawn(move || read_worker_stream(&reader_inner, reader));
        let writer_inner = Arc::clone(&inner);
        let writer_thread = std::thread::spawn(move || {
            let closer = writer.try_clone();
            write_worker_stdin(&writer_inner, WorkerStdin::Test(writer), queued);
            // The stdout reader holds a handle of the same stream, so the peer sees its stdin end, as a worker sees
            // its pipe close, only once Mago shuts down the stream's writing half.
            if let Ok(closer) = closer {
                let _result = closer.shutdown(std::net::Shutdown::Write);
            }
        });

        Arc::new(Self {
            inner,
            request_timeout: options.request_timeout,
            shutdown_timeout: options.shutdown_timeout,
            reader_thread: Mutex::new(Some(reader_thread)),
            stderr_thread: Mutex::new(None),
            writer_thread: Mutex::new(Some(writer_thread)),
        })
    }

    fn join_threads(&self) {
        let threads = [&self.reader_thread, &self.stderr_thread, &self.writer_thread].map(|thread| lock(thread).take());
        if threads.iter().all(Option::is_none) {
            return;
        }

        tracing::trace!(worker = self.id(), "Joining extension worker I/O threads.");
        for thread in threads.into_iter().flatten() {
            let _result = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) struct WorkerReservation {
    worker: Arc<Worker>,
}

impl WorkerReservation {
    pub fn worker(&self) -> &Worker {
        &self.worker
    }

    pub fn matches(&self, worker: &Arc<Worker>) -> bool {
        Arc::ptr_eq(&self.worker, worker)
    }
}

impl Drop for WorkerReservation {
    fn drop(&mut self) {
        self.worker.inner.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(windows)]
fn prepare_windows_input() -> std::io::Result<(TcpListener, String, String)> {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?.to_string();
    let random = rand::random::<[u8; 16]>();
    let mut token = String::with_capacity(random.len() * 2);
    for byte in random {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }

    Ok((listener, address, token))
}

#[cfg(windows)]
fn accept_windows_input(listener: &TcpListener, token: &str, child: &mut Child) -> std::io::Result<TcpStream> {
    let deadline = Instant::now() + INPUT_CONNECT_TIMEOUT;
    loop {
        match listener.accept() {
            Ok((mut stream, peer)) => {
                if !peer.ip().is_loopback() {
                    continue;
                }

                if !authenticate_windows_input(&mut stream, token)? {
                    continue;
                }

                stream.set_nodelay(true)?;
                tracing::trace!(process = child.id(), %peer, "Extension worker connected its Windows input stream.");
                return Ok(stream);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => return Err(error),
        }

        if let Some(status) = child.try_wait()? {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("extension worker exited with {status} before connecting its Windows input stream"),
            ));
        }

        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "extension worker did not connect its Windows input stream",
            ));
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(any(windows, test))]
fn authenticate_windows_input(stream: &mut TcpStream, token: &str) -> std::io::Result<bool> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(INPUT_AUTHENTICATION_TIMEOUT))?;
    let mut received = vec![0; token.len()];
    stream.read_exact(&mut received)?;
    stream.set_read_timeout(None)?;
    Ok(received == token.as_bytes())
}

/// The pipe `name` of worker `id`. A missing pipe fails the spawn, which drops the worker's group and so ends the
/// worker.
fn take_pipe<T>(id: usize, name: &'static str, pipe: Option<T>) -> Result<T, WorkerError> {
    pipe.ok_or(WorkerError::MissingPipe { worker: id, pipe: name })
}

/// Writes each frame `queued` holds to the worker's stdin, in order, on a thread of its own: a worker that stops reading
/// its stdin holds this thread, not the request that queued the frame, so the request still fails at its deadline. A
/// worker that failed gets none of the frames still queued, as a timed-out request's cancellation, since `fail` ends
/// it. Stdin closes once Mago stops queueing frames and those queued before are written, once the worker fails, or
/// once a write fails, which fails the worker.
fn write_worker_stdin(inner: &WorkerInner, mut stdin: WorkerStdin, queued: mpsc::Receiver<Vec<u8>>) {
    match &stdin {
        #[cfg(not(windows))]
        WorkerStdin::Process(pipe) => crate::process_group::block_sigpipe(pipe),
        #[cfg(windows)]
        WorkerStdin::Socket(_) => {}
        #[cfg(test)]
        WorkerStdin::Test(_) => {}
    }
    tracing::trace!(worker = inner.id, "Extension worker stdin writer started.");
    for frame in queued {
        if inner.state.load(Ordering::Acquire) == STATE_FAILED {
            break;
        }
        if let Err(error) = stdin.write_all(&frame).and_then(|()| stdin.flush()) {
            tracing::trace!(worker = inner.id, error = %error, "Extension worker stdin writer failed.");
            // A worker that exited closed the pipe Mago writes its requests to.
            inner.fail(format!("failed to write to the worker: {error}"));
            return;
        }
        if inner.trace_enabled {
            inner.telemetry.frames_sent.fetch_add(1, Ordering::Relaxed);
            inner.telemetry.bytes_sent.fetch_add(frame.len() as u64, Ordering::Relaxed);
        }
    }

    tracing::trace!(worker = inner.id, "Extension worker stdin writer closed stdin.");
}

fn read_worker_stream(inner: &WorkerInner, stream: impl Read) {
    tracing::trace!(worker = inner.id, "Extension worker stdout reader started.");
    let mut reader = BufReader::new(stream);
    loop {
        match Frame::read_from(&mut reader, inner.maximum_payload_size) {
            Ok(Some(frame)) => inner.route(frame),
            Ok(None) => {
                if inner.state.load(Ordering::Acquire) == STATE_RUNNING {
                    inner.fail("worker closed stdout");
                }
                tracing::trace!(worker = inner.id, "Extension worker stdout reader reached EOF.");
                return;
            }
            Err(error) => {
                tracing::trace!(worker = inner.id, error = %error, "Extension worker stdout reader failed.");
                inner.fail(format!("failed to read worker frame: {error}"));
                return;
            }
        }
    }
}

fn read_worker_stderr(inner: &WorkerInner, stderr: ChildStderr) {
    tracing::trace!(worker = inner.id, "Extension worker stderr reader started.");
    let mut reader = BufReader::new(stderr);
    let mut buffer = [0u8; 4096];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => {
                tracing::trace!(worker = inner.id, "Extension worker stderr reader reached EOF.");
                break;
            }
            Err(error) => {
                tracing::trace!(worker = inner.id, error = %error, "Extension worker stderr reader failed.");
                break;
            }
            Ok(read) => {
                if inner.trace_enabled {
                    inner.telemetry.stderr_bytes.fetch_add(read as u64, Ordering::Relaxed);
                }
                lock(&inner.stderr).extend(&buffer[..read]);
            }
        }
    }

    inner.end_stderr();
}

fn reap_child(inner: &WorkerInner) {
    tracing::trace!(worker = inner.id, "Reaping failed extension worker process.");
    // Dropping the group ends it and reaps its leader.
    let child = lock(&inner.child).take();
    drop(child);
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn duration_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[allow(clippy::cast_precision_loss, clippy::float_arithmetic)]
pub(crate) fn nanos_millis(nanos: u64) -> f64 {
    nanos as f64 / 1_000_000.0
}

pub(crate) fn average_micros(nanos: u64, count: u64) -> u64 {
    nanos.checked_div(count).unwrap_or(0) / 1_000
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::net::TcpListener;
    use std::net::TcpStream;
    use std::sync::Barrier;

    use super::*;

    /// The deadline of a test whose peer answers at once, the hosts' default, so a busy machine never reaches it.
    const UNREACHED_DEADLINE: Duration = Duration::from_secs(30);

    #[test]
    fn stderr_tail_keeps_only_the_configured_suffix() {
        let mut tail = StderrTail::new(5);
        tail.extend(b"abc");
        tail.extend(b"defg");

        assert_eq!(tail.display(), "cdefg");
    }

    #[test]
    fn stderr_tail_handles_a_chunk_larger_than_its_capacity() {
        let mut tail = StderrTail::new(4);
        tail.extend(b"abcdefgh");

        assert_eq!(tail.display(), "efgh");
    }

    #[test]
    fn windows_input_authentication_waits_for_a_delayed_token() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let address = listener.local_addr().expect("listener should have an address");
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(address).expect("peer should connect");
            std::thread::sleep(Duration::from_millis(25));
            stream.write_all(b"0123456789abcdef0123456789abcdef").expect("token should be written");
        });

        let (mut stream, _) = listener.accept().expect("host should accept peer");
        stream.set_nonblocking(true).expect("accepted stream should become non-blocking");

        assert!(
            authenticate_windows_input(&mut stream, "0123456789abcdef0123456789abcdef")
                .expect("authentication should wait for the token")
        );

        sender.join().expect("sender should finish");
    }

    #[test]
    fn routes_multiplexed_responses_by_request_id() {
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(UNREACHED_DEADLINE);
        let barrier = Arc::new(Barrier::new(3));

        let first_worker = Arc::clone(&worker);
        let first_barrier = Arc::clone(&barrier);
        let first = std::thread::spawn(move || {
            first_barrier.wait();
            first_worker.request(b"first".to_vec(), None).expect("first request should succeed")
        });
        let second_worker = Arc::clone(&worker);
        let second_barrier = Arc::clone(&barrier);
        let second = std::thread::spawn(move || {
            second_barrier.wait();
            second_worker.request(b"second".to_vec(), None).expect("second request should succeed")
        });
        barrier.wait();

        let request_a = Frame::read_from(&mut peer_reader, 1024)
            .expect("first request should decode")
            .expect("first request should be present");
        let request_b = Frame::read_from(&mut peer_reader, 1024)
            .expect("second request should decode")
            .expect("second request should be present");
        Frame::response(request_b.id, 0, request_b.payload)
            .write_to(&mut peer_writer, 1024)
            .expect("second response should encode");
        peer_writer.flush().expect("second response should flush");
        Frame::response(request_a.id, 0, request_a.payload)
            .write_to(&mut peer_writer, 1024)
            .expect("first response should encode");
        peer_writer.flush().expect("first response should flush");

        let mut responses =
            [first.join().expect("first thread should join"), second.join().expect("second thread should join")];
        responses.sort();
        assert_eq!(responses, [b"first".to_vec(), b"second".to_vec()]);

        drop(peer_reader);
        drop(peer_writer);
        worker.shutdown();
    }

    #[test]
    fn services_a_nested_worker_request_on_the_calling_thread() {
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(UNREACHED_DEADLINE);
        let peer = std::thread::spawn(move || {
            let outer = Frame::read_from(&mut peer_reader, 1024)
                .expect("outer request should decode")
                .expect("outer request should be present");
            let nested = Frame {
                version: crate::protocol::PROTOCOL_VERSION,
                kind: FrameKind::Request,
                flags: FrameFlags::empty(),
                id: 77,
                parent_id: outer.id,
                payload: b"metadata?".to_vec(),
            };
            nested.write_to(&mut peer_writer, 1024).expect("nested request should encode");
            peer_writer.flush().expect("nested request should flush");

            let nested_response = Frame::read_from(&mut peer_reader, 1024)
                .expect("nested response should decode")
                .expect("nested response should be present");
            assert_eq!(nested_response.kind, FrameKind::Response);
            assert_eq!(nested_response.id, nested.id);
            assert_eq!(nested_response.parent_id, outer.id);
            assert_eq!(nested_response.payload, b"metadata!");

            Frame::response(outer.id, 0, b"done".to_vec())
                .write_to(&mut peer_writer, 1024)
                .expect("outer response should encode");
            peer_writer.flush().expect("outer response should flush");

            let shutdown = Frame::read_from(&mut peer_reader, 1024)
                .expect("shutdown should decode")
                .expect("shutdown should be present");
            assert_eq!(shutdown.kind, FrameKind::Shutdown);
        });

        let mut handler = |request: &Frame| {
            assert_eq!(request.payload, b"metadata?");
            Ok(b"metadata!".to_vec())
        };
        let response =
            worker.request_with_handler(b"analyze".to_vec(), None, &mut handler).expect("outer request should succeed");
        assert_eq!(response, b"done");

        worker.shutdown();
        peer.join().expect("peer thread should join");
    }

    /// A request whose worker names nothing it started fails under the name Mago gave it.
    #[test]
    fn a_timed_out_request_stops_the_worker() {
        let (worker, mut peer_reader, peer_writer) = connected_worker(Duration::from_millis(20));
        let peer = std::thread::spawn(move || {
            let request = Frame::read_from(&mut peer_reader, 1024)
                .expect("request should decode")
                .expect("request should be present");
            read_to_the_end_after_a_timeout(&mut peer_reader, request.id);
        });

        let error =
            worker.request(Vec::new(), Some(&"the linter rules of app/User.php")).expect_err("request should time out");
        assert_eq!(
            error.to_string(),
            "extension host `acme` made no progress for 20 ms on the linter rules of app/User.php (worker 0): raise its `request-timeout-ms` for hooks that run that long"
        );
        assert!(!worker.is_running());

        drop(peer_writer);
        worker.shutdown();
        peer.join().expect("peer thread should join");
    }

    #[test]
    fn a_request_waiting_behind_others_gets_the_whole_deadline_once_the_worker_starts_it() {
        let deadline = Duration::from_secs(3);
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(deadline);
        let peer = std::thread::spawn(move || {
            for _ in 0..6 {
                let request = Frame::read_from(&mut peer_reader, 1024)
                    .expect("request should decode")
                    .expect("request should be present");
                std::thread::sleep(deadline / 5);
                Frame::response(request.id, 0, request.payload)
                    .write_to(&mut peer_writer, 1024)
                    .expect("response should encode");
                peer_writer.flush().expect("response should flush");
            }
        });

        let started_at = Instant::now();
        let responses = send_at_once::<6>(&worker);

        assert!(responses.iter().all(Result::is_ok), "{responses:?}");
        assert!(started_at.elapsed() > deadline, "the requests took {:?}", started_at.elapsed());
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    #[test]
    fn a_worker_that_hangs_on_a_request_fails_at_the_deadline() {
        // Long enough that the first request, which the peer answers at once, never reaches it on a busy machine.
        let deadline = Duration::from_secs(3);
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(deadline);
        let peer = std::thread::spawn(move || {
            let first = Frame::read_from(&mut peer_reader, 1024)
                .expect("first request should decode")
                .expect("first request should be present");
            let [hung, _] = [0, 1].map(|_| {
                Frame::read_from(&mut peer_reader, 1024)
                    .expect("request should decode")
                    .expect("request should be present")
            });
            Frame::response(first.id, 0, first.payload)
                .write_to(&mut peer_writer, 1024)
                .expect("response should encode");
            peer_writer.flush().expect("response should flush");

            read_to_the_end_after_a_timeout(&mut peer_reader, hung.id);
        });

        let started_at = Instant::now();
        let responses = send_at_once::<3>(&worker);
        let elapsed = started_at.elapsed();

        assert_eq!(responses.iter().filter(|response| response.is_ok()).count(), 1, "{responses:?}");
        assert_eq!(
            responses.iter().filter(|response| matches!(response, Err(WorkerError::Timeout { .. }))).count(),
            1,
            "{responses:?}"
        );
        assert!(elapsed >= deadline && elapsed < deadline * 2, "the hung request failed after {elapsed:?}");
        assert!(!worker.is_running());
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    /// Reads what Mago writes once request `request` timed out: at most the request's cancellation, which the worker
    /// fails before it may get, then the end of the stream, as Mago closes it.
    fn read_to_the_end_after_a_timeout(peer_reader: &mut TcpStream, request: u64) {
        while let Some(frame) = Frame::read_from(peer_reader, 1024).expect("a frame should decode") {
            assert_eq!((frame.kind, frame.id), (FrameKind::Cancel, request));
        }
    }

    /// A frame of `kind` the worker sends under request `parent_id`.
    fn from_worker(kind: FrameKind, id: u64, parent_id: u64, payload: Vec<u8>) -> Frame {
        Frame { version: crate::protocol::PROTOCOL_VERSION, kind, flags: FrameFlags::empty(), id, parent_id, payload }
    }

    /// A peer that runs six files' hooks for request `outer`, each for `per_file`. Before each file's hooks it sends a
    /// progress frame naming them when `progress` holds, and each hook asks Mago for metadata. It answers the request
    /// unless Mago cancels it, or closes the stream as it does once the request times out.
    fn run_six_files(
        mut peer_reader: TcpStream,
        mut peer_writer: TcpStream,
        per_file: Duration,
        progress: bool,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let outer = Frame::read_from(&mut peer_reader, 1024)
                .expect("outer request should decode")
                .expect("outer request should be present");
            for id in 1..=6 {
                if progress {
                    let hooks = format!("the hooks of app/F{id}.php").into_bytes();
                    from_worker(FrameKind::Notification, 0, outer.id, hooks)
                        .write_to(&mut peer_writer, 1024)
                        .expect("progress should encode");
                }
                std::thread::sleep(per_file);
                from_worker(FrameKind::Request, id, outer.id, b"metadata?".to_vec())
                    .write_to(&mut peer_writer, 1024)
                    .expect("nested request should encode");
                peer_writer.flush().expect("nested request should flush");
                let answer = Frame::read_from(&mut peer_reader, 1024).expect("nested response should decode");
                if answer.is_none_or(|answer| answer.kind == FrameKind::Cancel) {
                    return;
                }
            }
            Frame::response(outer.id, 0, b"done".to_vec())
                .write_to(&mut peer_writer, 1024)
                .expect("outer response should encode");
            peer_writer.flush().expect("outer response should flush");
        })
    }

    /// The deadline bounds one file's hooks, as Jest's `testTimeout` bounds one test: a batch of many files' hooks
    /// lives past it while the worker starts each file within it.
    #[test]
    fn a_batch_whose_every_file_runs_within_the_deadline_lives_past_it() {
        let deadline = Duration::from_secs(3);
        let (worker, peer_reader, peer_writer) = connected_worker(deadline);
        let peer = run_six_files(peer_reader, peer_writer, deadline / 5, true);

        let started_at = Instant::now();
        let mut handler = |_: &Frame| Ok(b"metadata!".to_vec());
        let response = worker.request_with_handler(b"analyze".to_vec(), None, &mut handler);

        assert_eq!(response.expect("the request should finish"), b"done");
        assert!(started_at.elapsed() > deadline, "the request took {:?}", started_at.elapsed());
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    /// A hook that keeps asking Mago for metadata and never finishes its file fails at the deadline. Its worker named
    /// no hooks it started, so the error names the host and the setting, and leaves the request's number to the trace
    /// log.
    #[test]
    fn a_hook_that_asks_for_metadata_but_never_finishes_its_file_fails_at_the_deadline() {
        let deadline = Duration::from_secs(3);
        let (worker, peer_reader, peer_writer) = connected_worker(deadline);
        let peer = run_six_files(peer_reader, peer_writer, deadline / 2, false);

        let started_at = Instant::now();
        let mut handler = |_: &Frame| Ok(b"metadata!".to_vec());
        let error =
            worker.request_with_handler(b"analyze".to_vec(), None, &mut handler).expect_err("the request times out");

        assert!(started_at.elapsed() < deadline * 2, "the request failed after {:?}", started_at.elapsed());
        assert_eq!(
            error.to_string(),
            "extension host `acme` made no progress for 3000 ms (worker 0): raise its `request-timeout-ms` for hooks that run that long"
        );
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    #[test]
    fn a_timeout_names_the_file_whose_hooks_made_no_progress() {
        // Long enough that the peer names the file before it on a busy machine.
        let deadline = Duration::from_secs(3);
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(deadline);
        let peer = std::thread::spawn(move || {
            let outer = Frame::read_from(&mut peer_reader, 1024)
                .expect("request should decode")
                .expect("request should be present");
            from_worker(FrameKind::Notification, 0, outer.id, b"the hooks of app/Hung.php".to_vec())
                .write_to(&mut peer_writer, 1024)
                .expect("progress should encode");
            peer_writer.flush().expect("progress should flush");
            read_to_the_end_after_a_timeout(&mut peer_reader, outer.id);
        });

        let error = worker.request(Vec::new(), None).expect_err("request should time out");

        assert_eq!(
            error.to_string(),
            "extension host `acme` made no progress for 3000 ms on the hooks of app/Hung.php (worker 0): raise its `request-timeout-ms` for hooks that run that long"
        );
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    /// The reader routes request 2's first progress before request 2's thread wakes to the start the response of
    /// request 1 gives it, so the progress names request 2's unit when request 2 never finishes.
    #[test]
    fn progress_routed_before_its_request_starts_still_names_the_request() {
        let deadline = Duration::from_secs(3);
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(deadline);
        let peer = std::thread::spawn(move || {
            let mut requests = [0, 1].map(|_| {
                Frame::read_from(&mut peer_reader, 1024)
                    .expect("request should decode")
                    .expect("request should be present")
            });
            requests.sort_by_key(|request| request.id);
            let [first, second] = requests;
            from_worker(FrameKind::Notification, 0, second.id, b"the after-analysis hooks of plugin `late`".to_vec())
                .write_to(&mut peer_writer, 1024)
                .expect("progress should encode");
            Frame::response(first.id, 0, first.payload)
                .write_to(&mut peer_writer, 1024)
                .expect("response should encode");
            peer_writer.flush().expect("progress and response should flush");
            read_to_the_end_after_a_timeout(&mut peer_reader, second.id);
        });

        let started_at = Instant::now();
        let responses = send_at_once::<2>(&worker);
        let elapsed = started_at.elapsed();

        let errors = responses.iter().filter_map(|response| response.as_ref().err()).collect::<Vec<_>>();
        assert_eq!(
            errors.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [
                "extension host `acme` made no progress for 3000 ms on the after-analysis hooks of plugin `late` (worker 0): raise its `request-timeout-ms` for hooks that run that long"
            ]
        );
        assert!(elapsed >= deadline && elapsed < deadline * 2, "the hung request failed after {elapsed:?}");
        peer.join().expect("peer thread should join");
        worker.shutdown();
    }

    #[test]
    fn reports_worker_cancellation_without_marking_the_worker_disconnected() {
        let (worker, mut peer_reader, mut peer_writer) = connected_worker(UNREACHED_DEADLINE);
        let peer = std::thread::spawn(move || {
            let request = Frame::read_from(&mut peer_reader, 1024)
                .expect("request should decode")
                .expect("request should be present");
            Frame::cancel(request.id).write_to(&mut peer_writer, 1024).expect("cancellation should encode");
            peer_writer.flush().expect("cancellation should flush");

            let shutdown = Frame::read_from(&mut peer_reader, 1024)
                .expect("shutdown should decode")
                .expect("shutdown should be present");
            assert_eq!(shutdown.kind, FrameKind::Shutdown);
        });

        let error = worker.request(Vec::new(), None).expect_err("request should be cancelled");
        assert!(matches!(error, WorkerError::Cancelled { worker: 0, request: 1 }));
        assert!(worker.is_running());

        worker.shutdown();
        peer.join().expect("peer thread should join");
    }

    /// A pool restarts its dead workers one after another before it sends them a request, so a worker that dies at
    /// startup, as one whose require-dev package `composer install --no-dev` left out does, has died again when the
    /// request reaches it. The error still names its host and says why it died.
    #[cfg(unix)]
    #[test]
    fn a_request_to_a_worker_that_died_at_startup_names_its_host_and_its_stderr() {
        let worker = dead_shell_worker(&format!("echo '{FATAL_ERROR}' >&2; exit 255"));

        let error = worker.request(b"describe".to_vec(), None).expect_err("the dead worker should fail the request");

        assert_eq!(
            error.to_string(),
            format!("extension host `acme` disconnected (worker 0): worker closed stdout; stderr: {FATAL_ERROR}")
        );
        worker.shutdown();
    }

    /// The stdout reader can see a worker exit before the stderr reader reads what it wrote last, here written by a
    /// process the worker started that writes it 200 ms later, so the failure waits for the end of the stderr.
    #[cfg(unix)]
    #[test]
    fn a_failure_holds_the_stderr_the_worker_writes_after_its_stdout_closes() {
        let worker = dead_shell_worker(&format!("( exec >&-; sleep 0.2; echo '{FATAL_ERROR}' >&2 ) & exit 255"));

        let error = worker.request(b"describe".to_vec(), None).expect_err("the dead worker should fail the request");

        assert_eq!(
            error.to_string(),
            format!("extension host `acme` disconnected (worker 0): worker closed stdout; stderr: {FATAL_ERROR}")
        );
        worker.shutdown();
    }

    /// A wrapper that forks its worker rather than replacing itself with it, as `docker compose exec` does, leaves a
    /// child that runs until its stdin ends. The failure closes stdin before it waits for the end of the stderr, so the
    /// child's last words make it into the error.
    #[cfg(unix)]
    #[test]
    fn a_failure_closes_stdin_before_it_waits_for_the_end_of_the_stderr() {
        let worker =
            dead_shell_worker("exec 3<&0; ( exec >&- 0<&3; cat >/dev/null; echo 'stdin closed' >&2 ) & exit 255");

        let error = worker.request(b"describe".to_vec(), None).expect_err("the dead worker should fail the request");

        assert_eq!(
            error.to_string(),
            "extension host `acme` disconnected (worker 0): worker closed stdout; stderr: stdin closed"
        );
        worker.shutdown();
    }

    /// A process the worker started that keeps its stderr open stops the wait at `STDERR_END_TIMEOUT`, and the error
    /// says the stderr was still open.
    #[cfg(unix)]
    #[test]
    fn a_failure_says_when_the_stderr_was_still_open_at_the_end_of_the_wait() {
        let worker = dead_shell_worker("( exec >&-; sleep 2 ) & exit 255");

        let error = worker.request(b"describe".to_vec(), None).expect_err("the dead worker should fail the request");

        assert_eq!(
            error.to_string(),
            "extension host `acme` disconnected (worker 0): worker closed stdout; stderr still open after 1000 ms"
        );
        worker.shutdown();
    }

    /// A worker that closed its stdin refuses the request as Mago writes it, and the error says so with its stderr.
    #[cfg(unix)]
    #[test]
    fn a_request_written_to_a_worker_that_closed_its_stdin_holds_the_write_error_and_the_stderr() {
        const TIMEOUT: Duration = Duration::from_secs(10);

        let worker = shell_worker(&format!("exec 0<&-; echo '{FATAL_ERROR}' >&2; exec sleep 10"));
        let deadline = Instant::now() + TIMEOUT;
        while lock(&worker.inner.stderr).display().is_empty() {
            assert!(Instant::now() < deadline, "the worker wrote no stderr within {TIMEOUT:?}");
            std::thread::sleep(Duration::from_millis(1));
        }

        let error = worker.request(b"describe".to_vec(), None).expect_err("the closed stdin should fail the request");

        assert_eq!(
            error.to_string(),
            format!(
                "extension host `acme` disconnected (worker 0): failed to write to the worker: Broken pipe (os error 32); stderr: {FATAL_ERROR}"
            )
        );
        worker.shutdown();
    }

    /// A worker that Mago shuts down while it fails gives the request it served the failure, not a shutdown.
    #[cfg(unix)]
    #[test]
    fn a_worker_shut_down_while_it_fails_fails_its_request_with_the_failure() {
        let worker = shell_worker(&format!(
            "head -c 1 >/dev/null; ( exec >&-; sleep 0.5; echo '{FATAL_ERROR}' >&2 ) & exit 255"
        ));

        let error = std::thread::scope(|scope| {
            let request = scope.spawn(|| worker.request(b"describe".to_vec(), None));
            wait_until_stopped(&worker);
            worker.shutdown();

            request.join().expect("request thread should join").expect_err("the dead worker should fail the request")
        });

        assert_eq!(
            error.to_string(),
            format!("extension host `acme` disconnected (worker 0): worker closed stdout; stderr: {FATAL_ERROR}")
        );
    }

    /// A worker that stops reading its stdin, as one held at a debugger breakpoint does, leaves Mago's write of a
    /// request larger than the pipe's buffer unfinished, and the request still fails at its deadline.
    #[cfg(unix)]
    #[test]
    fn a_request_to_a_worker_that_stopped_reading_its_stdin_fails_at_the_deadline() {
        let worker = shell_worker_within("exec sleep 20", STUCK_DEADLINE);

        let (error, elapsed) = request_larger_than_a_pipe_and_shut_down(worker);

        assert_eq!(error.to_string(), STUCK_TIMEOUT);
        assert!(elapsed < STUCK_DEADLINE + STDERR_END_TIMEOUT, "the request failed after {elapsed:?}");
    }

    /// A wrapper that forks the worker rather than replacing itself with it, as `docker compose exec` does, leaves the
    /// worker holding the pipe once the wrapper is killed. The request still fails at its deadline, and the failure
    /// ends the worker as well as the wrapper.
    #[cfg(unix)]
    #[test]
    fn a_request_to_a_stuck_worker_behind_a_forking_wrapper_fails_at_the_deadline_and_ends_both() {
        let started = std::env::temp_dir().join(format!("mago-extension-forking-wrapper-{}", std::process::id()));
        std::fs::create_dir_all(&started).expect("the folder of process ids should be created");
        let (wrapper, stuck) = (started.join("wrapper"), started.join("worker"));
        let worker = shell_worker_within(
            &format!(
                "echo $$ > '{}'; exec 3<&0; sh -c 'echo $$ > \"$0\"; exec sleep 20' '{}' 0<&3 3<&- & exec 3<&-; wait",
                wrapper.display(),
                stuck.display(),
            ),
            STUCK_DEADLINE,
        );

        let (error, elapsed) = request_larger_than_a_pipe_and_shut_down(worker);

        assert_eq!(error.to_string(), STUCK_TIMEOUT);
        assert!(elapsed < STUCK_DEADLINE + STDERR_END_TIMEOUT * 2, "the request failed after {elapsed:?}");
        for process in [wrapper, stuck] {
            let id = std::fs::read_to_string(&process).expect("the process should have written its id");
            wait_until_ended(id.trim());
        }
        std::fs::remove_dir_all(&started).expect("the folder of process ids should be removed");
    }

    /// The deadline of a test whose worker never answers.
    #[cfg(unix)]
    const STUCK_DEADLINE: Duration = Duration::from_secs(1);

    /// The error of a request whose worker made no progress for `STUCK_DEADLINE`.
    #[cfg(unix)]
    const STUCK_TIMEOUT: &str = "extension host `acme` made no progress for 1000 ms (worker 0): raise its `request-timeout-ms` for hooks that run that long";

    /// A worker that exits within the shutdown's grace period may leave a process it started running, here one that
    /// ignores its stdin and holds the worker's stdout and stderr. The shutdown ends it, rather than wait for the end
    /// of the pipes it holds.
    #[cfg(unix)]
    #[test]
    fn a_shutdown_ends_what_a_worker_that_exited_left_running() {
        let started = std::env::temp_dir().join(format!("mago-extension-exited-leader-{}", std::process::id()));
        std::fs::create_dir_all(&started).expect("the folder of process ids should be created");
        let left = started.join("left");
        let worker = shell_worker(&format!(
            "sh -c 'echo $$ > \"$0\"; exec sleep 20' '{}' </dev/null & while [ ! -s '{}' ]; do sleep 0.01; done; exit 0",
            left.display(),
            left.display(),
        ));
        let id = within_outer_timeout(move || {
            loop {
                match std::fs::read_to_string(&left) {
                    Ok(id) if !id.trim().is_empty() => break id,
                    _ => std::thread::sleep(Duration::from_millis(10)),
                }
            }
        });

        within_outer_timeout(move || worker.shutdown());

        wait_until_ended(id.trim());
        std::fs::remove_dir_all(&started).expect("the folder of process ids should be removed");
    }

    /// Sends `worker` a request of 1 MiB, past the 64 KiB a pipe buffers on macOS and Linux, then shuts the worker down,
    /// and returns the request's error and how long the request took, within `within_outer_timeout`.
    #[cfg(unix)]
    fn request_larger_than_a_pipe_and_shut_down(worker: Arc<Worker>) -> (WorkerError, Duration) {
        let (response, elapsed) = within_outer_timeout(move || {
            let started_at = Instant::now();
            let response = worker.request(vec![0; 1024 * 1024], None);
            let elapsed = started_at.elapsed();
            worker.shutdown();
            (response, elapsed)
        });

        (response.expect_err("the stuck worker should fail the request"), elapsed)
    }

    /// Runs `work` on a thread of its own and returns what it returns, and fails the test when it has not ended within
    /// 10 s, so work that never ends fails the test rather than hang the test run.
    #[cfg(unix)]
    fn within_outer_timeout<T>(work: impl FnOnce() -> T + Send + 'static) -> T
    where
        T: Send + 'static,
    {
        const OUTER_TIMEOUT: Duration = Duration::from_secs(10);

        let (finished, result) = mpsc::channel();
        std::thread::spawn(move || {
            let _result = finished.send(work());
        });

        result.recv_timeout(OUTER_TIMEOUT).expect("the work should end within 10 s")
    }

    /// Waits until the process `id` no longer runs. A process that ended and that nothing reaped yet, as an orphan is
    /// where no process reaps orphans, counts as ended.
    #[cfg(unix)]
    fn wait_until_ended(id: &str) {
        const TIMEOUT: Duration = Duration::from_secs(10);

        let runs = || {
            let state = std::process::Command::new("ps")
                .args(["-o", "stat=", "-p", id])
                .output()
                .expect("ps should run")
                .stdout;
            let state = state.trim_ascii();
            !state.is_empty() && !state.starts_with(b"Z")
        };
        let deadline = Instant::now() + TIMEOUT;
        while runs() {
            assert!(Instant::now() < deadline, "process {id} still ran after {TIMEOUT:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// What a PHP worker writes to stderr when it loads a class its missing require-dev package declares.
    #[cfg(unix)]
    const FATAL_ERROR: &str = r#"PHP Fatal error:  Uncaught Error: Class "ReportPlugin" not found"#;

    /// A worker of the host `acme` that runs `script` in `sh`.
    #[cfg(unix)]
    fn shell_worker(script: &str) -> Arc<Worker> {
        shell_worker_within(script, WorkerPoolOptions::default().request_timeout)
    }

    /// A worker of the host `acme` that runs `script` in `sh`, and fails a request that makes no progress for
    /// `request_timeout`.
    #[cfg(unix)]
    fn shell_worker_within(script: &str, request_timeout: Duration) -> Arc<Worker> {
        let command = WorkerCommand::new("sh").with_arguments(["-c", script]);
        let options = WorkerPoolOptions { host: "acme".to_owned(), request_timeout, ..WorkerPoolOptions::default() };

        Worker::spawn(0, &command, &options).expect("worker process should start")
    }

    /// A worker that runs `script` and has stopped, as a pool's restarted worker has when a request reaches it.
    #[cfg(unix)]
    fn dead_shell_worker(script: &str) -> Arc<Worker> {
        let worker = shell_worker(script);
        wait_until_stopped(&worker);

        worker
    }

    #[cfg(unix)]
    fn wait_until_stopped(worker: &Worker) {
        const TIMEOUT: Duration = Duration::from_secs(10);

        let deadline = Instant::now() + TIMEOUT;
        while worker.is_running() {
            assert!(Instant::now() < deadline, "the worker still ran after {TIMEOUT:?}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn send_at_once<const N: usize>(worker: &Arc<Worker>) -> [Result<Vec<u8>, WorkerError>; N] {
        let barrier = Barrier::new(N);
        std::thread::scope(|scope| {
            let requests: [_; N] = std::array::from_fn(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    worker.request(b"request".to_vec(), None)
                })
            });

            requests.map(|request| request.join().expect("request thread should join"))
        })
    }

    fn connected_worker(timeout: Duration) -> (Arc<Worker>, TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let address = listener.local_addr().expect("listener should have an address");
        let host_stream = TcpStream::connect(address).expect("host should connect");
        let peer_stream = listener.accept().expect("peer should connect").0;
        let host_reader = host_stream.try_clone().expect("host stream should clone");
        let peer_reader = peer_stream.try_clone().expect("peer stream should clone");
        let options = WorkerPoolOptions {
            host: "acme".to_owned(),
            maximum_payload_size: 1024,
            request_timeout: timeout,
            shutdown_timeout: Duration::from_millis(100),
            stderr_tail_size: 1024,
        };

        (Worker::from_streams(0, host_reader, host_stream, &options), peer_reader, peer_stream)
    }
}
