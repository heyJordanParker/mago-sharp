//! A method's or law's body as the statements of a Lean `do` block in `M`.
//!
//! Every value that can throw is bound to a temporary `«$n»` before it is used, as A-normal form binds it, so the
//! effects run in the order PHP# runs them and every other term is pure. A branch that runs only on one side of a
//! condition is a nested `do` block, so its effects run only there.

use foldhash::HashSet;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::DeclarationKind;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Literal;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_word::ascii_lowercase_word;

use crate::issues;
use crate::reach::Reach;
use crate::translate::Unmodeled;
use crate::translate::Use;
use crate::translate::names;

/// What a value of a PHP# type is in Lean, as far as an operator cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Int,
    Bool,
    String,
    Float,
    Enum,
    Object,
    Null,
    Other,
}

/// One branch of a choice: its statements, the last of which gives the branch's value.
type Branch = Vec<String>;

/// The translation of one body: its `do` statements, the declarations it uses, and each construct it cannot model.
pub(crate) struct Body<'body, 'program> {
    checked: &'body CheckedProgram<'program>,
    reach: &'body Reach,
    /// The fully qualified PHP name of the class the body belongs to.
    class: &'body [u8],
    /// The body as messages name it: `Money.add`.
    place: String,
    pub(crate) uses: Vec<Use>,
    pub(crate) unmodeled: Vec<Unmodeled>,
    lines: Vec<String>,
    temporaries: u32,
    /// Each local whose type takes `null`, by where its declaration's name starts.
    nullable_locals: HashSet<u32>,
}

impl<'body, 'program> Body<'body, 'program> {
    pub(crate) fn new(
        checked: &'body CheckedProgram<'program>,
        reach: &'body Reach,
        class: &'body [u8],
        place: String,
    ) -> Self {
        Self {
            checked,
            reach,
            class,
            place,
            uses: Vec::new(),
            unmodeled: Vec::new(),
            lines: Vec::new(),
            temporaries: 0,
            nullable_locals: HashSet::default(),
        }
    }

    /// The Lean type of `hint`.
    pub(crate) fn r#type(&mut self, hint: &Hint) -> String {
        match hint {
            Hint::Integer(_) => "int".to_owned(),
            Hint::Bool(_) => "Bool".to_owned(),
            Hint::String(_) => "string".to_owned(),
            Hint::Void(_) => "Unit".to_owned(),
            Hint::Float(_) => self.unmodeled(hint.span(), "a float".to_owned(), issues::FLOAT),
            Hint::Nullable(nullable) => format!("(Option {})", self.r#type(nullable.hint)),
            Hint::Parenthesized(parenthesized) => self.r#type(parenthesized.hint),
            Hint::Self_(_) => self.class_type(hint.span(), self.class),
            Hint::Identifier(identifier) => {
                let class = self.checked.names().get(identifier);

                self.class_type(hint.span(), class)
            }
            Hint::Generic(generic) => {
                // The name resolver leaves the built-in collections' names as written.
                let name = self.checked.names().resolve(&generic.name).unwrap_or(generic.name.value);
                if is_collection(name) {
                    self.unmodeled(hint.span(), "a collection".to_owned(), issues::COLLECTION)
                } else {
                    self.class_type(hint.span(), name)
                }
            }
            Hint::Function(_) => self.unmodeled(hint.span(), "a function type".to_owned(), issues::LAMBDA),
            Hint::Mixed(_) => self.unmodeled(hint.span(), "Any".to_owned(), issues::TYPE_TEST),
            Hint::Iterable(_) => self.unmodeled(hint.span(), "Iterable".to_owned(), issues::LAZY),
            _ => {
                self.unmodeled(hint.span(), format!("the type `{}`", self.source(hint.span())), issues::NOT_TRANSLATED)
            }
        }
    }

    /// The Lean type of a declaration's `hint`, which PHP# requires on every parameter and property; `span` is the
    /// declaration's.
    pub(crate) fn declared_type(&mut self, hint: Option<&Hint>, span: Span) -> String {
        match hint {
            Some(hint) => self.r#type(hint),
            None => self.unmodeled(span, "a declaration without a type".to_owned(), issues::NOT_TRANSLATED),
        }
    }

    /// Marks the parameter whose name starts at `offset` as one whose type takes `null`.
    pub(crate) fn nullable_parameter(&mut self, offset: u32) {
        self.nullable_locals.insert(offset);
    }

    /// The `do` statements of an expression body, which returns the expression's value.
    pub(crate) fn expression_body(mut self, expression: &Expression) -> (Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        let value = self.value(expression);
        self.lines.push(format!("return {value}"));

        (self.lines, self.uses, self.unmodeled)
    }

    /// The `do` statements of a block body. A body that may end without `return` ends in `pure ()`.
    pub(crate) fn block_body(mut self, block: &Block, void: bool) -> (Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        for statement in &block.statements {
            self.statement(statement);
        }
        if void || self.lines.is_empty() {
            self.lines.push("pure ()".to_owned());
        }

        (self.lines, self.uses, self.unmodeled)
    }

    /// The term of a constant's value, which Lean defines without `M`, so it must not throw.
    pub(crate) fn pure_value(&mut self, expression: &Expression) -> String {
        self.pure(expression, "a constant whose value can throw").unwrap_or_else(|| "default".to_owned())
    }

