use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use std::rc::Rc;

use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::PropertyAccess;
use mago_word::Word;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::binary::logical::merge_short_circuited_assignments;
use crate::resolver::property::resolve_method_value;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::StaticPropertyName;
use crate::utils::expression::get_bare_name_variable_id;

pub mod class_constant_access;
pub mod property_access;
pub mod static_property_access;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for Access<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        match self {
            // As after an invalid call target, the read gets no type, so nothing reports again on what it reads.
            Access::Property(PropertyAccess {
                object, property: ClassLikeMemberSelector::Identifier(name), ..
            }) if report_full_name(context, object, Some(name)) => Ok(()),
            // PHP# reads a member through a class value, `typeof(X).y` or `type.y`, as `Class.y` reads it on the class
            // the value holds, which is PHP's `$type::y`.
            Access::Property(access @ PropertyAccess { property: ClassLikeMemberSelector::Identifier(name), .. })
                if let Some(classes) = class_value_classes(context, block_context, access.object) =>
            {
                analyze_class_value_member(
                    context,
                    block_context,
                    artifacts,
                    access.object,
                    name,
                    access.span(),
                    classes,
                )
            }
            Access::NullSafeProperty(
                access @ NullSafePropertyAccess { property: ClassLikeMemberSelector::Identifier(name), .. },
            ) if let Some(classes) = class_value_classes(context, block_context, access.object) => {
                let (object, span) = (access.object, access.span());

                analyze_null_safe_class_value(
                    context,
                    block_context,
                    artifacts,
                    object,
                    span,
                    |context, block_context, artifacts| {
                        analyze_class_value_member(context, block_context, artifacts, object, name, span, classes)
                    },
                )
            }
            // PHP# writes both `Class::NAME` and `Class::$name` as `Class.name`, with a bare name bound to a class.
            // Like the engine, a read is the constant or enum case when the class has one by that name, then the
            // static property, then the static method as a closure, as `Class::name(...)`. A constant expression
            // reads only the constant or enum case, as PHP's does.
            Access::Property(access) => match StaticProperty::from_property_access(access, context.resolved_names) {
                Some(StaticProperty {
                    class: class @ Expression::ConstantAccess(class_name),
                    name: StaticPropertyName::Identifier(name),
                    span,
                }) if block_context.flags.inside_constant_expression()
                    || context
                        .codebase
                        .class_constant_exists(context.resolved_names.get(&class_name.name), name.value) =>
                {
                    class_constant_access::analyze_class_constant_access(
                        context,
                        block_context,
                        artifacts,
                        class,
                        &ClassLikeConstantSelector::Identifier(*name),
                        span,
                    )
                }
                Some(StaticProperty {
                    class: Expression::ConstantAccess(class_name),
                    name: StaticPropertyName::Identifier(name),
                    span,
                }) if is_static_method_value(context, context.resolved_names.get(&class_name.name), name.value) => {
                    let class_id = word(context.resolved_names.get(&class_name.name));
                    let method_type =
                        resolve_method_value(context, block_context, artifacts, class_id, word(name.value), span)
                            .unwrap_or_else(get_mixed);
                    artifacts.set_expression_type(self, method_type);

                    Ok(())
                }
                Some(static_property) => static_property_access::analyze_static_property_access(
                    context,
                    block_context,
                    artifacts,
                    static_property,
                ),
                None => {
                    access.analyze(context, block_context, artifacts)?;
                    report_member_of_mixed_kinds(context, artifacts, access.object, &access.property, false);

                    Ok(())
                }
            },
            Access::NullSafeProperty(access) => {
                access.analyze(context, block_context, artifacts)?;
                report_member_of_mixed_kinds(context, artifacts, access.object, &access.property, false);

                Ok(())
            }
            Access::StaticProperty(access) => access.analyze(context, block_context, artifacts),
            Access::ClassConstant(access) => access.analyze(context, block_context, artifacts),
        }
    }
}

