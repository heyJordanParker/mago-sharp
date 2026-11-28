//! Records a PHP# body's [`EffectSummary`] from its syntax, after the analysis has resolved its calls.

use std::sync::Arc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::r#extern::ExternMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_names::CREATES_ATTRIBUTES;
use mago_names::PARSES_TYPE_ARGUMENT;
use mago_names::RUNS_INITIAL_VALUES;
use mago_names::binding::Binding;
use mago_names::binding::Local;
use mago_names::binding::php_variable_name;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::AnonymousClass;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Binary;
use mago_syntax::cst::Call as CallExpression;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::ConstructorInitializer;
use mago_syntax::cst::Emit;
use mago_syntax::cst::Expression;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Is;
use mago_syntax::cst::LawProof;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalTarget;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects::AttributeArgument;
use crate::effects::Body;
use crate::effects::Call;
use crate::effects::Changed;
use crate::effects::Effect;
use crate::effects::EffectSummary;
use crate::effects::FunctionValue;
use crate::effects::Roots;
use crate::effects::Runs;
use crate::effects::member_of_effect_class;
use crate::effects::sharp_attributes;
use crate::expression::set_members;
use crate::external::Listing;
use crate::resolver::class_name::class_value_classes;
use crate::resolver::class_name::get_class_name_from_atomic;
use crate::resolver::static_property::StaticProperty;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::names::display_function_holder;
use crate::utils::names::display_sharp_type;
use crate::utils::names::is_prelude_stub;

/// Records the summary of the method, accessor, initial value, default, binding or constant `body`, which messages name
/// by `member`, and whose parameters are declared at `parameters`, into `artifacts`, with the summary of each lambda
/// written in it, and with the function values the analysis gave places since the last body. A constructor's
/// `initializer` runs before `code`.
pub(crate) fn record<'ast, 'arena, A>(
    context: &Context<'_, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    body: Body,
    member: Word,
    parameters: Vec<Span>,
    initializer: Option<&'ast ConstructorInitializer<'arena>>,
    code: FunctionLikeBody<'ast, 'arena>,
) where
    A: Arena,
{
    let class = match body {
        Body::Method(class, _)
        | Body::Accessor(class, _, _)
        | Body::InitialValue(class, _)
        | Body::Default(class, _, _)
        | Body::Constant(class, _) => class,
        Body::Lambda(_) | Body::Binding(_) => empty_word(),
    };

    let function_values = std::mem::take(&mut artifacts.function_values);
    let readonly_calls = std::mem::take(&mut artifacts.readonly_calls);
    let (mut summary, mut lambdas, listings) =
        summarize(context, artifacts, class, body, member, parameters, initializer, code);
    artifacts.listings.extend(listings);
    summary.function_values = function_values;
    summary.readonly_calls = readonly_calls;
    // Code that reaches an object of an abstract effect class, the real one or a fake, has that effect, whatever its
    // members do (spec section 29), so each member has it too. A pure declaration then refuses such an implementation,
    // and a call through an interface or a base class has the effect of the override it reaches.
    if let Some(effect) = effect_class(context.codebase, class) {
        summary.effects.insert(0, (Effect::Class(effect), code.span(), member_of_effect_class()));
    }
    artifacts.effect_summaries.push(summary);
    artifacts.effect_summaries.append(&mut lambdas);
}

/// Records the `none` that the method with a body `body`, whose summary [`record`] recorded, writes after `uses` at
/// `none`, which changes nothing, spec section 29, so the effect rules warn with the body's first effect.
pub(crate) fn record_uses_none(artifacts: &mut AnalysisArtifacts, body: Body, none: Span) {
    if let Some(summary) = artifacts.effect_summaries.iter_mut().rev().find(|summary| summary.body == body) {
        summary.uses_none = Some(none);
    }
}

/// Records the `get` and `set` accessors of the property `property` of the class `block_context` is in, which runs
/// through `behaviors`, each a class and where `via` names it, spec section 6.4. A behavior's `get` and `set` are the
/// property's accessors, so a read calls each behavior's `get` and a write each one's `set`, on the behavior the object
/// keeps, and a read or write carries what they do: their effects, and their writes of the behavior's own fields as
/// writes of `this`, which a `get` never makes (spec section 6.1).
pub(crate) fn record_behaviors<A>(
    context: &Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    property: Word,
    behaviors: &[(Word, Span)],
) where
    A: Arena,
{
    let Some(class) = block_context.scope.get_class_like() else {
        unreachable!("a property is analyzed in its class's scope");
    };
    if !context.dialect.is_sharp() || behaviors.is_empty() {
        return;
    }

    let member = word(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes()));
    for (accessor, value) in [(b"get".as_slice(), Changed::This), (b"set", Changed::Parameter(0))] {
        let calls = behaviors
            .iter()
            .map(|(behavior, span)| Call {
                callee: Body::Method(*behavior, word(accessor)),
                span: *span,
                receiver: Roots::from([Changed::This]),
                arguments: vec![Roots::from([value])],
                type_arguments: Vec::new(),
            })
            .collect();
        artifacts.effect_summaries.push(EffectSummary {
            body: Body::Accessor(class.name, property, word(accessor)),
            member,
            effects: Vec::new(),
            attribute_reads: Vec::new(),
            calls,
            changes: Vec::new(),
            function_values: Vec::new(),
            readonly_calls: Vec::new(),
            undeclared: Vec::new(),
            uses_none: None,
            imports: Arc::clone(&context.imported_names),
            scope: Arc::clone(&context.scope),
        });
    }
}

/// Records the `parse` spec section 10 gives the struct or PHP# class `class`, whose body the bridge writes, so its
/// function type lists the effects of what it runs. It calls the main constructor, writes each property field through
/// its `set` accessor, and calls the `parse` of each struct or class a field's type holds. The `parse` of a family calls
/// the `parse` of one of its variants. It makes the object it gives, so it changes no state. A call that reaches an effect object has its class's effect,
/// as a written call does.
pub(crate) fn record_parse<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    class: &ClassLikeMetadata,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let span = class.span;
    let summary = |body, member: &[u8]| {
        // Each member of an effect class has its class's effect, as `record` gives a written one.
        let effects = effect_class(codebase, class.name)
            .map(|effect| (Effect::Class(effect), span, member_of_effect_class()))
            .into_iter()
            .collect();

        EffectSummary {
            body,
            member: word(member),
            effects,
            attribute_reads: Vec::new(),
            calls: Vec::new(),
            changes: Vec::new(),
            function_values: Vec::new(),
            readonly_calls: Vec::new(),
            undeclared: Vec::new(),
            uses_none: None,
            imports: Arc::clone(&context.imported_names),
            scope: Arc::clone(&context.scope),
        }
    };
    // `callee`, a member of an object of `object` that messages name `member`, runs as a written call of it does.
    let reach = |summary: &mut EffectSummary, object: Word, callee: Body, member: &[u8]| {
        if let Some(effect) = effect_class(codebase, object) {
            let class_name = codebase.get_class_like(object).map_or(object, |metadata| metadata.original_name);
            summary.effects.push((Effect::Class(effect), span, (class_name, word(member))));
        } else {
            summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::new(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }
    };
    let parse = Body::Method(class.name, class.method_key(word("parse")));

    let mut parsing = summary(parse, b"parse");
    if class.tag_key().is_some() {
        for (_, variant) in codebase.variants(class) {
            // A case that carries no data is one object, which the family's `parse` gives as it is. A case is no
            // class the analysis visits, so the enum records the `parse` of each.
            if !codebase.parses(variant.name) {
                continue;
            }
            if variant.flags.is_enum_case_class() {
                record_parse(context, artifacts, variant);
            }
            reach(&mut parsing, variant.name, Body::Method(variant.name, variant.method_key(word("parse"))), b"parse");
        }
        artifacts.effect_summaries.push(parsing);

        return;
    }
    let constructor = codebase
        .get_declaring_method_class(class.name, word("__construct"))
        .and_then(|declaring| codebase.get_class_like(declaring));
    if let Some(constructor) = constructor {
        if constructor.flags.is_sharp() {
            reach(&mut parsing, class.name, Body::Method(constructor.name, word(b"__construct")), b"__construct");
        } else {
            // The bridge writes `parse`, so a plain PHP constructor no line declares is no use of the code's to
            // refuse, and `parse` may have any effect.
            let callee = (constructor.original_name, empty_word());
            let effects = codebase
                .extern_of_method(constructor.name, word(b"__construct"))
                .map_or_else(|| vec![Effect::Any], plain_php_effects);
            parsing.effects.extend(effects.into_iter().map(|effect| (effect, span, callee)));
        }
    }
    let mut nested = Vec::new();
    for field in codebase.struct_parse_fields(class.name) {
        if !field.is_parameter {
            let property = word([b"$".as_slice(), field.name].concat());
            let set = Body::Accessor(codebase.class_key(field.declaring), property, word(b"set"));
            reach(&mut parsing, class.name, set, field.name);
        }
        parsed_classes(codebase, field.r#type, &mut nested);
    }
    for nested in nested {
        reach(&mut parsing, nested.name, Body::Method(nested.name, nested.method_key(word("parse"))), b"parse");
    }

    artifacts.effect_summaries.push(parsing);
}

/// Pushes onto `parsed` each struct or PHP# class `r#type` holds, also as a collection's element, once each, whose
/// `parse` the bridge writes. A `parse` that reads a field of `r#type` calls each one's `parse`, as the bridge's
/// `parse_build` writes it.
fn parsed_classes<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    r#type: &TUnion,
    parsed: &mut Vec<&'codebase ClassLikeMetadata>,
) {
    for atomic in r#type.types.iter() {
        match atomic {
            TAtomic::Object(TObject::Named(object))
                if codebase.parses(object.name)
                    || codebase.is_backed_struct(object.name)
                    || codebase.parses_any_value(object.name) =>
            {
                let Some(class) = codebase.get_class_like(object.name) else {
                    unreachable!("a class that parses is in the codebase");
                };
                if !parsed.iter().any(|seen| seen.name == class.name) {
                    parsed.push(class);
                }
            }
            TAtomic::Array(array) => parsed_classes(codebase, &get_array_parameters(array, codebase).1, parsed),
            _ => {}
        }
    }
}

