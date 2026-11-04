//! Functions as values: PHP's `fn` and `function` as PHP#'s bare arrow, `(int n) => …`, which captures each variable
//! it uses with no `use` list, and a method named as a value, `this.name` (spec sections 14.2 and 14.3).

use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Block;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::Literal;
use mago_syntax::cst::PartialApplication;
use mago_syntax::cst::Sequence;

use super::Locals;
use super::locals::Capture;
use super::locals::Captures;
use crate::Converter;
use crate::Method;

/// A lambda's body: a block, or the expression an arrow function gives.
enum Body<'ast, 'arena> {
    Block(&'ast Block<'arena>),
    Expression(&'ast Expression<'arena>),
}

impl Converter<'_, '_> {
    /// A closure or an arrow function as a lambda. A closure captures the variables its `use` lists, and an arrow
    /// function each it reads, so their capture keeps PHP's meaning only where PHP# shares the variable as PHP does:
    /// by reference, or by value of a variable nothing writes after its declaration.
    pub(super) fn lambda(&mut self, expression: &Expression<'_>) {
        let (attributes, keywords, ampersand, parameters, body_start, body) = match expression {
            Expression::Closure(closure) => (
                &closure.attribute_lists,
                [closure.r#static.as_ref(), closure.function.as_ref()],
                closure.ampersand,
                &closure.parameter_list,
                closure.body.left_brace,
                Body::Block(&closure.body),
            ),
            Expression::ArrowFunction(arrow) => (
                &arrow.attribute_lists,
                [arrow.r#static.as_ref(), arrow.r#fn.as_ref()],
                arrow.ampersand,
                &arrow.parameter_list,
                arrow.expression.span(),
                Body::Expression(arrow.expression),
            ),
            _ => return,
        };
        if let Some(ampersand) = ampersand {
            return self
                .refuse(ampersand, "a lambda that returns by reference has no PHP# form: PHP# has no references");
        }

        let mut captures = Captures { every_name: matches!(body, Body::Expression(_)), names: Vec::new() };
        if let Expression::Closure(closure) = expression
            && let Some(clause) = &closure.use_clause
        {
            for used in clause.variables.iter() {
                captures.names.push(Capture {
                    name: used.variable.name.to_vec(),
                    at: used.variable.span,
                    by_reference: used.ampersand.is_some(),
                    written: false,
                    copied: false,
                    union: self.input.artifacts.get_expression_type(&used.variable).cloned(),
                });
            }
        }
        self.lambda_head(attributes, keywords, parameters, body_start, expression.span());

        let declared = self.input.codebase.get_closure_at(self.input.file, expression.span());
        let returning = declared
            .filter(|declared| declared.return_from_body.is_none())
            .and_then(|declared| declared.return_type_metadata.as_ref())
            .map(|metadata| metadata.type_union.clone());
        let resolution = self.method.as_ref().map(|method| method.resolution.clone()).unwrap_or_default();
        let around = self.method.take();
        self.method = Some(Method {
            returning: returning.clone(),
            resolution,
            parameters: parameters.parameters.iter().map(|parameter| parameter.variable.name.to_vec()).collect(),
            locals: Locals::default(),
            super_call: None,
            storage: None,
            captures: Some(captures),
        });
        match body {
            Body::Block(block) => self.block(block),
            Body::Expression(value) => self.expression(value, returning.as_ref()),
        }
        self.declare_locals();
        let lambda = std::mem::replace(&mut self.method, around);

        for capture in lambda.and_then(|lambda| lambda.captures).map(|captures| captures.names).unwrap_or_default() {
            if capture.written && !capture.by_reference {
                let shown = String::from_utf8_lossy(&capture.name);
                self.refuse(
                    capture.at,
                    format!("`{shown}` is captured by value and written in the lambda: PHP keeps the write to the lambda, and a PHP# lambda writes the variable itself (spec section 14.2)"),
                );
                continue;
            }
            self.captured(capture);
        }
    }

    /// The lambda's attributes and parameters, without `static`, `fn` or `function`, `use` and the return type, which
    /// a PHP# lambda doesn't write: `(int n) => ` up to `body`, where its body starts.
    fn lambda_head(
        &mut self,
        attributes: &Sequence<'_, AttributeList<'_>>,
        keywords: [Option<&Keyword<'_>>; 2],
        parameters: &FunctionLikeParameterList<'_>,
        body: Span,
        span: Span,
    ) {
        self.attributes(attributes);
        let start = keywords.iter().flatten().map(|keyword| keyword.span.start.offset).min();
        if let Some(start) = start {
            self.edits.delete(start, parameters.left_parenthesis.start.offset);
        }
        let declared = self.input.codebase.get_closure_at(self.input.file, span);
        for (index, parameter) in parameters.parameters.iter().enumerate() {
            let name = String::from_utf8_lossy(parameter.variable.name).into_owned();
            if parameter.ampersand.is_some() {
                self.refuse(
                    parameter.span(),
                    format!("a PHP# parameter is never taken by reference, so `{name}` can't be: return the new value instead"),
                );
                continue;
            }
            self.attributes(&parameter.attribute_lists);
            // A parameter PHP declares no type for has none in the lambda, which takes the one its function type gives.
            let union = declared
                .and_then(|declared| declared.parameters.get(index))
                .and_then(|parameter| parameter.get_type_metadata())
                .map(|metadata| metadata.type_union.clone());
            match (&parameter.hint, union) {
                (Some(hint), Some(union)) => {
                    if let Some(written) = self.type_text(&union, parameter.variable.span) {
                        self.edits.replace(hint.span(), written);
                    }
                }
                (None, Some(union)) => {
                    if let Some(written) = self.type_text(&union, parameter.variable.span) {
                        let at = parameter.ellipsis.unwrap_or(parameter.variable.span).start.offset;
                        self.edits.insert(at, format!("{written} "));
                    }
                }
                (Some(hint), None) => self.refuse(hint.span(), "the analysis holds no type of this parameter"),
                (None, None) => {}
            }
            self.edits.replace(parameter.variable.span, Self::variable_name(parameter.variable.name));
            if let Some(default) = &parameter.default_value {
                self.expression(default.value, None);
            }
        }
        self.edits.replace_range(parameters.right_parenthesis.end.offset, body.start.offset, " => ");
    }

    /// `$object->name(...)`, `Class::name(...)` and `name(...)` as the method value PHP# names without parentheses,
    /// `object.name` (spec section 14.3).
    pub(super) fn method_value(&mut self, expression: &Expression<'_>, application: &PartialApplication<'_>) {
        if !application.is_first_class_callable() {
            return self.refuse(
                expression.span(),
                "a partial application with placeholders has no PHP# form: write a lambda that calls it",
            );
        }
        let arguments = application.get_argument_list().span();
        match application {
            PartialApplication::Method(value) => {
                if self.untyped(value.object, "a method value") {
                    return;
                }
                self.edits.replace(value.arrow, ".");
                self.member(&value.method);
                let method = self.method_of(self.type_of(value.object), &value.method);
                self.method_name(value.object, &value.method, method);
            }
            PartialApplication::StaticMethod(value) => {
                self.expression(value.class, None);
                self.edits.replace(value.double_colon, ".");
                self.member(&value.method);
            }
            // A function value, `$f(...)`, is the value itself.
            PartialApplication::Function(value) => {
                let Expression::Identifier(identifier) = value.function else {
                    self.expression(value.function, None);
                    return self.edits.delete(arguments.start.offset, arguments.end.offset);
                };
                let Some(function) = self.function_of(identifier) else {
                    return self.refuse(expression.span(), "the analysis holds no function this value names");
                };
                let name = function.original_name.as_str_lossy().into_owned();
                if function.flags.is_built_in() {
                    return self.refuse(
                        expression.span(),
                        format!("`{name}` is PHP's own function, which PHP# reaches through the standard library member that runs it, and the converter writes that member for a call only"),
                    );
                }
                let Some(text) = self.through_namespace(expression.span(), &name, function.span) else {
                    return;
                };
                self.edits.replace(value.function.span(), text);
            }
        }
        self.edits.delete(arguments.start.offset, arguments.end.offset);
    }

    /// `[$object, 'name']`, `[Class::class, 'name']` and `'Class::name'`, which PHP calls as a function where a
    /// function type, `expected`, is declared, as the method value `object.name` (spec section 14.3). Gives whether
    /// `expression` is one.
    pub(super) fn callable_literal(&mut self, expression: &Expression<'_>, expected: &TUnion) -> bool {
        let is_function = expected.types.iter().all(|atomic| match atomic {
            TAtomic::Callable(_) => true,
            TAtomic::Object(TObject::Named(object)) => object.name.as_bytes().eq_ignore_ascii_case(b"closure"),
            _ => atomic.is_null(),
        });
        if !is_function {
            return false;
        }
        let (holder, name) = match expression {
            Expression::Array(array) => {
                let mut elements = array.elements.iter();
                match (elements.next(), elements.next(), elements.next()) {
                    (Some(ArrayElement::Value(holder)), Some(ArrayElement::Value(name)), None) => {
                        let Expression::Literal(Literal::String(string)) = name.value else {
                            return false;
                        };
                        let Some(name) = string.value else {
                            return false;
                        };
                        (Holder::Expression(holder.value), name.to_vec())
                    }
                    _ => return false,
                }
            }
            Expression::Literal(Literal::String(string)) => {
                let Some((class, name)) = string.value.and_then(|value| {
                    let text = std::str::from_utf8(value).ok()?;
                    text.split_once("::")
                }) else {
                    return false;
                };
                (Holder::Class(class.trim_start_matches('\\').as_bytes().to_vec()), name.as_bytes().to_vec())
            }
            _ => return false,
        };

        // The class the method is found on, and the object it is a value of, when it is no class's static method.
        let (class, object) = match &holder {
            Holder::Class(class) => (Some(class.clone()), None),
            Holder::Expression(Expression::Literal(Literal::String(string))) => {
                (string.value.map(|value| value.strip_prefix(b"\\").unwrap_or(value).to_vec()), None)
            }
            Holder::Expression(Expression::Access(Access::ClassConstant(access)))
                if matches!(&access.constant, ClassLikeConstantSelector::Identifier(constant)
                    if constant.value.eq_ignore_ascii_case(b"class")) =>
            {
                let class = match access.class {
                    Expression::Identifier(identifier) => Some(self.input.names.get(identifier).to_vec()),
                    _ => self.class.as_ref().map(|class| class.metadata.name.as_bytes().to_vec()),
                };
                (class, None)
            }
            Holder::Expression(object) => match self.type_of(object).map(|union| union.types.as_ref()) {
                Some([TAtomic::Object(TObject::Named(named))]) => (Some(named.name.as_bytes().to_vec()), Some(*object)),
                _ => (None, Some(*object)),
            },
        };
        let method = class.as_ref().and_then(|class| self.input.codebase.get_declaring_method(class, &name));
        let (Some(class), Some(method)) = (class, method) else {
            self.refuse(
                expression.span(),
                "a callable array or string whose method the analysis can't find has no PHP# form: PHP# names a method value on its class or object (spec section 14.3)",
            );
            return true;
        };
        let written = match object {
            Some(object) => {
                self.expression(object, None);
                let rendered = self.render(object);
                if super::is_primary(object) { rendered } else { format!("({rendered})") }
            }
            None => {
                let declared = self.input.codebase.get_class_like(&class).map(|class| class.original_name);
                let Some(declared) = declared else {
                    return false;
                };
                match self.imports.name_of(&declared.as_str_lossy()) {
                    Ok(name) => name,
                    Err(reason) => {
                        self.refuse(expression.span(), reason);
                        return true;
                    }
                }
            }
        };
        self.edits.replace(expression.span(), format!("{written}.{}", method.original_name.as_str_lossy()));

        true
    }
}

/// What a callable array or string names its method on.
enum Holder<'ast, 'arena> {
    /// A class by its name.
    Class(Vec<u8>),
    /// The array's first element: an object, a class value or a class name.
    Expression(&'ast Expression<'arena>),
}
