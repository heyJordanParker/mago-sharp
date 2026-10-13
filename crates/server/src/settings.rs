//! The curated inputs a [`Server`](crate::Server) needs to operate.
//!
//! The server takes a [`Settings`]; **not** the CLI's `Configuration`. The
//! caller is responsible for loading configuration from disk (or building it in
//! memory) and projecting it down to the specific, already-resolved settings
//! each subsystem requires. The server never reads configuration itself.

use std::sync::Arc;

use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings as AnalyzerSettings;
use mago_syntax::settings::ParserSettings;

/// The resolved settings a [`Server`](crate::Server) runs against.
///
/// Supplied by the caller alongside the file
/// [`Database`](mago_database::Database) and decoded codebase metadata passed
/// to [`Server::new`](crate::Server::new).
#[derive(Debug, Clone)]
pub struct Settings {
    /// Settings for parsing PHP source into an AST.
    pub parser: ParserSettings,
    /// Settings for the static analyzer.
    pub analyzer: AnalyzerSettings,
    /// The analyzer plugin registry, built once by the caller. Shared so it can
    /// be reused across workspaces.
    pub plugin_registry: Arc<PluginRegistry>,
    /// Whether analysis passes draw progress bars.
    pub use_progress_bars: bool,
}