/// The standard library's `ClassMembers`, which holds `Class<T>.parse`, spec section 25.
const CLASS_MEMBERS: &[u8] = b"Sharp\\ClassMembers";

/// The classes whose readers `parse` of a class value of `class_value`'s type runs, spec sections 10 and 29, as
/// [`readers_of`] finds them in the type it holds, or `None` when one of them may be any class's reader.
fn decoded_readers<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    class_value: &TUnion,
) -> Option<Vec<&'codebase ClassLikeMetadata>> {
    let mut readers = Vec::new();
    for atomic in class_value.types.iter().filter(|atomic| !atomic.is_null()) {
        match atomic {
            TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Literal { value })) => {
                reader_of(codebase, *value, &mut readers)?;
            }
            TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::OfType { constraint, .. })) => {
                readers_of(codebase, constraint, &mut readers)?;
            }
            _ => return None,
        }
    }

    Some(readers)
}

/// Pushes onto `readers` the class of each reader `parse` of `r#type` runs, once each, spec section 29: a type that is
/// no class runs only the readers of the classes inside it, so a scalar, an enum and `null` add none, and so does
/// `Any` and `Any?`, which read only maps, lists, scalars and null, and a `List`, a `Map`, a tuple, a `Set`, a union and a
/// generic class's type arguments add their classes'. `None` for a type whose reader may be any, as [`reader_of`] says.
fn readers_of<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    r#type: &TUnion,
    readers: &mut Vec<&'codebase ClassLikeMetadata>,
) -> Option<()> {
    for atomic in r#type.types.iter() {
        match atomic {
            TAtomic::Null | TAtomic::Mixed(_) | TAtomic::Scalar(_) | TAtomic::Object(TObject::Enum(_)) => {}
            TAtomic::Array(array) => {
                let (keys, values) = get_array_parameters(array, codebase);
                readers_of(codebase, &keys, readers)?;
                readers_of(codebase, &values, readers)?;
            }
            TAtomic::Object(TObject::Named(object)) => {
                if codebase.class_key(object.name) != codebase.class_key(word(SET_CLASS)) {
                    reader_of(codebase, object.name, readers)?;
                }
                for argument in object.get_type_parameters().unwrap_or_default() {
                    readers_of(codebase, argument, readers)?;
                }
            }
            _ => return None,
        }
    }

    Some(())
}

/// The prelude's `Set`, a type that is no class, spec section 12, whose readers are its element type's.
const SET_CLASS: &[u8] = b"Sharp\\Set";

/// Pushes onto `readers` the class `name` when `parse` of it runs its own reader, spec section 29: an enum has none of
/// a project's, and a `final` class, a struct, a family, whose reader reads one of its declared variants, and a class
/// whose constructor is `required`, below which no constructor adds an effect, each run the `parse` it declares or
/// the bridge writes. A sealed type without a reader of its own reads one of its permitted cases, the closed set
/// spec section 20 gives it, so it pushes each case's reader, through a sealed sub-family too. `None` for any other
/// class, which may be a subclass whose reader differs, and for a sealed type with a case that is one.
fn reader_of<'codebase>(
    codebase: &'codebase CodebaseMetadata,
    name: Word,
    readers: &mut Vec<&'codebase ClassLikeMetadata>,
) -> Option<()> {
    let class = codebase.get_class_like(name)?;
    if class.kind.is_enum() {
        return Some(());
    }
    let constructor = word("__construct");
    let required = codebase
        .get_declaring_method_class(class.name, constructor)
        .and_then(|declaring| codebase.get_method(declaring, constructor))
        .is_some_and(|constructor| constructor.flags.is_required());
    let known = class.flags.is_final() || class.flags.is_struct() || class.tag_key().is_some() || required;
    let parses = codebase.get_declaring_method_class(class.name, word("parse")) == Some(class.name);
    if !known || !parses {
        for case in set_members(class, codebase)? {
            reader_of(codebase, case.name, readers)?;
        }

        return Some(());
    }
    if !readers.iter().any(|seen| seen.name == class.name) {
        readers.push(class);
    }

    Some(())
}

/// Records each lambda and method value `value` holds where a `.sharp` file gives it the place of type `declared` at
/// `span`, when `declared`'s function types list effects. Only the solve knows the effects of the body such a value
/// runs, so it holds the value to the effects every one of those types lists, and refuses it with `code`, the code a
/// value of the wrong type gets in that place, naming `declared` as `display` writes it. A collection holds each of its
/// elements in its element type, so a value's elements are held to the place's, at any depth.
pub(crate) fn record_function_values<A>(
    context: &Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    value: &TUnion,
    declared: &TUnion,
    display: fn(&Context<'_, '_, A>, &TUnion) -> String,
    span: Span,
    code: IssueCode,
) where
    A: Arena,
{
    if !context.dialect.is_sharp() {
        return;
    }

    let codebase = context.codebase;
    let elements = |union: &TUnion| -> Vec<TUnion> {
        union
            .types
            .iter()
            .filter_map(|atomic| match atomic {
                TAtomic::Array(collection) => Some(get_array_parameters(collection, codebase).1),
                _ => None,
            })
            .collect()
    };
    let declared_elements = elements(declared);
    if !declared_elements.is_empty() {
        for element in elements(value) {
            for declared_element in &declared_elements {
                record_function_values(context, artifacts, &element, declared_element, display_sharp_type, span, code);
            }
        }
    }

    // A declared type holds one function type at most, as the checker refuses a union that writes `Function` twice.
    let Some(allowed) = declared.types.iter().find_map(|atomic| match atomic {
        TAtomic::Callable(TCallable::Signature(TCallableSignature { effects: Some(effects), .. })) => {
            Some(effects.clone())
        }
        _ => None,
    }) else {
        return;
    };

    for atomic in value.types.iter() {
        // A method or function value of plain PHP has a signature PHP wrote, which lists no effects.
        let source = match atomic {
            TAtomic::Callable(TCallable::Signature(signature)) => signature.source.filter(|source| {
                signature.effects.is_some()
                    || matches!(source, FunctionLikeIdentifier::Method(..) | FunctionLikeIdentifier::Function(_))
            }),
            TAtomic::Callable(TCallable::Alias(identifier)) => Some(*identifier),
            _ => None,
        };
        let runs = match source {
            Some(FunctionLikeIdentifier::Closure(name)) => Some(Runs::Body(Body::Lambda(name))),
            Some(FunctionLikeIdentifier::Method(class, method)) => method_value_runs(codebase, class, method),
            Some(FunctionLikeIdentifier::Function(function)) => Some(function_value_runs(codebase, function)),
            None => None,
        };
        let Some(runs) = runs else {
            continue;
        };

        artifacts.function_values.push(FunctionValue {
            runs,
            allowed: allowed.clone(),
            span,
            holder: display_function_holder(declared, &|union| display(context, union)),
            code,
        });
    }
}

/// What a method value of `method`, read through `class`, runs wherever it is called: the PHP# method's body, or the
/// effects a call of the method has without one, `None` when `class` has no such method.
fn method_value_runs(codebase: &CodebaseMetadata, class: Word, method: Word) -> Option<Runs> {
    let declaring_class = codebase.get_declaring_method_class(class, method)?;
    let declaring = codebase.get_class_like(declaring_class)?;
    let metadata = codebase.get_method(declaring_class, method)?;

    // A message names the method through `class`, the class the value is read through.
    let class_name = codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name);
    let callee = (class_name, metadata.original_name);

    Some(if let Some(effect) = effect_class(codebase, class) {
        Runs::Declared { callee, effects: vec![Effect::Class(effect)] }
    } else if declaring.flags.is_sharp() && metadata.method_metadata.as_ref().is_some_and(|m| m.is_abstract) {
        Runs::Declared { callee, effects: metadata.uses.iter().map(|effect| Effect::Class(*effect)).collect() }
    } else if declaring.flags.is_sharp() {
        Runs::Body(Body::Method(declaring.name, declaring.method_key(method)))
    } else if is_prelude_stub(declaring_class) {
        Runs::Declared { callee, effects: Vec::new() }
    } else {
        let line = class_line(codebase, declaring_class);

        plain_php_runs(codebase.extern_of_method(declaring_class, method), callee, line)
    })
}

/// What a value of the plain PHP function `function` runs wherever it is called, spec section 14.3: the effects its
/// `extern` line declares, as a call of the function has them.
fn function_value_runs(codebase: &CodebaseMetadata, function: Word) -> Runs {
    let (callee, declaration) = function_extern(codebase, function);

    plain_php_runs(declaration, callee, callee)
}