/// Reports a full name in a PHP# file's code, as in `App.Shared.Money.of(1)`, which spec section 23 keeps in `import`
/// lines. The binder reads the root of a chain of member names as a class, so the chain is a full name when its root
/// names no class and the root with the names after it, read as one name, names a class. One file cannot tell
/// `App.Status` from `Status.Active`, so the analyzer makes this refusal for the checker.
///
/// `object` is the chain before `last`, its last member name, which a method call does not pass. It runs at the
/// chain's outermost member, which the analysis reaches first, and returns whether it reported, so the caller analyzes
/// nothing inside the chain.
pub(crate) fn report_full_name<A>(
    context: &mut Context<'_, '_, A>,
    object: &Expression<'_>,
    last: Option<&LocalIdentifier<'_>>,
) -> bool
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return false;
    }

    let mut names: Vec<&LocalIdentifier<'_>> = last.into_iter().collect();
    let mut object = object;
    while let Expression::Access(Access::Property(property)) = object
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        names.push(name);
        object = property.object;
    }

    let Expression::ConstantAccess(root) = object else {
        return false;
    };

    if context.resolved_names.binding(&root.name) != Some(Binding::Class)
        || context.resolved_names.is_imported(&root.name)
        || context.codebase.class_like_exists(context.resolved_names.get(&root.name))
    {
        return false;
    }

    let mut full_name = root.name.value().to_vec();
    for name in names.iter().rev() {
        full_name.push(b'\\');
        full_name.extend_from_slice(name.value);
        if !context.codebase.class_like_exists(&full_name) {
            continue;
        }

        let class = BytesDisplay(name.value);
        let full_name = String::from_utf8_lossy(&full_name).replace('\\', ".");
        context.collector.report_with_code(
            IssueCode::NonExistentClassLike,
            Issue::error(format!(
                "Full names appear only in `import` lines: add `import {full_name};` and write `{class}`."
            ))
            .with_annotation(Annotation::primary(root.span().join(name.span)).with_message("Full name used here."))
            .with_help(
                "In code, `.` is always member access, so a class is written by the short name its import brings in.",
            ),
        );

        return true;
    }

    false
}

/// Reports a PHP# member read or call whose member is a method on some classes the receiver can be and a property on
/// others. The lowering reads a method as its closure and calls a property's function, so it needs one kind of member
/// on every class. `is_call` says whether `object.member(…)` calls it.
pub(crate) fn report_member_of_mixed_kinds<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    object: &Expression<'_>,
    member: &ClassLikeMemberSelector<'_>,
    is_call: bool,
) where
    A: Arena,
{
    let ClassLikeMemberSelector::Identifier(name) = member else {
        return;
    };
    if !context.dialect.is_sharp() {
        return;
    }
    let Some(receiver) = artifacts.get_expression_type(object) else {
        return;
    };

    // The lowering's order: a read takes a declared property before a method, and a call a declared method before a
    // property, with `__call` last.
    let codebase = context.codebase;
    let property = concat_word!("$", name.value);
    let mut kinds = Vec::new();
    for atomic in receiver.types.iter().filter(|atomic| !atomic.is_null()) {
        let Some(class) = atomic.get_object_or_enum_name() else {
            return;
        };
        let declares_property = codebase.get_declaring_property(class.as_bytes(), property.as_bytes()).is_some();
        let declares_method = codebase.get_declaring_method(class.as_bytes(), name.value).is_some();
        let is_method = if is_call {
            declares_method || (!declares_property && codebase.method_exists(class.as_bytes(), b"__call"))
        } else {
            declares_method && !declares_property
        };
        kinds.push((usize::from(!is_method), class));
    }

    let (code, kind_names) = if is_call {
        (IssueCode::AmbiguousObjectMethodAccess, ["method", "function in a property"])
    } else {
        (IssueCode::AmbiguousObjectPropertyAccess, ["method", "property"])
    };
    let Some(issue) = mixed_kinds_issue(context, object, name, is_call, &kind_names, kinds) else {
        return;
    };
    let (receiver, class) = (receiver_text(context, object), issue.1);
    let local = class.to_lowercase();

    context.collector.report_with_code(
        code,
        issue
            .0
            .with_help(format!("Narrow `{receiver}` to one class first, as in `if ({receiver} is {class} {local})`.")),
    );
}

