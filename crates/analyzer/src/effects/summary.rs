//! Records a PHP# body's [`EffectSummary`] from its syntax, after the analysis has resolved its calls.

use std::sync::Arc;

use foldhash::HashMap;
use mago_allocator::Arena;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::r#extern::ExternMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::TCallableSignature;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
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
use crate::effects::Body;
use crate::effects::Call;
use crate::effects::Changed;
use crate::effects::Effect;
use crate::effects::EffectSummary;
use crate::effects::FunctionValue;
use crate::effects::Roots;
use crate::effects::Runs;
use crate::effects::member_of_foreign_class;
use crate::resolver::static_property::StaticProperty;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::names::display_function_holder;
use crate::utils::names::display_sharp_type;
use crate::utils::names::is_prelude_stub;
use crate::utils::names::library_namespace;

/// Records the summary of the method, accessor or initial value `body`, which messages name by `member`, and whose
/// parameters are declared at `parameters`, into `artifacts`, with the summary of each lambda written in it, and with
/// the function values the analysis gave places since the last body. A constructor's `initializer` runs before `code`.
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
        Body::Method(class, _) | Body::Accessor(class, _, _) | Body::InitialValue(class, _) => class,
        Body::Lambda(_) => empty_word(),
    };

    let function_values = std::mem::take(&mut artifacts.function_values);
    let readonly_calls = std::mem::take(&mut artifacts.readonly_calls);
    let (mut summary, mut lambdas) = summarize(context, artifacts, class, body, member, parameters, initializer, code);
    summary.function_values = function_values;
    summary.readonly_calls = readonly_calls;
    // Code that reaches a `foreign` object has its class's effect, whatever its members do (spec section 29), so each
    // member has it too. A pure declaration then refuses a `foreign` implementation, and a call through an interface or
    // a base class has the effect of the `foreign` override it reaches.
    if let Some(effect) = foreign_class(context.codebase, class) {
        summary.effects.insert(0, (Effect::Foreign(effect), code.span(), member_of_foreign_class()));
    }
    artifacts.effect_summaries.push(summary);
    artifacts.effect_summaries.append(&mut lambdas);
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
            })
            .collect();
        artifacts.effect_summaries.push(EffectSummary {
            body: Body::Accessor(class.name, property, word(accessor)),
            member,
            effects: Vec::new(),
            calls,
            changes: Vec::new(),
            function_values: Vec::new(),
            readonly_calls: Vec::new(),
            imports: Arc::clone(&context.imported_names),
            scope: Arc::clone(&context.scope),
        });
    }
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

    let mut allowed: Option<Vec<Word>> = None;
    for atomic in declared.types.iter() {
        let TAtomic::Callable(TCallable::Signature(TCallableSignature { effects: Some(effects), .. })) = atomic else {
            continue;
        };

        allowed = Some(match allowed {
            None => effects.clone(),
            Some(known) => known
                .into_iter()
                .filter(|known| effects.iter().any(|effect| effect.as_bytes().eq_ignore_ascii_case(known.as_bytes())))
                .collect(),
        });
    }
    let Some(allowed) = allowed else {
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
            Some(FunctionLikeIdentifier::Method(class, method)) => {
                method_value_runs(codebase, class, method.as_bytes())
            }
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
fn method_value_runs(codebase: &CodebaseMetadata, class: Word, method: &[u8]) -> Option<Runs> {
    let declaring_class = codebase.get_declaring_method_class(class.as_bytes(), method)?;
    let declaring = codebase.get_class_like(declaring_class.as_bytes())?;
    let metadata = codebase.get_method(declaring_class.as_bytes(), method)?;

    // `class` is the class the value is read through, where a call's `extern` lookup starts too.
    let class_name = codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
    let callee = (class_name, metadata.original_name);
    let lowercase_method = ascii_lowercase_word(method);

    Some(if let Some(effect) = foreign_class(codebase, class) {
        Runs::Foreign { callee, effects: vec![Effect::Foreign(effect)] }
    } else if declaring.flags.is_sharp() && metadata.method_metadata.as_ref().is_some_and(|m| m.is_abstract) {
        Runs::Foreign { callee, effects: metadata.uses.iter().map(|effect| Effect::Foreign(*effect)).collect() }
    } else if declaring.flags.is_sharp() {
        Runs::Body(Body::Method(ascii_lowercase_word(declaring_class.as_bytes()), lowercase_method))
    } else if is_prelude_stub(declaring_class) {
        Runs::Foreign { callee, effects: Vec::new() }
    } else {
        Runs::Foreign { callee, effects: plain_php_effects(extern_of(codebase, class, lowercase_method), callee) }
    })
}

/// What a value of the plain PHP function `function` runs wherever it is called, spec section 14.3: the effects its
/// `extern` declares, as a call of the function has them.
fn function_value_runs(codebase: &CodebaseMetadata, function: Word) -> Runs {
    let (callee, declaration) = function_extern(codebase, function);

    Runs::Foreign { callee, effects: plain_php_effects(declaration, callee) }
}

/// The summary of `body`, a body of the class `class`, and the summaries of the lambdas written in it.
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
) -> (EffectSummary, Vec<EffectSummary>)
where
    A: Arena,
{
    let imports = Arc::clone(&context.imported_names);
    let scope = Arc::clone(&context.scope);
    let empty_summary = || EffectSummary {
        body,
        member,
        effects: Vec::new(),
        calls: Vec::new(),
        changes: Vec::new(),
        function_values: Vec::new(),
        readonly_calls: Vec::new(),
        imports: Arc::clone(&imports),
        scope: Arc::clone(&scope),
    };

    // The method's function parameters whose types state no effects, whose functions each caller runs as part of
    // its call.
    let effect_taking_parameters = match body {
        Body::Method(class, method) => context
            .codebase
            .get_method(class.as_bytes(), method.as_bytes())
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
                        .get_method(class.as_bytes(), method.as_bytes())
                        .is_some_and(|method| method.flags.is_named_constructor())
            }
            _ => false,
        },
        summary: empty_summary(),
        lambda_summaries: Vec::new(),
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

    (recorder.summary, recorder.lambda_summaries)
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
                Some(Binding::This | Binding::Member | Binding::Field) => Roots::from([Changed::This]),
                Some(Binding::Local(local)) => self.local_roots(local),
                _ => Roots::new(),
            },
            Expression::Parent(_) => Roots::from([Changed::This]),
            Expression::Access(Access::Property(access)) => {
                if StaticProperty::from_property_access(access, self.context.resolved_names).is_some() {
                    Roots::from([Changed::Shared])
                } else {
                    self.roots(access.object)
                }
            }
            Expression::Access(Access::NullSafeProperty(access)) => self.roots(access.object),
            Expression::Access(Access::StaticProperty(_)) => Roots::from([Changed::Shared]),
            Expression::ArrayAccess(access) => self.roots(access.array),
            // A call's result has the roots of what the call is given.
            Expression::Call(call) => {
                let (receiver, arguments) = match call {
                    CallExpression::Function(call) => (None, &call.argument_list),
                    CallExpression::NullSafeFunction(call) => (None, &call.argument_list),
                    CallExpression::Method(call) => (Some(call.object), &call.argument_list),
                    CallExpression::NullSafeMethod(call) => (Some(call.object), &call.argument_list),
                    CallExpression::StaticMethod(call) => (None, &call.argument_list),
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
        let classes = match object.unparenthesized() {
            Expression::ConstantAccess(class)
                if self.context.resolved_names.binding(&class.name) == Some(Binding::Class) =>
            {
                vec![word(self.context.resolved_names.get(&class.name))]
            }
            _ => self.object_classes(object),
        };

        classes.into_iter().filter_map(|class| self.accessor(class, name.value, accessor)).collect()
    }

    /// The `accessor` body of `property` that a read or write through `class` runs, by the class that declares the
    /// property. It has a summary only where PHP# code gives it one: an accessor body, an observer, or the behaviors
    /// `via` names, which a property's PHP metadata lists no hook for. A call of a body without one adds nothing.
    fn accessor(&self, class: Word, property: &[u8], accessor: &[u8]) -> Option<Body> {
        let property = php_variable_name(property);
        let declaring = *self.codebase().get_class_like(class.as_bytes())?.declaring_property_ids.get(&property)?;

        Some(Body::Accessor(ascii_lowercase_word(declaring.as_bytes()), property, word(accessor)))
    }

    fn read_property(&mut self, object: &Expression<'arena>, property: &ClassLikeMemberSelector<'arena>, span: Span) {
        // A member named at run time, as `payments.{name}`, still reaches the object.
        let member = match property {
            ClassLikeMemberSelector::Identifier(name) => word(name.value),
            _ => empty_word(),
        };
        let codebase = self.codebase();
        for class in self.object_classes(object) {
            if let Some(effect) = foreign_class(codebase, class) {
                let class = codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
                self.summary.effects.push((Effect::Foreign(effect), span, (class, member)));
            }
        }

        let receiver = self.roots(object);
        for callee in self.accessors(object, property, b"get") {
            self.summary.calls.push(Call { callee, span, receiver: receiver.clone(), arguments: Vec::new() });
        }

        if let Expression::ConstantAccess(class) = object.unparenthesized()
            && self.context.resolved_names.binding(&class.name) == Some(Binding::Class)
            && let ClassLikeMemberSelector::Identifier(name) = property
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
            .get_class_like(class.as_bytes())
            .and_then(|class| class.declaring_property_ids.get(&property))
            .and_then(|declaring| codebase.get_class_like(declaring.as_bytes()))
            .filter(|declaring| declaring.properties.get(&property).is_some_and(|used| used.flags.is_static()))
        else {
            return;
        };

        for (name, static_property) in &declaring.properties {
            if static_property.flags.is_static() && static_property.flags.has_runtime_initial_value() {
                let callee = Body::InitialValue(declaring.name, *name);
                self.summary.calls.push(Call { callee, span, receiver: Roots::new(), arguments: Vec::new() });
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
        let (summary, lambdas) =
            summarize(self.context, self.artifacts, self.class, body, empty_word(), parameters, None, code);
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
            CallExpression::StaticMethod(call) => (None, &call.argument_list, None),
        };

        // A call of a lambda written in this body runs the lambda here. A call of any other function value, such as a
        // local's or a parameter's, has the effects its function type lists.
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
            for effect in declared_effects(function_type) {
                self.summary.effects.push((Effect::Foreign(effect), span, cause));
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
                    self.plain_php(declaration, span, cause);
                    self.callbacks(arguments);
                }
                CallTarget::FunctionLike {
                    callee: FunctionLikeIdentifier::Method(declaring_class, method_name),
                    class: named_class,
                } => {
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
                CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Closure(_), .. }
                | CallTarget::MagicMethod { .. } => {}
            }
        }
    }

    /// Records a call of the property `property` of `class` that holds a function. A PHP# class declares it with a
    /// `Function` type, which has the effects its `uses` lists. A plain PHP closure or callable is `Unknown`, because an
    /// `extern` declares a class's members, not the code a property holds. An object's `__invoke` is recorded as the
    /// method it is.
    fn property_call(&mut self, class: Word, property: Word, span: Span) {
        let codebase = self.codebase();
        let Some(class_like) = codebase.get_class_like(class.as_bytes()) else {
            return;
        };
        let property_type = codebase
            .get_property(class.as_bytes(), property.as_bytes())
            .and_then(|metadata| metadata.type_metadata.as_ref())
            .map(|metadata| &metadata.type_union);
        let cause =
            (class_like.original_name, word(property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes())));
        if class_like.flags.is_sharp() {
            for effect in property_type.map(declared_effects).unwrap_or_default() {
                self.summary.effects.push((Effect::Foreign(effect), span, cause));
            }

            return;
        }

        let holds_object = property_type.is_some_and(|property_type| {
            property_type.types.iter().all(|atomic| {
                matches!(atomic, TAtomic::Object(object)
                    if object.get_name().is_some_and(|name| !name.as_bytes().eq_ignore_ascii_case(b"Closure")))
            })
        });
        if holds_object {
            return;
        }

        self.summary.effects.push((Effect::Unknown(None), span, cause));
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

            let callee = Body::Method(ascii_lowercase_word(class.as_bytes()), ascii_lowercase_word(method.as_bytes()));
            let arguments = operands.iter().map(|operand| self.roots(operand)).collect();
            self.summary.calls.push(Call { callee, span, receiver: Roots::new(), arguments });
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
        let Some(metadata) = codebase.get_method(declaring_class.as_bytes(), method_name.as_bytes()) else {
            return;
        };

        let class_name = codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
        let cause = (class_name, metadata.original_name);
        let receiver_roots = receiver.map(|receiver| self.roots(receiver)).unwrap_or_default();
        let roots = self.argument_roots(arguments, &metadata.parameters);
        self.passed_functions(arguments, &metadata.parameters, &receiver_roots, &roots);
        if let Some(effect) = foreign_class(codebase, class) {
            self.summary.effects.push((Effect::Foreign(effect), span, cause));

            return;
        }

        if codebase.get_class_like(declaring_class.as_bytes()).is_some_and(|declaring| declaring.flags.is_sharp()) {
            // A method without a body has the effects its `uses` lists, which each implementation is held to.
            if metadata.method_metadata.as_ref().is_some_and(|method| method.is_abstract) {
                for effect in &metadata.uses {
                    self.summary.effects.push((Effect::Foreign(*effect), span, cause));
                }

                return;
            }

            let callee = Body::Method(
                ascii_lowercase_word(declaring_class.as_bytes()),
                ascii_lowercase_word(method_name.as_bytes()),
            );
            self.summary.calls.push(Call { callee, span, receiver: receiver_roots, arguments: roots });

            return;
        }

        // `passed_functions` already took on the functions a collection method is given, as a PHP# method's, and plain
        // PHP may run any function it is given.
        if is_prelude_stub(declaring_class) {
            self.prelude_stub_call(metadata, span, receiver);
        } else {
            let declaration = extern_of(codebase, class, ascii_lowercase_word(method_name.as_bytes()));
            self.plain_php(declaration, span, cause);
            self.callbacks(arguments);
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
        let effects = self.artifacts.get_expression_type(value).map(declared_effects).unwrap_or_default();
        let cause = (empty_word(), self.text(span));
        for effect in effects {
            self.summary.effects.push((Effect::Foreign(effect), span, cause));
        }
    }

    /// Records `: super(…)`: the call of the base class's constructor the analysis resolved at its span.
    fn constructor_initializer(&mut self, initializer: &'ast ConstructorInitializer<'arena>) {
        self.call_targets(initializer.span(), None, &initializer.argument_list);
    }

    /// Records a `new`: a constructor PHP# declares runs its body on a fresh object, and one plain PHP declares has the
    /// effects of the `extern` of the class `new` names, which an error names as `new` does, or its nearest plain PHP
    /// parent when PHP# declares it. The class that declares the constructor decides between the two, and the class
    /// `new` names decides when no class declares one. A named constructor is the class's own, spec section
    /// 9.1, and its body runs on the object its `: this(…)` creates.
    fn instantiate(&mut self, instantiation: &'ast Instantiation<'arena>) {
        let resolved_names = self.context.resolved_names;
        let class_name = match instantiation.class.unparenthesized() {
            Expression::Identifier(identifier) => resolved_names.resolve(identifier),
            Expression::ConstantAccess(name) => resolved_names.resolve(&name.name),
            _ => None,
        };
        let codebase = self.codebase();
        let Some(class) = class_name.and_then(|class_name| codebase.get_class_like(class_name)) else {
            return;
        };

        let span = instantiation.span();
        let (declaring, constructor) = match &instantiation.constructor {
            Some(constructor) => (class, ascii_lowercase_word(constructor.name.value)),
            None => {
                let constructor = word(b"__construct");
                let declaring = codebase
                    .get_declaring_method_class(class.name.as_bytes(), constructor.as_bytes())
                    .and_then(|declaring_class| codebase.get_class_like(declaring_class.as_bytes()))
                    .unwrap_or(class);

                (declaring, constructor)
            }
        };
        if declaring.flags.is_sharp() {
            let Some(metadata) = codebase.get_method(declaring.name.as_bytes(), constructor.as_bytes()) else {
                return;
            };

            let mut roots = Vec::new();
            if let Some(arguments) = &instantiation.argument_list {
                roots = self.argument_roots(arguments, &metadata.parameters);
                self.passed_functions(arguments, &metadata.parameters, &Roots::new(), &roots);
            }
            let callee = Body::Method(ascii_lowercase_word(declaring.name.as_bytes()), constructor);
            self.summary.calls.push(Call { callee, span, receiver: Roots::new(), arguments: roots });
        } else if !is_prelude_stub(class.name) {
            let declaration = extern_of(codebase, class.name, constructor);
            // Only plain PHP takes an `extern`, so a PHP# class's error names its nearest plain PHP parent.
            let named = std::iter::successors(Some(class), |class| {
                class.direct_parent_class.and_then(|parent| codebase.get_class_like(parent.as_bytes()))
            })
            .find(|class| !class.flags.is_sharp())
            .unwrap_or(declaring);
            self.plain_php(declaration, span, (named.original_name, empty_word()));
        }
    }

    /// Records an `emit`, which has the effect `Sharp\Events` for each class its event may be.
    fn emit(&mut self, emit: &'ast Emit<'arena>) {
        let span = emit.span();
        for class in self.object_classes(emit.value) {
            let class =
                self.codebase().get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
            self.summary.effects.push((Effect::Emit, span, (class, empty_word())));
        }
    }

    /// Records a call into plain PHP, of the callee `cause`, with the effects `declaration` gives it.
    fn plain_php(&mut self, declaration: Option<&ExternMetadata>, span: Span, cause: (Word, Word)) {
        for effect in plain_php_effects(declaration, cause) {
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
                    method_value_runs(codebase, *class, method.as_bytes())?,
                    codebase.get_declaring_method(class.as_bytes(), method.as_bytes())?,
                )),
                TAtomic::Callable(TCallable::Signature(TCallableSignature {
                    source: Some(FunctionLikeIdentifier::Function(function)),
                    ..
                })) => Some((function_value_runs(codebase, *function), codebase.get_function(function.as_bytes())?)),
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
                    self.summary.calls.push(Call { callee, span, receiver: Roots::new(), arguments });
                }
                Runs::Body(callee) => {
                    let arguments = vec![elements.clone(); metadata.parameters.len()];
                    self.summary.calls.push(Call { callee, span, receiver: receiver.clone(), arguments });
                }
                Runs::Foreign { callee, effects } => {
                    for effect in effects {
                        self.summary.effects.push((effect, span, callee));
                    }
                }
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
/// root namespace through its [library namespace](library_namespace), as `Illuminate\Foundation\now` for Laravel's
/// `now()`, which `extern Foundation.now` declares.
fn function_extern(codebase: &CodebaseMetadata, function: Word) -> ((Word, Word), Option<&ExternMetadata>) {
    let metadata = codebase.get_function(function.as_bytes());
    let name = metadata.map_or(function, |metadata| metadata.original_name);
    let cause = match metadata.and_then(|metadata| library_namespace(codebase, metadata.span)) {
        Some(namespace) if !name.as_bytes().contains(&b'\\') => {
            word([namespace.as_bytes(), b"\\", name.as_bytes()].concat())
        }
        _ => name,
    };

    ((empty_word(), cause), extern_of(codebase, empty_word(), ascii_lowercase_word(function.as_bytes())))
}

