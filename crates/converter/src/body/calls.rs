//! Calls and `new`: method and static calls with their arguments, bare function calls, type tests as `is`, and `new`
//! with its type arguments (spec sections 7, 8, 11 and 21).

use std::borrow::Cow;

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PartialApplication;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StaticMethodCall;
use mago_syntax::walker::MutWalker;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use super::Kind;
use super::access::HELD_CLASS;
use super::is_primary;
use super::natively_checked;
use crate::Converter;
use crate::library::type_test;
use crate::types::raw_generic;
use crate::types::type_text;

/// What a method's body reads of the class PHP called: whether it names `static` or `parent`, and the methods it calls
/// through `self`, which forward that class.
#[derive(Default)]
struct CalledClass {
    names_static: bool,
    through_self: Vec<Vec<u8>>,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for CalledClass {
    fn walk_in_expression(&mut self, expression: &'ast Expression<'arena>, _: &mut ()) {
        let (class, method) = match expression {
            Expression::Static(_) | Expression::Parent(_) => return self.names_static = true,
            Expression::Call(Call::StaticMethod(call)) => (call.class, &call.method),
            Expression::PartialApplication(PartialApplication::StaticMethod(value)) => (value.class, &value.method),
            _ => return,
        };
        match (class, method) {
            (Expression::Self_(_), ClassLikeMemberSelector::Identifier(name)) => {
                self.through_self.push(name.value.to_vec());
            }
            (Expression::Self_(_), _) => self.names_static = true,
            _ => {}
        }
    }
}

/// The class of an object of the type `object`, where it is one named class.
fn object_class(object: Option<&TUnion>) -> Option<Word> {
    match object?.types.as_ref() {
        [TAtomic::Object(TObject::Named(class))] => Some(class.name),
        _ => None,
    }
}

impl<'input> Converter<'input, '_> {
    /// The method `selector` names on a value of type `object`, when the analysis knows the value's class, or the first
    /// class of an intersection, as `Item & Shareable`, that has it.
    pub(super) fn method_of(
        &self,
        object: Option<&TUnion>,
        selector: &ClassLikeMemberSelector<'_>,
    ) -> Option<&'input FunctionLikeMetadata> {
        let ClassLikeMemberSelector::Identifier(name) = selector else {
            return None;
        };
        let [atomic @ TAtomic::Object(TObject::Named(class))] = object?.types.as_ref() else {
            return None;
        };
        let codebase = self.input.codebase;

