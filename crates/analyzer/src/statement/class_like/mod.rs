use std::borrow::Cow;

use foldhash::HashMap;
use foldhash::HashSet;
use foldhash::fast::RandomState;
use indexmap::IndexMap;
use itertools::Itertools;
use mago_allocator::Arena;

use mago_bytes::BytesDisplay;
use mago_codex::context::ScopeContext;
use mago_codex::defaults_trait_name;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::generic::TGenericParameter;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::combine_union_types;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::get_specialized_template_type;
use mago_codex::ttype::intersect_union_types;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_codex::ttype::template::GenericTemplate;
use mago_codex::ttype::template::TemplateResult;
use mago_codex::ttype::template::definition_type_replacer;
use mago_codex::ttype::template::definition_type_replacer::DefinitionReplacementOptions;
use mago_codex::ttype::template::inferred_type_replacer;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_codex::visibility::Visibility;
use mago_names::binding::MethodParts;
use mago_names::binding::php_variable_name;
use mago_names::display_sharp_member;
use mago_names::kind::NameKind;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::comments::docblock::PrecedingDocblocks;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Event;
use mago_syntax::cst::Extends;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Implements;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::Property;
use mago_syntax::cst::Trait;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects::check::check_foreign_statics;
use crate::error::AnalysisError;
use crate::plugin::context::HookContext;
use crate::resolver::class_name::report_unknown_sharp_class;
use crate::resolver::extension::class_member;
use crate::statement::attributes::analyze_class_like_attributes;
use crate::statement::class_like::method_signature::SignatureCompatibilityIssue;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::HintPlace;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::report_invalid_template_arguments;
use crate::statement::function_like::report_undefined_type_references;
use crate::utils::missing_type_hints;
use crate::utils::names::and_list;
use crate::utils::names::article;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_member;
use crate::utils::names::display_property_name;
use crate::utils::names::display_sharp_accessor;
use crate::utils::names::display_sharp_type;
use crate::utils::names::display_type;
use crate::utils::names::sharp_method_declaration;
use crate::utils::template::find_broken_marker;
use crate::utils::template::find_template_uses;

pub mod constant;
pub mod enum_case;
pub mod initialization;
pub mod listener;
pub mod method;
pub mod method_signature;
pub mod override_attribute;
pub mod property;
pub mod r#struct;
pub mod unused_members;

/// Reports a duplicate definition issue for class-like types, naming the class-like as the analyzed file writes it.
fn report_duplicate_definition<A>(
    context: &mut Context<'_, '_, A>,
    title: &str,
    kind: &str,
    name: &[u8],
    duplicate_span: Span,
    original_span: Span,
) where
    A: Arena,
{
    // Spec section 20: a case of an enum with a case that carries data runs as the class of its name.
    if let Some(case) = context.codebase.get_class_like(name).filter(|original| original.flags.is_enum_case_class()) {
        let full_name = case.original_name.as_bytes();
        let separator = full_name.iter().rposition(|&byte| byte == b'\\').unwrap_or_default();
        let (enum_name, case_name) = (&full_name[..separator], &full_name[separator + 1..]);
        context.collector.report_with_code(
            IssueCode::DuplicateDefinition,
            Issue::error(format!(
                "{title} `{}` clashes with the case `{}`, which runs as a class of that name.",
                display_class_like_name(context, case.original_name),
                display_member(context, word(enum_name), BytesDisplay(case_name)),
            ))
            .with_annotation(Annotation::primary(duplicate_span).with_message(format!("{title} declared here.")))
            .with_annotation(Annotation::secondary(original_span).with_message("Case declared here."))
            .with_help("Rename the case or the class."),
        );

        return;
    }

    let name = if context.dialect.is_sharp() {
        display_class_like_name(context, word(name)).to_string()
    } else {
        BytesDisplay(name).to_string()
    };
    context.collector.report_with_code(
        IssueCode::DuplicateDefinition,
        Issue::error(format!("{title} `{name}` is already defined elsewhere."))
            .with_annotation(
                Annotation::primary(duplicate_span).with_message(format!("Duplicate {kind} definition here")),
            )
            .with_annotation(Annotation::secondary(original_span).with_message(format!("Original {kind} defined here")))
            .with_note("Each class, interface, trait, or enum must have a unique name within the same namespace.")
            .with_note("The duplicate definition will be ignored during analysis.")
            .with_help(
                "Consider using namespaces to avoid naming conflicts, or remove one of the duplicate definitions.",
            ),
    );
}

/// Refuses a class-like whose full name is also a namespace, as `App\Tenant\Store` is beside a class in
/// `App\Tenant\Store\`, when either is PHP#: spec section 23. A plain PHP class may still share its name with a
/// namespace of plain PHP, as PHP allows.
fn report_class_like_named_as_a_namespace<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    name_span: Span,
) where
    A: Arena,
{
    let name = class_like_metadata.name.as_bytes();
    // The codebase keeps a namespace after its last class-like leaves it, so the class-likes decide.
    if !context.codebase.namespace_exists(name) {
        return;
    }

    let sharp = context.dialect.is_sharp();
    // Spec section 22: PHP# code never names an interface's trait of default bodies, so the rule leaves it out.
    let defaults = ascii_lowercase_word(defaults_trait_name(name).as_bytes());
    let Some(inside) = context
        .codebase
        .class_likes
        .values()
        .filter(|other| {
            // Spec section 20: each case of an enum that carries data runs as a class in the enum's namespace, and a
            // class another file declares under a case's name clashes with the case instead.
            let is_own_case =
                class_like_metadata.permitted_inheritors.as_ref().is_some_and(|cases| cases.contains(&other.name));

            (sharp || other.flags.is_sharp())
                && other.name != defaults
                && !is_own_case
                && other.name.as_bytes().strip_prefix(name).is_some_and(|rest| rest.starts_with(b"\\"))
        })
        .min_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()))
    else {
        return;
    };

    let (full_name, inside_name) = if sharp {
        (
            class_like_metadata.original_name.to_string().replace('\\', "."),
            inside.original_name.to_string().replace('\\', "."),
        )
    } else {
        (class_like_metadata.original_name.to_string(), inside.original_name.to_string())
    };
    let kind = class_like_metadata.kind.as_str();
    let article = article(kind);

    context.collector.report_with_code(
        IssueCode::NameAlreadyInUse,
        Issue::error(format!(
            "`{full_name}` is both {article} {kind} and a namespace: rename the {kind} or the folder."
        ))
        .with_annotation(Annotation::primary(name_span).with_message(format!("The {kind} is declared here.")))
        .with_annotation(
            Annotation::secondary(inside.name_span.unwrap_or(inside.span))
                .with_message(format!("`{inside_name}` is in the namespace `{full_name}`.")),
        ),
    );
}

/// Refuses a PHP# class-like whose namespace, or a namespace above it, is the full name of a plain PHP class-like:
/// spec section 23 counts plain PHP classes the checker sees. The plain PHP file reports the clash as well, but
/// `mago compile` prints only `.sharp` files' errors, so the PHP# file reports it too.
fn report_namespace_named_as_a_plain_php_class_like<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    name_span: Span,
) where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return;
    }

    let name = class_like_metadata.original_name.as_bytes();
    let Some(plain) = name
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'\\')
        .filter_map(|(end, _)| context.codebase.get_class_like(&name[..end]))
        .find(|namespace| !namespace.flags.is_sharp())
    else {
        return;
    };

    let namespace = plain.original_name.to_string().replace('\\', ".");
    let kind = plain.kind.as_str();
    context.collector.report_with_code(
        IssueCode::NameAlreadyInUse,
        Issue::error(format!(
            "`{namespace}` is both a plain PHP {kind} and a namespace: rename the {kind} or the folder."
        ))
        .with_annotation(Annotation::primary(name_span).with_message(format!(
            "`{}` is in the namespace `{namespace}`.",
            class_like_metadata.original_name.to_string().replace('\\', ".")
        )))
        .with_annotation(
            Annotation::secondary(plain.name_span.unwrap_or(plain.span))
                .with_message(format!("The plain PHP {kind} is declared here.")),
        ),
    );
}

/// Helper function to check if a child type is compatible with (contained by) a parent type.
///
/// This is a convenience wrapper around `union_comparator::is_contained_by` with standard
/// settings for inheritance checks (no null/false ignoring, not inside assertion).
#[inline]
fn is_type_compatible(codebase: &CodebaseMetadata, child: &TUnion, parent: &TUnion) -> bool {
    union_comparator::is_contained_by(codebase, child, parent, false, false, false, &mut ComparisonResult::default())
}

/// Checks whether a child's property type violates the variance rules imposed by the parent.
///
/// - When the parent has only a `get` hook, covariance (declaring narrower than parent) is allowed.
/// - When the parent has only a `set` hook, contravariance (declaring wider than parent) is allowed.
/// - Otherwise, invariance is required.
/// - Types incomparable with the parent's are invalid under any variance.
///
/// Returns `true` when the declaring type is incompatible with the parent under these rules.
#[inline]
fn is_property_type_variance_invalid(
    codebase: &CodebaseMetadata,
    declaring_type: &TUnion,
    parent_type: &TUnion,
    parent_only_get: bool,
    parent_only_set: bool,
) -> bool {
    let declaring_is_subtype_of_parent = is_type_compatible(codebase, declaring_type, parent_type);
    let parent_is_subtype_of_declaring = is_type_compatible(codebase, parent_type, declaring_type);

    let declaring_is_narrower = declaring_is_subtype_of_parent && !parent_is_subtype_of_declaring;
    let declaring_is_wider = parent_is_subtype_of_declaring && !declaring_is_subtype_of_parent;
    let incomparable = !declaring_is_subtype_of_parent && !parent_is_subtype_of_declaring;

    incomparable || (declaring_is_wider && !parent_only_set) || (declaring_is_narrower && !parent_only_get)
}

/// Rewrites a type declared by `parent_class_name` in terms of the child's own templates,
/// substituting the arguments the child supplies through `@extends`.
///
/// Inherited property types are already localized this way, so the parent's type has to be
/// localized too before the two can be compared.
#[inline]
fn localize_parent_type(
    codebase: &CodebaseMetadata,
    class_like_metadata: &ClassLikeMetadata,
    parent_class_name: Word,
    parent_type: &TUnion,
) -> TUnion {
    let Some(mapping) = class_like_metadata.template_extended_parameters.get(&parent_class_name) else {
        return parent_type.clone();
    };

    let mut template_result = TemplateResult::default();
    for (template_name, concrete_type) in mapping {
        template_result.add_lower_bound(
            *template_name,
            GenericParent::ClassLike(parent_class_name),
            concrete_type.clone(),
        );
    }

    inferred_type_replacer::replace(parent_type, &template_result, codebase)
}

/// Represents different types of property conflicts between traits
#[derive(Debug)]
enum PropertyConflict {
    Visibility(Visibility, Visibility, Visibility, Visibility),
    Static(bool, bool),
    Readonly(bool, bool),
    Type(Option<Word>, Option<Word>),
    Default(Option<Word>, Option<Word>),
    HookedProperty,
}

impl PropertyConflict {
    fn describe(&self) -> String {
        match self {
            PropertyConflict::Visibility(r1, w1, r2, w2) => {
                let p1_vis = if r1 == w1 { r1.to_string() } else { format!("{r1} {w1}(set)") };
                let p2_vis = if r2 == w2 { r2.to_string() } else { format!("{r2} {w2}(set)") };
                format!("visibility differs ({p1_vis} vs {p2_vis})")
            }
            PropertyConflict::Static(s1, s2) => {
                let p1_mod = if *s1 { "static" } else { "instance" };
                let p2_mod = if *s2 { "static" } else { "instance" };
                format!("static modifier differs ({p1_mod} vs {p2_mod})")
            }
            PropertyConflict::Readonly(r1, r2) => {
                let p1_mod = if *r1 { "readonly" } else { "not readonly" };
                let p2_mod = if *r2 { "readonly" } else { "not readonly" };
                format!("readonly modifier differs ({p1_mod} vs {p2_mod})")
            }
            PropertyConflict::Type(t1, t2) => match (t1, t2) {
                (Some(type1), Some(type2)) => format!("type declaration differs ({type1} vs {type2})"),
                (Some(type1), None) => format!("type declaration differs ({type1} vs untyped)"),
                (None, Some(type2)) => format!("type declaration differs (untyped vs {type2})"),
                #[allow(clippy::unreachable)]
                (None, None) => unreachable!(),
            },
            PropertyConflict::Default(d1, d2) => match (d1, d2) {
                (Some(def1), Some(def2)) => format!("default value differs ({def1} vs {def2})"),
                (Some(def1), None) => format!("default value differs ({def1} vs no default)"),
                (None, Some(def2)) => format!("default value differs (no default vs {def2})"),
                #[allow(clippy::unreachable)]
                (None, None) => unreachable!(),
            },
            PropertyConflict::HookedProperty => {
                "conflict resolution between hooked properties is not supported".to_string()
            }
        }
    }

    fn get_issue_code(&self) -> IssueCode {
        match self {
            PropertyConflict::Visibility(_, _, _, _) => IssueCode::IncompatiblePropertyVisibility,
            PropertyConflict::Static(_, _) => IssueCode::IncompatiblePropertyStatic,
            PropertyConflict::Readonly(_, _) => IssueCode::IncompatiblePropertyReadonly,
            PropertyConflict::Type(_, _) => IssueCode::IncompatiblePropertyType,
            PropertyConflict::Default(_, _) => IssueCode::IncompatiblePropertyDefault,
            PropertyConflict::HookedProperty => IssueCode::IncompatiblePropertyOverride,
        }
    }
}

/// Checks if a type union contains a reference to a specific template parameter.
fn type_contains_template_param(type_union: &TUnion, param_name: Word, defining_class: Word) -> bool {
    use mago_codex::ttype::TypeRef;

    type_union.types.iter().any(|atomic| {
        if let TAtomic::GenericParameter(TGenericParameter {
            parameter_name,
            defining_entity: GenericParent::ClassLike(class_name),
            ..
        }) = atomic
            && *parameter_name == param_name
            && *class_name == defining_class
        {
            return true;
        }

        if let TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Generic {
            parameter_name,
            defining_entity: GenericParent::ClassLike(class_name),
            ..
        })) = atomic
            && *parameter_name == param_name
            && *class_name == defining_class
        {
            return true;
        }

        atomic.get_all_child_nodes().iter().any(|node| match node {
            TypeRef::Atomic(TAtomic::GenericParameter(gp)) => {
                gp.parameter_name == param_name
                    && matches!(gp.defining_entity, GenericParent::ClassLike(c) if c == defining_class)
            }
            TypeRef::Atomic(TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Generic {
                parameter_name,
                defining_entity,
                ..
            }))) => {
                *parameter_name == param_name
                    && matches!(defining_entity, GenericParent::ClassLike(c) if *c == defining_class)
            }
            TypeRef::Union(u) => type_contains_template_param(u, param_name, defining_class),
            _ => false,
        })
    })
}

/// Checks for unused template parameters in a class-like declaration.
///
/// A template parameter is considered "used" if it appears in:
/// - A property type
/// - A method parameter type
/// - A method return type
/// - An `@extends`, `@implements`, or `@use` annotation
fn check_unused_template_parameters<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
) where
    A: Arena,
{
    if !context.settings.find_unused_definitions {
        return;
    }

    if class_like_metadata.template_types.is_empty() {
        return;
    }

    let class_name = class_like_metadata.name;
    let class_original_name = display_class_like_name(context, class_like_metadata.original_name);
    let class_kind_str = class_like_metadata.kind.as_str();
    let class_name_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);
    let sharp_class = context.dialect.is_sharp().then(|| short_name(class_original_name));

    'templates: for (template_name, _) in &class_like_metadata.template_types {
        if template_name.as_bytes().starts_with(b"_") {
            continue;
        }

        for extended_params in class_like_metadata.template_extended_parameters.values() {
            for (_param_name, param_type) in extended_params {
                if type_contains_template_param(param_type, *template_name, class_name) {
                    continue 'templates;
                }
            }
        }

        for property_metadata in class_like_metadata.properties.values() {
            if let Some(type_metadata) = &property_metadata.type_metadata
                && type_contains_template_param(&type_metadata.type_union, *template_name, class_name)
            {
                continue 'templates;
            }
        }

        for method_id in class_like_metadata.declaring_method_ids.values() {
            let Some(function_like) = context
                .codebase
                .get_method(method_id.get_class_name().as_bytes(), method_id.get_method_name().as_bytes())
            else {
                continue;
            };
            // The `parse`, `tryParse` and `op_Equality` the codex writes span the whole type and give its own type
            // parameters back, which uses none of them.
            if function_like.span == class_like_metadata.span {
                continue;
            }

            // Check parameters
            for param in &function_like.parameters {
                if let Some(type_metadata) = &param.type_metadata
                    && type_contains_template_param(&type_metadata.type_union, *template_name, class_name)
                {
                    continue 'templates;
                }

                if let Some(type_metadata) = &param.closure_this_type
                    && type_contains_template_param(&type_metadata.type_union, *template_name, class_name)
                {
                    continue 'templates;
                }
            }

            // Check return type
            if let Some(return_type_metadata) = &function_like.return_type_metadata
                && type_contains_template_param(&return_type_metadata.type_union, *template_name, class_name)
            {
                continue 'templates;
            }
        }

        let (message, label, help) = match &sharp_class {
            Some(class) => (
                format!("Type parameter `{template_name}` is never used in {class_kind_str} `{class}`."),
                format!("Type parameter `{template_name}` is defined on this {class_kind_str} but never referenced"),
                format!("Remove `{template_name}` from `{class}<…>`."),
            ),
            None => (
                format!(
                    "Template parameter `{template_name}` is never used in {class_kind_str} `{class_original_name}`."
                ),
                format!("Template `{template_name}` is defined on this {class_kind_str} but never referenced"),
                format!(
                    "Remove the unused `@template {template_name}` from the docblock, or use it in a property, method signature, or inherited type."
                ),
            ),
        };

        context.collector.report_with_code(
            IssueCode::UnusedTemplateParameter,
            Issue::warning(message)
                .with_annotation(Annotation::primary(class_name_span).with_message(label))
                .with_help(help),
        );
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Class<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = context.resolved_names.get(&self.name);

        let Some(class_like_metadata) = context.codebase.get_class_like(name) else {
            tracing::warn!("Class {} not found in codebase", BytesDisplay(name));

            return Ok(());
        };

        analyze_class_like_attributes(context, artifacts, self.attribute_lists.as_slice(), class_like_metadata)?;

        if class_like_metadata.span != self.span() {
            report_duplicate_definition(context, "Class", "class", name, self.span(), class_like_metadata.span);
            return Ok(());
        }
        if context.codebase.parses(name) {
            r#struct::report_keys_read_twice(context, class_like_metadata, self.name.span);
        }

        // Call plugin on_enter_class hooks
        if context.plugin_registry.has_class_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_enter_class(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        analyze_class_like(
            context,
            artifacts,
            Some(self.name.span),
            self.span(),
            self.extends.as_ref(),
            self.implements.as_ref(),
            self.inheritance.as_ref(),
            class_like_metadata,
            self.members.as_slice(),
        )?;

        if context.settings.enforce_class_finality
            && !class_like_metadata.flags.is_final()
            && !class_like_metadata.flags.is_abstract()
            && !class_like_metadata.flags.is_public_api()
            && class_like_metadata.child_class_likes.as_ref().is_none_or(|children| children.is_empty())
        {
            context.collector.report_with_code(
                IssueCode::ClassMustBeFinal,
                Issue::warning(format!(
                    "Class `{}` should be declared `final`.",
                    display_class_like_name(context, class_like_metadata.original_name)
                ))
                    .with_annotation(
                        Annotation::primary(self.name.span)
                            .with_message("This class is not `final`, `abstract`, or marked with `@api`."),
                    )
                    .with_help(
                        "Declare the class as `final` to prevent inheritance, as `abstract` if it is meant to be extended, or add the `@api` tag to its docblock if backward compatibility must be preserved.",
                    ),
            );
        }

        if context.settings.require_api_or_internal
            && class_like_metadata.flags.is_abstract()
            && !class_like_metadata.flags.is_public_api()
            && !class_like_metadata.flags.is_internal()
        {
            context.collector.report_with_code(
                IssueCode::MissingApiOrInternal,
                Issue::warning(format!(
                    "Abstract class `{}` is missing an `@api` or `@internal` annotation.",
                    display_class_like_name(context, class_like_metadata.original_name),
                ))
                .with_annotation(
                    Annotation::primary(self.name.span)
                        .with_message("This abstract class does not declare its extensibility intent."),
                )
                .with_help(
                    "Add `@api` to indicate the class is part of the public API and may be extended by consumers, or `@internal` to indicate it is for internal use only.",
                ),
            );
        }

        override_attribute::check_override_attribute(class_like_metadata, self.members.as_slice(), context);
        listener::check_listener(class_like_metadata, self.members.as_slice(), context);

        let should_check_unused = 'check_unused: {
            if !context.settings.find_unused_definitions {
                break 'check_unused false;
            }

            if class_like_metadata.flags.is_unchecked() || class_like_metadata.has_incomplete_hierarchy() {
                break 'check_unused false;
            }

            if !context.settings.diff {
                break 'check_unused true;
            }

            if context.codebase.safe_symbols.contains(&class_like_metadata.name) {
                break 'check_unused false;
            }

            true
        };

        if should_check_unused {
            let additional_symbol_references = context.additional_symbol_references;
            let unused_members = unused_members::check_unused_members_with_transitivity(
                class_like_metadata.name,
                self.span(),
                class_like_metadata,
                &artifacts.symbol_references,
                additional_symbol_references,
                context,
            );

            unused_members::check_write_only_properties(
                class_like_metadata.name,
                self.span(),
                class_like_metadata,
                &artifacts.symbol_references,
                additional_symbol_references,
                &unused_members,
                context,
            );
        }

        // Call plugin on_leave_class hooks
        if context.plugin_registry.has_class_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_leave_class(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Interface<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = context.resolved_names.get(&self.name);
        let Some(class_like_metadata) = context.codebase.get_class_like(name) else {
            tracing::warn!("Interface {} not found in codebase", BytesDisplay(name));

            return Ok(());
        };

        analyze_class_like_attributes(context, artifacts, self.attribute_lists.as_slice(), class_like_metadata)?;

        if class_like_metadata.span != self.span() {
            report_duplicate_definition(context, "Interface", "interface", name, self.span(), class_like_metadata.span);
            return Ok(());
        }

        // Call plugin on_enter_interface hooks
        if context.plugin_registry.has_interface_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_enter_interface(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        analyze_class_like(
            context,
            artifacts,
            Some(self.name.span),
            self.span(),
            self.extends.as_ref(),
            None,
            self.inheritance.as_ref(),
            class_like_metadata,
            self.members.as_slice(),
        )?;

        if context.settings.require_api_or_internal
            && !class_like_metadata.flags.is_public_api()
            && !class_like_metadata.flags.is_internal()
        {
            context.collector.report_with_code(
                IssueCode::MissingApiOrInternal,
                Issue::warning(format!(
                    "Interface `{}` is missing an `@api` or `@internal` annotation.",
                    display_class_like_name(context, class_like_metadata.original_name),
                ))
                .with_annotation(
                    Annotation::primary(self.name.span)
                        .with_message("This interface does not declare its extensibility intent."),
                )
                .with_help(
                    "Add `@api` to indicate the interface is part of the public API and may be implemented by consumers, or `@internal` to indicate it is for internal use only.",
                ),
            );
        }

        override_attribute::check_override_attribute(class_like_metadata, self.members.as_slice(), context);
        listener::check_listener(class_like_metadata, self.members.as_slice(), context);

        // Call plugin on_leave_interface hooks
        if context.plugin_registry.has_interface_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_leave_interface(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

/// A PHP# event, spec section 15, analyzes as the final class it runs as, whose constructor's parameters are its
/// get-only properties. Its header names interfaces only, because no event extends another.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Event<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        _block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        // The scanner registers every event, or the class-like declared first under its name.
        let name = context.resolved_names.get(&self.name);

        context
            .codebase
            .get_class_like(name)
            .map_or(Ok(()), |class_like_metadata| analyze_event(self, class_like_metadata, context, artifacts))
    }
}

fn analyze_event<'ctx, 'arena, A>(
    event: &Event<'arena>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    // An interface or a class declared first in the file under the event's name keeps it, and has no constructor.
    if class_like_metadata.span != event.span() {
        let name = class_like_metadata.original_name.as_bytes();
        report_duplicate_definition(context, "Event", "event", name, event.span(), class_like_metadata.span);
        return Ok(());
    }

    let codebase = context.codebase;
    let named_class_likes: Vec<_> = event
        .inheritance
        .iter()
        .flat_map(|inheritance| inheritance.types.iter())
        .filter_map(|type_name| Some((type_name, codebase.get_class_like(context.resolved_names.get(type_name))?)))
        // A plain PHP trait works everywhere an interface does, spec section 22.
        .filter(|(_, named)| !named.kind.is_interface() && !named.kind.is_trait())
        .collect();

    for (type_name, named) in &named_class_likes {
        context.collector.report_with_code(
            IssueCode::InvalidEvent,
            Issue::error(format!(
                "An event's header names interfaces and traits only, and `{}` is neither.",
                display_class_like_name(context, named.original_name)
            ))
            .with_annotation(Annotation::primary(type_name.span()).with_message("Named here."))
            .with_note("No event extends another: a family of events shares an interface."),
        );
    }

    // The class the header names is the event's parent, which every other check would read.
    if !named_class_likes.is_empty() {
        return Ok(());
    }

    analyze_class_like(
        context,
        artifacts,
        Some(event.name.span),
        event.span(),
        None,
        None,
        event.inheritance.as_ref(),
        class_like_metadata,
        &[],
    )?;

    // The constructor has no body: its parameters' types and defaults are checked as a method's.
    let constructor = context.codebase.get_method(class_like_metadata.name.as_bytes(), b"__construct");
    constructor.map_or(Ok(()), |constructor| {
        let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, constructor.name)));
        scope.set_class_like(Some(class_like_metadata));
        scope.set_function_like(Some(constructor));

        let mut constructor_block_context = BlockContext::new(scope, context.settings.register_super_globals);
        analyze_function_like(
            context,
            artifacts,
            &mut constructor_block_context,
            constructor,
            &event.parameter_list,
            None,
            FunctionLikeBody::Statements(&[], event.parameter_list.span()),
            None,
        )
        .map(drop)
    })
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Trait<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = context.resolved_names.get(&self.name);
        let Some(class_like_metadata) = context.codebase.get_class_like(name) else {
            tracing::warn!("Trait {} not found in codebase", BytesDisplay(name));

            return Ok(());
        };

        analyze_class_like_attributes(context, artifacts, self.attribute_lists.as_slice(), class_like_metadata)?;

        if class_like_metadata.span != self.span() {
            report_duplicate_definition(context, "Trait", "trait", name, self.span(), class_like_metadata.span);
            return Ok(());
        }

        // Call plugin on_enter_trait hooks
        if context.plugin_registry.has_trait_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_enter_trait(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        analyze_class_like(
            context,
            artifacts,
            Some(self.name.span),
            self.span(),
            None,
            None,
            None,
            class_like_metadata,
            self.members.as_slice(),
        )?;

        if context.settings.require_api_or_internal
            && !class_like_metadata.flags.is_public_api()
            && !class_like_metadata.flags.is_internal()
        {
            context.collector.report_with_code(
                IssueCode::MissingApiOrInternal,
                Issue::warning(format!(
                    "Trait `{}` is missing an `@api` or `@internal` annotation.",
                    class_like_metadata.original_name,
                ))
                .with_annotation(
                    Annotation::primary(self.name.span)
                        .with_message("This trait does not declare its extensibility intent."),
                )
                .with_help(
                    "Add `@api` to indicate the trait is part of the public API and may be used by consumers, or `@internal` to indicate it is for internal use only.",
                ),
            );
        }

        override_attribute::check_override_attribute(class_like_metadata, self.members.as_slice(), context);

        // Call plugin on_leave_trait hooks
        if context.plugin_registry.has_trait_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_leave_trait(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

/// Spec sections 28 and 32: a module's bindings are checked where they are written. A folder's `Module.sharp` is also
/// its namespace's class-like `Module`, and its laws are analyzed as a class's. Semantics already refused a module its
/// file can't hold, and the scanner registered none.
impl<'ast, 'arena> Analyzable<'ast, 'arena> for Module<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        _block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let span = self.span();
        let Some(module) = context.codebase.modules.iter().find(|module| module.span == span) else {
            return Ok(());
        };
        crate::statement::module::analyze_bindings(context, artifacts, self, module)?;

        if module.kind != ModuleKind::Base {
            return Ok(());
        }

        // The scanner registers the module of every `Module.sharp`, or the class-like declared first under its name.
        let name = context.resolved_names.get(&self.module);
        context
            .codebase
            .get_class_like(name)
            .map_or(Ok(()), |class_like_metadata| analyze_module(self, name, class_like_metadata, context, artifacts))
    }
}