/// The `extern` declaration a call of `member` (empty for a function's own name) on `class` (empty for a function)
/// falls under: the member's, then the class's, then the same for each parent class, nearest first. A function in a
/// namespace is declared as `extern Pdf.render`, which the binder reads as a class and its member, spec section 23,
/// and so is one a library declares in PHP's root namespace, through a namespace of the class-likes in its folder, as
/// `extern Foundation.now` declares Laravel's `now()`.
pub(crate) fn extern_of(codebase: &CodebaseMetadata, class: Word, member: Word) -> Option<&ExternMetadata> {
    let declared = |key: (Word, Word)| codebase.externs.get(&key).and_then(|declarations| declarations.first());
    if class.is_empty() {
        return declared((empty_word(), member)).or_else(|| {
            let name = member.as_bytes();
            match name.iter().rposition(|byte| *byte == b'\\') {
                Some(separator) => declared((word(&name[..separator]), word(&name[separator + 1..]))),
                None => codebase
                    .folder_namespaces(codebase.get_function(name)?.span)
                    .iter()
                    .find_map(|namespace| declared((ascii_lowercase_word(namespace.as_bytes()), member))),
            }
        });
    }

    let mut current = Some(ascii_lowercase_word(class.as_bytes()));
    let mut remaining =
        codebase.get_class_like(class.as_bytes()).map_or(0, |metadata| metadata.all_parent_classes.len());
    while let Some(class) = current {
        if let Some(declaration) = declared((class, member)).or_else(|| declared((class, empty_word()))) {
            return Some(declaration);
        }

        if remaining == 0 {
            break;
        }
        remaining -= 1;
        current = codebase
            .get_class_like(class.as_bytes())
            .and_then(|metadata| metadata.direct_parent_class)
            .map(|parent| ascii_lowercase_word(parent.as_bytes()));
    }

    None
}

