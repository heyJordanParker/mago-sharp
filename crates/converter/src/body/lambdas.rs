//! Lambdas: PHP's `fn` and `function` as PHP#'s bare arrow, `(int n) => …`, which captures each variable it uses with
//! no `use` list (spec section 14.2).

use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Block;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Keyword;
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
}
