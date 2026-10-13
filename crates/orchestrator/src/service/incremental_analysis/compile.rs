//! Compiles each `.sharp` file of the last analysis into the `.sharpc` file the engine runs.

use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use rayon::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

use mago_allocator::LocalArena;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::external::ExternalAnalysisSession;
use mago_analyzer::external::Listing;
use mago_codex::reference::CascadeEdge;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_codex::ttype::TType;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_names::resolver::NameResolver;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_sharp_bridge::InlineForm;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::Refusal;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::check;
use mago_sharp_bridge::inline_forms;
use mago_sharp_bridge::lower;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Read;
use mago_sharp_bridge::unit::Reads;
use mago_sharp_bridge::unit::encode;
use mago_sharp_bridge::unit::key;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_lean::Reach;
use mago_sharp_lean::Translation;
use mago_sharp_lean::proof_file;
use mago_sharp_lean::translate;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file_with_settings;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;

use super::IncrementalAnalysisService;
use crate::error::OrchestratorError;

/// The folder Composer installs packages into. `composer.lock` stands for every file in it.
const VENDOR: &[u8] = b"vendor/";

/// The input that changes whenever Composer changes `vendor/`.
const COMPOSER_LOCK: &[u8] = b"composer.lock";

/// What [`IncrementalAnalysisService::compile`] made of one `.sharp` file.
#[derive(Debug)]
pub enum Compilation {
    /// The bytes of its `.sharpc` file.
    Accepted(Vec<u8>),
    /// Its error-level issues, which stop it from running.
    Refused(IssueCollection),
}

/// A `.sharp` file lowered and translated to Lean, or the errors that refused it.
type Lowered = Result<(Unit, Vec<(Vec<u8>, InlineForm)>, Translation), IssueCollection>;

impl IncrementalAnalysisService {
    /// Compiles each `.sharp` file of the last analysis, in file name order.
    ///
    /// A file is accepted when the analysis reported no error-level issue in it. The standard library's files, those
    /// `File::is_standard_library` marks, lower first, and the others inline their forms.
    ///
    /// An accepted file's inputs are its own source, `composer.lock`, and each file outside `vendor/` whose edit makes
    /// the warm path re-analyze it: each file whose signature edit reaches it through the cascade or reaches a symbol
    /// its extension hooks or providers read, every file when they listed names, each file that declares a class alias
    /// or a patch, each standard library file, whose `[Replaces]` decides which calls compile, and each file that
    /// declares a method whose return it reads from that method's body, along every chain of such returns, with that
    /// file's own inputs.
    ///
    /// A file that states a law also lists its proof file and every source its laws reach. `prove` gets the Lean
    /// translation of every accepted file and returns the issues of each file whose law is not proved, which refuse
    /// that file whatever pragma, ignore entry or baseline the analysis honors. It runs before any input is stamped,
    /// so a proof file it creates is stamped present.
    ///
    /// `stamp` gives the size, modification time and hash of the file at a workspace-relative path, or none when no
    /// file is there. It is called once per input. An input no file is at is stored absent: its size, modification
    /// time and hash are all zero. No file has that stamp, because the hash of an empty file is not zero.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when no analysis ran yet, when re-analyzing a file fails, or when `stamp` or
    /// `prove` fails.
    pub fn compile(
        &mut self,
        mut stamp: impl FnMut(&[u8]) -> std::io::Result<Option<Input>>,
        prove: impl FnOnce(Vec<Translation>) -> std::io::Result<HashMap<FileId, IssueCollection>>,
    ) -> Result<Vec<(FileId, Compilation)>, OrchestratorError> {
        if !self.initialized {
            return Err(OrchestratorError::General("analyze() must be called before compile()".to_string()));
        }

        let mut files: Vec<Arc<File>> = self
            .database
            .files()
            .filter(|file| file.file_type == FileType::Host && Dialect::of(file).is_sharp())
            .collect();
        files.sort_unstable_by(|a, b| a.name.cmp(&b.name));

        let mut errors: HashMap<FileId, IssueCollection> = HashMap::default();
        for issue in self.collect_all_issues() {
            if issue.level == Level::Error
                && let Some(span) = issue.primary_span()
            {
                errors.entry(span.file_id).or_default().push(issue);
            }
        }

        let session = self.plugin_registry.create_external_analysis_session(self.database.files());
        let (library, rest): (Vec<_>, Vec<_>) = files.iter().partition(|file| file.is_standard_library);
        let reach = Reach::new(&self.codebase, &self.native_symbol_references);

        let lowered_library = self.lower_all(&library, &InlineForms::default(), &errors, &reach, session.as_ref())?;
        let forms: InlineForms = lowered_library
            .iter()
            .filter_map(|lowered| lowered.as_ref().ok())
            .flat_map(|(_, forms, _)| forms.iter().cloned())
            .collect();
        let lowered_rest = self.lower_all(&rest, &forms, &errors, &reach, session.as_ref())?;
        let mut translations = Vec::new();
        let mut lowered: HashMap<FileId, Result<Unit, IssueCollection>> = library
            .iter()
            .chain(&rest)
            .map(|file| file.id)
            .zip(lowered_library.into_iter().chain(lowered_rest))
            .map(|(file_id, lowered)| {
                (
                    file_id,
                    lowered.map(|(unit, _, translation)| {
                        translations.push(translation);
                        unit
                    }),
                )
            })
            .collect();
        let mut unproven = prove(translations)
            .map_err(|error| OrchestratorError::General(format!("The Lean step failed: {error}.")))?;

        let mut reads = self.reads();
        let read_by_every_file: HashSet<FileId> = self
            .codebase
            .class_like_alias_declarations()
            .map(|(_, _, span)| span.file_id)
            .chain(
                self.database
                    .files()
                    .filter(|file| file.file_type.is_patch() || file.is_standard_library)
                    .map(|file| file.id),
            )
            .collect();

        let inputs_start = std::time::Instant::now();
        let dependencies = Dependencies::new(self);
        let mut input_paths: HashMap<FileId, Vec<Vec<u8>>> = HashMap::default();
        for file in files.iter().filter(|file| lowered.get(&file.id).is_some_and(Result::is_ok)) {
            // A method whose return comes from its body can return another such method's return, so the body files
            // close over the body files of each file they hold.
            let mut body_files: HashSet<FileId> = HashSet::default();
            let mut pending = vec![file.id];
            while let Some(next) = pending.pop() {
                for body_file in reads.get(&next).into_iter().flat_map(|(_, body_files)| body_files) {
                    if body_files.insert(*body_file) {
                        pending.push(*body_file);
                    }
                }
            }
            let mut sources: HashSet<FileId> = read_by_every_file.clone();
            for source in std::iter::once(file.id).chain(body_files.iter().copied()) {
                sources.extend(dependencies.reached_by(source));
            }
            sources.extend(body_files);
            sources.extend(reach.files(file.id));
            sources.insert(file.id);

            let proof = (!reach.files(file.id).is_empty() && is_source(file)).then(|| proof_file(&file.name));
            let mut paths: Vec<Vec<u8>> = sources
                .into_iter()
                .filter_map(|source| self.database.get(&source).ok())
                .filter(|source| source.id == file.id || is_source(source))
                .map(|source| source.name.to_vec())
                .chain(proof)
                .chain(std::iter::once(COMPOSER_LOCK.to_vec()))
                .collect();
            paths.sort_unstable();
            paths.dedup();
            input_paths.insert(file.id, paths);
        }
        let mut counts: Vec<usize> = input_paths.values().map(Vec::len).collect();
        counts.sort_unstable();
        tracing::debug!(
            "Found the inputs of {} PHP# files in {:?}: a median of {} inputs, and {} at most.",
            counts.len(),
            inputs_start.elapsed(),
            counts.get(counts.len() / 2).copied().unwrap_or_default(),
            counts.last().copied().unwrap_or_default(),
        );

        let mut stamps: HashMap<Vec<u8>, Input> = HashMap::default();
        let mut compiled = Vec::with_capacity(files.len());
        for file in &files {
            let Some(lowered) = lowered.remove(&file.id) else {
                continue;
            };
            let compilation = match (lowered, unproven.remove(&file.id)) {
                (Ok(_), Some(issues)) | (Err(issues), _) => Compilation::Refused(issues),
                (Ok(unit), None) => {
                    let (mut reads, _) = reads.remove(&file.id).unwrap_or_default();
                    reads.inlined = unit.inlined().to_vec();
                    let key = key(source_hash(&file.contents), &reads);

                    let paths = input_paths.remove(&file.id).unwrap_or_default();
                    let mut inputs = Vec::with_capacity(paths.len());
                    for path in paths {
                        let input = match stamps.get(&path) {
                            Some(input) => input.clone(),
                            None => {
                                let input =
                                    stamp(&path).map_err(mago_database::error::DatabaseError::from)?.unwrap_or_else(
                                        || Input { path: path.clone(), size: 0, mtime_ns: 0, hash: [0; 16] },
                                    );
                                stamps.insert(path, input.clone());
                                input
                            }
                        };
                        inputs.push(input);
                    }

                    Compilation::Accepted(encode(&unit, &file.contents, key, &inputs, &[]))
                }
            };

            compiled.push((file.id, compilation));
        }

        Ok(compiled)
    }

