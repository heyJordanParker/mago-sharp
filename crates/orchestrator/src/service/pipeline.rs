use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

use mago_allocator::LocalArena;
use rayon::prelude::*;

use mago_database::DatabaseReader;
use mago_database::ReadDatabase;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;

use crate::error::OrchestratorError;
use crate::progress::ProgressBarTheme;
use crate::progress::create_progress_bar;
use crate::progress::remove_progress_bar;
#[cfg(not(target_arch = "wasm32"))]
use crate::service::telemetry::SlowestFiles;
#[cfg(not(target_arch = "wasm32"))]
use crate::service::telemetry::measure;

// No-op `measure!` stub for wasm so the pipeline body compiles without
// pulling in the telemetry module. On wasm `trace_enabled` is always
// `false`, so the body just runs and `$out` is never actually read.
#[cfg(target_arch = "wasm32")]
macro_rules! measure {
    ($trace_enabled:expr, $out:expr, $body:expr) => {{
        let _ = $trace_enabled;
        let _ = &mut $out;
        $body
    }};
}

/// A trait that defines the final "reduce" step for a stateless parallel computation.
pub trait StatelessReducer<I, R>: Debug {
    /// Aggregates intermediate results from the parallel "map" phase into a final result.
    fn reduce(&self, results: Vec<I>) -> Result<R, OrchestratorError>;
}

/// An orchestrator for a simple, single-phase data-parallel computation.
///
/// This struct is designed for tasks like formatting that can process each file
/// in isolation without needing a shared, global view of the entire codebase.
#[derive(Debug)]
pub struct StatelessParallelPipeline<T, I, R> {
    task_name: &'static str,
    database: Arc<ReadDatabase>,
    shared_context: T,
    reducer: Box<dyn StatelessReducer<I, R> + Send + Sync>,
    should_use_progress_bar: bool,
}

