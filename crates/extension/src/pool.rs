use std::fmt::Display;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

use crate::command::WorkerCommand;
use crate::error::WorkerError;
use crate::reduction;
use crate::worker::Worker;
use crate::worker::WorkerRequestHandler;
use crate::worker::average_micros;
use crate::worker::duration_nanos;
use crate::worker::lock;
use crate::worker::nanos_millis;

const SLOW_REQUEST_THRESHOLD: Duration = Duration::from_millis(10);
const STATE_RUNNING: u8 = 0;
const STATE_FINALIZING: u8 = 1;
const STATE_STOPPING: u8 = 2;
const STATE_STOPPED: u8 = 3;

#[derive(Debug, Default)]
struct PoolTelemetry {
    requests: AtomicU64,
    request_errors: AtomicU64,
    request_bytes: AtomicU64,
    response_bytes: AtomicU64,
    request_ns: AtomicU64,
    reserve_ns: AtomicU64,
    exchange_ns: AtomicU64,
    recovery_ns: AtomicU64,
    contended_reservations: AtomicU64,
    peak_in_flight: AtomicUsize,
    broadcasts: AtomicU64,
    broadcast_workers: AtomicU64,
    broadcast_ns: AtomicU64,
    restarts: AtomicU64,
    restart_ns: AtomicU64,
    bootstrap_replays: AtomicU64,
    bootstrap_ns: AtomicU64,
}

/// Runtime and safety limits shared by every process in a worker pool.
#[derive(Debug, Clone)]
pub struct WorkerPoolOptions {
    /// The host's name in the configuration, which an error of its workers names.
    pub host: String,
    /// Maximum payload bytes accepted in one protocol frame.
    pub maximum_payload_size: usize,
    /// Longest one unit of a request may run. It counts from when the worker starts the request, not while the
    /// request waits behind earlier ones, and starts again each time the worker starts one of the request's units: a
    /// file's hooks in an after-file batch, one plugin's initialization, before-analysis or after-analysis hooks, a
    /// codebase-scan hook, or an extension's worker reducer.
    pub request_timeout: Duration,
    /// Grace period between the shutdown frame and forcibly killing a worker.
    pub shutdown_timeout: Duration,
    /// Number of trailing stderr bytes retained for failure diagnostics.
    pub stderr_tail_size: usize,
}

impl Default for WorkerPoolOptions {
    fn default() -> Self {
        Self {
            host: String::new(),
            maximum_payload_size: 64 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
            shutdown_timeout: Duration::from_millis(250),
            stderr_tail_size: 64 * 1024,
        }
    }
}

struct WorkerSlot {
    worker: Mutex<Arc<Worker>>,
    restart: Mutex<()>,
}

#[derive(Debug, Clone)]
struct Bootstrap {
    group: Option<u64>,
    request: Vec<u8>,
    response: Vec<u8>,
}

/// A pool of persistent, multiplexed extension processes, every one started at once, so the analysis runs on as many
/// as the run has jobs from its first request.
///
/// Scheduling prefers the live worker with the fewest requests in flight. Each process serves its requests one at a
/// time, in the order they arrive.
pub struct WorkerPool {
    command: WorkerCommand,
    options: WorkerPoolOptions,
    workers: Box<[WorkerSlot]>,
    /// Held while every worker takes a broadcast, a class loads question or the shutdown, so a worker restarted
    /// meanwhile replays the whole of a broadcast or none of it.
    lifecycle: Mutex<()>,
    bootstraps: Mutex<Vec<Bootstrap>>,
    cursor: AtomicUsize,
    state: AtomicU8,
    worker_reduction: AtomicBool,
    trace_enabled: bool,
    telemetry: PoolTelemetry,
    started_at: Option<Instant>,
}

impl std::fmt::Debug for WorkerPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (bootstrap_count, bootstrap_request_bytes, bootstrap_response_bytes) = {
            let bootstraps = lock(&self.bootstraps);
            (
                bootstraps.len(),
                bootstraps.iter().map(|bootstrap| bootstrap.request.len()).sum::<usize>(),
                bootstraps.iter().map(|bootstrap| bootstrap.response.len()).sum::<usize>(),
            )
        };

        formatter
            .debug_struct("WorkerPool")
            .field("command", &self.command)
            .field("options", &self.options)
            .field("worker_count", &self.workers.len())
            .field("lifecycle", &self.lifecycle)
            .field("bootstrap_count", &bootstrap_count)
            .field("bootstrap_request_bytes", &bootstrap_request_bytes)
            .field("bootstrap_response_bytes", &bootstrap_response_bytes)
            .field("cursor", &self.cursor.load(Ordering::Relaxed))
            .field("state", &pool_state_name(self.state.load(Ordering::Relaxed)))
            .field("worker_reduction", &self.worker_reduction.load(Ordering::Relaxed))
            .field("trace_enabled", &self.trace_enabled)
            .field("telemetry", &self.telemetry)
            .field("started_at", &self.started_at)
            .finish()
    }
}

