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
use mago_syntax::cst::Expression;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Is;
use mago_syntax::cst::LocalDeclaration;
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
use crate::context::Context;
use crate::effects::Body;
use crate::effects::Call;
use crate::effects::Changed;
use crate::effects::Effect;
use crate::effects::EffectSummary;
use crate::effects::Roots;
use crate::statement::function_like::FunctionLikeBody;
use crate::utils::names::is_prelude_stub;

/// Records the summary of `body`, which messages name by `member`, and whose parameters are declared at `parameters`,
/// into `artifacts`.
pub(crate) fn record<'arena, A>(
    context: &Context<'_, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    body: Body,
    member: Word,
    parameters: Vec<Span>,
    code: FunctionLikeBody<'_, 'arena>,
) where
    A: Arena,
{
    let class = match body {
        Body::Method(class, _) | Body::Accessor(class, _, _) => class,
    };

    let imports = Arc::clone(&context.imported_names);
    let scope = Arc::clone(&context.scope);
    let empty_summary = || EffectSummary {
        body,
        member,
        effects: Vec::new(),
        calls: Vec::new(),
        changes: Vec::new(),
        imports: Arc::clone(&imports),
        scope: Arc::clone(&scope),
    };

    let mut recorder = Recorder {
        context,
        artifacts,
        class,
        parameters,
        locals: HashMap::default(),
        lambdas: HashMap::default(),
        subjects: Vec::new(),
        inlining: Vec::new(),
        grew: false,
        summary: empty_summary(),
    };

    // A local takes the roots of every value assigned to it anywhere in the body, so the walk repeats until no
    // local gains a root, and the last walk records with every local's roots known.
    loop {
        recorder.grew = false;
        recorder.summary = empty_summary();
        match code {
            FunctionLikeBody::Statements(statements, _) => {
                for statement in statements {
                    SummaryWalker.walk_statement(statement, &mut recorder);
                }
            }
            FunctionLikeBody::Expression(expression) | FunctionLikeBody::ExpressionStatement(expression) => {
                SummaryWalker.walk_expression(expression, &mut recorder);
            }
        }

        if !recorder.grew {
            break;
        }
    }

    let summary = recorder.summary;
    artifacts.effect_summaries.push(summary);
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
    summary: EffectSummary,
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
                if self.context.resolved_names.static_property_class(access).is_some() {
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
                if self.context.resolved_names.static_property_class(access).is_some() {
                    self.change(Roots::from([Changed::Shared]), span, place);
                    return;
                }

                let receiver = self.roots(access.object);
                self.change(receiver.clone(), span, place);
                let arguments = vec![value.map(|value| self.roots(value)).unwrap_or_default()];
                for callee in self.accessors(access.object, &access.property, b"set") {
                    self.summary.calls.push(Call {
                        callee,
                        span,
                        receiver: receiver.clone(),
                        arguments: arguments.clone(),
                    });
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

    /// The `accessor` bodies a read or write of `object.property` runs.
    fn accessors(
        &self,
        object: &Expression<'arena>,
        property: &ClassLikeMemberSelector<'arena>,
        accessor: &[u8],
    ) -> Vec<Body> {
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return Vec::new();
        };

        self.object_classes(object).into_iter().filter_map(|class| self.accessor(class, name.value, accessor)).collect()
    }

    fn accessor(&self, class: Word, property: &[u8], accessor: &[u8]) -> Option<Body> {
        let property = php_variable_name(property);
        let declaring = *self.codebase().get_class_like(class.as_bytes())?.declaring_property_ids.get(&property)?;
        let metadata = self.codebase().get_property(declaring.as_bytes(), property.as_bytes())?;

        metadata
            .hooks
            .contains_key(&word(accessor))
            .then(|| Body::Accessor(ascii_lowercase_word(declaring.as_bytes()), property, word(accessor)))
    }

    fn read_property(&mut self, object: &Expression<'arena>, property: &ClassLikeMemberSelector<'arena>, span: Span) {
        let receiver = self.roots(object);
        for callee in self.accessors(object, property, b"get") {
            self.summary.calls.push(Call { callee, span, receiver: receiver.clone(), arguments: Vec::new() });
        }
    }

    /// A member read without `this.` runs its `get` accessor as `this.member` does.
    fn read_member(&mut self, name: &ConstantAccess<'arena>) {
        if self.context.resolved_names.binding(&name.name) == Some(Binding::Member)
            && let Some(callee) = self.accessor(self.class, name.name.value(), b"get")
        {
            let span = name.span();
            self.summary.calls.push(Call {
                callee,
                span,
                receiver: Roots::from([Changed::This]),
                arguments: Vec::new(),
            });
        }
    }

    /// The roots of each argument, by the index of the parameter it binds.
    fn argument_roots(
        &self,
        arguments: &ArgumentList<'arena>,
        parameters: &[FunctionLikeParameterMetadata],
    ) -> Vec<Roots> {
        let mut roots = vec![Roots::new(); parameters.len()];
        let last = parameters.len().saturating_sub(1);
        for (position, argument) in arguments.arguments.iter().enumerate() {
            let index = match argument {
                Argument::Named(named) => {
                    let name = php_variable_name(named.name.value);
                    parameters.iter().position(|parameter| parameter.name.0 == name).unwrap_or(position)
                }
                // Arguments past the last parameter are its variadic values.
                Argument::Positional(_) => position.min(last),
            };

            if let Some(parameter_roots) = roots.get_mut(index) {
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

        let Expression::ConstantAccess(name) = callee.unparenthesized() else {
            return None;
        };
        let Some(Binding::Local(local)) = self.context.resolved_names.binding(&name.name) else {
            return None;
        };

        self.lambdas.get(&local.declaration).copied()
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

    fn call(&mut self, call: &'ast CallExpression<'arena>) {
        let span = call.span();
        let (receiver, arguments, function) = match call {
            CallExpression::Function(call) => (None, &call.argument_list, Some(call.function)),
            CallExpression::Method(call) => (Some(call.object), &call.argument_list, None),
            CallExpression::NullSafeMethod(call) => (Some(call.object), &call.argument_list, None),
            CallExpression::StaticMethod(call) => (None, &call.argument_list, None),
        };

        // A call of a lambda written in this body runs the lambda here. A call of any other function a local holds is
        // pure, as a `Function` type is until the parser gives `Function<…>` its `uses`.
        if let Some(lambda) = function.and_then(|function| self.lambda(function)) {
            let roots: Vec<Roots> = arguments.arguments.iter().map(|argument| self.roots(argument.value())).collect();
            self.inline(lambda, |index| roots.get(index).cloned().unwrap_or_default());

            return;
        }

        let artifacts = self.artifacts;
        let Some(targets) = artifacts.call_targets.get(&(span.start.offset, span.end.offset)) else {
            return;
        };

        for target in targets {
            match *target {
                CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Function(function_name), .. } => {
                    let cause = self
                        .codebase()
                        .get_function(function_name.as_bytes())
                        .map_or(function_name, |metadata| metadata.original_name);
                    let declaration = self.extern_of(empty_word(), ascii_lowercase_word(function_name.as_bytes()));
                    self.plain_php(declaration, span, (empty_word(), sharp_name(cause)));
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
    /// `Function` type, which is pure without `uses`. A plain PHP closure or callable is `Unknown`, because an `extern`
    /// declares a class's members, not the code a property holds. An object's `__invoke` is recorded as the method it
    /// is.
    fn property_call(&mut self, class: Word, property: Word, span: Span) {
        let codebase = self.codebase();
        let Some(class_like) = codebase.get_class_like(class.as_bytes()) else {
            return;
        };
        if class_like.flags.is_sharp() {
            return;
        }

        let holds_object = codebase
            .get_property(class.as_bytes(), property.as_bytes())
            .and_then(|metadata| metadata.type_metadata.as_ref())
            .is_some_and(|metadata| {
                metadata.type_union.types.iter().all(|atomic| {
                    matches!(atomic, TAtomic::Object(object)
                        if object.get_name().is_some_and(|name| !name.as_bytes().eq_ignore_ascii_case(b"Closure")))
                })
            });
        if holds_object {
            return;
        }

        let property = property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes());
        self.summary.effects.push((Effect::Unknown(None), span, (class_like.original_name, word(property))));
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

        if codebase.get_class_like(declaring_class.as_bytes()).is_some_and(|declaring| declaring.flags.is_sharp()) {
            let callee = Body::Method(
                ascii_lowercase_word(declaring_class.as_bytes()),
                ascii_lowercase_word(method_name.as_bytes()),
            );
            let receiver = receiver.map(|receiver| self.roots(receiver)).unwrap_or_default();
            let arguments = self.argument_roots(arguments, &metadata.parameters);
            self.summary.calls.push(Call { callee, span, receiver, arguments });

            return;
        }

        let class_name = codebase.get_class_like(class.as_bytes()).map_or(class, |metadata| metadata.original_name);
        let cause = (class_name, metadata.original_name);
        if is_prelude_stub(declaring_class) || is_prelude_stub(class) {
            self.prelude_stub_call(class, metadata, span, receiver, arguments, cause);
        } else {
            let declaration = self.extern_of(class, ascii_lowercase_word(method_name.as_bytes()));
            self.plain_php(declaration, span, cause);
        }
    }

    /// Records a `new`: a PHP# class runs its constructor's body on a fresh object, and plain PHP has the effects
    /// its `extern` declares.
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
        let constructor = word(b"__construct");
        if class.flags.is_sharp() {
            let Some(declaring_class) =
                codebase.get_declaring_method_class(class.name.as_bytes(), constructor.as_bytes())
            else {
                return;
            };
            let Some(metadata) = codebase.get_method(declaring_class.as_bytes(), constructor.as_bytes()) else {
                return;
            };

            let arguments = instantiation
                .argument_list
                .as_ref()
                .map(|arguments| self.argument_roots(arguments, &metadata.parameters))
                .unwrap_or_default();
            let callee = Body::Method(ascii_lowercase_word(declaring_class.as_bytes()), constructor);
            self.summary.calls.push(Call { callee, span, receiver: Roots::new(), arguments });
        } else if !is_prelude_stub(class.name) {
            let declaration = self.extern_of(class.name, constructor);
            self.plain_php(declaration, span, (class.original_name, empty_word()));
        }
    }

    /// The `extern` declaration a call of `member` (empty for a function's own name) on `class` (empty for a
    /// function) falls under: the member's, then the class's, then the same for each parent class, nearest first.
    fn extern_of(&self, class: Word, member: Word) -> Option<&'ctx ExternMetadata> {
        let codebase = self.codebase();
        let declared = |key: (Word, Word)| codebase.externs.get(&key).and_then(|declarations| declarations.first());
        if class.is_empty() {
            return declared((empty_word(), member));
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

    /// Records a call into plain PHP, of the callee `cause`: the effects its declaration lists, or `Unknown` without
    /// one.
    fn plain_php(&mut self, declaration: Option<&ExternMetadata>, span: Span, cause: (Word, Word)) {
        match declaration {
            Some(declaration) => {
                for effect in &declaration.effects {
                    self.summary.effects.push((Effect::Foreign(*effect), span, cause));
                }
            }
            None => self.summary.effects.push((Effect::Unknown(Some(cause)), span, cause)),
        }
    }

    // Deleted once the standard library's `.sharp` sources replace the prelude stubs (php-sharp issue #60).
    fn prelude_stub_call(
        &mut self,
        class: Word,
        metadata: &FunctionLikeMetadata,
        span: Span,
        receiver: Option<&'ast Expression<'arena>>,
        arguments: &'ast ArgumentList<'arena>,
        cause: (Word, Word),
    ) {
        if class.as_bytes().eq_ignore_ascii_case(b"Sharp\\Environment") {
            self.summary.effects.push((Effect::Foreign(word(b"Sharp\\Environment")), span, cause));
        }

        // A function passed to the stub runs on the receiver's elements, as `uses f` will declare.
        let elements = receiver.map(|receiver| self.roots(receiver)).unwrap_or_default();
        for argument in &arguments.arguments {
            let value = argument.value();
            if let Some(lambda) = self.lambda(value) {
                self.inline(lambda, |_| elements.clone());
            } else if let Expression::Access(Access::Property(method)) = value.unparenthesized() {
                self.method_value(method, span, &elements);
            }
        }

        if !metadata.flags.is_mutation_free()
            && let Some(receiver) = receiver
        {
            self.change(self.roots(receiver), span, receiver);
        }
    }

    /// A method value, such as `this.label`, passed to a call that runs it on `elements`.
    fn method_value(&mut self, value: &PropertyAccess<'arena>, span: Span, elements: &Roots) {
        let ClassLikeMemberSelector::Identifier(name) = &value.property else {
            return;
        };

        let receiver = self.roots(value.object);
        for class in self.object_classes(value.object) {
            let codebase = self.codebase();
            let Some(declaring_class) = codebase.get_declaring_method_class(class.as_bytes(), name.value) else {
                continue;
            };
            let Some(metadata) = codebase.get_method(declaring_class.as_bytes(), name.value) else {
                continue;
            };

            if codebase.get_class_like(declaring_class.as_bytes()).is_some_and(|declaring| declaring.flags.is_sharp()) {
                let callee =
                    Body::Method(ascii_lowercase_word(declaring_class.as_bytes()), ascii_lowercase_word(name.value));
                let arguments = vec![elements.clone(); metadata.parameters.len()];
                self.summary.calls.push(Call { callee, span, receiver: receiver.clone(), arguments });
            }
        }
    }
}

/// Walks one body, leaving out the lambdas and classes written in it: a lambda runs only where it is called.
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
    fn walk_closure(&self, _: &'ast Closure<'arena>, _: &mut Recorder<'_, '_, 'ast, 'arena, A>) {}

    fn walk_arrow_function(&self, _: &'ast ArrowFunction<'arena>, _: &mut Recorder<'_, '_, 'ast, 'arena, A>) {}

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
        recorder.store(declaration.name.span, declaration.value);
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

    fn walk_in_property_access(
        &self,
        access: &'ast PropertyAccess<'arena>,
        recorder: &mut Recorder<'_, '_, 'ast, 'arena, A>,
    ) {
        if recorder.context.resolved_names.static_property_class(access).is_none() {
            recorder.read_property(access.object, &access.property, access.span());
        }
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

/// The lambda an expression is, unparenthesized.
fn lambda_literal<'ast, 'arena>(expression: &'ast Expression<'arena>) -> Option<&'ast Expression<'arena>> {
    let expression = expression.unparenthesized();

    matches!(expression, Expression::ArrowFunction(_) | Expression::Closure(_)).then_some(expression)
}

/// A plain PHP name as PHP# writes it: `App\now` as `App.now`.
fn sharp_name(name: Word) -> Word {
    let bytes: Vec<u8> = name.as_bytes().iter().map(|&byte| if byte == b'\\' { b'.' } else { byte }).collect();

    word(&bytes)
}
