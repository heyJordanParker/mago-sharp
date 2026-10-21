//! The loops a law reaches: a `for … of` loop over a `List` that only counts or sums, as the Lean list function it
//! is.

use mago_names::binding::Binding;
use mago_span::HasSpan;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::ForOfTarget;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;

use crate::issues;
use crate::translate::body::Body;
use crate::translate::body::Kind;
use crate::translate::body::literal;
use crate::translate::names;

/// What a counting or summing loop adds to an int local: one, or a value, under the conditions on the element, each
/// with whether it holds.
struct Addition<'expression, 'arena> {
    /// The local's name, as the loop writes it.
    local: &'expression Expression<'arena>,
    amount: Option<&'expression Expression<'arena>>,
    conditions: Vec<(&'expression Expression<'arena>, bool)>,
}

impl Body<'_, '_> {
    /// A `for … of` loop over a `List` whose body only adds to int locals, each addition optionally under an `if` on
    /// the element, as each local's new value: the `List.countP` of the elements it counts, or the `List.sum` of the
    /// values it adds. That sum throws only when it leaves the `int` range, and the loop PHP runs throws there or on an
    /// earlier partial sum, where a law holds, so a law proven of the sum holds of the loop. Any other loop is refused.
    pub(super) fn counting_loop(&mut self, for_of: &ForOf) {
        let refuse = |body: &mut Self| {
            body.unmodeled(for_of.span(), "a loop that does more than count or sum".to_owned(), issues::LOOP);
        };
        let ForOfTarget::Value(element) = &for_of.target else {
            return refuse(self);
        };
        let mut additions = Vec::new();
        if !self.is_list(for_of.expression) || !additions_of(for_of.body, &mut Vec::new(), &mut additions) {
            return refuse(self);
        }
        let mut accumulators: Vec<u32> = Vec::new();
        let mut locals: Vec<String> = Vec::new();
        for addition in &additions {
            let Expression::ConstantAccess(name) = addition.local else {
                return refuse(self);
            };
            let Some(Binding::Local(local)) = self.checked.names().binding(&name.name) else {
                return refuse(self);
            };
            let declaration = local.declaration.start.offset;
            let r#type = self.checked.types().expression_type(addition.local);
            if self.kind(r#type) != Kind::Int || r#type.is_nullable() || self.nullable_locals.contains(&declaration) {
                return refuse(self);
            }
            if !accumulators.contains(&declaration) {
                accumulators.push(declaration);
            }
            locals.push(names::identifier(name.name.value()));
        }

        let list = self.value(for_of.expression);
        let element = names::identifier(element.name.value);
        let outer = std::mem::replace(&mut self.accumulators, accumulators);
        let mut sums: Vec<(String, Vec<String>)> = Vec::new();
        for (addition, local) in additions.iter().zip(locals) {
            let Some(added) = self.added(&element, &list, addition) else {
                self.accumulators = outer;
                return refuse(self);
            };
            match sums.iter_mut().find(|(name, _)| *name == local) {
                Some((_, terms)) => terms.push(added),
                None => sums.push((local, vec![added])),
            }
        }
        self.accumulators = outer;

        for (local, terms) in sums {
            let sum = self.bind(format!("int.ofInt ({local}.toInt + {})", terms.join(" + ")));
            self.lines.push(format!("{local} := {sum}"));
        }
    }

    /// The `Int` that `addition` adds over the elements of `list`, each named `element`, or `None` when a condition or
    /// the value it adds is no pure term.
    fn added(&mut self, element: &str, list: &str, addition: &Addition) -> Option<String> {
        let mut conditions = Vec::new();
        for (condition, holds) in &addition.conditions {
            let (lines, condition) = self.nested(|body| body.value(condition));
            if !lines.is_empty() {
                return None;
            }
            conditions.push(if *holds { condition } else { format!("(!{condition})") });
        }
        let filtered = match conditions.as_slice() {
            [] => list.to_owned(),
            _ => format!("(List.filter (fun {element} => {}) {list})", conditions.join(" && ")),
        };

        let Some(amount) = addition.amount else {
            return Some(match conditions.as_slice() {
                [] => format!("(List.length {list} : Int)"),
                _ => format!("(List.countP (fun {element} => {}) {list} : Int)", conditions.join(" && ")),
            });
        };
        if self.kind(self.checked.types().expression_type(amount)) != Kind::Int {
            return None;
        }
        let (lines, amount) = self.nested(|body| body.value(amount));
        if !lines.is_empty() {
            return None;
        }

        Some(format!("(List.map (fun {element} => {amount}.toInt) {filtered}).sum"))
    }
}

/// The additions to locals of a counting or summing loop's `body`, under `conditions`, or `false` when the body does
/// anything else: `x += e`, `x++` and `++x`, in blocks and under an `if` with or without an `else`.
fn additions_of<'expression, 'arena>(
    body: &'expression Statement<'arena>,
    conditions: &mut Vec<(&'expression Expression<'arena>, bool)>,
    additions: &mut Vec<Addition<'expression, 'arena>>,
) -> bool {
    match body {
        Statement::Block(block) => {
            block.statements.iter().all(|statement| additions_of(statement, conditions, additions))
        }
        Statement::If(r#if) => {
            let IfBody::Statement(body) = &r#if.body else {
                return false;
            };
            conditions.push((r#if.condition, true));
            let then = additions_of(body.statement, conditions, additions);
            conditions.pop();
            let Some(clause) = &body.else_clause else {
                return then;
            };
            conditions.push((r#if.condition, false));
            let otherwise = additions_of(clause.statement, conditions, additions);
            conditions.pop();

            then && otherwise
        }
        Statement::Expression(statement) => {
            let (local, amount) = match statement.expression {
                Expression::Assignment(assignment)
                    if matches!(assignment.operator, AssignmentOperator::Addition(_)) =>
                {
                    (assignment.lhs, (literal(assignment.rhs) != Some(1)).then_some(assignment.rhs))
                }
                Expression::UnaryPostfix(postfix)
                    if matches!(postfix.operator, UnaryPostfixOperator::PostIncrement(_)) =>
                {
                    (postfix.operand, None)
                }
                Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::PreIncrement(_), operand }) => {
                    (*operand, None)
                }
                _ => return false,
            };
            additions.push(Addition { local, amount, conditions: conditions.clone() });

            true
        }
        _ => false,
    }
}