fn analyze_module<'ctx, 'arena, A>(
    module: &Module<'arena>,
    name: &[u8],
    class_like_metadata: &'ctx ClassLikeMetadata,
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if class_like_metadata.span != module.span() {
        report_duplicate_definition(context, "Module", "module", name, module.span(), class_like_metadata.span);
        return Ok(());
    }

    let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, mago_word::empty_word())));
    scope.set_class_like(Some(class_like_metadata));
    scope.set_static(true);

    let mut block_context = BlockContext::new(scope, context.settings.register_super_globals);
    for member in &module.members {
        if let ModuleMember::Law(law) = member {
            law.analyze(context, &mut block_context, artifacts)?;
        }
    }

    Ok(())
}

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Enum<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        let name = context.resolved_names.get(&self.name);

        let Some(class_like_metadata) = context.codebase.get_class_like(name) else {
            tracing::warn!("Enum {} not found in codebase", BytesDisplay(name));

            return Ok(());
        };

        analyze_class_like_attributes(context, artifacts, self.attribute_lists.as_slice(), class_like_metadata)?;

        if class_like_metadata.span != self.span() {
            report_duplicate_definition(context, "Enum", "enum", name, self.span(), class_like_metadata.span);
            return Ok(());
        }

        // Call plugin on_enter_enum hooks
        if context.plugin_registry.has_enum_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_enter_enum(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        analyze_class_like(
            context,
            artifacts,
            Some(self.name.span),
            self.span(),
            None,
            self.implements.as_ref(),
            self.inheritance.as_ref(),
            class_like_metadata,
            self.members.as_slice(),
        )?;

        check_duplicate_enum_case_values(context, artifacts, name, self);

        override_attribute::check_override_attribute(class_like_metadata, self.members.as_slice(), context);
        listener::check_listener(class_like_metadata, self.members.as_slice(), context);

        if context.settings.find_unused_definitions
            && !class_like_metadata.flags.is_unchecked()
            && !class_like_metadata.has_incomplete_hierarchy()
        {
            let additional_symbol_references = context.additional_symbol_references;
            unused_members::check_unused_members_with_transitivity(
                class_like_metadata.name,
                self.span(),
                class_like_metadata,
                &artifacts.symbol_references,
                additional_symbol_references,
                context,
            );
        }

        // Call plugin on_leave_enum hooks
        if context.plugin_registry.has_enum_hooks() {
            let mut hook_context = HookContext::new(context, block_context, artifacts);
            context.plugin_registry.on_leave_enum(self, class_like_metadata, &mut hook_context)?;
            for reported in hook_context.take_issues() {
                context.collector.report_with_code(reported.code, reported.issue);
            }
        }

        Ok(())
    }
}

fn check_duplicate_enum_case_values<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    enum_name: &[u8],
    r#enum: &Enum<'arena>,
) where
    A: Arena,
{
    let mut seen: Vec<(Word, &[u8], Span)> = Vec::new();

    for member in &r#enum.members {
        let ClassLikeMember::EnumCase(case) = member else {
            continue;
        };

        let EnumCaseItem::Backed(item) = &case.item else {
            continue;
        };

        let case_name = item.name.value;

        let Some(value_type) = artifacts.get_expression_type(item.value) else {
            continue;
        };

        if !value_type.is_single() {
            continue;
        }

        let atomic = value_type.get_single();
        let is_literal = match atomic {
            TAtomic::Scalar(s) => s.is_literal_value(),
            _ => false,
        };

        if !is_literal {
            continue;
        }

        let value_id = value_type.get_id();
        let value_span = item.value.span();

        if let Some((_, prev_case_name, prev_span)) = seen.iter().find(|(id, _, _)| *id == value_id) {
            let enum_name = if context.dialect.is_sharp() {
                display_class_like_name(context, word(enum_name)).to_string()
            } else {
                String::from_utf8_lossy(enum_name).into_owned()
            };
            let case_name_disp = BytesDisplay(case_name);
            let prev_case_name_disp = BytesDisplay(prev_case_name);
            context.collector.report_with_code(
                IssueCode::DuplicateEnumCaseValue,
                Issue::error(format!(
                    "Duplicate value in enum `{enum_name}`: case `{case_name_disp}` has the same value as case `{prev_case_name_disp}`."
                ))
                .with_annotation(
                    Annotation::primary(value_span).with_message("This value is a duplicate"),
                )
                .with_annotation(
                    Annotation::secondary(*prev_span)
                        .with_message(format!("Case `{prev_case_name_disp}` already uses this value")),
                )
                .with_help("Each case in a backed enum must have a unique value."),
            );
        } else {
            seen.push((value_id, case_name, value_span));
        }
    }
}

pub(crate) fn analyze_class_like<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    name_span: Option<Span>,
    declaration_span: Span,
    extends_ast: Option<&'ast Extends<'arena>>,
    implements_ast: Option<&'ast Implements<'arena>>,
    inheritance_ast: Option<&'ast Inheritance<'arena>>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    members: &'ast [ClassLikeMember<'arena>],
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    if context.settings.diff && context.codebase.safe_symbols.contains(&class_like_metadata.name) {
        return Ok(());
    }

    for parent_class in &class_like_metadata.all_parent_classes {
        artifacts.symbol_references.add_symbol_reference_to_symbol(class_like_metadata.name, *parent_class, true);
    }

    for parent_interface in &class_like_metadata.all_parent_interfaces {
        artifacts.symbol_references.add_symbol_reference_to_symbol(class_like_metadata.name, *parent_interface, true);
    }

    for trait_name in &class_like_metadata.used_traits {
        artifacts.symbol_references.add_symbol_reference_to_symbol(class_like_metadata.name, *trait_name, true);
    }

    if class_like_metadata.flags.is_unchecked() {
        return Ok(());
    }

    context.prepare_class_initializers(class_like_metadata)?;
    report_class_like_named_as_a_namespace(context, class_like_metadata, name_span.unwrap_or(declaration_span));
    report_namespace_named_as_a_plain_php_class_like(
        context,
        class_like_metadata,
        name_span.unwrap_or(declaration_span),
    );

    let name = display_class_like_name(context, class_like_metadata.original_name);

    let mut checked_signatures: HashSet<(Word, Word)> = HashSet::default();

    let mut extended_types: Vec<Identifier<'arena>> =
        extends_ast.iter().flat_map(|e| e.types.iter()).copied().collect();
    let mut implemented_types: Vec<Identifier<'arena>> =
        implements_ast.iter().flat_map(|i| i.types.iter()).copied().collect();
    let mut header_traits: Vec<Identifier<'arena>> = Vec::new();

    // A PHP# header, `: Base, IFace, Trait`, names an interface's parents, or a class's or an enum's parent and traits
    // among its interfaces: the one class the populator linked as the parent, and every trait, which it uses. A generic
    // type there, as in `: PaginatedList<Order>`, names its class before its type arguments, and the checker refuses
    // any other type there. `List`, `Map` and `Class` have no resolved name, because no class is one.
    for type_name in inheritance_ast.iter().flat_map(|inheritance| inheritance.names()) {
        let Some(resolved) = context.resolved_names.resolve(&type_name) else {
            continue;
        };
        let is_parent = class_like_metadata.kind.is_interface()
            || class_like_metadata
                .direct_parent_class
                .is_some_and(|parent| parent.as_bytes().eq_ignore_ascii_case(resolved));

        if is_parent {
            extended_types.push(type_name);
        } else if context.codebase.get_trait(resolved).is_some() {
            header_traits.push(type_name);
        } else {
            implemented_types.push(type_name);
        }
    }

    // The traits of a PHP# header compose as one `use` statement does, before the class's own `use` statements.
    let trait_uses: Vec<(Span, Vec<&Identifier<'arena>>)> = inheritance_ast
        .filter(|_| !header_traits.is_empty())
        .map(|inheritance| (inheritance.span(), header_traits.iter().collect()))
        .into_iter()
        .chain(members.iter().filter_map(|member| match member {
            ClassLikeMember::TraitUse(trait_use) => Some((trait_use.span(), trait_use.trait_names.iter().collect())),
            _ => None,
        }))
        .collect();

    if class_like_metadata.flags.is_sharp() {
        check_sharp_reached_twice(context, class_like_metadata, inheritance_ast, &mut checked_signatures);
    }
    check_class_like_extends(context, class_like_metadata, &extended_types);
    check_class_like_implements(context, artifacts, class_like_metadata, &implemented_types, &mut checked_signatures);

    for (_, used_types) in &trait_uses {
        check_class_like_use(context, class_like_metadata, used_types);
    }

    if context.dialect.is_sharp() && (class_like_metadata.kind.is_class() || class_like_metadata.kind.is_enum()) {
        let header_traits: Vec<Word> =
            header_traits.iter().map(|header_trait| word(context.resolved_names.get(header_trait))).collect();
        check_members_given_twice(context, class_like_metadata, &header_traits, name_span.unwrap_or(declaration_span));
    }

    let has_complete_hierarchy = !class_like_metadata.has_incomplete_hierarchy();
    if context.dialect.is_sharp() && has_complete_hierarchy && class_like_metadata.kind.is_class() {
        method::check_super_call(context, class_like_metadata, members, name_span.unwrap_or(declaration_span));
    }

    if context.dialect.is_sharp() {
        check_foreign_statics(context, class_like_metadata);
    }

    if has_complete_hierarchy && !class_like_metadata.kind.is_trait() && !class_like_metadata.flags.is_abstract() {
        for (method_name, method_id) in &class_like_metadata.declaring_method_ids {
            if class_like_metadata.kind.is_enum() {
                if method_name.as_bytes().eq_ignore_ascii_case(b"cases") {
                    continue;
                }

                if class_like_metadata.enum_type.is_some()
                    && (method_name.as_bytes().eq_ignore_ascii_case(b"from")
                        || method_name.as_bytes().eq_ignore_ascii_case(b"tryFrom"))
                {
                    continue;
                }
            }

            let Some(declaring_class_like_metadata) =
                context.codebase.get_class_like(method_id.get_class_name().as_bytes())
            else {
                continue;
            };

            let Some(function_like) = context
                .codebase
                .get_method(method_id.get_class_name().as_bytes(), method_id.get_method_name().as_bytes())
            else {
                continue;
            };

            let Some(method_metadata) = function_like.method_metadata.as_ref() else {
                continue;
            };

            if method_metadata.is_abstract {
                let fqcn = declaring_class_like_metadata.original_name;
                let method_span = function_like.name_span.unwrap_or(function_like.span);
                // PHP keeps the lowercase name Mago looks the method up by, and PHP# names it as it is declared.
                let abstract_method = display_member(
                    context,
                    fqcn,
                    if context.dialect.is_sharp() { function_like.original_name } else { *method_name },
                );

                context.collector.report_with_code(
                    IssueCode::UnimplementedAbstractMethod,
                    Issue::error(format!(
                        "Class `{name}` does not implement the abstract method `{method_name}`.",
                    ))
                    .with_annotation(
                        Annotation::primary(name_span.unwrap_or(declaration_span))
                            .with_message(format!("`{name}` is not abstract and must implement this method")),
                    )
                    .with_annotation(
                        Annotation::secondary(method_span).with_message(
                            format!("`{abstract_method}` is defined as abstract here")
                        ),
                    )
                    .with_note("When a concrete class extends an abstract class or implements an interface, it must provide an implementation for all inherited abstract methods.".to_string())
                    .with_help(format!(
                        "You can either implement the `{method_name}` method in `{name}`, or declare `{name}` as an abstract class.",
                    )),
                );
            }
        }

        for property_name in class_like_metadata.declaring_property_ids.keys() {
            let current_property = class_like_metadata.properties.get(property_name);

            for parent_fqcn in class_like_metadata
                .all_parent_classes
                .iter()
                .chain(class_like_metadata.all_parent_interfaces.iter())
                .chain(class_like_metadata.used_traits.iter())
            {
                let Some(parent_metadata) = context.codebase.get_class_like(parent_fqcn.as_bytes()) else {
                    continue;
                };

                let Some(parent_property) = parent_metadata.properties.get(property_name) else {
                    continue;
                };

                for (hook_name, hook_metadata) in &parent_property.hooks {
                    if !hook_metadata.is_abstract {
                        continue;
                    }

                    let is_implemented = current_property
                        .map(|p| {
                            if p.hooks.is_empty() {
                                !p.flags.is_virtual_property()
                            } else {
                                // Property has hooks - check if the specific hook is implemented
                                if p.hooks.get(hook_name).is_some_and(|h| !h.is_abstract) {
                                    return true;
                                }

                                p.hooks.values().any(|h| !h.is_abstract)
                            }
                        })
                        .unwrap_or_else(|| {
                            let mut all_parent_class = class_like_metadata
                                .all_parent_classes
                                .iter()
                                .chain(class_like_metadata.used_traits.iter());

                            all_parent_class.any(|parent_class_fqcn| {
                                context
                                    .codebase
                                    .get_class_like(parent_class_fqcn.as_bytes())
                                    .and_then(|parent| parent.properties.get(property_name))
                                    .is_some_and(|prop| {
                                        if prop.hooks.get(hook_name).is_some_and(|h| !h.is_abstract) {
                                            return true;
                                        }

                                        !prop.flags.is_virtual_property() || prop.hooks.values().any(|h| !h.is_abstract)
                                    })
                            })
                        });

                    if !is_implemented && context.dialect.is_sharp() {
                        // PHP# states a property, not its hooks, so it reports the property once.
                        report_unstated_property(
                            context,
                            class_like_metadata,
                            name_span.unwrap_or(declaration_span),
                            parent_metadata,
                            *property_name,
                            parent_property,
                            hook_metadata.span,
                        );

                        break;
                    } else if !is_implemented {
                        let fqcn = parent_metadata.original_name;
                        let hook_span = hook_metadata.span;
                        let missing_hook = format!("{property_name}::{hook_name}()");
                        let abstract_hook = format!("{fqcn}::{property_name}::{hook_name}()");
                        let property = display_property_name(context, *property_name);

                        context.collector.report_with_code(
                            IssueCode::UnimplementedAbstractPropertyHook,
                            Issue::error(format!(
                                "Class `{name}` does not implement the abstract property hook `{missing_hook}`.",
                            ))
                            .with_annotation(
                                Annotation::primary(name_span.unwrap_or(declaration_span))
                                    .with_message(format!("`{name}` is not abstract and must implement this hook")),
                            )
                            .with_annotation(
                                Annotation::secondary(hook_span)
                                    .with_message(format!("`{abstract_hook}` is defined as abstract here")),
                            )
                            .with_note("When a concrete class extends an abstract class or implements an interface, it must provide an implementation for all inherited abstract property hooks.".to_string())
                            .with_help(format!(
                                "You can either implement the `{hook_name}` hook for property `{property}` in `{name}`, or declare `{name}` as an abstract class.",
                            )),
                        );
                    }
                }
            }
        }
    }

    if !class_like_metadata.kind.is_trait() {
        check_inherited_operators(context, class_like_metadata, members, &mut checked_signatures);
        check_abstract_method_signatures(context, artifacts, class_like_metadata, &mut checked_signatures);
        check_trait_method_conflicts(context, class_like_metadata, &trait_uses);
    }

    check_trait_property_conflicts(context, class_like_metadata, &trait_uses, members);
    check_readonly_class_trait_properties(context, class_like_metadata, &trait_uses);
    if class_like_metadata.flags.is_sharp() {
        check_sharp_template_variance(context, class_like_metadata, inheritance_ast);
        check_sharp_serializable(context, class_like_metadata);
    } else {
        // A PHP# class that reaches a generic class-like twice is refused whole by `check_sharp_reached_twice`.
        check_template_variance_positions(context, class_like_metadata);
        check_uninhabitable_diamonds(context, class_like_metadata);
    }

    if !class_like_metadata.template_types.is_empty() {
        for (template_name, _) in &class_like_metadata.template_types {
            let (resolved_template_name, _) = context.scope.resolve(NameKind::Default, template_name);
            if let Some(conflicting_class) = context.codebase.get_class_like(&resolved_template_name) {
                let conflicting_name = &conflicting_class.name;
                let conflicting_class_span = conflicting_class.name_span.unwrap_or(conflicting_class.span);

                context.collector.report_with_code(
                    IssueCode::NameAlreadyInUse,
                    Issue::error(format!(
                        "In class `{name}`, the template parameter `{template_name}` conflicts with an existing class.",
                    ))
                    .with_annotation(
                        Annotation::primary(name_span.unwrap_or(declaration_span))
                            .with_message("The docblock for this class defines the conflicting template parameter"),
                    )
                    .with_annotation(
                        Annotation::secondary(conflicting_class_span)
                            .with_message(format!("The conflicting type `{conflicting_name}` is defined here")),
                    )
                    .with_note("Template parameter names (from `@template`) must not conflict with existing classes, interfaces, enums, or traits in the same scope.")
                    .with_help(format!(
                        "In the docblock for the `{name}` type, rename the `@template {template_name}` parameter to avoid this naming collision.",
                    )),
                );
            }
        }
    }

    if has_complete_hierarchy {
        check_unused_template_parameters(context, class_like_metadata);
    }
    check_class_like_properties(context, class_like_metadata);
    check_docblock_declared_members(context, class_like_metadata, declaration_span);

    let mut scope = ScopeContext::new(ReferenceOrigin::Symbol((class_like_metadata.name, mago_word::empty_word())));
    scope.set_class_like(Some(class_like_metadata));
    scope.set_static(true);

    let mut block_context = BlockContext::new(scope, context.settings.register_super_globals);
    // A PHP# property's initial value and accessor bodies name its class's type parameters, as a method's body does
    // through the type parameters the codex gives the method, spec section 11.
    let property_type_context =
        (context.dialect.is_sharp() && !class_like_metadata.template_types.is_empty()).then(|| {
            class_like_metadata
                .template_types
                .iter()
                .fold(TypeResolutionContext::new(), |type_context, (name, template)| {
                    type_context.with_template_definition(*name, vec![template.clone()])
                })
        });

    for member in members {
        match member {
            ClassLikeMember::Constant(class_like_constant) => {
                if context.settings.diff
                    && let Some(item) = class_like_constant.items.first()
                {
                    let constant_name = word(item.name.value);
                    if context.codebase.safe_symbol_members.contains(&(class_like_metadata.name, constant_name)) {
                        continue;
                    }
                }

                missing_type_hints::check_constant_type_hint(context, class_like_constant);

                class_like_constant.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::Property(property) => {
                if context.settings.diff {
                    let first_var_name = match property {
                        Property::Plain(plain) => plain.items.first().map(|item| word(item.variable().name)),
                        Property::Hooked(_) | Property::Computed(_) => Some(word(property.first_variable().name)),
                    };

                    if let Some(var_name) = first_var_name
                        && context.codebase.safe_symbol_members.contains(&(class_like_metadata.name, var_name))
                    {
                        continue;
                    }
                }

                missing_type_hints::check_property_type_hint(context, class_like_metadata, property);

                // Check for imprecise type hints (bare `array` or `iterable`)
                let first_property_name = match property {
                    Property::Plain(plain) => plain.items.first().map(|item| word(item.variable().name)),
                    Property::Hooked(_) | Property::Computed(_) => Some(word(property.first_variable().name)),
                };

                let prop_meta = first_property_name.and_then(|name| class_like_metadata.properties.get(&name));
                missing_type_hints::check_imprecise_property_type_hint(context, property, prop_meta);

                // A PHP# field's name has no `$`, and the codebase keys every property by its PHP name.
                let property_names: Vec<Word> = match property {
                    Property::Plain(plain) => {
                        plain.items.iter().map(|item| php_variable_name(item.variable().name)).collect()
                    }
                    Property::Hooked(_) | Property::Computed(_) => {
                        vec![php_variable_name(property.first_variable().name)]
                    }
                };

                for property_name in property_names {
                    if let Some(prop_meta) = class_like_metadata.properties.get(&property_name)
                        && let Some(type_meta) = &prop_meta.type_metadata
                    {
                        report_undefined_type_references(context, type_meta);
                        report_invalid_template_arguments(context, type_meta, HintPlace::Declaration);

                        if type_meta.from_docblock
                            && let Some(type_decl_meta) = &prop_meta.type_declaration_metadata
                        {
                            report_undefined_type_references(context, type_decl_meta);
                            report_property_type_mismatch(context, class_like_metadata, type_meta, type_decl_meta);
                        }
                    }
                }

                let outer_type_context = property_type_context
                    .clone()
                    .map(|type_context| std::mem::replace(&mut context.type_resolution_context, type_context));
                let analyzed = property.analyze(context, &mut block_context, artifacts);
                if let Some(outer_type_context) = outer_type_context {
                    context.type_resolution_context = outer_type_context;
                }
                analyzed?;
            }
            ClassLikeMember::EnumCase(enum_case) => {
                if context.settings.diff {
                    let case_name = word(enum_case.item.name().value);
                    if context.codebase.safe_symbol_members.contains(&(class_like_metadata.name, case_name)) {
                        continue;
                    }
                }

                enum_case.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::Method(method) => {
                method.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::Operator(operator) => {
                operator.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::Law(law) => {
                law.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::NamedConstructor(named_constructor) => {
                named_constructor.analyze(context, &mut block_context, artifacts)?;
            }
            ClassLikeMember::Extension(extension) => {
                extension.analyze(context, &mut block_context, artifacts)?;
            }
            _ => {}
        }
    }

    if context.dialect.is_sharp() && class_like_metadata.flags.is_sharp() {
        report_statics_named_like_class_members(context, members);
    }

    // Check trait constant overrides AFTER constants have been analyzed
    // so we can compare their inferred values
    check_class_like_constants(context, class_like_metadata, members);

    if has_complete_hierarchy {
        crate::readonly::finalize_class_writes(context, artifacts, class_like_metadata);
        initialization::check_storage_initialization(context, class_like_metadata);
        initialization::check_property_initialization(
            context,
            artifacts,
            class_like_metadata,
            declaration_span,
            name_span,
        )?;
    }

    Ok(())
}

/// Reports each static of a PHP# class-like named like a member of `Class`, spec section 25, as TypeScript's error 2699
/// refuses a static named like a member of `Function`: `type.y` through a class value reaches a static method, a
/// static property, a constant and an enum case alike, and `Class`'s own member wins, so the static would never run. A
/// member name is compared ignoring case, as PHP calls a method, and the message quotes each name as written.
fn report_statics_named_like_class_members<A>(context: &mut Context<'_, '_, A>, members: &[ClassLikeMember<'_>])
where
    A: Arena,
{
    let mut statics: Vec<(&[u8], Span)> = Vec::new();
    for member in members {
        match member {
            ClassLikeMember::Method(method) if method.is_static() => {
                statics.push((method.name.value, method.name.span));
            }
            ClassLikeMember::Property(property) if property.modifiers().iter().any(Modifier::is_static) => {
                for variable in property.variables() {
                    statics.push((variable.name.strip_prefix(b"$").unwrap_or(variable.name), variable.span));
                }
            }
            ClassLikeMember::Constant(constant) => {
                statics.extend(constant.items.iter().map(|item| (item.name.value, item.name.span)));
            }
            ClassLikeMember::EnumCase(enum_case) => {
                statics.push((enum_case.item.name().value, enum_case.item.name().span));
            }
            _ => {}
        }
    }

    for (name, span) in statics {
        let Some(member) = class_member(context.codebase, name) else {
            continue;
        };
        let (name, member) = (BytesDisplay(name), member.original_name);
        context.collector.report_with_code(
            IssueCode::NameAlreadyInUse,
            Issue::error(format!(
                "`type.{name}` on a class value reaches `Class`'s member `{member}`, never this static. Rename it."
            ))
            .with_annotation(Annotation::primary(span).with_message("Declared here.")),
        );
    }
}

/// Reports a PHP# class or event, declared at `span`, that does not give the property `property_name` that `parent`
/// states at `stated`. An event gives its properties through its parameters, which are set once, by `new`, spec
/// section 15, so it cannot give a property stated with `set`.
fn report_unstated_property<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    span: Span,
    parent: &ClassLikeMetadata,
    property_name: Word,
    parent_property: &PropertyMetadata,
    stated: Span,
) where
    A: Arena,
{
    let name = display_class_like_name(context, class_like_metadata.original_name);
    let property = display_property_name(context, property_name);
    let mixed = get_mixed();
    let written = display_sharp_type(
        context,
        parent_property.type_metadata.as_ref().map_or(&mixed, |metadata| &metadata.type_union),
    );
    let parent_name = display_class_like_name(context, parent.original_name);
    let settable = parent_property.hooks.contains_key(&word(b"set"));
    let issue = if class_like_metadata.flags.is_event() && settable {
        Issue::error(format!(
            "`{name}` cannot give `{property}`, which `{parent_name}` states with `set`: an event's properties are set once, by `new`."
        ))
    } else {
        let help = if class_like_metadata.flags.is_event() {
            format!("Add the parameter `{written} {property}`.")
        } else if settable {
            format!("Add the property `public {written} {property} {{ get; set; }}`.")
        } else {
            format!("Add the property `public {written} {property} {{ get; }}`.")
        };

        Issue::error(format!("`{name}` does not give `{property}`, which `{parent_name}` states.")).with_help(help)
    };

    context.collector.report_with_code(
        IssueCode::UnimplementedAbstractPropertyHook,
        issue
            .with_annotation(Annotation::primary(span).with_message("Declared here."))
            .with_annotation(Annotation::secondary(stated).with_message("Stated here.")),
    );
}

#[allow(clippy::unwrap_used)]
fn check_class_like_extends<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    extended_types: &[Identifier<'arena>],
) where
    A: Arena,
{
    // This check only applies to classes and interfaces, which can use `extends`.
    if !class_like_metadata.kind.is_class() && !class_like_metadata.kind.is_interface() {
        return;
    }

    if extended_types.is_empty() {
        return;
    }

    let using_kind_str = class_like_metadata.kind.as_str();
    let using_kind_capitalized =
        format!("{}{}", using_kind_str.chars().next().unwrap().to_uppercase(), &using_kind_str[1..]);
    let using_name = display_class_like_name(context, class_like_metadata.original_name);
    let using_class_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);

    for &extended_type in extended_types {
        let extended_type_str = context.resolved_names.get(&extended_type);
        let extended_class_metadata = context.codebase.get_class_like(extended_type_str);

        // Case: The extended type does not exist.
        let Some(extended_class_metadata) = extended_class_metadata else {
            let extended_name = BytesDisplay(extended_type.value());

            context.collector.report_with_code(
                IssueCode::NonExistentClassLike,
                Issue::error(format!("{using_kind_capitalized} `{using_name}` cannot extend unknown type `{extended_name}`"))
                    .with_annotation(Annotation::primary(extended_type.span()).with_message("This type could not be found"))
                    .with_note("The checker could not find a definition for this class, interface, or trait.")
                    .with_help("Ensure the name is correct, including its namespace, and that it is properly defined and autoloadable."),
            );
            continue;
        };

        let extended_name = display_class_like_name(context, extended_class_metadata.original_name);
        let extended_kind_str = extended_class_metadata.kind.as_str();
        let extended_kind_prefix =
            if extended_class_metadata.kind.is_class() || extended_class_metadata.kind.is_trait() { "a" } else { "an" };
        let extended_class_span = extended_class_metadata.name_span.unwrap_or(extended_class_metadata.span);

        if extended_class_metadata.flags.is_deprecated() {
            context.collector.report_with_code(
                IssueCode::DeprecatedClass,
                Issue::warning(format!("Use of deprecated class `{extended_name}` in `extends` clause"))
                    .with_annotation(Annotation::primary(extended_type.span()).with_message("This class is marked as deprecated"))
                    .with_annotation(Annotation::secondary(extended_class_span).with_message(format!("`{extended_name}` was marked deprecated here")))
                    .with_note("The parent type is deprecated and may be removed in a future version, which would break this child type.")
                    .with_help("Consider refactoring to avoid extending this type, or consult its documentation for alternatives."),
            );
        }

        if context.settings.check_experimental
            && extended_class_metadata.flags.is_experimental()
            && !class_like_metadata.flags.is_experimental()
        {
            context.collector.report_with_code(
                IssueCode::ExperimentalUsage,
                Issue::warning(format!("Usage of experimental class-like `{extended_name}`."))
                    .with_annotation(
                        Annotation::primary(extended_type.span())
                            .with_message(format!("`{extended_name}` is marked as `@experimental`.")),
                    )
                    .with_note("Experimental APIs may change or be removed without notice.")
                    .with_help("Mark the current class as `@experimental` to suppress this warning."),
            );
        }

        if class_like_metadata.kind.is_interface() {
            if !extended_class_metadata.kind.is_interface() {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Interface `{using_name}` cannot extend non-interface type `{extended_name}`"))
                        .with_annotation(Annotation::primary(extended_type.span())
                            .with_message(format!("...because it is {extended_kind_prefix} {extended_kind_str}, not an interface")))
                        .with_annotation(Annotation::secondary(extended_class_span)
                            .with_message(format!("`{extended_name}` is defined as {extended_kind_prefix} {extended_kind_str} here")))
                        .with_note("In PHP, an interface can only extend other interfaces.")
                        .with_help(format!("To resolve this, change `{extended_name}` to be an interface, or change `{using_name}` to a class if you intended to extend a class.")),
                );

                continue;
            }

            if extended_class_metadata.flags.is_enum_interface() && !class_like_metadata.flags.is_enum_interface() {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Interface `{using_name}` cannot extend enum-interface `{extended_name}`"))
                        .with_annotation(Annotation::primary(using_class_span).with_message("This interface is not an `@enum-interface`..."))
                        .with_annotation(Annotation::secondary(extended_type.span()).with_message("...but it extends an `@enum-interface`"))
                        .with_note("An interface marked with `@enum-interface` can only be extended by other interfaces that are also marked with `@enum-interface`.")
                        .with_help(format!("To resolve this, add the `@enum-interface` PHPDoc tag to `{using_name}`, or extend a regular, non-enum interface.")),
                );
            }
        }

        if class_like_metadata.kind.is_class() {
            if !extended_class_metadata.kind.is_class() {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Class `{using_name}` cannot extend non-class type `{extended_name}`"))
                        .with_annotation(Annotation::primary(extended_type.span()).with_message(format!(
                            "...because it is {extended_kind_prefix} {extended_kind_str}, not a class"
                        )))
                        .with_annotation(Annotation::secondary(extended_class_span).with_message(format!(
                            "`{extended_name}` is defined as {extended_kind_prefix} {extended_kind_str} here"
                        )))
                        .with_note("In PHP, a class can only extend another class.")
                        .with_help("To inherit from an interface, use `implements`. To use a trait, use `use`."),
                );

                continue;
            }

            if extended_class_metadata.flags.is_struct() {
                context.collector.report_with_code(
                    IssueCode::InvalidStruct,
                    Issue::error(format!("`{extended_name}` is a struct, which can't be inherited."))
                        .with_annotation(Annotation::primary(extended_type.span()).with_message("Inherited here."))
                        .with_note("A struct is a value, so no class extends it."),
                );
            } else if extended_class_metadata.flags.is_static() {
                let written_name = BytesDisplay(extended_type.value());

                context.collector.report_with_code(
                    IssueCode::ExtendFinalClass,
                    Issue::error(format!("`{written_name}` is a static class, so no class can extend it."))
                        .with_annotation(Annotation::primary(extended_type.span()).with_message("Extended here."))
                        .with_annotation(
                            Annotation::secondary(extended_class_span)
                                .with_message(format!("`{extended_name}` is declared `static` here.")),
                        ),
                );
            } else if extended_class_metadata.flags.is_module() {
                context.collector.report_with_code(
                    IssueCode::ExtendFinalClass,
                    Issue::error(format!(
                        "`{extended_name}` is its namespace's module, not a class: no class can extend it."
                    ))
                    .with_annotation(Annotation::primary(extended_type.span()).with_message("Extended here."))
                    .with_annotation(
                        Annotation::secondary(extended_class_span)
                            .with_message(format!("`{extended_name}` is declared here.")),
                    ),
                );
            } else if extended_class_metadata.flags.is_final() {
                context.collector.report_with_code(
                    IssueCode::ExtendFinalClass,
                    Issue::error(format!("Class `{using_name}` cannot extend final class `{extended_name}`"))
                        .with_annotation(Annotation::primary(extended_type.span()).with_message("This inheritance is not allowed"))
                        .with_annotation(Annotation::secondary(extended_class_span).with_message(format!("`{extended_name}` is declared 'final' here")))
                        .with_note("A class marked as `final` cannot be extended by any other class.")
                        .with_help(format!("To resolve this, either remove the `final` keyword from `{extended_name}`, or choose a different class to extend.")),
                );
            }

            if extended_class_metadata.flags.is_readonly() && !class_like_metadata.flags.is_readonly() {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Non-readonly class `{using_name}` cannot extend readonly class `{extended_name}`"))
                        .with_annotation(Annotation::primary(using_class_span).with_message("This class is not `readonly`..."))
                        .with_annotation(Annotation::secondary(extended_class_span).with_message(format!("...but it extends `{extended_name}`, which is `readonly`")))
                        .with_note("A `readonly` class can only be extended by another `readonly` class.")
                        .with_help(format!("To resolve this, either make the `{using_name}` class `readonly`, or extend a different, non-readonly class.")),
                );
            } else if !extended_class_metadata.flags.is_readonly() && class_like_metadata.flags.is_readonly() {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Readonly class `{using_name}` cannot extend non-readonly class `{extended_name}`"))
                        .with_annotation(Annotation::primary(using_class_span).with_message("This class is `readonly`..."))
                        .with_annotation(Annotation::secondary(extended_class_span).with_message(format!("...but it extends `{extended_name}`, which is not `readonly`")))
                        .with_note("A non-`readonly` class can only be extended by another non-`readonly` class.")
                        .with_help(format!("To resolve this, either make the `{using_name}` class non-`readonly`, or extend a different, readonly class.")),
                );
            }

            if let Some(required_interface) =
                class_like_metadata.get_missing_required_interface(extended_class_metadata)
            {
                context.collector.report_with_code(
                    IssueCode::MissingRequiredInterface,
                    Issue::error(format!("Class `{using_name}` must implement required interface `{required_interface}`"))
                        .with_annotation(Annotation::primary(using_class_span).with_message(format!("...because its parent `{extended_name}` requires it")))
                        .with_annotation(Annotation::secondary(extended_class_span).with_message("Requirement declared here (likely via `@require-implements`)"))
                        .with_note("When a class uses `@require-implements`, all of its concrete child classes must implement the specified interface.")
                        .with_help(format!("Add `implements {required_interface}` to the `{using_name}` definition, or declare `{using_name}` as `abstract`.")),
                );
            }

            if !class_like_metadata.is_permitted_to_inherit(extended_class_metadata) {
                context.collector.report_with_code(
                    IssueCode::InvalidExtend,
                    Issue::error(format!("Class `{using_name}` is not permitted to extend `{extended_name}`"))
                        .with_annotation(Annotation::primary(extended_type.span()).with_message("This inheritance is restricted"))
                        .with_annotation(Annotation::secondary(extended_class_span)
                            .with_message(format!("The `@inheritors` annotation on this class does not include `{using_name}`")))
                        .with_note("The `@inheritors` annotation on a class or interface restricts which types are allowed to extend it.")
                        .with_help(format!("To allow this, add `{using_name}` to the list in the `@inheritors` PHPDoc tag for `{extended_name}`.")),
                );
            }
        }

        let actual_parameters_count =
            class_like_metadata.template_type_extends_count.get(&extended_class_metadata.name).copied().unwrap_or(0);

        check_template_parameters(
            context,
            class_like_metadata,
            extended_class_metadata,
            actual_parameters_count,
            InheritanceKind::Extends(extended_type.span()),
        );
    }
}