    /// The body's statements so far, the declarations it uses, and each construct it cannot model.
    pub(crate) fn finish(self) -> (Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        (self.lines, self.uses, self.unmodeled)
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Block(block) => {
                for statement in &block.statements {
                    self.statement(statement);
                }
            }
            Statement::Return(r#return) => {
                let value = r#return.value.map_or_else(|| "()".to_owned(), |value| self.value(value));
                self.lines.push(format!("return {value}"));
            }
            Statement::LocalDeclaration(local) => {
                let value = self.value(local.value);
                if self.checked.types().expression_type(local.value).is_nullable() {
                    self.nullable_locals.insert(local.name.span.start.offset);
                }
                let keyword = if local.is_const() { "let" } else { "let mut" };
                self.lines.push(format!("{keyword} {} := {value}", names::identifier(local.name.value)));
            }
            Statement::Expression(statement) => match statement.expression {
                Expression::Assignment(assignment) => self.assignment(assignment),
                expression => {
                    self.value(expression);
                }
            },
            Statement::If(r#if) => {
                let IfBody::Statement(body) = &r#if.body else {
                    self.unmodeled(r#if.span(), "a colon-delimited if".to_owned(), issues::NOT_TRANSLATED);
                    return;
                };
                let condition = self.value(r#if.condition);
                let then = self.nested_statement(body.statement);
                self.lines.push(format!("if {condition} then"));
                self.lines.extend(indented(&then, 2));
                if let Some(clause) = &body.else_clause {
                    let otherwise = self.nested_statement(clause.statement);
                    self.lines.push("else".to_owned());
                    self.lines.extend(indented(&otherwise, 2));
                }
            }
            Statement::For(_)
            | Statement::ForOf(_)
            | Statement::Foreach(_)
            | Statement::While(_)
            | Statement::DoWhile(_)
            | Statement::Break(_)
            | Statement::Continue(_) => {
                self.unmodeled(statement.span(), "a loop".to_owned(), issues::LOOP);
            }
            Statement::Try(_) => {
                self.unmodeled(statement.span(), "a try/catch".to_owned(), issues::TRY_CATCH);
            }
            Statement::PatternMatch(_) => {
                self.unmodeled(statement.span(), "a match statement".to_owned(), issues::NOT_TRANSLATED);
            }
            _ => {
                self.unmodeled(
                    statement.span(),
                    format!("the statement `{}`", self.source(statement.span())),
                    issues::NOT_TRANSLATED,
                );
            }
        }
    }

    /// An assignment statement: a local takes a new value. Lean's `do` reassigns a `let mut` local.
    fn assignment(&mut self, assignment: &Assignment) {
        let Expression::ConstantAccess(target) = assignment.lhs else {
            let what =
                if matches!(assignment.lhs, Expression::Access(_)) { "a property write" } else { "an assignment" };
            self.unmodeled(assignment.lhs.span(), what.to_owned(), issues::PROPERTY_WRITE);
            return;
        };
        if !matches!(self.checked.names().binding(&target.name), Some(Binding::Local(_))) {
            self.unmodeled(assignment.lhs.span(), "an assignment".to_owned(), issues::NOT_TRANSLATED);
            return;
        }

        let local = names::identifier(target.name.value());
        let value = self.value(assignment.rhs);
        let kind = self.kind(self.checked.types().expression_type(assignment.rhs));
        let assigned = match (&assignment.operator, kind) {
            (AssignmentOperator::Assign(_), _) => value,
            (AssignmentOperator::Addition(_), Kind::String) => format!("({local} ++ {value})"),
            (AssignmentOperator::Addition(_), Kind::Int) => self.bind(format!("int.add {local} {value}")),
            (AssignmentOperator::Subtraction(_), Kind::Int) => self.bind(format!("int.sub {local} {value}")),
            (AssignmentOperator::Multiplication(_), Kind::Int) => self.bind(format!("int.mul {local} {value}")),
            (AssignmentOperator::Division(_), Kind::Int) => self.bind(format!("int.div {local} {value}")),
            (AssignmentOperator::Modulo(_), Kind::Int) => self.bind(format!("int.mod {local} {value}")),
            (_, Kind::Float) => self.unmodeled(assignment.span(), "a float".to_owned(), issues::FLOAT),
            (operator, _) => {
                let what = format!("the operator `{}`", self.source(operator.span()));
                self.unmodeled(assignment.span(), what, issues::NOT_TRANSLATED)
            }
        };
        self.lines.push(format!("{local} := {assigned}"));
    }

