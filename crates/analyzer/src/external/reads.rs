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

use crate::effects::summary::initial_values;
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
            read_to(&mut self.classes, worker, end);
        }
    }
}

/// Notes that callbacks read `worker`'s class log up to `end`, the furthest end any of them read.
fn read_to(classes: &mut HashMap<u64, u32>, worker: u64, end: u32) {
    let read = classes.entry(worker).or_default();
    *read = (*read).max(end);
}

/// The codebase reads one response reports, as the SDK writes them: the issue reads, then the code reads, then the
/// class loads of the worker, whose end the callbacks read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResponseReads {
    issues: SentReads,
    code: SentReads,
    loads: ClassLoads,
}

impl ResponseReads {
    pub(super) fn decode(reader: &mut ValueReader<'_>) -> Result<Self, ExternalAnalyzerError> {
        Ok(Self {
            issues: SentReads::decode(reader)?,
            code: SentReads::decode(reader)?,
            loads: ClassLoads::decode(reader)?,
        })
    }
}

/// One list of reads a response names: the number its worker sent it under, and the list itself the first time the
/// worker sends it in the session's generation. Providers and hooks often read the same symbols on every call, so the
/// list goes whole once and by its number after.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SentReads {
    number: u32,
    reads: Option<FileReads>,
}

impl SentReads {
    fn decode(reader: &mut ValueReader<'_>) -> Result<Self, ExternalAnalyzerError> {
        let number = reader.read_u32("codebase reads number")?;
        let reads = if reader.read_bool("codebase reads sent")? { Some(FileReads::decode(reader)?) } else { None };

        Ok(Self { number, reads })
    }
}

/// What a session's responses reported, as the workers sent it: each list of reads once, by its worker and number,
/// and the numbers each file's callbacks read, which [`RecordedReads::into_session_reads`] joins once the session ends.
#[derive(Debug, Default)]
pub(super) struct RecordedReads {
    lists: HashMap<(u64, u32), FileReads>,
    files: HashMap<Option<FileId>, ReadLists>,
    class_logs: ClassLogs,
}

/// The lists of reads one file's callbacks read, or the before-analysis hooks', each by its worker and number.
#[derive(Debug, Default)]
struct ReadLists {
    issues: HashSet<(u64, u32)>,
    code: HashSet<(u64, u32)>,
    classes: HashMap<u64, u32>,
}

impl RecordedReads {
    /// Records what the callbacks read while Mago analyzed `file_id`, or what the before-analysis hooks read, with the
    /// class loads their worker reported.
    ///
    /// # Errors
    ///
    /// Returns an error when the response names a list its worker never sent in this session.
    pub(super) fn record(
        &mut self,
        file_id: Option<FileId>,
        reads: ResponseReads,
    ) -> Result<(), ExternalAnalyzerError> {
        let worker = reads.loads.worker;
        let end = reads.loads.end();
        if !reads.loads.classes.is_empty() {
            self.class_logs.record(reads.loads);
        }

        let issues = self.keep(worker, reads.issues)?;
        let code = self.keep(worker, reads.code)?;
        if issues.is_none() && code.is_none() && end == 0 {
            return Ok(());
        }

        let lists = self.files.entry(file_id).or_default();
        lists.issues.extend(issues);
        lists.code.extend(code);
        if end != 0 {
            read_to(&mut lists.classes, worker, end);
        }

        Ok(())
    }

    /// Keeps the list `sent` names, and returns its key, or None when it holds no read.
    fn keep(&mut self, worker: u64, sent: SentReads) -> Result<Option<(u64, u32)>, ExternalAnalyzerError> {
        let key = (worker, sent.number);
        let reads = match sent.reads {
            Some(reads) => self.lists.entry(key).insert_entry(reads).into_mut(),
            None => self.lists.get(&key).ok_or_else(|| {
                protocol(format!("a worker named codebase reads {} it never sent in this session", sent.number))
            })?,
        };

        Ok((!reads.is_empty()).then_some(key))
    }

