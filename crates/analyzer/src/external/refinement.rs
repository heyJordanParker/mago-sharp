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
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::scanner::merge_type_preserving_nullability;
use mago_codex::ttype::template::GenericTemplate;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_extension::PayloadReader;
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
// The plugin's own code for a declaration it cannot state, reused for a body it cannot resolve.
const UNRESOLVED_BODY_RETURN: &str = "unsupported-type";

/// One declaration's refinement, located in the file that declares it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclarationRefinement {
    file_id: FileId,
    declaration: Declaration,
    issues: Vec<Issue>,
}

#[derive(Debug, Clone, PartialEq)]
enum Declaration {
    /// A named class-like by its lowercase name, or an anonymous class by the synthetic name its
    /// declaration offset gives it.
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
            CLASS_LIKE => {
                let class = if reader.read_bool("anonymous class refinement")? {
                    ascii_lowercase_word(get_anonymous_class_name(file, read_offset(reader, file)?).as_bytes())
                } else {
                    let class = reader.read_bytes("refinement class")?;
                    if class.is_empty() {
                        return Err(protocol("a declaration refinement names an empty class"));
                    }
                    ascii_lowercase_word(class)
                };

                let templates = read_templates(reader, file)?;
                let inherited_count = reader.read_count("inherited applications", MAXIMUM_MEMBERS)?;
                let mut inherited = Vec::with_capacity(inherited_count);
                for _ in 0..inherited_count {
                    let ancestor = ascii_lowercase_word(reader.read_bytes("inherited ancestor")?);
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
                    required_extends.push(ascii_lowercase_word(reader.read_bytes("required parent")?));
                }

                let properties = read_members(reader, file, "refined properties")?;
                let method_count = reader.read_count("refined methods", MAXIMUM_MEMBERS)?;
                let mut methods = Vec::with_capacity(method_count);
                for _ in 0..method_count {
                    let name = ascii_lowercase_word(reader.read_bytes("refined method")?);
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

        refinements.push(DeclarationRefinement { file_id: file.id, declaration, issues });
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
                apply_signature(function_like, key, *signature)
            }
        }
    }
}

fn apply_class_like(
    codebase: &mut CodebaseMetadata,
    class_name: Word,
    templates: Vec<RefinedTemplate>,
    inherited: Vec<InheritedApplication>,
    required_extends: Vec<Word>,
    properties: Vec<(Word, RefinedType)>,
    methods: Vec<(Word, SignatureRefinement)>,
    issues: Vec<Issue>,
) -> Result<(), ExternalAnalyzerError> {
    let Some(class) = codebase.class_likes.get_mut(&class_name) else {
        return Err(protocol(format!(
            "a declaration refinement names class `{class_name}`, which its file does not declare"
        )));
    };

    class.issues.extend(issues);
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

        let merged = merge_type_preserving_nullability(
            TypeMetadata::from_docblock(refined.type_union, refined.span),
            property.type_declaration_metadata.as_ref(),
        );
        property.set_type_metadata(Some(merged));
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

        apply_signature(function_like, (class_name, method), signature)?;
    }

    Ok(())
}

/// Installs a function-like's own type parameters, parameter and return types, the receiver a
/// closure passed to a parameter runs with, and a closure's own `$this`.
fn apply_signature(
    function_like: &mut FunctionLikeMetadata,
    key: (Word, Word),
    signature: SignatureRefinement,
) -> Result<(), ExternalAnalyzerError> {
    let display = if key.0.is_empty() { key.1.to_string() } else { format!("{}::{}", key.0, key.1) };

    if !signature.templates.is_empty() {
        let mut context = function_like.type_resolution_context.clone().unwrap_or_default();
        for template in signature.templates {
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
        let merged = merge_type_preserving_nullability(
            TypeMetadata::from_docblock(refined.type_union, refined.span),
            parameter.type_declaration_metadata.as_ref(),
        );
        parameter.set_type_metadata(Some(merged));
    }

    for (name, refined) in signature.closure_this {
        let parameter = function_like.get_parameter_mut(name).ok_or_else(|| {
            protocol(format!(
                "a declaration refinement binds `$this` for parameter `{name}` of `{display}`, which is not declared"
            ))
        })?;
        parameter.closure_this_type = Some(TypeMetadata::from_docblock(refined.type_union, refined.span));
    }

    if let Some(refined) = signature.return_type {
        function_like.return_type_metadata = Some(merge_type_preserving_nullability(
            TypeMetadata::from_docblock(refined.type_union, refined.span),
            function_like.return_type_declaration_metadata.as_ref(),
        ));
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
        function_like.refined_this_type = Some(TypeMetadata::from_docblock(refined.type_union, refined.span));
    }

    Ok(())
}

fn apply_inherited(class: &mut ClassLikeMetadata, application: InheritedApplication) {
    let ancestor = application.ancestor;
    let is_parent_interface = class.direct_parent_interfaces.contains(&ancestor);
    let counts = if class.direct_parent_class == Some(ancestor) || (class.kind.is_interface() && is_parent_interface) {
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
