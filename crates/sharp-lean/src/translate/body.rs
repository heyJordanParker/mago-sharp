//! A method's or law's body as the statements of a Lean `do` block in `M`.
//!
//! Every value that can throw is bound to a temporary `«$n»` before it is used, as A-normal form binds it, so the
//! effects run in the order PHP# runs them and every other term is pure. A branch that runs only on one side of a
//! condition is a nested `do` block, so its effects run only there.

use std::borrow::Cow;
use std::ops::Range;

use foldhash::HashSet;
use mago_analyzer::smaller::Path;
use mago_analyzer::smaller::Smaller;
use mago_analyzer::smaller::Step;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::reference::TReference;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::php_operator_name;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::DeclarationKind;
use mago_sharp_bridge::Equality;
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
use mago_syntax::cst::Law as LawNode;
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
use mago_word::Word;
use mago_word::word;

use crate::issues;
use crate::reach::Reach;
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
    /// The parameters of a method whose calls of its own class may recurse, and what the conditions and patterns on the
    /// path to the code being translated show about them.
    path: Path<'body, 'program>,
    /// The locals the loop being translated adds to, by where their declarations' names start, which its conditions
    /// and the values it adds must not read.
    pub(super) accumulators: Vec<u32>,
    /// Whether the body computes ints exactly, as [`Body::computes_exactly`] says.
    exact: bool,
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
            path: Path::new(checked.names(), []),
            accumulators: Vec::new(),
            exact: false,
        }
    }

    /// The translation of code of `law`, a law of `class`: its `when` condition, its claim or its body, which read its
    /// parameters, and whose calls of the law itself pass smaller values of them.
    pub(crate) fn for_law(
        checked: &'body CheckedProgram<'program>,
        reach: &'body Reach,
        class: &'body [u8],
        place: String,
        law: &LawNode,
    ) -> Self {
        let mut body = Self::new(checked, reach, class, place);
        body.takes(law.parameter_list.parameters.iter());
        body.recurse_on(law.parameter_list.parameters.iter());

        body
    }

    /// Reads each of `parameters` whose type takes `null` as Lean's `Option`, and as its value where the analysis
    /// typed it as a value.
    pub(crate) fn takes<'parameter>(
        &mut self,
        parameters: impl IntoIterator<Item = &'parameter FunctionLikeParameter<'parameter>>,
    ) {
        for parameter in parameters {
            if matches!(parameter.hint, Some(Hint::Nullable(_))) {
                self.nullable_locals.insert(parameter.variable.span.start.offset);
            }
        }
    }

    /// Lets the body's calls of methods of its own class pass smaller values of `parameters`, in order.
    pub(crate) fn recurse_on<'parameter>(
        &mut self,
        parameters: impl IntoIterator<Item = &'parameter FunctionLikeParameter<'parameter>>,
    ) {
        self.path = Path::new(self.checked.names(), parameters);
    }

    /// Lets the body's calls pass the smaller values `path`, a path of a law's body, shows.
    pub(crate) fn on_path(&mut self, path: Path<'body, 'program>) {
        self.path = path;
    }

    /// Computes ints exactly, as a law's claim and condition do (decision 057).
    pub(crate) fn computes_exactly(&mut self) {
        self.exact = true;
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
            Hint::Self_(_) => self.named_type(hint.span(), self.class),
            Hint::Identifier(identifier) if self.checked.names().is_type_parameter(identifier) => self.unmodeled(
                hint.span(),
                format!("the type parameter {}", String::from_utf8_lossy(identifier.value())),
                issues::TYPE,
            ),
            Hint::Identifier(identifier) => {
                let class = self.checked.names().get(identifier);

                self.named_type(hint.span(), class)
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
                    self.named_type(hint.span(), name)
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

    /// The `do` statements of an expression body, which returns the expression's value.
    pub(crate) fn expression_body(mut self, expression: &Expression) -> (Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        let value = self.value(expression);
        self.lines.push(format!("return {value}"));

        (self.lines, self.uses, self.unmodeled)
    }

    /// The `do` statements that compute `values` after the local declarations `locals`, and the term of each value.
    pub(crate) fn terms(
        mut self,
        locals: &[&Statement],
        values: &[&Expression],
    ) -> (Vec<String>, Vec<String>, Vec<Use>, Vec<Unmodeled>) {
        for local in locals {
            self.statement(local);
        }
        let terms = values.iter().map(|value| self.value(value)).collect();

        (self.lines, terms, self.uses, self.unmodeled)
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
        let outer = self.path.clone();
        for statement in statements {
            self.statement(statement);
            if let Statement::If(r#if) = statement
                && let IfBody::Statement(body) = &r#if.body
                && body.else_clause.is_none()
                && returns(body.statement)
            {
                self.path = self.path.branch(r#if.condition, false);
            }
        }
        self.path = outer;
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
                let holds = self.path.branch(r#if.condition, true);
                // `if (x is Money m)` names `m` where the test holds, so it is a `match` on `x`.
                if let Expression::Is(is) = r#if.condition.unparenthesized() {
                    let Some(test) = self.is_test(is) else {
                        return;
                    };
                    let then = self.under(holds, |this| this.nested_statement(body.statement));
                    let otherwise = body.else_clause.as_ref().map(|clause| self.nested_statement(clause.statement));
                    let choice = choice(vec![(test, then)], otherwise.unwrap_or_default());
                    self.lines.extend(statements(&choice));
                    return;
                }
                let condition = self.value(r#if.condition);
                let then = self.under(holds, |this| this.nested_statement(body.statement));
                self.lines.push(format!("if {condition} then"));
                self.lines.extend(indented(&then, 2));
                if let Some(clause) = &body.else_clause {
                    let fails = self.path.branch(r#if.condition, false);
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
            Expression::NameOf(name_of) => names::string(name_of.named()),
            Expression::ConstantAccess(name) => self.name(expression, name),
            Expression::Binary(binary) => self.binary(expression, binary),
            Expression::UnaryPrefix(unary) => self.prefix(unary),
            Expression::Conditional(conditional) => match conditional.then {
                Some(then) => {
                    let holds = self.path.branch(conditional.condition, true);
                    if let Expression::Is(is) = conditional.condition.unparenthesized() {
                        let Some(test) = self.is_test(is) else {
                            return "default".to_owned();
                        };
                        let then = self.under(holds, |body| body.branch(then));
                        let otherwise = self.branch(conditional.r#else);

                        return self.choose(&choice(vec![(test, then)], otherwise));
                    }
                    let condition = self.value(conditional.condition);
                    let then = self.under(holds, |body| body.branch(then));
                    let fails = self.path.branch(conditional.condition, false);
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
            // Spec section 12: a `List` read gives the element at the index, and throws `OutOfRangeException` at an
            // index outside the list, as the engine does.
            Expression::ArrayAccess(access) if self.is_list(access.array) => {
                let list = self.value(access.array);
                let index = self.value(access.index);

                self.bind(format!("int.read {list} {index}"))
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
            // A throw ends the code there with `Throwable.Thrown`, which proves nothing about a law, as an engine error
            // does. Its value, which no code reads, is a stand-in.
            Expression::Throw(_) => {
                self.lines.push("throw Throwable.Thrown".to_owned());
                "default".to_owned()
            }
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
            Some(Binding::Member) => {
                self.field("this".to_owned(), word(self.class), word(name.name.value()), expression)
            }
            _ => {
                let what = format!("the constant `{}`", self.source(name.name.span()));
                self.unmodeled(expression.span(), what, issues::CONSTANT)
            }
        }
    }

    fn binary(&mut self, expression: &Expression, binary: &Binary) -> String {
        match binary.operator {
            BinaryOperator::And(_) | BinaryOperator::LowAnd(_) => return self.short_circuit(binary, true),
            BinaryOperator::Or(_) => return self.short_circuit(binary, false),
            BinaryOperator::NullCoalesce(_) => return self.coalesce(binary),
            _ => {}
        }
        if self.exact
            && let Some(exact) = self.exact_arithmetic(binary)
        {
            return self.bind(format!("int.ofInt {exact}"));
        }

        let (lhs, rhs) = if self.exact && binary.operator.is_comparison() && self.on_ints(binary) {
            (self.exact_int(binary.lhs), self.exact_int(binary.rhs))
        } else {
            (self.value(binary.lhs), self.value(binary.rhs))
        };
        let types = self.checked.types();
        let kinds = (self.kind(types.expression_type(binary.lhs)), self.kind(types.expression_type(binary.rhs)));
        if kinds.0 == Kind::Float || kinds.1 == Kind::Float {
            return self.unmodeled(binary.span(), "a float".to_owned(), issues::FLOAT);
        }

        let operator = self.source(binary.operator.span());
        let ints = kinds == (Kind::Int, Kind::Int);
        let ordered = ints || kinds == (Kind::String, Kind::String);
        if let Some(compared) = self.compared(expression, binary, &lhs, &rhs) {
            return compared;
        }
        match binary.operator {
            BinaryOperator::Spaceship(_) if ints => {
                format!("(if {lhs} < {rhs} then (-1 : int) else if {lhs} > {rhs} then (1 : int) else (0 : int))")
            }
            BinaryOperator::Addition(_) if ints => self.bind(format!("int.add {lhs} {rhs}")),
            BinaryOperator::Addition(_) if kinds == (Kind::String, Kind::String) => format!("({lhs} ++ {rhs})"),
            BinaryOperator::Subtraction(_) if ints => self.bind(format!("int.sub {lhs} {rhs}")),
            BinaryOperator::Multiplication(_) if ints => self.bind(format!("int.mul {lhs} {rhs}")),
            BinaryOperator::Division(_) if ints => self.bind(format!("int.div {lhs} {rhs}")),
            BinaryOperator::Modulo(_) if ints => self.bind(format!("int.mod {lhs} {rhs}")),
            // `===` works on objects only, and asks whether both are the same one.
            BinaryOperator::Identical(_) | BinaryOperator::NotIdentical(_) => {
                self.unmodeled(binary.span(), format!("the identity test `{operator}`"), issues::IDENTITY)
            }
            BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_) => self.equal(
                binary.span(),
                (types.expression_type(binary.lhs), types.expression_type(binary.rhs)),
                (&lhs, &rhs),
                binary.operator.is_negated_equality(),
            ),
            BinaryOperator::LessThan(_) if ordered => format!("(decide ({lhs} < {rhs}))"),
            BinaryOperator::LessThanOrEqual(_) if ordered => format!("(decide ({lhs} <= {rhs}))"),
            // `>` and `>=` stay Lean's `>` and `≥`, so a goal prints them as the code writes them.
            BinaryOperator::GreaterThan(_) if ordered => format!("(decide ({lhs} > {rhs}))"),
            BinaryOperator::GreaterThanOrEqual(_) if ordered => format!("(decide ({lhs} >= {rhs}))"),
            _ => self.unmodeled(binary.span(), format!("the operator `{operator}` here"), issues::OPERATOR),
        }
    }

    /// `<=>`, `<`, `<=`, `>` or `>=` of `lhs` and `rhs`, two values of one class, as the `<=>` the analysis chose for
    /// `expression` orders them, spec section 19: the class's own, or the one of the sealed interface it implements,
    /// whose values have the interface's type. None for any other operator or operands.
    fn compared(&mut self, expression: &Expression, binary: &Binary, lhs: &str, rhs: &str) -> Option<String> {
        let test = match binary.operator {
            BinaryOperator::Spaceship(_) => None,
            BinaryOperator::LessThan(_) => Some("<"),
            BinaryOperator::LessThanOrEqual(_) => Some("<="),
            BinaryOperator::GreaterThan(_) => Some(">"),
            BinaryOperator::GreaterThanOrEqual(_) => Some(">="),
            _ => return None,
        };
        let types = self.checked.types();
        let ([TAtomic::Object(TObject::Named(left))], [TAtomic::Object(TObject::Named(right))]) =
            (types.expression_type(binary.lhs).types.as_ref(), types.expression_type(binary.rhs).types.as_ref())
        else {
            return None;
        };
        let class = left.name.as_bytes();
        let comparison = php_operator_name(&BinaryOperator::Spaceship(binary.operator.span()), 2)?;
        if !self.reach.same_class(class, right.name.as_bytes()) {
            return None;
        }
        let owner = types.call_target(expression).class;
        let owner = owner.as_bytes();
        let held = self.reach.same_class(owner, class) || self.implements(class, owner);
        if !held || self.reach.method(owner, comparison).is_none() {
            return None;
        }

        let compared = self.operator(owner, comparison, "<=>", (lhs, rhs));

        Some(match test {
            Some(test) => format!("(decide ({compared} {test} (0 : int)))"),
            None => compared,
        })
    }

    /// `==`, or `!=` when `negated`, of the values `lhs` and `rhs` of the types `types`, as spec section 19 compares
    /// them, the way the bridge's [`Equality`] tells: Lean's `==` on values that hold no object, and the `==` a class
    /// declares, or a struct gets with no code, as its `op_Equality` gives it. A nullable side is lifted: null equals
    /// only null, and two values run that `==`. `span` is where a refusal stands.
    pub(crate) fn equal(
        &mut self,
        span: Span,
        types: (&TUnion, &TUnion),
        (lhs, rhs): (&str, &str),
        negated: bool,
    ) -> String {
        let (what, reason) = match self.checked.types().equality(types.0, types.1) {
            Equality::Identical if negated => return format!("({lhs} != {rhs})"),
            Equality::Identical => return format!("({lhs} == {rhs})"),
            Equality::Declared(owner, name) => {
                if !self.is_translated_class(span, owner.as_bytes()) {
                    return "default".to_owned();
                }
                let equal =
                    self.lifted(owner.as_bytes(), name, (types.0.is_nullable(), types.1.is_nullable()), (lhs, rhs));
                return if negated { format!("(!{equal})") } else { equal };
            }
            Equality::Elements => {
                (format!("the class operator `{}`", if negated { "!=" } else { "==" }), issues::CLASS_OPERATOR)
            }
            Equality::Numbers => ("a float".to_owned(), issues::FLOAT),
            Equality::Tables => ("a collection".to_owned(), issues::COLLECTION),
            Equality::Running => {
                let operator = if negated { "!=" } else { "==" };
                (format!("`{operator}` on values only the running code tells apart"), issues::TYPE_TEST)
            }
        };

        self.unmodeled(span, what, reason)
    }

    /// `==` of `lhs` and `rhs` by the `==` `class` declares as the static method `name`, where `nullable` tells which
    /// side may be null, as spec section 19 lifts it: null equals only null, and two values run the method.
    fn lifted(&mut self, class: &[u8], name: &[u8], nullable: (bool, bool), (lhs, rhs): (&str, &str)) -> String {
        let (held, absent) = match nullable {
            (false, false) => return self.operator(class, name, "==", (lhs, rhs)),
            (true, true) => (format!("({lhs}.isSome && {rhs}.isSome)"), format!("pure ({lhs}.isNone && {rhs}.isNone)")),
            (true, false) => (format!("{lhs}.isSome"), "pure false".to_owned()),
            (false, true) => (format!("{rhs}.isSome"), "pure false".to_owned()),
        };
        let value =
            |side: &str, nullable: bool| if nullable { format!("({side}.getD default)") } else { side.to_owned() };
        let values = (value(lhs, nullable.0), value(rhs, nullable.1));
        let present = self.nested_lines(|body| body.operator(class, name, "==", (&values.0, &values.1)));

        self.choose(&Choice::If(held, Box::new(Choice::Branch(present)), Box::new(Choice::Branch(vec![absent]))))
    }

    /// Whether the field `field`, of the declared type `r#type`, holds equal values in `a` and `b`, two values of the
    /// struct `class`, by the field's own `==`, spec section 19. An implementer of a sealed interface reads it through
    /// the interface's accessor. A class the type names is the enum or the object it names, as the codex populates a
    /// declared type, and a type that names one inside another, as `List<Instant>`, keeps the class operator refusal.
    pub(crate) fn fields_equal(&mut self, span: Span, class: &[u8], field: &[u8], r#type: &TUnion) -> String {
        let mut populated = Vec::new();
        for atomic in r#type.types.iter() {
            populated.push(match atomic {
                TAtomic::Reference(TReference::Symbol { name, parameters: None, intersection_types: None, .. }) => {
                    match self.checked.types().class_declaration(*name).kind {
                        DeclarationKind::Enum { .. } => TAtomic::Object(TObject::new_enum(*name)),
                        _ => TAtomic::Object(TObject::Named(TNamedObject::new(*name))),
                    }
                }
                atomic
                    if !atomic
                        .get_all_child_nodes()
                        .iter()
                        .any(|node| matches!(node, TypeRef::Atomic(TAtomic::Reference(_)))) =>
                {
                    atomic.clone()
                }
                _ => return self.unmodeled(span, "the class operator `==`".to_owned(), issues::CLASS_OPERATOR),
            });
        }
        let r#type = TUnion::from_vec(populated);

        let mut values = Vec::new();
        for side in ["a", "b"] {
            values.push(match self.sealed(span, class) {
                Some((_, constructor)) => {
                    let key = names::member(&constructor, field);
                    self.uses(class, &key, format!("{}.{}", names::short_name(class), String::from_utf8_lossy(field)));
                    format!("({key} {side})")
                }
                None => format!("{side}.{}", names::identifier(field)),
            });
        }

        self.equal(span, (&r#type, &r#type), (&values[0], &values[1]), false)
    }

    /// The call of the operator `class` declares, or gets with no code, as the static method `name` it runs as, spec
    /// section 19, on `lhs` and `rhs`, bound to a temporary. `symbol` is the operator as the code writes it.
    fn operator(&mut self, class: &[u8], name: &[u8], symbol: &str, (lhs, rhs): (&str, &str)) -> String {
        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), name);
        self.uses(class, &key, format!("{}.operator {symbol}", names::short_name(&class_name)));

        self.bind(format!("{key} {lhs} {rhs}"))
    }

    /// `+`, `-` or `*` of two non-nullable ints as the exact `Int` it computes.
    fn exact_arithmetic(&mut self, binary: &Binary) -> Option<String> {
        let operator = match binary.operator {
            BinaryOperator::Addition(_) => "+",
            BinaryOperator::Subtraction(_) => "-",
            BinaryOperator::Multiplication(_) => "*",
            _ => return None,
        };
        if !self.on_ints(binary) {
            return None;
        }
        let lhs = self.exact_int(binary.lhs);
        let rhs = self.exact_int(binary.rhs);

        Some(format!("({lhs} {operator} {rhs})"))
    }

    /// `expression`, an int, as the exact `Int` it computes.
    fn exact_int(&mut self, expression: &Expression) -> String {
        match expression {
            Expression::Parenthesized(parenthesized) => self.exact_int(parenthesized.expression),
            Expression::Literal(Literal::Integer(integer)) => {
                match integer.value.and_then(|value| i64::try_from(value).ok()) {
                    Some(value) => format!("({value} : Int)"),
                    None => format!("{}.toInt", self.value(expression)),
                }
            }
            Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Negation(_), operand }) => {
                format!("(-{})", self.exact_int(operand))
            }
            Expression::Binary(binary) => match self.exact_arithmetic(binary) {
                Some(exact) => exact,
                None => format!("{}.toInt", self.value(expression)),
            },
            _ => format!("{}.toInt", self.value(expression)),
        }
    }

    /// Whether both operands of `binary` are ints that are never null, which have an exact `Int` to read.
    fn on_ints(&self, binary: &Binary) -> bool {
        [binary.lhs, binary.rhs].into_iter().all(|operand| {
            let r#type = self.checked.types().expression_type(operand);

            self.kind(r#type) == Kind::Int && !r#type.is_nullable()
        })
    }

    /// `&&` and `||`, which run their right side only when the left one does not decide.
    fn short_circuit(&mut self, binary: &Binary, and: bool) -> String {
        let lhs = self.value(binary.lhs);
        let path = self.path.branch(binary.lhs, and);
        let rhs = self.under(path, |body| body.branch(binary.rhs));
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
                let r#type = self.checked.types().expression_type(unary.operand);
                match self.kind(r#type) {
                    Kind::Int if self.exact && !r#type.is_nullable() => {
                        let operand = self.exact_int(unary.operand);
                        self.bind(format!("int.ofInt (-{operand})"))
                    }
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
        let (paths, _) = self.path.arms(r#match.expression, r#match.arms.as_slice());
        let mut arms = Vec::new();
        let mut otherwise = vec!["throw Throwable.UnhandledMatchError".to_owned()];
        for (arm, path) in r#match.arms.iter().zip(paths) {
            let PatternMatchArm::Pattern(arm) = arm else {
                otherwise = self.under(path, |body| translate(body, arm.body()));
                continue;
            };

            let mut test = self.test(&subject, None, r#type, arm.pattern)?;
            if let Some(guard) = &arm.guard {
                test.conditions.push(self.pure(guard.condition, "a guard that can throw", issues::GUARD)?);
            }
            arms.push((test, self.under(path, |body| translate(body, &arm.body))));
        }

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

        self.test(&subject, None, r#type, is.pattern)
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
            TAtomic::Object(TObject::Named(named)) => self.reach.same_class(named.name.as_bytes(), class),
            TAtomic::Object(TObject::Enum(r#enum)) => self.reach.same_class(r#enum.name.as_bytes(), class),
            _ => false,
        }
    }

    /// The Lean constructor of the case of an enum `hint` names, when `atomic` is that enum or the sealed interface it
    /// implements, and with it the interface's constructor that holds the enum's value, when it implements one.
    pub(super) fn case(&mut self, hint: &Hint, atomic: &TAtomic) -> Option<(Option<String>, String)> {
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
        if !self.reach.same_class(named, r#enum) && !self.implements(r#enum, named) {
            return None;
        }
        self.checked.types().enum_case_class(word(r#enum), word(case))?;
        self.reads(r#enum);
        let held = self.sealed(hint.span(), r#enum).map(|(_, constructor)| constructor);

        Some((held, names::member(&names::full_name(&self.reached_name(r#enum)), case)))
    }

    /// The Lean constructor of the sealed interface `atomic` is that holds a value of the implementer `hint` names,
    /// spec section 20.1.
    pub(super) fn implementer(&mut self, hint: &Hint, atomic: &TAtomic) -> Option<String> {
        let (Hint::Identifier(identifier), TAtomic::Object(TObject::Named(interface))) = (hint, atomic) else {
            return None;
        };
        let class = self.checked.names().get(identifier);
        if !self.implements(class, interface.name.as_bytes()) {
            return None;
        }

        self.sealed(hint.span(), class).map(|(_, constructor)| constructor)
    }

    /// Whether `class` implements the sealed interface `interface`, spec section 20.1.
    fn implements(&self, class: &[u8], interface: &[u8]) -> bool {
        self.reach.sealed_interfaces(class).iter().any(|sealed| self.reach.same_class(sealed.as_bytes(), interface))
    }

    /// The name of the interface `atomic` is, when it is not sealed, so its implementers are open.
    pub(super) fn open_interface(&self, atomic: &TAtomic) -> Option<String> {
        let TAtomic::Object(TObject::Named(interface)) = atomic else {
            return None;
        };

        (self.checked.types().class_declaration(interface.name).kind == DeclarationKind::Interface
            && self.reach.implementers(interface.name.as_bytes()).is_none())
        .then(|| names::short_name(interface.name.as_bytes()))
    }

    /// `translate` on `path`, a path into a branch of the code being translated.
    fn under<T>(&mut self, path: Path<'body, 'program>, translate: impl FnOnce(&mut Self) -> T) -> T {
        let outer = std::mem::replace(&mut self.path, path);
        let translated = translate(self);
        self.path = outer;

        translated
    }

    /// The condition that `subject`, a value of `kind`, matches `pattern`, or `None` after recording why it has none.
    pub(super) fn pattern(&mut self, subject: &str, kind: Kind, pattern: &Pattern) -> Option<String> {
        match pattern {
            // A value an object is compared with is `null`: a pattern takes constants, and an object's identity is
            // nothing a law reasons about.
            Pattern::Value(value)
                if matches!(kind, Kind::Object | Kind::Other)
                    && !matches!(value.unparenthesized(), Expression::Literal(Literal::Null(_))) =>
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
                    BinaryOperator::GreaterThan(_) if kind == Kind::Int => format!("(decide ({subject} > {value}))"),
                    BinaryOperator::GreaterThanOrEqual(_) if kind == Kind::Int => {
                        format!("(decide ({subject} >= {value}))")
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
    pub(super) fn pure(&mut self, expression: &Expression, what: &str, reason: &'static str) -> Option<String> {
        let (lines, value) = self.code_term(expression);
        if lines.is_empty() {
            return Some(value);
        }

        self.unmodeled(expression.span(), what.to_owned(), reason);
        None
    }

    /// The statements and the term of `expression` as code computes it, with ints in their range: the pure term a
    /// guard, a pattern or a lambda holds, which must not bind a value.
    fn code_term(&mut self, expression: &Expression) -> (Vec<String>, String) {
        let exact = std::mem::replace(&mut self.exact, false);
        let term = self.nested(|body| body.value(expression));
        self.exact = exact;

        term
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
            // Spec section 23: `Pdf.render(…)` calls a plain PHP function through its namespace.
            if self.checked.types().namespace_function(expression).is_some() {
                let what =
                    format!("the plain PHP function `{}`", self.source(call.object.span().join(call.method.span())));
                return self.unmodeled(expression.span(), what, issues::PLAIN_PHP);
            }
            let class = names.get(&class.name);
            if let Some(lean) = self.checked.types().lean_function(expression) {
                return self.folded(&String::from_utf8_lossy(lean.as_bytes()), &call.argument_list);
            }
            if !self.is_translated_class(call.object.span(), class) {
                return "default".to_owned();
            }
            // Spec section 20: `PaymentResult.Paid(…)` creates the case, which is its constructor applied to the
            // fields in order, a value and no call.
            if let Some(case) = self.method_name(&call.method)
                && self.checked.types().enum_case_class(word(class), word(case)).is_some()
            {
                let class_name = self.reached_name(class);
                let lean = names::full_name(&class_name);
                let Some(arguments) = self.arguments(&call.argument_list, None) else {
                    return "default".to_owned();
                };
                self.uses(class, &lean, names::short_name(&class_name));

                let mut constructor = vec![names::member(&lean, case)];
                constructor.extend(arguments);

                return self.held(expression.span(), class, format!("({})", constructor.join(" ")));
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
                let target = self.checked.types().member_declaration(word(self.class), word(method));
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
            // Spec section 22: `super.m(…)` runs the method the parent declares or inherits.
            Expression::Parent(_) => {
                let Some(method) = self.method_name(&call.method) else {
                    return "default".to_owned();
                };
                let target = self.checked.types().call_target(expression);
                let receiver = (target.kind != DeclarationKind::StaticMethod).then(|| "this".to_owned());

                return self.call(
                    expression,
                    target.class.as_bytes(),
                    self.class,
                    method,
                    receiver,
                    Some(&call.argument_list),
                );
            }
            Expression::Static(_) => {
                return self.unmodeled(expression.span(), "a call through `static`".to_owned(), issues::OVERRIDABLE);
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

            let class = extension.class;
            let receiver = self.value(call.object);
            return self.call(
                expression,
                class.as_bytes(),
                class.as_bytes(),
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
                return self.overridable(expression, word(class), target.name.as_bytes());
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
    /// or that length checked into the int range in a body that computes exactly, and `map` and `filter` of a lambda
    /// are `List.map` and `List.filter`. Every other method of a collection is refused by name.
    fn list_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let arguments = &call.argument_list.arguments;
        match (method, arguments.len()) {
            (b"count", 0) => {
                let list = self.value(call.object);
                if self.exact {
                    return self.bind(format!("int.ofInt (List.length {list} : Int)"));
                }

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

    /// A call of a standard library method marked `LeanFunction`, as `Math.max(…)` is Lean's `max` on `int`, as the Lean
    /// function `lean` folded over the arguments. The checker knows what it computes, so a law reasons about it as it
    /// does about `<`, and it needs no `extern`. Each argument after the first joins with `lean`, and a spread one joins
    /// each element of its List.
    fn folded(&mut self, lean: &str, list: &ArgumentList) -> String {
        let mut joined: Option<String> = None;
        for argument in &list.arguments {
            let Argument::Positional(argument) = argument else {
                return self.unmodeled(argument.span(), "a named or spread argument".to_owned(), issues::ARGUMENT);
            };
            let value = self.value(argument.value);
            joined = Some(match (joined, argument.ellipsis.is_some()) {
                (None, false) => value,
                (Some(joined), false) => format!("({lean} {joined} {value})"),
                (Some(joined), true) => format!("(List.foldl {lean} {joined} {value})"),
                (None, true) => {
                    return self.unmodeled(argument.span(), "a named or spread argument".to_owned(), issues::ARGUMENT);
                }
            });
        }

        joined.unwrap_or_else(|| {
            unreachable!("a method marked `LeanFunction(\"{lean}\")` takes an argument, as at {:?}", list.span())
        })
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
        let (lines, body) = self.code_term(arrow.expression);
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

            let class = extension.class;
            let method = extension.name.as_bytes();
            return self.null_safe(
                expression,
                class.as_bytes(),
                class.as_bytes(),
                method,
                call.object,
                Some(&call.argument_list),
            );
        }
        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        if target.kind != (DeclarationKind::Method { overridable: false }) {
            return self.overridable(expression, word(class), target.name.as_bytes());
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

    /// A call of `class`'s method `method`, which `declaring` declares, `class` itself or a class above it, on
    /// `receiver`, or of the static method when `receiver` is `None`. A computed property's read passes no `arguments`.
    fn call(
        &mut self,
        expression: &Expression,
        declaring: &[u8],
        class: &[u8],
        method: &[u8],
        receiver: Option<String>,
        arguments: Option<&ArgumentList>,
    ) -> String {
        if !self.reach.class(declaring).is_some_and(|reached| reached.sharp) {
            let plain = self.checked.types().class_declaration(word(declaring)).name;
            let what = format!(
                "the plain PHP method {}.{}",
                names::short_name(plain.as_bytes()),
                String::from_utf8_lossy(method)
            );
            return self.unmodeled(expression.span(), what, issues::PLAIN_PHP);
        }
        let Some(facts) = self.reach.method(declaring, method) else {
            return self.unmodeled(
                expression.span(),
                format!("the method {}.{}", names::short_name(declaring), String::from_utf8_lossy(method)),
                issues::BODYLESS,
            );
        };

        let class_name = self.reached_name(declaring);
        let key = names::member(&names::full_name(&class_name), facts.name.as_bytes());
        let mut parts = vec![key.clone()];
        parts.extend(receiver.map(|receiver| self.upcast(expression.span(), receiver, class, declaring)));
        let smaller =
            arguments.filter(|_| self.reach.same_class(declaring, self.class)).and_then(|list| self.smaller(list));
        let stepped = smaller.as_ref().and_then(|(smaller, term)| Some((smaller.argument, term.clone()?)));
        let Some(arguments) = arguments.map_or_else(|| Some(Vec::new()), |list| self.arguments(list, stepped)) else {
            return "default".to_owned();
        };
        let place = format!("{}.{}", names::short_name(&class_name), facts.name);
        let defaults = self.defaults(declaring, &key, &place, arguments.len()..facts.parameters);
        parts.extend(arguments);
        parts.extend(defaults);
        self.use_of(declaring, &key, place, smaller.map(|(smaller, _)| smaller));

        self.bind(parts.join(" "))
    }

    /// `value`, a value of `class`'s Lean type, as a value of the PHP# class `ancestor` above it, which its structure
    /// holds: `value.toParent`, once for each class up to `ancestor`. A value of a sealed interface's implementer reads
    /// its parent's value through the interface's accessor, spec section 20.1. A plain PHP class between them, whose
    /// state the structure does not hold, is refused at `span`.
    fn upcast(&mut self, span: Span, value: String, class: &[u8], ancestor: &[u8]) -> String {
        if self.reach.same_class(class, ancestor) {
            return value;
        }
        let mut upcast = value;
        let mut below = class.to_vec();
        let mut sealed = self.sealed(span, class);
        while !self.reach.same_class(&below, ancestor) {
            let Some(parent) = self.reach.class(&below).and_then(|reached| reached.parent) else {
                unreachable!("{} is no class above {}", names::short_name(ancestor), names::short_name(class));
            };
            if !self.reach.class(parent.as_bytes()).is_some_and(|reached| reached.sharp) {
                let what = format!("the plain PHP class {}", names::short_name(parent.as_bytes()));
                return self.unmodeled(span, what, issues::PLAIN_PHP);
            }
            let field = self.parent_field(parent.as_bytes());
            upcast = match sealed.take() {
                Some((_, constructor)) => {
                    let key = names::member(&constructor, field.as_bytes());
                    self.uses(class, &key, format!("{}.{field}", names::short_name(&self.reached_name(class))));
                    format!("({key} {upcast})")
                }
                None => {
                    self.reads(&below);
                    format!("({upcast}).{field}")
                }
            };
            below = parent.as_bytes().to_vec();
        }

        upcast
    }

    /// The field of a class's structure that holds the value of its PHP# parent `parent`, as Lean names the field of
    /// the structure a structure extends: `toParent`.
    pub(crate) fn parent_field(&self, parent: &[u8]) -> String {
        names::identifier(format!("to{}", names::short_name(&self.reached_name(parent))).as_bytes())
    }

    /// The smaller value a call of a method of the body's own class passes, by spec section 28's smaller value rule,
    /// and the pure term of an int one, which cannot leave the `int` range where a condition showed it is above or below
    /// its bound.
    pub(crate) fn smaller(&self, list: &ArgumentList) -> Option<(Smaller, Option<String>)> {
        let arguments: Vec<&Expression> = list.arguments.iter().map(Argument::value).collect();
        let (smaller, operand) = self.path.smaller(&arguments)?;
        let term = operand.map(|operand| {
            let name = names::identifier(self.source(operand.span()).as_bytes());
            if matches!(smaller.step, Step::Down(_)) { format!("({name} - 1)") } else { format!("({name} + 1)") }
        });

        Some((smaller, term))
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

        let made = self.construct(instantiation.span(), class, instantiation.argument_list.as_ref());

        self.held(instantiation.span(), class, made)
    }

    /// A value of `class`'s own type, which its constructor builds from `arguments`, as `new` and `: super(…)` run it.
    /// A constructor a plain PHP parent declares or inherits is plain PHP code, which a law cannot read.
    pub(crate) fn construct(&mut self, span: Span, class: &[u8], arguments: Option<&ArgumentList>) -> String {
        if let Some(plain) = self.reach.class(class).and_then(|reached| reached.plain_constructor) {
            let what = format!("the constructor of the plain PHP class {}", names::short_name(plain.as_bytes()));
            return self.unmodeled(span, what, issues::PLAIN_PHP);
        }

        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), b"__construct");
        let parameters = self.reach.method(class, b"__construct").map_or(0, |method| method.parameters);
        let Some(arguments) = arguments.map_or_else(|| Some(Vec::new()), |list| self.arguments(list, None)) else {
            return "default".to_owned();
        };
        let place = format!("new {}", names::short_name(&class_name));
        let defaults = self.defaults(class, &key, &place, arguments.len()..parameters);
        let mut parts = vec![key.clone()];
        parts.extend(arguments);
        parts.extend(defaults);
        self.uses(class, &key, place);

        self.bind(parts.join(" "))
    }

    /// The terms of a call's arguments, or `None` when one cannot be translated. Each argument is passed by position.
    /// `stepped` is an argument's position and the term that stands for it.
    fn arguments(&mut self, list: &ArgumentList, stepped: Option<(usize, String)>) -> Option<Vec<String>> {
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
        Some(arguments)
    }

    /// The default of each parameter at `positions` of the method `key` of `class`, spec section 7, which a call that
    /// leaves those arguments out passes, as the declarations [`names::default`] names.
    fn defaults(&mut self, class: &[u8], key: &str, place: &str, positions: Range<usize>) -> Vec<String> {
        positions
            .map(|position| {
                let default = names::default(key, position);
                self.uses(class, &default, place.to_owned());

                default
            })
            .collect()
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

            return match self.checked.types().member_declaration(word(class), word(member.value)).kind {
                // Spec section 20: a plain case of an enum whose cases carry data is the constructor without fields.
                DeclarationKind::EnumCase | DeclarationKind::CaseObject => {
                    self.uses(class, &lean, short);
                    self.held(expression.span(), class, names::member(&lean, member.value))
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

            let class = extension.class;
            let receiver = self.value(access.object);
            return self.call(
                expression,
                class.as_bytes(),
                class.as_bytes(),
                extension.name.as_bytes(),
                Some(receiver),
                None,
            );
        }
        let Some(class) = self.receiver_class(access.object) else {
            return "default".to_owned();
        };
        let object = self.value(access.object);

        self.field(object, word(class), word(member.value), expression)
    }

    /// The read of `class`'s property `property` from `object`: a field of the object's structure, or a backed enum's
    /// `value`.
    fn field(&mut self, object: String, class: Word, property: Word, expression: &Expression) -> String {
        // Spec section 20: a case that carries data is a constructor, and a value of the case reads its field as the
        // constructor's.
        if let Some(separator) = class.as_bytes().iter().rposition(|&byte| byte == b'\\')
            && let (r#enum, case) = (&class.as_bytes()[..separator], &class.as_bytes()[separator + 1..])
            && self.checked.types().enum_case_class(word(r#enum), word(case)).is_some()
        {
            let enum_name = self.reached_name(r#enum);
            let place = format!(
                "{}.{}.{}",
                names::short_name(&enum_name),
                String::from_utf8_lossy(case),
                String::from_utf8_lossy(property.as_bytes())
            );
            let key = names::member(&names::member(&names::full_name(&enum_name), case), property.as_bytes());
            self.uses(r#enum, &key, place);

            return format!("({key} {object})");
        }
        let class_name = self.reached_name(class.as_bytes());
        let short = names::short_name(&class_name);
        let place = format!("{short}.{}", String::from_utf8_lossy(property.as_bytes()));
        let declaration = self.checked.types().member_declaration(class, property);
        if matches!(self.checked.types().class_declaration(class).kind, DeclarationKind::Enum { backed: true })
            && property == word("value")
        {
            let key = names::member(&names::full_name(&class_name), b"value");
            self.uses(class.as_bytes(), &key, place);
            return format!("({key} {object})");
        }
        if !matches!(declaration.kind, DeclarationKind::Property { .. }) {
            return self.unmodeled(expression.span(), format!("the member {place}"), issues::MEMBER);
        }
        let declaring = declaration.class.as_bytes();
        if !self.reach.class(declaring).is_some_and(|reached| reached.sharp) {
            let what = format!(
                "the plain PHP property {}.{}",
                names::short_name(self.checked.types().class_declaration(declaration.class).name.as_bytes()),
                String::from_utf8_lossy(property.as_bytes())
            );
            return self.unmodeled(expression.span(), what, issues::PLAIN_PHP);
        }
        if !self.reach.is_stored_field(declaring, property.as_bytes()) {
            return self.unmodeled(expression.span(), format!("the accessor of {place}"), issues::ACCESSOR);
        }

        // Spec section 20.1: an implementer of a sealed interface is a value of the interface, and reads its property
        // through the interface's accessor, as a data case reads its field. A property a PHP# parent declares is a field
        // of the parent's value.
        let read = if !self.reach.same_class(declaring, class.as_bytes()) {
            let parent = self.upcast(expression.span(), object, class.as_bytes(), declaring);
            format!("{parent}.{}", names::identifier(property.as_bytes()))
        } else if let Some((_, constructor)) = self.sealed(expression.span(), class.as_bytes()) {
            let key = names::member(&constructor, property.as_bytes());
            self.uses(class.as_bytes(), &key, place);
            format!("({key} {object})")
        } else {
            self.reads(class.as_bytes());
            format!("{object}.{}", names::identifier(property.as_bytes()))
        };
        if self.reach.is_nullable_field(declaring, property.as_bytes())
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

            let class = extension.class;
            return self.null_safe(
                expression,
                class.as_bytes(),
                class.as_bytes(),
                extension.name.as_bytes(),
                access.object,
                None,
            );
        }
        let Some(class) = self.receiver_class(access.object) else {
            return "default".to_owned();
        };
        let object = self.value(access.object);
        let read = self.field("present".to_owned(), word(class), word(member.value), expression);
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
        if !self.reach.class(class).is_some_and(|reached| reached.sharp) {
            self.unmodeled(span, format!("the plain PHP class {}", names::short_name(class)), issues::PLAIN_PHP);
            return false;
        }

        true
    }

    /// A call of `class`'s method `method`, which a subclass or, on an interface, each implementation gives its own body,
    /// so Lean cannot see which body runs, nor whether it has effects.
    fn overridable(&mut self, expression: &Expression, class: Word, method: &[u8]) -> String {
        let place = format!("{}.{}", names::short_name(class.as_bytes()), String::from_utf8_lossy(method));
        if self.checked.types().class_declaration(class).kind == DeclarationKind::Interface {
            return self.unmodeled(
                expression.span(),
                format!("the interface method {place}"),
                issues::INTERFACE_METHOD,
            );
        }

        self.unmodeled(expression.span(), format!("a call to the overridable method {place}"), issues::OVERRIDABLE)
    }

    /// The Lean type of the class `class` a hint names: the sealed interface it implements, spec section 20.1, as a
    /// data case's is its enum, and else its own.
    pub(crate) fn class_type(&mut self, span: Span, class: &[u8]) -> String {
        // Spec section 20: the class a case that carries data runs as is a constructor of its enum, as a machine's
        // `OrderStatus.Paid` is of `OrderStatus`.
        if let Some(separator) = class.iter().rposition(|&byte| byte == b'\\')
            && self.checked.types().enum_case_class(word(&class[..separator]), word(&class[separator + 1..])).is_some()
        {
            return self.class_type(span, &class[..separator]);
        }
        match self.sealed(span, class) {
            Some((interface, _)) => interface,
            None => self.own_type(span, class),
        }
    }

    /// The Lean type the class-like `class` declares itself, which the constructor of a sealed interface it implements
    /// holds.
    pub(crate) fn own_type(&mut self, span: Span, class: &[u8]) -> String {
        if !self.is_translated_class(span, class) {
            return "default".to_owned();
        }

        self.reads(class);

        names::full_name(&self.reached_name(class))
    }

    /// The Lean type of the sealed interface `class` implements, spec section 20.1, and the constructor that holds a
    /// value of `class`. A class that is a state of two sealed interfaces has no one Lean type, so the body that takes
    /// its value is refused at `span`.
    pub(crate) fn sealed(&mut self, span: Span, class: &[u8]) -> Option<(String, String)> {
        let interfaces = self.reach.sealed_interfaces(class);
        if let [first, second, ..] = interfaces.as_slice() {
            let what = format!("the class {}", names::short_name(&self.reached_name(class)));
            let reason = issues::two_sealed_interfaces(
                &names::short_name(first.as_bytes()),
                &names::short_name(second.as_bytes()),
            );
            self.unmodeled(span, what, reason);
        }
        let interface = interfaces.first()?;
        let lean = names::full_name(interface.as_bytes());
        self.uses(interface.as_bytes(), &lean, names::short_name(interface.as_bytes()));
        let constructor = names::member(&lean, names::short_name(&self.reached_name(class)).as_bytes());

        Some((lean, constructor))
    }

    /// `value`, a value of the class-like `class`'s own type, as a value of its Lean type: held by the constructor of
    /// the sealed interface `class` implements. `span` is where a refusal stands.
    pub(super) fn held(&mut self, span: Span, class: &[u8], value: String) -> String {
        match self.sealed(span, class) {
            Some((_, constructor)) => format!("({constructor} {value})"),
            None => value,
        }
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
        self.uses.push(Use { key: key.to_owned(), file, place, smaller, names_only: false });
    }

    /// Records that the body reads inside the values of `class`'s own type, as a read of its field or the structure
    /// its constructor builds does.
    pub(crate) fn reads(&mut self, class: &[u8]) {
        let class_name = self.reached_name(class);
        self.uses(class, &names::full_name(&class_name), names::short_name(&class_name));
    }

    /// The Lean type a declared type names, `class`'s, as [`Body::class_type`] gives it: naming a type reads nothing
    /// inside its values.
    fn named_type(&mut self, span: Span, class: &[u8]) -> String {
        let start = self.uses.len();
        let r#type = self.class_type(span, class);
        for named in &mut self.uses[start..] {
            named.names_only = true;
        }

        r#type
    }

    /// The source text at `span`, as messages quote it.
    fn source(&self, span: Span) -> String {
        let contents = &self.checked.file().contents;
        let text = contents.get(span.start.offset as usize..span.end.offset as usize).unwrap_or_default();

        String::from_utf8_lossy(text).into_owned()
    }

    /// Records a construct the translation does not model, and returns a term that stands in for it. A declaration
    /// that holds one is never written to Lean, so the term is never read.
    pub(crate) fn unmodeled(&mut self, span: Span, what: String, reason: impl Into<Cow<'static, str>>) -> String {
        self.unmodeled.push(Unmodeled { span, what, place: self.place.clone(), reason: reason.into() });

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
                if matches!(self.checked.types().class_declaration(named.name).kind, DeclarationKind::Enum { .. }) {
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

/// Whether `class` names a PHP# collection.
fn is_collection(class: &[u8]) -> bool {
    matches!(names::short_name(class).as_str(), "List" | "Map" | "Set")
}