/// Reports `class`, which implements the sealed `interface` at `span` from another namespace, spec section 20.1.
fn report_outside_implementer<A>(
    context: &mut Context<'_, '_, A>,
    class: &ClassLikeMetadata,
    interface: &ClassLikeMetadata,
    span: Span,
) where
    A: Arena,
{
    let dotted = |name: Word| String::from_utf8_lossy(name.as_bytes()).replace('\\', ".");
    let (class_name, interface_name) = (dotted(class.original_name), dotted(interface.original_name));
    let namespace = interface_name.rsplit_once('.').map_or("", |(namespace, _)| namespace);

    context.collector.report_with_code(
        IssueCode::InvalidImplement,
        Issue::error(format!(
            "`{class_name}` cannot implement the sealed interface `{interface_name}`: its implementers sit in the namespace `{namespace}`."
        ))
        .with_annotation(Annotation::primary(span).with_message("Implemented here."))
        .with_note("A sealed interface is a closed set of classes, all in its namespace, so a `match` over it can handle each.")
        .with_help(format!("Move the class into the namespace `{namespace}`, or implement another interface.")),
    );
}

#[allow(clippy::unwrap_used)]
fn check_class_like_implements<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    class_like_metadata: &'ctx ClassLikeMetadata,
    implemented_types: &[Identifier<'arena>],
    checked_signatures: &mut HashSet<(Word, Word)>,
) where
    A: Arena,
{
    // This check only applies to classes and enums, which can use `implements`.
    if !class_like_metadata.kind.is_class() && !class_like_metadata.kind.is_enum() {
        // A separate check in the semantic analyzer will catch `implements` on an invalid type like a trait or interface.
        return;
    }

    if implemented_types.is_empty() {
        return;
    }

    let is_struct = class_like_metadata.flags.is_struct();
    let using_kind_str = if is_struct { "struct" } else { class_like_metadata.kind.as_str() };
    let using_kind_capitalized =
        format!("{}{}", using_kind_str.chars().next().unwrap().to_uppercase(), &using_kind_str[1..]);
    let using_name = display_class_like_name(context, class_like_metadata.original_name);
    let using_class_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);

    for &implemented_type in implemented_types {
        let implemented_type_str = context.resolved_names.get(&implemented_type);
        let implemented_interface_metadata = context.codebase.get_class_like(implemented_type_str);

        if let Some(implemented_metadata) = implemented_interface_metadata {
            let implemented_name = display_class_like_name(context, implemented_metadata.original_name);
            let implemented_kind_str = implemented_metadata.kind.as_str();
            let implemented_class_span = implemented_metadata.name_span.unwrap_or(implemented_metadata.span);
            let implemented_kind_prefix =
                if implemented_metadata.kind.is_class() || implemented_metadata.kind.is_trait() { "a" } else { "an" };

            // A PHP# class header names one class, which the populator linked as the parent.
            if context.dialect.is_sharp()
                && implemented_metadata.kind.is_class()
                && let Some(parent) = class_like_metadata
                    .direct_parent_class
                    .and_then(|parent| context.codebase.get_class_like(parent.as_bytes()))
            {
                let parent_name = display_class_like_name(context, parent.original_name);
                context.collector.report_with_code(
                    IssueCode::InvalidImplement,
                    Issue::error(format!(
                        "`{using_name}` names two base classes, `{parent_name}` and `{implemented_name}`."
                    ))
                    .with_annotation(Annotation::primary(implemented_type.span()).with_message("A second class."))
                    .with_note("A class has one base class, and its header's other names are interfaces and traits."),
                );

                continue;
            }

            if !implemented_metadata.kind.is_interface() {
                context.collector.report_with_code(
                    IssueCode::InvalidImplement,
                    Issue::error(format!("{using_kind_capitalized} `{using_name}` cannot implement non-interface type `{implemented_name}`"))
                        .with_annotation(Annotation::primary(implemented_type.span())
                            .with_message(format!("...because it is {implemented_kind_prefix} {implemented_kind_str}, not an interface")))
                        .with_annotation(Annotation::secondary(implemented_class_span)
                            .with_message(format!("`{implemented_name}` is defined as {implemented_kind_prefix} {implemented_kind_str} here")))
                        .with_note("The `implements` keyword is exclusively for implementing interfaces.")
                        .with_help("To inherit from a class, use `extends`. To use a trait, use `use`."),
                );

                continue;
            }

            if context.settings.check_experimental
                && implemented_metadata.flags.is_experimental()
                && !class_like_metadata.flags.is_experimental()
            {
                context.collector.report_with_code(
                    IssueCode::ExperimentalUsage,
                    Issue::warning(format!("Usage of experimental interface `{implemented_name}`."))
                        .with_annotation(
                            Annotation::primary(implemented_type.span())
                                .with_message(format!("`{implemented_name}` is marked as `@experimental`.")),
                        )
                        .with_note("Experimental APIs may change or be removed without notice.")
                        .with_help("Mark the current class as `@experimental` to suppress this warning."),
                );
            }

            if implemented_metadata.flags.is_enum_interface() && !class_like_metadata.kind.is_enum() {
                context.collector.report_with_code(
                    IssueCode::InvalidImplement,
                    Issue::error(format!("{using_kind_capitalized} `{using_name}` cannot implement enum-only interface `{implemented_name}`"))
                        .with_annotation(Annotation::primary(using_class_span).with_message(format!("This {using_kind_str} is not an enum...")))
                        .with_annotation(Annotation::secondary(implemented_type.span()).with_message("...but it implements an interface restricted to enums"))
                        .with_annotation(Annotation::secondary(implemented_class_span).with_message("This interface is marked with `@enum-interface` here"))
                        .with_note("An interface marked with `@enum-interface` can only be implemented by enums.")
                        .with_help(format!("To resolve this, either change `{using_name}` to be an enum, or implement a different, non-enum interface.")),
                );
            }

            // A sealed interface counts its implementers through the interfaces that extend it, spec section 20.1.
            let sealed = std::iter::once(implemented_metadata)
                .chain(
                    implemented_metadata
                        .all_parent_interfaces
                        .iter()
                        .filter_map(|interface| context.codebase.get_class_like(interface.as_bytes())),
                )
                .find(|interface| {
                    interface.flags.is_sealed() && !class_like_metadata.is_permitted_to_inherit(interface)
                });
            if let Some(sealed) = sealed {
                report_outside_implementer(context, class_like_metadata, sealed, implemented_type.span());
            } else if !class_like_metadata.is_permitted_to_inherit(implemented_metadata) {
                context.collector.report_with_code(
                    IssueCode::InvalidImplement,
                    Issue::error(format!("{using_kind_capitalized} `{using_name}` is not permitted to implement `{implemented_name}`"))
                         .with_annotation(Annotation::primary(implemented_type.span()).with_message("This implementation is restricted"))
                        .with_annotation(Annotation::secondary(implemented_class_span)
                            .with_message(format!("The `@inheritors` annotation on this interface does not include `{using_name}`")))
                        .with_note("The `@inheritors` annotation on an interface restricts which types are allowed to implement it.")
                        .with_help(format!("To allow this, add `{using_name}` to the list in the `@inheritors` PHPDoc tag for `{implemented_name}`.")),
                );
            }

            let actual_parameters_count = class_like_metadata
                .template_type_implements_count
                .get(&implemented_metadata.name)
                .copied()
                .unwrap_or(0);

            check_template_parameters(
                context,
                class_like_metadata,
                implemented_metadata,
                actual_parameters_count,
                InheritanceKind::Implements(implemented_type.span()),
            );

            check_interface_method_signatures(
                context,
                artifacts,
                class_like_metadata,
                implemented_metadata,
                checked_signatures,
            );
        } else if !report_unknown_sharp_class(context, implemented_type_str, implemented_type.span()) {
            let implemented_name = BytesDisplay(implemented_type.value());
            let (note, help) = if is_struct {
                ("A struct's header lists the interfaces it implements.", "Check the interface's name and its import.")
            } else {
                (
                    "The checker could not find a definition for this interface. The `implements` keyword is for interfaces only.",
                    "Ensure the name is correct, including its namespace, and that it is properly defined and autoloadable.",
                )
            };

            context.collector.report_with_code(
                IssueCode::NonExistentClassLike,
                Issue::error(format!(
                    "{using_kind_capitalized} `{using_name}` cannot implement unknown type `{implemented_name}`"
                ))
                .with_annotation(
                    Annotation::primary(implemented_type.span()).with_message("This type could not be found"),
                )
                .with_note(note)
                .with_help(help),
            );
        }
    }
}

