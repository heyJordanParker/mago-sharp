//! Statements and expressions. This module walks them and gives each construct to the module of its family. Each
//! construct whose PHP# form is not a token's rewrite builds its text from its children's, rendered with their own
//! edits.

mod access;
mod builtins;
mod calls;
mod collections;
mod conditions;
mod locals;
mod loops;
mod matching;
mod operators;
mod strings;

pub(crate) use locals::Locals;

use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Block;
use mago_syntax::cst::Expression;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Try;
use mago_syntax::cst::Variable;

use crate::Converter;

/// What a value of an atomic type is to the operators PHP and PHP# share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Null,
    Int,
    Float,
    String,
    Bool,
    Enum,
    /// It may hold an object, as a class, `object` or `mixed` does.
    Object,
    List,
    Map,
    /// A `Set` or an `Iterable`.
    Collection,
    Other,
}

/// Whether `expression` binds tighter than any operator, so text built around it, as `x.isEmpty()` or `x != 0`, needs
/// no parentheses around it.
fn is_primary(expression: &Expression<'_>) -> bool {
    matches!(
        expression,
        Expression::Variable(_)
            | Expression::Parenthesized(_)
            | Expression::Literal(_)
            | Expression::Access(_)
            | Expression::Call(_)
            | Expression::ArrayAccess(_)
            | Expression::Identifier(_)
            | Expression::ConstantAccess(_)
    )
}

/// Each kind of value `union` holds, sorted, once each.
fn kinds(union: &TUnion) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = union
        .types
        .iter()
        .map(|atomic| {
            if atomic.is_null() {
                return Kind::Null;
            }
            if atomic.is_mixed() {
                return Kind::Object;
            }
            match get_sharp_type(atomic) {
                Some(SharpType::Int) => Kind::Int,
                Some(SharpType::Float) => Kind::Float,
                Some(SharpType::String) => Kind::String,
                Some(SharpType::Bool) => Kind::Bool,
                Some(SharpType::Enum(_)) => Kind::Enum,
                Some(SharpType::Class(..) | SharpType::Object | SharpType::Parameter(_)) => Kind::Object,
                Some(SharpType::List(_)) => Kind::List,
                Some(SharpType::Map(..)) => Kind::Map,
                Some(SharpType::Set(_) | SharpType::Iterable(_)) => Kind::Collection,
                _ => Kind::Other,
            }
        })
        .collect();
    kinds.sort_unstable();
    kinds.dedup();

    kinds
}

