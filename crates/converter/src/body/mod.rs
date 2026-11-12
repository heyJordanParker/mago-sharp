//! Statements and expressions. This module walks them and gives each construct to the module of its family. Each
//! construct whose PHP# form is not a token's rewrite builds its text from its children's, rendered with their own
//! edits.

mod access;
mod builtins;
mod calls;
mod collections;
mod conditions;
mod lambdas;
mod locals;
mod loops;
mod matching;
mod operators;
mod strings;

pub(crate) use locals::Captures;
pub(crate) use locals::Locals;

use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Block;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Hint;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Try;
use mago_syntax::cst::Variable;

use crate::Converter;
use matching::MatchPlace;

/// What a value of an atomic type is to the operators PHP and PHP# share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Null,
    Int,
    Float,
    String,
    Bool,
    Enum,
    /// It may hold an object, as a class, `object`, a closure or `mixed` does.
    Object,
    /// A class value, `class-string` to PHP, whose value is the class's name.
    Class,
    List,
    Map,
    /// A `Set` or an `Iterable`.
    Collection,
    /// A tuple, or a type PHP# has none of, as a `resource`, an `array{…}` shape or `never`.
    Other,
}

/// Why a read of a `Map` at a key it may lack refuses, by an index or by destructuring. A converted file never
/// succeeds where PHP warned or failed, and a PHP# `Map` read gives `null` and goes on where PHP warns (spec section
/// 12).
const MISSING_KEY: &str = "a `Map` read at a key it may lack has no PHP# form: PHP warns on a missing key, and a PHP# `Map` read gives `null` and goes on, which succeeds where PHP warned, so it converts behind `??` or `isset` only (spec section 12)";

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

/// Whether PHP turns the string key `value` into an `int`: a decimal integer with no `+`, no leading zero and no
/// space, which fits an `int`, as `'42'` and `'-7'` are and `'042'` and `'4.2'` are not.
fn is_integer_key(value: &[u8]) -> bool {
    let digits = value.strip_prefix(b"-").unwrap_or(value);
    let canonical = match digits {
        [b'0'] => value.len() == 1,
        [first, rest @ ..] => (b'1'..=b'9').contains(first) && rest.iter().all(u8::is_ascii_digit),
        [] => false,
    };

    canonical && std::str::from_utf8(value).is_ok_and(|text| text.parse::<i64>().is_ok())
}

/// Each kind of value `atomic` holds: two for PHP's `array-key`, which is `int|string`, the keys a PHP array holds.
fn atomic_kinds(atomic: &TAtomic) -> &'static [Kind] {
    if atomic.is_null() {
        return &[Kind::Null];
    }
    if atomic.is_mixed() {
        return &[Kind::Object];
    }
    if matches!(atomic, TAtomic::Scalar(TScalar::ArrayKey)) {
        return &[Kind::Int, Kind::String];
    }
    match get_sharp_type(atomic) {
        Some(SharpType::Int) => &[Kind::Int],
        Some(SharpType::Float) => &[Kind::Float],
        Some(SharpType::String) => &[Kind::String],
        Some(SharpType::Bool) => &[Kind::Bool],
        Some(SharpType::Enum(_)) => &[Kind::Enum],
        Some(SharpType::Class(..) | SharpType::Object | SharpType::Parameter(_) | SharpType::Function(_)) => {
            &[Kind::Object]
        }
        Some(SharpType::ClassValue(_)) => &[Kind::Class],
        Some(SharpType::List(_)) => &[Kind::List],
        Some(SharpType::Map(..)) => &[Kind::Map],
        Some(SharpType::Set(_) | SharpType::Iterable(_)) => &[Kind::Collection],
        Some(SharpType::Tuple(_) | SharpType::Void) | None => &[Kind::Other],
    }
}

/// Each kind of value `union` holds, sorted, once each.
fn kinds(union: &TUnion) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = union.types.iter().flat_map(|atomic| atomic_kinds(atomic).iter().copied()).collect();
    kinds.sort_unstable();
    kinds.dedup();

    kinds
}