#[allow(clippy::unwrap_used)]
fn check_class_like_use<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    used_types: &[&Identifier<'arena>],
) where
    A: Arena,
{
    let using_kind_str = class_like_metadata.kind.as_str();
    let using_kind_capitalized =
        format!("{}{}", using_kind_str.chars().next().unwrap().to_uppercase(), &using_kind_str[1..]);
    let using_name = display_class_like_name(context, class_like_metadata.original_name);
    let using_class_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);

    for &used_type in used_types {
        let used_type_str = context.resolved_names.get(used_type);
        let used_trait_metadata = context.codebase.get_class_like(used_type_str);

        let Some(used_trait_metadata) = used_trait_metadata else {
            let used_name = BytesDisplay(used_type.value());

            context.collector.report_with_code(
                IssueCode::NonExistentClassLike,
                Issue::error(format!("{using_kind_capitalized} `{using_name}` cannot use unknown type `{used_name}`"))
                    .with_annotation(Annotation::primary(used_type.span()).with_message("This type could not be found"))
                    .with_note("The checker could not find a definition for this trait. The `use` keyword is for traits only.")
                    .with_help("Ensure the name is correct, including its namespace, and that it is properly defined and autoloadable."),
            );

            continue;
        };

        let used_name = display_class_like_name(context, used_trait_metadata.original_name);
        let used_kind_str = used_trait_metadata.kind.as_str();
        let used_kind_prefix =
            if used_trait_metadata.kind.is_class() || used_trait_metadata.kind.is_trait() { "a" } else { "an" };
        let used_class_span = used_trait_metadata.name_span.unwrap_or(used_trait_metadata.span);

        // Case: Using something that is not a trait.
        if !used_trait_metadata.kind.is_trait() {
            context.collector.report_with_code(
                IssueCode::InvalidTraitUse,
                Issue::error(format!(
                    "{using_kind_capitalized} `{using_name}` cannot use non-trait type `{used_name}`"
                ))
                .with_annotation(
                    Annotation::primary(used_type.span())
                        .with_message(format!("...because it is {used_kind_prefix} {used_kind_str}, not a trait")),
                )
                .with_annotation(
                    Annotation::secondary(used_class_span)
                        .with_message(format!("`{used_name}` is defined as {used_kind_prefix} {used_kind_str} here")),
                )
                .with_note("The `use` keyword is exclusively for including traits in classes, enums, or other traits.")
                .with_help("To inherit from a class, use `extends`. To implement an interface, use `implements`."),
            );

            continue;
        }

        if context.settings.check_experimental
            && used_trait_metadata.flags.is_experimental()
            && !class_like_metadata.flags.is_experimental()
        {
            context.collector.report_with_code(
                IssueCode::ExperimentalUsage,
                Issue::warning(format!("Usage of experimental trait `{used_name}`."))
                    .with_annotation(
                        Annotation::primary(used_type.span())
                            .with_message(format!("`{used_name}` is marked as `@experimental`.")),
                    )
                    .with_note("Experimental APIs may change or be removed without notice.")
                    .with_help("Mark the current class as `@experimental` to suppress this warning."),
            );
        }

        if used_trait_metadata.flags.is_deprecated() {
            context.collector.report_with_code(
                IssueCode::DeprecatedTrait,
                Issue::error(format!("Use of deprecated trait `{used_name}` in `{using_name}`"))
                    .with_annotation(Annotation::primary(used_type.span()).with_message("This trait is marked as deprecated"))
                    .with_annotation(Annotation::secondary(used_class_span).with_message(format!("`{used_name}` was marked as deprecated here")))
                    .with_note("This trait is deprecated and may be removed in a future version, which would break the consuming type.")
                    .with_help("Consider refactoring to avoid using this trait, or consult its documentation for alternatives."),
            );
        }

        if let Some(required_interface) = class_like_metadata.get_missing_required_interface(used_trait_metadata) {
            context.collector.report_with_code(
                IssueCode::MissingRequiredInterface,
                Issue::error(format!("{using_kind_capitalized} `{using_name}` must implement required interface `{required_interface}`"))
                    .with_annotation(Annotation::primary(using_class_span).with_message(format!("...because the trait `{used_name}` requires it")))
                    .with_annotation(Annotation::secondary(used_type.span()).with_message(format!("The requirement is introduced by using `{used_name}` here")))
                    .with_note("When a trait uses `@require-implements`, any concrete class using that trait must implement the specified interface.")
                    .with_help(format!("Add `implements {required_interface}` to the `{using_name}` definition, or declare it as `abstract`.")),
            );
        }

        if let Some(required_class) = class_like_metadata.get_missing_required_extends(used_trait_metadata) {
            context.collector.report_with_code(
                IssueCode::MissingRequiredParent,
                Issue::error(format!(
                    "{using_kind_capitalized} `{using_name}` must extend required class `{required_class}`"
                ))
                .with_annotation(
                    Annotation::primary(using_class_span)
                        .with_message(format!("...because the trait `{used_name}` requires it")),
                )
                .with_annotation(
                    Annotation::secondary(used_type.span())
                        .with_message(format!("The requirement is introduced by using `{used_name}` here")),
                )
                .with_note(
                    "When a trait uses `@require-extends`, any class using that trait must extend the specified class.",
                )
                .with_help(format!(
                    "Add `extends {required_class}` to the `{using_name}` definition, or ensure it is a parent class."
                )),
            );
        }

        if !class_like_metadata.is_permitted_to_inherit(used_trait_metadata) {
            context.collector.report_with_code(
                IssueCode::InvalidTraitUse,
                Issue::error(format!(
                    "{using_kind_capitalized} `{using_name}` is not permitted to use trait `{used_name}`"
                ))
                .with_annotation(Annotation::primary(used_type.span()).with_message("This usage is restricted"))
                .with_annotation(Annotation::secondary(used_class_span).with_message(format!(
                    "The `@inheritors` annotation on this trait does not include `{using_name}`"
                )))
                .with_note("The `@inheritors` annotation on a trait restricts which types are allowed to use it.")
                .with_help(format!(
                    "To allow this, add `{using_name}` to the list in the `@inheritors` PHPDoc tag for `{used_name}`."
                )),
            );
        }

        check_template_parameters(
            context,
            class_like_metadata,
            used_trait_metadata,
            class_like_metadata.template_type_uses_count.get(&used_trait_metadata.name).copied().unwrap_or(0),
            InheritanceKind::Use(used_type.span()),
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InheritanceKind {
    Extends(Span),
    Implements(Span),
    Use(Span),
}

impl HasSpan for InheritanceKind {
    fn span(&self) -> Span {
        match self {
            InheritanceKind::Extends(span) => *span,
            InheritanceKind::Implements(span) => *span,
            InheritanceKind::Use(span) => *span,
        }
    }
}

fn check_template_parameters<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    parent_metadata: &'ctx ClassLikeMetadata,
    actual_parameters_count: usize,
    inheritance: InheritanceKind,
) where
    A: Arena,
{
    let expected_parameters_count = parent_metadata.template_types.len();
    let min_required_parameters_count =
        parent_metadata.template_types.values().take_while(|t| t.default.is_none()).count();

    let class_name = display_class_like_name(context, class_like_metadata.original_name);
    let class_kind_str = class_like_metadata.kind.as_str();
    let parent_name = display_class_like_name(context, parent_metadata.original_name);
    let class_name_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);
    let parent_definition_span = parent_metadata.name_span.unwrap_or(parent_metadata.span);
    let primary_annotation_span = inheritance.span();
    let (inheritance_keyword, inheritance_tag) = match inheritance {
        InheritanceKind::Extends(_) => ("extends", "@extends"),
        InheritanceKind::Implements(_) => ("implements", "@implements"),
        InheritanceKind::Use(_) => ("uses", "@use"),
    };

    // A PHP# class writes its type arguments in its header, `: PaginatedList<…>`, where PHP writes a docblock tag, and
    // names each class by its short name.
    let sharp_parent = context.dialect.is_sharp().then(|| short_name(parent_name));
    let written_class_name =
        if context.dialect.is_sharp() { short_name(class_name) } else { class_name.as_str_lossy().into_owned() };
    let type_parameters = and_list(&parent_metadata.template_types.keys().copied().collect::<Vec<_>>());
    let is_missing = min_required_parameters_count > actual_parameters_count;
    if is_missing || expected_parameters_count < actual_parameters_count {
        let (code, amount, expected) = if is_missing {
            (IssueCode::MissingTemplateParameter, "few", format!("at least {min_required_parameters_count}"))
        } else {
            (IssueCode::ExcessTemplateParameter, "many", expected_parameters_count.to_string())
        };
        let (message, label, parent_label, help) = match &sharp_parent {
            Some(parent) => (
                format!(
                    "Too {amount} type arguments for `{parent}`: expected {expected}, but found {actual_parameters_count}."
                ),
                format!("Too {amount} type arguments here"),
                format!(
                    "`{parent}` declares {expected_parameters_count} type parameter{}",
                    if expected_parameters_count == 1 { "" } else { "s" }
                ),
                format!(
                    "Write {}a type for {type_parameters} in the header, as in `: {parent}<…>`.",
                    if is_missing { "" } else { "only " }
                ),
            ),
            None => (
                format!(
                    "Too {amount} template arguments for `{parent_name}`: expected {expected}, but found {actual_parameters_count}."
                ),
                format!(
                    "Too {amount} template arguments provided here when `{class_name}` {inheritance_keyword} `{parent_name}`"
                ),
                format!("`{parent_name}` is defined with {expected_parameters_count} template parameters"),
                if is_missing {
                    format!(
                        "Provide all {expected_parameters_count} required template arguments in the `{inheritance_tag}` docblock tag for `{class_name}`."
                    )
                } else {
                    format!("Remove the extra arguments from the `{inheritance_tag}` tag for `{class_name}`.")
                },
            ),
        };
        let issue = Issue::error(message)
            .with_annotation(Annotation::primary(primary_annotation_span).with_message(label))
            .with_annotation(
                Annotation::secondary(class_name_span)
                    .with_message(format!("Declaration of `{written_class_name}` is here")),
            )
            .with_annotation(Annotation::secondary(parent_definition_span).with_message(parent_label))
            .with_help(help);

        context.collector.report_with_code(code, issue);
    }

    let own_template_parameters_len = class_like_metadata.template_types.len();
    if parent_metadata.flags.has_consistent_templates() && own_template_parameters_len != expected_parameters_count {
        context.collector.report_with_code(
            IssueCode::InconsistentTemplate,
            Issue::error(format!(
                "Template parameter count mismatch: `{class_name}` must have {expected_parameters_count} template parameters to match `{parent_name}`."
            ))
            .with_annotation(Annotation::primary(class_name_span).with_message(format!("This {class_kind_str} defines {own_template_parameters_len} template parameters...")))
            .with_annotation(Annotation::secondary(parent_definition_span).with_message(format!("...but parent `{parent_name}` is marked `@consistent-templates` and expects {expected_parameters_count}.")))
            .with_help("Ensure the number of template parameters on this {class_kind_str} matches its parent."),
        );
    }

    if expected_parameters_count > 0
        && let Some(extended_parameters) = class_like_metadata.template_extended_parameters.get(&parent_metadata.name)
    {
        let mut i = 0;
        let mut previous_extended_types: IndexMap<Word, Vec<GenericTemplate>, RandomState> = IndexMap::default();

        for (template_name, _) in &parent_metadata.template_types {
            if let Some(extended_type) = extended_parameters.get(template_name) {
                previous_extended_types
                    .entry(*template_name)
                    .or_default()
                    .push(GenericTemplate::new(GenericParent::ClassLike(parent_metadata.name), extended_type.clone()));
            }
        }

        for (template_name, parent_template) in &parent_metadata.template_types {
            let Some(mut extended_type) = extended_parameters.get(template_name).cloned() else {
                i += 1;
                continue;
            };

            let mut template_type = parent_template.constraint.clone();

            expand_union(
                context.codebase,
                &mut extended_type,
                &TypeExpansionOptions { self_class: Some(class_like_metadata.original_name), ..Default::default() },
            );

            expand_union(
                context.codebase,
                &mut template_type,
                &TypeExpansionOptions { self_class: Some(class_like_metadata.original_name), ..Default::default() },
            );

            let extended_type_str = display_type(context, &extended_type);

            // `check_sharp_template_variance` checks a PHP# header's type arguments against spec section 11.1.
            if !class_like_metadata.flags.is_sharp()
                && parent_metadata
                    .template_variance
                    .get(i)
                    .is_some_and(mago_codex::ttype::template::variance::Variance::is_invariant)
            {
                for extended_type_atomic in extended_type.types.as_ref() {
                    let TAtomic::GenericParameter(generic_parameter) = extended_type_atomic else {
                        continue;
                    };

                    let Some(local_offset) = class_like_metadata
                        .template_types
                        .iter()
                        .position(|(name, _)| *name == generic_parameter.parameter_name)
                    else {
                        continue;
                    };

                    if class_like_metadata
                        .template_variance
                        .get(local_offset)
                        .is_some_and(mago_codex::ttype::template::variance::Variance::is_covariant)
                    {
                        let child_template_name = generic_parameter.parameter_name;

                        context.collector.report_with_code(
                            IssueCode::InvalidTemplateParameter,
                            Issue::error("Invalid template variance: cannot use a covariant template to satisfy an invariant one.")
                                .with_annotation(Annotation::primary(class_name_span).with_message(format!("In the definition of `{class_name}`")))
                                .with_note(format!("The parent `{parent_name}` defines template `{template_name}` as invariant (`@template`)."))
                                .with_note(format!("But it is being satisfied by the covariant template `{child_template_name}` (`@template-covariant`) from `{class_name}`."))
                                .with_help("Make the child template parameter invariant as well (`@template`), or change the parent's variance if appropriate."),
                        );
                    }
                }
            }

            if parent_metadata.flags.has_consistent_templates() {
                for extended_type_atomic in extended_type.types.as_ref() {
                    let extended_as_template = extended_type_atomic.get_generic_parameter_name();
                    if extended_as_template.is_none() {
                        context.collector.report_with_code(
                            IssueCode::InvalidTemplateParameter,
                            Issue::error("Inconsistent template: expected a template parameter, but found a concrete type.")
                                .with_annotation(Annotation::primary(parent_definition_span).with_message(format!(
                                    "Expected a template parameter, but got `{extended_type_str}`",
                                )))
                                .with_note(format!("Because `{parent_name}` is marked `@consistent-templates`, its template parameters must be extended with other template parameters, not concrete types."))
                                .with_help(format!("Change this to a template parameter defined on `{class_name}`.")),
                        );
                    } else if let Some(child_template_name) = extended_as_template
                        && let Some(child_template) = class_like_metadata.get_template_type(child_template_name)
                        && let child_template_type = &child_template.constraint
                        && child_template_type.get_id() != template_type.get_id()
                    {
                        let child_constraint = display_type(context, child_template_type);
                        let parent_constraint = display_type(context, &template_type);
                        context.collector.report_with_code(
                            IssueCode::InvalidTemplateParameter,
                            Issue::error("Inconsistent template: template parameter constraints do not match.")
                                .with_annotation(Annotation::primary(class_name_span).with_message(format!("This template parameter has constraint `{child_constraint}`...")))
                                .with_annotation(Annotation::secondary(parent_definition_span).with_message(format!("...but parent `{parent_name}` requires a constraint of `{parent_constraint}` for this template.")))
                                .with_note(format!("Because `{parent_name}` is marked `@consistent-templates`, the constraints of its template parameters must be identical in child classes."))
                                .with_help("Adjust the constraint on the child template parameter to match the parent's."),
                        );
                    }
                }
            }

            if template_type.is_mixed() {
                previous_extended_types
                    .entry(*template_name)
                    .or_default()
                    .push(GenericTemplate::new(GenericParent::ClassLike(parent_metadata.name), extended_type));
            } else {
                let mut template_result = TemplateResult::new(previous_extended_types.clone(), HashMap::default());
                let mut replaced_template_type = definition_type_replacer::replace(
                    &template_type,
                    &mut template_result,
                    context.codebase,
                    DefinitionReplacementOptions::default(),
                );

                expand_union(
                    context.codebase,
                    &mut replaced_template_type,
                    &TypeExpansionOptions { self_class: Some(class_like_metadata.original_name), ..Default::default() },
                );

                if is_type_compatible(context.codebase, &extended_type, &replaced_template_type) {
                    previous_extended_types
                        .entry(*template_name)
                        .or_default()
                        .push(GenericTemplate::new(GenericParent::ClassLike(parent_metadata.name), extended_type));
                } else {
                    let replaced_type_str = display_type(context, &replaced_template_type);
                    let (message, provided, bound, help) = match &sharp_parent {
                        Some(parent) => (
                            format!("Type argument for `{parent}` is not compatible with its bound."),
                            format!("The type `{extended_type_str}` provided for type parameter `{template_name}`..."),
                            format!("...does not satisfy the bound `{replaced_type_str}` from `{parent}`."),
                            format!("Supply a type contained by `{replaced_type_str}`."),
                        ),
                        None => (
                            format!("Template argument for `{parent_name}` is not compatible with its constraint."),
                            format!("The type `{extended_type_str}` provided for template `{template_name}`..."),
                            format!(
                                "...does not satisfy the required constraint of `{replaced_type_str}` from `{parent_name}`."
                            ),
                            "Change the provided type to be compatible with the template constraint.".to_owned(),
                        ),
                    };

                    context.collector.report_with_code(
                        IssueCode::InvalidTemplateParameter,
                        Issue::error(message)
                            .with_annotation(
                                Annotation::primary(class_name_span)
                                    .with_message(format!("In the definition of `{written_class_name}`")),
                            )
                            .with_note(provided)
                            .with_note(bound)
                            .with_help(help),
                    );
                }
            }

            i += 1;
        }
    }
}

/// Checks if this is the same method that was inherited (not overridden).
/// Example: `StringBox` extends Box and inherits `Box::setValue` without overriding it.
#[inline]
fn should_skip_same_method(appearing_fqcn: &[u8], overridden_fqcn: &[u8]) -> bool {
    appearing_fqcn.eq_ignore_ascii_case(overridden_fqcn)
}

/// Checks if this is a trait-to-trait abstract method conflict that should be handled
/// by `check_trait_method_conflicts` instead of here.
///
/// We skip when BOTH methods are:
/// - From different traits
/// - Both are abstract
/// - Both traits are used by the current class
#[inline]
fn should_skip_trait_to_trait_conflict(
    appearing_class: &ClassLikeMetadata,
    appearing_method: &FunctionLikeMetadata,
    overridden_class: &ClassLikeMetadata,
    overridden_method: &FunctionLikeMetadata,
    class_like_metadata: &ClassLikeMetadata,
) -> bool {
    if !appearing_class.kind.is_trait() || !overridden_class.kind.is_trait() {
        return false;
    }

    let appearing_is_abstract = appearing_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);
    let overridden_is_abstract = overridden_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);

    if !appearing_is_abstract || !overridden_is_abstract {
        return false;
    }

    let appearing_lowercase = ascii_lowercase_word(appearing_class.name.as_bytes());
    let overridden_lowercase = ascii_lowercase_word(overridden_class.name.as_bytes());

    if !class_like_metadata.used_traits.contains(&appearing_lowercase)
        || !class_like_metadata.used_traits.contains(&overridden_lowercase)
    {
        return false;
    }

    appearing_lowercase != overridden_lowercase
}

/// Checks if this is an enum implementing BackedEnum/UnitEnum, which is allowed
/// to narrow the method signatures (e.g., `from(string)` instead of `from(int|string)`).
#[inline]
fn should_skip_enum_builtin_interface(class_like_metadata: &ClassLikeMetadata, interface_fqcn: &[u8]) -> bool {
    class_like_metadata.kind.is_enum() && (interface_fqcn == b"backedenum" || interface_fqcn == b"unitenum")
}

fn check_template_variance_positions<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
) where
    A: Arena,
{
    if class_like_metadata.template_variance.is_empty() {
        return;
    }

    let mut covariant_templates: Vec<Word> = Vec::new();
    let mut contravariant_templates: Vec<Word> = Vec::new();
    for (index, (template_name, _)) in class_like_metadata.template_types.iter().enumerate() {
        match class_like_metadata.template_variance.get(index) {
            Some(Variance::Covariant) => covariant_templates.push(*template_name),
            Some(Variance::Contravariant) => contravariant_templates.push(*template_name),
            _ => {}
        }
    }

    if covariant_templates.is_empty() && contravariant_templates.is_empty() {
        return;
    }

    let class_name = display_class_like_name(context, class_like_metadata.original_name);
    let own_entity = GenericParent::ClassLike(class_like_metadata.name);

    for method_name in &class_like_metadata.methods {
        let Some(method) = context.codebase.get_method(class_like_metadata.name.as_bytes(), method_name.as_bytes())
        else {
            continue;
        };

        if let Some(method_metadata) = &method.method_metadata
            && (method_metadata.is_constructor || matches!(method_metadata.visibility, Visibility::Private))
        {
            continue;
        }

        for parameter in &method.parameters {
            let Some(type_metadata) = &parameter.type_metadata else {
                continue;
            };

            for atomic in type_metadata.type_union.types.as_ref() {
                let TAtomic::GenericParameter(generic) = atomic else {
                    continue;
                };

                if generic.defining_entity != own_entity || !covariant_templates.contains(&generic.parameter_name) {
                    continue;
                }

                let template_name = generic.parameter_name;
                context.collector.report_with_code(
                    IssueCode::InvalidTemplateParameter,
                    Issue::error(format!(
                        "Covariant template parameter `{template_name}` cannot appear in a parameter position."
                    ))
                    .with_annotation(Annotation::primary(parameter.span).with_message(format!(
                        "`{template_name}` is declared covariant (`@template-covariant`) on `{class_name}`"
                    )))
                    .with_note("A method parameter is a contravariant position; a covariant template parameter may only appear in covariant positions, such as a return type.")
                    .with_help(format!(
                        "Declare `{template_name}` as invariant (`@template`), or remove it from parameter positions."
                    )),
                );
            }
        }

        if let Some(return_type) = &method.return_type_metadata {
            for atomic in return_type.type_union.types.as_ref() {
                let TAtomic::GenericParameter(generic) = atomic else {
                    continue;
                };

                if generic.defining_entity != own_entity || !contravariant_templates.contains(&generic.parameter_name) {
                    continue;
                }

                let template_name = generic.parameter_name;
                context.collector.report_with_code(
                    IssueCode::InvalidTemplateParameter,
                    Issue::error(format!(
                        "Contravariant template parameter `{template_name}` cannot appear in a return position."
                    ))
                    .with_annotation(Annotation::primary(return_type.span).with_message(format!(
                        "`{template_name}` is declared contravariant (`@template-contravariant`) on `{class_name}`"
                    )))
                    .with_note("A method return type is a covariant position; a contravariant template parameter may only appear in contravariant positions, such as a method parameter.")
                    .with_help(format!(
                        "Declare `{template_name}` as invariant (`@template`), or remove it from return positions."
                    )),
                );
            }
        }
    }
}

/// Checks spec section 11.1 on every member of a PHP# class or interface another class reaches, and on the generic
/// types of its header: an `out` type parameter is only handed out, and an `in` type parameter is only taken in.
fn check_sharp_template_variance<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    inheritance: Option<&Inheritance<'_>>,
) where
    A: Arena,
{
    if !class_like_metadata.template_variance.iter().any(|variance| !variance.is_invariant()) {
        return;
    }

    let header_spans: WordMap<Span> = inheritance
        .iter()
        .flat_map(|inheritance| inheritance.types.iter())
        .filter_map(|hint| match hint {
            Hint::Generic(generic) => {
                Some((ascii_lowercase_word(context.resolved_names.resolve(&generic.name)?), generic.span()))
            }
            _ => None,
        })
        .collect();
    for template_use in find_template_uses(context, class_like_metadata, &header_spans) {
        let Some(marker) = find_broken_marker(class_like_metadata, template_use.template, template_use.position) else {
            continue;
        };
        let refused = if marker == "out" { "take it in" } else { "hand it out" };

        let template_name = template_use.template;
        context.collector.report_with_code(
            IssueCode::InvalidTemplateParameter,
            Issue::error(format!(
                "`{template_name}` is declared `{marker}`, so {} cannot {refused}.",
                template_use.member
            ))
            .with_annotation(
                Annotation::primary(template_use.span)
                    .with_message(format!("{} uses `{template_name}` here", template_use.member)),
            )
            .with_note("An `out` type parameter is only handed out, and an `in` type parameter is only taken in."),
        );
    }
}

/// Refuses a generic PHP# class that is a `Serializable`, through its header or a parent's. Its objects carry their
/// type arguments through serialization, and `Serializable::serialize` writes a string the engine cannot add them to.
fn check_sharp_serializable<A>(context: &mut Context<'_, '_, A>, class_like_metadata: &ClassLikeMetadata)
where
    A: Arena,
{
    if !class_like_metadata.kind.is_class()
        || class_like_metadata.template_types.is_empty()
        || !context.codebase.class_implements(class_like_metadata.name.as_bytes(), b"Serializable")
    {
        return;
    }

    let class_name = short_name(class_like_metadata.original_name);
    context.collector.report_with_code(
        IssueCode::InvalidImplement,
        Issue::error(format!(
            "`{class_name}` has type parameters, so it cannot be a `Serializable`: `serialize` drops the type arguments its objects carry."
        ))
        .with_annotation(
            Annotation::primary(class_like_metadata.name_span.unwrap_or(class_like_metadata.span))
                .with_message(format!("`{class_name}` has type parameters")),
        )
        .with_help("Leave `Serializable` out: `serialize()` then uses `__serialize` and `__unserialize`, or its default form, which keep them."),
    );
}

/// Refuses a PHP# class-like that reaches one generic class-like with other type arguments through two entries of its
/// header, as `Strings : Ints, Feeder<string>` does when `Ints` is a `Feeder<int>`. It implements it once, as a header
/// that names it twice is refused, so the refusal names each way it is reached. Each method of the class-like reached
/// twice is marked checked, since its return type merges to `never` and reports nothing true.
fn check_sharp_reached_twice<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    inheritance: Option<&Inheritance<'_>>,
    checked_signatures: &mut HashSet<(Word, Word)>,
) where
    A: Arena,
{
    let header: Vec<&ClassLikeMetadata> = inheritance
        .iter()
        .flat_map(|inheritance| inheritance.names())
        .filter_map(|name| context.codebase.get_class_like(context.resolved_names.resolve(&name)?))
        .collect();
    let class_name = display_class_like_name(context, class_like_metadata.original_name);

    let ancestors = class_like_metadata
        .template_extended_parameter_paths
        .iter()
        .filter(|(_, paths)| paths.len() > 1)
        .filter_map(|(ancestor_name, _)| context.codebase.get_class_like(ancestor_name.as_bytes()));
    for ancestor in ancestors {
        let ancestor_display = display_class_like_name(context, ancestor.original_name);
        // Each type the header reaches the ancestor as, with the header entry it is reached through, or `None` when the
        // header names the ancestor itself.
        let mut reached: Vec<(String, Option<&ClassLikeMetadata>)> = Vec::new();
        for parent in &header {
            if parent.name != ancestor.name
                && !context.codebase.is_instance_of(parent.name.as_bytes(), ancestor.name.as_bytes())
            {
                continue;
            }

            let type_arguments = class_like_metadata.template_extended_offsets.get(&parent.name).map(Vec::as_slice);
            let arguments: Vec<String> = ancestor
                .template_types
                .iter()
                .filter_map(|(template_name, _)| {
                    get_specialized_template_type(
                        context.codebase,
                        *template_name,
                        ancestor.name,
                        parent,
                        type_arguments,
                    )
                })
                .map(|argument| display_type(context, &argument))
                .collect();
            let reached_type = format!("`{ancestor_display}<{}>`", arguments.join(", "));
            if !reached.iter().any(|(known, _)| *known == reached_type) {
                reached.push((reached_type, (parent.name != ancestor.name).then_some(*parent)));
            }
        }

        let ways: Vec<String> = reached
            .into_iter()
            .map(|(reached_type, through)| match through {
                Some(parent) => {
                    format!("{reached_type} through `{}`", display_class_like_name(context, parent.original_name))
                }
                None => reached_type,
            })
            .collect();
        let Some((last, rest)) = ways.split_last().filter(|(_, rest)| !rest.is_empty()) else {
            continue;
        };

        let times = if rest.len() == 1 { "twice".to_string() } else { format!("{} times", ways.len()) };
        context.collector.report_with_code(
            IssueCode::InvalidImplement,
            Issue::error(format!(
                "`{class_name}` implements `{ancestor_display}` {times}: {}, and {last}.",
                rest.join(", ")
            ))
            .with_annotation(
                Annotation::primary(class_like_metadata.name_span.unwrap_or(class_like_metadata.span))
                    .with_message(format!("`{class_name}` reaches `{ancestor_display}` with other type arguments")),
            )
            .with_help(format!("Implement `{ancestor_display}` once.")),
        );
        checked_signatures.extend(ancestor.declaring_method_ids.iter().map(|(name, id)| (id.get_class_name(), *name)));
    }
}

/// Rejects an uninhabitable diamond at the declaration site: a class-like
/// reaching one generic ancestor through two conflicting parameterizations
/// inherits a method whose return positions merge (by intersection) to an
/// uninhabited type, so no class could ever implement it. A class that declares
/// the method itself is left to the per-path override checks.
fn check_uninhabitable_diamonds<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
) where
    A: Arena,
{
    if class_like_metadata.template_extended_parameter_paths.is_empty() {
        return;
    }

    let class_name = display_class_like_name(context, class_like_metadata.original_name);
    let class_span = class_like_metadata.name_span.unwrap_or(class_like_metadata.span);

    let mut findings: Vec<(Word, Word, usize)> = Vec::new();
    for (ancestor_name, paths) in &class_like_metadata.template_extended_parameter_paths {
        if paths.len() < 2 {
            continue;
        }

        let Some(ancestor_metadata) = context.codebase.get_class_like(ancestor_name.as_bytes()) else {
            continue;
        };
        let ancestor_display = display_class_like_name(context, ancestor_metadata.original_name);

        for method_name in &ancestor_metadata.methods {
            let method_name = *method_name;

            if let Some(declaring_id) = class_like_metadata.declaring_method_ids.get(&method_name)
                && declaring_id.get_class_name() == class_like_metadata.name
            {
                continue;
            }

            let Some(method) = context.codebase.get_method(ancestor_name.as_bytes(), method_name.as_bytes()) else {
                continue;
            };

            let Some(return_type) = &method.return_type_metadata else {
                continue;
            };
            if return_type.type_union.is_never() {
                continue;
            }

            let merged = get_substituted_method(method, class_like_metadata, *ancestor_name, context.codebase);
            if merged.return_type_metadata.as_ref().is_some_and(|r| r.type_union.is_never()) {
                // PHP keeps the lowercase name Mago looks the method up by, and PHP# names it as it is declared.
                let method_name = if context.dialect.is_sharp() { method.original_name } else { method_name };

                findings.push((ancestor_display, method_name, paths.len()));
            }
        }
    }

    for (ancestor_display, method_name, path_count) in findings {
        let method = if context.dialect.is_sharp() {
            display_sharp_member(ancestor_display, method_name)
        } else {
            format!("{ancestor_display}::{method_name}()")
        };
        context.collector.report_with_code(
            IssueCode::InvalidTemplateParameter,
            Issue::error(format!("Diamond inheritance of `{method}` requires an uninhabited return type."))
            .with_annotation(Annotation::primary(class_span).with_message(format!(
                "`{class_name}` reaches `{ancestor_display}` through {path_count} conflicting parameterizations"
            )))
            .with_note("A method return is a covariant position, so a diamond merges it by intersection.")
            .with_note(format!(
                "The parameterizations of `{ancestor_display}` give `{method_name}()` disjoint return types whose intersection is uninhabited, so no class could implement `{class_name}`."
            ))
            .with_help(
                "Constrain the template parameter with an object bound, since object intersections stay inhabitable, or remove one of the conflicting parameterizations.",
            ),
        );
    }
}

