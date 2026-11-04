//! Variables, locals and assignment (spec sections 2 and 3): `$` leaves each name, and a local is declared where it is
//! first assigned.

use mago_allocator::LocalArena;
use mago_codex::reference::SymbolReferences;
use mago_codex::ttype::builder::get_union_from_type;
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
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Variable;

use super::Kind;
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
    locals: Vec<(Vec<u8>, Local)>,
    /// The block the conversion is in, innermost last.
    pub(super) blocks: Vec<Span>,
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

impl Converter<'_, '_> {
    pub(super) fn locals(&mut self) -> Option<&mut Locals> {
        self.method.as_mut().map(|method| &mut method.locals)
    }

    /// Declares a local at `statement` when its expression is the local's first assignment, and gives whether it did.
    pub(super) fn declaration(&mut self, statement: &Statement<'_>, expression: &Expression<'_>) -> bool {
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
