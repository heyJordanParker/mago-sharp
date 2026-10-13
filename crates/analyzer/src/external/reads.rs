//! Codebase reads: what a file's extension hooks and providers read while Mago analyzed the file,
//! so a later analysis runs them again when what they read changes.

use std::hash::DefaultHasher;
use std::hash::Hash;
use std::hash::Hasher;

use foldhash::HashSet;

use mago_codex::metadata::CodebaseMetadata;
use mago_codex::symbol::SymbolIdentifier;
use mago_extension::PayloadReader;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::external::ExternalAnalyzerError;
use crate::external::error::protocol;
use crate::external::metadata;

/// The most reads one list holds: a file's hooks and providers can read each symbol of a large
/// codebase once.
const MAXIMUM_READS: usize = 1_000_000;

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
    /// A method search across classes, which depends on every class-like's signature.
    Methods,
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
                self.listings.insert(Listing::Methods);
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
    /// Answers this listing against `codebase` as a hash. Two codebases answer it alike exactly
    /// when no name joined or left the set it lists.
    #[must_use]
    pub fn answer(&self, codebase: &CodebaseMetadata) -> u64 {
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
            Self::Methods => {
                let mut signatures = codebase
                    .file_signatures
                    .values()
                    .flat_map(|signature| &signature.ast_nodes)
                    .filter(|node| !node.is_function)
                    .map(|node| {
                        let members = node
                            .children
                            .iter()
                            .map(|member| (member.name.as_bytes(), member.signature_hash))
                            .collect::<Vec<_>>();
                        (node.name.as_bytes(), node.signature_hash, members)
                    })
                    .collect::<Vec<_>>();
                signatures.sort_unstable();
                for signature in signatures {
                    signature.hash(&mut hasher);
                }
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
