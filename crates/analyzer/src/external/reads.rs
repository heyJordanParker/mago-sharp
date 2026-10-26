//! Codebase reads: what a file's extension hooks and providers read while Mago analyzed the file,
//! so a later analysis runs them again when what they read changes.

use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use foldhash::HashMap;
use foldhash::HashSet;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::symbol::SymbolIdentifier;
use mago_database::file::FileId;
use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::external::ExternalAnalysisSession;
use crate::external::ExternalAnalyzerError;
use crate::external::error::protocol;
use crate::external::metadata;

/// The most reads one list holds: a file's hooks and providers can read each symbol of a large
/// codebase once.
const MAXIMUM_READS: usize = 1_000_000;

/// What the extension hooks, issue filters and providers read from the codebase in one run.
#[derive(Debug, Default)]
pub struct ExtensionReads {
    /// The reads that decide only the issues reported in each file: what its after-file and targeted hooks read, and
    /// what its issue filters read.
    pub issues: HashMap<FileId, FileReads>,
    /// The reads that can change each file's analyzed code: what the providers read while Mago analyzed it.
    pub code: HashMap<FileId, FileReads>,
    /// What the before-analysis hooks read, for no one file.
    pub before_analysis: FileReads,
}

/// What one file's extension hooks and providers read from the codebase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileReads {
    /// The class-likes, functions, and constants they read, keyed as the reference graph keys
    /// top-level symbols. A class-like stands for its members too: the invalidation cascade marks a
    /// class-like invalid when any of its members or ancestors changes signature.
    pub symbols: HashSet<SymbolIdentifier>,
    /// The sets of names they read whole.
    pub listings: HashSet<Listing>,
}

/// A set of names a hook or provider read whole, which changes when a name joins or leaves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Listing {
    /// The class-likes of one kind, by the class-like kind filter of the codebase query protocol.
    ClassLikes(u8),
    Functions,
    Constants,
    /// The class-likes that extend, implement, or use this lowercase class-like name directly.
    DirectDescendants(Word),
    /// Every class-like below this lowercase class-like name.
    Descendants(Word),
    /// Whether this namespace exists.
    Namespace(Word),
    /// The methods a search across classes finds, with the fields it projects, by the search as the worker wrote it.
    Methods(Word),
}

impl FileReads {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty() && self.listings.is_empty()
    }

    pub fn extend(&mut self, other: FileReads) {
        self.symbols.extend(other.symbols);
        self.listings.extend(other.listings);
    }

    /// Reads a list of reads as the SDK writes it: a count, then each read's codebase query
    /// operation, argument, and name.
    pub(super) fn decode(reader: &mut PayloadReader<'_>) -> Result<Self, ExternalAnalyzerError> {
        let mut reads = Self::default();
        for _ in 0..reader.read_count("codebase reads", MAXIMUM_READS)? {
            let operation = reader.read_u8("codebase read operation")?;
            let argument = reader.read_u8("codebase read argument")?;
            reads.record(operation, argument, reader.read_bytes("codebase read name")?)?;
        }

        Ok(reads)
    }

    fn record(&mut self, operation: u8, argument: u8, name: &[u8]) -> Result<(), ExternalAnalyzerError> {
        let name = name.strip_prefix(b"\\").unwrap_or(name);
        match (operation, argument) {
            (metadata::GET_CLASS_LIKES | metadata::GET_FUNCTIONS, 0) if !name.is_empty() => {
                self.symbols.insert((ascii_lowercase_word(name), empty_word()));
            }
            (metadata::GET_CONSTANTS, 0) if !name.is_empty() => {
                self.symbols.insert((word(name), empty_word()));
            }
            (metadata::LIST_CLASS_LIKES, filter) if filter <= metadata::ENUM => {
                self.listings.insert(Listing::ClassLikes(filter));
            }
            (metadata::LIST_FUNCTIONS, 0) => {
                self.listings.insert(Listing::Functions);
            }
            (metadata::LIST_CONSTANTS, 0) => {
                self.listings.insert(Listing::Constants);
            }
            (metadata::GET_CLASS_LIKE_RELATIONS, metadata::DIRECT_DESCENDANTS) if !name.is_empty() => {
                self.listings.insert(Listing::DirectDescendants(ascii_lowercase_word(name)));
            }
            (metadata::GET_CLASS_LIKE_RELATIONS, metadata::ALL_DESCENDANTS) if !name.is_empty() => {
                self.listings.insert(Listing::Descendants(ascii_lowercase_word(name)));
            }
            (metadata::CHECK_EXISTENCE, metadata::EXISTS_NAMESPACE) if !name.is_empty() => {
                self.listings.insert(Listing::Namespace(word(name)));
            }
            (metadata::FIND_METHODS, 0) => {
                self.listings.insert(Listing::Methods(metadata::method_search(name)?));
            }
            _ => {
                return Err(protocol(format!(
                    "a worker recorded unknown codebase read {operation}/{argument} of `{}`",
                    String::from_utf8_lossy(name)
                )));
            }
        }

        Ok(())
    }
}

impl Listing {
    /// Answers this listing against `codebase` as a hash, a method search with each method it finds as the worker reads
    /// it from `session`'s files. Two codebases answer it alike exactly when no name joined or left the set it lists,
    /// and no method a search finds changed what the search projects of it.
    #[must_use]
    pub fn answer(&self, codebase: &CodebaseMetadata, session: &ExternalAnalysisSession) -> u64 {
        let mut hasher = DefaultHasher::new();
        match self {
            Self::ClassLikes(filter) => hash_names(&mut hasher, metadata::class_like_names(codebase, *filter)),
            Self::Functions => hash_names(&mut hasher, metadata::function_names(codebase)),
            Self::Constants => hash_names(&mut hasher, metadata::constant_names(codebase)),
            Self::DirectDescendants(name) => {
                hash_names(&mut hasher, metadata::direct_descendants(codebase, name.as_bytes()));
            }
            Self::Descendants(name) => hash_names(&mut hasher, codebase.get_class_descendants(name.as_bytes())),
            Self::Namespace(name) => codebase.namespace_exists(name.as_bytes()).hash(&mut hasher),
            Self::Methods(search) => {
                let mut found = PayloadWriter::new();
                // A search that fails, as one that finds more methods than an answer holds, answers apart from each
                // result one gives.
                let failed = metadata::write_found_methods(search.as_bytes(), &mut found, codebase, session).is_err();
                failed.hash(&mut hasher);
                found.finish().hash(&mut hasher);
            }
        }

        hasher.finish()
    }
}

fn hash_names(hasher: &mut DefaultHasher, names: impl IntoIterator<Item = Word>) {
    let mut names = names.into_iter().collect::<Vec<_>>();
    names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    for name in names {
        name.as_bytes().hash(hasher);
    }
}
