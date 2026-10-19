use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;
use std::sync::OnceLock;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::reference::SymbolReferenceKind;
use mago_codex::reference::SymbolReferences;
use mago_codex::symbol::SymbolIdentifier;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_extension::PayloadReader;
use mago_extension::PayloadWriter;
use mago_extension::source::SourceSnapshot;
use mago_names::ResolvedNames;
use mago_reporting::Annotation;
use mago_reporting::AnnotationKind;
use mago_reporting::Issue;
use mago_reporting::IssueCollection;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Node;
use mago_syntax::cst::NodeKind;
use mago_syntax::cst::Program;
use mago_syntax::dialect::Dialect;
use mago_text_edit::Safety;
use mago_text_edit::TextEdit;
use mago_text_edit::TextRange;
use mago_word::ascii_lowercase_constant_name_word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::analysis_result::AnalysisResult;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::ResolvedMethodCall;
use crate::plugin::hook::StaticCall;
use crate::utils::names::member_namespace;
use crate::utils::names::namespace_function_name;
use crate::utils::names::namespace_member;

use super::ExternalAnalysisSession;
use super::ExternalPlugin;
use super::FileReads;
use super::NODE_REQUIREMENT_ARGUMENT_TYPES;
use super::NODE_REQUIREMENT_RECEIVER_TYPE;
use super::NODE_REQUIREMENT_TARGET_EXPRESSION_TYPES;
use super::NODE_REQUIREMENT_VARIABLE_DEFINEDNESS;
use super::NodeAnalysisRequirements;
use super::error::ExternalAnalyzerError;
use super::error::protocol;
use super::protocol;

pub(super) const BEFORE_ANALYSIS_REQUEST: u16 = 5;
pub(super) const AFTER_FILE_ANALYSIS_REQUEST: u16 = 6;
pub(super) const AFTER_ANALYSIS_REQUEST: u16 = 7;
const ANALYSIS_QUERY_REQUEST: u16 = 8;
pub(super) const AFTER_FILE_ANALYSIS_BATCH_REQUEST: u16 = 9;
const SYMBOL_REFERENCE_QUERY_REQUEST: u16 = 10;
const BEFORE_ANALYSIS_RESPONSE: u16 = 0x8005;
const AFTER_FILE_ANALYSIS_RESPONSE: u16 = 0x8006;
const AFTER_ANALYSIS_RESPONSE: u16 = 0x8007;
const ANALYSIS_QUERY_RESPONSE: u16 = 0x8008;
const AFTER_FILE_ANALYSIS_BATCH_RESPONSE: u16 = 0x8009;
const SYMBOL_REFERENCE_QUERY_RESPONSE: u16 = 0x800A;

const GET_EXPRESSION_TYPES: u8 = 1;
const GET_ALL_EXPRESSION_TYPES: u8 = 2;
const GET_INFERRED_RETURN_TYPES: u8 = 3;
const GET_INFERRED_YIELD_KEY_TYPES: u8 = 4;
const GET_INFERRED_YIELD_VALUE_TYPES: u8 = 5;
const GET_SOURCE_FILE: u8 = 6;
const GET_REFERENCES_TO: u8 = 1;
const GET_REFERENCES_FROM: u8 = 2;
const MAXIMUM_ISSUES: usize = 1_000_000;
const MAXIMUM_ANNOTATIONS: usize = 0x0001_0000;
const MAXIMUM_EDITS: usize = 0x0001_0000;
const MAXIMUM_NOTES: usize = 0x0001_0000;
const MAXIMUM_TYPE_QUERIES: usize = 1_000_000;
const MAXIMUM_REFERENCE_QUERIES: usize = 1_000_000;
const MAXIMUM_REFERENCES: usize = 10_000_000;
const EXPRESSION_TYPE_PREFETCH: usize = 16;

#[derive(Debug, Default)]
pub(super) struct LifecycleEffects {
    pub issues: IssueCollection,
    pub references: SymbolReferences,
    pub references_by_file: HashMap<FileId, SymbolReferences>,
}

pub(super) struct SymbolReferenceStore<'analysis> {
    references: &'analysis SymbolReferences,
    by_target: OnceLock<HashMap<SymbolIdentifier, Vec<RecordedReference>>>,
    by_source: OnceLock<HashMap<ReferenceOrigin, Vec<RecordedReference>>>,
}

impl<'analysis> SymbolReferenceStore<'analysis> {
    pub fn new(references: &'analysis SymbolReferences) -> Self {
        Self { references, by_target: OnceLock::new(), by_source: OnceLock::new() }
    }

    fn references_to(&self, target: SymbolIdentifier) -> &[RecordedReference] {
        self.by_target
            .get_or_init(|| {
                let mut index = HashMap::<SymbolIdentifier, Vec<RecordedReference>>::default();
                self.references.for_each_reference(|source, target, kind| {
                    index.entry(target).or_default().push(RecordedReference { source, target, kind });
                });
                for references in index.values_mut() {
                    references.sort_unstable();
                }
                index
            })
            .get(&target)
            .map_or(&[], Vec::as_slice)
    }

    fn references_from(&self, source: ReferenceOrigin) -> &[RecordedReference] {
        self.by_source
            .get_or_init(|| {
                let mut index = HashMap::<ReferenceOrigin, Vec<RecordedReference>>::default();
                self.references.for_each_reference(|source, target, kind| {
                    index.entry(source).or_default().push(RecordedReference { source, target, kind });
                });
                for references in index.values_mut() {
                    references.sort_unstable();
                }
                index
            })
            .get(&source)
            .map_or(&[], Vec::as_slice)
    }
}

#[derive(Debug)]
pub struct FileAnalysisSnapshot {
    file_id: mago_database::file::FileId,
    name: Arc<[u8]>,
    size: u32,
    encoded_source: Box<[u8]>,
    encoded_target_source: Box<[u8]>,
    encoded_target_analysis: Box<[u8]>,
    encoded_types: Box<[u8]>,
    expression_types: HashMap<(u32, u32), Range<usize>>,
    inferred_return_types: Vec<Range<usize>>,
    inferred_yield_key_types: Vec<Range<usize>>,
    inferred_yield_value_types: Vec<Range<usize>>,
    references: ReferenceSummary,
    node_analysis_targets: usize,
}

#[derive(Debug, Clone, Copy)]
struct ReferenceSummary {
    body: u64,
    signature: u64,
    maps: u64,
}

/// Number of completed files sent through one external after-file request.
pub const AFTER_FILE_ANALYSIS_BATCH_SIZE: usize = 32;

type NodeTargetKey = (NodeKind, u32, u32);

struct NodeAnalysisTarget<'ast, 'arena> {
    node: Node<'ast, 'arena>,
    requirements: u8,
    targeted_hook_routes: Vec<u32>,
    /// The kind of node a PHP# method call means when it is not a method call: [`NodeKind::StaticMethodCall`] for
    /// `Calc.make()`, [`NodeKind::FunctionCall`] for `global.max()` or `Pdf.render()`, and
    /// [`NodeKind::Instantiation`] for `PaymentResult.Declined(…)`, which builds an enum case.
    meaning: Option<NodeKind>,
}

struct NodeAnalysisPlan<'ast, 'arena> {
    targets: Vec<NodeAnalysisTarget<'ast, 'arena>>,
    by_node: HashMap<NodeTargetKey, usize>,
}

impl<'ast, 'arena> NodeAnalysisPlan<'ast, 'arena> {
    fn configuration(&self, node: Node<'ast, 'arena>) -> Option<bool> {
        let span = node.span();
        self.by_node
            .get(&(node.kind(), span.start.offset, span.end.offset))
            .map(|index| self.targets[*index].requirements & super::NODE_REQUIREMENT_TARGET_SUBTREE != 0)
    }
}

fn build_node_analysis_plan<'ast, 'arena>(
    program: &'ast Program<'arena>,
    artifacts: &AnalysisArtifacts,
    resolved_names: &ResolvedNames<'arena>,
    codebase: &CodebaseMetadata,
    requirements: &NodeAnalysisRequirements,
) -> NodeAnalysisPlan<'ast, 'arena> {
    let (method_calls, method_call_targets) = if requirements.method_call_hooks.is_empty() {
        (None, None)
    } else {
        let mut targets = artifacts.resolved_method_calls.iter().collect::<Vec<_>>();
        targets.sort_unstable_by_key(|target| target.span);
        let mut calls = HashMap::<(u32, u32), Range<usize>>::default();
        for (index, call) in targets.iter().enumerate() {
            calls.entry(call.span).and_modify(|range| range.end = index + 1).or_insert(index..index + 1);
        }
        (Some(calls), Some(targets))
    };
    let mut targets = Vec::new();
    let mut by_node = HashMap::default();
    let mut stack = Vec::with_capacity(64);
    stack.push(Node::Program(program));
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        let span = node.span();
        let mut requested = requirements.requirements(kind);
        let mut targeted_hook_routes = Vec::new();
        // A PHP# call is the kind of call it means, as the analyzer reads it: `global.max()` and `Pdf.render()` after
        // `import Vendor.Pdf;` call a plain PHP function, and `Calc.make()`, whose object the binder bound to a class,
        // a static method. A hook that targets that kind receives it, as it receives `max()` or `Calc::make()` in PHP.
        // `PaymentResult.Declined(…)` builds an enum case, spec section 20, as `new PaymentResult\Declined(…)` does.
        let meaning = match node {
            Node::MethodCall(call) if program.dialect.is_sharp() => {
                if namespace_function_name(resolved_names, codebase, call).is_some() {
                    Some(NodeKind::FunctionCall)
                } else if let Some(class) = resolved_names.static_call_class(call)
                    && let ClassLikeMemberSelector::Identifier(member) = &call.method
                    && built_data_case(resolved_names, codebase, class, member.value).is_some()
                {
                    Some(NodeKind::Instantiation)
                } else {
                    StaticCall::from_method_call(call, resolved_names).map(|_| NodeKind::StaticMethodCall)
                }
            }
            _ => None,
        };
        if let Some(meaning) = meaning {
            requested |= requirements.requirements(meaning);
        }

        if is_method_call_kind(kind)
            && let Some(call_range) =
                method_calls.as_ref().and_then(|calls| calls.get(&(span.start.offset, span.end.offset)))
        {
            let calls = &method_call_targets.as_deref().unwrap_or_default()[call_range.clone()];
            for hook in requirements.method_call_hooks.iter() {
                if method_call_matches_hook(calls, hook, codebase) {
                    requested |= hook.requirements;
                    targeted_hook_routes.push(hook.route);
                }
            }
        }

        if !requirements.class_like_hooks.is_empty()
            && let Some(class_like) = class_like_name(node, resolved_names)
        {
            for hook in requirements.class_like_hooks.iter() {
                if class_like_matches_hook(class_like, hook, codebase) {
                    requested |= hook.requirements;
                    targeted_hook_routes.push(hook.route);
                }
            }
        }

        if requirements.targets()[kind as usize]
            || meaning.is_some_and(|meaning| requirements.targets()[meaning as usize])
            || !targeted_hook_routes.is_empty()
        {
            let index = targets.len();
            by_node.insert((kind, span.start.offset, span.end.offset), index);
            targets.push(NodeAnalysisTarget { node, requirements: requested, targeted_hook_routes, meaning });
        }

        let start = stack.len();
        node.visit_children(|child| stack.push(child));
        stack[start..].reverse();
    }

    NodeAnalysisPlan { targets, by_node }
}

