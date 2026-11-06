//! Properties as fields or properties with accessors (spec section 6), and methods with their return types, `virtual`
//! and `override` (spec sections 7 and 22).

use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator::is_contained_by;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_php_declared_type;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::Block;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::Method as MethodDeclaration;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
use mago_syntax::walker::MutWalker;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::Converter;
use crate::Method;
use crate::body::Locals;

use super::NameCase;
use super::accessors;
use super::has;

/// Finds the first place of `this` a walked body writes: a property of `$this`, or an element of one, that an
/// assignment, `++` or `--` writes.
#[derive(Default)]
struct ThisWrites {
    place: Option<Span>,
}

impl ThisWrites {
    fn written(&mut self, place: &Expression<'_>) {
        let mut base = place;
        loop {
            base = match base {
                Expression::ArrayAccess(access) => access.array,
                Expression::ArrayAppend(append) => append.array,
                Expression::Access(Access::Property(access)) if matches!(access.object, Expression::Variable(Variable::Direct(this)) if this.name == b"$this") =>
                {
                    if self.place.is_none() {
                        self.place = Some(place.span());
                    }
                    return;
                }
                Expression::Access(Access::Property(access)) => access.object,
                _ => return,
            };
        }
    }
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for ThisWrites {
    fn walk_in_assignment(&mut self, assignment: &'ast Assignment<'arena>, _: &mut ()) {
        self.written(assignment.lhs);
    }

    fn walk_in_unary_prefix(&mut self, unary: &'ast UnaryPrefix<'arena>, _: &mut ()) {
        if matches!(unary.operator, UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_)) {
            self.written(unary.operand);
        }
    }

    fn walk_in_unary_postfix(&mut self, unary: &'ast UnaryPostfix<'arena>, _: &mut ()) {
        self.written(unary.operand);
    }
}

