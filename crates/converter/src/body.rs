//! Statements and expressions: `$` leaves each name, `->` and `::` become `.`, a local is declared where it is first
//! assigned, and each construct whose PHP# form is not a token's rewrite builds its text from its children's, rendered
//! with their own edits.
//!
//! An operator converts only where PHP# gives it PHP's meaning for the types the analysis gives its operands, as `+`
//! between two numbers. Anywhere else the file stays plain PHP, with the operator and its operands' types named.

use mago_allocator::LocalArena;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::SymbolReferences;
use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::builder::get_union_from_type;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::union::populate_union_type;
use mago_phpdoc_syntax::PHPDocParser;
use mago_phpdoc_syntax::cst::Element;
use mago_phpdoc_syntax::cst::TagValue;
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
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralStringKind;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Try;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;

use crate::Converter;
use crate::library::type_test;
use crate::types::type_text;

/// The variables PHP fills for every scope, which PHP# removes (spec sections 2 and 29).
const SUPERGLOBALS: [&[u8]; 9] =
    [b"$GLOBALS", b"$_SERVER", b"$_GET", b"$_POST", b"$_FILES", b"$_COOKIE", b"$_SESSION", b"$_REQUEST", b"$_ENV"];

/// The locals of the method being converted. A PHP local starts at its first assignment, and a PHP# local at its
/// declaration (spec section 2), so the first assignment declares it: `const` when nothing writes it again, `let`
/// when something does, and its type when a `@var` tag declares one.
#[derive(Debug, Default)]
pub(crate) struct Locals {
    /// Each local, in the order the body first names it, by its PHP name.
    locals: Vec<(Vec<u8>, Local)>,
    /// The block the conversion is in, innermost last.
    blocks: Vec<Span>,
}

#[derive(Debug)]
struct Local {
    /// The first place the body names it.
    first: Span,
    declaration: Option<Declaration>,
    /// Whether something writes it after its declaration.
    rewritten: bool,
    /// A place that names it outside the block that declares it.
    escapes: Option<Span>,
}

#[derive(Debug)]
struct Declaration {
    /// Where the keyword or the type goes.
    at: u32,
    /// The block whose statement declares it, which PHP# scopes it to (spec section 3).
    block: Span,
    /// The type a `@var` tag declares for it.
    written: Option<String>,
}

/// What a value of an atomic type is to the operators PHP and PHP# share.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Null,
    Int,
    Float,
    String,
    Bool,
    Enum,
    /// It may hold an object, as a class, `object` or `mixed` does.
    Object,
    List,
    Map,
    /// A `Set` or an `Iterable`.
    Collection,
    Other,
}

/// Each kind of value `union` holds, sorted, once each.
fn kinds(union: &TUnion) -> Vec<Kind> {
    let mut kinds: Vec<Kind> = union
        .types
        .iter()
        .map(|atomic| {
            if atomic.is_null() {
                return Kind::Null;
            }
            if atomic.is_mixed() {
                return Kind::Object;
            }
            match get_sharp_type(atomic) {
                Some(SharpType::Int) => Kind::Int,
                Some(SharpType::Float) => Kind::Float,
                Some(SharpType::String) => Kind::String,
                Some(SharpType::Bool) => Kind::Bool,
                Some(SharpType::Enum(_)) => Kind::Enum,
                Some(SharpType::Class(..) | SharpType::Object | SharpType::Parameter(_)) => Kind::Object,
                Some(SharpType::List(_)) => Kind::List,
                Some(SharpType::Map(..)) => Kind::Map,
                Some(SharpType::Set(_) | SharpType::Iterable(_)) => Kind::Collection,
                _ => Kind::Other,
            }
        })
        .collect();
    kinds.sort_unstable();
    kinds.dedup();

    kinds
}