/// The names the analysis finds a PHP# file writes where name resolution alone can't tell, as a host reads them in
/// PHP, each as `(start, end, name, binding)` with the binding code of [`SourceSnapshot::rename`]:
///
/// - `Pdf.render()` and `Pdf.DEFAULT_DPI` after `import Vendor.Pdf;`, where no class is named `Vendor\Pdf`, name the
///   namespace `Vendor\Pdf` at `Pdf` and its function `Vendor\Pdf\render` or constant `Vendor\Pdf\DEFAULT_DPI` at the
///   member, as `Pdf\render()` does in PHP. `global.max()` names `max` already, spec section 23.
/// - `PaymentResult.Declined(reason: "expired")` builds the case `Declined` of the enum `PaymentResult`, spec section
///   20, so it names the class the case runs as at `Declined`, as `new PaymentResult\Declined(…)` does.
/// - The pattern `PaymentResult.Paid(string id)` names the enum `PaymentResult` at `PaymentResult`, as the value
///   pattern `Plan.Starter` names `Plan`, and its case has no name of its own.
///
/// None in a PHP file.
fn analyzed_names(
    program: &Program<'_>,
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
) -> Vec<(u32, u32, Vec<u8>, u8)> {
    let mut names = Vec::new();
    if !program.dialect.is_sharp() {
        return names;
    }

    let mut stack = vec![Node::Program(program)];
    while let Some(node) = stack.pop() {
        if let Node::TypePattern(pattern) = node
            && let Hint::Identifier(hint) = &pattern.hint
            && let Some(case) = data_case(codebase, resolved_names.get(hint))
            && let Some(r#enum) = case.direct_parent_class.and_then(|name| codebase.get_class_like(name.as_bytes()))
            && let Some(dot) = hint.value().iter().rposition(|byte| *byte == b'.')
        {
            let start = hint.span().start.offset;
            names.push((start, start + dot as u32, r#enum.original_name.as_bytes().to_vec(), SNAPSHOT_CLASS_BINDING));
        }

        let class_member = match node {
            Node::MethodCall(call) => resolved_names.static_call_class(call).map(|class| (class, &call.method)),
            Node::PropertyAccess(access) => {
                resolved_names.static_property_class(access).map(|class| (class, &access.property))
            }
            _ => None,
        };
        if let Some((class, ClassLikeMemberSelector::Identifier(member))) = class_member {
            let member_span = (member.span.start.offset, member.span.end.offset);
            if matches!(node, Node::MethodCall(_))
                && let Some(case) = built_data_case(resolved_names, codebase, class, member.value)
            {
                names.push((
                    member_span.0,
                    member_span.1,
                    case.original_name.as_bytes().to_vec(),
                    SNAPSHOT_CLASS_BINDING,
                ));
            } else if class.name.value() != b"global"
                && let Some(namespace) = member_namespace(resolved_names, codebase, &class.name, member.value)
            {
                let span = class.name.span();
                names.push((span.start.offset, span.end.offset, namespace.as_bytes().to_vec(), 0));
                let full_name = namespace_member(namespace, member.value);
                names.push((member_span.0, member_span.1, full_name.as_bytes().to_vec(), 0));
            }
        }

        node.visit_children(|child| stack.push(child));
    }

    names
}

/// The binding code a snapshot's resolved name carries for a class, as `SourceSnapshot` writes `Binding::Class`.
const SNAPSHOT_CLASS_BINDING: u8 = 1;

/// The class a case of an enum with data runs as, spec section 20, when `name` names one.
fn data_case<'codebase>(codebase: &'codebase CodebaseMetadata, name: &[u8]) -> Option<&'codebase ClassLikeMetadata> {
    codebase.get_class_like(name).filter(|case| case.flags.is_enum_case_class())
}

/// The class of the case a PHP# `Enum.Case(…)` builds, spec section 20, when `class` names an enum with data and
/// `member` one of its cases.
fn built_data_case<'codebase>(
    resolved_names: &ResolvedNames<'_>,
    codebase: &'codebase CodebaseMetadata,
    class: &ConstantAccess<'_>,
    member: &[u8],
) -> Option<&'codebase ClassLikeMetadata> {
    let enum_name = resolved_names.get(&class.name);

    data_case(codebase, &[enum_name, b"\\", member].concat())
}

fn is_method_call_kind(kind: mago_syntax::cst::NodeKind) -> bool {
    matches!(
        kind,
        mago_syntax::cst::NodeKind::MethodCall
            | mago_syntax::cst::NodeKind::NullSafeMethodCall
            | mago_syntax::cst::NodeKind::StaticMethodCall
    )
}

fn method_call_matches_hook(
    calls: &[&ResolvedMethodCall],
    hook: &super::MethodCallAnalysisHookRegistration,
    codebase: &CodebaseMetadata,
) -> bool {
    calls.iter().any(|call_target| {
        hook.targets
            .iter()
            .any(|target| target.matches(codebase, call_target.class.as_bytes(), call_target.method.as_bytes()))
    })
}

fn class_like_name<'arena>(node: Node<'_, 'arena>, resolved_names: &ResolvedNames<'arena>) -> Option<&'arena [u8]> {
    match node {
        Node::Class(class) => resolved_names.resolve(&class.name),
        Node::Enum(r#enum) => resolved_names.resolve(&r#enum.name),
        Node::Interface(interface) => resolved_names.resolve(&interface.name),
        Node::Trait(r#trait) => resolved_names.resolve(&r#trait.name),
        _ => None,
    }
}

fn class_like_matches_hook(
    class_like: &[u8],
    hook: &super::ClassLikeAnalysisHookRegistration,
    codebase: &CodebaseMetadata,
) -> bool {
    let class_like = ascii_lowercase_word(class_like);
    let Some(metadata) = codebase.get_class_like(class_like.as_bytes()) else {
        return false;
    };

    hook.targets.iter().any(|ancestor| {
        if metadata.name != *ancestor
            && (metadata.all_parent_classes.contains(ancestor) || metadata.all_parent_interfaces.contains(ancestor))
        {
            return true;
        }

        if codebase.class_like_aliases.is_empty() {
            return false;
        }

        codebase.class_like_aliases.get(ancestor).is_some_and(|actual| {
            metadata.name != *actual
                && (metadata.all_parent_classes.contains(actual) || metadata.all_parent_interfaces.contains(actual))
        })
    })
}

pub(super) fn has_node_analysis_target(
    program: &Program<'_>,
    artifacts: &AnalysisArtifacts,
    resolved_names: &ResolvedNames<'_>,
    codebase: &CodebaseMetadata,
    requirements: &NodeAnalysisRequirements,
) -> bool {
    !build_node_analysis_plan(program, artifacts, resolved_names, codebase, requirements).targets.is_empty()
}

impl FileAnalysisSnapshot {
    /// Builds a compact, thread-safe snapshot of one file's lazy analysis data.
    ///
    /// Without `node_analysis`, the node-analysis hooks skip the file. Its syntax still marks
    /// their targets, so an after-analysis hook reads it as when they run.
    ///
    /// # Errors
    ///
    /// Returns an error when the source snapshot or an inferred type cannot be represented by the extension protocol.
    pub fn new(
        file: &File,
        program: &Program<'_>,
        resolved_names: &ResolvedNames<'_>,
        artifacts: &AnalysisArtifacts,
        codebase: &CodebaseMetadata,
        node_analysis_requirements: Option<&NodeAnalysisRequirements>,
        node_analysis: bool,
    ) -> Result<Self, ExternalAnalyzerError> {
        let dialect = program.dialect;
        let node_analysis_plan = node_analysis_requirements
            .map(|requirements| build_node_analysis_plan(program, artifacts, resolved_names, codebase, requirements));
        let analyzed = analyzed_names(program, resolved_names, codebase);
        let mut source = SourceSnapshot::complete_with_target_filter(program, resolved_names, |node| {
            node_analysis_plan.as_ref().is_some_and(|plan| plan.configuration(node).is_some())
        })
        .map_err(|error| {
            protocol(format!("failed to retain syntax for `{}`: {error}", String::from_utf8_lossy(&file.name)))
        })?;
        source.rename(&analyzed);
        let matched_target_count = source.target_count();

        let mut source_writer = PayloadWriter::with_capacity(source.encoded_len());
        source.write_to(&mut source_writer).map_err(|error| {
            protocol(format!("failed to encode retained syntax for `{}`: {error}", String::from_utf8_lossy(&file.name)))
        })?;

        let (encoded_target_source, encoded_target_analysis, node_analysis_targets) = if let Some(plan) =
            node_analysis_plan.as_ref().filter(|_| node_analysis && matched_target_count != 0)
        {
            let target_source = SourceSnapshot::targeted_with_filter(
                program,
                resolved_names,
                |node| plan.configuration(node),
                node_analysis_requirements.is_some_and(NodeAnalysisRequirements::includes_source_text),
            )
            .map_err(|error| {
                protocol(format!(
                    "failed to retain targeted syntax for `{}`: {error}",
                    String::from_utf8_lossy(&file.name)
                ))
            })?;
            if let Some(mut target_source) = target_source {
                target_source.rename(&analyzed);
                let target_count = target_source.target_count();
                if target_count != matched_target_count {
                    return Err(protocol(format!(
                        "complete syntax contains {matched_target_count} targets, but targeted syntax contains {target_count}"
                    )));
                }
                let mut target_source_writer = PayloadWriter::with_capacity(target_source.encoded_len());
                target_source.write_to(&mut target_source_writer).map_err(|error| {
                    protocol(format!(
                        "failed to encode targeted syntax for `{}`: {error}",
                        String::from_utf8_lossy(&file.name)
                    ))
                })?;
                let mut target_analysis_writer = PayloadWriter::new();
                write_target_analysis(&mut target_analysis_writer, artifacts, codebase, dialect, plan, target_count)?;
                (
                    target_source_writer.finish().into_boxed_slice(),
                    target_analysis_writer.finish().into_boxed_slice(),
                    target_count,
                )
            } else {
                (Box::default(), Box::default(), 0)
            }
        } else {
            (Box::default(), Box::default(), 0)
        };

        let mut writer = PayloadWriter::new();
        let mut type_handles = Vec::new();
        let mut expression_types =
            HashMap::with_capacity_and_hasher(artifacts.expression_types.len(), foldhash::fast::RandomState::default());
        for (span, union) in &artifacts.expression_types {
            expression_types.insert(
                *span,
                encode_snapshot_type(&mut writer, union, &mut type_handles, codebase, dialect).map_err(|error| {
                    protocol(format!(
                        "failed to retain expression type at {}:{} in `{}`: {error}",
                        span.0,
                        span.1,
                        String::from_utf8_lossy(&file.name)
                    ))
                })?,
            );
        }

        let inferred_return_types = encode_snapshot_types(
            &mut writer,
            artifacts.inferred_return_types.iter().map(AsRef::as_ref),
            &mut type_handles,
            codebase,
            dialect,
        )?;
        let inferred_yield_key_types = encode_snapshot_types(
            &mut writer,
            &artifacts.inferred_yield_key_types,
            &mut type_handles,
            codebase,
            dialect,
        )?;
        let inferred_yield_value_types = encode_snapshot_types(
            &mut writer,
            &artifacts.inferred_yield_value_types,
            &mut type_handles,
            codebase,
            dialect,
        )?;

        Ok(Self {
            file_id: file.id,
            name: Arc::from(file.name.as_ref()),
            size: file.size,
            encoded_source: source_writer.finish().into_boxed_slice(),
            encoded_target_source,
            encoded_target_analysis,
            encoded_types: writer.finish().into_boxed_slice(),
            expression_types,
            inferred_return_types,
            inferred_yield_key_types,
            inferred_yield_value_types,
            references: ReferenceSummary::from(&artifacts.symbol_references),
            node_analysis_targets,
        })
    }

    #[must_use]
    pub const fn file_id(&self) -> mago_database::file::FileId {
        self.file_id
    }

    #[must_use]
    pub const fn has_node_analysis_targets(&self) -> bool {
        self.node_analysis_targets != 0
    }
}

impl From<&SymbolReferences> for ReferenceSummary {
    fn from(references: &SymbolReferences) -> Self {
        Self {
            body: references.count_body_references() as u64,
            signature: references.count_signature_references() as u64,
            maps: references.total_map_entries() as u64,
        }
    }
}

impl ReferenceSummary {
    fn write_to(self, writer: &mut PayloadWriter) {
        writer.write_u64(self.body);
        writer.write_u64(self.signature);
        writer.write_u64(self.maps);
    }
}

fn encode_snapshot_types<'type_info>(
    writer: &mut PayloadWriter,
    types: impl IntoIterator<Item = &'type_info TUnion>,
    handles: &mut Vec<Cow<'type_info, TUnion>>,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
) -> Result<Vec<Range<usize>>, ExternalAnalyzerError> {
    types.into_iter().map(|ty| encode_snapshot_type(writer, ty, handles, codebase, dialect)).collect()
}

