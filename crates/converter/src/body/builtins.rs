//! A bare call of a PHP built-in as the standard library member that runs that function with PHP's meaning, as
//! `count($items)` is `items.count()` (decision 4). A member fits a call when every literal and constant it passes is
//! the call's own, its receiver and parameters take the call's arguments, it [`differs`] in nothing, and a value it
//! reads out of the call's order runs nothing. Any other call keeps the file plain PHP, with the difference named.

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by_with_erased_template_arguments;
use mago_codex::ttype::get_mixed;
use mago_sharp_bridge::PhpCallArgument;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;

use super::is_primary;
use crate::Converter;
use crate::library::Member;
use crate::library::Shape;
use crate::library::differs;
use crate::library::short;

impl Converter<'_, '_> {
    /// `call` of the built-in `function`, named `name`, as the first standard library member that fits it.
    pub(super) fn built_in(&mut self, call: &FunctionCall<'_>, function: &FunctionLikeMetadata, name: &str) {
        let members = self.input.library.members(name.as_bytes());
        if members.is_empty() {
            return self.refuse(
                call.span(),
                format!("`{name}` has no PHP# form: no standard library member runs `{name}` with PHP's meaning (decision 083)"),
            );
        }
        let mut reasons = Vec::new();
        for member in members {
            match self.fits(member, function, call) {
                Ok(slots) => return self.member_call(call, member, &slots),
                Err(reason) => {
                    reasons.push(format!("`{}.{}` {reason}", short(&member.class.as_str_lossy()), member.name));
                }
            }
        }

        self.refuse(call.span(), format!("`{name}` has no PHP# form here: {}", reasons.join(", and ")));
    }

    /// The call's argument in each of `member`'s slots, when `member` runs `function` with PHP's meaning for `call`, or
    /// how it doesn't.
    fn fits<'call, 'arena>(
        &self,
        member: &Member,
        function: &FunctionLikeMetadata,
        call: &'call FunctionCall<'arena>,
    ) -> Result<Vec<&'call Expression<'arena>>, String> {
        let codebase = self.input.codebase;
        if let Some(reason) = differs(member, function, codebase) {
            return Err(reason);
        }
        let mut arguments = Vec::new();
        for argument in call.argument_list.arguments.iter() {
            match argument {
                Argument::Positional(argument) if argument.ellipsis.is_none() => arguments.push(argument.value),
                _ => return Err("takes no named or spread argument".to_owned()),
            }
        }
        if arguments.len() != member.call.arguments.len() {
            return Err(format!(
                "passes {} arguments, where this call passes {}",
                member.call.arguments.len(),
                arguments.len()
            ));
        }

        let mut slots: Vec<Option<&Expression<'_>>> = Vec::new();
        for (argument, passed) in arguments.into_iter().zip(&member.call.arguments) {
            match passed {
                PhpCallArgument::Slot(slot) => {
                    if slots.len() <= *slot {
                        slots.resize(slot + 1, None);
                    }
                    slots[*slot] = Some(argument);
                }
                PhpCallArgument::Literal(literal) => {
                    let own = matches!(argument, Expression::Literal(written) if PhpCallArgument::literal(written) == *passed);
                    if !own {
                        return Err(format!(
                            "passes `{}` where this call passes another value",
                            String::from_utf8_lossy(literal)
                        ));
                    }
                }
                PhpCallArgument::Constant(constant) => {
                    let own = matches!(argument, Expression::ConstantAccess(written) if codebase
                        .get_constant(self.input.names.get(&written.name))
                        .or_else(|| codebase.get_constant(written.name.value()))
                        .is_some_and(|metadata| metadata.name.as_bytes().eq_ignore_ascii_case(constant.as_bytes())));
                    if !own {
                        return Err(format!("passes `{constant}` where this call passes another value"));
                    }
                }
            }
        }
        let slots: Vec<&Expression<'_>> = slots
            .into_iter()
            .collect::<Option<_>>()
            .ok_or_else(|| "reads a value this call doesn't pass".to_owned())?;

        let method = codebase
            .get_method(member.class.as_bytes(), member.name.as_bytes())
            .ok_or_else(|| "has no declaration in the analysis".to_owned())?;
        for (index, (argument, parameter)) in slots.iter().zip(&method.parameters).enumerate() {
            let declared = parameter.get_type_metadata().map_or_else(get_mixed, |metadata| metadata.type_union.clone());
            let passed = self.type_of(argument).cloned().unwrap_or_else(get_mixed);
            if !is_contained_by_with_erased_template_arguments(
                codebase,
                &passed,
                &declared,
                &mut ComparisonResult::new(),
            ) {
                return Err(format!("takes `{}` where this call passes `{}`", declared.get_id(), passed.get_id()));
            }
            // A value read after the form's first call, or out of order, runs in the form's order, not the call's.
            if index >= member.in_order && !runs_nothing(argument) {
                return Err("reads this call's arguments in another order, and one of them runs code".to_owned());
            }
        }

        Ok(slots)
    }

    /// `call` as `member`, with `slots` in its slots: `Class.name(…)`, `receiver.name(…)` or `receiver.name`.
    fn member_call(&mut self, call: &FunctionCall<'_>, member: &Member, slots: &[&Expression<'_>]) {
        let mut values = Vec::new();
        for slot in slots {
            self.expression(slot, None);
            values.push(self.render(slot));
        }
        let receiver = |slot: &Expression<'_>, value: &str| {
            if is_primary(slot) { value.to_owned() } else { format!("({value})") }
        };
        let text = match (member.shape, slots.split_first()) {
            (Shape::Static, _) => match self.imports.name_of(&member.class.as_str_lossy()) {
                Ok(class) => format!("{class}.{}({})", member.name, values.join(", ")),
                Err(reason) => return self.refuse(call.span(), reason),
            },
            (Shape::Method, Some((slot, _))) => {
                format!("{}.{}({})", receiver(slot, &values[0]), member.name, values[1..].join(", "))
            }
            (Shape::Property, Some((slot, _))) => format!("{}.{}", receiver(slot, &values[0]), member.name),
            (_, None) => return self.refuse(call.span(), "an extension member with no receiver has no call"),
        };
        self.edits.replace(call.span(), text);
    }
}

/// Whether reading `argument` runs nothing: a variable, a literal, a constant or a property read.
fn runs_nothing(argument: &Expression<'_>) -> bool {
    match argument {
        Expression::Parenthesized(parenthesized) => runs_nothing(parenthesized.expression),
        Expression::Variable(_) | Expression::Literal(_) | Expression::ConstantAccess(_) => true,
        Expression::Access(Access::Property(access)) => runs_nothing(access.object),
        Expression::Access(Access::NullSafeProperty(access)) => runs_nothing(access.object),
        Expression::Access(Access::ClassConstant(_) | Access::StaticProperty(_)) => true,
        _ => false,
    }
}