    /// Joins the lists each file's callbacks read into what they read.
    #[must_use]
    pub(super) fn into_session_reads(self) -> SessionReads {
        let join = |numbers: &HashSet<(u64, u32)>| {
            let mut reads = FileReads::default();
            for list in numbers.iter().filter_map(|key| self.lists.get(key)) {
                reads.symbols.extend(list.symbols.iter().copied());
                reads.listings.extend(list.listings.iter().copied());
            }

            reads
        };

        let mut session = SessionReads::default();
        for (file_id, lists) in &self.files {
            let reads =
                ExtensionReads { issues: join(&lists.issues), code: join(&lists.code), classes: lists.classes.clone() };
            match file_id {
                Some(file_id) => {
                    session.files.insert(*file_id, reads);
                }
                None => session.before_analysis = reads,
            }
        }
        session.class_logs = self.class_logs;

        session
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
    /// The initial values a read of a property's initial value can run, spec section 29: every PHP# property that
    /// declares one.
    InitialValues,
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
            Self::InitialValues => {
                let mut properties = initial_values(codebase);
                properties.sort_unstable_by(|left, right| {
                    (left.0.as_bytes(), left.1.as_bytes()).cmp(&(right.0.as_bytes(), right.1.as_bytes()))
                });
                for (class, property) in properties {
                    (class.as_bytes(), property.as_bytes()).hash(&mut hasher);
                }
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

    /// One list of reads as the SDK sends it the first time: its number, then each read, an operation and its name.
    fn write_list(writer: &mut ValueWriter, number: u32, reads: &[(u8, &[u8])]) {
        writer.write_u32(number);
        writer.write_bool(true);
        writer.write_u32(reads.len() as u32);
        for (operation, name) in reads {
            writer.write_u8(*operation);
            writer.write_u8(0);
            writer.write_bytes(name).expect("the name should fit in a frame");
        }
    }

    /// The entries `from` on of worker 7's class log, as the SDK writes them after a response's reads.
    fn write_loads(writer: &mut ValueWriter, from: u32, classes: &[&[u8]]) {
        writer.write_u64(7);
        writer.write_u32(from);
        writer.write_u32(classes.len() as u32);
        for class in classes {
            writer.write_bytes(class).expect("the class should fit in a frame");
        }
    }

    /// A response's reads as worker 7 writes them: `issues` sent whole as list 0, `code` as list 1, then the entries
    /// `from` on of its class log.
    fn written_with(issues: &[(u8, &[u8])], code: &[(u8, &[u8])], from: u32, classes: &[&[u8]]) -> Vec<Value> {
        let mut writer = ValueWriter::new();
        write_list(&mut writer, 0, issues);
        write_list(&mut writer, 1, code);
        write_loads(&mut writer, from, classes);

        writer.finish()
    }

    /// What a session records of `responses`, each the reads of the file it is paired with.
    fn recorded(responses: &[(&[u8], Vec<Value>)]) -> Result<SessionReads, ExternalAnalyzerError> {
        let mut recorded = RecordedReads::default();
        for (file, response) in responses {
            let mut reader = ValueReader::new(response);
            let reads = ResponseReads::decode(&mut reader)?;
            reader.finish()?;
            recorded.record(Some(FileId::new(file)), reads)?;
        }

        Ok(recorded.into_session_reads())
    }

    /// What the callbacks of one file read in one response.
    fn decoded(issues: &[(u8, &[u8])], code: &[(u8, &[u8])]) -> Result<ExtensionReads, ExternalAnalyzerError> {
        let reads = recorded(&[(b"a.php", written_with(issues, code, 0, &[]))])?;

        Ok(reads.files.get(&FileId::new(b"a.php")).cloned().unwrap_or_default())
    }

    /// The class loads after a response's reads report the entries of the worker's log Mago has not had, and the reads
    /// read every entry of the log before their end, the ones earlier responses reported too.
    #[test]
    fn reads_read_every_class_their_worker_loaded_before_they_ended() {
        let reads = recorded(&[
            (b"first.php", written_with(&[], &[], 0, &[b"app\\rule"])),
            (b"later.php", written_with(&[], &[], 1, &[b"app\\order"])),
            (b"none.php", written_with(&[], &[], 0, &[])),
        ])
        .expect("three files' class loads");
        let first = &reads.files[&FileId::new(b"first.php")];
        let later = &reads.files[&FileId::new(b"later.php")];

        assert_eq!(first.classes, HashMap::from_iter([(7, 1)]));
        assert_eq!(later.classes, HashMap::from_iter([(7, 2)]));
        assert!(
            !reads.files.contains_key(&FileId::new(b"none.php")),
            "reads that end at the log's start read no class"
        );
        let rule = HashSet::from_iter([word(b"app\\rule")]);
        let order = HashSet::from_iter([word(b"app\\order")]);
        assert!(reads.class_logs.read_any(later, &rule), "a later file reads the class an earlier file's hooks loaded");
        assert!(reads.class_logs.read_any(later, &order));
        assert!(!reads.class_logs.read_any(first, &order), "a file reads no class its worker loaded after it");
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

    /// A list a worker sent once it names by its number after, and each file that names it reads all of it, while
    /// another worker's list of the same number is another list.
    #[test]
    fn a_list_of_reads_sent_once_reads_the_same_for_each_file_that_names_it() {
        let mut named = ValueWriter::new();
        for number in [0, 1] {
            named.write_u32(number);
            named.write_bool(false);
        }
        write_loads(&mut named, 0, &[]);
        let mut other_worker = written_with(&[], &[(metadata::GET_CONSTANTS, b"LIMIT")], 0, &[]);
        let worker = other_worker.len() - 3;
        other_worker[worker] = Value::Int(8);

        let reads = recorded(&[
            (
                b"a.php",
                written_with(&[(metadata::GET_CLASS_LIKES, b"Order")], &[(metadata::LIST_FUNCTIONS, b"")], 0, &[]),
            ),
            (b"b.php", named.finish()),
            (b"c.php", other_worker),
        ])
        .expect("lists sent once and named after");

        assert_eq!(reads.files[&FileId::new(b"a.php")], reads.files[&FileId::new(b"b.php")]);
        assert_eq!(
            reads.files[&FileId::new(b"b.php")].issues.symbols,
            HashSet::from_iter([(word(b"Order"), empty_word())])
        );
        assert_eq!(
            reads.files[&FileId::new(b"c.php")].code.symbols,
            HashSet::from_iter([(word(b"LIMIT"), empty_word())])
        );
        assert_eq!(reads.files[&FileId::new(b"a.php")].code.listings, HashSet::from_iter([Listing::Functions]));
    }

    /// A list named by a number its worker never sent in the session is refused, since Mago can't know what it read.
    #[test]
    fn a_list_of_reads_never_sent_is_refused() {
        let mut named = ValueWriter::new();
        named.write_u32(4);
        named.write_bool(false);
        write_list(&mut named, 5, &[]);
        write_loads(&mut named, 0, &[]);

        let refusal = recorded(&[(b"a.php", named.finish())]);

        assert!(
            matches!(&refusal, Err(ExternalAnalyzerError::Protocol(message)) if message == "a worker named codebase reads 4 it never sent in this session"),
            "{refusal:?}"
        );
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
        let reads = recorded(&[
            (b"a.php", written_with(&[], &[], 0, &[b"app\\rule", b"app\\order"])),
            (b"a.php", written_with(&[], &[], 0, &[b"app\\rule"])),
        ])
        .expect("two class loads of one file");

        assert_eq!(reads.files[&FileId::new(b"a.php")].classes, HashMap::from_iter([(7, 2)]));
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
