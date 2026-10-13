use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::float::TFloat;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::comparator::union_comparator::can_expression_types_be_identical;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_bool;
use mago_codex::ttype::get_int;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::php_operator_name;
use mago_php_version::PHPVersion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_word::Word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::invocation::InvocationTarget;
use crate::invocation::arguments::verify_argument_type;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;
use crate::utils::php_emulation::numeric_string_equals_int;

/// The static method a PHP# operator runs as, named `name`, on operands of `operand_types`: the one the left
/// operand's class declares or inherits, else the right one's, as `Money::op_Addition` for `money + other`. A class
/// that declares none, or an operand that is no instance of one class, has none.
pub(crate) fn get_operator_method<'ctx>(
    codebase: &'ctx CodebaseMetadata,
    name: &[u8],
    operand_types: &[&TUnion],
) -> Option<(MethodIdentifier, &'ctx FunctionLikeMetadata)> {
    operand_types.iter().find_map(|operand_type| {
        let class = get_instance_class(operand_type, codebase)?;
        let method = codebase.get_declaring_method_identifier(&MethodIdentifier::new(class, word(name)));
        let metadata = codebase.get_method_by_id(&method)?;

        metadata.method_metadata.as_ref().is_some_and(|method| method.is_static).then_some((method, metadata))
    })
}

/// Whether each of `operand_types` can be the value the matching parameter of the static method `method` takes. An
/// `int` never is the `Money` that `Money::op_Comparison` takes, so `money < 5` is refused before the call is checked.
pub(crate) fn can_take_operands(
    codebase: &CodebaseMetadata,
    (method, metadata): (MethodIdentifier, &FunctionLikeMetadata),
    operand_types: &[&TUnion],
) -> bool {
    operand_types.iter().zip(&metadata.parameters).all(|(operand_type, parameter)| {
        parameter.get_type_metadata().is_none_or(|parameter_type| {
            let parameter_type = expand_in_class(codebase, &method, &parameter_type.type_union);

            can_expression_types_be_identical(codebase, operand_type, &parameter_type, false, false)
        })
    })
}

/// Whether each of `operand_types` is always a value the matching parameter of the static method `method` takes, as
/// `verify_argument_type` decides it for the call the operator runs as. `Money::op_Equality` takes no `Line` of a
/// `Money|Line`.
pub(crate) fn takes_operands(
    codebase: &CodebaseMetadata,
    (method, metadata): (MethodIdentifier, &FunctionLikeMetadata),
    operand_types: &[&TUnion],
) -> bool {
    operand_types.iter().zip(&metadata.parameters).all(|(operand_type, parameter)| {
        parameter.get_type_metadata().is_none_or(|parameter_type| {
            let parameter_type = expand_in_class(codebase, &method, &parameter_type.type_union);

            union_comparator::is_contained_by(
                codebase,
                operand_type,
                &parameter_type,
                true,
                true,
                false,
                &mut ComparisonResult::with_sharp_rules(true),
            )
        })
    })
}

