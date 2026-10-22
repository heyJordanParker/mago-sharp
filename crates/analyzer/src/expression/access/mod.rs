use mago_allocator::Arena;
use std::rc::Rc;

use mago_bytes::BytesDisplay;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::combine_union_types_rc;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::display_sharp_member;
use mago_reporting::Annotation;
use mago_reporting::Issue;

use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::PropertyAccess;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::concat_word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::constant_access::analyze_namespace_constant;
use crate::resolver::class_name::holds_no_class;
use crate::resolver::class_name::report_class_value_of_no_class;
use crate::resolver::class_name::report_full_class_name;
use crate::resolver::class_name::report_unknown_sharp_class;
use crate::resolver::class_name::resolve_classnames_from_expression;
use crate::resolver::class_name::statics_type_arguments;
use crate::resolver::property::resolve_method_value;
use crate::resolver::property::static_member_type_arguments;
use crate::resolver::static_property::StaticProperty;
use crate::resolver::static_property::StaticPropertyName;
use crate::statement::module::check_listed_import;
use crate::utils::expression::get_block_expression_id;
use crate::utils::expression::get_non_nullsafe_expression_id;
use crate::utils::expression::get_nullsafe_base_expressions;
use crate::utils::names::and_list;
use crate::utils::names::display_class_like_name;
use crate::utils::names::member_namespace;
use crate::utils::names::namespace_member;
use crate::visibility::report_private_self_member;
use mago_names::short_name;

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
            // As after an invalid call target, the read is an error, so nothing reports again on what it reads.
            Access::Property(PropertyAccess {
                object, property: ClassLikeMemberSelector::Identifier(name), ..
            }) if report_full_name(context, object, Some(name)) => {
                artifacts.set_expression_type(self, TUnion::from_atomic(TAtomic::Error));

                Ok(())
            }
            // `TItem.y` reads `y` on the class of the type argument the code runs with, any class `TItem`'s bound holds,
            // and `Self.y` on the class the code is called on, spec section 25, as a class value's member is read. The
            // resolution reports a bound that holds a type that is no class.
            Access::Property(
                access @ PropertyAccess { object, property: ClassLikeMemberSelector::Identifier(name), .. },
            ) if match object {
                Expression::ConstantAccess(class) => context.resolved_names.is_type_parameter(&class.name),
                Expression::Self_(_) => context.dialect.is_sharp(),
                _ => false,
            } =>
            {
                let classes: Vec<Word> =
                    resolve_classnames_from_expression(context, block_context, artifacts, access.object, false)?
                        .into_iter()
                        .filter_map(|class| class.fqcn)
                        .collect();
                if classes.is_empty() {
                    artifacts.set_expression_type(&access.span(), get_never());

                    return Ok(());
                }

                // `Self` is no value, so nothing before the member is analyzed.
                let is_self = matches!(object, Expression::Self_(_));
                if is_self
                    && let Some(constant) = context
                        .codebase
                        .get_class_like(classes[0].as_bytes())
                        .and_then(|class| class.constants.get(&word(name.value)))
                    && report_private_self_member(
                        context,
                        classes[0],
                        name.value,
                        constant.visibility.is_private(),
                        access.span(),
                        IssueCode::UnavailableClassConstant,
                    )
                {
                    artifacts.set_expression_type(&access.span(), TUnion::from_atomic(TAtomic::Error));

                    return Ok(());
                }

                analyze_class_value_member(
                    context,
                    block_context,
                    artifacts,
                    access.object,
                    name,
                    access.span(),
                    classes,
                    is_self,
                )
            }
            // PHP# writes both `Class::NAME` and `Class::$name` as `Class.name`, with a bare name bound to a class.
            // Like the engine, a read is the constant or enum case when the class has one by that name, then the
            // static property, then the static method as a closure, as `Class::name(...)`. A constant expression
            // reads only the constant or enum case, as PHP's does, and so does a pattern, which spec section 21 says
            // takes constants only.
            Access::Property(access) => match StaticProperty::from_property_access(access, context.resolved_names) {
                // Spec section 23: `Pdf.DEFAULT_DPI` after `import Vendor.Pdf;` reads a plain PHP constant through its
                // namespace, as a plain PHP function is called through it.
                Some(StaticProperty {
                    class: Expression::ConstantAccess(class_name),
                    name: StaticPropertyName::Identifier(name),
                    ..
                }) if context.dialect.is_sharp()
                    && let Some(namespace) =
                        member_namespace(context.resolved_names, context.codebase, &class_name.name, name.value) =>
                {
                    analyze_namespace_constant(
                        context,
                        block_context,
                        artifacts,
                        access,
                        &class_name.name,
                        name,
                        namespace,
                    );

                    Ok(())
                }
                // A name that is no class, no namespace the file imports and no namespace of a function or constant
                // of that name, as `Nope` in `Nope.render`.
                Some(StaticProperty { class: Expression::ConstantAccess(class_name), .. })
                    if context.dialect.is_sharp()
                        && !context.codebase.class_like_exists(context.resolved_names.get(&class_name.name)) =>
                {
                    report_unknown_class_read(context, artifacts, access, &class_name.name);

                    Ok(())
                }
                Some(StaticProperty { class, name: StaticPropertyName::Identifier(name), span })
                    if let Some(class_name) = context.resolved_names.static_property_class(access)
                        && (block_context.flags.inside_constant_expression()
                            || context.patterns.iter().any(|(_, pattern)| pattern.span().contains(&span.start))
                            || context
                                .codebase
                                .class_constant_exists(context.resolved_names.get(&class_name.name), name.value)) =>
                {
                    class_constant_access::analyze_class_constant_access(
                        context,
                        block_context,
                        artifacts,
                        class,
                        &ClassLikeConstantSelector::Identifier(*name),
                        span,
                        false,
                    )
                }
                Some(StaticProperty { class: class_expression, name: StaticPropertyName::Identifier(name), span })
                    if let Some(class_name) = context.resolved_names.static_property_class(access)
                        && is_static_method_value(
                            context,
                            context.resolved_names.get(&class_name.name),
                            name.value,
                        ) =>
                {
                    // `Counter<int>.bump` names the type arguments the value runs `bump` with.
                    if matches!(class_expression, Expression::GenericClass(_)) {
                        class_expression.analyze(context, block_context, artifacts)?;
                    }
                    let class = word(context.resolved_names.get(&class_name.name));
                    let method_type = resolve_method_value(
                        context,
                        block_context,
                        artifacts,
                        &TObject::new_named(class),
                        None,
                        word(name.value),
                        span,
                        span,
                    )
                    .unwrap_or_else(get_mixed);
                    record_class_type_arguments(context, block_context, artifacts, class_expression, class, name, span);
                    artifacts.set_expression_type(self, method_type);

                    Ok(())
                }
                Some(static_property) => static_property_access::analyze_static_property_access(
                    context,
                    block_context,
                    artifacts,
                    static_property,
                    false,
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
/// names no class and the root with the names after it, read as one name, names a class, or a namespace written in two
/// parts or more that holds a function or constant of the next name, as in `Vendor.Pdf.render(html)`. A root the
/// file imports as a namespace starts the name with the namespace, as `Pdf.Document` after `import Vendor.Pdf;` does.
/// One file cannot tell `App.Status` from `Status.Active`, so the analyzer makes this refusal for the checker.
///
/// `object` is the chain before `last`, its last member name or the method a call calls. It runs at the chain's
/// outermost member, which the analysis reaches first, and returns whether it reported, so the caller analyzes nothing
/// inside the chain.
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

    // `new App.Shared.Money()` reads as the named constructor `Shared` of `App`, as `new Order.fromJson(json)` does,
    // and the rest after it.
    let (root, new) = match object {
        Expression::ConstantAccess(root) if context.resolved_names.binding(&root.name) == Some(Binding::Class) => {
            (&root.name, None)
        }
        // A `new`'s class is a class name with no binding.
        Expression::Identifier(root) => (root, None),
        Expression::Instantiation(
            new @ Instantiation {
                class: Expression::Identifier(root),
                type_arguments: None,
                constructor: Some(constructor),
                argument_list: None,
                initializer: None,
                ..
            },
        ) => {
            names.push(&constructor.name);
            (root, Some(new))
        }
        _ => return false,
    };

    let resolved = context.resolved_names.get(root);
    let reported = !context.codebase.class_like_exists(resolved) && report_full_name_in(context, root, &names);
    // The semantic checks leave a `new` without arguments a chain reads through to this refusal, as
    // `new Order.of.total`, when it is no full name.
    if let Some(new) = new
        && !reported
    {
        let written = BytesDisplay(&context.source_file.contents[new.span().to_range_usize()]);
        context.collector.report_with_code(
            IssueCode::ReplacedBySyntax,
            Issue::error(format!("`{written}` needs parentheses: write `{written}()`."))
                .with_annotation(Annotation::primary(new.span()).with_message("Written here."))
                .with_note("PHP# creates an object with `new Class(arguments)`, parentheses included, or with an object initializer, as in `new Class { name: value }`."),
        );

        return true;
    }

    reported
}

/// Reports the chain `root` and the member `names` after it, innermost last, as [`report_full_name`] describes, when
/// `root` names no class. Returns whether it reported.
fn report_full_name_in<A>(
    context: &mut Context<'_, '_, A>,
    root: &Identifier<'_>,
    names: &[&LocalIdentifier<'_>],
) -> bool
where
    A: Arena,
{
    let resolved = context.resolved_names.get(root);

    // A root the file doesn't import starts a full name, or a name relative to the file's namespace, as `Shop.Order`
    // in `App` names `App.Shop.Order`.
    let starts: Vec<Vec<u8>> = if context.resolved_names.is_imported(root) {
        vec![resolved.to_vec()]
    } else {
        std::iter::once(root.value().to_vec())
            .chain(context.scope.namespace_name().map(|namespace| [namespace, b"\\", root.value()].concat()))
            .collect()
    };

    starts.into_iter().any(|start| report_full_name_from(context, root, start, names))
}

/// Reports the chain that `root` starts, read as one name from `start`, the full name `root` stands for, and the
/// member `names` after it, innermost last, when it names a class-like or a member of a namespace written in two parts
/// or more. Returns whether it reported.
fn report_full_name_from<A>(
    context: &mut Context<'_, '_, A>,
    root: &Identifier<'_>,
    start: Vec<u8>,
    names: &[&LocalIdentifier<'_>],
) -> bool
where
    A: Arena,
{
    let mut full_name = start;
    // `written_parts` counts the parts of `full_name` the code writes: one in `Pdf.render` after `import Vendor.Pdf;`.
    for (written_parts, (index, name)) in (1..).zip(names.iter().enumerate().rev()) {
        let member = [full_name.as_slice(), b"\\", name.value].concat();
        let reached = namespace_member(context.codebase, word(&full_name), name.value);
        if written_parts > 1
            && index == 0
            && (context.codebase.function_exists(reached.as_bytes())
                || context.codebase.constant_exists(reached.as_bytes()))
        {
            // A full name counts as an import, spec section 32.
            if !check_listed_import(context, &member, root.span().join(name.span)) {
                return true;
            }

            let namespace = String::from_utf8_lossy(&full_name).replace('\\', ".");
            let written = display_sharp_member(&full_name, BytesDisplay(name.value));
            context.collector.report_with_code(
                IssueCode::NonExistentClassLike,
                Issue::error(format!(
                    "Full names appear only in `import` lines: add `import {namespace};` and write `{written}`."
                ))
                .with_annotation(Annotation::primary(root.span().join(name.span)).with_message("Full name used here."))
                .with_help("In code, `.` is always member access, so a namespace is written by the last part its import brings in."),
            );

            return true;
        }

        full_name = member;
        if context.codebase.class_like_exists(&full_name) {
            return report_full_class_name(context, &full_name, root.span().join(name.span));
        }
    }

    false
}

/// Reports the PHP# read `read` of `name.member` when `name` names no class, no namespace the file imports, and no
/// namespace with a function or constant of that member's name, so the read reaches nothing. The read is refused, so
/// nothing that uses it reports again.
fn report_unknown_class_read<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    read: &PropertyAccess<'_>,
    name: &Identifier<'_>,
) where
    A: Arena,
{
    artifacts.set_expression_type(read, TUnion::from_atomic(TAtomic::Error));
    report_unknown_member_owner(context, name);
}

/// Reports `name`, written before a member, as in `name.member` and `name.member(…)`, when it names no class, no
/// namespace the file imports, and no namespace with a function or constant of that member's name: what the spec says
/// the code meant, as [`report_unknown_sharp_class`] says, or else the name that names nothing.
pub(crate) fn report_unknown_member_owner<A>(context: &mut Context<'_, '_, A>, name: &Identifier<'_>)
where
    A: Arena,
{
    if report_unknown_sharp_class(context, context.resolved_names.get(name), name.span()) {
        return;
    }

    let written = BytesDisplay(name.value());
    context.collector.report_with_code(
        IssueCode::NonExistentClassLike,
        Issue::error(format!("`{written}` names no class, and no namespace this file imports."))
            .with_annotation(Annotation::primary(name.span()).with_message("Names nothing here."))
            .with_help(format!(
                "Check the spelling of `{written}`, or import the class or the namespace whose member this names."
            )),
    );
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

    // The lowering's order: a read takes a property before a method, and a call a declared method before a property,
    // with `__call` last. A class that has `__get`, which only plain PHP defines, has the properties it reads, and a
    // read the code takes as a function takes the method the analysis recorded over the property, spec section 14.3.
    let codebase = context.codebase;
    let property = concat_word!("$", name.value);
    let read = object.span().join(name.span);
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
            let has_property = declares_property || codebase.method_exists(class.as_bytes(), b"__get");
            let reads_method = artifacts.get_callees(&read).any(|target| {
                matches!(target, CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(method_class, _), .. }
                    if *method_class == class)
            });

            declares_method && (!has_property || reads_method)
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
            let classes: Vec<Word> = kinds
                .iter()
                .filter(|(kind, _)| *kind == index)
                .map(|(_, class)| display_class_like_name(context, *class))
                .collect();

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
    Some((
        Issue::error(message)
            .with_annotation(Annotation::primary(name.span).with_message("Not the same kind of member on every class")),
        short_name(first_class),
    ))
}

/// The source text of `object`, as the message names it.
fn receiver_text<A>(context: &Context<'_, '_, A>, object: &Expression<'_>) -> String
where
    A: Arena,
{
    String::from_utf8_lossy(&context.source_file.contents[object.span().to_range_usize()]).into_owned()
}

/// Spec section 11: a static method of a generic PHP# class runs with its class's type arguments ahead of its own, so
/// the value at `span` of the method `name` of `class`, read through `class_expression`, as `Counter<int>.bump` or
/// `Self.bump`, keeps them, as a call through it does. A class written without its type arguments gives the value none.
fn record_class_type_arguments<A>(
    context: &Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    class_expression: &Expression<'_>,
    class: Word,
    name: &LocalIdentifier<'_>,
    span: Span,
) where
    A: Arena,
{
    let Some(type_arguments) = statics_type_arguments(context, block_context, artifacts, class_expression) else {
        return;
    };
    let method = MethodIdentifier::new(class, ascii_lowercase_word(name.value));
    let declaring_class = context.codebase.get_declaring_method_identifier(&method).get_class_name();
    let Some(class_type_arguments) =
        static_member_type_arguments(context, class, declaring_class, Some(&type_arguments))
    else {
        return;
    };

    let span = (span.start.offset, span.end.offset);
    let own = artifacts.inferred_type_arguments.remove(&span).unwrap_or_default();
    artifacts.inferred_type_arguments.insert(span, class_type_arguments.into_iter().chain(own).collect());
}

/// Analyzes PHP#'s `type.name` read through a class value as `Class.y` reads it on the classes the value holds: the
/// constant or enum case, then the static method as a closure, then the static property, as PHP's `$type::name`. A
/// class value the read analyzed to find it is one is `object_is_analyzed`, and a type parameter, `TItem.name`, is not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn analyze_class_value_member<'ctx, 'ast, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &'ast Expression<'arena>,
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    classes: Vec<Word>,
    object_is_analyzed: bool,
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
            object_is_analyzed,
        ),
        STATIC_METHOD => {
            if !object_is_analyzed {
                object.analyze(context, block_context, artifacts)?;
            }
            let mut method_type = None;
            for class in &classes {
                let class = TObject::new_named(*class);
                let value =
                    resolve_method_value(context, block_context, artifacts, &class, None, word(name.value), span, span)
                        .unwrap_or_else(get_mixed);
                method_type = Some(add_optional_union_type(value, method_type.as_ref(), context.codebase));
            }
            record_class_type_arguments(context, block_context, artifacts, object, classes[0], name, span);
            artifacts.set_expression_type(&span, method_type.unwrap_or_else(get_mixed));

            Ok(())
        }
        _ => static_property_access::analyze_static_property_access(
            context,
            block_context,
            artifacts,
            StaticProperty { class: object, name: StaticPropertyName::Identifier(name), span },
            object_is_analyzed,
        ),
    }
}

