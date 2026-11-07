//! The population of a test's files on top of the prelude, which was populated once, when it was built. The tests that
//! scan their files into a copy of the prelude's metadata include this module and populate them with it.

use foldhash::HashSet;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::populator::populate_codebase;
use mago_codex::reference::SymbolReferences;
use mago_word::empty_word;

/// Populates the files scanned into `metadata`, a copy of the prelude's metadata, and skips the prelude's own symbols,
/// which are populated already. A file's symbol of a prelude symbol's name takes the prelude's place, as a polyfill's
/// class does, so it is populated as a file's, as watch mode populates every symbol of a changed file.
pub fn populate_on_prelude(metadata: &mut CodebaseMetadata, symbol_references: &mut SymbolReferences) {
    let mut safe_symbols = std::mem::take(&mut metadata.safe_symbols);
    safe_symbols.retain(|name| {
        metadata.class_likes.get(name).is_none_or(|class_like| class_like.flags.is_built_in())
            && metadata.function_likes.get(&(*name, empty_word())).is_none_or(|function| function.flags.is_built_in())
            && metadata.constants.get(name).is_none_or(|constant| constant.flags.is_built_in())
    });

    populate_codebase(metadata, symbol_references, safe_symbols, HashSet::default());
}