/// The error for a member that is not one kind on every class the receiver can be, with the short name of the first
/// class, or none when every class gives one kind. `kinds` pairs each class with its kind, an index into `kind_names`,
/// and the message names each kind's classes once, in alphabetical order, as a union keeps no written order.
fn mixed_kinds_issue<A>(
    context: &Context<'_, '_, A>,
    object: &Expression<'_>,
    name: &LocalIdentifier<'_>,
    is_call: bool,
    kind_names: &[&str],
    mut kinds: Vec<(usize, Word)>,
) -> Option<(Issue, String)>
where
    A: Arena,
{
    kinds.sort_unstable_by(|(a_kind, a), (b_kind, b)| a_kind.cmp(b_kind).then(a.as_bytes().cmp(b.as_bytes())));
    kinds.dedup();
    let (first_kind, first_class) = *kinds.first()?;
    if kinds.iter().all(|(kind, _)| *kind == first_kind) {
        return None;
    }

    let parts: Vec<String> = kind_names
        .iter()
        .enumerate()
        .filter_map(|(index, kind_name)| {
            let classes: Vec<Word> = kinds.iter().filter(|(kind, _)| *kind == index).map(|(_, class)| *class).collect();

            (!classes.is_empty()).then(|| format!("a {kind_name} on {}", and_list(&classes)))
        })
        .collect();
    let (last, rest) = parts.split_last()?;
    let receiver = receiver_text(context, object);
    let member = BytesDisplay(name.value);
    let message = if is_call {
        format!("`{receiver}.{member}()` calls {} but {last}, so PHP# cannot tell how to call it.", rest.join(", "))
    } else {
        format!("`{receiver}.{member}` is {} but {last}, so PHP# cannot tell how to read it.", rest.join(", "))
    };
    let class = first_class.as_bytes().rsplit(|byte| *byte == b'\\').next().unwrap_or_default();

    Some((
        Issue::error(message)
            .with_annotation(Annotation::primary(name.span).with_message("Not the same kind of member on every class")),
        String::from_utf8_lossy(class).into_owned(),
    ))
}

