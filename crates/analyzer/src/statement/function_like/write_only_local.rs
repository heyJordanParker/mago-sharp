use foldhash::HashMap;
use foldhash::HashSet;
use mago_allocator::Arena;
use mago_bytes::BytesDisplay;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Program;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::walker::Walker;

use crate::code::IssueCode;
use crate::context::Context;

/// Spec section 2: a `let` or typed local changed after its declaration, by a reassignment, a `ref` member call or an
/// index write, and read nowhere in its method is refused at its last change, since the change goes nowhere, as Swift
/// warns that a variable "was written to, but never read". Any other use of its name reads it: a compound assignment
/// such as `+=`, `++`, and a lambda that captures it, a change in the lambda included, since the local outlives the
/// lambda. A `catch` variable keeps its own rules.
pub(crate) fn check_write_only_locals<'arena, A>(context: &mut Context<'_, 'arena, A>, program: &Program<'arena>)
where
    A: Arena,
{
    let mut uses = LocalUses {
        names: context.resolved_names,
        collection_changes: &context.local_changes,
        lambdas: Vec::new(),
        reassigned: HashSet::default(),
        compound: HashSet::default(),
        caught: HashSet::default(),
        locals: HashMap::default(),
    };
    LocalUseWalker.walk_program(program, &mut uses);

    let mut lost = uses
        .locals
        .into_values()
        .filter(|local_use| !local_use.read)
        .filter_map(|local_use| Some((local_use.last_change?, local_use.local)))
        .collect::<Vec<_>>();
    lost.sort_by_key(|((change, _), _)| change.start.offset);

    for ((change, name), local) in lost {
        context.collector.report_with_code(
            IssueCode::WriteOnlyLocal,
            Issue::error(format!("`{name}` is changed here and never read, so the change is lost."))
                .with_annotation(Annotation::primary(change).with_message("Changed here."))
                .with_annotation(Annotation::secondary(local.declaration).with_message("Declared here."))
                .with_help(format!("Read `{name}` where its change matters, or remove the change.")),
        );
    }
}

/// How the code of a file uses each of its `let` locals.
struct LocalUses<'names, 'arena> {
    names: &'names ResolvedNames<'arena>,
    /// The names where the analysis saw a `ref` member call or an index write change a `let` local's collection.
    collection_changes: &'names HashSet<Span>,
    /// The start of each lambda the walk is in, innermost last.
    lambdas: Vec<u32>,
    /// The names a plain `=` assigns.
    reassigned: HashSet<Span>,
    /// The names a compound assignment, such as `+=` or `??=`, or `++` or `--` writes, at the root of any index,
    /// which each reads.
    compound: HashSet<Span>,
    /// The declaration of each `catch` variable.
    caught: HashSet<Span>,
    /// Each `let` local the code uses, by the start of its declaration.
    locals: HashMap<u32, LocalUse>,
}

/// Whether the code reads a `let` local, and the name and place of its last change.
struct LocalUse {
    local: Local,
    read: bool,
    last_change: Option<(Span, String)>,
}

struct LocalUseWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, LocalUses<'_, 'arena>> for LocalUseWalker {
    fn walk_in_closure(&self, closure: &'ast Closure<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        uses.lambdas.push(closure.span().start.offset);
    }

    fn walk_out_closure(&self, _closure: &'ast Closure<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        uses.lambdas.pop();
    }

    fn walk_in_arrow_function(&self, arrow_function: &'ast ArrowFunction<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        uses.lambdas.push(arrow_function.span().start.offset);
    }

    fn walk_out_arrow_function(&self, _arrow_function: &'ast ArrowFunction<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        uses.lambdas.pop();
    }

    fn walk_in_try_catch_clause(&self, clause: &'ast TryCatchClause<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        if let Some(variable) = &clause.variable {
            uses.caught.insert(variable.span);
        }
    }

    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        match (&assignment.operator, assignment.lhs.unparenthesized()) {
            (AssignmentOperator::Assign(_), Expression::ConstantAccess(name)) => {
                uses.reassigned.insert(name.span());
            }
            (AssignmentOperator::Assign(_), _) => {}
            (_, target) => {
                if let Some(name) = indexed_name(target) {
                    uses.compound.insert(name.span());
                }
            }
        }
    }

    fn walk_in_unary_prefix(&self, unary_prefix: &'ast UnaryPrefix<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        if unary_prefix.operator.is_increment_or_decrement()
            && let Some(name) = indexed_name(unary_prefix.operand)
        {
            uses.compound.insert(name.span());
        }
    }

    fn walk_in_unary_postfix(&self, unary_postfix: &'ast UnaryPostfix<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        if let Some(name) = indexed_name(unary_postfix.operand) {
            uses.compound.insert(name.span());
        }
    }

    fn walk_in_constant_access(&self, constant_access: &'ast ConstantAccess<'arena>, uses: &mut LocalUses<'_, 'arena>) {
        let Some(Binding::Local(local @ Local { kind: LocalKind::Let, .. })) =
            uses.names.binding(&constant_access.name)
        else {
            return;
        };
        if uses.caught.contains(&local.declaration) {
            return;
        }

        let span = constant_access.span();
        let declared = local.declaration.start.offset;
        let captured = uses.lambdas.iter().any(|&lambda| declared < lambda);
        let changes = (uses.reassigned.contains(&span) || uses.collection_changes.contains(&span))
            && !uses.compound.contains(&span)
            && !captured;

        let local_use = uses.locals.entry(declared).or_insert(LocalUse { local, read: false, last_change: None });
        if !changes {
            local_use.read = true;
        } else if local_use.last_change.as_ref().is_none_or(|(last, _)| last.start.offset < span.start.offset) {
            local_use.last_change = Some((span, BytesDisplay(constant_access.name.value()).to_string()));
        }
    }
}

/// The local name at the root of `target`, through any index, as `counts` in `counts[key]`.
fn indexed_name<'ast, 'arena>(target: &'ast Expression<'arena>) -> Option<&'ast ConstantAccess<'arena>> {
    let mut target = target.unparenthesized();
    while let Expression::ArrayAccess(access) = target {
        target = access.array.unparenthesized();
    }

    match target {
        Expression::ConstantAccess(name) => Some(name),
        _ => None,
    }
}
