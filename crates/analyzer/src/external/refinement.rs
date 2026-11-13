//! Declaration refinements returned by codebase-scan hooks.
//!
//! A refinement supplies what a declaration's native syntax cannot state — generic
//! parameters, applied ancestor arguments, richer member types, and the receiver a closure
//! runs with — while leaving the native declaration in place, so an incompatible refinement
//! is still diagnosed. Refinements are installed into each file's partial metadata before the
//! codebase is merged and populated, so inheritance, body analysis, and call sites all see them.

use foldhash::HashMap;

use mago_codex::build_synthetic_name;
use mago_codex::get_anonymous_class_name;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::ttype::HostRefinement;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::scanner::merge_type_preserving_nullability;
use mago_codex::ttype::TType;
use mago_codex::ttype::is_written_in_sharp;
use mago_codex::ttype::template::GenericTemplate;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_extension::PayloadReader;
use mago_names::display_sharp_member;
use mago_names::short_name;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_reporting::Level;
use mago_span::Position;
use mago_span::Span;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::external::error::ExternalAnalyzerError;
use crate::external::error::protocol;
use crate::external::protocol::decode_refined_type;

const MAXIMUM_REFINEMENTS: usize = 1_000_000;
const MAXIMUM_MEMBERS: usize = 0x0001_0000;
const CLASS_LIKE: u8 = 1;
const FUNCTION_LIKE: u8 = 2;
// The scanner's own codes for the docblock tags a refinement stands in for, so one suppression
// covers a conflict however it was declared.
const INVALID_TEMPLATE_TAG: &str = "invalid-template-tag";
const INVALID_EXTENDS_TAG: &str = "invalid-extends-tag";
const DOCBLOCK_TYPE_MISMATCH: &str = "docblock-type-mismatch";
// The plugin's own code for a declaration it cannot state, reused for a body it cannot resolve.
const UNRESOLVED_BODY_RETURN: &str = "unsupported-type";

/// One declaration's refinement, located in the file that declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclarationRefinement {
    file_id: FileId,
    /// The plugin of the host that refines the declaration.
    host: Word,
    declaration: Declaration,
    issues: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq)]
enum Declaration {
    /// A named class-like by its name as written, which `apply` keys, or an anonymous class by the
    /// synthetic name its declaration offset gives it.
    ClassLike {
        class: Word,
        templates: Vec<RefinedTemplate>,
        inherited: Vec<InheritedApplication>,
        /// The classes every user of a trait must extend, as `@require-extends` states them.
        required_extends: Vec<Word>,
        properties: Vec<(Word, RefinedType)>,
        methods: Vec<(Word, SignatureRefinement)>,
    },
    /// A named function by its lowercase name, or a closure or arrow function by the synthetic
    /// name its declaration offset gives it.
    FunctionLike { key: (Word, Word), signature: Box<SignatureRefinement> },
}

#[derive(Debug, Clone, PartialEq)]
struct RefinedType {
    type_union: TUnion,
    span: Span,
}

#[derive(Debug, Clone, PartialEq)]
struct RefinedTemplate {
    name: Word,
    bound: RefinedType,
    /// How the declaring class's members use the parameter; a function's own parameters are invariant.
    variance: Variance,
}

#[derive(Debug, Clone, PartialEq)]
struct InheritedApplication {
    ancestor: Word,
    span: Span,
    arguments: Vec<TUnion>,
}

#[derive(Debug, Clone, PartialEq)]
struct SignatureRefinement {
    templates: Vec<RefinedTemplate>,
    parameters: Vec<(Word, RefinedType)>,
    closure_this: Vec<(Word, RefinedType)>,
    return_type: Option<RefinedType>,
    this: Option<RefinedType>,
    /// The issue reported when the return this signature takes from its body stays unresolved.
    return_from_body: Option<Issue>,
}

