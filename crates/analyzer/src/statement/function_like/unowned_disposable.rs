use foldhash::HashSet;
use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_names::ResolvedNames;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::BindingValue;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Program;
use mago_syntax::cst::PropertyConcreteItem;
use mago_syntax::cst::Return;
use mago_syntax::walker::Walker;
use mago_word::Word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::utils::expression::is_this;
use crate::utils::quoted_source;

/// Spec section 2: a `Disposable` value has one owner, as .NET's CA2000 ("Dispose objects before losing scope")
/// decides. Ownership starts at an owning expression: `new` of a `Disposable` class, or a call whose value is a
/// `Disposable`. It is accepted where one of the four owners takes it: the value of a `using` local, the value a
/// `return` hands its caller, a lambda's or a value-returning method's body included, a write to a field of `this` in
/// a `Disposable` class, and a module binding's value, which the container owns. A parenthesis, either branch of a
/// `?:`, either side of `??` and a `match` arm pass the owner on. Anywhere else the value has no owner and is
/// refused. A read of a local, field or parameter borrows, and is never refused.
pub(crate) fn check_unowned_disposables<'arena, A>(
    context: &mut Context<'_, 'arena, A>,
    artifacts: &AnalysisArtifacts,
    program: &Program<'arena>,
) where
    A: Arena,
{
    let mut walk = Ownership {
        codebase: context.codebase,
        names: context.resolved_names,
        artifacts,
        disposable: word(b"Sharp\\Disposable"),
        classes: Vec::new(),
        owned: HashSet::default(),
        unowned: Vec::new(),
    };
    OwnershipWalker.walk_program(program, &mut walk);

    for span in walk.unowned {
        let quoted = quoted_source(context, span);
        context.collector.report_with_code(
            IssueCode::UnownedDisposable,
            Issue::error(format!(
                "`{quoted}` makes a `Disposable`, and only its owner may hold it: a `using` local, a `return`, a field of a `Disposable` class, or a module binding."
            ))
            .with_annotation(Annotation::primary(span).with_message("Made here, with no owner."))
            .with_help("Hold it in a `using` local, which disposes it when its block ends."),
        );
    }
}

struct Ownership<'walk, 'arena> {
    codebase: &'walk CodebaseMetadata,
    names: &'walk ResolvedNames<'arena>,
    artifacts: &'walk AnalysisArtifacts,
    disposable: Word,
    /// Whether each class the walk is in is `Disposable`, innermost last.
    classes: Vec<bool>,
    /// The span of each expression an owner takes.
    owned: HashSet<(u32, u32)>,
    /// The span of each owning expression no owner takes, in the order of the source.
    unowned: Vec<mago_span::Span>,
}

impl Ownership<'_, '_> {
    /// `expression` goes to an owner, and so does each expression its value may be.
    fn own(&mut self, expression: &Expression<'_>) {
        let span = expression.span();
        self.owned.insert((span.start.offset, span.end.offset));
        match expression {
            Expression::Parenthesized(parenthesized) => self.own(parenthesized.expression),
            Expression::Conditional(conditional) => {
                if let Some(then) = conditional.then {
                    self.own(then);
                }
                self.own(conditional.r#else);
            }
            Expression::Binary(binary) if matches!(binary.operator, BinaryOperator::NullCoalesce(_)) => {
                self.own(binary.lhs);
                self.own(binary.rhs);
            }
            Expression::PatternMatch(pattern_match) => {
                for arm in pattern_match.arms.iter() {
                    if let PatternMatchArmBody::Expression(value) = arm.body() {
                        self.own(value);
                    }
                }
            }
            _ => {}
        }
    }

    /// Records the owning expression at `node` when no owner takes it: its value may be a `Disposable`.
    fn check(&mut self, node: &impl HasSpan) {
        let span = node.span();
        if self.owned.contains(&(span.start.offset, span.end.offset)) {
            return;
        }
        let Some(value_type) = self.artifacts.get_expression_type(node) else {
            return;
        };
        if value_type.types.iter().any(|atomic| match atomic {
            TAtomic::Object(TObject::Named(named)) => self.codebase.is_instance_of(named.name, self.disposable),
            _ => false,
        }) {
            self.unowned.push(span);
        }
    }
}

struct OwnershipWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, Ownership<'_, 'arena>> for OwnershipWalker {
    fn walk_in_class(&self, class: &'ast Class<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        let name = word(ownership.names.get(&class.name));
        ownership.classes.push(ownership.codebase.is_instance_of(name, ownership.disposable));
    }

    fn walk_out_class(&self, _class: &'ast Class<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        ownership.classes.pop();
    }

    fn walk_in_method(&self, method: &'ast Method<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        let is_void = method.return_type_hint.as_ref().is_some_and(|hint| matches!(hint.hint, Hint::Void(_)));
        if let MethodBody::Expression(body) = &method.body
            && !is_void
        {
            ownership.own(body.expression);
        }
    }

    fn walk_in_arrow_function(&self, arrow: &'ast ArrowFunction<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        ownership.own(arrow.expression);
    }

    fn walk_in_return(&self, r#return: &'ast Return<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        if let Some(value) = r#return.value {
            ownership.own(value);
        }
    }

    fn walk_in_local_declaration(
        &self,
        declaration: &'ast LocalDeclaration<'arena>,
        ownership: &mut Ownership<'_, 'arena>,
    ) {
        if declaration.is_using() {
            ownership.own(declaration.value);
        }
    }

    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        if matches!(assignment.operator, AssignmentOperator::Assign(_))
            && ownership.classes.last() == Some(&true)
            && let Expression::Access(Access::Property(access)) = assignment.lhs
            && is_this(access.object, ownership.names)
        {
            ownership.own(assignment.rhs);
        }
    }

    fn walk_in_property_concrete_item(
        &self,
        item: &'ast PropertyConcreteItem<'arena>,
        ownership: &mut Ownership<'_, 'arena>,
    ) {
        if ownership.classes.last() == Some(&true) {
            ownership.own(item.value);
        }
    }

    fn walk_in_binding_value(&self, value: &'ast BindingValue<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        if let BindingValue::Expression(expression) = value {
            ownership.own(expression);
        }
    }

    fn walk_in_instantiation(&self, instantiation: &'ast Instantiation<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        ownership.check(instantiation);
    }

    fn walk_in_call(&self, call: &'ast Call<'arena>, ownership: &mut Ownership<'_, 'arena>) {
        ownership.check(call);
    }
}
