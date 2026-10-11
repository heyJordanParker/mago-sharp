//! The PHP each PHP# pattern form runs as.
//!
//! The analyzer analyzes this PHP and the engine's bridge lowers it, so the code the analyzer narrows on is the code
//! that runs. Spec section 21 defines the forms:
//!
//! - `x is Calc` is `x instanceof Calc`, and `x is int` is `is_int(x)`, with `is_float`, `is_string` and `is_bool`.
//! - `x is Calc c` is `($c = x) instanceof Calc`, so `c` holds the value whether or not it matches, and the binder
//!   keeps it in scope only where it matches.
//! - `x is 200` is `x === 200`, `x is < 10` is `x < 10`, and `x is limit` is `x === limit` when `limit` is a local.
//! - `not`, `and` and `or` are `!`, `&&` and `||`.
//! - `x is { total: > 0 }` is `is_object(x) && x->total > 0`.
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
use crate::cst::As;
use crate::cst::Assignment;
use crate::cst::AssignmentOperator;
use crate::cst::Binary;
use crate::cst::BinaryOperator;
use crate::cst::Block;
use crate::cst::Call;
use crate::cst::ClassLikeMemberSelector;
use crate::cst::Conditional;
use crate::cst::ConstantAccess;
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
use crate::cst::Literal;
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
use crate::cst::PropertyAccess;
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

/// Builds, in `arena`, the PHP a PHP# `is`, `as` or `match` runs as.
///
/// Returns `None` for any other node, and for a form the slice refuses, such as a nullable type pattern.
/// `is_local` says whether the bare name at a span is a local or a parameter. `temporaries` is how many hidden
/// variables the forms around this one hold, so the new ones are numbered after them.
pub fn php_shape<'arena, A>(
    arena: &'arena A,
    node: Node<'_, 'arena>,
    temporaries: u32,
    is_local: &dyn Fn(Span) -> bool,
) -> Option<PhpShape<'arena>>
where
    A: Arena,
{
    let mut shape = Shape { arena, is_local, temporaries, first: temporaries, tests: std::vec::Vec::new() };
    let php = match node {
        Node::Expression(Expression::Is(is)) => Node::Expression(shape.is(is)?),
        Node::Expression(Expression::As(r#as)) => Node::Expression(shape.r#as(r#as)?),
        Node::Expression(Expression::PatternMatch(pattern_match)) => Node::Expression(shape.r#match(pattern_match)?),
        Node::Statement(Statement::PatternMatch(pattern_match)) => Node::Statement(shape.statement(pattern_match)?),
        _ => return None,
    };

    Some(PhpShape { php, temporaries: shape.temporaries - shape.first, tests: shape.tests })
}

/// The PHP function a pattern node calls to test a value's type, as the identifier its PHP calls.
///
/// It is `is_int`, `is_float`, `is_string` or `is_bool` at a scalar type of a type pattern or `as`, and `is_object` at
/// the `{` of a properties pattern. The binder records each as the name resolved at that span, as it records a call's
/// name.
#[must_use]
pub fn called_function(node: Node<'_, '_>) -> Option<LocalIdentifier<'static>> {
    match node {
        Node::TypePattern(type_pattern) => scalar_test(&type_pattern.hint),
        Node::As(r#as) => scalar_test(r#as.hint),
        Node::PropertiesPattern(properties) => {
            Some(LocalIdentifier { span: properties.left_brace, value: b"is_object" })
        }
        _ => None,
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
    is_local: &'shape dyn Fn(Span) -> bool,
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
            // Reading (a): a bare name is a local's value when one is in scope, and a type otherwise.
            Pattern::Type(TypePattern { hint: Hint::Identifier(name @ Identifier::Local(local)), variable: None })
                if (self.is_local)(local.span) =>
            {
                let value = self.alloc(Expression::ConstantAccess(ConstantAccess { name: *name }));
                let read = self.read(subject, follows);

                self.binary(read, BinaryOperator::Identical(start_of(local.span)), value)
            }
            Pattern::Type(TypePattern { hint, variable }) => {
                let read = self.read(subject, pattern.span());
                let operand = match variable {
                    Some(variable) => {
                        let name = self.php_variable(variable.value);
                        let target = self.variable(name, start_of(read.span()));

                        self.alloc(Expression::Assignment(Assignment {
                            lhs: target,
                            operator: AssignmentOperator::Assign(start_of(read.span())),
                            rhs: read,
                        }))
                    }
                    None => read,
                };

                self.type_test(hint, operand)?
            }
            // A list or enum case pattern, which the parser refuses as not supported yet.
            Pattern::Value(Expression::Error(_)) => return None,
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
                let function = called_function(Node::PropertiesPattern(properties))?;
                let read = self.read(subject, pattern.span());
                let mut test = self.call(function, read);
                for property in properties.properties.iter() {
                    let at = start_of(property.name.span);
                    let object = self.read(subject, property.name.span);
                    let value = self.alloc(Expression::Access(Access::Property(PropertyAccess {
                        object,
                        arrow: at,
                        property: ClassLikeMemberSelector::Identifier(property.name),
                    })));
                    let mut property_subject = if is_named(property.pattern) {
                        Subject { first: Some(value), variable: None }
                    } else {
                        self.subject(value)
                    };
                    let property_test = self.test(property.pattern, property.colon, &mut property_subject)?;

                    test = self.binary(test, BinaryOperator::And(at), property_test);
                }

                test
            }
        })
    }

    /// `instanceof` for a class, and the scalar test for a scalar type.
    fn type_test(
        &self,
        hint: &'arena Hint<'arena>,
        operand: &'arena Expression<'arena>,
    ) -> Option<&'arena Expression<'arena>> {
        if let Some(function) = scalar_test(hint) {
            return Some(self.call(function, operand));
        }

        let Hint::Identifier(class) = hint else {
            return None;
        };
        let class = self.alloc(Expression::Identifier(*class));

        Some(self.binary(
            operand,
            BinaryOperator::Instanceof(Keyword { span: start_of(hint.span()), value: b"instanceof" }),
            class,
        ))
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
            && (self.is_local)(access.name.span())
        {
            return Subject { first: Some(value), variable: Some(self.php_variable(access.name.value())) };
        }

        self.temporaries += 1;
        let name = self.arena.alloc_fmt(format_args!("${prefix}#{}", self.temporaries)).as_bytes();
        let at = start_of(value.span());
        let variable = self.variable(name, at);
        let assignment = self.alloc(Expression::Assignment(Assignment {
            lhs: variable,
            operator: AssignmentOperator::Assign(at),
            rhs: self.tested(value),
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
        matches!(value, Expression::ConstantAccess(access) if (self.is_local)(access.name.span()))
    }

    fn call(
        &self,
        function: LocalIdentifier<'static>,
        argument: &'arena Expression<'arena>,
    ) -> &'arena Expression<'arena> {
        let end = end_of(function.span);
        let arguments = self
            .arena
            .alloc_slice_fill_iter([Argument::Positional(PositionalArgument { ellipsis: None, value: argument })]);

        self.alloc(Expression::Call(Call::Function(FunctionCall {
            function: self.alloc(Expression::Identifier(Identifier::Local(function))),
            argument_list: ArgumentList {
                left_parenthesis: end,
                arguments: TokenSeparatedSequence::from_slices(arguments, &[]),
                right_parenthesis: end,
            },
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
