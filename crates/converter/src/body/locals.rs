//! Variables, locals and assignment (spec sections 2 and 3): `$` leaves each name, and a local is declared where it is
//! first assigned.

use mago_allocator::LocalArena;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::reference::SymbolReferences;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::builder::get_union_from_type;
use mago_codex::ttype::combine_union_types;
use mago_codex::ttype::combiner::CombinerOptions;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::union::TUnion;
use mago_codex::ttype::union::populate_union_type;
use mago_phpdoc_syntax::PHPDocParser;
use mago_phpdoc_syntax::cst::Element;
use mago_phpdoc_syntax::cst::TagValue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::cst::TokenSeparatedSequence;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;

use super::Kind;
use crate::Converter;
use crate::declaration::NameCase;

/// The variables PHP fills for every scope, which PHP# removes (spec sections 2 and 29).
const SUPERGLOBALS: [&[u8]; 9] =
    [b"$GLOBALS", b"$_SERVER", b"$_GET", b"$_POST", b"$_FILES", b"$_COOKIE", b"$_SESSION", b"$_REQUEST", b"$_ENV"];

/// The locals of the method being converted. A PHP local starts at its first assignment, and a PHP# local at its
/// declaration (spec section 2), so the first assignment declares it: `const` when nothing writes it again, `let`
/// when something does, and its type when a `@var` tag declares one.
#[derive(Debug, Default)]
pub(crate) struct Locals {
    /// Each local, in the order the body first names it, by its PHP name.
    entries: Vec<(Vec<u8>, Local)>,
    /// The block the conversion is in, innermost last.
    pub(super) blocks: Vec<Span>,
    /// The body of the method or lambda, its first block in braces, where a local read after the block that assigns it
    /// is declared.
    pub(super) body: Option<Span>,
    /// Each variable a lambda captures by value, with where. PHP's lambda keeps the value the variable had, and a
    /// PHP# lambda reads the variable itself, so nothing may write it after its declaration (spec section 14.2).
    copied: Vec<(Vec<u8>, Span)>,
    /// Each parameter the body writes.
    written_parameters: Vec<Vec<u8>>,
}

/// What a lambda captures from the body around it (spec section 14.2).
#[derive(Debug)]
pub(crate) struct Captures {
    /// Whether the lambda captures each name it reads that isn't its parameter, as an arrow function does, or only
    /// the names its `use` lists.
    pub(super) every_name: bool,
    pub(super) names: Vec<Capture>,
}

#[derive(Debug)]
pub(super) struct Capture {
    /// The variable's PHP name.
    pub(super) name: Vec<u8>,
    /// Where the lambda names it first.
    pub(super) at: Span,
    pub(super) by_reference: bool,
    /// Whether the lambda writes it.
    pub(super) written: bool,
    /// Whether a lambda inside this one captures it by value.
    pub(super) copied: bool,
    /// The analysis's type of it where the lambda names it.
    pub(super) union: Option<TUnion>,
}

#[derive(Debug)]
struct Local {
    /// The first place the body names it.
    first: Span,
    /// The last place the body names it.
    last: Span,
    declaration: Option<Declaration>,
    /// Whether something writes it after its declaration.
    rewritten: bool,
    /// A place that names it outside the block that declares it.
    escapes: Option<Span>,
    /// Each value written to it, which the type of a local declared first in the body is made of.
    writes: Vec<Write>,
    /// What its first assignment gives it when that says nothing of what it holds, `null` or `[]`.
    start: Option<Start>,
    /// Each value `$local[] = value` appends to it.
    appends: Vec<Write>,
    /// Each key and value `$local[$key] = value` writes in it.
    keyed: Vec<(Write, Write)>,
    /// Each `[]` written to it, whose PHP# form, `[]` or `[:]`, its declared type decides.
    empties: Vec<Span>,
}

impl Local {
    fn new(first: Span, declaration: Option<Declaration>, rewritten: bool, writes: Vec<Write>) -> Self {
        Self {
            first,
            last: first,
            declaration,
            rewritten,
            escapes: None,
            writes,
            start: None,
            appends: Vec::new(),
            keyed: Vec::new(),
            empties: Vec::new(),
        }
    }
}

/// What a local starts as when its first assignment says nothing of what it holds (spec sections 2 and 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    Null,
    /// `[]`, which becomes a `List` or a `Map` by how the body fills it.
    Empty,
}

/// A value an assignment writes to a local.
#[derive(Debug)]
struct Write {
    /// Where the value is.
    at: Span,
    /// The analysis's type of what the write leaves in the local.
    union: Option<TUnion>,
    source: Source,
}

