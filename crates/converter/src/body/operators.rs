//! Operators and casts (spec sections 19, 21 and 24). An operator converts only where PHP# gives it PHP's meaning for
//! the types the analysis gives its operands, as `+` between two numbers. Anywhere else the file stays plain PHP, with
//! the operator and its operands' types named.

use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::UnaryPrefixOperator;

use super::Kind;
use super::kinds;
use crate::Converter;

impl Converter<'_, '_> {
    pub(super) fn unary(
        &mut self,
        expression: &Expression<'_>,
        operator: &UnaryPrefixOperator<'_>,
        operand: &Expression<'_>,
    ) {
        let numbers = [Kind::Int, Kind::Float];
        match operator {
            UnaryPrefixOperator::ErrorControl(_) => {
                return self.refuse(
                    expression.span(),
                    "the `@` silence operator has no PHP# form: `@` marks a keyword used as a name (spec section 2)",
                );
            }
            UnaryPrefixOperator::Reference(_) => {
                return self
                    .refuse(expression.span(), "a reference has no PHP# form: PHP# passes every value as it is");
            }
            UnaryPrefixOperator::BoolCast(..)
            | UnaryPrefixOperator::BooleanCast(..)
            | UnaryPrefixOperator::ArrayCast(..)
            | UnaryPrefixOperator::ObjectCast(..)
            | UnaryPrefixOperator::UnsetCast(..)
            | UnaryPrefixOperator::VoidCast(..) => {
                return self.refuse(expression.span(), "this cast has no PHP# form (spec section 24)");
            }
            // PHP's cast aliases are its `(int)`, `(float)` and `(string)` (spec section 24).
            UnaryPrefixOperator::IntegerCast(span, _) => {
                self.edits.replace(*span, "(int)");
                self.untyped(operand, "a cast");
                return;
            }
            UnaryPrefixOperator::DoubleCast(span, _) | UnaryPrefixOperator::RealCast(span, _) => {
                self.edits.replace(*span, "(float)");
                self.untyped(operand, "a cast");
                return;
            }
            UnaryPrefixOperator::BinaryCast(span, _) => {
                self.edits.replace(*span, "(string)");
                self.untyped(operand, "a cast");
                return;
            }
            UnaryPrefixOperator::IntCast(..)
            | UnaryPrefixOperator::FloatCast(..)
            | UnaryPrefixOperator::StringCast(..) => {
                self.untyped(operand, "a cast");
                return;
            }
            UnaryPrefixOperator::Not(_) => {
                if !self.is(operand, &[Kind::Bool]) {
                    let shown = self.shown(operand);
                    return self.refuse(
                        expression.span(),
                        format!(
                            "`!` on `{shown}` reads PHP's truthiness, which is not converted yet: `!` takes a `bool`"
                        ),
                    );
                }
                // `!is_string($x)` and `!$x instanceof A` are `x is not string` and `x is not A` (spec section 21).
                if let Some((value, tested)) = self.test(operand) {
                    return self.edits.replace(expression.span(), format!("{value} is not {tested}"));
                }
            }
            UnaryPrefixOperator::BitwiseNot(_) if !self.is(operand, &[Kind::Int]) => {
                let shown = self.shown(operand);
                return self
                    .refuse(expression.span(), format!("`~` on `{shown}` has no PHP# form: `~` takes an `int`"));
            }
            UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => {
                if !self.is(operand, &numbers) {
                    let shown = self.shown(operand);
                    return self.refuse(
                        expression.span(),
                        format!("`++` and `--` on `{shown}` step PHP's way, which PHP# does only for a number"),
                    );
                }
                return self.written(operand);
            }
            UnaryPrefixOperator::Plus(_) | UnaryPrefixOperator::Negation(_) if !self.is(operand, &numbers) => {
                let shown = self.shown(operand);
                return self.refuse(
                    expression.span(),
                    format!("a sign on `{shown}` turns it into a number PHP's way, which PHP# does only for a number"),
                );
            }
            _ => {}
        }
        self.expression(operand, None);
    }

