//! The bridge from Mago's PHP# checker to the php-sharp engine.
//!
//! [`check`] accepts a `.sharp` file the checker found no error in, and [`lower`] lowers it into the tree php-src's
//! own parser builds for the equivalent PHP, reading the checker's types where the PHP form depends on them. The tree
//! is a flat node array, which `unit.rs` encodes as the `.sharpc` file `ext/sharp` turns into `zend_ast`, the way
//! HHVM's `hackc-translator.cpp` turns hackc's unit into runtime structures. cbindgen writes this file's layout and
//! the compiled file's layout in `unit.rs` to `sharp_unit.h` in `OUT_DIR`.

#![allow(non_camel_case_types)]

mod kind;
mod lower;
pub mod unit;

use unit::Read;

pub use kind::SHARP_UNIT_ABI;
pub use kind::sharp_kind;
pub use lower::checked::CheckedProgram;
pub use lower::checked::Refusal;
pub use lower::checked::check;
pub use lower::inline::InlineForm;
pub use lower::inline::InlineForms;
pub use lower::inline::inline_forms;
pub use lower::lower;
pub use lower::types::Declaration;
pub use lower::types::DeclarationKind;
pub use lower::types::Types;

/// `len` bytes of UTF-8 at `offset` in the unit's texts, not NUL-terminated.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_str {
    pub offset: u32,
    pub len: u32,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum sharp_value {
    SHARP_NULL,
    SHARP_FALSE,
    SHARP_TRUE,
    SHARP_LONG,
    SHARP_DOUBLE,
    SHARP_STRING,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct sharp_node {
    pub kind: sharp_kind,
    /// `zend_ast` attr: flags, modifiers, operator, `ZEND_NAME_FQ`.
    pub attr: u32,
    /// First line.
    pub line: u32,
    /// Closing line, for declarations.
    pub end_line: u32,
    /// Index into `children[]`.
    pub first_child: u32,
    /// List kinds are variadic.
    pub child_count: u32,
    /// `SHARP_AST_ZVAL` only.
    pub value: sharp_value,
    pub long_value: i64,
    pub double_value: f64,
    /// Names, string values, doc comments.
    pub text: sharp_str,
}

/// A lowered file: its nodes, their children and the texts their `sharp_str`s point into.
#[derive(Debug)]
pub struct Unit {
    nodes: Vec<sharp_node>,
    /// `u32::MAX` is a null child.
    children: Vec<u32>,
    /// A `SHARP_AST_STMT_LIST`.
    root: u32,
    texts: Vec<u8>,
    /// Each standard library method the file inlines, once, with its form's fingerprint.
    inlined: Vec<Read>,
}

impl Unit {
    #[must_use]
    pub fn nodes(&self) -> &[sharp_node] {
        &self.nodes
    }

    #[must_use]
    pub fn children(&self) -> &[u32] {
        &self.children
    }

    #[must_use]
    pub fn root(&self) -> u32 {
        self.root
    }

    #[must_use]
    pub fn texts(&self) -> &[u8] {
        &self.texts
    }

    /// The bytes `text` points to.
    #[must_use]
    pub fn text(&self, text: sharp_str) -> &[u8] {
        &self.texts[text.offset as usize..(text.offset + text.len) as usize]
    }

    /// Each standard library method the file inlines, once, named `class::method` in lowercase, with its form's
    /// fingerprint. The orchestrator stores them as `Reads::inlined`.
    #[must_use]
    pub fn inlined(&self) -> &[Read] {
        &self.inlined
    }
}

/// Appends `bytes` to `texts`, and returns the string that points to them. A `sharp_str` offset is 32 bits, so a
/// unit's texts hold at most 4 GiB.
fn store_text(texts: &mut Vec<u8>, bytes: &[u8]) -> sharp_str {
    let offset = u32::try_from(texts.len()).unwrap_or_else(|_| panic!("a unit's texts exceed 4 GiB"));
    let len = u32::try_from(bytes.len()).unwrap_or_else(|_| panic!("one text exceeds 4 GiB"));
    if offset.checked_add(len).is_none() {
        panic!("a unit's texts exceed 4 GiB");
    }

    texts.extend_from_slice(bytes);

    sharp_str { offset, len }
}

impl sharp_str {
    const EMPTY: Self = Self { offset: 0, len: 0 };
}