impl<'input> Converter<'input, '_> {
    fn locals(&mut self) -> Option<&mut Locals> {
        self.method.as_mut().map(|method| &mut method.locals)
    }

    pub(crate) fn block(&mut self, block: &Block<'_>) {
        if let Some(locals) = self.locals() {
            locals.blocks.push(block.span());
        }
        for statement in &block.statements {
            self.statement(statement);
        }
        if let Some(locals) = self.locals() {
            locals.blocks.pop();
        }
    }

    pub(crate) fn statement(&mut self, statement: &Statement<'_>) {
        match statement {
            Statement::Expression(expression) => {
                if !self.declaration(statement, expression.expression) {
                    self.expression(expression.expression, None);
                }
            }
            Statement::Return(r#return) => {
                if let Some(value) = r#return.value {
                    let returning = self.method.as_ref().and_then(|method| method.returning.clone());
                    self.expression(value, returning.as_ref());
                }
            }
            Statement::Block(block) => self.block(block),
            Statement::If(r#if) => {
                self.condition(r#if.condition);
                let IfBody::Statement(body) = &r#if.body else {
                    self.refuse(r#if.span(), "an `if` with `:` and `endif` has no PHP# form: write its body in braces");
                    return;
                };
                self.braced(body.statement);
                for clause in &body.else_if_clauses {
                    self.edits.replace(clause.elseif.span, "else if");
                    self.condition(clause.condition);
                    self.braced(clause.statement);
                }
                if let Some(clause) = &body.else_clause {
                    match clause.statement {
                        Statement::If(_) => self.statement(clause.statement),
                        statement => self.braced(statement),
                    }
                }
            }
            Statement::Try(r#try) => self.try_statement(r#try),
            Statement::Noop(_) => {}
            Statement::Foreach(_) | Statement::For(_) | Statement::While(_) | Statement::DoWhile(_) => {
                self.refuse(statement.span(), "a loop is not converted yet");
            }
            Statement::Switch(_) => self.refuse(statement.span(), "a `switch` is not converted yet: it becomes a `match`"),
            Statement::Unset(_) => self.refuse(statement.span(), "`unset` is not converted yet: a `Map` removes a key"),
            Statement::Echo(_) | Statement::EchoTag(_) => self.refuse(
                statement.span(),
                "`echo` is removed: output goes through `Console`, which a class receives through its constructor (spec section 8)",
            ),
            Statement::Global(_) => self.refuse(statement.span(), "`global` is removed (spec section 2)"),
            Statement::Static(r#static) => {
                self.refuse(
                    statement.span(),
                    "a `static` local is not converted yet: its PHP# form is a private static field (spec section 6)",
                );
                // The refusal covers each later read of the local, which has no declaration to point at.
                if let Some(method) = self.method.as_mut() {
                    method.parameters.extend(r#static.items.iter().map(|item| item.variable().name.to_vec()));
                }
            }
            Statement::Goto(_) => self.refuse(statement.span(), "`goto` has no PHP# form"),
            Statement::Label(_) => self.refuse(statement.span(), "a `goto` label has no PHP# form"),
            _ => self.refuse(statement.span(), "this statement has no PHP# form in a method"),
        }
    }

    /// The body of an `if` or an `else`, which PHP# writes in braces (spec section 17).
    fn braced(&mut self, statement: &Statement<'_>) {
        match statement {
            Statement::Block(block) => self.block(block),
            _ => self.refuse(statement.span(), "a body without braces is not converted yet"),
        }
    }

    fn try_statement(&mut self, r#try: &Try<'_>) {
        self.block(&r#try.block);
        for clause in &r#try.catch_clauses {
            match &clause.hint {
                mago_syntax::cst::Hint::Identifier(identifier) => self.rename_class(identifier),
                hint => self.refuse(hint.span(), "a `catch` of several types is not converted yet"),
            }
            // A `catch` variable is bound by the clause, never declared by an assignment.
            if let Some(variable) = &clause.variable {
                self.edits.replace(variable.span, Self::variable_name(variable.name));
                if let Some(method) = self.method.as_mut() {
                    method.parameters.push(variable.name.to_vec());
                }
            }
            self.block(&clause.block);
        }
        if let Some(clause) = &r#try.finally_clause {
            self.block(&clause.block);
        }
    }

    /// A condition, which PHP# takes only as a `bool` (spec section 21).
    fn condition(&mut self, condition: &Expression<'_>) {
        if !self.is(condition, &[Kind::Bool]) {
            let shown = self.shown(condition);
            self.refuse(
                condition.span(),
                format!("a condition of `{shown}` reads PHP's truthiness, which is not converted yet: a PHP# condition is a `bool`"),
            );
            return;
        }

        self.expression(condition, None);
    }

    fn type_of(&self, expression: &Expression<'_>) -> Option<&'input TUnion> {
        self.input.artifacts.get_expression_type(expression)
    }

    /// Whether the analysis gives `expression` a type whose every value is one of `allowed`.
    fn is(&self, expression: &Expression<'_>, allowed: &[Kind]) -> bool {
        self.type_of(expression).is_some_and(|union| {
            let kinds = kinds(union);
            !kinds.is_empty() && kinds.iter().all(|kind| allowed.contains(kind))
        })
    }

    /// The analysis's type of `expression`, as a refusal names it.
    fn shown(&self, expression: &Expression<'_>) -> String {
        self.type_of(expression).map_or_else(|| "no type".to_owned(), |union| union.get_id().to_string())
    }

    pub(crate) fn render(&mut self, expression: &Expression<'_>) -> String {
        let span = expression.span();
        self.edits.render(self.source, span.start.offset, span.end.offset)
    }

    /// Declares a local at `statement` when its expression is the local's first assignment, and gives whether it did.
    fn declaration(&mut self, statement: &Statement<'_>, expression: &Expression<'_>) -> bool {
        let Expression::Assignment(Assignment { lhs: Expression::Variable(Variable::Direct(variable)), operator, rhs }) =
            expression
        else {
            return false;
        };
        if !operator.is_assign() || !self.is_local(variable.name) {
            return false;
        }
        let Some(method) = self.method.as_ref() else {
            return false;
        };
        if method.locals.locals.iter().any(|(name, _)| name.as_slice() == variable.name) {
            return false;
        }

        // A `@var` tag declares the local's type, which PHP# writes in its declaration.
        let tagged = self.tagged_type(statement, variable);
        let written = match tagged {
            Some(union) => {
                let Some(text) = self.type_text(&union, variable.span) else {
                    return true;
                };
                self.docblock(statement.span());
                Some((text, union))
            }
            None => None,
        };
        // An empty literal or `null` says nothing of what the local holds (spec sections 2 and 12).
        if written.is_none() && is_untyped_value(rhs) {
            let name = String::from_utf8_lossy(variable.name);
            self.refuse(
                statement.span(),
                format!(
                    "`{name}` starts as `{}`, which says nothing of what it holds: declare its type with a `@var` tag",
                    String::from_utf8_lossy(self.text(rhs.span()))
                ),
            );
        }

        let block = self.method.as_ref().and_then(|method| method.locals.blocks.last().copied());
        let Some(method) = self.method.as_mut() else {
            return true;
        };
        let declaration = Declaration {
            at: variable.span.start.offset,
            block: block.unwrap_or(statement.span()),
            written: written.as_ref().map(|(text, _)| text.clone()),
        };
        method.locals.locals.push((
            variable.name.to_vec(),
            Local { first: variable.span, declaration: Some(declaration), rewritten: false, escapes: None },
        ));
        self.edits.replace(variable.span, Self::variable_name(variable.name));
        let expected = written.map(|(_, union)| union);
        self.expression(rhs, expected.as_ref());

        true
    }

    /// The type a `@var` tag right before `statement` declares for `variable`, read as the analysis reads the tag: by
    /// the file's names and the method's type parameters.
    fn tagged_type(&self, statement: &Statement<'_>, variable: &DirectVariable<'_>) -> Option<TUnion> {
        let (span, text) = self.docblock_before(statement.span())?;
        let arena = LocalArena::new();
        let document = PHPDocParser::parse_with_span(&arena, text, span);
        let resolution = self.method.as_ref().map(|method| method.resolution.clone()).unwrap_or_default();
        let class = self.class.as_ref().map(|class| class.metadata.name);

        document.elements.iter().find_map(|element| {
            let Element::Tag(tag) = element else {
                return None;
            };
            let TagValue::Var(var) = &tag.value else {
                return None;
            };
            if var.variable.is_some_and(|named| named.value != variable.name) {
                return None;
            }
            let mut union = get_union_from_type(var.r#type, &self.scope, &resolution, class).ok()?;
            populate_union_type(&mut union, &self.input.codebase.symbols, None, &mut SymbolReferences::new(), true);
            expand_union(
                self.input.codebase,
                &mut union,
                &TypeExpansionOptions { self_class: class, ..Default::default() },
            );

            Some(union)
        })
    }

    /// Whether `name` is a local of the method being converted: no parameter, no `catch` variable and not `$this`.
    fn is_local(&self, name: &[u8]) -> bool {
        name != b"$this"
            && self.method.as_ref().is_some_and(|method| !method.parameters.iter().any(|parameter| parameter == name))
    }

    /// Records that the body names the local `variable` here, writing it when `write` says so.
    fn local(&mut self, variable: &DirectVariable<'_>, write: bool) {
        if !self.is_local(variable.name) {
            return;
        }
        let span = variable.span;
        let Some(method) = self.method.as_mut() else {
            return;
        };
        match method.locals.locals.iter_mut().find(|(name, _)| name.as_slice() == variable.name) {
            Some((_, local)) => {
                local.rewritten |= write;
                if let Some(declaration) = &local.declaration
                    && local.escapes.is_none()
                    && !(declaration.block.start.offset <= span.start.offset
                        && span.end.offset <= declaration.block.end.offset)
                {
                    local.escapes = Some(span);
                }
            }
            None => method.locals.locals.push((
                variable.name.to_vec(),
                Local { first: span, declaration: None, rewritten: write, escapes: None },
            )),
        }
    }

    /// Writes each local's declaration at its first assignment, once the method's body is converted and every write
    /// to it is known.
    pub(crate) fn declare_locals(&mut self) {
        let Some(method) = self.method.as_mut() else {
            return;
        };
        let locals = std::mem::take(&mut method.locals.locals);
        for (name, local) in locals {
            let name = String::from_utf8_lossy(&name);
            let Some(declaration) = local.declaration else {
                self.refuse(
                    local.first,
                    format!("`{name}` is first named here, outside an assignment of its own, which is not converted yet: a PHP# local is declared where it is first assigned"),
                );
                continue;
            };
            if let Some(span) = local.escapes {
                self.refuse(
                    span,
                    format!("`{name}` is named here, after the block it is first assigned in, which is not converted yet: PHP# scopes a local to its block (spec section 3)"),
                );
                continue;
            }
            let keyword = match declaration.written {
                Some(written) => written,
                None if local.rewritten => "let".to_owned(),
                None => "const".to_owned(),
            };
            self.edits.insert(declaration.at, format!("{keyword} "));
        }
    }

    /// `expression` in PHP#. `expected` is the declared type of the place its value lands, when there is one, which
    /// decides the PHP# form of a literal, as `[]` is an empty `Map`, `[:]`, where a `Map` is declared (spec section
    /// 12).
    pub(crate) fn expression(&mut self, expression: &Expression<'_>, expected: Option<&TUnion>) {
        match expression {
            Expression::Variable(Variable::Direct(variable)) => self.variable(variable, false),
            Expression::Variable(_) => {
                self.refuse(expression.span(), "a variable named by a variable is removed (spec section 2)");
            }
            Expression::Parenthesized(parenthesized) => self.expression(parenthesized.expression, expected),
            Expression::Literal(literal) => self.literal(literal, expected),
            Expression::Identifier(identifier) => self.rename_class(identifier),
            Expression::ConstantAccess(constant) => self.constant(expression, constant),
            Expression::Self_(keyword) => {
                let name = self.class.as_ref().map(|class| class.name.clone()).unwrap_or_default();
                self.edits.replace(keyword.span, name);
            }
            Expression::Static(keyword) => self.edits.replace(keyword.span, "Self"),
            Expression::Parent(keyword) => self.edits.replace(keyword.span, "super"),
            Expression::Access(access) => self.access(expression, access),
            Expression::Call(call) => self.call(call),
            Expression::Instantiation(instantiation) => self.instantiation(expression, instantiation, expected),
            Expression::Binary(binary) => self.binary(expression, binary),
            Expression::UnaryPrefix(unary) => self.unary(expression, &unary.operator, unary.operand),
            Expression::UnaryPostfix(unary) => {
                if !self.is(unary.operand, &[Kind::Int, Kind::Float]) {
                    let shown = self.shown(unary.operand);
                    return self.refuse(
                        expression.span(),
                        format!("`++` and `--` on `{shown}` step PHP's way, which PHP# does only for a number"),
                    );
                }
                self.written(unary.operand);
            }
            Expression::Assignment(assignment) => self.assignment(expression, assignment, expected),
            Expression::Array(array) => {
                if array.elements.is_empty() {
                    return self.empty_array(expression, expected);
                }
                self.elements(array.elements.iter());
            }
            Expression::LegacyArray(array) => {
                if array.elements.is_empty() {
                    return self.empty_array(expression, expected);
                }
                self.edits.replace_range(array.array.span.start.offset, array.left_parenthesis.end.offset, "[");
                self.edits.replace(array.right_parenthesis, "]");
                self.elements(array.elements.iter());
            }
            Expression::ArrayAccess(access) => {
                self.untyped(access.array, "an index");
                self.untyped(access.index, "a key");
            }
            Expression::Conditional(conditional) => {
                let Some(then) = conditional.then else {
                    return self.refuse(expression.span(), "`?:` has no PHP# form: `??` covers null (spec section 14)");
                };
                self.condition(conditional.condition);
                self.expression(then, expected);
                self.expression(conditional.r#else, expected);
            }
            Expression::Throw(throw) => self.expression(throw.exception, None),
            Expression::Closure(_) | Expression::ArrowFunction(_) | Expression::PartialApplication(_) => {
                self.refuse(expression.span(), "a function as a value is not converted yet");
            }
            Expression::Match(_) => self.refuse(expression.span(), "a `match` is not converted yet"),
            Expression::Clone(_) => {
                self.refuse(expression.span(), "`clone` is not converted yet: it becomes `with { }`")
            }
            Expression::List(_) => self.refuse(expression.span(), "destructuring is not converted yet"),
            Expression::CompositeString(_) => {
                self.refuse(
                    expression.span(),
                    "an interpolated string or a heredoc is not converted yet: it becomes a template",
                );
            }
            Expression::MagicConstant(_) => {
                self.refuse(
                    expression.span(),
                    "a magic constant is not converted yet: `Position` replaces it (spec section 27)",
                );
            }
            Expression::AnonymousClass(_) => {
                self.refuse(expression.span(), "an anonymous class has no PHP# form: no type nests in another");
            }
            Expression::Construct(_) => self.construct(expression),
            Expression::Yield(_) => self.refuse(expression.span(), "a generator is not converted yet"),
            _ => self.refuse(expression.span(), "this expression has no PHP# form"),
        }
    }

    fn variable(&mut self, variable: &DirectVariable<'_>, write: bool) {
        if SUPERGLOBALS.contains(&variable.name) {
            return self.refuse(
                variable.span,
                format!("`{}` is removed (spec sections 2 and 29)", String::from_utf8_lossy(variable.name)),
            );
        }
        self.local(variable, write);
        self.edits.replace(variable.span, Self::variable_name(variable.name));
    }

    /// Converts `place`, the target of a write, and records the write on the local it names.
    fn written(&mut self, place: &Expression<'_>) {
        match place {
            Expression::Variable(Variable::Direct(variable)) => self.variable(variable, true),
            place => self.expression(place, None),
        }
    }

    /// `[]`, which PHP# writes `[:]` where a `Map` is declared, and refuses where the place holds a `List` or a `Map`,
    /// since nothing picks one.
    fn empty_array(&mut self, expression: &Expression<'_>, expected: Option<&TUnion>) {
        let kinds = expected.map(kinds).unwrap_or_default();
        let (list, map) = (kinds.contains(&Kind::List), kinds.contains(&Kind::Map));
        if list && map {
            let shown = expected.map(|union| union.get_id().to_string()).unwrap_or_default();
            return self.refuse(
                expression.span(),
                format!("`[]` lands in `{shown}`, which holds a `List` or a `Map`, and PHP# writes an empty one as `[]` or `[:]` (spec section 12)"),
            );
        }
        self.edits.replace(expression.span(), if map { "[:]" } else { "[]" });
    }

    fn elements<'element, 'arena>(&mut self, elements: impl Iterator<Item = &'element ArrayElement<'arena>>)
    where
        'arena: 'element,
    {
        for element in elements {
            match element {
                ArrayElement::KeyValue(element) => {
                    self.expression(element.key, None);
                    self.edits.replace(element.double_arrow, ":");
                    self.expression(element.value, None);
                }
                ArrayElement::Value(element) => self.expression(element.value, None),
                ArrayElement::Variadic(_) => self.refuse(element.span(), "a spread is not converted yet"),
                ArrayElement::Missing(_) => self.refuse(element.span(), "a skipped element has no PHP# form"),
            }
        }
    }

    fn literal(&mut self, literal: &Literal<'_>, expected: Option<&TUnion>) {
        match literal {
            Literal::True(keyword) => self.edits.replace(keyword.span, "true"),
            Literal::False(keyword) => self.edits.replace(keyword.span, "false"),
            Literal::Null(keyword) => self.edits.replace(keyword.span, "null"),
            // PHP turns an `int` that lands in a `float` into a `float`, and PHP# writes the `float` (spec section 24).
            Literal::Integer(integer)
                if integer.raw.iter().all(u8::is_ascii_digit)
                    && expected.is_some_and(|union| kinds(union) == [Kind::Float]) =>
            {
                self.edits.replace(integer.span, format!("{}.0", String::from_utf8_lossy(integer.raw)));
            }
            Literal::String(_) | Literal::Integer(_) | Literal::Float(_) => {}
        }
    }

    /// A constant: a library's, through its namespace as its functions are (spec section 23), or PHP's own, which the
    /// standard library names.
    fn constant(&mut self, expression: &Expression<'_>, constant: &ConstantAccess<'_>) {
        let codebase = self.input.codebase;
        let written = constant.name.value();
        let Some(metadata) = codebase
            .get_constant(self.input.names.get(&constant.name))
            .or_else(|| (!written.contains(&b'\\')).then(|| codebase.get_constant(written)).flatten())
        else {
            return self.refuse(expression.span(), "the analysis holds no constant of this name");
        };
        let name = metadata.name.as_str_lossy().into_owned();
        if metadata.flags.is_built_in() {
            return self.refuse(
                expression.span(),
                format!("`{name}` is PHP's own constant, which is not converted yet: the standard library names it"),
            );
        }
        let Some(text) = self.through_namespace(expression.span(), &name, metadata.span) else {
            return;
        };
        self.edits.replace(expression.span(), text);
    }

    /// `name`, a library's function or constant declared at `declared`, read through its namespace: its own, or the
    /// namespace of the library's classes beside it when it is in PHP's root namespace (spec section 23, decision 083).
    fn through_namespace(&mut self, span: Span, name: &str, declared: Span) -> Option<String> {
        let (namespace, member) = match name.rsplit_once('\\') {
            Some((namespace, member)) => (namespace.to_owned(), member.to_owned()),
            None => match self.input.codebase.folder_namespaces(declared).first() {
                Some(namespace) => (namespace.as_str_lossy().into_owned(), name.to_owned()),
                None => {
                    self.refuse(
                        span,
                        format!("`{name}` is declared in PHP's root namespace beside no class of its library, so it has no namespace to be reached through (spec section 23)"),
                    );
                    return None;
                }
            },
        };
        let alias = self.imports.import(&namespace);

        Some(format!("{alias}.{member}"))
    }

    fn access(&mut self, expression: &Expression<'_>, access: &Access<'_>) {
        match access {
            Access::Property(access) => {
                self.untyped(access.object, "a property access");
                self.edits.replace(access.arrow, ".");
                self.member(&access.property);
            }
            Access::NullSafeProperty(access) => {
                self.untyped(access.object, "a property access");
                self.edits.replace(access.question_mark_arrow, "?.");
                self.member(&access.property);
            }
            Access::StaticProperty(access) => {
                self.expression(access.class, None);
                self.edits.replace(access.double_colon, ".");
                match &access.property {
                    Variable::Direct(variable) => self.edits.replace(variable.span, Self::variable_name(variable.name)),
                    _ => self.refuse(
                        access.property.span(),
                        "a property named by a variable has no PHP# form (spec section 25)",
                    ),
                }
            }
            Access::ClassConstant(access) => {
                let ClassLikeConstantSelector::Identifier(constant) = &access.constant else {
                    return self.refuse(expression.span(), "a constant named by an expression has no PHP# form");
                };
                self.expression(access.class, None);
                if constant.value.eq_ignore_ascii_case(b"class") {
                    // `X::class` is the class value `typeof(X)` (spec section 25).
                    let class = self.render(access.class);
                    self.edits.replace(expression.span(), format!("typeof({class})"));
                } else {
                    self.edits.replace(access.double_colon, ".");
                }
            }
        }
    }

    fn member(&mut self, selector: &ClassLikeMemberSelector<'_>) {
        if !matches!(selector, ClassLikeMemberSelector::Identifier(_)) {
            self.refuse(selector.span(), "a member named by a variable has no PHP# form (spec section 25)");
        }
    }

    /// Refuses `expression` when the analysis gives it `mixed`, which PHP lets reach `operation` and PHP# lets reach it
    /// only from a known type, and gives whether it did. Its own parts convert either way.
    fn untyped(&mut self, expression: &Expression<'_>, operation: &str) -> bool {
        self.expression(expression, None);
        if !self.type_of(expression).is_some_and(|union| union.types.iter().any(TAtomic::is_mixed)) {
            return false;
        }
        let shown = self.shown(expression);
        self.refuse(
            expression.span(),
            format!("`{shown}` reaches {operation}, which PHP# allows only from a known type: narrow it with `is`, or declare its type (spec section 24)"),
        );

        true
    }

    /// The method `selector` names on a value of type `object`, when the analysis knows the value's class.
    fn method_of(
        &self,
        object: Option<&TUnion>,
        selector: &ClassLikeMemberSelector<'_>,
    ) -> Option<&'input FunctionLikeMetadata> {
        let ClassLikeMemberSelector::Identifier(name) = selector else {
            return None;
        };
        let [TAtomic::Object(TObject::Named(class))] = object?.types.as_ref() else {
            return None;
        };

        self.input.codebase.get_declaring_method(class.name.as_bytes(), name.value)
    }

    /// The method `selector` names, written as its class declares it: PHP finds a method in any case, and PHP# only in
    /// its own (spec section 25).
    fn method_name(&mut self, selector: &ClassLikeMemberSelector<'_>, method: Option<&FunctionLikeMetadata>) {
        if let (ClassLikeMemberSelector::Identifier(name), Some(method)) = (selector, method)
            && name.value != method.original_name.as_bytes()
        {
            self.edits.replace(name.span, method.original_name.as_str_lossy().into_owned());
        }
    }

    /// The arguments of a call of `callee`, when the analysis knows it. An argument the analysis gives `mixed` reaches
    /// a parameter of a known type only in PHP.
    fn arguments(&mut self, arguments: &ArgumentList<'_>, callee: Option<&FunctionLikeMetadata>) {
        for (index, argument) in arguments.arguments.iter().enumerate() {
            let parameter = callee.and_then(|callee| match argument {
                Argument::Named(argument) => {
                    let name = [b"$".as_slice(), argument.name.value].concat();
                    callee.parameters.iter().find(|parameter| parameter.get_name().0.as_bytes() == name)
                }
                Argument::Positional(_) => callee.parameters.get(index),
            });
            let typed = parameter
                .and_then(|parameter| parameter.get_type_metadata())
                .is_some_and(|declared| !declared.type_union.types.iter().any(TAtomic::is_mixed));
            match argument {
                Argument::Positional(argument) if argument.ellipsis.is_some() => {
                    self.refuse(argument.span(), "a spread argument is not converted yet");
                }
                Argument::Positional(argument) if typed => {
                    self.untyped(argument.value, "a parameter of a known type");
                }
                Argument::Named(argument) if typed => {
                    self.untyped(argument.value, "a parameter of a known type");
                }
                Argument::Positional(argument) => self.expression(argument.value, None),
                Argument::Named(argument) => self.expression(argument.value, None),
            }
        }
    }

    fn call(&mut self, call: &Call<'_>) {
        match call {
            Call::Method(call) => {
                self.untyped(call.object, "a method call");
                self.edits.replace(call.arrow, ".");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(&call.method, method);
                self.arguments(&call.argument_list, method);
            }
            Call::NullSafeMethod(call) => {
                self.untyped(call.object, "a method call");
                self.edits.replace(call.question_mark_arrow, "?.");
                self.member(&call.method);
                let method = self.method_of(self.type_of(call.object), &call.method);
                self.method_name(&call.method, method);
                self.arguments(&call.argument_list, method);
            }
            Call::StaticMethod(call) => {
                self.expression(call.class, None);
                self.edits.replace(call.double_colon, ".");
                self.member(&call.method);
                self.arguments(&call.argument_list, None);
            }
            Call::Function(call) => self.function_call(call),
            Call::NullSafeFunction(_) => self.refuse(call.span(), "`?.(…)` on a function has no PHP# form"),
        }
    }

    /// The function a bare call runs: the one its resolved name names, or PHP's own of the name it writes, as PHP falls
    /// back to a function of the root namespace.
    fn function_of(&self, identifier: &Identifier<'_>) -> Option<&'input FunctionLikeMetadata> {
        let codebase = self.input.codebase;

        codebase.get_function(self.input.names.get(identifier)).or_else(|| {
            (!identifier.value().contains(&b'\\')).then(|| codebase.get_function(identifier.value())).flatten()
        })
    }

    /// A bare PHP function call: a library's function through its namespace, a type test as `is`, or a refusal that
    /// names the function.
    fn function_call(&mut self, call: &FunctionCall<'_>) {
        let span = call.span();
        let Expression::Identifier(identifier) = call.function else {
            return self.refuse(span, "a call of a value is not converted yet");
        };
        let Some(function) = self.function_of(identifier) else {
            return self.refuse(span, "the analysis holds no function this call runs");
        };
        let name = function.original_name.as_str_lossy().into_owned();

        if !function.flags.is_built_in() {
            let Some(callee) = self.through_namespace(span, &name, function.span) else {
                return;
            };
            self.arguments(&call.argument_list, Some(function));
            let list = call.argument_list.span();
            let arguments = self.edits.render(self.source, list.start.offset, list.end.offset);
            return self.edits.replace(span, format!("{callee}{arguments}"));
        }

        if name.eq_ignore_ascii_case("compact") || name.eq_ignore_ascii_case("extract") {
            return self.refuse(span, format!("`{name}` is removed (spec section 2)"));
        }
        let Some(target) = type_test(function) else {
            return self.refuse(
                span,
                format!("`{name}` is a PHP built-in, which is not converted yet: the standard library runs it"),
            );
        };
        let [Argument::Positional(value)] = call.argument_list.arguments.as_slice() else {
            return self.refuse(span, format!("`{name}` with a named or spread argument has no PHP# form"));
        };
        if value.ellipsis.is_some() {
            return self.refuse(span, format!("`{name}` with a named or spread argument has no PHP# form"));
        }
        let tested = if target.is_null() {
            None
        } else {
            match type_text(target, self.input.codebase, &mut self.imports) {
                Ok(text) => Some(text),
                Err(reason) => return self.refuse(span, format!("`{name}` tests a type PHP# can't write: {reason}")),
            }
        };
        self.expression(value.value, None);
        let value = self.render(value.value);
        let test = match tested {
            Some(tested) => format!("{value} is {tested}"),
            None => format!("{value} == null"),
        };
        self.edits.replace(span, test);
    }

    /// The value and the type `expression` tests, each converted, when it is `$x instanceof A`, or a call of the
    /// prelude's type test of a type PHP# writes, as `is_string($x)`.
    fn test(&mut self, expression: &Expression<'_>) -> Option<(String, String)> {
        match expression {
            Expression::Parenthesized(parenthesized) => self.test(parenthesized.expression),
            Expression::Binary(Binary { lhs, operator: BinaryOperator::Instanceof(_), rhs }) => {
                let Expression::Identifier(class) = rhs else {
                    return None;
                };
                let tested = self.class_name(class)?;
                self.expression(lhs, None);

                Some((self.render(lhs), tested))
            }
            Expression::Call(Call::Function(call)) => {
                let Expression::Identifier(identifier) = call.function else {
                    return None;
                };
                let target = type_test(self.function_of(identifier)?)?;
                if target.is_null() {
                    return None;
                }
                let [Argument::Positional(value)] = call.argument_list.arguments.as_slice() else {
                    return None;
                };
                if value.ellipsis.is_some() {
                    return None;
                }
                let tested = type_text(target, self.input.codebase, &mut self.imports).ok()?;
                self.expression(value.value, None);

                Some((self.render(value.value), tested))
            }
            _ => None,
        }
    }

    /// `new`, which names a generic class's type arguments (spec section 11). Generics are reified, so an argument the
    /// analysis inferred would be a runtime `TypeError` when it is wrong: the arguments are those of the declared type
    /// the value lands in, or the class's bounds, which plain PHP's `new` has.
    fn instantiation(
        &mut self,
        expression: &Expression<'_>,
        instantiation: &Instantiation<'_>,
        expected: Option<&TUnion>,
    ) {
        // PHP# always writes a call's parentheses.
        let parentheses = if instantiation.argument_list.is_none() { "()" } else { "" };
        match instantiation.class {
            Expression::Identifier(identifier) => {
                let Some(name) = self.class_name(identifier) else {
                    return;
                };
                let mut written = Vec::new();
                if let Some(class) = self.input.codebase.get_class_like(self.input.names.get(identifier))
                    && !class.template_types.is_empty()
                {
                    let declared = expected.and_then(|union| {
                        let mut objects = union.types.iter().filter(|atomic| !atomic.is_null());
                        match (objects.next(), objects.next()) {
                            (Some(TAtomic::Object(object)), None)
                                if object.get_name().is_some_and(|name| {
                                    name.as_bytes().eq_ignore_ascii_case(class.name.as_bytes())
                                }) =>
                            {
                                object.get_type_parameters().map(<[TUnion]>::to_vec)
                            }
                            _ => None,
                        }
                    });
                    let arguments = declared.unwrap_or_else(|| {
                        class.template_types.values().map(|template| template.constraint.clone()).collect()
                    });
                    for argument in &arguments {
                        match type_text(argument, self.input.codebase, &mut self.imports) {
                            Ok(text) => written.push(text),
                            Err(reason) => return self.refuse(expression.span(), reason),
                        }
                    }
                }
                let arguments = if written.is_empty() { String::new() } else { format!("<{}>", written.join(", ")) };
                self.edits.replace(identifier.span(), format!("{name}{arguments}{parentheses}"));
            }
            Expression::Self_(keyword) => {
                let name = self.class.as_ref().map(|class| class.name.clone()).unwrap_or_default();
                self.edits.replace(keyword.span, format!("{name}{parentheses}"));
            }
            _ => return self.refuse(expression.span(), "`new` of a class value is not converted yet"),
        }
        if let Some(arguments) = &instantiation.argument_list {
            self.arguments(arguments, None);
        }
    }

    fn construct(&mut self, expression: &Expression<'_>) {
        let Expression::Construct(construct) = expression else {
            return;
        };
        let reason = match construct {
            mago_syntax::cst::Construct::Isset(_) | mago_syntax::cst::Construct::Empty(_) => {
                "`isset` and `empty` are not converted yet"
            }
            mago_syntax::cst::Construct::Print(_) => {
                "`print` is removed: output goes through `Console`, which a class receives through its constructor (spec section 8)"
            }
            mago_syntax::cst::Construct::Exit(_) | mago_syntax::cst::Construct::Die(_) => {
                "`exit` and `die` are removed: `Process` exits, which a class receives through its constructor (spec section 8)"
            }
            _ => "`include`, `require` and `eval` have no PHP# form: a .sharp file declares one type (spec section 27)",
        };
        self.refuse(expression.span(), reason);
    }

    fn unary(&mut self, expression: &Expression<'_>, operator: &UnaryPrefixOperator<'_>, operand: &Expression<'_>) {
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

    fn binary(&mut self, expression: &Expression<'_>, binary: &Binary<'_>) {
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
    fn operands(&self, operator: &str, lhs: &Expression<'_>, rhs: &Expression<'_>, reason: &str) -> String {
        format!("`{operator}` between `{}` and `{}` {reason}", self.shown(lhs), self.shown(rhs))
    }

    /// A chain of `.`: `+` between strings (spec section 18), or a template when a part is a number, which a template
    /// shows as PHP shows it.
    fn concatenation(&mut self, expression: &Expression<'_>) {
        let mut parts = Vec::new();
        flatten_concatenation(expression, &mut parts);

        if parts.iter().all(|part| self.is(part, &[Kind::String])) {
            let mut operators = Vec::new();
            collect_concatenation_operators(expression, &mut operators);
            for span in operators {
                self.edits.replace(span, "+");
            }
            for part in parts {
                self.expression(part, None);
            }
            return;
        }

        let mut template = String::from("`");
        for part in parts {
            if let Expression::Literal(Literal::String(string)) = part
                && let Some(value) = string.value
                && (string.kind == LiteralStringKind::SingleQuoted || !string.raw.contains(&b'\\'))
                && !value.iter().any(|byte| matches!(byte, b'`' | b'$' | b'\\'))
            {
                template.push_str(&String::from_utf8_lossy(value));
                continue;
            }
            if !self.is(part, &[Kind::Int, Kind::Float, Kind::String]) {
                let shown = self.shown(part);
                return self.refuse(
                    part.span(),
                    format!("`.` of `{shown}` shows it PHP's way, and a template shows only `int`, `float` and `string` that way (spec section 18)"),
                );
            }
            self.expression(part, None);
            let rendered = self.render(part);
            template.push_str("${");
            template.push_str(&rendered);
            template.push('}');
        }
        template.push('`');
        self.edits.replace(expression.span(), template);
    }

    fn assignment(&mut self, expression: &Expression<'_>, assignment: &Assignment<'_>, expected: Option<&TUnion>) {
        let (lhs, rhs) = (assignment.lhs, assignment.rhs);
        let destructured = match lhs {
            Expression::Array(array) => Some(&array.elements),
            Expression::LegacyArray(array) => Some(&array.elements),
            Expression::List(list) => Some(&list.elements),
            _ => None,
        };
        if let Some(elements) = destructured {
            self.refuse(expression.span(), "destructuring is not converted yet");
            // The refusal covers each later read of the locals it assigns, which have no declaration to point at.
            if let Some(method) = self.method.as_mut() {
                for element in elements.iter() {
                    if let Some(Expression::Variable(Variable::Direct(variable))) = element.get_value() {
                        method.parameters.push(variable.name.to_vec());
                    }
                }
            }
            return;
        }
        if let Expression::ArrayAppend(append) = lhs {
            if !assignment.operator.is_assign() {
                return self.refuse(expression.span(), "a compound write to `[]` has no PHP# form");
            }
            // `$list[] = $value` appends, which PHP# writes `list.add(value)` (spec section 12).
            self.written(append.array);
            self.expression(rhs, None);
            let list = self.render(append.array);
            let value = self.render(rhs);
            return self.edits.replace(expression.span(), format!("{list}.add({value})"));
        }
        if let Expression::ArrayAccess(access) = lhs {
            // A `Map` sets a key PHP's way, and a `List`'s index write replaces an element where PHP's adds one.
            if !self.is(access.array, &[Kind::Map]) {
                let shown = self.shown(access.array);
                return self.refuse(
                    expression.span(),
                    format!("a write at an index of `{shown}` is not converted yet: a PHP# `Map` sets a key, and a `List` replaces an element"),
                );
            }
            self.written(access.array);
            self.expression(access.index, None);
            return self.expression(rhs, None);
        }

        let numbers = [Kind::Int, Kind::Float];
        let refusal = match &assignment.operator {
            AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => None,
            AssignmentOperator::Concat(span) => {
                if self.is(lhs, &[Kind::String]) && self.is(rhs, &[Kind::String]) {
                    self.edits.replace(*span, "+=");
                    None
                } else {
                    Some(self.operands(
                        ".=",
                        lhs,
                        rhs,
                        "shows a value PHP's way, which is not converted yet: PHP# joins only strings with `+=`",
                    ))
                }
            }
            AssignmentOperator::Addition(_)
            | AssignmentOperator::Subtraction(_)
            | AssignmentOperator::Multiplication(_) => {
                (!self.is(lhs, &numbers) || !self.is(rhs, &numbers)).then(|| {
                    self.operands(
                        &String::from_utf8_lossy(self.text(assignment.operator.span())),
                        lhs,
                        rhs,
                        "turns them into numbers PHP's way, which PHP# does only between numbers",
                    )
                })
            }
            AssignmentOperator::Division(_) => (!self.is(lhs, &[Kind::Float]) || !self.is(rhs, &numbers)).then(|| {
                self.operands(
                    "/=",
                    lhs,
                    rhs,
                    "divides PHP's way, which PHP# does only into a `float` (spec section 24)",
                )
            }),
            AssignmentOperator::Modulo(_)
            | AssignmentOperator::Exponentiation(_)
            | AssignmentOperator::BitwiseAnd(_)
            | AssignmentOperator::BitwiseOr(_)
            | AssignmentOperator::BitwiseXor(_)
            | AssignmentOperator::LeftShift(_)
            | AssignmentOperator::RightShift(_) => {
                (!self.is(lhs, &[Kind::Int]) || !self.is(rhs, &[Kind::Int])).then(|| {
                    self.operands(
                        &String::from_utf8_lossy(self.text(assignment.operator.span())),
                        lhs,
                        rhs,
                        "has PHP's meaning in PHP# only between `int`s (spec section 19)",
                    )
                })
            }
        };
        if let Some(reason) = refusal {
            return self.refuse(expression.span(), reason);
        }

        let declared = self.declared_type(lhs).or_else(|| expected.cloned());
        self.written(lhs);
        self.expression(rhs, declared.as_ref());
    }

    /// The declared type of the place `place` names, when it is a property of `this`: the type a value written there
    /// takes, which the analysis's type of the place, narrowed by the code before it, may not be.
    fn declared_type(&self, place: &Expression<'_>) -> Option<TUnion> {
        let Expression::Access(Access::Property(access)) = place else {
            return None;
        };
        let (Expression::Variable(Variable::Direct(object)), ClassLikeMemberSelector::Identifier(name)) =
            (access.object, &access.property)
        else {
            return None;
        };
        if object.name != b"$this" {
            return None;
        }
        let property = [b"$".as_slice(), name.value].concat();
        let metadata = self.class.as_ref()?.metadata.properties.get(&mago_word::word(&property))?;

        Some(metadata.type_metadata.as_ref()?.type_union.clone())
    }
}

/// Whether `value` says nothing of what a local holds: `null`, or an empty array.
fn is_untyped_value(value: &Expression<'_>) -> bool {
    match value {
        Expression::Literal(Literal::Null(_)) => true,
        Expression::Array(array) => array.elements.is_empty(),
        Expression::LegacyArray(array) => array.elements.is_empty(),
        Expression::Parenthesized(parenthesized) => is_untyped_value(parenthesized.expression),
        _ => false,
    }
}

fn flatten_concatenation<'expression, 'arena>(
    expression: &'expression Expression<'arena>,
    parts: &mut Vec<&'expression Expression<'arena>>,
) {
    match expression {
        Expression::Binary(Binary { lhs, operator: BinaryOperator::StringConcat(_), rhs }) => {
            flatten_concatenation(lhs, parts);
            flatten_concatenation(rhs, parts);
        }
        _ => parts.push(expression),
    }
}

fn collect_concatenation_operators(expression: &Expression<'_>, operators: &mut Vec<Span>) {
    if let Expression::Binary(Binary { lhs, operator: BinaryOperator::StringConcat(span), rhs }) = expression {
        collect_concatenation_operators(lhs, operators);
        operators.push(*span);
        collect_concatenation_operators(rhs, operators);
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