    /// What each file's analysis read from other files: each declaration it read through its signature, each method
    /// whose return is taken from its body, with the files that declare those methods, and each declaration and set of
    /// names its extensions read.
    fn reads(&self) -> HashMap<FileId, (Reads, HashSet<FileId>)> {
        let mut declarations: HashMap<SymbolIdentifier, (FileId, u64)> = HashMap::default();
        for (file_id, signature) in &self.codebase.file_signatures {
            for node in &signature.ast_nodes {
                declarations.insert((node.name, empty_word()), (*file_id, node.signature_hash));
                for child in &node.children {
                    declarations.insert((node.name, child.name), (*file_id, child.signature_hash));
                }
            }
        }

        let declared = |target: SymbolIdentifier| -> Option<(SymbolIdentifier, FileId, u64)> {
            let find = |class: mago_word::Word| {
                [(class, target.1), (ascii_lowercase_word(class.as_bytes()), target.1)]
                    .into_iter()
                    .find_map(|member| declarations.get(&member).map(|(file_id, hash)| (member, *file_id, *hash)))
            };
            if target.1.is_empty() {
                return find(target.0);
            }

            let mut class = Some(target.0);
            while let Some(name) = class {
                let metadata = self.codebase.class_likes.get(&ascii_lowercase_word(name.as_bytes()))?;
                if let Some(found) = find(name).or_else(|| metadata.used_traits.iter().find_map(|t| find(*t))) {
                    return Some(found);
                }
                class = metadata.direct_parent_class;
            }

            let metadata = self.codebase.class_likes.get(&target.0)?;
            metadata.all_parent_interfaces.iter().find_map(|interface| find(*interface))
        };

        let mut reads: HashMap<FileId, (Reads, HashSet<FileId>)> = HashMap::default();
        self.native_symbol_references.for_each_reference(|origin, target, _| {
            let origin = match origin {
                ReferenceOrigin::Symbol(symbol) => {
                    declarations.get(&(symbol.0, empty_word())).map(|(file_id, _)| *file_id)
                }
                ReferenceOrigin::File(name) => Some(FileId::new(name.as_bytes())),
            };
            let (Some(origin), Some((target, target_file, hash))) = (origin, declared(target)) else {
                return;
            };
            if origin == target_file {
                return;
            }

            let name = if target.1.is_empty() {
                target.0.as_bytes().to_vec()
            } else {
                [target.0.as_bytes(), b"::", target.1.as_bytes()].concat()
            };
            let (file_reads, body_files) = reads.entry(origin).or_default();
            if let Some(method) = self.codebase.function_likes.get(&target)
                && method.return_from_body.is_some()
                && let Some(returned) = &method.return_type_metadata
            {
                file_reads
                    .bodies
                    .push(Read { name: name.clone(), fingerprint: xxh3_64(returned.type_union.get_id().as_bytes()) });
                body_files.insert(target_file);
            }
            file_reads.signatures.push(Read { name, fingerprint: hash });
        });

        for (file_id, state) in self.file_states.iter().filter(|(_, state)| !state.reads.is_empty()) {
            let (file_reads, _) = reads.entry(*file_id).or_default();
            for symbol in state.reads.symbols.iter().flat_map(|&(name, _)| self.with_ancestors(name)) {
                if let Some((target, target_file, hash)) = declared((symbol, empty_word()))
                    && target_file != *file_id
                {
                    file_reads.signatures.push(Read { name: target.0.as_bytes().to_vec(), fingerprint: hash });
                }
            }
            for listing in &state.reads.listings {
                file_reads
                    .listed
                    .push(Read { name: listing_name(listing), fingerprint: listing.answer(&self.codebase) });
            }
        }

        reads
    }

    /// `name` and, when it names a class-like, every class, interface and trait it inherits from, since an extension
    /// that read a class-like read its inherited members too.
    fn with_ancestors(&self, name: Word) -> Vec<Word> {
        let Some(class_like) = self.codebase.class_likes.get(&name) else {
            return vec![name];
        };

        let classes = std::iter::once(name).chain(class_like.all_parent_classes.iter().copied());
        let traits = classes
            .clone()
            .filter_map(|class| self.codebase.class_likes.get(&ascii_lowercase_word(class.as_bytes())))
            .flat_map(|class| class.used_traits.iter().copied());

        classes.chain(class_like.all_parent_interfaces.iter().copied()).chain(traits).collect()
    }

    /// Lowers each of `files` with `forms` to inline, and translates it to Lean as far as `reach` asks, the files the
    /// analysis reported `errors` in refused.
    fn lower_all(
        &self,
        files: &[&Arc<File>],
        forms: &InlineForms,
        errors: &HashMap<FileId, IssueCollection>,
        reach: &Reach,
        session: Option<&ExternalAnalysisSession>,
    ) -> Result<Vec<Lowered>, OrchestratorError> {
        files
            .par_iter()
            .map(|file| {
                let issues: Vec<Issue> =
                    errors.get(&file.id).map(|issues| issues.iter().cloned().collect()).unwrap_or_default();
                let arena = LocalArena::new();
                let program = parse_file_with_settings(&arena, file, self.parser_settings);
                let names = NameResolver::new(&arena).resolve(program);
                let artifacts = self
                    .file_analyzer(&arena, file, &names, session)
                    .analyze_with_artifacts(program, &mut AnalysisResult::new(SymbolReferences::new()))?;

                Ok(match check(file, program, names, &artifacts, &self.codebase, forms, &issues) {
                    Ok(checked) => Ok((lower(&checked), inline_forms(&checked), translate(&checked, reach))),
                    Err(Refusal::Parse(errors)) => Err(errors.iter().map(Issue::from).collect()),
                    Err(Refusal::Compile(errors)) => Err(IssueCollection::from(errors)),
                })
            })
            .collect()
    }
}

