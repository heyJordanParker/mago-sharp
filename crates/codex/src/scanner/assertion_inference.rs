use std::collections::BTreeMap;

use mago_allocator::Arena;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::php_variable_name;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Literal;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_word::Word;
use mago_word::WordSet;
use mago_word::word;

use crate::assertion::Assertion;
use crate::metadata::function_like::FunctionLikeMetadata;
use crate::scanner::Context;
use crate::ttype::atomic::TAtomic;
use crate::ttype::atomic::object::TObject;
use crate::ttype::atomic::object::named::TNamedObject;
use crate::ttype::atomic::scalar::TScalar;

type AssertionMap = BTreeMap<Word, Vec<Assertion>>;

/// The functions whose call counts its one argument.
const COUNT_FUNCTIONS: [&[u8]; 2] = [b"count", b"sizeof"];

/// Infers assertions for a function-like whose body is a single expression
/// (arrow function).
pub(super) fn infer_assertions_from_expression_body<'arena, A>(
    expression: &'arena Expression<'arena>,
    metadata: &mut FunctionLikeMetadata,
    context: &Context<'_, 'arena, A>,
) where
    A: Arena,
{
    if has_explicit_assertions(metadata) {
        return;
    }

    let parameter_names = collect_parameter_names(metadata);
    if parameter_names.is_empty() {
        return;
    }

    let (if_true, if_false) = infer_from_expression(expression, &parameter_names, context, false);

    apply_assertions(metadata, if_true, if_false);
}

/// Infers assertions for a function-like whose body is a block. Only single
/// `return expr;` bodies are recognized to keep inference sound: we cannot
/// reason about side effects or multi-path returns at scan time.
pub(super) fn infer_assertions_from_block_body<'arena, A>(
    body: &'arena Block<'arena>,
    metadata: &mut FunctionLikeMetadata,
    context: &Context<'_, 'arena, A>,
) where
    A: Arena,
{
    if has_explicit_assertions(metadata) {
        return;
    }

    let parameter_names = collect_parameter_names(metadata);
    if parameter_names.is_empty() {
        return;
    }

    let Some(return_expression) = single_return_expression(body) else {
        return;
    };

    let (if_true, if_false) = infer_from_expression(return_expression, &parameter_names, context, false);

    apply_assertions(metadata, if_true, if_false);
}

fn has_explicit_assertions(metadata: &FunctionLikeMetadata) -> bool {
    !metadata.assertions.is_empty()
        || !metadata.if_true_assertions.is_empty()
        || !metadata.if_false_assertions.is_empty()
}

fn collect_parameter_names(metadata: &FunctionLikeMetadata) -> WordSet {
    metadata.parameters.iter().map(|p| p.get_name().0).collect()
}

fn single_return_expression<'arena>(body: &'arena Block<'arena>) -> Option<&'arena Expression<'arena>> {
    if body.statements.len() != 1 {
        return None;
    }

    let Statement::Return(ret) = &body.statements.as_slice()[0] else {
        return None;
    };

    ret.value
}

fn apply_assertions(metadata: &mut FunctionLikeMetadata, if_true: AssertionMap, if_false: AssertionMap) {
    if if_true.is_empty() && if_false.is_empty() {
        return;
    }

    for (var, assertions) in if_true {
        metadata.if_true_assertions.entry(var).or_default().extend(assertions);
    }
    for (var, assertions) in if_false {
        metadata.if_false_assertions.entry(var).or_default().extend(assertions);
    }

    metadata.assertions_inferred = true;
}

fn infer_from_expression<'arena, A>(
    expression: &'arena Expression<'arena>,
    parameter_names: &WordSet,
    context: &Context<'_, 'arena, A>,
    negated: bool,
) -> (AssertionMap, AssertionMap)
where
    A: Arena,
{
    let expression = unwrap_parens(expression);
    let resolved_names = context.resolved_names;

    ensure_sufficient_stack(|| match expression {
        Expression::UnaryPrefix(unary) if matches!(unary.operator, UnaryPrefixOperator::Not(_)) => {
            infer_from_expression(unary.operand, parameter_names, context, !negated)
        }
        Expression::Binary(binary)
            if let Some((var, holds_when_filled)) =
                parse_count_compare(binary.lhs, &binary.operator, binary.rhs, parameter_names, context) =>
        {
            build_count_assertions(var, holds_when_filled != negated)
        }
        Expression::Binary(binary) => match &binary.operator {
            BinaryOperator::Instanceof(_) => parse_instanceof(binary.lhs, binary.rhs, parameter_names, resolved_names)
                .map(|(var, atomic)| build_assertions(var, atomic, negated))
                .unwrap_or_default(),
            BinaryOperator::Identical(_) | BinaryOperator::Equal(_) => {
                parse_null_compare(binary.lhs, binary.rhs, parameter_names, resolved_names)
                    .map(|var| build_assertions(var, TAtomic::Null, negated))
                    .unwrap_or_default()
            }
            BinaryOperator::NotIdentical(_) | BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => {
                parse_null_compare(binary.lhs, binary.rhs, parameter_names, resolved_names)
                    .map(|var| build_assertions(var, TAtomic::Null, !negated))
                    .unwrap_or_default()
            }
            _ => Default::default(),
        },
        Expression::Call(Call::Function(call)) => parse_type_check_function(call, parameter_names, resolved_names)
            .map(|(var, atomic)| build_assertions(var, atomic, negated))
            .unwrap_or_default(),
        _ => Default::default(),
    })
}

