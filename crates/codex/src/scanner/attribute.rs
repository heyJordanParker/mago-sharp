use mago_allocator::Arena;
use mago_names::scope::NamespaceScope;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Array;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::ClassConstantAccess;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::LegacyArray;
use mago_syntax::cst::Literal;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::word;

use crate::flags::attribute::AttributeFlags;
use crate::metadata::attribute::AttributeArgumentMetadata;
use crate::metadata::attribute::AttributeMetadata;
use crate::metadata::attribute::ConstantExpression;
use crate::scanner::Context;
use crate::scanner::inference::infer;

#[inline]
pub fn scan_attribute_lists<'arena, A>(
    attribute_lists: &'arena Sequence<'arena, AttributeList<'arena>>,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
    enclosing_class: Option<Word>,
) -> Vec<AttributeMetadata>
where
    A: Arena,
{
    let mut metadata = vec![];

    for attribute_list in attribute_lists {
        for attribute in &attribute_list.attributes {
            let arguments = attribute
                .argument_list
                .iter()
                .flat_map(|arguments| &arguments.arguments)
                .map(|argument| {
                    let (name, name_span, value) = match argument {
                        PartialArgument::Positional(argument) => (None, None, Some(argument.value)),
                        PartialArgument::Named(argument) => {
                            (Some(word(argument.name.value)), Some(argument.name.span), Some(argument.value))
                        }
                        PartialArgument::NamedPlaceholder(argument) => {
                            (Some(word(argument.name.value)), Some(argument.name.span), None)
                        }
                        PartialArgument::Placeholder(_) | PartialArgument::VariadicPlaceholder(_) => (None, None, None),
                    };

                    AttributeArgumentMetadata {
                        name,
                        span: argument.span(),
                        name_span,
                        value_span: value.map(HasSpan::span),
                        value_type: value.and_then(|value| infer(context, scope, value, enclosing_class)),
                        value: value.map(|value| evaluate(context, value, enclosing_class)),
                    }
                })
                .collect();
            metadata.push(AttributeMetadata {
                name: word(context.resolved_names.get(&attribute.name)),
                span: attribute.span(),
                arguments,
            });
        }
    }

    metadata
}

fn evaluate<'arena, A>(
    context: &Context<'_, 'arena, A>,
    expression: &'arena Expression<'arena>,
    enclosing_class: Option<Word>,
) -> ConstantExpression
where
    A: Arena,
{
    let unsupported = || ConstantExpression::Unsupported(expression.span());
    let class_name = |class: &'arena Expression<'arena>| match class {
        Expression::Identifier(identifier) => Some(word(context.resolved_names.get(identifier))),
        Expression::Self_(_) | Expression::Static(_) => enclosing_class,
        _ => None,
    };

    ensure_sufficient_stack(|| match expression {
        Expression::Parenthesized(parenthesized) => evaluate(context, parenthesized.expression, enclosing_class),
        Expression::Literal(Literal::Null(_)) => ConstantExpression::Null,
        Expression::Literal(Literal::True(_)) => ConstantExpression::Bool(true),
        Expression::Literal(Literal::False(_)) => ConstantExpression::Bool(false),
        Expression::Literal(Literal::Integer(integer)) => {
            integer.value.and_then(|value| i64::try_from(value).ok()).map_or_else(unsupported, ConstantExpression::Int)
        }
        Expression::Literal(Literal::Float(float)) => ConstantExpression::Float(float.value),
        Expression::Literal(Literal::String(string)) => {
            string.value.map_or_else(unsupported, |value| ConstantExpression::String(word(value)))
        }
        Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Negation(_), operand }) => {
            match evaluate(context, operand, enclosing_class) {
                ConstantExpression::Int(value) => value.checked_neg().map_or_else(unsupported, ConstantExpression::Int),
                ConstantExpression::Float(value) => ConstantExpression::Float(-value),
                _ => unsupported(),
            }
        }
        Expression::Array(Array { elements, .. }) | Expression::LegacyArray(LegacyArray { elements, .. }) => {
            let mut entries = Vec::with_capacity(elements.len());
            for element in elements.iter() {
                entries.push(match element {
                    ArrayElement::KeyValue(element) => (
                        Some(evaluate(context, element.key, enclosing_class)),
                        evaluate(context, element.value, enclosing_class),
                    ),
                    ArrayElement::Value(element) => (None, evaluate(context, element.value, enclosing_class)),
                    ArrayElement::Variadic(_) | ArrayElement::Missing(_) => return unsupported(),
                });
            }

            ConstantExpression::Array(entries)
        }
        Expression::ConstantAccess(access) => {
            let names = context.resolved_names;
            let name = if names.is_imported(&access.name) {
                names.get(&access.name)
            } else {
                access.name.value().strip_prefix(b"\\").unwrap_or(access.name.value())
            };

            if name.eq_ignore_ascii_case(b"null") {
                ConstantExpression::Null
            } else if name.eq_ignore_ascii_case(b"true") {
                ConstantExpression::Bool(true)
            } else if name.eq_ignore_ascii_case(b"false") {
                ConstantExpression::Bool(false)
            } else {
                ConstantExpression::Constant(word(name))
            }
        }
        Expression::Access(Access::ClassConstant(ClassConstantAccess {
            class,
            constant: ClassLikeConstantSelector::Identifier(constant),
            ..
        })) => match class_name(class) {
            Some(class) if constant.value.eq_ignore_ascii_case(b"class") => ConstantExpression::ClassName(class),
            Some(class) => ConstantExpression::ClassConstant(class, word(constant.value)),
            None => unsupported(),
        },
        Expression::Instantiation(Instantiation { class, argument_list, .. }) => {
            let Some(class) = class_name(class) else {
                return unsupported();
            };

            let mut arguments = Vec::new();
            for argument in argument_list.iter().flat_map(|list| list.arguments.iter()) {
                arguments.push(match argument {
                    Argument::Positional(argument) if argument.ellipsis.is_none() => {
                        (None, evaluate(context, argument.value, enclosing_class))
                    }
                    Argument::Named(argument) => {
                        (Some(word(argument.name.value)), evaluate(context, argument.value, enclosing_class))
                    }
                    Argument::Positional(_) => return unsupported(),
                });
            }

            ConstantExpression::New(class, arguments)
        }
        _ => unsupported(),
    })
}