/// The effects of a call into plain PHP of the callee `cause`: the effects its declaration lists, or `Unknown` without
/// one.
fn plain_php_effects(declaration: Option<&ExternMetadata>, cause: (Word, Word)) -> Vec<Effect> {
    match declaration {
        Some(declaration) => declaration.effects.iter().map(|effect| Effect::Foreign(*effect)).collect(),
        None => vec![Effect::Unknown(Some(cause))],
    }
}

/// The `foreign` class an object of `class` is, spec section 29: `class` itself or a parent marked `foreign`, by its
/// original name. Code that reaches such an object has that class's effect, whatever its members do.
fn foreign_class(codebase: &CodebaseMetadata, class: Word) -> Option<Word> {
    let metadata = codebase.get_class_like(class.as_bytes())?;

    std::iter::once(metadata)
        .chain(metadata.all_parent_classes.iter().filter_map(|parent| codebase.get_class_like(parent.as_bytes())))
        .find(|metadata| metadata.flags.is_foreign())
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
/// whose effects its body's summary records instead.
fn declared_effects(value_type: &TUnion) -> Vec<Word> {
    let mut effects: Vec<Word> = Vec::new();
    for atomic in value_type.types.iter() {
        let TAtomic::Callable(TCallable::Signature(signature)) = atomic else {
            continue;
        };
        if signature.source.is_some() {
            continue;
        }

        for effect in signature.effects.iter().flatten() {
            if !effects.iter().any(|known| known.as_bytes().eq_ignore_ascii_case(effect.as_bytes())) {
                effects.push(*effect);
            }
        }
    }

    effects
}