/// The parameter whose count a comparison with `0` tests, and whether the comparison holds when the parameter holds an
/// element: `count($items) > 0` and `0 != count($items)` hold for a non-empty one, and `count($items) === 0` and
/// `count($items) <= 0` for an empty one.
fn parse_count_compare<'arena, A>(
    lhs: &'arena Expression<'arena>,
    operator: &BinaryOperator<'arena>,
    rhs: &'arena Expression<'arena>,
    parameter_names: &WordSet,
    context: &Context<'_, 'arena, A>,
) -> Option<(Word, bool)>
where
    A: Arena,
{
    let (counted, holds_when_filled) = if is_zero_literal(rhs) {
        let holds_when_filled = match operator {
            BinaryOperator::NotEqual(_)
            | BinaryOperator::NotIdentical(_)
            | BinaryOperator::AngledNotEqual(_)
            | BinaryOperator::GreaterThan(_) => true,
            BinaryOperator::Equal(_) | BinaryOperator::Identical(_) | BinaryOperator::LessThanOrEqual(_) => false,
            _ => return None,
        };

        (lhs, holds_when_filled)
    } else if is_zero_literal(lhs) {
        let holds_when_filled = match operator {
            BinaryOperator::NotEqual(_)
            | BinaryOperator::NotIdentical(_)
            | BinaryOperator::AngledNotEqual(_)
            | BinaryOperator::LessThan(_) => true,
            BinaryOperator::Equal(_) | BinaryOperator::Identical(_) | BinaryOperator::GreaterThanOrEqual(_) => false,
            _ => return None,
        };

        (rhs, holds_when_filled)
    } else {
        return None;
    };

    Some((counted_parameter(counted, parameter_names, context)?, holds_when_filled))
}

/// The parameter `expression` counts: PHP's `count($items)` or `sizeof($items)`, or `global.count(items)`, which calls
/// PHP's `count` through its root namespace in the standard library, spec section 23.
fn counted_parameter<'arena, A>(
    expression: &'arena Expression<'arena>,
    parameter_names: &WordSet,
    context: &Context<'_, 'arena, A>,
) -> Option<Word>
where
    A: Arena,
{
    let (function, argument_list) = match unwrap_parens(expression) {
        Expression::Call(Call::Function(FunctionCall {
            function: Expression::Identifier(function),
            argument_list,
        })) => (called_function_name(function, context.resolved_names), argument_list),
        Expression::Call(Call::Method(
            call @ MethodCall { method: ClassLikeMemberSelector::Identifier(function), argument_list, .. },
        )) if context.file.is_standard_library
            && context.resolved_names.static_call_class(call).is_some_and(|class| class.name.value() == b"global") =>
        {
            (function.value, argument_list)
        }
        _ => return None,
    };

    if !COUNT_FUNCTIONS.iter().any(|count| function.eq_ignore_ascii_case(count)) {
        return None;
    }

    single_argument_parameter(argument_list, parameter_names, context.resolved_names)
}

/// The function PHP's `name(…)` runs, as the scan reads it with no codebase: a name written without a namespace and not
/// imported is PHP's own function of the root namespace, as `count` in `namespace Lib;` runs PHP's `count`, and any
/// other name is the one the file resolves.
fn called_function_name<'arena>(function: &Identifier<'arena>, resolved_names: &ResolvedNames<'arena>) -> &'arena [u8] {
    let written = function.value();
    if !resolved_names.is_imported(function) && !written.contains(&b'\\') {
        return written;
    }

    resolved_names.get(function)
}

fn is_zero_literal(expression: &Expression<'_>) -> bool {
    matches!(unwrap_parens(expression), Expression::Literal(Literal::Integer(integer)) if integer.value == Some(0))
}

/// The assertions of a test that holds when the parameter `var` holds an element, if `holds_when_filled`, or when it
/// holds none.
fn build_count_assertions(var: Word, holds_when_filled: bool) -> (AssertionMap, AssertionMap) {
    let filled = vec![Assertion::NonEmptyCountable(true)];
    let empty = vec![Assertion::EmptyCountable];
    let (if_true, if_false) = if holds_when_filled { (filled, empty) } else { (empty, filled) };

    (AssertionMap::from([(var, if_true)]), AssertionMap::from([(var, if_false)]))
}

