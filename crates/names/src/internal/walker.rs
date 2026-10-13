use std::borrow::Cow;
use std::collections::HashSet;
use std::hash::Hash;
use std::hash::Hasher;

use mago_allocator::prelude::*;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Attribute;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassConstantAccess;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::Constant;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Enum;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extends;
use mago_syntax::cst::For;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionPartialApplication;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Implements;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::MethodPartialApplication;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::StaticMethodCall;
use mago_syntax::cst::StaticMethodPartialApplication;
use mago_syntax::cst::StaticPropertyAccess;
use mago_syntax::cst::Trait;
use mago_syntax::cst::TraitUse;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TypeOf;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItems;
use mago_syntax::walker::MutWalker;

use crate::CHANGING_COLLECTION_METHODS;
use crate::ResolvedNames;
use crate::binding::Binding;
use crate::binding::BindingError;
use crate::binding::Local;
use crate::binding::LocalKind;
use crate::internal::context::NameResolutionContext;
use crate::internal::locals::LocalScopes;
use crate::kind::NameKind;
use crate::scope::concat_with_sep;
use crate::scope::php_name;
use crate::scope::trim_start_byte;

/// A CST visitor (`MutWalker`) that traverses a PHP Concrete Syntax Tree
/// to resolve names (classes, functions, constants, etc.) according to
/// PHP's scoping and aliasing rules.
///
/// In a PHP# file it is also the binder: it classifies every bare name as a local, `this`,
/// a class or a constant, from the scopes of the file alone.
#[derive(Debug, Default)]
pub struct NameWalker<'arena> {
    /// Accumulates the resolved names found during the CST walk.
    pub resolved_names: ResolvedNames<'arena>,
    /// Whether the walked program is PHP#.
    sharp: bool,
    locals: LocalScopes<'arena>,
    /// The start offsets of bare names written before `.`.
    member_objects: HashSet<u32>,
    /// The member names of each class being walked, innermost last.
    class_members: std::vec::Vec<ClassMembers<'arena>>,
    /// The start offsets of the lambdas being walked, innermost last.
    lambdas: std::vec::Vec<u32>,
}

/// The member names of one class, compared as PHP compares them: method names ignoring case, and property and
/// constant names exactly.
#[derive(Debug, Default)]
struct ClassMembers<'arena> {
    methods: foldhash::HashSet<IgnoringCase<'arena>>,
    others: foldhash::HashSet<&'arena [u8]>,
}

/// A name that hashes and compares ignoring ASCII case, as PHP compares method names.
#[derive(Debug, Clone, Copy)]
struct IgnoringCase<'name>(&'name [u8]);

impl PartialEq for IgnoringCase<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(other.0)
    }
}

impl Eq for IgnoringCase<'_> {}

impl Hash for IgnoringCase<'_> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        for byte in self.0 {
            state.write_u8(byte.to_ascii_lowercase());
        }
    }
}

impl<'arena> NameWalker<'arena> {
    pub fn new(sharp: bool) -> Self {
        Self { sharp, ..Self::default() }
    }

    fn declare(&mut self, name: &'arena [u8], declaration: Span, kind: LocalKind) {
        let local = Local { declaration, kind };
        if let Some(earlier) = self.locals.declare(name, local) {
            self.resolved_names.report_binding_error(BindingError::Redeclared { name: declaration, earlier });
        }

        self.resolved_names.bind(declaration, Binding::Local(local));
    }

    /// Binds a bare name to a local, which each lambda declared after the local captures.
    fn bind_local(&mut self, name: &'arena [u8], span: Span, local: Local) {
        self.resolved_names.bind(span, Binding::Local(local));
        for &lambda in &self.lambdas {
            if local.declaration.start.offset < lambda {
                self.resolved_names.capture(lambda, name, local);
            }
        }
    }

    /// Records a write to `target` when it is a bare name of a local whose block is open, or an index of one, as in
    /// `counts[key] = 1`, which changes the collection the local holds.
    fn record_write(&mut self, target: &Expression<'arena>) {
        let mut target = target;
        while let Expression::ArrayAccess(access) = target {
            target = access.array;
        }

        if self.sharp
            && let Expression::ConstantAccess(target) = target
            && let Some(local) = self.locals.lookup(target.name.value())
        {
            self.resolved_names.write(local);
        }
    }

