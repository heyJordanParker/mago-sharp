//! A bare call of a PHP built-in as the standard library member that runs that function with PHP's meaning, as
//! `count($items)` is `items.count()` (decision 4). A member fits a call when every literal and constant it passes is
//! the call's own, its receiver and parameters take the call's arguments, or a trailing parameter's default is PHP's
//! own default where the call leaves the argument out, it [`differs`] in nothing, and a value it reads out of the
//! call's order runs nothing. Any other call keeps the file plain PHP, with the difference named. A built-in as a value
//! is the lambda that makes such a call with its parameters.

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by_with_erased_template_arguments;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_string;
use mago_codex::ttype::takes_bytes;
use mago_codex::ttype::union::TUnion;
use mago_sharp_bridge::PhpCallArgument;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::token::is_sharp_keyword;
use mago_word::word;

use super::is_primary;
use crate::Converter;
use crate::library::Member;
use crate::library::Shape;
use crate::library::differs;
use crate::library::short;

/// A value a call of a built-in passes: the call's argument, or a parameter of the lambda a built-in's value becomes,
/// which is no literal and runs nothing, of the type `union`.
struct Passed<'call, 'arena> {
    argument: Option<&'call Expression<'arena>>,
    union: TUnion,
}

impl<'input> Converter<'input, '_> {
    /// `call` of the built-in `function`, named `name`, as the first standard library member that fits it.
    pub(super) fn built_in(&mut self, call: &FunctionCall<'_>, function: &FunctionLikeMetadata, name: &str) {
        let passed: Option<Vec<Passed<'_, '_>>> = call
            .argument_list
            .arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(argument) if argument.ellipsis.is_none() => Some(Passed {
                    argument: Some(argument.value),
                    union: self.type_of(argument.value).cloned().unwrap_or_else(get_mixed),
                }),
                _ => None,
            })
            .collect();
        let found = self.fitting(call.span(), function, name, passed.as_deref());
        let Some((member, slots)) = found else {
            return;
        };
        let mut values = Vec::new();
        for (slot, as_bytes) in slots {
            let Some(argument) = passed.as_ref().and_then(|passed| passed[slot].argument) else {
                return;
            };
            self.expression(argument, None);
            values.push(slot_value(self.render(argument), is_primary(argument), as_bytes));
        }
        match self.member_text(member, &values) {
            Ok(text) => self.edits.replace(call.span(), text),
            Err(reason) => self.refuse(call.span(), reason),
        }
    }

    /// `name(...)`, the built-in `function` as a value at `span`, as the lambda that calls the first standard library
    /// member that fits a call of it with as many arguments as `expected`, the one function type it lands in, passes,
    /// as `(string value) => value.trim()` for `mb_trim(...)` a `Function<string(string)>` takes (spec section 14.2).
    /// PHP calls the function with each argument the function type passes, so the lambda takes each, of the type and
    /// with the name the function gives its parameter there, or `value` where the name is no PHP# one or its type's.
    pub(super) fn built_in_value(
        &mut self,
        span: Span,
        function: &FunctionLikeMetadata,
        name: &str,
        expected: Option<&TUnion>,
    ) {
        let signatures: Vec<&TCallableSignature> = expected
            .into_iter()
            .flat_map(|union| union.types.iter())
            .filter_map(|atomic| match atomic {
                TAtomic::Callable(TCallable::Signature(signature)) => Some(signature),
                _ => None,
            })
            .collect();
        let [signature] = signatures.as_slice() else {
            return self.refuse(
                span,
                format!("`{name}(...)` has no PHP# form here: PHP# writes PHP's own function as a value as a lambda that calls the standard library member that runs it, with as many parameters as the one function type it lands in passes, and no declaration gives one (spec section 14.2)"),
            );
        };
        let given = signature.get_parameters();
        if given.iter().any(|parameter| parameter.has_default() || parameter.is_variadic()) {
            return self.refuse(
                span,
                format!("`{name}(...)` lands in a function type with an optional or a variadic parameter, so the count of arguments PHP passes it is unknown, which the lambda PHP# writes for it takes (spec section 14.1)"),
            );
        }
        let takes = &function.parameters;
        let variadic = takes.last().is_some_and(|parameter| parameter.flags.is_variadic());
        if given.len() > takes.len() && !variadic {
            return self.refuse(
                span,
                format!("`{name}(...)` lands in a function type that passes {} arguments, more than `{name}` takes, which PHP refuses with an `ArgumentCountError` where it calls it", given.len()),
            );
        }
        let mut chosen_names: Vec<String> = Vec::new();
        let mut written = Vec::new();
        let mut lambda_parameters = Vec::new();
        for position in 0..given.len() {
            let Some(parameter) = takes.get(position).or(takes.last()) else {
                return;
            };
            let union = parameter.get_type_metadata().map_or_else(get_mixed, |declared| declared.type_union.clone());
            let Some(text) = self.type_text(&union, span) else {
                return;
            };
            // The function's name of the parameter, where it is a camel case PHP# name (spec section 24) other than
            // its type's, as `trim`'s `$string` is.
            let own = String::from_utf8_lossy(parameter.get_name().0.as_bytes()).trim_start_matches('$').to_owned();
            let fits = mago_casing::is_camel_case(own.as_bytes()) && !is_sharp_keyword(own.as_bytes()) && own != text;
            let chosen = self.unused_name(if fits { &own } else { "value" }, &chosen_names);
            written.push(format!("{text} {chosen}"));
            chosen_names.push(chosen);
            lambda_parameters.push(Passed { argument: None, union });
        }
        let Some((member, slots)) = self.fitting(span, function, name, Some(&lambda_parameters)) else {
            return;
        };
        let values: Vec<(String, bool)> =
            slots.iter().map(|(slot, as_bytes)| slot_value(chosen_names[*slot].clone(), true, *as_bytes)).collect();
        match self.member_text(member, &values) {
            Ok(text) => self.edits.replace(span, format!("({}) => {text}", written.join(", "))),
            Err(reason) => self.refuse(span, reason),
        }
    }

    /// The first standard library member that runs the built-in `function`, named `name`, with PHP's meaning for a call
    /// that passes `passed`, with the place in `passed` of the value in each of its slots and whether the slot takes it
    /// as `Bytes`, as [`Converter::fits`] says, or a refusal at `span` that names how each member doesn't. A member
    /// that fits the call but for the literal its body needs runs the function with this call's values, so its reason
    /// alone is the call's, and a member that passes other values says nothing of it. `passed` is `None` for a call
    /// with a named or spread argument.
    fn fitting(
        &mut self,
        span: Span,
        function: &FunctionLikeMetadata,
        name: &str,
        passed: Option<&[Passed<'_, '_>]>,
    ) -> Option<(&'input Member, Vec<(usize, bool)>)> {
        let members = self.input.library.members(name.as_bytes());
        if members.is_empty() {
            self.refuse(
                span,
                format!("`{name}` has no PHP# form: no standard library member runs `{name}` with PHP's meaning (decision 083)"),
            );
            return None;
        }
        let (mut reasons, mut unfolded) = (Vec::new(), Vec::new());
        for member in members {
            let shown = format!("`{}.{}`", short(&member.class.as_str_lossy()), member.name);
            match self.fits(member, function, passed) {
                Ok(slots) => match passed.and_then(|passed| literal_needed(member, passed, &slots)) {
                    None => return Some((member, slots)),
                    Some(reason) => unfolded.push(format!("{shown} {reason}")),
                },
                Err(reason) => reasons.push(format!("{shown} {reason}")),
            }
        }
        let reasons = if unfolded.is_empty() { reasons } else { unfolded };
        self.refuse(span, format!("`{name}` has no PHP# form here: {}", reasons.join(", and ")));

        None
    }

    /// The place in `passed` of the value in each of `member`'s slots, when `member` runs `function` with PHP's meaning
    /// for a call that passes `passed`, or how it doesn't, with whether the slot is a `Bytes` that takes the call's
    /// `string` as its bytes, as `md5($data)` is `Hash.md5(data.toBytes())`: the member passes PHP the `Bytes`'s string,
    /// which holds the text's bytes (spec section 18).
    fn fits(
        &self,
        member: &Member,
        function: &FunctionLikeMetadata,
        passed: Option<&[Passed<'_, '_>]>,
    ) -> Result<Vec<(usize, bool)>, String> {
        let codebase = self.input.codebase;
        if let Some(reason) = differs(member, function, codebase) {
            return Err(reason);
        }
        let arguments = passed.ok_or_else(|| "takes no named or spread argument".to_owned())?;
        let method = codebase
            .get_method(member.class, member.name)
            .ok_or_else(|| "has no declaration in the analysis".to_owned())?;
        // The member's call of `function` may pass more than this call does, where each argument this call leaves out
        // is a parameter of the member with the default PHP's parameter has, as `x.trim()` passes `mb_trim` the `null`
        // of `trim(string? characters = null)`.
        let default = |parameter: Option<&FunctionLikeParameterMetadata>| {
            parameter
                .and_then(FunctionLikeParameterMetadata::get_default_type)
                .map(|default| default.type_union.get_id())
        };
        let left_out = member.call.arguments.get(arguments.len()..).unwrap_or_default();
        let defaulted = left_out.iter().zip(arguments.len()..).all(|(passed, position)| {
            matches!(passed, PhpCallArgument::Slot(slot) if default(method.parameters.get(*slot)).is_some_and(|ours| Some(ours) == default(function.parameters.get(position))))
        });
        if arguments.len() > member.call.arguments.len() || !defaulted {
            return Err(format!(
                "passes {} arguments, where this call passes {}",
                member.call.arguments.len(),
                arguments.len()
            ));
        }

        let mut slots: Vec<Option<usize>> = Vec::new();
        for (index, (argument, passed)) in arguments.iter().zip(&member.call.arguments).enumerate() {
            match passed {
                PhpCallArgument::Slot(slot) => {
                    if slots.len() <= *slot {
                        slots.resize(slot + 1, None);
                    }
                    slots[*slot] = Some(index);
                }
                PhpCallArgument::Literal(literal) => {
                    let own = matches!(argument.argument, Some(Expression::Literal(written)) if PhpCallArgument::literal(written) == *passed);
                    if !own {
                        return Err(format!(
                            "passes `{}` where this call passes another value",
                            String::from_utf8_lossy(literal)
                        ));
                    }
                }
                PhpCallArgument::Constant(constant) => {
                    let own = matches!(argument.argument, Some(Expression::ConstantAccess(written)) if codebase
                        .get_constant(word(self.input.names.get(&written.name)))
                        .or_else(|| codebase.get_constant(word(written.name.value())))
                        .is_some_and(|metadata| metadata.name.as_bytes().eq_ignore_ascii_case(constant.as_bytes())));
                    if !own {
                        return Err(format!("passes `{constant}` where this call passes another value"));
                    }
                }
            }
        }
        let slots: Vec<usize> = slots
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(|| "reads a value this call doesn't pass".to_owned())?;

        let mut taken = Vec::with_capacity(slots.len());
        for (index, (slot, parameter)) in slots.iter().zip(&method.parameters).enumerate() {
            let declared = parameter.get_type_metadata().map_or_else(get_mixed, |metadata| metadata.type_union.clone());
            let passed = &arguments[*slot].union;
            let fits = |container: &TUnion| {
                is_contained_by_with_erased_template_arguments(
                    codebase,
                    passed,
                    container,
                    &mut ComparisonResult::new(),
                )
            };
            let as_bytes = takes_bytes(&declared) && fits(&get_string());
            if !as_bytes && !fits(&declared) {
                return Err(format!("takes `{}` where this call passes `{}`", declared.get_id(), passed.get_id()));
            }
            // A value read after the form's first call, or out of order, runs in the form's order, not the call's.
            if index >= member.in_order && !arguments[*slot].argument.is_none_or(runs_nothing) {
                return Err("reads this call's arguments in another order, and one of them runs code".to_owned());
            }
            taken.push((*slot, as_bytes));
        }

        Ok(taken)
    }

    /// The call of `member` with `values` in its slots, each in PHP# with whether it binds tighter than any operator:
    /// `Class.name(…)`, `receiver.name(…)` or `receiver.name`.
    fn member_text(&mut self, member: &Member, values: &[(String, bool)]) -> Result<String, String> {
        let receiver = |(value, primary): &(String, bool)| if *primary { value.clone() } else { format!("({value})") };
        let arguments =
            |values: &[(String, bool)]| values.iter().map(|(value, _)| value.as_str()).collect::<Vec<_>>().join(", ");

        match (member.shape, values.split_first()) {
            (Shape::Static, _) => {
                let class = self.imports.name_of(&member.class.as_str_lossy())?;
                Ok(format!("{class}.{}({})", member.name, arguments(values)))
            }
            (Shape::Method, Some((first, rest))) => {
                Ok(format!("{}.{}({})", receiver(first), member.name, arguments(rest)))
            }
            (Shape::Property, Some((first, _))) => Ok(format!("{}.{}", receiver(first), member.name)),
            (_, None) => Err("an extension member with no receiver has no call".to_owned()),
        }
    }
}

