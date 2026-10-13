//! Services for running specific analysis tools on PHP code.
//!
//! This module contains the service implementations for each of Mago's analysis tools.
//! Each service encapsulates the logic for running a specific tool (linting, analysis,
//! formatting, or architectural guarding) on a codebase.
//!
//! # Available Services
//!
//! - [`incremental_analysis::IncrementalAnalysisService`]: Static analysis with type checking and control flow,
//!   kept warm across edits
//! - [`analysis::AnalysisService`]: Single-file static analysis for hosts without threads, such as WebAssembly
//! - [`format::FormatService`]: Code formatting to ensure consistent style
//! - [`guard::GuardService`]: Architectural rule enforcement
//! - [`lint::LintService`]: Linting for code quality and best practices
//!
//! # Architecture
//!
//! Services use a parallel processing pipeline (defined in the private `pipeline` module)
//! to efficiently process large codebases. The pipeline pattern allows services to:
//!
//! 1. Process files in parallel using thread pools
//! 2. Aggregate results from multiple workers
//! 3. Display progress bars for long-running operations
//!
//! # Usage
//!
//! Services are typically created via the [`Orchestrator`](crate::Orchestrator) factory
//! methods rather than being instantiated directly.

mod body_return;
mod issue_reconciliation;
mod pipeline;
#[cfg(not(target_arch = "wasm32"))]
mod telemetry;

pub mod analysis;
pub mod format;
pub mod guard;
pub mod incremental_analysis;
pub mod lint;

use mago_database::file::File;
use mago_syntax::dialect::Dialect;

use crate::error::OrchestratorError;

/// Refuses a PHP# file, which only the analyzer reads yet, and which no tool may change.
///
/// # Errors
///
/// Returns [`OrchestratorError::SharpNotSupported`] for a PHP# file.
pub fn refuse_sharp_file(tool: &'static str, file: &File) -> Result<(), OrchestratorError> {
    if Dialect::of(file).is_sharp() {
        return Err(OrchestratorError::SharpNotSupported {
            tool,
            file: String::from_utf8_lossy(&file.name).into_owned(),
        });
    }

    Ok(())
}
