//! Calls and `new`: method and static calls with their arguments, bare function calls, type tests as `is`, and `new`
//! with its type arguments (spec sections 7, 8, 11 and 21).

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
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

use super::Kind;
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
            let parameter = callee.and_then(|callee| match argument {
                Argument::Named(argument) => {
                    let name = [b"$".as_slice(), argument.name.value].concat();
                    callee.parameters.iter().find(|parameter| parameter.get_name().0.as_bytes() == name)
                }
                // Each argument past the `...` parameter lands in it.
                Argument::Positional(_) => callee
                    .parameters
                    .get(index)
                    .or_else(|| callee.parameters.last().filter(|parameter| parameter.flags.is_variadic())),
            });
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
            // a lambda is held to the parameters its function type passes, whatever its type parameters.
            let is_lambda = matches!(value, Expression::Closure(_) | Expression::ArrowFunction(_));
            let expected = (is_lambda || !declared.has_template_types()).then_some(declared);
            self.expression(value, expected);
            self.narrowed(value, declared);
            if self.mixed(value, "a parameter of a known type") || declared.has_template_types() {
                continue;
            }
            // PHP passes a value its parameter's type may not hold, and PHP# refuses it (spec section 7).
            if let Some(passed) = self.type_of(value)
                && !is_contained_by(
                    self.input.codebase,
                    passed,
                    declared,
                    false,
                    false,
                    false,
                    &mut ComparisonResult::new(),
                )
            {
                let (passed, declared) = (passed.get_id(), declared.get_id());
                self.refuse(
                    value.span(),
                    format!("`{passed}` is wider than `{declared}`, the type of the parameter it reaches, which PHP passes and PHP# refuses (spec section 7)"),
                );
            }
        }
    }

    pub(super) fn call(&mut self, call: &Call<'_>) {
        match call {
            Call::Method(call) => {
                self.member_of(call.object, "a method call");
                self.edits.replace(call.arrow, ".");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(call.object, &call.method, method);
                self.arguments(&call.argument_list, method);
                self.raw_result(call.span(), method);
            }
            Call::NullSafeMethod(call) => {
                self.member_of(call.object, "a method call");
                self.edits.replace(call.question_mark_arrow, "?.");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(call.object, &call.method, method);
                self.arguments(&call.argument_list, method);
                self.raw_result(call.span(), method);
            }
            Call::StaticMethod(call) => {
                if self.forwards_called_class(call.span(), call.class, &call.method) {
                    return;
                }
                self.expression(call.class, None);
                self.edits.replace(call.double_colon, ".");
                self.member(&call.method);
                let method = self.static_method_of(call);
                self.arguments(&call.argument_list, method);
                self.raw_result(call.span(), method);
            }
            Call::Function(call) => self.function_call(call),
            Call::NullSafeFunction(_) => self.refuse(call.span(), "`?.(…)` on a function has no PHP# form"),
        }
    }

    /// Refuses the call at `call` of `callee`, a plain PHP function or method, whose result holds a PHP# generic class
    /// with a type argument other than its bound, which [`raw_generic`] finds.
    fn raw_result(&mut self, call: Span, callee: Option<&FunctionLikeMetadata>) {
        if callee.is_none_or(|callee| callee.flags.is_sharp()) {
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
    }

    /// The method a static call runs, of the class it names, or of the class being converted for `self`, `static` and
    /// `parent`, which the analysis types as the class.
    pub(super) fn static_method_of(&self, call: &StaticMethodCall<'_>) -> Option<&'input FunctionLikeMetadata> {
        let ClassLikeMemberSelector::Identifier(name) = &call.method else {
            return None;
        };
        let class = match call.class {
            Expression::Identifier(identifier) => self.input.names.get(identifier),
            _ => self.class.as_ref()?.metadata.name.as_bytes(),
        };

        self.input.codebase.get_declaring_method(class, name.value)
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
            format!("`self::{method}` forwards the class PHP called, which late static binding in `{method}` reads, and `{named}.{method}` doesn't: it converts in a `final` class, and for a static method of this file that names neither `static` nor `parent`"),
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
            self.raw_result(span, Some(function));
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