/// The summary of `body`, a body of the class `class`, the summaries of the lambdas written in it, and the sets of the
/// program they read whole.
#[allow(clippy::too_many_arguments)]
fn summarize<'ast, 'arena, A>(
    context: &Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    class: Word,
    body: Body,
    member: Word,
    parameters: Vec<Span>,
    initializer: Option<&'ast ConstructorInitializer<'arena>>,
    code: FunctionLikeBody<'ast, 'arena>,
) -> (EffectSummary, Vec<EffectSummary>, HashSet<Listing>)
where
    A: Arena,
{
    let imports = Arc::clone(&context.imported_names);
    let scope = Arc::clone(&context.scope);
    let empty_summary = || EffectSummary {
        body,
        member,
        effects: Vec::new(),
        attribute_reads: Vec::new(),
        calls: Vec::new(),
        changes: Vec::new(),
        function_values: Vec::new(),
        readonly_calls: Vec::new(),
        undeclared: Vec::new(),
        uses_none: None,
        imports: Arc::clone(&imports),
        scope: Arc::clone(&scope),
    };

    // The method's function parameters whose types state no effects, whose functions each caller runs as part of
    // its call.
    let effect_taking_parameters = match body {
        Body::Method(class, method) => context
            .codebase
            .get_method(class, method)
            .map(|metadata| {
                metadata
                    .parameters
                    .iter()
                    .zip(&parameters)
                    .filter(|(parameter, _)| parameter.flags.takes_on_effects())
                    .map(|(_, declaration)| *declaration)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut recorder = Recorder {
        context,
        artifacts,
        class,
        parameters,
        effect_taking_parameters,
        locals: HashMap::default(),
        lambdas: HashMap::default(),
        subjects: Vec::new(),
        inlining: Vec::new(),
        grew: false,
        builds_this: match body {
            Body::Method(class, method) => {
                method.as_bytes().eq_ignore_ascii_case(b"__construct")
                    || context
                        .codebase
                        .get_method(class, method)
                        .is_some_and(|method| method.flags.is_named_constructor())
            }
            _ => false,
        },
        summary: empty_summary(),
        lambda_summaries: Vec::new(),
        listings: HashSet::default(),
    };

    // A local takes the roots of every value assigned to it anywhere in the body, so the walk repeats until no
    // local gains a root, and the last walk records with every local's roots known.
    loop {
        recorder.grew = false;
        recorder.summary = empty_summary();
        if let Some(initializer) = initializer {
            SummaryWalker.walk_constructor_initializer(initializer, &mut recorder);
        }
        match code {
            FunctionLikeBody::Statements(statements, _) => {
                for statement in statements {
                    SummaryWalker.walk_statement(statement, &mut recorder);
                }
            }
            FunctionLikeBody::Expression(expression) | FunctionLikeBody::ExpressionStatement(expression) => {
                SummaryWalker.walk_expression(expression, &mut recorder);
            }
            // A law's effects are those of its condition, its claim and every statement of its body.
            FunctionLikeBody::Law(law) => {
                if let Some(condition) = &law.condition {
                    SummaryWalker.walk_expression(condition.expression, &mut recorder);
                }
                SummaryWalker.walk_expression(law.claim, &mut recorder);
                if let LawProof::Body(body) = &law.proof {
                    SummaryWalker.walk_block(body, &mut recorder);
                }
            }
        }

        if !recorder.grew {
            break;
        }
    }

    (recorder.summary, recorder.lambda_summaries, recorder.listings)
}

/// Every PHP# property that declares an initial value, as its class and its name, which a read of an initial value can
/// run, spec section 29. PHP takes only a constant expression for a property, so a plain PHP one runs no code.
pub(crate) fn initial_values(codebase: &CodebaseMetadata) -> Vec<(Word, Word)> {
    codebase
        .class_likes
        .values()
        .filter(|class_like| class_like.flags.is_sharp())
        .flat_map(|class_like| {
            class_like
                .properties
                .iter()
                .filter(|(_, metadata)| metadata.flags.has_default())
                .map(|(property, _)| (class_like.name, *property))
        })
        .collect()
}

/// The state of one body's walk.
struct Recorder<'analysis, 'ctx, 'ast, 'arena, A>
where
    A: Arena,
{
    context: &'analysis Context<'ctx, 'arena, A>,
    artifacts: &'analysis AnalysisArtifacts,
    /// The lowercase name of the class the body belongs to.
    class: Word,
    /// Where each parameter of the body is declared, by index.
    parameters: Vec<Span>,
    /// Where each function parameter whose type states no effects is declared, spec section 29: a call of its function
    /// adds no effect here, since each caller of the method adds the effects of the function it passes.
    effect_taking_parameters: Vec<Span>,
    /// The roots of each local and lambda parameter, by where it is declared.
    locals: HashMap<Span, Roots>,
    /// The lambda literal each local holds, by where the local is declared.
    lambdas: HashMap<Span, &'ast Expression<'arena>>,
    /// The roots of each value an enclosing pattern tests, innermost last.
    subjects: Vec<Roots>,
    /// The lambdas being walked as part of a call, so a lambda that reaches itself is walked once.
    inlining: Vec<Span>,
    /// Whether a local gained a root during this walk.
    grew: bool,
    /// Whether the body builds `this`: a constructor's or a named constructor's, whose writes of `this` run no
    /// observer, spec section 6.2.
    builds_this: bool,
    summary: EffectSummary,
    /// The summary of each lambda written in the body, once, which a function value of the lambda runs.
    lambda_summaries: Vec<EffectSummary>,
    /// The sets of the program the body's summary read whole, so a watch analyzes the file again when one changes.
    listings: HashSet<Listing>,
}

impl<'ctx, 'ast, 'arena, A> Recorder<'_, 'ctx, 'ast, 'arena, A>
where
    A: Arena,
{
    fn codebase(&self) -> &'ctx CodebaseMetadata {
        self.context.codebase
    }

    fn text(&self, span: Span) -> Word {
        word(&self.context.source_file.contents[span.start.offset as usize..span.end.offset as usize])
    }

    /// The roots of a value: whose state changing the value changes.
    fn roots(&self, expression: &Expression<'arena>) -> Roots {
        match expression {
            Expression::Parenthesized(parenthesized) => self.roots(parenthesized.expression),
            Expression::ConstantAccess(name) => match self.context.resolved_names.binding(&name.name) {
                Some(Binding::This) => Roots::from([Changed::This]),
                Some(Binding::Member | Binding::Field) => self.read_roots(expression, Roots::from([Changed::This])),
                Some(Binding::Local(local)) => self.local_roots(local),
                _ => Roots::new(),
            },
            Expression::Parent(_) => Roots::from([Changed::This]),
            Expression::Access(Access::Property(access)) => {
                if StaticProperty::from_property_access(access, self.context.resolved_names).is_some() {
                    Roots::from([Changed::Shared])
                } else {
                    self.read_roots(expression, self.roots(access.object))
                }
            }
            Expression::Access(Access::NullSafeProperty(access)) => {
                self.read_roots(expression, self.roots(access.object))
            }
            Expression::Access(Access::StaticProperty(_)) => Roots::from([Changed::Shared]),
            Expression::ArrayAccess(access) => self.read_roots(expression, self.roots(access.array)),
            // A call's result has the roots of what the call is given.
            Expression::Call(call) => {
                let (receiver, arguments) = match call {
                    CallExpression::Function(call) => (None, &call.argument_list),
                    CallExpression::NullSafeFunction(call) => (None, &call.argument_list),
                    CallExpression::Method(call) => (Some(call.object), &call.argument_list),
                    CallExpression::NullSafeMethod(call) => (Some(call.object), &call.argument_list),
                    CallExpression::StaticMethod(_) => unreachable!("the parser reads PHP's `::` in PHP# as an error"),
                };

                let mut roots = receiver.map(|receiver| self.roots(receiver)).unwrap_or_default();
                for argument in &arguments.arguments {
                    roots.extend(self.roots(argument.value()));
                }

                roots
            }
            Expression::Pipe(pipe) => self.roots(pipe.input),
            Expression::Assignment(assignment) => self.roots(assignment.rhs),
            Expression::Binary(binary) => {
                let mut roots = self.roots(binary.lhs);
                roots.extend(self.roots(binary.rhs));
                roots
            }
            Expression::Conditional(conditional) => {
                let mut roots = self.roots(conditional.then.unwrap_or(conditional.condition));
                roots.extend(self.roots(conditional.r#else));
                roots
            }
            Expression::Match(r#match) => r#match.arms.iter().flat_map(|arm| self.roots(arm.expression())).collect(),
            Expression::As(r#as) => self.roots(r#as.value),
            _ => Roots::new(),
        }
    }

    /// The roots of `read`, a value read from a place of the roots `place`. An object is no part of the object that
    /// holds it, so an object `this` holds is held by `this`, and changing it writes no field of `this`, spec section
    /// 6.1. A collection, string or scalar is part of the object that holds it, spec section 13, and so is a value of
    /// a type the analysis doesn't know.
    fn read_roots(&self, read: &Expression<'arena>, place: Roots) -> Roots {
        let is_object = self.artifacts.get_expression_type(read).is_some_and(|read_type| {
            !read_type.types.is_empty()
                && read_type.types.iter().all(|atomic| atomic.is_object_type() || atomic.is_null())
        });

        if is_object { place.into_iter().map(Changed::held).collect() } else { place }
    }

    fn local_roots(&self, local: Local) -> Roots {
        let mut roots = self.locals.get(&local.declaration).cloned().unwrap_or_default();
        if let Some(index) = self.parameters.iter().position(|parameter| *parameter == local.declaration) {
            roots.insert(Changed::Parameter(index as u32));
        }

        roots
    }

    /// The roots a value has once stored in a local. A collection, string or scalar is copied into the local, which
    /// owns the copy (spec section 13), so it is fresh.
    fn stored_roots(&self, value: &Expression<'arena>) -> Roots {
        let is_value = self.artifacts.get_expression_type(value).is_some_and(|value_type| {
            !value_type.types.is_empty()
                && value_type
                    .types
                    .iter()
                    .all(|atomic| atomic.is_some_scalar() || atomic.is_array() || atomic.is_null())
        });

        if is_value { Roots::new() } else { self.roots(value) }
    }

    fn bind(&mut self, declaration: Span, roots: Roots) {
        let known = self.locals.entry(declaration).or_default();
        let before = known.len();
        known.extend(roots);
        self.grew |= known.len() != before;
    }

    fn store(&mut self, declaration: Span, value: &'ast Expression<'arena>) {
        if let Some(lambda) = lambda_literal(value) {
            self.lambdas.insert(declaration, lambda);
        }

        let roots = self.stored_roots(value);
        self.bind(declaration, roots);
    }

    fn change(&mut self, roots: Roots, span: Span, place: &Expression<'arena>) {
        let place = self.text(place.span());
        for root in roots {
            self.summary.changes.push((root, span, place));
        }
    }

    fn assign(&mut self, assignment: &'ast Assignment<'arena>) {
        if let Expression::ConstantAccess(name) = assignment.lhs.unparenthesized()
            && let Some(Binding::Local(local)) = self.context.resolved_names.binding(&name.name)
        {
            self.store(local.declaration, assignment.rhs);
        } else {
            self.write(assignment.lhs, assignment.span(), Some(assignment.rhs));
        }
    }

    /// Records a write of `place`, with `value` when the write stores one.
    fn write(&mut self, place: &Expression<'arena>, span: Span, value: Option<&Expression<'arena>>) {
        match place.unparenthesized() {
            Expression::ConstantAccess(name) => {
                if let Some(Binding::Member | Binding::Field) = self.context.resolved_names.binding(&name.name) {
                    self.change(Roots::from([Changed::This]), span, place);
                }
            }
            Expression::Access(Access::Property(access)) => {
                if StaticProperty::from_property_access(access, self.context.resolved_names).is_some() {
                    self.change(Roots::from([Changed::Shared]), span, place);
                    return;
                }

                let receiver = self.roots(access.object);
                self.change(receiver.clone(), span, place);
                let arguments = vec![value.map(|value| self.roots(value)).unwrap_or_default()];
                // A write of `this` while a constructor builds it runs no observer, spec section 6.2.
                let builds_this = self.builds_this
                    && matches!(access.object.unparenthesized(), Expression::ConstantAccess(name)
                        if self.context.resolved_names.binding(&name.name) == Some(Binding::This));
                for set in self.accessors(access.object, &access.property, b"set") {
                    // The write runs the observers inside the `set` hook too, spec section 6.2. A property without one
                    // has no such body, and a call of a body without a summary adds nothing.
                    let Body::Accessor(class, property, _) = set else {
                        unreachable!("an accessor's body is an accessor body");
                    };
                    let observers = [b"willSet".as_slice(), b"didSet"]
                        .map(|observer| Body::Accessor(class, property, word(observer)))
                        .into_iter()
                        .filter(|_| !builds_this);
                    for callee in std::iter::once(set).chain(observers) {
                        self.summary.calls.push(Call {
                            callee,
                            span,
                            receiver: receiver.clone(),
                            arguments: arguments.clone(),
                            type_arguments: Vec::new(),
                        });
                    }
                }
            }
            Expression::Access(Access::NullSafeProperty(access)) => {
                self.change(self.roots(access.object), span, place);
            }
            Expression::Access(Access::StaticProperty(_)) => self.change(Roots::from([Changed::Shared]), span, place),
            Expression::ArrayAccess(access) => self.change(self.roots(access.array), span, place),
            Expression::ArrayAppend(append) => self.change(self.roots(append.array), span, place),
            _ => {}
        }
    }

    /// The PHP# classes a value may hold an object of.
    fn object_classes(&self, value: &Expression<'arena>) -> Vec<Word> {
        let Some(value_type) = self.artifacts.get_expression_type(value) else {
            return Vec::new();
        };

        value_type
            .types
            .iter()
            .filter_map(|atomic| match atomic {
                TAtomic::Object(object) => object.get_name(),
                _ => None,
            })
            .collect()
    }

    /// The `accessor` bodies a read or write of `object.property` runs. For `Class.property`, a static's, they are the
    /// class's own.
    fn accessors(
        &self,
        object: &Expression<'arena>,
        property: &ClassLikeMemberSelector<'arena>,
        accessor: &[u8],
    ) -> Vec<Body> {
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return Vec::new();
        };

        self.member_classes(object).into_iter().filter_map(|class| self.accessor(class, name.value, accessor)).collect()
    }

    /// The classes a member read through `object` reaches: the class `Class.member` names, for a static, or each class
    /// the value may hold an object of.
    fn member_classes(&self, object: &Expression<'arena>) -> Vec<Word> {
        match object.unparenthesized() {
            Expression::ConstantAccess(class)
                if self.context.resolved_names.binding(&class.name) == Some(Binding::Class) =>
            {
                vec![word(self.context.resolved_names.get(&class.name))]
            }
            _ => self.object_classes(object),
        }
    }

    /// The `accessor` body of `property` that a read or write through `class` runs, by the class that declares the
    /// property. It has a summary only where PHP# code gives it one: an accessor body, an observer, or the behaviors
    /// `via` names, which a property's PHP metadata lists no hook for. A call of a body without one adds nothing.
    fn accessor(&self, class: Word, property: &[u8], accessor: &[u8]) -> Option<Body> {
        let property = php_variable_name(property);
        let declaring = *self.codebase().get_class_like(class)?.declaring_property_ids.get(&property)?;

        Some(Body::Accessor(self.codebase().class_key(declaring), property, word(accessor)))
    }

    fn read_property(&mut self, object: &Expression<'arena>, property: &ClassLikeMemberSelector<'arena>, span: Span) {
        // A member named by an expression, as `payments.{"fee"}`, is a parse refusal, so its read has no effect a
        // caller reaches.
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return;
        };
        let member = word(name.value);
        let codebase = self.codebase();
        for class in self.object_classes(object) {
            if let Some(effect) = effect_class(codebase, class) {
                let class = codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name);
                self.summary.effects.push((Effect::Class(effect), span, (class, member)));
            }
        }
        for class in self.member_classes(object) {
            if codebase.get_class_like(class).is_some_and(|metadata| !metadata.flags.is_sharp()) {
                self.plain_property(class, php_variable_name(name.value), span);
            }
        }

        let receiver = self.roots(object);
        for callee in self.accessors(object, property, b"get") {
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: receiver.clone(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }

        if let Expression::ConstantAccess(class) = object.unparenthesized()
            && self.context.resolved_names.binding(&class.name) == Some(Binding::Class)
        {
            self.use_statics(word(self.context.resolved_names.get(&class.name)), name.value, span);
        }
    }

    /// Records a use of `property` through `class` when it is a static: the first use of the statics of the class that
    /// declares it runs each of their initial values that is not constant, spec section 6, so it has their effects.
    fn use_statics(&mut self, class: Word, property: &[u8], span: Span) {
        let codebase = self.codebase();
        let property = php_variable_name(property);
        let Some(declaring) = codebase
            .get_class_like(class)
            .and_then(|class| class.declaring_property_ids.get(&property))
            .and_then(|declaring| codebase.get_class_like(*declaring))
            .filter(|declaring| declaring.properties.get(&property).is_some_and(|used| used.flags.is_static()))
        else {
            return;
        };

        for (name, static_property) in &declaring.properties {
            if static_property.flags.is_static() && static_property.flags.has_runtime_initial_value() {
                let callee = Body::InitialValue(declaring.name, *name);
                self.summary.calls.push(Call {
                    callee,
                    span,
                    receiver: Roots::new(),
                    arguments: Vec::new(),
                    type_arguments: Vec::new(),
                });
            }
        }
    }

    /// A member read without `this.` runs its `get` accessor as `this.member` does, and a static's uses its class's
    /// statics as `Class.member` does.
    fn read_member(&mut self, name: &ConstantAccess<'arena>) {
        if self.context.resolved_names.binding(&name.name) != Some(Binding::Member) {
            return;
        }

        let span = name.span();
        if let Some(callee) = self.accessor(self.class, name.name.value(), b"get") {
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::from([Changed::This]),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }
        self.use_statics(self.class, name.name.value(), span);
    }

    /// The roots of each argument, by the index of the parameter it binds.
    fn argument_roots(
        &self,
        arguments: &ArgumentList<'arena>,
        parameters: &[FunctionLikeParameterMetadata],
    ) -> Vec<Roots> {
        let mut roots = vec![Roots::new(); parameters.len()];
        for (position, argument) in arguments.arguments.iter().enumerate() {
            if let Some(parameter_roots) = roots.get_mut(parameter_index(argument, position, parameters)) {
                parameter_roots.extend(self.roots(argument.value()));
            }
        }

        roots
    }

    /// The lambda literal a callee expression is, or holds through a local.
    fn lambda(&self, callee: &'ast Expression<'arena>) -> Option<&'ast Expression<'arena>> {
        if let Some(lambda) = lambda_literal(callee) {
            return Some(lambda);
        }

        self.lambdas.get(&self.local(callee)?.declaration).copied()
    }

    /// The local or parameter an expression names.
    fn local(&self, expression: &Expression<'arena>) -> Option<Local> {
        let name = match expression.unparenthesized() {
            Expression::ConstantAccess(name) => &name.name,
            Expression::Identifier(name) => name,
            _ => return None,
        };
        let Some(Binding::Local(local)) = self.context.resolved_names.binding(name) else {
            return None;
        };

        Some(local)
    }

    /// Whether an expression names one of the method's function parameters whose types state no effects.
    fn is_effect_taking_parameter(&self, expression: &Expression<'arena>) -> bool {
        self.local(expression).is_some_and(|local| self.effect_taking_parameters.contains(&local.declaration))
    }

    /// Walks a lambda's body as part of the call that runs it, with its parameters given `parameter_roots`.
    fn inline(&mut self, lambda: &'ast Expression<'arena>, parameter_roots: impl Fn(usize) -> Roots) {
        let span = lambda.span();
        if self.inlining.contains(&span) {
            return;
        }

        let parameters = match lambda {
            Expression::ArrowFunction(arrow_function) => &arrow_function.parameter_list,
            Expression::Closure(closure) => &closure.parameter_list,
            _ => return,
        };
        for (index, parameter) in parameters.parameters.iter().enumerate() {
            self.bind(parameter.variable.span, parameter_roots(index));
        }

        self.inlining.push(span);
        match lambda {
            Expression::ArrowFunction(arrow_function) => SummaryWalker.walk_expression(arrow_function.expression, self),
            Expression::Closure(closure) => SummaryWalker.walk_block(&closure.body, self),
            _ => {}
        }
        self.inlining.pop();
    }

    /// Records the summary of the lambda written at `span` as its own body, once, so the solve knows the effects of a
    /// function value of it.
    fn record_lambda(
        &mut self,
        span: Span,
        parameters: &FunctionLikeParameterList<'arena>,
        code: FunctionLikeBody<'ast, 'arena>,
    ) {
        let FunctionLikeIdentifier::Closure(name) = FunctionLikeIdentifier::for_closure(self.context.source_file, span)
        else {
            return;
        };
        let body = Body::Lambda(name);
        if self.lambda_summaries.iter().any(|summary| summary.body == body) {
            return;
        }

        let parameters = parameters.parameters.iter().map(|parameter| parameter.variable.span).collect();
        let (summary, lambdas, listings) =
            summarize(self.context, self.artifacts, self.class, body, empty_word(), parameters, None, code);
        self.listings.extend(listings);
        self.lambda_summaries.push(summary);
        for lambda in lambdas {
            if !self.lambda_summaries.iter().any(|summary| summary.body == lambda.body) {
                self.lambda_summaries.push(lambda);
            }
        }
    }

    fn call(&mut self, call: &'ast CallExpression<'arena>) {
        let span = call.span();
        let (receiver, arguments, function) = match call {
            CallExpression::Function(call) => (None, &call.argument_list, Some(call.function)),
            CallExpression::NullSafeFunction(call) => (None, &call.argument_list, Some(call.function)),
            CallExpression::Method(call) => (Some(call.object), &call.argument_list, None),
            CallExpression::NullSafeMethod(call) => (Some(call.object), &call.argument_list, None),
            CallExpression::StaticMethod(_) => unreachable!("the parser reads PHP's `::` in PHP# as an error"),
        };

        // A call of a lambda written in this body runs the lambda here. A call of any other function value, such as a
        // local's or a parameter's, has the effects its function type lists. A call of one whose type plain PHP gives,
        // with no effects, is refused where the analysis reads it, spec section 21.
        if let Some(lambda) = function.and_then(|function| self.lambda(function)) {
            let roots: Vec<Roots> = arguments.arguments.iter().map(|argument| self.roots(argument.value())).collect();
            self.inline(lambda, |index| roots.get(index).cloned().unwrap_or_default());

            return;
        }

        if let Some(function) = function
            && !self.is_effect_taking_parameter(function)
            && let Some(function_type) = self.artifacts.get_expression_type(function)
        {
            let cause = (empty_word(), self.text(function.span()));
            for effect in declared_effects(self.context.codebase, function_type) {
                self.summary.effects.push((Effect::Class(effect), span, cause));
            }
        }

        self.call_targets(span, receiver, arguments);
    }

    /// Records the functions and methods the analysis resolved the call at `span` to, with `receiver` and `arguments`.
    fn call_targets(
        &mut self,
        span: Span,
        receiver: Option<&'ast Expression<'arena>>,
        arguments: &'ast ArgumentList<'arena>,
    ) {
        let artifacts = self.artifacts;
        let Some(targets) = artifacts.call_targets.get(&(span.start.offset, span.end.offset)) else {
            return;
        };

        for target in targets {
            match *target {
                CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Function(function_name), .. } => {
                    let (cause, declaration) = function_extern(self.codebase(), function_name);
                    self.plain_php(declaration, span, cause, cause);
                    self.callbacks(arguments);
                }
                CallTarget::FunctionLike {
                    callee: FunctionLikeIdentifier::Method(declaring_class, method_name),
                    class: named_class,
                } => {
                    // A read of attributes has the effects of the class its type argument names, spec section 1: none
                    // for a PHP# attribute class, and those each call gives for the body's own type parameter.
                    match self.attributes_read(declaring_class, method_name, span) {
                        Some(AttributeArgument::Class(class)) if sharp_attributes(self.codebase(), class) => continue,
                        Some(AttributeArgument::Parameter(index)) => {
                            let cause = self.member_cause(declaring_class, method_name, named_class);
                            self.summary.attribute_reads.push((index, span, cause));
                            continue;
                        }
                        Some(AttributeArgument::Class(_) | AttributeArgument::Other) | None => {}
                    }
                    if self.runs_initial_values(declaring_class, method_name, span) {
                        continue;
                    }
                    if self.decode_read(declaring_class, method_name, receiver, span) {
                        continue;
                    }
                    let class = named_class.unwrap_or(declaring_class);
                    self.method_call(declaring_class, method_name, class, span, receiver, arguments);
                }
                CallTarget::MagicMethod {
                    callee: FunctionLikeIdentifier::Method(magic_class, magic_method),
                    class: served_class,
                    ..
                } => {
                    self.method_call(magic_class, magic_method, served_class, span, receiver, arguments);
                }
                CallTarget::Property { class, property } => self.property_call(class, property, span),
                CallTarget::Element { position } => self.element_call(receiver, position, span),
                CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Closure(_), .. }
                | CallTarget::MagicMethod { .. } => {}
            }
        }
    }

    /// What the call at `span` of `method` of `class` reads, when the standard library marks the method
    /// `CreatesAttributes`, as `Class<T>.attributes<TAttribute>()`: the attributes of the class its own type argument
    /// names, the last the checker inferred, after an extension block's, as [`Self::attribute_argument`] gives it. The
    /// summary decides what the read does from every descendant of a class it names, so it records that set. None for
    /// any other method.
    fn attributes_read(&mut self, class: Word, method: Word, span: Span) -> Option<AttributeArgument> {
        let codebase = self.codebase();
        if !codebase
            .get_method(class, method)
            .is_some_and(|method| method.has_attribute(codebase, word(CREATES_ATTRIBUTES)))
        {
            return None;
        }

        let argument = self.attribute_arguments(span).last().copied().unwrap_or(AttributeArgument::Other);
        if let AttributeArgument::Class(attribute_class) = argument {
            self.listings.insert(Listing::Descendants(attribute_class));
        }

        Some(argument)
    }

    /// Records the call at `span` of the standard library's `Class<T>.parse`, or of a method it marks
    /// `ParsesTypeArgument`, as `Json.parse<T>`, as calls of the readers it runs, spec sections 10, 23, 24, 25 and 29:
    /// the `parse` of each class [`decoded_readers`] finds in the type of the class value `receiver`, or [`readers_of`]
    /// finds in the call's type argument, when it knows each one. A marked method reads its text first with its class's
    /// `read`, which the call also runs. True when it records them. False for any other call, and for a type whose
    /// reader it can't know, which the call of the member itself gives every effect through its `uses any`.
    fn decode_read(
        &mut self,
        class: Word,
        method: Word,
        receiver: Option<&'ast Expression<'arena>>,
        span: Span,
    ) -> bool {
        let codebase = self.codebase();
        let (readers, text_read) =
            if codebase.class_key(class) == codebase.class_key(word(CLASS_MEMBERS)) && method.as_bytes() == b"parse" {
                let readers = receiver
                    .and_then(|receiver| self.artifacts.get_expression_type(receiver))
                    .and_then(|class_value| decoded_readers(codebase, class_value));
                (readers, None)
            } else if codebase
                .get_method(class, method)
                .is_some_and(|metadata| metadata.has_attribute(codebase, word(PARSES_TYPE_ARGUMENT)))
            {
                let readers = self
                    .artifacts
                    .inferred_type_arguments
                    .get(&(span.start.offset, span.end.offset))
                    .and_then(|arguments| arguments.first())
                    .and_then(|parsed| {
                        let mut readers = Vec::new();
                        readers_of(codebase, parsed, &mut readers).map(|()| readers)
                    });
                (readers, codebase.get_class_like(class).map(|metadata| metadata.method_key(word("read"))))
            } else {
                return false;
            };
        let Some(readers) = readers else {
            return false;
        };
        if let Some(read) = text_read {
            self.summary.calls.push(Call {
                callee: Body::Method(class, read),
                span,
                receiver: Roots::new(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }
        for reader in readers {
            self.summary.calls.push(Call {
                callee: Body::Method(reader.name, reader.method_key(word("parse"))),
                span,
                receiver: Roots::new(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }

        true
    }

    /// The type arguments the checker inferred for the call at `span`, by the callee's own type parameter index, each as
    /// [`Self::attribute_argument`] gives it, which [`super::through`] substitutes for a read of attributes.
    fn attribute_arguments(&self, span: Span) -> Vec<AttributeArgument> {
        self.artifacts
            .inferred_type_arguments
            .get(&(span.start.offset, span.end.offset))
            .map(|arguments| arguments.iter().map(|argument| self.attribute_argument(argument)).collect())
            .unwrap_or_default()
    }

    /// What `argument` is to a read of the attributes of the class it names, spec sections 1 and 25: the class-like it
    /// names, which [`sharp_attributes`] decides where a read takes it, a type parameter of the body's own method, which
    /// each of its calls gives, or any other type, which may have every effect.
    fn attribute_argument(&self, argument: &TUnion) -> AttributeArgument {
        let codebase = self.codebase();
        if let [TAtomic::GenericParameter(parameter)] = &*argument.types
            && let GenericParent::FunctionLike((class, method)) = &parameter.defining_entity
            && let Body::Method(own_class, own_method) = self.summary.body
            && codebase.class_key(*class) == own_class
            && codebase.method_key(own_class, *method) == own_method
            && let Some(index) = codebase
                .get_method(own_class, own_method)
                .and_then(|metadata| metadata.template_types.get_index_of(&parameter.parameter_name))
        {
            return AttributeArgument::Parameter(index as u32);
        }

        argument
            .get_single_named_object()
            .map_or(AttributeArgument::Other, |named| AttributeArgument::Class(named.name))
    }

    /// The callee of a call of `method` of `declaring_class`, through `named_class` when the call names another, as
    /// messages name it: the class's full name and the member, as `ClassMembers` and `attributes`.
    fn member_cause(&self, declaring_class: Word, method: Word, named_class: Option<Word>) -> (Word, Word) {
        let codebase = self.codebase();
        let class = named_class.unwrap_or(declaring_class);
        let class_name = codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name);
        let member = codebase.get_method(declaring_class, method).map_or(method, |metadata| metadata.original_name);

        (class_name, member)
    }

    /// Records the call at `span` of `method` of `class` as calls of the code it runs, when the standard library marks
    /// it as running a property's initial value, `RunsInitialValues`, as `Property.defaultValue()` and
    /// `Field.defaultValue()`: it reaches every one the program declares that is not constant, spec section 29, and a
    /// constant one runs no code. Every initial value that runs code is in a `.sharp` file, since PHP takes only a
    /// constant expression for a property. Gives whether it recorded the call.
    fn runs_initial_values(&mut self, class: Word, method: Word, span: Span) -> bool {
        let codebase = self.codebase();
        if !codebase
            .get_method(class, method)
            .is_some_and(|marked| marked.has_attribute(codebase, word(RUNS_INITIAL_VALUES)))
        {
            return false;
        }
        let mut callees: Vec<Body> =
            initial_values(codebase).into_iter().map(|(class, property)| Body::InitialValue(class, property)).collect();
        self.listings.insert(Listing::InitialValues);

        callees.sort_unstable();
        for callee in callees {
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::new(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }

        true
    }

    /// Records a call of the property `property` of `class` that holds a function. A PHP# class declares it with a
    /// `Function` type, which has the effects its `uses` lists. A plain PHP class's property is one of its members, so
    /// the code it holds has the effects of the `extern` line of the class that declares it, spec section 29, as its
    /// read records them. An object's `__invoke` is recorded as the method it is.
    fn property_call(&mut self, class: Word, property: Word, span: Span) {
        let codebase = self.codebase();
        let Some(class_like) = codebase.get_class_like(class) else {
            return;
        };
        if !class_like.flags.is_sharp() {
            self.plain_property(class, property, span);

            return;
        }

        let cause =
            (class_like.original_name, word(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes())));
        let effects = codebase
            .get_property(class, property)
            .and_then(|metadata| metadata.type_metadata.as_ref())
            .map(|metadata| declared_effects(codebase, &metadata.type_union))
            .unwrap_or_default();
        for effect in effects {
            self.summary.effects.push((Effect::Class(effect), span, cause));
        }
    }

    /// Records a call at `span` of the function the element at `position` holds of the tuple `receiver` is, spec section
    /// 30, which has the effects the element's function type lists, as a function a local holds does.
    fn element_call(&mut self, receiver: Option<&'ast Expression<'arena>>, position: usize, span: Span) {
        let Some(receiver) = receiver else {
            return;
        };
        let codebase = self.codebase();
        let elements: Vec<(TUnion, Word)> = self
            .artifacts
            .get_expression_type(receiver)
            .into_iter()
            .flat_map(|receiver_type| receiver_type.types.iter())
            .filter_map(|atomic| match atomic {
                TAtomic::Array(TArray::List(list)) => {
                    let (element, name) = *list.tuple_elements()?.get(position)?;

                    Some((element.clone(), name?))
                }
                _ => None,
            })
            .collect();
        for (element, name) in elements {
            let called = format!("{}.{name}", self.text(receiver.span()));
            let cause = (empty_word(), word(called.as_bytes()));
            for effect in declared_effects(codebase, &element) {
                self.summary.effects.push((Effect::Class(effect), span, cause));
            }
        }
    }

    /// Records a read at `span` of the property `property`, a PHP name such as `$id`, through the plain PHP class
    /// `class`, spec section 29: it takes the `extern` line of the class that declares the property, whose effects
    /// cover the class's members. A read of a property no class declares, as a constant, a case or a method value is
    /// read, is no read of a property.
    fn plain_property(&mut self, class: Word, property: Word, span: Span) {
        let codebase = self.codebase();
        if is_prelude_stub(class) {
            return;
        }
        let Some(declaring) = codebase
            .get_class_like(class)
            .and_then(|metadata| metadata.declaring_property_ids.get(&property))
            .and_then(|declaring| codebase.get_class_like(*declaring))
            .filter(|declaring| !declaring.flags.is_sharp())
        else {
            return;
        };

        let cause =
            (declaring.original_name, word(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes())));
        self.plain_php(
            codebase.extern_of(declaring.name, empty_word()),
            span,
            cause,
            class_line(codebase, declaring.name),
        );
    }

    /// Records a PHP# operator on instances at `span`: it runs the static method its class declares, which takes
    /// `operands` as its arguments.
    fn operator(&mut self, span: Span, operands: &[&Expression<'arena>]) {
        let artifacts = self.artifacts;
        let Some(targets) = artifacts.call_targets.get(&(span.start.offset, span.end.offset)) else {
            return;
        };

        for target in targets {
            let CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. } = target else {
                continue;
            };

            let class = self.codebase().class_key(*class);
            let callee = Body::Method(class, self.codebase().method_key(class, *method));
            let arguments = operands.iter().map(|operand| self.roots(operand)).collect();
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::new(),
                arguments,
                type_arguments: Vec::new(),
            });
        }
    }

    fn method_call(
        &mut self,
        declaring_class: Word,
        method_name: Word,
        class: Word,
        span: Span,
        receiver: Option<&'ast Expression<'arena>>,
        arguments: &'ast ArgumentList<'arena>,
    ) {
        let codebase = self.codebase();
        let Some(metadata) = codebase.get_method(declaring_class, method_name) else {
            return;
        };

        let cause = self.member_cause(declaring_class, method_name, Some(class));
        let receiver_roots = receiver.map(|receiver| self.roots(receiver)).unwrap_or_default();
        // An extension's member takes its receiver as its first parameter, as its method value does, and the call's
        // arguments fill the parameters after it.
        let (parameters, extension) = match metadata.parameters.split_first() {
            Some((_, parameters)) if metadata.flags.is_extension_method() => (parameters, true),
            _ => (metadata.parameters.as_slice(), false),
        };
        let roots = self.argument_roots(arguments, parameters);
        self.passed_functions(arguments, parameters, &receiver_roots, &roots);
        let (receiver_roots, roots) = if extension {
            (Roots::new(), std::iter::once(receiver_roots).chain(roots).collect())
        } else {
            (receiver_roots, roots)
        };
        if let Some(effect) = effect_class(codebase, class) {
            self.summary.effects.push((Effect::Class(effect), span, cause));

            return;
        }

        if codebase.get_class_like(declaring_class).is_some_and(|declaring| declaring.flags.is_sharp()) {
            // A method without a body has the effects its `uses` lists, which each implementation is held to.
            if metadata.method_metadata.as_ref().is_some_and(|method| method.is_abstract) {
                for effect in &metadata.uses {
                    self.summary.effects.push((Effect::Class(*effect), span, cause));
                }

                return;
            }

            let declaring_class = codebase.class_key(declaring_class);
            let method = codebase.method_key(declaring_class, method_name);
            self.left_out_defaults(declaring_class, method, parameters, Some(arguments), span);
            self.summary.calls.push(Call {
                callee: Body::Method(declaring_class, method),
                span,
                receiver: receiver_roots,
                arguments: roots,
                type_arguments: self.attribute_arguments(span),
            });

            return;
        }

        // `passed_functions` already took on the functions a collection method is given, as a PHP# method's, and plain
        // PHP may run any function it is given.
        if is_prelude_stub(declaring_class) {
            self.prelude_stub_call(metadata, span, receiver);
        } else {
            let declaration = codebase.extern_of_method(declaring_class, method_name);
            self.plain_php(declaration, span, cause, class_line(codebase, declaring_class));
            self.callbacks(arguments);
        }
    }

    /// Records the call at `span` of each default of `parameters`, of the PHP# method `method` of `class`, that
    /// `arguments` leave out: it runs at the call, spec section 7, so the call has its effects. A spread fills as many
    /// parameters as its list surely holds. A constant default has no body of its own, so its call reaches only the
    /// default an override declares at its place.
    fn left_out_defaults(
        &mut self,
        class: Word,
        method: Word,
        parameters: &[FunctionLikeParameterMetadata],
        arguments: Option<&ArgumentList<'arena>>,
        span: Span,
    ) {
        let mut given = vec![false; parameters.len()];
        let mut position = 0;
        for argument in arguments.into_iter().flat_map(|arguments| arguments.arguments.iter()) {
            let filled = match argument {
                Argument::Named(named) => {
                    let name = php_variable_name(named.name.value);
                    if let Some(index) = parameters.iter().position(|parameter| parameter.name.0 == name) {
                        given[index] = true;
                    }

                    continue;
                }
                Argument::Positional(positional) if positional.ellipsis.is_some() => self
                    .artifacts
                    .get_expression_type(positional.value)
                    .and_then(|spread| {
                        spread
                            .types
                            .iter()
                            .map(|atomic| match atomic {
                                TAtomic::Array(array) => array.get_minimum_size(),
                                _ => 0,
                            })
                            .min()
                    })
                    .unwrap_or(0),
                Argument::Positional(_) => 1,
            };
            for parameter in given.iter_mut().skip(position).take(filled) {
                *parameter = true;
            }
            position += filled;
        }

        for (parameter, given) in parameters.iter().zip(given) {
            if !given && parameter.flags.has_default() {
                let callee = Body::Default(class, method, parameter.get_name().0);
                self.summary.calls.push(Call {
                    callee,
                    span,
                    receiver: Roots::new(),
                    arguments: Vec::new(),
                    type_arguments: Vec::new(),
                });
            }
        }
    }

    /// Records each function a call passes to a function parameter that takes on its effects, spec section 29: one
    /// whose type states no effects, of a PHP# method or of a collection method. The method may run it on its receiver,
    /// with the roots `receiver`, and on anything it is given, with the roots `roots` by parameter.
    fn passed_functions(
        &mut self,
        arguments: &'ast ArgumentList<'arena>,
        parameters: &[FunctionLikeParameterMetadata],
        receiver: &Roots,
        roots: &[Roots],
    ) {
        let reached: Roots = roots.iter().flatten().chain(receiver).copied().collect();
        for (position, argument) in arguments.arguments.iter().enumerate() {
            if parameters
                .get(parameter_index(argument, position, parameters))
                .is_some_and(|parameter| parameter.flags.takes_on_effects())
            {
                self.passed_function(argument.value(), &reached);
            }
        }
    }

    /// Records the function values plain PHP is given, as `Sorter.sort(list, (a, b) => …)` gives one, since plain PHP
    /// may run each.
    fn callbacks(&mut self, arguments: &'ast ArgumentList<'arena>) {
        for argument in arguments.arguments.iter() {
            self.passed_function(argument.value(), &Roots::new());
        }
    }

    /// Records `value`, a function passed to code that may run it on values with the roots `reached`: a lambda runs
    /// here, a method value calls its method, and any other function value has the effects its function type lists.
    fn passed_function(&mut self, value: &'ast Expression<'arena>, reached: &Roots) {
        if let Some(lambda) = self.lambda(value) {
            self.inline(lambda, |_| reached.clone());

            return;
        }

        let span = value.span();
        self.method_value(value, span, reached);

        // A value the analysis gave no type passes no function type's effects.
        let codebase = self.context.codebase;
        let effects = self
            .artifacts
            .get_expression_type(value)
            .map(|value_type| declared_effects(codebase, value_type))
            .unwrap_or_default();
        let cause = (empty_word(), self.text(span));
        for effect in effects {
            self.summary.effects.push((Effect::Class(effect), span, cause));
        }
    }

    /// Records `: super(…)`: the call of the base class's constructor the analysis resolved at its span.
    fn constructor_initializer(&mut self, initializer: &'ast ConstructorInitializer<'arena>) {
        self.call_targets(initializer.span(), None, &initializer.argument_list);
    }

    /// Records a `new`: a constructor PHP# declares runs its body on a fresh object, and one plain PHP declares has the
    /// effects of the `extern` line of the class `new` creates, or of its nearest plain PHP parent when PHP# declares
    /// that class, and takes on the effects of each function it is given, spec section 29. A named constructor is the
    /// class's own, spec section 9.1, and its body runs on the object its `: this(…)` creates.
    fn instantiate(&mut self, instantiation: &'ast Instantiation<'arena>) {
        let resolved_names = self.context.resolved_names;
        let classes: Vec<Word> = match instantiation.class.unparenthesized() {
            // `new (type)(…)` builds the class `type`'s `Class<T>` names or a subclass, whose constructor below a
            // `required` one may add no effect to it, so it has the effects of `T`'s constructor, spec sections 25 and
            // 29, as `new T(…)` has them. A `Class<Owner & Row>` builds a `Row`, since an interface declares no
            // constructor, spec section 9. The class value's type is its expression's, whichever expression it is, so
            // a local or a parameter, whose bare name reads as a class's without the parentheses, takes this path too.
            class_value if matches!(instantiation.class, Expression::Parenthesized(_)) => {
                let codebase = self.codebase();
                self.artifacts
                    .get_expression_type(class_value)
                    .filter(|class_value_type| class_value_classes(class_value_type).is_some())
                    .map(|class_value_type| {
                        class_value_type
                            .types
                            .iter()
                            .filter_map(|atomic| get_class_name_from_atomic(codebase, atomic)?.created_class(codebase))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            // `new Self(…)` builds the class the body belongs to or a subclass, whose constructor below a `required`
            // one may add no effect to it, so it has the effects of the class's own constructor, spec sections 25 and
            // 29, as `new Saved(…)` has them inside `Saved`. A lambda belongs to the class it is written in.
            Expression::Self_(_) => vec![self.class],
            // `new TItem(…)` builds the type argument, the class its bound names or one below it, whose constructor
            // below the bound's `required` one may add no effect to it, spec sections 11 and 29. A type parameter's
            // name resolves to the name as written, which names no class, so the class is the one the type the
            // analysis gave the `new` holds through its bound.
            Expression::Identifier(identifier)
                if matches!(resolved_names.binding(identifier), Some(Binding::TypeParameter { .. })) =>
            {
                let codebase = self.codebase();
                self.artifacts
                    .get_expression_type(instantiation)
                    .map(|created| {
                        created
                            .types
                            .iter()
                            .filter_map(|atomic| get_class_name_from_atomic(codebase, atomic)?.created_class(codebase))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Expression::Identifier(identifier) => resolved_names.resolve(identifier).map(word).into_iter().collect(),
            Expression::ConstantAccess(name) => resolved_names.resolve(&name.name).map(word).into_iter().collect(),
            _ => Vec::new(),
        };
        for class in classes {
            self.instantiate_class(class, instantiation);
        }
    }

    /// Records the `new` `instantiation` of `class_name`, as [`Self::instantiate`] says.
    fn instantiate_class(&mut self, class_name: Word, instantiation: &'ast Instantiation<'arena>) {
        let codebase = self.codebase();
        let Some(class) = codebase.get_class_like(class_name) else {
            return;
        };

        let span = instantiation.span();
        let (declaring, constructor) = match &instantiation.constructor {
            Some(constructor) => (class, class.method_key(word(constructor.name.value))),
            None => {
                let constructor = word(b"__construct");
                let declaring = codebase
                    .get_declaring_method_class(class.name, constructor)
                    .and_then(|declaring_class| codebase.get_class_like(declaring_class))
                    .unwrap_or(class);

                (declaring, constructor)
            }
        };
        if declaring.flags.is_sharp() {
            let Some(metadata) = codebase.get_method(declaring.name, constructor) else {
                return;
            };

            let mut roots = Vec::new();
            if let Some(arguments) = &instantiation.argument_list {
                roots = self.argument_roots(arguments, &metadata.parameters);
                self.passed_functions(arguments, &metadata.parameters, &Roots::new(), &roots);
            }
            let arguments = instantiation.argument_list.as_ref();
            self.left_out_defaults(declaring.name, constructor, &metadata.parameters, arguments, span);
            let callee = Body::Method(declaring.name, constructor);
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::new(),
                arguments: roots,
                type_arguments: Vec::new(),
            });
        } else if !is_prelude_stub(class.name) {
            // `new` takes the line of the class it creates, as `new ValueError(…)` takes `ValueError`'s, and only plain
            // PHP takes an `extern` line, so a PHP# class takes its nearest plain PHP parent's.
            let named = std::iter::successors(Some(class), |class| {
                class.direct_parent_class.and_then(|parent| codebase.get_class_like(parent))
            })
            .find(|class| !class.flags.is_sharp())
            .unwrap_or(declaring);
            let declaration = codebase.extern_of_method(named.name, constructor);
            let line = class_line(codebase, named.name);
            self.plain_php(declaration, span, (named.original_name, empty_word()), line);
            if let Some(arguments) = &instantiation.argument_list {
                self.callbacks(arguments);
            }
        }
    }

    /// Records the `dispose()` a `using` local's block runs on its value when it ends, spec section 2, as a call of
    /// each class the value may be, which gives the body that `dispose`'s effects as a written call would. A `dispose`
    /// without a body has the effects its `uses` lists.
    fn dispose(&mut self, declaration: &'ast LocalDeclaration<'arena>) {
        let codebase = self.codebase();
        let dispose = word(b"dispose");
        let span = declaration.span();
        let receiver = self.roots(declaration.value);
        for class in self.object_classes(declaration.value) {
            let Some(declaring_class) = codebase.get_declaring_method_class(class, dispose) else {
                continue;
            };
            let Some(metadata) = codebase.get_method(declaring_class, dispose) else {
                continue;
            };
            if metadata.method_metadata.as_ref().is_some_and(|method| method.is_abstract) {
                let cause = self.member_cause(declaring_class, dispose, Some(class));
                for effect in &metadata.uses {
                    self.summary.effects.push((Effect::Class(*effect), span, cause));
                }

                continue;
            }

            let declaring_class = codebase.class_key(declaring_class);
            self.summary.calls.push(Call {
                callee: Body::Method(declaring_class, codebase.method_key(declaring_class, dispose)),
                span,
                receiver: receiver.clone(),
                arguments: Vec::new(),
                type_arguments: Vec::new(),
            });
        }
    }

    /// Records an `emit`, which has the effect `Sharp\Events` for each class its event may be.
    fn emit(&mut self, emit: &'ast Emit<'arena>) {
        let span = emit.span();
        for class in self.object_classes(emit.value) {
            let class = self.codebase().get_class_like(class).map_or(class, |metadata| metadata.original_name);
            self.summary.effects.push((Effect::Emit, span, (class, empty_word())));
        }
    }

    /// Records a use of plain PHP at `span`, of the callee `cause`, with the effects its `extern` line `declaration`
    /// gives it, or, without one, as a use that needs the line `line`.
    fn plain_php(&mut self, declaration: Option<&ExternMetadata>, span: Span, cause: (Word, Word), line: (Word, Word)) {
        let Some(declaration) = declaration else {
            self.summary.undeclared.push((span, line));

            return;
        };

        for effect in plain_php_effects(declaration) {
            self.summary.effects.push((effect, span, cause));
        }
    }

    /// Records a call of a prelude stub, such as `List.add`, which changes its receiver unless the stub is free of
    /// mutation.
    // Deleted once the standard library's `.sharp` sources replace the prelude stubs (php-sharp issue #60).
    fn prelude_stub_call(
        &mut self,
        metadata: &FunctionLikeMetadata,
        span: Span,
        receiver: Option<&'ast Expression<'arena>>,
    ) {
        if !metadata.flags.is_mutation_free()
            && let Some(receiver) = receiver
        {
            self.change(self.roots(receiver), span, receiver);
        }
    }

    /// A method or function value, such as `this.label`, `Job.ping` or `Pdf.render`, passed to a call that runs it on
    /// `elements`: a call of each method or function its type names the analysis resolved it to, with the object it is
    /// read from as the receiver, which has the effects a call of it has. An extension method takes that object as its
    /// first argument, spec section 26.
    fn method_value(&mut self, value: &'ast Expression<'arena>, span: Span, elements: &Roots) {
        let receiver = match value.unparenthesized() {
            Expression::Access(Access::Property(access)) => self.roots(access.object),
            _ => Roots::new(),
        };
        let codebase = self.codebase();
        let runs: Vec<(Runs, &FunctionLikeMetadata)> = self
            .artifacts
            .get_expression_type(value)
            .into_iter()
            .flat_map(|value| value.types.iter())
            .filter_map(|atomic| match atomic {
                TAtomic::Callable(TCallable::Signature(TCallableSignature {
                    source: Some(FunctionLikeIdentifier::Method(class, method)),
                    ..
                })) => Some((
                    method_value_runs(codebase, *class, *method)?,
                    codebase.get_declaring_method(*class, *method)?,
                )),
                TAtomic::Callable(TCallable::Signature(TCallableSignature {
                    source: Some(FunctionLikeIdentifier::Function(function)),
                    ..
                })) => Some((function_value_runs(codebase, *function), codebase.get_function(*function)?)),
                _ => None,
            })
            .collect();
        for (runs, metadata) in runs {
            match runs {
                Runs::Body(callee) if metadata.flags.is_extension_method() => {
                    let others = metadata.parameters.len().saturating_sub(1);
                    let arguments = std::iter::once(receiver.clone())
                        .chain(std::iter::repeat_n(elements.clone(), others))
                        .collect();
                    self.summary.calls.push(Call {
                        callee,
                        span,
                        receiver: Roots::new(),
                        arguments,
                        type_arguments: Vec::new(),
                    });
                }
                Runs::Body(callee) => {
                    let arguments = vec![elements.clone(); metadata.parameters.len()];
                    self.summary.calls.push(Call {
                        callee,
                        span,
                        receiver: receiver.clone(),
                        arguments,
                        type_arguments: Vec::new(),
                    });
                }
                Runs::Declared { callee, effects } => {
                    for effect in effects {
                        self.summary.effects.push((effect, span, callee));
                    }
                }
                Runs::Undeclared { line, .. } => self.summary.undeclared.push((span, line)),
            }
        }
    }
}

/// Walks one body, leaving out the lambdas and classes written in it: a lambda runs only where it is called, so the walk
/// records each lambda as a body of its own.
struct SummaryWalker;

impl SummaryWalker {
    /// Walks what a write reads to reach its place, leaving out the place itself.
    fn walk_place<'ast, 'arena, A>(
        &self,
        place: &'ast Expression<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) where
        A: Arena,
    {
        match place.unparenthesized() {
            Expression::Access(Access::Property(access)) => self.walk_expression(access.object, recorder),
            Expression::Access(Access::NullSafeProperty(access)) => self.walk_expression(access.object, recorder),
            Expression::ArrayAccess(access) => {
                self.walk_place(access.array, recorder);
                self.walk_expression(access.index, recorder);
            }
            Expression::ArrayAppend(append) => self.walk_place(append.array, recorder),
            _ => {}
        }
    }
}

impl<'ast, 'arena, A> Walker<'ast, 'arena, Recorder<'_, '_, 'ast, 'arena, A>> for SummaryWalker
where
    A: Arena,
{
    fn walk_closure(&self, closure: &'ast Closure<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        let code = FunctionLikeBody::Statements(closure.body.statements.as_slice(), closure.body.span());
        recorder.record_lambda(closure.span(), &closure.parameter_list, code);
    }

    fn walk_arrow_function(
        &self,
        arrow_function: &'ast ArrowFunction<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        let code = FunctionLikeBody::Expression(arrow_function.expression);
        recorder.record_lambda(arrow_function.span(), &arrow_function.parameter_list, code);
    }

    fn walk_anonymous_class(&self, _: &'ast AnonymousClass<'arena>, _: &mut Recorder<'_, '_, 'ast, 'arena, A>) {}

    fn walk_assignment(&self, assignment: &'ast Assignment<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.operator(assignment.span(), &[assignment.lhs, assignment.rhs]);
        recorder.assign(assignment);
        self.walk_place(assignment.lhs, recorder);
        self.walk_expression(assignment.rhs, recorder);
    }

    fn walk_in_local_declaration(
        &self,
        declaration: &'ast LocalDeclaration<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        if declaration.is_using() {
            recorder.dispose(declaration);
        }
        match &declaration.target {
            LocalTarget::Name(name) => recorder.store(name.span, declaration.value),
            // Each local an unpack declares holds a part of the value, as a loop variable holds a part of its source.
            LocalTarget::Unpack(unpack) => {
                let roots = recorder.stored_roots(declaration.value);
                for name in unpack.names.iter() {
                    recorder.bind(name.span, roots.clone());
                }
            }
        }
    }

    fn walk_in_for_of(&self, for_of: &'ast ForOf<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        let roots = recorder.roots(for_of.expression);
        for variable in for_of.target.variables() {
            recorder.bind(variable.name.span, roots.clone());
        }
    }

    fn walk_in_is(&self, is: &'ast Is<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        let roots = recorder.roots(is.value);
        recorder.subjects.push(roots);
    }

    fn walk_out_is(&self, _: &'ast Is<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.subjects.pop();
    }

    fn walk_in_pattern_match(
        &self,
        pattern_match: &'ast PatternMatch<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        let roots = recorder.roots(pattern_match.expression);
        recorder.subjects.push(roots);
    }

    fn walk_out_pattern_match(&self, _: &'ast PatternMatch<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.subjects.pop();
    }

    /// A pattern's variable holds the value the pattern tests.
    fn walk_in_type_pattern(
        &self,
        pattern: &'ast TypePattern<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        if let Some(variable) = &pattern.variable {
            let roots = recorder.subjects.last().cloned().unwrap_or_default();
            recorder.bind(variable.span, roots);
        }
    }

    fn walk_in_call(&self, call: &'ast CallExpression<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.call(call);
    }

    fn walk_in_instantiation(
        &self,
        instantiation: &'ast Instantiation<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        recorder.instantiate(instantiation);
    }

    fn walk_in_emit(&self, emit: &'ast Emit<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.emit(emit);
    }

    fn walk_in_constructor_initializer(
        &self,
        initializer: &'ast ConstructorInitializer<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        recorder.constructor_initializer(initializer);
    }

    fn walk_in_property_access(
        &self,
        access: &'ast PropertyAccess<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        // `Class.name` reads a static, which runs its class's `get` body too.
        recorder.read_property(access.object, &access.property, access.span());
    }

    fn walk_in_null_safe_property_access(
        &self,
        access: &'ast NullSafePropertyAccess<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        recorder.read_property(access.object, &access.property, access.span());
    }

    fn walk_in_constant_access(
        &self,
        name: &'ast ConstantAccess<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        recorder.read_member(name);
    }

    fn walk_in_binary(&self, binary: &'ast Binary<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.operator(binary.span(), &[binary.lhs, binary.rhs]);
    }

    fn walk_in_unary_prefix(&self, unary: &'ast UnaryPrefix<'arena>, recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>) {
        recorder.operator(unary.span(), &[unary.operand]);
        if unary.operator.is_increment_or_decrement() {
            recorder.write(unary.operand, unary.span(), None);
        }
    }

    fn walk_in_unary_postfix(
        &self,
        unary: &'ast UnaryPostfix<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        recorder.write(unary.operand, unary.span(), None);
    }
}

/// The plain PHP function `function` as the cause of its effects, with no class and the full name PHP# names it by,
/// and the `extern` declaration a call of it falls under. Spec section 23 names a function a library declares in PHP's
/// root namespace through the namespace an `extern namespace` line gives it, as `Illuminate\Foundation\now` for
/// Laravel's `now()` after `extern namespace Foundation for now;`, which `extern Foundation.now` declares.
fn function_extern(codebase: &CodebaseMetadata, function: Word) -> ((Word, Word), Option<&ExternMetadata>) {
    let name = codebase.get_function(function).map_or(function, |metadata| metadata.original_name);
    let cause = match codebase.extern_namespace_of(name) {
        Some(namespace) if !name.as_bytes().contains(&b'\\') => {
            word([namespace.as_bytes(), b"\\", name.as_bytes()].concat())
        }
        _ => name,
    };

    ((empty_word(), cause), codebase.extern_of(empty_word(), ascii_lowercase_word(function.as_bytes())))
}

/// The effects a call into plain PHP has under its `extern` line `declaration`: each effect its `uses` lists, or
/// [`Effect::Any`] for `uses any`.
fn plain_php_effects(declaration: &ExternMetadata) -> Vec<Effect> {
    let any = declaration.uses_any.then_some(Effect::Any);

    declaration.effects.iter().map(|effect| Effect::Class(*effect)).chain(any).collect()
}

/// What a value of plain PHP runs wherever it is called: the `callee`, with the effects its `extern` line
/// `declaration` declares, or, without one, the line it needs, `line`.
fn plain_php_runs(declaration: Option<&ExternMetadata>, callee: (Word, Word), line: (Word, Word)) -> Runs {
    match declaration {
        Some(declaration) => Runs::Declared { callee, effects: plain_php_effects(declaration) },
        None => Runs::Undeclared { callee, line },
    }
}

/// The `extern` line a use of a member the plain PHP class-like `class` declares needs, spec section 29: the class's
/// own, by its full name and an empty member.
fn class_line(codebase: &CodebaseMetadata, class: Word) -> (Word, Word) {
    (codebase.get_class_like(class).map_or(class, |metadata| metadata.original_name), empty_word())
}

/// The effect an object of `class` has, spec section 29: the abstract effect class it is or extends, nearest first, by
/// its original name. Code that reaches such an object has that effect, whatever its members do and whichever class,
/// the real one or a fake, is behind it.
fn effect_class(codebase: &CodebaseMetadata, class: Word) -> Option<Word> {
    let metadata = codebase.get_class_like(class)?;

    std::iter::once(metadata)
        .chain(metadata.all_parent_classes.iter().filter_map(|parent| codebase.get_class_like(*parent)))
        .find(|metadata| metadata.flags.is_abstract_effect())
        .map(|metadata| metadata.original_name)
}

/// The index of the parameter of `parameters` that `argument`, at `position` in its call, is given to: the one it
/// names, or the one at its position. Arguments past the last parameter are its variadic values.
pub(crate) fn parameter_index(
    argument: &Argument<'_>,
    position: usize,
    parameters: &[FunctionLikeParameterMetadata],
) -> usize {
    match argument {
        Argument::Named(named) => {
            let name = php_variable_name(named.name.value);
            parameters.iter().position(|parameter| parameter.name.0 == name).unwrap_or(position)
        }
        Argument::Positional(_) => position.min(parameters.len().saturating_sub(1)),
    }
}

/// The lambda an expression is, unparenthesized.
fn lambda_literal<'ast, 'arena>(expression: &'ast Expression<'arena>) -> Option<&'ast Expression<'arena>> {
    let expression = expression.unparenthesized();

    matches!(expression, Expression::ArrowFunction(_) | Expression::Closure(_)).then_some(expression)
}

/// The effects the function types of `value_type` list, each once. A function value with a `source` runs a PHP# body,
/// whose effects its body's summary records instead. An effect is a class-like, named by its key, spec section 24.
fn declared_effects(codebase: &CodebaseMetadata, value_type: &TUnion) -> Vec<Word> {
    let mut effects: Vec<Word> = Vec::new();
    for atomic in value_type.types.iter() {
        let TAtomic::Callable(TCallable::Signature(signature)) = atomic else {
            continue;
        };
        if signature.source.is_some() {
            continue;
        }

        for effect in signature.effects.iter().flatten() {
            let key = codebase.class_key(*effect);
            if !effects.iter().any(|known| codebase.class_key(*known) == key) {
                effects.push(*effect);
            }
        }
    }

    effects
}