/// Spec section 19: a subclass inherits its parent's operators, which are static, and PHP# has no overloading by
/// parameter types, so a class cannot declare an operator a parent declares. Each refusal marks the parent's method
/// checked, so `check_abstract_method_signatures` does not report the same declaration as a final-method override.
fn check_inherited_operators<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
    checked_signatures: &mut HashSet<(Word, Word)>,
) where
    A: Arena,
{
    let Some(parent) =
        class_like_metadata.direct_parent_class.and_then(|parent| context.codebase.get_class_like(parent.as_bytes()))
    else {
        return;
    };

    for member in members {
        let ClassLikeMember::Operator(operator) = member else {
            continue;
        };
        let Some(method) = MethodParts::of_operator(operator) else {
            continue;
        };
        let name = ascii_lowercase_word(method.name);
        let Some(declaring_method_id) = parent.declaring_method_ids.get(&name) else {
            continue;
        };
        let Some(declaring_class) = context.codebase.get_class_like(declaring_method_id.get_class_name().as_bytes())
        else {
            continue;
        };
        checked_signatures.insert((declaring_method_id.get_class_name(), name));

        let unary = if method.name == b"op_UnaryNegation" { "unary " } else { "" };
        let class = display_class_like_name(context, class_like_metadata.name);
        let parent = display_class_like_name(context, declaring_class.name);
        context.collector.report_with_code(
            IssueCode::OverrideFinalMethod,
            Issue::error(format!(
                "`{class}` cannot declare {unary}`operator {}`: it inherits it from `{parent}`.",
                String::from_utf8_lossy(operator.symbol.as_bytes()),
            ))
            .with_annotation(Annotation::primary(operator.operator.span).with_message("Declared here.")),
        );
    }
}

fn check_abstract_method_signatures<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    class_like_metadata: &'ctx ClassLikeMetadata,
    checked_signatures: &mut HashSet<(Word, Word)>,
) where
    A: Arena,
{
    for (method_name_atom, overridden_method_ids) in &class_like_metadata.overridden_method_ids {
        let method_name_str = method_name_atom.as_ref();

        let Some(declaring_method_id) = class_like_metadata.declaring_method_ids.get(method_name_atom) else {
            continue;
        };

        let declaring_fqcn = declaring_method_id.get_class_name();
        let declaring_method_opt = context.codebase.get_method(declaring_fqcn.as_bytes(), method_name_str);

        let (method_fqcn, appearing_method) = if let Some(method) = declaring_method_opt {
            (declaring_fqcn, method)
        } else if let Some(appearing_method_id) = class_like_metadata.appearing_method_ids.get(method_name_atom) {
            let appearing_fqcn = appearing_method_id.get_class_name();
            let Some(method) = context.codebase.get_method(appearing_fqcn.as_bytes(), method_name_str) else {
                continue;
            };

            (appearing_fqcn, method)
        } else {
            continue;
        };

        let overridden_declaring_classes: Vec<Word> =
            overridden_method_ids.values().map(|id| id.get_class_name()).collect();

        for (parent_fqcn, parent_declaring_method_id) in overridden_method_ids {
            let parent_fqcn_str = parent_fqcn.as_ref();

            let declaring_class_name = parent_declaring_method_id.get_class_name();
            let declaring_class_name_str = declaring_class_name.as_ref();

            if should_skip_same_method(method_fqcn.as_bytes(), parent_fqcn_str) {
                continue;
            }

            if overridden_declaring_classes.iter().any(|other| {
                *other != declaring_class_name
                    && context.codebase.is_instance_of(other.as_bytes(), declaring_class_name_str)
            }) {
                continue;
            }

            let declaring_class_name = required_constructor_class(context, class_like_metadata, *method_name_atom)
                .unwrap_or(declaring_class_name);
            let declaring_class_name_str = declaring_class_name.as_bytes();
            if !checked_signatures.insert((declaring_class_name, *method_name_atom)) {
                continue;
            }

            let Some(overridden_method) =
                context.codebase.get_declaring_method(declaring_class_name_str, method_name_str)
            else {
                continue;
            };

            let Some(overridden_class) = context.codebase.get_class_like(declaring_class_name_str) else {
                continue;
            };

            let Some(appearing_class) = context.codebase.get_class_like(method_fqcn.as_bytes()) else {
                continue;
            };

            if should_skip_trait_to_trait_conflict(
                appearing_class,
                appearing_method,
                overridden_class,
                overridden_method,
                class_like_metadata,
            ) {
                continue;
            }

            if should_skip_enum_builtin_interface(class_like_metadata, declaring_class_name_str) {
                continue;
            }

            let error_span = if appearing_class.kind.is_trait() {
                class_like_metadata.name_span.unwrap_or(class_like_metadata.span)
            } else {
                appearing_method.name_span.unwrap_or(appearing_method.span)
            };

            if report_named_constructor_clash(
                context,
                class_like_metadata,
                appearing_method,
                overridden_class,
                overridden_method,
                error_span,
            ) {
                continue;
            }

            let substituted_overridden_method =
                get_substituted_method(overridden_method, class_like_metadata, declaring_class_name, context.codebase);

            let substituted_appearing_method =
                get_substituted_method(appearing_method, class_like_metadata, method_fqcn, context.codebase);

            let mut issues = method_signature::validate_method_signature_compatibility(
                context.codebase,
                class_like_metadata.name,
                &substituted_appearing_method,
                &substituted_overridden_method,
                context.dialect,
            );
            method_signature::validate_erased_signature_compatibility(
                context.codebase,
                class_like_metadata,
                overridden_class,
                appearing_method,
                overridden_method,
                &mut issues,
            );

            for incompatibility in issues {
                if widens_parameter(artifacts, class_like_metadata, method_fqcn, appearing_method, &incompatibility) {
                    continue;
                }
                report_signature_compatibility_issue(
                    context,
                    class_like_metadata,
                    overridden_class,
                    *method_name_atom,
                    appearing_method,
                    incompatibility,
                    error_span,
                );
            }
        }
    }
}

