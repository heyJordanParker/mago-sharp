//! Compiles each `.sharp` file of the last analysis into the `.sharpc` file the engine runs.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use rayon::prelude::*;
use serde::Deserialize;
use serde::Serialize;
use xxhash_rust::xxh3::xxh3_64;

use mago_allocator::LocalArena;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::artifacts::LoweringInput;
use mago_analyzer::code::IssueCode;
use mago_analyzer::effects::EffectSummary;
use mago_analyzer::effects::Effects;
use mago_analyzer::external::ExternalAnalysisSession;
use mago_analyzer::external::FileReads;
use mago_analyzer::external::Listing;
use mago_codex::graph::strongly_connected_parts;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::metadata::module::ModuleMetadata;
use mago_codex::metadata::module::STANDARD_LIBRARY_NAMESPACE;
use mago_codex::metadata::module::top_dispatcher_namespaces;
use mago_codex::reference::CascadeEdge;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_codex::ttype::TType;
use mago_database::DatabaseReader;
use mago_database::error::DatabaseError;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_names::PROGRAM_ENTRY;
use mago_names::resolver::NameResolver;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::InlineForm;
use mago_sharp_bridge::InlineForms;
use mago_sharp_bridge::Refusal;
use mago_sharp_bridge::Unit;
use mago_sharp_bridge::check;
use mago_sharp_bridge::function_type_fingerprint;
use mago_sharp_bridge::inline_forms;
use mago_sharp_bridge::lower;
use mago_sharp_bridge::type_text;
use mago_sharp_bridge::unit;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::Library;
use mago_sharp_bridge::unit::Read;
use mago_sharp_bridge::unit::Reads;
use mago_sharp_bridge::unit::SOURCE_LISTING;
use mago_sharp_bridge::unit::encode;
use mago_sharp_bridge::unit::key;
use mago_sharp_bridge::unit::source_hash;
use mago_sharp_lean::Reach;
use mago_sharp_lean::Translation;
use mago_sharp_lean::refusals;
use mago_sharp_lean::translate;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file_with_settings;
use mago_word::Word;
use mago_word::empty_word;
use mago_word::word;

use super::IncrementalAnalysisService;
use crate::error::OrchestratorError;
use crate::service::phase::Phase;

/// The folder Composer installs packages into. `composer.lock` stands for every file in it.
const VENDOR: &[u8] = b"vendor/";

/// The input that changes whenever Composer changes `vendor/`.
const COMPOSER_LOCK: &[u8] = b"composer.lock";

/// The file of a folder's module, spec section 32.
const MODULE_FILE: &str = "Module.sharp";

/// What [`IncrementalAnalysisService::compile`] made of one `.sharp` file, or of a plain PHP file whose class the
/// engine would refuse to link.
#[derive(Debug)]
pub enum Compilation<Written = Vec<u8>> {
    /// What its folder's [`Folder::write`] gave for the bytes of its `.sharpc` file.
    Accepted(Written),
    /// Its error-level issues, which stop it from running.
    Refused(IssueCollection),
    /// Its `.sharpc` file holds the bytes this compile would write, or bytes the engine treats the same: the caller
    /// vouched it is the one the last compile wrote, its key held, and each input kept the stamp it holds, or the hash
    /// of the declarations it names, so the compile neither read it nor gives its bytes.
    Kept,
}

/// A `.sharp` file lowered and its Lean translation, or the errors that refused it.
type Lowered = Result<(LoweredUnit, Translation), IssueCollection>;

/// A file that keeps its compiled file: the compiled file's bytes, none when the compile keeps it unread, and what the
/// last compile kept of it.
type KeptFile<'last> = (Option<Vec<u8>>, &'last CompiledFile);

/// A folder [`IncrementalAnalysisService::compile`] compiles into.
pub struct Folder<'folder, Written = Vec<u8>> {
    /// The [`CompiledFiles`] the last compile into the folder kept.
    pub last: &'folder CompiledFiles,
    /// The compiled file of a file in the folder, or none when no file is there.
    pub compiled: &'folder CompiledReader<'folder>,
    /// Whether a file's compiled file in the folder is the one the last compile wrote, as the caller knows from the
    /// file's status.
    pub intact: &'folder mut dyn FnMut(&File) -> bool,
    /// Takes the bytes of an accepted file's compiled file the moment the compile makes them, from any of its threads,
    /// and gives what [`Compilation::Accepted`] holds of it, a failed write included, so no file's bytes wait on
    /// another file's.
    pub write: &'folder CompiledWriter<'folder, Written>,
}

/// What gives a file's compiled file in a [`Folder`], or none when no file is there, from any thread.
pub type CompiledReader<'folder> = dyn Fn(&File) -> Result<Option<Vec<u8>>, DatabaseError> + Sync + 'folder;

/// What takes the bytes of a file's compiled file in a [`Folder`], from any thread.
pub type CompiledWriter<'folder, Written> = dyn Fn(&File, Vec<u8>) -> Written + Sync + 'folder;

/// What [`IncrementalAnalysisService::compile`] made of each file of one folder, and the [`CompiledFiles`] the next
/// compile into the folder keeps files by.
pub type Compiled<Written = Vec<u8>> = (Vec<(FileId, Compilation<Written>)>, CompiledFiles);

/// A `.sharp` file lowered, or the errors that refused it, and its Lean translation until the laws' check takes it.
type Lowering = (Result<LoweredUnit, IssueCollection>, Option<Translation>);

/// A file a folder keeps or lowered, its compilation there, and what the next compile into the folder keeps of it.
type Made<'files, Written> = (&'files Arc<File>, Compilation<Written>, Option<CompiledFile>);

/// What a compile holds of one folder it compiles into while it lowers.
struct Target<'folder, Written> {
    folder: Folder<'folder, Written>,
    /// The errors that refuse a file in the folder, by file.
    errors: HashMap<FileId, IssueCollection>,
    /// The errors of each file no error refuses in the folder, by file.
    unrefused: HashMap<FileId, IssueCollection>,
    /// Each file that keeps its compiled file in the folder.
    kept: HashMap<FileId, KeptFile<'folder>>,
    /// Each file lowered for the folder, by its place in the compile's lowerings.
    lowered: HashMap<FileId, usize>,
}

impl<'folder, Written> Target<'folder, Written> {
    fn new(
        folder: Folder<'folder, Written>,
        errors: HashMap<FileId, IssueCollection>,
        unrefused: HashMap<FileId, IssueCollection>,
    ) -> Self {
        Self { folder, errors, unrefused, kept: HashMap::default(), lowered: HashMap::default() }
    }

    /// The inline forms of the files of `library` the folder lowered, from `lowerings`, then of those it keeps.
    fn library_forms(&self, library: &[&Arc<File>], lowerings: &[Lowering]) -> Vec<(Vec<u8>, InlineForm)> {
        let lowered = library
            .iter()
            .filter_map(|file| self.lowered.get(&file.id))
            .filter_map(|lowering| lowerings[*lowering].0.as_ref().ok())
            .flat_map(|lowered| lowered.forms.iter().cloned());
        let kept = library
            .iter()
            .filter_map(|file| self.kept.get(&file.id))
            .flat_map(|(_, record)| record.forms.iter().cloned());

        lowered.chain(kept).collect()
    }
}

/// What a compile holds of one folder it compiles into once the laws are checked.
struct Inputs<'folder, Written> {
    target: Target<'folder, Written>,
    /// The issues of each file whose law is not proved, which refuse it in the folder.
    refused: HashMap<FileId, IssueCollection>,
    /// The hash of the declarations of each file the folder accepts, where it is not all zero.
    read_by_declarations: HashMap<FileId, [u8; 16]>,
    /// The inputs of each file the folder keeps or lowered.
    input_paths: HashMap<FileId, Vec<InputPath>>,
}

impl<Written> Inputs<'_, Written> {
    /// Whether the folder accepts `file`: no law refuses it, and it keeps its compiled file or lowered, from
    /// `lowerings`.
    fn accepted(&self, file: &File, lowerings: &[Lowering]) -> bool {
        !self.refused.contains_key(&file.id)
            && (self.target.kept.contains_key(&file.id)
                || self.target.lowered.get(&file.id).is_some_and(|lowering| lowerings[*lowering].0.is_ok()))
    }
}

/// The fingerprint of each of `forms`, by its name.
fn fingerprints_of(forms: &[(Vec<u8>, InlineForm)]) -> HashMap<Vec<u8>, u64> {
    forms.iter().map(|(name, form)| (name.clone(), form.fingerprint())).collect()
}

/// A lowered `.sharp` file, with what its inputs depend on.
#[derive(Debug)]
struct LoweredUnit {
    unit: Unit,
    /// The inline forms of a standard library file, or none of another file.
    forms: Vec<(Vec<u8>, InlineForm)>,
    /// Each class-like of the file's namespace that would win over a default class-like it names, in order.
    namespace_classes: Vec<Word>,
    /// Whether the file's code reaches a PHP# extension member, spec section 26.
    reaches_extension: bool,
}

/// What [`IncrementalAnalysisService::compile`] keeps of each `.sharp` file it accepted, by the file's name, so the
/// next compile keeps the file's compiled file while its key holds.
///
/// Each list it keeps is in order, so two compiles of one tree by one checker build encode the same bytes on any
/// number of threads.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CompiledFiles {
    files: BTreeMap<Vec<u8>, CompiledFile>,
}

/// What [`CompiledFiles`] keeps of one file: its compiled file's key, the names of the reads its lowering made, which
/// the next compile fingerprints again to check the key, its inline forms, what its inputs depend on, as
/// [`LoweredUnit`] holds them, and what its compiled file holds beside its code, as [`stamps_hash`] hashes it. The
/// key's other reads come from the next compile's analysis, so the record holds none of them: with the declarations an
/// extension read, which every file reads, they grew with the project's files times the extension's reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompiledFile {
    key: [u8; 16],
    /// Each library method whose body the file inlines, named `Class::method`.
    inlined: Vec<Vec<u8>>,
    /// Each method and lambda the file declares whose function type its key reads.
    function_types: Vec<Vec<u8>>,
    forms: Vec<(Vec<u8>, InlineForm)>,
    namespace_classes: Vec<Word>,
    reaches_extension: bool,
    stamps: [u8; 16],
}

impl CompiledFiles {
    /// The bytes [`Self::decode`] reads back: `checker`, the build ID of the checker that compiled the files, the hash of
    /// the `configuration` they were compiled with, then the files.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when the files do not encode.
    pub fn encode(&self, checker: [u8; 16], configuration: [u8; 16]) -> Result<Vec<u8>, OrchestratorError> {
        let mut bytes = compiled_with(checker, configuration);
        bytes.extend(bincode::serde::encode_to_vec(self, bincode::config::standard()).map_err(|error| {
            OrchestratorError::General(format!("The record of the compiled files does not encode: {error}."))
        })?);

        Ok(bytes)
    }

    /// The files `bytes` hold, or none when a checker build other than `checker` or another `configuration` compiled
    /// them or they hold no record, so every file compiles again.
    #[must_use]
    pub fn decode(bytes: &[u8], checker: [u8; 16], configuration: [u8; 16]) -> Self {
        bytes
            .strip_prefix(compiled_with(checker, configuration).as_slice())
            .and_then(|record| bincode::serde::decode_from_slice(record, bincode::config::standard()).ok())
            .map(|(files, _)| files)
            .unwrap_or_default()
    }
}

/// What the record of the compiled files starts with: the build ID `checker` and the hash of the `configuration`.
fn compiled_with(checker: [u8; 16], configuration: [u8; 16]) -> Vec<u8> {
    let mut bytes = checker.to_vec();
    bytes.extend(configuration);

    bytes
}

/// The workspace path of an input of a compiled file, with the hash of the declarations the compiled file reads there,
/// spec section 27, or all zero for an input that holds while its stamp does.
type InputPath = (Vec<u8>, [u8; 16]);

/// The hash of what a compiled file holds beside its code that decides whether the engine runs it: the
/// `declarations_hash` of its source, each of its `inputs` in their order, and the standard `library` it names. An input
/// that names the hash of an import's declarations counts by that hash alone, since the engine keeps the compiled file
/// current while the import's compiled file names it, whatever stamp the import has. A compile keeps a compiled file
/// unread while this hash holds.
#[allow(clippy::little_endian_bytes)]
fn stamps_hash(declarations_hash: [u8; 16], inputs: &[Input], library: &Library) -> [u8; 16] {
    let mut bytes = declarations_hash.to_vec();
    for Input { path, size, mtime_ns, hash, declarations_hash } in inputs {
        bytes.extend((path.len() as u64).to_le_bytes());
        bytes.extend(path);
        if *declarations_hash == [0; 16] {
            bytes.push(0);
            bytes.extend(size.to_le_bytes());
            bytes.extend(mtime_ns.to_le_bytes());
            bytes.extend(hash);
        } else {
            bytes.push(1);
            bytes.extend(declarations_hash);
        }
    }
    bytes.extend(library.native_fingerprint);
    bytes.extend(&library.version);

    source_hash(&bytes)
}

/// A PHP# listener, spec section 15: a method with an `on` clause, which the engine runs when an event it lists is
/// raised.
#[derive(Debug)]
pub struct Listener {
    /// The file that declares it.
    pub file_id: FileId,
    /// The fully qualified name of its class.
    pub class: Word,
    pub method: Word,
    /// The types its `on` clause lists, in their written order: each by its fully qualified name, and a generic event
    /// with its type arguments in the type text the engine reads, as `App.Saved<App.Order>`.
    pub events: Vec<String>,
    /// Whether it takes the event as its parameter.
    pub takes_event: bool,
}

/// An `extern` line that gives a plain PHP class, method or function attributes, spec section 29, which the engine gives
/// it when reflection reads its attributes.
#[derive(Debug, PartialEq, Eq)]
pub struct ExternAttributes {
    /// The file that declares the line.
    pub file_id: FileId,
    /// The workspace-relative name of that file.
    pub file: Word,
    /// What the line declares, in lowercase, as PHP looks it up: a class's fully qualified name, `class::method` for
    /// a method, and `function()` for a function.
    pub declaration: Word,
}

/// The effects an `extern` line declares for a plain PHP class or method, spec section 29, which
/// `Method.createDelegate` holds a plain PHP method to, spec section 25.
#[derive(Debug, PartialEq, Eq)]
pub struct ExternEffects {
    /// What the line declares, keyed as [`ExternAttributes::declaration`] is.
    pub declaration: Word,
    /// The dotted full name of each effect its `uses` lists, none for a line that declares its target pure, and `None`
    /// for a line that says `uses any`, whose target may have every effect.
    pub effects: Option<Vec<String>>,
}

/// A family, spec section 10: a sealed interface, abstract class or enum marked `[Tag]`, whose `parse` and `withTag`
/// the engine answers by finding the variant its input names.
///
/// A machine that stores as its case's value, spec section 20.2, is one too, whose `withTag` and `tag()` read its cases
/// by their values.
#[derive(Debug, PartialEq, Eq)]
pub struct Family {
    /// The file that declares it.
    pub file_id: FileId,
    /// The fully qualified name of the family.
    pub class: Word,
    /// The key its `[Tag]` names, whose value is a variant's name, and empty for a machine that stores as its value.
    pub key: Word,
    /// Each variant's name, as its `[Key]` or its case's value gives it, with the fully qualified name of its class, in
    /// the order of names.
    pub variants: Vec<(Word, Word)>,
}

/// A base or environment module the listener table records, spec section 32, which the engine reads for a class of its
/// namespace or of a namespace below it, wherever the class's file is.
#[derive(Debug, PartialEq, Eq)]
pub struct RecordedModule {
    /// The full name of the namespace it serves, in lowercase.
    pub namespace: Word,
    /// The environment `SHARP_ENV` picks it for, as in `Production`, or `None` for a base module.
    pub environment: Option<Word>,
    /// The workspace-relative name of its file.
    pub file: Word,
}

/// The name the listener table gives what an `extern` line declares, as PHP looks it up: a class's fully qualified
/// name, `class::method` for a method, and `function()` for a function, from its lowercase key in the codebase.
fn extern_key((class, member): (Word, Word)) -> Word {
    match (class, member) {
        (class, member) if member.is_empty() => class,
        (class, function) if class.is_empty() => word(format!("{function}()")),
        (class, method) => word(format!("{class}::{method}")),
    }
}

impl IncrementalAnalysisService {
    /// Every `extern` line of the last analysis that gives a class, a method or a function attributes, by what it
    /// declares, then by the file.
    #[must_use]
    pub fn extern_attributes(&self) -> Vec<ExternAttributes> {
        let mut lines: Vec<ExternAttributes> = self
            .codebase
            .externs
            .values()
            .flatten()
            .filter(|line| !line.attributes.is_empty())
            .filter_map(|line| {
                let declaration = extern_key(self.codebase.extern_declaration(line)?);

                Some(ExternAttributes { file_id: line.span.file_id, file: line.file, declaration })
            })
            .collect();
        lines.sort_by(|a, b| {
            (a.declaration.as_bytes(), a.file.as_bytes()).cmp(&(b.declaration.as_bytes(), b.file.as_bytes()))
        });

        lines
    }

    /// The effects of each plain PHP class and method an `extern` line of the last analysis declares, by what it
    /// declares: the first line's, as a call's effects take them, spec section 29, `uses any` included, so a method's
    /// own `uses any` line overrides its class's line at run time as it does in the checker. A function's are left out,
    /// since reflection gives no function a `Method`.
    #[must_use]
    pub fn extern_effects(&self) -> Vec<ExternEffects> {
        let mut declared: Vec<ExternEffects> = self
            .codebase
            .externs
            .values()
            .filter_map(|lines| {
                let line = lines.first()?;
                let target = self.codebase.extern_declaration(line).filter(|(class, _)| !class.is_empty())?;
                let declaration = extern_key(target);
                let effects = (!line.uses_any).then(|| {
                    line.effects
                        .iter()
                        .map(|effect| {
                            let name =
                                self.codebase.get_class_like(*effect).map_or(*effect, |class| class.original_name);

                            name.as_str_lossy().trim_start_matches('\\').replace('\\', ".")
                        })
                        .collect()
                });

                Some(ExternEffects { declaration, effects })
            })
            .collect();
        declared.sort_by(|a, b| a.declaration.as_bytes().cmp(b.declaration.as_bytes()));

        declared
    }

    /// Every PHP# listener of the last analysis, in the order the engine runs them: by its class's full name, then by
    /// its place in its class.
    #[must_use]
    pub fn listeners(&self) -> Vec<Listener> {
        let mut listeners: Vec<(Word, u32, Listener)> = self
            .codebase
            .function_likes
            .iter()
            .filter(|(_, method)| !method.listens.is_empty())
            .filter_map(|((class, _), method)| {
                let class = self.codebase.class_likes.get(class)?;
                let events = method
                    .listens
                    .iter()
                    .filter_map(|listed| {
                        let named = listed.get_single_named_object()?;

                        Some(match named.type_parameters {
                            Some(_) => type_text(listed, &self.codebase),
                            None => self
                                .codebase
                                .get_class_like(named.name)
                                .map_or(named.name, |event| event.original_name)
                                .to_string(),
                        })
                    })
                    .collect();

                Some((
                    class.name,
                    method.span.start.offset,
                    Listener {
                        file_id: method.span.file_id,
                        class: class.original_name,
                        method: method.original_name,
                        events,
                        takes_event: !method.parameters.is_empty(),
                    },
                ))
            })
            .collect();
        listeners.sort_unstable_by(|(a, a_offset, _), (b, b_offset, _)| {
            a.as_bytes().cmp(b.as_bytes()).then(a_offset.cmp(b_offset))
        });

        listeners.into_iter().map(|(_, _, listener)| listener).collect()
    }

    /// Every PHP# family of the last analysis, spec section 10, and every machine that stores as its case's value,
    /// spec section 20.2, by its full name, with each variant that gives its name, which the analyzer requires of
    /// every variant.
    #[must_use]
    pub fn families(&self) -> Vec<Family> {
        let mut families: Vec<Family> = self
            .codebase
            .class_likes
            .values()
            .filter(|class| class.flags.is_sharp())
            .filter_map(|family| {
                let key =
                    word(family.tag_key().or_else(|| self.codebase.stores_as_value(family.name).then_some(&b""[..]))?);
                let variants = self
                    .codebase
                    .variants(family)
                    .into_iter()
                    .filter_map(|(name, variant)| Some((word(name?), variant.original_name)))
                    .collect();

                Some(Family { file_id: family.span.file_id, class: family.original_name, key, variants })
            })
            .collect();
        families.sort_by(|a, b| a.class.as_bytes().cmp(b.class.as_bytes()));

        families
    }

    /// The namespace, in lowercase, of the app's modules whose binding of the dispatcher serves the whole app, spec
    /// section 15: the one namespace `top_dispatcher_namespaces` gives. `None` when no app module binds it, and the
    /// standard library's binding serves, and when two namespaces tie, which the analyzer refuses.
    #[must_use]
    pub fn dispatcher_namespace(&self) -> Option<Word> {
        let [namespace] = top_dispatcher_namespaces(&self.codebase.modules, &self.codebase)[..] else {
            return None;
        };

        Some(namespace)
    }

    /// Every base and environment module of the last analysis but the standard library's, which the library's
    /// autoloader reads, by namespace, then by environment, then by file. A test module is left out: its test file
    /// hands the container its bindings while a test of the file runs.
    #[must_use]
    pub fn modules(&self) -> Vec<RecordedModule> {
        let mut modules: Vec<RecordedModule> = self
            .codebase
            .modules
            .iter()
            .filter(|module| module.namespace.as_bytes() != STANDARD_LIBRARY_NAMESPACE.as_bytes())
            .filter_map(|module| {
                let environment = match module.kind {
                    ModuleKind::Base => None,
                    ModuleKind::Environment(environment) => Some(environment),
                    ModuleKind::Test => return None,
                };

                Some(RecordedModule { namespace: module.namespace, environment, file: module.file })
            })
            .collect();
        modules.sort_by(|a, b| {
            (a.namespace.as_bytes(), a.environment.as_ref().map(Word::as_bytes), a.file.as_bytes()).cmp(&(
                b.namespace.as_bytes(),
                b.environment.as_ref().map(Word::as_bytes),
                b.file.as_bytes(),
            ))
        });
        modules.dedup();

        modules
    }

    /// For each of `files`, the source files other than it whose signature edit makes the warm path re-analyze it, as
    /// the cascade over the last analysis's references finds them.
    #[must_use]
    pub fn files_reaching(&self, files: &[FileId]) -> Vec<HashSet<FileId>> {
        let dependencies = Dependencies::new(self);

        files.iter().map(|file_id| dependencies.reached_by(*file_id)).collect()
    }

