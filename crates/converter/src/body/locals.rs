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
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StaticPropertyAccess;
use mago_syntax::cst::TokenSeparatedSequence;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
use mago_syntax::walker::MutWalker;

use super::Kind;
use super::conditions::unproven_scalar;
use crate::Converter;

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

/// Each variable a method's body names and each parameter of a lambda in it, by their PHP names, which a local's new
/// name must be none of.
#[derive(Debug, Default)]
pub(crate) struct Names {
    variables: Vec<Vec<u8>>,
    parameters: Vec<Vec<u8>>,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for Names {
    fn walk_in_direct_variable(&mut self, variable: &'ast DirectVariable<'arena>, _: &mut ()) {
        if !self.variables.iter().any(|name| name == variable.name) {
            self.variables.push(variable.name.to_vec());
        }
    }

    fn walk_in_function_like_parameter(&mut self, parameter: &'ast FunctionLikeParameter<'arena>, _: &mut ()) {
        self.parameters.push(parameter.variable.name.to_vec());
    }

    /// `Settings::$page_size` names a member of its class, which keeps its name, and no variable of the method.
    fn walk_static_property_access(&mut self, access: &'ast StaticPropertyAccess<'arena>, context: &mut ()) {
        self.walk_expression(access.class, context);
        if !matches!(access.property, Variable::Direct(_)) {
            self.walk_variable(&access.property, context);
        }
    }
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
    /// The first condition that reads it as the scalar it holds, which every later write keeps true only when it holds
    /// its type at runtime too.
    tested: Option<Span>,
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
            tested: None,
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
    /// Whether the value holds that type at runtime, which [`Converter::holds_type_at_runtime`] gives.
    runtime: bool,
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
            self.destructure(assignment, elements);
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
            Some(tagged) => self.wrote(variable.name, rhs, rhs, Some(tagged.clone()), Source::Tagged),
            None => self.wrote(variable.name, rhs, rhs, self.type_of(rhs).cloned(), self.source(rhs)),
        }
        self.value(rhs, expected.as_ref());