    /// The pure term of `expression`, after binding each effect it has.
    pub(crate) fn value(&mut self, expression: &Expression) -> String {
        match expression {
            Expression::Parenthesized(parenthesized) => self.value(parenthesized.expression),
            Expression::Literal(literal) => self.literal(literal),
            Expression::ConstantAccess(name) => self.name(expression, name),
            Expression::Binary(binary) => self.binary(binary),
            Expression::UnaryPrefix(unary) => self.prefix(unary),
            Expression::Conditional(conditional) => match conditional.then {
                Some(then) => {
                    let condition = self.value(conditional.condition);
                    let then = self.branch(then);
                    let otherwise = self.branch(conditional.r#else);

                    self.choose(vec![(condition, then)], otherwise)
                }
                None => self.unmodeled(expression.span(), "the operator `?:`".to_owned(), issues::NOT_TRANSLATED),
            },
            Expression::PatternMatch(r#match) => self.pattern_match(r#match),
            Expression::Call(Call::Method(call)) => self.method_call(expression, call),
            Expression::Call(Call::NullSafeMethod(call)) => self.null_safe_call(expression, call),
            Expression::Call(Call::Function(call)) => {
                let what = format!("the plain PHP function `{}`", self.source(call.function.span()));
                self.unmodeled(expression.span(), what, issues::PLAIN_PHP)
            }
            Expression::Instantiation(instantiation) => self.instantiation(instantiation),
            Expression::Access(Access::Property(access)) => self.property(expression, access),
            Expression::Access(Access::NullSafeProperty(access)) => self.null_safe_property(expression, access),
            Expression::Array(_)
            | Expression::LegacyArray(_)
            | Expression::List(_)
            | Expression::ArrayAccess(_)
            | Expression::ArrayAppend(_) => {
                self.unmodeled(expression.span(), "a collection".to_owned(), issues::COLLECTION)
            }
            Expression::Closure(_) | Expression::ArrowFunction(_) | Expression::PartialApplication(_) => {
                self.unmodeled(expression.span(), "a lambda".to_owned(), issues::LAMBDA)
            }
            Expression::Is(_) | Expression::As(_) | Expression::TypeOf(_) => {
                self.unmodeled(expression.span(), "a type test".to_owned(), issues::TYPE_TEST)
            }
            Expression::Yield(_) => self.unmodeled(expression.span(), "a lazy sequence".to_owned(), issues::LAZY),
            Expression::Throw(_) => self.unmodeled(expression.span(), "a throw".to_owned(), issues::NOT_TRANSLATED),
            Expression::CompositeString(_) => {
                self.unmodeled(expression.span(), "a string template".to_owned(), issues::NOT_TRANSLATED)
            }
            Expression::Assignment(_) => self.unmodeled(
                expression.span(),
                "an assignment inside an expression".to_owned(),
                issues::NOT_TRANSLATED,
            ),
            _ => {
                let what = format!("the expression `{}`", self.source(expression.span()));
                self.unmodeled(expression.span(), what, issues::NOT_TRANSLATED)
            }
        }
    }

    fn literal(&mut self, literal: &Literal) -> String {
        match literal {
            Literal::String(string) => names::string(string.value.unwrap_or_default()),
            Literal::Integer(integer) => match integer.value.and_then(|value| i64::try_from(value).ok()) {
                Some(value) => format!("({value} : int)"),
                None => self.unmodeled(literal.span(), "a float".to_owned(), issues::FLOAT),
            },
            Literal::Float(_) => self.unmodeled(literal.span(), "a float".to_owned(), issues::FLOAT),
            Literal::True(_) => "true".to_owned(),
            Literal::False(_) => "false".to_owned(),
            Literal::Null(_) => "none".to_owned(),
        }
    }

    /// A bare name: a local, `this`, or a member of `this`.
    fn name(&mut self, expression: &Expression, name: &ConstantAccess) -> String {
        match self.checked.names().binding(&name.name) {
            Some(Binding::Local(local)) => {
                let lean = names::identifier(name.name.value());
                if self.nullable_locals.contains(&local.declaration.start.offset)
                    && !self.checked.types().expression_type(expression).is_nullable()
                {
                    format!("({lean}.getD default)")
                } else {
                    lean
                }
            }
            Some(Binding::This) => "this".to_owned(),
            Some(Binding::Member) => self.field("this".to_owned(), self.class, name.name.value(), expression),
            _ => {
                let what = format!("the constant `{}`", self.source(name.name.span()));
                self.unmodeled(expression.span(), what, issues::NOT_TRANSLATED)
            }
        }
    }

    fn binary(&mut self, binary: &Binary) -> String {
        match binary.operator {
            BinaryOperator::And(_) | BinaryOperator::LowAnd(_) => return self.short_circuit(binary, true),
            BinaryOperator::Or(_) => return self.short_circuit(binary, false),
            BinaryOperator::NullCoalesce(_) => return self.coalesce(binary),
            _ => {}
        }

        let lhs = self.value(binary.lhs);
        let rhs = self.value(binary.rhs);
        let types = self.checked.types();
        let kinds = (self.kind(types.expression_type(binary.lhs)), self.kind(types.expression_type(binary.rhs)));
        if kinds.0 == Kind::Float || kinds.1 == Kind::Float {
            return self.unmodeled(binary.span(), "a float".to_owned(), issues::FLOAT);
        }

        let operator = self.source(binary.operator.span());
        let ints = kinds == (Kind::Int, Kind::Int);
        let ordered = ints || kinds == (Kind::String, Kind::String);
        match binary.operator {
            BinaryOperator::Addition(_) if ints => self.bind(format!("int.add {lhs} {rhs}")),
            BinaryOperator::Addition(_) if kinds == (Kind::String, Kind::String) => format!("({lhs} ++ {rhs})"),
            BinaryOperator::Subtraction(_) if ints => self.bind(format!("int.sub {lhs} {rhs}")),
            BinaryOperator::Multiplication(_) if ints => self.bind(format!("int.mul {lhs} {rhs}")),
            BinaryOperator::Division(_) if ints => self.bind(format!("int.div {lhs} {rhs}")),
            BinaryOperator::Modulo(_) if ints => self.bind(format!("int.mod {lhs} {rhs}")),
            BinaryOperator::Identical(_) | BinaryOperator::NotIdentical(_)
                if kinds.0 == Kind::Object || kinds.1 == Kind::Object =>
            {
                self.unmodeled(binary.span(), format!("the identity test `{operator}`"), issues::IDENTITY)
            }
            BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)
                if kinds.0 == Kind::Object || kinds.1 == Kind::Object =>
            {
                self.unmodeled(binary.span(), format!("the class operator `{operator}`"), issues::NOT_TRANSLATED)
            }
            BinaryOperator::Equal(_) | BinaryOperator::Identical(_) => format!("({lhs} == {rhs})"),
            BinaryOperator::NotEqual(_) | BinaryOperator::NotIdentical(_) => format!("({lhs} != {rhs})"),
            BinaryOperator::LessThan(_) if ordered => format!("(decide ({lhs} < {rhs}))"),
            BinaryOperator::LessThanOrEqual(_) if ordered => format!("(decide ({lhs} <= {rhs}))"),
            BinaryOperator::GreaterThan(_) if ordered => format!("(decide ({rhs} < {lhs}))"),
            BinaryOperator::GreaterThanOrEqual(_) if ordered => format!("(decide ({rhs} <= {lhs}))"),
            _ => self.unmodeled(binary.span(), format!("the operator `{operator}` here"), issues::NOT_TRANSLATED),
        }
    }

    /// `&&` and `||`, which run their right side only when the left one does not decide.
    fn short_circuit(&mut self, binary: &Binary, and: bool) -> String {
        let lhs = self.value(binary.lhs);
        let rhs = self.branch(binary.rhs);
        let operator = if and { "&&" } else { "||" };
        if let [only] = rhs.as_slice()
            && let Some(rhs) = only.strip_prefix("pure ")
        {
            return format!("({lhs} {operator} {rhs})");
        }

        if and {
            self.choose(vec![(lhs, rhs)], vec!["pure false".to_owned()])
        } else {
            self.choose(vec![(lhs, vec!["pure true".to_owned()])], rhs)
        }
    }

    /// `a ?? b`, which runs `b` only when `a` is null.
    fn coalesce(&mut self, binary: &Binary) -> String {
        let lhs = self.value(binary.lhs);
        let keeps_null = self.checked.types().expression_type(binary.rhs).is_nullable();
        let rhs = self.branch(binary.rhs);
        let present = if keeps_null { lhs.clone() } else { format!("({lhs}.getD default)") };

        self.choose(vec![(format!("{lhs}.isSome"), vec![format!("pure {present}")])], rhs)
    }

    fn prefix(&mut self, unary: &UnaryPrefix) -> String {
        match &unary.operator {
            UnaryPrefixOperator::Negation(_) => {
                if let Expression::Literal(Literal::Integer(integer)) = unary.operand
                    && let Some(value) = integer.value.and_then(|value| i64::try_from(value).ok())
                {
                    return format!("({} : int)", -value);
                }
                match self.kind(self.checked.types().expression_type(unary.operand)) {
                    Kind::Int => {
                        let operand = self.value(unary.operand);
                        self.bind(format!("int.neg {operand}"))
                    }
                    Kind::Float => self.unmodeled(unary.span(), "a float".to_owned(), issues::FLOAT),
                    _ => self.unmodeled(unary.span(), "the operator `-` here".to_owned(), issues::NOT_TRANSLATED),
                }
            }
            UnaryPrefixOperator::Not(_) => {
                let operand = self.value(unary.operand);
                format!("(!{operand})")
            }
            UnaryPrefixOperator::Plus(_) => self.value(unary.operand),
            operator => {
                let what = format!("the operator `{}`", self.source(operator.span()));
                self.unmodeled(unary.span(), what, issues::NOT_TRANSLATED)
            }
        }
    }

    /// A `match` tests its subject against each arm's pattern and guard in order, and throws when no arm matches.
    fn pattern_match(&mut self, r#match: &PatternMatch) -> String {
        let kind = self.kind(self.checked.types().expression_type(r#match.expression));
        match kind {
            Kind::Float => return self.unmodeled(r#match.span(), "a float".to_owned(), issues::FLOAT),
            Kind::Object | Kind::Other => {
                return self.unmodeled(r#match.span(), "a match on this subject".to_owned(), issues::NOT_TRANSLATED);
            }
            Kind::Int | Kind::Bool | Kind::String | Kind::Enum | Kind::Null => {}
        }

        let subject = self.value(r#match.expression);
        let mut arms = Vec::new();
        let mut otherwise = vec!["throw Throwable.UnhandledMatchError".to_owned()];
        for arm in r#match.arms.iter() {
            let body = match arm {
                PatternMatchArm::Pattern(arm) => &arm.body,
                PatternMatchArm::Default(arm) => &arm.body,
            };
            let PatternMatchArmBody::Expression(body) = body else {
                return self.unmodeled(arm.span(), "a match arm with a block".to_owned(), issues::NOT_TRANSLATED);
            };
            let PatternMatchArm::Pattern(arm) = arm else {
                otherwise = self.branch(body);
                continue;
            };

            let Some(mut condition) = self.pattern(&subject, kind, arm.pattern) else {
                return "default".to_owned();
            };
            if let Some(guard) = &arm.guard {
                let Some(guard) = self.pure(guard.condition, "a guard that can throw") else {
                    return "default".to_owned();
                };
                condition = format!("({condition} && {guard})");
            }
            arms.push((condition, self.branch(body)));
        }

        self.choose(arms, otherwise)
    }

    /// The condition that `subject`, a value of `kind`, matches `pattern`, or `None` after recording why it has none.
    fn pattern(&mut self, subject: &str, kind: Kind, pattern: &Pattern) -> Option<String> {
        match pattern {
            Pattern::Value(value) => Some(format!("({subject} == {})", self.pure(value, "a pattern that can throw")?)),
            Pattern::Comparison(comparison) => {
                let value = self.pure(comparison.value, "a pattern that can throw")?;
                let condition = match comparison.operator {
                    BinaryOperator::Equal(_) | BinaryOperator::Identical(_) => format!("({subject} == {value})"),
                    BinaryOperator::NotEqual(_) | BinaryOperator::NotIdentical(_) => {
                        format!("({subject} != {value})")
                    }
                    BinaryOperator::LessThan(_) if kind == Kind::Int => format!("(decide ({subject} < {value}))"),
                    BinaryOperator::LessThanOrEqual(_) if kind == Kind::Int => {
                        format!("(decide ({subject} <= {value}))")
                    }
                    BinaryOperator::GreaterThan(_) if kind == Kind::Int => format!("(decide ({value} < {subject}))"),
                    BinaryOperator::GreaterThanOrEqual(_) if kind == Kind::Int => {
                        format!("(decide ({value} <= {subject}))")
                    }
                    _ => {
                        let what = format!("the pattern `{}`", self.source(comparison.span()));
                        self.unmodeled(comparison.span(), what, issues::NOT_TRANSLATED);
                        return None;
                    }
                };

                Some(condition)
            }
            Pattern::Not(not) => Some(format!("(!{})", self.pattern(subject, kind, not.pattern)?)),
            Pattern::Binary(binary) => {
                let left = self.pattern(subject, kind, binary.left)?;
                let right = self.pattern(subject, kind, binary.right)?;

                Some(format!("({left} {} {right})", if binary.is_and() { "&&" } else { "||" }))
            }
            Pattern::Parenthesized(parenthesized) => self.pattern(subject, kind, parenthesized.pattern),
            Pattern::Type(_) | Pattern::Properties(_) => {
                self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
                None
            }
        }
    }

    /// The term of `expression`, which must not throw, or `None` after recording `what` when it can.
    fn pure(&mut self, expression: &Expression, what: &str) -> Option<String> {
        let (lines, value) = self.nested(|body| body.value(expression));
        if lines.is_empty() {
            return Some(value);
        }

        self.unmodeled(expression.span(), what.to_owned(), issues::NOT_TRANSLATED);
        None
    }

    /// `object.m(…)`, `Class.m(…)` and `Self.m(…)`.
    fn method_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let names = self.checked.names();
        if let Some(class) = names.static_call_class(call) {
            let class = names.get(&class.name);
            if !self.is_translated_class(call.object.span(), class) {
                return "default".to_owned();
            }
            let target = self.checked.types().call_target(expression);
            if target.kind != DeclarationKind::StaticMethod {
                return self.unmodeled(expression.span(), "a call through a class value".to_owned(), issues::TYPE_TEST);
            }

            let Some(method) = self.method_name(&call.method) else {
                return "default".to_owned();
            };

            return self.call(expression, target.class.as_bytes(), class, method, None, &call.argument_list);
        }
        match call.object {
            Expression::Self_(_) => {
                let Some(method) = self.method_name(&call.method) else {
                    return "default".to_owned();
                };
                let target = self.checked.types().member_declaration(self.class, method);
                if target.kind != DeclarationKind::StaticMethod {
                    return self.unmodeled(expression.span(), "a call through `Self`".to_owned(), issues::OVERRIDABLE);
                }

                return self.call(expression, target.class.as_bytes(), self.class, method, None, &call.argument_list);
            }
            Expression::Parent(_) | Expression::Static(_) => {
                return self.unmodeled(
                    expression.span(),
                    "a call to a parent's method".to_owned(),
                    issues::INHERITANCE,
                );
            }
            _ => {}
        }

        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        match target.kind {
            DeclarationKind::Method { overridable: false } => {}
            DeclarationKind::Method { overridable: true } => {
                return self.unmodeled(
                    expression.span(),
                    format!("a call to the overridable method {}.{}", names::short_name(class), target.name),
                    issues::OVERRIDABLE,
                );
            }
            _ => return self.unmodeled(expression.span(), "a call of a function value".to_owned(), issues::LAMBDA),
        }

        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let receiver = self.value(call.object);
        self.call(expression, target.class.as_bytes(), class, method, Some(receiver), &call.argument_list)
    }

    /// `object?.m(…)`, which calls `m` only when `object` is not null.
    fn null_safe_call(&mut self, expression: &Expression, call: &NullSafeMethodCall) -> String {
        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        if target.kind != (DeclarationKind::Method { overridable: false }) {
            return self.unmodeled(
                expression.span(),
                format!("a call to the overridable method {}.{}", names::short_name(class), target.name),
                issues::OVERRIDABLE,
            );
        }

        let receiver = self.value(call.object);
        let returns_null = self
            .reach
            .method(target.class.as_bytes(), target.name.as_bytes())
            .is_some_and(|method| method.returns_nullable);
        let present = self.nested_lines(|body| {
            let called = body.call(
                expression,
                target.class.as_bytes(),
                class,
                method,
                Some(format!("({receiver}.getD default)")),
                &call.argument_list,
            );
            if returns_null { called } else { format!("(some {called})") }
        });

        self.choose(vec![(format!("{receiver}.isSome"), present)], vec!["pure none".to_owned()])
    }

    /// The name a member selector spells, or `None` after recording a name computed while the code runs.
    fn method_name<'arena>(&mut self, selector: &ClassLikeMemberSelector<'arena>) -> Option<&'arena [u8]> {
        if let ClassLikeMemberSelector::Identifier(method) = selector {
            return Some(method.value);
        }

        self.unmodeled(selector.span(), "a dynamic member name".to_owned(), issues::NOT_TRANSLATED);
        None
    }

    /// A call of `class`'s method `method`, which `declaring` declares, on `receiver`, or of the static method when
    /// `receiver` is `None`.
    fn call(
        &mut self,
        expression: &Expression,
        declaring: &[u8],
        class: &[u8],
        method: &[u8],
        receiver: Option<String>,
        arguments: &ArgumentList,
    ) -> String {
        if ascii_lowercase_word(declaring) != ascii_lowercase_word(class) {
            return self.unmodeled(
                expression.span(),
                format!("the inherited method {}.{}", names::short_name(class), String::from_utf8_lossy(method)),
                issues::INHERITANCE,
            );
        }
        let Some(facts) = self.reach.method(class, method) else {
            return self.unmodeled(
                expression.span(),
                format!("the method {}.{}", names::short_name(class), String::from_utf8_lossy(method)),
                issues::NOT_TRANSLATED,
            );
        };

        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), facts.name.as_bytes());
        let mut parts = vec![key.clone()];
        parts.extend(receiver);
        let Some(arguments) = self.arguments(arguments, facts.parameters) else {
            return "default".to_owned();
        };
        parts.extend(arguments);
        self.uses(class, &key, format!("{}.{}", names::short_name(&class_name), facts.name));

        self.bind(parts.join(" "))
    }

