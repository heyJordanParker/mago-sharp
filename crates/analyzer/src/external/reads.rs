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
use mago_extension::Value;
use mago_extension::ValueReader;
use mago_extension::ValueWriter;
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

/// What the extension callbacks read from the codebase in one session.
#[derive(Debug, Default)]
pub struct SessionReads {
    /// What they read while Mago analyzed each file.
    pub files: HashMap<FileId, ExtensionReads>,
    /// What the before-analysis hooks read, for no one file.
    pub before_analysis: ExtensionReads,
    /// The entries of each worker's class log the session's responses reported.
    pub class_logs: ClassLogs,
}

/// What extension callbacks read from the codebase, kept apart by what a read can change.
///
/// The worker sorts each read by the plugin that made it, in any callback: a plugin that registers a provider may answer
/// it from what it read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExtensionReads {
    /// What plugins with no provider read, which decides only the issues reported.
    pub issues: FileReads,
    /// What plugins with a provider read, which can change the analyzed code too.
    pub code: FileReads,
    /// How many entries of each worker's class log the callbacks read, by the id the worker drew at its start: a
    /// callback may keep what any class its worker loaded before it ran left behind.
    pub classes: HashMap<u64, u32>,
}

impl ExtensionReads {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.issues.is_empty() && self.code.is_empty() && self.classes.is_empty()
    }

    pub fn extend(&mut self, other: ExtensionReads) {
        self.issues.extend(other.issues);
        self.code.extend(other.code);
        for (worker, end) in other.classes {
            let read = self.classes.entry(worker).or_default();
            *read = (*read).max(end);
        }
    }

    /// Reads the two lists of reads the SDK writes, the issue reads then the code reads, and the class loads of the
    /// worker that follow them, whose end the callbacks read.
    pub(super) fn decode(reader: &mut ValueReader<'_>) -> Result<(Self, ClassLoads), ExternalAnalyzerError> {
        let issues = FileReads::decode(reader)?;
        let code = FileReads::decode(reader)?;
        let loads = ClassLoads::decode(reader)?;
        let end = loads.end();
        let classes = if end == 0 { HashMap::default() } else { HashMap::from_iter([(loads.worker, end)]) };

        Ok((Self { issues, code, classes }, loads))
    }
}

/// The entries of one worker's class log a response reports.
///
/// The log holds the classes the worker declared from a `.sharp` file and those it was asked for and never declared, in
/// the order it met them, each by its lowercase name. The SDK sends the entries Mago has not had yet with each
/// response's reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassLoads {
    /// The id the worker drew at its start.
    pub worker: u64,
    /// The place of the first entry in the worker's log.
    pub from: u32,
    /// The entries, in their order in the log.
    pub classes: Vec<Word>,
}

impl ClassLoads {
    /// The place after the last entry, which the callbacks that sent them read up to.
    #[must_use]
    pub fn end(&self) -> u32 {
        self.from.saturating_add(u32::try_from(self.classes.len()).unwrap_or(u32::MAX))
    }

    fn decode(reader: &mut ValueReader<'_>) -> Result<Self, ExternalAnalyzerError> {
        let worker = reader.read_u64("class loads worker")?;
        let from = reader.read_u32("class loads start")?;
        let count = reader.read_count("class loads", MAXIMUM_READS)?;
        let classes = std::iter::repeat_with(|| Ok(word(reader.read_bytes("class load")?)))
            .take(count)
            .collect::<Result<_, ExternalAnalyzerError>>()?;

        Ok(Self { worker, from, classes })
    }
}

/// Each worker's class log, as far as responses reported it, by the id the worker drew at its start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClassLogs(HashMap<u64, Vec<Word>>);

impl ClassLogs {
    /// Places the entries `loads` reports in their worker's log.
    pub fn record(&mut self, loads: ClassLoads) {
        let log = self.0.entry(loads.worker).or_default();
        let from = loads.from as usize;
        let end = from + loads.classes.len();
        if log.len() < end {
            log.resize(end, empty_word());
        }
        for (entry, class) in log[from..end].iter_mut().zip(loads.classes) {
            *entry = class;
        }
    }

    /// Places every entry of `other`'s logs in these.
    pub fn extend(&mut self, other: ClassLogs) {
        for (worker, log) in other.0 {
            let held = self.0.entry(worker).or_default();
            held.resize(held.len().max(log.len()), empty_word());
            for (entry, class) in held.iter_mut().zip(log) {
                if !class.is_empty() {
                    *entry = class;
                }
            }
        }
    }

    /// Whether `reads` read one of `classes`, each a lowercase class name: an entry of a worker's log before the end
    /// `reads` gives that worker.
    #[must_use]
    pub fn read_any(&self, reads: &ExtensionReads, classes: &HashSet<Word>) -> bool {
        reads.classes.iter().any(|(worker, end)| {
            self.0.get(worker).is_some_and(|log| log.iter().take(*end as usize).any(|class| classes.contains(class)))
        })
    }
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
    /// The class-likes that extend, implement, or use this class-like directly, by its name as written, which the
    /// codebase keys, spec section 24.
    DirectDescendants(Word),
    /// Every class-like below this class-like, by its name as written.
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