#[inline]
pub fn get_attribute_flags<'arena, A>(
    class_like_name: Word,
    attribute_lists: &'arena Sequence<'arena, AttributeList<'arena>>,
    context: &Context<'_, 'arena, A>,
    scope: &NamespaceScope,
    classname: Option<Word>,
) -> Option<AttributeFlags>
where
    A: Arena,
{
    if class_like_name.as_bytes().eq_ignore_ascii_case(b"Attribute") {
        return Some(AttributeFlags::TARGET_CLASS);
    }

    for attribute in attribute_lists.iter().flat_map(|list| list.attributes.iter()) {
        let attribute_name = context.resolved_names.get(&attribute.name);
        if !attribute_name.eq_ignore_ascii_case(b"Attribute") {
            continue;
        }

        let Some(first_argument) =
            attribute.argument_list.as_ref().and_then(|argument_list| argument_list.arguments.first())
        else {
            // No target specified means all targets
            return Some(AttributeFlags::TARGET_ALL);
        };

        let Some(value) = first_argument.value() else {
            return None; // Semantically invalid, but we don't want to panic here.
        };

        let inferred_type = infer(context, scope, value, classname);
        let bits = inferred_type.and_then(|i| i.get_single_literal_int_value()).and_then(|value| {
            if !(0..=255).contains(&value) {
                return None;
            }

            Some(value as u8)
        });

        return Some(if let Some(bits) = bits {
            AttributeFlags::from_bits(bits)
        } else {
            // Unable to infer the target, allow all targets + repeatable
            AttributeFlags::all()
        });
    }

    None
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_database::file::File;
    use mago_names::resolver::NameResolver;
    use mago_syntax::parser::parse_file;
    use mago_word::ascii_lowercase_word;
    use mago_word::word;
    use ordered_float::OrderedFloat;

    use crate::metadata::attribute::ConstantExpression;
    use crate::scanner::scan_program;

    #[test]
    fn attribute_arguments_evaluate_to_constant_expressions() {
        let code = r"<?php
namespace App;

use Other\Type;

#[Type(list: new Type(Item::class, nullable: true), keys: Type::String, limit: \PHP_INT_MAX)]
#[Type([-2, 1.5, 'key' => null, true], self::class, strlen('x'))]
final class Holder {}
";
        let file = File::ephemeral(Cow::Borrowed(b"code.php"), Cow::Borrowed(code.as_bytes()));
        let arena = LocalArena::new();
        let program = parse_file(&arena, &file);
        assert!(!program.has_errors(), "the fixture parses: {:?}", program.errors);
        let resolved_names = NameResolver::new(&arena).resolve(program);
        let codebase = scan_program(&arena, &file, program, &resolved_names, mago_php_version::PHPVersion::LATEST);
        let holder = codebase.class_likes.get(&ascii_lowercase_word(b"App\\Holder")).expect("Holder is scanned");
        let values = |index: usize| {
            holder.attributes[index].arguments.iter().map(|argument| argument.value.clone()).collect::<Vec<_>>()
        };

        assert_eq!(
            values(0),
            [
                Some(ConstantExpression::New(
                    word("Other\\Type"),
                    vec![
                        (None, ConstantExpression::ClassName(word("App\\Item"))),
                        (Some(word("nullable")), ConstantExpression::Bool(true)),
                    ],
                )),
                Some(ConstantExpression::ClassConstant(word("Other\\Type"), word("String"))),
                Some(ConstantExpression::Constant(word("PHP_INT_MAX"))),
            ]
        );

        let unsupported = values(1).pop().flatten();
        assert!(matches!(unsupported, Some(ConstantExpression::Unsupported(_))), "a call is not evaluated");
        assert_eq!(
            values(1)[..2],
            [
                Some(ConstantExpression::Array(vec![
                    (None, ConstantExpression::Int(-2)),
                    (None, ConstantExpression::Float(OrderedFloat(1.5))),
                    (Some(ConstantExpression::String(word("key"))), ConstantExpression::Null),
                    (None, ConstantExpression::Bool(true)),
                ])),
                Some(ConstantExpression::ClassName(word("App\\Holder"))),
            ]
        );
    }
}
