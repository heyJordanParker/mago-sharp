//! Parameters, promoted ones as fields or properties (spec section 9), and the properties a constructor sets.

use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Method as MethodDeclaration;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Variable;
use mago_word::word;

use crate::Converter;

use super::accessors;
use super::has;

impl Converter<'_, '_> {
    pub(super) fn parameter(&mut self, parameter: &FunctionLikeParameter<'_>, declared: Option<TUnion>) {
        let name = String::from_utf8_lossy(parameter.variable.name).into_owned();
        if parameter.ampersand.is_some() {
            self.refuse(
                parameter.span(),
                format!(
                    "a PHP# parameter is never taken by reference, so `{name}` can't be: return the new value instead"
                ),
            );
            return;
        }
        if parameter.hooks.is_some() {
            self.refuse(parameter.span(), "a parameter with property hooks is not converted yet");
            return;
        }

        self.attributes(&parameter.attribute_lists);
        let promoted = parameter.is_promoted_property();
        // A promoted parameter declares one type for the parameter and its property, and the analysis keeps the
        // property's, which a refinement may have given.
        let declared = if promoted {
            self.class
                .as_ref()
                .and_then(|class| class.metadata.properties.get(&word(parameter.variable.name)))
                .and_then(|property| property.type_metadata.as_ref())
                .map(|metadata| metadata.type_union.clone())
                .or(declared)
        } else {
            declared
        };
        let union = declared.unwrap_or_else(get_mixed);
        let Some(written) = self.type_text(&union, parameter.variable.span) else {
            return;
        };

        match &parameter.hint {
            Some(hint) => self.edits.replace(hint.span(), written),
            None => {
                let at = parameter.ellipsis.unwrap_or(parameter.variable.span).start.offset;
                self.edits.insert(at, format!("{written} "));
            }
        }
        self.edits.replace(parameter.variable.span, Self::variable_name(parameter.variable.name));

        if promoted {
            // PHP's `private(set)` is the `set` accessor's own visibility in PHP# (spec section 6).
            if let Some(span) = has(&parameter.modifiers, |modifier| modifier.is_write_visibility()) {
                self.edits.delete(span.start.offset, self.skip_whitespace(span.end.offset));
            }
            let readonly = has(&parameter.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_)));
            if let Some(readonly) = readonly {
                self.edits.delete(readonly.start.offset, self.skip_whitespace(readonly.end.offset));
            }
            // PHP makes a promoted parameter with no visibility public, and PHP# reads one as an ordinary parameter.
            let visibility = parameter.modifiers.iter().find(|modifier| modifier.is_read_visibility());
            if visibility.is_none()
                && let Some(first) = parameter.modifiers.first()
            {
                self.edits.insert(first.span().start.offset, "public ");
            }
            let is_public = visibility.is_none_or(|modifier| matches!(modifier, Modifier::Public(_)));
            let accessors = accessors(is_public, readonly.is_some(), &parameter.modifiers);
            if !accessors.is_empty() {
                self.edits.insert(parameter.variable.span.end.offset, accessors);
            }
        }

        if let Some(default) = &parameter.default_value {
            self.expression(default.value, Some(&union));
        }
    }
}

impl Converter<'_, '_> {
    /// The `: super(…)` of a subclass's constructor, which runs the parent's first (spec section 9): the arguments of
    /// the `parent::__construct(…)` its body starts with, which leaves the body, or none when no ancestor of `parent`
    /// declares a constructor. A parent's constructor that runs after other statements, or never, has no PHP# form.
    pub(super) fn super_call(&mut self, method: &MethodDeclaration<'_>, block: &Block<'_>, parent: &[u8]) {
        let first = block.statements.first().and_then(|statement| match statement {
            Statement::Expression(expression) => match expression.expression {
                Expression::Call(Call::StaticMethod(call))
                    if matches!(call.class, Expression::Parent(_))
                        && matches!(&call.method, ClassLikeMemberSelector::Identifier(name)
                            if name.value.eq_ignore_ascii_case(b"__construct")) =>
                {
                    Some((statement, call))
                }
                _ => None,
            },
            _ => None,
        });
        let constructor = self.input.codebase.get_declaring_method(parent, b"__construct");
        let arguments = match (first, constructor) {
            (Some((statement, call)), Some(constructor)) => {
                self.arguments(&call.argument_list, Some(constructor));
                let list = call.argument_list.span();
                let arguments = self.edits.render(self.source, list.start.offset, list.end.offset);
                self.delete_lines(statement.span());
                if let Some(method) = self.method.as_mut() {
                    method.super_call = Some(statement.span());
                }
                arguments
            }
            (None, None) => "()".to_owned(),
            _ => {
                return self.refuse(
                    method.name.span,
                    "a subclass's constructor that runs its parent's after other statements, or never where an ancestor declares one, has no PHP# form: `: super(…)` runs it first (spec section 9)",
                );
            }
        };
        self.edits.insert(method.parameter_list.right_parenthesis.end.offset, format!(" : super{arguments}"));
    }
}

/// Each property, as `$name`, that `class`'s constructor sets with `$this->name = …` in a statement of its own body,
/// which runs on every path through it.
pub(super) fn set_by_constructor(class: &mago_syntax::cst::Class<'_>) -> Vec<Vec<u8>> {
    let constructor = class.members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if method.name.value.eq_ignore_ascii_case(b"__construct") => Some(method),
        _ => None,
    });
    let Some(MethodBody::Concrete(block)) = constructor.map(|method| &method.body) else {
        return Vec::new();
    };

    block
        .statements
        .iter()
        .filter_map(|statement| {
            let Statement::Expression(statement) = statement else {
                return None;
            };
            let Expression::Assignment(assignment) = statement.expression else {
                return None;
            };
            let Expression::Access(Access::Property(access)) = assignment.lhs else {
                return None;
            };
            match (access.object, &access.property) {
                (Expression::Variable(Variable::Direct(object)), ClassLikeMemberSelector::Identifier(name))
                    if object.name == b"$this" && assignment.operator.is_assign() =>
                {
                    Some([b"$".as_slice(), name.value].concat())
                }
                _ => None,
            }
        })
        .collect()
}