/// Runs the PHP# arithmetic operator `symbol` on `operands` when one of them is an instance: the static method its
/// class declares or inherits, checked as the call it runs as, gives the result. An instance whose class declares
/// none, or an operand the operator does not take, is refused, and the rest of the code is checked as if the operator
/// gave that instance. Operands with no instance run none.
pub(crate) fn analyze_instance_operator<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    symbol: &BinaryOperator<'_>,
    operands: &[(&Expression<'arena>, &TUnion)],
    span: Span,
) -> Option<TUnion>
where
    A: Arena,
{
    let codebase = context.codebase;
    let instance =
        operands.iter().position(|(_, operand_type)| get_instance_class(operand_type, codebase).is_some())?;
    let name = php_operator_name(symbol, operands.len())?;
    let operand_types: Vec<&TUnion> = operands.iter().map(|(_, operand_type)| *operand_type).collect();

    Some(match get_operator_method(codebase, name, &operand_types) {
        Some(method) if can_take_operands(codebase, method, &operand_types) => {
            analyze_operator_call(context, artifacts, method, operands, span)
        }
        method => {
            report_refused_operator(context, symbol, operands, instance, method);

            operands[instance].1.to_non_nullable()
        }
    })
}

/// `union` as the class declaring `method` reads it, so its `self` is that class.
fn expand_in_class(codebase: &CodebaseMetadata, method: &MethodIdentifier, union: &TUnion) -> TUnion {
    let class = codebase.get_class_like(method.get_class_name().as_bytes()).map(|class| class.name);
    let options = TypeExpansionOptions {
        self_class: class,
        static_class_type: class.map_or(StaticClassType::None, StaticClassType::Name),
        ..Default::default()
    };
    let mut union = union.clone();
    expander::expand_union(codebase, &mut union, &options);

    union
}

/// The one class of an instance of `operand_type`, `null` aside. An enum, which no operator takes, is none.
fn get_instance_class(operand_type: &TUnion, codebase: &CodebaseMetadata) -> Option<Word> {
    let mut types = operand_type.types.iter().filter(|atomic| !atomic.is_null());
    let (Some(TAtomic::Object(TObject::Named(object))), None) = (types.next(), types.next()) else {
        return None;
    };

    codebase.get_enum(object.name.as_bytes()).is_none().then_some(object.name)
}

/// Checks the operands of a PHP# operator as the arguments of the static call `method` it runs as, at `span`, and
/// returns what the call returns. The call is recorded at `span`, so the effects of a body that applies the operator
/// include the operator's.
pub(crate) fn analyze_operator_call<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    (method, metadata): (MethodIdentifier, &FunctionLikeMetadata),
    operands: &[(&Expression<'arena>, &TUnion)],
    span: Span,
) -> TUnion
where
    A: Arena,
{
    let codebase = context.codebase;
    let expand = |union: &TUnion| expand_in_class(codebase, &method, union);

    let identifier = FunctionLikeIdentifier::Method(method.get_class_name(), method.get_method_name());
    artifacts.record_call_target(span, CallTarget::FunctionLike { callee: identifier, class: None });

    let target = InvocationTarget::FunctionLike {
        identifier,
        metadata,
        inferred_return_type: None,
        effective_signature: None,
        method_context: None,
        span,
    };
    for (offset, ((operand, operand_type), parameter)) in operands.iter().zip(&metadata.parameters).enumerate() {
        if let Some(parameter_type) = parameter.get_type_metadata() {
            let parameter_type = expand(&parameter_type.type_union);

            verify_argument_type(context, artifacts, operand_type, &parameter_type, offset, operand, &target);
        }
    }

    metadata.return_type_metadata.as_ref().map_or_else(get_mixed, |return_type| expand(&return_type.type_union))
}

/// Reports a PHP# arithmetic operator `symbol` on `operands`, the one at `instance` an instance. Its class neither
/// declares nor inherits the operator, or `declared`, the operator it declares, takes no such operands. One operand
/// makes the operator unary.
fn report_refused_operator<A>(
    context: &mut Context<'_, '_, A>,
    symbol: &BinaryOperator<'_>,
    operands: &[(&Expression<'_>, &TUnion)],
    instance: usize,
    declared: Option<(MethodIdentifier, &FunctionLikeMetadata)>,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let op = BytesDisplay(symbol.as_bytes());
    let names: Vec<String> = operands.iter().map(|(_, operand_type)| display_operand(context, operand_type)).collect();

    let issue = match (names.as_slice(), declared) {
        ([operand], None) => {
            let class = display_operand(context, &operands[instance].1.to_non_nullable());

            Issue::error(format!(
                "Unary `{op}` cannot apply to `{operand}`: `{class}` declares no unary `operator {op}`."
            ))
            .with_note(format!(
                "Unary `{op}` on a class instance exists only where its class declares unary `operator {op}`."
            ))
            .with_help("Apply it to a value the instance holds, such as a property.")
        }
        ([lhs, rhs, ..], None) => {
            let class = display_operand(context, &operands[instance].1.to_non_nullable());

            Issue::error(format!("`{op}` cannot apply to `{lhs}` and `{rhs}`: `{class}` declares no `operator {op}`."))
                .with_note(format!("`{op}` on a class instance exists only where its class declares `operator {op}`."))
                .with_help("Apply it to values the instances hold, such as their properties.")
        }
        (names, Some((method, metadata))) => {
            let class = display_class_like_name(context, method.get_class_name());
            let taken: Vec<String> = metadata
                .parameters
                .iter()
                .filter_map(|parameter| parameter.get_type_metadata())
                .map(|parameter_type| {
                    display_operand(context, &expand_in_class(codebase, &method, &parameter_type.type_union))
                })
                .collect();
            let (names, taken) = (names.join("` and `"), taken.join("` and `"));
            let (opening, unary) = if operands.len() == 1 { ("Unary ", "unary ") } else { ("", "") };

            Issue::error(format!(
                "{opening}`{op}` cannot apply to `{names}`: `{class}` declares {unary}`operator {op}` on `{taken}`."
            ))
            .with_note(format!(
                "`{op}` on a class instance runs the `operator {op}` its class declares, on the types it declares."
            ))
            .with_help("Apply it to values of the types the operator takes.")
        }
        ([], None) => return,
    };

    let issue = operands.iter().zip(&names).enumerate().fold(issue, |issue, (offset, ((operand, _), name))| {
        let annotation = if offset == instance {
            Annotation::primary(operand.span())
        } else {
            Annotation::secondary(operand.span())
        };

        issue.with_annotation(annotation.with_message(format!("This is `{name}`.")))
    });

    context.collector.report_with_code(IssueCode::InvalidOperand, issue);
}

/// Refuses each operand of the PHP# bitwise operator written at `operator` that is not an `int`, as spec section 19
/// gives `|`, `&`, `^`, `~`, `<<`, `>>` and their compound forms to `int` only. Two `bool`s name the operator that
/// joins them. When it refuses, returns the type the rest of the code reads: the `bool` two `bool`s meant, else `int`.
pub(crate) fn refuse_non_int_operands<A>(
    context: &mut Context<'_, '_, A>,
    operator: Span,
    operands: &[(&Expression<'_>, &TUnion)],
) -> Option<TUnion>
where
    A: Arena,
{
    let written = String::from_utf8_lossy(&context.source_file.contents[operator.to_range_usize()]).into_owned();
    let note = "PHP# gives `|`, `&`, `^`, `~`, `<<`, `>>` and their compound forms to `int` only.";

    if let [(lhs, lhs_type), (rhs, rhs_type)] = operands
        && lhs_type.is_bool()
        && rhs_type.is_bool()
        && let Some(joiner) = match written.trim_end_matches('=') {
            "|" => Some("||"),
            "&" => Some("&&"),
            "^" => Some("!="),
            _ => None,
        }
    {
        context.collector.report_with_code(
            IssueCode::InvalidOperand,
            Issue::error(format!(
                "`{written}` takes `int`, but both sides are `bool`: write `{joiner}` for two `bool` values."
            ))
            .with_annotation(Annotation::primary(lhs.span()).with_message("This is `bool`."))
            .with_annotation(Annotation::secondary(rhs.span()).with_message("This is `bool`."))
            .with_note(note)
            .with_help("`||`, `&&` and `!=` join two `bool` values."),
        );

        return Some(get_bool());
    }

    let mut refused = false;
    for (operand, operand_type) in operands {
        if operand_type.is_int() || operand_type.is_never() {
            continue;
        }

        let name = display_operand(context, operand_type);
        let help = if operand_type.is_nullable() && operand_type.to_non_nullable().is_int() {
            "Test it with `!= null` first."
        } else {
            "Use an `int`: `(int)` converts a `float`, and `Int.parse` reads a `string`."
        };

        context.collector.report_with_code(
            IssueCode::InvalidOperand,
            Issue::error(format!("`{written}` takes `int`, but this is `{name}`."))
                .with_annotation(Annotation::primary(operand.span()).with_message(format!("This is `{name}`.")))
                .with_note(note)
                .with_help(help),
        );
        refused = true;
    }

    refused.then(get_int)
}

/// An operand's type as PHP# writes it, so a literal or a narrowed scalar shows as its scalar type.
pub(crate) fn display_operand<A>(context: &Context<'_, '_, A>, operand_type: &TUnion) -> String
where
    A: Arena,
{
    let mut shown = operand_type.clone();
    shown.widen_scalars();

    display_sharp_type(context, &shown)
}

#[inline]
pub fn is_always_less_than_or_equal(lhs: &TUnion, rhs: &TUnion) -> bool {
    if let (Some(max_lhs), Some(min_rhs)) = (lhs.get_maximum_int_value(), rhs.get_minimum_int_value()) {
        return max_lhs <= min_rhs;
    }

    is_always_less_than(lhs, rhs) || is_always_identical_to(lhs, rhs)
}

#[inline]
pub fn is_always_greater_than_or_equal(lhs: &TUnion, rhs: &TUnion) -> bool {
    if let (Some(min_lhs), Some(max_rhs)) = (lhs.get_minimum_int_value(), rhs.get_maximum_int_value()) {
        return min_lhs >= max_rhs;
    }

    is_always_greater_than(lhs, rhs) || is_always_identical_to(lhs, rhs)
}

/// Checks if the left-hand side type is always strictly less than the right-hand side type.
/// Returns `false` if uncertain.
pub fn is_always_less_than(lhs: &TUnion, rhs: &TUnion) -> bool {
    if lhs.is_null() && !rhs.is_null() {
        return true;
    }

    if lhs.is_false() && rhs.is_true() {
        return true;
    }

    if lhs.is_false() && !rhs.is_null() && !rhs.is_false() {
        return true;
    }

    if let (Some(max_lhs), Some(min_rhs)) = (lhs.get_maximum_int_value(), rhs.get_minimum_int_value()) {
        return max_lhs < min_rhs;
    }

    if !lhs.is_single() || !rhs.is_single() {
        return false;
    }

    let lhs_atomic = lhs.get_single();
    let rhs_atomic = rhs.get_single();

    match (lhs_atomic, rhs_atomic) {
        (TAtomic::Scalar(TScalar::Float(l)), TAtomic::Scalar(TScalar::Float(r))) => match (l, r) {
            (TFloat::Literal(l_val), TFloat::Literal(r_val)) => return l_val < r_val,
            _ => return false,
        },
        _ => {}
    }

    false
}

/// Checks if the left-hand side type is always strictly greater than the right-hand side type.
/// Returns `false` if uncertain.
pub fn is_always_greater_than(lhs: &TUnion, rhs: &TUnion) -> bool {
    if !lhs.is_null() && rhs.is_null() {
        return true;
    }

    if lhs.is_true() && rhs.is_false() {
        return true;
    }

    if lhs.is_true() && !rhs.is_null() && !rhs.is_true() {
        return true;
    }

    if let (Some(min_lhs), Some(max_rhs)) = (lhs.get_minimum_int_value(), rhs.get_maximum_int_value()) {
        return min_lhs > max_rhs;
    }

    if !lhs.is_single() || !rhs.is_single() {
        return false;
    }

    let lhs_atomic = lhs.get_single();
    let rhs_atomic = rhs.get_single();

    match (lhs_atomic, rhs_atomic) {
        (TAtomic::Scalar(TScalar::Float(l)), TAtomic::Scalar(TScalar::Float(r))) => match (l, r) {
            (TFloat::Literal(l_val), TFloat::Literal(r_val)) => return l_val > r_val,
            _ => return false,
        },
        _ => {}
    }

    false
}

pub fn is_always_identical_to(lhs: &TUnion, rhs: &TUnion) -> bool {
    if lhs.is_null() && rhs.is_null() {
        return true;
    }

    if lhs.is_false() && rhs.is_false() {
        return true;
    }

    if lhs.is_true() && rhs.is_true() {
        return true;
    }

    if lhs.is_enum() && rhs.is_enum() {
        let left_cases = lhs.get_enum_cases();
        let right_cases = rhs.get_enum_cases();

        if left_cases.len() > 1 || right_cases.len() > 1 {
            return false;
        }

        let (left_enum, left_case) = left_cases[0];
        let (right_enum, right_case) = right_cases[0];

        return right_case.is_some() && left_case.is_some() && left_enum == right_enum && left_case == right_case;
    }

    if let (Some(l), Some(r)) = (lhs.get_single_literal_int_value(), rhs.get_single_literal_int_value()) {
        return l == r;
    }

    if let (Some(l), Some(r)) = (lhs.get_single_literal_float_value(), rhs.get_single_literal_float_value()) {
        return l == r;
    }

    if let (Some(l), Some(r)) = (lhs.get_single_literal_string_value(), rhs.get_single_literal_string_value()) {
        return l == r;
    }

    if !lhs.is_single() || !rhs.is_single() {
        return false;
    }

    match (lhs.get_single(), rhs.get_single()) {
        (TAtomic::Array(lhs), TAtomic::Array(rhs)) => are_sealed_arrays_always_identical(lhs, rhs),
        _ => false,
    }
}

fn are_sealed_arrays_always_identical(lhs: &TArray, rhs: &TArray) -> bool {
    match (lhs, rhs) {
        (TArray::List(lhs), TArray::List(rhs)) => {
            if !lhs.element_type.is_never()
                || !rhs.element_type.is_never()
                || lhs.known_count != rhs.known_count
                || lhs.known_count.is_none()
            {
                return false;
            }

            let (Some(lhs_elements), Some(rhs_elements)) = (&lhs.known_elements, &rhs.known_elements) else {
                return false;
            };

            lhs_elements.len() == rhs_elements.len()
                && lhs_elements.iter().zip(rhs_elements).all(
                    |((lhs_offset, (lhs_optional, lhs_type)), (rhs_offset, (rhs_optional, rhs_type)))| {
                        lhs_offset == rhs_offset
                            && !lhs_optional
                            && !rhs_optional
                            && is_always_identical_to(lhs_type, rhs_type)
                    },
                )
        }
        (TArray::Keyed(lhs), TArray::Keyed(rhs)) => {
            if lhs.parameters.is_some() || rhs.parameters.is_some() {
                return false;
            }

            let lhs_items = lhs.known_items.as_ref().filter(|items| !items.is_empty());
            let rhs_items = rhs.known_items.as_ref().filter(|items| !items.is_empty());

            match (lhs_items, rhs_items) {
                (None, None) => !lhs.non_empty && !rhs.non_empty,
                (Some(lhs_items), Some(rhs_items)) if lhs_items.len() <= 1 && rhs_items.len() <= 1 => {
                    lhs_items.len() == rhs_items.len()
                        && lhs_items.iter().zip(rhs_items).all(
                            |((lhs_key, (lhs_optional, lhs_type)), (rhs_key, (rhs_optional, rhs_type)))| {
                                lhs_key == rhs_key
                                    && !lhs_optional
                                    && !rhs_optional
                                    && is_always_identical_to(lhs_type, rhs_type)
                            },
                        )
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Checks if two types are guaranteed to be non-equal under PHP's loose equality (`==`).
///
/// Loose equality performs type juggling that can make values of different types compare
/// equal (e.g. `0 == "0"`, `0 == false`, `5 == 5.0`, `"10" == "1e1"`). This function is
/// a safe approximation that handles primitive categories and literal integer-string pairs.
pub fn are_definitely_not_loosely_equal(
    codebase: &CodebaseMetadata,
    php_version: PHPVersion,
    lhs: &TUnion,
    rhs: &TUnion,
) -> bool {
    if let Some(loosely_equal) = compare_literal_int_and_string(php_version, lhs, rhs) {
        return !loosely_equal;
    }

    if (lhs.is_int() && rhs.is_int())
        || (lhs.is_bool() && rhs.is_bool())
        || (lhs.is_float() && rhs.is_float())
        || (lhs.is_null() && rhs.is_null())
    {
        are_definitely_not_identical(codebase, lhs, rhs, false)
    } else {
        false
    }
}

pub fn are_definitely_loosely_equal(php_version: PHPVersion, lhs: &TUnion, rhs: &TUnion) -> bool {
    compare_literal_int_and_string(php_version, lhs, rhs) == Some(true)
}

fn compare_literal_int_and_string(php_version: PHPVersion, lhs: &TUnion, rhs: &TUnion) -> Option<bool> {
    if php_version < PHPVersion::PHP80 {
        return None;
    }

    if let (Some(integer), Some(string)) = (lhs.get_single_literal_int_value(), rhs.get_single_literal_string_value()) {
        return Some(numeric_string_equals_int(string, integer));
    }

    if let (Some(string), Some(integer)) = (lhs.get_single_literal_string_value(), rhs.get_single_literal_int_value()) {
        return Some(numeric_string_equals_int(string, integer));
    }

    None
}

pub fn are_definitely_not_identical(
    codebase: &CodebaseMetadata,
    lhs: &TUnion,
    rhs: &TUnion,
    allow_type_coercion: bool,
) -> bool {
    // If either type is mixed, we cannot determine non-identity.
    if lhs.has_mixed() || lhs.has_mixed_template() || rhs.has_mixed() || rhs.has_mixed_template() {
        return false;
    }

    if !can_expression_types_be_identical(codebase, lhs, rhs, true, allow_type_coercion) {
        return true;
    }

    if (lhs.is_never() && !rhs.is_never()) || (!lhs.is_never() && rhs.is_never()) {
        return true;
    }

    if lhs.is_enum() && rhs.is_enum() {
        let left_cases = lhs.get_enum_cases();
        let right_cases = rhs.get_enum_cases();

        if left_cases.len() == 1
            && right_cases.len() == 1
            && let (left_enum, Some(left_case)) = left_cases[0]
            && let (right_enum, Some(right_case)) = right_cases[0]
        {
            return !left_enum.as_bytes().eq_ignore_ascii_case(right_enum.as_bytes()) || left_case != right_case;
        }
    }

    if (lhs.is_null() && (!rhs.is_null() && !rhs.can_be_null()))
        || (rhs.is_null() && (!lhs.is_null() && !lhs.can_be_null()))
    {
        return true;
    }

    if lhs.is_bool() {
        if !rhs.has_bool() {
            return true;
        }

        if rhs.is_true() && lhs.is_false() {
            return true;
        }

        if rhs.is_false() && lhs.is_true() {
            return true;
        }

        return !rhs.has_bool();
    } else if rhs.is_bool() && !lhs.has_bool() {
        return true;
    }
    // neither side is a fixed bool; fall through to literal-value comparisons

    if let Some(l) = lhs.get_single_literal_int_value()
        && let Some(r) = rhs.get_single_literal_int_value()
    {
        l != r
    } else if let Some(l) = lhs.get_single_literal_float_value()
        && let Some(r) = rhs.get_single_literal_float_value()
    {
        l != r
    } else if let Some(l) = lhs.get_single_literal_string_value() {
        if let Some(r) = rhs.get_single_literal_string_value() {
            l != r
        } else if let Some(r) = rhs.get_single_class_string_value() {
            !l.eq_ignore_ascii_case(r.as_bytes())
        } else {
            false
        }
    } else if let Some(r) = rhs.get_single_literal_string_value()
        && let Some(l) = lhs.get_single_class_string_value()
    {
        !r.eq_ignore_ascii_case(l.as_bytes())
    } else {
        false
    }
}