    /// Records a call of one of the [`CHANGING_COLLECTION_METHODS`] on `object` as a write to it. PHP finds a method
    /// ignoring case, so the name is compared ignoring case.
    fn record_changing_call(&mut self, object: &Expression<'arena>, method: &ClassLikeMemberSelector<'arena>) {
        if let ClassLikeMemberSelector::Identifier(method) = method
            && CHANGING_COLLECTION_METHODS.iter().any(|name| method.value.eq_ignore_ascii_case(name.as_bytes()))
        {
            self.record_write(object);
        }
    }

    fn enter_lambda(&mut self, lambda: Span) {
        if self.sharp {
            self.lambdas.push(lambda.start.offset);
            self.locals.enter_block();
        }
    }

    fn exit_lambda(&mut self) {
        if self.sharp {
            self.lambdas.pop();
            self.locals.exit_block();
        }
    }

    /// Marks a bare name written before `.` or `?.`, which binds as a class unless it names a local or `this`.
    fn mark_member_object(&mut self, object: &Expression<'arena>) {
        if self.sharp
            && let Expression::ConstantAccess(object) = object
        {
            self.member_objects.insert(object.name.span().start.offset);
        }
    }

    fn is_member(&self, name: &[u8]) -> bool {
        self.class_members
            .last()
            .is_some_and(|members| members.others.contains(name) || members.methods.contains(&IgnoringCase(name)))
    }
}

/// Returns the name PHP writes for `identifier`, allocated in the arena only when it differs from the source.
fn arena_name<'arena, A>(context: &NameResolutionContext<'arena, A>, identifier: &Identifier<'arena>) -> &'arena [u8]
where
    A: Arena,
{
    match php_name(identifier) {
        Cow::Borrowed(name) => name,
        Cow::Owned(name) => context.intern(&name),
    }
}

/// The class of PHP#'s engine-level standard library that a bare `name` before `.` binds to unless the file imports it.
fn sharp_library_class(name: &[u8]) -> Option<&'static [u8]> {
    match name {
        b"Int" => Some(b"Sharp\\Int"),
        b"Float" => Some(b"Sharp\\Float"),
        _ => None,
    }
}

fn class_member_names<'arena>(members: &Sequence<'arena, ClassLikeMember<'arena>>) -> ClassMembers<'arena> {
    let mut names = ClassMembers::default();
    for member in members {
        match member {
            ClassLikeMember::Method(method) => {
                names.methods.insert(IgnoringCase(method.name.value));
                names.others.extend(
                    method
                        .parameter_list
                        .parameters
                        .iter()
                        .filter(|parameter| parameter.is_promoted_property())
                        .map(|parameter| trim_start_byte(parameter.variable.name, b'$')),
                );
            }
            ClassLikeMember::Property(property) => names
                .others
                .extend(property.variables().into_iter().map(|variable| trim_start_byte(variable.name, b'$'))),
            ClassLikeMember::Constant(constant) => {
                names.others.extend(constant.items.iter().map(|item| item.name.value));
            }
            ClassLikeMember::EnumCase(case) => {
                names.others.insert(case.item.name().value);
            }
            _ => {}
        }
    }

    names
}

