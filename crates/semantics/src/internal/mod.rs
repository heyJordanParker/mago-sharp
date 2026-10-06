use mago_syntax::cst::Access;
use mago_syntax::cst::AnonymousClass;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::Closure;
use mago_syntax::cst::Constant;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Declare;
use mago_syntax::cst::Enum;
use mago_syntax::cst::Expression;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::FunctionLikeReturnTypeHint;
use mago_syntax::cst::Global;
use mago_syntax::cst::Goto;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::List;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::Match;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::Node;
use mago_syntax::cst::PartialApplication;
use mago_syntax::cst::Pipe;
use mago_syntax::cst::Program;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Switch;
use mago_syntax::cst::Trait;
use mago_syntax::cst::TraitUseAliasAdaptation;
use mago_syntax::cst::Try;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::walker::Walker;

use crate::internal::context::Context;

pub mod checker;
pub mod consts;
pub mod context;

#[derive(Clone, Debug)]
pub struct CheckingWalker;

impl<'ast, 'arena> Walker<'ast, 'arena, Context<'_, 'ast, 'arena>> for CheckingWalker {
    #[inline]
    fn walk_in_node(&self, node: Node<'ast, 'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        if checks_slice(context) {
            checker::sharp::check_slice(node, context);
        }
    }

    #[inline]
    fn walk_out_node(&self, _node: Node<'ast, 'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        if checks_slice(context) {
            context.slice_places.pop();
        }
    }

    #[inline]
    fn walk_in_statement(&self, statement: &'ast Statement<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        context.ancestors.push(Node::Statement(statement));
    }

    #[inline]
    fn walk_in_expression(&self, expression: &'ast Expression<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        context.ancestors.push(Node::Expression(expression));

        checker::expression::check_for_clone_with(expression, context);
    }

    #[inline]
    fn walk_in_instantiation(
        &self,
        instantiation: &'ast Instantiation<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::expression::check_instantiation_class_reference(instantiation, context);
    }

    #[inline]
    fn walk_out_statement(&self, _statement: &'ast Statement<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        context.ancestors.pop();
    }

    #[inline]
    fn walk_out_expression(&self, _expression: &'ast Expression<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        context.ancestors.pop();
    }

    #[inline]
    fn walk_in_program(&self, program: &'ast Program<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::statement::check_top_level_statements(program, context);

        if program.dialect.is_sharp() {
            checker::sharp::check_declarations(program, context);
            checker::sharp::check_binding_errors(context);
        }
    }

    #[inline]
    fn walk_in_declare(&self, declare: &Declare<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::statement::check_declare(declare, context);
    }

    #[inline]
    fn walk_in_namespace(&self, namespace: &Namespace<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::statement::check_namespace(namespace, context);
    }

    #[inline]
    fn walk_out_namespace(&self, namespace: &Namespace<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::statement::check_namespace_body(namespace, context);
    }

    #[inline]
    fn walk_in_hint(&self, hint: &Hint<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        context.hint_depth += 1;
        checker::hint::check_hint(hint, context);
    }

    #[inline]
    fn walk_out_hint(&self, _hint: &Hint, context: &mut Context<'_, 'ast, 'arena>) {
        context.hint_depth -= 1;
    }

    #[inline]
    fn walk_in_try(&self, r#try: &Try<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::r#try::check_try(r#try, context);
    }

    #[inline]
    fn walk_in_try_catch_clause(
        &self,
        try_catch_clause: &'ast TryCatchClause<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_try_catch_clause(try_catch_clause, context);
        }
    }

    #[inline]
    fn walk_in_class(&self, class: &'ast Class<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::class_like::check_class(class, context);

        if context.program.dialect.is_sharp() {
            checker::sharp::check_class_name(&class.name, context);
        }
    }

    #[inline]
    fn walk_in_interface(&self, interface: &'ast Interface<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::class_like::check_interface(interface, context);
    }

    #[inline]
    fn walk_in_trait(&self, r#trait: &'ast Trait<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::class_like::check_trait(r#trait, context);
    }

    #[inline]
    fn walk_in_trait_use_alias_adaptation(
        &self,
        trait_use_alias_adaptation: &'ast TraitUseAliasAdaptation<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::class_like::check_trait_use_alias_adaptation(trait_use_alias_adaptation, context);
    }

    #[inline]
    fn walk_in_enum(&self, r#enum: &'ast Enum<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::class_like::check_enum(r#enum, context);

        if context.program.dialect.is_sharp() {
            checker::sharp::check_class_name(&r#enum.name, context);
        }
    }

    #[inline]
    fn walk_in_anonymous_class(
        &self,
        anonymous_class: &'ast AnonymousClass<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::class_like::check_anonymous_class(anonymous_class, context);
    }

    #[inline]
    fn walk_in_function(&self, function: &'ast Function<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::function_like::check_function(function, context);

        if context.program.dialect.is_sharp() {
            checker::sharp::check_function(function, context);
        }
    }

    #[inline]
    fn walk_in_local_declaration(
        &self,
        local_declaration: &'ast LocalDeclaration<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::sharp::check_local_declaration(local_declaration, context);
    }

    #[inline]
    fn walk_in_for_of(&self, for_of: &'ast ForOf<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::sharp::check_for_of(for_of, context);
    }

    #[inline]
    fn walk_in_type_pattern(&self, type_pattern: &'ast TypePattern<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::sharp::check_type_pattern(type_pattern, context);
    }

    #[inline]
    fn walk_in_function_like_parameter(
        &self,
        function_like_parameter: &'ast FunctionLikeParameter<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_parameter(function_like_parameter, context);
        }
    }

    #[inline]
    fn walk_in_constant_access(
        &self,
        constant_access: &'ast ConstantAccess<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_constant_access(constant_access, context);
        }
    }

    #[inline]
    fn walk_in_function_call(
        &self,
        function_call: &'ast FunctionCall<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_function_call(function_call, context);
        }
    }

    #[inline]
    fn walk_in_global(&self, global: &'ast Global<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_global(global, context);
        }
    }

    #[inline]
    fn walk_in_attribute_list(
        &self,
        attribute_list: &'ast AttributeList<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::attribute::check_attribute_list(attribute_list, context);
    }

    #[inline]
    fn walk_in_goto(&self, goto: &'ast Goto<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::statement::check_goto(goto, context);
    }

    #[inline]
    fn walk_in_argument_list(
        &self,
        argument_list: &'ast ArgumentList<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::argument::check_argument_list(argument_list, context);
    }

    #[inline]
    fn walk_in_closure(&self, closure: &'ast Closure<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::function_like::check_closure(closure, context);
    }

    #[inline]
    fn walk_in_arrow_function(
        &self,
        arrow_function: &'ast ArrowFunction<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::function_like::check_arrow_function(arrow_function, context);
    }

    #[inline]
    fn walk_in_function_like_parameter_list(
        &self,
        function_like_parameter_list: &'ast FunctionLikeParameterList<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::function_like::check_parameter_list(function_like_parameter_list, context);
    }

    #[inline]
    fn walk_in_match(&self, r#match: &'ast Match<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::control_flow::check_match(r#match, context);
    }

    #[inline]
    fn walk_in_switch(&self, switch: &'ast Switch<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::control_flow::check_switch(switch, context);
    }

    #[inline]
    fn walk_in_assignment(&self, assignment: &'ast Assignment<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::assignment::check_assignment(assignment, context);

        if context.program.dialect.is_sharp() {
            checker::sharp::check_assignment(assignment, context);
        }
    }

    #[inline]
    fn walk_in_function_like_return_type_hint(
        &self,
        function_like_return_type_hint: &'ast FunctionLikeReturnTypeHint<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::function_like::check_return_type_hint(function_like_return_type_hint, context);
    }

    #[inline]
    fn walk_in_partial_application(
        &self,
        partial_application: &'ast PartialApplication<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::partial_application::check_partial_application(partial_application, context);
    }

    #[inline]
    fn walk_in_list(&self, list: &'ast List<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::array::check_list(list, context);
    }

    fn walk_in_call(&self, call: &'ast Call<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::call::check_call(call, context);
    }

    #[inline]
    fn walk_in_access(&self, access: &'ast Access<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::access::check_access(access, context);
    }

    #[inline]
    fn walk_in_unary_prefix_operator(
        &self,
        unary_prefix_operator: &'ast UnaryPrefixOperator<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::expression::check_unary_prefix_operator(unary_prefix_operator, context);
    }

    #[inline]
    fn walk_in_unary_prefix(&self, unary_prefix: &'ast UnaryPrefix<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_unary_prefix(unary_prefix, context);
        }
    }

    #[inline]
    fn walk_in_unary_postfix(
        &self,
        unary_postfix: &'ast UnaryPostfix<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        if context.program.dialect.is_sharp() {
            checker::sharp::check_unary_postfix(unary_postfix, context);
        }
    }

    #[inline]
    fn walk_in_literal_expression(
        &self,
        literal_expression: &'ast Literal<'arena>,
        context: &mut Context<'_, 'ast, 'arena>,
    ) {
        checker::literal::check_literal(literal_expression, context);
    }

    #[inline]
    fn walk_in_constant(&self, constant: &'ast Constant<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::constant::check_constant(constant, context);
    }

    fn walk_in_pipe(&self, pipe: &'ast Pipe<'arena>, context: &mut Context<'_, 'ast, 'arena>) {
        checker::pipe::check_pipe(pipe, context);
    }
}

/// Whether the walk checks the PHP# slice, so the place stack pops only what it pushed. A parse error already stops
/// the file, so it is the one error to fix first.
#[inline]
fn checks_slice(context: &Context<'_, '_, '_>) -> bool {
    context.program.dialect.is_sharp() && context.program.errors.is_empty()
}
