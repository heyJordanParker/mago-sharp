//! A method's or law's body as the statements of a Lean `do` block in `M`.
//!
//! Every value that can throw is bound to a temporary `«$n»` before it is used, as A-normal form binds it, so the
//! effects run in the order PHP# runs them and every other term is pure. A branch that runs only on one side of a
//! condition is a nested `do` block, so its effects run only there.

use foldhash::HashMap;
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
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Is;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalTarget;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_word::ascii_lowercase_word;

use crate::issues;
use crate::reach::Reach;
use crate::translate::Bound;
use crate::translate::Smaller;
use crate::translate::Step;
use crate::translate::Unmodeled;
use crate::translate::Use;
use crate::translate::names;
use crate::translate::pattern::Choice;
use crate::translate::pattern::Test;
use crate::translate::pattern::choice;

/// What a value of a PHP# type is in Lean, as far as an operator cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
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
pub(super) type Branch = Vec<String>;

/// What the conditions on the path to the code being translated show about an int parameter: a call may pass it
/// one step towards `step`'s bound.
#[derive(Debug, Clone, Copy)]
struct Fact {
    parameter: usize,
    step: Step,
}

/// The translation of one body: its `do` statements, the declarations it uses, and each construct it cannot model.
pub(crate) struct Body<'body, 'program> {
    pub(super) checked: &'body CheckedProgram<'program>,
    reach: &'body Reach,
    /// The fully qualified PHP name of the class the body belongs to.
    class: &'body [u8],
    /// The body as messages name it: `Money.add`.
    place: String,
    pub(crate) uses: Vec<Use>,
    pub(crate) unmodeled: Vec<Unmodeled>,
    pub(super) lines: Vec<String>,
    temporaries: u32,
    /// Each local whose type takes `null`, by where its declaration's name starts.
    pub(super) nullable_locals: HashSet<u32>,
    /// The parameters of a method whose calls of its own class may recurse, in order: where each one's name starts,
    /// and whether it is an `int`.
    parameters: Vec<(u32, bool)>,
    /// What the conditions on the path to the code being translated show about the parameters.
    facts: Vec<Fact>,
    /// Each local a list pattern binds to the rest of a list parameter, by where its name starts, and the parameter.
    pub(super) rests: HashMap<u32, usize>,
    /// The locals the loop being translated adds to, by where their declarations' names start, which its conditions
    /// and the values it adds must not read.
    pub(super) accumulators: Vec<u32>,
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
            parameters: Vec::new(),
            facts: Vec::new(),
            rests: HashMap::default(),
            accumulators: Vec::new(),
        }
    }

    /// Lets the body's calls of methods of its own class pass smaller values of `parameters`, in order.
    pub(crate) fn recurse_on<'parameter, 'arena, I>(&mut self, parameters: I)
    where
        'arena: 'parameter,
        I: Iterator<Item = &'parameter FunctionLikeParameter<'arena>>,
    {
        self.parameters = parameters
            .map(|parameter| (parameter.variable.span.start.offset, matches!(parameter.hint, Some(Hint::Integer(_)))))
            .collect();
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
                // A `List` is Lean's `List`, which `map`, `filter` and `count` read as Lean's own functions.
                if names::short_name(name) == "List"
                    && let Some(element) = generic.type_arguments.arguments.first()
                {
                    format!("(List {})", self.r#type(element))
                } else if is_collection(name) {
                    self.unmodeled(hint.span(), "a collection".to_owned(), issues::COLLECTION)
                } else {
                    self.class_type(hint.span(), name)
                }
            }
            Hint::Function(_) => self.unmodeled(hint.span(), "a function type".to_owned(), issues::LAMBDA),
            Hint::Mixed(_) => self.unmodeled(hint.span(), "Any".to_owned(), issues::TYPE_TEST),
            Hint::Iterable(_) => self.unmodeled(hint.span(), "Iterable".to_owned(), issues::LAZY),
            _ => self.unmodeled(hint.span(), format!("the type `{}`", self.source(hint.span())), issues::TYPE),
        }
    }

    /// The Lean type of a declaration's `hint`, which PHP# requires on every parameter and property; `span` is the
    /// declaration's.
    pub(crate) fn declared_type(&mut self, hint: Option<&Hint>, span: Span) -> String {
        match hint {
            Some(hint) => self.r#type(hint),
            None => self.unmodeled(span, "a declaration without a type".to_owned(), issues::MISSING_TYPE),
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
        self.statements(&block.statements);
        if void || self.lines.is_empty() {
            self.lines.push("pure ()".to_owned());
        }

        (self.lines, self.uses, self.unmodeled)
    }

    /// The term of a constant's value, which Lean defines without `M`, so it must not throw.
    pub(crate) fn pure_value(&mut self, expression: &Expression) -> String {
        self.pure(expression, "a constant whose value can throw", issues::CONSTANT_VALUE)
            .unwrap_or_else(|| "default".to_owned())
    }

    /// The body's statements so far, the declarations it uses, and each construct it cannot model.
    pub(crate) fn finish(self) -> (Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        (self.lines, self.uses, self.unmodeled)
    }

    /// `statements` in order. The statements after an `if` whose block returns run where its condition does not hold.
    fn statements(&mut self, statements: &Sequence<'_, Statement<'_>>) {
        let depth = self.facts.len();
        for statement in statements {
            self.statement(statement);
            if let Statement::If(r#if) = statement
                && let IfBody::Statement(body) = &r#if.body
                && body.else_clause.is_none()
                && returns(body.statement)
            {
                let facts = self.facts(r#if.condition, false);
                self.facts.extend(facts);
            }
        }
        self.facts.truncate(depth);
    }

    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Block(block) => self.statements(&block.statements),
            Statement::Return(r#return) => {
                let value = r#return.value.map_or_else(|| "()".to_owned(), |value| self.value(value));
                self.lines.push(format!("return {value}"));
            }
            Statement::LocalDeclaration(local) => {
                let LocalTarget::Name(name) = &local.target else {
                    self.unmodeled(statement.span(), "an unpacked tuple".to_owned(), issues::TUPLE);

                    return;
                };
                let value = self.value(local.value);
                if self.checked.types().expression_type(local.value).is_nullable() {
                    self.nullable_locals.insert(name.span.start.offset);
                }
                let keyword = if local.is_const() { "let" } else { "let mut" };
                self.lines.push(format!("{keyword} {} := {value}", names::identifier(name.value)));
            }
            Statement::Expression(statement) => match statement.expression {
                Expression::Assignment(assignment) => self.assignment(assignment),
                expression => {
                    self.value(expression);
                }
            },
            Statement::If(r#if) => {
                let IfBody::Statement(body) = &r#if.body else {
                    unreachable!("PHP# parses no colon-delimited if, as {:?}", r#if.span());
                };
                // `if (x is Money m)` names `m` where the test holds, so it is a `match` on `x`.
                if let Expression::Is(is) = unparenthesized(r#if.condition) {
                    let Some(test) = self.is_test(is) else {
                        return;
                    };
                    let then = self.nested_statement(body.statement);
                    let otherwise = body.else_clause.as_ref().map(|clause| self.nested_statement(clause.statement));
                    let choice = choice(vec![(test, then)], otherwise.unwrap_or_default());
                    self.lines.extend(statements(&choice));
                    return;
                }
                let condition = self.value(r#if.condition);
                let holds = self.facts(r#if.condition, true);
                let then = self.under(holds, |this| this.nested_statement(body.statement));
                self.lines.push(format!("if {condition} then"));
                self.lines.extend(indented(&then, 2));
                if let Some(clause) = &body.else_clause {
                    let fails = self.facts(r#if.condition, false);
                    let otherwise = self.under(fails, |this| this.nested_statement(clause.statement));
                    self.lines.push("else".to_owned());
                    self.lines.extend(indented(&otherwise, 2));
                }
            }
            Statement::ForOf(for_of) => self.counting_loop(for_of),
            Statement::For(_)
            | Statement::Foreach(_)
            | Statement::While(_)
            | Statement::DoWhile(_)
            | Statement::Break(_)
            | Statement::Continue(_) => {
                self.unmodeled(statement.span(), "a loop".to_owned(), issues::LOOP);
            }
            Statement::Try(_) => {
                self.unmodeled(statement.span(), "a try/catch".to_owned(), issues::EXCEPTION);
            }
            Statement::PatternMatch(r#match) => {
                let Some((arms, otherwise)) = self.arms(r#match, Self::arm_statements) else {
                    return;
                };
                let choice = choice(arms, otherwise);
                self.lines.extend(statements(&choice));
            }
            _ => {
                self.unmodeled(
                    statement.span(),
                    format!("the statement `{}`", self.source(statement.span())),
                    issues::STATEMENT,
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
            self.unmodeled(assignment.lhs.span(), "an assignment".to_owned(), issues::LOCAL_ASSIGNMENT);
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
                self.unmodeled(assignment.span(), what, issues::UPDATE)
            }
        };
        self.lines.push(format!("{local} := {assigned}"));
    }

    /// The pure term of `expression`, after binding each effect it has.
    pub(crate) fn value(&mut self, expression: &Expression) -> String {
        match expression {
            Expression::Parenthesized(parenthesized) => self.value(parenthesized.expression),
            Expression::Literal(literal) => self.literal(literal),
            // `nameof(X.y)` is the string `'y'`, which the checker proved names a property of `X`.
            Expression::NameOf(name_of) => names::string(name_of.property.value),
            Expression::ConstantAccess(name) => self.name(expression, name),
            Expression::Binary(binary) => self.binary(binary),
            Expression::UnaryPrefix(unary) => self.prefix(unary),
            Expression::Conditional(conditional) => match conditional.then {
                Some(then) => {
                    if let Expression::Is(is) = unparenthesized(conditional.condition) {
                        let Some(test) = self.is_test(is) else {
                            return "default".to_owned();
                        };
                        let then = self.branch(then);
                        let otherwise = self.branch(conditional.r#else);

                        return self.choose(&choice(vec![(test, then)], otherwise));
                    }
                    let condition = self.value(conditional.condition);
                    let holds = self.facts(conditional.condition, true);
                    let then = self.under(holds, |body| body.branch(then));
                    let fails = self.facts(conditional.condition, false);
                    let otherwise = self.under(fails, |body| body.branch(conditional.r#else));

                    self.choose(&Choice::If(
                        condition,
                        Box::new(Choice::Branch(then)),
                        Box::new(Choice::Branch(otherwise)),
                    ))
                }
                None => unreachable!("PHP# parses no `?:` with two operands, as {:?}", expression.span()),
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
            // A spread is Lean's `++`: `[x, ...rest]` is `[x] ++ rest`. `[]` types as no array in particular, and Lean
            // reads it as the empty List the code around it expects.
            Expression::Array(array) if array.elements.is_empty() || self.is_list(expression) => {
                let mut parts: Vec<String> = Vec::new();
                let mut elements: Vec<String> = Vec::new();
                for element in &array.elements {
                    match element {
                        ArrayElement::Value(element) => elements.push(self.value(element.value)),
                        ArrayElement::Variadic(spread) => {
                            if !elements.is_empty() {
                                parts.push(format!("[{}]", elements.join(", ")));
                                elements.clear();
                            }
                            parts.push(self.value(spread.value));
                        }
                        element => unreachable!(
                            "the checker accepts a List literal of values and spreads of Lists, not {:?}",
                            element.span()
                        ),
                    }
                }
                if !elements.is_empty() || parts.is_empty() {
                    parts.push(format!("[{}]", elements.join(", ")));
                }

                match parts.as_slice() {
                    [only] => only.clone(),
                    _ => format!("({})", parts.join(" ++ ")),
                }
            }
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
            Expression::Is(is) => {
                if names_values(is.pattern) {
                    return self.unmodeled(
                        expression.span(),
                        "a pattern that names a value outside the condition of an if or a ?:".to_owned(),
                        issues::NAMED_PATTERN,
                    );
                }
                let Some(test) = self.is_test(is) else {
                    return "default".to_owned();
                };

                self.choose(&choice(vec![(test, vec!["pure true".to_owned()])], vec!["pure false".to_owned()]))
            }
            Expression::As(_) | Expression::TypeOf(_) => {
                self.unmodeled(expression.span(), "a type test".to_owned(), issues::TYPE_TEST)
            }
            Expression::Yield(_) => self.unmodeled(expression.span(), "a lazy sequence".to_owned(), issues::LAZY),
            Expression::Throw(_) => self.unmodeled(expression.span(), "a throw".to_owned(), issues::EXCEPTION),
            Expression::CompositeString(_) => {
                self.unmodeled(expression.span(), "a string template".to_owned(), issues::TEMPLATE)
            }
            Expression::Assignment(_) => self.unmodeled(
                expression.span(),
                "an assignment inside an expression".to_owned(),
                issues::NESTED_ASSIGNMENT,
            ),
            _ => {
                let what = format!("the expression `{}`", self.source(expression.span()));
                self.unmodeled(expression.span(), what, issues::EXPRESSION)
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
                if self.accumulators.contains(&local.declaration.start.offset) {
                    return self.unmodeled(
                        expression.span(),
                        "a loop that reads a local it adds to".to_owned(),
                        issues::LOOP,
                    );
                }
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
                self.unmodeled(expression.span(), what, issues::CONSTANT)
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
                self.unmodeled(binary.span(), format!("the class operator `{operator}`"), issues::CLASS_OPERATOR)
            }
            BinaryOperator::Equal(_) | BinaryOperator::Identical(_) => format!("({lhs} == {rhs})"),
            BinaryOperator::NotEqual(_) | BinaryOperator::NotIdentical(_) => format!("({lhs} != {rhs})"),
            BinaryOperator::LessThan(_) if ordered => format!("(decide ({lhs} < {rhs}))"),
            BinaryOperator::LessThanOrEqual(_) if ordered => format!("(decide ({lhs} <= {rhs}))"),
            BinaryOperator::GreaterThan(_) if ordered => format!("(decide ({rhs} < {lhs}))"),
            BinaryOperator::GreaterThanOrEqual(_) if ordered => format!("(decide ({rhs} <= {lhs}))"),
            _ => self.unmodeled(binary.span(), format!("the operator `{operator}` here"), issues::OPERATOR),
        }
    }

    /// `&&` and `||`, which run their right side only when the left one does not decide.
    fn short_circuit(&mut self, binary: &Binary, and: bool) -> String {
        let lhs = self.value(binary.lhs);
        let facts = self.facts(binary.lhs, and);
        let rhs = self.under(facts, |body| body.branch(binary.rhs));
        let operator = if and { "&&" } else { "||" };
        if let [only] = rhs.as_slice()
            && let Some(rhs) = only.strip_prefix("pure ")
        {
            return format!("({lhs} {operator} {rhs})");
        }

        let (then, otherwise) =
            if and { (rhs, vec!["pure false".to_owned()]) } else { (vec!["pure true".to_owned()], rhs) };
        self.choose(&Choice::If(lhs, Box::new(Choice::Branch(then)), Box::new(Choice::Branch(otherwise))))
    }

    /// `a ?? b`, which runs `b` only when `a` is null.
    fn coalesce(&mut self, binary: &Binary) -> String {
        let lhs = self.value(binary.lhs);
        let keeps_null = self.checked.types().expression_type(binary.rhs).is_nullable();
        let rhs = self.branch(binary.rhs);
        let present = if keeps_null { lhs.clone() } else { format!("({lhs}.getD default)") };

        self.choose(&Choice::If(
            format!("{lhs}.isSome"),
            Box::new(Choice::Branch(vec![format!("pure {present}")])),
            Box::new(Choice::Branch(rhs)),
        ))
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
                    _ => self.unmodeled(unary.span(), "the operator `-` here".to_owned(), issues::OPERATOR),
                }
            }
            UnaryPrefixOperator::Not(_) => {
                let operand = self.value(unary.operand);
                format!("(!{operand})")
            }
            UnaryPrefixOperator::Plus(_) => self.value(unary.operand),
            operator => {
                let what = format!("the operator `{}`", self.source(operator.span()));
                self.unmodeled(unary.span(), what, issues::OPERATOR)
            }
        }
    }

    /// A `match` in an expression gives the value of the first arm whose pattern and guard hold, and throws when none
    /// does.
    fn pattern_match(&mut self, r#match: &PatternMatch) -> String {
        let Some((arms, otherwise)) = self.arms(r#match, Self::arm_value) else {
            return "default".to_owned();
        };

        self.choose(&choice(arms, otherwise))
    }

    /// The test and the branch of each arm of `r#match`, in order, each branch as `translate` gives it, and the branch
    /// that runs when no arm matches: the `default` arm's, else a throw. `None` after recording why an arm has no test.
    fn arms(
        &mut self,
        r#match: &PatternMatch,
        mut translate: impl FnMut(&mut Self, &PatternMatchArmBody) -> Branch,
    ) -> Option<(Vec<(Test, Branch)>, Branch)> {
        let r#type = self.checked.types().expression_type(r#match.expression);
        if self.kind(r#type) == Kind::Float {
            self.unmodeled(r#match.span(), "a float".to_owned(), issues::FLOAT);
            return None;
        }

        let subject = self.value(r#match.expression);
        let parameter = self.parameter(r#match.expression);
        let depth = self.facts.len();
        let mut arms = Vec::new();
        let mut otherwise = vec!["throw Throwable.UnhandledMatchError".to_owned()];
        for arm in r#match.arms.iter() {
            let PatternMatchArm::Pattern(arm) = arm else {
                otherwise = translate(self, arm.body());
                continue;
            };

            let mut test = self.test(&subject, r#type, parameter, arm.pattern)?;
            if let Some(guard) = &arm.guard {
                test.conditions.push(self.pure(guard.condition, "a guard that can throw", issues::GUARD)?);
            }
            let holds = self.pattern_facts(parameter, arm.pattern, true);
            arms.push((test, self.under(holds, |body| translate(body, &arm.body))));
            // A later arm runs only where this one's pattern fails, unless its guard is what failed.
            if arm.guard.is_none() {
                let fails = self.pattern_facts(parameter, arm.pattern, false);
                self.facts.extend(fails);
            }
        }
        self.facts.truncate(depth);

        Some((arms, otherwise))
    }

    /// The branch of an arm of a `match` in an expression, which ends in the arm's value.
    fn arm_value(&mut self, body: &PatternMatchArmBody) -> Branch {
        match body {
            PatternMatchArmBody::Expression(expression) => self.branch(expression),
            PatternMatchArmBody::Block(block) => {
                unreachable!("the checker accepts a block arm only in a match statement, not {:?}", block.span())
            }
        }
    }

    /// The statements of an arm of a `match` statement.
    fn arm_statements(&mut self, body: &PatternMatchArmBody) -> Branch {
        let outer = std::mem::take(&mut self.lines);
        match body {
            PatternMatchArmBody::Expression(expression) => {
                self.value(expression);
            }
            PatternMatchArmBody::Block(block) => self.statements(&block.statements),
        }

        std::mem::replace(&mut self.lines, outer)
    }

    /// The test of `x is P`, of the value of `x`.
    fn is_test(&mut self, is: &Is) -> Option<Test> {
        let r#type = self.checked.types().expression_type(is.value);
        let subject = self.value(is.value);
        let parameter = self.parameter(is.value);

        self.test(&subject, r#type, parameter, is.pattern)
    }

    /// Whether `hint` names the type of every value of `atomic`.
    pub(super) fn names_type(&self, hint: &Hint, atomic: &TAtomic) -> bool {
        let class = match hint {
            Hint::Integer(_) => return matches!(atomic, TAtomic::Scalar(TScalar::Integer(_))),
            Hint::String(_) => return matches!(atomic, TAtomic::Scalar(TScalar::String(_))),
            Hint::Bool(_) => return matches!(atomic, TAtomic::Scalar(TScalar::Bool(_))),
            Hint::Parenthesized(parenthesized) => return self.names_type(parenthesized.hint, atomic),
            Hint::Generic(generic) => {
                let name = self.checked.names().resolve(&generic.name).unwrap_or(generic.name.value);
                if names::short_name(name) == "List"
                    && let TAtomic::Array(TArray::List(list)) = atomic
                    && let [element] = generic.type_arguments.arguments.as_slice()
                    && !list.element_type.is_nullable()
                    && let [element_type] = list.element_type.types.as_ref()
                {
                    return self.names_type(element, element_type);
                }
                return false;
            }
            Hint::Self_(_) => self.class,
            Hint::Identifier(identifier) => self.checked.names().get(identifier),
            _ => return false,
        };

        match atomic {
            TAtomic::Object(TObject::Named(named)) => named.name.as_bytes().eq_ignore_ascii_case(class),
            TAtomic::Object(TObject::Enum(r#enum)) => r#enum.name.as_bytes().eq_ignore_ascii_case(class),
            _ => false,
        }
    }

    /// The Lean constructor of the case of an enum `hint` names, when `atomic` is that enum.
    pub(super) fn case(&self, hint: &Hint, atomic: &TAtomic) -> Option<String> {
        let Hint::Identifier(identifier) = hint else {
            return None;
        };
        let class = self.checked.names().get(identifier);
        let separator = class.iter().rposition(|&byte| byte == b'\\')?;
        let (r#enum, case) = (&class[..separator], &class[separator + 1..]);
        let named = match atomic {
            TAtomic::Object(TObject::Named(named)) => named.name.as_bytes(),
            TAtomic::Object(TObject::Enum(r#enum)) => r#enum.name.as_bytes(),
            _ => return None,
        };
        if !named.eq_ignore_ascii_case(r#enum) || self.checked.types().enum_case_class(r#enum, case).is_none() {
            return None;
        }

        Some(names::member(&names::full_name(&self.reached_name(r#enum)), case))
    }

    /// The parameter of the method being translated that `expression` reads, by position.
    fn parameter(&self, expression: &Expression) -> Option<usize> {
        let Expression::ConstantAccess(name) = unparenthesized(expression) else {
            return None;
        };
        let Some(Binding::Local(local)) = self.checked.names().binding(&name.name) else {
            return None;
        };

        self.parameters.iter().position(|(offset, _)| *offset == local.declaration.start.offset)
    }

    /// What `condition` shows about the parameters where it is `holds`: a comparison of an int parameter with an int
    /// literal or another parameter, under `!`, `&&` where it holds, and `||` where it fails.
    fn facts(&self, condition: &Expression, holds: bool) -> Vec<Fact> {
        match unparenthesized(condition) {
            Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) => {
                self.facts(operand, !holds)
            }
            Expression::Binary(binary) => match binary.operator {
                BinaryOperator::And(_) | BinaryOperator::LowAnd(_) if holds => {
                    [self.facts(binary.lhs, true), self.facts(binary.rhs, true)].concat()
                }
                BinaryOperator::Or(_) if !holds => {
                    [self.facts(binary.lhs, false), self.facts(binary.rhs, false)].concat()
                }
                _ => {
                    let fact = match (self.int_parameter(binary.lhs), self.int_parameter(binary.rhs)) {
                        (Some(parameter), _) => self.fact(parameter, &binary.operator, binary.rhs, holds, false),
                        (None, Some(parameter)) => self.fact(parameter, &binary.operator, binary.lhs, holds, true),
                        (None, None) => None,
                    };
                    fact.into_iter().collect()
                }
            },
            _ => Vec::new(),
        }
    }

    /// What a comparison pattern of `pattern`'s shows about the `match`'s subject, the parameter `parameter`, where
    /// the pattern is `holds`.
    fn pattern_facts(&self, parameter: Option<usize>, pattern: &Pattern, holds: bool) -> Vec<Fact> {
        match (parameter, pattern) {
            (Some(parameter), Pattern::Comparison(comparison)) if self.parameters[parameter].1 => {
                self.fact(parameter, &comparison.operator, comparison.value, holds, false).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    /// What `parameter OPERATOR other`, or `other OPERATOR parameter` when `swapped`, shows where it is `holds`: that
    /// the parameter is above or below `other`, an int literal or another parameter. A literal bound takes `>=` and
    /// `<=` as one step past it, and a parameter only `>` and `<`, since a parameter may be the end of the range, where
    /// one step further leaves it.
    fn fact(
        &self,
        parameter: usize,
        operator: &BinaryOperator,
        other: &Expression,
        holds: bool,
        swapped: bool,
    ) -> Option<Fact> {
        // (above, inclusive) for `parameter > other`, `>=`, `<`, `<=`.
        let (above, inclusive) = match (operator, swapped) {
            (BinaryOperator::GreaterThan(_), false) | (BinaryOperator::LessThan(_), true) => (true, false),
            (BinaryOperator::GreaterThanOrEqual(_), false) | (BinaryOperator::LessThanOrEqual(_), true) => (true, true),
            (BinaryOperator::LessThan(_), false) | (BinaryOperator::GreaterThan(_), true) => (false, false),
            (BinaryOperator::LessThanOrEqual(_), false) | (BinaryOperator::GreaterThanOrEqual(_), true) => {
                (false, true)
            }
            _ => return None,
        };
        // Where the comparison fails, the opposite one holds: not `>` is `<=`.
        let (above, inclusive) = if holds { (above, inclusive) } else { (!above, !inclusive) };
        let bound = match (self.int_parameter(other), literal(other)) {
            (Some(bound), _) if !inclusive && bound != parameter => Bound::Parameter(bound),
            (_, Some(value)) => Bound::Literal(match (inclusive, above) {
                (false, _) => value,
                (true, true) => value.checked_sub(1)?,
                (true, false) => value.checked_add(1)?,
            }),
            _ => return None,
        };

        Some(Fact { parameter, step: if above { Step::Down(bound) } else { Step::Up(bound) } })
    }

    /// The int parameter `expression` reads, by position.
    fn int_parameter(&self, expression: &Expression) -> Option<usize> {
        self.parameter(expression).filter(|parameter| self.parameters[*parameter].1)
    }

    /// `translate` where the conditions on the path also show `facts`.
    fn under<T>(&mut self, facts: Vec<Fact>, translate: impl FnOnce(&mut Self) -> T) -> T {
        let depth = self.facts.len();
        self.facts.extend(facts);
        let translated = translate(self);
        self.facts.truncate(depth);

        translated
    }

    /// The condition that `subject`, a value of `kind`, matches `pattern`, or `None` after recording why it has none.
    pub(super) fn pattern(&mut self, subject: &str, kind: Kind, pattern: &Pattern) -> Option<String> {
        match pattern {
            // A value an object is compared with is `null`: a pattern takes constants, and an object's identity is
            // nothing a law reasons about.
            Pattern::Value(value)
                if matches!(kind, Kind::Object | Kind::Other)
                    && !matches!(unparenthesized(value), Expression::Literal(Literal::Null(_))) =>
            {
                self.unmodeled(pattern.span(), "a match on this subject".to_owned(), issues::MATCH_SUBJECT);
                None
            }
            Pattern::Value(value) => {
                Some(format!("({subject} == {})", self.pure(value, "a pattern that can throw", issues::PATTERN_VALUE)?))
            }
            Pattern::Comparison(_) if matches!(kind, Kind::Object | Kind::Other) => {
                self.unmodeled(pattern.span(), "a match on this subject".to_owned(), issues::MATCH_SUBJECT);
                None
            }
            Pattern::Comparison(comparison) => {
                let value = self.pure(comparison.value, "a pattern that can throw", issues::PATTERN_VALUE)?;
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
                        self.unmodeled(comparison.span(), what, issues::COMPARISON_PATTERN);
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
            Pattern::Type(_) | Pattern::Properties(_) | Pattern::List(_) => {
                self.unmodeled(pattern.span(), "a type test".to_owned(), issues::TYPE_TEST);
                None
            }
        }
    }

    /// The term of `expression`, which must not throw, or `None` after recording `what` and `reason` when it can.
    fn pure(&mut self, expression: &Expression, what: &str, reason: &'static str) -> Option<String> {
        let (lines, value) = self.nested(|body| body.value(expression));
        if lines.is_empty() {
            return Some(value);
        }

        self.unmodeled(expression.span(), what.to_owned(), reason);
        None
    }

    /// `object.m(…)`, `Class.m(…)` and `Self.m(…)`.
    fn method_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let names = self.checked.names();
        if let Some(class) = names.static_call_class(call) {
            if names.is_type_parameter(&class.name) {
                return self.unmodeled(
                    expression.span(),
                    "a call through a type parameter".to_owned(),
                    issues::TYPE_TEST,
                );
            }
            let class = names.get(&class.name);
            if !self.is_translated_class(call.object.span(), class) {
                return "default".to_owned();
            }
            // Spec section 20: `PaymentResult.Paid(…)` creates the case, which is its constructor applied to the
            // fields in order, a value and no call.
            if let Some(case) = self.method_name(&call.method)
                && self.checked.types().enum_case_class(class, case).is_some()
            {
                let class_name = self.reached_name(class);
                let lean = names::full_name(&class_name);
                let Some(arguments) = self.arguments(&call.argument_list, call.argument_list.arguments.len(), None)
                else {
                    return "default".to_owned();
                };
                self.uses(class, &lean, names::short_name(&class_name));

                let mut constructor = vec![names::member(&lean, case)];
                constructor.extend(arguments);

                return format!("({})", constructor.join(" "));
            }
            let target = self.checked.types().call_target(expression);
            if target.kind != DeclarationKind::StaticMethod {
                return self.unmodeled(expression.span(), "a call through a class value".to_owned(), issues::TYPE_TEST);
            }

            let Some(method) = self.method_name(&call.method) else {
                return "default".to_owned();
            };

            return self.call(expression, target.class.as_bytes(), class, method, None, Some(&call.argument_list));
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

                return self.call(
                    expression,
                    target.class.as_bytes(),
                    self.class,
                    method,
                    None,
                    Some(&call.argument_list),
                );
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

        if self.is_list(call.object) {
            return self.list_call(expression, call);
        }
        if let Some(extension) = self.checked.types().extension(expression) {
            if !self.is_translated_class(expression.span(), extension.class.as_bytes()) {
                return "default".to_owned();
            }

            let class = extension.class.as_bytes();
            let receiver = self.value(call.object);
            return self.call(
                expression,
                class,
                class,
                extension.name.as_bytes(),
                Some(receiver),
                Some(&call.argument_list),
            );
        }
        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        match target.kind {
            DeclarationKind::Method { overridable: false } => {}
            DeclarationKind::Method { overridable: true } => {
                return self.overridable(expression, class, target.name.as_bytes());
            }
            _ => return self.unmodeled(expression.span(), "a call of a function value".to_owned(), issues::LAMBDA),
        }

        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let receiver = self.value(call.object);
        self.call(expression, target.class.as_bytes(), class, method, Some(receiver), Some(&call.argument_list))
    }

    /// Whether `expression`'s value is a `List`, null aside.
    pub(super) fn is_list(&self, expression: &Expression) -> bool {
        let r#type = self.checked.types().expression_type(expression);
        let mut types = r#type.types.iter().filter(|atomic| !atomic.is_null()).peekable();

        types.peek().is_some() && types.all(TAtomic::is_list)
    }

    /// A `List` method a law reasons about, as the Lean function it is: `count()` is `int.count`, the list's length,
    /// and `map` and `filter` of a lambda are `List.map` and `List.filter`. Every other method of a collection is
    /// refused by name.
    fn list_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let arguments = &call.argument_list.arguments;
        match (method, arguments.len()) {
            (b"count", 0) => {
                let list = self.value(call.object);

                format!("(int.count {list})")
            }
            (b"map" | b"filter", 1) => {
                let list = self.value(call.object);
                let Some(function) = arguments.first().and_then(|argument| self.lambda(argument.value())) else {
                    return "default".to_owned();
                };
                let lean = if method == b"map" { "map" } else { "filter" };

                format!("(List.{lean} {function} {list})")
            }
            _ => {
                let what = format!("the List method {}", String::from_utf8_lossy(method));
                self.unmodeled(expression.span(), what, issues::COLLECTION)
            }
        }
    }

    /// A lambda `map` or `filter` takes, as a Lean `fun`. Its body is one expression that cannot throw, so the function
    /// is pure, as `List.map` and `List.filter` take it.
    fn lambda(&mut self, expression: &Expression) -> Option<String> {
        let Expression::ArrowFunction(arrow) = expression else {
            self.unmodeled(expression.span(), "a function value that is no lambda".to_owned(), issues::LAMBDA);
            return None;
        };
        let parameters: Vec<String> = arrow
            .parameter_list
            .parameters
            .iter()
            .map(|parameter| {
                names::identifier(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
            })
            .collect();
        let (lines, body) = self.nested(|body| body.value(arrow.expression));
        if !lines.is_empty() {
            self.unmodeled(arrow.expression.span(), "a lambda whose body can throw".to_owned(), issues::LAMBDA);
            return None;
        }

        Some(format!("(fun {} => {body})", parameters.join(" ")))
    }

    /// `object?.m(…)`, which calls `m` only when `object` is not null.
    fn null_safe_call(&mut self, expression: &Expression, call: &NullSafeMethodCall) -> String {
        if let Some(extension) = self.checked.types().extension(expression) {
            if !self.is_translated_class(expression.span(), extension.class.as_bytes()) {
                return "default".to_owned();
            }

            let class = extension.class.as_bytes();
            let method = extension.name.as_bytes();
            return self.null_safe(expression, class, class, method, call.object, Some(&call.argument_list));
        }
        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        if target.kind != (DeclarationKind::Method { overridable: false }) {
            return self.overridable(expression, class, target.name.as_bytes());
        }

        self.null_safe(expression, target.class.as_bytes(), class, method, call.object, Some(&call.argument_list))
    }

    /// The call of `class`'s method `method`, which `declaring` declares, on the value of `object`, made only when that
    /// value is not null.
    fn null_safe(
        &mut self,
        expression: &Expression,
        declaring: &[u8],
        class: &[u8],
        method: &[u8],
        object: &Expression,
        arguments: Option<&ArgumentList>,
    ) -> String {
        let receiver = self.value(object);
        let returns_null = self.reach.method(declaring, method).is_some_and(|method| method.returns_nullable);
        let present = self.nested_lines(|body| {
            let called =
                body.call(expression, declaring, class, method, Some(format!("({receiver}.getD default)")), arguments);
            if returns_null { called } else { format!("(some {called})") }
        });

        self.choose(&Choice::If(
            format!("{receiver}.isSome"),
            Box::new(Choice::Branch(present)),
            Box::new(Choice::Branch(vec!["pure none".to_owned()])),
        ))
    }

    /// The name a member selector spells, or `None` after recording a name computed while the code runs.
    fn method_name<'arena>(&mut self, selector: &ClassLikeMemberSelector<'arena>) -> Option<&'arena [u8]> {
        if let ClassLikeMemberSelector::Identifier(method) = selector {
            return Some(method.value);
        }

        self.unmodeled(selector.span(), "a dynamic member name".to_owned(), issues::MEMBER_NAME);
        None
    }

    /// A call of `class`'s method `method`, which `declaring` declares, on `receiver`, or of the static method when
    /// `receiver` is `None`. A computed property's read passes no `arguments`.
    fn call(
        &mut self,
        expression: &Expression,
        declaring: &[u8],
        class: &[u8],
        method: &[u8],
        receiver: Option<String>,
        arguments: Option<&ArgumentList>,
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
                issues::BODYLESS,
            );
        };

        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), facts.name.as_bytes());
        let mut parts = vec![key.clone()];
        parts.extend(receiver);
        let smaller = arguments
            .filter(|_| ascii_lowercase_word(class) == ascii_lowercase_word(self.class))
            .and_then(|list| self.smaller(list));
        let stepped = smaller.as_ref().and_then(|(smaller, term)| Some((smaller.argument, term.clone()?)));
        let Some(arguments) =
            arguments.map_or_else(|| Some(Vec::new()), |list| self.arguments(list, facts.parameters, stepped))
        else {
            return "default".to_owned();
        };
        parts.extend(arguments);
        let place = format!("{}.{}", names::short_name(&class_name), facts.name);
        self.use_of(class, &key, place, smaller.map(|(smaller, _)| smaller));

        self.bind(parts.join(" "))
    }

    /// The smaller value a call of a method of the body's own class passes, and the pure term of an int one, which
    /// cannot leave the `int` range where a condition showed it is above or below its bound: the rest of a list
    /// parameter's list pattern, or an int parameter one step towards a bound, at the first argument that passes one.
    fn smaller(&self, list: &ArgumentList) -> Option<(Smaller, Option<String>)> {
        let arguments: Vec<&Expression> = list.arguments.iter().map(Argument::value).collect();
        for (argument, value) in arguments.iter().enumerate() {
            let value = unparenthesized(value);
            if let Expression::ConstantAccess(name) = value
                && let Some(Binding::Local(local)) = self.checked.names().binding(&name.name)
                && let Some(&parameter) = self.rests.get(&local.declaration.start.offset)
            {
                return Some((Smaller { parameter, argument, step: Step::Rest }, None));
            }
            let Expression::Binary(binary) = value else {
                continue;
            };
            let (operand, down) = match binary.operator {
                BinaryOperator::Subtraction(_) if literal(binary.rhs) == Some(1) => (binary.lhs, true),
                BinaryOperator::Addition(_) if literal(binary.rhs) == Some(1) => (binary.lhs, false),
                BinaryOperator::Addition(_) if literal(binary.lhs) == Some(1) => (binary.rhs, false),
                _ => continue,
            };
            let (Some(parameter), Expression::ConstantAccess(name)) =
                (self.int_parameter(operand), unparenthesized(operand))
            else {
                continue;
            };
            // A parameter bound is the same value on every call, so the call passes it on unchanged.
            let passes_on = |bound: Bound| match bound {
                Bound::Literal(_) => true,
                Bound::Parameter(index) => {
                    arguments.get(index).is_some_and(|value| self.parameter(value) == Some(index))
                }
            };
            let Some(fact) = self.facts.iter().rev().find(|fact| {
                fact.parameter == parameter
                    && match fact.step {
                        Step::Down(bound) => down && passes_on(bound),
                        Step::Up(bound) => !down && passes_on(bound),
                        Step::Rest => false,
                    }
            }) else {
                continue;
            };
            let name = names::identifier(name.name.value());
            let term = if down { format!("({name} - 1)") } else { format!("({name} + 1)") };

            return Some((Smaller { parameter, argument, step: fact.step }, Some(term)));
        }

        None
    }

    /// `new Class(…)`, which runs the class's constructor. A named constructor runs a body, which Mago does not
    /// translate.
    fn instantiation(&mut self, instantiation: &Instantiation) -> String {
        let class = match instantiation.class {
            Expression::Identifier(identifier) => self.checked.names().get(identifier),
            Expression::Self_(_) => self.class,
            class => return self.unmodeled(class.span(), "a class value".to_owned(), issues::TYPE_TEST),
        };
        if !self.is_translated_class(instantiation.class.span(), class) {
            return "default".to_owned();
        }
        if let Some(constructor) = &instantiation.constructor {
            return self.unmodeled(
                instantiation.span(),
                format!(
                    "the named constructor {}.{}",
                    names::short_name(class),
                    String::from_utf8_lossy(constructor.name.value)
                ),
                issues::CONSTRUCTOR_BODY,
            );
        }

        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), b"__construct");
        let parameters = self.reach.method(class, b"__construct").map_or(0, |method| method.parameters);
        let mut parts = vec![key.clone()];
        if let Some(arguments) = &instantiation.argument_list {
            let Some(arguments) = self.arguments(arguments, parameters, None) else {
                return "default".to_owned();
            };
            parts.extend(arguments);
        }
        self.uses(class, &key, format!("new {}", names::short_name(&class_name)));

        self.bind(parts.join(" "))
    }

    /// The terms of a call's arguments, or `None` when one cannot be translated. Each argument is passed by position,
    /// and the call passes every parameter. `stepped` is an argument's position and the term that stands for it.
    fn arguments(
        &mut self,
        list: &ArgumentList,
        parameters: usize,
        stepped: Option<(usize, String)>,
    ) -> Option<Vec<String>> {
        let mut arguments = Vec::new();
        for (position, argument) in list.arguments.iter().enumerate() {
            if let Some((_, term)) = stepped.as_ref().filter(|(at, _)| *at == position) {
                arguments.push(term.clone());
                continue;
            }
            match argument {
                Argument::Positional(argument) if argument.ellipsis.is_none() => {
                    arguments.push(self.value(argument.value));
                }
                argument => {
                    self.unmodeled(argument.span(), "a named or spread argument".to_owned(), issues::ARGUMENT);
                    return None;
                }
            }
        }
        if arguments.len() < parameters {
            self.unmodeled(list.span(), "a default argument".to_owned(), issues::DEFAULT_ARGUMENT);
            return None;
        }

        Some(arguments)
    }

    /// `object.y`, `Class.CASE` and `Class.CONSTANT`.
    fn property(&mut self, expression: &Expression, access: &PropertyAccess) -> String {
        let ClassLikeMemberSelector::Identifier(member) = &access.property else {
            return self.unmodeled(access.property.span(), "a dynamic member name".to_owned(), issues::MEMBER_NAME);
        };
        let names = self.checked.names();
        if let Some(class) = names.static_property_class(access) {
            if names.is_type_parameter(&class.name) {
                return self.unmodeled(expression.span(), "a member of a type parameter".to_owned(), issues::TYPE_TEST);
            }
            let class = names.get(&class.name);
            if !self.is_translated_class(access.object.span(), class) {
                return "default".to_owned();
            }
            let class_name = self.reached_name(class);
            let lean = names::full_name(&class_name);
            let short = names::short_name(&class_name);

            return match self.checked.types().member_declaration(class, member.value).kind {
                // Spec section 20: a plain case of an enum whose cases carry data is the constructor without fields.
                DeclarationKind::EnumCase | DeclarationKind::CaseObject => {
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

        if let Some(extension) = self.checked.types().extension(expression) {
            if !self.is_translated_class(expression.span(), extension.class.as_bytes()) {
                return "default".to_owned();
            }

            let class = extension.class.as_bytes();
            let receiver = self.value(access.object);
            return self.call(expression, class, class, extension.name.as_bytes(), Some(receiver), None);
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
        // Spec section 20: a case that carries data is a constructor, whose fields Lean reads only by matching on it.
        if let Some(separator) = class.iter().rposition(|&byte| byte == b'\\')
            && let (r#enum, case) = (&class[..separator], &class[separator + 1..])
            && self.checked.types().enum_case_class(r#enum, case).is_some()
        {
            let place = format!(
                "{}.{}.{}",
                names::short_name(r#enum),
                String::from_utf8_lossy(case),
                String::from_utf8_lossy(property)
            );

            return self.unmodeled(expression.span(), format!("the field {place} of a case"), issues::CASE_FIELD);
        }
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
            return self.unmodeled(expression.span(), format!("the member {place}"), issues::MEMBER);
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
            return self.unmodeled(access.property.span(), "a dynamic member name".to_owned(), issues::MEMBER_NAME);
        };
        if let Some(extension) = self.checked.types().extension(expression) {
            if !self.is_translated_class(expression.span(), extension.class.as_bytes()) {
                return "default".to_owned();
            }

            let class = extension.class.as_bytes();
            return self.null_safe(expression, class, class, extension.name.as_bytes(), access.object, None);
        }
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
                        issues::RECEIVER,
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

    /// A call of `class`'s method `method`, which a subclass or, on an interface, each implementation gives its own body,
    /// so Lean cannot see which body runs, nor whether it has effects.
    fn overridable(&mut self, expression: &Expression, class: &[u8], method: &[u8]) -> String {
        let place = format!("{}.{}", names::short_name(class), String::from_utf8_lossy(method));
        if self.checked.types().class_declaration(class).kind == DeclarationKind::Interface {
            return self.unmodeled(
                expression.span(),
                format!("the interface method {place}"),
                issues::INTERFACE_METHOD,
            );
        }

        self.unmodeled(expression.span(), format!("a call to the overridable method {place}"), issues::OVERRIDABLE)
    }

    /// The Lean type of the class `class` a hint names.
    pub(crate) fn class_type(&mut self, span: Span, class: &[u8]) -> String {
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
        self.use_of(class, key, place, None);
    }

    /// [`Body::uses`], for a call that passes the `smaller` value.
    fn use_of(&mut self, class: &[u8], key: &str, place: String, smaller: Option<Smaller>) {
        let file = self.reach.class(class).map(|reached| reached.file);
        self.uses.push(Use { key: key.to_owned(), file, place, smaller });
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

    pub(super) fn kind(&self, r#type: &TUnion) -> Kind {
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
    pub(super) fn bind(&mut self, effect: String) -> String {
        self.temporaries += 1;
        let temporary = format!("«${}»", self.temporaries);
        self.lines.push(format!("let {temporary} ← {effect}"));

        temporary
    }

    /// The statements `translate` adds, apart from the body's own, and the term it returns.
    pub(super) fn nested(&mut self, translate: impl FnOnce(&mut Self) -> String) -> (Vec<String>, String) {
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

    /// The value of the branch `choice` runs. A choice whose branches only return a pure term is a term, and any other
    /// is bound from a nested `do` block, which runs only its own branch's effects.
    fn choose(&mut self, choice: &Choice) -> String {
        if let Some(term) = term(choice) {
            return term;
        }

        let temporary = self.fresh();
        self.lines.push(format!("let {temporary} ← do"));
        self.lines.extend(indented(&statements(choice), 4));

        temporary
    }

    /// A name no PHP# name is: `«$n»`.
    pub(super) fn fresh(&mut self) -> String {
        self.temporaries += 1;

        format!("«${}»", self.temporaries)
    }
}

/// `choice` as a pure term, when each of its branches only returns one.
fn term(choice: &Choice) -> Option<String> {
    Some(match choice {
        Choice::Branch(branch) => match branch.as_slice() {
            [only] => only.strip_prefix("pure ")?.to_owned(),
            _ => return None,
        },
        Choice::If(condition, then, otherwise) => {
            format!("(if {condition} then {} else {})", term(then)?, term(otherwise)?)
        }
        Choice::Match(matched, alternatives) => {
            let mut arms = Vec::new();
            for (pattern, branch) in alternatives {
                arms.push(format!("| {pattern} => {}", term(branch)?));
            }
            format!("(match {matched} with {})", arms.join(" "))
        }
        Choice::Let(name, value, branch) => format!("(let {name} := {value}; {})", term(branch)?),
    })
}

/// `choice` as the statements of a `do` block.
fn statements(choice: &Choice) -> Vec<String> {
    match choice {
        Choice::Branch(branch) if branch.is_empty() => vec!["pure ()".to_owned()],
        Choice::Branch(branch) => branch.clone(),
        Choice::If(condition, then, otherwise) => {
            let mut lines = vec![format!("if {condition} then")];
            lines.extend(indented(&statements(then), 2));
            lines.push("else".to_owned());
            lines.extend(indented(&statements(otherwise), 2));
            lines
        }
        Choice::Match(matched, alternatives) => {
            let mut lines = vec![format!("match {matched} with")];
            for (pattern, branch) in alternatives {
                lines.push(format!("| {pattern} =>"));
                lines.extend(indented(&statements(branch), 2));
            }
            lines
        }
        Choice::Let(name, value, branch) => {
            let mut lines = vec![format!("let {name} := {value}")];
            lines.extend(statements(branch));
            lines
        }
    }
}

/// `expression` without the parentheses around it.
fn unparenthesized<'expression, 'arena>(
    expression: &'expression Expression<'arena>,
) -> &'expression Expression<'arena> {
    match expression {
        Expression::Parenthesized(parenthesized) => unparenthesized(parenthesized.expression),
        expression => expression,
    }
}

/// The value of an int literal, negated or not.
pub(super) fn literal(expression: &Expression) -> Option<i64> {
    match unparenthesized(expression) {
        Expression::Literal(Literal::Integer(integer)) => integer.value.and_then(|value| i64::try_from(value).ok()),
        Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Negation(_), operand }) => {
            literal(operand)?.checked_neg()
        }
        _ => None,
    }
}

/// Whether `statement` always returns: a `return`, or a block that ends in one.
fn returns(statement: &Statement) -> bool {
    match statement {
        Statement::Return(_) => true,
        Statement::Block(block) => block.statements.last().is_some_and(returns),
        _ => false,
    }
}

/// Whether `pattern` names a value it matches, which exists only where the pattern holds.
fn names_values(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Type(pattern) => {
            pattern.variable.is_some()
                || pattern.fields.is_some_and(|fields| fields.fields.iter().any(|field| names_values(field)))
                || pattern.properties.is_some_and(|properties| {
                    properties.properties.iter().any(|property| names_values(property.pattern))
                })
        }
        Pattern::Not(not) => names_values(not.pattern),
        Pattern::Binary(binary) => names_values(binary.left) || names_values(binary.right),
        Pattern::Parenthesized(parenthesized) => names_values(parenthesized.pattern),
        Pattern::Properties(properties) => properties.properties.iter().any(|property| names_values(property.pattern)),
        Pattern::List(list) => list.elements.iter().any(|element| match element {
            ListPatternElement::Pattern(pattern) => names_values(pattern),
            ListPatternElement::Rest(rest) => rest.variable.is_some(),
        }),
        Pattern::Value(_) | Pattern::Comparison(_) => false,
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