impl<'ast, 'arena, A> MutWalker<'ast, 'arena, NameResolutionContext<'arena, A>> for NameWalker<'arena>
where
    A: Arena,
{
    fn walk_in_namespace(
        &mut self,
        namespace: &'ast Namespace<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        context.exit_namespace();

        let name = namespace.name.as_ref().map(|ns| arena_name(context, ns));
        if let (Some(ns), Some(name)) = (namespace.name.as_ref(), name) {
            self.resolved_names.insert_at(ns.span(), name, false);
        }

        context.enter_namespace(name);
    }

    fn walk_in_use(&mut self, r#use: &'ast Use<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        context.populate_from_use(r#use);

        match &r#use.items {
            UseItems::Sequence(seq) => {
                for item in &seq.items {
                    let fqn = trim_start_byte(arena_name(context, &item.name), b'\\');
                    self.resolved_names.insert_at(item.name.span(), fqn, true);
                }
            }
            UseItems::TypedSequence(seq) => {
                for item in &seq.items {
                    let fqn = trim_start_byte(item.name.value(), b'\\');
                    self.resolved_names.insert_at(item.name.span(), fqn, true);
                }
            }
            UseItems::TypedList(list) => {
                let prefix = trim_start_byte(list.namespace.value(), b'\\');
                self.resolved_names.insert_at(list.namespace.span(), context.intern(prefix), true);
                for item in &list.items {
                    let fqn = context.intern(&concat_with_sep(&[prefix, item.name.value()], b'\\'));
                    self.resolved_names.insert_at(item.name.span(), fqn, true);
                }
            }
            UseItems::MixedList(list) => {
                let prefix = trim_start_byte(list.namespace.value(), b'\\');
                self.resolved_names.insert_at(list.namespace.span(), context.intern(prefix), true);
                for mixed in &list.items {
                    let fqn = context.intern(&concat_with_sep(&[prefix, mixed.item.name.value()], b'\\'));
                    self.resolved_names.insert_at(mixed.item.name.span(), fqn, true);
                }
            }
        }
    }

    fn walk_in_constant(&mut self, constant: &'ast Constant<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        for item in &constant.items {
            let name = context.qualify_name(item.name.value);

            self.resolved_names.insert_at(item.name.span, name, false);
        }
    }

    fn walk_in_function(&mut self, function: &'ast Function<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        let name = context.qualify_name(function.name.value);

        self.resolved_names.insert_at(function.name.span, name, false);

        if self.sharp {
            self.locals.enter_method();
        }
    }

    fn walk_in_class(&mut self, class: &'ast Class<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        let classlike = context.qualify_name(class.name.value);

        self.resolved_names.insert_at(class.name.span, classlike, false);

        if self.sharp {
            self.class_members.push(class_member_names(&class.members));
        }
    }

    fn walk_out_class(&mut self, _class: &'ast Class<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.class_members.pop();
        }
    }

    fn walk_in_method(&mut self, _method: &'ast Method<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.enter_method();
        }
    }

    fn walk_out_method(&mut self, _method: &'ast Method<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.exit_method();
        }
    }

    fn walk_out_function(
        &mut self,
        _function: &'ast Function<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        if self.sharp {
            self.locals.exit_method();
        }
    }

    fn walk_in_closure(&mut self, closure: &'ast Closure<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        self.enter_lambda(closure.span());
    }

    fn walk_out_closure(&mut self, _closure: &'ast Closure<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        self.exit_lambda();
    }

    fn walk_in_arrow_function(
        &mut self,
        arrow_function: &'ast ArrowFunction<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.enter_lambda(arrow_function.span());
    }

    fn walk_out_arrow_function(
        &mut self,
        _arrow_function: &'ast ArrowFunction<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.exit_lambda();
    }

    fn walk_in_assignment(
        &mut self,
        assignment: &'ast Assignment<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.record_write(assignment.lhs);
    }

    fn walk_in_unary_prefix(
        &mut self,
        unary_prefix: &'ast UnaryPrefix<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        if unary_prefix.operator.is_increment_or_decrement() {
            self.record_write(unary_prefix.operand);
        }
    }

    fn walk_in_unary_postfix(
        &mut self,
        unary_postfix: &'ast UnaryPostfix<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.record_write(unary_postfix.operand);
    }

    fn walk_in_block(&mut self, _block: &'ast Block<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.enter_block();
        }
    }

    fn walk_out_block(&mut self, _block: &'ast Block<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.exit_block();
        }
    }

    /// A PHP# `for` is a block of its own, so the local it declares lives until the loop ends.
    fn walk_in_for(&mut self, _for: &'ast For<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.enter_block();
        }
    }

    fn walk_out_for(&mut self, _for: &'ast For<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.locals.exit_block();
        }
    }

    /// A `for … of` loop is a block of its own. The collection binds before the loop variables exist, and the loop
    /// variables live until the loop ends.
    fn walk_for_of(&mut self, for_of: &'ast ForOf<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        self.locals.enter_block();
        self.walk_expression(for_of.expression, context);

        let kind = if for_of.is_const() { LocalKind::Const } else { LocalKind::Let };
        for name in for_of.target.names() {
            self.declare(name.value, name.span, kind);
        }

        self.walk_statement(for_of.body, context);
        self.locals.exit_block();
    }

    /// A PHP# catch clause is a block of its own, whose variable lives until the clause's block ends.
    fn walk_try_catch_clause(
        &mut self,
        try_catch_clause: &'ast TryCatchClause<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.walk_hint(&try_catch_clause.hint, context);
        if self.sharp {
            self.locals.enter_block();
        }
        if let Some(variable) = &try_catch_clause.variable {
            if self.sharp {
                self.declare(variable.name, variable.span, LocalKind::Let);
            }
            self.walk_direct_variable(variable, context);
        }

        self.walk_block(&try_catch_clause.block, context);
        if self.sharp {
            self.locals.exit_block();
        }
    }

    fn walk_out_function_like_parameter(
        &mut self,
        parameter: &'ast FunctionLikeParameter<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        if self.sharp {
            self.declare(parameter.variable.name, parameter.variable.span, LocalKind::Parameter);
        }
    }

    fn walk_out_local_declaration(
        &mut self,
        local_declaration: &'ast LocalDeclaration<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        let kind = if local_declaration.is_const() { LocalKind::Const } else { LocalKind::Let };

        self.declare(local_declaration.name.value, local_declaration.name.span, kind);
    }

    fn walk_in_method_call(
        &mut self,
        method_call: &'ast MethodCall<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.mark_member_object(method_call.object);
        self.record_changing_call(method_call.object, &method_call.method);
    }

    fn walk_in_method_partial_application(
        &mut self,
        method_partial_application: &'ast MethodPartialApplication<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.mark_member_object(method_partial_application.object);
    }

    fn walk_in_property_access(
        &mut self,
        property_access: &'ast PropertyAccess<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.mark_member_object(property_access.object);
    }

    fn walk_in_null_safe_method_call(
        &mut self,
        null_safe_method_call: &'ast NullSafeMethodCall<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.mark_member_object(null_safe_method_call.object);
        self.record_changing_call(null_safe_method_call.object, &null_safe_method_call.method);
    }

    fn walk_in_null_safe_property_access(
        &mut self,
        null_safe_property_access: &'ast NullSafePropertyAccess<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        self.mark_member_object(null_safe_property_access.object);
    }

    fn walk_in_interface(
        &mut self,
        interface: &'ast Interface<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        let classlike = context.qualify_name(interface.name.value);

        self.resolved_names.insert_at(interface.name.span, classlike, false);

        if self.sharp {
            self.class_members.push(class_member_names(&interface.members));
        }
    }

    fn walk_out_interface(
        &mut self,
        _interface: &'ast Interface<'arena>,
        _context: &mut NameResolutionContext<'arena, A>,
    ) {
        if self.sharp {
            self.class_members.pop();
        }
    }

    fn walk_in_trait(&mut self, r#trait: &'ast Trait<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        let classlike = context.qualify_name(r#trait.name.value);

        self.resolved_names.insert_at(r#trait.name.span, classlike, false);
    }

    fn walk_in_enum(&mut self, r#enum: &'ast Enum<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        let classlike = context.qualify_name(r#enum.name.value);

        self.resolved_names.insert_at(r#enum.name.span, classlike, false);

        if self.sharp {
            self.class_members.push(class_member_names(&r#enum.members));
        }
    }

    fn walk_out_enum(&mut self, _enum: &'ast Enum<'arena>, _context: &mut NameResolutionContext<'arena, A>) {
        if self.sharp {
            self.class_members.pop();
        }
    }

    fn walk_in_trait_use(&mut self, trait_use: &'ast TraitUse<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        for trait_name in &trait_use.trait_names {
            let (trait_classlike, imported) = context.resolve(NameKind::Default, trait_name.value());

            self.resolved_names.insert_at(trait_name.span(), trait_classlike, imported);
        }
    }

    fn walk_in_extends(&mut self, extends: &'ast Extends<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        for parent in &extends.types {
            let (parent_classlike, imported) = context.resolve(NameKind::Default, parent.value());

            self.resolved_names.insert_at(parent.span(), parent_classlike, imported);
        }
    }

    fn walk_in_implements(
        &mut self,
        implements: &'ast Implements<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        for parent in &implements.types {
            let (parent_classlike, imported) = context.resolve(NameKind::Default, parent.value());

            self.resolved_names.insert_at(parent.span(), parent_classlike, imported);
        }
    }

    fn walk_in_inheritance(
        &mut self,
        inheritance: &'ast Inheritance<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        for parent in &inheritance.types {
            let (parent_classlike, imported) = context.resolve(NameKind::Default, parent.value());

            self.resolved_names.insert_at(parent.span(), parent_classlike, imported);
        }
    }

    fn walk_in_hint(&mut self, hint: &'ast Hint<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        if let Hint::Identifier(identifier) = hint {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_attribute(
        &mut self,
        attribute: &'ast Attribute<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        let (name, imported) = context.resolve(NameKind::Default, attribute.name.value());

        self.resolved_names.insert_at(attribute.name.span(), name, imported);
    }

    fn walk_in_function_call(
        &mut self,
        function_call: &'ast FunctionCall<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = function_call.function {
            let (name, imported) = context.resolve(NameKind::Function, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);

            if !self.sharp {
                return;
            }

            if let Some(local) = self.locals.lookup(identifier.value()) {
                self.bind_local(identifier.value(), identifier.span(), local);
            } else if self.is_member(identifier.value()) {
                self.resolved_names.bind(identifier.span(), Binding::Member);
            }
        }
    }

    fn walk_in_function_partial_application(
        &mut self,
        function_partial_application: &'ast FunctionPartialApplication<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = function_partial_application.function {
            let (name, imported) = context.resolve(NameKind::Function, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_instantiation(
        &mut self,
        instantiation: &'ast Instantiation<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = instantiation.class {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_type_of(&mut self, type_of: &'ast TypeOf<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        let (name, imported) = context.resolve(NameKind::Default, type_of.class.value());

        self.resolved_names.insert_at(type_of.class.span(), name, imported);
    }

    fn walk_in_static_method_call(
        &mut self,
        static_method_call: &'ast StaticMethodCall<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = static_method_call.class {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_static_method_partial_application(
        &mut self,
        static_method_partial_application: &'ast StaticMethodPartialApplication<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = static_method_partial_application.class {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_static_property_access(
        &mut self,
        static_property_access: &'ast StaticPropertyAccess<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = static_property_access.class {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_class_constant_access(
        &mut self,
        class_constant_access: &'ast ClassConstantAccess<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        if let Expression::Identifier(identifier) = class_constant_access.class {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_binary(&mut self, binary: &'ast Binary<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        if let (BinaryOperator::Instanceof(_), Expression::Identifier(identifier)) = (binary.operator, binary.rhs) {
            let (name, imported) = context.resolve(NameKind::Default, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_in_constant_access(
        &mut self,
        constant_access: &'ast ConstantAccess<'arena>,
        context: &mut NameResolutionContext<'arena, A>,
    ) {
        let identifier = &constant_access.name;

        if self.sharp {
            let name = identifier.value();
            let span = identifier.span();
            if name == b"this" {
                self.resolved_names.bind(span, Binding::This);

                return;
            }

            if let Some(local) = self.locals.lookup(name) {
                self.bind_local(name, span, local);

                return;
            }

            if let Some(local) = self.locals.lookup_closed(name) {
                self.resolved_names.report_binding_error(BindingError::OutOfScope { name: span, local });
                self.resolved_names.bind(span, Binding::Local(local));

                return;
            }

            let is_member_object = self.member_objects.contains(&span.start.offset);
            let (binding, kind) = if is_member_object {
                (Binding::Class, NameKind::Default)
            } else if self.is_member(name) {
                (Binding::Member, NameKind::Constant)
            } else {
                (Binding::Constant, NameKind::Constant)
            };

            let (mut fqn, imported) = context.resolve(kind, name);
            if is_member_object
                && !imported
                && let Some(class) = sharp_library_class(name)
            {
                fqn = class;
            }
            self.resolved_names.insert_at(span, fqn, imported);
            self.resolved_names.bind(span, binding);

            return;
        }

        if !self.resolved_names.contains(&identifier.span().start) {
            let (name, imported) = context.resolve(NameKind::Constant, identifier.value());

            self.resolved_names.insert_at(identifier.span(), name, imported);
        }
    }

    fn walk_out_namespace(&mut self, _namespace: &Namespace<'arena>, context: &mut NameResolutionContext<'arena, A>) {
        context.exit_namespace();
    }
}
