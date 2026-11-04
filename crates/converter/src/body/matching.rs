//! `match` (spec section 21). A PHP arm compares the value with `===`, and a PHP# pattern takes types and constants
//! only, so an arm's constants become patterns joined by `or`, each passing the `===` rule of section 19.

use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Match;
use mago_syntax::cst::MatchArm;

use super::Kind;
use super::operators::identity;
use crate::Converter;

impl Converter<'_, '_> {
    /// `match`, whose arms take blocks when it stands as a statement, as a PHP# `match` statement's do.
    pub(super) fn match_expression(&mut self, r#match: &Match<'_>, statement: bool) {
        let subject = r#match.expression;
        if !r#match.arms.iter().any(MatchArm::is_default) && !self.is(subject, &[Kind::Enum]) {
            let shown = self.shown(subject);
            return self.refuse(
                r#match.r#match.span,
                format!("a `match` on `{shown}` with no `default` arm is not converted yet: PHP throws `UnhandledMatchError` where no arm fits, and a PHP# `match` on a value that is no enum has a `default` arm (spec section 21)"),
            );
        }

        self.expression(subject, None);
        for arm in r#match.arms.iter() {
            let value = match arm {
                MatchArm::Expression(arm) => {
                    for condition in arm.conditions.iter() {
                        self.pattern(subject, condition);
                    }
                    // `a, b =>` is `a or b =>`, and a comma after the last condition goes.
                    let count = arm.conditions.len();
                    for (index, token) in arm.conditions.tokens.iter().enumerate() {
                        let start = token.start.offset;
                        let end = start + u32::try_from(token.value.len()).unwrap_or(1);
                        if index + 1 < count {
                            self.edits.replace_range(start, end, " or");
                        } else {
                            self.edits.delete(start, end);
                        }
                    }
                    arm.expression
                }
                MatchArm::Default(arm) => {
                    if let Some(comma) = arm.comma {
                        self.edits.delete(comma.start.offset, comma.end.offset);
                    }
                    arm.expression
                }
            };
            if statement {
                let span = value.span();
                self.edits.insert(span.start.offset, "{ ");
                self.edits.insert(span.end.offset, "; }");
            }
            self.expression(value, None);
        }
    }

    /// One of an arm's conditions, which PHP compares with the value using `===`, as a constant pattern.
    fn pattern(&mut self, subject: &Expression<'_>, condition: &Expression<'_>) {
        let constant = matches!(
            condition,
            Expression::Literal(_) | Expression::ConstantAccess(_) | Expression::Access(Access::ClassConstant(_))
        );
        if !constant {
            return self.refuse(
                condition.span(),
                "a `match` arm on a value that is no constant is not converted yet: a PHP# pattern takes types and constants only (spec section 21)",
            );
        }
        if !matches!(identity(self.type_of(subject), self.type_of(condition)), Ok(true)) {
            let (compared, constant) = (self.shown(subject), self.shown(condition));
            return self.refuse(
                condition.span(),
                format!("a `match` arm compares `{compared}` with `{constant}` by PHP's `===`, which a PHP# pattern gives only between one scalar type, enums, or `null` (spec sections 19 and 21)"),
            );
        }
        self.expression(condition, None);
    }
}
