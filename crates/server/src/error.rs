//! Error types for the server crate.

use mago_orchestrator::error::OrchestratorError;

/// Errors that can occur while constructing or driving a [`Server`](crate::Server).
#[derive(Debug)]
pub enum ServerError {
    /// An error surfaced by the underlying analysis orchestrator (analysis,
    /// database access, or a poisoned cache lock).
    Orchestrator(OrchestratorError),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Orchestrator(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Orchestrator(error) => Some(error),
        }
    }
}

impl From<OrchestratorError> for ServerError {
    fn from(error: OrchestratorError) -> Self {
        Self::Orchestrator(error)
    }
}