/// The name `listing` enters a key under: what it lists, and the class-like or namespace it lists for.
fn listing_name(listing: &Listing) -> Vec<u8> {
    match listing {
        Listing::ClassLikes(kind) => format!("class-likes of kind {kind}").into_bytes(),
        Listing::Functions => b"functions".to_vec(),
        Listing::Constants => b"constants".to_vec(),
        Listing::DirectDescendants(class) => [b"direct descendants of ", class.as_bytes()].concat(),
        Listing::Descendants(class) => [b"descendants of ", class.as_bytes()].concat(),
        Listing::Namespace(namespace) => [b"namespace ", namespace.as_bytes()].concat(),
        Listing::Methods => b"methods".to_vec(),
    }
}

/// Returns `true` for a file whose edit can make a `.sharpc` file stale: every loaded file outside `vendor/`, which
/// `composer.lock` stands for, other than the prelude's.
fn is_source(file: &File) -> bool {
    file.file_type != FileType::Builtin && !file.name.starts_with(VENDOR)
}

/// The cascade's edges, indexed to walk backward from a file to the source files whose signature edit makes the warm
/// path re-analyze it.
///
/// With every declaration of a source file `E` changed, the warm path re-analyzes a file `F` when the cascade from
/// `E`'s declarations reaches `F`: its top-level code, one of its declarations, or a member of one.
/// [`SymbolReferences::get_invalid_symbols`] walks that forward from `E`. This walks the same
/// [`SymbolReferences::cascade_edges`] backward from `F`, to every declaration whose change reaches it, and answers
/// the files that declare them.
///
/// It skips the [`CascadeEdge::Inherits`] edges. The compile step runs after a full analysis, which records a
/// signature reference from every class to each parent, interface and trait it has, so a parent's change reaches the
/// class through that reference. The warm cascade follows `Inherits` edges for the warm graph, which can lose the
/// reference while a parent is deleted and re-added.
struct Dependencies<'service> {
    service: &'service IncrementalAnalysisService,
    /// The declarations each declaration's signature references.
    signature: HashMap<SymbolIdentifier, Vec<SymbolIdentifier>>,
    /// The declarations each declaration's signature or body references.
    references: HashMap<SymbolIdentifier, Vec<SymbolIdentifier>>,
    /// The declarations each file's top-level code references, by file name.
    files: HashMap<Word, Vec<SymbolIdentifier>>,
    /// The members of each class a cascade can invalidate through their signature: the members files declare, and
    /// the members whose signature references a declaration.
    signature_members: HashMap<Word, Vec<SymbolIdentifier>>,
    /// The members of each class whose signature or body references a declaration.
    referencing_members: HashMap<Word, Vec<SymbolIdentifier>>,
    /// The source files that declare each declaration.
    declared_in: HashMap<SymbolIdentifier, Vec<FileId>>,
}

impl<'service> Dependencies<'service> {
    fn new(service: &'service IncrementalAnalysisService) -> Self {
        let mut dependencies = Self {
            service,
            signature: HashMap::default(),
            references: HashMap::default(),
            files: HashMap::default(),
            signature_members: HashMap::default(),
            referencing_members: HashMap::default(),
            declared_in: HashMap::default(),
        };

        for edge in service.native_symbol_references.cascade_edges(&service.codebase) {
            match edge {
                CascadeEdge::Signature { from, to } => {
                    if !dependencies.signature.contains_key(&from) && !from.1.is_empty() {
                        dependencies.signature_members.entry(from.0).or_default().push(from);
                    }
                    dependencies.signature.entry(from).or_default().push(to);
                    dependencies.add_reference(from, to);
                }
                CascadeEdge::Body { from, to } => dependencies.add_reference(from, to),
                CascadeEdge::File { file, to } => dependencies.files.entry(file).or_default().push(to),
                CascadeEdge::Inherits { .. } => {}
            }
        }

        for (file_id, signature) in &service.codebase.file_signatures {
            if !service.database.get(file_id).is_ok_and(|file| is_source(&file)) {
                continue;
            }

            for node in signature.ast_nodes.iter().filter(|node| !node.name.is_empty()) {
                dependencies.declared_in.entry((node.name, empty_word())).or_default().push(*file_id);
                for child in node.children.iter().filter(|child| !child.name.is_empty()) {
                    let member = (node.name, child.name);
                    dependencies.declared_in.entry(member).or_default().push(*file_id);
                    dependencies.signature_members.entry(node.name).or_default().push(member);
                }
            }
        }

        dependencies
    }

    fn add_reference(&mut self, from: SymbolIdentifier, to: SymbolIdentifier) {
        if !self.references.contains_key(&from) && !from.1.is_empty() {
            self.referencing_members.entry(from.0).or_default().push(from);
        }
        self.references.entry(from).or_default().push(to);
    }

    /// The source files other than `file_id` whose signature edit makes the warm path re-analyze `file_id`.
    fn reached_by(&self, file_id: FileId) -> HashSet<FileId> {
        let reads = self.service.file_states.get(&file_id).map(|state| &state.reads);
        let signature = self.service.codebase.get_file_signature(&file_id);
        let Some(signature) = signature.filter(|_| reads.is_none_or(|reads| reads.listings.is_empty())) else {
            // The warm path re-analyzes a file with no signature after every signature edit, and a file whose hooks or
            // providers listed names after any edit that adds or removes one.
            return self
                .service
                .codebase
                .file_signatures
                .keys()
                .copied()
                .filter(|source| *source != file_id)
                .filter(|source| self.service.database.get(source).is_ok_and(|file| is_source(&file)))
                .collect();
        };

        let mut changes = HashSet::default();
        let mut visited = HashSet::default();
        if let Ok(file) = self.service.database.get(&file_id) {
            for referenced in self.files.get(&mago_word::word(file.name.as_ref())).into_iter().flatten() {
                self.signature_changes(*referenced, &mut changes, &mut visited);
            }
        }
        for node in signature.ast_nodes.iter().filter(|node| !node.name.is_empty()) {
            self.changes((node.name, empty_word()), &mut changes, &mut visited);
            for member in self.referencing_members.get(&node.name).into_iter().flatten() {
                self.changes(*member, &mut changes, &mut visited);
            }
            for member in self.signature_members.get(&node.name).into_iter().flatten() {
                self.changes(*member, &mut changes, &mut visited);
            }
        }
        for read in reads.into_iter().flat_map(|reads| &reads.symbols) {
            self.signature_changes(*read, &mut changes, &mut visited);
        }

        changes
            .iter()
            .filter_map(|change| self.declared_in.get(change))
            .flatten()
            .copied()
            .filter(|source| *source != file_id)
            .collect()
    }

    /// Adds to `changes` each declaration whose change invalidates `declaration`'s signature or body.
    fn changes(
        &self,
        declaration: SymbolIdentifier,
        changes: &mut HashSet<SymbolIdentifier>,
        visited: &mut HashSet<SymbolIdentifier>,
    ) {
        self.signature_changes(declaration, changes, visited);
        for referenced in self.references.get(&declaration).into_iter().flatten() {
            self.signature_changes(*referenced, changes, visited);
        }
    }

