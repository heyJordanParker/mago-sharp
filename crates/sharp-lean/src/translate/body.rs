//! A method's or law's body as the statements of a Lean `do` block in `M`.
//!
//! Every value that can throw is bound to a temporary `«$n»` before it is used, as A-normal form binds it, so the
//! effects run in the order PHP# runs them and every other term is pure. A branch that runs only on one side of a
//! condition is a nested `do` block, so its effects run only there.

use std::borrow::Cow;
use std::ops::Range;
use std::sync::Arc;

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
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
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
use mago_syntax::cst::Array;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::BracedExpressionStringPart;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::GenericHint;
use mago_syntax::cst::Hint;
use mago_syntax::cst::If;
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
use mago_syntax::cst::StringPart;
use mago_syntax::cst::TypeOf;
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
    /// The interface whose values the body reads as values of one PHP# implementer, and that implementer, each by its
    /// full name, as [`Body::reads_as`] says.
    reading: Option<(Word, Word)>,
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
            reading: None,
        }
    }

    /// Reads each value of `interface` as a value of its PHP# implementer `implementer`, as the implementer's proof of
    /// the interface's laws does, spec sections 22 and 28: the interface's type is the implementer's, and a call of the
    /// interface's method or a read of its property runs the implementer's, which calls the implementer's own methods
    /// as this body reads them.
    pub(crate) fn reads_as(&mut self, interface: Word, implementer: Word) {
        self.reading = Some((interface, implementer));
    }

    /// The class a value of `class` is read as: the implementer the body reads `class` as, or `class`.
    fn read_as(&self, class: Word) -> Word {
        match self.reading {
            Some((interface, implementer)) if self.reach.same_class(class.as_bytes(), interface.as_bytes()) => {
                implementer
            }
            _ => class,
        }
    }

    /// The name of the method `method` of the class `declaring` that a call runs: the method as the body reads it,
    /// `credit$Ledger`, when `declaring` is the implementer the body reads its interface as and the interface declares
    /// the method, and else its own.
    pub(crate) fn read_method(&self, declaring: &[u8], method: &str) -> String {
        match self.reading {
            Some((interface, implementer))
                if self.reach.same_class(declaring, implementer.as_bytes())
                    && self.reach.method(interface.as_bytes(), method.as_bytes()).is_some() =>
            {
                format!("{method}${}", names::short_name(interface.as_bytes()))
            }
            _ => method.to_owned(),
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
                // A `List` is Lean's `List`, which `map`, `filter` and `count` read as Lean's own functions, and a `Map`
                // is `Sharp.Map`, whose members are functions of its entries.
                if names::short_name(name) == "List"
                    && let Some(element) = generic.type_arguments.arguments.first()
                {
                    format!("(List {})", self.r#type(element))
                } else if names::short_name(name) == "Map"
                    && let [key, value] = generic.type_arguments.arguments.as_slice()
                {
                    format!("(Sharp.Map {} {})", self.r#type(key), self.r#type(value))
                } else if let Some(family) = self.class_hint_family(name, generic) {
                    // Spec section 25: a `Class<F>` of a family `F` of classes is one of its classes.
                    self.class_value_type(&family)
                } else if names::short_name(name) == "Class" {
                    CLASS_VALUE.to_owned()
                } else if is_collection(name) {
                    self.unmodeled(hint.span(), "a collection".to_owned(), issues::list_members(&LIST_MEMBERS))
                } else {
                    self.named_type(hint.span(), name)
                }
            }
            Hint::Function(_) => self.unmodeled(hint.span(), "a function type".to_owned(), issues::LAMBDA),
            Hint::Mixed(_) => self.unmodeled(hint.span(), "Any".to_owned(), issues::TYPE_TEST),
            Hint::Iterable(_) => self.unmodeled(hint.span(), "Iterable".to_owned(), issues::LAZY),
            Hint::Union(_) if let Some(r#enum) = self.union_enum(hint) => self.named_type(hint.span(), r#enum),
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

    /// The premise that `value`, of the declared type `hint`, keeps the precondition every value of its class-like
    /// keeps, spec section 28, as the class-like's `«$valid»` states it: of the value, of each element of a `List`, and
    /// of the value a nullable type holds. `None` for a type whose values keep none.
    pub(crate) fn valid(&mut self, hint: Option<&Hint>, value: &str) -> Option<String> {
        let class = match hint? {
            Hint::Parenthesized(parenthesized) => return self.valid(Some(parenthesized.hint), value),
            Hint::Nullable(nullable) => {
                let held = self.fresh();
                let valid = self.valid(Some(nullable.hint), &held)?;
                return Some(format!("(∀ {held}, {value} = some {held} → {valid})"));
            }
            Hint::Generic(generic) => {
                let name = self.checked.names().resolve(&generic.name).unwrap_or(generic.name.value);
                let [element] = generic.type_arguments.arguments.as_slice() else {
                    return None;
                };
                if names::short_name(name) != "List" {
                    return None;
                }
                let held = self.fresh();
                let valid = self.valid(Some(element), &held)?;
                return Some(format!("(∀ {held} ∈ {value}, {valid})"));
            }
            hint @ Hint::Union(_) => self.union_enum(hint)?,
            Hint::Self_(_) => self.class,
            Hint::Identifier(identifier) if !self.checked.names().is_type_parameter(identifier) => {
                let class = self.checked.names().get(identifier);
                self.case_enum(class).unwrap_or(class)
            }
            _ => return None,
        };
        let read = self.read_as(word(class));
        if !self.reach.is_valid(read.as_bytes()) {
            return None;
        }
        let class_name = self.reached_name(read.as_bytes());
        let key = names::member(&names::full_name(&class_name), b"$valid");
        self.uses(read.as_bytes(), &key, names::short_name(&class_name));

        Some(format!("({key} {value})"))
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
    pub(super) fn statements(&mut self, statements: &Sequence<'_, Statement<'_>>) {
        let outer = self.path.clone();
        for statement in statements {
            let Statement::If(r#if) = statement else {
                self.statement(statement);
                continue;
            };
            if let Some(branches) = self.r#if(r#if)
                && branches.otherwise.is_none()
                && returns(branches.then)
            {
                self.path = self.path.branch(branches.condition, false);
            }
        }
        self.path = outer;
    }

    /// The branches of `r#if`, or `None` for an `if` with an else-if clause, which is refused: PHP# writes it `else
    /// if`, and a translation that read only the other branches would drop its branch.
    pub(crate) fn branches<'ast, 'arena>(&mut self, r#if: &'ast If<'arena>) -> Option<Branches<'ast, 'arena>> {
        let IfBody::Statement(body) = &r#if.body else {
            unreachable!("PHP# parses no colon-delimited if, as {:?}", r#if.span());
        };
        if let Some(clause) = body.else_if_clauses.first() {
            self.unmodeled(clause.span(), "an else-if clause".to_owned(), issues::ELSE_IF);

            return None;
        }

        Some(Branches {
            condition: r#if.condition,
            then: body.statement,
            otherwise: body.else_clause.as_ref().map(|clause| clause.statement),
        })
    }

    /// The statements of `r#if`, which give its branches, or `None` when it is refused.
    fn r#if<'ast, 'arena>(&mut self, r#if: &'ast If<'arena>) -> Option<Branches<'ast, 'arena>> {
        let branches = self.branches(r#if)?;
        let holds = self.path.branch(branches.condition, true);
        // `if (x is Money m)` names `m` where the test holds, so it is a `match` on `x`.
        if let Expression::Is(is) = branches.condition.unparenthesized() {
            let Some(test) = self.is_test(is) else {
                return Some(branches);
            };
            let then = self.under(holds, |this| this.nested_statement(branches.then));
            let otherwise = branches.otherwise.map(|otherwise| self.nested_statement(otherwise));
            let choice = choice(vec![(test, then)], otherwise.unwrap_or_default());
            self.lines.extend(statements(&choice));

            return Some(branches);
        }
        let condition = self.value(branches.condition);
        let then = self.under(holds, |this| this.nested_statement(branches.then));
        self.lines.push(format!("if {condition} then"));
        self.lines.extend(indented(&then, 2));
        if let Some(otherwise) = branches.otherwise {
            let fails = self.path.branch(branches.condition, false);
            let otherwise = self.under(fails, |this| this.nested_statement(otherwise));
            self.lines.push("else".to_owned());
            self.lines.extend(indented(&otherwise, 2));
        }

        Some(branches)
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
                self.r#if(r#if);
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
            Expression::Conditional(conditional) => self.conditional(expression, conditional, false),
            Expression::PatternMatch(r#match) => self.pattern_match(expression, r#match),
            Expression::Call(Call::Method(call)) => self.method_call(expression, call),
            Expression::Call(Call::NullSafeMethod(call)) => self.null_safe_call(expression, call),
            Expression::Call(Call::Function(call)) => {
                let what = format!("the plain PHP function `{}`", self.source(call.function.span()));
                self.unmodeled(expression.span(), what, issues::PLAIN_PHP)
            }
            Expression::Instantiation(instantiation) => self.instantiation(instantiation),
            Expression::Access(Access::Property(access)) => self.property(expression, access),
            Expression::Access(Access::NullSafeProperty(access)) => self.null_safe_property(expression, access),
            // Spec section 12: a `Map` literal sets each entry in order, so a later key replaces an earlier one's value,
            // and a spread sets each entry of its `Map`. `[:]` is the empty `Map`.
            Expression::Array(array) if array.colon.is_some() || self.is_map(expression) => self.map_literal(array),
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
            // Spec section 12: a `Map` read gives the value of an equal key, or null.
            Expression::ArrayAccess(access) if self.is_map(access.array) => {
                let map = self.value(access.array);
                let key = self.value(access.index);

                self.map_get(access.array, &map, &key)
            }
            Expression::Array(_)
            | Expression::LegacyArray(_)
            | Expression::List(_)
            | Expression::ArrayAccess(_)
            | Expression::ArrayAppend(_) => {
                self.unmodeled(expression.span(), "a collection".to_owned(), issues::list_members(&LIST_MEMBERS))
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
            Expression::TypeOf(type_of) => self.type_of(expression, type_of),
            Expression::As(_) => self.unmodeled(expression.span(), "a type test".to_owned(), issues::TYPE_TEST),
            Expression::Yield(_) => self.unmodeled(expression.span(), "a lazy sequence".to_owned(), issues::LAZY),
            // A throw ends the code there with `Throwable.Thrown`, which proves nothing about a law, as an engine error
            // does. Its value, which no code reads, is a stand-in.
            Expression::Throw(_) => {
                self.lines.push("throw Throwable.Thrown".to_owned());
                "default".to_owned()
            }
            Expression::CompositeString(template) => self.template(expression, template),
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

    /// A string template, spec section 18: the `++` of its parts, each literal as its bytes, a `string` as itself and an
    /// `int` as the text PHP# writes for it, `int.text`. A template with a part of any other type is refused.
    fn template(&mut self, expression: &Expression, template: &CompositeString) -> String {
        let mut parts = Vec::new();
        for part in template.parts() {
            parts.push(match part {
                StringPart::Literal(literal) => names::string(literal.value.unwrap_or_default()),
                StringPart::Expression(value)
                | StringPart::BracedExpression(BracedExpressionStringPart { expression: value, .. }) => {
                    match self.kind(self.checked.types().expression_type(value)) {
                        Kind::String => self.value(value),
                        Kind::Int => format!("(int.text {})", self.value(value)),
                        _ => {
                            return self.unmodeled(expression.span(), "a string template".to_owned(), issues::TEMPLATE);
                        }
                    }
                }
            });
        }

        if parts.is_empty() { names::string(b"") } else { format!("({})", parts.join(" ++ ")) }
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
            BinaryOperator::NullCoalesce(_) => return self.coalesce(binary, false),
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
    /// them, the way the bridge's [`Equality`] tells: Lean's `==` on values that hold no object, the `==` a class
    /// declares, or a struct gets with no code, as its `op_Equality` gives it, and two `List`s element by element and two
    /// `Map`s key by key, as [`Body::elements_equal`] gives it. A nullable side is lifted: null equals only null, and two
    /// values run that `==`. `span` is where a refusal stands.
    pub(crate) fn equal(
        &mut self,
        span: Span,
        types: (&TUnion, &TUnion),
        (lhs, rhs): (&str, &str),
        negated: bool,
    ) -> String {
        // Spec section 25: two class values of one family's classes are equal when they are the same class, and two of
        // types no family names are values of `CLASS_VALUE`, which Lean's `==` compares.
        let families = (self.class_value_family(types.0), self.class_value_family(types.1));
        let same_family = matches!(&families, (Some(left), Some(right)) if self.reach.same_class(left, right));
        if same_family || (families == (None, None) && is_class_value(types.0) && is_class_value(types.1)) {
            return if negated { format!("({lhs} != {rhs})") } else { format!("({lhs} == {rhs})") };
        }
        let nullable = (types.0.is_nullable(), types.1.is_nullable());
        let equal = match self.checked.types().equality(types.0, types.1) {
            Equality::Identical if negated => return format!("({lhs} != {rhs})"),
            Equality::Identical => return format!("({lhs} == {rhs})"),
            Equality::Declared(owner, name) => {
                if !self.is_translated_class(span, owner.as_bytes()) {
                    return "default".to_owned();
                }

                self.lifted(nullable, (lhs, rhs), |body, sides| body.operator(owner.as_bytes(), name, "==", sides))
            }
            Equality::Elements => {
                let elements = (element_type(types.0), element_type(types.1));

                self.lifted(nullable, (lhs, rhs), |body, sides| {
                    body.elements_equal(span, "Sharp.elementsEqual", &elements, sides)
                })
            }
            Equality::Tables if is_map_type(types.0) && is_map_type(types.1) => {
                let values = (element_type(types.0), element_type(types.1));

                self.lifted(nullable, (lhs, rhs), |body, sides| {
                    body.elements_equal(span, "Sharp.Map.equalBy", &values, sides)
                })
            }
            Equality::Numbers => return self.unmodeled(span, "a float".to_owned(), issues::FLOAT),
            Equality::Tables => {
                return self.unmodeled(span, "a collection".to_owned(), issues::list_members(&LIST_MEMBERS));
            }
            Equality::OwnType if let Some(interface) = self.equatable_interface(types) => {
                self.lifted(nullable, (lhs, rhs), |body, sides| body.own_type_equal(span, &interface, sides))
            }
            Equality::Running | Equality::OwnType => {
                let operator = if negated { "!=" } else { "==" };
                let what = format!("`{operator}` on values only the running code tells apart");
                return self.unmodeled(span, what, issues::TYPE_TEST);
            }
        };

        if negated { format!("(!{equal})") } else { equal }
    }

    /// The one interface that extends `Equatable` whose Lean type holds the values of both `types`, null aside, by its
    /// full name, which [`Equality::OwnType`] compares them through: the interface a side is typed as, or the sealed
    /// interface whose constructor holds an implementer's value, spec section 20.1, as a `Failure` holds
    /// `Declined.Card`. `None` when a side may be a value of another type.
    fn equatable_interface(&self, types: (&TUnion, &TUnion)) -> Option<Vec<u8>> {
        let mut held = [types.0, types.1]
            .into_iter()
            .flat_map(|r#type| r#type.types.iter())
            .filter(|atomic| !atomic.is_null())
            .map(|atomic| {
                let name = match atomic {
                    TAtomic::Object(object) if atomic.get_intersection_types().is_none() => object.get_name()?,
                    _ => return None,
                };
                if matches!(self.checked.types().class_declaration(name).kind, DeclarationKind::Interface) {
                    return Some(name.as_bytes().to_vec());
                }
                let class = self.case_enum(name.as_bytes()).unwrap_or(name.as_bytes());

                self.reach.sealed_interfaces(class).first().map(|interface| interface.as_bytes().to_vec())
            });
        let first = held.next()??;

        held.all(|interface| interface.is_some_and(|interface| self.reach.same_class(&interface, &first)))
            .then_some(first)
    }

    /// `==` of `lhs` and `rhs`, two values of `interface`, an interface that extends `Equatable`, spec section 19: equal
    /// when they are values of one implementer and its own `==` holds. A sealed interface's value is one of its
    /// implementers' values, spec section 20.1, so `==` splits on each implementer, and else is false. Lean knows nothing
    /// about the values of any other interface, so `==` on them is Lean's equality of the values.
    fn own_type_equal(&mut self, span: Span, interface: &[u8], (lhs, rhs): (&str, &str)) -> String {
        self.reads(interface);
        let reach = self.reach;
        let implementers = reach.implementers(interface).unwrap_or_default();
        if implementers.is_empty() {
            return format!("({lhs} == {rhs})");
        }

        let lean = names::full_name(&self.reached_name(interface));
        let mut alternatives = Vec::new();
        for implementer in implementers {
            let held = TUnion::from_atomic(match self.checked.types().class_declaration(*implementer).kind {
                DeclarationKind::Enum { .. } => TAtomic::Object(TObject::new_enum(*implementer)),
                _ => TAtomic::Object(TObject::Named(TNamedObject::new(*implementer))),
            });
            let constructor = names::member(&lean, names::short_name(implementer.as_bytes()).as_bytes());
            let (a, b) = (self.fresh(), self.fresh());
            let equal = self.nested_lines(|body| body.equal(span, (&held, &held), (&a, &b), false));
            alternatives.push((format!("({constructor} {a}), ({constructor} {b})"), Choice::Branch(equal)));
        }
        if implementers.len() > 1 {
            alternatives.push(("_, _".to_owned(), Choice::Branch(vec!["pure false".to_owned()])));
        }

        self.choose(&Choice::Match(format!("{lhs}, {rhs}"), alternatives))
    }

    /// `==` of two `List`s or two `Map`s `lhs` and `rhs`, whose elements, or values, have the types `elements`, as spec
    /// section 19 compares them, by `compare` of the elements' own `==`: `Sharp.elementsEqual`, which holds when both
    /// lists hold as many elements and each pair is equal, or `Sharp.Map.equalBy`, which holds when both maps hold as
    /// many entries and each key of `lhs` is a key of `rhs` with an equal value. Each compares in order until one pair is
    /// not equal, as the engine compares them.
    fn elements_equal(
        &mut self,
        span: Span,
        compare: &str,
        elements: &(TUnion, TUnion),
        (lhs, rhs): (&str, &str),
    ) -> String {
        let (a, b) = (self.fresh(), self.fresh());
        let element = self.nested_lines(|body| body.equal(span, (&elements.0, &elements.1), (&a, &b), false));
        let temporary = self.fresh();
        self.lines.push(format!("let {temporary} ← {compare} (fun {a} {b} => do"));
        self.lines.extend(indented(&element, 4));
        self.lines.push(format!("  ) {lhs} {rhs}"));

        temporary
    }

    /// `==` of `lhs` and `rhs` by `compare`, which compares two values, where `nullable` tells which side may be null,
    /// as spec section 19 lifts it: null equals only null, and two values run `compare`.
    fn lifted(
        &mut self,
        nullable: (bool, bool),
        (lhs, rhs): (&str, &str),
        compare: impl FnOnce(&mut Self, (&str, &str)) -> String,
    ) -> String {
        let (held, absent) = match nullable {
            (false, false) => return compare(self, (lhs, rhs)),
            (true, true) => (format!("({lhs}.isSome && {rhs}.isSome)"), format!("pure ({lhs}.isNone && {rhs}.isNone)")),
            (true, false) => (format!("{lhs}.isSome"), "pure false".to_owned()),
            (false, true) => (format!("{rhs}.isSome"), "pure false".to_owned()),
        };
        let value =
            |side: &str, nullable: bool| if nullable { format!("({side}.getD default)") } else { side.to_owned() };
        let values = (value(lhs, nullable.0), value(rhs, nullable.1));
        let present = self.nested_lines(|body| compare(body, (&values.0, &values.1)));

        self.choose(&Choice::If(held, Box::new(Choice::Branch(present)), Box::new(Choice::Branch(vec![absent]))))
    }

    /// Whether the field `field`, of the declared type `r#type`, holds equal values in `a` and `b`, two values of the
    /// struct `class`, by the field's own `==`, spec section 19. An implementer of a sealed interface reads it through
    /// the interface's accessor. The type is read as the codex populates a declared type, as [`Body::populated`] reads
    /// it.
    pub(crate) fn fields_equal(&mut self, span: Span, class: &[u8], field: &[u8], r#type: &TUnion) -> String {
        let Some(populated) = r#type.types.iter().map(|atomic| self.populated(atomic)).collect::<Option<Vec<_>>>()
        else {
            return self.unmodeled(span, "the class operator `==`".to_owned(), issues::CLASS_OPERATOR);
        };
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

    /// `atomic`, a part of a declared type, as the codex populates it: a class it names is the enum or the object it
    /// names, and so is each class a `List`'s elements name. None for any other type that names a class inside it.
    fn populated(&self, atomic: &TAtomic) -> Option<TAtomic> {
        Some(match atomic {
            TAtomic::Reference(TReference::Symbol { name, parameters: None, intersection_types: None, .. }) => {
                match self.checked.types().class_declaration(*name).kind {
                    DeclarationKind::Enum { .. } => TAtomic::Object(TObject::new_enum(*name)),
                    _ => TAtomic::Object(TObject::Named(TNamedObject::new(*name))),
                }
            }
            TAtomic::Array(TArray::List(list)) if list.known_elements.is_none() => {
                let elements = list.element_type.types.iter().map(|atomic| self.populated(atomic));
                let mut list = list.clone();
                list.element_type = Arc::new(TUnion::from_vec(elements.collect::<Option<Vec<_>>>()?));
                TAtomic::Array(TArray::List(list))
            }
            atomic
                if !atomic
                    .get_all_child_nodes()
                    .iter()
                    .any(|node| matches!(node, TypeRef::Atomic(TAtomic::Reference(_)))) =>
            {
                atomic.clone()
            }
            _ => return None,
        })
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

    /// `expression`, an int, as the exact `Int` it computes. A `? :` and a `??` choose an exact `Int` of each branch, so
    /// the automatic steps read the value each branch gives as they read any other `Int`.
    fn exact_int(&mut self, expression: &Expression) -> String {
        match expression {
            Expression::Parenthesized(parenthesized) => self.exact_int(parenthesized.expression),
            Expression::Conditional(conditional) => self.conditional(expression, conditional, true),
            Expression::Binary(binary) if matches!(binary.operator, BinaryOperator::NullCoalesce(_)) => {
                self.coalesce(binary, true)
            }
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

    /// `c ? a : b`, `expression`, which runs only the branch `c` chooses, as the exact `Int` it computes when `exact`. It
    /// is a step that gives each branch's value, as an `if` statement is, even when both branches are values, so the
    /// fixed steps read the value it gives in each case, and a fact about that step is a fact about the value.
    fn conditional(&mut self, expression: &Expression, conditional: &Conditional, exact: bool) -> String {
        if let Expression::Is(_) = conditional.condition.unparenthesized() {
            let Some((arms, otherwise)) = self.tests(expression, conditional, exact) else {
                return "default".to_owned();
            };

            return self.choose(&choice(arms, otherwise));
        }
        let Some(then) = conditional.then else {
            unreachable!("PHP# parses no `?:` with two operands, as {:?}", expression.span());
        };
        let holds = self.path.branch(conditional.condition, true);
        let condition = self.value(conditional.condition);
        let then = self.under(holds, |body| body.conditional_branch(then, expression, exact));
        let fails = self.path.branch(conditional.condition, false);
        let otherwise = self.under(fails, |body| body.conditional_branch(conditional.r#else, expression, exact));

        self.bind_choice(&Choice::If(condition, Box::new(Choice::Branch(then)), Box::new(Choice::Branch(otherwise))))
    }

    /// The arms of `conditional`, a `?:` of `whole` whose condition is an `is` test, and the branch that runs where no
    /// arm's test holds, or `None` after recording why a test has none. A `?:` with an `is` test in the else branch
    /// adds its own arms in place of that branch when its test adds no statement, as a test of a local adds none, so
    /// `choice` joins the tests of one subject into one `match` that names each constructor once.
    fn tests(
        &mut self,
        whole: &Expression,
        conditional: &Conditional,
        exact: bool,
    ) -> Option<(Vec<(Test, Branch)>, Branch)> {
        let (Expression::Is(is), Some(then)) = (conditional.condition.unparenthesized(), conditional.then) else {
            unreachable!("a `?:` of an `is` test has three operands, as {:?}", whole.span());
        };
        let holds = self.path.branch(conditional.condition, true);
        let test = self.is_test(is)?;
        let then = self.under(holds, |body| body.conditional_branch(then, whole, exact));
        let mut arms = vec![(test, then)];
        let next = match conditional.r#else.unparenthesized() {
            Expression::Conditional(next) if matches!(next.condition.unparenthesized(), Expression::Is(_)) => next,
            _ => return Some((arms, self.conditional_branch(conditional.r#else, whole, exact))),
        };

        let outer = std::mem::take(&mut self.lines);
        let rest = self.tests(whole, next, exact);
        let lines = std::mem::replace(&mut self.lines, outer);
        let (rest, otherwise) = rest?;
        if lines.is_empty() {
            arms.extend(rest);

            return Some((arms, otherwise));
        }
        let otherwise = self.nested_lines(|body| {
            body.lines = lines;
            body.choose(&choice(rest, otherwise))
        });

        Some((arms, otherwise))
    }

    /// The statements of `branch`, a branch of `whole`, a `?:`, as the exact `Int` it computes when `exact`.
    fn conditional_branch(&mut self, branch: &Expression, whole: &Expression, exact: bool) -> Branch {
        if exact { self.exact_branch(branch) } else { self.branch_of(branch, whole) }
    }

    /// `a ?? b`, which runs `b` only when `a` is null, as the exact `Int` it computes when `exact`.
    fn coalesce(&mut self, binary: &Binary, exact: bool) -> String {
        let lhs = self.value(binary.lhs);
        let keeps_null = self.checked.types().expression_type(binary.rhs).is_nullable();
        let (present, rhs) = if exact {
            (format!("({lhs}.getD default).toInt"), self.exact_branch(binary.rhs))
        } else if keeps_null {
            (lhs.clone(), self.branch(binary.rhs))
        } else {
            (format!("({lhs}.getD default)"), self.branch(binary.rhs))
        };

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
            // `(string)` of an int is its decimal digits, as `Sharp/Lean/String.lean`'s `int.text` writes them.
            UnaryPrefixOperator::StringCast(..)
                if self.kind(self.checked.types().expression_type(unary.operand)) == Kind::Int
                    && !self.checked.types().expression_type(unary.operand).is_nullable() =>
            {
                let operand = self.value(unary.operand);
                format!("(int.text {operand})")
            }
            operator => {
                let what = format!("the operator `{}`", self.source(operator.span()));
                self.unmodeled(unary.span(), what, issues::OPERATOR)
            }
        }
    }

    /// A `match` in an expression, `expression`, gives the value of the first arm whose pattern and guard hold, and
    /// throws when none does.
    fn pattern_match(&mut self, expression: &Expression, r#match: &PatternMatch) -> String {
        let Some((arms, otherwise)) = self.arms(r#match, |body, arm| body.arm_value(arm, expression)) else {
            return "default".to_owned();
        };

        self.choose(&choice(arms, otherwise))
    }

    /// The test and the branch of each arm of `r#match`, in order, each branch as `translate` gives it, and the branch
    /// that runs when no arm matches: the `default` arm's, else a throw. `None` after recording why an arm has no test.
    /// An arm with no guard whose pattern joins alternatives with `or` is an arm for each alternative, with the same
    /// branch, so [`choice`] joins a case's alternative to the `match` of the arms after it, and no arm before it runs
    /// the whole `or` again in each case it leaves out.
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

            let Some(guard) = &arm.guard else {
                let mut tests = Vec::new();
                for alternative in alternatives(arm.pattern) {
                    tests.push(self.test(&subject, None, r#type, alternative)?);
                }
                let branch = self.under(path, |body| translate(body, &arm.body));
                arms.extend(tests.into_iter().map(|test| (test, branch.clone())));
                continue;
            };
            let mut test = self.test(&subject, None, r#type, arm.pattern)?;
            test.conditions.push(self.pure(guard.condition, "a guard that can throw", issues::GUARD)?);
            arms.push((test, self.under(path, |body| translate(body, &arm.body))));
        }

        Some((arms, otherwise))
    }

    /// The branch of an arm of `whole`, a `match` in an expression, which ends in the arm's value.
    fn arm_value(&mut self, body: &PatternMatchArmBody, whole: &Expression) -> Branch {
        match body {
            PatternMatchArmBody::Expression(expression) => self.branch_of(expression, whole),
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

    /// The enum whose case `value` names, when its cases run as classes, spec section 20.2, and the Lean pattern of
    /// every value of the case, as `(Counter.Counting ..)`, held by the constructor of the sealed interface the enum
    /// implements.
    fn case_pattern(&mut self, value: &Expression) -> Option<(&'program [u8], String)> {
        let Expression::Access(Access::Property(access)) = value.unparenthesized() else {
            return None;
        };
        let ClassLikeMemberSelector::Identifier(case) = &access.property else {
            return None;
        };
        let r#enum = self.checked.names().get(&self.checked.names().static_property_class(access)?.name);
        self.checked.types().enum_case_class(word(r#enum), word(case.value))?;
        self.reads(r#enum);
        let constructor = names::member(&names::full_name(&self.reached_name(r#enum)), case.value);

        Some((r#enum, self.held(value.span(), r#enum, format!("({constructor} ..)"))))
    }

    /// The implementer `hint` names, and the Lean constructor of the sealed interface `atomic` is that holds its value,
    /// spec section 20.1.
    pub(super) fn implementer(&mut self, hint: &Hint, atomic: &TAtomic) -> Option<(&'program [u8], String)> {
        let (Hint::Identifier(identifier), TAtomic::Object(TObject::Named(interface))) = (hint, atomic) else {
            return None;
        };
        let class = self.checked.names().get(identifier);
        if !self.implements(class, interface.name.as_bytes()) {
            return None;
        }

        self.sealed(hint.span(), class).map(|(_, constructor)| (class, constructor))
    }

    /// The Lean pattern of each value of `class`'s Lean type, one for each of the type's constructors, so a match that
    /// writes each leaves no value to a `_` arm: each value of each implementer of the sealed interface `class` is or
    /// implements, spec section 20.1, and else each value of `class`, as [`Body::values`] gives them.
    pub(super) fn constructors(&self, class: &[u8]) -> Vec<String> {
        let interface = match self.reach.implementers(class) {
            Some(_) => self.reached_name(class),
            None => match self.reach.sealed_interfaces(class).first() {
                Some(interface) => interface.as_bytes().to_vec(),
                None => return self.values(class, None),
            },
        };
        let lean = names::full_name(&interface);

        self.reach
            .implementers(&interface)
            .unwrap_or_default()
            .iter()
            .flat_map(|implementer| {
                let constructor = names::member(&lean, names::short_name(implementer.as_bytes()).as_bytes());
                self.values(implementer.as_bytes(), Some(&constructor))
            })
            .collect()
    }

    /// The Lean pattern of each value of the class-like `class`'s own type, inside `constructor`, the constructor of
    /// the sealed interface that holds it, when one does: each case of an enum, whatever fields it holds, as
    /// `(Shop.OrderStatus.Paid ..)`, and else `_`, every value.
    pub(super) fn values(&self, class: &[u8], constructor: Option<&str>) -> Vec<String> {
        let lean = names::full_name(&self.reached_name(class));
        let cases = self.reach.cases(class);
        let values: Vec<String> = if cases.is_empty() {
            vec!["_".to_owned()]
        } else {
            cases.iter().map(|case| format!("({} ..)", names::member(&lean, case.as_bytes()))).collect()
        };

        match constructor {
            Some(constructor) => values.into_iter().map(|value| format!("({constructor} {value})")).collect(),
            None => values,
        }
    }

    /// Each class a value of the class-like `family` runs as, spec section 25, as the constructor of `family`'s
    /// `«$Class»` it is and the Lean pattern of the values of that class: each case of a machine or of an enum whose
    /// cases carry data, and each implementer of a sealed interface. `None` for any other class-like, and for a sealed
    /// interface an enum implements, whose classes are the enum's cases, and for a family that implements a sealed
    /// interface, whose values are the interface's.
    pub(super) fn classes(&self, family: &[u8]) -> Option<Vec<(String, String)>> {
        if !self.reach.sealed_interfaces(family).is_empty() {
            return None;
        }
        let lean = names::full_name(&self.reached_name(family));
        let cases = self.reach.cases(family);
        if let Some(case) = cases.first() {
            self.checked.types().enum_case_class(word(family), *case)?;

            return Some(
                cases
                    .iter()
                    .map(|case| {
                        (names::identifier(case.as_bytes()), format!("({} ..)", names::member(&lean, case.as_bytes())))
                    })
                    .collect(),
            );
        }
        let implementers = self.reach.implementers(family)?;
        if implementers.iter().any(|implementer| !self.reach.cases(implementer.as_bytes()).is_empty()) {
            return None;
        }

        Some(
            implementers
                .iter()
                .map(|implementer| {
                    let constructor = names::short_name(implementer.as_bytes());
                    (
                        names::identifier(constructor.as_bytes()),
                        format!("({} _)", names::member(&lean, constructor.as_bytes())),
                    )
                })
                .collect(),
        )
    }

    /// The class-like whose `«$Class»` holds the class `class`, and the constructor that is `class`: the family of
    /// [`Body::classes`] that runs a value as `class`, or `class` itself, whose class values are those of its family.
    fn class_family(&self, class: &[u8]) -> Option<(Vec<u8>, Option<String>)> {
        if self.classes(class).is_some() {
            return Some((class.to_vec(), None));
        }
        let family = match self.case_enum(class) {
            Some(r#enum) => r#enum.to_vec(),
            None => match self.reach.sealed_interfaces(class).as_slice() {
                [interface] => interface.as_bytes().to_vec(),
                _ => return None,
            },
        };
        let (constructor, _) = self
            .classes(&family)?
            .into_iter()
            .find(|(constructor, _)| *constructor == names::identifier(names::short_name(class).as_bytes()))?;

        Some((family, Some(constructor)))
    }

    /// The class-like whose `«$Class»` a class value of `r#type` is, spec section 25: `typeof` of one of its classes, or
    /// `Class.of` a value of it or of one of its classes. `None` for a nullable value and for any other type.
    pub(super) fn class_value_family(&self, r#type: &TUnion) -> Option<Vec<u8>> {
        let [TAtomic::Scalar(TScalar::ClassLikeString(class))] = r#type.types.as_ref() else {
            return None;
        };
        match class {
            TClassLikeString::Literal { value } => self.class_family(value.as_bytes()).map(|(family, _)| family),
            TClassLikeString::OfType { constraint, .. } => {
                let mut families = constraint.types.iter().map(|atomic| match atomic {
                    TAtomic::Object(TObject::Named(named)) => {
                        self.class_family(named.name.as_bytes()).map(|(family, _)| family)
                    }
                    _ => None,
                });
                let first = families.next()??;
                families
                    .all(|family| family.is_some_and(|family| self.reach.same_class(&family, &first)))
                    .then_some(first)
            }
            _ => None,
        }
    }

    /// The family `F` of the hint `Class<F>`, written `name<…>` as `generic`, when `F` is a family of classes, as
    /// [`Body::classes`] gives them.
    fn class_hint_family(&self, name: &[u8], generic: &GenericHint) -> Option<Vec<u8>> {
        let [Hint::Identifier(held)] = generic.type_arguments.arguments.as_slice() else {
            return None;
        };
        if names::short_name(name) != "Class" || self.checked.names().is_type_parameter(held) {
            return None;
        }
        let family = self.checked.names().get(held);

        self.classes(family).map(|_| family.to_vec())
    }

    /// The Lean type of a class value of the class-like `family`, `family.«$Class»`, spec section 25.
    pub(super) fn class_value_type(&mut self, family: &[u8]) -> String {
        let class_name = self.reached_name(family);
        let key = names::member(&names::full_name(&class_name), b"$Class");
        self.uses(family, &key, format!("Class<{}>", names::short_name(&class_name)));

        key
    }

    /// `typeof(F.C)`, the class of `F`'s values that run as `F.C`, spec section 25, as the constructor of `F.«$Class»`
    /// it is. A class no family holds is refused as a type test.
    fn type_of(&mut self, expression: &Expression, type_of: &TypeOf) -> String {
        if let Hint::Identifier(identifier) = &type_of.r#type
            && !self.checked.names().is_type_parameter(identifier)
            && let Some((family, Some(constructor))) = self.class_family(self.checked.names().get(identifier))
        {
            return format!("{}.{constructor}", self.class_value_type(&family));
        }

        self.unmodeled(expression.span(), "a type test".to_owned(), issues::TYPE_TEST)
    }

    /// `Class.of(value)`, the class `value` runs as, spec section 25: `family.«$classOf»` of `value`, which the body
    /// already holds as a value of the Lean type of `family`.
    fn class_of(&mut self, value: &Expression, family: &[u8]) -> String {
        let class_name = self.reached_name(family);
        let key = names::member(&names::full_name(&class_name), b"$classOf");
        self.uses(family, &key, format!("Class.of a {}", names::short_name(&class_name)));

        format!("({key} {})", self.value(value))
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

    /// Spec section 20.2: the test of `pattern`, a case of an enum whose cases run as classes, as a machine's do, which
    /// tests which case `subject`, a value of `r#type`, is, whatever fields the case holds. `None` for any other pattern.
    pub(super) fn case_test(&mut self, subject: &str, r#type: &TUnion, pattern: &Pattern) -> Option<Test> {
        let Pattern::Value(value) = pattern else {
            return None;
        };
        if self.kind(r#type) != Kind::Enum {
            return None;
        }
        let (r#enum, case) = self.case_pattern(value)?;

        Some(Test::case(subject, case, self.constructors(r#enum), r#type.is_nullable()))
    }

    /// The condition that `subject`, a value of `r#type`, matches `pattern`, or `None` after recording why it has none.
    /// A type or list pattern's is the test [`Self::test`] gives it, so `or`, `and` and `not` combine it as any other,
    /// unless it names a value, which only an arm's own match binds.
    pub(super) fn pattern(&mut self, subject: &str, r#type: &TUnion, pattern: &Pattern) -> Option<String> {
        let kind = self.kind(r#type);
        match pattern {
            Pattern::Value(_) if let Some(test) = self.case_test(subject, r#type, pattern) => Some(test.condition()),
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
            Pattern::Not(not) => Some(format!("(!{})", self.pattern(subject, r#type, not.pattern)?)),
            Pattern::Binary(binary) => {
                let left = self.pattern(subject, r#type, binary.left)?;
                let right = self.pattern(subject, r#type, binary.right)?;

                Some(format!("({left} {} {right})", if binary.is_and() { "&&" } else { "||" }))
            }
            Pattern::Parenthesized(parenthesized) => self.pattern(subject, r#type, parenthesized.pattern),
            Pattern::Type(_) | Pattern::List(_) if !names_values(pattern) => {
                Some(self.test(subject, None, r#type, pattern)?.condition())
            }
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
            // Spec section 25: `Class.of(value)` is the class `value` runs as, of its family's classes.
            if class.eq_ignore_ascii_case(br"Sharp\Class")
                && matches!(&call.method, ClassLikeMemberSelector::Identifier(method) if method.value == b"of")
                && let [argument] = call.argument_list.arguments.as_slice()
                && let Some(family) = self.class_value_family(self.checked.types().expression_type(expression))
            {
                return self.class_of(argument.value(), &family);
            }
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
        if self.is_map(call.object) {
            return self.map_call(expression, call);
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
        let checked = self.checked;
        let receiver = checked.types().expression_type(call.object);
        if let Some(r#enum) = self.joined_enum(receiver)
            && checked.types().machine(word(r#enum)).is_some()
        {
            return self.case_call(expression, call, r#enum, receiver);
        }
        let Some(class) = self.receiver_class(call.object) else {
            return "default".to_owned();
        };
        let target = self.checked.types().call_target(expression);
        let read = self.read_as(word(class));
        match target.kind {
            DeclarationKind::Method { overridable: false } => {}
            // Spec sections 22 and 28: a body that reads an interface as one implementer runs that implementer's
            // method, and any other body knows of an interface's method only what the interface's laws say, so it calls
            // it only on an interface that states laws.
            DeclarationKind::Method { overridable: true } if read != word(class) => {
                let Some(method) = self.method_name(&call.method) else {
                    return "default".to_owned();
                };
                let declaring = self.checked.types().member_declaration(read, word(method)).class;
                let receiver = self.value(call.object);
                return self.call(
                    expression,
                    declaring.as_bytes(),
                    read.as_bytes(),
                    method,
                    Some(receiver),
                    Some(&call.argument_list),
                );
            }
            DeclarationKind::Method { overridable: true } if !self.reach.interface_laws(class).is_empty() => {}
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

    /// `call` on a value of `r#type`, a union of the machine `r#enum`'s cases, spec section 20.2, as the case it holds
    /// runs it: a `match` on the value whose arm for each case of the union calls the method the case's class declares
    /// or inherits. The checker refuses a move called on a union that holds a case it doesn't move from, so each case
    /// of the union has the method. No other value of the machine's Lean type reaches the call, so each one's arm is
    /// the default.
    fn case_call(&mut self, expression: &Expression, call: &MethodCall, r#enum: &[u8], r#type: &TUnion) -> String {
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let receiver = self.value(call.object);
        self.reads(r#enum);
        let held = self.sealed(call.object.span(), r#enum).map(|(_, constructor)| constructor);
        let lean = names::full_name(&self.reached_name(r#enum));
        let mut alternatives: Vec<(String, Choice)> = Vec::new();
        for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
            let TAtomic::Object(TObject::Named(named)) = atomic else {
                unreachable!("a union of a machine's cases names each case's class, not {}", atomic.get_id());
            };
            let case = names::short_name(named.name.as_bytes());
            let declaring = self.checked.types().member_declaration(named.name, word(method));
            if !matches!(declaring.kind, DeclarationKind::Method { .. }) {
                return self.unmodeled(expression.span(), "a call of a function value".to_owned(), issues::LAMBDA);
            }
            let branch = self.nested_lines(|body| {
                body.call(
                    expression,
                    declaring.class.as_bytes(),
                    named.name.as_bytes(),
                    method,
                    Some(receiver.clone()),
                    Some(&call.argument_list),
                )
            });
            let pattern = format!("({} ..)", names::member(&lean, case.as_bytes()));
            let pattern = held.as_ref().map_or_else(|| pattern.clone(), |held| format!("({held} {pattern})"));
            alternatives.push((pattern, Choice::Branch(branch)));
        }
        for constructor in self.constructors(r#enum) {
            if alternatives.iter().all(|(pattern, _)| *pattern != constructor) {
                alternatives.push((constructor, Choice::Branch(vec!["pure default".to_owned()])));
            }
        }

        self.choose(&Choice::Match(receiver, alternatives))
    }

    /// Whether `expression`'s value is a `List`, null aside.
    pub(super) fn is_list(&self, expression: &Expression) -> bool {
        let r#type = self.checked.types().expression_type(expression);
        let mut types = r#type.types.iter().filter(|atomic| !atomic.is_null()).peekable();

        types.peek().is_some() && types.all(TAtomic::is_list)
    }

    /// A `List` member a law reads, one of [`LIST_MEMBERS`], as the Lean function it is: `count()` is `int.count`, the
    /// list's length, or that length checked into the int range in a body that computes exactly, `isEmpty()` and
    /// `isNotEmpty()` are `List.isEmpty` and its negation, `map`, `filter`, `any` and `all` of a lambda are Lean's own
    /// functions of their names, and `join` of a list of strings is `Sharp.join`. Every other member is refused, naming
    /// those.
    fn list_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let name = String::from_utf8_lossy(method).into_owned();
        let arguments = &call.argument_list.arguments;
        if !LIST_MEMBERS.contains(&(name.as_str(), arguments.len())) {
            let what = format!("the List method {name}");
            return self.unmodeled(expression.span(), what, issues::list_members(&LIST_MEMBERS));
        }
        let list = self.value(call.object);
        match (name.as_str(), arguments.first()) {
            ("count", _) if self.exact => self.bind(format!("int.ofInt (List.length {list} : Int)")),
            ("count", _) => format!("(int.count {list})"),
            ("isEmpty", _) => format!("(List.isEmpty {list})"),
            ("isNotEmpty", _) => format!("(!(List.isEmpty {list}))"),
            ("join", Some(separator)) => format!("(Sharp.join {} {list})", self.value(separator.value())),
            ("map" | "filter" | "any" | "all", Some(argument)) => {
                let Some((parameters, steps, body)) = self.lambda(argument.value()) else {
                    return "default".to_owned();
                };
                if !steps.is_empty() {
                    return self.monadic(&format!("List.{name}M"), (&parameters, &steps, &body), &list);
                }
                let function = format!("(fun {parameters} => {body})");
                if name == "any" || name == "all" {
                    return format!("(List.{name} {list} {function})");
                }

                format!("(List.{name} {function} {list})")
            }
            _ => unreachable!("LIST_MEMBERS holds only the members list_call translates, with their arguments"),
        }
    }

    /// Whether `expression`'s value is a `Map`, null aside.
    pub(super) fn is_map(&self, expression: &Expression) -> bool {
        is_map_type(self.checked.types().expression_type(expression))
    }

    /// A `Map` literal, spec section 12, as `Sharp.Map.ofList` of its entries, each spread's entries in its place.
    fn map_literal(&mut self, array: &Array) -> String {
        let mut parts: Vec<String> = Vec::new();
        let mut entries: Vec<String> = Vec::new();
        for element in &array.elements {
            match element {
                ArrayElement::KeyValue(entry) => {
                    let key = self.value(entry.key);
                    let value = self.value(entry.value);
                    entries.push(format!("({key}, {value})"));
                }
                ArrayElement::Variadic(spread) => {
                    if !entries.is_empty() {
                        parts.push(format!("[{}]", entries.join(", ")));
                        entries.clear();
                    }
                    parts.push(format!("(Sharp.Map.entries {})", self.value(spread.value)));
                }
                element => unreachable!(
                    "the checker accepts a Map literal of entries and spreads of Maps, not {:?}",
                    element.span()
                ),
            }
        }
        if !entries.is_empty() || parts.is_empty() {
            parts.push(format!("[{}]", entries.join(", ")));
        }

        format!("(Sharp.Map.ofList ({}))", parts.join(" ++ "))
    }

    /// A `Map` member a law reads, spec section 28, as the function of `Sharp/Lean/Map.lean` of its name, one of
    /// [`MAP_MEMBERS`]: `count()` in a body that computes exactly is the entries' length checked into the int range,
    /// and `filter` and `any` take a lambda, as a `List`'s do. Every other member is refused, naming those.
    fn map_call(&mut self, expression: &Expression, call: &MethodCall) -> String {
        let Some(method) = self.method_name(&call.method) else {
            return "default".to_owned();
        };
        let name = String::from_utf8_lossy(method).into_owned();
        if !MAP_MEMBERS.contains(&(name.as_str(), call.argument_list.arguments.len())) {
            let what = format!("the Map method {name}");
            return self.unmodeled(expression.span(), what, issues::map_members(&MAP_MEMBERS));
        }
        let map = self.value(call.object);
        if let ("filter" | "any", [argument]) = (name.as_str(), call.argument_list.arguments.as_slice()) {
            let Some((parameters, steps, body)) = self.lambda(argument.value()) else {
                return "default".to_owned();
            };
            if !steps.is_empty() {
                let entries = format!("(Sharp.Map.entries {map})");
                let entry = format!("(_, {parameters})");
                let kept = self.monadic(&format!("List.{name}M"), (&entry, &steps, &body), &entries);

                return if name == "filter" { format!("(Sharp.Map.ofList {kept})") } else { kept };
            }

            return format!("(Sharp.Map.{name} (fun {parameters} => {body}) {map})");
        }
        let Some(arguments) = self.arguments(&call.argument_list, None) else {
            return "default".to_owned();
        };

        match (name.as_str(), arguments.as_slice()) {
            ("count", []) if self.exact => {
                self.bind(format!("int.ofInt (List.length (Sharp.Map.entries {map}) : Int)"))
            }
            ("get", [key]) => self.map_get(call.object, &map, key),
            _ => {
                let mut parts = vec![format!("Sharp.Map.{name}"), map];
                parts.extend(arguments);

                format!("({})", parts.join(" "))
            }
        }
    }

    /// The value of `key` in `map`, the value of `object`, or null. A `Map` of nullable values reads a missing key and a
    /// stored null alike, as its `TValue?` does.
    fn map_get(&self, object: &Expression, map: &str, key: &str) -> String {
        let get = format!("(Sharp.Map.get {map} {key})");
        let nullable = self.checked.types().expression_type(object).types.iter().any(|atomic| match atomic {
            TAtomic::Array(TArray::Keyed(keyed)) => {
                keyed.parameters.as_ref().is_some_and(|(_, value)| value.is_nullable())
                    || keyed
                        .known_items
                        .as_ref()
                        .is_some_and(|items| items.values().any(|(_, value)| value.is_nullable()))
            }
            _ => false,
        });

        if nullable { format!("(Option.join {get})") } else { get }
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

    /// The name bound to `function` of a lambda over `list`, where `function` is Lean's list function of a monadic
    /// lambda, as `List.filterM`, and the lambda takes `parameters`, binds `steps` and gives `body`. A `Map`'s `filter`
    /// and `any` run it over the map's entries, since `ofList` of entries whose keys are distinct keeps their order.
    fn monadic(&mut self, function: &str, (parameters, steps, body): (&str, &[String], &str), list: &str) -> String {
        let temporary = self.fresh();
        self.lines.push(format!("let {temporary} ← {function} (fun {parameters} => do"));
        self.lines.extend(indented(steps, 4));
        self.lines.push(format!("    pure {body}"));
        self.lines.push(format!("  ) {list}"));

        temporary
    }

    /// A lambda `map`, `filter`, `any` or `all` takes: its parameters' Lean names, the steps its body binds, and its
    /// body's term. A body with no steps is a pure function, as `List.map` takes it, and one with steps, as a call of a
    /// PHP# method or arithmetic that can overflow, runs in `M`, as `List.mapM` takes it.
    fn lambda(&mut self, expression: &Expression) -> Option<(String, Vec<String>, String)> {
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
        let (steps, body) = self.code_term(arrow.expression);

        Some((parameters.join(" "), steps, body))
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
        let own = names::member(&names::full_name(&class_name), facts.name.as_bytes());
        let read = self.read_method(declaring, &String::from_utf8_lossy(facts.name.as_bytes()));
        let key = names::member(&names::full_name(&class_name), read.as_bytes());
        let mut parts = vec![key.clone()];
        parts.extend(receiver.map(|receiver| self.upcast(expression.span(), receiver, class, declaring)));
        let smaller =
            arguments.filter(|_| self.reach.same_class(declaring, self.class)).and_then(|list| self.smaller(list));
        let stepped = smaller.as_ref().and_then(|(smaller, term)| Some((smaller.argument, term.clone()?)));
        let Some(arguments) = arguments.map_or_else(|| Some(Vec::new()), |list| self.arguments(list, stepped)) else {
            return "default".to_owned();
        };
        let place = format!("{}.{}", names::short_name(&class_name), facts.name);
        let defaults = self.defaults(declaring, &own, &place, arguments.len()..facts.parameters);
        parts.extend(arguments);
        parts.extend(defaults);
        self.use_of(declaring, &key, place, smaller.map(|(smaller, _)| smaller));

        self.bind(parts.join(" "))
    }

    /// `value`, a value of `class`'s Lean type, as a value of the PHP# class `ancestor` above it, which its structure
    /// holds: `value.toParent`, once for each class up to `ancestor`. A value of a sealed interface's implementer reads
    /// its parent's value through the interface's accessor, spec section 20.1. A value of a case's class is already
    /// one of its enum's, spec section 20. A plain PHP class between them, whose state the structure does not hold, is
    /// refused at `span`.
    fn upcast(&mut self, span: Span, value: String, class: &[u8], ancestor: &[u8]) -> String {
        if self.reach.same_class(class, ancestor)
            || self.case_enum(class).is_some_and(|r#enum| self.reach.same_class(r#enum, ancestor))
        {
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
                    format!("({upcast}).{}", names::identifier(field.as_bytes()))
                }
            };
            below = parent.as_bytes().to_vec();
        }

        upcast
    }

    /// The name of the field of a class's structure that holds the value of its PHP# parent `parent`, as Lean names the
    /// field of the structure a structure extends: `toParent`.
    pub(crate) fn parent_field(&self, parent: &[u8]) -> String {
        format!("to{}", names::short_name(&self.reached_name(parent)))
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

    /// `Class.__construct` of `class`, which builds a value of its own type from every argument its constructor takes,
    /// and records that the body uses it. None for a class `new` cannot build, one whose `new` runs plain PHP code, and
    /// one the law checker does not translate.
    pub(super) fn constructor(&mut self, class: &[u8]) -> Option<String> {
        let reached = self.reach.class(class)?;
        if !reached.sharp || !reached.instantiable || reached.plain_constructor.is_some() || is_collection(class) {
            return None;
        }
        let class_name = self.reached_name(class);
        let key = names::member(&names::full_name(&class_name), b"__construct");
        self.uses(class, &key, format!("new {}", names::short_name(&class_name)));

        Some(key)
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
        let class = self.read_as(class);
        let declaration = self.checked.types().member_declaration(class, property);
        let declaring = declaration.class.as_bytes();
        // Spec section 6: a computed property runs its `get` body as a method of no arguments, which a case of a machine
        // reads as the machine's.
        if matches!(declaration.kind, DeclarationKind::Property { .. })
            && self.reach.method(declaring, property.as_bytes()).is_some()
        {
            return self.call(expression, declaring, class.as_bytes(), property.as_bytes(), Some(object), None);
        }
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
        if !self.reach.class(declaring).is_some_and(|reached| reached.sharp) {
            let what = format!(
                "the plain PHP property {}.{}",
                names::short_name(self.checked.types().class_declaration(declaration.class).name.as_bytes()),
                String::from_utf8_lossy(property.as_bytes())
            );
            return self.unmodeled(expression.span(), what, issues::PLAIN_PHP);
        }
        // Spec section 20.1: an implementer of a sealed interface is a value of the interface, and reads its property
        // through the interface's accessor, as a data case reads its field. A property a PHP# parent declares is a field
        // of the parent's value.
        let read = if self.reach.is_abstract_property(declaring, property.as_bytes()) {
            self.reader(expression.span(), object, class.as_bytes(), declaration.class, property.as_bytes())
        } else if !self.reach.is_stored_field(declaring, property.as_bytes()) {
            return self.unmodeled(expression.span(), format!("the accessor of {place}"), issues::ACCESSOR);
        } else if !self.reach.same_class(declaring, class.as_bytes()) {
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

    /// The read of `declaring`'s property `property`, which has no storage of its own, from `object`, a value of
    /// `class`: a call of `declaring`'s reader, which each implementation answers. A class takes `object` as a value of
    /// the class above it that declares the property, and an interface through the coercion each class that implements
    /// it declares.
    fn reader(&mut self, span: Span, object: String, class: &[u8], declaring: Word, property: &[u8]) -> String {
        let declaring_name = self.reached_name(declaring.as_bytes());
        let key = names::member(&names::full_name(&declaring_name), property);
        let place = format!("{}.{}", names::short_name(&declaring_name), String::from_utf8_lossy(property));
        self.uses(declaring.as_bytes(), &key, place);
        let receiver = if self.checked.types().class_declaration(declaring).kind == DeclarationKind::Interface {
            object
        } else {
            self.upcast(span, object, class, declaring.as_bytes())
        };

        format!("({key} {receiver})")
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
                    self.unmodeled(object.span(), "a collection".to_owned(), issues::list_members(&LIST_MEMBERS));
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
            self.unmodeled(span, "a collection".to_owned(), issues::list_members(&LIST_MEMBERS));
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
        let read = self.read_as(word(class));
        if read != word(class) {
            return self.class_type(span, read.as_bytes());
        }
        if let Some(r#enum) = self.case_enum(class) {
            return self.class_type(span, r#enum);
        }
        match self.sealed(span, class) {
            Some((interface, _)) => interface,
            None => self.own_type(span, class),
        }
    }

    /// The enum whose case the class `class` runs as, spec section 20: a constructor of the enum's inductive type, as a
    /// machine's `OrderStatus.Paid` is of `OrderStatus`.
    fn case_enum<'class>(&self, class: &'class [u8]) -> Option<&'class [u8]> {
        let separator = class.iter().rposition(|&byte| byte == b'\\')?;
        let (r#enum, case) = (&class[..separator], &class[separator + 1..]);

        self.checked.types().enum_case_class(word(r#enum), word(case)).map(|_| r#enum)
    }

    /// The enum whose cases the union `hint` joins, spec section 20.2, as `Counter.Counting|Counter.Done` joins
    /// `Counter`'s, whose Lean type holds each of them, or `None` when it joins anything else.
    fn union_enum(&self, hint: &Hint) -> Option<&'program [u8]> {
        match hint {
            Hint::Union(union) => {
                let (left, right) = (self.union_enum(union.left)?, self.union_enum(union.right)?);

                self.reach.same_class(left, right).then_some(left)
            }
            Hint::Parenthesized(parenthesized) => self.union_enum(parenthesized.hint),
            Hint::Identifier(identifier) => self.case_enum(self.checked.names().get(identifier)),
            _ => None,
        }
    }

    /// The enum whose cases the union `r#type` joins, null aside, spec section 20.2, as a value's type
    /// `Counter.Counting|Counter.Done` joins `Counter`'s, whose Lean type holds each of them. `None` for a type of one
    /// class, and for a union that joins anything else.
    pub(super) fn joined_enum<'joined>(&self, r#type: &'joined TUnion) -> Option<&'joined [u8]> {
        let mut enums = r#type.types.iter().filter(|atomic| !atomic.is_null()).map(|atomic| match atomic {
            TAtomic::Object(TObject::Named(named)) => self.case_enum(named.name.as_bytes()),
            _ => None,
        });
        let first = enums.next()??;
        let mut joined = false;
        for r#enum in enums {
            if !r#enum.is_some_and(|r#enum| self.reach.same_class(r#enum, first)) {
                return None;
            }
            joined = true;
        }

        joined.then_some(first)
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

    /// What a value of `r#type` is in Lean. A machine's value, spec section 20.2, a value of an enum whose cases carry
    /// data, which runs as a class, and a value of the class an enum's case runs as are values of the enum's inductive
    /// type, the closed set of its cases, as any enum's are.
    pub(super) fn kind(&self, r#type: &TUnion) -> Kind {
        let mut kinds = r#type.types.iter().filter(|atomic| !atomic.is_null()).map(|atomic| match atomic {
            TAtomic::Scalar(TScalar::Integer(_)) => Kind::Int,
            TAtomic::Scalar(TScalar::Bool(_)) => Kind::Bool,
            TAtomic::Scalar(TScalar::String(_)) => Kind::String,
            TAtomic::Scalar(TScalar::Float(_)) => Kind::Float,
            TAtomic::Object(TObject::Enum(_)) => Kind::Enum,
            TAtomic::Object(TObject::Named(named)) => {
                let types = self.checked.types();
                if matches!(types.class_declaration(named.name).kind, DeclarationKind::Enum { .. })
                    || types.machine(named.name).is_some()
                    || !self.reach.cases(named.name.as_bytes()).is_empty()
                    || self.case_enum(named.name.as_bytes()).is_some()
                {
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

    /// The statements of a branch whose value is the exact `Int` that `expression`, an int, computes.
    fn exact_branch(&mut self, expression: &Expression) -> Branch {
        self.nested_lines(|body| body.exact_int(expression))
    }

    /// The statements of a branch whose value is `expression`'s, as a value of `whole`, the `?:` or `match` it is a
    /// branch of: where `whole` may be null and `expression` never is, its value is `some` of it, so each branch gives
    /// `whole`'s Lean type, which a branch that gives `none` needs.
    fn branch_of(&mut self, expression: &Expression, whole: &Expression) -> Branch {
        let types = self.checked.types();
        let lifted = types.expression_type(whole).is_nullable() && !types.expression_type(expression).can_be_null();

        self.nested_lines(|body| {
            let value = body.value(expression);
            if lifted { format!("(some {value})") } else { value }
        })
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
    /// is [`Self::bind_choice`].
    fn choose(&mut self, choice: &Choice) -> String {
        term(choice).unwrap_or_else(|| self.bind_choice(choice))
    }

    /// The value of the branch `choice` runs, bound from a nested `do` block, which runs only its own branch's effects.
    fn bind_choice(&mut self, choice: &Choice) -> String {
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

/// An `if` as a translation reads it: its condition, the statement that runs where the condition holds, and the
/// statement of its `else` clause, when it has one.
pub(crate) struct Branches<'ast, 'arena> {
    pub(crate) condition: &'ast Expression<'arena>,
    pub(crate) then: &'ast Statement<'arena>,
    pub(crate) otherwise: Option<&'ast Statement<'arena>>,
}

/// Whether `statement` always returns: a `return`, or a block that ends in one.
fn returns(statement: &Statement) -> bool {
    match statement {
        Statement::Return(_) => true,
        Statement::Block(block) => block.statements.last().is_some_and(returns),
        _ => false,
    }
}

/// The alternatives `pattern` joins with `or`, outside any `and` or `not`, in order: the pattern alone when it joins
/// none.
fn alternatives<'pattern, 'arena>(pattern: &'pattern Pattern<'arena>) -> Vec<&'pattern Pattern<'arena>> {
    match pattern {
        Pattern::Binary(binary) if !binary.is_and() => {
            let mut joined = alternatives(binary.left);
            joined.extend(alternatives(binary.right));

            joined
        }
        Pattern::Parenthesized(parenthesized) => alternatives(parenthesized.pattern),
        _ => vec![pattern],
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

/// The Lean type of a class value of a type no family names, spec section 25, as `Class<Object>`: one opaque type of
/// `Sharp/Lean/Interface.lean`, so a law holds and compares such values and knows nothing else about them.
const CLASS_VALUE: &str = "(Interface \"Sharp.Class\")";

/// Whether `r#type` is a class value and never null, spec section 25.
fn is_class_value(r#type: &TUnion) -> bool {
    matches!(r#type.types.as_ref(), [TAtomic::Scalar(TScalar::ClassLikeString(_))])
}

/// The `List` members a law reads, each by its name and the arguments it takes, spec section 28: [`Body::list_call`]
/// translates each. The refusal of any other member names them.
const LIST_MEMBERS: [(&str, usize); 8] =
    [("count", 0), ("isEmpty", 0), ("isNotEmpty", 0), ("map", 1), ("filter", 1), ("join", 1), ("any", 1), ("all", 1)];

/// The `Map` members a law reads, each by its name and the arguments it takes, spec section 28: each is the function of
/// `Sharp/Lean/Map.lean` of its name. The refusal of any other member names them.
const MAP_MEMBERS: [(&str, usize); 9] = [
    ("count", 0),
    ("isEmpty", 0),
    ("isNotEmpty", 0),
    ("get", 1),
    ("has", 1),
    ("keys", 0),
    ("values", 0),
    ("filter", 1),
    ("any", 1),
];

/// The type of an element of a value of `collection`, a `List` or a `Map` type: a list's element type, or a map's value
/// type, joined with the type of each element or value it is known to hold, as a literal's are.
fn element_type(collection: &TUnion) -> TUnion {
    let mut types: Vec<TAtomic> = Vec::new();
    for atomic in collection.types.iter() {
        let elements: Vec<&TUnion> = match atomic {
            TAtomic::Array(TArray::List(list)) => std::iter::once(list.element_type.as_ref())
                .chain(list.known_elements.iter().flat_map(|known| known.values().map(|(_, element)| element)))
                .collect(),
            TAtomic::Array(TArray::Keyed(map)) => map
                .parameters
                .iter()
                .map(|(_, value)| value.as_ref())
                .chain(map.known_items.iter().flat_map(|known| known.values().map(|(_, value)| value)))
                .collect(),
            _ => continue,
        };
        for element in elements {
            for atomic in element.types.iter().filter(|atomic| !atomic.is_never()) {
                if !types.contains(atomic) {
                    types.push(atomic.clone());
                }
            }
        }
    }

    TUnion::from_vec(types)
}

/// Whether a value of `r#type` is a `Map`, null aside.
fn is_map_type(r#type: &TUnion) -> bool {
    let mut types = r#type.types.iter().filter(|atomic| !atomic.is_null()).peekable();

    types.peek().is_some() && types.all(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_))))
}

/// Whether `class` names a PHP# collection.
fn is_collection(class: &[u8]) -> bool {
    matches!(names::short_name(class).as_str(), "List" | "Map" | "Set")
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_allocator::LocalArena;
    use mago_analyzer::artifacts::AnalysisArtifacts;
    use mago_analyzer::effects::Effects;
    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::ttype::get_int;
    use mago_codex::ttype::get_list;
    use mago_database::file::File;
    use mago_names::resolver::NameResolver;
    use mago_sharp_bridge::InlineForms;
    use mago_sharp_bridge::check;
    use mago_span::Position;
    use mago_syntax::cst::ClassLikeMember;
    use mago_syntax::cst::MethodBody;
    use mago_syntax::cst::Program;
    use mago_syntax::dialect::Dialect;
    use mago_syntax::parser::parse_file_with_dialect;
    use mago_syntax::settings::ParserSettings;

    use super::*;
    use crate::translate::proof::Prover;

    /// Each construct `translate` refuses in `code`, a file in `dialect` that the checks would refuse and that nothing
    /// checks here, as what it is and why. The analysis typed only each expression of `typed`: the text it writes
    /// where it first follows the text before it, as `values` in `("of ", "values")`, with its type.
    fn refusals(
        code: &'static str,
        dialect: Dialect,
        typed: &[(&str, &str, TUnion)],
        translate: impl FnOnce(&Program<'_>, &CheckedProgram<'_>) -> Vec<Unmodeled>,
    ) -> Vec<(String, String)> {
        let file = File::ephemeral(Cow::Borrowed(b"src/Sign.sharp"), Cow::Borrowed(code.as_bytes()));
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, dialect, ParserSettings::default());
        let mut artifacts = AnalysisArtifacts::new();
        for (before, text, r#type) in typed {
            let Some(at) = code.find(before) else {
                panic!("the code writes `{before}`");
            };
            let start = (at + before.len()) as u32;
            let span = Span::new(file.id, Position::new(start), Position::new(start + text.len() as u32));
            artifacts.set_expression_type(&span, r#type.clone());
        }
        let input = artifacts.into_lowering_input();
        let (codebase, forms) = (CodebaseMetadata::new(), InlineForms::default());
        let effects = Effects::solve(&codebase, &[]);
        let names = NameResolver::new(&arena).resolve(program);
        let Ok(checked) = check(&file, program, names, &input, &codebase, &effects, &forms, &[]) else {
            panic!("a program that parses with no issue is accepted");
        };

        translate(program, &checked)
            .into_iter()
            .map(|construct| (construct.what, construct.reason.into_owned()))
            .collect()
    }

    /// The first member of the class `program` declares first.
    fn first_member<'program>(program: &'program Program<'program>) -> &'program ClassLikeMember<'program> {
        let Some(Statement::Class(class)) = program.statements.first() else {
            panic!("the program declares a class first");
        };
        let Some(member) = class.members.first() else {
            panic!("the class declares a member");
        };

        member
    }

    /// An `if` with an else-if clause, which the PHP# checker refuses in a `.sharp` file, is a construct the
    /// translation does not model, so the branch it would drop never becomes part of a proof. Plain PHP writes the
    /// clause.
    #[test]
    fn an_if_with_an_else_if_clause_is_unmodeled() {
        let code = "<?php\nif (true) { return 1; } elseif (false) { return 0; } else { return 2; }\n";

        let refused = refusals(code, Dialect::Php, &[], |program, checked| {
            let Some(r#if) = program.statements.iter().find(|statement| matches!(statement, Statement::If(_))) else {
                panic!("the program holds its if");
            };
            let reach = Reach::default();
            let mut body = Body::new(checked, &reach, b"Sign", "Sign.of".to_owned());
            body.statement(r#if);

            body.finish().2
        });

        assert_eq!(refused, [("an else-if clause".to_owned(), issues::ELSE_IF.to_owned())]);
    }

    /// A law's body proves each branch of an `if`, so an `if` with an else-if clause, whose branch the proof would
    /// leave out, is a construct the translation does not model.
    #[test]
    fn a_law_body_s_if_with_an_else_if_clause_is_unmodeled() {
        let code = "class Sign\n{\n    law holds() => true\n    {\n        if (true) {\n            return;\n        } elseif (false) {\n            return;\n        }\n    }\n}\n";

        let refused = refusals(code, Dialect::Sharp, &[], |program, checked| {
            let ClassLikeMember::Law(law) = first_member(program) else {
                panic!("the class declares its law first");
            };
            let reach = Reach::default();
            let prover = Prover::new(checked, &reach, b"Sign", "Sign.holds".to_owned(), law, "Sign.holds".to_owned());

            prover.prove(&[], &[], false).unmodeled
        });

        assert_eq!(refused, [("an else-if clause".to_owned(), issues::ELSE_IF.to_owned())]);
    }

    /// A counting loop is the sum of what each branch of its `if` adds, so an `if` with an else-if clause, whose
    /// additions the sum would leave out, is a construct the translation does not model, and the loop with it.
    #[test]
    fn a_counting_loop_s_if_with_an_else_if_clause_is_unmodeled() {
        let code = "class Tally\n{\n    public int count(List<int> values)\n    {\n        let total = 0;\n        for (const value of values) {\n            if (true) {\n                total += 1;\n            } elseif (false) {\n                total += 2;\n            }\n        }\n\n        return total;\n    }\n}\n";
        let typed = [("of ", "values", get_list(get_int())), ("{\n                ", "total", get_int())];

        let refused = refusals(code, Dialect::Sharp, &typed, |program, checked| {
            let ClassLikeMember::Method(method) = first_member(program) else {
                panic!("the class declares its method first");
            };
            let MethodBody::Concrete(block) = &method.body else {
                panic!("the method has a body");
            };
            let Some(r#loop) = block.statements.iter().find(|statement| matches!(statement, Statement::ForOf(_)))
            else {
                panic!("the method holds its loop");
            };
            let reach = Reach::default();
            let mut body = Body::new(checked, &reach, b"Tally", "Tally.count".to_owned());
            body.statement(r#loop);

            body.finish().2
        });

        assert_eq!(
            refused,
            [
                ("an else-if clause".to_owned(), issues::ELSE_IF.to_owned()),
                ("a loop that does more than count or sum".to_owned(), issues::LOOP.to_owned()),
            ]
        );
    }
}