fn build_assertions(var: Word, atomic: TAtomic, negated: bool) -> (AssertionMap, AssertionMap) {
    let mut if_true = AssertionMap::new();
    let mut if_false = AssertionMap::new();

    if negated {
        if_true.insert(var, vec![Assertion::IsNotType(atomic.clone())]);
        if_false.insert(var, vec![Assertion::IsType(atomic)]);
    } else {
        if_true.insert(var, vec![Assertion::IsType(atomic.clone())]);
        if_false.insert(var, vec![Assertion::IsNotType(atomic)]);
    }

    (if_true, if_false)
}

fn unwrap_parens<'expr, 'arena>(mut expression: &'expr Expression<'arena>) -> &'expr Expression<'arena> {
    while let Expression::Parenthesized(p) = expression {
        expression = p.expression;
    }
    expression
}

/// The parameter `expression` reads: PHP's `$items`, or in a PHP# body the bare name `items`, as a parameter and an
/// extension's receiver are read.
fn parameter_var(
    expression: &Expression<'_>,
    parameter_names: &WordSet,
    resolved_names: &ResolvedNames<'_>,
) -> Option<Word> {
    let candidate = match unwrap_parens(expression) {
        Expression::Variable(Variable::Direct(direct)) => word(direct.name),
        Expression::ConstantAccess(access)
            if matches!(resolved_names.binding(&access.name), Some(Binding::Local(_))) =>
        {
            php_variable_name(access.name.value())
        }
        _ => return None,
    };

    parameter_names.contains(&candidate).then_some(candidate)
}

/// The parameter the one argument of `argument_list` reads.
fn single_argument_parameter(
    argument_list: &ArgumentList<'_>,
    parameter_names: &WordSet,
    resolved_names: &ResolvedNames<'_>,
) -> Option<Word> {
    if argument_list.arguments.len() != 1 {
        return None;
    }

    parameter_var(argument_list.arguments.iter().next()?.value(), parameter_names, resolved_names)
}

fn parse_instanceof<'arena>(
    lhs: &Expression<'arena>,
    rhs: &Expression<'arena>,
    parameter_names: &WordSet,
    resolved_names: &ResolvedNames<'arena>,
) -> Option<(Word, TAtomic)> {
    let var = parameter_var(lhs, parameter_names, resolved_names)?;

    let Expression::Identifier(identifier) = unwrap_parens(rhs) else {
        return None;
    };

    let class_name = word(resolved_names.get(identifier));

    Some((var, TAtomic::Object(TObject::Named(TNamedObject::new(class_name)))))
}

fn parse_null_compare<'arena>(
    lhs: &Expression<'arena>,
    rhs: &Expression<'arena>,
    parameter_names: &WordSet,
    resolved_names: &ResolvedNames<'arena>,
) -> Option<Word> {
    if let Some(var) = parameter_var(lhs, parameter_names, resolved_names)
        && is_null_literal(rhs)
    {
        return Some(var);
    }

    if let Some(var) = parameter_var(rhs, parameter_names, resolved_names)
        && is_null_literal(lhs)
    {
        return Some(var);
    }

    None
}

fn is_null_literal(expression: &Expression<'_>) -> bool {
    matches!(unwrap_parens(expression), Expression::Literal(Literal::Null(_)))
}

fn parse_type_check_function<'arena>(
    call: &FunctionCall<'arena>,
    parameter_names: &WordSet,
    resolved_names: &ResolvedNames<'arena>,
) -> Option<(Word, TAtomic)> {
    let Expression::Identifier(function_id) = call.function else {
        return None;
    };

    let atomic = type_for_check_function(called_function_name(function_id, resolved_names))?;

    single_argument_parameter(&call.argument_list, parameter_names, resolved_names).map(|var| (var, atomic))
}

fn type_for_check_function(name: &[u8]) -> Option<TAtomic> {
    match name.to_ascii_lowercase().as_slice() {
        b"is_int" | b"is_integer" | b"is_long" => Some(TAtomic::Scalar(TScalar::int())),
        b"is_string" => Some(TAtomic::Scalar(TScalar::string())),
        b"is_float" | b"is_double" | b"is_real" => Some(TAtomic::Scalar(TScalar::float())),
        b"is_bool" => Some(TAtomic::Scalar(TScalar::bool())),
        b"is_null" => Some(TAtomic::Null),
        b"is_object" => Some(TAtomic::Object(TObject::Any)),
        b"is_numeric" => Some(TAtomic::Scalar(TScalar::numeric())),
        _ => None,
    }
}
