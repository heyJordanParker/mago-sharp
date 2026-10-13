//! Returns a declaration refinement asks to take from a method's body.
//!
//! A method can declare a native return that names a generic class without its arguments,
//! while its body returns the applied class. Once the codebase is populated, every file holding
//! such a method is analyzed and each method's returned type becomes its refined return. A
//! method whose body calls another such method sees that method's refined return on a later
//! round, so rounds repeat until no return changes, which makes the result independent of file
//! order. A method whose return stays unresolved keeps its native declaration and reports the
//! issue its refinement carries.

use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use rayon::prelude::*;

use mago_allocator::LocalArena;
use mago_analyzer::Analyzer;
use mago_analyzer::analysis_result::AnalysisResult;
use mago_analyzer::external::ExternalAnalysisSession;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::reference::SymbolReferences;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_names::resolver::NameResolver;
use mago_syntax::parser::parse_file_with_settings;
use mago_syntax::settings::ParserSettings;
use mago_word::Word;

use crate::error::OrchestratorError;

/// Installs the returns marked methods take from their bodies, analyzing the host `files`
/// that declare them against the populated `codebase`.
///
/// # Errors
///
/// Returns [`OrchestratorError`] when analyzing a declaring file fails.
pub(crate) fn resolve_body_returns(
    codebase: &mut CodebaseMetadata,
    files: &[Arc<File>],
    plugin_registry: &PluginRegistry,
    settings: &Settings,
    parser_settings: ParserSettings,
    session: Option<&ExternalAnalysisSession>,
) -> Result<(), OrchestratorError> {
    let marked = codebase
        .function_likes
        .iter()
        .filter(|(_, method)| method.return_from_body.is_some())
        .map(|(key, method)| (*key, method.span.file_id))
        .collect::<Vec<_>>();

    if marked.is_empty() {
        return Ok(());
    }

    let declaring = marked.iter().map(|(_, file_id)| *file_id).collect::<HashSet<_>>();
    let files = files.iter().filter(|file| declaring.contains(&file.id)).cloned().collect::<Vec<_>>();
    let mut resolved: HashMap<(Word, Word), TUnion> = HashMap::default();

    for _ in 0..=marked.len() {
        let populated: &CodebaseMetadata = codebase;
        let rounds = files
            .par_iter()
            .map_init(LocalArena::new, |arena, file| -> Result<_, OrchestratorError> {
                let returns = {
                    let program = parse_file_with_settings(arena, file, parser_settings);
                    let resolved_names = NameResolver::new(arena).resolve(program);
                    let mut analyzer =
                        Analyzer::new(arena, file, &resolved_names, populated, plugin_registry, settings.clone());
                    if let Some(session) = session {
                        analyzer = analyzer.with_external_analysis_session(session);
                    }

                    let mut result = AnalysisResult::new(SymbolReferences::new());
                    analyzer.analyze_with_artifacts(program, &mut result)?.body_returns
                };

                arena.reset();
                Ok(returns)
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut changed = false;
        for (key, returned) in rounds.into_iter().flatten() {
            if !is_applied_declaration(codebase, key, &returned) || resolved.get(&key) == Some(&returned) {
                continue;
            }

            let Some(method) = codebase.function_likes.get_mut(&key) else {
                continue;
            };

            let span = method.return_type_declaration_metadata.as_ref().map_or(method.span, |native| native.span);
            method.return_type_metadata = Some(TypeMetadata::from_docblock(returned.clone(), span));
            resolved.insert(key, returned);
            changed = true;
        }

        if !changed {
            break;
        }
    }

    for (key, _) in marked {
        if resolved.contains_key(&key) {
            continue;
        }

        if let Some(method) = codebase.function_likes.get_mut(&key)
            && let Some(issue) = method.return_from_body.clone()
        {
            method.issues.push(issue);
        }
    }

    Ok(())
}

// A returned type resolves the declaration when it is the class the native return names, or a
// descendant, with every type argument known.
fn is_applied_declaration(codebase: &CodebaseMetadata, key: (Word, Word), returned: &TUnion) -> bool {
    let Some(native) = codebase
        .function_likes
        .get(&key)
        .and_then(|method| method.return_type_declaration_metadata.as_ref())
        .and_then(|native| native.type_union.get_single_named_object())
    else {
        return false;
    };

    let Some(applied) = returned.get_single_named_object() else {
        return false;
    };

    let Some(arguments) = &applied.type_parameters else {
        return false;
    };

    codebase.is_instance_of(applied.name.as_bytes(), native.name.as_bytes())
        && arguments.iter().all(|argument| !argument.is_mixed() && !argument.has_template_types())
}