fn encode_snapshot_type<'type_info>(
    writer: &mut PayloadWriter,
    ty: &'type_info TUnion,
    handles: &mut Vec<Cow<'type_info, TUnion>>,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
) -> Result<Range<usize>, ExternalAnalyzerError> {
    handles.clear();
    let start = writer.len();
    protocol::encode_union_snapshot(writer, ty, handles, codebase, dialect, 0)?;
    Ok(start..writer.len())
}

fn write_target_analysis(
    writer: &mut PayloadWriter,
    artifacts: &AnalysisArtifacts,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
    plan: &NodeAnalysisPlan<'_, '_>,
    expected_count: usize,
) -> Result<(), ExternalAnalyzerError> {
    if plan.targets.len() != expected_count {
        return Err(protocol(format!(
            "targeted syntax contains {expected_count} targets, but the analysis plan contains {}",
            plan.targets.len()
        )));
    }
    writer.write_u32(u32::try_from(expected_count).map_err(|_| protocol("too many node-analysis targets"))?);
    for target in &plan.targets {
        let requested = target.requirements;
        writer.write_u8(requested);
        if requested & NODE_REQUIREMENT_TARGET_EXPRESSION_TYPES != 0 {
            write_optional_expression_type(writer, artifacts, codebase, dialect, Some(target.node.span()))?;
        }
        if requested & NODE_REQUIREMENT_RECEIVER_TYPE != 0 {
            write_optional_expression_type(writer, artifacts, codebase, dialect, receiver_span(target.node))?;
        }
        if requested & NODE_REQUIREMENT_ARGUMENT_TYPES != 0 {
            write_argument_types(writer, artifacts, codebase, dialect, target.node)?;
        }
        if requested & NODE_REQUIREMENT_VARIABLE_DEFINEDNESS != 0 {
            write_variable_definedness(writer, artifacts, target.node.span())?;
        }
        writer.write_u32(
            u32::try_from(target.targeted_hook_routes.len())
                .map_err(|_| protocol("too many targeted analysis hook routes matched one node"))?,
        );
        for route in &target.targeted_hook_routes {
            writer.write_u32(*route);
        }
        writer.write_bool(target.meaning.is_some());
        if let Some(meaning) = target.meaning {
            writer.write_u16(meaning as u16);
        }
    }

    Ok(())
}

fn write_variable_definedness(
    writer: &mut PayloadWriter,
    artifacts: &AnalysisArtifacts,
    span: Span,
) -> Result<(), ExternalAnalyzerError> {
    let variables = artifacts.variable_definedness.get(&(span.start.offset, span.end.offset));
    writer.write_bool(variables.is_some());
    let Some(variables) = variables else {
        return Ok(());
    };

    writer.write_u32(u32::try_from(variables.len()).map_err(|_| protocol("too many variables in scope"))?);
    let mut variables = variables.iter().collect::<Vec<_>>();
    variables.sort_unstable_by_key(|(variable, _)| variable.as_bytes());
    for (variable, definedness) in variables {
        writer.write_bytes(variable.as_bytes())?;
        writer.write_u8(*definedness as u8);
    }

    Ok(())
}

fn write_optional_expression_type(
    writer: &mut PayloadWriter,
    artifacts: &AnalysisArtifacts,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
    span: Option<Span>,
) -> Result<(), ExternalAnalyzerError> {
    let ty = span.and_then(|span| artifacts.expression_types.get(&(span.start.offset, span.end.offset)));
    writer.write_bool(ty.is_some());
    if let Some(ty) = ty {
        write_type(writer, ty, codebase, dialect)?;
    }
    Ok(())
}

fn receiver_span(node: Node<'_, '_>) -> Option<Span> {
    match node {
        Node::MethodCall(call) => Some(call.object.span()),
        Node::NullSafeMethodCall(call) => Some(call.object.span()),
        Node::StaticMethodCall(call) => Some(call.class.span()),
        _ => None,
    }
}

fn write_argument_types(
    writer: &mut PayloadWriter,
    artifacts: &AnalysisArtifacts,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
    node: Node<'_, '_>,
) -> Result<(), ExternalAnalyzerError> {
    let arguments = match node {
        Node::FunctionCall(call) => Some(&call.argument_list.arguments),
        Node::MethodCall(call) => Some(&call.argument_list.arguments),
        Node::NullSafeMethodCall(call) => Some(&call.argument_list.arguments),
        Node::StaticMethodCall(call) => Some(&call.argument_list.arguments),
        _ => None,
    };
    let Some(arguments) = arguments else {
        writer.write_u32(0);
        return Ok(());
    };

    writer.write_u32(u32::try_from(arguments.len()).map_err(|_| protocol("too many call arguments"))?);
    for argument in arguments.iter() {
        write_optional_expression_type(writer, artifacts, codebase, dialect, Some(argument.value().span()))?;
    }
    Ok(())
}

pub(super) enum AnalysisStore<'analysis> {
    File {
        file: &'analysis File,
        program: &'analysis Program<'analysis>,
        resolved_names: &'analysis ResolvedNames<'analysis>,
        artifacts: &'analysis AnalysisArtifacts,
        codebase: &'analysis CodebaseMetadata,
        node_analysis_targets: Option<&'analysis [bool; NodeKind::COUNT]>,
    },
    Project(&'analysis [Arc<FileAnalysisSnapshot>]),
}

impl AnalysisStore<'_> {
    fn file(&self, name: &[u8]) -> Option<FileView<'_>> {
        match self {
            Self::File { file, program, resolved_names, artifacts, codebase, node_analysis_targets }
                if file.name.as_ref() == name =>
            {
                Some(FileView::Artifacts(file, program, resolved_names, artifacts, codebase, *node_analysis_targets))
            }
            Self::File { .. } => None,
            Self::Project(files) => {
                files.iter().find(|file| file.name.as_ref() == name).map(|file| FileView::Snapshot(file.as_ref()))
            }
        }
    }
}

enum FileView<'analysis> {
    Artifacts(
        &'analysis File,
        &'analysis Program<'analysis>,
        &'analysis ResolvedNames<'analysis>,
        &'analysis AnalysisArtifacts,
        &'analysis CodebaseMetadata,
        Option<&'analysis [bool; NodeKind::COUNT]>,
    ),
    Snapshot(&'analysis FileAnalysisSnapshot),
}