/// Where the type of a value written to a local comes from (decision 2).
#[derive(Debug, Clone, Copy)]
enum Source {
    /// A `@var` tag on the assignment, which declares the type of the local itself, so every value written to it has it.
    Tagged,
    /// A literal, or a declaration, a PHPDoc tag, an extension host's refinement or a type inherited from the parent.
    Declared,
    /// The variable of the local at this index of [`Locals::entries`], whose writes give its type.
    Local(usize),
    /// Nothing declares it, so only the analysis's inference gives its type.
    Inferred,
}

#[derive(Debug)]
struct Declaration {
    /// Where the keyword or the type goes.
    at: u32,
    /// The block whose statement declares it, which PHP# scopes it to (spec section 3).
    block: Span,
    /// The type a `@var` tag declares for it.
    written: Option<String>,
    /// Whether an assignment statement declares it, which can become a plain write of a local declared earlier. A
    /// loop's variable can't.
    hoistable: bool,
}

impl Converter<'_, '_> {
    pub(super) fn locals(&mut self) -> Option<&mut Locals> {
        self.method.as_mut().map(|method| &mut method.locals)
    }

    /// Declares a local at `statement` when its expression is the local's first assignment, and gives whether it did.
    pub(super) fn declaration(&mut self, statement: &Statement<'_>, expression: &Expression<'_>) -> bool {
        if let Expression::Assignment(assignment) = expression
            && assignment.operator.is_assign()
            && let Some(elements) = destructured(assignment.lhs)
        {
            self.destructure(statement, assignment, elements);
            return true;
        }
        let Expression::Assignment(Assignment { lhs: Expression::Variable(Variable::Direct(variable)), operator, rhs }) =
            expression
        else {
            return false;
        };
        if !operator.is_assign() || !self.is_local(variable.name) {
            return false;
        }
        if self.method.is_none() || self.in_scope(variable.name, variable.span.start.offset) {
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
        self.declare(statement, variable, written.as_ref().map(|(text, _)| text.clone()));
        // `null` or `[]` says nothing of what the local holds, so the values written to it later give its type, and
        // it is declared first in the body (spec sections 2, 3 and 12).
        if written.is_none()
            && let Some((start, literal)) = untyped_start(rhs)
        {
            self.started(variable.name, start);
            if start == Start::Empty {
                self.emptied(variable.name, literal);
                return true;
            }
        }
        let expected = written.map(|(_, union)| union);
        match &expected {
            Some(tagged) => self.wrote(variable.name, rhs.span(), Some(tagged.clone()), Source::Tagged),
            None => self.wrote(variable.name, rhs.span(), self.type_of(rhs).cloned(), self.source(rhs)),
        }
        self.expression(rhs, expected.as_ref());

        true
    }

    /// Records that a write gives the local `name` the value at `at`, of type `union`, whose type comes from `source`.
    fn wrote(&mut self, name: &[u8], at: Span, union: Option<TUnion>, source: Source) {
        let Some(method) = self.method.as_mut() else {
            return;
        };
        if let Some((_, local)) = method.locals.entries.iter_mut().rev().find(|(local, _)| local.as_slice() == name) {
            local.writes.push(Write { at, union, source });
        }
    }

    /// The local `name` names last, the one a write to `name` writes.
    fn current(&mut self, name: &[u8]) -> Option<&mut Local> {
        let method = self.method.as_mut()?;
        method.locals.entries.iter_mut().rev().find(|(local, _)| local.as_slice() == name).map(|(_, local)| local)
    }

    /// Records that the local `name` starts as `start`.
    fn started(&mut self, name: &[u8], start: Start) {
        if let Some(local) = self.current(name) {
            local.start = Some(start);
        }
    }

    /// Records the `[]` at `literal` written to the local `name`, whose form its declared type decides.
    fn emptied(&mut self, name: &[u8], literal: Span) {
        if let Some(local) = self.current(name) {
            local.empties.push(literal);
        }
    }

    /// Whether the local `name` names starts as `[]`, so the body fills it by appending or by key.
    fn starts_empty(&self, name: &[u8]) -> bool {
        self.method.as_ref().is_some_and(|method| {
            method
                .locals
                .entries
                .iter()
                .rev()
                .find(|(local, _)| local.as_slice() == name)
                .is_some_and(|(_, local)| local.start == Some(Start::Empty))
        })
    }

    /// The write of `value`, written to a local at its own place: its type and where that comes from.
    fn write_of(&self, value: &Expression<'_>) -> Write {
        Write { at: value.span(), union: self.type_of(value).cloned(), source: self.source(value) }
    }

    /// Where the type of `value`, written to a local, comes from: a literal, a parameter, a property, the return of a
    /// call or a `new` of a class with no type parameters, whose declarations give it, or another local, whose writes
    /// do. Any other value's type is the analysis's inference, which the converter never writes (decision 2).
    fn source(&self, value: &Expression<'_>) -> Source {
        let declared = |declared: bool| if declared { Source::Declared } else { Source::Inferred };
        // A return that names a type parameter takes the type the analysis infers for this call (decision 2).
        let returns = |function: Option<&FunctionLikeMetadata>| {
            declared(function.is_some_and(|function| {
                function.return_from_body.is_none()
                    && function
                        .return_type_metadata
                        .as_ref()
                        .is_some_and(|returned| !returned.type_union.has_template_types())
            }))
        };
        match value {
            Expression::Parenthesized(parenthesized) => self.source(parenthesized.expression),
            Expression::Literal(_) => Source::Declared,
            Expression::UnaryPrefix(unary)
                if matches!(unary.operator, UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_))
                    && matches!(unary.operand, Expression::Literal(_)) =>
            {
                Source::Declared
            }
            Expression::Variable(Variable::Direct(variable)) => {
                let Some(method) = self.method.as_ref() else {
                    return Source::Inferred;
                };
                if variable.name == b"$this" || method.parameters.iter().any(|parameter| parameter == variable.name) {
                    return Source::Declared;
                }
                // A lambda's capture is the variable of the body around it, whose writes this body doesn't see.
                if !self.is_local(variable.name) {
                    return Source::Inferred;
                }
                method
                    .locals
                    .entries
                    .iter()
                    .rposition(|(local, _)| local.as_slice() == variable.name)
                    .map_or(Source::Inferred, Source::Local)
            }
            Expression::Access(Access::Property(access)) => {
                declared(self.declared_property(access.object, &access.property))
            }
            Expression::Access(Access::NullSafeProperty(access)) => {
                declared(self.declared_property(access.object, &access.property))
            }
            Expression::Call(Call::Method(call)) => returns(self.method_of(self.type_of(call.object), &call.method)),
            Expression::Call(Call::NullSafeMethod(call)) => {
                returns(self.method_of(self.type_of(call.object), &call.method))
            }
            Expression::Call(Call::StaticMethod(call)) => returns(self.static_method_of(call)),
            Expression::Call(Call::Function(FunctionCall { function: Expression::Identifier(identifier), .. })) => {
                returns(self.function_of(identifier))
            }
            Expression::Instantiation(Instantiation { class: Expression::Identifier(identifier), .. }) => declared(
                self.input
                    .codebase
                    .get_class_like(self.input.names.get(identifier))
                    .is_some_and(|class| class.template_types.is_empty()),
            ),
            _ => Source::Inferred,
        }
    }