    pub(super) fn binary(&mut self, expression: &Expression<'_>, binary: &Binary<'_>) {
        let (lhs, rhs) = (binary.lhs, binary.rhs);
        let numbers = [Kind::Int, Kind::Float];
        let operator = String::from_utf8_lossy(self.text(binary.operator.span())).into_owned();
        let refusal = match &binary.operator {
            BinaryOperator::StringConcat(_) => return self.concatenation(expression),
            BinaryOperator::Instanceof(keyword) => {
                let Expression::Identifier(class) = rhs else {
                    return self.refuse(expression.span(), "`instanceof` of a class value is not converted yet");
                };
                self.edits.replace(keyword.span, "is");
                self.rename_class(class);
                return self.expression(lhs, None);
            }
            BinaryOperator::Identical(span) | BinaryOperator::NotIdentical(span) => {
                match identity(self.type_of(lhs), self.type_of(rhs)) {
                    Ok(true) => {
                        let equal = matches!(binary.operator, BinaryOperator::Identical(_));
                        self.edits.replace(*span, if equal { "==" } else { "!=" });
                        None
                    }
                    Ok(false) => None,
                    Err(reason) => Some(reason),
                }
            }
            BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => {
                Some(format!("PHP's loose `{operator}` is not converted yet"))
            }
            BinaryOperator::LowAnd(_) | BinaryOperator::LowOr(_) | BinaryOperator::LowXor(_) => {
                Some(format!("`{operator}` is removed: PHP# writes `&&` and `||` (spec section 21)"))
            }
            BinaryOperator::And(_) | BinaryOperator::Or(_) => (!self.is(lhs, &[Kind::Bool])
                || !self.is(rhs, &[Kind::Bool]))
            .then(|| self.operands(&operator, lhs, rhs, "reads PHP's truthiness, which is not converted yet")),
            // A key that may be missing is read behind `??`, which PHP# allows on a `Map` (spec section 12).
            BinaryOperator::NullCoalesce(_) if let Expression::ArrayAccess(access) = lhs => {
                self.untyped(access.array, "an index");
                self.untyped(access.index, "a key");
                return self.expression(rhs, None);
            }
            BinaryOperator::NullCoalesce(_) => None,
            BinaryOperator::Addition(_) | BinaryOperator::Subtraction(_) | BinaryOperator::Multiplication(_) => {
                (!self.is(lhs, &numbers) || !self.is(rhs, &numbers)).then(|| {
                    self.operands(
                        &operator,
                        lhs,
                        rhs,
                        "turns them into numbers PHP's way, which PHP# does only between numbers",
                    )
                })
            }
            BinaryOperator::Division(_) => {
                if !self.is(lhs, &numbers) || !self.is(rhs, &numbers) {
                    Some(self.operands(
                        &operator,
                        lhs,
                        rhs,
                        "turns them into numbers PHP's way, which PHP# does only between numbers",
                    ))
                } else if self.is(lhs, &[Kind::Int]) && self.is(rhs, &[Kind::Int]) {
                    Some("PHP's `/` of two `int`s gives a `float` when it doesn't divide evenly, and PHP#'s truncates (spec section 24)".to_owned())
                } else {
                    None
                }
            }
            BinaryOperator::Modulo(_)
            | BinaryOperator::BitwiseAnd(_)
            | BinaryOperator::BitwiseOr(_)
            | BinaryOperator::BitwiseXor(_)
            | BinaryOperator::LeftShift(_)
            | BinaryOperator::RightShift(_) => {
                (!self.is(lhs, &[Kind::Int]) || !self.is(rhs, &[Kind::Int])).then(|| {
                    self.operands(
                        &operator,
                        lhs,
                        rhs,
                        "has PHP's meaning in PHP# only between `int`s (spec section 19)",
                    )
                })
            }
            BinaryOperator::Exponentiation(_) => {
                let float = self.is(lhs, &[Kind::Float]) || self.is(rhs, &[Kind::Float]);
                let non_negative = matches!(rhs, Expression::Literal(Literal::Integer(_)));
                (!self.is(lhs, &numbers) || !self.is(rhs, &numbers) || !(float || non_negative)).then(|| {
                    self.operands(&operator, lhs, rhs, "has PHP's meaning in PHP# only with a `float`, or an `int` power written as a number (spec section 24)")
                })
            }
            BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_)
            | BinaryOperator::Spaceship(_) => (!self.is(lhs, &numbers) || !self.is(rhs, &numbers)).then(|| {
                self.operands(
                    &operator,
                    lhs,
                    rhs,
                    "compares PHP's way, which PHP# does only between numbers (spec section 19)",
                )
            }),
        };
        if let Some(reason) = refusal {
            return self.refuse(expression.span(), reason);
        }
        self.expression(lhs, None);
        self.expression(rhs, None);
    }

    /// Why `operator` between `lhs` and `rhs` keeps the file plain PHP: `reason`, after the types of both sides.
    pub(super) fn operands(&self, operator: &str, lhs: &Expression<'_>, rhs: &Expression<'_>, reason: &str) -> String {
        format!("`{operator}` between `{}` and `{}` {reason}", self.shown(lhs), self.shown(rhs))
    }
}