/// Decodes the refinements one codebase-scan response carries.
///
/// `plugin_of_hook` names the plugin that registered each hook, so reported issue codes
/// carry their plugin's identifier exactly as lifecycle issues do.
pub(super) fn decode(
    reader: &mut PayloadReader<'_>,
    files: &HashMap<&[u8], &File>,
    plugin_of_hook: &dyn Fn(u16) -> Option<String>,
) -> Result<Vec<DeclarationRefinement>, ExternalAnalyzerError> {
    let count = reader.read_count("declaration refinements", MAXIMUM_REFINEMENTS)?;
    let mut refinements = Vec::with_capacity(count);
    for _ in 0..count {
        let hook = reader.read_u16("refinement hook index")?;
        let plugin = plugin_of_hook(hook)
            .ok_or_else(|| protocol(format!("a declaration refinement names inactive hook {hook}")))?;
        let name = reader.read_bytes("refinement file")?;
        let file = files.get(name).ok_or_else(|| {
            protocol(format!(
                "a declaration refinement names `{}`, which was not selected for this scan",
                String::from_utf8_lossy(name)
            ))
        })?;

        let declaration = match reader.read_u8("refinement declaration kind")? {
            // The class-likes a refinement names stay as written, and `apply` keys each in the codebase, spec section
            // 24, and each method by its class.
            CLASS_LIKE => {
                let class = if reader.read_bool("anonymous class refinement")? {
                    get_anonymous_class_name(file, read_offset(reader, file)?)
                } else {
                    let class = reader.read_bytes("refinement class")?;
                    if class.is_empty() {
                        return Err(protocol("a declaration refinement names an empty class"));
                    }
                    word(class)
                };

                let templates = read_templates(reader, file)?;
                let inherited_count = reader.read_count("inherited applications", MAXIMUM_MEMBERS)?;
                let mut inherited = Vec::with_capacity(inherited_count);
                for _ in 0..inherited_count {
                    let ancestor = word(reader.read_bytes("inherited ancestor")?);
                    let span = read_span(reader, file)?;
                    let argument_count = reader.read_count("inherited arguments", MAXIMUM_MEMBERS)?;
                    let mut arguments = Vec::with_capacity(argument_count);
                    for _ in 0..argument_count {
                        arguments.push(decode_refined_type(reader)?);
                    }
                    inherited.push(InheritedApplication { ancestor, span, arguments });
                }

                let required_count = reader.read_count("required parents", MAXIMUM_MEMBERS)?;
                let mut required_extends = Vec::with_capacity(required_count);
                for _ in 0..required_count {
                    required_extends.push(word(reader.read_bytes("required parent")?));
                }

                let properties = read_members(reader, file, "refined properties")?;
                let method_count = reader.read_count("refined methods", MAXIMUM_MEMBERS)?;
                let mut methods = Vec::with_capacity(method_count);
                for _ in 0..method_count {
                    let name = word(reader.read_bytes("refined method")?);
                    methods.push((name, read_signature(reader, file, &plugin)?));
                }

                Declaration::ClassLike { class, templates, inherited, required_extends, properties, methods }
            }
            FUNCTION_LIKE => {
                let name = if reader.read_bool("anonymous function refinement")? {
                    build_synthetic_name("closure", file, read_offset(reader, file)?)
                } else {
                    let function = reader.read_bytes("refinement function")?;
                    if function.is_empty() {
                        return Err(protocol("a declaration refinement names an empty function"));
                    }
                    ascii_lowercase_word(function)
                };

                Declaration::FunctionLike {
                    key: (empty_word(), name),
                    signature: Box::new(read_signature(reader, file, &plugin)?),
                }
            }
            kind => return Err(protocol(format!("invalid declaration refinement kind {kind}"))),
        };

        let issue_count = reader.read_count("refinement issues", MAXIMUM_MEMBERS)?;
        let mut issues = Vec::with_capacity(issue_count);
        for _ in 0..issue_count {
            let level = match reader.read_u8("refinement issue level")? {
                1 => Level::Note,
                2 => Level::Help,
                3 => Level::Warning,
                4 => Level::Error,
                value => return Err(protocol(format!("invalid refinement issue level {value}"))),
            };
            let code = reader.read_string("refinement issue code")?;
            let message = reader.read_string("refinement issue message")?;
            if code.is_empty() || message.is_empty() {
                return Err(protocol(format!(
                    "plugin `{plugin}` reported a refinement issue without a code or message"
                )));
            }
            let span = read_span(reader, file)?;
            issues.push(
                Issue::new(level, message)
                    .with_code(format!("{plugin}/{code}"))
                    .with_annotation(Annotation::primary(span)),
            );
        }

        refinements.push(DeclarationRefinement {
            file_id: file.id,
            host: word(plugin.as_bytes()),
            declaration,
            issues,
        });
    }

    Ok(refinements)
}