    /// Whether the class of `object` declares a type for its property `property`: natively, in PHPDoc, by an extension
    /// host's refinement, or by inheriting it.
    fn declared_property(&self, object: &Expression<'_>, property: &ClassLikeMemberSelector<'_>) -> bool {
        let ClassLikeMemberSelector::Identifier(name) = property else {
            return false;
        };
        let Some(union) = self.type_of(object) else {
            return false;
        };
        let classes: Vec<&TAtomic> = union.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let [TAtomic::Object(TObject::Named(class))] = classes.as_slice() else {
            return false;
        };
        let property = [b"$".as_slice(), name.value].concat();

        self.input
            .codebase
            .get_declaring_property(class.name.as_bytes(), &property)
            .is_some_and(|metadata| metadata.type_metadata.is_some())
    }

    /// Declares the local `variable` at its first assignment, in `statement`, with the type `written` gives.
    fn declare(&mut self, statement: &Statement<'_>, variable: &DirectVariable<'_>, written: Option<String>) {
        self.misnamed(&variable.name[1..], variable.span, "local", NameCase::Camel);
        let block = self.method.as_ref().and_then(|method| method.locals.blocks.last().copied());
        let Some(method) = self.method.as_mut() else {
            return;
        };
        let declaration = Declaration {
            at: variable.span.start.offset,
            block: block.unwrap_or(statement.span()),
            written,
            hoistable: matches!(statement, Statement::Expression(_)),
        };
        method
            .locals
            .entries
            .push((variable.name.to_vec(), Local::new(variable.span, Some(declaration), false, Vec::new())));
        self.edits.replace(variable.span, Self::variable_name(variable.name));
    }