/// The source text of `object`, as the message names it.
fn receiver_text<A>(context: &Context<'_, '_, A>, object: &Expression<'_>) -> String
where
    A: Arena,
{
    String::from_utf8_lossy(&context.source_file.contents[object.span().to_range_usize()]).into_owned()
}

/// The class names `classes` as an English list, each in backticks: "`A`", "`A` and `B`", "`A`, `B` and `C`".
fn and_list(classes: &[Word]) -> String {
    let names: Vec<String> = classes.iter().map(|class| format!("`{}`", BytesDisplay(class.as_bytes()))).collect();

    match names.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

/// The classes `object` holds when it is a PHP# class value: `typeof(X)`, or a local holding the class-string of a
/// class. A `Class<T>` value holds `T` or a subclass, which has the same kind of member, so `T` stands for it.
pub(crate) fn class_value_classes<A>(
    context: &Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    object: &Expression<'_>,
) -> Option<Vec<Word>>
where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return None;
    }

    match object {
        Expression::TypeOf(type_of) => Some(vec![word(context.resolved_names.get(&type_of.class))]),
        Expression::ConstantAccess(local) => {
            let local = block_context.locals.get(&get_bare_name_variable_id(&local.name, context.resolved_names)?)?;
            let classes = local
                .types
                .iter()
                .filter(|atomic| !atomic.is_null())
                .map(|atomic| match atomic {
                    TAtomic::Scalar(TScalar::ClassLikeString(class)) => {
                        class.literal_value().or_else(|| class.constraint()?.get_object_or_enum_name())
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;

            (!classes.is_empty()).then_some(classes)
        }
        _ => None,
    }
}

/// Analyzes PHP#'s `type.name` read through a class value as `Class.y` reads it on the classes the value holds: the
/// constant or enum case, then the static method as a closure, then the static property, as PHP's `$type::name`.
fn analyze_class_value_member<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &'ast Expression<'arena>,
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    classes: Vec<Word>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    const CONSTANT: usize = 0;
    const STATIC_PROPERTY: usize = 1;
    const STATIC_METHOD: usize = 2;

    let kinds: Vec<(usize, Word)> = classes
        .iter()
        .map(|class| {
            let kind = if context.codebase.class_constant_exists(class.as_bytes(), name.value) {
                CONSTANT
            } else if is_static_method_value(context, class.as_bytes(), name.value) {
                STATIC_METHOD
            } else {
                STATIC_PROPERTY
            };

            (kind, *class)
        })
        .collect();
    let kind_names = ["constant", "static property", "static method"];
    if let Some((issue, _)) = mixed_kinds_issue(context, object, name, false, &kind_names, kinds.clone()) {
        context.collector.report_with_code(IssueCode::AmbiguousClassLikeConstantAccess, issue);
        artifacts.set_expression_type(&span, get_never());

        return Ok(());
    }

    match kinds[0].0 {
        CONSTANT => class_constant_access::analyze_class_constant_access(
            context,
            block_context,
            artifacts,
            object,
            &ClassLikeConstantSelector::Identifier(*name),
            span,
        ),
        STATIC_METHOD => {
            object.analyze(context, block_context, artifacts)?;
            let mut method_type = None;
            for class in classes {
                let value = resolve_method_value(context, block_context, artifacts, class, word(name.value), span)
                    .unwrap_or_else(get_mixed);
                method_type = Some(add_optional_union_type(value, method_type.as_ref(), context.codebase));
            }
            artifacts.set_expression_type(&span, method_type.unwrap_or_else(get_mixed));

            Ok(())
        }
        _ => static_property_access::analyze_static_property_access(
            context,
            block_context,
            artifacts,
            StaticProperty { class: object, name: StaticPropertyName::Identifier(name), span },
        ),
    }
}

/// Analyzes a PHP# null-safe read or call through the class value `object`, `type?.y` or `type?.m()`, with `analyze`.
/// The member is read only when `object` holds a class, so `analyze` runs with the local narrowed to its class values,
/// as `if (type !== null)` narrows it, and the result at `span` can also be the `null` the read stops at. Mago's
/// null-safe property read leaves out a receiver's `null` while it resolves instance members, and has no path that
/// resolves a class value's static members, so the narrowing is this local's.
///
/// A null receiver skips a call's arguments, so the locals afterwards are the ones from before, with each local an
/// argument writes merged as the short-circuited right-hand side of `&&` merges it.
pub(crate) fn analyze_null_safe_class_value<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &Expression<'arena>,
    span: Span,
    analyze: impl FnOnce(
        &mut Context<'ctx, 'arena, A>,
        &mut BlockContext<'ctx>,
        &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let before = block_context.locals.clone();
    let assigned_before = std::mem::take(&mut block_context.assigned_variable_ids);
    if let Expression::ConstantAccess(local) = object
        && let Some(local) = get_bare_name_variable_id(&local.name, context.resolved_names)
        && let Some(local_type) = block_context.locals.get(&local)
    {
        let class_values = local_type.to_non_nullable();
        block_context.locals.insert(local, Rc::new(class_values));
    }

    analyze(context, block_context, artifacts)?;
    let after = std::mem::replace(&mut block_context.locals, before.clone());
    let assigned = std::mem::replace(&mut block_context.assigned_variable_ids, assigned_before);
    merge_short_circuited_assignments(context, block_context, &before, &after, &assigned);
    block_context.assigned_variable_ids.extend(assigned);
    let member_type = artifacts.get_expression_type(&span).cloned().unwrap_or_else(get_mixed);
    artifacts.set_expression_type(&span, member_type.as_nullable());

    Ok(())
}

/// Whether PHP# `Class.name` reads the static method `name`: the class has no static property of that name, which the
/// engine finds first, and its method of that name is static.
fn is_static_method_value<A>(context: &Context<'_, '_, A>, class: &[u8], name: &[u8]) -> bool
where
    A: Arena,
{
    context.codebase.get_declaring_property_class(class, concat_word!("$", name).as_bytes()).is_none()
        && context
            .codebase
            .get_declaring_method(class, name)
            .and_then(|method| method.method_metadata.as_ref())
            .is_some_and(|method| method.is_static)
}
