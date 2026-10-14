//! The PHP each PHP# pattern form runs as.
//!
//! The analyzer analyzes this PHP and the engine's bridge lowers it, so the code the analyzer narrows on is the code
//! that runs. Spec section 21 defines the forms:
//!
//! - `x is Calc` is `x instanceof Calc`, and `x is int` is `is_int(x)`, with `is_float`, `is_string` and `is_bool`.
//! - `x is Calc c` is `($c = x) instanceof Calc`, so `c` holds the value whether or not it matches, and the binder
//!   keeps it in scope only where it matches.
//! - `x is 200` is `x === 200`, and `x is < 10` is `x < 10`. A pattern's value is a constant, and a local's value is
//!   compared in `when`.
//! - `not`, `and` and `or` are `!`, `&&` and `||`.
//! - `x is { total: > 0 }` is `is_object(x) && x->total > 0`, and `x is Paid { total: > 0 } p` is
//!   `($p = x) instanceof Paid && $p->total > 0`.
//! - `x is [Line first, ...List<Line> rest]` is `\is_array(x) && \count(x) >= 1 && ($first = x[0]) instanceof Line &&
//!   \is_array($rest = \array_slice(x, 1))`, and `x is []` is `\is_array(x) && \count(x) === 0`.
//! - `x as Calc` is `x instanceof Calc ? x : null`.
//! - A `match` that gives a value is `match (true)` with one condition per arm, its pattern's test `and` its `when`
//!   condition. A `match` that starts a statement is `if`, then `else if` per arm, and `else` for `default`.
//! - A tested index reads a missing key as null, as `??` does, so `x[k] is int n` is `is_int($n = x[k] ?? null)`.
//!   Spec section 12 handles a `Map` read with `is`, `as` and `match`, where a bare read throws on a missing key.
//!
//! A tested value that is not a local or a parameter goes into a hidden variable, `$match#N` or `$as#N`, which no
//! PHP# source can name. Its first test assigns it, so it is evaluated once, and the analyzer narrows the hidden
//! variable instead of a property, which spec section 21 never narrows. The `#` marks it as hidden, so the analyzer
//! reports no issue on its assignment. A value one named type pattern tests needs none, because the name holds it.
//!
//! Each node of the PHP keeps a span inside the form it comes from, and a span never ends before it starts. A node
//! the form has no token for, such as `instanceof` or `(`, has an empty span.

use mago_allocator::prelude::*;
use mago_span::HasSpan;
use mago_span::Span;

use crate::cst::Access;
use crate::cst::Argument;
use crate::cst::ArgumentList;
use crate::cst::ArrayAccess;
use crate::cst::As;
use crate::cst::Assignment;
use crate::cst::AssignmentOperator;
use crate::cst::Binary;
use crate::cst::BinaryOperator;
use crate::cst::Block;
use crate::cst::Call;
use crate::cst::ClassLikeMemberSelector;
use crate::cst::Conditional;
use crate::cst::DirectVariable;
use crate::cst::Expression;
use crate::cst::ExpressionStatement;
use crate::cst::FunctionCall;
use crate::cst::Hint;
use crate::cst::Identifier;
use crate::cst::If;
use crate::cst::IfBody;
use crate::cst::IfStatementBody;
use crate::cst::IfStatementBodyElseClause;
use crate::cst::Is;
use crate::cst::Keyword;
use crate::cst::ListPattern;
use crate::cst::ListPatternElement;
use crate::cst::Literal;
use crate::cst::LiteralInteger;
use crate::cst::LocalIdentifier;
use crate::cst::Match;
use crate::cst::MatchArm;
use crate::cst::MatchDefaultArm;
use crate::cst::MatchExpressionArm;
use crate::cst::Node;
use crate::cst::Parenthesized;
use crate::cst::Pattern;
use crate::cst::PatternMatch;
use crate::cst::PatternMatchArm;
use crate::cst::PatternMatchArmBody;
use crate::cst::PatternMatchPatternArm;
use crate::cst::PositionalArgument;
use crate::cst::PropertiesPattern;
use crate::cst::PropertyAccess;
use crate::cst::RestPattern;
use crate::cst::Statement;
use crate::cst::Terminator;
use crate::cst::TypePattern;
use crate::cst::UnaryPrefix;
use crate::cst::UnaryPrefixOperator;
use crate::cst::Variable;
use crate::cst::sequence::Sequence;
use crate::cst::sequence::TokenSeparatedSequence;

