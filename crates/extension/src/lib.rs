//! Infrastructure for running Mago's extension hosts.
//!
//! This crate deliberately knows nothing about linter rules or analyzer
//! providers. It runs each host's worker inside mago on the PHP# engine,
//! and owns the capability-neutral source snapshots and payload codecs every
//! extension capability uses.

pub mod error;
pub mod hosts;
pub mod payload;
pub mod process_group;
pub mod source;

mod reduction;

pub use error::PayloadError;
pub use error::WorkerError;
pub use hosts::Host;
pub use hosts::HostFile;
pub use hosts::Hosts;
pub use hosts::WorkerRequestHandler;
pub use payload::PayloadReader;
pub use payload::PayloadWriter;
pub use process_group::ProcessGroup;
