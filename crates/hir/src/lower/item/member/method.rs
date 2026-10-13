use mago_allocator::Arena;
use mago_flags::U8Flags;
use mago_names::binding::MethodParts;
use mago_span::HasSpan;
use mago_syntax::cst;

use crate::ir::identifier::Identifier;
use crate::ir::item::annotation::generics::TypeParameterDefiningEntity;
use crate::ir::item::member::method::Method;
use crate::ir::item::member::method::MethodFlag;
use crate::ir::item::modifier::ModifierKind;
use crate::ir::name::Name;
use crate::ir::statement::Block;
use crate::ir::statement::Statement;
use crate::ir::statement::StatementKind;
use crate::lower::Lowering;

impl<'scratch, 'arena, S, A> Lowering<'_, 'scratch, 'arena, S, A>
where
    S: Arena,
    A: Arena,
{
    pub(crate) fn lower_method(
        &mut self,
        method: &'scratch cst::Method<'scratch>,
        owner: Identifier<'arena>,
    ) -> Method<'arena, (), (), ()> {
        let name = self.lower_name(&method.name);

        self.lower_method_parts(MethodParts::of_method(method), name, owner)
    }

    /// A PHP# operator lowers to the static method it runs as, named after .NET's operator method, such as
    /// `op_Addition`, at its symbol. An operator a class cannot declare, which semantics refuses, lowers to none.
    pub(crate) fn lower_operator(
        &mut self,
        operator: &'scratch cst::Operator<'scratch>,
        owner: Identifier<'arena>,
    ) -> Option<Method<'arena, (), (), ()>> {
        let parts = MethodParts::of_operator(operator)?;
        let name = Name { span: parts.name_span, value: self.interner.intern(parts.name) };

        Some(self.lower_method_parts(parts, name, owner))
    }

    fn lower_method_parts(
        &mut self,
        parts: MethodParts<'scratch, 'scratch>,
        name: Name<'arena>,
        owner: Identifier<'arena>,
    ) -> Method<'arena, (), (), ()> {
        let attributes = self.lower_attribute_lists(parts.attribute_lists);
        let version_constraint = self.lower_version_constraint(parts.attribute_lists);
        let modifiers = self.lower_modifiers(parts.modifiers);
        let return_type = parts.return_type_hint.map(|hint| self.lower_type(&hint.hint));

        let document = self.phpdoc_resolution.get(parts.span);
        let is_static = modifiers.iter().any(|modifier| modifier.kind == ModifierKind::Static);
        self.type_resolution.enter_scope_with(TypeParameterDefiningEntity::Method(owner, name), is_static);
        let type_parameters = self.register_item_type_parameters(document.as_ref(), None);

        let parameters = self.lower_parameter_list(parts.parameter_list);
        let outer_effects = self.enter_function_like_body();
        let body = match parts.body {
            cst::MethodBody::Abstract(_) => None,
            cst::MethodBody::Concrete(block) => Some(&*self.arena.alloc(self.lower_block(block))),
            // A PHP# expression body is the block that returns its expression, or runs it as a statement.
            cst::MethodBody::Expression(body) => {
                let expression = &*self.arena.alloc(self.lower_expression(body.expression));
                let kind = if parts.returns_value {
                    StatementKind::Return(Some(expression))
                } else {
                    StatementKind::Expression(expression)
                };
                let statement = Statement { meta: (), span: body.span(), kind, terminator: None };

                Some(
                    &*self
                        .arena
                        .alloc(Block { span: body.span(), statements: self.arena.alloc_slice_copy(&[statement]) }),
                )
            }
        };
        let effects = self.leave_function_like_body(outer_effects);

        let return_expression = match parts.body {
            cst::MethodBody::Concrete(block) => self.single_return_expression(block),
            cst::MethodBody::Expression(body) if parts.returns_value => Some(body.expression),
            cst::MethodBody::Expression(_) | cst::MethodBody::Abstract(_) => None,
        };
        let inferred_assertions = self.infer_function_like_assertions(return_expression, parameters.as_slice());
        let (annotation, assertions_inferred) =
            self.build_item_annotation(document.as_ref(), None, type_parameters, inferred_assertions);

        self.type_resolution.leave_scope();

        let mut flags = U8Flags::new();
        if parts.returns_by_reference {
            flags.set(MethodFlag::ReturnsByReference);
        }
        if assertions_inferred {
            flags.set(MethodFlag::AssertionsInferred);
        }
        if effects.yields {
            flags.set(MethodFlag::Yields);
        }
        if effects.throws {
            flags.set(MethodFlag::Throws);
        }

        Method {
            span: parts.span,
            annotation,
            attributes,
            version_constraint,
            flags,
            modifiers,
            name,
            parameters,
            return_type,
            body,
            direct_accessed_globals: effects.accessed_globals.leak(),
        }
    }
}
