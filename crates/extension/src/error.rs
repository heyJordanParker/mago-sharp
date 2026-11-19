use std::io;
use std::time::Duration;

/// Errors encountered while encoding or decoding a domain payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadError {
    /// A collection or byte string cannot be represented by the wire `u32` length.
    LengthOverflow { length: usize },
    /// Advancing the reader would overflow its byte offset.
    OffsetOverflow { field: &'static str },
    /// The payload ended before a field was fully decoded.
    UnexpectedEnd { field: &'static str, needed: usize, remaining: usize },
    /// A byte string expected to contain text is not valid UTF-8.
    InvalidUtf8 { field: &'static str },
    /// A boolean was encoded with a value other than zero or one.
    InvalidBoolean { field: &'static str, value: u8 },
    /// A decoded collection count exceeds its domain-specific limit.
    CountExceeded { field: &'static str, count: usize, maximum: usize },
    /// A message contained bytes after its last expected field.
    TrailingBytes { remaining: usize },
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LengthOverflow { length } => {
                write!(formatter, "payload length {length} exceeds u32::MAX")
            }
            Self::OffsetOverflow { field } => write!(formatter, "{field} length overflowed the payload offset"),
            Self::UnexpectedEnd { field, needed, remaining } => {
                write!(formatter, "payload ended while reading {field}: needed {needed} bytes, found {remaining}")
            }
            Self::InvalidUtf8 { field } => write!(formatter, "{field} is not valid UTF-8"),
            Self::InvalidBoolean { field, value } => write!(formatter, "{field} has invalid boolean value {value}"),
            Self::CountExceeded { field, count, maximum } => {
                write!(formatter, "{field} count {count} exceeds limit {maximum}")
            }
            Self::TrailingBytes { remaining } => write!(formatter, "payload contains {remaining} trailing bytes"),
        }
    }
}

impl std::error::Error for PayloadError {}

/// Why mago could not run an extension host, or why a host failed a request.
#[derive(Debug)]
pub enum WorkerError {
    /// The engine, or a thread's interpreter, could not start.
    Start(mago_engine::EngineError),
    /// mago could not write its own SDK, which every interpreter loads, to its cache.
    Sdk(io::Error),
    /// A host's file failed while an interpreter ran it.
    Load { host: String, message: String },
    /// Two hosts declare the class-like `class`, which PHP declares once.
    Clash { class: String, first: String, second: String },
    /// Two hosts set the environment variable `name` to different values.
    Variable { name: String, first: String, second: String },
    /// The host's worker failed the request, with what it threw.
    Remote { host: String, message: String },
    /// A request made no progress within its deadline: `running` is the unit the worker last said the request started,
    /// such as "the after-analysis hooks of plugin `routes`", or else what mago named the request, such as "the linter
    /// rules of app/User.php".
    Timeout { host: String, running: Option<String>, duration: Duration },
    /// The engine stopped the request on a fatal error, with what it printed.
    Failed { host: String, message: String },
    /// A worker reducer could not collect or reduce the workers' state.
    Reduction { message: String },
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start(error) => write!(formatter, "the extension hosts could not start: {error}"),
            Self::Sdk(error) => write!(formatter, "mago could not write its extension SDK to its cache: {error}"),
            Self::Load { host, message } => write!(formatter, "extension host `{host}` could not load: {message}"),
            Self::Clash { class, first, second } => write!(
                formatter,
                "extension hosts `{first}` and `{second}` both declare `{class}`, which PHP declares once: give one of them another name"
            ),
            Self::Variable { name, first, second } => write!(
                formatter,
                "extension hosts `{first}` and `{second}` set the environment variable `{name}` to different values, which their shared interpreters can't both read"
            ),
            Self::Remote { host, message } => {
                write!(formatter, "extension host `{host}` failed the request: {message}")
            }
            Self::Timeout { host, running, duration } => {
                write!(formatter, "extension host `{host}` made no progress for {} ms", duration.as_millis())?;
                if let Some(running) = running {
                    write!(formatter, " on {running}")?;
                }
                write!(formatter, ": raise its `request-timeout-ms` for hooks that run that long")
            }
            Self::Failed { host, message } => write!(formatter, "extension host `{host}` failed: {message}"),
            Self::Reduction { message } => write!(formatter, "extension worker reduction failed: {message}"),
        }
    }
}

impl std::error::Error for WorkerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Start(source) => Some(source),
            Self::Sdk(source) => Some(source),
            _ => None,
        }
    }
}