enum TypeView<'analysis> {
    Union(&'analysis TUnion, &'analysis CodebaseMetadata, Dialect),
    Encoded(&'analysis [u8]),
}

impl TypeView<'_> {
    fn write_to(self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Union(ty, codebase, dialect) => write_type(writer, ty, codebase, dialect),
            Self::Encoded(bytes) => {
                writer.write_raw(bytes);
                Ok(())
            }
        }
    }
}

impl FileView<'_> {
    fn name(&self) -> &[u8] {
        match self {
            Self::Artifacts(file, ..) => &file.name,
            Self::Snapshot(file) => &file.name,
        }
    }

    fn size(&self) -> u32 {
        match self {
            Self::Artifacts(file, ..) => file.size,
            Self::Snapshot(file) => file.size,
        }
    }

    fn expression_count(&self) -> usize {
        match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.expression_types.len(),
            Self::Snapshot(file) => file.expression_types.len(),
        }
    }

    fn expression_type(&self, span: &(u32, u32)) -> Option<TypeView<'_>> {
        match self {
            Self::Artifacts(_, program, _, artifacts, codebase, _) => {
                artifacts.expression_types.get(span).map(|ty| TypeView::Union(ty, codebase, program.dialect))
            }
            Self::Snapshot(file) => file
                .expression_types
                .get(span)
                .and_then(|range| file.encoded_types.get(range.clone()))
                .map(TypeView::Encoded),
        }
    }

    fn nearby_expression_spans(&self, requested: &[(u32, u32)]) -> Vec<(u32, u32)> {
        let mut spans = match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.expression_types.keys().copied().collect::<Vec<_>>(),
            Self::Snapshot(file) => file.expression_types.keys().copied().collect::<Vec<_>>(),
        };
        spans.sort_unstable();

        let requested = requested.iter().copied().collect::<HashSet<_>>();
        let mut selected = HashSet::default();
        let mut nearby = Vec::with_capacity(EXPRESSION_TYPE_PREFETCH.min(spans.len()));
        for span in &requested {
            let index = spans.binary_search(span).unwrap_or_else(|index| index);
            let start = index.saturating_sub(2);
            let end = (start + EXPRESSION_TYPE_PREFETCH).min(spans.len());
            for candidate in &spans[start..end] {
                if !requested.contains(candidate) && selected.insert(*candidate) {
                    nearby.push(*candidate);
                }
            }
        }

        nearby
    }

    fn inferred_return_count(&self) -> usize {
        match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.inferred_return_types.len(),
            Self::Snapshot(file) => file.inferred_return_types.len(),
        }
    }

    fn write_expression_types(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        let mut spans = match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.expression_types.keys().copied().collect::<Vec<_>>(),
            Self::Snapshot(file) => file.expression_types.keys().copied().collect::<Vec<_>>(),
        };

        spans.sort_unstable();
        writer.write_u32(u32::try_from(spans.len()).map_err(|_| protocol("too many expression types"))?);
        for span in spans {
            writer.write_u32(span.0);
            writer.write_u32(span.1);
            let ty = self
                .expression_type(&span)
                .ok_or_else(|| protocol("expression type disappeared while encoding analysis artifacts"))?;
            ty.write_to(writer).map_err(|error| {
                protocol(format!(
                    "failed to encode expression type at {}:{} in `{}`: {error}",
                    span.0,
                    span.1,
                    String::from_utf8_lossy(self.name())
                ))
            })?;
        }

        Ok(())
    }

    fn write_expression_type_snapshot(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(_, program, _, artifacts, codebase, _) => {
                let mut spans = artifacts.expression_types.keys().copied().collect::<Vec<_>>();
                spans.sort_unstable();
                let mut encoded = PayloadWriter::new();
                let mut handles = Vec::new();
                let mut records = Vec::with_capacity(spans.len());
                for span in spans {
                    let ty = artifacts
                        .expression_types
                        .get(&span)
                        .ok_or_else(|| protocol("expression type disappeared while snapshotting analysis artifacts"))?;
                    let range = encode_snapshot_type(&mut encoded, ty, &mut handles, codebase, program.dialect)?;
                    records.push((span, range));
                }
                write_expression_type_records(writer, &records, &encoded.finish())
            }
            Self::Snapshot(file) => {
                let mut records =
                    file.expression_types.iter().map(|(span, range)| (*span, range.clone())).collect::<Vec<_>>();
                records.sort_unstable_by_key(|(span, _)| *span);
                let length = records.iter().map(|(_, range)| range.end).max().unwrap_or(0);
                let encoded = file
                    .encoded_types
                    .get(..length)
                    .ok_or_else(|| protocol("expression type snapshot lies outside retained analysis artifacts"))?;
                write_expression_type_records(writer, &records, encoded)
            }
        }
    }

    fn write_inferred_return_types(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(_, program, _, artifacts, codebase, _) => {
                writer.write_u32(
                    u32::try_from(artifacts.inferred_return_types.len())
                        .map_err(|_| protocol("too many inferred types"))?,
                );
                for ty in &artifacts.inferred_return_types {
                    write_type(writer, ty, codebase, program.dialect)?;
                }
            }
            Self::Snapshot(file) => {
                write_encoded_types(writer, &file.encoded_types, &file.inferred_return_types)?;
            }
        }
        Ok(())
    }

    fn inferred_yield_key_count(&self) -> usize {
        match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.inferred_yield_key_types.len(),
            Self::Snapshot(file) => file.inferred_yield_key_types.len(),
        }
    }

    fn inferred_yield_value_count(&self) -> usize {
        match self {
            Self::Artifacts(_, _, _, artifacts, ..) => artifacts.inferred_yield_value_types.len(),
            Self::Snapshot(file) => file.inferred_yield_value_types.len(),
        }
    }

    fn write_inferred_yield_key_types(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(_, program, _, artifacts, codebase, _) => {
                write_types(writer, &artifacts.inferred_yield_key_types, codebase, program.dialect)
            }
            Self::Snapshot(file) => write_encoded_types(writer, &file.encoded_types, &file.inferred_yield_key_types),
        }
    }

    fn write_inferred_yield_value_types(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(_, program, _, artifacts, codebase, _) => {
                write_types(writer, &artifacts.inferred_yield_value_types, codebase, program.dialect)
            }
            Self::Snapshot(file) => write_encoded_types(writer, &file.encoded_types, &file.inferred_yield_value_types),
        }
    }

    fn write_reference_summary(&self, writer: &mut PayloadWriter) {
        match self {
            Self::Artifacts(_, _, _, artifacts, ..) => write_reference_summary(writer, &artifacts.symbol_references),
            Self::Snapshot(file) => file.references.write_to(writer),
        }
    }

    fn file_id(&self) -> mago_database::file::FileId {
        match self {
            Self::Artifacts(file, ..) => file.id,
            Self::Snapshot(file) => file.file_id,
        }
    }

    fn write_source_snapshot(&self, writer: &mut PayloadWriter) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(file, program, resolved_names, _, _, targets) => {
                let snapshot =
                    SourceSnapshot::complete_with_targets(program, resolved_names, *targets).map_err(|error| {
                        protocol(format!(
                            "failed to snapshot syntax for `{}`: {error}",
                            String::from_utf8_lossy(&file.name)
                        ))
                    })?;

                snapshot.write_to(writer).map_err(|error| {
                    protocol(format!("failed to encode syntax for `{}`: {error}", String::from_utf8_lossy(&file.name)))
                })
            }
            Self::Snapshot(file) => {
                writer.write_raw(&file.encoded_source);
                Ok(())
            }
        }
    }

    fn write_target_source_snapshot(
        &self,
        writer: &mut PayloadWriter,
        plan: Option<&NodeAnalysisPlan<'_, '_>>,
        include_trivia: bool,
    ) -> Result<(), ExternalAnalyzerError> {
        match self {
            Self::Artifacts(file, program, resolved_names, artifacts, codebase, _) => {
                let plan = plan.ok_or_else(|| protocol("targeted file summary is missing its node-analysis plan"))?;
                let snapshot = SourceSnapshot::targeted_with_filter(
                    program,
                    resolved_names,
                    |node| plan.configuration(node),
                    include_trivia,
                )
                .map_err(|error| {
                    protocol(format!(
                        "failed to snapshot targeted syntax for `{}`: {error}",
                        String::from_utf8_lossy(&file.name)
                    ))
                })?
                .ok_or_else(|| protocol("targeted file summary contains no matching syntax nodes"))?;
                snapshot.write_to(writer).map_err(|error| {
                    protocol(format!(
                        "failed to encode targeted syntax for `{}`: {error}",
                        String::from_utf8_lossy(&file.name)
                    ))
                })?;
                write_target_analysis(writer, artifacts, codebase, program.dialect, plan, snapshot.target_count())
            }
            Self::Snapshot(file) => {
                writer.write_raw(&file.encoded_target_source);
                writer.write_raw(&file.encoded_target_analysis);
                Ok(())
            }
        }
    }
}

fn write_expression_type_records(
    writer: &mut PayloadWriter,
    records: &[((u32, u32), Range<usize>)],
    encoded: &[u8],
) -> Result<(), ExternalAnalyzerError> {
    writer.write_u32(u32::try_from(records.len()).map_err(|_| protocol("too many expression types"))?);
    for (span, range) in records {
        writer.write_u32(span.0);
        writer.write_u32(span.1);
        writer.write_u32(u32::try_from(range.start).map_err(|_| protocol("expression type offset exceeds u32::MAX"))?);
        writer.write_u32(u32::try_from(range.len()).map_err(|_| protocol("expression type length exceeds u32::MAX"))?);
    }
    writer.write_bytes(encoded)?;
    Ok(())
}

pub(super) fn encode_before_analysis_request(
    generation: u64,
    plugins: &[u16],
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let writer = lifecycle_writer(BEFORE_ANALYSIS_REQUEST, generation, plugins)?;
    Ok(writer.finish())
}

