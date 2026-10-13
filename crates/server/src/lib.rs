//! A transport-agnostic backend that keeps one workspace's analysis warm.
//!
//! `mago-server` analyzes a **single workspace** and keeps that analysis warm
//! across edits. It is deliberately ignorant of any wire protocol: there is no
//! LSP, no JSON-RPC, no stdio. Callers speak in plain inputs (a
//! [`FileId`](mago_database::file::FileId) and in-memory file contents) and
//! receive plain domain values back. A thin protocol layer (the CLI)
//! translates those values for its users.
//!
//! # No I/O
//!
//! The server never touches the filesystem. It does not discover or load
//! configuration, it does not walk directories, and it does not read files
//! from disk. The caller builds a [`Database`](mago_database::Database) of
//! files, assembles a [`Settings`], and hands both to [`Server::new`] with the
//! function that decodes the prelude into a
//! [`CodebaseMetadata`](mago_codex::metadata::CodebaseMetadata). File edits
//! arrive as in-memory byte buffers, not paths.
//!
//! # Multiple workspaces
//!
//! A `Server` owns exactly one workspace. Several workspaces are modelled by
//! the protocol layer holding one `Server` per workspace and routing each
//! request to the right one.

pub mod error;
pub mod server;
pub mod settings;

pub use error::ServerError;
pub use server::Server;
pub use settings::Settings;