impl<'input> Converter<'input, '_> {
    pub(crate) fn block(&mut self, block: &Block<'_>) {
        if let Some(locals) = self.locals() {
            locals.blocks.push(block.span());
            locals.body.get_or_insert(block.span());
        }
        for statement in &block.statements {
            self.statement(statement);
        }
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
    }

    pub(crate) fn statement(&mut self, statement: &Statement<'_>) {
        if self.method.as_ref().is_some_and(|method| method.super_call == Some(statement.span())) {
            return;
        }
        match statement {
            Statement::Expression(expression) => {
                // A `match` statement ends at its `}` (spec section 21).
                if let Expression::Match(r#match) = expression.expression {
                    let terminator = expression.terminator.span();
                    self.edits.delete(r#match.right_brace.end.offset, terminator.end.offset);
                    return self.match_expression(r#match, MatchPlace::Statement);
                }
                if !self.declaration(statement, expression.expression) {
                    self.expression(expression.expression, None);
                }
            }
            Statement::Return(r#return) => {
                let returned = r#return.value.and_then(|value| self.type_of(value)).cloned();
                if let Some(method) = self.method.as_mut() {
                    method.returned.push(returned);
                }
                if let Some(value) = r#return.value {
                    let returning = self.method.as_ref().and_then(|method| method.returning.clone());
                    self.value(value, returning.as_ref());
                    if let Some(returning) = &returning {
                        self.narrowed(value, returning);
                    }
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
            Statement::If(r#if) => self.if_statement(statement, r#if, false),
            Statement::Try(r#try) => self.try_statement(r#try),
            Statement::Noop(_) => {}
            Statement::Foreach(foreach) => self.foreach(statement, foreach),
            Statement::For(r#for) => self.for_loop(statement, r#for),
            Statement::While(r#while) => self.while_loop(statement, r#while),
            Statement::DoWhile(r#do) => self.do_while(r#do),
            Statement::Break(r#break) => self.leave(r#break.level),
            Statement::Continue(r#continue) => self.leave(r#continue.level),
            Statement::Switch(switch) => self.switch_statement(statement, switch),
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

    /// `if`, whose condition may assign the local it tests unless the `if` follows an `else`.
    fn if_statement(&mut self, statement: &Statement<'_>, r#if: &If<'_>, follows_else: bool) {
        if follows_else {
            self.chained_condition(r#if.condition);
        } else if !self.assigned_condition(statement, r#if) && !self.compared_assignment(statement, r#if) {
            self.condition(r#if.condition, true);
        }
        let IfBody::Statement(body) = &r#if.body else {
            self.refuse(r#if.span(), "an `if` with `:` and `endif` has no PHP# form: write its body in braces");
            return;
        };
        self.braced(body.statement);
        for clause in &body.else_if_clauses {
            self.edits.replace(clause.elseif.span, "else if");
            self.chained_condition(clause.condition);
            self.braced(clause.statement);
        }
        if let Some(clause) = &body.else_clause {
            match clause.statement {
                Statement::If(nested) => self.if_statement(clause.statement, nested, true),
                statement => self.braced(statement),
            }
        }
    }

    fn try_statement(&mut self, r#try: &Try<'_>) {
        self.block(&r#try.block);
        for clause in &r#try.catch_clauses {
            self.caught(&clause.hint);
            // A `catch` variable is bound by the clause, never declared by an assignment.
            if let Some(variable) = &clause.variable {
                self.misnamed_local(variable);
                let name = self.variable_name(variable.name);
                self.edits.replace(variable.span, name);
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

    /// The classes a `catch` takes, as a union of them, `catch (Timeout | Refused failure)`, takes several.
    fn caught(&mut self, hint: &Hint<'_>) {
        match hint {
            Hint::Identifier(identifier) => self.rename_class(identifier),
            Hint::Union(union) => {
                self.caught(union.left);
                self.caught(union.right);
            }
            hint => self.refuse(hint.span(), "a `catch` of this type has no PHP# form: a `catch` takes classes"),
        }
    }

    fn type_of(&self, expression: &Expression<'_>) -> Option<&'input TUnion> {
        self.input.artifacts.get_expression_type(expression)
    }

    /// Whether the analysis gives `expression` a type whose every value is one of `allowed`.
    fn is(&self, expression: &Expression<'_>, allowed: &[Kind]) -> bool {
        if let Expression::Variable(Variable::Direct(variable)) = expression
            && self.method.as_ref().and_then(|method| method.variadic.as_deref()) == Some(variable.name)
        {
            return allowed.contains(&Kind::List);
        }
        self.type_of(expression).is_some_and(|union| {
            let kinds = kinds(union);
            !kinds.is_empty() && kinds.iter().all(|kind| allowed.contains(kind))
        })
    }

    /// Whether the analysis gives `expression` a type whose every value is an object of a class that implements
    /// `ArrayAccess`, as a `WeakMap` or a framework's collection, which PHP reads and writes by index through it.
    fn is_array_access(&self, expression: &Expression<'_>) -> bool {
        let codebase = self.input.codebase;
        self.type_of(expression).is_some_and(|union| {
            union.types.iter().all(|atomic| {
                matches!(atomic, TAtomic::Object(TObject::Named(_)))
                    && atomic.extends_or_implements(codebase, b"ArrayAccess")
            })
        })
    }

    /// Refuses `key` of the `Map` `map`, and gives whether it did, where the `Map` is keyed by `int` and `string` both,
    /// as PHP's `array<array-key, V>` is, and `key` may be a string of an integer's digits. PHP turns such a key into
    /// the `int`, as `'42'` into 42, and a PHP# `Map<int|string, V>` keeps it a string, so the two read and write
    /// different entries. An `int` key, and a string literal PHP keeps a string, give the same entry.
    fn numeric_key(&mut self, map: &Expression<'_>, key: &Expression<'_>) -> bool {
        let keyed_by_both = self.keyed_by_both(map);
        let keeps = match key {
            Expression::Literal(Literal::String(string)) => string.value.is_some_and(|value| !is_integer_key(value)),
            key => self.is(key, &[Kind::Int]),
        };
        if !keyed_by_both || keeps {
            return false;
        }
        let shown = self.shown(key);
        self.refuse(
            key.span(),
            format!("a key of `{shown}` has no PHP# form in a `Map` keyed by `int|string`: PHP turns a string of an integer's digits into the `int`, as `'42'` into 42, and a PHP# `Map` keeps it a string (spec section 12)"),
        );

        true
    }

    /// Whether `map` is a `Map` keyed by `int` and `string` both, as PHP's `array<array-key, V>` is, whose keys a PHP
    /// array gives as PHP holds them (spec section 12).
    fn keyed_by_both(&self, map: &Expression<'_>) -> bool {
        self.type_of(map).is_some_and(|union| {
            union.types.iter().any(|atomic| {
                let TAtomic::Array(TArray::Keyed(keyed)) = atomic else {
                    return false;
                };
                keyed.parameters.as_ref().is_some_and(|(keys, _)| {
                    let kinds = kinds(keys);
                    kinds.contains(&Kind::Int) && kinds.contains(&Kind::String)
                })
            })
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

    /// `value`, the whole value a `return` gives or an assignment writes, in PHP#, where nothing binds around it, as
    /// [`Converter::expression`] converts it.
    pub(super) fn value(&mut self, value: &Expression<'_>, expected: Option<&TUnion>) {
        match value {
            Expression::Match(r#match) => self.match_expression(r#match, MatchPlace::Value),
            _ => self.expression(value, expected),
        }
    }

    /// `expression` in PHP#. `expected` is the declared type of the place its value lands, when there is one, which
    /// decides the PHP# form of a literal, as `[]` is an empty `Map`, `[:]`, where a `Map` is declared (spec section
    /// 12).
    pub(crate) fn expression(&mut self, expression: &Expression<'_>, expected: Option<&TUnion>) {
        if let Some(expected) = expected
            && matches!(expression, Expression::Array(_) | Expression::Literal(Literal::String(_)))
            && self.callable_literal(expression, expected)
        {
            return;
        }
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
                self.elements(expression, array.elements.iter(), expected);
            }
            Expression::LegacyArray(array) => {
                if array.elements.is_empty() {
                    return self.empty_array(expression, expected);
                }
                self.edits.replace_range(array.array.span.start.offset, array.left_parenthesis.end.offset, "[");
                self.edits.replace(array.right_parenthesis, "]");
                self.elements(expression, array.elements.iter(), expected);
            }
            Expression::ArrayAccess(access) => {
                self.untyped(access.array, "an index");
                self.untyped(access.index, "a key");
                if self.is(access.array, &[Kind::Map]) {
                    self.refuse(expression.span(), MISSING_KEY);
                }
            }
            Expression::Conditional(conditional) => {
                let Some(then) = conditional.then else {
                    return self.elvis(expression, conditional.condition, conditional.r#else);
                };
                self.condition(conditional.condition, false);
                self.expression(then, expected);
                self.expression(conditional.r#else, expected);
            }
            Expression::Throw(throw) => self.expression(throw.exception, None),
            Expression::Closure(_) | Expression::ArrowFunction(_) => self.lambda(expression, expected),
            Expression::PartialApplication(application) => self.method_value(expression, application),
            Expression::Match(r#match) => self.match_expression(r#match, MatchPlace::Operand),
            Expression::Clone(clone) => self.clone(expression, clone.object),
            Expression::List(_) => self.refuse(expression.span(), "destructuring is not converted yet"),
            Expression::CompositeString(string) => self.template(expression, string),
            Expression::MagicConstant(constant) => self.magic_constant(constant),
            Expression::AnonymousClass(_) => {
                self.refuse(expression.span(), "an anonymous class has no PHP# form: no type nests in another");
            }
            Expression::Construct(_) => self.construct(expression),
            Expression::Yield(_) => self.refuse(expression.span(), "a generator is not converted yet"),
            _ => self.refuse(expression.span(), "this expression has no PHP# form"),
        }
    }

    /// `clone $x` as `x with { }`, a copy with no property set (spec section 10). `with` runs as PHP's `clone`, so a
    /// class's `__clone` runs on the copy as it does in PHP.
    fn clone(&mut self, expression: &Expression<'_>, object: &Expression<'_>) {
        let known = self.type_of(object).is_some_and(|union| {
            union
                .types
                .iter()
                .any(|atomic| matches!(atomic, TAtomic::Object(mago_codex::ttype::atomic::object::TObject::Named(_))))
        });
        if !known {
            let shown = self.shown(object);
            return self.refuse(
                expression.span(),
                format!("`clone` of `{shown}` is not converted yet: `with` copies a value of a known class"),
            );
        }
        self.expression(object, None);
        let copied = self.render(object);
        let copied = if is_primary(object) { copied } else { format!("({copied})") };
        self.edits.replace(expression.span(), format!("{copied} with {{ }}"));
    }

    /// Refuses `expression` when the analysis gives it `mixed`, which PHP lets reach `operation` and PHP# lets reach it
    /// only from a known type, and gives whether it did. Its own parts convert either way.
    fn untyped(&mut self, expression: &Expression<'_>, operation: &str) -> bool {
        self.expression(expression, None);
        self.mixed(expression, operation)
    }

    /// Refuses each property read that `value`, a value that lands in the declared type `place`, as a method returns
    /// it or a call passes it, gives as itself or as a branch of `?:`, where the analysis narrows the property past
    /// its declared type and `place` holds only the narrowed one, as `$this->owner` after `$this->owner instanceof
    /// Page`. PHP# reads a property by its declared type wherever it is read, since it may change between a test and a
    /// read (spec section 21).
    pub(crate) fn narrowed(&mut self, value: &Expression<'_>, place: &TUnion) {
        let (object, property) = match value {
            Expression::Parenthesized(parenthesized) => return self.narrowed(parenthesized.expression, place),
            Expression::Conditional(conditional) => {
                if let Some(then) = conditional.then {
                    self.narrowed(then, place);
                }
                return self.narrowed(conditional.r#else, place);
            }
            Expression::Access(Access::Property(access)) => (access.object, &access.property),
            _ => return,
        };
        let (Some(read), ClassLikeMemberSelector::Identifier(name)) = (self.type_of(value), property) else {
            return;
        };
        // An accessor's own storage is its `field`, which no other code writes.
        if self.method.as_ref().and_then(|method| method.storage.as_deref()) == Some(name.value) {
            return;
        }
        let classes: Vec<&TAtomic> = self
            .type_of(object)
            .map(|union| union.types.iter().filter(|atomic| !atomic.is_null()).collect())
            .unwrap_or_default();
        let [TAtomic::Object(TObject::Named(class))] = classes.as_slice() else {
            return;
        };
        let property = [b"$".as_slice(), name.value].concat();
        let codebase = self.input.codebase;
        let Some(declared) = codebase
            .get_declaring_property(class.name.as_bytes(), &property)
            .and_then(|metadata| metadata.type_metadata.as_ref())
            .map(|metadata| &metadata.type_union)
            .filter(|declared| !declared.has_template_types() && !declared.types.iter().any(TAtomic::is_mixed))
        else {
            return;
        };
        let holds = |wider: &TUnion, narrower: &TUnion| {
            is_contained_by(codebase, narrower, wider, false, false, false, &mut ComparisonResult::new())
        };
        if holds(read, declared) || holds(place, declared) {
            return;
        }
        let shown = String::from_utf8_lossy(self.text(value.span())).into_owned();
        self.refuse(
            value.span(),
            format!(
                "`{shown}` reads as `{}` here, which PHP narrows it to, and PHP# reads a property by its declared type `{}`, since it may change between a test and a read (spec section 21): read it into a local first",
                read.get_id(),
                declared.get_id()
            ),
        );
    }

    /// Refuses `object`, whose member `operation` reaches, when the analysis gives it `mixed` or `object`, and gives
    /// whether it did. PHP# reaches a member only on a known class: an `Object` has none (spec section 24). Its own
    /// parts convert either way.
    fn member_of(&mut self, object: &Expression<'_>, operation: &str) -> bool {
        if self.untyped(object, operation) {
            return true;
        }
        let classless = self
            .type_of(object)
            .is_some_and(|union| union.types.iter().any(|atomic| matches!(atomic, TAtomic::Object(TObject::Any))));
        if !classless {
            return false;
        }
        let shown = self.shown(object);
        self.refuse(
            object.span(),
            format!("`{shown}` reaches {operation}, which PHP# allows only from a known class: narrow it with `is`, or declare its type (spec section 24)"),
        );

        true
    }

    /// Refuses `expression`, converted already, when the analysis gives it `mixed`, as [`Converter::untyped`] does.
    fn mixed(&mut self, expression: &Expression<'_>, operation: &str) -> bool {
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