/// Installs every refinement into the partial metadata of the file that declares it.
///
/// # Errors
///
/// Returns an error when a refinement names a declaration its file does not contain, because
/// an adapter describing code that is not there has failed and analysis must not proceed as if
/// the refinement applied.
pub fn apply_refinements<'codebase>(
    refinements: Vec<DeclarationRefinement>,
    partials: impl IntoIterator<Item = (FileId, &'codebase mut CodebaseMetadata)>,
) -> Result<(), ExternalAnalyzerError> {
    if refinements.is_empty() {
        return Ok(());
    }

    let mut partials = partials.into_iter().collect::<HashMap<_, _>>();
    for refinement in refinements {
        let Some(codebase) = partials.get_mut(&refinement.file_id) else {
            continue;
        };

        refinement.apply(codebase)?;
    }

    Ok(())
}

impl DeclarationRefinement {
    fn apply(self, codebase: &mut CodebaseMetadata) -> Result<(), ExternalAnalyzerError> {
        match self.declaration {
            Declaration::ClassLike { class, templates, inherited, required_extends, properties, methods } => {
                apply_class_like(
                    codebase,
                    self.host,
                    class,
                    templates,
                    inherited,
                    required_extends,
                    properties,
                    methods,
                    self.issues,
                )
            }
            Declaration::FunctionLike { key, signature } => {
                let function_like = codebase.function_likes.get_mut(&key).ok_or_else(|| {
                    protocol(format!(
                        "a declaration refinement names function `{}`, which its file does not declare",
                        key.1
                    ))
                })?;

                function_like.issues.extend(self.issues);
                apply_signature(function_like, self.host, key, *signature)
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_class_like(
    codebase: &mut CodebaseMetadata,
    host: Word,
    class_name: Word,
    templates: Vec<RefinedTemplate>,
    inherited: Vec<InheritedApplication>,
    required_extends: Vec<Word>,
    properties: Vec<(Word, RefinedType)>,
    methods: Vec<(Word, SignatureRefinement)>,
    issues: Vec<Issue>,
) -> Result<(), ExternalAnalyzerError> {
    // A refinement runs on its file's codebase before population, so the file keys its own class-like, and the names
    // of the class-like's header stay as written until the populator keys them, as the ancestors here do.
    let class_name = codebase.class_key(class_name.as_bytes());
    let Some(class) = codebase.class_likes.get_mut(&class_name) else {
        return Err(protocol(format!(
            "a declaration refinement names class `{class_name}`, which its file does not declare"
        )));
    };
    let methods: Vec<_> =
        methods.into_iter().map(|(method, signature)| (class.method_key(method.as_bytes()), signature)).collect();

    class.issues.extend(issues);
    let sharp = class.flags.is_sharp();
    let shown = short_name(class.original_name.as_bytes());
    let templates = keep_written(sharp, templates, &mut class.issues, |template| {
        let part = format!("the bound of `{shown}`'s type parameter `{}`", template.name);
        unwritten(host, &part, &template.bound.type_union, template.bound.span, INVALID_TEMPLATE_TAG)
    });
    let inherited = keep_written(sharp, inherited, &mut class.issues, |application| {
        let part = format!("a type argument `{shown}` gives its parent");
        application
            .arguments
            .iter()
            .find_map(|argument| unwritten(host, &part, argument, application.span, INVALID_EXTENDS_TAG))
    });
    let mut class_templates = Vec::with_capacity(templates.len());
    if !templates.is_empty() {
        if class.template_types.is_empty() {
            let mut variance = std::mem::take(&mut class.template_variance);
            for template in templates {
                let definition = GenericTemplate::new(GenericParent::ClassLike(class_name), template.bound.type_union);
                class.add_template_type(template.name, definition.clone());
                variance.push(template.variance);
                class_templates.push((template.name, definition));
            }
            class.set_template_variance(variance);
        } else {
            for template in &templates {
                class.issues.push(
                    Issue::error(
                        "A class cannot declare type parameters in both a docblock and its declaration refinement.",
                    )
                    .with_code(INVALID_TEMPLATE_TAG)
                    .with_annotation(Annotation::primary(template.bound.span).with_message(format!(
                        "`{}` is declared here while the class already declares `@template` parameters.",
                        template.name
                    )))
                    .with_help("Declare the class's type parameters in one place."),
                );
            }
        }
    }

    for application in inherited {
        apply_inherited(class, application);
    }

    if !required_extends.is_empty() && !class.kind.is_trait() {
        return Err(protocol(format!(
            "a declaration refinement requires a parent of `{class_name}`, which is no trait"
        )));
    }
    class.require_extends.extend(required_extends);

    for (name, refined) in properties {
        let Some(property) = class.properties.get_mut(&name) else {
            return Err(protocol(format!(
                "a declaration refinement names property `{class_name}::{name}`, which is not declared"
            )));
        };

        // A PHP# property declares its type, unless `Mago\UntypedSince` or `Mago\UntypedUntil` drops it for the
        // project's PHP version.
        match member_type(sharp, host, refined, property.type_declaration_metadata.as_ref()) {
            Ok(refined) => property.set_type_metadata(Some(refined)),
            Err(refused) => {
                let part = format!("`{}`", display_sharp_member(&shown, name));
                class.issues.push(refusal(host, &part, &refused.type_union, refused.span, DOCBLOCK_TYPE_MISMATCH));
            }
        }
    }

    let method_names = class.methods.clone();
    if !class_templates.is_empty() {
        for method_name in &method_names {
            let Some(function_like) = codebase.function_likes.get_mut(&(class_name, *method_name)) else {
                continue;
            };

            let mut context = function_like.type_resolution_context.clone().unwrap_or_default();
            for (template_name, definition) in &class_templates {
                context = context.with_template_definition(*template_name, vec![definition.clone()]);
            }
            function_like.type_resolution_context = Some(context);
        }
    }

    for (method, signature) in methods {
        let function_like = method_names
            .contains(&method)
            .then(|| codebase.function_likes.get_mut(&(class_name, method)))
            .flatten()
            .ok_or_else(|| {
                protocol(format!(
                    "a declaration refinement names method `{class_name}::{method}`, which is not declared"
                ))
            })?;

        apply_signature(function_like, host, (class_name, method), signature)?;
    }

    Ok(())
}

/// Installs a function-like's own type parameters, parameter and return types, the receiver a
/// closure passed to a parameter runs with, and a closure's own `$this`.
fn apply_signature(
    function_like: &mut FunctionLikeMetadata,
    host: Word,
    key: (Word, Word),
    signature: SignatureRefinement,
) -> Result<(), ExternalAnalyzerError> {
    let display = if key.0.is_empty() { key.1.to_string() } else { format!("{}::{}", key.0, key.1) };
    let sharp = function_like.flags.is_sharp();
    let shown = if function_like.kind.is_closure() || function_like.kind.is_arrow_function() {
        "a lambda".to_owned()
    } else {
        format!("`{}`", function_like.original_name)
    };

    let templates = keep_written(sharp, signature.templates, &mut function_like.issues, |template| {
        let part = format!("the bound of {shown}'s type parameter `{}`", template.name);
        unwritten(host, &part, &template.bound.type_union, template.bound.span, INVALID_TEMPLATE_TAG)
    });
    if !templates.is_empty() {
        let mut context = function_like.type_resolution_context.clone().unwrap_or_default();
        for template in templates {
            let definition = GenericTemplate::new(GenericParent::FunctionLike(key), template.bound.type_union);
            function_like.add_template_type(template.name, definition.clone());
            context = context.with_template_definition(template.name, vec![definition]);
        }
        function_like.type_resolution_context = Some(context);
    }

    for (name, refined) in signature.parameters {
        let parameter = function_like.get_parameter_mut(name).ok_or_else(|| {
            protocol(format!("a declaration refinement names parameter `{name}` of `{display}`, which is not declared"))
        })?;
        match member_type(sharp, host, refined, parameter.type_declaration_metadata.as_ref()) {
            Ok(refined) => parameter.set_type_metadata(Some(refined)),
            Err(refused) => {
                let part = format!("the parameter `{}` of {shown}", name.as_str_lossy().trim_start_matches('$'));
                let issue = refusal(host, &part, &refused.type_union, refused.span, DOCBLOCK_TYPE_MISMATCH);
                function_like.issues.push(issue);
            }
        }
    }

    let closure_this = keep_written(sharp, signature.closure_this, &mut function_like.issues, |(name, refined)| {
        let part = format!(
            "the `this` of a lambda given to the parameter `{}` of {shown}",
            name.as_str_lossy().trim_start_matches('$')
        );
        unwritten(host, &part, &refined.type_union, refined.span, DOCBLOCK_TYPE_MISMATCH)
    });
    for (name, refined) in closure_this {
        let parameter = function_like.get_parameter_mut(name).ok_or_else(|| {
            protocol(format!(
                "a declaration refinement binds `$this` for parameter `{name}` of `{display}`, which is not declared"
            ))
        })?;
        parameter.closure_this_type = Some(TypeMetadata::from_docblock(refined.type_union, refined.span));
    }

    if let Some(refined) = signature.return_type {
        match member_type(sharp, host, refined, function_like.return_type_declaration_metadata.as_ref()) {
            Ok(refined) => function_like.return_type_metadata = Some(refined),
            Err(refused) => {
                let part = format!("the result of {shown}");
                let issue = refusal(host, &part, &refused.type_union, refused.span, DOCBLOCK_TYPE_MISMATCH);
                function_like.issues.push(issue);
            }
        }
    }

    if let Some(issue) = signature.return_from_body {
        if !function_like.kind.is_method() {
            return Err(protocol(format!(
                "a declaration refinement takes the return of `{display}` from its body, which is no method"
            )));
        }
        let span = function_like.name_span.unwrap_or(function_like.span);
        function_like.return_from_body = Some(
            issue
                .with_annotation(Annotation::primary(span))
                .with_help("Declare the relationship with #[Type(of: ...)] when its body cannot be read."),
        );
    }

    if let Some(refined) = signature.this {
        if !function_like.kind.is_closure() && !function_like.kind.is_arrow_function() {
            return Err(protocol(format!(
                "a declaration refinement binds `$this` for `{display}`, which is no closure"
            )));
        }
        let refused = unwritten(host, "a lambda's `this`", &refined.type_union, refined.span, DOCBLOCK_TYPE_MISMATCH);
        match refused.filter(|_| sharp) {
            Some(issue) => function_like.issues.push(issue),
            None => {
                function_like.refined_this_type = Some(TypeMetadata::from_docblock(refined.type_union, refined.span))
            }
        }
    }

    Ok(())
}

/// The type a member takes from `refined`, the refinement `host` gives it, nullable where `declared`, the type the
/// member declares, is. A member of a PHP# declaration, `sharp`, takes only a type PHP# can write: otherwise it keeps
/// `declared`, which records the refused type, so the analysis reports it at the member. A PHP# member that declares
/// no type keeps none, and the refinement comes back as the error.
fn member_type(
    sharp: bool,
    host: Word,
    refined: RefinedType,
    declared: Option<&TypeMetadata>,
) -> Result<TypeMetadata, RefinedType> {
    let (mut member, refused) = match declared {
        _ if !sharp || is_written_in_sharp(&refined.type_union) => (
            merge_type_preserving_nullability(TypeMetadata::from_docblock(refined.type_union, refined.span), declared),
            None,
        ),
        Some(declared) => (declared.clone(), Some(refined.type_union)),
        None => return Err(refined),
    };
    member.host_refinement = Some(Box::new(HostRefinement { host, refused }));

    Ok(member)
}

/// The parts of a declaration's refinement it keeps: all of them for plain PHP, and for PHP#, `sharp`, each one
/// `refusal` finds no error in. Each error goes in `issues`.
fn keep_written<T>(
    sharp: bool,
    parts: Vec<T>,
    issues: &mut Vec<Issue>,
    refusal: impl Fn(&T) -> Option<Issue>,
) -> Vec<T> {
    if !sharp {
        return parts;
    }

    parts
        .into_iter()
        .filter(|part| {
            let Some(issue) = refusal(part) else {
                return true;
            };
            issues.push(issue);
            false
        })
        .collect()
}

/// [`refusal`] of `refined`, at `span`, when PHP# can't write it.
fn unwritten(host: Word, part: &str, refined: &TUnion, span: Span, code: &str) -> Option<Issue> {
    (!is_written_in_sharp(refined)).then(|| refusal(host, part, refined, span, code))
}

/// The error for `refused`, a type PHP# can't write, that `host` gives `part` of a PHP# declaration at `span`, as "the
/// result of `merged`". The declaration keeps no refinement of that part.
fn refusal(host: Word, part: &str, refused: &TUnion, span: Span, code: &str) -> Issue {
    Issue::error(format!("The host `{host}` refines {part} to `{}`, which PHP# can't write.", refused.get_id()))
        .with_code(code)
        .with_annotation(Annotation::primary(span).with_message("The host refines it here."))
        .with_help("A host refines a PHP# declaration only to a type PHP# can write.")
}

/// Applies `application`'s arguments to the direct parent it names, in any case, as a docblock's `@extends` does: the
/// header's names stay as written until the populator keys them, spec section 24.
fn apply_inherited(class: &mut ClassLikeMetadata, application: InheritedApplication) {
    let names = |name: &Word| name.as_bytes().eq_ignore_ascii_case(application.ancestor.as_bytes());
    let parent_interface = class.direct_parent_interfaces.iter().find(|name| names(name)).copied();
    let parent_class = class.direct_parent_class.filter(names);
    let ancestor = parent_class.or(parent_interface).unwrap_or(application.ancestor);
    let is_parent_interface = parent_interface.is_some();
    let counts = if parent_class.is_some() || (class.kind.is_interface() && is_parent_interface) {
        &mut class.template_type_extends_count
    } else if is_parent_interface {
        &mut class.template_type_implements_count
    } else {
        class.issues.push(
            Issue::error("A declaration refinement can only apply arguments to a direct parent class or interface.")
                .with_code(INVALID_EXTENDS_TAG)
                .with_annotation(
                    Annotation::primary(application.span)
                        .with_message(format!("`{ancestor}` is not a direct parent of this declaration.")),
                ),
        );
        return;
    };

    counts.insert(ancestor, application.arguments.len());
    if class.add_template_extended_offset(ancestor, application.arguments).is_some() {
        class.issues.push(
            Issue::error("A parent's arguments are applied twice.").with_code(INVALID_EXTENDS_TAG).with_annotation(
                Annotation::primary(application.span)
                    .with_message(format!("`{ancestor}` already receives arguments from a docblock tag.")),
            ),
        );
    }
}

fn read_signature(
    reader: &mut PayloadReader<'_>,
    file: &File,
    plugin: &str,
) -> Result<SignatureRefinement, ExternalAnalyzerError> {
    let templates = read_templates(reader, file)?;
    let parameters = read_members(reader, file, "refined parameters")?;
    let closure_this = read_members(reader, file, "refined closure receivers")?;
    let return_type =
        if reader.read_bool("refined return presence")? { Some(read_refined_type(reader, file)?) } else { None };
    let this =
        if reader.read_bool("refined receiver presence")? { Some(read_refined_type(reader, file)?) } else { None };
    let return_from_body = reader.read_bool("return taken from body")?.then(|| {
        Issue::error("The type this method returns cannot be resolved from its body.")
            .with_code(format!("{plugin}/{UNRESOLVED_BODY_RETURN}"))
    });

    Ok(SignatureRefinement { templates, parameters, closure_this, return_type, this, return_from_body })
}

fn read_members(
    reader: &mut PayloadReader<'_>,
    file: &File,
    what: &'static str,
) -> Result<Vec<(Word, RefinedType)>, ExternalAnalyzerError> {
    let count = reader.read_count(what, MAXIMUM_MEMBERS)?;
    let mut members = Vec::with_capacity(count);
    for _ in 0..count {
        members.push((variable(reader.read_bytes(what)?)?, read_refined_type(reader, file)?));
    }

    Ok(members)
}

fn read_templates(reader: &mut PayloadReader<'_>, file: &File) -> Result<Vec<RefinedTemplate>, ExternalAnalyzerError> {
    let count = reader.read_count("refined type parameters", MAXIMUM_MEMBERS)?;
    let mut templates = Vec::with_capacity(count);
    for _ in 0..count {
        let name = reader.read_bytes("refined type parameter")?;
        if name.is_empty() {
            return Err(protocol("a declaration refinement declares an unnamed type parameter"));
        }
        let bound = read_refined_type(reader, file)?;
        let variance = match reader.read_u8("refined type parameter variance")? {
            1 => Variance::Invariant,
            2 => Variance::Covariant,
            3 => Variance::Contravariant,
            unknown => {
                return Err(protocol(format!("a declaration refinement declares the unknown variance {unknown}")));
            }
        };
        templates.push(RefinedTemplate { name: word(name), bound, variance });
    }

    Ok(templates)
}

fn read_refined_type(reader: &mut PayloadReader<'_>, file: &File) -> Result<RefinedType, ExternalAnalyzerError> {
    let type_union = decode_refined_type(reader)?;
    Ok(RefinedType { type_union, span: read_span(reader, file)? })
}

/// An anonymous declaration is named by where it starts, exactly as the scanner names it.
fn read_offset(reader: &mut PayloadReader<'_>, file: &File) -> Result<Span, ExternalAnalyzerError> {
    let offset = reader.read_u32("anonymous declaration offset")?;
    if offset > file.size {
        return Err(protocol(format!(
            "a declaration refinement names an anonymous declaration at invalid offset {offset}"
        )));
    }

    Ok(Span::new(file.id, Position::new(offset), Position::new(offset)))
}

fn read_span(reader: &mut PayloadReader<'_>, file: &File) -> Result<Span, ExternalAnalyzerError> {
    let start = reader.read_u32("refinement span start")?;
    let end = reader.read_u32("refinement span end")?;
    if start > end || end > file.size {
        return Err(protocol(format!("a declaration refinement reports invalid span {start}..{end}")));
    }

    Ok(Span::new(file.id, Position::new(start), Position::new(end)))
}

fn variable(name: &[u8]) -> Result<Word, ExternalAnalyzerError> {
    if name.is_empty() {
        return Err(protocol("a declaration refinement names an unnamed member"));
    }

    let mut variable = Vec::with_capacity(name.len() + 1);
    variable.push(b'$');
    variable.extend_from_slice(name);
    Ok(word(&variable))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_codex::scanner::scan_program;
    use mago_codex::ttype::get_arraykey;
    use mago_codex::ttype::get_named_object;
    use mago_codex::ttype::get_string;
    use mago_names::resolver::NameResolver;
    use mago_syntax::parser::parse_file;

    use super::*;
    use crate::settings::Settings;

    /// A PHP# file whose class takes a type parameter and a parent's type argument, holds a property `Mago\UntypedSince`
    /// leaves untyped, whose methods take a lambda, a type parameter of their own and a value, and which declares a
    /// lambda with no types.
    const NOTICE: &str = "namespace Proof;\n\npublic class Base<T> { }\n\npublic class Notice<T> : Base<string>\n{\n    [\\Mago\\UntypedSince(80000)]\n    public string? label { get; set; } = null;\n\n    public void each(Function<void()> run) { }\n\n    public U pick<U>(U item) => item;\n\n    public void run()\n    {\n        const echo = (value) => value;\n    }\n}\n";

    /// A refinement by the host `typing` of `NOTICE`'s `declaration`.
    fn refinement(file: &File, declaration: Declaration) -> DeclarationRefinement {
        DeclarationRefinement { file_id: file.id, host: word("typing"), declaration, issues: Vec::new() }
    }

    /// `array-key`, which PHP# can't write, at the start of `file`.
    fn unwritable(file: &File) -> RefinedType {
        RefinedType { type_union: get_arraykey(), span: Span::new(file.id, Position::new(0), Position::new(0)) }
    }

    /// `type_union`, a type PHP# writes, with no place in a file.
    fn written(type_union: TUnion) -> RefinedType {
        RefinedType { type_union, span: Span::dummy(0, 0) }
    }

    fn signature() -> SignatureRefinement {
        SignatureRefinement {
            templates: Vec::new(),
            parameters: Vec::new(),
            closure_this: Vec::new(),
            return_type: None,
            this: None,
            return_from_body: None,
        }
    }

    /// Spec section 1: a host refines each part of a PHP# declaration only to a type PHP# can write, where the
    /// refinement enters the codebase: a type parameter's bound, a parent's type argument, the `this` of a lambda a
    /// parameter takes, a lambda's own `this`, parameter and result, which declare no type here, and a property
    /// `Mago\UntypedSince` leaves untyped. Each refused part
    /// keeps no refinement, and its error names the host and the part. A type parameter's bound and a lambda's `this`
    /// PHP# writes apply.
    #[test]
    fn a_host_refines_each_part_of_a_sharp_declaration_only_to_a_type_sharp_writes() {
        let file = File::ephemeral(Cow::Borrowed(b"src/Proof/Notice.sharp"), Cow::Borrowed(NOTICE.as_bytes()));
        let arena = LocalArena::new();
        let program = parse_file(&arena, &file);
        let names = NameResolver::new(&arena).resolve(program);
        let mut codebase = scan_program(&arena, &file, program, &names, Settings::default().version);
        let notice = word("Proof\\Notice");
        let lambda_key = *codebase
            .function_likes
            .iter()
            .find_map(|(key, function)| function.kind.is_arrow_function().then_some(key))
            .expect("the file declares a lambda");
        let template =
            |name: &str| RefinedTemplate { name: word(name), bound: unwritable(&file), variance: Variance::Invariant };

        let class = Declaration::ClassLike {
            class: notice,
            templates: vec![template("T")],
            inherited: vec![InheritedApplication {
                ancestor: word("Proof\\Base"),
                span: unwritable(&file).span,
                arguments: vec![get_arraykey()],
            }],
            required_extends: Vec::new(),
            properties: vec![(word("$label"), unwritable(&file))],
            methods: vec![
                (
                    word("each"),
                    SignatureRefinement { closure_this: vec![(word("$run"), unwritable(&file))], ..signature() },
                ),
                (
                    word("pick"),
                    SignatureRefinement {
                        templates: vec![
                            template("U"),
                            RefinedTemplate {
                                name: word("V"),
                                bound: written(get_string()),
                                variance: Variance::Invariant,
                            },
                        ],
                        ..signature()
                    },
                ),
            ],
        };
        let lambda_refinement = Declaration::FunctionLike {
            key: lambda_key,
            signature: Box::new(SignatureRefinement {
                parameters: vec![(word("$value"), unwritable(&file))],
                return_type: Some(unwritable(&file)),
                this: Some(unwritable(&file)),
                ..signature()
            }),
        };
        refinement(&file, class).apply(&mut codebase).expect("the class refinement applies");
        refinement(&file, lambda_refinement).apply(&mut codebase).expect("the lambda refinement applies");

        let messages = |issues: &[Issue]| issues.iter().map(|issue| issue.message.clone()).collect::<Vec<_>>();
        let class = codebase.class_likes.get(&notice).expect("the file declares `Notice`");
        let each = codebase.function_likes.get(&(notice, word("each"))).expect("`Notice` declares `each`");
        let pick = codebase.function_likes.get(&(notice, word("pick"))).expect("`Notice` declares `pick`");
        let lambda = codebase.function_likes.get(&lambda_key).expect("the file declares a lambda");

        assert_eq!(
            messages(&class.issues),
            [
                "The host `typing` refines the bound of `Notice`'s type parameter `T` to `array-key`, which PHP# can't write.",
                "The host `typing` refines a type argument `Notice` gives its parent to `array-key`, which PHP# can't write.",
                "The host `typing` refines `Notice.label` to `array-key`, which PHP# can't write.",
            ]
        );
        assert_eq!(
            messages(&each.issues),
            [
                "The host `typing` refines the `this` of a lambda given to the parameter `run` of `each` to `array-key`, which PHP# can't write."
            ]
        );
        assert_eq!(
            messages(&pick.issues),
            [
                "The host `typing` refines the bound of `pick`'s type parameter `U` to `array-key`, which PHP# can't write."
            ]
        );
        assert_eq!(
            messages(&lambda.issues),
            [
                "The host `typing` refines the parameter `value` of a lambda to `array-key`, which PHP# can't write.",
                "The host `typing` refines the result of a lambda to `array-key`, which PHP# can't write.",
                "The host `typing` refines a lambda's `this` to `array-key`, which PHP# can't write.",
            ]
        );
        assert!(
            class.template_types.values().all(|template| template.constraint != get_arraykey()),
            "{:?}",
            class.template_types
        );
        assert_eq!(pick.template_types.get(&word("V")).map(|template| &template.constraint), Some(&get_string()));
        assert!(lambda.refined_this_type.is_none());
        assert!(lambda.return_type_metadata.is_none());

        let this = Box::new(SignatureRefinement { this: Some(written(get_named_object(notice, None))), ..signature() });
        refinement(&file, Declaration::FunctionLike { key: lambda_key, signature: this })
            .apply(&mut codebase)
            .expect("the lambda refinement applies");
        let lambda = codebase.function_likes.get(&lambda_key).expect("the file declares a lambda");

        assert_eq!(
            lambda.refined_this_type.as_ref().map(|this| &this.type_union),
            Some(&get_named_object(notice, None))
        );
    }
}
