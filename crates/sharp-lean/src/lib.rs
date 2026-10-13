//! The Lean side of the PHP# checker: translates the code a law reaches into Lean, and proves each law with Lean's
//! kernel.
//!
//! `mago compile` builds a [`Reach`] from the analysis, [`translate`]s each accepted `.sharp` file, and hands the
//! translations to [`Lean::prove`], which returns the issues that refuse each file whose law is not proved.

mod issues;
mod lean;
mod library;
mod package;
mod reach;
mod runner;
mod translate;

pub use lean::Lean;
pub use lean::proof_file;
pub use package::PACKAGE_FOLDER;
pub use reach::Reach;
pub use translate::Translation;
pub use translate::translate;