        std::iter::once(class.name)
            .chain(atomic.get_intersection_types().unwrap_or_default().iter().filter_map(
                |intersected| match intersected {
                    TAtomic::Object(TObject::Named(other)) => Some(other.name),
                    _ => None,
                },
            ))
            .find_map(|class| codebase.get_declaring_method(class.as_bytes(), name.value))
    }

    /// The method `selector` names, written as its class declares it: PHP finds a method in any case, and PHP# only in
    /// its own (spec section 25). A call of a method the object's class doesn't have, which PHP reaches through an
    /// implementation or `__call`, is refused.
    pub(super) fn method_name(
        &mut self,
        object: &Expression<'_>,
        selector: &ClassLikeMemberSelector<'_>,
        method: Option<&FunctionLikeMetadata>,
    ) {
        let ClassLikeMemberSelector::Identifier(name) = selector else {
            return;
        };
        match method {
            // PHP's `__toString()` is PHP#'s `toString()` (spec section 18).
            Some(method) if method.original_name.as_bytes().eq_ignore_ascii_case(b"__tostring") => {
                self.edits.replace(name.span, "toString");
            }
            Some(method) if name.value != method.original_name.as_bytes() => {
                self.edits.replace(name.span, method.original_name.as_str_lossy().into_owned());
            }
            Some(_) => {}
            None => {
                let Some([TAtomic::Object(TObject::Named(class))]) =
                    self.type_of(object).map(|union| union.types.as_ref())
                else {
                    return;
                };
                let codebase = self.input.codebase;
                if codebase.get_class_like(class.name.as_bytes()).is_some()
                    && codebase.get_declaring_method(class.name.as_bytes(), b"__call").is_none()
                {
                    let (method, class) = (String::from_utf8_lossy(name.value), class.name.as_str_lossy());
                    self.refuse(
                        name.span,
                        format!(
                            "`{class}` declares no method `{method}`, which PHP# calls only on a class that has it"
                        ),
                    );
                }
            }
        }
    }

    /// The arguments of a call of `callee`, when the analysis knows it. An argument the analysis gives `mixed` reaches
    /// a parameter of a known type only in PHP.
    pub(crate) fn arguments(&mut self, arguments: &ArgumentList<'_>, callee: Option<&FunctionLikeMetadata>) {
        for (index, argument) in arguments.arguments.iter().enumerate() {
            // The parameter an argument fills, and PHP's number of the argument: the parameter's place for a named
            // one, and its own place for a positional one, as each one past the `...` parameter has.
            let filled = callee.and_then(|callee| match argument {
                Argument::Named(argument) => {
                    let name = [b"$".as_slice(), argument.name.value].concat();
                    callee
                        .parameters
                        .iter()
                        .enumerate()
                        .find(|(_, parameter)| parameter.get_name().0.as_bytes() == name)
                }
                // Each argument past the `...` parameter lands in it.
                Argument::Positional(_) => callee
                    .parameters
                    .get(index)
                    .or_else(|| callee.parameters.last().filter(|parameter| parameter.flags.is_variadic()))
                    .map(|parameter| (index, parameter)),
            });
            let parameter = filled.map(|(_, parameter)| parameter);
            let declared = parameter
                .and_then(|parameter| parameter.get_type_metadata())
                .map(|declared| &declared.type_union)
                .filter(|declared| !declared.types.iter().any(TAtomic::is_mixed));
            let value = match argument {
                // Only a `List` spreads into a call, and one of unknown length fills only a `...` parameter (spec
                // section 7). PHP spreads a string key as a named argument.
                Argument::Positional(argument) if argument.ellipsis.is_some() => {
                    self.expression(argument.value, None);
                    let fills_variadic = parameter.is_some_and(|parameter| parameter.flags.is_variadic());
                    if !self.is(argument.value, &[Kind::List]) || !fills_variadic {
                        let shown = self.shown(argument.value);
                        self.refuse(
                            argument.span(),
                            format!("a spread of `{shown}` here has no PHP# form: only a `List` spreads into a call, and only into a `...` parameter (spec section 7)"),
                        );
                    }
                    continue;
                }
                Argument::Positional(argument) => argument.value,
                Argument::Named(argument) => argument.value,
            };
            let Some(declared) = declared else {
                self.expression(value, None);
                continue;
            };
            // The parameter's type decides a literal's form, as `[:]` for a `Map` and `this.name` for a function, and
            // a lambda, a built-in's value among them, is held to the parameters its function type passes, whatever
            // its type parameters.
            let is_lambda = matches!(
                value,
                Expression::Closure(_) | Expression::ArrowFunction(_) | Expression::PartialApplication(_)
            );
            let expected = (is_lambda || !declared.has_template_types()).then_some(declared);
            self.expression(value, expected);
            self.narrowed(value, declared);
            let wider = self.type_of(value).filter(|passed| self.wider(passed, declared));
            // PHP checks the argument where it lands, and throws a `TypeError` for a value its parameter's type doesn't
            // hold, as PHP# does after `as`.
            if let (Some(passed), Some((number, parameter))) = (wider, filled) {
                let subject = format!("Argument #{} ({})", number + 1, parameter.get_name().0.as_str_lossy());
                let natively =
                    natively_checked(parameter.get_type_declaration_metadata(), declared, self.input.codebase);
                if self.checked(value, passed, declared, &subject, natively) {
                    continue;
                }
            }
            if self.mixed(value, "a parameter of a known type") || declared.has_template_types() {
                continue;
            }
            // PHP passes a value its parameter's type may not hold, and PHP# refuses it (spec section 7).
            if let Some(passed) = wider {
                let (passed, declared) = (passed.get_id(), declared.get_id());
                self.refuse(
                    value.span(),
                    format!("`{passed}` is wider than `{declared}`, the type of the parameter it reaches, which PHP passes and PHP# refuses (spec section 7)"),
                );
            }
        }
    }

    /// `call`, the call `expression` makes, whose result the code reads where `used` says so.
    pub(super) fn call(&mut self, expression: &Expression<'_>, call: &Call<'_>, used: bool) {
        match call {
            Call::Method(call) => {
                self.member_of(call.object, "a method call");
                self.edits.replace(call.arrow, ".");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(call.object, &call.method, method);
                self.arguments(&call.argument_list, method);
                self.plain_call(call.span(), method, object_class(self.type_of(call.object)));
            }
            Call::NullSafeMethod(call) => {
                self.member_of(call.object, "a method call");
                self.edits.replace(call.question_mark_arrow, "?.");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(call.object, &call.method, method);
                self.arguments(&call.argument_list, method);
                self.plain_call(call.span(), method, object_class(self.type_of(call.object)));
            }
            Call::StaticMethod(call) => {
                if self.forwards_called_class(call.span(), call.class, &call.method) {
                    return;
                }
                self.member(&call.method);
                let method = self.static_method_of(call);
                if !self.static_class(call.class) {
                    return self.reflected_call(expression, call, method, used);
                }
                self.edits.replace(call.double_colon, ".");
                self.arguments(&call.argument_list, method);
                self.plain_call(call.span(), method, self.static_call_class(call));
            }
            Call::Function(call) => self.function_call(call),
            Call::NullSafeFunction(_) => self.refuse(call.span(), "`?.(…)` on a function has no PHP# form"),
        }
    }

    /// Refuses the call at `call` of `callee`, a function or a method it reaches through `class`, in an enum, where it
    /// has an effect, since an enum has none (spec section 29), and the call of a plain PHP one where its result holds a
    /// PHP# generic class with a type argument other than its bound, which [`raw_generic`] finds, or an object of a
    /// class a module binds, which PHP# takes only through a constructor or a parameter, as a container's
    /// `app(StoreService::class)` gives one (spec section 32).
    fn plain_call(&mut self, call: Span, callee: Option<&FunctionLikeMetadata>, class: Option<Word>) {
        let Some(callee) = callee else {
            return;
        };
        let in_enum = self.class.as_ref().is_some_and(|class| class.metadata.kind.is_enum());
        if in_enum && let Some(effect) = self.declared_effect(callee, class) {
            // The call as PHP writes it up to its arguments, as `Translator::get`.
            let written = String::from_utf8_lossy(self.text(call)).into_owned();
            let shown = written.split('(').next().unwrap_or(&written).trim().to_owned();
            self.refuse(
                call,
                format!("`{shown}` has the effect `{effect}`, and a PHP# enum has no effects (spec section 29)"),
            );
        }
        if callee.flags.is_sharp() {
            return;
        }
        let Some(result) = self.input.artifacts.get_expression_type(&call) else {
            return;
        };
        if let Some((shown, raw)) = raw_generic(result, self.input.codebase) {
            self.refuse(
                call,
                format!("a plain PHP call that returns `{shown}` has no PHP# form: plain PHP creates a PHP# generic object without type arguments, so it is a `{raw}`, and PHP# checks it where it enters and throws (spec section 11)"),
            );
        }
        if let Some((service, namespace)) = self.bound_service(result) {
            self.refuse(
                call,
                format!("`{service}` is bound in `{namespace}`'s module, so PHP# takes it through a constructor or a parameter"),
            );
        }
    }

    /// The effect a call of `callee`, a function or a method it reaches through `class`, has as the checker reads it
    /// (spec section 29): the effect of a `foreign` class `class` is or extends, or the first effect the `extern`
    /// declaration the call falls under lists, as [`CodebaseMetadata::extern_of`] finds it. A call no `extern` covers
    /// has no effect the checker names, and neither does PHP's own function, the converted file's own code or another
    /// PHP# body, whose effects the proof's analysis reads.
    ///
    /// [`CodebaseMetadata::extern_of`]: mago_codex::metadata::CodebaseMetadata::extern_of
    fn declared_effect(&self, callee: &FunctionLikeMetadata, class: Option<Word>) -> Option<Word> {
        let codebase = self.input.codebase;
        let class = class.and_then(|class| codebase.get_class_like(class.as_bytes()));
        if let Some(class) = class {
            let parents =
                class.all_parent_classes.iter().filter_map(|parent| codebase.get_class_like(parent.as_bytes()));
            if let Some(foreign) = std::iter::once(class).chain(parents).find(|class| class.flags.is_foreign()) {
                return Some(foreign.original_name);
            }
        }
        if callee.flags.is_sharp() || callee.flags.is_built_in() || callee.span.file_id == self.input.file.id {
            return None;
        }
        let declaration = codebase.extern_of(
            class.map_or_else(empty_word, |class| class.name),
            ascii_lowercase_word(callee.name.as_bytes()),
        )?;

        declaration.effects.first().copied()
    }

    /// The short name of the first class `result` holds an object of that a module binds as a service, and the
    /// namespace of that module, as its classes write it. A test module binds for its tests alone.
    fn bound_service(&self, result: &TUnion) -> Option<(String, String)> {
        let codebase = self.input.codebase;
        result.types.iter().find_map(|atomic| {
            let TAtomic::Object(TObject::Named(object)) = atomic else {
                return None;
            };
            let class = codebase.get_class_like(object.name.as_bytes())?;
            let key = codebase.class_key(class.name.as_bytes());
            let module = codebase.modules.iter().filter(|module| module.kind != ModuleKind::Test).find(|module| {
                module.bindings.iter().any(|binding| {
                    binding.service.type_union.types.iter().any(|service| {
                        matches!(service, TAtomic::Object(TObject::Named(service))
                            if codebase.class_key(service.name.as_bytes()) == key)
                    })
                })
            })?;
            let declared = class.original_name.as_str_lossy();
            let short = declared.rsplit('\\').next().unwrap_or(&declared).to_owned();
            // The module's namespace is lowercase, and a class of it writes the namespace's own case.
            let namespace = codebase
                .class_likes
                .values()
                .map(|other| other.original_name.as_str_lossy())
                .find_map(|other| {
                    let (namespace, _) = other.rsplit_once('\\')?;
                    namespace.eq_ignore_ascii_case(&module.namespace.as_str_lossy()).then(|| namespace.to_owned())
                })
                .unwrap_or_else(|| module.namespace.as_str_lossy().into_owned());

            Some((short, namespace))
        })
    }

    /// The method a static call runs: of the class it names, of the class being converted for `self` and `static`, of
    /// its parent for `parent`, and of the class a class value or an object holds, as the analysis types it.
    pub(super) fn static_method_of(&self, call: &StaticMethodCall<'_>) -> Option<&'input FunctionLikeMetadata> {
        let ClassLikeMemberSelector::Identifier(name) = &call.method else {
            return None;
        };

        self.input.codebase.get_declaring_method(self.static_call_class(call)?.as_bytes(), name.value)
    }

    /// The class the static call `call` finds its method on: the one it names, the converted class for `self` and
    /// `static`, its parent for `parent`, or the class a class value or an object holds.
    fn static_call_class(&self, call: &StaticMethodCall<'_>) -> Option<Word> {
        match call.class {
            Expression::Identifier(identifier) => Some(word(self.input.names.get(identifier))),
            Expression::Self_(_) | Expression::Static(_) => Some(self.class.as_ref()?.metadata.name),
            Expression::Parent(_) => self.class.as_ref()?.metadata.direct_parent_class,
            class => self.held_class(class),
        }
    }

    /// The class `class` holds at run time, as the analysis types it: the class of `class-string<Order>`, the bound of
    /// `class-string<T>`, or the class of an `Order` object.
    fn held_class(&self, class: &Expression<'_>) -> Option<Word> {
        let [atomic] = self.type_of(class)?.types.as_ref() else {
            return None;
        };
        let held = match atomic {
            TAtomic::Scalar(TScalar::ClassLikeString(held)) => Cow::Owned(held.get_object_type(self.input.codebase)),
            atomic => Cow::Borrowed(atomic),
        };
        let class = match held.as_ref() {
            TAtomic::GenericParameter(parameter) => match parameter.constraint.types.as_ref() {
                [bound] => bound,
                _ => return None,
            },
            held => held,
        };

        match class {
            TAtomic::Object(TObject::Named(named)) => Some(named.name),
            TAtomic::Object(TObject::Enum(enumeration)) => Some(enumeration.name),
            _ => None,
        }
    }

    /// `$class::find(7)` of a class value held at run time as the call through reflection PHP# makes it with,
    /// `class.methods().first(method => method.name == "find").invoke(null, [7])`, which runs the `find` of the class the
    /// value holds as PHP's call does, the class's own before one it inherits (spec section 25). `invoke` gives `Any?`,
    /// which [`Converter::checked`] takes into the type the analysis gives the call where the code reads it, as `used`
    /// says. It keeps the file plain PHP for a value that may hold something other than a class value, as an object or
    /// null, where the analysis knows no `method` the call runs, which would type its arguments and its result, and for
    /// a named argument, since `invoke` takes a `List` of them.
    fn reflected_call(
        &mut self,
        expression: &Expression<'_>,
        call: &StaticMethodCall<'_>,
        method: Option<&FunctionLikeMetadata>,
        used: bool,
    ) {
        let ClassLikeMemberSelector::Identifier(name) = &call.method else {
            return;
        };
        let shown = format!(
            "{}::{}(…)",
            String::from_utf8_lossy(self.text(call.class.span())),
            String::from_utf8_lossy(name.value)
        );
        let held = self.shown(call.class);
        if !self.is(call.class, &[Kind::Class]) {
            return self.refuse(
                call.span(),
                format!("`{shown}` calls a static method through `{held}`, which holds no class value, and has no PHP# form: {HELD_CLASS}, which reads a class value"),
            );
        }
        let Some(method) = method else {
            return self.refuse(
                call.span(),
                format!("`{shown}` is a static call through a class held at run time, which PHP# makes through reflection, and the analysis finds no method `{}` of `{held}` to type it by (spec section 25)", String::from_utf8_lossy(name.value)),
            );
        };
        if let Some(named) = call.argument_list.arguments.iter().find(|argument| matches!(argument, Argument::Named(_)))
        {
            return self.refuse(
                named.span(),
                format!("a named argument of `{shown}` has no PHP# form: it is a static call through a class held at run time, which PHP# makes through reflection, whose `invoke` takes its arguments as a `List` (spec section 25)"),
            );
        }
        self.expression(call.class, None);
        self.arguments(&call.argument_list, Some(method));
        self.plain_call(call.span(), Some(method), self.held_class(call.class));
        let class = self.render(call.class);
        let class = if is_primary(call.class) { class } else { format!("({class})") };
        let arguments: Vec<String> = call
            .argument_list
            .arguments
            .iter()
            .map(|argument| {
                let span = argument.span();
                self.edits.render(self.source, span.start.offset, span.end.offset)
            })
            .collect();
        let found = self.unused_name("method", &[]);
        let declared = method.original_name.as_str_lossy();
        self.edits.replace(
            call.span(),
            format!(
                "{class}.methods().first({found} => {found}.name == \"{declared}\").invoke(null, [{}])",
                arguments.join(", ")
            ),
        );

        let Some(result) = self.type_of(expression).filter(|_| used) else {
            return;
        };
        if result.has_mixed() || result.is_void() || result.is_null() || result.is_never() {
            return;
        }
        let natively = natively_checked(method.return_type_declaration_metadata.as_ref(), result, self.input.codebase);
        if !self.checked(expression, &get_mixed(), result, "Return value", natively) {
            self.refuse(
                call.span(),
                format!("`{shown}` is a static call through a class held at run time, which PHP# makes through reflection, whose `invoke` gives `Any?`, and no PHP# test gives it the `{}` the call returns (spec section 21)", result.get_id()),
            );
        }
    }

    /// Refuses `self::m()`, or its method value `self::m(...)`, at `span` in a class a subclass can extend, where `m` may
    /// read the class PHP called, and gives whether it did. PHP's call through `self` forwards the class PHP called,
    /// which late static binding in `m` reads, and a call through the class's name doesn't. So it converts in a `final`
    /// class or an enum, and for a static method this file declares whose body, and each such method it calls through
    /// `self` in turn, names neither `static` nor `parent`.
    pub(super) fn forwards_called_class(
        &mut self,
        span: Span,
        class: &Expression<'_>,
        method: &ClassLikeMemberSelector<'_>,
    ) -> bool {
        let Some(current) = self.class.as_ref() else {
            return false;
        };
        if !matches!(class, Expression::Self_(_))
            || current.metadata.flags.is_final()
            || current.metadata.kind.is_enum()
        {
            return false;
        }
        if let ClassLikeMemberSelector::Identifier(name) = method
            && !self.reads_called_class(name.value, &mut Vec::new())
        {
            return false;
        }
        let method = String::from_utf8_lossy(self.text(method.span())).into_owned();
        let named = current.name.clone();
        self.refuse(
            span,
            format!("`self::{method}` forwards the class PHP called, which late static binding in `{method}` reads, and `{named}.{method}` doesn't, so on a subclass `{method}` would read `{named}` where PHP reads the subclass"),
        );

        true
    }

    /// Whether the method `name`, called through `self`, may read the class PHP called: it is no static method this file
    /// declares, its return type or body names `static` or `parent`, or it calls a method through `self` that may.
    /// `seen` holds the methods already read, which one calling itself reads again.
    fn reads_called_class(&self, name: &[u8], seen: &mut Vec<Vec<u8>>) -> bool {
        if seen.iter().any(|method| method.eq_ignore_ascii_case(name)) {
            return false;
        }
        seen.push(name.to_vec());
        let declared = self.input.program.statements.iter().flat_map(|statement| match statement {
            Statement::Namespace(namespace) => namespace.statements().iter().collect::<Vec<_>>(),
            statement => vec![statement],
        });
        let method = declared
            .filter_map(|statement| match statement {
                Statement::Class(class) => Some(class.members.iter()),
                _ => None,
            })
            .flatten()
            .find_map(|member| match member {
                ClassLikeMember::Method(method) if method.name.value.eq_ignore_ascii_case(name) => Some(method),
                _ => None,
            });
        let Some(method) = method else {
            return true;
        };
        let is_static = method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Static(_)));
        // A return type of `static`, alone or in a union, checks the value against the class PHP called.
        let returns_static = method.return_type_hint.as_ref().is_some_and(|hint| {
            self.text(hint.hint.span())
                .split(|byte| !byte.is_ascii_alphanumeric())
                .any(|word| word.eq_ignore_ascii_case(b"static"))
        });
        let MethodBody::Concrete(body) = &method.body else {
            return true;
        };
        if !is_static || returns_static {
            return true;
        }
        let mut reads = CalledClass::default();
        reads.walk_block(body, &mut ());

        reads.names_static || reads.through_self.iter().any(|called| self.reads_called_class(called, seen))
    }

    /// The function a bare call runs: the one its resolved name names, or PHP's own of the name it writes, as PHP falls
    /// back to a function of the root namespace.
    pub(super) fn function_of(&self, identifier: &Identifier<'_>) -> Option<&'input FunctionLikeMetadata> {
        let codebase = self.input.codebase;

        codebase.get_function(self.input.names.get(identifier)).or_else(|| {
            (!identifier.value().contains(&b'\\')).then(|| codebase.get_function(identifier.value())).flatten()
        })
    }

    /// A call of a function value, `f(x)`, as PHP writes it (spec section 14.1). PHP also calls a string or an array
    /// that names a function, which PHP# doesn't.
    fn value_call(&mut self, call: &FunctionCall<'_>) {
        let is_function = self.type_of(call.function).is_some_and(|union| {
            union.types.iter().all(|atomic| match atomic {
                TAtomic::Callable(_) => true,
                TAtomic::Object(TObject::Named(object)) => object.name.as_bytes().eq_ignore_ascii_case(b"closure"),
                _ => false,
            })
        });
        if !is_function {
            let shown = self.shown(call.function);
            return self.refuse(
                call.span(),
                format!("a call of `{shown}` has no PHP# form: PHP# calls a function value only (spec section 14.1)"),
            );
        }
        self.expression(call.function, None);
        self.arguments(&call.argument_list, None);
    }

    /// A bare PHP function call: a library's function through its namespace, a type test as `is`, or a refusal that
    /// names the function.
    fn function_call(&mut self, call: &FunctionCall<'_>) {
        let span = call.span();
        let Expression::Identifier(identifier) = call.function else {
            return self.value_call(call);
        };
        let Some(function) = self.function_of(identifier) else {
            return self.refuse(span, "the analysis holds no function this call runs");
        };
        let name = function.original_name.as_str_lossy().into_owned();

        if !function.flags.is_built_in() {
            let Some(callee) = self.through_namespace(span, &name, function.span) else {
                return;
            };
            self.arguments(&call.argument_list, Some(function));
            self.plain_call(span, Some(function), None);
            let list = call.argument_list.span();
            let arguments = self.edits.render(self.source, list.start.offset, list.end.offset);
            return self.edits.replace(span, format!("{callee}{arguments}"));
        }

        if name.eq_ignore_ascii_case("compact") || name.eq_ignore_ascii_case("extract") {
            return self.refuse(span, format!("`{name}` is removed (spec section 2)"));
        }
        let Some(target) = type_test(function) else {
            return self.built_in(call, function, &name);
        };
        let [Argument::Positional(value)] = call.argument_list.arguments.as_slice() else {
            return self.refuse(span, format!("`{name}` with a named or spread argument has no PHP# form"));
        };
        if value.ellipsis.is_some() {
            return self.refuse(span, format!("`{name}` with a named or spread argument has no PHP# form"));
        }
        // A PHP array is a `List` or a `Map` to PHP#, by its keys, so no one PHP# type tests what `is_array` does.
        if target.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(_))) {
            return self.refuse(
                span,
                format!("`{name}` has no PHP# form: a PHP array is a `List` or a `Map` by its keys, and a PHP# type test names one"),
            );
        }
        let tested = if target.is_null() {
            None
        } else {
            match type_text(target, self.input.codebase, &mut self.imports) {
                Ok(text) => Some(text),
                Err(reason) => return self.refuse(span, format!("`{name}` tests a type PHP# can't write: {reason}")),
            }
        };
        self.expression(value.value, None);
        let value = self.render(value.value);
        let test = match tested {
            Some(tested) => format!("{value} is {tested}"),
            None => format!("{value} == null"),
        };
        self.edits.replace(span, test);
    }

    /// The value and the type `expression` tests, each converted, when it is `$x instanceof A`, or a call of the
    /// prelude's type test of a type PHP# writes, as `is_string($x)`.
    pub(super) fn test(&mut self, expression: &Expression<'_>) -> Option<(String, String)> {
        match expression {
            Expression::Parenthesized(parenthesized) => self.test(parenthesized.expression),
            Expression::Binary(Binary { lhs, operator: BinaryOperator::Instanceof(_), rhs }) => {
                let Expression::Identifier(class) = rhs else {
                    return None;
                };
                let tested = self.class_name(class)?;
                self.expression(lhs, None);

                Some((self.render(lhs), tested))
            }
            Expression::Call(Call::Function(call)) => {
                let Expression::Identifier(identifier) = call.function else {
                    return None;
                };
                let target = type_test(self.function_of(identifier)?)?;
                if target.is_null() {
                    return None;
                }
                let [Argument::Positional(value)] = call.argument_list.arguments.as_slice() else {
                    return None;
                };
                if value.ellipsis.is_some() {
                    return None;
                }
                let tested = type_text(target, self.input.codebase, &mut self.imports).ok()?;
                self.expression(value.value, None);

                Some((self.render(value.value), tested))
            }
            _ => None,
        }
    }

    /// `new`, which names a generic class's type arguments (spec section 11). Generics are reified, so an argument the
    /// analysis inferred would be a runtime `TypeError` when it is wrong: the arguments are those of the declared type
    /// the value lands in, or the class's bounds, which plain PHP's `new` has.
    pub(super) fn instantiation(
        &mut self,
        expression: &Expression<'_>,
        instantiation: &Instantiation<'_>,
        expected: Option<&TUnion>,
    ) {
        // PHP# always writes a call's parentheses.
        let parentheses = if instantiation.argument_list.is_none() { "()" } else { "" };
        match instantiation.class {
            Expression::Identifier(identifier) => {
                let Some(name) = self.class_name(identifier) else {
                    return;
                };
                let mut written = Vec::new();
                if let Some(class) = self.input.codebase.get_class_like(self.input.names.get(identifier))
                    && !class.template_types.is_empty()
                {
                    let declared = expected.and_then(|union| {
                        let mut objects = union.types.iter().filter(|atomic| !atomic.is_null());
                        match (objects.next(), objects.next()) {
                            (Some(TAtomic::Object(object)), None)
                                if object.get_name().is_some_and(|name| {
                                    name.as_bytes().eq_ignore_ascii_case(class.name.as_bytes())
                                }) =>
                            {
                                object.get_type_parameters().map(<[TUnion]>::to_vec)
                            }
                            _ => None,
                        }
                    });
                    let arguments = declared.unwrap_or_else(|| {
                        class.template_types.values().map(|template| template.constraint.clone()).collect()
                    });
                    for argument in &arguments {
                        match type_text(argument, self.input.codebase, &mut self.imports) {
                            Ok(text) => written.push(text),
                            Err(reason) => return self.refuse(expression.span(), reason),
                        }
                    }
                }
                let arguments = if written.is_empty() { String::new() } else { format!("<{}>", written.join(", ")) };
                self.edits.replace(identifier.span(), format!("{name}{arguments}{parentheses}"));
            }
            Expression::Self_(keyword) => {
                let name = self.class.as_ref().map(|class| class.name.clone()).unwrap_or_default();
                self.edits.replace(keyword.span, format!("{name}{parentheses}"));
            }
            // `new static(…)` is `new Self(…)`, which a `final` class calls as its own `new`, and any other class
            // through its `required` constructor (spec section 25).
            Expression::Static(keyword) => {
                if !self.class.as_ref().is_some_and(|class| class.is_final || class.required) {
                    return self.refuse(
                        expression.span(),
                        "`new static(…)` in a class that declares no constructor of its own has no PHP# form: its `new Self(…)` calls the class's own `required` constructor (spec section 25), and declaring one changes the constructor PHP's reflection gives",
                    );
                }
                self.edits.replace(keyword.span, format!("Self{parentheses}"));
            }
            // `new $class(…)` of a class value is `new (type)(…)`, whose parentheses mark the class value (spec
            // section 25).
            class => {
                let is_class_value = self.type_of(class).is_some_and(|union| {
                    union.types.iter().all(|atomic| matches!(atomic, TAtomic::Scalar(TScalar::ClassLikeString(_))))
                });
                if !is_class_value {
                    let shown = self.shown(class);
                    return self.refuse(
                        expression.span(),
                        format!("`new` of `{shown}` has no PHP# form: PHP# makes an object of a class value only (spec section 25)"),
                    );
                }
                self.expression(class, None);
                let written = self.render(class);
                self.edits.replace(class.span(), format!("({written}){parentheses}"));
            }
        }
        // A constructor's parameters take the arguments as a method's do, `[:]` for a `Map` and a known type alike.
        let class = match instantiation.class {
            Expression::Identifier(identifier) => Some(self.input.names.get(identifier)),
            Expression::Self_(_) => self.class.as_ref().map(|class| class.metadata.name.as_bytes()),
            _ => None,
        };
        let constructor = class.and_then(|class| self.input.codebase.get_declaring_method(class, b"__construct"));
        if let Some(arguments) = &instantiation.argument_list {
            self.arguments(arguments, constructor);
        }
    }

    pub(super) fn construct(&mut self, expression: &Expression<'_>) {
        let Expression::Construct(construct) = expression else {
            return;
        };
        let reason = match construct {
            mago_syntax::cst::Construct::Isset(isset) => return self.isset(expression, isset, false),
            mago_syntax::cst::Construct::Empty(empty) => return self.empty(expression, empty, false),
            mago_syntax::cst::Construct::Print(_) => {
                "`print` is removed: output goes through `Console`, which a class receives through its constructor (spec section 8)"
            }
            mago_syntax::cst::Construct::Exit(_) | mago_syntax::cst::Construct::Die(_) => {
                "`exit` and `die` are removed: `Process` exits, which a class receives through its constructor (spec section 8)"
            }
            _ => "`include`, `require` and `eval` have no PHP# form: a .sharp file declares one type (spec section 27)",
        };
        self.refuse(expression.span(), reason);
    }
}