    /// `['id' => $id, 'name' => $name] = $row;` of a `Map` as one read per key, `const id = row.get('id');`, each a
    /// `TValue?` as PHP gives `null` for a missing key (spec section 12). The `Map` is read once per key, so it must be
    /// a variable.
    fn destructure(&mut self, statement: &Statement<'_>, assignment: &Assignment<'_>, elements: Elements<'_, '_>) {
        let source = assignment.rhs;
        let mut targets = Vec::new();
        for element in elements.iter() {
            match element {
                ArrayElement::KeyValue(element)
                    if matches!(element.key, Expression::Literal(Literal::String(_) | Literal::Integer(_))) =>
                {
                    let Expression::Variable(Variable::Direct(variable)) = element.value else {
                        self.refuse(
                            element.value.span(),
                            "destructuring into a place that is no variable has no PHP# form: read each key into a local",
                        );
                        return self.unassigned(elements);
                    };
                    targets.push((element.key, variable));
                }
                _ => {
                    self.refuse(
                        assignment.lhs.span(),
                        "destructuring by position, or by a key that is no literal, has no PHP# form: PHP# destructures a tuple, and reads a `Map` key by key (spec sections 12 and 30)",
                    );
                    return self.unassigned(elements);
                }
            }
        }
        if targets.is_empty()
            || !matches!(source, Expression::Variable(Variable::Direct(_)))
            || !self.is(source, &[Kind::Map])
        {
            let shown = self.shown(source);
            self.refuse(
                source.span(),
                format!("destructuring `{shown}` by key converts for a `Map` variable only, which PHP# reads once per key (spec section 12)"),
            );
            return self.unassigned(elements);
        }

        self.expression(source, None);
        let read = self.render(source);
        let indentation = self.indentation(statement.span().start.offset);
        let mut start = assignment.lhs.span().start.offset;
        let last = targets.len() - 1;
        for (index, (key, variable)) in targets.into_iter().enumerate() {
            self.expression(key, None);
            let key = self.render(key);
            self.edits.delete(start, variable.span.start.offset);
            if !self.is_local(variable.name) || self.in_scope(variable.name, variable.span.start.offset) {
                self.variable(variable, true);
            } else {
                self.declare(statement, variable, None);
            }
            let union = self.input.artifacts.get_expression_type(variable).cloned();
            self.wrote(variable.name, variable.span, union, self.source(source));
            let end = variable.span.end.offset;
            if index == last {
                self.edits.insert(end, format!(" = {read}.get({key})"));
                self.edits.delete(end, assignment.rhs.span().end.offset);
            } else {
                self.edits.insert(end, format!(" = {read}.get({key});\n{indentation}"));
            }
            start = end;
        }
    }

