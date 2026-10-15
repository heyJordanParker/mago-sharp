use mago_allocator::Arena;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Program;
use mago_syntax::cst::ReadonlyHint;
use mago_syntax::walker::Walker;

use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::quoted_source;

/// Spec section 13: `readonly` holds only types PHP# checks, and only a value something can change. So each `readonly`
/// before a type of a PHP# program that reaches a plain PHP class, at its top or in a type argument, is an error, and
/// one before a type that holds no PHP# object changes nothing: an enum, a struct, a function type, and a collection of
/// those, which is copied as a struct is. The semantics checks refuse `readonly` before a keyword type before any class
/// is known.
pub(crate) fn check_readonly_types<A>(context: &mut Context<'_, '_, A>, program: &Program<'_>)
where
    A: Arena,
{
    let mut found = Vec::new();
    ReadonlyTypes.walk_program(program, &mut found);

    for readonly in found {
        let fix = quoted_source(context, readonly.hint.span());
        let mut plain = Vec::new();
        let changes = can_change(context, readonly.hint, &mut plain);
        // The semantics checks judge a keyword type alone, as `readonly int`.
        let written = match readonly.hint {
            Hint::Nullable(nullable) => nullable.hint,
            hint => hint,
        };
        let names_a_type = matches!(
            written,
            Hint::Identifier(_) | Hint::Generic(_) | Hint::Function(_) | Hint::Union(_) | Hint::Parenthesized(_)
        );

        for name in &plain {
            let class = quoted_source(context, *name);
            report(
                context,
                readonly,
                *name,
                format!("`readonly` holds only types PHP# checks, and `{class}` is a plain PHP class: write `{fix}`."),
            );
        }
        if plain.is_empty() && !changes && names_a_type {
            report(
                context,
                readonly,
                readonly.hint.span(),
                format!("A `{fix}` never changes, so `readonly` changes nothing: write `{fix}`."),
            );
        }
    }
}

/// Whether a value of `hint` holds a PHP# object, which a write could change. Each plain PHP class `hint` names, at
/// its top or in a type argument, is pushed to `plain`. A name that is no class, such as a type parameter's, can hold
/// an object.
fn can_change<A>(context: &Context<'_, '_, A>, hint: &Hint<'_>, plain: &mut Vec<Span>) -> bool
where
    A: Arena,
{
    let class = |name: Span, plain: &mut Vec<Span>| {
        let class =
            context.resolved_names.resolve(&name).and_then(|resolved| context.codebase.get_class_like(resolved));
        if class.is_some_and(|class| !class.flags.is_sharp()) {
            plain.push(name);
        }

        class.map(|class| !class.kind.is_enum() && !class.flags.is_struct())
    };

    match hint {
        Hint::Nullable(nullable) => can_change(context, nullable.hint, plain),
        Hint::Parenthesized(parenthesized) => can_change(context, parenthesized.hint, plain),
        Hint::Union(union) => {
            let left = can_change(context, union.left, plain);

            can_change(context, union.right, plain) || left
        }
        Hint::Identifier(identifier) => class(identifier.span(), plain).unwrap_or(true),
        Hint::Generic(generic) => {
            let arguments: Vec<bool> =
                generic.type_arguments.arguments.iter().map(|argument| can_change(context, argument, plain)).collect();

            // A collection's name, as `List`, resolves to no class, and holds what its type arguments hold.
            class(generic.name.span, plain).unwrap_or_else(|| arguments.contains(&true))
        }
        Hint::Function(_)
        | Hint::Integer(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::String(_)
        | Hint::Null(_)
        | Hint::True(_)
        | Hint::False(_)
        | Hint::Void(_)
        | Hint::Never(_) => false,
        _ => true,
    }
}

fn report<A>(context: &mut Context<'_, '_, A>, readonly: &ReadonlyHint<'_>, span: Span, message: String)
where
    A: Arena,
{
    context.collector.report_with_code(
        IssueCode::IncompatibleReadonlyModifier,
        Issue::error(message)
            .with_annotation(Annotation::primary(readonly.readonly.span).with_message("Written here."))
            .with_annotation(Annotation::secondary(span)),
    );
}

/// Collects each `readonly` before a type of a program.
struct ReadonlyTypes;

impl<'ast, 'arena> Walker<'ast, 'arena, Vec<&'ast ReadonlyHint<'arena>>> for ReadonlyTypes {
    fn walk_in_readonly_hint(&self, readonly: &'ast ReadonlyHint<'arena>, found: &mut Vec<&'ast ReadonlyHint<'arena>>) {
        found.push(readonly);
    }
}