pub(super) fn encode_after_file_analysis_request(
    generation: u64,
    plugins: &[u16],
    file: &File,
    program: &Program<'_>,
    resolved_names: &ResolvedNames<'_>,
    artifacts: &AnalysisArtifacts,
    codebase: &CodebaseMetadata,
    include_expression_types: bool,
    node_analysis_requirements: Option<&NodeAnalysisRequirements>,
    backend: u16,
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let mut writer = lifecycle_writer(AFTER_FILE_ANALYSIS_REQUEST, generation, plugins)?;
    let node_analysis_plan = node_analysis_requirements
        .map(|requirements| build_node_analysis_plan(program, artifacts, resolved_names, codebase, requirements));
    write_file_summary(
        &mut writer,
        FileView::Artifacts(
            file,
            program,
            resolved_names,
            artifacts,
            codebase,
            node_analysis_requirements.map(NodeAnalysisRequirements::targets),
        ),
        include_expression_types,
        node_analysis_requirements,
        node_analysis_plan.as_ref(),
        backend,
        node_analysis_requirements
            .is_some_and(NodeAnalysisRequirements::includes_source_text)
            .then_some(file.contents.as_ref()),
    )?;
    Ok(writer.finish())
}

pub(super) fn encode_after_file_analysis_batch_request(
    generation: u64,
    plugins: &[u16],
    files: &[Arc<FileAnalysisSnapshot>],
    include_expression_types: bool,
    node_analysis_requirements: Option<&NodeAnalysisRequirements>,
    session: &ExternalAnalysisSession,
    backend: u16,
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let mut writer = lifecycle_writer(AFTER_FILE_ANALYSIS_BATCH_REQUEST, generation, plugins)?;
    writer.write_u32(u32::try_from(files.len()).map_err(|_| protocol("after-file batch exceeds u32::MAX files"))?);
    for file in files {
        let file_node_analysis_requirements = node_analysis_requirements.filter(|_| file.has_node_analysis_targets());
        let contents = if file_node_analysis_requirements.is_some_and(NodeAnalysisRequirements::includes_source_text) {
            Some(
                session
                    .source_file(file.file_id())
                    .filter(|source| source.name.as_ref() == file.name.as_ref() && source.size == file.size)
                    .ok_or_else(|| {
                        protocol(format!(
                            "analysis source `{}` is unavailable in generation {}",
                            String::from_utf8_lossy(&file.name),
                            session.generation()
                        ))
                    })?
                    .contents
                    .as_ref(),
            )
        } else {
            None
        };
        write_file_summary(
            &mut writer,
            FileView::Snapshot(file),
            include_expression_types,
            file_node_analysis_requirements,
            None,
            backend,
            contents,
        )?;
    }

    Ok(writer.finish())
}

pub(super) fn encode_after_analysis_request(
    generation: u64,
    plugins: &[u16],
    result: &AnalysisResult,
    files: &[Arc<FileAnalysisSnapshot>],
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let mut writer = lifecycle_writer(AFTER_ANALYSIS_REQUEST, generation, plugins)?;
    writer
        .write_u32(u32::try_from(result.issues.len()).map_err(|_| protocol("analysis has more than u32::MAX issues"))?);
    write_reference_summary(&mut writer, &result.symbol_references);
    writer.write_u32(u32::try_from(files.len()).map_err(|_| protocol("analysis has more than u32::MAX files"))?);
    for file in files {
        write_file_summary(&mut writer, FileView::Snapshot(file), false, None, None, 0, None)?;
    }

    Ok(writer.finish())
}

fn lifecycle_writer(kind: u16, generation: u64, plugins: &[u16]) -> Result<PayloadWriter, ExternalAnalyzerError> {
    let mut writer = protocol::message_writer(kind);
    writer.write_u64(generation);
    writer
        .write_u16(u16::try_from(plugins.len()).map_err(|_| protocol("more than u16::MAX lifecycle plugins matched"))?);
    for plugin in plugins {
        writer.write_u16(*plugin);
    }

    Ok(writer)
}

fn write_file_summary(
    writer: &mut PayloadWriter,
    file: FileView<'_>,
    include_expression_types: bool,
    node_analysis_requirements: Option<&NodeAnalysisRequirements>,
    node_analysis_plan: Option<&NodeAnalysisPlan<'_, '_>>,
    backend: u16,
    source_contents: Option<&[u8]>,
) -> Result<(), ExternalAnalyzerError> {
    writer.write_bytes(file.name())?;
    writer.write_u32(file.size());
    writer.write_u32(u32::try_from(file.expression_count()).map_err(|_| protocol("too many expression types"))?);
    writer.write_u32(u32::try_from(file.inferred_return_count()).map_err(|_| protocol("too many return types"))?);
    writer.write_u32(u32::try_from(file.inferred_yield_key_count()).map_err(|_| protocol("too many yield key types"))?);

    writer.write_u32(
        u32::try_from(file.inferred_yield_value_count()).map_err(|_| protocol("too many yield value types"))?,
    );

    file.write_reference_summary(writer);
    writer.write_bool(include_expression_types);
    if include_expression_types {
        file.write_expression_type_snapshot(writer)?;
    }
    writer.write_bool(node_analysis_requirements.is_some());
    if node_analysis_requirements.is_some() {
        writer.write_u16(backend);
        writer.write_bool(source_contents.is_some());
        if let Some(contents) = source_contents {
            writer.write_bytes(contents)?;
        }
        file.write_target_source_snapshot(
            writer,
            node_analysis_plan,
            node_analysis_requirements.is_some_and(NodeAnalysisRequirements::includes_source_text),
        )?;
    }
    Ok(())
}

fn write_reference_summary(writer: &mut PayloadWriter, references: &SymbolReferences) {
    writer.write_u64(references.count_body_references() as u64);
    writer.write_u64(references.count_signature_references() as u64);
    writer.write_u64(references.total_map_entries() as u64);
}

pub(super) fn decode_lifecycle_response(
    payload: &[u8],
    request_kind: u16,
    active_plugins: &[u16],
    plugins: &[ExternalPlugin],
    session: &ExternalAnalysisSession,
    default_file: Option<&File>,
    codebase: &CodebaseMetadata,
) -> Result<LifecycleEffects, ExternalAnalyzerError> {
    let response_kind = match request_kind {
        BEFORE_ANALYSIS_REQUEST => BEFORE_ANALYSIS_RESPONSE,
        AFTER_FILE_ANALYSIS_REQUEST => AFTER_FILE_ANALYSIS_RESPONSE,
        AFTER_ANALYSIS_REQUEST => AFTER_ANALYSIS_RESPONSE,
        AFTER_FILE_ANALYSIS_BATCH_REQUEST => AFTER_FILE_ANALYSIS_BATCH_RESPONSE,
        _ => return Err(protocol(format!("unknown lifecycle request kind {request_kind}"))),
    };

    let mut reader = protocol::message_reader(payload, response_kind)?;
    let count = reader.read_count("lifecycle issues", MAXIMUM_ISSUES)?;
    let mut issues = IssueCollection::new();
    issues.reserve(count);
    for _ in 0..count {
        let plugin_index = reader.read_u16("lifecycle issue plugin index")?;
        if !active_plugins.contains(&plugin_index) {
            return Err(protocol(format!("worker reported an issue for inactive plugin index {plugin_index}")));
        }

        let plugin = plugins
            .get(plugin_index as usize)
            .ok_or_else(|| protocol(format!("worker reported unknown plugin index {plugin_index}")))?;
        let level = read_level(&mut reader)?;
        let local_code = reader.read_string("lifecycle issue code")?;
        if local_code.is_empty() {
            return Err(protocol(format!("plugin `{}` reported an empty issue code", plugin.identifier)));
        }

        let message = reader.read_string("lifecycle issue message")?;
        if message.is_empty() {
            return Err(protocol(format!("plugin `{}` reported an empty issue message", plugin.identifier)));
        }

        let note_count = reader.read_count("lifecycle issue notes", MAXIMUM_NOTES)?;
        let mut notes = Vec::with_capacity(note_count);
        for _ in 0..note_count {
            notes.push(reader.read_string("lifecycle issue note")?);
        }

        let help = reader.read_optional_string("lifecycle issue help")?;
        let link = reader.read_optional_string("lifecycle issue link")?;
        let annotation_count = reader.read_count("lifecycle issue annotations", MAXIMUM_ANNOTATIONS)?;
        let mut annotations = Vec::with_capacity(annotation_count);
        let mut has_primary = false;
        for _ in 0..annotation_count {
            let kind = match reader.read_u8("lifecycle annotation kind")? {
                1 => AnnotationKind::Primary,
                2 => AnnotationKind::Secondary,
                value => return Err(protocol(format!("invalid lifecycle annotation kind {value}"))),
            };

            let named_file = if reader.read_bool("lifecycle annotation file presence")? {
                Some(reader.read_bytes("lifecycle annotation file")?)
            } else {
                None
            };

            let (file_id, size) = match named_file {
                Some(name) => session.source(name).ok_or_else(|| {
                    protocol(format!("lifecycle annotation names unknown file `{}`", String::from_utf8_lossy(name)))
                })?,
                None => default_file
                    .map(|file| (file.id, file.size))
                    .ok_or_else(|| protocol("a project lifecycle annotation must name its source file"))?,
            };

            let start = reader.read_u32("lifecycle annotation start")?;
            let end = reader.read_u32("lifecycle annotation end")?;
            if start > end || end > size {
                return Err(protocol(format!(
                    "plugin `{}` reported invalid annotation span {start}..{end}",
                    plugin.identifier
                )));
            }

            has_primary |= kind == AnnotationKind::Primary;
            let mut annotation = Annotation::new(kind, Span::new(file_id, Position::new(start), Position::new(end)));
            if let Some(message) = reader.read_optional_string("lifecycle annotation message")? {
                annotation = annotation.with_message(message);
            }

            annotations.push(annotation);
        }

        // FIXME(azjezz): Should we forbid missing primary annotations? Rust-side can create issues without primary annotations,
        // but it messes up the baseline stuff.
        if !has_primary {
            return Err(protocol(format!(
                "plugin `{}` reported an issue without a primary annotation",
                plugin.identifier
            )));
        }

        let edit_count = reader.read_count("lifecycle issue edits", MAXIMUM_EDITS)?;
        let mut edits = HashMap::default();
        for _ in 0..edit_count {
            let named_file = if reader.read_bool("lifecycle edit file presence")? {
                Some(reader.read_bytes("lifecycle edit file")?)
            } else {
                None
            };

            let (file_id, size) = match named_file {
                Some(name) => session.source(name).ok_or_else(|| {
                    protocol(format!("lifecycle edit names unknown file `{}`", String::from_utf8_lossy(name)))
                })?,
                None => default_file
                    .map(|file| (file.id, file.size))
                    .ok_or_else(|| protocol("a project lifecycle edit must name its source file"))?,
            };

            let start = reader.read_u32("lifecycle edit start")?;
            let end = reader.read_u32("lifecycle edit end")?;
            if start > end || end > size {
                return Err(protocol(format!(
                    "plugin `{}` reported invalid edit range {start}..{end}",
                    plugin.identifier
                )));
            }

            let safety = read_safety(&mut reader)?;
            let new_text = reader.read_bytes("lifecycle edit replacement")?.to_vec();
            edits
                .entry(file_id)
                .or_insert_with(Vec::new)
                .push(TextEdit::replace(TextRange::new(start, end), new_text).with_safety(safety));
        }

        let mut issue = Issue::new(level, message)
            .with_code(format!("{}/{}", plugin.identifier, local_code))
            .with_annotations(annotations);
        issue.notes = notes;
        issue.help = help;
        issue.link = link;
        issue.edits = edits;
        issues.push(issue);
    }

    let reference_count = reader.read_count("lifecycle contributed references", MAXIMUM_REFERENCES)?;
    if request_kind == AFTER_ANALYSIS_REQUEST && reference_count != 0 {
        return Err(protocol("after-analysis hooks may not contribute symbol references"));
    }

    let mut references = SymbolReferences::new();
    let mut references_by_file = HashMap::<FileId, SymbolReferences>::default();
    for _ in 0..reference_count {
        let plugin_index = reader.read_u16("reference contribution plugin index")?;
        if !active_plugins.contains(&plugin_index) {
            return Err(protocol(format!("worker contributed a reference for inactive plugin index {plugin_index}")));
        }

        let plugin = plugins.get(plugin_index as usize).ok_or_else(|| {
            protocol(format!("worker contributed a reference for unknown plugin index {plugin_index}"))
        })?;

        let discovery_file = if request_kind == BEFORE_ANALYSIS_REQUEST {
            None
        } else {
            let name = reader.read_bytes("reference contribution discovery file")?;
            Some(
                session
                    .source(name)
                    .ok_or_else(|| {
                        protocol(format!(
                            "reference contribution names unknown discovery file `{}`",
                            String::from_utf8_lossy(name)
                        ))
                    })?
                    .0,
            )
        };

        let source = read_reference_origin(&mut reader, codebase)?;
        let target = read_symbol_identifier(&mut reader, codebase, true)?;
        let kind = read_reference_kind(&mut reader)?;
        validate_reference_target(codebase, target, kind).map_err(|reason| {
            protocol(format!("plugin `{}` contributed an invalid reference: {reason}", plugin.identifier))
        })?;

        let references =
            discovery_file.map_or(&mut references, |file_id| references_by_file.entry(file_id).or_default());
        match kind {
            SymbolReferenceKind::Body => references.add_reference(source, target, false),
            SymbolReferenceKind::Signature => references.add_reference(source, target, true),
            SymbolReferenceKind::OverriddenMember => {
                let ReferenceOrigin::Symbol(source) = source else {
                    return Err(protocol("an overridden-member reference must originate from a symbol"));
                };
                references.add_overridden_member_reference(source, target);
            }
            SymbolReferenceKind::FunctionLikeReturn => {
                let ReferenceOrigin::Symbol(source) = source else {
                    return Err(protocol("a function-like-return reference must originate from a symbol"));
                };
                references.add_functionlike_return_reference(source, target);
            }
            SymbolReferenceKind::PropertyRead => {
                references.add_property_read_reference(source, target);
            }
            SymbolReferenceKind::PropertyWrite => {
                references.add_property_write_reference(source, target);
            }
        }
    }

    let read_file_count = reader.read_count("files with codebase reads", AFTER_FILE_ANALYSIS_BATCH_SIZE)?;
    if read_file_count != 0 && !matches!(request_kind, AFTER_FILE_ANALYSIS_REQUEST | AFTER_FILE_ANALYSIS_BATCH_REQUEST)
    {
        return Err(protocol("only after-file hooks record codebase reads"));
    }

    let mut reads = Vec::with_capacity(read_file_count);
    for _ in 0..read_file_count {
        let name = reader.read_bytes("codebase read file")?;
        let (file_id, _) = session
            .source(name)
            .ok_or_else(|| protocol(format!("codebase reads name unknown file `{}`", String::from_utf8_lossy(name))))?;
        reads.push((file_id, FileReads::decode(&mut reader)?));
    }

    reader.finish()?;
    for (file_id, file_reads) in reads {
        session.record_reads(file_id, file_reads);
    }

    Ok(LifecycleEffects { issues, references, references_by_file })
}