impl WorkerPool {
    /// Starts a pool of `size` identical worker processes, every one at once.
    ///
    /// # Errors
    ///
    /// Returns an error if a worker process or one of its stream reader threads
    /// cannot be started. The workers that started stop first.
    pub fn spawn(command: WorkerCommand, size: NonZeroUsize, options: WorkerPoolOptions) -> Result<Self, WorkerError> {
        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        let started_at = trace_enabled.then(Instant::now);
        tracing::trace!(
            program = ?command.program(),
            workers = size.get(),
            maximum_payload_bytes = options.maximum_payload_size,
            request_timeout_ms = options.request_timeout.as_millis(),
            shutdown_timeout_ms = options.shutdown_timeout.as_millis(),
            stderr_tail_bytes = options.stderr_tail_size,
            "Starting extension worker pool."
        );
        let results = std::thread::scope(|scope| {
            let (command, options) = (&command, &options);
            (0..size.get())
                .map(|id| scope.spawn(move || Worker::spawn(id, command, options)))
                .collect::<Vec<_>>()
                .into_iter()
                .enumerate()
                .map(|(id, handle)| handle.join().unwrap_or(Err(WorkerError::CoordinatorPanic { worker: id })))
                .collect::<Vec<_>>()
        });
        let mut workers = Vec::with_capacity(size.get());
        let mut first_error = None;
        for result in results {
            match result {
                Ok(worker) => workers.push(worker),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if let Some(error) = first_error {
            for worker in &workers {
                worker.shutdown();
            }
            return Err(error);
        }

        if let Some(start) = started_at {
            tracing::trace!(
                program = ?command.program(),
                workers = size.get(),
                elapsed = ?start.elapsed(),
                "Extension worker pool started."
            );
        }

        Ok(Self {
            command,
            options,
            workers: workers
                .into_iter()
                .map(|worker| WorkerSlot { worker: Mutex::new(worker), restart: Mutex::new(()) })
                .collect(),
            lifecycle: Mutex::new(()),
            bootstraps: Mutex::new(Vec::new()),
            cursor: AtomicUsize::new(0),
            state: AtomicU8::new(STATE_RUNNING),
            worker_reduction: AtomicBool::new(false),
            trace_enabled,
            telemetry: PoolTelemetry::default(),
            started_at,
        })
    }

    /// Number of processes currently active in this pool.
    #[must_use]
    pub fn len(&self) -> usize {
        if self.state.load(Ordering::Acquire) == STATE_RUNNING { self.workers.len() } else { 0 }
    }

    /// Returns whether this pool has been shut down and can no longer serve requests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.state.load(Ordering::Acquire) != STATE_RUNNING
    }

    /// Enables terminal worker-state reduction for this pool.
    ///
    /// Capability registration calls this only when at least one logical
    /// extension advertises a worker reducer. Keeping it opt-in means ordinary
    /// worker pools pay no shutdown request or payload cost.
    pub fn enable_worker_reduction(&self) {
        self.worker_reduction.store(true, Ordering::Release);
        tracing::trace!(workers = self.workers.len(), "Enabled worker reduction.");
    }

    /// Sends a request through the least-loaded worker and blocks for its response. `running` names what the
    /// request runs, such as "the linter rules of app/User.php", for its deadline's error when the worker names
    /// nothing it started.
    ///
    /// # Errors
    ///
    /// Returns an error when no worker is available, communication fails, the
    /// deadline expires, or the worker returns an error response.
    pub fn request(&self, payload: Vec<u8>, running: Option<&dyn Display>) -> Result<Vec<u8>, WorkerError> {
        self.request_inner(payload, None, |worker, payload| worker.request(payload, running), false)
    }

    /// Sends a request, named by `running` as [`Self::request`] names it, and services nested worker requests on
    /// the calling thread.
    ///
    /// # Errors
    ///
    /// Returns an error when no worker is available, communication fails, the
    /// deadline expires, the nested handler fails to respond, or the worker
    /// returns an error response.
    pub fn request_with_handler<H>(
        &self,
        payload: Vec<u8>,
        running: Option<&dyn Display>,
        handler: &mut H,
    ) -> Result<Vec<u8>, WorkerError>
    where
        H: WorkerRequestHandler,
    {
        self.request_inner(
            payload,
            None,
            |worker, payload| worker.request_with_handler(payload, running, handler),
            true,
        )
    }

    /// Sends a nested-request-capable request with a stable worker-affinity key.
    ///
    /// Affinity is honored only among equally idle workers, preserving
    /// throughput while improving process-local extension cache locality.
    ///
    /// # Errors
    ///
    /// Returns an error under the same conditions as [`Self::request_with_handler`].
    pub fn request_with_handler_affinity<H>(
        &self,
        payload: Vec<u8>,
        running: Option<&dyn Display>,
        affinity: &[u8],
        handler: &mut H,
    ) -> Result<Vec<u8>, WorkerError>
    where
        H: WorkerRequestHandler,
    {
        self.request_inner(
            payload,
            Some(affinity),
            |worker, payload| worker.request_with_handler(payload, running, handler),
            true,
        )
    }

    fn request_inner(
        &self,
        payload: Vec<u8>,
        affinity: Option<&[u8]>,
        exchange: impl FnOnce(&Worker, Vec<u8>) -> Result<Vec<u8>, WorkerError>,
        nested_handler: bool,
    ) -> Result<Vec<u8>, WorkerError> {
        let trace_start = self.trace_enabled.then(Instant::now);
        let request_bytes = payload.len();
        let reserve_start = self.trace_enabled.then(Instant::now);
        let (index, reservation) = match self.reserve_worker(affinity) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.record_trace_request(
                    request_bytes,
                    0,
                    trace_start.map_or(Duration::ZERO, |start| start.elapsed()),
                    reserve_start.map_or(Duration::ZERO, |start| start.elapsed()),
                    Duration::ZERO,
                    Duration::ZERO,
                    true,
                    nested_handler,
                );
                return Err(error);
            }
        };
        let reserve_duration = reserve_start.map_or(Duration::ZERO, |start| start.elapsed());
        let exchange_start = self.trace_enabled.then(Instant::now);
        let result = exchange(reservation.worker(), payload);
        let exchange_duration = exchange_start.map_or(Duration::ZERO, |start| start.elapsed());
        let recovery_start = self.trace_enabled.then(Instant::now);
        let recovery = self.recover_if_needed(index, &reservation, &result);
        let recovery_duration = recovery_start.map_or(Duration::ZERO, |start| start.elapsed());
        let response_bytes = result.as_ref().map_or(0, Vec::len);
        let failed = result.is_err() || recovery.is_err();
        self.record_trace_request(
            request_bytes,
            response_bytes,
            trace_start.map_or(Duration::ZERO, |start| start.elapsed()),
            reserve_duration,
            exchange_duration,
            recovery_duration,
            failed,
            nested_handler,
        );
        recovery?;
        result
    }

    /// Sends the same request to every active worker concurrently and returns
    /// results in worker-index order.
    ///
    /// This is intended for initialization and registration validation, where
    /// every process in a pool must begin with identical extension state. The
    /// exchange is recorded and replayed by workers started later.
    ///
    /// # Errors
    ///
    /// Returns an error if any worker cannot be started, communicated with, or
    /// restarted after failure. All coordinator threads are joined before the
    /// error is returned.
    pub fn broadcast(&self, payload: &[u8]) -> Result<Vec<Vec<u8>>, WorkerError> {
        let mut responses = self.broadcast_inner(None, &[payload.to_vec()])?;
        Ok(responses.pop().unwrap_or_default())
    }

    /// Starts again each running worker whose class loads hold one of `classes`, each a lowercase class name, as a new
    /// process that replays the pool's initialization, since PHP declares a class once and the worker would keep the
    /// one it loaded. Returns how many workers it started again.
    ///
    /// # Errors
    ///
    /// Returns an error when a worker cannot answer or cannot be started again.
    pub fn reload_holding(&self, classes: &[&[u8]]) -> Result<usize, WorkerError> {
        if classes.is_empty() || self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Ok(0);
        }

        let request = reduction::class_loads_request(classes)?;
        let _lifecycle = lock(&self.lifecycle);
        let mut reloaded = 0;
        for index in 0..self.workers.len() {
            let worker = self.ensure_running(index)?;
            if reduction::decode_class_loads_response(index, &worker.request(request.clone(), None)?)? {
                tracing::debug!(worker = index, "Starting an extension worker again: a class it loaded changed.");
                self.restart(index, &worker)?;
                reloaded += 1;
            }
        }

        Ok(reloaded)
    }

    /// Returns the maximum payload accepted by a worker frame.
    #[inline]
    #[must_use]
    pub const fn maximum_payload_size(&self) -> usize {
        self.options.maximum_payload_size
    }

    /// Sends an ordered state replacement to every worker and records only the
    /// latest successful sequence for replay by future workers.
    ///
    /// Requests for one worker remain serialized while different workers process the sequence concurrently.
    ///
    /// # Errors
    ///
    /// Returns an error if any worker cannot process the complete sequence.
    pub fn broadcast_sequence(&self, group: u64, payloads: &[Vec<u8>]) -> Result<Vec<Vec<Vec<u8>>>, WorkerError> {
        self.broadcast_inner(Some(group), payloads)
    }

    fn broadcast_inner(&self, group: Option<u64>, payloads: &[Vec<u8>]) -> Result<Vec<Vec<Vec<u8>>>, WorkerError> {
        let trace_start = self.trace_enabled.then(Instant::now);
        if self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(WorkerError::Unavailable);
        }
        if payloads.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(payload) = payloads.iter().find(|payload| payload.len() > self.options.maximum_payload_size) {
            return Err(WorkerError::Protocol {
                worker: 0,
                source: crate::ProtocolError::FrameTooLarge {
                    length: payload.len(),
                    maximum: self.options.maximum_payload_size,
                },
            });
        }

        let _lifecycle = lock(&self.lifecycle);
        if self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(WorkerError::Unavailable);
        }
        let worker_count = self.workers.len();
        let workers = (0..worker_count)
            .map(|index| self.ensure_running(index).map(|worker| (index, worker)))
            .collect::<Result<Vec<_>, _>>()?;
        let results = std::thread::scope(|scope| {
            workers
                .into_iter()
                .map(|(index, worker)| {
                    (
                        index,
                        scope.spawn(move || {
                            let reservation = worker.reserve();
                            let responses = payloads
                                .iter()
                                .map(|payload| reservation.worker().request(payload.clone(), None))
                                .collect::<Result<Vec<_>, _>>();
                            (reservation, responses)
                        }),
                    )
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(index, handle)| (index, handle.join()))
                .collect::<Vec<_>>()
        });

        let mut responses = vec![vec![None; worker_count]; payloads.len()];
        let mut first_error = None;
        for (worker_index, result) in results {
            let Ok((reservation, result)) = result else {
                first_error.get_or_insert(WorkerError::CoordinatorPanic { worker: worker_index });
                continue;
            };

            if let Err(error) = self.recover_if_needed(worker_index, &reservation, &result) {
                first_error.get_or_insert(error);
            }

            match result {
                Ok(worker_responses) => {
                    for (request_index, response) in worker_responses.into_iter().enumerate() {
                        responses[request_index][worker_index] = Some(response);
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }

        if let Some(error) = first_error {
            self.record_broadcast(payloads.len(), worker_count, trace_start);
            tracing::trace!(
                ?group,
                workers = worker_count,
                requests = payloads.len(),
                request_bytes = payloads.iter().map(Vec::len).sum::<usize>(),
                error = %error,
                "Extension broadcast failed."
            );
            return Err(error);
        }

        let responses = responses
            .into_iter()
            .map(|responses| responses.into_iter().flatten().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let replacements = payloads
            .iter()
            .zip(&responses)
            .filter_map(|(request, responses)| {
                responses.first().map(|response| Bootstrap {
                    group,
                    request: request.clone(),
                    response: response.clone(),
                })
            })
            .collect::<Vec<_>>();
        let mut bootstraps = lock(&self.bootstraps);
        if group.is_some() {
            bootstraps.retain(|bootstrap| bootstrap.group != group);
        }
        bootstraps.extend(replacements);
        drop(bootstraps);

        if let Some(start) = trace_start {
            self.record_broadcast(payloads.len(), worker_count, Some(start));
            tracing::trace!(
                ?group,
                workers = worker_count,
                requests = payloads.len(),
                request_bytes = payloads.iter().map(Vec::len).sum::<usize>(),
                response_bytes = responses.iter().flatten().map(Vec::len).sum::<usize>(),
                elapsed = ?start.elapsed(),
                "Extension broadcast completed."
            );
        }

        Ok(responses)
    }

    fn record_broadcast(&self, requests: usize, workers: usize, started_at: Option<Instant>) {
        let Some(started_at) = started_at else {
            return;
        };
        self.telemetry.broadcasts.fetch_add(requests as u64, Ordering::Relaxed);
        self.telemetry.broadcast_workers.fetch_add(requests.saturating_mul(workers) as u64, Ordering::Relaxed);
        self.telemetry.broadcast_ns.fetch_add(duration_nanos(started_at.elapsed()), Ordering::Relaxed);
    }

    /// Gracefully stops every worker, then kills workers that exceed the
    /// configured shutdown grace period.
    pub fn shutdown(&self) {
        if self.state.compare_exchange(STATE_RUNNING, STATE_FINALIZING, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return;
        }

        if tracing::enabled!(tracing::Level::TRACE) {
            let traced_requests = self.telemetry.requests.load(Ordering::Relaxed);
            tracing::trace!(workers = self.workers.len(), "Shutting down extension worker pool.");
            tracing::trace!(
                requests = traced_requests,
                errors = self.telemetry.request_errors.load(Ordering::Relaxed),
                request_bytes = self.telemetry.request_bytes.load(Ordering::Relaxed),
                response_bytes = self.telemetry.response_bytes.load(Ordering::Relaxed),
                total_request_ms = nanos_millis(self.telemetry.request_ns.load(Ordering::Relaxed)),
                average_request_micros =
                    average_micros(self.telemetry.request_ns.load(Ordering::Relaxed), traced_requests),
                "Extension worker pool request summary."
            );
            tracing::trace!(
                reserve_ms = nanos_millis(self.telemetry.reserve_ns.load(Ordering::Relaxed)),
                exchange_ms = nanos_millis(self.telemetry.exchange_ns.load(Ordering::Relaxed)),
                recovery_ms = nanos_millis(self.telemetry.recovery_ns.load(Ordering::Relaxed)),
                contended_reservations = self.telemetry.contended_reservations.load(Ordering::Relaxed),
                peak_in_flight = self.telemetry.peak_in_flight.load(Ordering::Relaxed),
                "Extension worker pool scheduling and IPC summary."
            );
            tracing::trace!(
                broadcasts = self.telemetry.broadcasts.load(Ordering::Relaxed),
                broadcast_workers = self.telemetry.broadcast_workers.load(Ordering::Relaxed),
                broadcast_ms = nanos_millis(self.telemetry.broadcast_ns.load(Ordering::Relaxed)),
                restarts = self.telemetry.restarts.load(Ordering::Relaxed),
                restart_ms = nanos_millis(self.telemetry.restart_ns.load(Ordering::Relaxed)),
                bootstrap_replays = self.telemetry.bootstrap_replays.load(Ordering::Relaxed),
                bootstrap_ms = nanos_millis(self.telemetry.bootstrap_ns.load(Ordering::Relaxed)),
                lifetime = ?self.started_at.map(|start| start.elapsed()).unwrap_or_default(),
                "Extension worker pool lifecycle summary."
            );
        }

        let _lifecycle = lock(&self.lifecycle);
        if self.worker_reduction.load(Ordering::Acquire)
            && let Err(error) = self.reduce_worker_state()
        {
            tracing::warn!(error = %error, "Extension worker reduction failed; continuing pool shutdown.");
        }

        self.state.store(STATE_STOPPING, Ordering::Release);
        let workers = self.workers.iter().map(|slot| Arc::clone(&lock(&slot.worker))).collect::<Vec<_>>();
        self.shutdown_workers(&workers);
        self.state.store(STATE_STOPPED, Ordering::Release);

        tracing::trace!("Extension worker pool shut down.");
    }

    fn reduce_worker_state(&self) -> Result<(), WorkerError> {
        let started_at = Instant::now();
        let workers = (0..self.workers.len())
            .map(|index| {
                let worker = Arc::clone(&lock(&self.workers[index].worker));
                if !worker.is_running() {
                    return Err(WorkerError::Reduction {
                        worker: index,
                        message: "worker stopped before its state could be collected".to_string(),
                    });
                }

                Ok(worker)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let drain_started_at = Instant::now();
        while workers.iter().any(|worker| worker.in_flight() != 0) {
            std::thread::sleep(Duration::from_micros(100));
        }
        tracing::trace!(
            workers = workers.len(),
            elapsed = ?drain_started_at.elapsed(),
            "Drained extension requests before worker reduction."
        );

        let request = reduction::collect_request();
        tracing::trace!(
            workers = workers.len(),
            request_bytes = request.len(),
            "Collecting process-local extension state from workers."
        );
        let results = std::thread::scope(|scope| {
            workers
                .iter()
                .enumerate()
                .map(|(index, worker)| {
                    let request = request.clone();
                    (index, scope.spawn(move || worker.request(request, None)))
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|(index, handle)| (index, handle.join()))
                .collect::<Vec<_>>()
        });

        let mut responses = vec![None; workers.len()];
        let mut reducers = None;
        let mut first_error = None;
        for (index, result) in results {
            let Ok(result) = result else {
                first_error.get_or_insert(WorkerError::CoordinatorPanic { worker: index });
                continue;
            };

            match result {
                Ok(response) => match reduction::decode_collect_response(index, &response) {
                    Ok(worker_has_reducers) => {
                        if reducers.is_some_and(|reducers| reducers != worker_has_reducers) {
                            first_error.get_or_insert_with(|| WorkerError::Reduction {
                                worker: index,
                                message: "workers advertised inconsistent reducer registrations".to_string(),
                            });
                        } else {
                            reducers = Some(worker_has_reducers);
                            responses[index] = Some(response);
                        }
                    }
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                },
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }

        if reducers != Some(true) {
            tracing::trace!(workers = workers.len(), "No worker reducer data was collected.");
            return Ok(());
        }

        let responses = responses.into_iter().flatten().collect::<Vec<_>>();
        let leader = &workers[0];
        let leader_index = leader.id();
        let followers = &workers[1..];
        let request = reduction::reduce_request(leader_index, &responses, self.options.maximum_payload_size)?;
        let response_bytes = responses.iter().map(Vec::len).sum::<usize>();
        tracing::trace!(
            leader = leader_index,
            followers = followers.len(),
            "Stopping follower extension workers before reduction."
        );
        self.shutdown_workers(followers);

        tracing::trace!(
            leader = leader_index,
            workers = responses.len(),
            collected_bytes = response_bytes,
            request_bytes = request.len(),
            "Sending the complete worker reduction batch to the surviving worker."
        );
        let response = leader.request(request, None)?;
        reduction::decode_reduce_response(leader_index, &response)?;
        tracing::trace!(
            leader = leader_index,
            workers = responses.len(),
            collected_bytes = response_bytes,
            elapsed = ?started_at.elapsed(),
            "Extension worker reduction completed."
        );

        Ok(())
    }

    fn shutdown_workers(&self, workers: &[Arc<Worker>]) {
        for worker in workers {
            worker.begin_shutdown();
        }

        let started_at = Instant::now();
        for worker in workers {
            worker.finish_shutdown(started_at, self.options.shutdown_timeout);
        }
    }

    fn reserve_worker(
        &self,
        affinity: Option<&[u8]>,
    ) -> Result<(usize, crate::worker::WorkerReservation), WorkerError> {
        if self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(WorkerError::Unavailable);
        }

        let worker_count = self.workers.len();
        let start = affinity.map_or_else(
            || self.cursor.fetch_add(1, Ordering::Relaxed) % worker_count,
            |key| affinity_hash(key) % worker_count,
        );
        let mut selected: Option<(usize, Arc<Worker>, usize)> = None;
        let mut last_error = None;

        for offset in 0..worker_count {
            let index = (start + offset) % worker_count;
            let worker = match self.ensure_running(index) {
                Ok(worker) => worker,
                Err(error) => {
                    last_error = Some(error);
                    continue;
                }
            };
            let load = worker.in_flight();
            if selected.as_ref().is_none_or(|(_, _, selected_load)| load < *selected_load) {
                selected = Some((index, worker, load));
            }
        }

        let Some((index, worker, load)) = selected else {
            tracing::trace!(worker_count, "No running extension worker could be reserved.");
            return Err(last_error.unwrap_or(WorkerError::Unavailable));
        };

        if self.trace_enabled {
            if load > 0 {
                self.telemetry.contended_reservations.fetch_add(1, Ordering::Relaxed);
            }
            self.telemetry.peak_in_flight.fetch_max(load.saturating_add(1), Ordering::Relaxed);
        }

        let reservation = worker.reserve();
        if self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Err(WorkerError::Unavailable);
        }

        Ok((index, reservation))
    }

    fn ensure_running(&self, index: usize) -> Result<Arc<Worker>, WorkerError> {
        let worker = Arc::clone(&lock(&self.workers[index].worker));
        if worker.is_running() {
            return Ok(worker);
        }

        tracing::trace!(worker = index, "Extension worker is not running; attempting restart.");
        self.restart(index, &worker)
    }

    fn recover_if_needed<T>(
        &self,
        index: usize,
        reservation: &crate::worker::WorkerReservation,
        result: &Result<T, WorkerError>,
    ) -> Result<(), WorkerError> {
        if result.is_ok() || reservation.worker().is_running() || self.state.load(Ordering::Acquire) != STATE_RUNNING {
            return Ok(());
        }

        let failed = Arc::clone(&lock(&self.workers[index].worker));
        if reservation.matches(&failed) {
            self.restart(index, &failed)?;
        }

        Ok(())
    }

    fn restart(&self, index: usize, failed: &Arc<Worker>) -> Result<Arc<Worker>, WorkerError> {
        let trace_start = self.trace_enabled.then(Instant::now);
        tracing::trace!(worker = index, "Restarting extension worker.");
        let _restart = lock(&self.workers[index].restart);
        let mut slot = lock(&self.workers[index].worker);
        if !Arc::ptr_eq(&slot, failed) && slot.is_running() {
            return Ok(Arc::clone(&slot));
        }

        failed.shutdown();
        let replacement = self
            .spawn_initialized(index)
            .map_err(|source| WorkerError::Restart { worker: index, source: Box::new(source) })?;
        *slot = Arc::clone(&replacement);

        if let Some(start) = trace_start {
            let elapsed = start.elapsed();
            self.telemetry.restarts.fetch_add(1, Ordering::Relaxed);
            self.telemetry.restart_ns.fetch_add(duration_nanos(elapsed), Ordering::Relaxed);
            tracing::trace!(worker = index, elapsed = ?elapsed, "Extension worker restarted.");
        }

        Ok(replacement)
    }

    fn spawn_initialized(&self, index: usize) -> Result<Arc<Worker>, WorkerError> {
        let trace_start = self.trace_enabled.then(Instant::now);
        let worker = Worker::spawn(index, &self.command, &self.options)?;
        let bootstraps = lock(&self.bootstraps).clone();
        tracing::trace!(worker = index, bootstrap_requests = bootstraps.len(), "Initializing extension worker state.");
        for bootstrap in &bootstraps {
            let bootstrap_start = self.trace_enabled.then(Instant::now);
            let response = match worker.request(bootstrap.request.clone(), None) {
                Ok(response) => response,
                Err(error) => {
                    worker.shutdown();
                    return Err(error);
                }
            };
            if response != bootstrap.response {
                tracing::trace!(worker = index, "Extension worker returned inconsistent bootstrap metadata.");
                worker.shutdown();
                return Err(WorkerError::InconsistentInitialization { worker: index });
            }
            if let Some(start) = bootstrap_start {
                self.telemetry.bootstrap_replays.fetch_add(1, Ordering::Relaxed);
                self.telemetry.bootstrap_ns.fetch_add(duration_nanos(start.elapsed()), Ordering::Relaxed);
            }
        }

        if let Some(start) = trace_start {
            tracing::trace!(
                worker = index,
                bootstrap_requests = bootstraps.len(),
                elapsed = ?start.elapsed(),
                "Extension worker state initialized."
            );
        }

        Ok(worker)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_trace_request(
        &self,
        request_bytes: usize,
        response_bytes: usize,
        total: Duration,
        reserve: Duration,
        exchange: Duration,
        recovery: Duration,
        failed: bool,
        nested_handler: bool,
    ) {
        if !self.trace_enabled {
            return;
        }

        self.telemetry.requests.fetch_add(1, Ordering::Relaxed);
        self.telemetry.request_errors.fetch_add(u64::from(failed), Ordering::Relaxed);
        self.telemetry.request_bytes.fetch_add(request_bytes as u64, Ordering::Relaxed);
        self.telemetry.response_bytes.fetch_add(response_bytes as u64, Ordering::Relaxed);
        self.telemetry.request_ns.fetch_add(duration_nanos(total), Ordering::Relaxed);
        self.telemetry.reserve_ns.fetch_add(duration_nanos(reserve), Ordering::Relaxed);
        self.telemetry.exchange_ns.fetch_add(duration_nanos(exchange), Ordering::Relaxed);
        self.telemetry.recovery_ns.fetch_add(duration_nanos(recovery), Ordering::Relaxed);

        if total >= SLOW_REQUEST_THRESHOLD {
            tracing::trace!(
                request_bytes,
                response_bytes,
                elapsed = ?total,
                reserve = ?reserve,
                exchange = ?exchange,
                recovery = ?recovery,
                nested_handler,
                failed,
                "Slow extension worker-pool request completed."
            );
        }
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[inline]
fn affinity_hash(bytes: &[u8]) -> usize {
    bytes.iter().fold(0x811c_9dc5usize, |hash, byte| (hash ^ usize::from(*byte)).wrapping_mul(0x0100_0193usize))
}

fn pool_state_name(state: u8) -> &'static str {
    match state {
        STATE_RUNNING => "running",
        STATE_FINALIZING => "finalizing",
        STATE_STOPPING => "stopping",
        STATE_STOPPED => "stopped",
        _ => "unknown",
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn reports_a_missing_worker_program() {
        let command = WorkerCommand::new(OsString::from("mago-extension-worker-that-does-not-exist"));
        let error = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
            .expect_err("missing program should fail");

        assert!(matches!(error, WorkerError::Spawn { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn debug_output_redacts_arguments_and_bootstrap_payloads() {
        let pool = WorkerPool::spawn(
            WorkerCommand::new("cat").with_argument("secret-argument"),
            NonZeroUsize::MIN,
            WorkerPoolOptions::default(),
        )
        .expect("worker pool should start");
        lock(&pool.bootstraps).push(Bootstrap {
            group: None,
            request: b"secret-request".to_vec(),
            response: b"secret-response".to_vec(),
        });

        let debug = format!("{pool:?}");
        assert!(!debug.contains("secret-argument"));
        assert!(!debug.contains("secret-request"));
        assert!(!debug.contains("secret-response"));
        assert!(debug.contains("bootstrap_count: 1"));

        pool.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn starts_every_worker_at_once() {
        let pool = WorkerPool::spawn(
            WorkerCommand::new("cat"),
            NonZeroUsize::new(4).expect("four is non-zero"),
            WorkerPoolOptions::default(),
        )
        .expect("worker pool should start");

        assert_eq!(pool.len(), 4);
        assert!(!pool.is_empty());
        assert!(pool.workers.iter().all(|slot| lock(&slot.worker).is_running()));
        pool.shutdown();
        assert_eq!(pool.len(), 0);
        assert!(pool.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_restarted_worker_replays_the_initialization() {
        const WORKER: &str = concat!(
            "dd bs=1 count=36 of=/dev/null 2>/dev/null; ",
            "printf '\\115\\101\\107\\117",              // MAGO
            "\\000\\001\\000\\000",                      // protocol 1.0
            "\\002\\000\\000\\000",                      // response, no flags or reserved bits
            "\\000\\000\\000\\000\\000\\000\\000\\001",  // request id 1
            "\\000\\000\\000\\000\\000\\000\\000\\000",  // no parent
            "\\000\\000\\000\\005",                      // five payload bytes
            "\\162\\145\\141\\144\\171'; ",              // ready
            "dd bs=1 count=32 of=/dev/null 2>/dev/null", // shutdown frame
        );

        let pool = WorkerPool::spawn(
            WorkerCommand::new("sh").with_arguments(["-c", WORKER]),
            NonZeroUsize::new(3).expect("three is non-zero"),
            WorkerPoolOptions::default(),
        )
        .expect("worker pool should start");

        assert_eq!(
            pool.broadcast(b"init").expect("initialization should succeed"),
            [b"ready".to_vec(), b"ready".to_vec(), b"ready".to_vec()]
        );
        let worker = pool.spawn_initialized(3).expect("a restarted worker should replay the initialization");
        assert!(worker.is_running());

        worker.shutdown();
        pool.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn grouped_broadcast_replaces_the_state_a_restarted_worker_replays() {
        const WORKER: &str = concat!(
            "dd bs=1 count=33 of=/dev/null 2>/dev/null; ",
            r"printf '\115\101\107\117\000\001\000\000\002\000\000\000",
            r"\000\000\000\000\000\000\000\001\000\000\000\000\000\000\000\000\000\000\000\002\157\153'; ",
            "dd bs=1 count=33 of=/dev/null 2>/dev/null; ",
            r"printf '\115\101\107\117\000\001\000\000\002\000\000\000",
            r"\000\000\000\000\000\000\000\002\000\000\000\000\000\000\000\000\000\000\000\002\157\153'; ",
            "dd bs=1 count=33 of=/dev/null 2>/dev/null; ",
            r"printf '\115\101\107\117\000\001\000\000\002\000\000\000",
            r"\000\000\000\000\000\000\000\003\000\000\000\000\000\000\000\000\000\000\000\002\157\153'; ",
            "dd bs=1 count=32 of=/dev/null 2>/dev/null",
        );

        let pool = WorkerPool::spawn(
            WorkerCommand::new("sh").with_arguments(["-c", WORKER]),
            NonZeroUsize::new(3).expect("three is non-zero"),
            WorkerPoolOptions::default(),
        )
        .expect("worker pool should start");

        pool.broadcast_sequence(7, &[b"a".to_vec(), b"b".to_vec()]).expect("initial state sequence should succeed");
        pool.broadcast_sequence(7, &[b"c".to_vec()]).expect("replacement state sequence should succeed");

        let grouped =
            lock(&pool.bootstraps).iter().filter(|bootstrap| bootstrap.group == Some(7)).cloned().collect::<Vec<_>>();
        assert_eq!(grouped.len(), 1);
        assert_eq!(grouped[0].request, b"c");

        let worker = pool.spawn_initialized(3).expect("a restarted worker should replay only the replacement sequence");
        assert!(worker.is_running());
        worker.shutdown();
        pool.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn starts_requests_and_gracefully_stops_a_real_process() {
        const WORKER: &str = concat!(
            "dd bs=1 count=36 of=/dev/null 2>/dev/null; ",
            "printf '\\115\\101\\107\\117",              // MAGO
            "\\000\\001\\000\\000",                      // protocol 1.0
            "\\002\\000\\000\\000",                      // response, no flags or reserved bits
            "\\000\\000\\000\\000\\000\\000\\000\\001",  // request id 1
            "\\000\\000\\000\\000\\000\\000\\000\\000",  // no parent
            "\\000\\000\\000\\004",                      // four payload bytes
            "\\160\\157\\156\\147'; ",                   // pong
            "dd bs=1 count=32 of=/dev/null 2>/dev/null", // shutdown frame
        );

        let command = WorkerCommand::new("sh").with_arguments(["-c", WORKER]);
        let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
            .expect("worker process should start");

        assert_eq!(pool.request(b"ping".to_vec(), None).expect("request should succeed"), b"pong");
        pool.shutdown();
    }

    /// A worker that dies at startup, as one whose require-dev package `composer install --no-dev` left out does, fails
    /// the first request with its host's name and what it wrote to stderr. Why Mago learns it stopped depends on when it
    /// dies: before the request reaches it, while Mago writes the request, or after, so only the reason varies.
    #[cfg(unix)]
    #[test]
    fn a_worker_that_dies_at_startup_names_its_host_and_its_stderr() {
        const WORKER: &str = r#"echo 'PHP Fatal error:  Uncaught Error: Class "ReportPlugin" not found' >&2; exit 255"#;

        let pool = WorkerPool::spawn(
            WorkerCommand::new("sh").with_arguments(["-c", WORKER]),
            NonZeroUsize::MIN,
            WorkerPoolOptions { host: "acme".to_owned(), ..WorkerPoolOptions::default() },
        )
        .expect("worker process should start");

        let error = pool.broadcast(b"describe").expect_err("the dead worker should fail the request").to_string();

        assert!(error.starts_with("extension host `acme` disconnected (worker 0): "), "{error}");
        assert!(
            error.ends_with(r#"; stderr: PHP Fatal error:  Uncaught Error: Class "ReportPlugin" not found"#),
            "{error}"
        );
        pool.shutdown();
    }

    /// A worker whose group SIGKILL cannot end, as the kernel cannot end a process it holds on a stalled disk, leaves a
    /// process holding the worker's pipes. The pool still restarts the worker after its request fails, and still drops,
    /// where each waited for the worker's I/O threads forever. The process that holds the pipes runs in a group of its
    /// own, which the end of the worker's group misses as it would miss a process the kernel holds.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_pool_restarts_and_drops_a_worker_whose_group_outlives_its_end() {
        const TIMEOUT: Duration = Duration::from_secs(30);

        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        let folder = std::env::temp_dir().join(format!("mago-outlived-{}-{}", std::process::id(), started.as_nanos()));
        std::fs::create_dir_all(&folder).expect("the test folder should be made");
        let script = format!(
            "cd '{}' || exit 1; if [ -e started ]; then exec cat > /dev/null; fi; : > started; echo $$ > group; \
             set -m; sleep 2147483647 & echo $! > held; wait",
            folder.display()
        );
        let options = WorkerPoolOptions {
            host: "acme".to_owned(),
            request_timeout: Duration::from_millis(200),
            ..WorkerPoolOptions::default()
        };
        let pool =
            WorkerPool::spawn(WorkerCommand::new("sh").with_arguments(["-c", &script]), NonZeroUsize::MIN, options)
                .expect("worker pool should start");
        let read = |name: &str| {
            let deadline = Instant::now() + TIMEOUT;
            loop {
                if let Ok(id) = std::fs::read_to_string(folder.join(name))
                    && let Ok(id) = id.trim().parse::<libc::pid_t>()
                {
                    return id;
                }
                assert!(Instant::now() < deadline, "the worker wrote no {name} within {TIMEOUT:?}");
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let group = read("group");
        let held = read("held");
        lock(&crate::process_group::UNENDING).push(group);

        let (done, returned) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let failed = pool.request(b"describe".to_vec(), None).is_err();
            drop(pool);
            let _result = done.send(failed);
        });
        let outcome = returned.recv_timeout(TIMEOUT);

        lock(&crate::process_group::UNENDING).retain(|unending| *unending != group);
        // SAFETY: `kill` only sends a signal, to the process the worker left holding its pipes.
        unsafe { libc::kill(held, libc::SIGKILL) };
        let _result = std::fs::remove_dir_all(&folder);
        assert_eq!(outcome, Ok(true), "the request should fail, and the pool restart the worker and drop");
    }
}