    /// Compiles each `.sharp` file of the last analysis, in file name order, then refuses each plain PHP file whose
    /// class the engine would refuse to link, as [`Self::unlinkable_plain_php_classes`] finds them. Gives each
    /// compilation, and the [`CompiledFiles`] the next compile keeps files by, of `folder` and of `hosts`.
    ///
    /// A file is accepted when the analysis reported no error-level issue in it. The standard library's files, those
    /// `File::is_standard_library` marks, lower first, and the others inline their forms.
    ///
    /// A file a folder's `last`, the last compile's [`CompiledFiles`], holds whose key holds keeps its compiled file,
    /// which its `compiled` gives, or none when no file is there: it is neither analyzed again, lowered nor translated,
    /// and only its inputs' stamps and the standard library are written anew. Its key holds while its source is the one
    /// the key hashes, and the declarations, inferred returns and sets of names it read, the library forms it inlined
    /// and the effects of its function types have the fingerprints the key hashes, as [`unit::is_stale`] checks it.
    /// Every other file is analyzed, lowered and translated again.
    ///
    /// A folder's `intact` vouches that a file's compiled file is the one the last compile wrote, as its caller knows
    /// from the file's status. Such a file whose key holds is read only when a stamp its compiled file holds changed, so
    /// a compile that changes neither leaves it [`Compilation::Kept`], unread.
    ///
    /// An accepted file's inputs are its own source, `composer.lock`, and each file outside `vendor/` whose edit makes
    /// the warm path re-analyze it and can change its code: each file whose signature edit reaches it through the
    /// cascade or reaches a symbol a plugin that registers a provider read, each file that declares a class alias its
    /// code reaches, and along a chain of aliases each file that declares the next alias and the class at its end, each
    /// file that declares a patch, and each file that declares a method whose return it reads from that method's body or
    /// a body its calls reach, whose effects its functions' types list, along every chain of such returns and calls,
    /// with that file's own inputs. A set of names its analysis or such a plugin listed enters its key by the hash of
    /// the answer, and names no input. What a plugin with no provider read decides only which issues the analysis
    /// reports, never the code, so it enters the key and no input.
    ///
    /// Spec section 27: such an input that is a `.sharp` file of the workspace, other than the file itself and a source
    /// its laws reach, names the hash of its declarations beside its stamp, so the engine and the next compile count it
    /// by that hash: a body edit to it changes no other compiled file, since the key holds what the file read of its
    /// bodies. A file that states an `extern` line or whose signature holds no declaration keeps its stamp alone.
    ///
    /// A file that states a law also lists every source its laws reach. A file outside `vendor/` also lists the module
    /// files its check reads, by the namespaces they serve, whether or not a file is at each, as
    /// [`Self::module_inputs`] gives them: the modules whose `imports` line limits what it imports, and for a file that
    /// declares a module, a listener or a program, the modules its check resolves services through.
    ///
    /// The laws of a file are checked again when the file or a file its laws reach is lowered again, and the files they
    /// reach that keep their compiled file are translated without being lowered. `check` gets those translations,
    /// `reach`, and each file the analysis reported an error in, whether or not its errors refuse it, as
    /// [`Self::law_refusals`] reads them, and returns the issues of each file whose law is not proved, which refuse that
    /// file whatever ignore entry or baseline the analysis honors.
    ///
    /// `stamp` gives the size, modification time and hash of the file at a workspace-relative path, or none when no
    /// file is there. It is called once per input, from several threads at once. An input no file is at is stored
    /// absent: its size, modification time and hash are all zero. No file has that stamp, because the hash of an empty
    /// file is not zero.
    ///
    /// Each compiled file's header names `checker`, the build ID of the checker that compiles, and `standard_library`,
    /// the standard library the project compiles against. A compiled file another checker build wrote compiles again.
    ///
    /// `refusing` keeps, of the error-level issues the analysis reported, those that refuse their file: the ones no
    /// `ignore` entry or baseline entry hides for `mago compile`, or none for the compile the extension hosts load the
    /// project's classes from. A file whose errors refuse nothing lowers as far as its code allows, as `tsc` emits
    /// despite type errors, and a file whose lowering fails on them is refused with them.
    ///
    /// `hosts`, when given, is the folder the extension hosts load the project's classes from, which the same compile
    /// fills: no error and no law refuses a file there, as for the compile whose `refusing` keeps none. A file both
    /// folders lower the same, one neither refuses that inlines the same forms in both, is lowered once.
    ///
    /// Each folder's `write` takes the bytes of each compiled file the folder accepts as soon as they are made, once the
    /// laws are checked, so a compile that fails after that may have written some of them.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when no analysis ran yet, when re-analyzing a file fails, or when `compiled`,
    /// `stamp` or `check` fails.
    #[allow(clippy::too_many_arguments)]
    pub fn compile<'folder, Written>(
        &mut self,
        folder: Folder<'folder, Written>,
        hosts: Option<Folder<'folder, Written>>,
        stamp: impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync,
        check: impl FnOnce(Vec<Translation>, &Reach, &HashSet<FileId>) -> std::io::Result<HashMap<FileId, IssueCollection>>,
        checker: [u8; 16],
        standard_library: &Library,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> Result<(Compiled<Written>, Option<Compiled<Written>>), OrchestratorError>
    where
        Written: Send,
    {
        if !self.initialized {
            return Err(OrchestratorError::General("analyze() must be called before compile()".to_string()));
        }

        let lowering = Phase::start("lowering");
        let files = self.sharp_files();
        let (errors, unrefused) = self.errors(refusing);
        let errored: HashSet<FileId> = errors.keys().chain(unrefused.keys()).copied().collect();
        // No error refuses a file in the hosts' folder: each is one that refuses nothing.
        let mut hosts = hosts.map(|hosts| {
            let mut every = unrefused.clone();
            every.extend(errors.iter().map(|(file_id, issues)| (*file_id, issues.clone())));

            Target::new(hosts, HashMap::default(), every)
        });
        let mut first = Target::new(folder, errors, unrefused);
        let session = self.plugin_registry.create_external_analysis_session(self.database.files());
        let (library, rest): (Vec<_>, Vec<_>) = files.iter().partition(|file| file.is_standard_library);
        let reach = Reach::new(&self.codebase, &self.native_symbol_references);
        let effects = self.effects();
        let reads = self.reads(&effects);
        let mut lowerings: Vec<Lowering> = Vec::new();

        // A library file inlines no form, so whether it keeps its compiled file needs none, and the files that keep
        // theirs give the forms the others' lowering and keys read.
        let no_forms = InlineForms::default();
        for target in std::iter::once(&mut first).chain(hosts.as_mut()) {
            self.keep(target, checker, &library, &reads, &HashMap::default(), &effects)?;
        }
        self.lower_into(&mut first, &library, None, &no_forms, &reach, &effects, session.as_ref(), &mut lowerings)?;
        if let Some(hosts) = hosts.as_mut() {
            let lowered = Some(&first);
            self.lower_into(hosts, &library, lowered, &no_forms, &reach, &effects, session.as_ref(), &mut lowerings)?;
        }
        let forms = first.library_forms(&library, &lowerings);
        let fingerprints = fingerprints_of(&forms);
        let hosts_forms = hosts.as_ref().map(|hosts| hosts.library_forms(&library, &lowerings));
        let hosts_fingerprints = hosts_forms.as_deref().map(fingerprints_of);
        let forms: InlineForms = forms.into_iter().collect();
        self.keep(&mut first, checker, &rest, &reads, &fingerprints, &effects)?;
        self.lower_into(&mut first, &rest, None, &forms, &reach, &effects, session.as_ref(), &mut lowerings)?;
        if let (Some(hosts), Some(hosts_forms), Some(hosts_fingerprints)) =
            (hosts.as_mut(), hosts_forms, hosts_fingerprints)
        {
            self.keep(hosts, checker, &rest, &reads, &hosts_fingerprints, &effects)?;
            // A file lowers the same for both folders when it inlines the same forms in both.
            let lowered = (hosts_fingerprints == fingerprints).then_some(&first);
            let hosts_forms: InlineForms = hosts_forms.into_iter().collect();
            self.lower_into(hosts, &rest, lowered, &hosts_forms, &reach, &effects, session.as_ref(), &mut lowerings)?;
        }
        lowering.end();

        let laws = Phase::start("laws");
        // The laws of a file are checked again when it or a file they reach was lowered again, with each file they reach
        // that kept its compiled file translated beside them.
        let lowered_again: HashSet<FileId> = first.lowered.keys().copied().collect();
        let rechecked: HashSet<FileId> = reach
            .law_files()
            .map(|law_file| reach.files(law_file))
            .filter(|reached| reached.iter().any(|file_id| lowered_again.contains(file_id)))
            .flatten()
            .copied()
            .collect();
        let translated_only: Vec<&Arc<File>> =
            files.iter().filter(|file| rechecked.contains(&file.id) && first.kept.contains_key(&file.id)).collect();
        let refusals = (&first.errors, &first.unrefused);
        let translation = Phase::start("translation");
        let translated =
            self.checked_all(&translated_only, &forms, refusals, &effects, session.as_ref(), |checked, _, _| {
                translate(checked, &reach)
            })?;
        translation.end();
        tracing::debug!(
            "Kept the compiled file of {} PHP# files, lowered {} again, and translated {} more for the laws that reach them.",
            first.kept.len(),
            lowered_again.len(),
            translated_only.len(),
        );
        // Past the laws' translation only `law_refusals` reads what the analysis gave the lowering, of each file a law
        // reaches, until the next analysis replaces it all, so every other file's goes before the compile stamps and
        // writes.
        let (_, reached_by_laws) = self.law_reach(&reach);
        for (file_id, state) in &mut self.file_states {
            if !reached_by_laws.contains(file_id) {
                state.lowering_input = None;
            }
        }
        // A file the last analysis kept no lowering input for is analyzed again against the codebase that analysis saw,
        // so it sends the provider requests that analysis sent, and every read their answers rest on is one the
        // analysis already holds.
        debug_assert!(
            session.as_ref().is_none_or(|session| {
                let lowering = session.take_reads();
                let held = super::code_reads(&self.file_states, &self.before_analysis_reads);
                let held_issues = super::extension_reads(&self.file_states, &self.before_analysis_reads);
                lowering
                    .files
                    .values()
                    .chain([&lowering.before_analysis])
                    .all(|reads| held.includes(&reads.code) && held_issues.includes(&reads.issues))
            }),
            "lowering read the codebase where the analysis it follows did not"
        );
        let mut translations: Vec<Translation> = library
            .iter()
            .chain(&rest)
            .filter_map(|file| first.lowered.get(&file.id))
            .filter_map(|lowering| lowerings[*lowering].1.take())
            .collect();
        translations.extend(translated.into_iter().filter_map(Result::ok));
        let refused = check(translations, &reach, &errored)
            .map_err(|error| OrchestratorError::General(format!("The laws could not be checked: {error}.")))?;
        laws.end();

        let inputs = Phase::start("inputs");
        let read_by_every_file: HashSet<FileId> =
            self.database.files().filter(|file| file.file_type.is_patch()).map(|file| file.id).collect();
        let mut dependencies = Dependencies::new(self);
        // A method whose return comes from its body can return another such method's return, and a body has the
        // effects of each body it calls, so a file's inputs close over the files whose bodies it reads.
        dependencies.follow_bodies(&files.iter().map(|file| file.id).collect::<Vec<FileId>>(), &reads);
        let modules = self.module_files();
        let mut folder_files: HashMap<&[u8], Vec<&[u8]>> = HashMap::default();
        for file in &files {
            folder_files.entry(folder_of(&file.name)).or_default().push(&file.name);
        }
        let first = self.inputs(
            first,
            refused,
            &files,
            &lowerings,
            &reach,
            &dependencies,
            &modules,
            &folder_files,
            &read_by_every_file,
        );
        let hosts = hosts.map(|hosts| {
            self.inputs(
                hosts,
                HashMap::default(),
                &files,
                &lowerings,
                &reach,
                &dependencies,
                &modules,
                &folder_files,
                &read_by_every_file,
            )
        });
        let mut counts: Vec<usize> = first.input_paths.values().map(Vec::len).collect();
        counts.sort_unstable();
        tracing::debug!(
            "Found the inputs of {} PHP# files: a median of {} inputs, and {} at most.",
            counts.len(),
            counts.get(counts.len() / 2).copied().unwrap_or_default(),
            counts.last().copied().unwrap_or_default(),
        );

        // Each input of an accepted file is read once, all at once, and the first in the order the files name them that
        // can't be read fails the compile, as reading them one after another did.
        let named: Vec<Vec<u8>> = {
            let mut seen: HashSet<&[u8]> = HashSet::default();
            std::iter::once(&first)
                .chain(hosts.as_ref())
                .flat_map(|inputs| {
                    files
                        .iter()
                        .filter(|file| inputs.accepted(file, &lowerings))
                        .flat_map(|file| inputs.input_paths.get(&file.id).into_iter().flatten())
                })
                .map(|(path, _)| path)
                .filter(|path| seen.insert(path.as_slice()))
                .cloned()
                .collect()
        };
        let stamped: Vec<Result<Option<Input>, DatabaseError>> = named.par_iter().map(|path| stamp(path)).collect();
        let mut stamps: HashMap<Vec<u8>, Input> = HashMap::default();
        for (path, input) in named.into_iter().zip(stamped) {
            let input = input?.unwrap_or_else(|| Input {
                path: path.clone(),
                size: 0,
                mtime_ns: 0,
                hash: [0; 16],
                declarations_hash: [0; 16],
            });
            stamps.insert(path, input);
        }
        inputs.end();
        let write = Phase::start("write");
        let compiled = self.compiled(first, &files, &lowerings, &reads, &stamps, checker, standard_library)?;
        let hosts = match hosts {
            Some(hosts) => {
                Some(self.compiled(hosts, &files, &lowerings, &reads, &stamps, checker, standard_library)?)
            }
            None => None,
        };
        write.end();

        Ok((compiled, hosts))
    }

    /// Keeps in `target` each of `files` whose key holds in its folder for the checker build `checker`, as
    /// [`Self::kept`] finds it with `reads`, `forms` and `effects`.
    fn keep<Written>(
        &self,
        target: &mut Target<'_, Written>,
        checker: [u8; 16],
        files: &[&Arc<File>],
        reads: &HashMap<FileId, (Reads, HashSet<FileId>)>,
        forms: &HashMap<Vec<u8>, u64>,
        effects: &Effects,
    ) -> Result<(), OrchestratorError> {
        for file in files {
            let Target { folder, errors, kept, .. } = &mut *target;
            if let Some(found) = self.kept(
                file,
                checker,
                folder.last,
                folder.compiled,
                &mut folder.intact,
                reads,
                forms,
                effects,
                errors,
            )? {
                kept.insert(file.id, found);
            }
        }

        Ok(())
    }

    /// Lowers for `target` each of `files` its folder keeps no compiled file of, with `forms` to inline, as
    /// [`Self::lower_all`] lowers it, into `lowerings`. A file `lowered` lowered and neither target refuses takes that
    /// lowering instead, since a file's lowering rests only on its errors and the forms it inlines, which `lowered`
    /// passes only when they are `target`'s.
    #[allow(clippy::too_many_arguments)]
    fn lower_into<Written>(
        &self,
        target: &mut Target<'_, Written>,
        files: &[&Arc<File>],
        lowered: Option<&Target<'_, Written>>,
        forms: &InlineForms,
        reach: &Reach,
        effects: &Effects,
        session: Option<&ExternalAnalysisSession>,
        lowerings: &mut Vec<Lowering>,
    ) -> Result<(), OrchestratorError> {
        let mut stale = Vec::new();
        for file in files.iter().filter(|file| !target.kept.contains_key(&file.id)) {
            let taken = lowered
                .filter(|lowered| !lowered.errors.contains_key(&file.id) && !target.errors.contains_key(&file.id))
                .and_then(|lowered| lowered.lowered.get(&file.id));
            match taken {
                Some(lowering) => {
                    target.lowered.insert(file.id, *lowering);
                }
                None => stale.push(*file),
            }
        }
        let lowered = self.lower_all(&stale, forms, (&target.errors, &target.unrefused), reach, effects, session)?;
        for (file, lowered) in stale.iter().zip(lowered) {
            target.lowered.insert(file.id, lowerings.len());
            lowerings.push(match lowered {
                Ok((unit, translation)) => (Ok(unit), Some(translation)),
                Err(issues) => (Err(issues), None),
            });
        }

        Ok(())
    }