impl<T, I, R> StatelessParallelPipeline<T, I, R>
where
    T: Clone + Send + Sync + 'static,
    I: Send + 'static,
    R: Send + 'static,
{
    pub fn new(
        task_name: &'static str,
        database: ReadDatabase,
        shared_context: T,
        reducer: Box<dyn StatelessReducer<I, R> + Send + Sync>,
        should_use_progress_bar: bool,
    ) -> Self {
        Self { task_name, database: Arc::new(database), shared_context, reducer, should_use_progress_bar }
    }

    /// Executes the pipeline with a given map function on all `Host` files.
    pub fn run<F>(&self, map_function: F) -> Result<R, OrchestratorError>
    where
        F: Fn(T, &LocalArena, Arc<File>) -> Result<I, OrchestratorError> + Send + Sync,
    {
        #[cfg(not(target_arch = "wasm32"))]
        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        #[cfg(target_arch = "wasm32")]
        let trace_enabled = false;

        #[cfg(not(target_arch = "wasm32"))]
        let pipeline_start = trace_enabled.then(Instant::now);
        #[cfg(not(target_arch = "wasm32"))]
        let slowest_files = trace_enabled.then(|| Arc::new(SlowestFiles::new()));

        let mut host_discover_duration = Duration::ZERO;
        let host_files: Vec<Arc<File>> = measure!(
            trace_enabled,
            host_discover_duration,
            self.database.files().filter(|f| f.file_type == FileType::Host).collect()
        );

        if host_files.is_empty() {
            return self.reducer.reduce(Vec::new());
        }

        #[cfg(not(target_arch = "wasm32"))]
        let host_count = host_files.len();

        let progress_bar = self
            .should_use_progress_bar
            .then(|| create_progress_bar(host_files.len(), self.task_name, ProgressBarTheme::Magenta));

        #[cfg(not(target_arch = "wasm32"))]
        let slowest_files_for_closure = slowest_files.as_ref().map(Arc::clone);

        let mut map_duration = Duration::ZERO;
        let results: Vec<I> = measure!(
            trace_enabled,
            map_duration,
            host_files
                .into_par_iter()
                .map_init(LocalArena::new, |arena, file| {
                    let context = self.shared_context.clone();

                    #[cfg(not(target_arch = "wasm32"))]
                    let file_for_record = trace_enabled.then(|| Arc::clone(&file));
                    #[cfg(not(target_arch = "wasm32"))]
                    let file_start = trace_enabled.then(Instant::now);

                    let result = map_function(context, arena, file)?;

                    #[cfg(not(target_arch = "wasm32"))]
                    if let (Some(sink), Some(start), Some(recorded_file)) =
                        (slowest_files_for_closure.as_ref(), file_start, file_for_record)
                    {
                        sink.record(start.elapsed(), recorded_file);
                    }

                    arena.reset();
                    if let Some(bar) = &progress_bar {
                        bar.inc(1);
                    }

                    Ok(result)
                })
                .collect::<Result<Vec<I>, OrchestratorError>>()?
        );

        if let Some(bar) = progress_bar {
            remove_progress_bar(&bar);
        }

        let mut reduce_duration = Duration::ZERO;
        let reduced = measure!(trace_enabled, reduce_duration, self.reducer.reduce(results));

        #[cfg(not(target_arch = "wasm32"))]
        #[allow(clippy::float_arithmetic)]
        if let Some(start) = pipeline_start {
            let per_file_us = map_duration.as_micros() as f64 / host_count as f64;

            tracing::trace!("Discovered {host_count} host files in {host_discover_duration:?}.");
            tracing::trace!(
                "Processed {host_count} files in parallel in {map_duration:?} (average {per_file_us:.1} µs per file)."
            );
            tracing::trace!("Reduced results in {reduce_duration:?}.");
            tracing::trace!("Pipeline finished in {:?}.", start.elapsed());

            if let Some(slowest) = slowest_files.as_ref() {
                let phase_label = format!("the {} phase", self.task_name);
                slowest.emit_slowest(20, &phase_label);
            }
        }

        reduced
    }

    /// Executes the pipeline with a given map function on specific files by ID.
    ///
    /// This method processes only the files with the given IDs, rather than all
    /// `Host` files in the database. This is useful for operations like formatting
    /// only staged files in git pre-commit hooks.
    ///
    /// # Arguments
    ///
    /// * `file_ids` - Iterator of file IDs to process
    /// * `map_function` - The function to apply to each file
    pub fn run_on_files<F, Iter>(&self, file_ids: Iter, map_function: F) -> Result<R, OrchestratorError>
    where
        F: Fn(T, &LocalArena, Arc<File>) -> Result<I, OrchestratorError> + Send + Sync,
        Iter: IntoIterator<Item = FileId>,
    {
        #[cfg(not(target_arch = "wasm32"))]
        let trace_enabled = tracing::enabled!(tracing::Level::TRACE);
        #[cfg(target_arch = "wasm32")]
        let trace_enabled = false;

        #[cfg(not(target_arch = "wasm32"))]
        let pipeline_start = trace_enabled.then(Instant::now);
        #[cfg(not(target_arch = "wasm32"))]
        let slowest_files = trace_enabled.then(|| Arc::new(SlowestFiles::new()));

        let mut lookup_duration = Duration::ZERO;
        let files: Vec<Arc<File>> = measure!(
            trace_enabled,
            lookup_duration,
            file_ids.into_iter().filter_map(|id| self.database.get(&id).ok()).collect()
        );

        if files.is_empty() {
            return self.reducer.reduce(Vec::new());
        }

        #[cfg(not(target_arch = "wasm32"))]
        let file_count = files.len();

        let progress_bar = self
            .should_use_progress_bar
            .then(|| create_progress_bar(files.len(), self.task_name, ProgressBarTheme::Magenta));

        #[cfg(not(target_arch = "wasm32"))]
        let slowest_files_for_closure = slowest_files.as_ref().map(Arc::clone);

        let mut map_duration = Duration::ZERO;
        let results: Vec<I> = measure!(
            trace_enabled,
            map_duration,
            files
                .into_par_iter()
                .map_init(LocalArena::new, |arena, file| {
                    let context = self.shared_context.clone();

                    #[cfg(not(target_arch = "wasm32"))]
                    let file_for_record = trace_enabled.then(|| Arc::clone(&file));
                    #[cfg(not(target_arch = "wasm32"))]
                    let file_start = trace_enabled.then(Instant::now);

                    let result = map_function(context, arena, file)?;

                    #[cfg(not(target_arch = "wasm32"))]
                    if let (Some(sink), Some(start), Some(recorded_file)) =
                        (slowest_files_for_closure.as_ref(), file_start, file_for_record)
                    {
                        sink.record(start.elapsed(), recorded_file);
                    }

                    arena.reset();
                    if let Some(bar) = &progress_bar {
                        bar.inc(1);
                    }

                    Ok(result)
                })
                .collect::<Result<Vec<I>, OrchestratorError>>()?
        );

        if let Some(bar) = progress_bar {
            remove_progress_bar(&bar);
        }

        let mut reduce_duration = Duration::ZERO;
        let reduced = measure!(trace_enabled, reduce_duration, self.reducer.reduce(results));

        #[cfg(not(target_arch = "wasm32"))]
        #[allow(clippy::float_arithmetic)]
        if let Some(start) = pipeline_start {
            let per_file_us = map_duration.as_micros() as f64 / file_count as f64;

            tracing::trace!("Resolved {file_count} files by id in {lookup_duration:?}.");
            tracing::trace!(
                "Processed {file_count} files in parallel in {map_duration:?} (average {per_file_us:.1} µs per file)."
            );
            tracing::trace!("Reduced results in {reduce_duration:?}.");
            tracing::trace!("Pipeline finished in {:?}.", start.elapsed());

            if let Some(slowest) = slowest_files.as_ref() {
                let phase_label = format!("the {} phase", self.task_name);
                slowest.emit_slowest(20, &phase_label);
            }
        }

        reduced
    }
}
