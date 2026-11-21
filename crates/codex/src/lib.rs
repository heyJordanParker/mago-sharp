#![allow(clippy::wildcard_imports)]
#![allow(clippy::exhaustive_enums)]
#![allow(clippy::float_arithmetic)]
#![allow(clippy::pub_use)]
#![allow(clippy::else_if_without_else)]
#![allow(clippy::match_wildcard_for_single_variants)]

use mago_database::file::File;
use mago_span::Span;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::u32_word;

pub mod assertion;
pub mod consts;
pub mod context;
pub mod diff;
pub mod differ;
pub mod flags;
pub mod graph;
pub mod identifier;
pub mod issue;
pub mod metadata;
pub mod misc;
pub mod populator;
pub mod reference;
pub mod scanner;
pub mod signature;
pub mod signature_builder;
pub mod symbol;
pub mod ttype;
pub mod visibility;

mod utils;

/// Builds the synthetic display name for an anonymous construct (closure,
/// arrow function, or anonymous class).
///
/// Format: `{<prefix>:<workspace-relative path>:<line>:<column>}`, where the
/// line and column are 1-based and computed from the span's start offset.
/// Matches PHP's own `{closure:...}` stringification.
#[must_use]
pub fn build_synthetic_name(prefix: &str, file: &File, span: Span) -> Word {
    let line = file.line_number(span.start.offset).saturating_add(1);
    let column = file.column_number(span.start.offset).saturating_add(1);

    concat_word!(b"{", prefix.as_bytes(), b":", file.name.as_ref(), b":", u32_word(line), b":", u32_word(column), b"}",)
}

#[must_use]
pub fn get_anonymous_class_name(file: &File, span: Span) -> Word {
    build_synthetic_name("anonymous-class", file, span)
}

/// The name of the trait that holds a PHP# interface's default bodies, spec section 22.
///
/// It is `App\HasDesign\Defaults` for `App\HasDesign`. The engine declares it in the interface's compiled file, and a
/// plain PHP class that implements the interface writes `use HasDesign\Defaults;` to get the bodies.
#[must_use]
pub fn defaults_trait_name(interface: &[u8]) -> Word {
    concat_word!(interface, DEFAULTS_TRAIT_SUFFIX)
}

/// What [`defaults_trait_name`] adds to an interface's name.
pub(crate) const DEFAULTS_TRAIT_SUFFIX: &[u8] = b"\\Defaults";

/// `Sharp.Security.Secret`, spec section 24, whose encryption is bound to the field that holds one.
pub const SECRET_CLASS: &[u8] = b"Sharp\\Security\\Secret";

/// `Sharp.Security.Hashed`, spec section 24, which keeps only a value's hash.
pub const HASHED_CLASS: &[u8] = b"Sharp\\Security\\Hashed";

/// Whether the class `name` is `Sharp.Security.Secret` or `Sharp.Security.Hashed`, spec section 24.
///
/// The engine writes and reads their stored forms: a Secret's encrypted for the field that holds it, and a Hashed's
/// hash.
#[must_use]
pub fn is_sealed_class(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(SECRET_CLASS) || name.eq_ignore_ascii_case(HASHED_CLASS)
}
