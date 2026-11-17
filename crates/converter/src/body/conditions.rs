//! Conditions (spec section 21). A PHP# condition is a `bool`, so a PHP condition of another type becomes the
//! comparison PHP's truthiness makes for that type: `count != 0` for an `int`, `!items.isEmpty()` for a collection,
//! `order != null` for an object that may be null. `isset` and `empty` become the same comparisons.

use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Clone;
use mago_syntax::cst::Closure;
use mago_syntax::cst::Construct;
use mago_syntax::cst::DoWhile;
use mago_syntax::cst::EmptyConstruct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::Foreach;
use mago_syntax::cst::ForeachTarget;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::IssetConstruct;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Unset;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::Yield;
use mago_syntax::token::is_sharp_keyword;
use mago_syntax::walker::MutWalker;

use super::Kind;
use super::atomic_kinds;
use super::is_primary;
use super::kinds;
use super::locals::Bound;
use crate::Converter;
use crate::types::type_text;

/// The assignment PHP runs first in `condition`, when a comparison reads its value: the left side, in parentheses, of
/// a comparison, as `($at = strpos($url, '#')) !== false`, which is itself the condition or the left side of one.
fn first_compared<'condition, 'arena>(
    condition: &'condition Expression<'arena>,
) -> Option<&'condition Expression<'arena>> {
    let Expression::Binary(binary) = condition.unparenthesized() else {
        return None;
    };
    let lhs = binary.lhs.unparenthesized();
    match lhs {
        Expression::Assignment(_) if binary.operator.is_comparison() => {
            matches!(binary.lhs, Expression::Parenthesized(_)).then_some(lhs)
        }
        _ => first_compared(binary.lhs),
    }
}

/// Whether PHP reads every value of `union` as true: an object, a closure, a callable, an enum case or a class value,
/// never null, and no `mixed` or type parameter that may hold a scalar.
fn always_truthy(union: &TUnion) -> bool {
    !union.types.is_empty()
        && union.types.iter().all(|atomic| {
            !atomic.is_mixed()
                && (!atomic.is_generic_parameter() || atomic.is_object_type())
                && matches!(atomic_kinds(atomic), [Kind::Object | Kind::Enum | Kind::Class])
        })
}

/// The patterns of the values PHP reads as false among those of the kinds `bare`, or of every value where it is `mixed`:
/// `null`, `false`, `0`, `0.0`, `""`, `"0"` and the empty list `[]`. An object, an enum case and a class value are
/// always truthy and add none. None where `bare` holds a `Map`, whose empty one no pattern matches.
fn falsy_patterns(bare: &[Kind], mixed: bool) -> Option<Vec<&'static str>> {
    if mixed {
        return Some(vec!["null", "false", "0", "0.0", "\"\"", "\"0\"", "[]"]);
    }
    let mut patterns = Vec::new();
    for kind in bare {
        match kind {
            Kind::Bool => patterns.push("false"),
            Kind::Int => patterns.push("0"),
            Kind::Float => patterns.push("0.0"),
            Kind::String => patterns.extend(["\"\"", "\"0\""]),
            Kind::List => patterns.push("[]"),
            Kind::Object | Kind::Enum | Kind::Class => {}
            _ => return None,
        }
    }

    Some(patterns)
}

/// Whether each value of `union` but null is a `bool`, an `int`, a `float` or a string, which a condition reads by its
/// type.
fn is_scalar(union: &TUnion) -> bool {
    let bare: Vec<Kind> = kinds(union).into_iter().filter(|kind| *kind != Kind::Null).collect();

    !bare.is_empty() && bare.iter().all(|kind| matches!(kind, Kind::Bool | Kind::Int | Kind::Float | Kind::String))
}

/// The variable and the name of the property `expression` reads, as `$this` and `pages` for `$this->pages`.
fn property_place<'arena>(expression: &Expression<'arena>) -> Option<(&'arena [u8], &'arena [u8])> {
    let Expression::Access(Access::Property(access)) = expression else {
        return None;
    };
    let (Expression::Variable(Variable::Direct(object)), ClassLikeMemberSelector::Identifier(name)) =
        (access.object, &access.property)
    else {
        return None;
    };

    Some((object.name, name.value))
}

/// How a condition tests a property.
enum Test<'ast, 'arena> {
    /// PHP's truthiness of it.
    Truthy,
    /// `!== null`.
    NotNull,
    /// `instanceof` the class this names.
    Instance(&'ast Expression<'arena>),
    /// `=== null`, which holds where the property holds no value.
    Null,
    /// `!(… instanceof …)` of the class this names, which holds where the property is no instance of it.
    NotInstance(&'ast Expression<'arena>),
}

impl Test<'_, '_> {
    /// Whether the test holds where the property holds no value it binds, as `=== null` does, so the code after an
    /// `if` of it that always leaves reads the binding, and never the code the `if` guards.
    fn holds_unbound(&self) -> bool {
        matches!(self, Test::Null | Test::NotInstance(_))
    }
}