    /// `new Class(…)`, which runs the class's constructor.
    fn instantiation(&mut self, instantiation: &Instantiation) -> String {
        let class = match instantiation.class {
            Expression::Identifier(identifier) => self.checked.names().get(identifier),
            Expression::Self_(_) => self.class,
            class => return self.unmodeled(class.span(), "a class value".to_owned(), issues::TYPE_TEST),
        };
        if !self.is_translated_class(instantiation.class.span(), class) {
            return "default".to_owned();
        }

        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), b"__construct");
        let parameters = self.reach.method(class, b"__construct").map_or(0, |method| method.parameters);
        let mut parts = vec![key.clone()];
        if let Some(arguments) = &instantiation.argument_list {
            let Some(arguments) = self.arguments(arguments, parameters) else {
                return "default".to_owned();
            };
            parts.extend(arguments);
        }
        self.uses(class, &key, format!("new {}", names::short_name(&class_name)));

        self.bind(parts.join(" "))
    }

    /// The terms of a call's arguments, or `None` when one cannot be translated. Each argument is passed by position,
    /// and the call passes every parameter.
    fn arguments(&mut self, list: &ArgumentList, parameters: usize) -> Option<Vec<String>> {
        let mut arguments = Vec::new();
        for argument in list.arguments.iter() {
            match argument {
                Argument::Positional(argument) if argument.ellipsis.is_none() => {
                    arguments.push(self.value(argument.value));
                }
                argument => {
                    self.unmodeled(argument.span(), "a named or spread argument".to_owned(), issues::NOT_TRANSLATED);
                    return None;
                }
            }
        }
        if arguments.len() < parameters {
            self.unmodeled(list.span(), "a default argument".to_owned(), issues::NOT_TRANSLATED);
            return None;
        }

        Some(arguments)
    }

    /// `object.y`, `Class.CASE` and `Class.CONSTANT`.
    fn property(&mut self, expression: &Expression, access: &PropertyAccess) -> String {
        let ClassLikeMemberSelector::Identifier(member) = &access.property else {
            return self.unmodeled(access.property.span(), "a dynamic member name".to_owned(), issues::NOT_TRANSLATED);
        };
        let names = self.checked.names();
        if let Some(class) = names.static_property_class(access) {
            let class = names.get(&class.name);
            if !self.is_translated_class(access.object.span(), class) {
                return "default".to_owned();
            }
            let class_name = self.reached_name(class);
            let lean = names::full_name(&class_name);
            let short = names::short_name(&class_name);

            return match self.checked.types().member_declaration(class, member.value).kind {
                DeclarationKind::EnumCase => {
                    self.uses(class, &lean, short);
                    names::member(&lean, member.value)
                }
                DeclarationKind::Constant => {
                    let key = names::member(&lean, member.value);
                    self.uses(class, &key, format!("{short}.{}", String::from_utf8_lossy(member.value)));
                    key
                }
                DeclarationKind::StaticProperty => self.unmodeled(
                    expression.span(),
                    format!("the static property {short}.{}", String::from_utf8_lossy(member.value)),
                    issues::STATIC_PROPERTY,
                ),
                _ => self.unmodeled(expression.span(), "a method value".to_owned(), issues::LAMBDA),
            };
        }

        let Some(class) = self.receiver_class(access.object) else {
            return "default".to_owned();
        };
        let object = self.value(access.object);

        self.field(object, class, member.value, expression)
    }

    /// The read of `class`'s property `property` from `object`: a field of the object's structure, or a backed enum's
    /// `value`.
    fn field(&mut self, object: String, class: &[u8], property: &[u8], expression: &Expression) -> String {
        let class_name = self.reached_name(class);
        let short = names::short_name(&class_name);
        let place = format!("{short}.{}", String::from_utf8_lossy(property));
        let declaration = self.checked.types().member_declaration(class, property);
        if matches!(self.checked.types().class_declaration(class).kind, DeclarationKind::Enum { backed: true })
            && property == b"value"
        {
            let key = names::member(&names::full_name(&class_name), b"value");
            self.uses(class, &key, place);
            return format!("({key} {object})");
        }
        if !matches!(declaration.kind, DeclarationKind::Property { .. }) {
            return self.unmodeled(expression.span(), format!("the member {place}"), issues::NOT_TRANSLATED);
        }
        if ascii_lowercase_word(declaration.class.as_bytes()) != ascii_lowercase_word(class) {
            return self.unmodeled(expression.span(), format!("the inherited property {place}"), issues::INHERITANCE);
        }
        if !self.reach.is_stored_field(class, property) {
            return self.unmodeled(expression.span(), format!("the accessor of {place}"), issues::ACCESSOR);
        }

        self.uses(class, &names::full_name(&class_name), short);
        let read = format!("{object}.{}", names::identifier(property));
        if self.reach.is_nullable_field(class, property)
            && !self.checked.types().expression_type(expression).is_nullable()
        {
            format!("({read}.getD default)")
        } else {
            read
        }
    }

    /// `object?.y`, null when `object` is.
    fn null_safe_property(&mut self, expression: &Expression, access: &NullSafePropertyAccess) -> String {
        let ClassLikeMemberSelector::Identifier(member) = &access.property else {
            return self.unmodeled(access.property.span(), "a dynamic member name".to_owned(), issues::NOT_TRANSLATED);
        };
        let Some(class) = self.receiver_class(access.object) else {
            return "default".to_owned();
        };
        let object = self.value(access.object);
        let read = self.field("present".to_owned(), class, member.value, expression);
        let combine = if self.reach.is_nullable_field(class, member.value) { "bind" } else { "map" };

        format!("({object}.{combine} (fun present => {read}))")
    }

    /// The one PHP# class a receiver's value is an instance of, `null` aside, or `None` after recording why it is not.
    fn receiver_class(&mut self, object: &Expression) -> Option<&'program [u8]> {
        let r#type = self.checked.types().expression_type(object);
        let mut classes = Vec::new();
        for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
            match atomic {
                TAtomic::Object(TObject::Named(named)) => classes.push(named.name.as_bytes()),
                TAtomic::Object(TObject::Enum(r#enum)) => classes.push(r#enum.name.as_bytes()),
                TAtomic::Array(TArray::List(_) | TArray::Keyed(_)) => {
                    self.unmodeled(object.span(), "a collection".to_owned(), issues::COLLECTION);
                    return None;
                }
                TAtomic::Scalar(TScalar::ClassLikeString(_)) => {
                    self.unmodeled(object.span(), "a class value".to_owned(), issues::TYPE_TEST);
                    return None;
                }
                TAtomic::Scalar(_) => {
                    self.unmodeled(
                        object.span(),
                        "a standard library method of a built-in type".to_owned(),
                        issues::LIBRARY,
                    );
                    return None;
                }
                _ => {
                    self.unmodeled(
                        object.span(),
                        format!("a value of the type `{}`", r#type.get_id()),
                        issues::NOT_TRANSLATED,
                    );
                    return None;
                }
            }
        }

        let [class] = classes.as_slice() else {
            self.unmodeled(object.span(), format!("a value of the type `{}`", r#type.get_id()), issues::OVERRIDABLE);
            return None;
        };

        self.is_translated_class(object.span(), class).then_some(*class)
    }

    /// Whether `class` is PHP# code a law can reach. Records why it is not.
    fn is_translated_class(&mut self, span: Span, class: &[u8]) -> bool {
        if is_collection(class) {
            self.unmodeled(span, "a collection".to_owned(), issues::COLLECTION);
            return false;
        }
        if is_library(class) {
            self.unmodeled(span, format!("the standard library class {}", names::short_name(class)), issues::LIBRARY);
            return false;
        }
        if !self.reach.class(class).is_some_and(|reached| reached.sharp) {
            self.unmodeled(span, format!("the plain PHP class {}", names::short_name(class)), issues::PLAIN_PHP);
            return false;
        }

        true
    }

    /// The Lean type of the class `class` a hint names.
    fn class_type(&mut self, span: Span, class: &[u8]) -> String {
        if !self.is_translated_class(span, class) {
            return "default".to_owned();
        }

        let class_name = self.reached_name(class);
        let lean = names::full_name(&class_name);
        self.uses(class, &lean, names::short_name(&class_name));

        lean
    }

    /// `class` as its declaration spells it.
    fn reached_name(&self, class: &[u8]) -> Vec<u8> {
        self.reach.class(class).map_or_else(|| class.to_vec(), |reached| reached.name.as_bytes().to_vec())
    }

    /// Records that the body uses the declaration `key` of `class`, which messages name `place`.
    fn uses(&mut self, class: &[u8], key: &str, place: String) {
        let file = self.reach.class(class).map(|reached| reached.file);
        self.uses.push(Use { key: key.to_owned(), file, place });
    }

    /// The source text at `span`, as messages quote it.
    fn source(&self, span: Span) -> String {
        let contents = &self.checked.file().contents;
        let text = contents.get(span.start.offset as usize..span.end.offset as usize).unwrap_or_default();

        String::from_utf8_lossy(text).into_owned()
    }

    /// Records a construct the translation does not model, and returns a term that stands in for it. A declaration
    /// that holds one is never written to Lean, so the term is never read.
    pub(crate) fn unmodeled(&mut self, span: Span, what: String, reason: &'static str) -> String {
        self.unmodeled.push(Unmodeled { span, what, place: self.place.clone(), reason });

        "default".to_owned()
    }

    fn kind(&self, r#type: &TUnion) -> Kind {
        let mut kinds = r#type.types.iter().filter(|atomic| !atomic.is_null()).map(|atomic| match atomic {
            TAtomic::Scalar(TScalar::Integer(_)) => Kind::Int,
            TAtomic::Scalar(TScalar::Bool(_)) => Kind::Bool,
            TAtomic::Scalar(TScalar::String(_)) => Kind::String,
            TAtomic::Scalar(TScalar::Float(_)) => Kind::Float,
            TAtomic::Object(TObject::Enum(_)) => Kind::Enum,
            TAtomic::Object(TObject::Named(named)) => {
                if matches!(
                    self.checked.types().class_declaration(named.name.as_bytes()).kind,
                    DeclarationKind::Enum { .. }
                ) {
                    Kind::Enum
                } else {
                    Kind::Object
                }
            }
            _ => Kind::Other,
        });
        let Some(first) = kinds.next() else {
            return Kind::Null;
        };

        if kinds.all(|kind| kind == first) { first } else { Kind::Other }
    }

    /// Binds `effect` to a new temporary, and returns the temporary.
    fn bind(&mut self, effect: String) -> String {
        self.temporaries += 1;
        let temporary = format!("«${}»", self.temporaries);
        self.lines.push(format!("let {temporary} ← {effect}"));

        temporary
    }

    /// The statements `translate` adds, apart from the body's own, and the term it returns.
    fn nested(&mut self, translate: impl FnOnce(&mut Self) -> String) -> (Vec<String>, String) {
        let outer = std::mem::take(&mut self.lines);
        let term = translate(self);
        let lines = std::mem::replace(&mut self.lines, outer);

        (lines, term)
    }

    /// The statements of a branch that `translate` gives the value of, ending in `pure` of that value.
    fn nested_lines(&mut self, translate: impl FnOnce(&mut Self) -> String) -> Branch {
        let (mut lines, term) = self.nested(translate);
        lines.push(format!("pure {term}"));

        lines
    }

    /// The statements of a branch whose value is `expression`'s.
    fn branch(&mut self, expression: &Expression) -> Branch {
        self.nested_lines(|body| body.value(expression))
    }

    /// The statements `statement` translates to on its own, which a branch of an `if` holds.
    fn nested_statement(&mut self, statement: &Statement) -> Vec<String> {
        let outer = std::mem::take(&mut self.lines);
        self.statement(statement);
        let mut lines = std::mem::replace(&mut self.lines, outer);
        if lines.is_empty() {
            lines.push("pure ()".to_owned());
        }

        lines
    }

    /// The value of the first branch whose condition holds, else `otherwise`'s. Branches that only return a pure term
    /// are an `if` term, and any other is bound from a nested `do` block, which runs only its own branch's effects.
    fn choose(&mut self, arms: Vec<(String, Branch)>, otherwise: Branch) -> String {
        let pure = |branch: &Branch| match branch.as_slice() {
            [only] => only.strip_prefix("pure ").map(str::to_owned),
            _ => None,
        };
        if let Some(mut term) = pure(&otherwise)
            && arms.iter().all(|(_, branch)| pure(branch).is_some())
        {
            for (condition, branch) in arms.iter().rev() {
                term = format!("(if {condition} then {} else {term})", pure(branch).unwrap_or_default());
            }

            return term;
        }

        self.temporaries += 1;
        let temporary = format!("«${}»", self.temporaries);
        let mut lines = Vec::new();
        for (index, (condition, branch)) in arms.iter().enumerate() {
            if index == 0 {
                lines.push(format!("let {temporary} ← if {condition} then do"));
            } else {
                lines.push(format!("  else if {condition} then do"));
            }
            lines.extend(indented(branch, 4));
        }
        if arms.is_empty() {
            lines.push(format!("let {temporary} ← do"));
            lines.extend(indented(&otherwise, 4));
        } else {
            lines.push("  else do".to_owned());
            lines.extend(indented(&otherwise, 4));
        }
        self.lines.extend(lines);

        temporary
    }
}

/// `lines`, each `by` spaces further in.
pub(crate) fn indented(lines: &[String], by: usize) -> impl Iterator<Item = String> + '_ {
    lines.iter().map(move |line| format!("{}{line}", " ".repeat(by)))
}

/// Whether the fully qualified `class` belongs to the standard library, which owns the namespace `Sharp`.
fn is_library(class: &[u8]) -> bool {
    let class = class.strip_prefix(b"\\").unwrap_or(class);

    class.len() > 6 && class[..6].eq_ignore_ascii_case(b"sharp\\")
}

/// Whether `class` names a PHP# collection.
fn is_collection(class: &[u8]) -> bool {
    matches!(names::short_name(class).as_str(), "List" | "Map" | "Set")
}