    /// Takes the variables a refused destructuring assigns as given, so the refusal covers each later read of them,
    /// which has no declaration to point at.
    fn unassigned(&mut self, elements: Elements<'_, '_>) {
        let Some(method) = self.method.as_mut() else {
            return;
        };
        for element in elements.iter() {
            if let Some(Expression::Variable(Variable::Direct(variable))) = element.get_value() {
                method.parameters.push(variable.name.to_vec());
            }
        }
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

    /// Whether a local named `name` is in scope at `offset`: declared by a block around it, or named with no
    /// declaration. A write to one out of scope declares it again, as a later loop's variable of the same name does.
    fn in_scope(&self, name: &[u8], offset: u32) -> bool {
        self.method.as_ref().is_some_and(|method| {
            method.locals.entries.iter().rev().find(|(local, _)| local.as_slice() == name).is_some_and(|(_, local)| {
                local.declaration.as_ref().is_none_or(|declaration| {
                    declaration.block.start.offset <= offset && offset <= declaration.block.end.offset
                })
            })
        })
    }

    /// Declares `variable`, a loop's own over `iterable`, at `at` and scoped to `scope`, the loop, and gives whether it
    /// could. A PHP# loop declares its variables, so one a block around the loop already names is refused (spec
    /// section 17).
    pub(super) fn loop_local(
        &mut self,
        variable: &DirectVariable<'_>,
        iterable: &Expression<'_>,
        at: u32,
        scope: Span,
    ) -> bool {
        if !self.is_local(variable.name) || self.in_scope(variable.name, variable.span.start.offset) {
            let name = String::from_utf8_lossy(variable.name);
            self.refuse(
                variable.span,
                format!(
                    "`{name}` is named before the loop, and a PHP# loop declares its own variables (spec section 17)"
                ),
            );
            return false;
        }
        self.misnamed(&variable.name[1..], variable.span, "local", NameCase::Camel);
        // Each pass writes the loop's variable an element of what it loops over, whose type that value's source gives.
        let write = Write {
            at: variable.span,
            union: self.input.artifacts.get_expression_type(variable).cloned(),
            source: self.source(iterable),
        };
        let Some(method) = self.method.as_mut() else {
            return false;
        };
        let declaration = Declaration { at, block: scope, written: None, hoistable: false };
        method
            .locals
            .entries
            .push((variable.name.to_vec(), Local::new(variable.span, Some(declaration), false, vec![write])));

        true
    }

    /// Whether `name` is a local of the method being converted: no parameter, no `catch` variable, no variable a
    /// lambda captures and not `$this`.
    fn is_local(&self, name: &[u8]) -> bool {
        name != b"$this"
            && self.method.as_ref().is_some_and(|method| {
                !method.parameters.iter().any(|parameter| parameter == name)
                    && method.captures.as_ref().is_none_or(|captures| {
                        !captures.every_name && !captures.names.iter().any(|capture| capture.name == name)
                    })
            })
    }

    /// Records that the body names the local `variable` here, writing it when `write` says so.
    fn local(&mut self, variable: &DirectVariable<'_>, write: bool) {
        let artifacts = self.input.artifacts;
        self.named(variable.name, variable.span, write, false, || artifacts.get_expression_type(variable).cloned());
    }

    /// Records a variable a lambda captures, `capture`, in the body around the lambda, where the lambda is made.
    pub(super) fn captured(&mut self, capture: Capture) {
        let copied = !capture.by_reference || capture.copied;
        self.named(&capture.name, capture.at, capture.written, copied, || capture.union);
    }

    /// Records that the body names the variable `name` at `span`, writing it when `write` says so, and that a lambda
    /// captures it by value when `copied` says so. `union` gives the analysis's type of it there.
    fn named(&mut self, name: &[u8], span: Span, write: bool, copied: bool, union: impl FnOnce() -> Option<TUnion>) {
        if name == b"$this" {
            return;
        }
        let Some(method) = self.method.as_mut() else {
            return;
        };
        let is_parameter = method.parameters.iter().any(|parameter| parameter == name);
        if let Some(captures) = method.captures.as_mut()
            && !is_parameter
        {
            if let Some(capture) = captures.names.iter_mut().find(|capture| capture.name == name) {
                capture.written |= write;
                capture.copied |= copied;
                return;
            }
            if captures.every_name {
                captures.names.push(Capture {
                    name: name.to_vec(),
                    at: span,
                    by_reference: false,
                    written: write,
                    copied,
                    union: union(),
                });
                return;
            }
        }
        if copied {
            method.locals.copied.push((name.to_vec(), span));
        }
        if is_parameter {
            if write {
                method.locals.written_parameters.push(name.to_vec());
            }
            return;
        }
        match method.locals.entries.iter_mut().rev().find(|(local, _)| local.as_slice() == name) {
            Some((_, local)) => {
                local.rewritten |= write;
                local.last = span;
                if let Some(declaration) = &local.declaration
                    && local.escapes.is_none()
                    && !(declaration.block.start.offset <= span.start.offset
                        && span.end.offset <= declaration.block.end.offset)
                {
                    local.escapes = Some(span);
                }
            }
            None => method.locals.entries.push((name.to_vec(), Local::new(span, None, write, Vec::new()))),
        }
    }

    /// Writes each local's declaration at its first assignment, once the method's body is converted and every write
    /// to it is known. A loop declares its key and value at one place, which takes `let` when either is written.
    ///
    /// A local read after the block that assigns it is declared first in the body of its method or lambda, as `T? name
    /// = null;` (spec section 3). PHP has one variable of a name in a function, which keeps its value from one pass of a
    /// loop to the next, so each assignment of the name then writes it. `T` joins the types of the values assigned to
    /// it, and is written only when a declaration gives each of them (decision 2). So is a local that starts as `null`,
    /// and one that starts as `[]` is a `List<V>` the body only appends to or a `Map<K, V>` it only writes by key. One
    /// first assigned in the body itself keeps its declaration there, with that type.
    pub(crate) fn declare_locals(&mut self) {
        let Some(method) = self.method.as_mut() else {
            return;
        };
        let locals = std::mem::take(&mut method.locals.entries);
        let body = method.locals.body.take();
        let copied = std::mem::take(&mut method.locals.copied);
        let written_parameters = std::mem::take(&mut method.locals.written_parameters);
        // A PHP local lives for the whole function, so one read after the block that assigns it is declared first in
        // the body of the method or lambda it belongs to, where no pass of a loop declares it again.
        // So is one that starts as `null` or `[]`, whose type the values written to it later give.
        let mut hoisted: Vec<Vec<u8>> = Vec::new();
        // The type of each such local first assigned in the body itself, by where its declaration goes.
        let mut typed: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
        for (name, local) in &locals {
            let Some(declaration) = &local.declaration else {
                continue;
            };
            let Some(span) = local.escapes.or(local.start.map(|_| local.first)) else {
                continue;
            };
            let Some(body) = body.filter(|_| declaration.hoistable) else {
                continue;
            };
            if hoisted.contains(name) {
                continue;
            }
            hoisted.push(name.clone());
            let Some((written, initial)) = self.hoisted_type(&locals, name, local, span) else {
                continue;
            };
            // One first assigned in the body itself, and nowhere else, already is declared first in the body.
            let declarations = locals.iter().filter(|(local, _)| local == name).count();
            if local.escapes.is_none() && declaration.block == body && declarations == 1 {
                hoisted.pop();
                typed.insert(declaration.at, written);
                continue;
            }
            let first = self.skip_whitespace(body.start.offset + 1);
            let indentation = self.indentation(first);
            let declared = format!("{written} {} = {initial};\n{indentation}", Self::variable_name(name));
            self.edits.insert(first, declared);
        }
        // Each assignment of a hoisted local writes it after its declaration.
        for (name, at) in copied {
            let rewritten = written_parameters.contains(&name)
                || hoisted.contains(&name)
                || locals.iter().any(|(local, entry)| *local == name && entry.rewritten);
            if rewritten {
                let shown = String::from_utf8_lossy(&name);
                self.refuse(
                    at,
                    format!("`{shown}` is captured by value and written after its declaration: PHP's lambda keeps the value it had, and a PHP# lambda reads the variable itself (spec section 14.2)"),
                );
            }
        }

        let mut keywords: std::collections::BTreeMap<u32, String> = std::collections::BTreeMap::new();
        for (name, local) in locals {
            let shown = String::from_utf8_lossy(&name);
            let Some(declaration) = local.declaration else {
                self.refuse(
                    local.first,
                    format!("`{shown}` is first named here, outside an assignment of its own, which is not converted yet: a PHP# local is declared where it is first assigned"),
                );
                continue;
            };
            if hoisted.contains(&name) {
                if !declaration.hoistable {
                    self.refuse(
                        local.first,
                        format!("`{shown}` is a loop's variable and a local read after a block, and a PHP# loop declares its own variables (spec section 17)"),
                    );
                }
                continue;
            }
            if let Some(span) = local.escapes {
                self.refuse(
                    span,
                    format!("`{shown}` is named here, after the block it is first assigned in, which is not converted yet: PHP# scopes a local to its block (spec section 3)"),
                );
                continue;
            }
            let keyword = match declaration.written.or_else(|| typed.remove(&declaration.at)) {
                Some(written) => written,
                None if local.rewritten => "let".to_owned(),
                None => "const".to_owned(),
            };
            keywords
                .entry(declaration.at)
                .and_modify(|shared| {
                    if keyword == "let" {
                        shared.clone_from(&keyword);
                    }
                })
                .or_insert(keyword);
        }
        for (at, keyword) in keywords {
            self.edits.insert(at, format!("{keyword} "));
        }
    }

    /// The type and the initial value of `name`, the local `local` of `locals` that PHP# declares first in the body:
    /// `List<V>` or `Map<K, V>` and its empty literal for one that starts as `[]`, and `T?` and `null` for any other,
    /// `T` joining the declared types of the values assigned to it (decision 2). Refuses it at `span` otherwise.
    fn hoisted_type(
        &mut self,
        locals: &[(Vec<u8>, Local)],
        name: &[u8],
        local: &Local,
        span: Span,
    ) -> Option<(String, &'static str)> {
        if local.start == Some(Start::Empty) {
            let (written, empty) = self.filled(locals, name, span)?;
            for literal in locals.iter().filter(|(local, _)| local == name).flat_map(|(_, local)| &local.empties) {
                self.edits.replace(*literal, empty);
            }
            return Some((written, empty));
        }
        let shown = String::from_utf8_lossy(name).into_owned();
        let why = match local.escapes {
            Some(_) => "is read after the block that assigns it",
            None => "starts as `null`",
        };
        if let Some(at) = inferred_write(locals, name, &mut Vec::new()) {
            let value = String::from_utf8_lossy(self.text(at));
            self.refuse(
                at,
                format!("`{shown}` {why}, so PHP# declares it first in the body with a type, and no declaration gives the type of `{value}`, which it is assigned: declare its type with a `@var` tag (decision 2)"),
            );
            return None;
        }
        if writes(locals, name).all(|write| write.union.as_ref().is_some_and(TUnion::is_null)) {
            self.refuse(
                span,
                format!("`{shown}` starts as `null` and is assigned nothing else, which says nothing of what it holds: declare its type with a `@var` tag"),
            );
            return None;
        }
        let codebase = self.input.codebase;
        let tagged = writes(locals, name).find(|write| matches!(write.source, Source::Tagged));
        let joined = match tagged {
            Some(tagged) => tagged.union.clone(),
            None => writes(locals, name)
                .try_fold(None, |joined: Option<TUnion>, write| {
                    let union = write.union.as_ref()?;
                    Some(Some(match joined {
                        Some(joined) => combine_union_types(&joined, union, codebase, CombinerOptions::default()),
                        None => union.clone(),
                    }))
                })
                .flatten(),
        };
        let Some(mut union) = joined else {
            self.refuse(
                span,
                format!("`{shown}` {why}, so PHP# declares it first in the body with a type, and the analysis gives no type to a value it is assigned: declare its type with a `@var` tag"),
            );
            return None;
        };
        union.widen_scalars();

        Some((self.type_text(&union.as_nullable(), span)?, "null"))
    }

    /// The declared type of `name`, a local that starts as `[]`, and the empty literal of it: `List<V>` when the body
    /// only appends to it and `Map<K, V>` when it only writes it by key, each of the types a declaration gives
    /// (decision 2). Refuses it at `span` otherwise.
    fn filled(&mut self, locals: &[(Vec<u8>, Local)], name: &[u8], span: Span) -> Option<(String, &'static str)> {
        let entries: Vec<&Local> = locals.iter().filter(|(local, _)| local == name).map(|(_, local)| local).collect();
        let appends: Vec<&Write> = entries.iter().flat_map(|local| &local.appends).collect();
        let keyed: Vec<&(Write, Write)> = entries.iter().flat_map(|local| &local.keyed).collect();
        let shown = String::from_utf8_lossy(name).into_owned();
        let codebase = self.input.codebase;
        let join = |values: &mut dyn Iterator<Item = &Write>| -> Option<TUnion> {
            let mut joined: Option<TUnion> = None;
            for write in values {
                if !is_declared(locals, write) {
                    return None;
                }
                let union = write.union.as_ref()?;
                joined = Some(match joined {
                    Some(joined) => combine_union_types(&joined, union, codebase, CombinerOptions::default()),
                    None => union.clone(),
                });
            }
            joined.map(|mut union| {
                union.widen_scalars();
                union
            })
        };
        let reassigned = entries.iter().any(|local| !local.writes.is_empty());
        let filled = match (appends.is_empty(), keyed.is_empty()) {
            (false, true) if !reassigned => join(&mut appends.iter().copied()).map(|value| (None, value)),
            (true, false) if !reassigned => join(&mut keyed.iter().map(|(key, _)| key))
                .zip(join(&mut keyed.iter().map(|(_, value)| value)))
                .map(|(key, value)| (Some(key), value)),
            _ => {
                self.refuse(
                    span,
                    format!("`{shown}` starts as `[]`, and PHP# gives a `List` type to one the body only appends to and a `Map` type to one it only writes by key, which `{shown}` is not: declare its type with a `@var` tag (spec section 12)"),
                );
                return None;
            }
        };
        let Some((key, value)) = filled else {
            self.refuse(
                span,
                format!("`{shown}` starts as `[]`, so the values written in it give its type, and no declaration gives the type of one of them: declare its type with a `@var` tag (decision 2)"),
            );
            return None;
        };
        let value = self.type_text(&value, span)?;
        match key {
            None => Some((format!("List<{value}>"), "[]")),
            Some(key) => {
                let key = self.type_text(&key, span)?;
                Some((format!("Map<{key}, {value}>"), "[:]"))
            }
        }
    }

    pub(super) fn variable(&mut self, variable: &DirectVariable<'_>, write: bool) {
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
    pub(super) fn written(&mut self, place: &Expression<'_>) {
        match place {
            Expression::Variable(Variable::Direct(variable)) => self.variable(variable, true),
            place => self.expression(place, None),
        }
    }

    pub(super) fn assignment(
        &mut self,
        expression: &Expression<'_>,
        assignment: &Assignment<'_>,
        expected: Option<&TUnion>,
    ) {
        let (lhs, rhs) = (assignment.lhs, assignment.rhs);
        if let Some(elements) = destructured(lhs) {
            self.refuse(
                expression.span(),
                "destructuring inside another expression has no PHP# form: write it as a statement of its own",
            );
            return self.unassigned(elements);
        }
        if let Expression::ArrayAppend(append) = lhs {
            if !assignment.operator.is_assign() {
                return self.refuse(expression.span(), "a compound write to `[]` has no PHP# form");
            }
            // `$list[] = $value` appends, which PHP# writes `list.add(value)` (spec section 12).
            if let Expression::Variable(Variable::Direct(variable)) = append.array {
                let write = self.write_of(rhs);
                if let Some(local) = self.current(variable.name) {
                    local.appends.push(write);
                }
            }
            self.written(append.array);
            self.expression(rhs, None);
            let list = self.render(append.array);
            let value = self.render(rhs);
            return self.edits.replace(expression.span(), format!("{list}.add({value})"));
        }
        if let Expression::ArrayAccess(access) = lhs {
            // A local that starts as `[]` and is written by key is a `Map`, which its declaration says.
            let starts_empty = matches!(access.array, Expression::Variable(Variable::Direct(variable)) if self.starts_empty(variable.name));
            if let (true, Expression::Variable(Variable::Direct(variable))) = (starts_empty, access.array) {
                let written = (self.write_of(access.index), self.write_of(rhs));
                if let Some(local) = self.current(variable.name) {
                    local.keyed.push(written);
                }
            }
            // A `Map` sets a key PHP's way, and a `List`'s index write replaces an element where PHP's adds one.
            if !starts_empty && !self.is(access.array, &[Kind::Map]) {
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

        // A `[]` written again to a local that starts as one takes the form its declaration gives.
        if let Expression::Variable(Variable::Direct(variable)) = lhs
            && assignment.operator.is_assign()
            && self.starts_empty(variable.name)
            && let Some((Start::Empty, literal)) = untyped_start(rhs)
        {
            self.written(lhs);
            return self.emptied(variable.name, literal);
        }
        let property = self.declared_type(lhs);
        let declared = property.clone().or_else(|| expected.cloned());
        self.written(lhs);
        if let Expression::Variable(Variable::Direct(variable)) = lhs {
            // A compound write leaves what its operator gives, as `$sum += 0.5` makes an `int` sum a `float`.
            let left = if assignment.operator.is_assign() { rhs } else { expression };
            self.wrote(variable.name, rhs.span(), self.type_of(left).cloned(), self.source(rhs));
        }
        self.expression(rhs, declared.as_ref());
        // PHP stores any value in a typed property and checks it there, and PHP# takes one of a known type.
        if property.is_some_and(|property| !property.types.iter().any(TAtomic::is_mixed)) {
            self.mixed(rhs, "a property of a known type");
        }
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

/// The elements of a destructuring's place.
type Elements<'ast, 'arena> = &'ast TokenSeparatedSequence<'arena, ArrayElement<'arena>>;

/// The elements `place` destructures into, when the place of a write is a destructuring.
fn destructured<'ast, 'arena>(place: &'ast Expression<'arena>) -> Option<Elements<'ast, 'arena>> {
    match place {
        Expression::Array(array) => Some(&array.elements),
        Expression::LegacyArray(array) => Some(&array.elements),
        Expression::List(list) => Some(&list.elements),
        _ => None,
    }
}

/// The values written to the variable `name`, which PHP has one of in a function, whichever of `locals` declares it.
fn writes<'locals>(locals: &'locals [(Vec<u8>, Local)], name: &'locals [u8]) -> impl Iterator<Item = &'locals Write> {
    locals.iter().filter(move |(local, _)| local.as_slice() == name).flat_map(|(_, local)| &local.writes)
}

/// Where a value is written to the variable `name` whose type no declaration gives, directly or through the writes of
/// the local it is read from. `seen` holds the variables already followed, as one read from itself.
fn inferred_write<'locals>(
    locals: &'locals [(Vec<u8>, Local)],
    name: &'locals [u8],
    seen: &mut Vec<&'locals [u8]>,
) -> Option<Span> {
    if seen.contains(&name) || writes(locals, name).any(|write| matches!(write.source, Source::Tagged)) {
        return None;
    }
    seen.push(name);

    writes(locals, name).find_map(|write| match write.source {
        Source::Tagged | Source::Declared => None,
        Source::Inferred => Some(write.at),
        Source::Local(local) => inferred_write(locals, &locals[local].0, seen).map(|_| write.at),
    })
}

/// What `value` starts a local as when it says nothing of what the local holds, `null` or an empty array, with the
/// literal's place.
fn untyped_start(value: &Expression<'_>) -> Option<(Start, Span)> {
    match value {
        Expression::Literal(Literal::Null(_)) => Some((Start::Null, value.span())),
        Expression::Array(array) if array.elements.is_empty() => Some((Start::Empty, value.span())),
        Expression::LegacyArray(array) if array.elements.is_empty() => Some((Start::Empty, value.span())),
        Expression::Parenthesized(parenthesized) => untyped_start(parenthesized.expression),
        _ => None,
    }
}

/// Whether a declaration gives the type of `write`'s value, as [`inferred_write`] follows a local's writes.
fn is_declared(locals: &[(Vec<u8>, Local)], write: &Write) -> bool {
    match write.source {
        Source::Tagged | Source::Declared => true,
        Source::Inferred => false,
        Source::Local(local) => inferred_write(locals, &locals[local].0, &mut Vec::new()).is_none(),
    }
}