impl Converter<'_, '_> {
    /// A property, written again whole, as a field when it is private or protected storage, or a property with
    /// accessors otherwise. Two names in one declaration become one declaration each, on its own line (spec section
    /// 6).
    pub(super) fn property(&mut self, property: &PlainProperty<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let is_static = has(&property.modifiers, |modifier| matches!(modifier, Modifier::Static(_))).is_some();
        let is_readonly = has(&property.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_))).is_some()
            || class.metadata.flags.is_readonly();

        self.attributes(&property.attribute_lists);
        let span = property.span();
        let declaration_start = property
            .modifiers
            .first()
            .map(HasSpan::span)
            .or(property.var.as_ref().map(HasSpan::span))
            .or(property.hint.as_ref().map(HasSpan::span))
            .map_or(span.start.offset, |first| first.start.offset);
        let attributes = self.edits.render(self.source, span.start.offset, declaration_start);

        let mut declarations = Vec::new();
        for item in property.items.iter() {
            let Some((head, union, has_accessors)) = self.property_head(&property.modifiers, item.variable()) else {
                continue;
            };
            let Some(value) = self.initial_value(item, &union, property.hint.is_none(), is_static) else {
                continue;
            };
            let accessors = accessors(has_accessors, is_readonly, &property.modifiers);
            // A property's accessors end it, unless an initial value follows them (spec section 6).
            let end = if accessors.is_empty() || !value.is_empty() { ";" } else { "" };
            declarations.push(format!("{attributes}{head}{accessors}{value}{end}"));
        }

        let indentation = self.indentation(span.start.offset);
        self.edits.replace(span, declarations.join(&format!("\n{indentation}")));
        self.docblock(span);
    }

    /// A property with hooks, as a property whose accessors are its hooks (spec section 6). Each keeps its body, where
    /// PHP's `$this->name` is the storage `field` and `$value` is `value`, and a short `set => expr` stores `expr`. A
    /// property with storage gains the accessor PHP gives it where it declares no hook, and its initial value follows
    /// the accessors.
    pub(super) fn hooked_property(&mut self, property: &HookedProperty<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let metadata = class.metadata;
        let variable = property.item.variable();
        self.attributes(&property.attribute_lists);
        let Some((head, union, _)) = self.property_head(&property.modifiers, variable) else {
            return;
        };
        let Some(declared) = metadata.properties.get(&word(variable.name)) else {
            return;
        };
        let is_abstract = metadata.kind.is_interface()
            || has(&property.modifiers, |modifier| matches!(modifier, Modifier::Abstract(_))).is_some();
        let has_storage = !is_abstract && !declared.flags.is_virtual_property();
        let value = if has_storage {
            let Some(value) = self.initial_value(&property.item, &union, property.hint.is_none(), false) else {
                return;
            };
            value
        } else {
            String::new()
        };
        let start = property
            .modifiers
            .first()
            .map(HasSpan::span)
            .or(property.var.as_ref().map(HasSpan::span))
            .or(property.hint.as_ref().map(HasSpan::span))
            .unwrap_or(variable.span)
            .start
            .offset;
        self.edits.replace_range(start, property.item.span().end.offset, head);

        // PHP's `private(set)` and `protected(set)` are the `set` accessor's own access level.
        let set_access = match property.modifiers.iter().find(|modifier| modifier.is_write_visibility()) {
            Some(Modifier::PrivateSet(_)) => "private ",
            Some(Modifier::ProtectedSet(_)) => "protected ",
            _ => "",
        };
        let hooks = &property.hook_list.hooks;
        let has_get = hooks.iter().any(|hook| hook.name.value == b"get");
        let has_set = hooks.iter().any(|hook| hook.name.value == b"set");
        if has_storage && let (Some(first), Some(last)) = (hooks.first(), hooks.last()) {
            if !has_get {
                self.edits.insert(first.span().start.offset, "get; ");
            }
            if !has_set {
                self.edits.insert(last.span().end.offset, format!(" {set_access}set;"));
            }
        } else if has_set && !has_get {
            self.refuse(
                property.hook_list.left_brace,
                "a property with a `set` hook, no `get` hook and no storage has no PHP# form: a PHP# property needs a `get` accessor (spec section 6)",
            );
        }
        for hook in hooks.iter() {
            let is_set = hook.name.value == b"set";
            self.hook(hook, is_set, &union, declared, variable);
            if is_set && !set_access.is_empty() {
                self.edits.insert(hook.name.span.start.offset, set_access);
            }
        }
        if !value.is_empty() {
            self.edits.insert(property.hook_list.right_brace.end.offset, format!("{value};"));
        }
        self.docblock(property.span());
    }

    /// A hook as an accessor, `get` or `set` by `is_set`, of the property `variable` names, whose type is `union`.
    fn hook(
        &mut self,
        hook: &PropertyHook<'_>,
        is_set: bool,
        union: &TUnion,
        declared: &PropertyMetadata,
        variable: &DirectVariable<'_>,
    ) {
        self.attributes(&hook.attribute_lists);
        if let Some(ampersand) = hook.ampersand {
            self.refuse(ampersand, "a `get` hook that returns by reference has no PHP# form: PHP# has no references");
            return;
        }
        // A `final` hook stops a subclass replacing it, and PHP# replaces a property whole or not at all.
        for modifier in hook.modifiers.iter() {
            let span = modifier.span();
            self.edits.delete(span.start.offset, self.skip_whitespace(span.end.offset));
        }
        // A `set` accessor's `value` has the property's type (spec section 6), as a `set` hook's `$value` has when
        // the hook declares no parameter.
        if let Some(list) = &hook.parameter_list {
            let parameter = declared.hooks.get(&word(b"set")).and_then(|hook| hook.parameter.as_ref());
            let fits = list.parameters.len() == 1
                && list.parameters.first().is_some_and(|parameter| parameter.variable.name == b"$value")
                && parameter.is_some_and(|parameter| {
                    parameter.get_type_metadata().is_none_or(|declared| declared.type_union.get_id() == union.get_id())
                });
            if !fits {
                self.refuse(
                    list.span(),
                    "a `set` hook whose parameter is other than `$value` of the property's type has no PHP# form: a `set` accessor's `value` has the property's type (spec section 6)",
                );
                return;
            }
            self.edits.delete(hook.name.span.end.offset, list.span().end.offset);
        }
        let PropertyHookBody::Concrete(body) = &hook.body else {
            return;
        };
        // A PHP `get` hook may keep what it computes in a field, which PHP# makes the language's own work in `lazy`
        // (spec sections 6.1 and 6.5).
        if !is_set {
            let mut writes = ThisWrites::default();
            match body {
                PropertyHookConcreteBody::Block(block) => writes.walk_block(block, &mut ()),
                PropertyHookConcreteBody::Expression(body) => writes.walk_expression(body.expression, &mut ()),
            }
            if let Some(place) = writes.place {
                let shown = String::from_utf8_lossy(self.text(place)).into_owned();
                return self.refuse(
                    place,
                    format!("a `get` hook that writes `{shown}` has no PHP# form: a `get` body never writes `this` (spec section 6.1)"),
                );
            }
        }

        self.method = Some(Method {
            returning: (!is_set).then(|| union.clone()),
            resolution: TypeResolutionContext::default(),
            parameters: if is_set { vec![b"$value".to_vec()] } else { Vec::new() },
            variadic: None,
            locals: Locals::default(),
            super_call: None,
            storage: Some(variable.name[1..].to_vec()),
            captures: None,
            returned: Vec::new(),
        });
        match body {
            PropertyHookConcreteBody::Block(block) => self.block(block),
            PropertyHookConcreteBody::Expression(body) => {
                self.expression(body.expression, Some(union));
                if !is_set {
                    self.narrowed(body.expression, union);
                }
                // A short `set => expr` stores `expr`.
                if is_set {
                    self.edits.insert(body.expression.span().start.offset, "field = ");
                }
            }
        }
        self.declare_locals();
        self.method = None;
    }

    /// `public override string name`, which the declaration of the property `variable` names starts with, its type,
    /// and whether it is a property with accessors rather than a field: a public one, or one that replaces its
    /// parent's (spec sections 6 and 22). An interface's property takes no access modifier (spec section 5).
    fn property_head(
        &mut self,
        modifiers: &Sequence<'_, Modifier<'_>>,
        variable: &DirectVariable<'_>,
    ) -> Option<(String, TUnion, bool)> {
        let metadata = self.class.as_ref()?.metadata;
        // A type the codex marks `inferred` here is inherited: declared in the parent, which the override rules need.
        let Some(declared) = metadata.properties.get(&word(variable.name)) else {
            self.refuse(variable.span, "the analysis holds no property of this name");
            return None;
        };
        if self.narrowed_by_docblock(declared.type_metadata.as_ref(), "@var", variable.span) {
            return None;
        }
        let union = declared.type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone());
        let written = self.type_text(&union, variable.span)?;
        let name = Self::variable_name(variable.name);
        if self.names_inherited_method(variable) {
            return None;
        }

        let visibility = match modifiers.iter().find(|modifier| modifier.is_read_visibility()) {
            Some(Modifier::Protected(_)) => "protected ",
            Some(Modifier::Private(_)) => "private ",
            _ if metadata.kind.is_interface() => "",
            _ => "public ",
        };
        // A property that redeclares its parent's, as one that changes its initial value does, replaces it, which
        // only a property with accessors marked `override` does: a field replaces nothing (spec section 22).
        let overrides =
            metadata.overridden_property_ids.get(&word(variable.name)).is_some_and(|parents| !parents.is_empty());
        let is_static = has(modifiers, |modifier| matches!(modifier, Modifier::Static(_))).is_some();
        let is_abstract = has(modifiers, |modifier| matches!(modifier, Modifier::Abstract(_))).is_some();
        let is_public = !matches!(visibility, "private " | "protected ");
        // A PHP# property is closed unless it is `virtual` (spec section 6.1), so one a subclass may replace takes it,
        // as an open method does.
        let is_open = is_public
            && !overrides
            && !is_static
            && !is_abstract
            && !metadata.kind.is_interface()
            && !self.class.as_ref().is_some_and(|class| class.is_final)
            && has(modifiers, |modifier| matches!(modifier, Modifier::Final(_))).is_none();
        // An override keeps the name of what it replaces.
        let kind = if is_public || overrides { "property" } else { "field" };
        if !overrides && self.misnamed(&variable.name[1..], variable.span, kind, NameCase::Camel) {
            return None;
        }
        let mut opening = String::new();
        if is_static {
            opening.push_str("static ");
        }
        if is_abstract {
            opening.push_str("abstract ");
        }
        if overrides {
            opening.push_str("override ");
        }
        if is_open {
            opening.push_str("virtual ");
        }

        Some((format!("{visibility}{opening}{written} {name}"), union, is_public || overrides))
    }

    /// Refuses the property `variable` names, a declared or a promoted one, when the class inherits a method of its
    /// name, and gives whether it did. PHP# reads a name as a property or a method by where it is written, so a class
    /// has one member of each name (spec section 14). A method the class declares is refused where it is declared.
    pub(super) fn names_inherited_method(&mut self, variable: &DirectVariable<'_>) -> bool {
        let Some(metadata) = self.class.as_ref().map(|class| class.metadata) else {
            return false;
        };
        let method = ascii_lowercase_word(&variable.name[1..]);
        if !metadata.appearing_method_ids.contains_key(&method) || metadata.methods.contains(&method) {
            return false;
        }
        let name = Self::variable_name(variable.name);
        self.refuse(
            variable.span,
            format!(
                "`{name}` names a property and a method, and a PHP# class has one member of each name (spec section 14)"
            ),
        );

        true
    }

    /// ` = value`, which the property `item` declares ends with, or nothing when the constructor sets it, or a
    /// refusal. A property of no type, `untyped`, starts as PHP's `null`.
    fn initial_value(
        &mut self,
        item: &PropertyItem<'_>,
        union: &TUnion,
        untyped: bool,
        is_static: bool,
    ) -> Option<String> {
        let variable = item.variable();
        let name = String::from_utf8_lossy(variable.name);
        match item {
            PropertyItem::Concrete(concrete) => {
                self.expression(concrete.value, Some(union));
                Some(format!(" = {}", self.render(concrete.value)))
            }
            // PHP gives an untyped property null, which PHP# writes, since a member has no implicit value (spec
            // section 6).
            PropertyItem::Abstract(_) if untyped => Some(" = null".to_owned()),
            // A static holds a value of its type before any code reads it (spec section 6).
            PropertyItem::Abstract(_) if is_static => {
                self.refuse(
                    variable.span,
                    format!("`{name}` is static and has no initial value: a PHP# static holds a value of its type before any code reads it (spec section 6)"),
                );
                None
            }
            PropertyItem::Abstract(_)
                if self.class.as_ref().is_some_and(|class| {
                    class.set_by_constructor.iter().any(|set| set.as_slice() == variable.name)
                }) =>
            {
                Some(String::new())
            }
            // PHP leaves a typed property unset until code outside the class sets it, where PHP# sets every property
            // before the object is used.
            PropertyItem::Abstract(_) => {
                self.refuse(
                    variable.span,
                    format!(
                        "`{name}` has no initial value, and the constructor doesn't set it: a PHP# property is set before the object is used (spec section 6)"
                    ),
                );
                None
            }
        }
    }

    /// A class constant as `public const int LIMIT = 3;`, one declaration per name, with the type its declaration
    /// writes or its value has (spec section 2). PHP's `final` goes, since a PHP# subclass declares no constant of its
    /// parent's. A constant with no PHP type converts only where no subclass declares it again, since PHP makes a
    /// subclass's constant keep a type its parent's declares.
    pub(super) fn class_constant(&mut self, constant: &ClassLikeConstant<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let metadata = class.metadata;
        let visibility = match constant.modifiers.iter().find(|modifier| modifier.is_read_visibility()) {
            Some(Modifier::Protected(_)) => "protected",
            Some(Modifier::Private(_)) => "private",
            _ => "public",
        };

        self.attributes(&constant.attribute_lists);
        let span = constant.span();
        let start = constant.modifiers.first().map_or(constant.r#const.span, HasSpan::span).start.offset;
        let attributes = self.edits.render(self.source, span.start.offset, start);
        // The lowering writes the type into the constant's PHP, which a subclass's constant of the name must then keep.
        let overridable = constant.hint.is_none()
            && visibility != "private"
            && !metadata.flags.is_final()
            && !metadata.kind.is_enum();
        let mut declarations = Vec::new();
        for item in constant.items.iter() {
            let name = String::from_utf8_lossy(item.name.value).into_owned();
            if self.misnamed(item.name.value, item.name.span, "constant", NameCase::UpperSnake) {
                continue;
            }
            if overridable {
                self.refuse(
                    item.name.span,
                    format!("the constant `{name}` has no type in PHP, and the type PHP# gives it would bind a subclass's `{name}` to a compatible one: a constant with no type converts only in a `final` class or as `private`"),
                );
                continue;
            }
            let declared = metadata.constants.get(&word(item.name.value));
            let union = declared.and_then(|declared| {
                declared
                    .type_metadata
                    .as_ref()
                    .map(|metadata| metadata.type_union.clone())
                    .or_else(|| declared.inferred_type.clone().map(TUnion::from_atomic))
            });
            let Some(mut union) = union else {
                self.refuse(item.name.span, format!("the analysis gives the constant `{name}` no type"));
                continue;
            };
            union.widen_literals();
            let Some(written) = self.type_text(&union, item.name.span) else {
                continue;
            };
            self.expression(item.value, Some(&union));
            let value = self.render(item.value);
            declarations.push(format!("{attributes}{visibility} const {written} {name} = {value};"));
        }

        let indentation = self.indentation(span.start.offset);
        self.edits.replace(span, declarations.join(&format!("\n{indentation}")));
        self.docblock(span);
    }

    pub(super) fn method(&mut self, method: &MethodDeclaration<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let (class_name, metadata, class_is_final) = (class.name.clone(), class.metadata, class.is_final);
        let name = method.name.value;
        let shown = String::from_utf8_lossy(name).into_owned();
        let Some(declared) = self.input.codebase.get_method(metadata.name.as_bytes(), name) else {
            self.refuse(method.name.span, "the analysis holds no method of this name");
            return;
        };
        let Some(function) = method.function else {
            return;
        };
        if let Some(ampersand) = method.ampersand {
            self.refuse(ampersand, "a method that returns by reference has no PHP# form: PHP# has no references");
            return;
        }
        // A static method keeps `static`, and a subclass replaces it as it does an instance method, so it takes
        // `virtual` and `override` after it (spec section 11).
        let is_constructor = name.eq_ignore_ascii_case(b"__construct");
        // PHP's `__toString()` is PHP#'s `string toString()`, which every conversion to text calls (spec section 18).
        let is_text = name.eq_ignore_ascii_case(b"__tostring");
        if is_text {
            self.edits.replace(method.name.span, "toString");
        } else if name.starts_with(b"__") && !is_constructor {
            self.refuse(method.name.span, format!("the magic method `{shown}` is not converted yet"));
            return;
        }
        // A method that replaces another keeps the name of what it replaces.
        if !is_constructor && !is_text && !self.input.codebase.method_is_overriding(metadata.name.as_bytes(), name) {
            self.misnamed(name, method.name.span, "method", NameCase::Camel);
        }
        // PHP# reads a name as a property or a method by where it is written, so a class has one member of each name
        // (spec section 14).
        // A property the class inherits clashes as one it declares does.
        if metadata.appearing_property_ids.contains_key(&word([b"$", name].concat())) {
            self.refuse(
                method.name.span,
                format!("`{shown}` names a property and a method, and a PHP# class has one member of each name (spec section 14)"),
            );
            return;
        }

        // An interface member is as visible as its interface, so it takes no access modifier (spec section 5).
        let is_interface = metadata.kind.is_interface();
        if is_interface && has(&method.modifiers, |modifier| matches!(modifier, Modifier::Static(_))).is_some() {
            self.refuse(
                method.name.span,
                format!("`{shown}` is a static method of an interface, which has no PHP# form: a PHP# interface declares instance members (spec section 22)"),
            );
            return;
        }
        self.attributes(&method.attribute_lists);
        if is_interface {
            for modifier in method.modifiers.iter().filter(|modifier| modifier.is_read_visibility()) {
                let span = modifier.span();
                self.edits.delete(span.start.offset, self.skip_whitespace(span.end.offset));
            }
        } else if let Some(at) = Self::missing_visibility(&method.modifiers, function.span) {
            self.edits.insert(at, "public ");
        }
        let is_abstract = has(&method.modifiers, |modifier| matches!(modifier, Modifier::Abstract(_))).is_some();

        if is_constructor {
            self.edits.replace_range(function.span.start.offset, method.name.span.end.offset, class_name);
        } else {
            // A return the codex marks `inferred` is inherited from the parent, which is declared. A return taken from
            // the body is the analysis's inference.
            if declared.return_from_body.is_some() {
                self.refuse(
                    method.name.span,
                    format!("`{shown}`'s return is taken from its body, which the analysis infers, and the converter writes no inferred type"),
                );
                return;
            }
            if self.narrowed_by_docblock(declared.return_type_metadata.as_ref(), "@return", method.name.span) {
                return;
            }
            let returned =
                declared.return_type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone());
            let Some(written) = self.type_text(&returned, method.name.span) else {
                return;
            };

            let is_private = has(&method.modifiers, |modifier| matches!(modifier, Modifier::Private(_))).is_some();
            let is_final = has(&method.modifiers, |modifier| matches!(modifier, Modifier::Final(_)));
            let overrides = metadata.overridden_method_ids.get(&ascii_lowercase_word(name)).is_some_and(|parents| {
                parents.keys().any(|parent| {
                    self.input
                        .codebase
                        .get_class_like(parent.as_bytes())
                        .is_some_and(|parent| !parent.kind.is_interface())
                })
            });
            // An override takes at least what each method it replaces takes (spec section 22). PHP lets it take less,
            // as a typed parameter in place of an untyped one, and a parameter's type here is the one it declares.
            let replaced =
                metadata.overridden_method_ids.get(&ascii_lowercase_word(name)).into_iter().flat_map(|parents| {
                    parents.keys().filter_map(|parent| self.input.codebase.get_method(parent.as_bytes(), name))
                });
            let type_id = |parameter: Option<&mago_codex::metadata::parameter::FunctionLikeParameterMetadata>| {
                parameter.map(|parameter| {
                    parameter
                        .get_type_metadata()
                        .map_or_else(get_mixed, |declared| declared.type_union.clone())
                        .get_id()
                })
            };
            let narrower = replaced.flat_map(|parent| {
                declared.parameters.iter().enumerate().filter(move |(index, parameter)| {
                    let theirs = type_id(parent.parameters.get(*index));
                    theirs.is_some() && theirs != type_id(Some(parameter))
                })
            });
            if let Some((index, _)) = narrower.into_iter().next() {
                let parameter = &method.parameter_list.parameters.as_slice()[index];
                let shown = String::from_utf8_lossy(parameter.variable.name);
                self.refuse(
                    parameter.variable.span,
                    format!("`{shown}` takes another type than the parameter of the method this one replaces, and a PHP# override takes at least what it replaces takes (spec section 22)"),
                );
                return;
            }
            if self.narrows_linked_parameter(metadata, declared, method) {
                return;
            }
            // An interface's member and an abstract method are replaced by definition, so they take neither marker.
            let opening = if is_interface || is_abstract {
                ""
            } else if overrides {
                "override "
            } else if !class_is_final && !is_private && is_final.is_none() {
                "virtual "
            } else {
                ""
            };
            // A closed method is PHP#'s default, and `final` stops only an override chain (spec section 22).
            if let Some(span) = is_final
                && !overrides
            {
                self.edits.delete(span.start.offset, self.skip_whitespace(span.end.offset));
            }
            self.edits.replace(function.span, format!("{opening}{written}"));
            if let Some(hint) = &method.return_type_hint {
                self.edits.delete(method.parameter_list.right_parenthesis.end.offset, hint.span().end.offset);
            }
        }

        let parameters: Vec<Vec<u8>> =
            method.parameter_list.parameters.iter().map(|parameter| parameter.variable.name.to_vec()).collect();
        let variadic = method
            .parameter_list
            .parameters
            .iter()
            .find(|parameter| parameter.ellipsis.is_some())
            .map(|parameter| parameter.variable.name.to_vec());
        for (index, parameter) in method.parameter_list.parameters.iter().enumerate() {
            let metadata = declared.parameters.get(index).and_then(|parameter| parameter.type_metadata.as_ref());
            let tag = format!("@param {}", String::from_utf8_lossy(parameter.variable.name));
            if self.narrowed_by_docblock(metadata, &tag, parameter.variable.span) {
                continue;
            }
            self.parameter(parameter, metadata.map(|metadata| metadata.type_union.clone()));
        }

        let returning = (!is_constructor).then(|| {
            declared.return_type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone())
        });
        match &method.body {
            MethodBody::Concrete(block) => {
                let resolution = declared.type_resolution_context.clone().unwrap_or_default();
                self.method = Some(Method {
                    returning,
                    resolution,
                    parameters,
                    variadic,
                    locals: Locals::default(),
                    super_call: None,
                    storage: None,
                    captures: None,
                    returned: Vec::new(),
                });
                if is_constructor && let Some(parent) = metadata.direct_parent_class {
                    self.super_call(method, block, parent.as_bytes());
                }
                self.block(block);
                self.declare_locals();
                let closed = class_is_final
                    || has(&method.modifiers, |modifier| matches!(modifier, Modifier::Final(_) | Modifier::Private(_)))
                        .is_some();
                if closed && !declared.flags.has_yield() {
                    self.unused_null(method, block);
                }
                self.method = None;
            }
            MethodBody::Abstract(_) => {}
            MethodBody::Expression(_) => self.refuse(method.body.span(), "an expression body is PHP# only"),
        }
        self.docblock(method.span());
    }

    /// Refuses a parameter of `declared`, the method `method` declares in the class `metadata`, that takes less once
    /// PHP# writes its type in the PHP it compiles to than the PHP declaration of a method it replaces takes, and
    /// gives whether it did. PHP links an override only when it takes at least that (spec section 22), and an untyped
    /// parameter takes the type the PHPDoc of the method it replaces gives it, which PHP's own declaration may not.
    fn narrows_linked_parameter(
        &mut self,
        metadata: &ClassLikeMetadata,
        declared: &FunctionLikeMetadata,
        method: &MethodDeclaration<'_>,
    ) -> bool {
        let codebase = self.input.codebase;
        let name = ascii_lowercase_word(declared.name.as_bytes());
        let parents = metadata.overridden_method_ids.get(&name).into_iter().flat_map(|parents| parents.keys());
        for parent in parents {
            let (Some(class), Some(replaced)) = (
                codebase.get_class_like(parent.as_bytes()),
                codebase.get_method(parent.as_bytes(), declared.name.as_bytes()),
            ) else {
                continue;
            };
            for (index, (theirs, ours)) in replaced.parameters.iter().zip(&declared.parameters).enumerate() {
                let linked = |parameter: &mago_codex::metadata::parameter::FunctionLikeParameterMetadata| {
                    parameter
                        .type_declaration_metadata
                        .as_ref()
                        .map_or_else(get_mixed, |declared| declared.type_union.clone())
                };
                let theirs = get_php_declared_type(&linked(theirs), codebase);
                let written = ours.get_type_metadata().map_or_else(get_mixed, |declared| declared.type_union.clone());
                let ours = get_php_declared_type(&written, codebase);
                if is_contained_by(codebase, &theirs, &ours, false, false, false, &mut ComparisonResult::new()) {
                    continue;
                }
                let Some(parameter) = method.parameter_list.parameters.get(index) else {
                    continue;
                };
                let shown = String::from_utf8_lossy(parameter.variable.name);
                self.refuse(
                    parameter.variable.span,
                    format!(
                        "`{shown}` takes `{}`, which PHP# writes in the PHP it compiles to, and `{}::{}`'s PHP takes `{}` there, which PHP links an override to only when it takes at least that (spec section 22)",
                        ours.get_id(),
                        class.original_name.as_str_lossy(),
                        replaced.original_name.as_str_lossy(),
                        theirs.get_id(),
                    ),
                );
                return true;
            }
        }

        false
    }

    /// Refuses `method`, which no subclass replaces, when its declared return holds `null` and no `return` of its
    /// `block` gives one. PHP# refuses such a `?` (spec section 14.4), where PHP keeps it, and writing the narrower type
    /// would write what the analysis infers (decision 2), so the PHP declares it first.
    fn unused_null(&mut self, method: &MethodDeclaration<'_>, block: &Block<'_>) {
        let Some(state) = self.method.as_ref() else {
            return;
        };
        let Some(returning) = state.returning.clone() else {
            return;
        };
        let uninformative =
            |union: &TUnion| union.has_mixed() || union.is_never() || union.is_void() || union.has_template_types();
        // A body that runs on past its last statement returns `null`.
        let ends = match block.statements.last() {
            Some(Statement::Return(_)) => true,
            Some(Statement::Expression(statement)) => matches!(statement.expression, Expression::Throw(_)),
            _ => false,
        };
        if !returning.has_null() || uninformative(&returning) || !ends || state.returned.is_empty() {
            return;
        }
        let codebase = self.input.codebase;
        let fits = state.returned.iter().all(|returned| {
            returned.as_ref().is_some_and(|returned| {
                !uninformative(returned)
                    && !returned.has_null()
                    && is_contained_by(
                        codebase,
                        returned,
                        &returning,
                        false,
                        false,
                        false,
                        &mut ComparisonResult::new(),
                    )
            })
        });
        if !fits {
            return;
        }
        let bare = TUnion::from_vec(returning.types.iter().filter(|atomic| !atomic.is_null()).cloned().collect());
        let (Some(written), Some(narrower)) =
            (self.type_text(&returning, method.name.span), self.type_text(&bare, method.name.span))
        else {
            return;
        };
        let name = String::from_utf8_lossy(method.name.value);
        self.refuse(
            method.name.span,
            format!("`{name}` declares `{written}` and never returns `null`, which PHP# refuses on a method no subclass replaces (spec section 14.4): declare `{narrower}`"),
        );
    }
}