    /// What `target`'s folder reads once `refused`, the issues of each file whose law is not proved, refuse their
    /// files in it: the declarations hash of each of `files` it accepts, and the inputs of each it keeps or lowered from
    /// `lowerings`. A file's inputs are each source `reach` and `dependencies` give it, the module files its check reads
    /// as `modules` places them, the files of its folder in `folder_files` when its code reaches an extension, and the
    /// files in `read_by_every_file`.
    #[allow(clippy::too_many_arguments)]
    fn inputs<'folder, Written>(
        &self,
        target: Target<'folder, Written>,
        refused: HashMap<FileId, IssueCollection>,
        files: &[Arc<File>],
        lowerings: &[Lowering],
        reach: &Reach,
        dependencies: &Dependencies<'_>,
        modules: &ModuleFiles,
        folder_files: &HashMap<&[u8], Vec<&[u8]>>,
        read_by_every_file: &HashSet<FileId>,
    ) -> Inputs<'folder, Written> {
        let mut inputs =
            Inputs { target, refused, read_by_declarations: HashMap::default(), input_paths: HashMap::default() };
        // Spec section 27: each file this compile writes a compiled file for whose declarations hash is not all zero is
        // read through its declarations, which its compiled file's header names.
        let read_by_declarations: HashMap<FileId, [u8; 16]> = files
            .iter()
            .filter(|file| inputs.accepted(file, lowerings))
            .map(|file| (file.id, declarations_hash(&self.codebase, file)))
            .filter(|(_, declarations_hash)| *declarations_hash != [0; 16])
            .collect();
        let mut input_paths: HashMap<FileId, Vec<InputPath>> = HashMap::default();
        for (file, namespace_classes, reaches_extension) in files.iter().filter_map(|file| {
            if let Some((_, record)) = inputs.target.kept.get(&file.id) {
                return Some((file, &record.namespace_classes, record.reaches_extension));
            }
            let lowered = lowerings[*inputs.target.lowered.get(&file.id)?].0.as_ref().ok()?;

            Some((file, &lowered.namespace_classes, lowered.reaches_extension))
        }) {
            let mut sources: HashSet<FileId> = read_by_every_file.clone();
            sources.extend(reach.files(file.id));
            sources.insert(file.id);
            // A file this one reaches through declarations or bodies names the hash of its declarations beside its
            // stamp, so an edit to its bodies leaves this file current: the key holds the returns and effects it read
            // from them. So does a module file, whose declarations are its `imports` line and its bindings, all a file
            // reads of it. Every other source keeps its stamp alone.
            let mut imports: Vec<InputPath> = Vec::new();
            for reached in dependencies.reached_through_bodies(file.id) {
                if sources.contains(&reached) {
                    continue;
                }
                match (read_by_declarations.get(&reached), self.database.get(&reached)) {
                    (Some(declarations_hash), Ok(import)) => imports.push((import.name.to_vec(), *declarations_hash)),
                    _ => {
                        sources.insert(reached);
                    }
                }
            }

            // Spec section 27 puts a class-like of the file's namespace in the file's folder, under its own name, so the
            // files that would declare one are inputs, stamped absent while the namespace lacks it.
            let folder = folder_of(&file.name);
            let namespace_class_files = namespace_classes.iter().filter(|_| is_source(file)).flat_map(|class| {
                let class = short_name(class);

                [".sharp", ".php"].map(|extension| [folder, class.as_bytes(), extension.as_bytes()].concat())
            });
            // Spec section 26 applies the extensions of the file's namespace with no import, so a file whose code reaches
            // an extension compiles to other code when a file of its folder gains, changes or loses one. Its inputs are
            // the folder's listing and each `.sharp` file in it. A call no extension reaches can't change this way: a
            // member the type declares wins, and a call with no member is a compile error.
            let namespace_files: Vec<Vec<u8>> = if reaches_extension && is_source(file) {
                folder_files
                    .get(folder)
                    .into_iter()
                    .flatten()
                    .map(|name| name.to_vec())
                    .chain(std::iter::once([folder, SOURCE_LISTING.as_bytes()].concat()))
                    .collect()
            } else {
                Vec::new()
            };
            // A module no file is at, or one this compile refuses, has no declarations hash and keeps its stamp alone.
            let module_files = self.module_inputs(file, dependencies, modules).into_iter().map(|path| {
                let declarations_hash = read_by_declarations.get(&FileId::new(&path)).copied().unwrap_or_default();

                (path, declarations_hash)
            });
            let mut paths: Vec<InputPath> = sources
                .into_iter()
                .filter_map(|source| self.database.get(&source).ok())
                .filter(|source| source.id == file.id || is_source(source))
                .map(|source| source.name.to_vec())
                .chain(namespace_class_files)
                .chain(namespace_files)
                .chain(std::iter::once(COMPOSER_LOCK.to_vec()))
                .map(|path| (path, [0; 16]))
                .chain(module_files)
                .chain(imports)
                .collect();
            // A path another rule also names keeps its stamp alone, which sorts first.
            paths.sort_unstable();
            paths.dedup_by(|later, first| later.0 == first.0);
            input_paths.insert(file.id, paths);
        }
        inputs.read_by_declarations = read_by_declarations;
        inputs.input_paths = input_paths;

        inputs
    }

    /// The compilation of each of `files` in `inputs`' folder, in file name order, then of each plain PHP file the
    /// folder refuses, as [`Self::unlinkable_plain_php_classes`] finds them, and the [`CompiledFiles`] the next compile
    /// into the folder keeps files by. A file the folder keeps or lowered from `lowerings` compiles with what `reads`
    /// holds it read, the stamp `stamps` holds of each of its inputs, and `standard_library`. The files compile at once,
    /// and the folder's `write` takes each accepted file's bytes as soon as they are made.
    fn compiled<Written>(
        &self,
        inputs: Inputs<'_, Written>,
        files: &[Arc<File>],
        lowerings: &[Lowering],
        reads: &HashMap<FileId, (Reads, HashSet<FileId>)>,
        stamps: &HashMap<Vec<u8>, Input>,
        checker: [u8; 16],
        standard_library: &Library,
    ) -> Result<Compiled<Written>, OrchestratorError>
    where
        Written: Send,
    {
        let Inputs { mut target, mut refused, read_by_declarations, mut input_paths } = inputs;
        let stamped = |paths: Vec<InputPath>| -> Vec<Input> {
            paths
                .into_iter()
                .map(|(path, declarations_hash)| Input { declarations_hash, ..stamps[&path].clone() })
                .collect()
        };
        // Each file the folder keeps or lowered, in file name order, with the issues that refuse it, its inputs and what
        // it keeps.
        let made: Vec<_> = files
            .iter()
            .filter_map(|file| {
                let kept = target.kept.remove(&file.id);
                (kept.is_some() || target.lowered.contains_key(&file.id))
                    .then(|| (file, refused.remove(&file.id), input_paths.remove(&file.id).unwrap_or_default(), kept))
            })
            .collect();
        let Target { folder: Folder { compiled, write, .. }, lowered, .. } = &target;
        let made: Vec<Result<Made<'_, Written>, OrchestratorError>> = made
            .into_par_iter()
            .map(|(file, issues, paths, kept)| {
                let declarations_hash = read_by_declarations.get(&file.id).copied().unwrap_or_default();
                if let Some((bytes, record)) = kept {
                    if let Some(issues) = issues {
                        return Ok((file, Compilation::Refused(issues), None));
                    }
                    let inputs = stamped(paths);
                    let stamps = stamps_hash(declarations_hash, &inputs, standard_library);
                    let next = CompiledFile { stamps, ..record.clone() };
                    let bytes = match bytes {
                        None if stamps == record.stamps => return Ok((file, Compilation::Kept, Some(next))),
                        Some(bytes) => bytes,
                        None => compiled(file)?
                            .filter(|bytes| !unit::is_stale(bytes, checker, record.key))
                            .ok_or_else(|| {
                                OrchestratorError::General(format!(
                                    "The compiled file of {} changed while the compile ran. Compile again.",
                                    String::from_utf8_lossy(&file.name)
                                ))
                            })?,
                    };
                    let Some(bytes) = unit::restamp(&bytes, declarations_hash, &inputs, checker, standard_library)
                    else {
                        unreachable!("a compiled file the compile keeps is one `unit::is_stale` read");
                    };

                    return Ok((file, Compilation::Accepted(write(file, bytes)), Some(next)));
                }
                match (&lowerings[lowered[&file.id]].0, issues) {
                    (Ok(_), Some(issues)) => Ok((file, Compilation::Refused(issues), None)),
                    (Err(issues), _) => Ok((file, Compilation::Refused(issues.clone()), None)),
                    (Ok(LoweredUnit { unit, forms, namespace_classes, reaches_extension }), None) => {
                        let mut reads = reads.get(&file.id).map(|(reads, _)| reads.clone()).unwrap_or_default();
                        reads.inlined = unit.inlined().to_vec();
                        reads.function_types = unit.function_types().to_vec();
                        let key = key(checker, source_hash(&file.contents), &reads);
                        let inputs = stamped(paths);
                        let stamps = stamps_hash(declarations_hash, &inputs, standard_library);
                        let next = CompiledFile {
                            key,
                            inlined: reads.inlined.into_iter().map(|read| read.name).collect(),
                            function_types: reads.function_types.into_iter().map(|read| read.name).collect(),
                            forms: forms.clone(),
                            namespace_classes: namespace_classes.clone(),
                            reaches_extension: *reaches_extension,
                            stamps,
                        };
                        let bytes =
                            encode(unit, &file.contents, key, declarations_hash, &inputs, checker, standard_library);

                        Ok((file, Compilation::Accepted(write(file, bytes)), Some(next)))
                    }
                }
            })
            .collect();
        let mut next = CompiledFiles::default();
        let mut compilations = Vec::with_capacity(made.len());
        for made in made {
            let (file, compilation, record) = made?;
            if let Some(record) = record {
                next.files.insert(file.name.to_vec(), record);
            }
            compilations.push((file.id, compilation));
        }
        compilations.extend(self.unlinkable_plain_php_classes(&target.errors));

        Ok((compilations, next))
    }

    /// The compiled file of `file` and what `last` kept of it, when its key for the checker build `checker` holds:
    /// `last` keeps it, the analysis
    /// refused nothing in it, and the key of its source and the current fingerprints of the reads `last` kept, as
    /// `reads` holds the declarations, inferred returns and sets of names it reads, `forms` the fingerprint of each
    /// library form by its name and `effects` its function types' effects, is the one `last` kept and its compiled
    /// file, as `compiled` gives it, names. The compiled file is none, unread, when `intact` vouches for it.
    #[allow(clippy::too_many_arguments)]
    fn kept<'last>(
        &self,
        file: &File,
        checker: [u8; 16],
        last: &'last CompiledFiles,
        compiled: &CompiledReader<'_>,
        intact: &mut impl FnMut(&File) -> bool,
        reads: &HashMap<FileId, (Reads, HashSet<FileId>)>,
        forms: &HashMap<Vec<u8>, u64>,
        effects: &Effects,
        errors: &HashMap<FileId, IssueCollection>,
    ) -> Result<Option<KeptFile<'last>>, OrchestratorError> {
        let Some(record) = last.files.get(file.name.as_ref()).filter(|_| !errors.contains_key(&file.id)) else {
            return Ok(None);
        };
        let read = reads.get(&file.id).map(|(reads, _)| reads);
        let current = Reads {
            signatures: read.map(|reads| reads.signatures.clone()).unwrap_or_default(),
            bodies: read.map(|reads| reads.bodies.clone()).unwrap_or_default(),
            listed: read.map(|reads| reads.listed.clone()).unwrap_or_default(),
            inlined: record
                .inlined
                .iter()
                .map(|name| Read { name: name.clone(), fingerprint: forms.get(name).copied().unwrap_or(0) })
                .collect(),
            function_types: record
                .function_types
                .iter()
                .map(|name| Read {
                    name: name.clone(),
                    fingerprint: function_type_fingerprint(&self.codebase, effects, name),
                })
                .collect(),
        };
        let key = key(checker, source_hash(&file.contents), &current);
        if key != record.key {
            return Ok(None);
        }
        if intact(file) {
            return Ok(Some((None, record)));
        }

        Ok(compiled(file)?.filter(|bytes| !unit::is_stale(bytes, checker, key)).map(|bytes| (Some(bytes), record)))
    }

    /// Refuses each plain PHP file that declares a class extending or implementing a PHP# type that it doesn't satisfy,
    /// spec section 22: each error the analysis reports at the class's declaration that names a PHP# declaration, as a
    /// missing `Defaults` trait or a member whose signature doesn't fit the PHP# member it implements, a case the engine
    /// refuses when it links the class. The analysis names the class in each error of its declaration's checks. A plain
    /// PHP class that takes a PHP# class's name meets that type too, and its duplicate-definition error is refused, since
    /// Composer's loader would load it in the PHP# class's place. Every other error in a plain PHP file stays the
    /// analysis's, as kotlinc checks a Java type only where it meets a Kotlin declaration and leaves every other Java
    /// error to javac: the type arguments a docblock's `@implements` or `@extends` gives a PHP# generic type among them,
    /// since the engine erases type parameters and reads no docblock.
    fn unlinkable_plain_php_classes<Written>(
        &self,
        errors: &HashMap<FileId, IssueCollection>,
    ) -> Vec<(FileId, Compilation<Written>)> {
        let in_sharp_file = |annotation: &Annotation| {
            self.database.get(&annotation.span.file_id).is_ok_and(|file| Dialect::of(&file).is_sharp())
        };
        let in_plain_php_file = |file_id: &FileId| {
            self.database
                .get(file_id)
                .is_ok_and(|file| file.file_type == FileType::Host && !Dialect::of(&file).is_sharp())
        };
        let duplicates_here = |issue: &Issue, annotation: &Annotation| {
            annotation.is_primary()
                && issue.code.as_deref() == Some(IssueCode::DuplicateDefinition.as_str())
                && in_plain_php_file(&annotation.span.file_id)
        };
        let reads_docblock = |issue: &Issue| {
            issue.code.as_deref().is_some_and(|code| {
                [
                    IssueCode::MissingTemplateParameter,
                    IssueCode::ExcessTemplateParameter,
                    IssueCode::InconsistentTemplate,
                    IssueCode::InvalidTemplateParameter,
                ]
                .iter()
                .any(|docblock| docblock.as_str() == code)
            })
        };
        let names_sharp_type =
            |name: &Word| self.codebase.get_class_like(*name).is_some_and(|class| class.flags.is_sharp());
        let declarations: HashSet<_> = self
            .codebase
            .class_likes
            .values()
            .filter(|class| class.all_parent_classes.iter().chain(&class.all_parent_interfaces).any(names_sharp_type))
            .filter(|class| in_plain_php_file(&class.span.file_id))
            .map(|class| class.name_span.unwrap_or(class.span))
            .collect();

        let mut refused: HashMap<FileId, IssueCollection> = HashMap::default();
        for issue in errors.values().flat_map(IssueCollection::iter).filter(|issue| !reads_docblock(issue)) {
            if let Some(declaration) = issue
                .annotations
                .iter()
                .find(|annotation| declarations.contains(&annotation.span) || duplicates_here(issue, annotation))
                && issue.annotations.iter().any(in_sharp_file)
            {
                refused.entry(declaration.span.file_id).or_default().push(issue.clone());
            }
        }
        let mut refused: Vec<(FileId, IssueCollection)> = refused.into_iter().collect();
        refused.sort_unstable_by_key(|(file_id, _)| self.database.get(file_id).map(|file| file.name.clone()).ok());

        refused.into_iter().map(|(file_id, issues)| (file_id, Compilation::Refused(issues))).collect()
    }

    /// What each file's analysis read from other files, each list in order: each declaration it read through its
    /// signature, each method whose return is taken from its body, and each declaration and set of names its extensions
    /// read. With them, the files whose bodies it read: those that declare those methods, and those whose bodies its
    /// calls reach, which `effects` solved its functions' effects from.
    fn reads(&self, effects: &Effects) -> HashMap<FileId, (Reads, HashSet<FileId>)> {
        // A declaration several files hold, as two polyfill files each declare one function, is read as the one the
        // codebase keeps, else as the one in the first file by id, so every compile fingerprints the same one.
        let kept = |name: Word, is_function: bool| {
            let span = if is_function {
                self.codebase.get_function(name).map(|function| function.span)
            } else {
                self.codebase
                    .get_class_like(name)
                    .map(|class| class.span)
                    .or_else(|| self.codebase.get_constant(name).map(|constant| constant.span))
            };

            span.map(|span| span.file_id)
        };
        let mut signatures: Vec<_> = self.codebase.file_signatures.iter().collect();
        signatures.sort_unstable_by_key(|(file_id, _)| **file_id);
        let mut declarations: HashMap<SymbolIdentifier, (FileId, u64)> = HashMap::default();
        for (file_id, signature) in signatures {
            for node in &signature.ast_nodes {
                let replaces = kept(node.name, node.is_function) == Some(*file_id);
                let mut declare = |symbol: SymbolIdentifier, hash: u64| {
                    if replaces || !declarations.contains_key(&symbol) {
                        declarations.insert(symbol, (*file_id, hash));
                    }
                };
                declare((node.name, empty_word()), node.signature_hash);
                for child in &node.children {
                    declare((node.name, super::member_key(&self.codebase, node.name, child)), child.signature_hash);
                }
            }
        }

        let declared = |target: SymbolIdentifier| -> Option<(SymbolIdentifier, FileId, u64)> {
            let find = |class: mago_word::Word| {
                [(class, target.1), (self.codebase.class_key(class), target.1)]
                    .into_iter()
                    .find_map(|member| declarations.get(&member).map(|(file_id, hash)| (member, *file_id, *hash)))
            };
            if target.1.is_empty() {
                return find(target.0);
            }

            let mut class = Some(target.0);
            while let Some(name) = class {
                let metadata = self.codebase.get_class_like(name)?;
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
            let Some(origin) = origin else {
                return;
            };
            let Some((target, target_file, hash)) = declared(target) else {
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

        let session = ExternalAnalysisSession::from_files(self.database.files());
        let answered = |listing: &Listing| Read {
            name: listing_name(listing),
            fingerprint: listing.answer(&self.codebase, &session),
        };
        // A file's own analysis reads a set whole for itself alone.
        for (file_id, state) in self.file_states.iter().filter(|(_, state)| !state.reads.listings.is_empty()) {
            reads.entry(*file_id).or_default().0.listed.extend(state.reads.listings.iter().map(answered));
        }
        // Each PHP# file read what every file's hooks, issue filters and providers read, as `extension_reads` says, but
        // a declaration of its own.
        let extension_reads = super::extension_reads(&self.file_states, &self.before_analysis_reads);
        let symbols: Vec<(SymbolIdentifier, FileId, u64)> = extension_reads
            .symbols
            .iter()
            .flat_map(|&(name, _)| self.with_ancestors(name))
            .filter_map(|symbol| declared((symbol, empty_word())))
            .collect();
        let listed: Vec<Read> = extension_reads.listings.iter().map(answered).collect();
        if !symbols.is_empty() || !listed.is_empty() {
            for file in self.database.files().filter(|file| Dialect::of(file).is_sharp()) {
                let (file_reads, _) = reads.entry(file.id).or_default();
                file_reads.signatures.extend(
                    symbols
                        .iter()
                        .filter(|(_, target_file, _)| *target_file != file.id)
                        .map(|(target, _, hash)| Read { name: target.0.as_bytes().to_vec(), fingerprint: *hash }),
                );
                file_reads.listed.extend(listed.iter().cloned());
            }
        }
        for (file_id, callee_files) in effects.callee_files(&self.codebase) {
            reads.entry(file_id).or_default().1.extend(callee_files);
        }
        for (file_reads, _) in reads.values_mut() {
            file_reads.signatures.sort();
            file_reads.bodies.sort();
            file_reads.listed.sort();
        }

        reads
    }

    /// Spec section 32: the module files the check of `file` reads, whether a file is at each or not, as `modules`
    /// places them. The `imports` line of the module of the file's namespace or of a namespace above it limits what the
    /// file imports. A module's bindings, a listener's constructor and a program's constructor also take each service
    /// from the module of the taking class's namespace or of a namespace above it, so a file that declares a module, a
    /// listener or a program also reads the modules of each namespace a module of it serves, and of each class its
    /// check read. An environment module also reads its environment's modules. An addition or a deletion of one makes
    /// the compiled file stale, and so does an edit that changes its `imports` line or a binding: the compiled file
    /// names the hash of each one's declarations, so an edit that leaves them as they were leaves it current.
    fn module_inputs(&self, file: &File, dependencies: &Dependencies<'_>, modules: &ModuleFiles) -> Vec<Vec<u8>> {
        if !is_source(file) {
            return Vec::new();
        }

        let mut namespaces: Vec<Vec<u8>> =
            modules.namespaces.get(&file.id).map(|namespace| namespace.to_ascii_lowercase()).into_iter().collect();
        let declared: Vec<&ModuleMetadata> =
            self.codebase.modules.iter().filter(|module| module.file.as_bytes() == file.name.as_ref()).collect();
        let mut environments = Vec::new();
        if !declared.is_empty() || modules.built.contains(&file.id) {
            let read = dependencies.files.get(&word(file.name.as_ref())).into_iter().flatten();
            let read = read.filter_map(|(class, _)| self.codebase.get_class_like(*class));
            namespaces.extend(read.map(|class| namespace_of(class.name.as_bytes()).to_ascii_lowercase()));
            namespaces.extend(declared.iter().map(|module| module.namespace.as_bytes().to_vec()));
            environments.extend(declared.iter().filter_map(|module| match module.kind {
                ModuleKind::Environment(environment) => Some(environment),
                ModuleKind::Base | ModuleKind::Test => None,
            }));
        }
        namespaces.sort_unstable();
        namespaces.dedup();

        namespaces.iter().flat_map(|namespace| modules.paths(namespace, &environments)).collect()
    }

    /// Where the modules of each namespace are, for [`Self::module_inputs`], once this analysis's modules are recorded.
    fn module_files(&self) -> ModuleFiles {
        let mut namespaces: HashMap<FileId, Vec<u8>> = HashMap::default();
        // A machine's case classes are named under the machine, so a file's own namespace is the shortest of its
        // class-likes'.
        for class in self.codebase.class_likes.values() {
            let namespace = namespace_of(class.original_name.as_bytes());
            namespaces
                .entry(class.span.file_id)
                .and_modify(|held| {
                    if namespace.len() < held.len() {
                        *held = namespace.to_vec();
                    }
                })
                .or_insert_with(|| namespace.to_vec());
        }
        for import in &self.codebase.namespace_imports {
            namespaces.entry(import.span.file_id).or_insert_with(|| import.namespace.as_bytes().to_vec());
        }

        let mut folders: HashMap<Vec<u8>, BTreeSet<Vec<u8>>> = HashMap::default();
        namespaces.retain(|file_id, namespace| {
            let Some(file) = self.database.get(file_id).ok().filter(|file| is_source(file)) else {
                return false;
            };
            if namespace.is_empty() {
                return false;
            }

            // PSR-4 maps each folder below a namespace's folder to the namespace of the same name below it, so a
            // folder whose last part names its namespace's last part is held in the folder of the namespace above.
            let (mut folder, mut namespace) = (folder_of(&file.name), namespace.as_slice());
            loop {
                folders.entry(namespace.to_ascii_lowercase()).or_default().insert(folder.to_vec());
                let Some(separator) = namespace.iter().rposition(|byte| *byte == b'\\') else {
                    break;
                };
                let Some(inner) = folder.strip_suffix(b"/") else {
                    break;
                };
                let start = inner.iter().rposition(|byte| *byte == b'/').map_or(0, |slash| slash + 1);
                if inner[start..] != namespace[separator + 1..] {
                    break;
                }
                (folder, namespace) = (&folder[..start], &namespace[..separator]);
            }

            true
        });
        let built = self
            .codebase
            .function_likes
            .iter()
            .filter(|((class, _), method)| {
                !method.listens.is_empty()
                    || (method.original_name.as_bytes() == PROGRAM_ENTRY.as_bytes()
                        && self.codebase.class_likes.get(class).is_some_and(|class| {
                            class.flags.is_sharp() && class.kind.is_class() && !class.flags.is_struct()
                        }))
            })
            .map(|(_, method)| method.span.file_id)
            .collect();

        ModuleFiles { recorded: self.modules(), folders, namespaces, built }
    }

    /// `name` and, when it names a class-like, every class, interface and trait it inherits from, since an extension
    /// that read a class-like read its inherited members too.
    fn with_ancestors(&self, name: Word) -> Vec<Word> {
        let Some(class_like) = self.codebase.get_class_like(name) else {
            return vec![name];
        };

        let classes = std::iter::once(class_like.name).chain(class_like.all_parent_classes.iter().copied());
        let traits = classes
            .clone()
            .filter_map(|class| self.codebase.get_class_like(class))
            .flat_map(|class| class.used_traits.iter().copied());

        classes.chain(class_like.all_parent_interfaces.iter().copied()).chain(traits).collect()
    }

    /// The effects solve over every file's bodies, as the analysis ran it, which a PHP# function's type lists, spec
    /// section 29.
    #[must_use]
    pub fn effects(&self) -> Effects {
        let mut summaries: Vec<(&FileId, &[EffectSummary])> =
            self.file_states.iter().map(|(file_id, state)| (file_id, state.effect_summaries.as_slice())).collect();
        summaries.sort_unstable_by_key(|(file_id, _)| **file_id);
        let summaries: Vec<EffectSummary> =
            summaries.into_iter().flat_map(|(_, summaries)| summaries.iter().cloned()).collect();

        Effects::solve(&self.codebase, &summaries)
    }

    /// The standard library's inline forms, as [`Self::compile`] lowers every other PHP# file with them, which `mago
    /// convert` reads to find the library member that runs a PHP function. The library's files are those `compile`
    /// lowers first, and `refusing` keeps the errors that refuse one, as it does for `compile`, so no form comes from a
    /// file `compile` refuses.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when no analysis ran yet, or when re-analyzing a library file fails.
    pub fn standard_library_forms(
        &mut self,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> Result<InlineForms, OrchestratorError> {
        if !self.initialized {
            return Err(OrchestratorError::General(
                "analyze() must be called before standard_library_forms()".to_string(),
            ));
        }

        let files = self.sharp_files();
        let library: Vec<&Arc<File>> = files.iter().filter(|file| file.is_standard_library).collect();
        let (errors, unrefused) = self.errors(refusing);
        let session = self.plugin_registry.create_external_analysis_session(self.database.files());
        let reach = Reach::new(&self.codebase, &self.native_symbol_references);
        let effects = self.effects();
        let lowered = self.lower_all(
            &library,
            &InlineForms::default(),
            (&errors, &unrefused),
            &reach,
            &effects,
            session.as_ref(),
        )?;
        // A library file whose lowering panicked gives no forms, so a defect of the checker never leaves a caller
        // without them unseen: it fails the call with the panic's text.
        let panics: Vec<&str> = lowered
            .iter()
            .filter_map(|lowered| lowered.as_ref().err())
            .flat_map(|issues| issues.iter())
            .filter(|issue| issue.code.as_deref() == Some(LOWERING_PANIC))
            .map(|issue| issue.message.as_str())
            .collect();
        if !panics.is_empty() {
            return Err(OrchestratorError::General(panics.join("\n")));
        }

        Ok(lowered.into_iter().filter_map(Result::ok).flat_map(|(lowered, _)| lowered.forms).collect())
    }

    /// The issues that refuse the laws of the last analysis before any proof, as [`Self::compile`]'s `check` reports them.
    ///
    /// They are the [`refusals`] of the laws of the workspace's own files, each in the file that states its law, and no
    /// Lean process starts. Each PHP# file those laws reach is translated, a package's too, as `compile` translates it,
    /// but a file with an error the analysis reported, as `compile` leaves a file its errors refuse, so a law that
    /// reaches one is refused for it. A scan analyzes no method body, so it refuses no law.
    ///
    /// # Errors
    ///
    /// Returns [`OrchestratorError`] when no analysis ran yet, or when re-analyzing a file fails.
    pub fn law_refusals(&self) -> Result<HashMap<FileId, IssueCollection>, OrchestratorError> {
        if !self.initialized {
            return Err(OrchestratorError::General("analyze() must be called before law_refusals()".to_string()));
        }
        if self.scan_only {
            return Ok(HashMap::default());
        }

        let reach = Reach::new(&self.codebase, &self.native_symbol_references);
        let (law_files, reached) = self.law_reach(&reach);
        let files: Vec<Arc<File>> =
            self.database.files().filter(|file| reached.contains(&file.id) && Dialect::of(file).is_sharp()).collect();
        let reached_files: Vec<&Arc<File>> = files.iter().collect();
        let (errors, unrefused) = self.errors(|issues| issues);
        let session = self.plugin_registry.create_external_analysis_session(self.database.files());
        let translations = self.checked_all(
            &reached_files,
            &InlineForms::default(),
            (&errors, &unrefused),
            &self.effects(),
            session.as_ref(),
            |checked, _, _| translate(checked, &reach),
        )?;
        let errored: HashSet<FileId> = errors.keys().copied().collect();
        let mut refused = refusals(&translations.into_iter().filter_map(Result::ok).collect::<Vec<_>>(), &errored);
        refused.retain(|file_id, _| law_files.contains(file_id));

        Ok(refused)
    }

    /// The workspace's own files that state a law, whose laws [`Self::law_refusals`] refuses, and each file those laws
    /// reach, by `reach`.
    fn law_reach(&self, reach: &Reach) -> (HashSet<FileId>, HashSet<FileId>) {
        let law_files: HashSet<FileId> = reach
            .law_files()
            .filter(|law_file| self.database.get_ref(law_file).is_ok_and(|file| file.file_type == FileType::Host))
            .collect();
        let reached = law_files.iter().flat_map(|law_file| reach.files(*law_file)).copied().collect();

        (law_files, reached)
    }

    /// The PHP# files [`Self::compile`] lowers, the workspace's own, in file name order.
    fn sharp_files(&self) -> Vec<Arc<File>> {
        let mut files: Vec<Arc<File>> = self
            .database
            .files()
            .filter(|file| file.file_type == FileType::Host && Dialect::of(file).is_sharp())
            .collect();
        files.sort_unstable_by(|a, b| a.name.cmp(&b.name));

        files
    }

    /// The last analysis's errors, a package's PHP# file's that it reports none of among them, by file: those that
    /// refuse their file, as `refusing` keeps them, and those of each other file, which refuse nothing.
    fn errors(
        &self,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> (HashMap<FileId, IssueCollection>, HashMap<FileId, IssueCollection>) {
        let by_file = |issues: IssueCollection| {
            let mut by_file: HashMap<FileId, IssueCollection> = HashMap::default();
            for issue in issues {
                if let Some(span) = issue.primary_span() {
                    by_file.entry(span.file_id).or_default().push(issue);
                }
            }

            by_file
        };
        let reported: IssueCollection = self
            .collect_all_issues()
            .into_iter()
            .chain(self.file_states.values().flat_map(|state| state.package_errors.iter().cloned()))
            .filter(|issue| issue.level == Level::Error)
            .collect();
        let errors = by_file(refusing(reported.clone()));
        let mut unrefused = by_file(reported);
        unrefused.retain(|file_id, _| !errors.contains_key(file_id));

        (errors, unrefused)
    }

    /// Lowers each of `files` with `forms` to inline, gives a standard library file's own inline forms, and translates
    /// it to Lean as far as `reach` asks, as [`Self::checked_all`] checks it.
    fn lower_all(
        &self,
        files: &[&Arc<File>],
        forms: &InlineForms,
        refusals: (&HashMap<FileId, IssueCollection>, &HashMap<FileId, IssueCollection>),
        reach: &Reach,
        effects: &Effects,
        session: Option<&ExternalAnalysisSession>,
    ) -> Result<Vec<Lowered>, OrchestratorError> {
        self.checked_all(files, forms, refusals, effects, session, |checked, namespace_classes, reaches_extension| {
            let forms = if checked.file().is_standard_library { inline_forms(checked) } else { Vec::new() };

            (
                LoweredUnit { unit: lower(checked), forms, namespace_classes, reaches_extension },
                translate(checked, reach),
            )
        })
    }

    /// Gives what `output` makes of the checked program of each of `files`, with `forms` to inline, the class-likes of
    /// its namespace that would win over a default class-like it names and whether its code reaches a PHP# extension
    /// member, the files with errors that refuse them refused. Each file is checked from what the last analysis gave
    /// its lowering, or, where that analysis kept none, as an incremental one keeps none, from an analysis of the file
    /// alone. A file with a parse error or an error that refuses it is refused without being analyzed again, since its
    /// refusal holds only those errors. A file with only `unrefused` errors is checked as far as its code allows. The
    /// checked program assumes the checker accepted it, so one `output` can't handle is refused with those errors and
    /// the panic's own text, [`LOWERING_PANIC`], as rust-analyzer turns a panic on broken code into a failed request. A
    /// panic on a file with no error is a defect of the checker, which the same refusal names.
    fn checked_all<T>(
        &self,
        files: &[&Arc<File>],
        forms: &InlineForms,
        (errors, unrefused): (&HashMap<FileId, IssueCollection>, &HashMap<FileId, IssueCollection>),
        effects: &Effects,
        session: Option<&ExternalAnalysisSession>,
        output: impl Fn(&CheckedProgram<'_>, Vec<Word>, bool) -> T + Sync,
    ) -> Result<Vec<Result<T, IssueCollection>>, OrchestratorError>
    where
        T: Send,
    {
        files
            .par_iter()
            .map(|file| {
                let issues: Vec<Issue> =
                    errors.get(&file.id).map(|issues| issues.iter().cloned().collect()).unwrap_or_default();
                let arena = LocalArena::new();
                let program = parse_file_with_settings(&arena, file, self.parser_settings);
                let names = NameResolver::new(&arena).resolve_in(
                    program,
                    &self.namespace_classes,
                    &self.changing_members,
                    &self.extern_namespaces,
                );
                let mut namespace_classes: Vec<Word> = names.namespace_classes().map(mago_word::word).collect();
                namespace_classes.sort();
                let kept = self
                    .file_states
                    .get(&file.id)
                    .and_then(|state| Some((state.lowering_input.as_ref()?, state.reaches_extension)));
                let analyzed;
                let (input, reaches_extension) = match kept {
                    Some(kept) => kept,
                    None if issues.is_empty() && program.errors.is_empty() => {
                        #[cfg(test)]
                        self.analyzed_to_lower
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(file.id);
                        // This analysis of the one file records exactly what its code reaches, unlike the references
                        // the warm path keeps from a method it analyzed before.
                        let mut result = AnalysisResult::new(SymbolReferences::new());
                        let artifacts = self
                            .file_analyzer(&arena, file, &names, session)
                            .analyze_with_artifacts(program, &mut result)?;
                        analyzed = (
                            artifacts.into_lowering_input(),
                            super::reaches_extension(&self.codebase, &result.symbol_references),
                        );
                        (&analyzed.0, analyzed.1)
                    }
                    None => {
                        analyzed = (LoweringInput::default(), false);
                        (&analyzed.0, analyzed.1)
                    }
                };

                Ok(match check(file, program, names, input, &self.codebase, effects, forms, &issues) {
                    Ok(checked) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        output(&checked, namespace_classes, reaches_extension)
                    }))
                    .map_err(|panic| {
                        let mut refused = unrefused.get(&file.id).cloned().unwrap_or_default();
                        let accepted = refused.is_empty();
                        refused.push(lowering_panic(file, panic.as_ref(), accepted));
                        refused
                    }),
                    Err(Refusal::Parse(errors)) => Err(errors.iter().map(Issue::from).collect()),
                    Err(Refusal::Compile(errors)) => Err(IssueCollection::from(errors)),
                })
            })
            .collect()
    }
}

/// The code of the issue that refuses a file whose lowering panicked.
pub const LOWERING_PANIC: &str = "lowering-panic";

/// The error that refuses `file`, whose lowering panicked with `panic`, with the panic's text. A panic on a file the
/// checker `accepted` is a defect of the checker, and one on a file with errors is the lowering of a program the
/// checker refuses.
fn lowering_panic(file: &File, panic: &(dyn std::any::Any + Send), accepted: bool) -> Issue {
    let text = panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no text".to_owned());
    let name = String::from_utf8_lossy(&file.name);
    let message = if accepted {
        format!("The lowering of {name} panicked though the checker accepted it, a defect of the checker: {text}")
    } else {
        format!("The lowering of {name} panicked on the checker's errors in it: {text}")
    };

    Issue::error(message).with_code(LOWERING_PANIC)
}

/// The name `listing` enters a key under: what it lists, and the class-like or namespace it lists for or the method
/// search it lists by.
fn listing_name(listing: &Listing) -> Vec<u8> {
    match listing {
        Listing::ClassLikes(kind) => format!("class-likes of kind {kind}").into_bytes(),
        Listing::Functions => b"functions".to_vec(),
        Listing::Constants => b"constants".to_vec(),
        Listing::DirectDescendants(class) => [b"direct descendants of ", class.as_bytes()].concat(),
        Listing::Descendants(class) => [b"descendants of ", class.as_bytes()].concat(),
        Listing::Namespace(namespace) => [b"namespace ", namespace.as_bytes()].concat(),
        Listing::Methods(search) => [b"methods ", search.as_bytes()].concat(),
    }
}

/// Returns `true` for a file whose edit can make a `.sharpc` file stale: every loaded file outside `vendor/`, which
/// `composer.lock` stands for, other than the prelude's.
fn is_source(file: &File) -> bool {
    file.file_type != FileType::Builtin && !file.name.starts_with(VENDOR)
}

/// The hash of the declarations of the PHP# file `file`, spec section 27, from its signature in `codebase`.
///
/// It is the hash [`unit::declarations_hash`] gives, which the file's compiled file's header names, and each file that
/// reads its declarations or bodies names for it beside its stamp. It is all zero, so every file that reads
/// it keeps its stamp, for a file in `vendor/`, which `composer.lock` stands for, a file outside the workspace, a file
/// that states an `extern` line, and a file whose signature holds no declaration, as one of an event or a struct: no
/// signature holds an `extern` line, an event or a struct.
fn declarations_hash(codebase: &CodebaseMetadata, file: &File) -> [u8; 16] {
    let states_extern = codebase.externs.values().flatten().any(|line| line.span.file_id == file.id)
        || codebase.extern_namespaces.iter().any(|line| line.span.file_id == file.id);
    if !Dialect::of(file).is_sharp()
        || !is_source(file)
        || Path::new(String::from_utf8_lossy(&file.name).as_ref()).is_absolute()
        || states_extern
    {
        return [0; 16];
    }

    codebase
        .get_file_signature(&file.id)
        .filter(|signature| !signature.ast_nodes.is_empty())
        .map_or([0; 16], unit::declarations_hash)
}

/// The workspace-relative folder of the file `name`, with its trailing `/`, or nothing for a file at the root.
fn folder_of(name: &[u8]) -> &[u8] {
    &name[..name.iter().rposition(|byte| *byte == b'/').map_or(0, |slash| slash + 1)]
}

/// The namespace of the class-like named `name`, as PHP writes both, or nothing for one of the root namespace.
fn namespace_of(name: &[u8]) -> &[u8] {
    &name[..name.iter().rposition(|byte| *byte == b'\\').unwrap_or(0)]
}

/// Spec section 32: where the modules of each namespace are, as a compiled file names them as inputs.
///
/// The engine reads a class's modules from the listener table's record of each module by the namespace it serves,
/// so a file names the file of each recorded module it reads. A module added for a namespace sits in a folder that
/// holds a file of the namespace, or in a PSR-4 parent of one, so a file also names the `Module.sharp` of each such
/// folder, stamped absent while no file is there. A file added beside a module, other than a module, changes no input.
struct ModuleFiles {
    /// Each base and environment module the listener table records.
    recorded: Vec<RecordedModule>,
    /// Each folder, with its trailing `/`, that holds a source file of a namespace or is a PSR-4 parent of a folder
    /// that holds one of a namespace below it, by the namespace in lowercase, as PHP writes it.
    folders: HashMap<Vec<u8>, BTreeSet<Vec<u8>>>,
    /// The namespace each source file declares, as PHP writes it, for each such file of a namespace other than the
    /// root one.
    namespaces: HashMap<FileId, Vec<u8>>,
    /// Each file that declares a listener or a program, whose class PHP# builds through the container, spec sections
    /// 15 and 27.
    built: HashSet<FileId>,
}

impl ModuleFiles {
    /// The files of the modules that serve `namespace`, in lowercase as PHP writes it, and each namespace above it but
    /// the root one: each recorded base module of them, and each of `environments`', and the `Module.sharp` and
    /// `Module.<Environment>.sharp` of each of their folders.
    fn paths(&self, namespace: &[u8], environments: &[Word]) -> Vec<Vec<u8>> {
        let names: Vec<Vec<u8>> = std::iter::once(MODULE_FILE.as_bytes().to_vec())
            .chain(environments.iter().map(|environment| [b"Module.", environment.as_bytes(), b".sharp"].concat()))
            .collect();
        let mut paths = Vec::new();
        let mut served = namespace;
        while !served.is_empty() {
            paths.extend(
                self.recorded
                    .iter()
                    .filter(|module| module.namespace.as_bytes() == served)
                    .filter(|module| module.environment.as_ref().is_none_or(|named| environments.contains(named)))
                    .map(|module| module.file.as_bytes().to_vec()),
            );
            for folder in self.folders.get(served).into_iter().flatten() {
                paths.extend(names.iter().map(|name| [folder.as_slice(), name.as_slice()].concat()));
            }
            served = namespace_of(served);
        }

        paths
    }
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
///
/// The walk from each declaration is worked out once, for every file it serves: the declarations that reach each other
/// form one strongly connected part, and each part's source files are its own declarations' and those of every part it
/// reaches, which come before it.
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
    /// Each source file that declares a declaration, whose place here is its bit in a set of source files.
    sources: Vec<FileId>,
    /// The place of each file of `sources`.
    places: HashMap<FileId, usize>,
    /// The sets of source files the walk from a declaration reaches, one bit per place in `sources`. The first is the
    /// empty set.
    reached: Vec<Vec<u64>>,
    /// The set in `reached` of each declaration the walk leaves through a signature reference or a class's members.
    /// The walk from any other declaration reaches only the files that declare it.
    reaches: HashMap<SymbolIdentifier, usize>,
    /// The source files the walk from each declaration a plugin that registers a provider read reaches, in any
    /// callback, as `code_reads` gives it. A read of a plugin with no provider decides only which issues are reported,
    /// never the lowered code, so it names no input.
    read: Vec<u64>,
    /// The sets of files [`Self::follow_bodies`] works out, one per strongly connected part of the files whose bodies
    /// each file reads.
    bodies: Vec<Vec<u64>>,
    /// The set in `bodies` of each file [`Self::follow_bodies`] took.
    body_sets: HashMap<FileId, usize>,
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
            sources: Vec::new(),
            places: HashMap::default(),
            reached: vec![Vec::new()],
            reaches: HashMap::default(),
            read: Vec::new(),
            bodies: Vec::new(),
            body_sets: HashMap::default(),
        };

        for edge in service.native_symbol_references.cascade_edges(&service.codebase) {
            match edge {
                CascadeEdge::Signature { from, to } | CascadeEdge::Attribute { from, to, signature: true } => {
                    if !dependencies.signature.contains_key(&from) && !from.1.is_empty() {
                        dependencies.signature_members.entry(from.0).or_default().push(from);
                    }
                    dependencies.signature.entry(from).or_default().push(to);
                    dependencies.add_reference(from, to);
                }
                CascadeEdge::Body { from, to } | CascadeEdge::Attribute { from, to, signature: false } => {
                    dependencies.add_reference(from, to);
                }
                CascadeEdge::File { file, to } => dependencies.files.entry(file).or_default().push(to),
                CascadeEdge::Inherits { .. } => {}
            }
        }

        // A library's polyfill of a name the engine builds in never declares it, since the built-in keeps the name, so
        // its file is no input of the code that reads the name.
        for (file_id, signature) in &service.codebase.file_signatures {
            if !service.database.get(file_id).is_ok_and(|file| is_source(&file)) {
                continue;
            }

            for node in signature
                .ast_nodes
                .iter()
                .filter(|node| !node.name.is_empty() && !super::is_kept_built_in(&service.codebase, node))
            {
                dependencies.declared_in.entry((node.name, empty_word())).or_default().push(*file_id);
                for child in node.children.iter().filter(|child| !child.name.is_empty()) {
                    let member = (node.name, super::member_key(&service.codebase, node.name, child));
                    dependencies.declared_in.entry(member).or_default().push(*file_id);
                    dependencies.signature_members.entry(node.name).or_default().push(member);
                }
            }
        }

        // A class alias declares its name, and its signature is its target's, so a file whose code reaches the alias
        // lists the file that declares it and, along the chain of aliases, each file that declares the next name.
        for (alias, target, span) in service.codebase.class_like_alias_declarations() {
            let alias = (alias, empty_word());
            let target = service.codebase.class_key(target);
            dependencies.signature.entry(alias).or_default().push((target, empty_word()));
            if service.database.get(&span.file_id).is_ok_and(|file| is_source(&file)) {
                dependencies.declared_in.entry(alias).or_default().push(span.file_id);
            }
        }

        dependencies.walk(&super::code_reads(&service.file_states, &service.before_analysis_reads));

        dependencies
    }

    /// Works out the source files the walk from each declaration reaches, as [`Self::signature_changes`] reads them,
    /// and those the walk from each declaration in `code_reads` reaches.
    fn walk(&mut self, code_reads: &FileReads) {
        for file_id in self.declared_in.values().flatten() {
            self.places.entry(*file_id).or_insert_with(|| {
                self.sources.push(*file_id);
                self.sources.len() - 1
            });
        }
        let words = self.sources.len().div_ceil(64);

        let mut nodes: Vec<SymbolIdentifier> = Vec::new();
        let mut index: HashMap<SymbolIdentifier, usize> = HashMap::default();
        let signatures = self.signature.iter().flat_map(|(from, to)| std::iter::once(*from).chain(to.iter().copied()));
        let classes = self
            .signature_members
            .iter()
            .flat_map(|(class, members)| std::iter::once((*class, empty_word())).chain(members.iter().copied()));
        for declaration in signatures.chain(classes) {
            index.entry(declaration).or_insert_with(|| {
                nodes.push(declaration);
                nodes.len() - 1
            });
        }
        let successors: Vec<Vec<usize>> = nodes
            .iter()
            .map(|node| {
                let members = if node.1.is_empty() { self.signature_members.get(&node.0) } else { None };

                self.signature.get(node).into_iter().chain(members).flatten().map(|next| index[next]).collect()
            })
            .collect();

        let parts = strongly_connected_parts(&successors);
        let mut part_of = vec![0; nodes.len()];
        for (part, members) in parts.iter().enumerate() {
            for node in members {
                part_of[*node] = part;
            }
        }
        // A part comes after every part it reaches, so their sets are there when it takes them. A part that declares
        // nothing and reaches one set shares that set.
        let mut set_of_part: Vec<usize> = Vec::with_capacity(parts.len());
        for (part, members) in parts.iter().enumerate() {
            let mut reached: Vec<usize> = members
                .iter()
                .flat_map(|node| &successors[*node])
                .map(|next| part_of[*next])
                .filter(|next| *next != part)
                .map(|next| set_of_part[next])
                .filter(|set| *set != 0)
                .collect();
            reached.sort_unstable();
            reached.dedup();
            let mut sources = vec![0; words];
            let mut declares = false;
            for file_id in members.iter().filter_map(|node| self.declared_in.get(&nodes[*node])).flatten() {
                add_source(&mut sources, self.places[file_id]);
                declares = true;
            }
            let set = match (declares, reached.as_slice()) {
                (false, []) => 0,
                (false, [set]) => *set,
                _ => {
                    for set in &reached {
                        add_sources(&mut sources, &self.reached[*set]);
                    }
                    self.reached.push(sources);
                    self.reached.len() - 1
                }
            };
            set_of_part.push(set);
        }
        self.reaches = nodes.into_iter().zip(part_of).map(|(node, part)| (node, set_of_part[part])).collect();

        // A read names a class-like as the extension wrote it, which the codebase keys, spec section 24.
        let mut read = vec![0; words];
        for &(class, member) in &code_reads.symbols {
            self.signature_changes((self.service.codebase.class_key(class), member), &mut read);
        }
        self.read = read;
    }

    fn add_reference(&mut self, from: SymbolIdentifier, to: SymbolIdentifier) {
        if !self.references.contains_key(&from) && !from.1.is_empty() {
            self.referencing_members.entry(from.0).or_default().push(from);
        }
        self.references.entry(from).or_default().push(to);
    }

    /// The source files other than `file_id` whose signature edit can change `file_id`'s analyzed code: those that
    /// declare what its code and its declarations reach. A set of names its analysis or a plugin listed is no file's:
    /// its key holds the hash of the answer, so only an edit that changes the answer changes the file.
    fn reached_by(&self, file_id: FileId) -> HashSet<FileId> {
        let mut sources = self.read.clone();
        self.file_changes(file_id, &mut sources);

        self.files_of(sources).filter(|source| *source != file_id).collect()
    }

    /// Works out, once for each of `files` and each file whose bodies they read, as `body_files` lists them by the
    /// file that reads them, the files [`Self::reached_through_bodies`] gives: the files whose bodies it reads along
    /// every chain of such reads, the file itself, and the source files [`Self::reached_by`] gives for each of them. The
    /// files that read each other's bodies form one strongly connected part, and each part's set is its own files'
    /// and that of every part it reads, which comes before it.
    fn follow_bodies(&mut self, files: &[FileId], body_files: &HashMap<FileId, (Reads, HashSet<FileId>)>) {
        let mut nodes: Vec<FileId> = Vec::new();
        let mut index: HashMap<FileId, usize> = HashMap::default();
        let reading = body_files.iter().flat_map(|(file_id, (_, bodies))| std::iter::once(file_id).chain(bodies));
        for file_id in files.iter().chain(reading) {
            index.entry(*file_id).or_insert_with(|| {
                nodes.push(*file_id);
                nodes.len() - 1
            });
        }
        // A file whose bodies are read may declare nothing the walk reaches, so each file takes a place of its own.
        for file_id in &nodes {
            self.places.entry(*file_id).or_insert_with(|| {
                self.sources.push(*file_id);
                self.sources.len() - 1
            });
        }
        let words = self.sources.len().div_ceil(64);
        let successors: Vec<Vec<usize>> = nodes
            .iter()
            .map(|file_id| {
                body_files.get(file_id).into_iter().flat_map(|(_, bodies)| bodies).map(|body| index[body]).collect()
            })
            .collect();

        let parts = strongly_connected_parts(&successors);
        let mut part_of = vec![0; nodes.len()];
        for (part, members) in parts.iter().enumerate() {
            for node in members {
                part_of[*node] = part;
            }
        }
        let mut bodies: Vec<Vec<u64>> = Vec::with_capacity(parts.len());
        for (part, members) in parts.iter().enumerate() {
            let mut sources = vec![0; words];
            add_sources(&mut sources, &self.read);
            for node in members {
                add_source(&mut sources, self.places[&nodes[*node]]);
                self.file_changes(nodes[*node], &mut sources);
            }
            let mut read: Vec<usize> = members
                .iter()
                .flat_map(|node| &successors[*node])
                .map(|next| part_of[*next])
                .filter(|next| *next != part)
                .collect();
            read.sort_unstable();
            read.dedup();
            for next in read {
                add_sources(&mut sources, &bodies[next]);
            }
            bodies.push(sources);
        }
        self.body_sets = nodes.into_iter().zip(part_of).collect();
        self.bodies = bodies;
    }

    /// The files whose edit can change `file_id`'s code, of the files [`Self::follow_bodies`] took: the files whose
    /// bodies it reads along every chain of such reads, the source files whose signature edit can change the analyzed
    /// code of any of them or of `file_id`, and `file_id` itself.
    fn reached_through_bodies(&self, file_id: FileId) -> impl Iterator<Item = FileId> {
        let set = self.body_sets.get(&file_id).map_or(&[][..], |set| self.bodies[*set].as_slice());

        self.files_of(set.iter().copied())
    }

    /// Adds to `sources` each source file whose signature edit can change `file_id`'s analyzed code, as
    /// [`Self::reached_by`] gives them, except those of a plugin's reads.
    fn file_changes(&self, file_id: FileId, sources: &mut [u64]) {
        if let Ok(file) = self.service.database.get(&file_id) {
            for referenced in self.files.get(&mago_word::word(file.name.as_ref())).into_iter().flatten() {
                self.signature_changes(*referenced, sources);
            }
        }
        let signature = self.service.codebase.get_file_signature(&file_id);
        for node in signature.iter().flat_map(|signature| &signature.ast_nodes).filter(|node| !node.name.is_empty()) {
            self.changes((node.name, empty_word()), sources);
            for member in self.referencing_members.get(&node.name).into_iter().flatten() {
                self.changes(*member, sources);
            }
            for member in self.signature_members.get(&node.name).into_iter().flatten() {
                self.changes(*member, sources);
            }
        }
    }

    /// The files of `sources`, a set of files one bit per place in `sources`.
    fn files_of(&self, sources: impl IntoIterator<Item = u64>) -> impl Iterator<Item = FileId> {
        sources
            .into_iter()
            .enumerate()
            .flat_map(|(word, mut bits)| {
                std::iter::from_fn(move || {
                    (bits != 0).then(|| {
                        let bit = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        word * 64 + bit
                    })
                })
            })
            .map(|place| self.sources[place])
    }

    /// Adds to `sources` each source file that declares a declaration whose change invalidates `declaration`'s
    /// signature or body.
    fn changes(&self, declaration: SymbolIdentifier, sources: &mut [u64]) {
        self.signature_changes(declaration, sources);
        for referenced in self.references.get(&declaration).into_iter().flatten() {
            self.signature_changes(*referenced, sources);
        }
    }

    /// Adds to `sources` each source file that declares a declaration whose change invalidates `declaration`'s
    /// signature: a declaration whose signature change reaches it through signature references and from members to
    /// their class, and the class of a member whose signature references anything, which marks that member invalid
    /// when the class changes.
    fn signature_changes(&self, declaration: SymbolIdentifier, sources: &mut [u64]) {
        if self.signature.contains_key(&declaration) {
            self.declarers((declaration.0, empty_word()), sources);
        }
        match self.reaches.get(&declaration) {
            Some(set) => add_sources(sources, &self.reached[*set]),
            None => self.declarers(declaration, sources),
        }
    }

    /// Adds to `sources` each source file that declares `declaration`.
    fn declarers(&self, declaration: SymbolIdentifier, sources: &mut [u64]) {
        for file_id in self.declared_in.get(&declaration).into_iter().flatten() {
            add_source(sources, self.places[file_id]);
        }
    }
}