    /// Whether these reads hold every read of `other`.
    #[must_use]
    pub fn includes(&self, other: &FileReads) -> bool {
        other.symbols.is_subset(&self.symbols) && other.listings.is_subset(&self.listings)
    }

    /// Reads a list of reads as the SDK writes it: a count, then each read's codebase query
    /// operation, argument, and name.
    pub(super) fn decode(reader: &mut ValueReader<'_>) -> Result<Self, ExternalAnalyzerError> {
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
            // A class-like stays as written, which the codebase keys where the read is matched, spec section 24.
            (metadata::GET_CLASS_LIKES, 0) if !name.is_empty() => {
                self.symbols.insert((word(name), empty_word()));
            }
            (metadata::GET_FUNCTIONS, 0) if !name.is_empty() => {
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
                self.listings.insert(Listing::DirectDescendants(word(name)));
            }
            (metadata::GET_CLASS_LIKE_RELATIONS, metadata::ALL_DESCENDANTS) if !name.is_empty() => {
                self.listings.insert(Listing::Descendants(word(name)));
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
                hash_names(&mut hasher, metadata::direct_descendants(codebase, *name));
            }
            Self::Descendants(name) => hash_names(&mut hasher, codebase.get_class_descendants(*name)),
            Self::Namespace(name) => codebase.namespace_exists(*name).hash(&mut hasher),
            Self::Methods(search) => {
                let mut found = ValueWriter::new();
                // A search that fails, as one that finds more methods than an answer holds, answers apart from each
                // result one gives.
                let failed = metadata::write_found_methods(search.as_bytes(), &mut found, codebase, session).is_err();
                failed.hash(&mut hasher);
                hash_values(&mut hasher, &found.finish());
            }
        }

        hasher.finish()
    }
}

/// Hashes `values` by their kinds and contents, each float by its bits.
fn hash_values(hasher: &mut DefaultHasher, values: &[Value]) {
    values.len().hash(hasher);
    for value in values {
        match value {
            Value::Null => 0u8.hash(hasher),
            Value::Bool(value) => (1u8, value).hash(hasher),
            Value::Int(value) => (2u8, value).hash(hasher),
            Value::Float(value) => (3u8, value.to_bits()).hash(hasher),
            Value::String(value) => (4u8, value).hash(hasher),
            Value::List(values) => {
                5u8.hash(hasher);
                hash_values(hasher, values);
            }
            Value::Map(entries) => {
                (6u8, entries.len()).hash(hasher);
                for (key, value) in entries {
                    key.hash(hasher);
                    hash_values(hasher, std::slice::from_ref(value));
                }
            }
            Value::Object { class, properties } => {
                (7u8, class, properties.len()).hash(hasher);
                for (name, value) in properties {
                    name.hash(hasher);
                    hash_values(hasher, std::slice::from_ref(value));
                }
            }
            Value::Case { enumeration, case } => (8u8, enumeration, case).hash(hasher),
        }
    }
}