/// The property `condition` tests, and how: `$this->pages`, `$this->pages !== null`, `null !== $this->pages`,
/// `$this->owner instanceof Page`, `$this->pages === null` or `!$this->owner instanceof Page`.
fn tested_property<'ast, 'arena>(
    condition: &'ast Expression<'arena>,
) -> Option<(&'ast Expression<'arena>, Test<'ast, 'arena>)> {
    let is_null = |side: &Expression<'_>| matches!(side.unparenthesized(), Expression::Literal(Literal::Null(_)));
    let names_class = |class: &Expression<'_>| matches!(class, Expression::Identifier(_) | Expression::Self_(_));
    let (read, test) = match condition.unparenthesized() {
        read @ Expression::Access(Access::Property(_)) => (read, Test::Truthy),
        Expression::Binary(binary) => match binary.operator {
            BinaryOperator::NotIdentical(_) if is_null(binary.rhs) => (binary.lhs.unparenthesized(), Test::NotNull),
            BinaryOperator::NotIdentical(_) if is_null(binary.lhs) => (binary.rhs.unparenthesized(), Test::NotNull),
            BinaryOperator::Identical(_) if is_null(binary.rhs) => (binary.lhs.unparenthesized(), Test::Null),
            BinaryOperator::Identical(_) if is_null(binary.lhs) => (binary.rhs.unparenthesized(), Test::Null),
            BinaryOperator::Instanceof(_) if names_class(binary.rhs) => {
                (binary.lhs.unparenthesized(), Test::Instance(binary.rhs))
            }
            _ => return None,
        },
        Expression::UnaryPrefix(unary) if matches!(unary.operator, UnaryPrefixOperator::Not(_)) => {
            let Expression::Binary(binary) = unary.operand.unparenthesized() else {
                return None;
            };
            if !matches!(binary.operator, BinaryOperator::Instanceof(_)) || !names_class(binary.rhs) {
                return None;
            }
            (binary.lhs.unparenthesized(), Test::NotInstance(binary.rhs))
        }
        _ => return None,
    };

    property_place(read).map(|_| (read, test))
}

/// Whether `statement` always leaves the code after it: a `return`, a `throw`, a `break` or a `continue`, or a block
/// that ends in one.
fn leaves(statement: &Statement<'_>) -> bool {
    match statement {
        Statement::Return(_) | Statement::Break(_) | Statement::Continue(_) => true,
        Statement::Expression(expression) => matches!(expression.expression, Expression::Throw(_)),
        Statement::Block(block) => block.statements.last().is_some_and(leaves),
        _ => false,
    }
}

/// The parts `condition` joins with `&&`, each of which holds when the code it guards runs.
fn conjuncts<'ast, 'arena>(condition: &'ast Expression<'arena>) -> Vec<&'ast Expression<'arena>> {
    let joined = |expression: &Expression<'_>| matches!(expression, Expression::Binary(binary) if matches!(binary.operator, BinaryOperator::And(_)));
    match condition {
        Expression::Binary(binary) if joined(condition) => {
            let mut parts = conjuncts(binary.lhs);
            parts.extend(conjuncts(binary.rhs));
            parts
        }
        Expression::Parenthesized(parenthesized) if joined(parenthesized.expression) => {
            conjuncts(parenthesized.expression)
        }
        _ => vec![condition],
    }
}

/// Walks the code that runs after a condition tests the property `object->property` and while the test holds, and
/// finds each read of the property there, and whether code may change the property between the test and a read: a
/// call or a write of the property or of `object` before the read, a loop that reads it and runs either, or a lambda
/// that reads it, which runs later. A write after the last read changes nothing the binding gives.
pub(super) struct GuardedReads<'ast, 'arena> {
    object: &'arena [u8],
    property: &'arena [u8],
    reads: Vec<&'ast Expression<'arena>>,
    /// Each place a write names the property at, which writes the property itself and never the binding.
    writes: Vec<Span>,
    /// Whether the walk is past a call or a write that may change the property.
    after_change: bool,
    /// Whether each loop around the walk reads the property and whether it may change it, innermost last.
    loops: Vec<(bool, bool)>,
    lambdas: usize,
    changed: bool,
}

impl<'ast, 'arena> GuardedReads<'ast, 'arena> {
    fn new(object: &'arena [u8], property: &'arena [u8]) -> Self {
        Self {
            object,
            property,
            reads: Vec::new(),
            writes: Vec::new(),
            after_change: false,
            loops: Vec::new(),
            lambdas: 0,
            changed: false,
        }
    }

    /// The reads of the property that read its value, past the places that write it.
    fn values(&self) -> impl Iterator<Item = &'ast Expression<'arena>> {
        self.reads.iter().copied().filter(|read| !self.writes.contains(&read.span()))
    }

    /// Notes that `place` is written, which may change the property where it is the property, `object`, an element of
    /// either, or a destructuring.
    fn written(&mut self, place: &Expression<'_>) {
        let mut base = place;
        loop {
            base = match base {
                Expression::ArrayAccess(access) => access.array,
                Expression::ArrayAppend(append) => append.array,
                _ => break,
            };
        }
        let rebound = matches!(base, Expression::Variable(Variable::Direct(variable)) if variable.name == self.object);
        let property = property_place(base) == Some((self.object, self.property));
        if property {
            self.writes.push(base.span());
        }
        if rebound || property || super::locals::destructured(base).is_some() {
            self.may_change();
        }
    }

    fn may_change(&mut self) {
        self.after_change = true;
        if let Some(innermost) = self.loops.last_mut() {
            innermost.1 = true;
        }
    }