fn check_trait_method_conflicts<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    trait_uses: &[(Span, Vec<&Identifier<'arena>>)],
) where
    A: Arena,
{
    let trait_uses = resolve_trait_uses(context, trait_uses);

    for i in 0..trait_uses.len() {
        let (first_trait_use_span, first_traits) = &trait_uses[i];

        for k in 0..first_traits.len() {
            for l in (k + 1)..first_traits.len() {
                let first_trait_fqcn = &first_traits[k];
                let second_trait_fqcn = &first_traits[l];

                let Some(first_trait_metadata) = context.codebase.get_class_like(first_trait_fqcn.as_ref()) else {
                    continue;
                };
                let Some(second_trait_metadata) = context.codebase.get_class_like(second_trait_fqcn.as_ref()) else {
                    continue;
                };

                for (method_name, first_method_id) in &first_trait_metadata.declaring_method_ids {
                    if let Some(second_method_id) = second_trait_metadata.declaring_method_ids.get(method_name) {
                        let first_method_str = method_name.as_ref();
                        let Some(first_method) = context
                            .codebase
                            .get_declaring_method(first_method_id.get_class_name().as_ref(), first_method_str)
                        else {
                            continue;
                        };

                        let Some(second_method) = context
                            .codebase
                            .get_declaring_method(second_method_id.get_class_name().as_ref(), first_method_str)
                        else {
                            continue;
                        };

                        let first_is_abstract = first_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);
                        let second_is_abstract = second_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);

                        if first_is_abstract || second_is_abstract {
                            let issues = method_signature::validate_method_signature_compatibility(
                                context.codebase,
                                class_like_metadata.name,
                                second_method,
                                first_method,
                                context.dialect,
                            );

                            for incompatibility in issues {
                                report_signature_compatibility_issue(
                                    context,
                                    class_like_metadata,
                                    first_trait_metadata,
                                    *method_name,
                                    second_method,
                                    incompatibility,
                                    *first_trait_use_span,
                                );
                            }
                        }
                    }
                }
            }
        }

        for (second_trait_use_span, second_traits) in trait_uses.iter().skip(i + 1) {
            for first_trait_fqcn in first_traits {
                let Some(first_trait_metadata) = context.codebase.get_class_like(first_trait_fqcn.as_ref()) else {
                    continue;
                };

                for second_trait_fqcn in second_traits {
                    let Some(second_trait_metadata) = context.codebase.get_class_like(second_trait_fqcn.as_ref())
                    else {
                        continue;
                    };

                    for (method_name, first_method_id) in &first_trait_metadata.declaring_method_ids {
                        if let Some(second_method_id) = second_trait_metadata.declaring_method_ids.get(method_name) {
                            let first_method_str = method_name.as_ref();
                            let Some(first_method) = context
                                .codebase
                                .get_declaring_method(first_method_id.get_class_name().as_ref(), first_method_str)
                            else {
                                continue;
                            };
                            let Some(second_method) = context
                                .codebase
                                .get_declaring_method(second_method_id.get_class_name().as_ref(), first_method_str)
                            else {
                                continue;
                            };

                            let first_is_abstract =
                                first_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);
                            let second_is_abstract =
                                second_method.method_metadata.as_ref().is_some_and(|m| m.is_abstract);

                            if first_is_abstract || second_is_abstract {
                                let issues = method_signature::validate_method_signature_compatibility(
                                    context.codebase,
                                    class_like_metadata.name,
                                    second_method,
                                    first_method,
                                    context.dialect,
                                );

                                for incompatibility in issues {
                                    report_signature_compatibility_issue(
                                        context,
                                        class_like_metadata,
                                        first_trait_metadata,
                                        *method_name,
                                        second_method,
                                        incompatibility,
                                        *second_trait_use_span,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn check_trait_property_conflicts<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    trait_uses: &[(Span, Vec<&Identifier<'arena>>)],
    members: &[ClassLikeMember<'arena>],
) where
    A: Arena,
{
    let trait_uses = resolve_trait_uses(context, trait_uses);

    let mut class_properties: IndexMap<Word, &PropertyMetadata> = IndexMap::new();
    for (property_name, property_metadata) in class_like_metadata.properties.iter().sorted_by_key(|(k, _)| *k) {
        if let Some(declaring_class) = class_like_metadata.declaring_property_ids.get(property_name)
            && declaring_class == &class_like_metadata.name
        {
            class_properties.insert(*property_name, property_metadata);
        }
    }

    for i in 0..trait_uses.len() {
        let (first_trait_use_span, first_traits) = &trait_uses[i];

        for k in 0..first_traits.len() {
            for l in (k + 1)..first_traits.len() {
                let first_trait_fqcn = &first_traits[k];
                let second_trait_fqcn = &first_traits[l];

                let Some(first_trait_metadata) = context.codebase.get_class_like(first_trait_fqcn.as_ref()) else {
                    continue;
                };
                let Some(second_trait_metadata) = context.codebase.get_class_like(second_trait_fqcn.as_ref()) else {
                    continue;
                };

                for (property_name, first_property) in first_trait_metadata.properties.iter().sorted_by_key(|(k, _)| *k)
                {
                    let Some(second_property) = second_trait_metadata.properties.get(property_name) else {
                        continue;
                    };

                    let Err(conflict) = check_property_compatibility(first_property, second_property) else {
                        continue;
                    };

                    report_trait_property_conflict(
                        context,
                        class_like_metadata.name,
                        *property_name,
                        *first_trait_fqcn,
                        *second_trait_fqcn,
                        *first_trait_use_span,
                        conflict,
                    );
                }
            }
        }

        for (second_trait_use_span, second_traits) in trait_uses.iter().skip(i + 1) {
            for first_trait_fqcn in first_traits {
                let Some(first_trait_metadata) = context.codebase.get_class_like(first_trait_fqcn.as_ref()) else {
                    continue;
                };

                for second_trait_fqcn in second_traits {
                    let Some(second_trait_metadata) = context.codebase.get_class_like(second_trait_fqcn.as_ref())
                    else {
                        continue;
                    };

                    for (property_name, first_property) in
                        first_trait_metadata.properties.iter().sorted_by_key(|(k, _)| *k)
                    {
                        let Some(second_property) = second_trait_metadata.properties.get(property_name) else {
                            continue;
                        };

                        let Err(conflict) = check_property_compatibility(first_property, second_property) else {
                            continue;
                        };

                        report_trait_property_conflict(
                            context,
                            class_like_metadata.name,
                            *property_name,
                            *first_trait_fqcn,
                            *second_trait_fqcn,
                            *second_trait_use_span,
                            conflict,
                        );
                    }
                }
            }
        }

        for first_trait_fqcn in first_traits {
            let Some(first_trait_metadata) = context.codebase.get_class_like(first_trait_fqcn.as_ref()) else {
                continue;
            };

            for (property_name, trait_property) in first_trait_metadata.properties.iter().sorted_by_key(|(k, _)| *k) {
                let Some(class_property) = class_properties.get(property_name) else {
                    continue;
                };

                let Err(conflict) = check_property_compatibility(trait_property, class_property) else {
                    continue;
                };

                let conflict_span = members
                    .iter()
                    .find_map(|member| {
                        if let ClassLikeMember::Property(prop) = member {
                            match prop {
                                // A PHP# property's name has no `$`, which PHP's name has.
                                Property::Plain(plain_prop) => {
                                    for item in &plain_prop.items {
                                        let var_name = php_variable_name(item.variable().name);
                                        if var_name == *property_name {
                                            return Some(prop.span());
                                        }
                                    }
                                }
                                Property::Hooked(_) | Property::Computed(_) => {
                                    let var_name = php_variable_name(prop.first_variable().name);
                                    if var_name == *property_name {
                                        return Some(prop.span());
                                    }
                                }
                            }
                        }
                        None
                    })
                    .unwrap_or(*first_trait_use_span);

                report_trait_property_conflict(
                    context,
                    class_like_metadata.name,
                    *property_name,
                    *first_trait_fqcn,
                    class_like_metadata.name,
                    conflict_span,
                    conflict,
                );
            }
        }
    }
}

/// Check if two properties are compatible, returns Err with specific conflict type if not
fn check_property_compatibility(prop1: &PropertyMetadata, prop2: &PropertyMetadata) -> Result<(), PropertyConflict> {
    // PHP 8.4: Conflict resolution between hooked properties is not supported
    if !prop1.hooks.is_empty() || !prop2.hooks.is_empty() {
        return Err(PropertyConflict::HookedProperty);
    }

    if prop1.read_visibility != prop2.read_visibility || prop1.write_visibility != prop2.write_visibility {
        return Err(PropertyConflict::Visibility(
            prop1.read_visibility,
            prop1.write_visibility,
            prop2.read_visibility,
            prop2.write_visibility,
        ));
    }

    if prop1.flags.is_static() != prop2.flags.is_static() {
        return Err(PropertyConflict::Static(prop1.flags.is_static(), prop2.flags.is_static()));
    }

    if prop1.flags.is_readonly() != prop2.flags.is_readonly() {
        return Err(PropertyConflict::Readonly(prop1.flags.is_readonly(), prop2.flags.is_readonly()));
    }

    match (&prop1.type_declaration_metadata, &prop2.type_declaration_metadata) {
        (Some(t1), Some(t2)) => {
            let t1_id = t1.type_union.get_id();
            let t2_id = t2.type_union.get_id();
            if t1_id != t2_id {
                return Err(PropertyConflict::Type(Some(t1_id), Some(t2_id)));
            }
        }
        (Some(t1), None) => {
            return Err(PropertyConflict::Type(Some(t1.type_union.get_id()), None));
        }
        (None, Some(t2)) => {
            return Err(PropertyConflict::Type(None, Some(t2.type_union.get_id())));
        }
        (None, None) => {}
    }

    match (&prop1.default_type_metadata, &prop2.default_type_metadata) {
        (Some(d1), Some(d2)) => {
            let d1_id = d1.type_union.get_id();
            let d2_id = d2.type_union.get_id();
            if d1_id != d2_id {
                return Err(PropertyConflict::Default(Some(d1_id), Some(d2_id)));
            }
        }
        (Some(d1), None) => {
            return Err(PropertyConflict::Default(Some(d1.type_union.get_id()), None));
        }
        (None, Some(d2)) => {
            return Err(PropertyConflict::Default(None, Some(d2.type_union.get_id())));
        }
        (None, None) => {}
    }

    Ok(())
}

fn report_trait_property_conflict<A>(
    context: &mut Context<'_, '_, A>,
    class_name: Word,
    property_name: Word,
    trait1_name: Word,
    trait2_name: Word,
    conflict_span: Span,
    conflict: PropertyConflict,
) where
    A: Arena,
{
    let conflict_description = conflict.describe();
    let issue_code = conflict.get_issue_code();

    // A PHP# class's header traits, spec section 22, are named as PHP# writes classes, and a member has no `$`.
    if context.dialect.is_sharp() {
        let display = |name: Word| {
            let original = context.codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name);
            display_class_like_name(context, original)
        };
        let (class, first, second) = (display(class_name), display(trait1_name), display(trait2_name));
        let property = mago_bytes::BytesDisplay(mago_bytes::trim_start_byte(property_name.as_bytes(), b'$'));

        context.collector.report_with_code(
            issue_code,
            Issue::error(format!(
                "`{class}` gets `{property}` from both `{first}` and `{second}`, which declare it differently: {conflict_description}."
            ))
            .with_annotation(Annotation::primary(conflict_span).with_message("Declared differently.")),
        );

        return;
    }

    context.collector.report_with_code(
        issue_code,
        Issue::error(format!(
            "Property `{property_name}` is defined differently in `{trait1_name}` and `{trait2_name}` used by `{class_name}`: {conflict_description}"
        ))
        .with_annotation(Annotation::primary(conflict_span).with_message("Conflicting property definitions"))
        .with_note(format!("In PHP, this will cause a fatal error: '{trait1_name} and {trait2_name} define the same property ({property_name}) in the composition of {class_name}. However, the definition differs and is considered incompatible.'"))
        .with_help("Ensure both sources define the property identically (same visibility, type, default value, and modifiers), or use only one source."),
    );
}

/// Apply template parameter substitution to a method's parameter and return types
///
/// For example, if interface has `K` and `V` template parameters, and the implementation
/// maps them to `TKey` and `TValue`, this function replaces all occurrences of `K` with `TKey`
/// and `V` with `TValue` in the method signature.
///
/// Gets the substituted method by applying template parameter mapping from the class.
/// Returns the original method if no template substitution is needed.
#[inline]
fn get_substituted_method(
    method: &FunctionLikeMetadata,
    class_like_metadata: &ClassLikeMetadata,
    parent_class_name: Word,
    codebase: &CodebaseMetadata,
) -> FunctionLikeMetadata {
    let parameterizations: Vec<&IndexMap<Word, TUnion, RandomState>> =
        match class_like_metadata.template_extended_parameter_paths.get(&parent_class_name) {
            Some(paths) if !paths.is_empty() => paths.iter().collect(),
            _ => match class_like_metadata.template_extended_parameters.get(&parent_class_name) {
                Some(mapping) if !mapping.is_empty() => vec![mapping],
                _ => return method.clone(),
            },
        };

    let mut substituted = parameterizations.into_iter().map(|mapping| {
        let mut template_result = TemplateResult::default();
        for (template_name, concrete_type) in mapping {
            template_result.add_lower_bound(
                *template_name,
                GenericParent::ClassLike(parent_class_name),
                concrete_type.clone(),
            );
        }

        apply_template_substitution_to_method(method, &template_result, codebase)
    });

    let Some(mut merged) = substituted.next() else {
        return method.clone();
    };

    for other in substituted {
        merge_method_signature_into(&mut merged, &other, codebase);
    }

    merged
}

/// Folds one parameterization of an inherited method into the running merge:
/// parameter positions widen to the union of all paths, the return position
/// narrows to the intersection. This synthesises the diamond-merged contract an
/// implementer must satisfy.
fn merge_method_signature_into(
    merged: &mut FunctionLikeMetadata,
    other: &FunctionLikeMetadata,
    codebase: &CodebaseMetadata,
) {
    for (index, parameter) in merged.parameters.iter_mut().enumerate() {
        let Some(other_parameter) = other.parameters.get(index) else {
            continue;
        };

        join_type_metadata(parameter.type_metadata.as_mut(), other_parameter.type_metadata.as_ref(), codebase);
        join_type_metadata(
            parameter.type_declaration_metadata.as_mut(),
            other_parameter.type_declaration_metadata.as_ref(),
            codebase,
        );
    }

    meet_type_metadata(merged.return_type_metadata.as_mut(), other.return_type_metadata.as_ref(), codebase);
    meet_type_metadata(
        merged.return_type_declaration_metadata.as_mut(),
        other.return_type_declaration_metadata.as_ref(),
        codebase,
    );
}

fn join_type_metadata(target: Option<&mut TypeMetadata>, other: Option<&TypeMetadata>, codebase: &CodebaseMetadata) {
    if let (Some(target), Some(other)) = (target, other) {
        target.type_union =
            combine_union_types(&target.type_union, &other.type_union, codebase, CombinerOptions::default());
    }
}

fn meet_type_metadata(target: Option<&mut TypeMetadata>, other: Option<&TypeMetadata>, codebase: &CodebaseMetadata) {
    if let (Some(target), Some(other)) = (target, other) {
        target.type_union =
            intersect_union_types(&target.type_union, &other.type_union, codebase).unwrap_or_else(get_never);
    }
}

fn apply_template_substitution_to_method(
    method: &FunctionLikeMetadata,
    template_result: &TemplateResult,
    codebase: &CodebaseMetadata,
) -> FunctionLikeMetadata {
    let mut substituted_method = method.clone();

    for template in substituted_method.template_types.values_mut() {
        template.constraint = inferred_type_replacer::replace(&template.constraint, template_result, codebase);
    }

    for param in &mut substituted_method.parameters {
        if let Some(type_metadata) = &mut param.type_metadata {
            type_metadata.type_union =
                inferred_type_replacer::replace(&type_metadata.type_union, template_result, codebase);
        }
    }

    if let Some(return_type) = &mut substituted_method.return_type_declaration_metadata {
        return_type.type_union = inferred_type_replacer::replace(&return_type.type_union, template_result, codebase);
    }

    if let Some(return_type) = &mut substituted_method.return_type_metadata {
        return_type.type_union = inferred_type_replacer::replace(&return_type.type_union, template_result, codebase);
    }

    substituted_method
}

/// Records the parameter `incompatibility` names in `artifacts` when the bridge widens it, and returns whether it did:
/// a parameter of a PHP# method that `class_like_metadata` declares itself, at `method_class`, which erases to a
/// narrower type than the parameter it overrides or implements because it takes the type argument of a type parameter
/// that parameter names, as `string value` of a `Behavior<string>`'s `set`. The bridge declares it with no PHP type, and
/// the engine checks a call from plain PHP against its written type, as Java's bridge method does for its erased
/// generics. A plain PHP class, a method the class inherits, and a narrower type no type parameter explains, as a
/// closure over PHP's `callable`, keep the refusal: the bridge lowers neither of the first two here, and the last would
/// refuse calls its parent accepts.
fn widens_parameter(
    artifacts: &mut AnalysisArtifacts,
    class_like_metadata: &ClassLikeMetadata,
    method_class: Word,
    method: &FunctionLikeMetadata,
    incompatibility: &SignatureCompatibilityIssue,
) -> bool {
    let SignatureCompatibilityIssue::ErasedParameterNarrowed { parameter_index, substitutes: true, .. } =
        incompatibility
    else {
        return false;
    };
    if !class_like_metadata.flags.is_sharp() || method_class != class_like_metadata.name {
        return false;
    }

    // The index comes from zipping the parent's parameters with `method`'s, so `method` has that parameter.
    let span = method.parameters[*parameter_index].name_span;
    artifacts.widened_parameters.insert((span.start.offset, span.end.offset));

    true
}

fn check_interface_method_signatures<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    class_like_metadata: &'ctx ClassLikeMetadata,
    interface_metadata: &'ctx ClassLikeMetadata,
    checked_signatures: &mut HashSet<(Word, Word)>,
) where
    A: Arena,
{
    for (method_name_atom, interface_method_id) in &interface_metadata.declaring_method_ids {
        let method_name_str = method_name_atom.as_ref();
        let interface_fqcn_word = interface_method_id.get_class_name();
        let interface_fqcn_str = interface_fqcn_word.as_bytes();
        // `BackedEnum` and `UnitEnum` declare the enum's methods the engine writes, also through an interface that
        // extends them.
        if should_skip_enum_builtin_interface(class_like_metadata, interface_fqcn_str) {
            continue;
        }

        let Some(interface_method) = context.codebase.get_declaring_method(interface_fqcn_str, method_name_str) else {
            continue;
        };

        let Some(class_method_id) = class_like_metadata.declaring_method_ids.get(method_name_atom) else {
            continue;
        };

        let class_fqcn_word = class_method_id.get_class_name();
        let class_fqcn_str = class_fqcn_word.as_bytes();
        let Some(class_method) = context.codebase.get_declaring_method(class_fqcn_str, method_name_str) else {
            continue;
        };

        if should_skip_same_method(class_fqcn_str, interface_fqcn_str) {
            continue;
        }

        if !checked_signatures.insert((interface_method_id.get_class_name(), *method_name_atom)) {
            continue;
        }

        let declaring_class = context.codebase.get_class_like(interface_fqcn_str).unwrap_or(interface_metadata);
        let method_span = class_method.name_span.unwrap_or(class_method.span);
        if report_named_constructor_clash(
            context,
            class_like_metadata,
            class_method,
            declaring_class,
            interface_method,
            method_span,
        ) {
            continue;
        }

        let substituted_interface_method = get_substituted_method(
            interface_method,
            class_like_metadata,
            interface_method_id.get_class_name(),
            context.codebase,
        );

        let substituted_class_method = get_substituted_method(
            class_method,
            class_like_metadata,
            class_method_id.get_class_name(),
            context.codebase,
        );

        let mut issues = method_signature::validate_method_signature_compatibility(
            context.codebase,
            class_like_metadata.name,
            &substituted_class_method,
            &substituted_interface_method,
            context.dialect,
        );
        method_signature::validate_erased_signature_compatibility(
            context.codebase,
            class_like_metadata,
            declaring_class,
            class_method,
            interface_method,
            &mut issues,
        );

        for incompatibility in issues {
            if widens_parameter(
                artifacts,
                class_like_metadata,
                class_method_id.get_class_name(),
                class_method,
                &incompatibility,
            ) {
                continue;
            }
            report_signature_compatibility_issue(
                context,
                class_like_metadata,
                declaring_class,
                *method_name_atom,
                class_method,
                incompatibility,
                method_span,
            );
        }
    }
}

/// Reports a method that shares its name with a PHP# named constructor of a parent class or interface, or the reverse,
/// spec section 9.1, and returns whether it did. PHP refuses the pair, a static and an instance method of one name, so
/// the class is refused once, in the words the user wrote, and the signature checks that would follow are skipped. Two
/// named constructors of one name are a subclass's own, checked as any static method pair is.
fn report_named_constructor_clash<A>(
    context: &mut Context<'_, '_, A>,
    child_class: &ClassLikeMetadata,
    child_method: &FunctionLikeMetadata,
    parent_class: &ClassLikeMetadata,
    parent_method: &FunctionLikeMetadata,
    primary_span: Span,
) -> bool
where
    A: Arena,
{
    let child_is_constructor = child_method.flags.is_named_constructor();
    if child_is_constructor == parent_method.flags.is_named_constructor() {
        return false;
    }

    let child =
        display_sharp_member(display_class_like_name(context, child_class.original_name), child_method.original_name);
    let parent_name = display_class_like_name(context, parent_class.original_name);
    let message = if child_is_constructor {
        let parent = display_sharp_member(parent_name, parent_method.original_name);

        format!("The named constructor `{child}` cannot share its name with the method `{parent}`.")
    } else {
        format!(
            "`{child}` cannot share its name with `{parent_name}`'s named constructor `{}`.",
            parent_method.original_name
        )
    };

    context.collector.report_with_code(
        IssueCode::NamedConstructorClash,
        Issue::error(message).with_annotation(Annotation::primary(primary_span).with_message("Declared here.")),
    );

    true
}

/// The class whose own constructor is written `required` nearest above the PHP# class `class_like_metadata`, when
/// `method_name` is the constructor, spec section 25: `new Self(…)` creates any class below it with the arguments that
/// constructor takes, so each constructor below it is compared with that one, not with its parent's.
fn required_constructor_class<A>(
    context: &Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    method_name: Word,
) -> Option<Word>
where
    A: Arena,
{
    if !class_like_metadata.flags.is_sharp() || method_name.as_bytes() != b"__construct" {
        return None;
    }

    std::iter::successors(class_like_metadata.direct_parent_class, |class| {
        context.codebase.get_class_like(class.as_bytes())?.direct_parent_class
    })
    .find(|class| {
        context
            .codebase
            .get_method(class.as_bytes(), b"__construct")
            .is_some_and(|constructor| constructor.flags.is_required())
    })
}

/// Reports a PHP# constructor that `new Self(…)` cannot call, below a `required` one, by the rule of spec section 25 it
/// breaks: it must start with the parameters of the `required` constructor, and leave no parameter without a default
/// that constructor leaves out, whether it adds one or drops the default the `required` constructor gives. The message
/// names the shortest `new Self(…)`, and the code stays the one the signature check found. A parameter without a type,
/// which only a file the parser refused declares, reads as `Any?`.
fn report_required_constructor_mismatch<A>(
    context: &mut Context<'_, '_, A>,
    child_name: &Word,
    parent_name: &Word,
    child_method: &FunctionLikeMetadata,
    parent_method: &FunctionLikeMetadata,
    incompatibility: method_signature::SignatureCompatibilityIssue,
    primary_span: Span,
) where
    A: Arena,
{
    use method_signature::SignatureCompatibilityIssue;

    let name = |parameter: &FunctionLikeParameterMetadata| {
        String::from_utf8_lossy(mago_bytes::trim_start_byte(parameter.name.0.as_bytes(), b'$')).into_owned()
    };
    let arguments = parent_method
        .parameters
        .iter()
        .filter(|parameter| !parameter.flags.has_default())
        .map(name)
        .collect::<Vec<_>>()
        .join(", ");
    let keeps_the_start = parent_method
        .parameters
        .iter()
        .enumerate()
        .all(|(index, parent)| child_method.parameters.get(index).is_some_and(|child| child.name == parent.name));
    let without_default: Vec<Word> = child_method
        .parameters
        .iter()
        .enumerate()
        .filter(|(index, child)| {
            !child.flags.has_default()
                && parent_method.parameters.get(*index).is_none_or(|parent| parent.flags.has_default())
        })
        .map(|(_, child)| word(name(child)))
        .collect();

    let code = match incompatibility {
        SignatureCompatibilityIssue::ParameterNameMismatch { .. } => IssueCode::IncompatibleParameterName,
        SignatureCompatibilityIssue::IncompatibleParameterType { .. } => IssueCode::IncompatibleParameterType,
        _ => IssueCode::IncompatibleParameterCount,
    };
    let leading_type_differs = matches!(incompatibility, SignatureCompatibilityIssue::IncompatibleParameterType { .. });
    let issue = match without_default.as_slice() {
        [first, ..] if keeps_the_start && !leading_type_differs => {
            let fix = if without_default.len() == 1 {
                format!("give `{first}` a default")
            } else {
                "give each a default".to_owned()
            };

            Issue::error(format!(
                "`{child_name}`'s constructor gives {} no default, so `new Self({arguments})` cannot call it: {fix}.",
                and_list(&without_default)
            ))
        }
        _ => {
            let start = parent_method
                .parameters
                .iter()
                .map(|parameter| {
                    let r#type =
                        parameter.type_metadata.as_ref().map_or_else(get_mixed, |r#type| r#type.type_union.clone());

                    format!("{} {}", display_type(context, &r#type), name(parameter))
                })
                .collect::<Vec<_>>()
                .join(", ");

            Issue::error(format!(
                "`{child_name}`'s constructor must start with `{start}`, as `{parent_name}`'s does, so `new Self({arguments})` can call it."
            ))
        }
    };

    context.collector.report_with_code(
        code,
        issue
            .with_annotation(Annotation::primary(primary_span).with_message("Declared here."))
            .with_note("A `required` constructor lets `new Self(…)` create any subclass, so every class below it keeps a constructor `new Self(…)` can call."),
    );
}

fn report_signature_compatibility_issue<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    child_class: &'ctx ClassLikeMetadata,
    parent_class: &'ctx ClassLikeMetadata,
    method_name: Word,
    parent_method: &FunctionLikeMetadata,
    incompatibility: SignatureCompatibilityIssue,
    primary_span: Span,
) where
    A: Arena,
{
    let child_name = display_class_like_name(context, child_class.original_name);
    let parent_name = display_class_like_name(context, parent_class.original_name);
    let child_class_span = child_class.name_span.unwrap_or(child_class.span);
    let parent_class_span = parent_class.name_span.unwrap_or(parent_class.span);
    if context.dialect.is_sharp()
        && parent_class.flags.is_sharp()
        && parent_class.flags.has_consistent_constructor()
        && method_name.as_bytes().eq_ignore_ascii_case(b"__construct")
        && let Some(child_constructor) =
            context.codebase.get_declaring_method(child_class.name.as_bytes(), b"__construct")
        && let Some(parent_constructor) =
            context.codebase.get_declaring_method(parent_class.name.as_bytes(), b"__construct")
    {
        report_required_constructor_mismatch(
            context,
            &child_name,
            &parent_name,
            child_constructor,
            parent_constructor,
            incompatibility,
            primary_span,
        );

        return;
    }

    let (child_display, parent_display) = if context.dialect.is_sharp() {
        let method_name = parent_method.original_name;

        (display_sharp_member(child_name, method_name), display_sharp_member(parent_name, method_name))
    } else {
        (format!("{child_name}::{method_name}()"), format!("{parent_name}::{method_name}()"))
    };

    use method_signature::SignatureCompatibilityIssue;

    match incompatibility {
        // PHP# checks only `.sharp` code, and the engine runs a closed PHP# method as an open one, so a plain PHP
        // subclass, a test double included, overrides it.
        SignatureCompatibilityIssue::FinalMethodOverride
            if !context.dialect.is_sharp() && parent_class.flags.is_sharp() => {}
        // A PHP# method is closed unless it is `virtual`, spec section 22, and only an override is written `final`.
        SignatureCompatibilityIssue::FinalMethodOverride if parent_class.flags.is_sharp() => {
            let reason = if parent_class.overridden_method_ids.contains_key(&method_name) {
                "is `final`"
            } else {
                "is not `virtual`"
            };
            context.collector.report_with_code(
                IssueCode::OverrideFinalMethod,
                Issue::error(format!("`{parent_display}` {reason}, so no subclass replaces it."))
                    .with_annotation(Annotation::primary(primary_span).with_message("Replaced here."))
                    .with_annotation(
                        Annotation::secondary(parent_class_span)
                            .with_message(format!("`{parent_display}` is declared here.")),
                    ),
            );
        }
        SignatureCompatibilityIssue::FinalMethodOverride => {
            context.collector.report_with_code(
                IssueCode::OverrideFinalMethod,
                Issue::error(format!("Cannot override final method `{parent_display}`"))
                    .with_annotation(
                        Annotation::primary(primary_span).with_message("Attempting to override final method here"),
                    )
                    .with_annotation(
                        Annotation::secondary(parent_class_span)
                            .with_message(format!("Method `{parent_display}` is declared as final")),
                    )
                    .with_annotation(
                        Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                    )
                    .with_note("Final methods cannot be overridden in child classes or traits.")
                    .with_help(format!(
                        "Remove the method `{method_name}()` from `{child_name}`, or remove the final modifier from the parent method."
                    )),
            );
        }
        SignatureCompatibilityIssue::ReadonlyDropped => {
            let relation = if parent_class.kind.is_interface() { "implements" } else { "overrides" };
            let declaration = sharp_method_declaration(context, parent_method);
            context.collector.report_with_code(
                IssueCode::IncompatibleReadonlyModifier,
                Issue::error(format!(
                    "{child_display} {relation} the readonly {parent_display}, so it is readonly too: declare it `{declaration} readonly`."
                ))
                .with_annotation(Annotation::primary(primary_span))
                .with_annotation(
                    Annotation::secondary(parent_method.name_span.unwrap_or(parent_method.span))
                        .with_message("Declared `readonly` here."),
                )
                .with_note("A call through a readonly value runs any override, so every override keeps `this` readonly."),
            );
        }
        SignatureCompatibilityIssue::StaticModifierMismatch { child_is_static, parent_is_static: _ } => {
            let (child_modifier, parent_modifier) =
                if child_is_static { ("static", "non-static") } else { ("non-static", "static") };

            context.collector.report_with_code(
                IssueCode::IncompatibleStaticModifier,
                Issue::error(format!(
                    "Cannot make {parent_modifier} method `{parent_display}` {child_modifier} in class `{child_name}`"
                ))
                .with_annotation(
                    Annotation::primary(primary_span)
                        .with_message(format!("This method is {child_modifier} but should be {parent_modifier}")),
                )
                .with_annotation(
                    Annotation::secondary(parent_class_span)
                        .with_message(format!("`{parent_display}` is defined as {parent_modifier} here")),
                )
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("The static modifier must match exactly between parent and child methods.")
                .with_help(format!("Change the method in `{child_name}` to be {parent_modifier} like the parent.")),
            );
        }
        SignatureCompatibilityIssue::VisibilityNarrowed { child_visibility, parent_visibility } => {
            context.collector.report_with_code(
                IssueCode::IncompatibleVisibility,
                Issue::error(format!(
                    "Visibility of `{child_display}` must not be narrowed from {parent_visibility} to {child_visibility}"
                ))
                .with_annotation(Annotation::primary(primary_span).with_message(format!(
                    "Method declared as {child_visibility} but should be {parent_visibility} or wider"
                )))
                .with_annotation(Annotation::secondary(parent_class_span).with_message(format!(
                    "Parent method `{parent_display}` is declared as {parent_visibility} here"
                )))
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("Visibility can only be widened (e.g., protected → public) not narrowed.")
                .with_help(format!("Change the visibility to {parent_visibility} or wider.")),
            );
        }
        SignatureCompatibilityIssue::ParameterCountMismatch { child_required_count, parent_required_count } => {
            context.collector.report_with_code(
                IssueCode::IncompatibleParameterCount,
                Issue::error(format!(
                    "`{child_display}` must accept at least {parent_required_count} required parameters like `{parent_display}`"
                ))
                .with_annotation(Annotation::primary(primary_span).with_message(format!(
                    "Method requires {child_required_count} parameters but parent requires {parent_required_count}"
                )))
                .with_annotation(Annotation::secondary(parent_class_span).with_message(format!(
                    "Parent method `{parent_display}` requires {parent_required_count} parameters"
                )))
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("Child methods must accept at least as many required parameters as the parent.")
                .with_help("Add optional parameters or reduce the number of required parameters in the child method."),
            );
        }
        SignatureCompatibilityIssue::MissingVariadicParameter { parameter_index } => {
            // PHP# writes the names without `$`, and only a PHP# file reports this.
            let param_name = parent_method
                .parameters
                .get(parameter_index)
                .map_or_else(|| word("unknown"), |p| word(mago_bytes::trim_start_byte(p.name.0.as_bytes(), b'$')));

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterCount,
                Issue::error(format!(
                    "`{child_display}` must declare parameter `{param_name}` variadic like `{parent_display}`"
                ))
                .with_annotation(
                    Annotation::primary(primary_span).with_message("This method takes no variadic parameter"),
                )
                .with_annotation(
                    Annotation::secondary(parent_class_span)
                        .with_message(format!("Parent method `{parent_display}` takes any number of `{param_name}`")),
                )
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("PHP refuses an override that does not take every argument the overridden method takes.")
                .with_help(format!("Declare `{param_name}` variadic, as in `int ...{param_name}`.")),
            );
        }
        SignatureCompatibilityIssue::IncompatibleParameterType { parameter_index, child_type, parent_type } => {
            let param_name = parent_method.parameters.get(parameter_index).map_or_else(
                || word("unknown"),
                |p| {
                    if context.dialect.is_sharp() {
                        word(mago_bytes::trim_start_byte(p.name.0.as_bytes(), b'$'))
                    } else {
                        p.name.0
                    }
                },
            );
            let child_type = display_type(context, &child_type);
            let parent_type = display_type(context, &parent_type);

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterType,
                Issue::error(format!(
                    "Parameter `{param_name}` of `{child_display}` expects type `{child_type}` but parent `{parent_display}` expects type `{parent_type}`"
                ))
                .with_annotation(Annotation::primary(primary_span).with_message(format!(
                    "Parameter `{param_name}` expects type `{child_type}` but parent expects `{parent_type}`"
                )))
                .with_annotation(
                    Annotation::secondary(parent_class_span)
                        .with_message(format!("Parent method `{parent_display}` parameter defined here")),
                )
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("Parameter types must be contravariant: child must accept equal or wider types than parent.")
                .with_help("Change the parameter type to be compatible with the parent method."),
            );
        }
        SignatureCompatibilityIssue::ErasedParameterNarrowed {
            parameter_index,
            child_type,
            parent_type,
            bound,
            ..
        } => {
            let parameter = parent_method.parameters.get(parameter_index);
            let param_name = parameter
                .map_or_else(|| word("unknown"), |p| word(mago_bytes::trim_start_byte(p.name.0.as_bytes(), b'$')));
            let span = parameter
                .map(|p| p.name_span)
                .filter(|span| span.file_id == primary_span.file_id)
                .unwrap_or(primary_span);
            let child_type = method_signature::display_erased(&child_type, context);
            let parent_type = method_signature::display_erased(&parent_type, context);
            let (child_method, parent_method) = if context.dialect.is_sharp() {
                let method_name = parent_method.original_name;

                (display_sharp_member(child_name, method_name), display_sharp_member(parent_name, method_name))
            } else {
                (format!("{child_name}::{method_name}()"), format!("{parent_name}::{method_name}()"))
            };

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterType,
                Issue::error(format!(
                    "Parameter `{param_name}` of `{child_method}` must take at least {parent_type}, the type `{parent_method}` erases it to."
                ))
                .with_annotation(Annotation::primary(span).with_message(format!("Erases to {child_type}.")))
                .with_annotation(Annotation::secondary(parent_class_span).with_message(format!(
                    "`{parent_method}` takes {parent_type} once its type parameters are erased."
                )))
                .with_note("PHP# erases type parameters when it compiles, and PHP refuses a parameter narrower than the one it overrides when it links the class.")
                .with_help(method_signature::erased_type_help(param_name, &parent_type, bound)),
            );
        }
        SignatureCompatibilityIssue::ChangedTemplateBound { template, child_method, parent_method, bound } => {
            let child_method = display_sharp_member(child_name, child_method);
            let parent_method = display_sharp_member(parent_name, parent_method);
            let (message, help) = match bound.map(|bound| display_type(context, &bound)) {
                Some(bound) => (
                    format!("`{child_method}` must keep the bound `{bound}` of `{parent_method}`."),
                    format!("Bound `{template}` by `{bound}`, as `{parent_method}` does."),
                ),
                None => (
                    format!("`{child_method}` must keep `{template}` of `{parent_method}` without a bound."),
                    format!("Remove the bound of `{template}`, as `{parent_method}` has none."),
                ),
            };

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterType,
                Issue::error(message)
                    .with_annotation(
                        Annotation::primary(primary_span).with_message(format!("Changes the bound of `{template}`")),
                    )
                    .with_annotation(
                        Annotation::secondary(parent_class_span)
                            .with_message(format!("`{parent_method}` declares the bound here")),
                    )
                    .with_note("An override keeps the bound of each type parameter it overrides, as in C#.")
                    .with_help(help),
            );
        }
        SignatureCompatibilityIssue::ChangedTemplateCount { child_method, parent_method, parent_count } => {
            let child_method = display_sharp_member(child_name, child_method);
            let parent_method = display_sharp_member(parent_name, parent_method);
            let declared = match parent_count {
                0 => "no type parameter".to_owned(),
                1 => "1 type parameter".to_owned(),
                count => format!("{count} type parameters"),
            };

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterType,
                Issue::error(format!("`{child_method}` must declare {declared}, as `{parent_method}` does."))
                    .with_annotation(Annotation::primary(primary_span).with_message("Overrides it here"))
                    .with_annotation(
                        Annotation::secondary(parent_class_span)
                            .with_message(format!("`{parent_method}` is declared here")),
                    )
                    .with_note("An override declares one type parameter for each of the method it overrides, as in C#.")
                    .with_help(format!("Declare {declared}, as `{parent_method}` does.")),
            );
        }
        SignatureCompatibilityIssue::IncompatibleReturnType { child_type, parent_type } => {
            let child_type = display_type(context, &child_type);
            let parent_type = display_type(context, &parent_type);

            context.collector.report_with_code(
                IssueCode::IncompatibleReturnType,
                Issue::error(format!(
                    "Return type `{child_type}` of `{child_display}` is incompatible with parent return type `{parent_type}` of `{parent_display}`"
                ))
                .with_annotation(
                    Annotation::primary(primary_span)
                        .with_message(format!("Returns type `{child_type}` but parent expects `{parent_type}`")),
                )
                .with_annotation(
                    Annotation::secondary(parent_class_span)
                        .with_message(format!("Parent method `{parent_display}` return type defined here")),
                )
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("Return types must be covariant: child must return equal or narrower types than parent.")
                .with_help("Change the return type to be compatible with the parent method."),
            );
        }
        SignatureCompatibilityIssue::MissingReturnTypeDeclaration { parent_type } => {
            let parent_type = display_type(context, &parent_type);

            context.collector.report_with_code(
                IssueCode::IncompatibleReturnType,
                Issue::error(format!(
                    "`{child_display}` must declare a return type compatible with `{parent_type}` declared by `{parent_display}`"
                ))
                .with_annotation(Annotation::primary(primary_span).with_message(format!(
                    "This method has no return type declaration, but the parent declares `{parent_type}`"
                )))
                .with_annotation(
                    Annotation::secondary(parent_class_span)
                        .with_message(format!("Parent method `{parent_display}` return type declared here")),
                )
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("PHP requires an overriding method to declare a return type when the overridden method declares one.")
                .with_help(format!("Add a return type declaration compatible with `{parent_type}`.")),
            );
        }
        SignatureCompatibilityIssue::ParameterNameMismatch {
            parameter_index,
            child_name: child_param_name,
            parent_name: parent_param_name,
        } => {
            // PHP# keeps every parameter name of the method it overrides, spec section 22, so a rename is an error, and
            // PHP# writes the names without `$`.
            let (level, child_param_name, parent_param_name) = if context.dialect.is_sharp() {
                (
                    Level::Error,
                    word(mago_bytes::trim_start_byte(child_param_name.as_bytes(), b'$')),
                    word(mago_bytes::trim_start_byte(parent_param_name.as_bytes(), b'$')),
                )
            } else {
                (Level::Warning, child_param_name, parent_param_name)
            };

            context.collector.report_with_code(
                IssueCode::IncompatibleParameterName,
                Issue::new(
                    level,
                    format!(
                        "Parameter #{} of `{child_display}` is named `{child_param_name}` but parent `{parent_display}` names it `{parent_param_name}`",
                        parameter_index + 1,
                    ),
                )
                .with_annotation(Annotation::primary(primary_span).with_message(format!(
                    "Parameter named `{child_param_name}` but parent uses `{parent_param_name}`",
                )))
                .with_annotation(Annotation::secondary(parent_class_span).with_message(format!(
                    "Parent method `{parent_display}` parameter `{parent_param_name}` defined here",
                )))
                .with_annotation(
                    Annotation::secondary(child_class_span).with_message(format!("In class `{child_name}`")),
                )
                .with_note("Parameter name changes can break code using named arguments.")
                .with_help(format!(
                    "Consider renaming the parameter to `{parent_param_name}` to match the parent method."
                )),
            );
        }
    }
}

/// Reports a property whose docblock type cannot hold what its native declaration promises.
///
/// Parameters and returns already get this check; a property never did, so a `@var` narrowing a
/// native type to something unrelated was silently trusted everywhere the property was read.
fn report_property_type_mismatch<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    docblock_type: &TypeMetadata,
    native_type: &TypeMetadata,
) where
    A: Arena,
{
    let options = TypeExpansionOptions {
        self_class: Some(class_like_metadata.name),
        static_class_type: StaticClassType::Name(class_like_metadata.name),
        ..Default::default()
    };
    let mut expanded_docblock = docblock_type.type_union.clone();
    expand_union(context.codebase, &mut expanded_docblock, &options);
    let mut expanded_native = native_type.type_union.clone();
    expand_union(context.codebase, &mut expanded_native, &options);

    if union_comparator::is_contained_by_with_erased_template_arguments(
        context.codebase,
        &expanded_docblock,
        &expanded_native,
        &mut ComparisonResult::default(),
    ) {
        return;
    }

    let docblock_type_str = expanded_docblock.get_id();
    let native_type_str = native_type.type_union.get_id();
    context.collector.report_with_code(
        IssueCode::DocblockTypeMismatch,
        Issue::error(format!(
            "Docblock property type `{docblock_type_str}` is incompatible with native property type `{native_type_str}`."
        ))
        .with_annotation(Annotation::primary(native_type.span).with_message(format!("Native type is `{native_type_str}`...")))
        .with_annotation(
            Annotation::secondary(docblock_type.span).with_message(format!("...but docblock declares `{docblock_type_str}`")),
        )
        .with_note("The docblock type must be compatible with the native type declaration.")
        .with_help(format!(
            "Either change the docblock type to match `{native_type_str}`, or update the native type to be compatible with `{docblock_type_str}`."
        )),
    );
}

/// Reports undefined type references in the `@method`, `@property` and `@mixin`
/// tags of a class-like docblock.
///
/// The members these tags declare have no CST of their own, so they are never
/// visited by the analyzers that perform this check for real methods and
/// properties.
fn check_docblock_declared_members<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    declaration_span: Span,
) where
    A: Arena,
{
    for pseudo_method_name in &class_like_metadata.pseudo_methods {
        let Some(metadata) = context.codebase.function_likes.get(&(class_like_metadata.name, *pseudo_method_name))
        else {
            continue;
        };

        if let Some(return_type) = &metadata.return_type_metadata {
            report_undefined_type_references(context, return_type);
        }

        for parameter in &metadata.parameters {
            if let Some(parameter_type) = &parameter.type_declaration_metadata {
                report_undefined_type_references(context, parameter_type);
            }
        }
    }

    for (property_name, property_metadata) in &class_like_metadata.magic_properties {
        if class_like_metadata.magic_property_ids.get(property_name) != Some(&class_like_metadata.name) {
            continue;
        }

        for type_metadata in
            [&property_metadata.type_metadata, &property_metadata.write_type_metadata].into_iter().flatten()
        {
            report_undefined_type_references(context, type_metadata);
        }
    }

    // Unlike the tags above, mixins are merged into every subclass, so report only
    // the ones coming from this class-like's own docblock.
    let docblocks: Vec<Span> =
        PrecedingDocblocks::new(context.comments, declaration_span.start.offset).map(|trivia| trivia.span).collect();

    for mixin in &class_like_metadata.mixins {
        if docblocks.iter().any(|docblock| docblock.contains(&mixin.span)) {
            report_undefined_type_references(context, mixin);
        }
    }
}

/// Reports a PHP# override of a plain PHP property that has no `set`, an `init`, or a `set` at another level than the
/// parent's, spec section 6.1. The parent's own code writes the property at any time, which a get-only or an `init`
/// property promises no code does, and line 287 makes the override's access level the parent's.
fn report_plain_php_override_set<A>(
    context: &mut Context<'_, '_, A>,
    property: &PropertyMetadata,
    parent: &PropertyMetadata,
    (child_display, parent_display, parent_class): (&str, &str, Word),
    (span, parent_span): (Span, Span),
) where
    A: Arena,
{
    let settable = |property: &PropertyMetadata| {
        !property.flags.is_readonly()
            && !property.flags.is_init()
            && (!property.flags.is_virtual_property() || property.hooks.contains_key(&word("set")))
    };

    let issue = if settable(parent) && !settable(property) {
        let accessor = if property.flags.is_init() { "`set`, not `init`" } else { "`set`" };

        Issue::error(format!(
            "`{parent_display}` is settable, and `{parent_class}`'s own code writes it, so its override `{child_display}` has {accessor}."
        ))
        .with_help("Give the override `set`, as in `{ get; set; }`.")
    } else if settable(property)
        // A read level that differs is its own error, which also fixes the set level.
        && property.read_visibility == parent.read_visibility
        && property.write_visibility != parent.write_visibility
    {
        let level = parent.write_visibility;

        Issue::error(format!(
            "`{parent_display}` is set at `{level}`, so its override `{child_display}` is set at `{level}` too."
        ))
        .with_help("Write `set` with no access modifier: an override keeps its parent's access level.")
    } else {
        return;
    };

    context.collector.report_with_code(
        IssueCode::IncompatiblePropertyAccess,
        issue
            .with_annotation(Annotation::primary(span).with_message("Overridden here."))
            .with_annotation(Annotation::secondary(parent_span).with_message("The parent's property.")),
    );
}

#[allow(clippy::similar_names)]
fn check_class_like_properties<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
) where
    A: Arena,
{
    if class_like_metadata.kind.is_enum() {
        return;
    }

    // Check properties declared directly in this class against parent classes
    for (property_name, property_metadata) in &class_like_metadata.properties {
        // Only check properties declared in this class, not inherited ones
        let Some(declaring_fqcn) = class_like_metadata.declaring_property_ids.get(property_name) else {
            continue;
        };

        if declaring_fqcn != &class_like_metadata.name {
            // Property is inherited, not declared in this class
            continue;
        }

        // Validate set hook parameter type is supertype of property type
        if let Some(set_hook) = property_metadata.hooks.get(&word("set"))
            && let Some(param) = &set_hook.parameter
            && let Some(param_type) = param.type_declaration_metadata.as_ref()
            && let Some(property_type) = property_metadata.type_metadata.as_ref()
        {
            // The set hook parameter type must contain the property type (contravariance)
            // i.e., any value assignable to the property type should be accepted by the hook
            if !is_type_compatible(context.codebase, &property_type.type_union, &param_type.type_union) {
                let property_type_id = property_type.type_union.get_id();
                let param_type_id = param_type.type_union.get_id();
                let class_name = class_like_metadata.original_name;

                context.collector.report_with_code(
                    IssueCode::IncompatiblePropertyHookParameterType,
                    Issue::error(format!(
                        "Set hook parameter type `{param_type_id}` for property `{class_name}::{property_name}` is incompatible with property type `{property_type_id}`."
                    ))
                    .with_annotation(
                        Annotation::primary(param_type.span)
                            .with_message(format!("This type `{param_type_id}` does not accept all values of type `{property_type_id}`")),
                    )
                    .with_annotation(
                        Annotation::secondary(property_type.span)
                            .with_message(format!("Property is declared with type `{property_type_id}`")),
                    )
                    .with_note("The set hook parameter type must be equal to or wider than the property type (contravariance).")
                    .with_help(format!("Change the set hook parameter type to `{property_type_id}` or a wider type that contains `{property_type_id}`.")),
                );
            }
        }

        // Validate docblock param type >= native param type
        if let Some(set_hook) = property_metadata.hooks.get(&word("set"))
            && let Some(param) = &set_hook.parameter
            && let Some(native_type) = param.type_declaration_metadata.as_ref()
            && let Some(effective_type) = param.type_metadata.as_ref()
            && effective_type.from_docblock
            && !is_type_compatible(context.codebase, &native_type.type_union, &effective_type.type_union)
        {
            let native_type_str = native_type.type_union.get_id();
            let docblock_type_str = effective_type.type_union.get_id();

            context.collector.report_with_code(
                    IssueCode::DocblockTypeMismatch,
                    Issue::error(format!(
                        "Docblock type `{docblock_type_str}` is narrower than native parameter type `{native_type_str}`."
                    ))
                    .with_annotation(
                        Annotation::primary(effective_type.span)
                            .with_message(format!("Docblock type `{docblock_type_str}` cannot narrow native type `{native_type_str}`")),
                    )
                    .with_note(
                        "The @param docblock type must be a supertype of the native type. It can widen the type (e.g., int to int|string) but not narrow it.",
                    )
                    .with_help(format!(
                        "Change the docblock type to `{native_type_str}` or a wider type.",
                    )),
                );
        }

        // PHP links a class against the nearest declaration of the property, so PHP# measures an override against it
        // alone. The declarations up to the root decide whether the property runs untyped.
        let overridden: Vec<_> = if context.dialect.is_sharp() {
            override_attribute::overridden_properties(class_like_metadata, *property_name, context.codebase).collect()
        } else {
            Vec::new()
        };

        // Check each parent class for this property
        for parent_fqcn in &class_like_metadata.all_parent_classes {
            let parent_fqcn_str = parent_fqcn.as_ref();
            let Some(parent_metadata) = context.codebase.get_class_like(parent_fqcn_str) else {
                continue;
            };

            let Some(parent_property) = parent_metadata.properties.get(property_name) else {
                continue;
            };

            if parent_property.read_visibility.is_private() && parent_property.write_visibility.is_private() {
                continue;
            }

            if overridden.first().is_some_and(|(nearest, _)| nearest.name != parent_metadata.name) {
                continue;
            }

            let property_span = property_metadata.name_span.unwrap_or(class_like_metadata.span);
            let parent_property_span = parent_property.name_span.unwrap_or(parent_metadata.span);
            let declaring_class_name = display_class_like_name(context, class_like_metadata.original_name);
            let parent_class_name = parent_metadata.original_name;
            let child_display = display_member(context, declaring_class_name, property_name);
            let parent_display = display_member(context, parent_class_name, property_name);
            let property = display_property_name(context, *property_name);

            // A PHP# property is closed unless it is `virtual` or `abstract`, spec sections 6.1 and 22, so the codex reads
            // it as final. PHP# writes no `final` on a property, so a final PHP# property is a closed one.
            let sharp_parent = context.dialect.is_sharp() && parent_metadata.flags.is_sharp();
            // PHP# checks only `.sharp` code, so a plain PHP subclass, a test double included, overrides a closed PHP#
            // property. PHP itself makes one whose `set` is private final, and refuses that override.
            let open_to_php = !context.dialect.is_sharp()
                && parent_metadata.flags.is_sharp()
                && !parent_property.write_visibility.is_private();
            if sharp_parent && parent_property.flags.is_final() && !parent_property.write_visibility.is_private() {
                let parent_class = display_class_like_name(context, parent_class_name);

                context.collector.report_with_code(
                    IssueCode::OverrideFinalProperty,
                    Issue::error(format!(
                        "Cannot override `{parent_display}`: a PHP# property is closed unless it is `virtual` or `abstract`."
                    ))
                    .with_annotation(Annotation::primary(property_span).with_message("Overridden here."))
                    .with_annotation(Annotation::secondary(parent_property_span).with_message("Declared closed here."))
                    .with_help(format!("Declare `{property}` `virtual` in `{parent_class}` to let a subclass override it.")),
                );
            } else if !open_to_php
                && (parent_property.flags.is_final()
                // PHP makes a property whose `set` is private final, so the engine refuses a PHP# override of one.
                || (context.dialect.is_sharp() && parent_property.write_visibility.is_private()))
            {
                context.collector.report_with_code(
                    IssueCode::OverrideFinalProperty,
                    Issue::error(format!("Cannot override final property `{parent_display}`."))
                    .with_annotation(
                        Annotation::primary(property_span)
                            .with_message("Attempting to override final property here"),
                    )
                    .with_annotation(
                        Annotation::secondary(parent_property_span)
                            .with_message(format!("Property `{parent_display}` is declared as final")),
                    )
                    .with_note("Final properties cannot be overridden in child classes.")
                    .with_help(format!(
                        "Remove the property `{property}` from `{declaring_class_name}`, or remove the final modifier from the parent property.",
                    )),
                );
            }

            for (hook_name, child_hook) in &property_metadata.hooks {
                if let Some(parent_hook) = parent_property.hooks.get(hook_name)
                    && parent_hook.flags.is_final()
                {
                    let final_hook = if context.dialect.is_sharp() {
                        display_sharp_accessor(context, parent_class_name, *property_name, *hook_name)
                    } else {
                        format!("{parent_class_name}::{property_name}::{hook_name}()")
                    };

                    context.collector.report_with_code(
                            IssueCode::OverrideFinalPropertyHook,
                            Issue::error(format!("Cannot override final property hook `{final_hook}`."))
                            .with_annotation(
                                Annotation::primary(child_hook.span)
                                    .with_message("Attempting to override final hook here"),
                            )
                            .with_annotation(
                                Annotation::secondary(parent_hook.span)
                                    .with_message(format!("Hook `{final_hook}` is declared as final")),
                            )
                            .with_note("Final property hooks cannot be overridden in child classes.")
                            .with_help(format!(
                                "Remove the `{hook_name}` hook from `{child_display}`, or remove the final modifier from the parent hook.",
                            )),
                        );
                }
            }

            // Backed property with by-ref get + set hook is invalid
            if parent_property.hooks.is_empty()
                && let Some(get_hook) = property_metadata.hooks.get(&word("get"))
                && get_hook.returns_by_ref
                && property_metadata.hooks.contains_key(&word("set"))
            {
                context.collector.report_with_code(
                    IssueCode::BackedPropertyReferenceHook,
                    Issue::error(format!(
                        "Get hook of backed property `{child_display}` with set hook may not return by reference."
                    ))
                    .with_annotation(
                        Annotation::primary(get_hook.span)
                            .with_message("This get hook returns by reference"),
                    )
                    .with_annotation(
                        Annotation::secondary(parent_property_span)
                            .with_message(format!("Property `{parent_display}` creates a backing store")),
                    )
                    .with_note("A backed property (with backing store) that has a set hook cannot have a by-reference get hook.")
                    .with_help("Remove the `&` from the get hook declaration, or remove the set hook."),
                );
            }

            if property_metadata.read_visibility > parent_property.read_visibility {
                context.collector.report_with_code(
                        IssueCode::IncompatiblePropertyAccess,
                        Issue::error(format!(
                            "Property `{child_display}` has a different read access level than `{parent_display}`."
                        ))
                        .with_annotation(
                            Annotation::primary(property_span)
                                .with_message(format!("This property is declared as `{}`", property_metadata.read_visibility)),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_property_span)
                                .with_message(format!("Parent property is declared as `{}`", parent_property.read_visibility)),
                        )
                        .with_note("The access level of an overridden property must not be more restrictive than the parent property.")
                        .with_help("Adjust the access level of the property in the child class to match or be less restrictive than the parent class."),
                    );
            } else if context.dialect.is_sharp() && property_metadata.read_visibility < parent_property.read_visibility
            {
                let (visibility, parent_visibility) =
                    (property_metadata.read_visibility, parent_property.read_visibility);

                context.collector.report_with_code(
                    IssueCode::IncompatiblePropertyAccess,
                    Issue::error(format!(
                        "The override `{child_display}` is `{visibility}`, but `{parent_display}` is `{parent_visibility}`."
                    ))
                    .with_annotation(Annotation::primary(property_span).with_message(format!("Declared `{visibility}` here.")))
                    .with_annotation(
                        Annotation::secondary(parent_property_span)
                            .with_message(format!("Declared `{parent_visibility}` here.")),
                    )
                    .with_note("An override keeps the access level of the property it replaces.")
                    .with_help(format!("Declare the override `{parent_visibility}`.")),
                );
            }

            // Spec section 6.1: a plain PHP parent's own code writes its property, so its override has `set`, at the
            // parent's level (line 287). PHP refuses a `readonly` or a set level the parent lacks as the class links.
            let plain_php_parent = context.dialect.is_sharp() && !parent_metadata.flags.is_sharp();
            if plain_php_parent {
                report_plain_php_override_set(
                    context,
                    property_metadata,
                    parent_property,
                    (&child_display, &parent_display, display_class_like_name(context, parent_class_name)),
                    (property_span, parent_property_span),
                );
            }

            if !plain_php_parent
                && (property_metadata.write_visibility != property_metadata.read_visibility
                    || parent_property.write_visibility != parent_property.read_visibility)
                && property_metadata.write_visibility > parent_property.write_visibility
            {
                context.collector.report_with_code(
                        IssueCode::IncompatiblePropertyAccess,
                        Issue::error(format!(
                            "Property `{child_display}` has a different write access level than `{parent_display}`."
                        ))
                        .with_annotation(
                            Annotation::primary(property_span)
                                .with_message(format!("This property is declared as `{}(set)`", property_metadata.write_visibility)),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_property_span)
                                .with_message(format!("Parent property is declared as `{}(set)`", parent_property.write_visibility)),
                        )
                        .with_note("The access level of an overridden property must not be more restrictive than the parent property.")
                        .with_help("Adjust the access level of the property in the child class to match or be less restrictive than the parent class."),
                    );
            }

            // Check static modifier consistency
            if property_metadata.flags.is_static() != parent_property.flags.is_static() {
                let (child_modifier, parent_modifier) = if property_metadata.flags.is_static() {
                    ("static", "non-static")
                } else {
                    ("non-static", "static")
                };

                context.collector.report_with_code(
                        IssueCode::IncompatibleStaticModifier,
                        Issue::error(format!(
                            "Cannot redeclare {parent_modifier} property `{parent_display}` as {child_modifier} `{child_display}`."
                        ))
                        .with_annotation(
                            Annotation::primary(property_span)
                                .with_message(format!("This property is declared as `{child_modifier}`")),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_property_span)
                                .with_message(format!("Parent property is declared as `{parent_modifier}`")),
                        )
                        .with_note("Properties must maintain the same static modifier when overriding parent properties.")
                        .with_help(format!("Change this property to be `{parent_modifier}` to match the parent class.")),
                    );
            }

            // Check readonly modifier consistency
            if !plain_php_parent && property_metadata.flags.is_readonly() != parent_property.flags.is_readonly() {
                let (child_modifier, parent_modifier) = if property_metadata.flags.is_readonly() {
                    ("readonly", "non-readonly")
                } else {
                    ("non-readonly", "readonly")
                };

                context.collector.report_with_code(
                        IssueCode::IncompatibleReadonlyModifier,
                        Issue::error(format!(
                            "Cannot redeclare {parent_modifier} property `{parent_display}` as {child_modifier} `{child_display}`."
                        ))
                        .with_annotation(
                            Annotation::primary(property_span)
                                .with_message(format!("This property is declared as `{child_modifier}`")),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_property_span)
                                .with_message(format!("Parent property is declared as `{parent_modifier}`")),
                        )
                        .with_note("Properties must maintain the same readonly modifier when overriding parent properties.")
                        .with_help(format!("Change this property to be `{parent_modifier}` to match the parent class.")),
                    );
            }

            // Determine allowed variance based on parent hooks:
            // - Virtual property with only `get` hook: covariance (declaring narrower than parent)
            //   is safe because consumers only read, so a more specific type upholds the contract.
            // - Virtual property with only `set` hook: contravariance (declaring wider than parent)
            //   is safe because consumers only write, so accepting a wider type upholds the contract.
            // - Otherwise, invariance is required.
            let parent_is_virtual = parent_property.flags.is_virtual_property();
            let parent_has_get_hook = parent_property.hooks.contains_key(&word("get"));
            let parent_has_set_hook = parent_property.hooks.contains_key(&word("set"));
            let parent_only_get = parent_is_virtual && parent_has_get_hook && !parent_has_set_hook;
            let parent_only_set = parent_is_virtual && parent_has_set_hook && !parent_has_get_hook;

            let mut has_type_incompatibility = false;
            match (
                property_metadata.type_declaration_metadata.as_ref(),
                parent_property.type_declaration_metadata.as_ref(),
            ) {
                (Some(declaring_type), Some(parent_type)) => {
                    // A PHP# declaration may name a type parameter, which the child's header or `@extends` fills.
                    let is_sharp = parent_metadata.flags.is_sharp() || class_like_metadata.flags.is_sharp();
                    let parent_type_union = if is_sharp {
                        Cow::Owned(localize_parent_type(
                            context.codebase,
                            class_like_metadata,
                            parent_metadata.name,
                            &parent_type.type_union,
                        ))
                    } else {
                        Cow::Borrowed(&parent_type.type_union)
                    };

                    if is_property_type_variance_invalid(
                        context.codebase,
                        &declaring_type.type_union,
                        &parent_type_union,
                        parent_only_get,
                        parent_only_set,
                    ) {
                        has_type_incompatibility = true;

                        let declaring_type_id = display_type(context, &declaring_type.type_union);
                        let parent_type_id = display_type(context, &parent_type_union);

                        context.collector.report_with_code(
                                IssueCode::IncompatiblePropertyType,
                                Issue::error(format!(
                                    "Property `{child_display}` has an incompatible type declaration."
                                ))
                                .with_annotation(
                                    Annotation::primary(declaring_type.span)
                                        .with_message(format!("This type `{declaring_type_id}` is incompatible with the parent's type.")),
                                )
                                .with_annotation(
                                    Annotation::secondary(parent_type.span)
                                        .with_message(format!("The parent property is defined with type `{parent_type_id}` here.")),
                                )
                                .with_note("PHP requires property types to be invariant, meaning the type declaration in a child class must be exactly the same as in the parent class.")
                                .with_help(format!("Change the type of `{property}` to `{parent_type_id}` to match the parent property."))
                            );
                    } else if is_sharp {
                        let erased_type = method_signature::erase(&declaring_type.type_union, context.codebase);
                        let erased_parent_type = method_signature::erase(&parent_type.type_union, context.codebase);

                        if is_property_type_variance_invalid(
                            context.codebase,
                            &erased_type,
                            &erased_parent_type,
                            parent_only_get,
                            parent_only_set,
                        ) {
                            has_type_incompatibility = true;

                            let erased_type_id = method_signature::display_erased(&erased_type, context);
                            let erased_parent_type_id = method_signature::display_erased(&erased_parent_type, context);
                            let property_name = mago_bytes::trim_start_byte(property_metadata.name.0.as_bytes(), b'$');
                            let property_name = String::from_utf8_lossy(property_name);
                            let class_name = class_like_metadata.original_name;
                            let help = method_signature::erased_type_help(
                                &property_name,
                                &erased_parent_type_id,
                                method_signature::bound_example(context.codebase, Some(parent_type), &erased_type),
                            );
                            let message = if context.dialect.is_sharp() {
                                format!(
                                    "Property `{}` must have {erased_parent_type_id}, the type `{}` erases to.",
                                    display_sharp_member(class_name, &property_name),
                                    display_sharp_member(parent_class_name, &property_name)
                                )
                            } else {
                                format!(
                                    "Property `{class_name}::${property_name}` must have the type {erased_parent_type_id}, the type `{parent_class_name}::${property_name}` erases to."
                                )
                            };

                            context.collector.report_with_code(
                                IssueCode::IncompatiblePropertyType,
                                Issue::error(message)
                                .with_annotation(
                                    Annotation::primary(declaring_type.span)
                                        .with_message(format!("Erases to {erased_type_id}.")),
                                )
                                .with_annotation(Annotation::secondary(parent_type.span).with_message(format!(
                                    "Erases to {erased_parent_type_id}."
                                )))
                                .with_note("PHP# erases type parameters when it compiles, and PHP requires a property to keep the type of the property it overrides.")
                                .with_help(help),
                            );
                        }
                    }
                }
                (Some(declaring_type), None) => {
                    has_type_incompatibility = true;

                    // A PHP# field over an untyped PHP property writes a type the engine drops when the class links, so
                    // it fits the parent's `@var` type, or any type without one, spec section 6.1.
                    if context.dialect.is_sharp() {
                        if let Some(parent_type) = parent_property.type_metadata.as_ref().filter(|t| t.from_docblock) {
                            let parent_type_union = localize_parent_type(
                                context.codebase,
                                class_like_metadata,
                                parent_metadata.name,
                                &parent_type.type_union,
                            );

                            if !is_type_compatible(context.codebase, &declaring_type.type_union, &parent_type_union) {
                                let declaring_type_id = display_type(context, &declaring_type.type_union);
                                let parent_type_id = display_type(context, &parent_type_union);

                                context.collector.report_with_code(
                                    IssueCode::IncompatiblePropertyType,
                                    Issue::error(format!(
                                        "The override `{child_display}` has type `{declaring_type_id}`, which does not fit `{parent_type_id}`, the type of `{parent_display}`."
                                    ))
                                    .with_annotation(
                                        Annotation::primary(declaring_type.span)
                                            .with_message(format!("`{declaring_type_id}` is written here.")),
                                    )
                                    .with_annotation(
                                        Annotation::secondary(parent_type.span)
                                            .with_message(format!("`{parent_type_id}` is the parent's `@var` type.")),
                                    )
                                    .with_note("PHP does not check this property's type, so the override's type must fit the one the parent's code relies on.")
                                    .with_help(format!("Write a type that fits `{parent_type_id}`.")),
                                );
                            }
                        }

                        continue;
                    }

                    let mut issue = Issue::error(format!(
                        "Property `{child_display}` adds a type that is missing on the parent property."
                    ))
                    .with_annotation(
                        Annotation::primary(declaring_type.span)
                            .with_message("This type declaration is not present on the parent property"),
                    );

                    if let Some(parent_property_span) = parent_property.name_span {
                        issue = issue.with_annotation(
                            Annotation::secondary(parent_property_span)
                                .with_message("The parent property is defined here without a type"),
                        );
                    }

                    context.collector.report_with_code(IssueCode::IncompatiblePropertyType, issue
                            .with_note("Adding a type to a property that was untyped in a parent class is an incompatible change.")
                                   .with_help("You can either remove the type from this property or add an identical type to the property in the parent class."));
                }
                (None, Some(parent_type)) => {
                    has_type_incompatibility = true;

                    if let Some(property_span) = property_metadata.name_span {
                        let parent_type_id = display_type(context, &parent_type.type_union);

                        context.collector.report_with_code(
                                IssueCode::IncompatiblePropertyType,
                                Issue::error(format!(
                                    "Property `{child_display}` is missing the type declaration from its parent."
                                ))
                                .with_annotation(
                                    Annotation::primary(property_span)
                                        .with_message("This property declaration is missing a type"),
                                )
                                .with_annotation(
                                    Annotation::secondary(parent_type.span)
                                        .with_message(format!("The parent property is defined with type `{parent_type_id}` here")),
                                )
                                .with_note("Removing a type from a property that was typed in a parent class is an incompatible change.")
                                .with_help(format!("Add the type declaration `{parent_type_id}` to this property to match the parent definition."))
                            );
                    }
                }
                (None, None) => {
                    // no type declaration, nothing to check
                }
            }

            let localized_parent_type = parent_property.type_metadata.as_ref().map(|parent_type| {
                localize_parent_type(
                    context.codebase,
                    class_like_metadata,
                    parent_metadata.name,
                    &parent_type.type_union,
                )
            });

            if !has_type_incompatibility
                && let Some(declaring_type) = &property_metadata.type_metadata
                && declaring_type.from_docblock
                && let Some(parent_type) = &parent_property.type_metadata
                && let Some(localized_parent_type) = &localized_parent_type
                && is_property_type_variance_invalid(
                    context.codebase,
                    &declaring_type.type_union,
                    localized_parent_type,
                    parent_only_get,
                    parent_only_set,
                )
            {
                let declaring_type_id = declaring_type.type_union.get_id();
                let parent_type_id = localized_parent_type.get_id();

                context.collector.report_with_code(
                        IssueCode::IncompatiblePropertyType,
                        Issue::error(format!(
                            "Property `{child_display}` has an incompatible type declaration from docblock."
                        ))
                        .with_annotation(
                            Annotation::primary(declaring_type.span)
                                .with_message(format!("This type `{declaring_type_id}` is incompatible with the parent's type.")),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_type.span)
                                .with_message(format!("The parent property is defined with type `{parent_type_id}` here.")),
                        )
                        .with_note("PHP requires property types to be invariant, meaning the type declaration in a child class must be exactly the same as in the parent class.")
                        .with_help(format!("Change the type of `{property}` to `{parent_type_id}` to match the parent property.")),
                    );
            }
        }

        // Check interface hook by-ref signature compatibility
        for interface_fqcn in &class_like_metadata.all_parent_interfaces {
            let Some(interface_metadata) = context.codebase.get_class_like(interface_fqcn.as_bytes()) else {
                continue;
            };

            let Some(interface_property) = interface_metadata.properties.get(property_name) else {
                continue;
            };

            for (hook_name, interface_hook) in &interface_property.hooks {
                if !interface_hook.returns_by_ref {
                    continue;
                }

                let Some(impl_hook) = property_metadata.hooks.get(hook_name) else {
                    continue;
                };

                if impl_hook.returns_by_ref {
                    continue;
                }

                let declaring_class_name = class_like_metadata.original_name;
                let interface_name = interface_metadata.original_name;
                let (class_hook_name, interface_hook_name) = if context.dialect.is_sharp() {
                    (
                        display_sharp_accessor(context, declaring_class_name, *property_name, *hook_name),
                        display_sharp_accessor(context, interface_name, *property_name, *hook_name),
                    )
                } else {
                    (
                        format!("{declaring_class_name}::{property_name}::{hook_name}()"),
                        format!("{interface_name}::{property_name}::{hook_name}()"),
                    )
                };

                context.collector.report_with_code(
                    IssueCode::IncompatiblePropertyHookSignature,
                    Issue::error(format!(
                        "Declaration of `{class_hook_name}` must be compatible with `& {interface_hook_name}`."
                    ))
                    .with_annotation(
                        Annotation::primary(impl_hook.span)
                            .with_message("This hook does not return by reference"),
                    )
                    .with_annotation(
                        Annotation::secondary(interface_hook.span)
                            .with_message(format!(
                                "Interface `{}` requires this hook to return by reference",
                                display_class_like_name(context, interface_name)
                            )),
                    )
                    .with_note("When an interface declares a by-reference hook (`&get`), the implementing class must also return by reference.")
                    .with_help(format!("Add `&` to the `{hook_name}` hook declaration: `&{hook_name} => ...`")),
                );
            }
        }
    }

    if context.dialect.is_sharp() {
        check_interface_properties(context, class_like_metadata);
    }
}

