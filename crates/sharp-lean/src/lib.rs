//! The Lean side of the PHP# checker: translates the code a law reaches into Lean, proves each law with Lean's kernel,
//! and checks each structure rule over the code's facts.
//!
//! `mago compile` builds a [`Reach`] from the analysis, [`translate`]s each accepted `.sharp` file, and hands the
//! translations to [`Lean::check`], which returns the issues that refuse each file whose law is not proved or that
//! breaks a rule.

mod facts;
mod issues;
mod lean;
mod library;
mod package;
mod reach;
mod rules;
mod runner;
mod translate;

pub use lean::Lean;
pub use lean::proof_file;
pub use package::PACKAGE_FOLDER;
pub use reach::Reach;
pub use rules::rules_files;
pub use translate::Translation;
pub use translate::translate;