/// The PHP a PHP# `is`, `as` or `match` runs as.
#[derive(Debug)]
pub struct PhpShape<'arena> {
    /// An expression for an expression, and an `if` for a `match` that starts a statement.
    pub php: Node<'arena, 'arena>,
    /// How many hidden variables the PHP numbered, which the caller adds to the `temporaries` it gives the forms
    /// inside this one.
    pub temporaries: u32,
    /// Each pattern the form tests, or the type of an `as`, with the PHP that is true when the value matches it.
    pub tests: std::vec::Vec<(Span, &'arena Expression<'arena>)>,
}

/// What the PHP of a pattern depends on beyond its own text, which the analyzer and the bridge answer from the names
/// and the codebase they hold.
pub trait PatternNames {
    /// Whether the bare name at `span` is a local or a parameter.
    fn is_local(&self, span: Span) -> bool;

    /// The names of the fields of the case of an enum with data, spec section 20, that the class name resolved at
    /// `span` names, in the order the case declares them, none for a plain case. None when the name is no such case.
    /// The binder resolves a case pattern's type at its start, and a value pattern `Enum.Case` at `Case`.
    fn case_fields(&self, span: Span) -> Option<std::vec::Vec<std::vec::Vec<u8>>>;
}

/// Builds, in `arena`, the PHP a PHP# `is`, `as` or `match` runs as.
///
/// Returns `None` for any other node, and for a form the slice refuses, such as a nullable type pattern or a case
/// pattern whose fields are not the case's. `temporaries` is how many hidden variables the forms around this one hold,
/// so the new ones are numbered after them.
pub fn php_shape<'arena, A>(
    arena: &'arena A,
    node: Node<'_, 'arena>,
    temporaries: u32,
    names: &dyn PatternNames,
) -> Option<PhpShape<'arena>>
where
    A: Arena,
{
    let mut shape = Shape { arena, names, temporaries, first: temporaries, tests: std::vec::Vec::new() };
    let php = match node {
        Node::Expression(Expression::Is(is)) => Node::Expression(shape.is(is)?),
        Node::Expression(Expression::As(r#as)) => Node::Expression(shape.r#as(r#as)?),
        Node::Expression(Expression::PatternMatch(pattern_match)) => Node::Expression(shape.r#match(pattern_match)?),
        Node::Statement(Statement::PatternMatch(pattern_match)) => Node::Statement(shape.statement(pattern_match)?),
        _ => return None,
    };

    Some(PhpShape { php, temporaries: shape.temporaries - shape.first, tests: shape.tests })
}

/// The PHP functions a pattern node calls to test a value, as the identifiers its PHP calls.
///
/// They are `is_int`, `is_float`, `is_string` or `is_bool` at a scalar type of a type pattern or `as`, `is_object` at
/// the `{` of a properties pattern, `is_array` at the `[` and `count` at the `]` of a list pattern, and `is_array` at
/// the `...` and `array_slice` at the type of a list pattern's named rest. The binder records each as the name resolved
/// at that span, as it records a call's name.
pub fn called_functions(node: Node<'_, '_>) -> impl Iterator<Item = LocalIdentifier<'static>> {
    let called = |span, value: &'static [u8]| Some(LocalIdentifier { span, value });
    let functions = match node {
        Node::TypePattern(type_pattern) => [scalar_test(&type_pattern.hint), None],
        Node::As(r#as) => [scalar_test(r#as.hint), None],
        Node::PropertiesPattern(properties) => [called(properties.left_brace, b"is_object"), None],
        Node::ListPattern(list) => [called(list.left_bracket, b"is_array"), called(list.right_bracket, b"count")],
        Node::RestPattern(RestPattern { ellipsis, hint: Some(hint), .. }) => {
            [called(*ellipsis, b"is_array"), called(hint.span(), b"array_slice")]
        }
        _ => [None, None],
    };

    functions.into_iter().flatten()
}

/// Whether a type pattern or an `as` has PHP that tests a value against `hint`.
///
/// A scalar type has `is_int`, `is_float`, `is_string` or `is_bool`, and a class has `instanceof`. `php_shape` builds no
/// PHP for a form that tests any other type, so the semantic check refuses it.
#[must_use]
pub fn has_type_test(hint: &Hint<'_>) -> bool {
    type_test_of(hint).is_some()
}

/// The PHP that tests a value against a type.
enum TypeTest<'arena> {
    /// A call to the function that tests a scalar type.
    Scalar(LocalIdentifier<'static>),
    /// `instanceof` the class.
    Class(Identifier<'arena>),
}

/// The PHP test of `hint`, which both `has_type_test` and `php_shape` read, so the checker accepts exactly the types
/// the bridge lowers.
fn type_test_of<'arena>(hint: &Hint<'arena>) -> Option<TypeTest<'arena>> {
    match hint {
        Hint::Identifier(class) => Some(TypeTest::Class(*class)),
        hint => scalar_test(hint).map(TypeTest::Scalar),
    }
}

/// The function that tests a value against a scalar type, at the type's span.
fn scalar_test(hint: &Hint<'_>) -> Option<LocalIdentifier<'static>> {
    let value: &'static [u8] = match hint {
        Hint::Integer(_) => b"is_int",
        Hint::Float(_) => b"is_float",
        Hint::String(_) => b"is_string",
        Hint::Bool(_) => b"is_bool",
        _ => return None,
    };

    Some(LocalIdentifier { span: hint.span(), value })
}

struct Shape<'shape, 'arena, A> {
    arena: &'arena A,
    names: &'shape dyn PatternNames,
    /// The number of the last hidden variable.
    temporaries: u32,
    first: u32,
    tests: std::vec::Vec<(Span, &'arena Expression<'arena>)>,
}

/// A value a pattern tests. Its first read, when it has one, is the value where it is written or the assignment of
/// its hidden variable. Every other read is the variable that holds it, at the pattern that reads it, as PHP writes
/// the variable in each arm of a `match (true)`.
struct Subject<'arena> {
    first: Option<&'arena Expression<'arena>>,
    variable: Option<&'arena [u8]>,
}

impl<'arena, A> Shape<'_, 'arena, A>
where
    A: Arena,
{
    fn is(&mut self, is: &Is<'arena>) -> Option<&'arena Expression<'arena>> {
        let named = match is.pattern {
            Pattern::Not(not) => is_named(not.pattern),
            pattern => is_named(pattern),
        };
        let mut subject =
            if named { Subject { first: Some(self.tested(is.value)), variable: None } } else { self.subject(is.value) };
        let test = self.test(is.pattern, is.is.span, &mut subject)?;
        self.tests.push((is.pattern.span(), test));

        Some(test)
    }

    fn r#as(&mut self, r#as: &As<'arena>) -> Option<&'arena Expression<'arena>> {
        let mut subject = self.subject_named(r#as.value, "as");
        let tested = self.read(&mut subject, r#as.value.span());
        let condition = self.type_test(r#as.hint, tested)?;
        self.tests.push((r#as.hint.span(), condition));
        let then = self.read(&mut subject, r#as.r#as.span);
        // `null` sits at the type's start, so the conditional's span is not its test's, and each keeps its own type.
        let at = start_of(r#as.hint.span());

        Some(self.alloc(Expression::Conditional(Conditional {
            condition,
            question_mark: start_of(r#as.r#as.span),
            then: Some(then),
            colon: at,
            r#else: self.alloc(Expression::Literal(Literal::Null(Keyword { span: at, value: b"null" }))),
        })))
    }

    fn r#match(&mut self, pattern_match: &PatternMatch<'arena>) -> Option<&'arena Expression<'arena>> {
        let (mut subject, tested) = if pattern_match.arms.iter().any(|arm| !arm.is_default()) {
            let tested = self.alloc(Expression::Literal(Literal::True(Keyword {
                span: end_of(pattern_match.left_parenthesis),
                value: b"true",
            })));

            (self.arm_subject(pattern_match.expression), tested)
        } else {
            (Subject { first: None, variable: None }, self.tested(pattern_match.expression))
        };

        let mut arms = Vec::new_in(self.arena);
        for (index, arm) in pattern_match.arms.iter().enumerate() {
            arms.push(match arm {
                PatternMatchArm::Pattern(arm) => {
                    let PatternMatchArmBody::Expression(expression) = arm.body else {
                        return None;
                    };
                    let follows = arm_follows(pattern_match, index);
                    let condition = self.arm_condition(arm, follows, &mut subject)?;

                    MatchArm::Expression(MatchExpressionArm {
                        conditions: TokenSeparatedSequence::from_slices(self.arena.alloc_slice_copy(&[condition]), &[]),
                        arrow: arm.arrow,
                        expression,
                    })
                }
                PatternMatchArm::Default(arm) => {
                    let PatternMatchArmBody::Expression(expression) = arm.body else {
                        return None;
                    };

                    MatchArm::Default(MatchDefaultArm {
                        default: arm.default,
                        comma: None,
                        arrow: arm.arrow,
                        expression,
                    })
                }
            });
        }

        Some(self.alloc(Expression::Match(Match {
            r#match: pattern_match.r#match,
            left_parenthesis: pattern_match.left_parenthesis,
            expression: tested,
            right_parenthesis: pattern_match.right_parenthesis,
            left_brace: pattern_match.left_brace,
            arms: TokenSeparatedSequence::new(arms, Vec::new_in(self.arena)),
            right_brace: pattern_match.right_brace,
        })))
    }

    /// `if`, then `else if` per arm in order, then `else` for `default` wherever it is written. A `match` with only
    /// `default` runs its value as a statement when it is not a local, then the `default` arm.
    fn statement(&mut self, pattern_match: &PatternMatch<'arena>) -> Option<&'arena Statement<'arena>> {
        let mut otherwise = None;
        for arm in pattern_match.arms.iter() {
            if let PatternMatchArm::Default(arm) = arm {
                otherwise = Some((arm.default.span, self.arm_statement(&arm.body)));
            }
        }

        if !pattern_match.arms.iter().any(|arm| !arm.is_default()) {
            let (span, body) = otherwise?;
            if self.is_local_value(pattern_match.expression) {
                return Some(body);
            }

            let value = self.expression_statement(self.tested(pattern_match.expression));
            let statements = self.arena.alloc_slice_fill_iter([value.clone(), body.clone()]);

            return Some(self.arena.alloc(Statement::Block(Block {
                left_brace: start_of(pattern_match.r#match.span),
                statements: Sequence::from_slice(statements),
                right_brace: end_of(span),
            })));
        }

        let mut subject = self.arm_subject(pattern_match.expression);
        let mut branches = std::vec::Vec::new();
        for (index, arm) in pattern_match.arms.iter().enumerate() {
            if let PatternMatchArm::Pattern(arm) = arm {
                let follows = arm_follows(pattern_match, index);
                let condition = self.arm_condition(arm, follows, &mut subject)?;
                branches.push((arm.pattern.span(), condition, self.arm_statement(&arm.body)));
            }
        }

        let mut next = otherwise;
        let mut statement = None;
        for (span, condition, body) in branches.into_iter().rev() {
            let r#if = self.arena.alloc(Statement::If(If {
                r#if: Keyword { span: start_of(span), value: b"if" },
                left_parenthesis: start_of(span),
                condition,
                right_parenthesis: end_of(condition.span()),
                body: IfBody::Statement(IfStatementBody {
                    statement: body,
                    else_if_clauses: Sequence::empty(),
                    else_clause: next.map(|(span, statement)| IfStatementBodyElseClause {
                        r#else: Keyword { span: start_of(span), value: b"else" },
                        statement,
                    }),
                }),
            }));

            next = Some((span, &*r#if));
            statement = Some(&*r#if);
        }

        statement
    }

    /// An arm's pattern test, `and` its `when` condition. The `and` is the `when` keyword, so a report on the condition
    /// names `when`. The subject's read may be on the line before the pattern, and php-src gives an arm and the jump
    /// after a branch their condition's line, so the condition is parenthesized from the pattern to the arrow: the arm
    /// starts on its pattern's line, and the parentheses' span is no other node's.
    fn arm_condition(
        &mut self,
        arm: &PatternMatchPatternArm<'arena>,
        follows: Span,
        subject: &mut Subject<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        let test = self.test(arm.pattern, follows, subject)?;
        self.tests.push((arm.pattern.span(), test));
        let condition = match &arm.guard {
            Some(guard) => self.binary(test, BinaryOperator::LowAnd(guard.when), guard.condition),
            None => test,
        };

        Some(self.alloc(Expression::Parenthesized(Parenthesized {
            left_parenthesis: start_of(arm.pattern.span()),
            expression: condition,
            right_parenthesis: arm.arrow,
        })))
    }

    fn arm_statement(&self, body: &PatternMatchArmBody<'arena>) -> &'arena Statement<'arena> {
        match body {
            PatternMatchArmBody::Block(block) => self.arena.alloc(Statement::Block(block.clone())),
            PatternMatchArmBody::Expression(expression) => self.expression_statement(expression),
        }
    }

    fn expression_statement(&self, expression: &'arena Expression<'arena>) -> &'arena Statement<'arena> {
        self.arena.alloc(Statement::Expression(ExpressionStatement {
            expression,
            terminator: Terminator::Semicolon(end_of(expression.span())),
        }))
    }

    /// The PHP that is `true` when the subject matches `pattern`, reading the subject in evaluation order. `follows`
    /// is the token the pattern follows. A value's read sits at its start, so `read === value` spans more than the
    /// value: the analyzer keeps one type per span, and the value keeps its own.
    fn test(
        &mut self,
        pattern: &'arena Pattern<'arena>,
        follows: Span,
        subject: &mut Subject<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        Some(match pattern {
            // The fields and the properties read the name when there is one, and the subject otherwise. A case's
            // fields are read by the names the case declares them with, in their order.
            Pattern::Type(TypePattern { hint, fields, properties, variable }) => {
                let read = self.read(subject, pattern.span());
                let (operand, mut named) = match variable {
                    Some(variable) => {
                        let name = self.php_variable(variable.value);
                        let target = self.variable(name, start_of(read.span()));
                        let assignment = self.alloc(Expression::Assignment(Assignment {
                            lhs: target,
                            operator: AssignmentOperator::Assign(start_of(read.span())),
                            rhs: read,
                        }));

                        (assignment, Some(Subject { first: None, variable: Some(name) }))
                    }
                    None => (read, None),
                };
                let mut test = self.type_test(hint, operand)?;
                let target = named.as_mut().unwrap_or(subject);
                if let Some(fields) = fields {
                    let declared = self.names.case_fields(hint.span())?;
                    if declared.len() != fields.fields.len() {
                        return None;
                    }

                    for (index, (field, name)) in fields.fields.iter().zip(&declared).enumerate() {
                        let follows = match index.checked_sub(1).and_then(|previous| fields.fields.tokens.get(previous))
                        {
                            Some(comma) => comma.span_for(fields.left_parenthesis.file_id),
                            None => fields.left_parenthesis,
                        };
                        let name =
                            LocalIdentifier { span: start_of(field.span()), value: self.arena.alloc_slice_copy(name) };
                        let field_test = self.member_test(target, name, field, follows)?;

                        test = self.binary(test, BinaryOperator::And(start_of(field.span())), field_test);
                    }
                }

                match properties {
                    Some(properties) => self.property_tests(test, properties, target)?,
                    None => test,
                }
            }
            // `Enum.Case` names a case of an enum with data, which is an object of its class, as a type pattern does.
            Pattern::Value(Expression::Access(Access::Property(PropertyAccess {
                property: ClassLikeMemberSelector::Identifier(case),
                ..
            }))) if self.names.case_fields(case.span).is_some() => {
                let read = self.read(subject, follows);

                self.binary(
                    read,
                    BinaryOperator::Instanceof(Keyword { span: start_of(pattern.span()), value: b"instanceof" }),
                    self.alloc(Expression::Identifier(Identifier::Local(*case))),
                )
            }
            Pattern::Value(value) => {
                let read = self.read(subject, follows);

                self.binary(read, BinaryOperator::Identical(start_of(value.span())), value)
            }
            Pattern::Comparison(comparison) => {
                let read = self.read(subject, pattern.span());

                self.binary(read, comparison.operator, comparison.value)
            }
            Pattern::Not(not) => {
                let operand = self.test(not.pattern, not.not.span, subject)?;

                self.alloc(Expression::UnaryPrefix(UnaryPrefix {
                    operator: UnaryPrefixOperator::Not(not.not.span),
                    operand,
                }))
            }
            Pattern::Binary(binary) => {
                let left = self.test(binary.left, follows, subject)?;
                let right = self.test(binary.right, binary.operator.span, subject)?;
                let operator = if binary.is_and() {
                    BinaryOperator::And(binary.operator.span)
                } else {
                    BinaryOperator::Or(binary.operator.span)
                };

                self.binary(left, operator, right)
            }
            Pattern::Parenthesized(parenthesized) => {
                self.test(parenthesized.pattern, parenthesized.left_parenthesis, subject)?
            }
            Pattern::Properties(properties) => {
                let function = called_functions(Node::PropertiesPattern(properties)).next()?;
                let read = self.read(subject, pattern.span());
                let test = self.call(function, read);

                self.property_tests(test, properties, subject)?
            }
            Pattern::List(list) => self.list_test(list, subject)?,
        })
    }

    /// `\is_array(s) && \count(s) === n`, or `\count(s) >= n` with a rest, then `&&` each element's test, then the
    /// rest's `\is_array($rest = \array_slice(s, k, \count(s) - n))`, which is always true and assigns the rest of the
    /// list from its place `k`, and is `\array_slice(s, k)` for a rest at the end. The first `\is_array` is the null
    /// check of a `List?`, as `\is_object` is a properties pattern's. An element before the rest reads `s[i]` by its
    /// place, and one after it `s[\count(s) - m]`, the `m`th from the end. Each element is read at the start of its
    /// pattern, so an issue at the read is never taken for the `\is_array` at the `[`. The count each read calls is the
    /// one the binder records at the `]`.
    fn list_test(
        &mut self,
        list: &'arena ListPattern<'arena>,
        subject: &mut Subject<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        let elements = list.elements.len();
        let rest = list.elements.iter().enumerate().find_map(|(place, element)| match element {
            ListPatternElement::Rest(rest) => Some((place, rest)),
            ListPatternElement::Pattern(_) => None,
        });
        let patterns = elements - usize::from(rest.is_some());

        let mut functions = called_functions(Node::ListPattern(list));
        let (is_array, count) = (functions.next()?, functions.next()?);
        let read = self.read(subject, list.left_bracket);
        let array = self.call(is_array, read);
        let length = self.integer(patterns, start_of(list.right_bracket));
        let operator = if rest.is_some() {
            BinaryOperator::GreaterThanOrEqual(start_of(list.right_bracket))
        } else {
            BinaryOperator::Identical(start_of(list.right_bracket))
        };
        let read = self.read(subject, list.right_bracket);
        let counted = self.call(count, read);
        let counted = self.binary(counted, operator, length);
        let mut test = self.binary(array, BinaryOperator::And(start_of(list.right_bracket)), counted);

        for (place, element) in list.elements.iter().enumerate() {
            let ListPatternElement::Pattern(pattern) = element else {
                continue;
            };
            let follows = match place.checked_sub(1).and_then(|previous| list.elements.tokens.get(previous)) {
                Some(comma) => comma.span_for(list.left_bracket.file_id),
                None => list.left_bracket,
            };
            let at = start_of(pattern.span());
            let index = match rest {
                Some((rest_place, _)) if place > rest_place => {
                    let read = self.read(subject, list.right_bracket);
                    let counted = self.call(count, read);
                    let from_end = self.integer(elements - place, end_of(list.right_bracket));

                    self.binary(counted, BinaryOperator::Subtraction(end_of(list.right_bracket)), from_end)
                }
                _ => self.integer(place, at),
            };
            let array = self.read(subject, at);
            let value =
                self.alloc(Expression::ArrayAccess(ArrayAccess { array, left_bracket: at, index, right_bracket: at }));
            // The count test makes the element there, so it is read without the `?? null` of a tested index.
            let mut element = if is_named(pattern) {
                Subject { first: Some(value), variable: None }
            } else {
                self.hidden(value, "match")
            };
            let element_test = self.test(pattern, follows, &mut element)?;

            test = self.binary(test, BinaryOperator::And(start_of(follows)), element_test);
        }

        if let Some((place, rest)) = rest
            && let (Some(hint), Some(variable)) = (rest.hint, rest.variable)
        {
            let mut functions = called_functions(Node::RestPattern(rest));
            let (is_array, array_slice) = (functions.next()?, functions.next()?);
            let array = self.read(subject, hint.span());
            let mut arguments = vec![array, self.integer(place, end_of(hint.span()))];
            if place + 1 < elements {
                let read = self.read(subject, list.right_bracket);
                let counted = self.call(count, read);
                let length = self.integer(patterns, end_of(list.right_bracket));
                arguments.push(self.binary(counted, BinaryOperator::Subtraction(end_of(list.right_bracket)), length));
            }
            let slice = self.call_with(array_slice, &arguments);
            let name = self.php_variable(variable.value);
            let assignment = self.alloc(Expression::Assignment(Assignment {
                lhs: self.variable(name, start_of(rest.ellipsis)),
                operator: AssignmentOperator::Assign(start_of(rest.ellipsis)),
                rhs: slice,
            }));
            let assigned = self.call(is_array, assignment);

            test = self.binary(test, BinaryOperator::And(start_of(rest.ellipsis)), assigned);
        }

        Some(test)
    }

    /// `test`, then `&&` each property's test on the object `subject` holds, each property read once.
    fn property_tests(
        &mut self,
        mut test: &'arena Expression<'arena>,
        properties: &'arena PropertiesPattern<'arena>,
        subject: &mut Subject<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        for property in properties.properties.iter() {
            let property_test = self.member_test(subject, property.name, property.pattern, property.colon)?;

            test = self.binary(test, BinaryOperator::And(start_of(property.name.span)), property_test);
        }

        Some(test)
    }

    /// The test of the property `name` of the object `subject` holds against `pattern`, which follows the token at
    /// `follows`, reading the property once.
    fn member_test(
        &mut self,
        subject: &mut Subject<'arena>,
        name: LocalIdentifier<'arena>,
        pattern: &'arena Pattern<'arena>,
        follows: Span,
    ) -> Option<&'arena Expression<'arena>> {
        let object = self.read(subject, name.span);
        let value = self.alloc(Expression::Access(Access::Property(PropertyAccess {
            object,
            arrow: start_of(name.span),
            property: ClassLikeMemberSelector::Identifier(name),
        })));
        let mut member =
            if is_named(pattern) { Subject { first: Some(value), variable: None } } else { self.subject(value) };

        self.test(pattern, follows, &mut member)
    }

    /// `instanceof` for a class, and the scalar test for a scalar type.
    fn type_test(
        &self,
        hint: &'arena Hint<'arena>,
        operand: &'arena Expression<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        Some(match type_test_of(hint)? {
            TypeTest::Scalar(function) => self.call(function, operand),
            TypeTest::Class(class) => self.binary(
                operand,
                BinaryOperator::Instanceof(Keyword { span: start_of(hint.span()), value: b"instanceof" }),
                self.alloc(Expression::Identifier(class)),
            ),
        })
    }

    /// A tested value: itself when it is a local or a parameter, and a hidden `$match#N` otherwise.
    fn subject(&mut self, value: &'arena Expression<'arena>) -> Subject<'arena> {
        self.subject_named(value, "match")
    }

    /// The value a `match` tests. A local or a parameter is read in each arm, so its first read is the arm's too.
    fn arm_subject(&mut self, value: &'arena Expression<'arena>) -> Subject<'arena> {
        let mut subject = self.subject(value);
        if self.is_local_value(value) {
            subject.first = None;
        }

        subject
    }

    /// A tested value: itself when it is a local or a parameter, and a hidden `$<prefix>#N` otherwise.
    fn subject_named(&mut self, value: &'arena Expression<'arena>, prefix: &str) -> Subject<'arena> {
        if let Expression::ConstantAccess(access) = value
            && self.names.is_local(access.name.span())
        {
            return Subject { first: Some(value), variable: Some(self.php_variable(access.name.value())) };
        }

        let tested = self.tested(value);

        self.hidden(tested, prefix)
    }

    /// `value` in a hidden `$<prefix>#N`, which its first read assigns.
    fn hidden(&mut self, value: &'arena Expression<'arena>, prefix: &str) -> Subject<'arena> {
        self.temporaries += 1;
        let name = self.arena.alloc_fmt(format_args!("${prefix}#{}", self.temporaries)).as_bytes();
        let at = start_of(value.span());
        let variable = self.variable(name, at);
        let assignment = self.alloc(Expression::Assignment(Assignment {
            lhs: variable,
            operator: AssignmentOperator::Assign(at),
            rhs: value,
        }));

        Subject { first: Some(assignment), variable: Some(name) }
    }

    /// The PHP that reads a tested value: `x[k] ?? null` for an index, and the value itself otherwise.
    fn tested(&self, value: &'arena Expression<'arena>) -> &'arena Expression<'arena> {
        if !matches!(value.unparenthesized(), Expression::ArrayAccess(_)) {
            return value;
        }

        let at = end_of(value.span());
        let null = self.alloc(Expression::Literal(Literal::Null(Keyword { span: at, value: b"null" })));

        self.binary(value, BinaryOperator::NullCoalesce(at), null)
    }

    /// The subject's next read, at the start of the pattern node at `at` when it is not the first.
    fn read(&self, subject: &mut Subject<'arena>, at: Span) -> &'arena Expression<'arena> {
        match (subject.first.take(), subject.variable) {
            (Some(first), _) => first,
            (None, Some(name)) => self.variable(name, start_of(at)),
            (None, None) => unreachable!("a value without a variable is read by one named type pattern"),
        }
    }

    /// The PHP variable of a PHP# name.
    fn php_variable(&self, name: &[u8]) -> &'arena [u8] {
        self.arena.alloc_slice_fill_iter(b"$".iter().chain(name).copied())
    }

    /// Whether a value is a bare name of a local or a parameter.
    fn is_local_value(&self, value: &Expression<'arena>) -> bool {
        matches!(value, Expression::ConstantAccess(access) if self.names.is_local(access.name.span()))
    }

    fn call(
        &self,
        function: LocalIdentifier<'static>,
        argument: &'arena Expression<'arena>,
    ) -> &'arena Expression<'arena> {
        self.call_with(function, &[argument])
    }

    fn call_with(
        &self,
        function: LocalIdentifier<'static>,
        arguments: &[&'arena Expression<'arena>],
    ) -> &'arena Expression<'arena> {
        let end = end_of(function.span);
        let arguments = self.arena.alloc_slice_fill_iter(
            arguments.iter().map(|value| Argument::Positional(PositionalArgument { ellipsis: None, value })),
        );

        self.alloc(Expression::Call(Call::Function(FunctionCall {
            function: self.alloc(Expression::Identifier(Identifier::Local(function))),
            argument_list: ArgumentList {
                left_parenthesis: end,
                arguments: TokenSeparatedSequence::from_slices(arguments, &[]),
                right_parenthesis: end,
            },
        })))
    }

    fn integer(&self, value: usize, span: Span) -> &'arena Expression<'arena> {
        self.alloc(Expression::Literal(Literal::Integer(LiteralInteger {
            span,
            raw: self.arena.alloc_fmt(format_args!("{value}")).as_bytes(),
            value: Some(value as u64),
        })))
    }

    fn binary(
        &self,
        lhs: &'arena Expression<'arena>,
        operator: BinaryOperator<'arena>,
        rhs: &'arena Expression<'arena>,
    ) -> &'arena Expression<'arena> {
        self.alloc(Expression::Binary(Binary { lhs, operator, rhs }))
    }

    fn variable(&self, name: &'arena [u8], span: Span) -> &'arena Expression<'arena> {
        self.alloc(Expression::Variable(Variable::Direct(DirectVariable { span, name })))
    }

    fn alloc(&self, expression: Expression<'arena>) -> &'arena Expression<'arena> {
        self.arena.alloc(expression)
    }
}

/// Whether a pattern is one type pattern with a name, which holds the tested value, so the value needs no hidden
/// variable.
fn is_named(pattern: &Pattern<'_>) -> bool {
    matches!(pattern, Pattern::Type(TypePattern { variable: Some(_), .. }))
}

/// The token the pattern of the arm at `index` follows: `{` for the first arm, and the comma before it otherwise.
fn arm_follows(pattern_match: &PatternMatch<'_>, index: usize) -> Span {
    match index.checked_sub(1).and_then(|previous| pattern_match.arms.tokens.get(previous)) {
        Some(comma) => comma.span_for(pattern_match.left_brace.file_id),
        None => pattern_match.left_brace,
    }
}

fn start_of(span: Span) -> Span {
    Span::new(span.file_id, span.start, span.start)
}

fn end_of(span: Span) -> Span {
    Span::new(span.file_id, span.end, span.end)
}