/// Adds the source file at `place` to `sources`, a set of source files one bit per place.
fn add_source(sources: &mut [u64], place: usize) {
    sources[place / 64] |= 1 << (place % 64);
}

/// Adds every source file of `other` to `sources`, each a set of source files one bit per place.
fn add_sources(sources: &mut [u64], other: &[u64]) {
    for (bits, other) in sources.iter_mut().zip(other) {
        *bits |= other;
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
    use mago_database::file::FolderNamespace;
    use mago_prelude::Prelude;
    use mago_sharp_bridge::unit::header;
    use mago_sharp_bridge::unit::sharp_unit_header;
    use mago_syntax::settings::ParserSettings;

    use super::*;
    use crate::service::PRELUDE;

    static PLUGIN_REGISTRY: LazyLock<Arc<PluginRegistry>> =
        LazyLock::new(|| Arc::new(PluginRegistry::with_library_providers()));

    /// The build ID of the checker the tests compile with.
    const CHECKER: [u8; 16] = *b"the checker's id";

    fn prelude() -> (CodebaseMetadata, SymbolReferences) {
        let Prelude { metadata, symbol_references, .. } = PRELUDE.clone();

        (metadata, symbol_references)
    }

    /// The standard library's `Sharp\System\Environment`, the abstract effect class of the effect `Environment`, at
    /// the library's path, with its `currentDirectory`.
    const ENVIRONMENT: (&str, &str) = (
        "vendor/heyjordanparker/php-sharp-composer/library/Sharp/System/Environment.sharp",
        "namespace Sharp.System;\n\npublic effect abstract class Environment\n{\n    public abstract string currentDirectory();\n}\n",
    );

    /// A project of files, each a workspace-relative name and its contents. A PHP file under `vendor/` is vendored, and
    /// every other file is a host file, as `mago compile` loads every `.sharp` file. A file under the standard
    /// library's package is the library's, as the loader marks it from the package's `composer.json`, and a file under
    /// `tests/` is a test file, as the loader marks a file of a folder `composer.json` lists under `autoload-dev`.
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
            if name.starts_with("tests/") {
                file.folder_namespace = Some(FolderNamespace::Test);
            }
            database.add(file);
        }

        database
    }

    /// The service after a full analysis of `database`.
    fn analyzed(database: &Database<'static>) -> IncrementalAnalysisService {
        analyzed_with(database, Settings::default())
    }

    /// The service after a full analysis of `database` that kept what it gave the lowering of each PHP# file.
    fn analyzed_for_lowering(database: &Database<'static>) -> IncrementalAnalysisService {
        let mut service = IncrementalAnalysisService::new(
            database.read_only(),
            prelude,
            Settings::default(),
            ParserSettings::default(),
            Arc::clone(&PLUGIN_REGISTRY),
        )
        .keeping_lowering_inputs();
        service.analyze().expect("the analysis runs");

        service
    }

    /// The name of each file the compiles of `service` analyzed again to lower it.
    fn analyzed_to_lower(service: &IncrementalAnalysisService) -> BTreeSet<String> {
        let analyzed = service.analyzed_to_lower.lock().expect("no compile panicked while it held the files");

        analyzed
            .iter()
            .map(|file_id| {
                String::from_utf8_lossy(&service.database().get(file_id).expect("the file exists").name).into()
            })
            .collect()
    }

    /// The service after a full analysis of `database` under `settings`.
    fn analyzed_with(database: &Database<'static>, settings: Settings) -> IncrementalAnalysisService {
        let mut service = IncrementalAnalysisService::new(
            database.read_only(),
            prelude,
            settings,
            ParserSettings::default(),
            Arc::clone(&PLUGIN_REGISTRY),
        );
        service.analyze().expect("the analysis runs");

        service
    }

    /// Stamps every input as an empty file, so a test sees which inputs a file has.
    fn empty_stamp(path: &[u8]) -> Result<Option<Input>, DatabaseError> {
        Ok(Some(absent(path)))
    }

    /// The input of `path` stamped absent, as one no file is at.
    fn absent(path: &[u8]) -> Input {
        Input { path: path.to_vec(), size: 0, mtime_ns: 0, hash: [0; 16], declarations_hash: [0; 16] }
    }

    /// Stamps each file of `database` with its size and hash, and `composer.lock` as an empty file.
    fn database_stamp(database: &Database<'static>) -> impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync {
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
                declarations_hash: [0; 16],
            }))
        }
    }

    /// Proves every law, as a Lean step that refuses nothing.
    fn proved(
        _: Vec<Translation>,
        _: &Reach,
        _: &HashSet<FileId>,
    ) -> std::io::Result<HashMap<FileId, IssueCollection>> {
        Ok(HashMap::default())
    }

    /// No compiled file, as a compile with no `.sharp` folder finds.
    fn uncompiled(_: &File) -> Result<Option<Vec<u8>>, DatabaseError> {
        Ok(None)
    }

    /// The bytes of a compiled file, which a compile into no folder gives back as they are.
    fn unwritten(_: &File, bytes: Vec<u8>) -> Vec<u8> {
        bytes
    }

    /// Each compilation of a compile of `service` that keeps no file, as the first compile of a project runs.
    fn fresh(
        service: &mut IncrementalAnalysisService,
        stamp: impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync,
        check: impl FnOnce(Vec<Translation>, &Reach, &HashSet<FileId>) -> std::io::Result<HashMap<FileId, IssueCollection>>,
        standard_library: &Library,
        refusing: impl FnOnce(IssueCollection) -> IssueCollection,
    ) -> Result<Vec<(FileId, Compilation)>, OrchestratorError> {
        let folder = Folder {
            last: &CompiledFiles::default(),
            compiled: &uncompiled,
            intact: &mut |_| false,
            write: &unwritten,
        };

        service
            .compile(folder, None, stamp, check, CHECKER, standard_library, refusing)
            .map(|((compiled, _), _)| compiled)
    }

    /// Each compiled file of `service` by its name.
    fn compile(
        service: &mut IncrementalAnalysisService,
        stamp: impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync,
    ) -> std::collections::BTreeMap<String, Compilation> {
        let compiled = fresh(service, stamp, proved, &Library::default(), |issues| issues).expect("the compile runs");
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
        let header = size_of::<sharp_unit_header>();
        let texts = header + 56 * count + 56 * nodes + 4 * children;

        (0..count)
            .map(|index| {
                let input = header + 56 * index;
                let (offset, length) = (word(input), word(input + 4));

                Input {
                    path: bytes[texts + offset..texts + offset + length].to_vec(),
                    size: u64::from_le_bytes(bytes[input + 8..input + 16].try_into().unwrap()),
                    mtime_ns: i64::from_le_bytes(bytes[input + 16..input + 24].try_into().unwrap()),
                    hash: bytes[input + 24..input + 40].try_into().unwrap(),
                    declarations_hash: bytes[input + 40..input + 56].try_into().unwrap(),
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
    fn an_accepted_file_compiles_to_bytes_whose_header_names_its_source_and_the_standard_library() {
        let database = project(&[("app/Order.sharp", ORDER)]);
        let mut service = analyzed(&database);
        let library = Library { native_fingerprint: [0x2d; 16], version: b"0.3.2".to_vec() };

        let compiled = fresh(&mut service, empty_stamp, proved, &library, |issues| issues).expect("the compile runs");

        assert_eq!(compiled.len(), 1, "{compiled:?}");
        let (file_id, Compilation::Accepted(bytes)) = &compiled[0] else {
            panic!("app/Order.sharp is refused: {compiled:?}");
        };
        assert_eq!(*file_id, FileId::new(b"app/Order.sharp"));
        let header = header(bytes).expect("the header is valid");
        assert_eq!(header.source_hash, source_hash(ORDER.as_bytes()));
        assert_eq!(header.source_size, ORDER.len() as u64);
        assert_eq!(header.library, library);
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

    /// A compile whose errors refuse nothing, as the hosts' compile, lowers a file with errors as far as its code
    /// allows. A lowering that panics on it refuses the file with the panic's text beside its errors, so the panic
    /// never passes unseen: a `Map` spread into a `List` literal is an error the bridge doesn't survive.
    #[test]
    fn a_lowering_that_panics_refuses_its_file_with_the_panic_s_text() {
        let menu = "namespace App;\n\npublic class Menu\n{\n    public List<int|string> names(Map<string, int> counts)\n    {\n        return [...counts, \"soup\"];\n    }\n}\n";
        let database = project(&[("app/Menu.sharp", menu)]);
        let mut service = analyzed(&database);

        let compiled = fresh(&mut service, empty_stamp, proved, &Library::default(), |_| IssueCollection::default())
            .expect("the compile runs");

        let [(_, Compilation::Refused(errors))] = compiled.as_slice() else {
            panic!("app/Menu.sharp is not refused: {compiled:?}");
        };
        let panics: Vec<&str> = errors
            .iter()
            .filter(|issue| issue.code.as_deref() == Some(LOWERING_PANIC))
            .map(|issue| issue.message.as_str())
            .collect();
        assert_eq!(
            panics,
            [
                "The lowering of app/Menu.sharp panicked on the checker's errors in it: internal error: entered unreachable code: the checker refuses a `List` element in a `Map` literal"
            ]
        );
        assert!(errors.iter().any(|issue| issue.code.as_deref() == Some("invalid-array-element")), "{errors:?}");
    }

    /// A panic's text is its `&str` or `String`, as `std` prints it, and a panic on a file the checker accepted names a
    /// defect of the checker.
    #[test]
    fn a_lowering_panic_names_its_text_and_whether_the_checker_accepted_the_file() {
        let file = File::ephemeral(Cow::Borrowed(b"app/Menu.sharp"), Cow::Borrowed(b""));
        let message = |panic: &(dyn std::any::Any + Send), accepted| lowering_panic(&file, panic, accepted).message;

        assert_eq!(
            message(&"boom", true),
            "The lowering of app/Menu.sharp panicked though the checker accepted it, a defect of the checker: boom"
        );
        assert_eq!(
            message(&"boom".to_owned(), false),
            "The lowering of app/Menu.sharp panicked on the checker's errors in it: boom"
        );
        assert_eq!(
            message(&1, false),
            "The lowering of app/Menu.sharp panicked on the checker's errors in it: a panic with no text"
        );
        assert_eq!(lowering_panic(&file, &"boom", true).code.as_deref(), Some(LOWERING_PANIC));
    }

    /// Spec section 11: a type parameter's bound is a class or an interface, so `Any?` is a refused bound, which bounds
    /// nothing, and its one error is its own file's. A file that creates the class has none, and compiles.
    #[test]
    fn a_file_that_creates_a_class_whose_bound_is_refused_compiles_and_the_class_s_file_is_refused() {
        let boxed = "namespace App;\n\npublic class Box<T : Any?>\n{\n    public Box(public T item { get; })\n    {\n    }\n}\n";
        let packer = "namespace App;\n\npublic class Packer\n{\n    public Box<int> pack()\n    {\n        return new Box<int>(1);\n    }\n}\n";
        let database = project(&[("app/Box.sharp", boxed), ("app/Packer.sharp", packer)]);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        accepted(&compiled, "app/Packer.sharp");
        let Some(Compilation::Refused(errors)) = compiled.get("app/Box.sharp") else {
            panic!("app/Box.sharp is not refused: {compiled:?}");
        };
        assert!(!errors.is_empty());
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
            (
                "app/Stubs/Shop.sharp",
                "namespace App.Stubs;\n\nimport Acme.Tax;\nimport Lib.Rate;\n\nextern Rate uses none;\nextern Tax uses none;\n".to_string(),
            ),
            ("vendor/acme/tax/src/Tax.php", TAX.to_string()),
        ]
    }

    fn shop_project(files: &[(&'static str, String)]) -> Database<'static> {
        project(&files.iter().map(|(name, contents)| (*name, contents.as_str())).collect::<Vec<_>>())
    }

    #[test]
    fn a_file_names_its_source_its_namespaces_module_files_each_file_whose_edit_reanalyzes_it_and_composer_lock() {
        let database = shop_project(&shop(MONEY, RATE));
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        // `Cash` holds the `parse` that overrides `Money`'s, which `Money`'s `tryParse` calls, so the effects of the
        // bodies `Order` reaches through `Money` can come from its file.
        assert_eq!(
            input_paths(accepted(&compiled, "app/Order.sharp")),
            [
                "app/Cash.sharp",
                "app/Module.sharp",
                "app/Money.sharp",
                "app/Order.sharp",
                "composer.lock",
                "lib/Rate.php"
            ],
            "a vendor file is left to composer.lock"
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/Unrelated.sharp")),
            ["app/Module.sharp", "app/Unrelated.sharp", "composer.lock"]
        );
    }

    #[test]
    fn a_package_file_names_its_own_source_and_leaves_the_rest_of_its_package_to_composer_lock() {
        let database = project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)]);

        let compiled = compile(&mut analyzed(&database), empty_stamp);

        assert_eq!(input_paths(accepted(&compiled, TEXT_PATH)), ["composer.lock", TEXT_PATH]);
        assert_eq!(
            input_paths(accepted(&compiled, "app/Title.sharp")),
            ["app/Module.sharp", "app/Title.sharp", "composer.lock"]
        );
    }

    #[test]
    fn an_input_no_file_is_at_is_stored_absent_as_an_all_zero_stamp() {
        let database = project(&[("app/Order.sharp", ORDER)]);
        let stamp = database_stamp(&database);

        let compiled =
            compile(&mut analyzed(&database), |path| if path == b"composer.lock" { Ok(None) } else { stamp(path) });

        assert_eq!(
            inputs(accepted(&compiled, "app/Order.sharp")),
            [
                absent(b"app/Module.sharp"),
                Input {
                    path: b"app/Order.sharp".to_vec(),
                    size: ORDER.len() as u64,
                    mtime_ns: 0,
                    hash: source_hash(ORDER.as_bytes()),
                    declarations_hash: [0; 16],
                },
                absent(b"composer.lock"),
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
                declarations_hash: [0; 16],
            }]
        );
    }

    /// A PHP# file that reads `Lib\Rate` through its alias `Lib\Pace`.
    const PACED: &str = "namespace App;\n\nimport Lib.Pace;\n\npublic class Paced\n{\n    public int percent()\n    {\n        return Pace.percent();\n    }\n}\n";

    #[test]
    fn a_file_that_declares_a_class_alias_is_an_input_of_each_file_that_reads_the_alias() {
        let mut files = shop(MONEY, RATE);
        files.push(("lib/aliases.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'Lib\\\\Pace');\n".to_string()));
        files.push(("app/Paced.sharp", PACED.to_string()));
        let database = shop_project(&files);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Paced.sharp")),
            ["app/Module.sharp", "app/Paced.sharp", "composer.lock", "lib/Rate.php", "lib/aliases.php"]
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/Unrelated.sharp")),
            ["app/Module.sharp", "app/Unrelated.sharp", "composer.lock"]
        );
    }

    /// A PHP# file that names `Lib\Pace` only as a parameter type.
    const FITTED: &str = "namespace App;\n\nimport Lib.Pace;\n\npublic class Fitted\n{\n    public int fits(Pace pace)\n    {\n        return 1;\n    }\n}\n";

    #[test]
    fn a_file_that_reads_an_alias_of_an_alias_lists_the_file_of_each_alias_and_of_the_class_it_names() {
        let mut files = shop(MONEY, RATE);
        files.push(("lib/aliases.php", "<?php\n\nclass_alias('Lib\\\\Rapid', 'Lib\\\\Pace');\n".to_string()));
        files.push(("lib/rapid.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'Lib\\\\Rapid');\n".to_string()));
        files.push(("app/Paced.sharp", PACED.to_string()));
        files.push(("app/Fitted.sharp", FITTED.to_string()));
        let database = shop_project(&files);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Paced.sharp")),
            [
                "app/Module.sharp",
                "app/Paced.sharp",
                "composer.lock",
                "lib/Rate.php",
                "lib/aliases.php",
                "lib/rapid.php"
            ]
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/Fitted.sharp")),
            [
                "app/Fitted.sharp",
                "app/Module.sharp",
                "composer.lock",
                "lib/Rate.php",
                "lib/aliases.php",
                "lib/rapid.php"
            ]
        );
    }

    /// A file that names `App\Pace` by its bare name in its own namespace, `App`, with no import, in a call and in a
    /// parameter type, where `App\Pace` is an alias of `Lib\Rate`.
    #[test]
    fn a_file_that_names_an_alias_of_its_own_namespace_with_no_import_lists_the_file_of_the_alias() {
        let mut files = shop(MONEY, RATE);
        files.push(("lib/aliases.php", "<?php\n\nclass_alias(\\Lib\\Rate::class, 'App\\\\Pace');\n".to_string()));
        files.push((
            "app/Bare.sharp",
            "namespace App;\n\npublic class Bare\n{\n    public int percent()\n    {\n        return Pace.percent();\n    }\n}\n"
                .to_string(),
        ));
        files.push((
            "app/BareFit.sharp",
            "namespace App;\n\npublic class BareFit\n{\n    public int fits(Pace pace)\n    {\n        return 1;\n    }\n}\n"
                .to_string(),
        ));
        let database = shop_project(&files);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Bare.sharp")),
            ["app/Bare.sharp", "app/Module.sharp", "composer.lock", "lib/Rate.php", "lib/aliases.php"]
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/BareFit.sharp")),
            ["app/BareFit.sharp", "app/Module.sharp", "composer.lock", "lib/Rate.php", "lib/aliases.php"]
        );
    }

    /// A library's polyfill of a function the engine builds in, as `if (!function_exists('str_starts_with'))` declares,
    /// never runs beside the engine's own, so its file is no input of a file whose code calls the function, here through
    /// the standard library's `startsWith`. A library's polyfill of a function with no built-in of its name, as
    /// `shop_label`, declares the function, so its file is an input.
    #[test]
    fn a_polyfill_of_a_built_in_function_is_no_input_and_one_of_a_library_function_is() {
        let database = project(&[
            (
                "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/StringMembers.sharp",
                "namespace Sharp.Text;\n\npublic static class StringMembers\n{\n    extension(string text)\n    {\n        public bool startsWith(string prefix) => global.str_starts_with(text, prefix);\n    }\n}\n",
            ),
            (
                "app/Prefix.sharp",
                "namespace App;\n\nimport Lib;\n\npublic class Prefix\n{\n    public bool starts() => \"ab\".startsWith(\"a\");\n\n    public string label() => Lib.shop_label(\"a\");\n}\n",
            ),
            (
                "app/Externs.sharp",
                "namespace App;\n\nimport Lib;\n\nextern namespace Lib for shop_label;\n\nextern Lib.shop_label uses none;\n",
            ),
            (
                "lib/polyfills.php",
                "<?php\n\nif (!function_exists('str_starts_with')) {\n    function str_starts_with($haystack, $needle) {\n        return true;\n    }\n}\n",
            ),
            (
                "lib/helpers.php",
                "<?php\n\nif (!function_exists('shop_label')) {\n    function shop_label(string $name): string {\n        return $name;\n    }\n}\n",
            ),
        ]);

        let compiled = compile(&mut analyzed(&database), empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Prefix.sharp")),
            [
                "app/*.sharp",
                "app/Externs.sharp",
                "app/Module.sharp",
                "app/Prefix.sharp",
                "composer.lock",
                "lib/helpers.php"
            ]
        );
    }

    /// A library's polyfill of a function the engine builds in never stands for the function, since the codebase keeps
    /// the built-in, so an edit of the polyfill changes no caller's result: its body, its return type changed, or its
    /// return type dropped, which takes the polyfill's return from its body. `mago analyze --watch` analyzes the
    /// polyfill's file alone after each, and reports what a full analysis reports.
    #[test]
    fn an_edit_of_a_polyfill_of_a_built_in_function_analyzes_the_polyfill_alone() {
        let polyfill = |returned: &str, body: &str| {
            format!(
                "<?php\n\nif (!function_exists('str_starts_with')) {{\n    function str_starts_with(string $haystack, string $needle){returned}\n    {{\n        return {body};\n    }}\n}}\n"
            )
        };
        let caller =
            |name: &str| format!("<?php\n\nfunction {name}(): bool\n{{\n    return str_starts_with('ab', 'a');\n}}\n");
        let files = |returned: &str, body: &str| {
            project(&[
                ("lib/polyfills.php", polyfill(returned, body).as_str()),
                ("app/first.php", caller("first").as_str()),
                ("app/second.php", caller("second").as_str()),
            ])
        };
        let messages = |service: &IncrementalAnalysisService| {
            let mut messages: Vec<String> =
                service.collect_all_issues().iter().map(|issue| issue.message.clone()).collect();
            messages.sort();

            messages
        };

        let mut warm = analyzed(&files(": bool", "true"));
        for (returned, body) in [(": bool", "false"), (": int", "1"), ("", "false")] {
            let edited = files(returned, body);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");

            assert_eq!(
                warm.analyzed_files,
                HashSet::from_iter([FileId::new(b"lib/polyfills.php")]),
                "the polyfill `{returned}` returning `{body}`"
            );
            assert_eq!(messages(&warm), messages(&analyzed(&edited)));
        }
    }

    /// Spec section 22: a class's `use` names its header's traits and the `Defaults` trait of each interface with a
    /// default body, so the file of each is an input. A header trait that turns into an interface, or an interface
    /// that gains or loses its last default body, leaves the compiled class stale.
    #[test]
    fn a_class_lists_the_files_of_its_header_traits_and_of_its_interfaces_default_bodies_as_inputs() {
        let database = project(&[
            ("app/Page.sharp", "namespace App;\n\npublic class Page : HasSlug, HasDesign\n{\n}\n"),
            (
                "app/HasSlug.php",
                "<?php\n\nnamespace App;\n\ntrait HasSlug\n{\n    public function slug(): string\n    {\n        return 'slug';\n    }\n}\n",
            ),
            (
                "app/HasDesign.sharp",
                "namespace App;\n\npublic interface HasDesign\n{\n    string designKey() => \"design\";\n}\n",
            ),
        ]);

        let compiled = compile(&mut analyzed(&database), empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Page.sharp")),
            ["app/HasDesign.sharp", "app/HasSlug.php", "app/Module.sharp", "app/Page.sharp", "composer.lock"]
        );
    }

    #[test]
    fn each_input_is_stamped_once_however_many_files_name_it() {
        let database = shop_project(&shop(MONEY, RATE));
        let mut service = analyzed(&database);
        let stamped: std::sync::Mutex<std::collections::BTreeMap<Vec<u8>, usize>> = std::sync::Mutex::default();

        compile(&mut service, |path| {
            *stamped.lock().unwrap().entry(path.to_vec()).or_default() += 1;
            empty_stamp(path)
        });
        let stamped = stamped.into_inner().unwrap();

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

    const TEXT: &str = "namespace Sharp;\n\nextern global.ucfirst uses none;\n\npublic class Text\n{\n    public static string capitalized(string text) => global.ucfirst(text);\n}\n";
    const TITLE: &str = "namespace App;\n\nimport Sharp.Text;\n\npublic class Title\n{\n    public string of(string name)\n    {\n        return Text.capitalized(name);\n    }\n}\n";
    const TEXT_PATH: &str = "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text.sharp";

    #[test]
    fn a_caller_inlines_the_library_form_and_a_library_body_edit_changes_its_key() {
        let before = project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)]);
        let lowered = TEXT.replace("ucfirst", "lcfirst");
        let after = project(&[(TEXT_PATH, lowered.as_str()), ("app/Title.sharp", TITLE)]);

        let before = compile(&mut analyzed(&before), empty_stamp);
        let after = compile(&mut analyzed(&after), empty_stamp);

        let title = accepted(&before, "app/Title.sharp");
        assert!(title.windows(b"ucfirst".len()).any(|window| window == b"ucfirst"), "the form is inlined");
        accepted(&before, TEXT_PATH);
        assert_ne!(compiled_key(title), compiled_key(accepted(&after, "app/Title.sharp")));
    }

    /// Decision 13: the standard library's forms come from the library files `compile` lowers and accepts, so neither a
    /// library file it refuses for an error nor one the database holds as a package's, which it doesn't lower, gives
    /// one.
    #[test]
    fn the_library_forms_come_only_from_the_library_files_the_compile_accepts() {
        let words = "namespace Sharp;\n\npublic class Words\n{\n    public static string loud(string text) => global.ucfirst(text);\n\n    public int wrong() => \"one\";\n}\n";
        let quiet = "namespace Sharp;\n\npublic class Quiet\n{\n    public static string low(string text) => global.lcfirst(text);\n}\n";
        let mut database = project(&[
            (TEXT_PATH, TEXT),
            ("vendor/heyjordanparker/php-sharp-composer/library/Sharp/Words.sharp", words),
        ]);
        let name = "vendor/heyjordanparker/php-sharp-composer/library/Sharp/Quiet.sharp";
        let mut packaged = File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Vendored,
            Some(Path::new("/project").join(name)),
            Cow::Borrowed(quiet.as_bytes()),
        );
        packaged.is_standard_library = true;
        database.add(packaged);
        let mut service = analyzed(&database);

        let forms = service.standard_library_forms(|issues| issues).expect("the library lowers");
        let compiled = compile(&mut service, empty_stamp);

        let members: Vec<String> = forms
            .iter()
            .map(|(class, member, _)| {
                format!("{}::{}", String::from_utf8_lossy(class), String::from_utf8_lossy(member))
            })
            .collect();
        assert_eq!(members, ["Sharp\\Text::capitalized"]);
        assert!(matches!(
            compiled["vendor/heyjordanparker/php-sharp-composer/library/Sharp/Words.sharp"],
            Compilation::Refused(_)
        ));
        assert!(!compiled.contains_key(name));
    }

    /// A library file whose lowering panics fails the library's forms with the panic's text, where its forms would
    /// leave unseen. Its errors refuse nothing here, as the hosts' compile's don't, and a `Map` spread into a `List`
    /// literal is an error the bridge doesn't survive.
    #[test]
    fn a_library_file_whose_lowering_panics_fails_the_library_forms_with_the_panic() {
        let menu = "namespace Sharp;\n\npublic class Menu\n{\n    public static List<int|string> names(Map<string, int> counts)\n    {\n        return [...counts, \"soup\"];\n    }\n}\n";
        let database =
            project(&[(TEXT_PATH, TEXT), ("vendor/heyjordanparker/php-sharp-composer/library/Sharp/Menu.sharp", menu)]);
        let mut service = analyzed(&database);

        let Err(OrchestratorError::General(message)) = service.standard_library_forms(|_| IssueCollection::default())
        else {
            panic!("the library's forms ignore the panic");
        };

        assert_eq!(
            message,
            "The lowering of vendor/heyjordanparker/php-sharp-composer/library/Sharp/Menu.sharp panicked on the checker's errors in it: internal error: entered unreachable code: the checker refuses a `List` element in a `Map` literal"
        );
    }

    /// A function several polyfill files declare, each its own way, is read as the declaration the codebase keeps,
    /// never as the one its map of files happens to list last. So every compile of one tree gives the PHP# file that
    /// calls it one key, whichever order the process walks the files in.
    #[test]
    fn a_function_several_files_declare_is_read_in_its_caller_key_as_the_declaration_the_codebase_keeps() {
        let text = TEXT.replace("ucfirst", "shout");
        let polyfills: Vec<(String, String)> = ["string", "?string", "int|string", "float|string", "bool|string"]
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                (
                    format!("vendor/acme/polyfill/bootstrap{index}.php"),
                    format!("<?php\n\nif (!\\function_exists('shout')) {{\n    function shout({parameter} $text): string\n    {{\n        return (string) $text;\n    }}\n}}\n"),
                )
            })
            .collect();
        let key = |polyfills: &[(String, String)]| {
            let mut files: Vec<(&str, &str)> =
                polyfills.iter().map(|(name, code)| (name.as_str(), code.as_str())).collect();
            files.push((TEXT_PATH, text.as_str()));
            let mut service = analyzed(&project(&files));
            let kept =
                service.codebase.get_function(word("shout")).expect("the codebase keeps one `shout`").span.file_id;

            (kept, compiled_key(accepted(&compile(&mut service, empty_stamp), TEXT_PATH)))
        };

        let (kept, every) = key(&polyfills);
        let kept = polyfills.iter().find(|(name, _)| FileId::new(name.as_bytes()) == kept).expect("a polyfill is kept");
        let (_, alone) = key(std::slice::from_ref(kept));

        assert_eq!(every, alone, "the caller reads `shout` as {}", kept.0);
    }

    /// Spec section 21: a lambda's function type lists the effects of each body it calls, in any file, so the file
    /// of each body a call reaches is an input, along every chain of calls. An edit two calls away that gives or takes
    /// an effect changes the lambda's type text and its key on a warm compile, as on a clean one. An edit that keeps
    /// the effects keeps the key.
    #[test]
    fn a_body_edit_two_calls_away_that_changes_its_effects_changes_the_callers_function_type() {
        let meter = "namespace App;\n\npublic effect abstract class Meter\n{\n    public abstract int now();\n}\n";
        let clerk = "namespace App;\n\npublic class Clerk\n{\n    public int count(Meter meter) => new Ledger().tally(meter);\n}\n";
        let ledger = |tally: &str| {
            format!("namespace App;\n\npublic class Ledger\n{{\n    public int tally(Meter meter) => {tally};\n}}\n")
        };
        // Desk names neither `Ledger` nor its method, so only the effects reach its file from there.
        let desk = "namespace App;\n\npublic class Desk\n{\n    public Any? counter(Clerk clerk, Meter meter) => () => clerk.count(meter);\n}\n";
        let files = |tally: &str| {
            let ledger = ledger(tally);

            project(&[
                ("app/Meter.sharp", meter),
                ("app/Clerk.sharp", clerk),
                ("app/Ledger.sharp", ledger.as_str()),
                ("app/Desk.sharp", desk),
            ])
        };
        let spells = |bytes: &[u8], text: &[u8]| bytes.windows(text.len()).any(|window| window == text);

        let (pure, kept, metered) = (files("1"), files("2"), files("meter.now()"));
        for (before, after, text) in
            [(&pure, &metered, b"Function<int() uses App.Meter>".as_slice()), (&metered, &pure, b"Function<int()>")]
        {
            let mut warm = analyzed(before);
            let before_compiled = compile(&mut warm, empty_stamp);
            warm.update_database(after.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let warm_compiled = compile(&mut warm, empty_stamp);
            let clean_compiled = compile(&mut analyzed(after), empty_stamp);

            let desk = accepted(&warm_compiled, "app/Desk.sharp");
            assert!(input_paths(desk).contains(&"app/Ledger.sharp".to_string()), "{:?}", input_paths(desk));
            assert!(spells(desk, text), "{}", String::from_utf8_lossy(text));
            assert_ne!(compiled_key(accepted(&before_compiled, "app/Desk.sharp")), compiled_key(desk));
            assert_eq!(bodies(&warm_compiled), bodies(&clean_compiled));
        }

        let pure = compile(&mut analyzed(&pure), empty_stamp);
        let kept = compile(&mut analyzed(&kept), empty_stamp);
        assert_eq!(
            compiled_key(accepted(&pure, "app/Desk.sharp")),
            compiled_key(accepted(&kept, "app/Desk.sharp")),
            "an edit that keeps the effects keeps the key"
        );
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

    /// Each edit compiles the same after the warm path as after a full analysis, in a one-off compile and under
    /// `mago analyze --watch`, which analyzes with `diff`. A body edit leaves every other class unanalyzed, and the
    /// compile still types each of their expressions.
    #[test]
    fn a_compile_after_each_edit_matches_a_clean_compile() {
        let base = shop_project(&shop(MONEY, RATE));

        for diff in [false, true] {
            for edit in edits() {
                let mut files = shop(&edit.money, &edit.rate);
                if edit.deleted {
                    files.retain(|(name, _)| *name != edit.edited);
                }
                let edited = shop_project(&files);

                let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm_compiled = compile(&mut warm, empty_stamp);
                let clean_compiled = compile(&mut analyzed(&edited), empty_stamp);

                assert_eq!(bodies(&warm_compiled), bodies(&clean_compiled), "diff {diff}: {}", edit.name);
            }
        }
    }

    /// A compile after a full analysis that kept what it gave the lowering of each PHP# file lowers each file from it,
    /// analyzing none again, and writes the bytes and refuses the files a compile that analyzes each file again does.
    #[test]
    fn a_compile_lowers_each_file_from_the_analysis_before_it_as_analyzing_the_file_again_lowers_it() {
        let generated = generated_project(40, 11);
        let fixtures = [
            shop_project(&shop(MONEY, RATE)),
            shop_project(&headers()),
            shop_project(&bounds()),
            shop_project(&constants()),
            project(&[(TEXT_PATH, TEXT), ("app/Title.sharp", TITLE)]),
            project(&generated.iter().map(|(name, contents)| (name.as_str(), contents.as_str())).collect::<Vec<_>>()),
        ];

        for database in &fixtures {
            let mut from_analysis = analyzed_for_lowering(database);
            let mut analyzing_again = analyzed(database);
            let lowered = compile(&mut from_analysis, empty_stamp);
            let lowered_again = compile(&mut analyzing_again, empty_stamp);

            assert_eq!(whole(&lowered), whole(&lowered_again));
            assert_eq!(analyzed_to_lower(&from_analysis), BTreeSet::new());
            assert_eq!(
                analyzed_to_lower(&analyzing_again),
                lowered
                    .iter()
                    .filter(|(_, compilation)| matches!(compilation, Compilation::Accepted(_)))
                    .map(|(name, _)| name.clone())
                    .collect::<BTreeSet<_>>()
            );
        }
    }

    /// An incremental analysis keeps nothing for the lowering, since it skips the methods it keeps, so the compile
    /// after it analyzes each file it lowers again, and writes what a clean compile writes.
    #[test]
    fn a_compile_after_an_incremental_analysis_analyzes_each_file_it_lowers_again() {
        let edited = shop_project(&shop(&MONEY.replace("return 1;", "return 2;"), RATE));
        let mut warm = analyzed_for_lowering(&shop_project(&shop(MONEY, RATE)));
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");

        let warm_compiled = compile(&mut warm, empty_stamp);

        assert_eq!(
            analyzed_to_lower(&warm),
            BTreeSet::from(
                [
                    "app/Cash.sharp",
                    "app/Money.sharp",
                    "app/Order.sharp",
                    "app/Rates.sharp",
                    "app/Stubs/Shop.sharp",
                    "app/Unrelated.sharp",
                ]
                .map(str::to_owned)
            )
        );
        assert_eq!(whole(&warm_compiled), whole(&compile(&mut analyzed_for_lowering(&edited), empty_stamp)));
    }

    /// The compiled files and the [`CompiledFiles`] of a compile of `service` that keeps each file `last` and
    /// `previous`, the compiled files of the last compile by name, hold whose key holds, and the name of each file whose
    /// compiled file it read. `intact` tells it which compiled files are the ones `previous` holds.
    fn compile_keeping(
        service: &mut IncrementalAnalysisService,
        last: &CompiledFiles,
        previous: &BTreeMap<String, Compilation>,
        stamp: impl Fn(&[u8]) -> Result<Option<Input>, DatabaseError> + Sync,
        mut intact: impl FnMut(&File) -> bool,
    ) -> (BTreeMap<String, Compilation>, CompiledFiles, BTreeSet<String>) {
        let asked = std::sync::Mutex::new(BTreeSet::new());
        let compiled = |file: &File| {
            let name = String::from_utf8_lossy(&file.name).into_owned();
            let bytes = match previous.get(&name) {
                Some(Compilation::Accepted(bytes)) => Some(bytes.clone()),
                _ => None,
            };
            asked.lock().unwrap().insert(name);
            Ok(bytes)
        };
        let folder = Folder { last, compiled: &compiled, intact: &mut intact, write: &unwritten };
        let ((compiled, kept), _) = service
            .compile(folder, None, stamp, proved, CHECKER, &Library::default(), |issues| issues)
            .expect("the compile runs");
        let database = service.database();
        let compiled = compiled
            .into_iter()
            .map(|(file_id, compilation)| {
                (
                    String::from_utf8_lossy(&database.get(&file_id).expect("a compiled file exists").name).into_owned(),
                    compilation,
                )
            })
            .collect();

        (compiled, kept, asked.into_inner().unwrap())
    }

    /// Each compiled file's bytes, and each refused file's messages.
    fn whole(compiled: &BTreeMap<String, Compilation>) -> BTreeMap<&str, Result<&[u8], Vec<String>>> {
        compiled
            .iter()
            .map(|(name, compilation)| {
                let outcome = match compilation {
                    Compilation::Accepted(bytes) => Ok(bytes.as_slice()),
                    Compilation::Refused(issues) => Err(messages(issues)),
                    Compilation::Kept => panic!("{name} was kept unread"),
                };
                (name.as_str(), outcome)
            })
            .collect()
    }

    /// After each edit, a compile that keeps the compiled file of each file whose key holds writes the bytes a clean
    /// compile of the edited project writes, and keeps exactly the files whose key the edit left as it was, so the
    /// edited file is lowered again and a file the edit doesn't reach is not.
    #[test]
    fn a_compile_that_keeps_each_file_whose_key_holds_matches_a_clean_compile_after_each_edit() {
        let base = shop_project(&shop(MONEY, RATE));

        for edit in edits().into_iter().filter(|edit| !edit.deleted) {
            let edited = shop_project(&shop(&edit.money, &edit.rate));
            let mut warm = analyzed(&base);
            let (before, last, _) =
                compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), empty_stamp, |_| false);
            let last =
                CompiledFiles::decode(&last.encode(CHECKER, [0; 16]).expect("the record encodes"), CHECKER, [0; 16]);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");

            let (kept_compiled, _, kept) = compile_keeping(&mut warm, &last, &before, empty_stamp, |_| false);
            let clean = compile(&mut analyzed(&edited), empty_stamp);

            assert_eq!(whole(&kept_compiled), whole(&clean), "{}", edit.name);
            let unchanged: BTreeSet<String> = before
                .iter()
                .filter(|(name, compilation)| match (compilation, clean.get(*name)) {
                    (Compilation::Accepted(before), Some(Compilation::Accepted(after))) => {
                        compiled_key(before) == compiled_key(after)
                    }
                    _ => false,
                })
                .map(|(name, _)| name.clone())
                .collect();
            assert_eq!(kept, unchanged, "{}", edit.name);
            assert!(!kept.contains(edit.edited), "{}", edit.name);
            assert!(kept.contains("app/Unrelated.sharp"), "{}", edit.name);
        }
    }

    /// The lowering reads the configuration, which no key holds, so the record of another configuration's compile
    /// keeps no compiled file, and neither does the record another checker build wrote.
    #[test]
    fn the_record_of_the_compiled_files_keeps_none_under_another_configuration_or_checker_build() {
        let (_, last, _) = compile_keeping(
            &mut analyzed(&shop_project(&shop(MONEY, RATE))),
            &CompiledFiles::default(),
            &BTreeMap::new(),
            empty_stamp,
            |_| false,
        );
        let bytes = last.encode(CHECKER, [0; 16]).expect("the record encodes");

        assert_eq!(CompiledFiles::decode(&bytes, CHECKER, [0; 16]).files.len(), last.files.len());
        assert!(!last.files.is_empty());
        assert!(CompiledFiles::decode(&bytes, CHECKER, [1; 16]).files.is_empty());
        assert!(CompiledFiles::decode(&bytes, [1; 16], [0; 16]).files.is_empty());
    }

    /// A vendor file is no input of a compiled file, which `composer.lock` stands for, but a declaration a file reads
    /// from it is in the file's key, so a compile after a vendor edit lowers again each file that reads what changed.
    #[test]
    fn a_compile_after_a_vendor_edit_lowers_again_each_file_that_reads_the_edited_declaration() {
        let base = shop_project(&shop(MONEY, RATE));
        let mut files = shop(MONEY, RATE);
        let (_, tax) =
            files.iter_mut().find(|(name, _)| *name == "vendor/acme/tax/src/Tax.php").expect("the shop has Tax");
        *tax = TAX.replace("rate(): int", "rate(): float").replace("return 2;", "return 2.5;");
        let edited = shop_project(&files);
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), empty_stamp, |_| false);
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");

        let (after, _, kept) = compile_keeping(&mut warm, &last, &before, empty_stamp, |_| false);

        assert!(!kept.contains("app/Order.sharp"), "{kept:?}");
        assert!(kept.contains("app/Unrelated.sharp"), "{kept:?}");
        assert_eq!(whole(&after), whole(&compile(&mut analyzed(&edited), empty_stamp)));
    }

    /// An edit of a plain PHP file a PHP# file never reads, as a test of another part of the app, is no input of its
    /// compiled file, for the engine's check and for the next compile, which keeps the file without lowering it.
    #[test]
    fn an_edit_of_a_plain_php_file_a_file_never_reads_leaves_its_compiled_file_current() {
        let test = |members: &str| {
            format!("<?php\n\nnamespace Tests\\Api;\n\nfinal class LinkVisibilityTest\n{{\n{members}}}\n")
        };
        let files = |test: &str| project(&[("app/Order.sharp", ORDER), ("tests/api/LinkVisibilityTest.php", test)]);
        let base = files(&test("    public function test(): bool\n    {\n        return true;\n    }\n"));
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), database_stamp(&base), |_| false);
        let inputs = input_paths(accepted(&before, "app/Order.sharp"));
        assert!(!inputs.iter().any(|input| input.starts_with("tests/")), "{inputs:?}");

        let edited = files(&test(
            "    public function test(): bool\n    {\n        return false;\n    }\n\n    public function other(): int\n    {\n        return 2;\n    }\n",
        ));
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let (after, _, kept) = compile_keeping(&mut warm, &last, &before, database_stamp(&edited), |_| false);

        assert_eq!(kept, BTreeSet::from(["app/Order.sharp".to_owned()]));
        assert_eq!(accepted(&after, "app/Order.sharp"), accepted(&before, "app/Order.sharp"));
    }

    /// A compile whose caller vouches that each compiled file is the one the last compile wrote keeps a file whose key
    /// holds and whose inputs kept their stamps without reading it, and reads only a kept file an input of which changed,
    /// to restamp it. Each file then holds the bytes a clean compile of the edited project writes.
    #[test]
    fn a_compile_keeps_unread_each_intact_file_whose_inputs_kept_their_stamps() {
        let base = shop_project(&shop(MONEY, RATE));
        let edited = shop_project(&shop(MONEY, &RATE.replace("return 5;", "return 6;")));
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), database_stamp(&base), |_| false);
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");

        let (after, _, read) = compile_keeping(&mut warm, &last, &before, database_stamp(&edited), |_| true);

        assert!(matches!(after.get("app/Unrelated.sharp"), Some(Compilation::Kept)), "{after:?}");
        assert!(!read.contains("app/Unrelated.sharp"), "{read:?}");
        assert!(read.contains("app/Order.sharp"), "Order lists the edited plain PHP Rate as an input: {read:?}");
        let clean = compile(&mut analyzed(&edited), database_stamp(&edited));
        let resolved: BTreeMap<String, Compilation> = after
            .into_iter()
            .map(|(name, compilation)| match compilation {
                Compilation::Kept => {
                    let bytes = accepted(&before, &name).to_vec();
                    (name, Compilation::Accepted(bytes))
                }
                compilation => (name, compilation),
            })
            .collect();
        assert_eq!(whole(&engine_view(&resolved)), whole(&engine_view(&clean)));
    }

    /// Spec section 27: a body edit to a file leaves unread and unwritten each compiled file that reads its declarations
    /// or calls its bodies, which names the hash of those declarations, and a signature edit rewrites it with the new
    /// hash.
    #[test]
    fn a_body_edit_keeps_unread_each_file_that_reads_the_edited_file() {
        let base = shop_project(&shop(MONEY, RATE));
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), database_stamp(&base), |_| false);
        let money = |compiled: &BTreeMap<String, Compilation>, name: &str| {
            inputs(accepted(compiled, name)).into_iter().find(|input| input.path == b"app/Money.sharp")
        };
        let money_declarations = header(accepted(&before, "app/Money.sharp")).unwrap().declarations_hash;
        assert_ne!(money_declarations, [0; 16]);
        assert_eq!(money(&before, "app/Rates.sharp").map(|input| input.declarations_hash), Some(money_declarations));
        assert_eq!(money(&before, "app/Order.sharp").map(|input| input.declarations_hash), Some(money_declarations));

        let edited = shop_project(&shop(&MONEY.replace("return 1;", "return 2;"), RATE));
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let (after, last, read) = compile_keeping(&mut warm, &last, &before, database_stamp(&edited), |_| true);

        assert_eq!(header(accepted(&after, "app/Money.sharp")).unwrap().declarations_hash, money_declarations);
        for name in ["app/Order.sharp", "app/Rates.sharp", "app/Cash.sharp", "app/Unrelated.sharp"] {
            assert!(matches!(after.get(name), Some(Compilation::Kept)), "{name}: {after:?}");
            assert!(!read.contains(name), "{name}: {read:?}");
        }

        let signature = shop_project(&shop(&MONEY.replace("int amount()", "int amount(int scale = 1)"), RATE));
        warm.update_database(signature.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let (changed, _, _) = compile_keeping(&mut warm, &last, &before, database_stamp(&signature), |_| true);

        let changed_declarations = header(accepted(&changed, "app/Money.sharp")).unwrap().declarations_hash;
        assert_ne!(changed_declarations, money_declarations);
        assert_eq!(money(&changed, "app/Rates.sharp").map(|input| input.declarations_hash), Some(changed_declarations));
    }

    /// The project the input and key tests edit, with `module` as the module of `App`, the namespace of every
    /// `.sharp` file in it.
    fn shop_with_module(module: &str) -> Vec<(&'static str, String)> {
        let mut files = shop(MONEY, RATE);
        files.push(("app/Module.sharp", format!("namespace App;\n\nmodule\n{{\n{module}}}\n")));
        files
    }

    /// The compilations of a compile of `edited` after a compile of `base`, and the files whose compiled files it read.
    fn compiled_after_edit(
        base: &[(&'static str, String)],
        edited: &[(&'static str, String)],
    ) -> (BTreeMap<String, Compilation>, BTreeMap<String, Compilation>, BTreeSet<String>) {
        let base = shop_project(base);
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), database_stamp(&base), |_| false);
        let edited = shop_project(edited);
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let (after, _, read) = compile_keeping(&mut warm, &last, &before, database_stamp(&edited), |_| true);

        (before, after, read)
    }

    /// Spec sections 27 and 32: a file reads the modules of its namespaces only through their `imports` lines and
    /// bindings, so a newline appended to `app/Module.sharp` compiles the module again and leaves every other compiled
    /// file unread and unwritten.
    #[test]
    fn a_newline_in_a_module_rewrites_only_that_module_s_compiled_file() {
        let module = "    singleton Money = Money;\n";
        let (_, after, read) =
            compiled_after_edit(&shop_with_module(module), &shop_with_module(&format!("{module}\n")));

        assert!(matches!(after.get("app/Module.sharp"), Some(Compilation::Accepted(_))), "{after:?}");
        let sharp: Vec<&String> = after
            .keys()
            .filter(|name| Path::new(name).extension().is_some_and(|extension| extension == "sharp"))
            .collect();
        assert!(sharp.len() > 5, "{sharp:?}");
        for name in sharp.into_iter().filter(|name| *name != "app/Module.sharp") {
            assert!(matches!(after.get(name), Some(Compilation::Kept)), "{name}: {after:?}");
            assert!(!read.contains(name), "{name}: {read:?}");
        }
    }

    /// Spec section 32: a module's `imports` line limits what each file of its namespace imports, so one added to
    /// `app/Module.sharp` makes each compiled file below it stale, and each names the module's new declarations.
    #[test]
    fn an_import_added_to_a_module_makes_each_file_below_it_stale() {
        let module = "    singleton Money = Money;\n";
        let (before, after, _) = compiled_after_edit(
            &shop_with_module(module),
            &shop_with_module(&format!("    imports Acme, Lib;\n{module}")),
        );

        let declarations = header(accepted(&after, "app/Module.sharp")).unwrap().declarations_hash;
        assert_ne!(declarations, header(accepted(&before, "app/Module.sharp")).unwrap().declarations_hash);
        for name in ["app/Money.sharp", "app/Order.sharp", "app/Cash.sharp", "app/Rates.sharp", "app/Unrelated.sharp"] {
            let module = inputs(accepted(&after, name)).into_iter().find(|input| input.path == b"app/Module.sharp");
            assert_eq!(module.map(|input| input.declarations_hash), Some(declarations), "{name}");
        }
    }

    /// Each compiled file of `compiled` as the engine decides whether to run it: an input that names the hash of an
    /// import's declarations counts by that hash, whatever stamp it holds, so a file the compile keeps unread after a
    /// body edit to such an import compares equal to the file a clean compile writes.
    fn engine_view(compiled: &BTreeMap<String, Compilation>) -> BTreeMap<String, Compilation> {
        compiled
            .iter()
            .map(|(name, compilation)| match compilation {
                Compilation::Accepted(bytes) => {
                    let header = header(bytes).expect("the header is valid");
                    let inputs: Vec<Input> = inputs(bytes)
                        .into_iter()
                        .map(|input| {
                            if input.declarations_hash == [0; 16] {
                                input
                            } else {
                                Input { size: 0, mtime_ns: 0, hash: [0; 16], ..input }
                            }
                        })
                        .collect();
                    let bytes =
                        unit::restamp(bytes, header.declarations_hash, &inputs, header.checker, &header.library)
                            .expect("a compiled file restamps");
                    (name.clone(), Compilation::Accepted(bytes))
                }
                Compilation::Refused(issues) => (name.clone(), Compilation::Refused(issues.clone())),
                Compilation::Kept => (name.clone(), Compilation::Kept),
            })
            .collect()
    }

    fn declarations(codebase: &CodebaseMetadata, signature: &FileSignature) -> Vec<SymbolIdentifier> {
        signature
            .ast_nodes
            .iter()
            .flat_map(|node| {
                std::iter::once((node.name, empty_word())).chain(
                    node.children.iter().map(|child| (node.name, super::super::member_key(codebase, node.name, child))),
                )
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
        let every_declaration: Vec<SymbolIdentifier> = service
            .codebase
            .file_signatures
            .values()
            .flat_map(|signature| declarations(&service.codebase, signature))
            .collect();
        let files: Vec<FileId> =
            database.files().filter(|file| file.file_type != FileType::Builtin).map(|file| file.id).collect();

        let mut reached_by: BTreeMap<String, BTreeSet<String>> =
            files.iter().map(|file_id| (name(file_id), BTreeSet::new())).collect();
        for edited in database.files().filter(|file| is_source(file)) {
            let Some(signature) = service.codebase.get_file_signature(&edited.id) else {
                continue;
            };
            let diff = CodebaseDiff::new()
                .with_keep(every_declaration.iter().copied())
                .with_changed(declarations(&service.codebase, signature));
            service.codebase.safe_symbols.clear();
            service.codebase.safe_symbol_members.clear();
            let unchanged: Vec<FileId> = files.iter().copied().filter(|file_id| *file_id != edited.id).collect();
            let extension_reads = super::super::extension_reads(&service.file_states, &service.before_analysis_reads);
            let (mut invalid_files, extension_symbol_changed) = service
                .codebase
                .mark_safe_symbols(&diff, &service.native_symbol_references, &extension_reads.symbols)
                .expect("each fixture's cascade ends within the warm path's limit");
            if extension_symbol_changed {
                invalid_files.extend(unchanged.iter().map(|file_id| mago_word::word(name(file_id).as_bytes())));
            }

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

    /// Once the laws are translated, a compile keeps what the analysis gave the lowering only of each file a law reaches,
    /// which [`IncrementalAnalysisService::law_refusals`] reads after it, so that analyzes none again.
    #[test]
    fn a_compile_keeps_the_lowering_inputs_of_the_files_a_law_reaches_alone() {
        let database = lawful_project();
        let mut service = analyzed_for_lowering(&database);

        compile(&mut service, empty_stamp);
        let kept: BTreeSet<String> = service
            .file_states
            .iter()
            .filter(|(_, state)| state.lowering_input.is_some())
            .map(|(file_id, _)| {
                String::from_utf8_lossy(&service.database().get(file_id).expect("the file exists").name).into_owned()
            })
            .collect();
        service.law_refusals().expect("the laws are translated");

        assert_eq!(
            kept,
            BTreeSet::from(["app/Shared/Digits.sharp", "app/Shared/Money.sharp", "app/Shared/Rounding.sharp"])
                .into_iter()
                .map(String::from)
                .collect()
        );
        assert_eq!(analyzed_to_lower(&service), BTreeSet::new());
    }

    /// Spec section 28: a law is proven from its `.sharp` file alone, so its file's inputs are the sources its laws
    /// reach, and no Lean file beside it.
    #[test]
    fn a_law_file_lists_every_source_its_law_reaches_as_inputs() {
        let database = lawful_project();
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        let money = input_paths(accepted(&compiled, "app/Shared/Money.sharp"));
        for input in ["app/Shared/Rounding.sharp", "app/Shared/Digits.sharp"] {
            assert!(money.iter().any(|path| path == input), "{input} is missing from {money:?}");
        }
        assert!(!money.iter().any(|path| path == "app/Shared/Money.lean"), "{money:?}");
    }

    /// A law of `Shelf` reaches `Bag`, whose header names `Iterable<string>`, which names no class, so the translation
    /// of `Bag` reads the classes of its header as the checker does and both files compile.
    #[test]
    fn a_class_a_law_reaches_compiles_with_iterable_in_its_header() {
        let database = project(&[
            (
                "app/Bag.sharp",
                "namespace App;\n\npublic class Bag : Iterable<string>, Countable\n{\n    public Iterable<string> elements() => [\"tea\"];\n\n    public int count() => 1;\n}\n",
            ),
            (
                "app/Shelf.sharp",
                "namespace App;\n\npublic class Shelf\n{\n    law holdsOne(Bag bag) => bag.count() == 1;\n}\n",
            ),
        ]);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        accepted(&compiled, "app/Bag.sharp");
        accepted(&compiled, "app/Shelf.sharp");
    }

    /// `Row`'s base class is missing, so the analysis finds no `key` on it, and the call of it in `Reader` is the error
    /// that refuses `Reader`, never a lowering that panics.
    #[test]
    fn a_call_of_a_member_of_a_class_whose_base_class_is_missing_refuses_its_file() {
        let database = project(&[
            ("app/Row.sharp", "namespace App;\n\npublic class Row : Missing\n{\n}\n"),
            (
                "app/Reader.sharp",
                "namespace App;\n\npublic class Reader\n{\n    public Any? key(Row row) => row.key();\n}\n",
            ),
        ]);
        let mut service = analyzed(&database);

        let compiled = compile(&mut service, empty_stamp);

        let Some(Compilation::Refused(errors)) = compiled.get("app/Reader.sharp") else {
            panic!("app/Reader.sharp is not refused: {compiled:?}");
        };
        let codes: Vec<Option<&str>> = errors.iter().map(|issue| issue.code.as_deref()).collect();
        assert_eq!(codes, [Some("non-existent-method")], "{errors:?}");
    }

    /// A law reaches only what the members it reaches run. An attribute's constructor never runs as part of them, so
    /// the law of `Level`, whose class and whose `meets` carry `[Tag]`, reaches neither `Tag` nor the `Bag` it holds,
    /// and its translation names no declaration of their files. `Shelf` and its `count` carry `[Tag]` too, and `count`
    /// builds a `Tag`, so the law of `Shelf` reaches both files.
    #[test]
    fn a_law_reaches_no_attribute_of_what_it_reaches_and_every_class_its_code_builds() {
        let database = project(&[
            (
                "app/Level.sharp",
                "namespace App;\n\n[Tag]\npublic enum Level\n{\n    case View;\n    case Edit;\n\n    [Tag]\n    public bool meets(Level other) => this == other;\n\n    law meetsItself(Level a) => a.meets(a);\n}\n",
            ),
            (
                "app/Shelf.sharp",
                "namespace App;\n\n[Tag]\npublic class Shelf\n{\n    [Tag]\n    public int count()\n    {\n        const made = new Tag();\n        return 1;\n    }\n\n    law countsOne(Shelf shelf) => shelf.count() == 1;\n}\n",
            ),
            (
                "app/Tag.sharp",
                "namespace App;\n\n[Attribute(Attribute.TARGET_CLASS | Attribute.TARGET_METHOD)]\npublic class Tag\n{\n    public Bag bag { get; } = new Bag();\n}\n",
            ),
            ("app/Bag.sharp", "namespace App;\n\npublic class Bag\n{\n    public int size() => 1;\n}\n"),
        ]);
        let service = analyzed(&database);

        let reach = Reach::new(&service.codebase, &service.native_symbol_references);

        let reached =
            |law: &str, file: &str| reach.files(FileId::new(law.as_bytes())).contains(&FileId::new(file.as_bytes()));
        assert!(reached("app/Level.sharp", "app/Level.sharp"));
        assert!(!reached("app/Level.sharp", "app/Tag.sharp"));
        assert!(!reached("app/Level.sharp", "app/Bag.sharp"));
        assert!(reached("app/Shelf.sharp", "app/Tag.sharp"));
        assert!(reached("app/Shelf.sharp", "app/Bag.sharp"));
    }

    /// Spec section 32: a file whose check resolves services through modules names the module files of every namespace
    /// it resolves through as inputs, with no file at most of them, so the engine refuses it after one is edited,
    /// added or deleted. `app/Shop/Module.sharp` binds `Panel` of `App.Shop.Admin`, whose `Mailer` comes from
    /// `app/Shop/Admin/Module.sharp`.
    #[test]
    fn a_module_test_module_or_listener_file_names_the_module_files_it_resolves_services_through() {
        let database = project(&[
            ("app/Mail/Mailer.sharp", "namespace App.Mail;\n\npublic interface Mailer\n{\n}\n"),
            ("app/Mail/SmtpMailer.sharp", "namespace App.Mail;\n\npublic class SmtpMailer : Mailer\n{\n}\n"),
            (
                "app/Shop/Admin/Panel.sharp",
                "namespace App.Shop.Admin;\n\nimport App.Mail.Mailer;\n\npublic class Panel\n{\n    public Panel(private Mailer mailer) { }\n}\n",
            ),
            (
                "app/Shop/Admin/Module.sharp",
                "namespace App.Shop.Admin;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\n\nmodule\n{\n    singleton Mailer = SmtpMailer;\n}\n",
            ),
            (
                "app/Shop/Module.sharp",
                "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\nimport App.Shop.Admin.Panel;\n\nmodule\n{\n    scoped Mailer = SmtpMailer;\n    singleton Panel = Panel;\n}\n",
            ),
            (
                "app/Shop/Module.Production.sharp",
                "namespace App.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\n\nmodule : App.Shop\n{\n    singleton Mailer = SmtpMailer;\n}\n",
            ),
            (
                "vendor/phpunit/phpunit/src/Framework/TestCase.php",
                "<?php\n\nnamespace PHPUnit\\Framework;\n\nabstract class TestCase\n{\n}\n",
            ),
            (
                "tests/Shop/PanelTest.sharp",
                "namespace Tests.Shop;\n\nimport App.Mail.Mailer;\nimport App.Mail.SmtpMailer;\nimport PHPUnit.Framework.TestCase;\n\nmodule : App.Shop\n{\n    transient Mailer = SmtpMailer;\n}\n\npublic class PanelTest : TestCase\n{\n}\n",
            ),
            ("app/Shop/OrderPaid.sharp", "namespace App.Shop;\n\npublic event OrderPaid(int orderId);\n"),
            (
                "app/Shop/Tick.sharp",
                "namespace App.Shop;\n\nimport App.Mail.Mailer;\n\npublic class Tick\n{\n    public Tick(private Mailer mailer) { }\n\n    public void count() on OrderPaid { }\n}\n",
            ),
        ]);

        let compiled = compile(&mut analyzed(&database), database_stamp(&database));
        // Each module file the inputs of `name` name, and whether a file is at it, as its stamp says.
        let modules = |name: &str| -> Vec<(String, bool)> {
            inputs(accepted(&compiled, name))
                .into_iter()
                .filter(|input| input.path.ends_with(b".sharp"))
                .filter(|input| input.path.starts_with(b"Module.") || input.path.windows(8).any(|w| w == b"/Module."))
                .map(|input| (String::from_utf8_lossy(&input.path).into_owned(), input.hash != [0; 16]))
                .collect()
        };
        let names_each = |name: &str, paths: &[&str]| {
            let named = modules(name);
            for path in paths {
                let present = database.read_only().get(&FileId::new(path.as_bytes())).is_ok();
                assert!(named.contains(&((*path).to_owned(), present)), "{name} misses {path}: {named:?}");
            }
        };

        // A module names its folder's modules and those of each class its bindings build: `app/Mail/` for
        // `SmtpMailer` and `app/Shop/Admin/` for `Panel`.
        let base = ["app/Mail/Module.sharp", "app/Module.sharp", "app/Shop/Admin/Module.sharp"];
        names_each("app/Shop/Module.sharp", &[&base[..], &["app/Shop/Module.sharp"]].concat());
        assert!(
            !modules("app/Shop/Module.sharp").iter().any(|(path, _)| path == "app/Module.Production.sharp"),
            "a base module reads no environment's modules"
        );
        // An environment module also names its environment's module of each of those folders.
        names_each(
            "app/Shop/Module.Production.sharp",
            &[
                &base[..],
                &[
                    "app/Shop/Module.sharp",
                    "app/Mail/Module.Production.sharp",
                    "app/Module.Production.sharp",
                    "app/Shop/Admin/Module.Production.sharp",
                    "app/Shop/Module.Production.sharp",
                ],
            ]
            .concat(),
        );
        // A test module names its own folders' modules and those of the namespace it overrides.
        names_each(
            "tests/Shop/PanelTest.sharp",
            &[&base[..], &["app/Shop/Module.sharp", "tests/Module.sharp", "tests/Shop/Module.sharp"]].concat(),
        );
        // A listener names the modules its constructor's services come from.
        names_each("app/Shop/Tick.sharp", &["app/Module.sharp", "app/Shop/Module.sharp"]);
        // A bound class's own check reads only the modules of its own folders, whose `imports` lines limit its imports.
        assert_eq!(
            modules("app/Shop/Admin/Panel.sharp"),
            [
                ("app/Module.sharp".to_owned(), false),
                ("app/Shop/Admin/Module.sharp".to_owned(), true),
                ("app/Shop/Module.sharp".to_owned(), true),
            ]
        );
    }

    /// A project whose program `tests/lib/Link.sharp` declares `App.Testing.Lib` in a test folder, and takes the
    /// `Runtime` `app/Module.sharp` binds, with `module` as that module's bindings.
    fn testing_link(module: &str) -> Vec<(&'static str, String)> {
        vec![
            ("app/Shared/Runtime.sharp", "namespace App.Shared;\n\npublic interface Runtime\n{\n}\n".to_owned()),
            ("app/Shared/LiveRuntime.sharp", "namespace App.Shared;\n\npublic class LiveRuntime : Runtime\n{\n}\n".to_owned()),
            ("app/Shared/QuietRuntime.sharp", "namespace App.Shared;\n\npublic class QuietRuntime : Runtime\n{\n}\n".to_owned()),
            (
                "app/Module.sharp",
                format!("namespace App;\n\nimport App.Shared.Runtime;\nimport App.Shared.LiveRuntime;\nimport App.Shared.QuietRuntime;\n\nmodule\n{{\n    {module}\n}}\n"),
            ),
            (
                "tests/lib/Link.sharp",
                "namespace App.Testing.Lib;\n\nimport App.Shared.Runtime;\n\npublic class Link\n{\n    public Link(private Runtime runtime) { }\n\n    public void main()\n    {\n    }\n}\n".to_owned(),
            ),
        ]
    }

    /// Spec section 32: a program's services come from the modules of its namespace and of each namespace above it,
    /// wherever its file is, so an edit to `App`'s module makes stale the program of `App.Testing.Lib` under `tests/lib`,
    /// and a file added beside the module makes nothing else stale.
    #[test]
    fn an_edit_to_a_module_makes_stale_each_program_of_its_namespace_wherever_its_file_is() {
        let base = shop_project(&testing_link("singleton Runtime = LiveRuntime;"));
        let mut warm = analyzed(&base);
        let (before, last, _) =
            compile_keeping(&mut warm, &CompiledFiles::default(), &BTreeMap::new(), database_stamp(&base), |_| false);
        let link = input_paths(accepted(&before, "tests/lib/Link.sharp"));
        assert!(link.contains(&"app/Module.sharp".to_owned()), "{link:?}");

        let edited = shop_project(&testing_link("singleton Runtime = QuietRuntime;"));
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let (after, last, _) = compile_keeping(&mut warm, &last, &before, database_stamp(&edited), |_| true);
        assert!(!matches!(after.get("tests/lib/Link.sharp"), Some(Compilation::Kept)), "{after:?}");

        let mut files = testing_link("singleton Runtime = QuietRuntime;");
        files.push(("app/Foo.sharp", "namespace App;\n\npublic class Foo\n{\n}\n".to_owned()));
        let added = shop_project(&files);
        warm.update_database(added.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        let previous: BTreeMap<String, Compilation> = after
            .into_iter()
            .map(|(name, compilation)| match compilation {
                Compilation::Kept => {
                    let bytes = accepted(&before, &name).to_vec();
                    (name, Compilation::Accepted(bytes))
                }
                compilation => (name, compilation),
            })
            .collect();
        let (grown, _, _) = compile_keeping(&mut warm, &last, &previous, database_stamp(&added), |_| true);
        let lowered: Vec<&String> = grown
            .iter()
            .filter(|(name, compilation)| *name != "app/Foo.sharp" && !matches!(compilation, Compilation::Kept))
            .map(|(name, _)| name)
            .collect();
        assert!(lowered.is_empty(), "adding app/Foo.sharp made stale {lowered:?}");
    }

    /// Spec section 32: a module added for a namespace sits in a folder that holds a file of the namespace, or in a
    /// PSR-4 parent of one, so a program names the `Module.sharp` of each such folder of its namespace and of each
    /// namespace above it, stamped absent while no file is there. `app/` holds no file of `App`, and is `App`'s
    /// folder as the parent of `app/Shared/`, which holds `App.Shared`.
    #[test]
    fn a_program_names_the_module_file_each_folder_of_its_namespaces_would_hold() {
        let database = project(&[
            ("app/Shared/Runtime.sharp", "namespace App.Shared;\n\npublic interface Runtime\n{\n}\n"),
            (
                "tests/lib/Link.sharp",
                "namespace App.Testing.Lib;\n\npublic class Link\n{\n    public void main()\n    {\n    }\n}\n",
            ),
        ]);

        let compiled = compile(&mut analyzed(&database), database_stamp(&database));

        let modules: Vec<(String, bool)> = inputs(accepted(&compiled, "tests/lib/Link.sharp"))
            .into_iter()
            .filter(|input| input.path.ends_with(b"Module.sharp"))
            .map(|input| (String::from_utf8_lossy(&input.path).into_owned(), input.hash != [0; 16]))
            .collect();
        assert_eq!(modules, [("app/Module.sharp".to_owned(), false), ("tests/lib/Module.sharp".to_owned(), false)]);
    }

    #[test]
    fn a_file_the_lean_step_refuses_gets_its_issues_and_no_compiled_file() {
        let database = lawful_project();
        let mut service = analyzed(&database);
        let money = FileId::new(b"app/Shared/Money.sharp");

        let compiled = fresh(
            &mut service,
            empty_stamp,
            |_, _, _| {
                let mut issues = IssueCollection::default();
                issues.push(Issue::error("Law addKeepsCurrency is not proven."));
                Ok(HashMap::from_iter([(money, issues)]))
            },
            &Library::default(),
            |issues| issues,
        )
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

    /// The law check gets each file the analysis reported an error in, as `law_refusals` does, whether or not its
    /// errors refuse it: a law that reaches it is refused for its errors, and one that reaches a file with none that
    /// was left untranslated, as one whose lowering panicked, is the checker's bug.
    #[test]
    fn the_law_check_gets_each_file_the_analysis_reported_an_error_in() {
        let database = project(&[
            (
                "app/Shared/Money.sharp",
                "namespace App.Shared;\n\npublic class Money\n{\n    public static int kept(int a) => Digits.keep(a);\n\n    law keeps(int a) => Money.kept(a) == a;\n}\n",
            ),
            (
                "app/Shared/Digits.sharp",
                "namespace App.Shared;\n\npublic class Digits\n{\n    public static int keep(int a) => a;\n\n    public static int broken() => \"text\";\n}\n",
            ),
        ]);
        let digits = FileId::new(b"app/Shared/Digits.sharp");
        let refusing_none = |_: IssueCollection| IssueCollection::default();
        for refusing in [(|issues| issues) as fn(IssueCollection) -> IssueCollection, refusing_none] {
            let mut service = analyzed(&database);
            let mut checked = None;

            fresh(
                &mut service,
                empty_stamp,
                |_, _, errored| {
                    checked = Some(errored.clone());
                    Ok(HashMap::default())
                },
                &Library::default(),
                refusing,
            )
            .expect("the compile runs");

            assert_eq!(checked, Some(HashSet::from_iter([digits])));
        }
    }

    #[test]
    fn a_failed_law_check_reports_itself_as_the_law_check() {
        let database = lawful_project();
        let mut service = analyzed(&database);

        let error = fresh(
            &mut service,
            empty_stamp,
            |_, _, _| Err(std::io::Error::other("the law checker failed with exit status: 1")),
            &Library::default(),
            |issues| issues,
        )
        .expect_err("the law check failed");

        assert_eq!(error.to_string(), "The laws could not be checked: the law checker failed with exit status: 1.");
    }

    /// A project whose `Report` reads what the headers of `Invoice`, `Taxable` and `Status` say they inherit, and whose
    /// `Notice` reads the effects the `extern` declaration of `Mailer` gives it.
    fn headers() -> Vec<(&'static str, String)> {
        vec![
            ("app/Shop/Document.sharp", "namespace App.Shop;\n\npublic class Document\n{\n    public int pages()\n    {\n        return 1;\n    }\n}\n".to_string()),
            ("app/Shop/Record.sharp", "namespace App.Shop;\n\npublic class Record\n{\n    public int rows()\n    {\n        return 2;\n    }\n}\n".to_string()),
            ("app/Shop/Payable.sharp", "namespace App.Shop;\n\npublic interface Payable\n{\n    int amount();\n}\n".to_string()),
            ("app/Shop/Taxable.sharp", "namespace App.Shop;\n\npublic interface Taxable : Payable\n{\n}\n".to_string()),
            ("app/Shop/Invoice.sharp", "namespace App.Shop;\n\npublic class Invoice : Document, Payable\n{\n    public int amount()\n    {\n        return 5;\n    }\n}\n".to_string()),
            ("app/Shop/Status.sharp", "namespace App.Shop;\n\npublic enum Status : Payable\n{\n    case Open;\n\n    public int amount() => 1;\n}\n".to_string()),
            ("app/Shop/Report.sharp", "namespace App.Shop;\n\npublic class Report\n{\n    public int pages(Invoice invoice)\n    {\n        return invoice.pages();\n    }\n\n    public int settle(Invoice invoice)\n    {\n        return this.pay(invoice);\n    }\n\n    public int tax(Taxable taxable)\n    {\n        return taxable.amount();\n    }\n\n    public int due()\n    {\n        return this.pay(Status.Open);\n    }\n\n    private int pay(Payable payable)\n    {\n        return payable.amount();\n    }\n}\n".to_string()),
            ("app/Lib/Mailer.php", "<?php\n\nnamespace Mail;\n\nfinal class Mailer\n{\n    public static function subject(string $text): string\n    {\n        return $text;\n    }\n}\n".to_string()),
            ("app/Stubs/Mail.sharp", "namespace App.Stubs;\n\nimport Mail.Mailer;\n\nextern Mailer uses none;\n".to_string()),
            ("app/Shop/Notice.sharp", "namespace App.Shop;\n\nimport Mail.Mailer;\n\npublic class Notice\n{\n    public Notice(private string name) { }\n\n    public string text => Mailer.subject(this.name);\n\n    law titled(Notice notice) => notice.text != \"\";\n}\n".to_string()),
            ("app/Shop/Rates.sharp", "namespace App.Shop;\n\npublic interface Rates\n{\n    int rate(int id);\n}\n".to_string()),
            ("app/Shop/Quote.sharp", "namespace App.Shop;\n\npublic class Quote\n{\n    public Quote(private Rates rates) { }\n\n    public int price => this.rates.rate(1);\n\n    law priced(Quote quote) => quote.price > 0;\n}\n".to_string()),
            ("app/Shop/Payments.sharp", "namespace App.Shop;\n\npublic class Payments\n{\n    public int charge(int cents) => cents;\n}\n".to_string()),
            ("app/Shop/Checkout.sharp", "namespace App.Shop;\n\npublic class Checkout\n{\n    public Checkout(private Payments payments) { }\n\n    public int total => this.payments.charge(100);\n\n    law totaled(Checkout checkout) => checkout.total > 0;\n}\n".to_string()),
            ("app/Shop/Base.sharp", "namespace App.Shop;\n\npublic abstract class Base\n{\n    public int size() => 1;\n}\n".to_string()),
            ("app/Shop/Kept.sharp", "namespace App.Shop;\n\npublic class Kept : Base\n{\n}\n".to_string()),
            ("app/Shop/Shelf.sharp", "namespace App.Shop;\n\npublic class Shelf\n{\n    public Shelf(private Kept kept) { }\n\n    public int count => this.kept.size();\n\n    law counted(Shelf shelf) => shelf.count > 0;\n}\n".to_string()),
        ]
    }

    /// The key of each accepted file's compiled file, and the messages of each refused file.
    fn outcomes(compiled: &BTreeMap<String, Compilation>) -> BTreeMap<&str, Result<[u8; 16], Vec<String>>> {
        compiled
            .iter()
            .map(|(name, compilation)| {
                let outcome = match compilation {
                    Compilation::Accepted(bytes) => Ok(compiled_key(bytes)),
                    Compilation::Refused(issues) => Err(messages(issues)),
                    Compilation::Kept => panic!("{name} was kept unread"),
                };
                (name.as_str(), outcome)
            })
            .collect()
    }

    #[test]
    fn a_compile_after_an_edit_to_a_header_or_an_extern_matches_a_clean_compile() {
        let base = shop_project(&headers());

        for (edited_file, from, to) in [
            ("app/Shop/Invoice.sharp", "Invoice : Document,", "Invoice : Record,"),
            ("app/Shop/Invoice.sharp", "Document, Payable", "Document"),
            ("app/Shop/Taxable.sharp", "Taxable : Payable", "Taxable"),
            ("app/Shop/Status.sharp", "Status : Payable", "Status"),
            ("app/Stubs/Mail.sharp", "extern Mailer uses none;", "extern Mailer uses Http;"),
            ("app/Shop/Rates.sharp", "int rate(int id);", "int rate(int id) uses Http;"),
            ("app/Shop/Payments.sharp", "public class Payments", "public effect abstract class Payments"),
            ("app/Shop/Base.sharp", "public abstract class Base", "public abstract effect class Base"),
        ] {
            let mut files = headers();
            let (_, contents) =
                files.iter_mut().find(|(name, _)| *name == edited_file).expect("the edited file exists");
            assert!(contents.contains(from), "{edited_file} holds `{from}`");
            *contents = contents.replace(from, to);
            let edited = shop_project(&files);

            let mut warm = analyzed(&base);
            compile(&mut warm, empty_stamp);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let warm = compile(&mut warm, empty_stamp);
            let clean = compile(&mut analyzed(&edited), empty_stamp);

            assert!(
                outcomes(&clean).iter().any(|(name, outcome)| *name != edited_file && outcome.is_err()),
                "`{to}` refuses a file that reads {edited_file}: {:#?}",
                outcomes(&clean)
            );
            assert_eq!(outcomes(&warm), outcomes(&clean), "{edited_file}: `{from}` to `{to}`");
        }
    }

    /// A project whose `Shelf` and `Orders` read the bounds of the type parameters of `Box`, `Source` and `Tools.same`,
    /// and whose `Reader` reads the variance of `Feed`'s and the count of `Pair`'s.
    fn bounds() -> Vec<(&'static str, String)> {
        vec![
            ("app/Shop/Entity.sharp", "namespace App.Shop;\n\npublic abstract class Entity\n{\n}\n".to_string()),
            ("app/Shop/Order.sharp", "namespace App.Shop;\n\npublic class Order : Entity\n{\n}\n".to_string()),
            ("app/Shop/Customer.sharp", "namespace App.Shop;\n\npublic class Customer\n{\n}\n".to_string()),
            ("app/Shop/Box.sharp", "namespace App.Shop;\n\npublic class Box<TItem : Entity>\n{\n    public Box() { }\n}\n".to_string()),
            ("app/Shop/Source.sharp", "namespace App.Shop;\n\npublic interface Source<TItem : Entity>\n{\n    TItem? first();\n}\n".to_string()),
            ("app/Shop/Tools.sharp", "namespace App.Shop;\n\npublic class Tools\n{\n    public static T same<T : Entity>(T value) => value;\n}\n".to_string()),
            ("app/Shop/Shelf.sharp", "namespace App.Shop;\n\npublic class Shelf\n{\n    public Box<Order> box() => new Box<Order>();\n\n    public Order same(Order order) => Tools.same(order);\n}\n".to_string()),
            ("app/Shop/Orders.sharp", "namespace App.Shop;\n\npublic class Orders : Source<Order>\n{\n    public Order? first() => null;\n}\n".to_string()),
            ("app/Shop/Feed.sharp", "namespace App.Shop;\n\npublic interface Feed<out TItem : Entity>\n{\n    TItem? next();\n}\n".to_string()),
            ("app/Shop/Pair.sharp", "namespace App.Shop;\n\npublic class Pair<TFirst>\n{\n    public Pair() { }\n}\n".to_string()),
            ("app/Shop/Reader.sharp", "namespace App.Shop;\n\npublic class Reader\n{\n    public Entity? read(Feed<Entity> feed) => feed.next();\n\n    public Entity? orders(Feed<Order> feed) => this.read(feed);\n\n    public Pair<Order> pair() => new Pair<Order>();\n}\n".to_string()),
        ]
    }

    #[test]
    fn a_compile_after_an_edit_to_a_type_parameter_list_matches_a_clean_compile() {
        let base = shop_project(&bounds());

        for (edited_file, from, to) in [
            ("app/Shop/Box.sharp", "<TItem : Entity>", "<TItem : Customer>"),
            ("app/Shop/Source.sharp", "<TItem : Entity>", "<TItem : Customer>"),
            ("app/Shop/Tools.sharp", "<T : Entity>", "<T : Customer>"),
            ("app/Shop/Feed.sharp", "<out TItem : Entity>", "<TItem : Entity>"),
            ("app/Shop/Pair.sharp", "<TFirst>", "<TFirst, TSecond>"),
        ] {
            let mut files = bounds();
            let (_, contents) =
                files.iter_mut().find(|(name, _)| *name == edited_file).expect("the edited file exists");
            assert!(contents.contains(from), "{edited_file} holds `{from}`");
            *contents = contents.replace(from, to);
            let edited = shop_project(&files);

            let mut warm = analyzed(&base);
            compile(&mut warm, empty_stamp);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let warm = compile(&mut warm, empty_stamp);
            let clean = compile(&mut analyzed(&edited), empty_stamp);

            assert!(
                outcomes(&clean).iter().any(|(name, outcome)| *name != edited_file && outcome.is_err()),
                "`{to}` refuses a file that reads {edited_file}: {:#?}",
                outcomes(&clean)
            );
            assert_eq!(outcomes(&warm), outcomes(&clean), "{edited_file}: `{from}` to `{to}`");
        }
    }

    /// A project whose `Report` reads a library's constant through its namespace, `Pdf.DEFAULT_DPI`, and a constant the
    /// library declares in PHP's root namespace beside its classes, `Pdf.APP_VERSION`, spec section 23, each declared in
    /// a plain PHP file of the project.
    fn constants() -> Vec<(&'static str, String)> {
        vec![
            (
                "lib/Pdf.php",
                "<?php\n\nnamespace Vendor\\Pdf;\n\nconst DEFAULT_DPI = 300;\n\nfinal class Document\n{\n}\n".to_string(),
            ),
            ("lib/version.php", "<?php\n\nconst APP_VERSION = 3;\n".to_string()),
            (
                "app/Shop/Externs.sharp",
                "namespace App.Shop;\n\nimport Vendor.Pdf;\n\nextern namespace Pdf for APP_VERSION;\n".to_string(),
            ),
            ("app/Shop/Report.sharp", "namespace App.Shop;\n\nimport Vendor.Pdf;\n\npublic class Report\n{\n    public int dpi() => Pdf.DEFAULT_DPI;\n\n    public int version() => Pdf.APP_VERSION;\n}\n".to_string()),
        ]
    }

    /// A read of a constant through its namespace lists the file that declares it as an input, as a call of a
    /// function through its namespace does, so the engine refuses the compiled file once that file changes.
    #[test]
    fn a_read_of_a_constant_through_its_namespace_lists_the_file_that_declares_it_as_an_input() {
        let database = shop_project(&constants());

        let compiled = compile(&mut analyzed(&database), empty_stamp);

        assert_eq!(
            input_paths(accepted(&compiled, "app/Shop/Report.sharp")),
            [
                "app/Module.sharp",
                "app/Shop/Module.sharp",
                "app/Shop/Report.sharp",
                "composer.lock",
                "lib/Pdf.php",
                "lib/version.php"
            ]
        );
    }

    /// Deleting a constant a file reads through its namespace, or changing its value's type, refuses that file, and a
    /// compile after the edit refuses it as a clean compile does.
    #[test]
    fn a_compile_after_an_edit_to_a_constant_read_through_its_namespace_matches_a_clean_compile() {
        let base = shop_project(&constants());

        for (edited_file, from, to) in [
            ("lib/Pdf.php", "const DEFAULT_DPI = 300;\n", ""),
            ("lib/Pdf.php", "DEFAULT_DPI = 300;", "DEFAULT_DPI = \"300\";"),
            ("lib/version.php", "const APP_VERSION = 3;\n", ""),
            ("lib/version.php", "APP_VERSION = 3;", "APP_VERSION = \"3\";"),
        ] {
            let mut files = constants();
            let (_, contents) =
                files.iter_mut().find(|(name, _)| *name == edited_file).expect("the edited file exists");
            assert!(contents.contains(from), "{edited_file} holds `{from}`");
            *contents = contents.replace(from, to);
            let edited = shop_project(&files);

            let mut warm = analyzed(&base);
            compile(&mut warm, empty_stamp);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let warm = compile(&mut warm, empty_stamp);
            let clean = compile(&mut analyzed(&edited), empty_stamp);

            assert!(
                outcomes(&clean)["app/Shop/Report.sharp"].is_err(),
                "`{to}` refuses Report: {:#?}",
                outcomes(&clean)
            );
            assert_eq!(outcomes(&warm), outcomes(&clean), "{edited_file}: `{from}` to `{to}`");
        }
    }

    /// A `uses` that names no effect is refused with the effects its file can name. A class elsewhere that becomes
    /// an effect class, in the file's namespace or in another, gives the same message after the edit as a clean compile
    /// does.
    #[test]
    fn a_uses_refusal_after_another_class_becomes_an_effect_class_matches_a_clean_compile() {
        let files = || {
            vec![
                (
                    "app/Shop/Gateway.sharp",
                    "namespace App.Shop;\n\npublic interface Gateway\n{\n    int charge(int cents) uses Payments;\n}\n"
                        .to_string(),
                ),
                ("app/Shop/Payments.sharp", "namespace App.Shop;\n\npublic class Payments\n{\n}\n".to_string()),
                ("app/Shop/Base.sharp", "namespace App.Shop;\n\npublic abstract class Base\n{\n}\n".to_string()),
                ("app/Vault/Vault.sharp", "namespace App.Vault;\n\npublic class Vault\n{\n}\n".to_string()),
            ]
        };
        let base = shop_project(&files());

        for (edited_file, from, to) in [
            ("app/Shop/Base.sharp", "public abstract class Base", "public abstract effect class Base"),
            ("app/Vault/Vault.sharp", "public class Vault", "public effect abstract class Vault"),
        ] {
            let mut edited_files = files();
            let (_, contents) =
                edited_files.iter_mut().find(|(name, _)| *name == edited_file).expect("the edited file exists");
            *contents = contents.replace(from, to);
            let edited = shop_project(&edited_files);

            let mut warm = analyzed(&base);
            compile(&mut warm, empty_stamp);
            warm.update_database(edited.read_only());
            warm.analyze_incremental(None).expect("the warm path runs");
            let warm = compile(&mut warm, empty_stamp);
            let clean = compile(&mut analyzed(&edited), empty_stamp);

            assert!(outcomes(&clean)["app/Shop/Gateway.sharp"].is_err(), "{:#?}", outcomes(&clean));
            assert_eq!(outcomes(&warm), outcomes(&clean), "{edited_file}: `{from}` to `{to}`");
        }
    }

    /// Each spec section 23 refusal of `service`, as `file: message`.
    fn named_like_a_namespace(service: &IncrementalAnalysisService) -> Vec<String> {
        let mut refusals: Vec<String> = service
            .collect_all_issues()
            .iter()
            .filter(|issue| issue.code.as_deref() == Some("name-already-in-use"))
            .map(|issue| {
                let file = service.database().get(&issue.primary_span().expect("a primary span").file_id);
                format!("{}: {}", String::from_utf8_lossy(&file.expect("the file exists").name), issue.message)
            })
            .collect();
        refusals.sort_unstable();
        refusals
    }

    /// Spec section 23 refuses `Store` while a PHP# class sits in the namespace `App.Tenant.Store`, so a file added to
    /// or removed from `app/Tenant/Store/` changes whether the unchanged file of `Store` is refused, in a one-off compile
    /// and under `mago analyze --watch`, which analyzes with `diff`. In the last row the namespace already holds a plain
    /// PHP class, and only the PHP# file added to it refuses the plain PHP `Store`.
    #[test]
    fn a_compile_after_the_folder_named_like_a_class_gains_or_loses_a_class_matches_a_clean_compile() {
        let store = ("app/Tenant/Store.sharp", "namespace App.Tenant;\n\npublic class Store\n{\n}\n".to_string());
        let receipts = (
            "app/Tenant/Store/Receipts.sharp",
            "namespace App.Tenant.Store;\n\npublic class Receipts\n{\n}\n".to_string(),
        );
        let php_store = ("app/Tenant/Store.php", "<?php\n\nnamespace App\\Tenant;\n\nclass Store\n{\n}\n".to_string());
        let php_order =
            ("app/Tenant/Store/Order.php", "<?php\n\nnamespace App\\Tenant\\Store;\n\nclass Order\n{\n}\n".to_string());
        let alone = vec![store.clone()];
        let beside = vec![store, receipts.clone()];
        let php = vec![php_store.clone(), php_order.clone()];
        let receipts_alone = vec![php_order.clone(), receipts.clone()];
        let php_beside = vec![php_store, php_order, receipts];
        let refused = "app/Tenant/Store.sharp: `App.Tenant.Store` is both a class and a namespace: rename the class or the folder.";
        let php_refused = "app/Tenant/Store.php: `App\\Tenant\\Store` is both a class and a namespace: rename the class or the folder.";
        let sharp_refused = "app/Tenant/Store/Receipts.sharp: `App.Tenant.Store` is both a plain PHP class and a namespace: rename the class or the folder.";

        for diff in [false, true] {
            for (before, after, expected) in [
                (&alone, &beside, vec![refused]),
                (&beside, &alone, vec![]),
                (&php, &php_beside, vec![php_refused, sharp_refused]),
                (&receipts_alone, &php_beside, vec![php_refused, sharp_refused]),
                (&php_beside, &receipts_alone, vec![]),
            ] {
                let (base, edited) = (shop_project(before), shop_project(after));

                let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm_compiled = compile(&mut warm, empty_stamp);
                let mut clean = analyzed(&edited);
                let clean_compiled = compile(&mut clean, empty_stamp);

                let names =
                    |files: &[(&str, String)]| files.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ");
                let row = format!("diff {diff}: {} to {}", names(before), names(after));
                assert_eq!(named_like_a_namespace(&clean), expected, "{row}");
                assert_eq!(named_like_a_namespace(&warm), expected, "{row}");
                assert_eq!(outcomes(&warm_compiled), outcomes(&clean_compiled), "{row}");
            }
        }
    }

    /// Spec section 20.1: a `match` over a sealed interface handles each class of its namespace that implements it, so a
    /// file added to or removed from that namespace changes whether the unchanged file of the `match` is refused, in a
    /// one-off compile and under `mago analyze --watch`, which analyzes with `diff`.
    #[test]
    fn a_compile_after_a_sealed_interface_gains_or_loses_an_implementer_matches_a_clean_compile() {
        let implementer = |name: &str| {
            (
                if name == "Popup" { "app/PopupInteraction.sharp" } else { "app/ScrollInteraction.sharp" },
                format!(
                    "namespace App;\n\npublic class {name}Interaction : Interaction\n{{\n    public string name() => \"{name}\";\n}}\n"
                ),
            )
        };
        let interaction = (
            "app/Interaction.sharp",
            "namespace App;\n\npublic sealed interface Interaction\n{\n    string name();\n}\n".to_string(),
        );
        let shop = (
            "app/Shop.sharp",
            "namespace App;\n\npublic class Shop\n{\n    public string run(Interaction interaction) => match (interaction) {\n        PopupInteraction popup => popup.name(),\n    };\n}\n"
                .to_string(),
        );
        let popup_only = vec![interaction.clone(), shop.clone(), implementer("Popup")];
        let both = vec![interaction, shop, implementer("Popup"), implementer("Scroll")];

        for diff in [false, true] {
            for (before, after, refused) in [(&popup_only, &both, true), (&both, &popup_only, false)] {
                let (base, edited) = (shop_project(before), shop_project(after));

                let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm = compile(&mut warm, empty_stamp);
                let clean = compile(&mut analyzed(&edited), empty_stamp);

                let row = format!("diff {diff}: {} to {} files", before.len(), after.len());
                let shop_refusal = match &clean["app/Shop.sharp"] {
                    Compilation::Refused(issues) => messages(issues),
                    Compilation::Accepted(_) | Compilation::Kept => Vec::new(),
                };
                let expected: Vec<String> =
                    if refused { vec!["This `match` misses `ScrollInteraction`.".to_string()] } else { Vec::new() };
                assert_eq!(shop_refusal, expected, "{row}");
                assert_eq!(outcomes(&warm), outcomes(&clean), "{row}");
            }
        }
    }

    /// Each spec section 26 refusal of `service`, as `file: message`.
    fn hidden_members(service: &IncrementalAnalysisService) -> Vec<String> {
        let mut refusals: Vec<String> = service
            .collect_all_issues()
            .iter()
            .filter(|issue| issue.message.ends_with("so this extension member never runs."))
            .map(|issue| {
                let file = service.database().get(&issue.primary_span().expect("a primary span").file_id);
                format!("{}: {}", String::from_utf8_lossy(&file.expect("the file exists").name), issue.message)
            })
            .collect();
        refusals.sort_unstable();
        refusals
    }

    const WALLET: &str = "<?php\n\nnamespace Lib;\n\nclass Wallet\n{\n    public int $balance = 0;\n}\n";
    const WALLET_EXTENSIONS: &str = "namespace App.Wallets;\n\nimport Lib.Wallet;\n\npublic static class WalletExtensions\n{\n    extension(Wallet wallet)\n    {\n        public int total() => wallet.balance + 1;\n    }\n}\n";
    const WALLET_REPORT: &str = "namespace App;\n\nimport App.Wallets.WalletExtensions;\nimport Lib.Purse;\n\npublic class Report\n{\n    public int of(Purse purse)\n    {\n        return purse.wallet().total();\n    }\n}\n";

    /// Spec section 26: an imported class's extensions apply to the whole file, so a file reads each class it imports
    /// though it names none of its members, and compiles again when the class gains an extension member.
    #[test]
    fn a_file_reads_each_class_it_imports() {
        let reader = "namespace App;\n\nimport App.Wallets.WalletExtensions;\n\npublic class Reader\n{\n    public int one() => 1;\n}\n";
        let files = |extensions: &str| {
            vec![
                ("lib/Wallet.php", WALLET.to_string()),
                ("app/Wallets/WalletExtensions.sharp", extensions.to_string()),
                ("app/Reader.sharp", reader.to_string()),
            ]
        };
        let gained =
            WALLET_EXTENSIONS.replace("public int total()", "public int count() => 0;\n\n        public int total()");

        let before = compile(&mut analyzed(&shop_project(&files(WALLET_EXTENSIONS))), empty_stamp);
        let after = compile(&mut analyzed(&shop_project(&files(&gained))), empty_stamp);

        let reader = accepted(&before, "app/Reader.sharp");
        assert!(
            input_paths(reader).contains(&"app/Wallets/WalletExtensions.sharp".to_string()),
            "{:?}",
            input_paths(reader)
        );
        assert_ne!(compiled_key(reader), compiled_key(accepted(&after, "app/Reader.sharp")));
    }

    /// Spec section 26 refuses an extension member named like a real member its receiver's class declares or
    /// inherits, so a member of that name joining or leaving that class, or a class above it, changes whether the
    /// unchanged file of the extension is refused, and gives the unchanged file that calls the member a new key, though
    /// it reaches the wallet through `Purse.wallet()` and never names `Wallet`. A class below the receiver's competes
    /// with no extension, so its member changes neither. Each row edits only a PHP file. The refusals match a clean
    /// analysis in a one-off compile and under `mago analyze --watch`, which analyzes with `diff` and never compiles,
    /// and each compiled file matches a clean compile after its header.
    #[test]
    fn a_compile_after_a_class_an_extension_receiver_is_gains_or_loses_its_member_matches_a_clean_compile() {
        let wallets = |wallet: &str, family: &str| {
            vec![
                ("lib/Wallet.php", wallet.to_string()),
                ("lib/Family.php", format!("<?php\n\nnamespace Lib;\n\n{family}")),
                (
                    "lib/Purse.php",
                    "<?php\n\nnamespace Lib;\n\nclass Purse\n{\n    public function wallet(): Wallet\n    {\n        return new Wallet();\n    }\n}\n".to_string(),
                ),
                ("app/Wallets/WalletExtensions.sharp", WALLET_EXTENSIONS.to_string()),
                ("app/Report.sharp", WALLET_REPORT.to_string()),
                (
                    "app/Stubs/Lib.sharp",
                    "namespace App.Stubs;\n\nimport Lib.Base;\nimport Lib.Purse;\nimport Lib.Wallet;\n\nextern Base uses none;\nextern Purse uses none;\nextern Wallet uses none;\n".to_string(),
                ),
            ]
        };
        let total = "\n{\n    public function total(): int\n    {\n        return 9;\n    }\n}\n";
        let plain = "class Base\n{\n}\n\nclass Audited extends Wallet\n{\n}\n";
        let below = format!("class Base\n{{\n}}\n\nclass Audited extends Wallet{total}");
        let above = format!("class Base{total}\nclass Audited extends Wallet\n{{\n}}\n");
        let wallet = WALLET.replace("class Wallet", "class Wallet extends Base");
        let method = wallet.replace(
            "public int $balance = 0;",
            "public int $balance = 0;\n\n    public function total(): int\n    {\n        return 9;\n    }",
        );
        let docblock = wallet.replace("class Wallet", "/** @method int total() */\nclass Wallet");

        // Each row: the files without the member, the files with it, and the refusal the member brings, if any.
        let rows = [
            (wallets(&wallet, plain), wallets(&method, plain), Some("`Wallet`")),
            (wallets(&wallet, plain), wallets(&docblock, plain), Some("`Wallet`")),
            (wallets(&wallet, plain), wallets(&wallet, &above), Some("`Wallet`")),
            (wallets(&wallet, plain), wallets(&wallet, &below), None),
        ];

        for (without, with, member) in &rows {
            for (before, after) in [(without, with), (with, without)] {
                let (base, edited) = (shop_project(before), shop_project(after));
                let row = format!("{member:?} {}", if after == with { "added" } else { "removed" });
                let expected: Vec<String> = member
                    .filter(|_| after == with)
                    .map(|class| {
                        format!(
                            "app/Wallets/WalletExtensions.sharp: {class} already declares `total()`, so this extension member never runs."
                        )
                    })
                    .into_iter()
                    .collect();

                for diff in [false, true] {
                    let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                    warm.update_database(edited.read_only());
                    warm.analyze_incremental(None).expect("the warm path runs");
                    assert_eq!(hidden_members(&warm), expected, "diff {diff}: {row}");
                }

                let mut warm = analyzed(&base);
                let base_compiled = compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm_compiled = compile(&mut warm, empty_stamp);
                let mut clean = analyzed(&edited);
                let clean_compiled = compile(&mut clean, empty_stamp);

                assert_eq!(hidden_members(&clean), expected, "{row}");
                assert_eq!(
                    compiled_key(accepted(&base_compiled, "app/Report.sharp"))
                        != compiled_key(accepted(&clean_compiled, "app/Report.sharp")),
                    member.is_some(),
                    "{row}: the caller compiles again only when the member competes"
                );
                assert_eq!(bodies(&warm_compiled), bodies(&clean_compiled), "{row}");
            }
        }
    }

    /// The bytes after the header of each accepted file's compiled file, and the messages of each refused file. A warm
    /// key can differ from a clean one while these match: incremental analysis keeps the references a method it
    /// analyzes again made before, so a warm compile reads members a clean one does not, and compiles its file again
    /// once more than it needs to.
    fn bodies(compiled: &BTreeMap<String, Compilation>) -> BTreeMap<&str, Result<&[u8], Vec<String>>> {
        compiled
            .iter()
            .map(|(name, compilation)| {
                let outcome = match compilation {
                    Compilation::Accepted(bytes) => Ok(&bytes[104..]),
                    Compilation::Refused(issues) => Err(messages(issues)),
                    Compilation::Kept => panic!("{name} was kept unread"),
                };
                (name.as_str(), outcome)
            })
            .collect()
    }

    const SHAPE: &str =
        "namespace App;\n\npublic class Shape\n{\n    public int size(Random random) => Random.size();\n}\n";
    const RANDOM: &str = "namespace App;\n\npublic class Random\n{\n    public static int size() => 1;\n}\n";

    /// Spec section 23: a class of the file's namespace wins over the class `Sharp` imports by default, so the unchanged
    /// `Shape`, whose bare `Random` meant the missing `Sharp.Random`, compiles once `app/App/Random.sharp` declares
    /// `App.Random`, and is refused again once it is removed, in a one-off compile and under `mago analyze --watch`. An
    /// edit to `Shape` itself keeps its bare `Random` naming `App.Random`.
    #[test]
    fn a_compile_after_the_namespace_gains_or_loses_a_class_named_like_a_default_class_matches_a_clean_compile() {
        let alone = vec![("app/App/Shape.sharp", SHAPE.to_string())];
        let beside = vec![("app/App/Shape.sharp", SHAPE.to_string()), ("app/App/Random.sharp", RANDOM.to_string())];
        let edited_shape = SHAPE.replace(
            "Random.size();\n",
            "Random.size();\n\n    public int twice(Random random) => this.size(random) * 2;\n",
        );
        let shape_edited = vec![("app/App/Shape.sharp", edited_shape), ("app/App/Random.sharp", RANDOM.to_string())];

        for diff in [false, true] {
            for (name, before, after, accepted) in [
                ("Random added", &alone, &beside, true),
                ("Random removed", &beside, &alone, false),
                ("Shape edited", &beside, &shape_edited, true),
            ] {
                let (base, edited) = (shop_project(before), shop_project(after));

                let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm_compiled = compile(&mut warm, empty_stamp);
                let clean_compiled = compile(&mut analyzed(&edited), empty_stamp);

                let row = format!("diff {diff}: {name}");
                assert_eq!(outcomes(&clean_compiled)["app/App/Shape.sharp"].is_ok(), accepted, "{row}");
                assert_eq!(outcomes(&warm_compiled), outcomes(&clean_compiled), "{row}");
            }
        }
    }

    /// Spec section 23: an analysis given another's scans compiles as a clean analysis of its own files. A host's stub
    /// that declares the plain PHP `App\Random`, which only the second analysis holds, wins over the default
    /// `Sharp.Random` that `Shape`'s bare `Random` names in the first, so the second refuses `Shape` for the stub's
    /// class, which no `extern` line declares. A file edited between the two analyses compiles as edited: `Counter`'s
    /// new `two` calls its new `three`, which the first analysis's scan of `Counter` doesn't declare.
    #[test]
    fn an_analysis_given_another_analysis_scans_compiles_as_a_clean_analysis_of_its_own_files() {
        const STUB: &str = "<?php\n\nnamespace App;\n\nclass Random\n{\n    public static function size(): int\n    {\n        return 1;\n    }\n}\n";
        const COUNTER: &str = "namespace App;\n\npublic class Counter\n{\n    public int one() => 1;\n}\n";
        const EDITED_COUNTER: &str = "namespace App;\n\npublic class Counter\n{\n    public int one() => 1;\n\n    public int two() => this.three();\n\n    public int three() => 3;\n}\n";
        let shape = [("app/App/Shape.sharp", SHAPE)];
        let mut with_stub = project(&shape);
        with_stub.add(File::new(
            Cow::Borrowed(b"stubs/Random.php"),
            FileType::External,
            None,
            Cow::Borrowed(STUB.as_bytes()),
        ));

        for (name, checked, first, first_refusal, second, second_refusal) in [
            (
                "a host's stub declares App\\Random",
                "app/App/Shape.sharp",
                project(&shape),
                vec![
                    "Cannot find type `Sharp.Random`.",
                    "`Random` names no class, and no namespace this file imports.",
                ],
                with_stub,
                vec!["`Random` has no `extern` line."],
            ),
            (
                "Counter edited",
                "app/App/Counter.sharp",
                project(&[("app/App/Counter.sharp", COUNTER)]),
                vec![],
                project(&[("app/App/Counter.sharp", EDITED_COUNTER)]),
                vec![],
            ),
        ] {
            let refusal = |compiled: &BTreeMap<String, Compilation>| {
                outcomes(compiled)[checked].clone().err().unwrap_or_default()
            };
            let service = |database: &Database<'static>| {
                IncrementalAnalysisService::new(
                    database.read_only(),
                    prelude,
                    Settings::default(),
                    ParserSettings::default(),
                    Arc::clone(&PLUGIN_REGISTRY),
                )
            };
            let mut first = service(&first).keeping_scans();
            first.analyze().expect("the first analysis runs");
            let mut given = service(&second).with_scans(first.take_scans());
            given.analyze().expect("the analysis given the first one's scans runs");

            assert_eq!(refusal(&compile(&mut first, empty_stamp)), first_refusal, "{name}: the first analysis");
            let given_compiled = compile(&mut given, empty_stamp);
            let clean_compiled = compile(&mut analyzed(&second), empty_stamp);
            assert_eq!(refusal(&clean_compiled), second_refusal, "{name}: a clean analysis of the second's files");
            assert_eq!(outcomes(&given_compiled), outcomes(&clean_compiled), "{name}");
        }
    }

    /// Spec section 26: an extension of the file's namespace shadows the standard library's of the same name that fits
    /// the same receiver, so the unchanged `Report`, whose `name.shout()` ran `Sharp.Text.Words`'s and returned its
    /// `string`, is refused once `app/App/Loud.sharp` declares `App.Loud`'s `int` one on `string`, and compiles again
    /// once it is removed or takes an `int`, in a one-off compile and under `mago analyze --watch`.
    #[test]
    fn a_compile_after_the_namespace_gains_or_loses_an_extension_that_shadows_a_default_one_matches_a_clean_compile() {
        let words = "namespace Sharp.Text;\n\npublic static class Words\n{\n    extension(string text)\n    {\n        public string shout() => text;\n    }\n}\n";
        let loud = |receiver: &str| {
            format!(
                "namespace App;\n\npublic static class Loud\n{{\n    extension({receiver} text)\n    {{\n        public int shout() => 1;\n    }}\n}}\n"
            )
        };
        let report =
            "namespace App;\n\npublic class Report\n{\n    public string text(string name) => name.shout();\n}\n";
        let alone = vec![
            ("vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Words.sharp", words.to_string()),
            ("app/App/Report.sharp", report.to_string()),
        ];
        let beside = |receiver: &str| {
            let mut files = alone.clone();
            files.push(("app/App/Loud.sharp", loud(receiver)));
            files
        };
        let (on_string, on_int) = (beside("string"), beside("int"));

        for diff in [false, true] {
            for (name, before, after, accepted) in [
                ("Loud added", &alone, &on_string, false),
                ("Loud removed", &on_string, &alone, true),
                ("Loud's receiver became string", &on_int, &on_string, false),
                ("Loud's receiver became int", &on_string, &on_int, true),
            ] {
                let (base, edited) = (shop_project(before), shop_project(after));

                let mut warm = analyzed_with(&base, Settings { diff, ..Settings::default() });
                compile(&mut warm, empty_stamp);
                warm.update_database(edited.read_only());
                warm.analyze_incremental(None).expect("the warm path runs");
                let warm_compiled = compile(&mut warm, empty_stamp);
                let clean_compiled = compile(&mut analyzed(&edited), empty_stamp);

                let row = format!("diff {diff}: {name}");
                assert_eq!(outcomes(&clean_compiled)["app/App/Report.sharp"].is_ok(), accepted, "{row}");
                assert_eq!(outcomes(&warm_compiled), outcomes(&clean_compiled), "{row}");
            }
        }

        let mut edited = on_int.clone();
        edited[2].1 = edited[2].1.replace("=> 1;", "=> 2;");
        let mut warm = analyzed(&shop_project(&on_int));
        warm.update_database(shop_project(&edited).read_only());
        warm.analyze_incremental(None).expect("the warm path runs");
        assert_eq!(
            warm.analyzed_files,
            HashSet::from_iter([FileId::new(b"app/App/Loud.sharp")]),
            "an edit to a member's body analyzes only its own file"
        );
    }

    /// An editor analyzes one file against the classes of its namespace too, so `Shape`'s bare `Random` is `App.Random`.
    #[test]
    fn one_file_analyzed_alone_names_the_class_of_its_namespace() {
        let database = project(&[("app/App/Shape.sharp", SHAPE), ("app/App/Random.sharp", RANDOM)]);
        let service = analyzed(&database);
        let shape = FileId::new(b"app/App/Shape.sharp");

        assert_eq!(messages(&service.analyze_file(shape)), Vec::<String>::new());
        assert_eq!(
            messages(&service.analyze_file_with_artifacts(shape).expect("the file exists").0),
            Vec::<String>::new()
        );
    }

    /// Spec section 23: once `App.Position` and `App.Environment` take the short names, a file reaches the standard
    /// classes through an aliased import, and compiles.
    #[test]
    fn a_standard_class_its_namespace_shadows_compiles_through_an_aliased_import() {
        let position =
            "namespace App;\n\npublic class Position\n{\n    public Position(public string title { get; }) { }\n}\n";
        let environment = "namespace App;\n\npublic class Environment\n{\n    public int level() => 1;\n}\n";
        let fixed = "namespace App;\n\nimport Sharp.System.Environment as SharpEnvironment;\nimport Sharp.Position as SharpPosition;\n\npublic class Fixed\n{\n    public string here() => SharpPosition.current().directory;\n\n    public string file() => SharpPosition.current().file;\n\n    public int line() => SharpPosition.current().line;\n\n    public string caller() => SharpPosition.current().function;\n\n    public string folder(SharpEnvironment environment) => environment.currentDirectory();\n\n    public string title(Position position) => position.title;\n\n    public int level(Environment environment) => environment.level();\n}\n";
        let database = project(&[
            ENVIRONMENT,
            ("app/App/Position.sharp", position),
            ("app/App/Environment.sharp", environment),
            ("app/App/Fixed.sharp", fixed),
        ]);
        let mut service = analyzed(&database);

        assert_eq!(messages(&service.analyze_file(FileId::new(b"app/App/Fixed.sharp"))), Vec::<String>::new());
        let compiled = compile(&mut service, empty_stamp);
        let fixed = &outcomes(&compiled)["app/App/Fixed.sharp"];
        assert!(fixed.is_ok(), "the aliased imports compile: {fixed:?}");
    }

    /// Only a class named like a default class changes what a bare name means, so adding another class to `App`
    /// analyzes only its own file under `mago analyze --watch`.
    #[test]
    fn a_class_named_like_no_default_class_leaves_the_other_files_of_its_namespace_unanalyzed() {
        let base = project(&[("app/App/Shape.sharp", SHAPE)]);
        let edited = project(&[("app/App/Shape.sharp", SHAPE), ("app/App/Order.sharp", ORDER)]);

        let mut warm = analyzed(&base);
        warm.update_database(edited.read_only());
        warm.analyze_incremental(None).expect("the warm path runs");

        assert_eq!(warm.analyzed_files, HashSet::from_iter([FileId::new(b"app/App/Order.sharp")]));
    }

    /// A file whose bare `Position` means `Sharp.Position` lists the files that would declare `App.Position` in its
    /// folder as inputs, stamped absent, so adding one makes its compiled file stale. A package file leaves them to
    /// `composer.lock`, as the rest of its package.
    #[test]
    fn a_default_class_name_lists_the_files_its_namespace_would_declare_it_in_as_absent_inputs() {
        let here =
            "namespace App;\n\npublic class Here\n{\n    public string folder() => Position.current().directory;\n}\n";
        let packaged = "vendor/acme/shapes/src/Here.sharp";
        let database =
            project(&[("app/App/Here.sharp", here), (packaged, &here.replace("namespace App;", "namespace Acme;"))]);

        let compiled = compile(&mut analyzed(&database), database_stamp(&database));

        assert_eq!(input_paths(accepted(&compiled, packaged)), ["composer.lock", packaged]);

        let inputs = inputs(accepted(&compiled, "app/App/Here.sharp"));
        assert_eq!(
            inputs.iter().map(|input| String::from_utf8_lossy(&input.path)).collect::<Vec<_>>(),
            [
                "app/App/Here.sharp",
                "app/App/Module.sharp",
                "app/App/Position.php",
                "app/App/Position.sharp",
                "composer.lock"
            ]
        );
        for input in &inputs[2..4] {
            assert_eq!((input.size, input.mtime_ns, input.hash), (0, 0, [0; 16]), "{input:?} is stamped absent");
        }
    }

    /// Spec section 26 applies an extension of the file's namespace with no import, so a file whose code reaches an
    /// extension, the standard library's included, lists its folder's listing and each `.sharp` file in its folder as
    /// inputs, and nothing below it or in another language. A file that reaches no extension lists neither, and a
    /// package file leaves them to `composer.lock`. A collection's member, the standard library's, is the collection's
    /// own, which no extension of the namespace replaces, so a file that reaches only those lists neither.
    #[test]
    fn a_file_whose_code_reaches_an_extension_lists_the_sharp_files_of_its_folder_as_inputs() {
        let words = "namespace Sharp.Text;\n\npublic static class Words\n{\n    extension(string text)\n    {\n        public string shout() => text;\n    }\n}\n";
        let report =
            "namespace App;\n\npublic class Report\n{\n    public string text(string name) => name.shout();\n}\n";
        let plain = "namespace App;\n\npublic class Plain\n{\n    public string text(string name) => name;\n}\n";
        let listed = "namespace App;\n\npublic class Listed\n{\n    public int size(List<int> ids)\n    {\n        let grown = ids;\n        grown.add(1);\n        return grown.count();\n    }\n}\n";
        let packaged = "vendor/acme/shapes/src/Report.sharp";
        let database = project(&[
            (
                "vendor/heyjordanparker/php-sharp-composer/library/Sharp/ListMembers.sharp",
                include_str!("../../../../../../composer/library/Sharp/ListMembers.sharp"),
            ),
            ("app/App/Listed.sharp", listed),
            ("vendor/heyjordanparker/php-sharp-composer/library/Sharp/Text/Words.sharp", words),
            ("app/App/Report.sharp", report),
            ("app/App/Plain.sharp", plain),
            ("app/App/Model.php", "<?php\n\nnamespace App;\n\nclass Model\n{\n}\n"),
            ("app/App/Deep/Inner.sharp", "namespace App.Deep;\n\npublic class Inner\n{\n}\n"),
            (packaged, &report.replace("namespace App;", "namespace Acme;")),
        ]);

        let compiled = compile(&mut analyzed(&database), database_stamp(&database));

        assert_eq!(
            input_paths(accepted(&compiled, "app/App/Report.sharp")),
            [
                "app/App/*.sharp",
                "app/App/Listed.sharp",
                "app/App/Module.sharp",
                "app/App/Plain.sharp",
                "app/App/Report.sharp",
                "composer.lock"
            ]
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/App/Plain.sharp")),
            ["app/App/Module.sharp", "app/App/Plain.sharp", "composer.lock"]
        );
        assert_eq!(
            input_paths(accepted(&compiled, "app/App/Listed.sharp")),
            ["app/App/Listed.sharp", "app/App/Module.sharp", "composer.lock"]
        );
        assert_eq!(input_paths(accepted(&compiled, packaged)), ["composer.lock", packaged]);
    }
}