impl<'input> Converter<'input, '_> {
    pub(crate) fn block(&mut self, block: &Block<'_>) {
        if let Some(locals) = self.locals() {
            locals.blocks.push(block.span());
            locals.scopes.push(block.span());
        }
        for statement in &block.statements {
            self.statement(statement);
        }
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
    }

    pub(crate) fn statement(&mut self, statement: &Statement<'_>) {
        match statement {
            Statement::Expression(expression) => {
                // A `match` statement ends at its `}` (spec section 21).
                if let Expression::Match(r#match) = expression.expression {
                    let terminator = expression.terminator.span();
                    self.edits.delete(r#match.right_brace.end.offset, terminator.end.offset);
                    return self.match_expression(r#match, true);
                }
                if !self.declaration(statement, expression.expression) {
                    self.expression(expression.expression, None);
                }
            }
            Statement::Return(r#return) => {
                if let Some(value) = r#return.value {
                    let returning = self.method.as_ref().and_then(|method| method.returning.clone());
                    self.expression(value, returning.as_ref());
                    // PHP returns a value its declared type doesn't hold, and PHP# refuses it (spec section 7).
                    if let (Some(returning), Some(returned)) = (&returning, self.type_of(value))
                        && !is_contained_by(
                            self.input.codebase,
                            returned,
                            returning,
                            false,
                            false,
                            false,
                            &mut ComparisonResult::new(),
                        )
                    {
                        let (returned, returning) = (returned.get_id(), returning.get_id());
                        self.refuse(
                            value.span(),
                            format!("`{returned}` is wider than `{returning}`, the type the method declares it returns, which PHP returns and PHP# refuses (spec section 7)"),
                        );
                    }
                }
            }
            Statement::Block(block) => self.block(block),
            Statement::If(r#if) => {
                self.condition(r#if.condition, true);
                let IfBody::Statement(body) = &r#if.body else {
                    self.refuse(r#if.span(), "an `if` with `:` and `endif` has no PHP# form: write its body in braces");
                    return;
                };
                self.braced(body.statement);
                for clause in &body.else_if_clauses {
                    self.edits.replace(clause.elseif.span, "else if");
                    self.condition(clause.condition, true);
                    self.braced(clause.statement);
                }
                if let Some(clause) = &body.else_clause {
                    match clause.statement {
                        Statement::If(_) => self.statement(clause.statement),
                        statement => self.braced(statement),
                    }
                }
            }
            Statement::Try(r#try) => self.try_statement(r#try),
            Statement::Noop(_) => {}
            Statement::Foreach(foreach) => self.foreach(statement, foreach),
            Statement::For(r#for) => self.for_loop(statement, r#for),
            Statement::While(r#while) => self.while_loop(statement, r#while),
            Statement::DoWhile(r#do) => self.do_while(r#do),
            Statement::Break(r#break) => self.leave(r#break.level),
            Statement::Continue(r#continue) => self.leave(r#continue.level),
            Statement::Switch(_) => self.refuse(statement.span(), "a `switch` is not converted yet: it becomes a `match`"),
            Statement::Unset(unset) => self.unset(unset),
            Statement::Echo(_) | Statement::EchoTag(_) => self.refuse(
                statement.span(),
                "`echo` is removed: output goes through `Console`, which a class receives through its constructor (spec section 8)",
            ),
            Statement::Global(_) => self.refuse(statement.span(), "`global` is removed (spec section 2)"),
            Statement::Static(r#static) => {
                self.refuse(
                    statement.span(),
                    "a `static` local is not converted yet: its PHP# form is a private static field (spec section 6)",
                );
                // The refusal covers each later read of the local, which has no declaration to point at.
                if let Some(method) = self.method.as_mut() {
                    method.parameters.extend(r#static.items.iter().map(|item| item.variable().name.to_vec()));
                }
            }
            Statement::Goto(_) => self.refuse(statement.span(), "`goto` has no PHP# form"),
            Statement::Label(_) => self.refuse(statement.span(), "a `goto` label has no PHP# form"),
            _ => self.refuse(statement.span(), "this statement has no PHP# form in a method"),
        }
    }

    /// The body of an `if`, an `else` or a loop, which PHP# writes in braces (spec section 17). A body without them gets
    /// them, and scopes its locals as a block does.
    fn braced(&mut self, statement: &Statement<'_>) {
        if let Statement::Block(block) = statement {
            return self.block(block);
        }
        let span = statement.span();
        self.edits.insert(span.start.offset, "{ ");
        self.edits.insert(span.end.offset, " }");
        if let Some(locals) = self.locals() {
            locals.blocks.push(span);
        }
        self.statement(statement);
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
    }

    fn try_statement(&mut self, r#try: &Try<'_>) {
        self.block(&r#try.block);
        for clause in &r#try.catch_clauses {
            match &clause.hint {
                mago_syntax::cst::Hint::Identifier(identifier) => self.rename_class(identifier),
                hint => self.refuse(hint.span(), "a `catch` of several types is not converted yet"),
            }
            // A `catch` variable is bound by the clause, never declared by an assignment.
            if let Some(variable) = &clause.variable {
                self.edits.replace(variable.span, Self::variable_name(variable.name));
                if let Some(method) = self.method.as_mut() {
                    method.parameters.push(variable.name.to_vec());
                }
            }
            self.block(&clause.block);
        }
        if let Some(clause) = &r#try.finally_clause {
            self.block(&clause.block);
        }
    }

    fn type_of(&self, expression: &Expression<'_>) -> Option<&'input TUnion> {
        self.input.artifacts.get_expression_type(expression)
    }

    /// Whether the analysis gives `expression` a type whose every value is one of `allowed`.
    fn is(&self, expression: &Expression<'_>, allowed: &[Kind]) -> bool {
        self.type_of(expression).is_some_and(|union| {
            let kinds = kinds(union);
            !kinds.is_empty() && kinds.iter().all(|kind| allowed.contains(kind))
        })
    }

    /// The analysis's type of `expression`, as a refusal names it.
    fn shown(&self, expression: &Expression<'_>) -> String {
        self.type_of(expression).map_or_else(|| "no type".to_owned(), |union| union.get_id().to_string())
    }

    pub(crate) fn render(&mut self, expression: &Expression<'_>) -> String {
        let span = expression.span();
        self.edits.render(self.source, span.start.offset, span.end.offset)
    }

    /// `expression` in PHP#. `expected` is the declared type of the place its value lands, when there is one, which
    /// decides the PHP# form of a literal, as `[]` is an empty `Map`, `[:]`, where a `Map` is declared (spec section
    /// 12).
    pub(crate) fn expression(&mut self, expression: &Expression<'_>, expected: Option<&TUnion>) {
        match expression {
            Expression::Variable(Variable::Direct(variable)) => self.variable(variable, false),
            Expression::Variable(_) => {
                self.refuse(expression.span(), "a variable named by a variable is removed (spec section 2)");
            }
            Expression::Parenthesized(parenthesized) => self.expression(parenthesized.expression, expected),
            Expression::Literal(literal) => self.literal(literal, expected),
            Expression::Identifier(identifier) => self.rename_class(identifier),
            Expression::ConstantAccess(constant) => self.constant(expression, constant),
            Expression::Self_(keyword) => {
                let name = self.class.as_ref().map(|class| class.name.clone()).unwrap_or_default();
                self.edits.replace(keyword.span, name);
            }
            Expression::Static(keyword) => self.edits.replace(keyword.span, "Self"),
            Expression::Parent(keyword) => self.edits.replace(keyword.span, "super"),
            Expression::Access(access) => self.access(expression, access),
            Expression::Call(call) => self.call(call),
            Expression::Instantiation(instantiation) => self.instantiation(expression, instantiation, expected),
            Expression::Binary(binary) => self.binary(expression, binary),
            Expression::UnaryPrefix(unary) => self.unary(expression, &unary.operator, unary.operand),
            Expression::UnaryPostfix(unary) => {
                if !self.is(unary.operand, &[Kind::Int, Kind::Float]) {
                    let shown = self.shown(unary.operand);
                    return self.refuse(
                        expression.span(),
                        format!("`++` and `--` on `{shown}` step PHP's way, which PHP# does only for a number"),
                    );
                }
                self.written(unary.operand);
            }
            Expression::Assignment(assignment) => self.assignment(expression, assignment, expected),
            Expression::Array(array) => {
                if array.elements.is_empty() {
                    return self.empty_array(expression, expected);
                }
                self.elements(array.elements.iter());
            }
            Expression::LegacyArray(array) => {
                if array.elements.is_empty() {
                    return self.empty_array(expression, expected);
                }
                self.edits.replace_range(array.array.span.start.offset, array.left_parenthesis.end.offset, "[");
                self.edits.replace(array.right_parenthesis, "]");
                self.elements(array.elements.iter());
            }
            Expression::ArrayAccess(access) => {
                self.untyped(access.array, "an index");
                self.untyped(access.index, "a key");
                if self.is(access.array, &[Kind::Map]) {
                    self.refuse(
                        expression.span(),
                        "a `Map` read at a key it may lack is not converted yet: PHP gives `null` for a missing key, and PHP# reads one only behind `??` (spec section 12)",
                    );
                }
            }
            Expression::Conditional(conditional) => {
                let Some(then) = conditional.then else {
                    return self.refuse(expression.span(), "`?:` has no PHP# form: `??` covers null (spec section 14)");
                };
                self.condition(conditional.condition, false);
                self.expression(then, expected);
                self.expression(conditional.r#else, expected);
            }
            Expression::Throw(throw) => self.expression(throw.exception, None),
            Expression::Closure(_) | Expression::ArrowFunction(_) | Expression::PartialApplication(_) => {
                self.refuse(expression.span(), "a function as a value is not converted yet");
            }
            Expression::Match(r#match) => self.match_expression(r#match, false),
            Expression::Clone(_) => {
                self.refuse(expression.span(), "`clone` is not converted yet: it becomes `with { }`")
            }
            Expression::List(_) => self.refuse(expression.span(), "destructuring is not converted yet"),
            Expression::CompositeString(_) => {
                self.refuse(
                    expression.span(),
                    "an interpolated string or a heredoc is not converted yet: it becomes a template",
                );
            }
            Expression::MagicConstant(_) => {
                self.refuse(
                    expression.span(),
                    "a magic constant is not converted yet: `Position` replaces it (spec section 27)",
                );
            }
            Expression::AnonymousClass(_) => {
                self.refuse(expression.span(), "an anonymous class has no PHP# form: no type nests in another");
            }
            Expression::Construct(_) => self.construct(expression),
            Expression::Yield(_) => self.refuse(expression.span(), "a generator is not converted yet"),
            _ => self.refuse(expression.span(), "this expression has no PHP# form"),
        }
    }

    /// Refuses `expression` when the analysis gives it `mixed`, which PHP lets reach `operation` and PHP# lets reach it
    /// only from a known type, and gives whether it did. Its own parts convert either way.
    fn untyped(&mut self, expression: &Expression<'_>, operation: &str) -> bool {
        self.expression(expression, None);
        if !self.type_of(expression).is_some_and(|union| union.types.iter().any(TAtomic::is_mixed)) {
            return false;
        }
        let shown = self.shown(expression);
        self.refuse(
            expression.span(),
            format!("`{shown}` reaches {operation}, which PHP# allows only from a known type: narrow it with `is`, or declare its type (spec section 24)"),
        );

        true
    }
}