    fn left_loop(&mut self) {
        let Some((read, changes)) = self.loops.pop() else {
            return;
        };
        self.changed |= read && changes;
        if let Some(outer) = self.loops.last_mut() {
            outer.0 |= read;
            outer.1 |= changes;
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for GuardedReads<'ast, 'arena> {
    fn walk_in_expression(&mut self, expression: &'ast Expression<'arena>, _: &mut ()) {
        if property_place(expression) != Some((self.object, self.property)) {
            return;
        }
        self.changed |= self.after_change || self.lambdas > 0;
        if let Some(innermost) = self.loops.last_mut() {
            innermost.0 = true;
        }
        self.reads.push(expression);
    }

    // A write happens after PHP reads the value it writes, so a read in that value reads what the test read.
    fn walk_out_assignment(&mut self, assignment: &'ast Assignment<'arena>, _: &mut ()) {
        self.written(assignment.lhs);
    }

    fn walk_out_unary_prefix(&mut self, unary: &'ast UnaryPrefix<'arena>, _: &mut ()) {
        if matches!(unary.operator, UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)) {
            self.written(unary.operand);
        }
    }

    fn walk_out_unary_postfix(&mut self, unary: &'ast UnaryPostfix<'arena>, _: &mut ()) {
        self.written(unary.operand);
    }

    fn walk_out_unset(&mut self, unset: &'ast Unset<'arena>, _: &mut ()) {
        for value in unset.values.iter() {
            self.written(value);
        }
    }

    fn walk_in_foreach_target(&mut self, target: &'ast ForeachTarget<'arena>, _: &mut ()) {
        match target {
            ForeachTarget::Value(target) => self.written(target.value),
            ForeachTarget::KeyValue(target) => {
                self.written(target.key);
                self.written(target.value);
            }
        }
    }

    fn walk_in_try_catch_clause(&mut self, clause: &'ast TryCatchClause<'arena>, _: &mut ()) {
        if clause.variable.as_ref().is_some_and(|variable| variable.name == self.object) {
            self.may_change();
        }
    }

    fn walk_out_call(&mut self, _: &'ast Call<'arena>, _: &mut ()) {
        self.may_change();
    }

    fn walk_out_instantiation(&mut self, _: &'ast Instantiation<'arena>, _: &mut ()) {
        self.may_change();
    }

    fn walk_out_clone(&mut self, _: &'ast Clone<'arena>, _: &mut ()) {
        self.may_change();
    }

    fn walk_out_yield(&mut self, _: &'ast Yield<'arena>, _: &mut ()) {
        self.may_change();
    }

    fn walk_out_construct(&mut self, construct: &'ast Construct<'arena>, _: &mut ()) {
        if !matches!(construct, Construct::Isset(_) | Construct::Empty(_)) {
            self.may_change();
        }
    }

    fn walk_in_closure(&mut self, _: &'ast Closure<'arena>, _: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_closure(&mut self, _: &'ast Closure<'arena>, _: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_arrow_function(&mut self, _: &'ast ArrowFunction<'arena>, _: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_arrow_function(&mut self, _: &'ast ArrowFunction<'arena>, _: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_foreach(&mut self, _: &'ast Foreach<'arena>, _: &mut ()) {
        self.loops.push((false, false));
    }

    fn walk_out_foreach(&mut self, _: &'ast Foreach<'arena>, _: &mut ()) {
        self.left_loop();
    }

    fn walk_in_for(&mut self, _: &'ast For<'arena>, _: &mut ()) {
        self.loops.push((false, false));
    }

    fn walk_out_for(&mut self, _: &'ast For<'arena>, _: &mut ()) {
        self.left_loop();
    }

    fn walk_in_while(&mut self, _: &'ast While<'arena>, _: &mut ()) {
        self.loops.push((false, false));
    }

    fn walk_out_while(&mut self, _: &'ast While<'arena>, _: &mut ()) {
        self.left_loop();
    }

    fn walk_in_do_while(&mut self, _: &'ast DoWhile<'arena>, _: &mut ()) {
        self.loops.push((false, false));
    }

    fn walk_out_do_while(&mut self, _: &'ast DoWhile<'arena>, _: &mut ()) {
        self.left_loop();
    }
}

/// Whether a test of `expression` may read it again and find the type it tested: a variable. A property never narrows,
/// because each read may give another value (spec section 21), so a test of one reads it once, and the code a test
/// guards reads the value the test binds, as [`Converter::guarded`] writes it.
fn rereads(expression: &Expression<'_>) -> bool {
    match expression {
        Expression::Parenthesized(parenthesized) => rereads(parenthesized.expression),
        Expression::Variable(Variable::Direct(_)) => true,
        _ => false,
    }
}

/// The walk of `statements`, the code a condition guards, for [`Converter::guarded`].
pub(super) fn walk_guarded<'ast, 'arena>(
    statements: &'ast [Statement<'arena>],
) -> impl Fn(&mut GuardedReads<'ast, 'arena>) {
    move |reads| statements.iter().for_each(|statement| reads.walk_statement(statement, &mut ()))
}

/// Why a condition of `shown`, a scalar no native declaration types, has no PHP# form.
pub(super) fn unproven_scalar(shown: &str) -> String {
    format!(
        "a condition of `{shown}` has no PHP# form: no native declaration makes it the type the analysis gives it at runtime, as a framework's PHPDoc `@property` doesn't, and PHP may hold another type there, as `1` for a `bool` or `\"0\"` for an `int`, which PHP# reads by its type (spec section 21)"
    )
}

impl Converter<'_, '_> {
    /// Whether a condition may read `value` as the scalar the analysis types it, which it holds at runtime, or refuses
    /// it. A local it reads keeps that only while every value written to it holds its type.
    pub(super) fn proven(&mut self, value: &Expression<'_>) -> bool {
        if !self.holds_type_at_runtime(value) {
            let shown = String::from_utf8_lossy(self.text(value.span())).into_owned();
            self.refuse(value.span(), unproven_scalar(&shown));
            return false;
        }
        if let Expression::Variable(Variable::Direct(variable)) = value.unparenthesized()
            && self.is_local(variable.name)
        {
            self.tested_local(variable.name, value.span());
        }

        true
    }

    /// A condition, which PHP# takes only as a `bool` (spec section 21). `top` says it stands alone, as an `if`'s does,
    /// so a comparison of several parts needs no parentheses.
    pub(super) fn condition(&mut self, condition: &Expression<'_>, top: bool) {
        if self.binding(condition) {
            return;
        }
        // A `bool` condition compiles to a test of the value itself, which reads PHP's truthiness of whatever the value
        // holds at runtime, as `1` where only PHPDoc says `bool`, so it needs no proof of the type.
        if self.is(condition, &[Kind::Bool]) {
            return self.expression(condition, None);
        }
        if let Some(test) = self.truthiness(condition, false, top) {
            self.edits.replace(condition.span(), test);
        }
    }

    /// Converts what `convert` converts, a condition and the code it guards, which reads a property the condition
    /// tests by the name the test binds it to, where the analysis narrows the read past the property's declared type
    /// and nothing changes the property between the test and the read, which `walk` walks. PHP# never narrows a
    /// property, since each read may give another value (spec section 21), and a binding holds the value it tested:
    /// `if ($this->pages) { return $this->pages; }` is `if (this.pages is int pages && pages != 0) { return pages; }`.
    pub(super) fn guarded<'ast, 'arena>(
        &mut self,
        condition: &'ast Expression<'arena>,
        walk: impl Fn(&mut GuardedReads<'ast, 'arena>),
        convert: impl FnOnce(&mut Self),
    ) {
        let bound = self.bind_tested(condition, walk);
        convert(self);
        if bound && let Some(locals) = self.locals() {
            locals.bound.pop();
        }
    }

    /// Binds the first property a part of `condition` tests to hold a value, by truthiness, `!== null` or `instanceof`,
    /// for the rest of the condition and the code `walk` walks, as [`Converter::bind`] does, and gives whether it did.
    fn bind_tested<'ast, 'arena>(
        &mut self,
        condition: &'ast Expression<'arena>,
        walk: impl Fn(&mut GuardedReads<'ast, 'arena>),
    ) -> bool {
        let parts = conjuncts(condition);
        parts.iter().enumerate().any(|(at, part)| {
            tested_property(part).is_some_and(|(_, test)| !test.holds_unbound())
                && self.bind(part, part.span().end.offset, |reads| {
                    for later in &parts[at + 1..] {
                        reads.walk_expression(later, &mut ());
                    }
                    walk(reads);
                })
        })
    }

    /// Binds the property `statement` tests to hold no value, by `=== null` or `!… instanceof`, for `rest`, the
    /// statements after it, where `statement` is an `if` with no `else` whose body always leaves, as [`Converter::bind`]
    /// does, and gives whether it did. `if ($this->logger === null) { return; }` is `if (this.logger is not Logger
    /// logger) { return; }`, as the standard library writes it, and the statements after it read `logger`.
    pub(super) fn bind_left<'ast, 'arena>(
        &mut self,
        statement: &'ast Statement<'arena>,
        rest: &'ast [Statement<'arena>],
    ) -> bool {
        let Statement::If(r#if) = statement else {
            return false;
        };
        let IfBody::Statement(body) = &r#if.body else {
            return false;
        };
        body.else_if_clauses.is_empty()
            && body.else_clause.is_none()
            && leaves(body.statement)
            && tested_property(r#if.condition).is_some_and(|(_, test)| test.holds_unbound())
            && self.bind(r#if.condition, statement.span().end.offset, walk_guarded(rest))
    }

    /// Binds the property `test` tests where the code `walk` walks reads it, from the offset `from` on, at a type the
    /// analysis narrows past the property's declared type, and nothing may change the property before the read, which
    /// [`GuardedReads`] decides. Gives whether it did. An accessor's own storage, `field`, narrows, and a property a
    /// binding around holds already reads that binding.
    fn bind<'ast, 'arena>(
        &mut self,
        test: &'ast Expression<'arena>,
        from: u32,
        walk: impl FnOnce(&mut GuardedReads<'ast, 'arena>),
    ) -> bool {
        let Some((object, property)) = tested_property(test).and_then(|(read, _)| property_place(read)) else {
            return false;
        };
        let storage = self.method.as_ref().and_then(|method| method.storage.as_deref());
        let held = tested_property(test).is_some_and(|(read, _)| self.bound_name(read).is_some());
        if (object == b"$this" && storage == Some(property)) || held {
            return false;
        }
        let mut reads = GuardedReads::new(object, property);
        walk(&mut reads);
        if reads.changed || !reads.values().any(|read| self.narrowed_read(read).is_some()) {
            return false;
        }
        let name = self.binding_name(property);
        let Some(locals) = self.locals() else {
            return false;
        };
        let (object, property, test, kept) = (object.to_vec(), property.to_vec(), test.span(), reads.writes);
        locals.bound.push(Bound { object, property, name, test, from, kept });

        true
    }

    /// The name a binding of `property` takes: the property's own name in camel case, `pages`, or the first of
    /// `pages2`, `pages3` and on where the method has a variable or a parameter of that name, a local it renames takes
    /// it, or a binding around holds it. Each of them takes one name, so one of the first that many numbers and one is
    /// free.
    fn binding_name(&self, property: &[u8]) -> String {
        let camel = String::from_utf8_lossy(&mago_casing::to_camel_case(property)).into_owned();
        let own = if camel.is_empty() { "value".to_owned() } else { camel };
        let bound = self.method.iter().flat_map(|method| &method.locals.bound);
        let taken = |candidate: &str| {
            self.variables.iter().any(|variable| variable.strip_prefix(b"$") == Some(candidate.as_bytes()))
                || self.renamed.iter().any(|(_, renamed)| renamed == candidate)
                || bound.clone().any(|bound| bound.name == candidate)
        };
        let names = self.variables.len() + self.renamed.len() + bound.clone().count();
        let name = (1..=names + 1)
            .map(|n| if n == 1 { own.clone() } else { format!("{own}{n}") })
            .find(|candidate| !taken(candidate))
            .unwrap_or_default();

        if is_sharp_keyword(name.as_bytes()) { format!("@{name}") } else { name }
    }

    /// The name a condition binds the property `expression` reads to, where `expression` reads it in the code the
    /// binding covers, and writes no value to it.
    pub(super) fn bound_name(&self, expression: &Expression<'_>) -> Option<String> {
        let (object, property) = property_place(expression)?;
        let span = expression.span();
        self.method.as_ref()?.locals.bound.iter().rev().find_map(|bound| {
            (bound.object == object
                && bound.property == property
                && span.start.offset >= bound.from
                && !bound.kept.contains(&span))
            .then(|| bound.name.clone())
        })
    }

    /// Writes `condition` as the binding of the property it tests, where [`Converter::bind`] binds it, and gives
    /// whether it did: `this.pages is int pages && pages != 0` for PHP's truthiness, `this.owner is Page owner` for
    /// `!== null` and `instanceof`, and `this.owner is not Page owner` for `=== null` and `!… instanceof`.
    fn binding(&mut self, condition: &Expression<'_>) -> bool {
        let Some(name) = self.method.as_ref().and_then(|method| {
            method.locals.bound.last().filter(|bound| bound.test == condition.span()).map(|bound| bound.name.clone())
        }) else {
            return false;
        };
        let Some((read, test)) = tested_property(condition) else {
            return false;
        };
        let Some(union) = self.type_of(read) else {
            self.refuse(read.span(), "the analysis gives this condition no type");
            return true;
        };
        let held = TUnion::from_vec(union.get_non_nullable_types());
        let is = if test.holds_unbound() { "is not" } else { "is" };
        let (pattern, then) = match test {
            Test::Instance(class) | Test::NotInstance(class) => {
                self.expression(class, None);
                (self.render(class), String::new())
            }
            Test::Truthy | Test::NotNull | Test::Null => {
                let truthy = matches!(test, Test::Truthy);
                if truthy && is_scalar(union) && !self.proven(read) {
                    return true;
                }
                let written = match type_text(&held, self.input.codebase, &mut self.imports) {
                    Ok(written) => written,
                    Err(reason) => {
                        self.refuse(read.span(), reason);
                        return true;
                    }
                };
                let then = if !truthy || always_truthy(&held) {
                    String::new()
                } else {
                    let Some((then, _)) = self.test_of(&held, read.span(), |_| name.clone(), true, false, true) else {
                        return true;
                    };
                    format!(" && {then}")
                };
                (written, then)
            }
        };
        self.expression(read, None);
        let read = self.render(read);
        self.edits.replace(condition.span(), format!("{read} {is} {pattern} {name}{then}"));

        true
    }

    /// The condition of an `else if`, which refuses an assignment of a local: PHP runs it only when the conditions
    /// before it fail, and the line of its own [`Converter::assigned_condition`] writes before an `if` runs every time.
    pub(super) fn chained_condition(&mut self, condition: &Expression<'_>) {
        if let Expression::Assignment(assignment) = condition
            && let Expression::Variable(Variable::Direct(variable)) = assignment.lhs
            && assignment.operator.is_assign()
        {
            let shown = String::from_utf8_lossy(variable.name);
            return self.refuse(
                condition.span(),
                format!("an `else if` whose condition assigns `{shown}` has no PHP# form: a PHP# assignment is a statement, and a line before the `if` runs it every time, where PHP runs this one only when the conditions before it fail"),
            );
        }
        self.condition(condition, true);
    }

    /// `if ($user = $this->find($id))`, which tests the value it assigns, as the assignment on its own line and the
    /// test of the local it writes, `const user = this.find(id);` and `if (user != null)`, since a PHP# assignment is
    /// a statement. PHP runs the assignment first and then tests the local, as the two lines do. Gives whether the
    /// condition is such an assignment. An `else if` keeps it as a condition, which refuses, since its assignment runs
    /// only when the conditions before it fail.
    pub(super) fn assigned_condition(&mut self, statement: &Statement<'_>, r#if: &If<'_>) -> bool {
        let condition = r#if.condition;
        let Expression::Assignment(assignment) = condition else {
            return false;
        };
        let (Expression::Variable(Variable::Direct(variable)), true) =
            (assignment.lhs, assignment.operator.is_assign())
        else {
            return false;
        };
        let Some(union) = self.type_of(assignment.rhs).filter(|_| self.is_local(variable.name)) else {
            return false;
        };
        if !self.declaration(statement, condition) {
            self.expression(condition, None);
        }
        if is_scalar(union) && !self.proven(assignment.lhs) {
            return true;
        }
        let named = |this: &mut Self| this.variable_name(variable.name);
        let Some((test, _)) = self.test_of(union, condition.span(), named, true, false, true) else {
            return true;
        };
        let indentation = self.indentation(statement.span().start.offset);
        self.edits.delete(r#if.r#if.span.start.offset, condition.span().start.offset);
        self.edits.insert(condition.span().end.offset, format!(";\n{indentation}if ({test}"));

        true
    }

    /// `if (($at = strpos($url, '#')) !== false)`, whose condition first assigns a local a comparison then reads, as
    /// the assignment on its own line and the comparison of the local, `const at = strpos(url, "#");` and
    /// `if ((at) != false)`. PHP runs the assignment before any other part of the condition, as the line before the
    /// `if` runs it. Gives whether the condition is such a comparison.
    pub(super) fn compared_assignment(&mut self, statement: &Statement<'_>, r#if: &If<'_>) -> bool {
        let Some(assigned) = first_compared(r#if.condition) else {
            return false;
        };
        let Expression::Assignment(assignment) = assigned else {
            return false;
        };
        let (Expression::Variable(Variable::Direct(variable)), true) =
            (assignment.lhs, assignment.operator.is_assign())
        else {
            return false;
        };
        if !self.is_local(variable.name) {
            return false;
        }
        self.condition(r#if.condition, true);
        self.declared_by(statement, assignment);
        // Only `(` and spaces stand between `if` and the assignment, which the comparison reads first.
        let start = assigned.span().start.offset;
        let opened = String::from_utf8_lossy(&self.source[r#if.r#if.span.start.offset as usize..start as usize]);
        let indentation = self.indentation(statement.span().start.offset);
        let name = self.variable_name(variable.name);
        self.edits.delete(r#if.r#if.span.start.offset, start);
        self.edits.insert(assigned.span().end.offset, format!(";\n{indentation}{opened}{name}"));

        true
    }

    /// The `bool` test of PHP's truthiness of `expression`, or of its falsiness when `negated`, which is what `!` and
    /// `empty` read (spec section 21). A test of several parts reads the value more than once, so it is written only
    /// for a value [`rereads`] gives again, and in parentheses unless `top`. Refuses the type where PHP# has
    /// no such test.
    pub(super) fn truthiness(&mut self, expression: &Expression<'_>, negated: bool, top: bool) -> Option<String> {
        self.tested(expression, negated, top).map(|(test, _)| test)
    }

    /// PHP's `$value ?: $other` as `test ? value : other`, where `test` is the truthiness of `value`, which reads it
    /// again, so `value` is a variable, or a value that is never null that [`Converter::gives_again`] gives again at
    /// the type the test read (spec section 21). Any other value is read once, by [`Converter::elvis_once`].
    pub(super) fn elvis(&mut self, expression: &Expression<'_>, value: &Expression<'_>, other: &Expression<'_>) {
        // PHP gives a value it always reads as true, and never reads `other`.
        if self.type_of(value).is_some_and(always_truthy) {
            self.expression(value, None);
            let value = self.render(value);
            return self.edits.replace(expression.span(), value);
        }
        let never_null = self.type_of(value).is_some_and(|union| !union.can_be_null());
        let reads_again = rereads(value) || (never_null && self.gives_again(value));
        if !reads_again {
            return self.elvis_once(expression, value, other);
        }
        // A `bool` tests itself, as a condition of it does.
        let (test, value) = if self.is(value, &[Kind::Bool]) {
            self.expression(value, None);
            let rendered = self.render(value);
            (rendered.clone(), rendered)
        } else {
            let Some(tested) = self.tested(value, false, false) else {
                return;
            };
            tested
        };
        self.expression(other, None);
        let other = self.render(other);
        self.edits.replace(expression.span(), format!("{test} ? {value} : {other}"));
    }

    /// PHP's `$value ?: $other` with one read of `value`, as a call, which runs once, as in PHP (spec section 21). An
    /// object that may be null is truthy where it is one, so it is `value ?? other`. Any other value is bound once, as
    /// `match (value) { string value2 when value2 != "" && value2 != "0" => value2, default => other }`, whose `when`
    /// is PHP's truthiness of the value. A value that is never null is its arm's type already, which the checker warns
    /// on as a test that always holds, so a never-null scalar refuses.
    fn elvis_once(&mut self, expression: &Expression<'_>, value: &Expression<'_>, other: &Expression<'_>) {
        let Some(union) = self.type_of(value).cloned() else {
            return self.refuse(value.span(), "the analysis gives this condition no type");
        };
        if is_scalar(&union) && !self.proven(value) {
            return;
        }
        let kinds = kinds(&union);
        let nullable = kinds.contains(&Kind::Null);
        let bare: Vec<Kind> = kinds.into_iter().filter(|kind| *kind != Kind::Null).collect();
        let shown = String::from_utf8_lossy(self.text(value.span())).into_owned();
        if !nullable && is_scalar(&union) {
            return self.refuse(
                expression.span(),
                format!("`?:` of `{shown}` has no PHP# form: PHP's truthiness test reads the value a second time, which gives the first read only for a variable or a property, and one read through `match` binds it as `{}`, which a value that is never null already is, so the checker warns on the test (spec section 21)", union.get_id()),
            );
        }
        if nullable && matches!(bare.as_slice(), [Kind::Object] | [Kind::Enum] | [Kind::Class]) {
            self.expression(value, None);
            self.expression(other, None);
            let (value, other) = (self.render(value), self.render(other));
            return self.edits.replace(expression.span(), format!("{value} ?? {other}"));
        }

        let bound = TUnion::from_vec(union.get_non_nullable_types());
        let name = self.unused_name("value", &[]);
        let Some((test, _)) = self.test_of(&bound, value.span(), |_| name.clone(), true, false, true) else {
            return;
        };
        let written = match type_text(&bound, self.input.codebase, &mut self.imports) {
            Ok(written) => written,
            Err(reason) => return self.refuse(value.span(), reason),
        };
        self.expression(value, None);
        self.expression(other, None);
        let (value, other) = (self.render(value), self.render(other));
        self.edits.replace(
            expression.span(),
            format!("match ({value}) {{ {written} {name} when {test} => {name}, default => {other} }}"),
        );
    }

    /// Whether reading `expression` a second time gives the value and the type the first read gave: a variable, or a
    /// property the analysis reads at its declared type, of one that is never null, which every class it may be
    /// declares with no `get` hook, and no class it may be declares `__get` for. PHP runs no code between two reads of
    /// one computation, so nothing changes the value between them.
    pub(super) fn gives_again(&self, expression: &Expression<'_>) -> bool {
        match expression {
            Expression::Parenthesized(parenthesized) => self.gives_again(parenthesized.expression),
            Expression::Access(Access::Property(access)) => {
                let ClassLikeMemberSelector::Identifier(name) = &access.property else {
                    return false;
                };
                let property = [b"$".as_slice(), name.value].concat();
                let codebase = self.input.codebase;
                let plain = |class: &[u8]| {
                    let declared = codebase
                        .get_declaring_property(class, &property)
                        .is_some_and(|metadata| metadata.hooks.is_empty() && !metadata.flags.is_virtual_property());
                    let descendants = codebase.get_all_descendants(class);
                    declared
                        && std::iter::once(class)
                            .chain(descendants.iter().map(|descendant| descendant.as_bytes()))
                            .all(|class| codebase.get_declaring_method(class, b"__get").is_none())
                };
                self.gives_again(access.object)
                    && self.narrowed_read(expression).is_none()
                    && self.type_of(access.object).is_some_and(|union| {
                        union.types.iter().all(|atomic| {
                            matches!(atomic, TAtomic::Object(TObject::Named(named)) if plain(named.name.as_bytes()))
                        })
                    })
            }
            expression => rereads(expression),
        }
    }

    /// The test [`Converter::truthiness`] gives, with the PHP# text of the value it tests.
    fn tested(&mut self, expression: &Expression<'_>, negated: bool, top: bool) -> Option<(String, String)> {
        let Some(union) = self.type_of(expression) else {
            self.refuse(expression.span(), "the analysis gives this condition no type");
            return None;
        };
        // A scalar's test reads it by its type, which it holds at runtime only where a declaration makes it.
        if is_scalar(union) && !self.proven(expression) {
            return None;
        }
        let rendered = |this: &mut Self| {
            this.expression(expression, None);
            let rendered = this.render(expression);
            if is_primary(expression) { rendered } else { format!("({rendered})") }
        };
        self.test_of(union, expression.span(), rendered, rereads(expression), negated, top)
    }

    /// The test of PHP's truthiness of a value of type `union`, at `span`, whose PHP# text `rendered` gives once the
    /// test is known to exist, with that text. `rereads` says whether a second read of the value gives the first's.
    fn test_of(
        &mut self,
        union: &TUnion,
        span: Span,
        rendered: impl FnOnce(&mut Self) -> String,
        rereads: bool,
        negated: bool,
        top: bool,
    ) -> Option<(String, String)> {
        let shown = union.get_id().to_string();
        let kinds = kinds(union);
        let nullable = kinds.contains(&Kind::Null);
        let bare: Vec<Kind> = kinds.into_iter().filter(|kind| *kind != Kind::Null).collect();
        // An object, and a class value, which names a class, are always truthy, so their test is their null check, as
        // is the test of a value that can only be null.
        let is_object = matches!(bare.as_slice(), [Kind::Object] | [Kind::Enum] | [Kind::Class] | []);
        let (truthy_arm, falsy_arm) = if negated { ("false", "true") } else { ("true", "false") };
        // A value PHP always reads as true has the test `true`, which still reads a value that runs code, once.
        if always_truthy(union) {
            let value = rendered(self);
            if rereads {
                return Some((truthy_arm.to_owned(), value));
            }
            let test = format!("match ({value}) {{ default => {truthy_arm} }}");
            return Some((if top { test } else { format!("({test})") }, value));
        }
        if bare.is_empty() && !nullable {
            self.refuse(
                span,
                format!("a condition of `{shown}` holds no value, which PHP# has no test for (spec section 21)"),
            );
            return None;
        }
        let scalar_parameter =
            union.types.iter().filter(|atomic| !atomic.is_object_type()).find_map(TAtomic::get_generic_parameter_name);
        if let Some(parameter) = scalar_parameter {
            self.refuse(
                span,
                format!("a condition of the type parameter `{parameter}` reads PHP's truthiness of a value that may be a scalar PHP reads as false, which PHP# has no test for: a PHP# pattern compares with `==`, which a type parameter has only through a bound (spec section 21)"),
            );
            return None;
        }

        let mixed = union.types.iter().any(TAtomic::is_mixed);
        let value = rendered(self);
        // PHP's truthiness of each type, as the test for a value that is truthy, and the test for one that is not.
        let compared = match bare.as_slice() {
            _ if mixed => None,
            [Kind::Bool] => Some((format!("{value} == true"), format!("{value} != true"))),
            [Kind::Int] => Some((format!("{value} != 0"), format!("{value} == 0"))),
            [Kind::Float] => Some((format!("{value} != 0.0"), format!("{value} == 0.0"))),
            [Kind::String] => {
                Some((format!("{value} != \"\" && {value} != \"0\""), format!("{value} == \"\" || {value} == \"0\"")))
            }
            [Kind::List] | [Kind::Map] => Some((format!("!{value}.isEmpty()"), format!("{value}.isEmpty()"))),
            _ if is_object => Some((format!("{value} != null"), format!("{value} == null"))),
            _ => None,
        };
        if let Some((truthy, falsy)) = compared {
            // `bool?` compares with `true`, which a null never equals, and an object's test is its null check.
            let (truthy, falsy) = if nullable && !is_object && bare != [Kind::Bool] {
                (format!("{value} != null && {truthy}"), format!("{value} == null || {falsy}"))
            } else {
                (truthy, falsy)
            };
            let test = if negated { falsy } else { truthy };
            let parts = test.matches("&&").count() + test.matches("||").count();
            if parts == 0 || rereads {
                return Some((if parts > 0 && !top { format!("({test})") } else { test }, value));
            }
        }
        // A value a second read may not give again, a union of such types and a value of no known type are read once, by
        // a `match` on the values PHP reads as false, which every other value is not (spec section 21).
        let Some(patterns) = falsy_patterns(&bare, mixed) else {
            let map = if bare.contains(&Kind::Map) { ", and no pattern matches an empty `Map`" } else { "" };
            self.refuse(
                span,
                format!("a condition of `{shown}` reads PHP's truthiness, which PHP# has no test for: a PHP# condition is a `bool`{map} (spec section 21)"),
            );
            return None;
        };
        let arms: Vec<&str> = (nullable && !mixed).then_some("null").into_iter().chain(patterns).collect();
        let test = format!("match ({value}) {{ {} => {falsy_arm}, default => {truthy_arm} }}", arms.join(" or "));

        Some((if top { test } else { format!("({test})") }, value))
    }

    /// `isset($a, $b)` as `a != null && b != null`, a `Map` key as `map.get(key) != null`, since `get` gives null for a
    /// missing key, as PHP's `isset` reads it, and a `List` index as `list.getOrNull(i) != null` (spec section 12). `negated` writes `!isset(…)`, which replaces
    /// `expression`, as the comparisons that test the opposite, since `!` binds tighter than `!=`.
    pub(super) fn isset(&mut self, expression: &Expression<'_>, isset: &IssetConstruct<'_>, negated: bool) {
        let compared = if negated { "==" } else { "!=" };
        let mut tests = Vec::new();
        for value in &isset.values {
            let test = match value {
                Expression::ArrayAccess(access) if self.is(access.array, &[Kind::Map]) => {
                    if self.unread(access.array, "isset") {
                        return;
                    }
                    self.untyped(access.array, "an index");
                    self.untyped(access.index, "a key");
                    if self.numeric_key(access.array, access.index) {
                        return;
                    }
                    let map = self.render(access.array);
                    let map = if is_primary(access.array) { map } else { format!("({map})") };
                    format!("{map}.get({}) {compared} null", self.render(access.index))
                }
                // A `List` gives `getOrNull(i)` past its end, where PHP's `isset` is false (spec section 12).
                Expression::ArrayAccess(access)
                    if self.is(access.array, &[Kind::List]) && self.is(access.index, &[Kind::Int]) =>
                {
                    if self.unread(access.array, "isset") {
                        return;
                    }
                    self.untyped(access.array, "an index");
                    self.untyped(access.index, "an index");
                    let list = self.render(access.array);
                    let list = if is_primary(access.array) { list } else { format!("({list})") };
                    format!("{list}.getOrNull({}) {compared} null", self.render(access.index))
                }
                Expression::ArrayAccess(access) => {
                    let shown = self.shown(access.array);
                    return self.refuse(
                        value.span(),
                        format!("`isset` at an index of `{shown}` has no PHP# form: PHP# reads a missing key from a `Map`, and a missing `int` index from a `List`"),
                    );
                }
                Expression::Variable(Variable::Direct(_)) | Expression::Access(_) => {
                    if self.unread(value, "isset") {
                        return;
                    }
                    self.expression(value, None);
                    let rendered = self.render(value);
                    format!("{rendered} {compared} null")
                }
                _ => return self.refuse(value.span(), "`isset` of this expression has no PHP# form"),
            };
            tests.push(test);
        }
        let joined = tests.join(if negated { " || " } else { " && " });
        self.edits.replace(expression.span(), if tests.len() > 1 { format!("({joined})") } else { joined });
    }

    /// `empty($x)` as the test of PHP's falsiness of `x`, as `items.isEmpty()` (spec section 21), and `!empty($x)`,
    /// which `negated` writes in place of `expression`, as the test of its truthiness.
    pub(super) fn empty(&mut self, expression: &Expression<'_>, empty: &EmptyConstruct<'_>, negated: bool) {
        if self.unread(empty.value, "empty") {
            return;
        }
        if let Some(test) = self.truthiness(empty.value, !negated, false) {
            self.edits.replace(expression.span(), test);
        }
    }

    /// Refuses `place`, which `construct`, `isset` or `empty`, reads, and gives whether it did, where PHP reads a place
    /// that holds no value without an error and PHP# reads the place itself. PHP gives a typed property that holds no
    /// value as unset, asks `__isset` or `__get` of a class that declares them, and reads past a `List`'s end. So each
    /// property along `place` converts only when it always holds a value, as one with a default or a promoted one does,
    /// and no class its object may be declares `__isset` or `__get`, and an index converts for a `Map` key only.
    fn unread(&mut self, place: &Expression<'_>, construct: &str) -> bool {
        let shown = String::from_utf8_lossy(self.text(place.span())).into_owned();
        let mut current = place;
        loop {
            let (object, property) = match current {
                Expression::Access(Access::Property(access)) => (access.object, &access.property),
                Expression::Access(Access::NullSafeProperty(access)) => (access.object, &access.property),
                Expression::ArrayAccess(access) if self.is(access.array, &[Kind::Map]) => {
                    current = access.array;
                    continue;
                }
                Expression::ArrayAccess(access) => {
                    let indexed = self.shown(access.array);
                    self.refuse(
                        place.span(),
                        format!("`{construct}` at an index of `{indexed}` reads past a `List`'s end, which PHP gives as unset and PHP# refuses"),
                    );
                    return true;
                }
                _ => return false,
            };
            if let Some(reason) = self.unset_property(object, property) {
                self.refuse(place.span(), format!("`{construct}` of `{shown}` {reason}"));
                return true;
            }
            current = object;
        }
    }

    /// Why PHP may read `property` of `object` as holding no value, where PHP# reads it: no default and no promotion,
    /// or a class `object` may be that declares `__isset` or `__get`.
    fn unset_property(&self, object: &Expression<'_>, property: &ClassLikeMemberSelector<'_>) -> Option<String> {
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return Some("reads a property named by a variable".to_owned());
        };
        let codebase = self.input.codebase;
        let Some(union) = self.type_of(object) else {
            return Some("reads a property of a value the analysis gives no type".to_owned());
        };
        let mut classes = Vec::new();
        for atomic in union.types.iter().filter(|atomic| !atomic.is_null()) {
            let TAtomic::Object(TObject::Named(named)) = atomic else {
                return Some(format!(
                    "reads a property of `{}`, whose class the analysis doesn't know",
                    union.get_id()
                ));
            };
            classes.push(named.name);
        }
        let property = [b"$".as_slice(), name.value].concat();
        for class in classes {
            let class = class.as_bytes();
            let valued = codebase.get_declaring_property(class, &property).is_some_and(|metadata| {
                metadata.flags.has_default()
                    || metadata.flags.is_promoted_property()
                    || metadata.type_declaration_metadata.is_none()
            });
            if !valued {
                return Some("reads a property that may hold no value, which PHP gives as unset without an error, and PHP# reads the property itself, which has no default and is not promoted".to_owned());
            }
            let descendants = codebase.get_all_descendants(class);
            let magic = std::iter::once(class).chain(descendants.iter().map(|descendant| descendant.as_bytes())).find(
                |class| {
                    [b"__isset".as_slice(), b"__get"]
                        .iter()
                        .any(|method| codebase.get_declaring_method(class, method).is_some())
                },
            );
            if let Some(magic) = magic {
                let magic = String::from_utf8_lossy(magic);
                return Some(format!(
                    "reads a property `{magic}`'s `__isset` or `__get` may answer for, which PHP asks and PHP# never does"
                ));
            }
        }

        None
    }
}