fn read_safety(reader: &mut PayloadReader<'_>) -> Result<Safety, ExternalAnalyzerError> {
    match reader.read_u8("lifecycle edit safety")? {
        1 => Ok(Safety::Safe),
        2 => Ok(Safety::PotentiallyUnsafe),
        3 => Ok(Safety::Unsafe),
        value => Err(protocol(format!("invalid lifecycle text edit safety {value}"))),
    }
}

pub(super) fn handle_analysis_query(
    payload: &[u8],
    session: &ExternalAnalysisSession,
    store: &AnalysisStore<'_>,
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let mut reader = protocol::message_reader(payload, ANALYSIS_QUERY_REQUEST)?;
    let generation = reader.read_u64("analysis query generation")?;
    if generation != session.generation() {
        return Err(protocol(format!(
            "analysis query generation {generation} does not match {}",
            session.generation()
        )));
    }

    let operation = reader.read_u8("analysis query operation")?;
    let file_name = reader.read_bytes("analysis query file")?;
    let file = store.file(file_name).ok_or_else(|| {
        protocol(format!("analysis query names unavailable file `{}`", String::from_utf8_lossy(file_name)))
    })?;

    let mut writer = protocol::message_writer(ANALYSIS_QUERY_RESPONSE);
    writer.write_u64(generation);
    writer.write_u8(operation);
    writer.write_bytes(file_name)?;
    match operation {
        GET_EXPRESSION_TYPES => {
            let count = reader.read_count("expression type queries", MAXIMUM_TYPE_QUERIES)?;
            let mut spans = Vec::with_capacity(count);
            for _ in 0..count {
                spans.push((reader.read_u32("expression start")?, reader.read_u32("expression end")?));
            }
            writer.write_u32(count as u32);
            for span in &spans {
                let ty = file.expression_type(span);
                writer.write_bool(ty.is_some());
                if let Some(ty) = ty {
                    ty.write_to(&mut writer)?;
                }
            }

            let nearby = file.nearby_expression_spans(&spans);
            writer.write_u32(nearby.len() as u32);
            for span in nearby {
                writer.write_u32(span.0);
                writer.write_u32(span.1);
                file.expression_type(&span)
                    .ok_or_else(|| {
                        protocol("prefetched expression type disappeared while encoding analysis artifacts")
                    })?
                    .write_to(&mut writer)?;
            }
        }
        GET_ALL_EXPRESSION_TYPES => file.write_expression_types(&mut writer)?,
        GET_INFERRED_RETURN_TYPES => file.write_inferred_return_types(&mut writer)?,
        GET_INFERRED_YIELD_KEY_TYPES => file.write_inferred_yield_key_types(&mut writer)?,
        GET_INFERRED_YIELD_VALUE_TYPES => file.write_inferred_yield_value_types(&mut writer)?,
        GET_SOURCE_FILE => {
            let source = session
                .source_file(file.file_id())
                .filter(|source| source.name.as_ref() == file.name() && source.size == file.size())
                .ok_or_else(|| {
                    protocol(format!(
                        "analysis source `{}` is unavailable in generation {generation}",
                        String::from_utf8_lossy(file.name())
                    ))
                })?;

            writer.write_bytes(&source.contents)?;
            file.write_source_snapshot(&mut writer)?;
        }
        value => return Err(protocol(format!("unknown analysis query operation {value}"))),
    }

    reader.finish()?;
    Ok(writer.finish())
}

pub(super) fn is_symbol_reference_query(payload: &[u8]) -> Result<bool, ExternalAnalyzerError> {
    Ok(protocol::message_kind(payload)? == SYMBOL_REFERENCE_QUERY_REQUEST)
}

