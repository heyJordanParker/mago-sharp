//! The standard library's codex: what a project's analysis and lowering read of the library, made when PHP# releases
//! it, so no project scans, analyzes or lowers the library's files again.

use mago_analyzer::effects::EffectSummary;
use mago_codex::metadata::CodebaseMetadata;
use mago_database::file::FileId;
use mago_sharp_bridge::InlineForm;
use mago_sharp_lean::Translation;

use crate::error::OrchestratorError;

/// The file of the standard library's `.sharp` folder that holds its codex.
pub const LIBRARY_CODEX: &str = "library.codex";

/// What a codex starts with, before the release that wrote it and a newline, which no release holds.
const MAGIC: &[u8; 8] = b"SHARPCDX";

/// The standard library as one release compiled it.
///
/// It holds the scan of its files, each method's return as the analysis of its body gave it, what each of its bodies
/// does by itself, the inline forms its lowering gave, which a project's lowering inlines, and the Lean translation of
/// each of its files with every declaration a project's law may reach, which a project's laws take as Lean's compiled
/// modules are imported by what a downstream file uses.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LibraryCodex {
    pub(crate) metadata: CodebaseMetadata,
    pub(crate) effect_summaries: Vec<EffectSummary>,
    pub(crate) forms: Vec<(Vec<u8>, InlineForm)>,
    pub(crate) translations: Vec<Translation>,
}

/// Why a codex can't be read.
#[derive(Debug, PartialEq, Eq)]
pub enum CodexError {
    /// The bytes are not a codex, or a damaged one.
    Unreadable,
    /// Another release wrote it, named here.
    OtherRelease(String),
}

impl LibraryCodex {
    /// Names each file the codex's translations name as `moved` gives it, as [`Translation::relocate`] does: the release
    /// wrote them where the library's package is the workspace, and a project installs the package in its own folder.
    pub fn relocate(&mut self, moved: impl Fn(FileId) -> Option<(FileId, Vec<u8>)>) {
        for translation in &mut self.translations {
            translation.relocate(&moved);
        }
    }

    /// The bytes [`Self::decode`] reads back, which name `release`, the PHP# release that wrote them.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when the codex does not encode.
    pub fn encode(&self, release: &str) -> Result<Vec<u8>, OrchestratorError> {
        let mut bytes = MAGIC.to_vec();
        bytes.extend(release.as_bytes());
        bytes.push(b'\n');
        bytes.extend(
            bincode::serde::encode_to_vec(self, bincode::config::standard()).map_err(|error| {
                OrchestratorError::General(format!("The library's codex does not encode: {error}."))
            })?,
        );

        Ok(bytes)
    }

    /// The codex `bytes` hold, when `release` wrote them.
    ///
    /// # Errors
    ///
    /// Returns [`CodexError::OtherRelease`] when another release wrote them, and [`CodexError::Unreadable`] when they
    /// hold no codex.
    pub fn decode(bytes: &[u8], release: &str) -> Result<Self, CodexError> {
        let rest = bytes.strip_prefix(MAGIC.as_slice()).ok_or(CodexError::Unreadable)?;
        let end = rest.iter().position(|byte| *byte == b'\n').ok_or(CodexError::Unreadable)?;
        let (written, rest) = rest.split_at(end);
        let rest = rest.get(1..).ok_or(CodexError::Unreadable)?;
        if written != release.as_bytes() {
            return Err(CodexError::OtherRelease(String::from_utf8_lossy(written).into_owned()));
        }

        bincode::serde::decode_from_slice(rest, bincode::config::standard())
            .map(|(codex, _)| codex)
            .map_err(|_| CodexError::Unreadable)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn a_codex_decodes_for_the_release_that_wrote_it_and_names_that_release_to_any_other() {
        let bytes = LibraryCodex::default().encode("1.2.3").unwrap();

        LibraryCodex::decode(&bytes, "1.2.3").unwrap();
        assert_eq!(LibraryCodex::decode(&bytes, "1.2.4").unwrap_err(), CodexError::OtherRelease("1.2.3".to_owned()));
        assert_eq!(LibraryCodex::decode(b"not a codex", "1.2.3").unwrap_err(), CodexError::Unreadable);
    }
}
