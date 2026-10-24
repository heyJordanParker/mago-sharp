//! PHP# parser fuzz target.
//!
//! Feeds arbitrary bytes to `parse_file` as a `.sharp` file, so the parser reads them in the PHP#
//! dialect, and asserts that it does not panic, abort, or stack-overflow regardless of input.
//! PHP# source is user input as much as PHP is. Parse errors are a normal outcome and are
//! intentionally ignored — only crashes are bugs.

#![no_main]

use std::borrow::Cow;

use mago_allocator::LocalArena;
use libfuzzer_sys::fuzz_target;

use mago_database::file::File;
use mago_syntax::parser::parse_file;

fuzz_target!(|data: &[u8]| {
    let arena = LocalArena::new();
    let file = File::ephemeral(Cow::Borrowed(b"fuzz.sharp"), Cow::Owned(data.to_vec()));
    let _ = parse_file(&arena, &file);
});