fn hash_names(hasher: &mut DefaultHasher, names: impl IntoIterator<Item = Word>) {
    let mut names = names.into_iter().collect::<Vec<_>>();
    names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    for name in names {
        name.as_bytes().hash(hasher);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    /// Two lists of reads as the SDK writes them, `issues` then `code`, each read an operation and its name, then the
    /// entries `from` on of worker 7's class log.
    fn written_with(issues: &[(u8, &[u8])], code: &[(u8, &[u8])], from: u32, classes: &[&[u8]]) -> Vec<Value> {
        let mut writer = ValueWriter::new();
        for reads in [issues, code] {
            writer.write_u32(reads.len() as u32);
            for (operation, name) in reads {
                writer.write_u8(*operation);
                writer.write_u8(0);
                writer.write_bytes(name).expect("the name should fit in a frame");
            }
        }
        writer.write_u64(7);
        writer.write_u32(from);
        writer.write_u32(classes.len() as u32);
        for class in classes {
            writer.write_bytes(class).expect("the class should fit in a frame");
        }

        writer.finish()
    }

    fn decoded_with(values: &[Value]) -> Result<(ExtensionReads, ClassLoads), ExternalAnalyzerError> {
        let mut reader = ValueReader::new(values);
        let decoded = ExtensionReads::decode(&mut reader)?;
        reader.finish()?;

        Ok(decoded)
    }

    fn decoded(issues: &[(u8, &[u8])], code: &[(u8, &[u8])]) -> Result<ExtensionReads, ExternalAnalyzerError> {
        Ok(decoded_with(&written_with(issues, code, 0, &[]))?.0)
    }

    /// The class loads after a response's reads report the entries of the worker's log Mago has not had, and the reads
    /// read every entry of the log before their end, the ones earlier responses reported too.
    #[test]
    fn reads_read_every_class_their_worker_loaded_before_they_ended() {
        let mut logs = ClassLogs::default();
        let (first, loads) = decoded_with(&written_with(&[], &[], 0, &[b"app\\rule"])).expect("a first class load");
        logs.record(loads);
        let (later, loads) = decoded_with(&written_with(&[], &[], 1, &[b"app\\order"])).expect("a later class load");
        logs.record(loads);
        let (none, loads) = decoded_with(&written_with(&[], &[], 0, &[])).expect("no class load");
        logs.record(loads);

        assert_eq!(first.classes, HashMap::from_iter([(7, 1)]));
        assert_eq!(later.classes, HashMap::from_iter([(7, 2)]));
        assert!(none.is_empty(), "reads that end at the log's start read no class");
        let rule = HashSet::from_iter([word(b"app\\rule")]);
        let order = HashSet::from_iter([word(b"app\\order")]);
        assert!(logs.read_any(&later, &rule), "a later file reads the class an earlier file's hooks loaded");
        assert!(logs.read_any(&later, &order));
        assert!(!logs.read_any(&first, &order), "a file reads no class its worker loaded after it");
    }

    /// A plugin's reads come as two lists, its issue reads and then its code reads, and each keeps its own, so a read
    /// the worker sorted as a code read is no issue read and the reverse.
    #[test]
    fn the_issue_reads_and_the_code_reads_of_a_response_stay_apart() {
        let reads = decoded(&[(metadata::GET_CLASS_LIKES, b"\\App\\Order")], &[(metadata::LIST_FUNCTIONS, b"")])
            .expect("two lists of known reads");

        // A class-like read stays as written, without its leading `\`, for the codebase to key.
        assert_eq!(reads.issues.symbols, HashSet::from_iter([(word(b"App\\Order"), empty_word())]));
        assert!(reads.issues.listings.is_empty());
        assert!(reads.code.symbols.is_empty());
        assert_eq!(reads.code.listings, HashSet::from_iter([Listing::Functions]));
        assert!(decoded(&[], &[]).expect("two empty lists").is_empty());
    }

    /// A read of a class-like's direct descendants or of all its descendants lists the class-like as written, without
    /// its leading `\`, for the codebase to key, spec section 24.
    #[test]
    fn a_read_of_descendants_lists_the_class_like_as_written() {
        let mut reads = FileReads::default();
        reads
            .record(metadata::GET_CLASS_LIKE_RELATIONS, metadata::DIRECT_DESCENDANTS, b"\\App\\Order")
            .expect("a direct descendants read");
        reads.record(metadata::GET_CLASS_LIKE_RELATIONS, metadata::ALL_DESCENDANTS, b"App\\Line").expect("a read");

        assert_eq!(
            reads.listings,
            HashSet::from_iter([
                Listing::DirectDescendants(word(b"App\\Order")),
                Listing::Descendants(word(b"App\\Line")),
            ])
        );
    }

    /// Reads of one file that arrive in several responses join: each list gains the other's reads.
    #[test]
    fn reads_that_arrive_apart_join_list_by_list() {
        let mut reads = decoded(&[(metadata::LIST_CONSTANTS, b"")], &[]).expect("a constant listing");
        reads.extend(decoded(&[], &[(metadata::GET_CONSTANTS, b"LIMIT")]).expect("a constant"));

        assert_eq!(reads.issues.listings, HashSet::from_iter([Listing::Constants]));
        assert_eq!(reads.code.symbols, HashSet::from_iter([(word(b"LIMIT"), empty_word())]));
        assert!(!reads.is_empty());
    }

    /// Reads of one file that arrive apart read each worker's class log up to the furthest end either read, since the
    /// file's callbacks read every class their worker loaded before the last of them ended.
    #[test]
    fn reads_that_arrive_apart_read_each_class_log_to_the_furthest_end() {
        let (mut reads, _) =
            decoded_with(&written_with(&[], &[], 0, &[b"app\\rule", b"app\\order"])).expect("two class loads");
        let (earlier, _) = decoded_with(&written_with(&[], &[], 0, &[b"app\\rule"])).expect("one class load");

        reads.extend(earlier);

        assert_eq!(reads.classes, HashMap::from_iter([(7, 2)]));
    }

    /// Reads include another's when they hold each of its symbols and listings, and not when either misses one.
    #[test]
    fn reads_include_another_only_when_they_hold_every_read_of_it() {
        let both = decoded(&[(metadata::GET_CLASS_LIKES, b"Order"), (metadata::LIST_FUNCTIONS, b"")], &[])
            .expect("a class and a listing")
            .issues;
        let class = decoded(&[(metadata::GET_CLASS_LIKES, b"Order")], &[]).expect("a class").issues;
        let listing = decoded(&[(metadata::LIST_FUNCTIONS, b"")], &[]).expect("a listing").issues;

        assert!(both.includes(&class) && both.includes(&listing) && both.includes(&FileReads::default()));
        assert!(!class.includes(&both), "a listing it misses");
        assert!(!listing.includes(&both), "a symbol it misses");
    }

    /// A read the protocol does not know is refused with its operation and name.
    #[test]
    fn an_unknown_read_is_refused() {
        let refusal = decoded(&[(0xEE, b"Order")], &[]);

        assert!(
            matches!(&refusal, Err(ExternalAnalyzerError::Protocol(message)) if message == "a worker recorded unknown codebase read 238/0 of `Order`"),
            "{refusal:?}"
        );
    }
}