/// Analyzes a PHP# read or call through `object`, a class value or a function, which is analyzed, with `analyze`, as
/// the rest of its chain. Through `?.`, `type?.y`, `type?.m()` or `f?.(x)`, the member is reached or the function
/// called only when `object` is not null, so `analyze` also runs with the type of `object` itself as its values that
/// are not null, and the result at `span` can also be the `null` the access stops at. `object` keeps its own type
/// afterwards.
pub(crate) fn analyze_through_value<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &Expression<'arena>,
    is_null_safe: bool,
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
    if !is_null_safe {
        return analyze_rest_of_chain(context, block_context, artifacts, object, false, analyze);
    }

    let Some(object_type) = artifacts.get_rc_expression_type(object).cloned() else {
        unreachable!("a class value's object is analyzed, and its type is how the class value was found");
    };
    artifacts.set_expression_type(object, object_type.to_non_nullable());

    analyze_rest_of_chain(context, block_context, artifacts, object, true, analyze)?;
    artifacts.set_rc_expression_type(object, object_type);
    let member_type = artifacts.get_expression_type(&span).cloned().unwrap_or_else(get_mixed);
    artifacts.set_expression_type(&span, member_type.as_nullable());

    Ok(())
}

/// Analyzes with `analyze` the part of a PHP# chain a null receiver skips: the arguments or the index after `object`,
/// or after `object?.` when `is_null_safe`, and the value a write through `object` stores. Spec section 14.4: a null
/// receiver of `?.` skips the rest of its chain, so `analyze` runs with each such receiver the block tracks, a local, a
/// parameter or a member read before, typed as its values that are not null, as `if (step != null)` narrows it. The
/// part may not run, so the locals afterwards join the ones from before as an `if` with no `else` joins them: a local
/// it changed holds its type from before or its new one, a local it removed stays removed, and a local it added is
/// gone. Nothing the narrowing proved outlives the chain.
pub(crate) fn analyze_rest_of_chain<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    object: &Expression<'arena>,
    is_null_safe: bool,
    analyze: impl FnOnce(
        &mut Context<'ctx, 'arena, A>,
        &mut BlockContext<'ctx>,
        &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let receivers: Vec<&Expression<'arena>> =
        is_null_safe.then_some(object).into_iter().chain(get_nullsafe_base_expressions(object)).collect();
    if !context.dialect.is_sharp() || receivers.is_empty() {
        return analyze(context, block_context, artifacts);
    }

    // A `?.` read stores its member under its own id, as `$step?->spare`, and a `.` read of it takes `$step->spare`.
    let before = block_context.locals.clone();
    let narrowed: Vec<(Word, Rc<TUnion>)> = receivers
        .into_iter()
        .filter_map(|receiver| get_block_expression_id(receiver, context, block_context))
        .filter_map(|id| {
            let not_null = Rc::new(before.get(&id)?.to_non_nullable());
            Some((get_non_nullsafe_expression_id(id).unwrap_or(id), not_null))
        })
        .collect();
    block_context.locals.extend(narrowed);

    analyze(context, block_context, artifacts)?;

    let after = std::mem::take(&mut block_context.locals);
    let options = context.settings.combiner_options();
    block_context.locals = before
        .into_iter()
        .filter_map(|(id, own)| {
            let current = after.get(&id)?;
            let joined =
                if *current == own { own } else { combine_union_types_rc(&own, current, context.codebase, options) };
            Some((id, joined))
        })
        .collect();

    Ok(())
}

/// Refuses a static member read, written or called at `span` through `object`, an analyzed PHP# class value of a type
/// that is no class, as `t.MAX` on a `Class<int>`: only a class's `Class` has statics, spec section 25. The access is
/// a refused value, so nothing reports again on it. Whether it was refused.
pub(crate) fn refuse_statics_of_no_class<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    object: &Expression<'_>,
    span: Span,
) -> bool
where
    A: Arena,
{
    let Some(object_type) = artifacts.get_expression_type(object).filter(|object_type| holds_no_class(object_type))
    else {
        return false;
    };

    let object_type = object_type.clone();
    report_class_value_of_no_class(
        context,
        object,
        &object_type,
        "has statics",
        "Reach a static through the class value of a class, as in `typeof(Order).defaultTag()`.",
    );
    artifacts.set_expression_type(&span, TUnion::from_atomic(TAtomic::Error));

    true
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