/// Whether PHP's `===` between values of `lhs` and `rhs` is PHP#'s `==`, `Ok(true)`, stays `===`, `Ok(false)`, or has
/// no form. `==` gives `===`'s answer when both sides have one scalar type, both are enums, or one side is `null`.
/// `===` stays where a side may hold an object, since PHP#'s `===` asks for the same object (spec section 19).
fn identity(lhs: Option<&TUnion>, rhs: Option<&TUnion>) -> Result<bool, String> {
    let (Some(lhs), Some(rhs)) = (lhs, rhs) else {
        return Err("the analysis gives a side of this `===` no type".to_owned());
    };
    let (left, right) = (kinds(lhs), kinds(rhs));
    let collection =
        |kinds: &[Kind]| kinds.iter().any(|kind| matches!(kind, Kind::List | Kind::Map | Kind::Collection));
    // `x == null` is PHP#'s null check, which runs no code of `x`'s type (spec section 19).
    if left == [Kind::Null] || right == [Kind::Null] {
        return Ok(true);
    }
    // PHP#'s `===` compares two objects only, and a value of no known type may hold anything.
    if [lhs, rhs].iter().any(|union| union.types.iter().any(TAtomic::is_mixed)) {
        return Err(format!(
            "`===` between `{}` and `{}` is not converted yet: PHP#'s `===` compares two objects, so narrow the value with `is` first (spec section 19)",
            lhs.get_id(),
            rhs.get_id()
        ));
    }
    if left.contains(&Kind::Object) || right.contains(&Kind::Object) {
        return Ok(false);
    }
    if collection(&left) || collection(&right) {
        return Err(
            "PHP's `===` on arrays compares keys, order and types, and PHP#'s `==` on collections compares their elements (spec section 19)"
                .to_owned(),
        );
    }
    let bare = |kinds: &[Kind]| kinds.iter().copied().filter(|kind| *kind != Kind::Null).collect::<Vec<_>>();
    match (bare(&left).as_slice(), bare(&right).as_slice()) {
        ([Kind::Int], [Kind::Float]) | ([Kind::Float], [Kind::Int]) => {
            Err("PHP's `1 === 1.0` is false, and PHP# compares an `int` with a `float` by value (spec section 12)"
                .to_owned())
        }
        ([a], [b]) if a == b && *a != Kind::Other => Ok(true),
        _ => Err(format!("`===` between `{}` and `{}` is not converted yet", lhs.get_id(), rhs.get_id())),
    }
}