/// Checks each property a PHP# class or event ends up with against the property each interface above it states, as
/// the engine links the class against it. A property the class declares answers to every interface above it. One it
/// inherits answers to the interfaces the class names itself, because a base class that names an interface answers for
/// it there. A base class's `private` property counts, as PHP links against it too.
fn check_interface_properties<A>(context: &mut Context<'_, '_, A>, class_like_metadata: &ClassLikeMetadata)
where
    A: Arena,
{
    let codebase = context.codebase;
    let names_itself = |interface_name: &Word| {
        class_like_metadata.direct_parent_interfaces.iter().any(|named| {
            named == interface_name
                || codebase.get_class_like(named.as_bytes()).is_some_and(|named| named.has_parent(*interface_name))
        })
    };

    for interface_name in &class_like_metadata.all_parent_interfaces {
        let Some(interface) = codebase.get_class_like(interface_name.as_bytes()) else {
            continue;
        };

        for (property_name, stated) in &interface.properties {
            let owner = std::iter::successors(Some(class_like_metadata), |metadata| {
                metadata.direct_parent_class.and_then(|parent| codebase.get_class_like(parent.as_bytes()))
            })
            .find_map(|metadata| metadata.properties.get(property_name).map(|property| (metadata, property)));
            let Some((owner, property)) = owner else {
                continue;
            };

            if owner.name == class_like_metadata.name || names_itself(interface_name) {
                report_unfit_interface_property(context, class_like_metadata, owner, interface, property, stated);
            }
        }
    }
}