    /// Adds to `changes` each declaration whose change invalidates `declaration`'s signature: a declaration whose
    /// signature change reaches it through signature references and from members to their class, and the class of a
    /// member whose signature references anything, which marks that member invalid when the class changes.
    fn signature_changes(
        &self,
        declaration: SymbolIdentifier,
        changes: &mut HashSet<SymbolIdentifier>,
        visited: &mut HashSet<SymbolIdentifier>,
    ) {
        if self.signature.contains_key(&declaration) {
            changes.insert((declaration.0, empty_word()));
        }

        let mut pending = vec![declaration];
        while let Some(next) = pending.pop() {
            if !visited.insert(next) {
                continue;
            }

            changes.insert(next);
            pending.extend(self.signature.get(&next).into_iter().flatten().copied());
            if next.1.is_empty() {
                pending.extend(self.signature_members.get(&next.0).into_iter().flatten().copied());
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::little_endian_bytes)]
mod tests {
    use std::borrow::Cow;
    use std::path::Path;
    use std::sync::LazyLock;

    use std::collections::BTreeMap;
    use std::collections::BTreeSet;

    use foldhash::HashSet;
    use mago_analyzer::plugin::PluginRegistry;
    use mago_analyzer::settings::Settings;
    use mago_codex::diff::CodebaseDiff;
    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::signature::FileSignature;
    use mago_database::Database;
    use mago_database::DatabaseConfiguration;
    use mago_prelude::Prelude;
    use mago_sharp_bridge::unit::header;
    use mago_syntax::settings::ParserSettings;

    use super::*;

    static PLUGIN_REGISTRY: LazyLock<Arc<PluginRegistry>> =
        LazyLock::new(|| Arc::new(PluginRegistry::with_library_providers()));
    static PRELUDE: LazyLock<Prelude> = LazyLock::new(Prelude::build);

    fn prelude() -> (CodebaseMetadata, SymbolReferences) {
        let Prelude { metadata, symbol_references, .. } = PRELUDE.clone();

        (metadata, symbol_references)
    }

    /// A project of files, each a workspace-relative name and its contents. A PHP file under `vendor/` is vendored, and
    /// every other file is a host file, as `mago compile` loads every `.sharp` file. A file under the standard
    /// library's package is the library's, as the loader marks it from the package's `composer.json`.
    fn project(files: &[(&str, &str)]) -> Database<'static> {
        let mut database =
            Database::new(DatabaseConfiguration::new(Path::new("/project"), vec![], vec![], vec![], vec![]));
        for (name, contents) in files {
            let file_type = if name.starts_with("vendor/")
                && Path::new(name).extension().is_some_and(|extension| extension == "php")
            {
                FileType::Vendored
            } else {
                FileType::Host
            };
            let mut file = File::new(
                Cow::Owned(name.as_bytes().to_vec()),
                file_type,
                Some(Path::new("/project").join(name)),
                Cow::Owned(contents.as_bytes().to_vec()),
            );
            file.is_standard_library = name.starts_with("vendor/heyjordanparker/php-sharp-composer/");
            database.add(file);
        }

        database
    }

    /// The service after a full analysis of `database`.
    fn analyzed(database: &Database<'static>) -> IncrementalAnalysisService {
        let mut service = IncrementalAnalysisService::new(
            database.read_only(),
            prelude,
            Settings::default(),
            ParserSettings::default(),
            Arc::clone(&PLUGIN_REGISTRY),
        );
        service.analyze().expect("the analysis runs");

        service
    }

    /// Stamps every input as an empty file, so a test sees which inputs a file has.
    fn empty_stamp(path: &[u8]) -> std::io::Result<Option<Input>> {
        Ok(Some(Input { path: path.to_vec(), size: 0, mtime_ns: 0, hash: [0; 16] }))
    }

    /// Stamps each file of `database` with its size and hash, and `composer.lock` as an empty file.
    fn database_stamp(database: &Database<'static>) -> impl FnMut(&[u8]) -> std::io::Result<Option<Input>> {
        let database = database.read_only();

        move |path| {
            if path == b"composer.lock" {
                return empty_stamp(path);
            }

            Ok(database.get(&FileId::new(path)).ok().map(|file| Input {
                path: path.to_vec(),
                size: file.contents.len() as u64,
                mtime_ns: 0,
                hash: source_hash(&file.contents),
            }))
        }
    }

    /// Proves every law, as a Lean step that refuses nothing.
    fn proved(_: Vec<Translation>) -> std::io::Result<HashMap<FileId, IssueCollection>> {
        Ok(HashMap::default())
    }

    /// Each compiled file of `service` by its name.
    fn compile(
        service: &mut IncrementalAnalysisService,
        stamp: impl FnMut(&[u8]) -> std::io::Result<Option<Input>>,
    ) -> std::collections::BTreeMap<String, Compilation> {
        let compiled = service.compile(stamp, proved).expect("the compile runs");
        let database = service.database();

        compiled
            .into_iter()
            .map(|(file_id, compilation)| {
                (
                    String::from_utf8_lossy(&database.get(&file_id).expect("a compiled file exists").name).into_owned(),
                    compilation,
                )
            })
            .collect()
    }

    /// The `.sharpc` bytes of the accepted file `name`.
    fn accepted<'compiled>(
        compiled: &'compiled std::collections::BTreeMap<String, Compilation>,
        name: &str,
    ) -> &'compiled [u8] {
        match compiled.get(name) {
            Some(Compilation::Accepted(bytes)) => bytes,
            other => panic!("{name} is not accepted: {other:?}"),
        }
    }

    fn compiled_key(bytes: &[u8]) -> [u8; 16] {
        header(bytes).expect("the header is valid").key
    }

    /// Each input a `.sharpc` file lists, read at the offsets `sharp_unit.h` gives.
    fn inputs(bytes: &[u8]) -> Vec<Input> {
        let word = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let (count, nodes, children) = (word(80), word(84), word(88));
        let texts = 104 + 40 * count + 56 * nodes + 4 * children;

        (0..count)
            .map(|index| {
                let input = 104 + 40 * index;
                let (offset, length) = (word(input), word(input + 4));

                Input {
                    path: bytes[texts + offset..texts + offset + length].to_vec(),
                    size: u64::from_le_bytes(bytes[input + 8..input + 16].try_into().unwrap()),
                    mtime_ns: i64::from_le_bytes(bytes[input + 16..input + 24].try_into().unwrap()),
                    hash: bytes[input + 24..input + 40].try_into().unwrap(),
                }
            })
            .collect()
    }

    fn input_paths(bytes: &[u8]) -> Vec<String> {
        inputs(bytes).into_iter().map(|input| String::from_utf8_lossy(&input.path).into_owned()).collect()
    }

    fn messages(issues: &IssueCollection) -> Vec<String> {
        let mut messages: Vec<String> = issues.iter().map(|issue| issue.message.clone()).collect();
        messages.sort_unstable();
        messages
    }

    const ORDER: &str = "namespace App;\n\npublic class Order\n{\n    public int total(int extra)\n    {\n        return extra + 1;\n    }\n}\n";

    #[test]
    fn an_accepted_file_compiles_to_bytes_whose_header_names_its_source() {
        let database = project(&[("app/Order.sharp", ORDER)]);
        let mut service = analyzed(&database);

        let compiled = service.compile(empty_stamp, proved).expect("the compile runs");

        assert_eq!(compiled.len(), 1, "{compiled:?}");
        let (file_id, Compilation::Accepted(bytes)) = &compiled[0] else {
            panic!("app/Order.sharp is refused: {compiled:?}");
        };
        assert_eq!(*file_id, FileId::new(b"app/Order.sharp"));
        let header = header(bytes).expect("the header is valid");
        assert_eq!(header.source_hash, source_hash(ORDER.as_bytes()));
        assert_eq!(header.source_size, ORDER.len() as u64);
    }

    #[test]
    fn a_file_with_a_type_error_is_refused_with_its_errors_and_a_php_file_gets_no_compiled_file() {
        let broken = "namespace App;\n\npublic class Broken\n{\n    public int total()\n    {\n        return \"one\";\n    }\n}\n";
        let database = project(&[
            ("app/Order.sharp", ORDER),
            ("app/Broken.sharp", broken),
            ("lib/Legacy.php", "<?php\n\nnamespace Lib;\n\nfinal class Legacy {}\n"),
        ]);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(compiled.keys().collect::<Vec<_>>(), ["app/Broken.sharp", "app/Order.sharp"]);
        accepted(&compiled, "app/Order.sharp");
        let Some(Compilation::Refused(errors)) = compiled.get("app/Broken.sharp") else {
            panic!("app/Broken.sharp is not refused: {compiled:?}");
        };
        assert!(!errors.is_empty());
        assert!(errors.iter().all(|issue| issue.level == Level::Error), "{errors:?}");
    }

    const MONEY: &str = "namespace App;\n\npublic class Money\n{\n    public const int CENTS = 100;\n\n    public int amount()\n    {\n        return 1;\n    }\n}\n";
    const ORDER_OF_MONEY: &str = "namespace App;\n\nimport Acme.Tax;\nimport Lib.Rate;\n\npublic class Order\n{\n    public int total(Money money)\n    {\n        return money.amount() + Rate.percent() + Tax.rate();\n    }\n}\n";
    const CASH: &str = "namespace App;\n\npublic class Cash : Money\n{\n}\n";
    const RATES: &str = "namespace App;\n\npublic class Rates\n{\n    public int cents()\n    {\n        return Money.CENTS;\n    }\n}\n";
    const UNRELATED: &str =
        "namespace App;\n\npublic class Unrelated\n{\n    public int one()\n    {\n        return 1;\n    }\n}\n";
    const RATE: &str = "<?php\n\nnamespace Lib;\n\nfinal class Rate\n{\n    public static function percent(): int\n    {\n        return 5;\n    }\n}\n";
    const TAX: &str = "<?php\n\nnamespace Acme;\n\nfinal class Tax\n{\n    public static function rate(): int\n    {\n        return 2;\n    }\n}\n";

    /// The project the input and key tests edit.
    fn shop(money: &str, rate: &str) -> Vec<(&'static str, String)> {
        vec![
            ("app/Money.sharp", money.to_string()),
            ("app/Order.sharp", ORDER_OF_MONEY.to_string()),
            ("app/Cash.sharp", CASH.to_string()),
            ("app/Rates.sharp", RATES.to_string()),
            ("app/Unrelated.sharp", UNRELATED.to_string()),
            ("lib/Rate.php", rate.to_string()),
            ("vendor/acme/tax/src/Tax.php", TAX.to_string()),
        ]
    }

    fn shop_project(files: &[(&'static str, String)]) -> Database<'static> {
        project(&files.iter().map(|(name, contents)| (*name, contents.as_str())).collect::<Vec<_>>())
    }

    #[test]
    fn a_file_names_its_source_each_file_whose_edit_reanalyzes_it_and_composer_lock_in_path_order() {
        let database = shop_project(&shop(MONEY, RATE));
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Order.sharp")),
            ["app/Money.sharp", "app/Order.sharp", "composer.lock", "lib/Rate.php"],
            "a vendor file is left to composer.lock"
        );
        assert_eq!(input_paths(accepted(&compiled, "app/Unrelated.sharp")), ["app/Unrelated.sharp", "composer.lock"]);
    }

    #[test]
    fn a_package_file_names_its_own_source_and_leaves_the_rest_of_its_package_to_composer_lock() {
        let database = project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)]);

        let compiled = compile(&mut analyzed(&database), empty_stamp);

        assert_eq!(input_paths(accepted(&compiled, TEXT_PATH)), ["composer.lock", TEXT_PATH]);
        assert_eq!(input_paths(accepted(&compiled, "app/Title.sharp")), ["app/Title.sharp", "composer.lock"]);
    }

    #[test]
    fn an_input_no_file_is_at_is_stored_absent_as_an_all_zero_stamp() {
        let database = project(&[("app/Order.sharp", ORDER)]);
        let mut stamp = database_stamp(&database);

        let compiled =
            compile(&mut analyzed(&database), |path| if path == b"composer.lock" { Ok(None) } else { stamp(path) });

        assert_eq!(
            inputs(accepted(&compiled, "app/Order.sharp")),
            [
                Input {
                    path: b"app/Order.sharp".to_vec(),
                    size: ORDER.len() as u64,
                    mtime_ns: 0,
                    hash: source_hash(ORDER.as_bytes()),
                },
                Input { path: b"composer.lock".to_vec(), size: 0, mtime_ns: 0, hash: [0; 16] },
            ]
        );
    }

    #[test]
    fn an_edit_to_a_source_changes_only_its_own_input_in_its_compiled_file() {
        let edited = ORDER_OF_MONEY.replace("Tax.rate()", "Tax.rate() + 1");
        let mut files = shop(MONEY, RATE);
        for (name, contents) in &mut files {
            if *name == "app/Order.sharp" {
                contents.clone_from(&edited);
            }
        }
        let before = shop_project(&shop(MONEY, RATE));
        let after = shop_project(&files);

        let before_compiled = compile(&mut analyzed(&before), database_stamp(&before));
        let after_compiled = compile(&mut analyzed(&after), database_stamp(&after));

        let changed: Vec<Input> = inputs(accepted(&after_compiled, "app/Order.sharp"))
            .into_iter()
            .filter(|input| !inputs(accepted(&before_compiled, "app/Order.sharp")).contains(input))
            .collect();
        assert_eq!(
            changed,
            [Input {
                path: b"app/Order.sharp".to_vec(),
                size: edited.len() as u64,
                mtime_ns: 0,
                hash: source_hash(edited.as_bytes()),
            }]
        );
    }

    #[test]
    fn a_file_that_declares_a_class_alias_is_an_input_of_every_file() {
        let mut files = shop(MONEY, RATE);
        files.push(("lib/aliases.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'Lib\\\\Pace');\n".to_string()));
        let database = shop_project(&files);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Unrelated.sharp")),
            ["app/Unrelated.sharp", "composer.lock", "lib/aliases.php"]
        );
    }

    #[test]
    fn each_input_is_stamped_once_however_many_files_name_it() {
        let database = shop_project(&shop(MONEY, RATE));
        let mut service = analyzed(&database);
        let mut stamped: std::collections::BTreeMap<Vec<u8>, usize> = std::collections::BTreeMap::new();

        compile(&mut service, |path| {
            *stamped.entry(path.to_vec()).or_default() += 1;
            empty_stamp(path)
        });

        assert!(stamped.contains_key(b"app/Money.sharp".as_slice()), "{stamped:?}");
        assert!(stamped.values().all(|count| *count == 1), "{stamped:?}");
    }

    #[test]
    fn a_signature_edit_in_a_dependency_gives_its_dependents_new_keys() {
        let before = shop_project(&shop(MONEY, RATE));
        let after = shop_project(&shop(&MONEY.replace("int amount()", "int amount(int scale = 1)"), RATE));

        let before = compile(&mut analyzed(&before), empty_stamp);
        let after = compile(&mut analyzed(&after), empty_stamp);

        for dependent in ["app/Order.sharp", "app/Cash.sharp"] {
            assert_ne!(
                compiled_key(accepted(&before, dependent)),
                compiled_key(accepted(&after, dependent)),
                "{dependent}"
            );
        }
        assert_eq!(
            compiled_key(accepted(&before, "app/Unrelated.sharp")),
            compiled_key(accepted(&after, "app/Unrelated.sharp"))
        );
    }

    #[test]
    fn a_body_edit_that_keeps_the_return_refreshes_the_dependents_stamps_and_keeps_their_keys() {
        let before = shop_project(&shop(MONEY, RATE));
        let after = shop_project(&shop(&MONEY.replace("return 1;", "return 2;"), RATE));

        let before_compiled = compile(&mut analyzed(&before), database_stamp(&before));
        let after_compiled = compile(&mut analyzed(&after), database_stamp(&after));

        let order_before = accepted(&before_compiled, "app/Order.sharp");
        let order_after = accepted(&after_compiled, "app/Order.sharp");
        assert_eq!(compiled_key(order_before), compiled_key(order_after));
        let money = |bytes: &[u8]| {
            inputs(bytes).into_iter().find(|input| input.path == b"app/Money.sharp").map(|input| input.hash)
        };
        assert_ne!(money(order_before), money(order_after));
        assert_eq!(money(order_after), Some(source_hash(MONEY.replace("return 1;", "return 2;").as_bytes())));
    }

    #[test]
    fn an_inherited_member_is_read_from_the_class_that_declares_it() {
        let reader = "namespace App;\n\npublic class Reader\n{\n    public int read(Cash cash)\n    {\n        return cash.amount();\n    }\n}\n";
        let files = |money: &str| {
            let mut files = shop(money, RATE);
            files.push(("app/Reader.sharp", reader.to_string()));
            files
        };
        let before = shop_project(&files(MONEY));
        let after = shop_project(&files(&MONEY.replace("int amount()", "int amount(int scale = 1)")));

        let before = compile(&mut analyzed(&before), empty_stamp);
        let after = compile(&mut analyzed(&after), empty_stamp);

        assert!(input_paths(accepted(&before, "app/Reader.sharp")).contains(&"app/Money.sharp".to_string()));
        assert_ne!(
            compiled_key(accepted(&before, "app/Reader.sharp")),
            compiled_key(accepted(&after, "app/Reader.sharp"))
        );
    }

    const TEXT: &str = "namespace Sharp;\n\npublic class Text\n{\n    public static string shout(string text) => strtoupper(text);\n}\n";
    const TITLE: &str = "namespace App;\n\nimport Sharp.Text;\n\npublic class Title\n{\n    public string of(string name)\n    {\n        return Text.shout(name);\n    }\n}\n";
    const TEXT_PATH: &str = "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text.sharp";

    #[test]
    fn a_caller_inlines_the_library_form_and_a_library_body_edit_changes_its_key() {
        let before = project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)]);
        let lowered = TEXT.replace("strtoupper", "strtolower");
        let after = project(&[(TEXT_PATH, lowered.as_str()), ("app/Title.sharp", TITLE)]);

        let before = compile(&mut analyzed(&before), empty_stamp);
        let after = compile(&mut analyzed(&after), empty_stamp);

        let title = accepted(&before, "app/Title.sharp");
        assert!(title.windows(b"strtoupper".len()).any(|window| window == b"strtoupper"), "the form is inlined");
        accepted(&before, TEXT_PATH);
        assert_ne!(compiled_key(title), compiled_key(accepted(&after, "app/Title.sharp")));
    }

    /// One edit to the shop project, and the files the edit truly reaches.
    struct Edit {
        name: &'static str,
        edited: &'static str,
        money: String,
        rate: String,
        deleted: bool,
        reaches: &'static [&'static str],
    }

    fn edits() -> Vec<Edit> {
        let edit = |name, edited, money: String, rate: &str, reaches| Edit {
            name,
            edited,
            money,
            rate: rate.to_string(),
            deleted: false,
            reaches,
        };

        vec![
            edit("a body edit", "app/Money.sharp", MONEY.replace("return 1;", "return 2;"), RATE, &[]),
            edit("a blank line", "app/Money.sharp", MONEY.replace("{\n    public", "{\n\n    public"), RATE, &[]),
            edit(
                "a moved member",
                "app/Money.sharp",
                "namespace App;\n\npublic class Money\n{\n    public int amount()\n    {\n        return 1;\n    }\n\n    public const int CENTS = 100;\n}\n".to_string(),
                RATE,
                &[],
            ),
            edit(
                "a signature change with a descendant",
                "app/Money.sharp",
                MONEY.replace("int amount()", "int amount(int scale = 1)"),
                RATE,
                &["app/Cash.sharp", "app/Order.sharp"],
            ),
            edit(
                "a parent gaining a method",
                "app/Money.sharp",
                MONEY.replace("    public int amount()", "    public int fee()\n    {\n        return 0;\n    }\n\n    public int amount()"),
                RATE,
                &["app/Cash.sharp"],
            ),
            edit("a constant change", "app/Money.sharp", MONEY.replace("= 100", "= 1000"), RATE, &["app/Rates.sharp"]),
            edit(
                "a PHP dependency's signature change",
                "lib/Rate.php",
                MONEY.to_string(),
                &RATE.replace("percent(): int", "percent(int $scale = 1): int"),
                &["app/Order.sharp"],
            ),
            Edit {
                name: "a removed class",
                edited: "app/Money.sharp",
                money: MONEY.to_string(),
                rate: RATE.to_string(),
                deleted: true,
                reaches: &["app/Cash.sharp", "app/Order.sharp", "app/Rates.sharp"],
            },
        ]
    }

    #[test]
    fn the_files_whose_inputs_name_an_edited_file_hold_every_file_the_edit_reaches_and_the_warm_path_rechecks() {
        let base = shop_project(&shop(MONEY, RATE));
        let mut service = analyzed(&base);
        let compiled = compile(&mut service, empty_stamp);

        for edit in edits() {
            let naming: HashSet<&str> = compiled
                .iter()
                .filter(|(_, compilation)| {
                    matches!(compilation, Compilation::Accepted(bytes) if input_paths(bytes).iter().any(|path| path == edit.edited))
                })
                .map(|(name, _)| name.as_str())
                .collect();

            let mut files = shop(&edit.money, &edit.rate);
            if edit.deleted {
                files.retain(|(name, _)| *name != edit.edited);
            }
            let edited = shop_project(&files);

            let mut fresh = analyzed(&edited);
            let fresh_compiled = compile(&mut fresh, empty_stamp);
            let changed = |name: &str| match (compiled.get(name), fresh_compiled.get(name)) {
                (Some(Compilation::Refused(before)), Some(Compilation::Refused(after))) => {
                    messages(before) != messages(after)
                }
                (Some(Compilation::Accepted(before)), Some(Compilation::Accepted(after))) => {
                    compiled_key(before) != compiled_key(after)
                }
                (_, None) => false,
                _ => true,
            };
            let changed = compiled.keys().map(String::as_str).filter(|name| *name != edit.edited && changed(name));
            for reached in edit.reaches.iter().copied().chain(changed) {
                assert!(naming.contains(reached), "{}: {reached} does not name {}: {naming:?}", edit.name, edit.edited);
            }

            // Deleting a file removes the class it declares, and the warm path then runs a full analysis, which
            // rechecks every file.
            if edit.deleted {
                continue;
            }

            let mut warm = analyzed(&base);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let database = warm.database();
            for file_id in &warm.analyzed_files {
                let name =
                    String::from_utf8_lossy(&database.get(file_id).expect("an analyzed file exists").name).into_owned();
                if name != edit.edited && Path::new(&name).extension().is_some_and(|extension| extension == "sharp") {
                    assert!(
                        naming.contains(name.as_str()),
                        "{}: the warm path rechecks {name}, which does not name {}",
                        edit.name,
                        edit.edited
                    );
                }
            }
        }
    }

    fn declarations(signature: &FileSignature) -> Vec<SymbolIdentifier> {
        signature
            .ast_nodes
            .iter()
            .flat_map(|node| {
                std::iter::once((node.name, empty_word()))
                    .chain(node.children.iter().map(|child| (node.name, child.name)))
            })
            .collect()
    }

    /// For each file of `service`, the source files whose signature edit makes the warm path re-analyze it, found
    /// forward: each source file runs the warm path's cascade with every declaration it holds changed, then the warm
    /// path's rule for the files it skips.
    fn forward_reached_by(service: &mut IncrementalAnalysisService) -> BTreeMap<String, BTreeSet<String>> {
        let database = &service.database;
        let name = |file_id: &FileId| {
            String::from_utf8_lossy(&database.get(file_id).expect("a file exists").name).into_owned()
        };
        let every_declaration: Vec<SymbolIdentifier> =
            service.codebase.file_signatures.values().flat_map(declarations).collect();
        let files: Vec<FileId> =
            database.files().filter(|file| file.file_type != FileType::Builtin).map(|file| file.id).collect();

        let mut reached_by: BTreeMap<String, BTreeSet<String>> =
            files.iter().map(|file_id| (name(file_id), BTreeSet::new())).collect();
        for edited in database.files().filter(|file| is_source(file)) {
            let Some(signature) = service.codebase.get_file_signature(&edited.id) else {
                continue;
            };
            let diff =
                CodebaseDiff::new().with_keep(every_declaration.iter().copied()).with_changed(declarations(signature));
            service.codebase.safe_symbols.clear();
            service.codebase.safe_symbol_members.clear();
            let unchanged: Vec<FileId> = files.iter().copied().filter(|file_id| *file_id != edited.id).collect();
            let reads = super::super::symbols_read_by(&service.file_states, &service.database, &unchanged);
            let invalid_files = service
                .codebase
                .mark_safe_symbols(&diff, &service.native_symbol_references, reads)
                .expect("each fixture's cascade ends within the warm path's limit");

            let skipped = service.files_to_skip(&service.codebase, &unchanged, &invalid_files);
            for reached in unchanged.iter().filter(|file_id| !skipped.contains(*file_id)) {
                reached_by.get_mut(&name(reached)).expect("every file is listed").insert(name(&edited.id));
            }
        }

        reached_by
    }

    /// Fails with each file whose source files the backward walk finds differently from the forward cascade. Returns
    /// how many pairs of a file and a source file whose edit reaches it the walks agree on.
    fn assert_the_walks_agree(label: &str, database: &Database<'static>) -> usize {
        let mut service = analyzed(database);
        let forward = forward_reached_by(&mut service);

        let dependencies = Dependencies::new(&service);
        let name = |file_id: &FileId| {
            String::from_utf8_lossy(&service.database.get(file_id).expect("a file exists").name).into_owned()
        };
        let mut differences = Vec::new();
        let mut pairs = 0;
        for (file, expected) in &forward {
            let found: BTreeSet<String> =
                dependencies.reached_by(FileId::new(file.as_bytes())).iter().map(name).collect();
            pairs += expected.len();
            if found != *expected {
                differences.push(format!(
                    "{file}: the backward walk misses {:?} and adds {:?}",
                    expected.difference(&found).collect::<Vec<_>>(),
                    found.difference(expected).collect::<Vec<_>>()
                ));
            }
        }

        assert!(differences.is_empty(), "{label}:\n{}", differences.join("\n"));

        pairs
    }

    #[test]
    fn the_backward_walk_finds_what_the_forward_cascade_finds_in_every_fixture() {
        let reader = "namespace App;\n\npublic class Reader\n{\n    public int read(Cash cash)\n    {\n        return cash.amount();\n    }\n}\n";
        let mut fixtures: Vec<(String, Database<'static>)> =
            vec![("the shop".to_string(), shop_project(&shop(MONEY, RATE)))];
        for edit in edits() {
            let mut files = shop(&edit.money, &edit.rate);
            if edit.deleted {
                files.retain(|(name, _)| *name != edit.edited);
            }
            fixtures.push((format!("the shop after {}", edit.name), shop_project(&files)));
        }
        let mut aliased = shop(MONEY, RATE);
        aliased.push(("lib/aliases.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'Lib\\\\Pace');\n".to_string()));
        fixtures.push(("the shop with an alias".to_string(), shop_project(&aliased)));
        let mut read = shop(MONEY, RATE);
        read.push(("app/Reader.sharp", reader.to_string()));
        fixtures.push(("the shop with an inherited member read".to_string(), shop_project(&read)));
        fixtures.push(("the library".to_string(), project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)])));
        fixtures.push((
            "a PHP# file at the head of a chain through types named only in the signatures of members".to_string(),
            project(&[
                (
                    "app/Hint.sharp",
                    "namespace App;\n\nimport Lib.A;\n\npublic class Hint\n{\n    public int take(A a)\n    {\n        return 1;\n    }\n}\n",
                ),
                (
                    "lib/A.php",
                    "<?php\n\nnamespace Lib;\n\nclass A\n{\n    public function f(?B $b = null): void {}\n}\n",
                ),
                (
                    "lib/B.php",
                    "<?php\n\nnamespace Lib;\n\nclass B\n{\n    public function g(?D $d = null): void {}\n}\n",
                ),
                ("lib/D.php", "<?php\n\nnamespace Lib;\n\nclass D\n{\n}\n"),
            ]),
        ));
        fixtures.push((
            "a package class that extends a project class".to_string(),
            project(&[
                ("lib/Base.php", "<?php\n\nnamespace Lib;\n\nclass Base\n{\n}\n"),
                ("vendor/acme/fee/src/Fee.php", "<?php\n\nnamespace Acme;\n\nclass Fee extends \\Lib\\Base\n{\n}\n"),
            ]),
        ));

        let pairs: usize = fixtures.iter().map(|(label, database)| assert_the_walks_agree(label, database)).sum();
        assert!(pairs > fixtures.len(), "the fixtures hold {pairs} pairs, too few for the walks to differ on");
    }

    /// A xorshift generator, so a generated project is the same on every run.
    struct Random(u64);

    impl Random {
        /// A number below `bound`.
        fn below(&mut self, bound: usize) -> usize {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;

            usize::try_from(self.0 % bound.max(1) as u64).expect("a number below a usize fits in one")
        }

        /// One time in three, one of the indexes below `below` that `wanted` accepts, and otherwise none. Most
        /// declarations then have no parent, interface or trait, so they form a forest of shallow trees, as in real
        /// code, and not one tree whose root reaches every declaration.
        fn earlier(&mut self, below: usize, wanted: impl Fn(usize) -> bool) -> Option<usize> {
            let candidates: Vec<usize> = (0..below).filter(|index| wanted(*index)).collect();

            (!candidates.is_empty() && self.below(3) == 0).then(|| candidates[self.below(candidates.len())])
        }
    }

    /// What the generated file at an index declares.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Generated {
        Interface,
        Trait,
        Script,
        Class,
    }

    impl Generated {
        fn at(index: usize) -> Self {
            match index % 10 {
                0 => Self::Interface,
                1 => Self::Trait,
                2 => Self::Script,
                _ => Self::Class,
            }
        }
    }

    /// A deterministic project of `count` PHP files whose classes, interfaces, traits, functions and top-level code
    /// reference each other at random through parents, signatures, bodies and constants. Every tenth file sits under
    /// `vendor/`.
    fn generated_project(count: usize, seed: u64) -> Vec<(String, String)> {
        let mut random = Random(seed);
        let class = |random: &mut Random| {
            let start = random.below(count);
            let index = (start..count).chain(0..start).find(|index| Generated::at(*index) == Generated::Class);
            format!("Class{}", index.expect("a generated project holds a class"))
        };
        // Most declared types are scalars, so a signature edit reaches part of the project, as it does in real code,
        // and not nearly all of it.
        let type_hint = |random: &mut Random| {
            if random.below(4) != 0 {
                return "int".to_string();
            }

            let index = random.below(count);
            match Generated::at(index) {
                Generated::Interface => format!("Interface{index}"),
                Generated::Class => format!("Class{index}"),
                Generated::Trait | Generated::Script => "int".to_string(),
            }
        };
        let signature = |random: &mut Random, name: &str| {
            format!("function {name}(?{} $x = null): ?{}", type_hint(random), type_hint(random))
        };
        let body = |random: &mut Random| {
            format!(
                "    {{\n        $value = {}::VALUE;\n        (new {}())->m0();\n\n        return null;\n    }}\n",
                class(random),
                class(random)
            )
        };

        (0..count)
            .map(|index| {
                let contents = match Generated::at(index) {
                    Generated::Interface => {
                        let parent = random
                            .earlier(index, |earlier| Generated::at(earlier) == Generated::Interface)
                            .map(|parent| format!(" extends Interface{parent}"))
                            .unwrap_or_default();
                        format!("interface Interface{index}{parent}\n{{\n    public {};\n}}\n", signature(&mut random, "m0"))
                    }
                    Generated::Trait => format!(
                        "trait Trait{index}\n{{\n    public const TRAIT_VALUE = {index};\n\n    public {}\n{}}}\n",
                        signature(&mut random, &format!("t{index}")),
                        body(&mut random)
                    ),
                    Generated::Script => format!(
                        "{}\n{}\n$script = new {}();\n$script->m0();\necho {}::VALUE;\n",
                        signature(&mut random, &format!("f{index}")),
                        body(&mut random),
                        class(&mut random),
                        class(&mut random)
                    ),
                    Generated::Class => {
                        let parent = random
                            .earlier(index, |earlier| Generated::at(earlier) == Generated::Class)
                            .map(|parent| format!(" extends Class{parent}"))
                            .unwrap_or_default();
                        let interface = random
                            .earlier(index, |earlier| Generated::at(earlier) == Generated::Interface)
                            .map(|interface| format!(" implements Interface{interface}"))
                            .unwrap_or_default();
                        let trait_use = random
                            .earlier(index, |earlier| Generated::at(earlier) == Generated::Trait)
                            .map(|trait_use| format!("    use Trait{trait_use};\n\n"))
                            .unwrap_or_default();
                        let property = format!("    public ?{} $property = null;\n", type_hint(&mut random));
                        let methods: Vec<String> = (0..=random.below(3))
                            .map(|method| {
                                format!("\n    public {}\n{}", signature(&mut random, &format!("m{method}")), body(&mut random))
                            })
                            .collect();
                        format!(
                            "class Class{index}{parent}{interface}\n{{\n{trait_use}    public const VALUE = {index};\n\n{property}{}}}\n",
                            methods.concat()
                        )
                    }
                };
                let folder = if index % 10 == 9 { "vendor/acme/generated/src" } else { "src" };

                (format!("{folder}/File{index}.php"), format!("<?php\n\nnamespace G;\n\n{contents}"))
            })
            .collect()
    }

    #[test]
    fn the_backward_walk_finds_what_the_forward_cascade_finds_in_a_generated_project() {
        let count = 300;
        for seed in [0x9E37_79B9_7F4A_7C15, 0x2545_F491_4F6C_DD1D] {
            let files = generated_project(count, seed);
            let database =
                project(&files.iter().map(|(name, contents)| (name.as_str(), contents.as_str())).collect::<Vec<_>>());

            let pairs = assert_the_walks_agree(&format!("the project generated from {seed:#x}"), &database);
            assert!(
                (count..count * count / 2).contains(&pairs),
                "{seed:#x}: {pairs} pairs, so edits reach too few files or nearly every file"
            );
        }
    }

    /// A law whose `add` calls `Rounding.sum`, which calls `Digits.keep` in a third file, beside a file no law reaches.
    fn lawful_project() -> Database<'static> {
        project(&[
            (
                "app/Shared/Money.sharp",
                "namespace App.Shared;\n\npublic class Money\n{\n    public Money(public int amount { get; }, public string currency { get; }) { }\n    public Money add(Money other) => new Money(Rounding.sum(this.amount, other.amount), this.currency);\n\n    law addKeepsCurrency(Money a, Money b) => a.add(b).currency == a.currency;\n}\n",
            ),
            (
                "app/Shared/Rounding.sharp",
                "namespace App.Shared;\n\npublic class Rounding\n{\n    public static int sum(int a, int b) => Digits.keep(a + b);\n}\n",
            ),
            (
                "app/Shared/Digits.sharp",
                "namespace App.Shared;\n\npublic class Digits\n{\n    public static int keep(int a) => a;\n}\n",
            ),
            ("app/Order.sharp", ORDER),
        ])
    }

    #[test]
    fn a_law_file_lists_its_proof_file_and_every_source_its_law_reaches_as_inputs() {
        let database = lawful_project();
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        let money = input_paths(accepted(&compiled, "app/Shared/Money.sharp"));
        for input in ["app/Shared/Money.lean", "app/Shared/Rounding.sharp", "app/Shared/Digits.sharp"] {
            assert!(money.iter().any(|path| path == input), "{input} is missing from {money:?}");
        }
        let order = input_paths(accepted(&compiled, "app/Order.sharp"));
        assert!(!order.iter().any(|path| path == "app/Order.lean"), "{order:?}");
    }

    #[test]
    fn the_proof_file_the_lean_step_creates_is_stamped_present() {
        let database = lawful_project();
        let mut service = analyzed(&database);
        let created = std::cell::Cell::new(false);

        let compiled = service
            .compile(
                |path| {
                    let present = path != b"app/Shared/Money.lean" || created.get();
                    Ok(present.then(|| Input { path: path.to_vec(), size: 1, mtime_ns: 0, hash: [1; 16] }))
                },
                |translations| {
                    assert_eq!(translations.len(), 4);
                    created.set(true);
                    Ok(HashMap::default())
                },
            )
            .expect("the compile runs");

        let (_, Compilation::Accepted(bytes)) =
            compiled.iter().find(|(file_id, _)| *file_id == FileId::new(b"app/Shared/Money.sharp")).unwrap()
        else {
            panic!("app/Shared/Money.sharp is refused: {compiled:?}");
        };
        let proof = inputs(bytes).into_iter().find(|input| input.path == b"app/Shared/Money.lean").unwrap();
        assert_eq!(proof.size, 1, "the proof file is stamped absent");
    }

    #[test]
    fn a_file_the_lean_step_refuses_gets_its_issues_and_no_compiled_file() {
        let database = lawful_project();
        let mut service = analyzed(&database);
        let money = FileId::new(b"app/Shared/Money.sharp");

        let compiled = service
            .compile(empty_stamp, |_| {
                let mut issues = IssueCollection::default();
                issues.push(Issue::error("Law addKeepsCurrency is not proven."));
                Ok(HashMap::from_iter([(money, issues)]))
            })
            .expect("the compile runs");

        for (file_id, compilation) in &compiled {
            match compilation {
                Compilation::Refused(issues) if *file_id == money => {
                    assert_eq!(messages(issues), ["Law addKeepsCurrency is not proven."]);
                }
                Compilation::Accepted(_) if *file_id != money => {}
                other => panic!("{file_id:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_failed_lean_step_reports_itself_as_the_lean_step() {
        let database = lawful_project();
        let mut service = analyzed(&database);

        let error = service
            .compile(empty_stamp, |_| Err(std::io::Error::other("lake crashed")))
            .expect_err("the Lean step failed");

        assert_eq!(error.to_string(), "The Lean step failed: lake crashed.");
    }
}