pub(super) fn handle_symbol_reference_query(
    payload: &[u8],
    session: &ExternalAnalysisSession,
    codebase: &CodebaseMetadata,
    store: &SymbolReferenceStore<'_>,
) -> Result<Vec<u8>, ExternalAnalyzerError> {
    let mut reader = protocol::message_reader(payload, SYMBOL_REFERENCE_QUERY_REQUEST)?;
    let generation = reader.read_u64("symbol-reference query generation")?;
    if generation != session.generation() {
        return Err(protocol(format!(
            "symbol-reference query generation {generation} does not match {}",
            session.generation()
        )));
    }

    let operation = reader.read_u8("symbol-reference query operation")?;
    let query_count = reader.read_count("symbol-reference queries", MAXIMUM_REFERENCE_QUERIES)?;
    let mut writer = protocol::message_writer(SYMBOL_REFERENCE_QUERY_RESPONSE);
    writer.write_u64(generation);
    writer.write_u8(operation);
    writer.write_u32(query_count as u32);

    match operation {
        GET_REFERENCES_TO => {
            let mut queries = Vec::with_capacity(query_count);
            for _ in 0..query_count {
                let target = read_symbol_identifier(&mut reader, codebase, false)?;
                queries.push(target);
            }

            for query in queries {
                write_recorded_references(&mut writer, store.references_to(query))?;
            }
        }
        GET_REFERENCES_FROM => {
            let mut queries = Vec::with_capacity(query_count);
            for _ in 0..query_count {
                let source = read_reference_origin(&mut reader, codebase)?;
                queries.push(source);
            }

            for query in queries {
                write_recorded_references(&mut writer, store.references_from(query))?;
            }
        }
        unknown => return Err(protocol(format!("unknown symbol-reference query operation {unknown}"))),
    }

    reader.finish()?;
    Ok(writer.finish())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct RecordedReference {
    source: ReferenceOrigin,
    target: SymbolIdentifier,
    kind: SymbolReferenceKind,
}

fn write_recorded_references(
    writer: &mut PayloadWriter,
    references: &[RecordedReference],
) -> Result<(), ExternalAnalyzerError> {
    writer.write_u32(u32::try_from(references.len()).map_err(|_| protocol("too many symbol references"))?);
    for reference in references {
        write_reference_origin(writer, reference.source)?;
        write_symbol_identifier(writer, reference.target)?;
        writer.write_u8(match reference.kind {
            SymbolReferenceKind::Body => 1,
            SymbolReferenceKind::Signature => 2,
            SymbolReferenceKind::OverriddenMember => 3,
            SymbolReferenceKind::FunctionLikeReturn => 4,
            SymbolReferenceKind::PropertyRead => 5,
            SymbolReferenceKind::PropertyWrite => 6,
        });
    }

    Ok(())
}

fn read_reference_origin(
    reader: &mut PayloadReader<'_>,
    codebase: &CodebaseMetadata,
) -> Result<ReferenceOrigin, ExternalAnalyzerError> {
    let endpoint = reader.read_u8("reference origin kind")?;
    if endpoint == 3 {
        let file = reader.read_bytes("reference origin file")?;
        if file.is_empty() {
            return Err(protocol("reference origin file cannot be empty"));
        }

        return Ok(ReferenceOrigin::File(word(file)));
    }

    read_symbol_identifier_with_kind(reader, codebase, false, endpoint).map(ReferenceOrigin::Symbol)
}

fn write_reference_origin(writer: &mut PayloadWriter, source: ReferenceOrigin) -> Result<(), ExternalAnalyzerError> {
    match source {
        ReferenceOrigin::Symbol(symbol) => write_symbol_identifier(writer, symbol),
        ReferenceOrigin::File(file) => {
            if file.is_empty() {
                return Err(protocol("reference origin file cannot be empty"));
            }
            writer.write_u8(3);
            writer.write_bytes(file.as_bytes())?;
            Ok(())
        }
    }
}

fn read_symbol_identifier(
    reader: &mut PayloadReader<'_>,
    codebase: &CodebaseMetadata,
    target: bool,
) -> Result<SymbolIdentifier, ExternalAnalyzerError> {
    let kind = reader.read_u8("symbol reference endpoint kind")?;
    read_symbol_identifier_with_kind(reader, codebase, target, kind)
}

fn read_symbol_identifier_with_kind(
    reader: &mut PayloadReader<'_>,
    codebase: &CodebaseMetadata,
    target: bool,
    kind: u8,
) -> Result<SymbolIdentifier, ExternalAnalyzerError> {
    let symbol = reader.read_bytes("symbol reference name")?;
    if symbol.is_empty() {
        return Err(protocol("symbol reference name cannot be empty"));
    }
    let symbol = normalize_symbol(codebase, symbol, target);

    match kind {
        1 => Ok((symbol, empty_word())),
        2 => {
            let member = reader.read_bytes("symbol reference member")?;
            if member.is_empty() {
                return Err(protocol("symbol reference member cannot be empty"));
            }
            let raw_member = word(member);
            let lowercase_member = ascii_lowercase_word(member);
            let member = if codebase.get_method(symbol.as_bytes(), lowercase_member.as_bytes()).is_some() {
                lowercase_member
            } else {
                raw_member
            };
            Ok((symbol, member))
        }
        unknown => Err(protocol(format!("unknown symbol reference endpoint kind {unknown}"))),
    }
}

fn normalize_symbol(codebase: &CodebaseMetadata, bytes: &[u8], target: bool) -> mago_word::Word {
    let lowercase = ascii_lowercase_word(bytes);
    if codebase.class_like_exists(lowercase.as_bytes())
        || codebase.function_likes.contains_key(&(empty_word(), lowercase))
    {
        return lowercase;
    }

    let constant = ascii_lowercase_constant_name_word(bytes);
    if codebase.constants.contains_key(&constant) {
        return constant;
    }

    if target { word(bytes) } else { lowercase }
}

fn write_symbol_identifier(writer: &mut PayloadWriter, symbol: SymbolIdentifier) -> Result<(), ExternalAnalyzerError> {
    if symbol.0.is_empty() {
        return Err(protocol("symbol reference name cannot be empty"));
    }

    if symbol.1.is_empty() {
        writer.write_u8(1);
        writer.write_bytes(symbol.0.as_bytes())?;
    } else {
        writer.write_u8(2);
        writer.write_bytes(symbol.0.as_bytes())?;
        writer.write_bytes(symbol.1.as_bytes())?;
    }

    Ok(())
}

fn read_reference_kind(reader: &mut PayloadReader<'_>) -> Result<SymbolReferenceKind, ExternalAnalyzerError> {
    match reader.read_u8("symbol reference kind")? {
        1 => Ok(SymbolReferenceKind::Body),
        2 => Ok(SymbolReferenceKind::Signature),
        3 => Ok(SymbolReferenceKind::OverriddenMember),
        4 => Ok(SymbolReferenceKind::FunctionLikeReturn),
        5 => Ok(SymbolReferenceKind::PropertyRead),
        6 => Ok(SymbolReferenceKind::PropertyWrite),
        unknown => Err(protocol(format!("unknown symbol reference kind {unknown}"))),
    }
}

fn validate_reference_target(
    codebase: &CodebaseMetadata,
    target: SymbolIdentifier,
    kind: SymbolReferenceKind,
) -> Result<(), String> {
    if target.1.is_empty() {
        let exists = if kind == SymbolReferenceKind::FunctionLikeReturn {
            codebase.function_likes.contains_key(&(empty_word(), target.0))
        } else {
            codebase.class_like_exists(target.0.as_bytes())
                || codebase.function_likes.contains_key(&(empty_word(), target.0))
                || codebase.constants.contains_key(&target.0)
        };
        return exists.then_some(()).ok_or_else(|| format!("target `{}` does not exist", target.0));
    }

    let Some(class_like) = codebase.get_class_like(target.0.as_bytes()) else {
        return Err(format!("target class-like `{}` does not exist", target.0));
    };
    let exists = match kind {
        SymbolReferenceKind::PropertyRead | SymbolReferenceKind::PropertyWrite => {
            class_like.properties.contains_key(&target.1)
        }
        SymbolReferenceKind::FunctionLikeReturn => {
            codebase.get_method(target.0.as_bytes(), target.1.as_bytes()).is_some()
        }
        SymbolReferenceKind::OverriddenMember => class_like.methods.contains(&target.1),
        SymbolReferenceKind::Body | SymbolReferenceKind::Signature => {
            class_like.methods.contains(&target.1)
                || class_like.properties.contains_key(&target.1)
                || class_like.constants.contains_key(&target.1)
                || class_like.enum_cases.contains_key(&target.1)
        }
    };

    exists.then_some(()).ok_or_else(|| format!("target member `{}::{}` does not exist", target.0, target.1))
}

fn write_types(
    writer: &mut PayloadWriter,
    types: &[TUnion],
    codebase: &CodebaseMetadata,
    dialect: Dialect,
) -> Result<(), ExternalAnalyzerError> {
    writer.write_u32(u32::try_from(types.len()).map_err(|_| protocol("too many inferred types"))?);
    for ty in types {
        write_type(writer, ty, codebase, dialect)?;
    }

    Ok(())
}

fn write_encoded_types(
    writer: &mut PayloadWriter,
    payload: &[u8],
    ranges: &[Range<usize>],
) -> Result<(), ExternalAnalyzerError> {
    writer.write_u32(u32::try_from(ranges.len()).map_err(|_| protocol("too many inferred types"))?);
    for range in ranges {
        let bytes = payload
            .get(range.clone())
            .ok_or_else(|| protocol("retained analysis type points outside its encoded payload"))?;
        writer.write_raw(bytes);
    }

    Ok(())
}

fn write_type(
    writer: &mut PayloadWriter,
    ty: &TUnion,
    codebase: &CodebaseMetadata,
    dialect: Dialect,
) -> Result<(), ExternalAnalyzerError> {
    protocol::encode_union_snapshot(writer, ty, &mut Vec::new(), codebase, dialect, 0)
}

fn read_level(reader: &mut PayloadReader<'_>) -> Result<Level, ExternalAnalyzerError> {
    match reader.read_u8("lifecycle issue level")? {
        1 => Ok(Level::Note),
        2 => Ok(Level::Help),
        3 => Ok(Level::Warning),
        4 => Ok(Level::Error),
        value => Err(protocol(format!("invalid lifecycle issue level {value}"))),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::rc::Rc;
    use std::sync::Arc;

    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::metadata::class_like::ClassLikeMetadata;
    use mago_codex::metadata::flags::MetadataFlags;
    use mago_codex::reference::ReferenceOrigin;
    use mago_codex::ttype::atomic::TAtomic;
    use mago_codex::ttype::atomic::array::TArray;
    use mago_codex::ttype::union::TUnion;
    use mago_extension::PayloadWriter;
    use mago_span::Span;
    use mago_syntax::cst::NodeKind;
    use mago_word::empty_word;
    use mago_word::word;

    use super::ANALYSIS_QUERY_REQUEST;
    use super::AnalysisStore;
    use super::FileAnalysisSnapshot;
    use super::GET_ALL_EXPRESSION_TYPES;
    use super::GET_EXPRESSION_TYPES;
    use super::GET_INFERRED_RETURN_TYPES;
    use super::GET_INFERRED_YIELD_KEY_TYPES;
    use super::GET_INFERRED_YIELD_VALUE_TYPES;
    use super::analyzed_names;
    use super::build_node_analysis_plan;
    use super::class_like_matches_hook;
    use super::encode_after_file_analysis_request;
    use super::handle_analysis_query;
    use super::write_reference_origin;
    use super::write_symbol_identifier;
    use crate::artifacts::AnalysisArtifacts;
    use crate::external::ClassLikeAnalysisHookRegistration;
    use crate::external::ExternalAnalysisSession;
    use crate::external::NODE_REQUIREMENTS_ALL;
    use crate::external::NodeAnalysisRequirements;
    use crate::external::protocol;
    use crate::external::protocol::testing::Analysis;
    use crate::external::protocol::testing::SHELF;
    use crate::external::protocol::testing::SINK;
    use crate::external::protocol::testing::analyzed;
    use crate::external::protocol::testing::names;

    /// The answer to the analysis query `operation` about the analyzed file, with `artifacts`, which asks for the
    /// types of `spans`.
    fn answer(
        analysis: &Analysis<'_, '_>,
        artifacts: &AnalysisArtifacts,
        operation: u8,
        spans: &[(u32, u32)],
    ) -> Vec<u8> {
        let session = ExternalAnalysisSession::from_files(analysis.files.iter().cloned());
        let mut writer = protocol::message_writer(ANALYSIS_QUERY_REQUEST);
        writer.write_u64(session.generation());
        writer.write_u8(operation);
        writer.write_bytes(&analysis.file().name).expect("the name should fit in a frame");
        if operation == GET_EXPRESSION_TYPES {
            writer.write_u32(spans.len() as u32);
            for (start, end) in spans {
                writer.write_u32(*start);
                writer.write_u32(*end);
            }
        }
        let store = AnalysisStore::File {
            file: analysis.file(),
            program: analysis.program,
            resolved_names: analysis.names,
            artifacts,
            codebase: analysis.codebase,
            node_analysis_targets: None,
        };

        handle_analysis_query(&writer.finish(), &session, &store).expect("the analysis query should succeed")
    }

    /// What an external analyzer reads of an analyzed PHP# file goes as the file's code sees it: each `Map` an
    /// expression, a node-analysis target, an inferred return type or a yielded key or value holds goes as the
    /// `Sharp\Map` object it runs as. The analyzer keeps the types a `return` and a `yield` give with their function,
    /// not the file, so the file's inferred types are ones the test gives it.
    #[test]
    fn an_analyzed_file_sends_its_types_as_its_code_sees_them() {
        analyzed(&[SHELF, SINK], |analysis| {
            let mut targets = [false; NodeKind::COUNT];
            let mut requirements = [0; NodeKind::COUNT];
            for kind in [NodeKind::MethodCall, NodeKind::StaticMethodCall] {
                targets[kind as usize] = true;
                requirements[kind as usize] = NODE_REQUIREMENTS_ALL;
            }
            let requirements = NodeAnalysisRequirements::new(&targets, &requirements, Arc::from([]), Arc::from([]));
            let snapshot = FileAnalysisSnapshot::new(
                analysis.file(),
                analysis.program,
                analysis.names,
                analysis.artifacts,
                analysis.codebase,
                Some(&requirements),
                true,
            )
            .expect("the file should snapshot");
            // An after-file request with the expression types alone, then with the node-analysis targets alone.
            let after_file = [(true, None), (false, Some(&requirements))].map(|(expression_types, requirements)| {
                encode_after_file_analysis_request(
                    1,
                    &[0],
                    analysis.file(),
                    analysis.program,
                    analysis.names,
                    analysis.artifacts,
                    analysis.codebase,
                    expression_types,
                    requirements,
                    0,
                )
                .expect("the request should encode")
            });
            let map = analysis
                .artifacts
                .expression_types
                .iter()
                .find(|(_, union)| union.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_)))))
                .map(|(span, _)| *span)
                .expect("an expression holds a `Map`");

            assert!(snapshot.has_node_analysis_targets(), "the call of `take` is a node-analysis target");
            assert!(names(&snapshot.encoded_types, b"Sharp\\Map"), "an expression's `Map`");
            assert!(names(&snapshot.encoded_target_analysis, b"Sharp\\Map"), "the argument of `take`");
            assert!(names(&after_file[0], b"Sharp\\Map"), "the after-file request's expression types");
            assert!(names(&after_file[1], b"Sharp\\Map"), "the after-file request's node-analysis targets");
            let map_type = &analysis.artifacts.expression_types[&map];
            let mut inferred = analysis.artifacts.clone();
            inferred.inferred_return_types.push(Rc::clone(map_type));
            inferred.inferred_yield_key_types.push(TUnion::clone(map_type));
            inferred.inferred_yield_value_types.push(TUnion::clone(map_type));
            let rows = [
                (GET_EXPRESSION_TYPES, analysis.artifacts),
                (GET_ALL_EXPRESSION_TYPES, analysis.artifacts),
                (GET_INFERRED_RETURN_TYPES, &inferred),
                (GET_INFERRED_YIELD_KEY_TYPES, &inferred),
                (GET_INFERRED_YIELD_VALUE_TYPES, &inferred),
            ];

            for (operation, artifacts) in rows {
                assert!(
                    names(&answer(analysis, artifacts, operation, &[map]), b"Sharp\\Map"),
                    "analysis query {operation}"
                );
            }
        });
    }

    /// A PHP# file whose calls mean a plain PHP function and an enum case, and whose pattern names a case.
    const PAY: (&str, &str) = (
        "src/App/Pay.sharp",
        "namespace App;\n\nimport Vendor.Pdf;\n\npublic enum PaymentResult\n{\n    case Paid(string id);\n    case Declined(string reason);\n}\n\npublic class Pay\n{\n    public static PaymentResult decline() => PaymentResult.Declined(reason: \"expired\");\n\n    public static string id(PaymentResult result) => match (result) {\n        PaymentResult.Paid(string id) => id,\n        PaymentResult.Declined(string why) => why,\n    };\n\n    public static string render() => Pdf.render(\"x\");\n\n    public static int dpi() => Pdf.DEFAULT_DPI;\n}\n",
    );

    /// A plain PHP namespace with a function and a constant, and no class of its name.
    const PDF: (&str, &str) = (
        "src/Vendor/pdf.php",
        "<?php\n\nnamespace Vendor\\Pdf;\n\nconst DEFAULT_DPI = 300;\n\nfunction render(string $html): string\n{\n    return $html;\n}\n",
    );

    /// A host reads each PHP# form as the PHP that means it: `Pdf.render("x")` calls the function `Vendor\Pdf\render`
    /// and `PaymentResult.Declined(…)` builds the case, so their targets carry those kinds and reach the hooks of
    /// function calls and of `new`, while a class declaration means only itself. The snapshot names what only the
    /// codebase tells: the namespace and its members at `Pdf.render` and `Pdf.DEFAULT_DPI`, the case's class at a
    /// built case, and the enum at a pattern of its case. A plain PHP file names nothing more.
    #[test]
    fn a_sharp_file_sends_each_form_as_the_php_that_means_it() {
        analyzed(&[PAY, PDF], |analysis| {
            let mut targets = [false; NodeKind::COUNT];
            let mut requirements = [0; NodeKind::COUNT];
            for kind in [NodeKind::FunctionCall, NodeKind::Instantiation, NodeKind::Class] {
                targets[kind as usize] = true;
                requirements[kind as usize] = NODE_REQUIREMENTS_ALL;
            }
            let requirements = NodeAnalysisRequirements::new(&targets, &requirements, Arc::from([]), Arc::from([]));
            let plan = build_node_analysis_plan(
                analysis.program,
                analysis.artifacts,
                analysis.names,
                analysis.codebase,
                &requirements,
            );
            let snapshot = FileAnalysisSnapshot::new(
                analysis.file(),
                analysis.program,
                analysis.names,
                analysis.artifacts,
                analysis.codebase,
                Some(&requirements),
                true,
            )
            .expect("the file should snapshot");
            let code = &analysis.file().contents;
            let mut named = analyzed_names(analysis.program, analysis.names, analysis.codebase)
                .into_iter()
                .map(|(start, end, name, binding)| {
                    let written = String::from_utf8_lossy(&code[start as usize..end as usize]).into_owned();
                    (written, String::from_utf8_lossy(&name).into_owned(), binding)
                })
                .collect::<Vec<_>>();
            named.sort();

            assert_eq!(
                plan.targets.iter().map(|target| target.meaning).collect::<Vec<_>>(),
                [None, Some(NodeKind::Instantiation), Some(NodeKind::FunctionCall)]
            );
            assert!(snapshot.has_node_analysis_targets(), "the class and the calls are node-analysis targets");
            assert_eq!(
                named,
                [
                    ("DEFAULT_DPI".to_owned(), "Vendor\\Pdf\\DEFAULT_DPI".to_owned(), 0),
                    ("Declined".to_owned(), "App\\PaymentResult\\Declined".to_owned(), 1),
                    ("PaymentResult".to_owned(), "App\\PaymentResult".to_owned(), 1),
                    ("PaymentResult".to_owned(), "App\\PaymentResult".to_owned(), 1),
                    ("Pdf".to_owned(), "Vendor\\Pdf".to_owned(), 0),
                    ("Pdf".to_owned(), "Vendor\\Pdf".to_owned(), 0),
                    ("render".to_owned(), "Vendor\\Pdf\\render".to_owned(), 0),
                ]
            );
        });
        analyzed(&[PDF], |analysis| {
            assert!(analyzed_names(analysis.program, analysis.names, analysis.codebase).is_empty());
        });
    }

    #[test]
    fn descendant_target_excludes_the_ancestor_declaration() {
        let ancestor = word(b"frameworktestcase");
        let child = word(b"applicationtest");
        let mut codebase = CodebaseMetadata::new();
        codebase.class_likes.insert(
            ancestor,
            ClassLikeMetadata::new(
                ancestor,
                word(b"FrameworkTestCase"),
                Span::dummy(0, 10),
                None,
                MetadataFlags::empty(),
            ),
        );
        let mut child_metadata =
            ClassLikeMetadata::new(child, word(b"ApplicationTest"), Span::dummy(11, 20), None, MetadataFlags::empty());
        child_metadata.all_parent_classes.insert(ancestor);
        codebase.class_likes.insert(child, child_metadata);

        let hook = ClassLikeAnalysisHookRegistration {
            plugin: 0,
            index: 0,
            requirements: 0,
            targets: vec![ancestor],
            route: 0,
        };

        assert!(!class_like_matches_hook(b"FrameworkTestCase", &hook, &codebase));
        assert!(class_like_matches_hook(b"ApplicationTest", &hook, &codebase));
    }

    #[test]
    fn outbound_reference_endpoints_reject_empty_names() {
        let mut writer = PayloadWriter::new();
        assert!(write_symbol_identifier(&mut writer, (empty_word(), empty_word())).is_err());
        assert!(write_reference_origin(&mut writer, ReferenceOrigin::File(empty_word())).is_err());
    }
}