/// Reports a PHP# class's or event's property, declared by `owner`, that does not fit the property `interface` states,
/// as the engine refuses the class: it is public, on each object, and of the stated type or a narrower one for a
/// get-only property. Every interface property is virtual, so its hooks alone say whether it is get-only. The stated
/// type reads a generic interface's type parameters as the type arguments the class's header gives it, as `TItem` is
/// `int` under `: Holder<int>`. A property the class inherits is reported on the class's name, naming the base class to
/// change.
fn report_unfit_interface_property<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    owner: &ClassLikeMetadata,
    interface: &ClassLikeMetadata,
    property: &PropertyMetadata,
    stated: &PropertyMetadata,
) where
    A: Arena,
{
    let name = display_class_like_name(context, class_like_metadata.original_name);
    let interface_name = display_class_like_name(context, interface.original_name);
    let property_name = display_property_name(context, property.name.0);
    let property_span = property.name_span.unwrap_or(owner.span);
    let stated_span = stated.name_span.unwrap_or(interface.span);
    let (from, place) = if owner.name == class_like_metadata.name {
        (String::new(), String::new())
    } else {
        let owner_name = display_class_like_name(context, owner.original_name);

        (format!(", from `{owner_name}`"), format!(" in `{owner_name}`"))
    };
    let report = |context: &mut Context<'_, '_, A>, code: IssueCode, given: String, stated: String, help: String| {
        let issue = Issue::error(format!(
            "`{name}` gives `{property_name}` {given}{from}, and `{interface_name}` states it {stated}."
        ));
        let issue = if from.is_empty() {
            issue.with_annotation(Annotation::primary(property_span).with_message("Given here."))
        } else {
            issue
                .with_annotation(
                    Annotation::primary(class_like_metadata.name_span.unwrap_or(class_like_metadata.span))
                        .with_message("Inherited here."),
                )
                .with_annotation(Annotation::secondary(property_span).with_message("Declared here."))
        };

        context.collector.report_with_code(
            code,
            issue.with_annotation(Annotation::secondary(stated_span).with_message("Stated here.")).with_help(help),
        );
    };

    if property.read_visibility != stated.read_visibility {
        let (given, stated) = (property.read_visibility, stated.read_visibility);
        let help = format!("Declare `{property_name}` `{stated}`{place}.");
        report(context, IssueCode::IncompatiblePropertyAccess, format!("as `{given}`"), format!("`{stated}`"), help);
    }

    if property.flags.is_static() != stated.flags.is_static() {
        let (given, help) = ("as `static`".to_owned(), format!("Remove `static`{place}."));
        report(context, IssueCode::IncompatibleStaticModifier, given, "on each object".to_owned(), help);
    }

    // PHP refuses a readonly, a `private(set)` or a get-only virtual property for one stated with `set`, and an `init`
    // property is set only while its object is created.
    let set = word(b"set");
    let given_settable = !property.flags.is_readonly()
        && !property.flags.is_init()
        && property.write_visibility == Visibility::Public
        && (!property.flags.is_virtual_property() || property.hooks.contains_key(&set));
    if stated.hooks.contains_key(&set) && !given_settable {
        let never_set_again = if owner.flags.is_event() {
            Some("an event's properties are set once, by `new`")
        } else if owner.flags.is_struct() {
            Some("a struct never changes")
        } else {
            None
        };

        if let Some(reason) = never_set_again {
            context.collector.report_with_code(
                IssueCode::IncompatiblePropertyAccess,
                Issue::error(format!(
                    "`{name}` cannot give `{property_name}`, which `{interface_name}` states with `set`: {reason}."
                ))
                .with_annotation(Annotation::primary(property_span).with_message("Given here."))
                .with_annotation(Annotation::secondary(stated_span).with_message("Stated here.")),
            );
        } else {
            let given = if property.flags.is_init() {
                "with `init`".to_owned()
            } else if property.flags.is_readonly() || property.write_visibility == Visibility::Public {
                "get-only".to_owned()
            } else {
                format!("with `{} set`", property.write_visibility)
            };
            let help = format!("Declare `{property_name}` with `{{ get; set; }}`{place}.");
            report(context, IssueCode::IncompatiblePropertyAccess, given, "with `set`".to_owned(), help);
        }
    }

    if let Some((given, stated_type)) =
        property.type_declaration_metadata.as_ref().zip(stated.type_declaration_metadata.as_ref())
        && let stated_type =
            localize_parent_type(context.codebase, class_like_metadata, interface.name, &stated_type.type_union)
        && is_property_type_variance_invalid(
            context.codebase,
            &given.type_union,
            &stated_type,
            !stated.hooks.contains_key(&word("set")),
            false,
        )
    {
        let stated_type = display_type(context, &stated_type);
        let given = format!("as `{}`", display_type(context, &given.type_union));
        let help = format!("Give `{property_name}` the type `{stated_type}`{place}.");
        report(context, IssueCode::IncompatiblePropertyType, given, format!("as `{stated_type}`"), help);
    }
}

fn check_class_like_constants<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    members: &[ClassLikeMember<'arena>],
) where
    A: Arena,
{
    for member in members {
        let ClassLikeMember::Constant(constant) = member else {
            continue;
        };

        for item in &constant.items {
            let constant_name = word(item.name.value);

            let Some(trait_fqcn) = class_like_metadata.trait_constant_ids.get(&constant_name) else {
                continue;
            };

            let Some(trait_metadata) = context.codebase.get_class_like(trait_fqcn.as_bytes()) else {
                continue;
            };

            let Some(trait_constant) = trait_metadata.constants.get(&constant_name) else {
                continue;
            };
            let Some(class_constant) = class_like_metadata.constants.get(&constant_name) else {
                continue;
            };

            let value_matches = trait_constant.inferred_type == class_constant.inferred_type;
            let visibility_matches = trait_constant.visibility == class_constant.visibility;
            let finality_matches = trait_constant.flags.is_final() == class_constant.flags.is_final();

            if value_matches && visibility_matches && finality_matches {
                continue;
            }

            let class_name = class_like_metadata.original_name;
            let trait_name = trait_metadata.original_name;

            if !value_matches && visibility_matches && finality_matches {
                context.collector.report_with_code(
                    IssueCode::TraitConstantOverride,
                    Issue::error(format!(
                        "Class `{class_name}` cannot override constant `{constant_name}` from trait `{trait_name}` with a different value."
                    ))
                    .with_annotation(
                        Annotation::primary(item.name.span())
                            .with_message(format!("This constant has a different value than in trait `{trait_name}`")),
                    )
                    .with_note("PHP does not allow a class to override constants from traits it directly uses with a different value.")
                    .with_note(format!("Trait `{trait_name}` declares constant `{constant_name}`, which is inherited by `{class_name}`."))
                    .with_help(format!("Either use the same value as in the trait, remove the constant declaration from `{class_name}`, or remove the `use {trait_name}` statement.")),
                );
            } else {
                let mut conflicts = Vec::new();
                if !value_matches {
                    conflicts.push("value");
                }
                if !visibility_matches {
                    conflicts.push("visibility");
                }
                if !finality_matches {
                    conflicts.push("finality");
                }
                let conflicts_str = conflicts.join(", ");

                context.collector.report_with_code(
                    IssueCode::IncompatibleConstantOverride,
                    Issue::error(format!(
                        "{class_name} and {trait_name} define the same constant ({constant_name}) in the composition of {class_name}. However, the definition differs and is considered incompatible."
                    ))
                    .with_annotation(
                        Annotation::primary(item.name.span())
                            .with_message(format!("This constant differs from trait definition ({conflicts_str} differ)")),
                    )
                    .with_note(format!("Trait `{trait_name}` declares constant `{constant_name}`, which is inherited by `{class_name}`."))
                    .with_note("PHP requires that constants from traits match exactly in value, visibility, and finality when redeclared.")
                    .with_help(format!("Either match the trait's definition exactly, remove the constant declaration from `{class_name}`, or remove the `use {trait_name}` statement.")),
                );
            }
        }
    }

    for member in members {
        let ClassLikeMember::Constant(constant) = member else {
            continue;
        };

        for item in &constant.items {
            let constant_name = word(item.name.value);

            let Some(child_constant) = class_like_metadata.constants.get(&constant_name) else {
                continue;
            };

            for parent_fqcn in &class_like_metadata.all_parent_classes {
                let parent_fqcn_str = parent_fqcn.as_ref();
                let Some(parent_metadata) = context.codebase.get_class_like(parent_fqcn_str) else {
                    continue;
                };

                let Some(parent_constant) = parent_metadata.constants.get(&constant_name) else {
                    continue;
                };

                if parent_constant.flags.is_final() {
                    let child_span = item.name.span();
                    let parent_span = parent_constant.span;
                    let class_name = display_class_like_name(context, class_like_metadata.original_name);
                    let parent_class_name = display_class_like_name(context, parent_metadata.original_name);

                    context.collector.report_with_code(
                        IssueCode::OverrideFinalConstant,
                        Issue::error(format!(
                            "Class `{class_name}` cannot override final constant `{constant_name}` from parent class `{parent_class_name}`."
                        ))
                        .with_annotation(
                            Annotation::primary(child_span)
                                .with_message("This constant attempts to override a final constant"),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_span)
                                .with_message(format!("The constant is declared as final in parent class `{parent_class_name}` here")),
                        )
                        .with_note("PHP 8.1+ allows constants to be marked as final to prevent overriding in child classes.")
                        .with_help(format!("Remove this constant declaration from `{class_name}` or remove the final modifier from the parent constant.")),
                    );
                }

                if child_constant.visibility > parent_constant.visibility {
                    let child_span = item.name.span();
                    let parent_span = parent_constant.span;
                    let constant = display_member(context, class_like_metadata.original_name, constant_name);
                    let parent_class_name = display_class_like_name(context, parent_metadata.original_name);
                    let child_visibility = child_constant.visibility;
                    let parent_visibility = parent_constant.visibility;

                    context.collector.report_with_code(
                        IssueCode::IncompatibleConstantAccess,
                        Issue::error(format!("Constant `{constant}` has narrower visibility than parent constant."))
                        .with_annotation(
                            Annotation::primary(child_span)
                                .with_message(format!("This constant is declared as `{child_visibility}`, which is narrower than the parent's `{parent_visibility}`")),
                        )
                        .with_annotation(
                            Annotation::secondary(parent_span)
                                .with_message(format!("Parent constant is declared as `{parent_visibility}` in `{parent_class_name}` here")),
                        )
                        .with_note("PHP requires that overriding constants maintain or widen visibility (public → protected → private).")
                        .with_help(format!("Change the visibility of `{constant_name}` to at least `{parent_visibility}` to match the parent constant.")),
                    );
                }

                let (Some(child_type), Some(parent_type)) =
                    (&child_constant.type_declaration, &parent_constant.type_declaration)
                else {
                    continue;
                };

                if is_type_compatible(context.codebase, &child_type.type_union, &parent_type.type_union) {
                    continue;
                }
                let child_type_id = display_type(context, &child_type.type_union);
                let parent_type_id = display_type(context, &parent_type.type_union);
                let constant = display_member(context, class_like_metadata.original_name, constant_name);
                let parent_class_name = display_class_like_name(context, parent_metadata.original_name);

                context.collector.report_with_code(
                    IssueCode::IncompatibleConstantType,
                    Issue::error(format!("Constant `{constant}` has an incompatible type declaration."))
                    .with_annotation(
                        Annotation::primary(child_type.span)
                            .with_message(format!("This type `{child_type_id}` is not compatible with the parent's type")),
                    )
                    .with_annotation(
                        Annotation::secondary(parent_type.span)
                            .with_message(format!("The parent constant is defined with type `{parent_type_id}` in `{parent_class_name}` here")),
                    )
                    .with_note("PHP 8.3+ allows typed constants with covariance, meaning the child type must be a subtype of the parent type.")
                    .with_help(format!("Change the type of `{constant_name}` to be compatible with `{parent_type_id}`.")),
                );
            }
        }
    }

    for member in members {
        let ClassLikeMember::Constant(constant) = member else {
            continue;
        };

        for item in &constant.items {
            let constant_name = word(item.name.value);

            let Some(child_constant) = class_like_metadata.constants.get(&constant_name) else {
                continue;
            };

            for interface_fqcn in &class_like_metadata.all_parent_interfaces {
                let interface_fqcn_str = interface_fqcn.as_ref();
                let Some(interface_metadata) = context.codebase.get_class_like(interface_fqcn_str) else {
                    continue;
                };

                let Some(interface_constant) = interface_metadata.constants.get(&constant_name) else {
                    continue;
                };

                if child_constant.visibility != Visibility::Public {
                    let child_span = item.name.span();
                    let interface_span = interface_constant.span;
                    let constant = display_member(context, class_like_metadata.original_name, constant_name);
                    let interface_name = display_class_like_name(context, interface_metadata.original_name);
                    let child_visibility = child_constant.visibility;

                    context.collector.report_with_code(
                        IssueCode::IncompatibleConstantVisibility,
                        Issue::error(format!(
                            "Constant `{constant}` must be public to implement interface `{interface_name}`."
                        ))
                        .with_annotation(
                            Annotation::primary(child_span)
                                .with_message(format!("This constant is declared as `{child_visibility}`, but must be `public`")),
                        )
                        .with_annotation(
                            Annotation::secondary(interface_span)
                                .with_message(format!("Interface constant is declared in `{interface_name}` here")),
                        )
                        .with_note("All interface constants are implicitly public and implementing classes must maintain public visibility.")
                        .with_help(format!("Change the visibility of `{constant_name}` to `public`.")),
                    );
                }

                if let (Some(child_type), Some(interface_type)) =
                    (&child_constant.type_declaration, &interface_constant.type_declaration)
                    && !is_type_compatible(context.codebase, &child_type.type_union, &interface_type.type_union)
                {
                    let child_type_id = display_type(context, &child_type.type_union);
                    let interface_type_id = display_type(context, &interface_type.type_union);
                    let constant = display_member(context, class_like_metadata.original_name, constant_name);
                    let interface_name = display_class_like_name(context, interface_metadata.original_name);

                    context.collector.report_with_code(
                            IssueCode::IncompatibleConstantType,
                            Issue::error(format!("Constant `{constant}` has an incompatible type declaration."))
                            .with_annotation(
                                Annotation::primary(child_type.span)
                                    .with_message(format!("This type `{child_type_id}` is not compatible with the interface's type")),
                            )
                            .with_annotation(
                                Annotation::secondary(interface_type.span)
                                    .with_message(format!("The interface constant is defined with type `{interface_type_id}` in `{interface_name}` here")),
                            )
                            .with_note("Constants implementing interface constants must have compatible types (covariance allowed).")
                            .with_help(format!("Change the type of `{constant_name}` to be compatible with `{interface_type_id}`.")),
                        );
                }
            }
        }
    }
}

/// Check that a readonly class does not use traits with non-readonly properties.
///
/// In PHP, a readonly class can only use traits where all properties are declared readonly.
/// Using a trait with non-readonly properties in a readonly class causes a fatal error.
fn check_readonly_class_trait_properties<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    class_like_metadata: &'ctx ClassLikeMetadata,
    trait_uses: &[(Span, Vec<&Identifier<'arena>>)],
) where
    A: Arena,
{
    if !class_like_metadata.flags.is_readonly() {
        return;
    }

    for trait_name_id in trait_uses.iter().flat_map(|(_, trait_names)| trait_names) {
        let trait_fqcn = word(context.resolved_names.get(*trait_name_id));
        let properties =
            context.codebase.get_class_like(trait_fqcn.as_ref()).map(|trait_metadata| &trait_metadata.properties);

        for (property_name, property) in properties.into_iter().flatten() {
            if !property.flags.is_readonly() {
                context.collector.report_with_code(
                    IssueCode::InvalidTraitUse,
                    Issue::error(format!(
                        "Readonly class `{}` cannot use trait `{}` which has non-readonly property `{}`",
                        class_like_metadata.name, trait_fqcn, property_name
                    ))
                    .with_annotation(Annotation::primary(trait_name_id.span()).with_message("Trait used here"))
                    .with_note("All properties in a trait used by a readonly class must be declared readonly.")
                    .with_help(format!(
                        "Add the `readonly` modifier to property `{}` in trait `{}`.",
                        property_name, trait_fqcn
                    )),
                );
            }
        }
    }
}

/// Spec section 22 gives a class one method per name. Reports each method that two places give a PHP# class-like a
/// body for, unless the class writes the method itself: two of its header traits and the traits of its interfaces'
/// default bodies, which PHP refuses, or a default body and the base class, whose body PHP would replace. A header
/// trait replaces the base class's method, as PHP's `use` does. A computed property's default body beside another
/// default body, a header trait's property or the base class's property is reported alike.
fn check_members_given_twice<A>(
    context: &mut Context<'_, '_, A>,
    class_like_metadata: &ClassLikeMetadata,
    header_traits: &[Word],
    name_span: Span,
) where
    A: Arena,
{
    let mut traits: Vec<&ClassLikeMetadata> =
        header_traits.iter().filter_map(|header_trait| context.codebase.get_trait(header_trait.as_bytes())).collect();
    traits.extend(context.codebase.get_defaults_traits(class_like_metadata));
    traits.sort_unstable_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));

    let has_body = |class: &ClassLikeMetadata, method: Word| {
        class
            .declaring_method_ids
            .get(&method)
            .and_then(|method_id| context.codebase.get_method_by_id(method_id))
            .filter(|method| {
                method
                    .method_metadata
                    .as_ref()
                    .is_some_and(|method| !method.is_abstract && !method.visibility.is_private())
            })
    };
    let parent =
        class_like_metadata.direct_parent_class.and_then(|parent| context.codebase.get_class_like(parent.as_bytes()));

    let mut methods: Vec<Word> =
        traits.iter().flat_map(|trait_metadata| trait_metadata.declaring_method_ids.keys().copied()).collect();
    methods.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    methods.dedup();

    let class_name = display_class_like_name(context, class_like_metadata.original_name);
    for method in methods {
        if class_like_metadata.methods.contains(&method) {
            continue;
        }

        let mut bodies = Vec::new();
        let mut original_name = method;
        for giver in traits.iter().copied() {
            if let Some(body) = has_body(giver, method) {
                original_name = body.original_name;
                bodies.push(
                    context
                        .codebase
                        .get_defaults_interface(giver)
                        .map_or((giver, false), |interface| (interface, true)),
                );
            }
        }
        let gives_default = bodies.iter().any(|(_, default)| *default);
        // A sub-interface's default body replaces its parent's, as the most specific interface's does in Java, C# and
        // Kotlin.
        let mut givers: Vec<Word> = bodies
            .iter()
            .filter(|(giver, default)| {
                !*default
                    || !bodies.iter().any(|(other, other_default)| {
                        *other_default && other.all_parent_interfaces.contains(&giver.name)
                    })
            })
            .map(|(giver, _)| giver.original_name)
            .collect();
        // The base class's body counts only against a default body: a header trait replaces it, as PHP's `use` does.
        if gives_default && let Some(parent) = parent.filter(|parent| has_body(parent, method).is_some()) {
            givers.insert(0, parent.original_name);
        }

        let [first, second, ..] = givers.as_slice() else {
            continue;
        };
        let (first, second) = (display_class_like_name(context, *first), display_class_like_name(context, *second));
        context.collector.report_with_code(
            IssueCode::AmbiguousInheritedMember,
            Issue::error(format!(
                "`{class_name}` gets `{original_name}` from both `{first}` and `{second}`: write `{original_name}` in `{class_name}`."
            ))
            .with_annotation(Annotation::primary(name_span).with_message(format!("`{class_name}` has one `{original_name}`.")))
            .with_help(format!("Declare `{original_name}` in `{class_name}`, and call `super<{second}>.{original_name}()` in it to reuse one body.")),
        );
    }

    // A computed property's default body is a body too, and against one, a header trait's property and the base
    // class's property are each one.
    let mut properties: Vec<(Word, &ClassLikeMetadata, bool)> = traits
        .iter()
        .flat_map(|giver| {
            let interface = context.codebase.get_defaults_interface(giver);
            let owner = interface.unwrap_or(giver);
            owner
                .properties
                .iter()
                .filter(move |(_, property)| {
                    interface.is_none() || property.hooks.values().any(|accessor| !accessor.is_abstract)
                })
                .map(move |(name, _)| (*name, owner, interface.is_some()))
        })
        .collect();
    properties.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for given in properties.chunk_by(|left, right| left.0 == right.0) {
        let name = given[0].0;
        if !given.iter().any(|(_, _, default)| *default)
            || class_like_metadata.declaring_property_ids.get(&name) == Some(&class_like_metadata.name)
        {
            continue;
        }
        let property = mago_bytes::BytesDisplay(mago_bytes::trim_start_byte(name.as_bytes(), b'$'));
        // A sub-interface's default body replaces its parent's here too.
        let givers: Vec<Word> = parent
            .filter(|parent| parent.properties.contains_key(&name))
            .map(|parent| parent.original_name)
            .into_iter()
            .chain(
                given
                    .iter()
                    .filter(|(_, giver, default)| {
                        !*default
                            || !given.iter().any(|(_, other, other_default)| {
                                *other_default && other.all_parent_interfaces.contains(&giver.name)
                            })
                    })
                    .map(|(_, giver, _)| giver.original_name),
            )
            .collect();
        let [first, second, ..] = givers.as_slice() else {
            continue;
        };
        let (first, second) = (display_class_like_name(context, *first), display_class_like_name(context, *second));
        context.collector.report_with_code(
            IssueCode::AmbiguousInheritedMember,
            Issue::error(format!(
                "`{class_name}` gets `{property}` from both `{first}` and `{second}`: write `{property}` in `{class_name}`."
            ))
            .with_annotation(Annotation::primary(name_span).with_message(format!("`{class_name}` has one `{property}`.")))
            .with_help(format!("Declare `{property}` in `{class_name}`.")),
        );
    }
}

/// The fully qualified names of the traits in each group of `trait_uses`, beside the span the group's errors point at.
fn resolve_trait_uses<A>(
    context: &Context<'_, '_, A>,
    trait_uses: &[(Span, Vec<&Identifier<'_>>)],
) -> Vec<(Span, Vec<Word>)>
where
    A: Arena,
{
    trait_uses
        .iter()
        .map(|(span, trait_names)| {
            (*span, trait_names.iter().map(|trait_name| word(context.resolved_names.get(*trait_name))).collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = direct_interface_incompatible_parameter_name,
        code = "<?php
            interface Sizer {
                public function getSize(string $document): int;
            }
            class SizerImpl implements Sizer {
                public function getSize(string $file): int { return 100; }
            }
        ",
        issues = [IssueCode::IncompatibleParameterName],
    }

    test_analysis! {
        name = diamond_inheritance_incompatible_parameter_name,
        code = "<?php
            interface Logger {
                public function log(string $message): void;
            }
            interface FileLogger extends Logger {}
            interface DatabaseLogger extends Logger {}
            class CompositeLogger implements FileLogger, DatabaseLogger {
                public function log(string $entry): void {}
            }
        ",
        issues = [IssueCode::IncompatibleParameterName],
    }

    test_analysis! {
        name = indirect_interface_incompatible_parameter_name,
        code = "<?php
            interface Serializer {
                public function serialize(mixed $data): string;
            }
            interface JsonSerializer extends Serializer {}
            class DefaultJsonSerializer implements JsonSerializer {
                public function serialize(mixed $payload): string { return ''; }
            }
        ",
        issues = [IssueCode::IncompatibleParameterName],
    }
}
