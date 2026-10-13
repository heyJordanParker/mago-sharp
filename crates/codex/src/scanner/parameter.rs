use mago_allocator::Arena;
use mago_names::binding::php_variable_name;
use mago_names::scope::NamespaceScope;
use mago_span::HasSpan;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Node;
use mago_word::Word;
use mago_word::WordMap;
use mago_word::word;

use crate::metadata::constant::ConstantMetadata;
use crate::metadata::flags::MetadataFlags;
use crate::metadata::parameter::FunctionLikeParameterMetadata;
use crate::metadata::ttype::TypeMetadata;
use crate::misc::VariableIdentifier;
use crate::scanner::Context;
use crate::scanner::attribute::scan_attribute_lists;
use crate::scanner::inference::infer_with_constants;
use crate::scanner::ttype::get_type_metadata_from_hint;
use crate::scanner::version_claim::TypeOverride;
use crate::scanner::version_claim::evaluate_version_attributes;
use crate::ttype::atomic::TAtomic;
use crate::ttype::resolution::TypeResolutionContext;
use crate::ttype::wrap_atomic;

#[inline]
pub fn scan_function_like_parameter<'arena, A>(
    parameter: &'arena FunctionLikeParameter<'arena>,
    classname: Option<Word>,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
) -> Option<FunctionLikeParameterMetadata>
where
    A: Arena,
{
    scan_function_like_parameter_with_constants(parameter, classname, type_context, context, scope, None)
}

#[inline]
pub fn scan_function_like_parameter_with_constants<'arena, A>(
    parameter: &'arena FunctionLikeParameter<'arena>,
    classname: Option<Word>,
    type_context: &TypeResolutionContext,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
    constants: Option<&WordMap<ConstantMetadata>>,
) -> Option<FunctionLikeParameterMetadata>
where
    A: Arena,
{
    let verdict = evaluate_version_attributes(&parameter.attribute_lists, context, context.php_version);
    if !verdict.is_available(context.php_version) {
        return None;
    }

    let mut flags = MetadataFlags::origin_flags(context.file.file_type);

    if parameter.ellipsis.is_some() {
        flags |= MetadataFlags::VARIADIC;
    }

    if parameter.ampersand.is_some() {
        flags |= MetadataFlags::BY_REFERENCE;
    }

    if parameter.is_promoted_property() {
        flags |= MetadataFlags::PROMOTED_PROPERTY;
    }

    // A PHP# parameter is written without `$` and runs as a PHP variable, which PHP callers name.
    let name = if context.program.dialect.is_sharp() {
        php_variable_name(parameter.variable.name)
    } else {
        word(parameter.variable.name)
    };

    let mut metadata =
        FunctionLikeParameterMetadata::new(VariableIdentifier(name), parameter.span(), parameter.variable.span, flags)
            .with_attributes(scan_attribute_lists(&parameter.attribute_lists, context, scope, classname));

    metadata.set_type_declaration_metadata(parameter.hint.as_ref().map(|hint| {
        // PHP#'s `Self` is only a return type, so the checker refuses a parameter that writes it, which then takes
        // any argument rather than report the refused type again at each call.
        if context.program.dialect.is_sharp()
            && !Node::Hint(hint).filter_map(|node| matches!(node, Node::Hint(Hint::Self_(_))).then_some(())).is_empty()
        {
            TypeMetadata::new(wrap_atomic(TAtomic::Error), hint.span())
        } else {
            get_type_metadata_from_hint(hint, classname, type_context, context)
        }
    }));

    if let Some(default_value) = &parameter.default_value {
        metadata.flags |= MetadataFlags::HAS_DEFAULT;
        metadata.default_type =
            infer_with_constants(context, scope, default_value.value, classname, constants).map(|u| {
                let mut type_metadata = TypeMetadata::new(u, default_value.span());
                type_metadata.inferred = true;
                type_metadata
            });
    }

    if let Some(optional) = verdict.optional {
        metadata.flags.set(MetadataFlags::HAS_DEFAULT, optional);
    }

    if matches!(verdict.type_override, Some(TypeOverride::Untyped)) {
        metadata.type_declaration_metadata = None;
        metadata.type_metadata = None;
    }
    // TypedWith* type-string parsing isn't wired to the type resolver yet; the
    // claim is still recognized, just left as a no-op until that lands.

    Some(metadata)
}
