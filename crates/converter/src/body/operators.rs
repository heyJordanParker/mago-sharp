//! Operators and casts (spec sections 19, 21 and 24). An operator converts only where PHP# gives it PHP's meaning for
//! the types the analysis gives its operands, as `+` between two numbers. Anywhere else the file stays plain PHP, with
//! the operator and its operands' types named.

use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::ttype::TypeMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Construct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Literal;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;

use super::Kind;
use super::atomic_kinds;
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
            // PHP# has no `(bool)`, and a condition's comparison replaces it (spec section 24): the test of PHP's
            // truthiness that a condition of the value's type gives, as `count != 0` (spec section 21).
            UnaryPrefixOperator::BoolCast(..) | UnaryPrefixOperator::BooleanCast(..) => {
                if self.is(operand, &[Kind::Bool]) {
                    let shown = self.shown(operand);
                    return self.refuse(
                        expression.span(),
                        format!("`(bool)` of `{shown}` has no PHP# form: PHP# has no `(bool)`, and a type only PHPDoc gives is no proof that the value is a `bool` (spec section 24)"),
                    );
                }
                if let Some(test) = self.truthiness(operand, false, false) {
                    let test = if test.starts_with('(') || !test.contains(' ') { test } else { format!("({test})") };
                    self.edits.replace(expression.span(), test);
                }
                return;
            }
            UnaryPrefixOperator::ArrayCast(..)
            | UnaryPrefixOperator::ObjectCast(..)
            | UnaryPrefixOperator::UnsetCast(..)
            | UnaryPrefixOperator::VoidCast(..) => {
                return self.refuse(expression.span(), "this cast has no PHP# form (spec section 24)");
            }
            // PHP's cast aliases are its `(int)`, `(float)` and `(string)` (spec section 24).
            UnaryPrefixOperator::IntCast(span, _) | UnaryPrefixOperator::IntegerCast(span, _) => {
                return self.cast(expression, *span, operand, Kind::Int);
            }
            UnaryPrefixOperator::FloatCast(span, _)
            | UnaryPrefixOperator::DoubleCast(span, _)
            | UnaryPrefixOperator::RealCast(span, _) => return self.cast(expression, *span, operand, Kind::Float),
            UnaryPrefixOperator::StringCast(span, _) | UnaryPrefixOperator::BinaryCast(span, _) => {
                return self.cast(expression, *span, operand, Kind::String);
            }
            UnaryPrefixOperator::Not(_) => {
                // `!isset(…)` and `!empty(…)` are the tests of the opposite, since `!` binds tighter than `!=`.
                match operand {
                    Expression::Construct(Construct::Isset(isset)) => return self.isset(expression, isset, true),
                    Expression::Construct(Construct::Empty(empty)) => return self.empty(expression, empty, true),
                    _ => {}
                }
                // `!` takes a `bool`, so PHP's `!` of another type is the test of its falsiness (spec section 21). `!`
                // of a `bool` compiles to PHP's `!`, which reads any value the `bool` holds at runtime as PHP does.
                if !self.is(operand, &[Kind::Bool]) {
                    if let Some(test) = self.truthiness(operand, true, false) {
                        self.edits.replace(expression.span(), test);
                    }
                    return;
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

    /// A cast to `kind`, written at `span`, of `operand`. A PHP# cast converts between `int` and `float`, and `(string)`
    /// writes a number as text (spec section 24), so a cast of a number keeps its form, a PHP alias of it becoming it,
    /// and a cast of a value that already has `kind` changes nothing, so the value stands alone. PHP's cast of any
    /// other value, a string it parses, a `bool` or an array, has no PHP# form.
    fn cast(&mut self, expression: &Expression<'_>, span: Span, operand: &Expression<'_>, kind: Kind) {
        let written = match kind {
            Kind::Int => "(int)",
            Kind::Float => "(float)",
            _ => "(string)",
        };
        if self.untyped(operand, "a cast") {
            return;
        }
        // PHP# casts a number, and its `(int)` and `(float)` run PHP's own conversion at runtime, so they stay. A
        // `(string)` of a string leaves only where a native declaration or a literal makes it one at runtime: a type
        // only PHPDoc gives, as a framework's `@property`, is no proof of the value. A template shows any other value
        // the analysis types `string` as PHP's interpolation does, which converts it to text as `(string)` does, and
        // `null` as `""` once `?? ""` gives it (spec section 18).
        if kind == Kind::String && self.is(operand, &[Kind::String, Kind::Null]) {
            let nullable = !self.is(operand, &[Kind::String]);
            if !nullable && self.holds_type_at_runtime(operand) {
                return self.edits.delete(expression.span().start.offset, operand.span().start.offset);
            }
            let rendered = self.render(operand);
            let text = match (nullable, super::is_primary(operand)) {
                (false, _) => rendered,
                (true, true) => format!("{rendered} ?? \"\""),
                (true, false) => format!("({rendered}) ?? \"\""),
            };
            return self.edits.replace(expression.span(), format!("`${{{text}}}`"));
        }
        // PHP's `(string)` of an object calls its `__toString`, which is a plain PHP class's `toString()` (spec section
        // 18).
        if kind == Kind::String && self.plain_stringable(operand) {
            let rendered = self.render(operand);
            let rendered = if super::is_primary(operand) { rendered } else { format!("({rendered})") };
            return self.edits.replace(expression.span(), format!("{rendered}.toString()"));
        }
        if !self.is(operand, &[Kind::Int, Kind::Float]) {
            let shown = self.shown(operand);
            return self.refuse(
                expression.span(),
                format!("`{written}` of `{shown}` has no PHP# form: a PHP# cast converts a number, and `(string)` writes one as text (spec section 24)"),
            );
        }
        self.edits.replace(span, written);
    }

    /// Whether `value` holds the type the analysis gives it at runtime, as a scalar PHP# reads by that type: a literal,
    /// a constant, an expression whose operator gives that type, as a comparison gives a `bool` and `.` a string, a
    /// parameter of the method, whose PHP# type the compiled PHP declares, a local each write of which holds its type,
    /// or a property or a call whose native PHP declaration, or the converted class's own PHP# one, gives the type. A
    /// type only PHPDoc gives, as a framework's `@property`, is no proof: PHP may hold `1` where it says `bool`.
    pub(super) fn holds_type_at_runtime(&self, value: &Expression<'_>) -> bool {
        let wanted: Vec<Kind> = self
            .type_of(value)
            .map(|union| kinds(union).into_iter().filter(|kind| *kind != Kind::Null).collect())
            .unwrap_or_default();
        let native = |metadata: Option<&TypeMetadata>| {
            metadata.is_some_and(|metadata| {
                let declared = kinds(&metadata.type_union);
                !metadata.from_docblock && !wanted.is_empty() && wanted.iter().all(|kind| declared.contains(kind))
            })
        };
        let codebase = self.input.codebase;
        let current = self.class.as_ref().map(|class| class.metadata.name);
        // A method the converted class declares itself gets its PHP# return type in the PHP it compiles to.
        let declared_here = |function: &FunctionLikeMetadata| {
            current
                .and_then(|class| codebase.get_method(class.as_bytes(), function.name.as_bytes()))
                .is_some_and(|own| std::ptr::eq(own, function))
        };
        let returns = |function: Option<&FunctionLikeMetadata>| {
            function.is_some_and(|function| {
                native(function.return_type_declaration_metadata.as_ref()) || declared_here(function)
            })
        };
        match value {
            Expression::Parenthesized(parenthesized) => self.holds_type_at_runtime(parenthesized.expression),
            Expression::Literal(_)
            | Expression::CompositeString(_)
            | Expression::ConstantAccess(_)
            | Expression::Access(Access::ClassConstant(_))
            | Expression::Construct(Construct::Isset(_) | Construct::Empty(_)) => true,
            Expression::Binary(binary) => match &binary.operator {
                // Each gives its own type, whatever its operands hold.
                BinaryOperator::StringConcat(_) | BinaryOperator::Instanceof(_) => true,
                operator if operator.is_comparison() || operator.is_logical() => true,
                // `??` gives one of its sides, and an arithmetic operator a number its operands make.
                _ => self.holds_type_at_runtime(binary.lhs) && self.holds_type_at_runtime(binary.rhs),
            },
            Expression::UnaryPrefix(unary) => match &unary.operator {
                UnaryPrefixOperator::Not(_)
                | UnaryPrefixOperator::BoolCast(..)
                | UnaryPrefixOperator::BooleanCast(..)
                | UnaryPrefixOperator::IntCast(..)
                | UnaryPrefixOperator::IntegerCast(..)
                | UnaryPrefixOperator::FloatCast(..)
                | UnaryPrefixOperator::DoubleCast(..)
                | UnaryPrefixOperator::RealCast(..)
                | UnaryPrefixOperator::StringCast(..)
                | UnaryPrefixOperator::BinaryCast(..) => true,
                UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_) => {
                    self.holds_type_at_runtime(unary.operand)
                }
                _ => false,
            },
            Expression::Conditional(conditional) => {
                conditional.then.is_none_or(|then| self.holds_type_at_runtime(then))
                    && self.holds_type_at_runtime(conditional.r#else)
            }
            Expression::Variable(Variable::Direct(variable)) => {
                variable.name == b"$this"
                    || self
                        .method
                        .as_ref()
                        .is_some_and(|method| method.parameters.iter().any(|parameter| parameter == variable.name))
                    || self.local_holds_type_at_runtime(variable.name)
            }
            Expression::Access(Access::Property(PropertyAccess { object, property, .. })) => {
                let ClassLikeMemberSelector::Identifier(name) = property else {
                    return false;
                };
                let classes: Vec<&TAtomic> = self
                    .type_of(object)
                    .map(|union| union.types.iter().filter(|atomic| !atomic.is_null()).collect())
                    .unwrap_or_default();
                let [TAtomic::Object(TObject::Named(class))] = classes.as_slice() else {
                    return false;
                };
                let property = [b"$".as_slice(), name.value].concat();
                let declaring = self.input.codebase.get_declaring_property(class.name.as_bytes(), &property);
                let own = self.class.as_ref().is_some_and(|own| {
                    own.metadata.name == class.name && own.metadata.properties.contains_key(&mago_word::word(&property))
                });
                own || declaring.is_some_and(|metadata| native(metadata.type_declaration_metadata.as_ref()))
            }
            Expression::Call(Call::Method(call)) => returns(self.method_of(self.type_of(call.object), &call.method)),
            Expression::Call(Call::StaticMethod(call)) => returns(self.static_method_of(call)),
            Expression::Call(Call::Function(FunctionCall { function: Expression::Identifier(identifier), .. })) => {
                returns(self.function_of(identifier))
            }
            _ => false,
        }
    }

    /// Whether `value` is never null and each class it may be is a plain PHP class or interface that declares or
    /// inherits `__toString`, which PHP's `(string)`, `.` and interpolation call, and which PHP# reads as the class's
    /// `toString()` (spec section 18).
    pub(super) fn plain_stringable(&self, value: &Expression<'_>) -> bool {
        let codebase = self.input.codebase;
        self.type_of(value).is_some_and(|union| {
            !union.types.is_empty()
                && union.types.iter().all(|atomic| {
                    matches!(atomic, TAtomic::Object(TObject::Named(named))
                        if codebase
                            .get_declaring_method(named.name.as_bytes(), b"__tostring")
                            .is_some_and(|method| !method.flags.is_sharp()))
                })
        })
    }

    /// The PHP# string literal of the class name `X::class` gives, for `X` a class's name or `self`, which name the
    /// class where PHP compiles the code. `static::class` names the class PHP called, which only the running code knows.
    fn class_name_text(&self, expression: &Expression<'_>) -> Option<String> {
        let Expression::Access(Access::ClassConstant(access)) = expression else {
            return None;
        };
        let ClassLikeConstantSelector::Identifier(constant) = &access.constant else {
            return None;
        };
        if !constant.value.eq_ignore_ascii_case(b"class")
            || !matches!(access.class, Expression::Identifier(_) | Expression::Self_(_))
        {
            return None;
        }
        let [TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Literal { value }))] =
            self.type_of(expression)?.types.as_ref()
        else {
            return None;
        };

        Some(format!("\"{}\"", value.as_str_lossy().replace('\\', "\\\\")))
    }

    /// Whether `side` is `Order::class` of a class that takes no type arguments, `typeof(Order)`.
    fn names_plain_class(&self, side: &Expression<'_>) -> bool {
        if self.class_name_text(side).is_none() {
            return false;
        }
        let Some([TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Literal { value }))]) =
            self.type_of(side).map(|union| union.types.as_ref())
        else {
            return false;
        };

        self.input.codebase.get_class_like(value.as_bytes()).is_some_and(|class| class.template_types.is_empty())
    }

    /// Whether `side`, a class value, may be the class of an object of a PHP# generic class, as `Class.of(box)` is,
    /// which carries the object's type arguments, so PHP#'s `==` with `typeof(Box)` is false for a `Box<int>`, where
    /// PHP's `===` compares names (spec section 25). `Box::class` is `typeof(Box)`, which carries none.
    fn may_carry_type_arguments(&self, side: &Expression<'_>) -> bool {
        if self.class_name_text(side).is_some() {
            return false;
        }
        let Some(union) = self.type_of(side) else {
            return false;
        };
        let codebase = self.input.codebase;
        // Whether a PHP# generic class an object can be of is `class` or extends or implements it, or is any class for
        // `None`.
        let generic_under = |class: Option<&[u8]>| {
            codebase.class_likes.values().any(|candidate| {
                candidate.flags.is_sharp()
                    && candidate.kind.is_class()
                    && !candidate.flags.is_abstract()
                    && !candidate.template_types.is_empty()
                    && class.is_none_or(|class| codebase.is_instance_of(candidate.name.as_bytes(), class))
            })
        };
        let object_may = |atomic: &TAtomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => generic_under(Some(object.name.as_bytes())),
            TAtomic::Null | TAtomic::Scalar(_) => false,
            _ => generic_under(None),
        };

        union.types.iter().any(|atomic| match atomic {
            TAtomic::Scalar(TScalar::ClassLikeString(class)) => match class {
                TClassLikeString::Literal { .. } => false,
                TClassLikeString::OfType { constraint, .. } => constraint.types.iter().any(object_may),
                TClassLikeString::Generic { constraint, .. } => object_may(constraint),
                TClassLikeString::Any { .. } => generic_under(None),
            },
            _ => false,
        })
    }

    pub(super) fn binary(&mut self, expression: &Expression<'_>, binary: &Binary<'_>) {
        let (lhs, rhs) = (binary.lhs, binary.rhs);
        let numbers = [Kind::Int, Kind::Float];
        let operator = String::from_utf8_lossy(self.text(binary.operator.span())).into_owned();
        let refusal = match &binary.operator {
            BinaryOperator::StringConcat(_) => return self.concatenation(expression),
            // `self` names the class that declares the method. `static` names the class PHP called, which PHP#'s `Self`
            // names only as a return type.
            BinaryOperator::Instanceof(keyword) => {
                if !matches!(rhs, Expression::Identifier(_) | Expression::Self_(_)) {
                    return self.refuse(
                        expression.span(),
                        "`instanceof` of a class value or of `static` has no PHP# form: `is` tests a type the code names, which a class value is not, and `Self` names the class PHP called only as a return type (spec sections 21 and 25)",
                    );
                }
                self.edits.replace(keyword.span, "is");
                self.expression(rhs, None);
                return self.expression(lhs, None);
            }
            BinaryOperator::Identical(span) | BinaryOperator::NotIdentical(span) => {
                let equal = matches!(binary.operator, BinaryOperator::Identical(_));
                // `Order::class` is the string of the class's name, which PHP compiles in, so a string compares with
                // that text, `"Shop\\Order"`, as PHP's `===` compares two strings (spec section 25).
                for (named, other) in [(lhs, rhs), (rhs, lhs)] {
                    if let Some(text) = self.class_name_text(named)
                        && self.is(other, &[Kind::String, Kind::Null])
                        && !self.is(other, &[Kind::Null])
                    {
                        self.edits.replace(named.span(), text);
                        self.edits.replace(*span, if equal { "==" } else { "!=" });
                        return self.expression(other, None);
                    }
                }
                match identity(self.type_of(lhs), self.type_of(rhs)) {
                    Ok(true) => {
                        // A class value equals `typeof(Order)` of a class that takes no type arguments just when it
                        // names that class, as PHP's `===` of names does, whatever arguments it carries.
                        let names_plain_class = [lhs, rhs].into_iter().any(|side| self.names_plain_class(side));
                        if !names_plain_class
                            && let Some(side) = [lhs, rhs].into_iter().find(|side| self.may_carry_type_arguments(side))
                        {
                            let shown = self.shown(side);
                            return self.refuse(
                                expression.span(),
                                format!("`===` of `{shown}` has no PHP# form: it may be the class of an object of a PHP# generic class, which carries the object's type arguments, as a `Box<int>`'s does, where PHP's `::class` is the name alone (spec section 25)"),
                            );
                        }
                        self.edits.replace(*span, if equal { "==" } else { "!=" });
                        None
                    }
                    Ok(false) => None,
                    Err(reason) => Some(reason),
                }
            }
            BinaryOperator::Equal(span) | BinaryOperator::NotEqual(span) | BinaryOperator::AngledNotEqual(span) => {
                if self.loose(lhs, rhs) {
                    let equal = matches!(binary.operator, BinaryOperator::Equal(_));
                    self.edits.replace(*span, if equal { "==" } else { "!=" });
                    None
                } else {
                    Some(self.operands(&operator, lhs, rhs, "compares PHP's loose way, which PHP#'s `==` gives only between numbers, `bool`s, enums, or strings one of which PHP never reads as a number (spec section 19)"))
                }
            }
            // PHP's `and`, `or` and `xor` bind looser than `=`, so each side keeps its own parentheses.
            BinaryOperator::LowXor(span) => {
                if !self.is(lhs, &[Kind::Bool]) || !self.is(rhs, &[Kind::Bool]) {
                    Some(self.operands(
                        &operator,
                        lhs,
                        rhs,
                        "has no PHP# form: `xor` is removed, and `!=` gives it between two `bool`s (spec section 21)",
                    ))
                } else {
                    if !self.proven(lhs) || !self.proven(rhs) {
                        return;
                    }
                    self.edits.replace(span.span, "!=");
                    for side in [lhs, rhs] {
                        self.grouped(side);
                        self.expression(side, None);
                    }
                    return;
                }
            }
            BinaryOperator::LowAnd(span) | BinaryOperator::LowOr(span) => {
                let and = matches!(binary.operator, BinaryOperator::LowAnd(_));
                self.edits.replace(span.span, if and { "&&" } else { "||" });
                for side in [lhs, rhs] {
                    self.grouped(side);
                    self.condition(side, false);
                }
                return;
            }
            // `&&` and `||` take a `bool`, which each side's truthiness gives (spec section 21).
            BinaryOperator::And(_) | BinaryOperator::Or(_) => {
                self.condition(lhs, false);
                return self.condition(rhs, false);
            }
            // A key that may be missing is read behind `??`, which PHP# allows on a `Map` (spec section 12).
            BinaryOperator::NullCoalesce(_) if let Expression::ArrayAccess(access) = lhs => {
                self.untyped(access.array, "an index");
                self.untyped(access.index, "a key");
                self.numeric_key(access.array, access.index);
                // A `List` read past its end throws, and `getOrNull(i)` gives null there, as PHP's `??` reads it.
                if self.is(access.array, &[Kind::List]) && self.is(access.index, &[Kind::Int]) {
                    let list = self.render(access.array);
                    let list = if super::is_primary(access.array) { list } else { format!("({list})") };
                    let index = self.render(access.index);
                    self.edits.replace(lhs.span(), format!("{list}.getOrNull({index})"));
                }
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

    /// Puts `side` of a `&&`, `||` or `!=` that was PHP's `and`, `or` or `xor` in parentheses, unless nothing in it
    /// binds looser than those, so it keeps the grouping PHP's looser operator gave it.
    fn grouped(&mut self, side: &Expression<'_>) {
        if matches!(side, Expression::Assignment(_) | Expression::Binary(_) | Expression::Conditional(_)) {
            let span = side.span();
            self.edits.insert(span.start.offset, "(");
            self.edits.insert(span.end.offset, ")");
        }
    }

    /// Whether PHP's loose `==` between `lhs` and `rhs` gives what PHP#'s `==` gives: between two numbers, two `bool`s
    /// or two enums, none of them null, and between two strings when one is a literal PHP never reads as a number,
    /// since PHP compares two numeric strings as numbers (spec section 19).
    fn loose(&self, lhs: &Expression<'_>, rhs: &Expression<'_>) -> bool {
        let (Some(left), Some(right)) = (self.type_of(lhs), self.type_of(rhs)) else {
            return false;
        };
        let (left, right) = (kinds(left), kinds(right));
        let numbers =
            |kinds: &[Kind]| !kinds.is_empty() && kinds.iter().all(|kind| matches!(kind, Kind::Int | Kind::Float));
        let never_numeric = |side: &Expression<'_>| {
            matches!(side, Expression::Literal(Literal::String(string)) if string.value.is_some_and(|value| {
                value.iter().any(|byte| !(byte.is_ascii_digit() || byte.is_ascii_whitespace() || b"+-.eE".contains(byte)))
                    || value.is_empty()
            }))
        };

        match (left.as_slice(), right.as_slice()) {
            _ if numbers(&left) && numbers(&right) => true,
            ([Kind::Bool], [Kind::Bool]) | ([Kind::Enum], [Kind::Enum]) => true,
            ([Kind::String], [Kind::String]) => never_numeric(lhs) || never_numeric(rhs),
            _ => false,
        }
    }

    /// Why `operator` between `lhs` and `rhs` keeps the file plain PHP: `reason`, after the types of both sides.
    pub(super) fn operands(&self, operator: &str, lhs: &Expression<'_>, rhs: &Expression<'_>, reason: &str) -> String {
        format!("`{operator}` between `{}` and `{}` {reason}", self.shown(lhs), self.shown(rhs))
    }
}

/// Whether PHP's `===` between values of `lhs` and `rhs` is PHP#'s `==`, `Ok(true)`, stays `===`, `Ok(false)`, or has
/// no form. `==` gives `===`'s answer when both sides have one scalar type, both are enums, or one side is `null`.
/// `===` stays where a side may hold an object, since PHP#'s `===` asks for the same object (spec section 19).
pub(super) fn identity(lhs: Option<&TUnion>, rhs: Option<&TUnion>) -> Result<bool, String> {
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
    // An `Any?` compared with `==` against a string, a `bool` or an enum is equal only when it holds the same type and
    // value, as PHP's `===` (spec section 19). Against a number it compares an `int` with a `float` by value, and
    // PHP#'s `===` compares two objects only, so any other value of no known type refuses.
    let mixed = |union: &TUnion| union.types.iter().any(TAtomic::is_mixed);
    let exact = |kinds: &[Kind]| {
        !kinds.is_empty()
            && kinds.iter().all(|kind| matches!(kind, Kind::String | Kind::Bool | Kind::Enum | Kind::Null))
    };
    if (mixed(lhs) && !mixed(rhs) && exact(&right)) || (mixed(rhs) && !mixed(lhs) && exact(&left)) {
        return Ok(true);
    }
    if mixed(lhs) || mixed(rhs) {
        return Err(format!(
            "`===` between `{}` and `{}` has no PHP# form: PHP#'s `===` compares two objects, and an `Any?` compares an `int` with a `float` by value, so narrow the value with `is` first (spec section 19)",
            lhs.get_id(),
            rhs.get_id()
        ));
    }
    if left.contains(&Kind::Object) || right.contains(&Kind::Object) {
        return Ok(false);
    }
    // A class value is the class's name, and PHP#'s `==` of two compares the names as PHP's `===` does, since a class
    // value PHP gives carries no type arguments (spec section 25). A string is no class value in PHP#.
    if left.contains(&Kind::Class) || right.contains(&Kind::Class) {
        let class = |kinds: &[Kind]| kinds.iter().all(|kind| matches!(kind, Kind::Class | Kind::Null));
        return if class(&left) && class(&right) {
            Ok(true)
        } else {
            Err(format!(
                "`===` between `{}` and `{}` has no PHP# form: a PHP# class value equals only a class value, and a string is none (spec section 25)",
                lhs.get_id(),
                rhs.get_id()
            ))
        };
    }
    // A resource, an `array{…}` shape or `never` has no PHP# type to compare.
    if let Some(other) = lhs.types.iter().chain(rhs.types.iter()).find(|atomic| atomic_kinds(atomic) == [Kind::Other]) {
        return Err(format!(
            "`===` between `{}` and `{}` has no PHP# form: PHP# has no type `{}`",
            lhs.get_id(),
            rhs.get_id(),
            other.get_id()
        ));
    }
    if collection(&left) || collection(&right) {
        return Err(
            "PHP's `===` on arrays compares keys, order and types, and PHP#'s `==` on collections compares their elements (spec section 19)"
                .to_owned(),
        );
    }
    let either = |kind: Kind| left.contains(&kind) || right.contains(&kind);
    if either(Kind::Int) && either(Kind::Float) {
        return Err("PHP's `1 === 1.0` is false, and PHP# compares an `int` with a `float` by value (spec section 12)"
            .to_owned());
    }
    // What is left is two scalar types, or unions of them, as `string|false` and `false`, between which PHP#'s `==` is
    // PHP's `===`: equal when both hold the same type and value. Two sides that share no type never compare in PHP#.
    if left.iter().any(|kind| *kind != Kind::Null && right.contains(kind)) {
        Ok(true)
    } else {
        Err(format!(
            "`===` between `{}` and `{}` is always false, and PHP# has no `==` between types that share no value (spec section 19)",
            lhs.get_id(),
            rhs.get_id()
        ))
    }
}