/// How `member`, which takes the values a call passes, `passed`, in its `slots`, still runs the function without PHP's
/// meaning: an argument its body folds as a literal, which this call doesn't pass as one that decides the body as the
/// function's meaning needs.
fn literal_needed(member: &Member, passed: &[Passed<'_, '_>], slots: &[(usize, bool)]) -> Option<String> {
    let fixed = member.fixed.iter().find(|fixed| {
        !slots.get(fixed.slot).and_then(|(slot, _)| passed[*slot].argument).is_some_and(|slot| fixed.holds(slot))
    })?;
    let position = member.call.arguments.iter().position(|argument| *argument == PhpCallArgument::Slot(fixed.slot));

    Some(format!(
        "runs it only where argument {} is a literal {} `{}`",
        position.map_or(0, |position| position + 1),
        if fixed.equal { "equal to" } else { "other than" },
        String::from_utf8_lossy(&fixed.literal)
    ))
}

/// `value`, PHP# text that binds tighter than any operator when `primary`, as a slot takes it, with whether the result
/// binds tighter than any operator: as its bytes when `as_bytes`, as `(a ?? b).toBytes()` is, and as it is otherwise.
fn slot_value(value: String, primary: bool, as_bytes: bool) -> (String, bool) {
    match (as_bytes, primary) {
        (false, _) => (value, primary),
        (true, true) => (format!("{value}.toBytes()"), true),
        (true, false) => (format!("({value}).toBytes()"), true),
    }
}

/// Whether reading `argument` runs nothing: a variable, a literal, a constant or a property read.
pub(super) fn runs_nothing(argument: &Expression<'_>) -> bool {
    match argument {
        Expression::Parenthesized(parenthesized) => runs_nothing(parenthesized.expression),
        Expression::Variable(_) | Expression::Literal(_) | Expression::ConstantAccess(_) => true,
        Expression::Access(Access::Property(access)) => runs_nothing(access.object),
        Expression::Access(Access::NullSafeProperty(access)) => runs_nothing(access.object),
        Expression::Access(Access::ClassConstant(_) | Access::StaticProperty(_)) => true,
        _ => false,
    }
}