        true
    }

    /// Records that a write gives the local `name` the value `value`, of type `union`, whose type comes from `source`,
    /// and which `left`, the value or the compound assignment that makes it, leaves. A condition that read the local as
    /// the scalar it holds refuses when this value may not hold that type at runtime.
    fn wrote(
        &mut self,
        name: &[u8],
        value: &Expression<'_>,
        left: &Expression<'_>,
        union: Option<TUnion>,
        source: Source,
    ) {
        let runtime = self.holds_type_at_runtime(left);
        let Some(local) = self.current(name) else {
            return;
        };
        local.writes.push(Write { at: value.span(), union, source, runtime });
        if let (false, Some(tested)) = (runtime, local.tested) {
            self.refuse(tested, unproven_scalar(&String::from_utf8_lossy(name)));
        }
    }

    /// Whether the local `name` holds the type the analysis gives it at runtime: each value written to it so far
    /// does, and it has one.
    pub(super) fn local_holds_type_at_runtime(&self, name: &[u8]) -> bool {
        let local = self
            .method
            .as_ref()
            .and_then(|method| method.locals.entries.iter().rev().find(|(local, _)| local.as_slice() == name));

        local.is_some_and(|(_, local)| !local.writes.is_empty() && local.writes.iter().all(|write| write.runtime))
    }

    /// Records that a condition at `at` reads the local `name` as the scalar it holds, which a later write that may not
    /// hold that type at runtime refuses.
    pub(super) fn tested_local(&mut self, name: &[u8], at: Span) {
        if let Some(local) = self.current(name)
            && local.tested.is_none()
        {
            local.tested = Some(at);
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
        Write {
            at: value.span(),
            union: self.type_of(value).cloned(),
            source: self.source(value),
            runtime: self.holds_type_at_runtime(value),
        }
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

    /// Names each local and `catch` variable of the method `walk` visits, whose own parameters are `parameters`, in the
    /// camel case spec section 24 gives a local, as `$line_total` is `lineTotal`. Code outside the method never reaches
    /// one by its name, since the converter refuses `compact`, `extract` and a variable named by a variable, so the
    /// new name keeps PHP's meaning where no other variable or parameter of the method, its lambdas' included, has it.
    pub(crate) fn rename_locals(&mut self, parameters: &[Vec<u8>], walk: impl FnOnce(&mut Names)) {
        let mut names = Names::default();
        walk(&mut names);
        let parameters: Vec<&[u8]> = parameters.iter().chain(&names.parameters).map(Vec::as_slice).collect();
        let taken = |name: &[u8]| parameters.contains(&name) || names.variables.iter().any(|variable| variable == name);
        let candidates: Vec<(Vec<u8>, String)> = names
            .variables
            .iter()
            .filter(|name| name.as_slice() != b"$this" && !parameters.contains(&name.as_slice()))
            .filter(|name| !mago_casing::is_camel_case(&name[1..]))
            .filter_map(|name| {
                let camel = String::from_utf8_lossy(&mago_casing::to_camel_case(&name[1..])).into_owned();
                let fits = mago_casing::is_camel_case(camel.as_bytes()) && !taken(format!("${camel}").as_bytes());
                fits.then(|| (name.clone(), camel))
            })
            .collect();
        // Two names of one camel case name, as `$line_total` and `$line__total`, would be one variable.
        self.renamed = candidates
            .iter()
            .filter(|(_, camel)| candidates.iter().filter(|(_, other)| other == camel).count() == 1)
            .cloned()
            .collect();
    }

    /// Refuses the local or `catch` variable `variable` when its name breaks the camel case spec section 24 gives a
    /// local, and the method couldn't rename it, since its camel case name is another variable's or parameter's.
    pub(super) fn misnamed_local(&mut self, variable: &DirectVariable<'_>) {
        let name = &variable.name[1..];
        if mago_casing::is_camel_case(name) || self.renamed.iter().any(|(local, _)| local == variable.name) {
            return;
        }
        let (shown, camel) =
            (String::from_utf8_lossy(name), String::from_utf8_lossy(&mago_casing::to_camel_case(name)).into_owned());
        self.refuse(
            variable.span,
            format!("the local `{shown}` has no PHP# name: PHP# names a local in camel case, and `{camel}` is the name of another variable or parameter of the method (spec section 24)"),
        );
    }

    /// Declares the local `variable` at its first assignment, in `statement`, with the type `written` gives.
    fn declare(&mut self, statement: &Statement<'_>, variable: &DirectVariable<'_>, written: Option<String>) {
        self.misnamed_local(variable);
        let block = self.method.as_ref().and_then(|method| method.locals.blocks.last().copied());
        let Some(method) = self.method.as_mut() else {
            return;
        };
        let declaration = Declaration {
            at: variable.span.start.offset,
            block: block.unwrap_or(statement.span()),
            written,
            // An `if` whose condition assigns it declares it before the `if`, as its own statement.
            hoistable: matches!(statement, Statement::Expression(_) | Statement::If(_)),
        };
        method
            .locals
            .entries
            .push((variable.name.to_vec(), Local::new(variable.span, Some(declaration), false, Vec::new())));
        let name = self.variable_name(variable.name);
        self.edits.replace(variable.span, name);
    }

    /// Declares the local `assignment` writes in the condition of `statement`, an `if`, where the condition named it
    /// first, since the converter writes the assignment on its own line before the `if`. A local assigned before keeps
    /// its declaration, and this is a write of it.
    pub(super) fn declared_by(&mut self, statement: &Statement<'_>, assignment: &Assignment<'_>) {
        let Expression::Variable(Variable::Direct(variable)) = assignment.lhs else {
            return;
        };
        let block = self.method.as_ref().and_then(|method| method.locals.blocks.last().copied());
        let write = self.write_of(assignment.rhs);
        let Some(local) = self.current(variable.name).filter(|local| local.first == variable.span) else {
            return;
        };
        local.declaration = Some(Declaration {
            at: variable.span.start.offset,
            block: block.unwrap_or(statement.span()),
            written: None,
            hoistable: true,
        });
        local.rewritten = false;
        local.writes.push(write);
        self.misnamed_local(variable);
    }

    /// Refuses `['id' => $id] = $row;` and `[$first, $second] = $pair;`. A destructuring by key reads keys the array may
    /// lack, as a `Map` read does, and a plain PHP array is no tuple, so a destructuring by position has no PHP# form
    /// (spec sections 12 and 30).
    fn destructure(&mut self, assignment: &Assignment<'_>, elements: Elements<'_, '_>) {
        self.refuse(assignment.lhs.span(), unpacked(elements));
        self.unassigned(elements);
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
        self.misnamed_local(variable);
        // Each pass writes the loop's variable an element of what it loops over, whose type that value's source gives,
        // and which nothing proves holds that type at runtime.
        let write = Write {
            at: variable.span,
            union: self.input.artifacts.get_expression_type(variable).cloned(),
            source: self.source(iterable),
            runtime: false,
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
    pub(super) fn is_local(&self, name: &[u8]) -> bool {
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
            let declared = format!("{written} {} = {initial};\n{indentation}", self.variable_name(name));
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
                    format!("`{shown}` is first named here, outside an assignment of its own, which has no PHP# form: a PHP# local is declared by a statement that assigns it (spec section 2), and PHP assigns this one inside an expression, through a reference, or not at all"),
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
            // A local an assignment declares is declared first in the body when it escapes its block, so one that
            // escapes here is a loop's variable.
            if let Some(span) = local.escapes {
                self.refuse(
                    span,
                    format!("`{shown}` is named here, after the loop that declares it, which has no PHP# form: a PHP# loop declares its own variables, which live in its block (spec sections 3 and 17)"),
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
        let name = self.variable_name(variable.name);
        self.edits.replace(variable.span, name);
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
            // A `Map` sets a key PHP's way, and `=` at an index of an `ArrayAccess` object calls its `offsetSet`, as the
            // bridge lowers it. A `List`'s index write replaces an element and throws past its end, where PHP's adds the
            // key.
            let array_access = assignment.operator.is_assign() && self.is_array_access(access.array);
            if !starts_empty && !self.is(access.array, &[Kind::Map]) && !array_access {
                let shown = self.shown(access.array);
                let reason = if self.is(access.array, &[Kind::List]) {
                    "PHP adds a key past a list's end, and a PHP# `List` replaces an element and throws past its end (spec section 12)"
                } else {
                    "PHP# writes by index into a `Map`, a `List` or an `ArrayAccess` object only (spec section 12)"
                };
                return self
                    .refuse(expression.span(), format!("a write at an index of `{shown}` has no PHP# form: {reason}"));
            }
            self.written(access.array);
            self.numeric_key(access.array, access.index);
            self.expression(access.index, None);
            return self.expression(rhs, None);
        }

        let numbers = [Kind::Int, Kind::Float];
        let refusal = match &assignment.operator {
            AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => None,
            AssignmentOperator::Concat(span) => {
                // PHP's `.` and an interpolated string give a string, which their own conversion checks part by part,
                // and the analysis keeps no type of such a value `.=` appends.
                let text = matches!(
                    rhs,
                    Expression::Binary(Binary { operator: BinaryOperator::StringConcat(_), .. })
                        | Expression::CompositeString(_)
                );
                let appended = self.is(lhs, &[Kind::String]);
                if appended && (text || self.is(rhs, &[Kind::String])) {
                    self.edits.replace(*span, "+=");
                    None
                } else if appended
                    && (self.is(rhs, &[Kind::Int, Kind::Float]) || self.is(rhs, &[Kind::String, Kind::Null]))
                {
                    // A number shows in a template as PHP's `.` shows it, and `null` as nothing, which `?? ""` gives
                    // a `string` that may be null (spec section 18).
                    self.edits.replace(*span, "+=");
                    let (start, end) = (rhs.span().start.offset, rhs.span().end.offset);
                    match (self.is(rhs, &[Kind::Int, Kind::Float]), super::is_primary(rhs)) {
                        (true, _) => {
                            self.edits.insert(start, "`${");
                            self.edits.insert(end, "}`");
                        }
                        (false, true) => {
                            self.edits.insert(start, "`${");
                            self.edits.insert(end, " ?? \"\"}`");
                        }
                        (false, false) => {
                            self.edits.insert(start, "`${(");
                            self.edits.insert(end, ") ?? \"\"}`");
                        }
                    }
                    None
                } else {
                    Some(self.operands(
                        ".=",
                        lhs,
                        rhs,
                        "shows a value PHP's way, and a template shows only `int`, `float` and `string` that way (spec section 18)",
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
            self.wrote(variable.name, rhs, left, self.type_of(left).cloned(), self.source(rhs));
        }
        self.value(rhs, declared.as_ref());
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
pub(super) type Elements<'ast, 'arena> = &'ast TokenSeparatedSequence<'arena, ArrayElement<'arena>>;

/// Why a destructuring into `elements`, an assignment's or a loop's, has no PHP# form.
pub(super) fn unpacked(elements: Elements<'_, '_>) -> &'static str {
    if elements.iter().all(|element| matches!(element, ArrayElement::KeyValue(_))) {
        super::MISSING_KEY
    } else {
        "destructuring by position has no PHP# form: a plain PHP array is no tuple, which PHP# destructures (spec sections 12 and 30)"
    }
}

/// The elements `place` destructures into, when the place of a write, or of a loop's value, is a destructuring.
pub(super) fn destructured<'ast, 'arena>(place: &'ast Expression<'arena>) -> Option<Elements<'ast, 'arena>> {
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
