//! Classes and their members: the header, properties and fields, methods and constructors, and their parameters.
//!
//! PHP# defaults a class to `internal` and a member to `private` (spec section 5), so the converter writes `public`
//! wherever PHP leaves the visibility out. PHP's methods are open unless `final`, and PHP#'s are closed unless
//! `virtual` (spec section 22), so an open PHP method is `virtual`, and one that replaces its parent's is `override`.

use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Method as MethodDeclaration;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::Variable;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::Class;
use crate::Converter;
use crate::Method;
use crate::body::Locals;

/// The PHPDoc tags whose types the converter writes in the declaration, which leave its docblock.
const WRITTEN_TAGS: [&str; 10] = [
    "@var",
    "@param",
    "@return",
    "@template",
    "@template-covariant",
    "@template-contravariant",
    "@psalm-template",
    "@psalm-var",
    "@psalm-param",
    "@psalm-return",
];

/// The accessors PHP# writes after a property's name: none for a field, which is storage, and `get` alone for a
/// `readonly` one (spec sections 6 and 13).
fn accessors(is_public: bool, is_readonly: bool) -> &'static str {
    match (is_public, is_readonly) {
        (_, true) => " { get; }",
        (true, false) => " { get; set; }",
        (false, false) => "",
    }
}

fn has(modifiers: &Sequence<'_, Modifier<'_>>, matches: impl Fn(&Modifier<'_>) -> bool) -> Option<Span> {
    modifiers.iter().find(|modifier| matches(modifier)).map(HasSpan::span)
}

impl Converter<'_, '_> {
    pub(crate) fn class(&mut self, class: &mago_syntax::cst::Class<'_>) {
        let name = String::from_utf8_lossy(class.name.value).into_owned();
        let Some(metadata) = self.input.codebase.get_class_like(self.input.names.get(&class.name)) else {
            self.refuse(class.name.span, "the analysis holds no class of this name");
            return;
        };
        if let Some(span) = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Abstract(_))) {
            self.refuse(span, "an abstract class is not converted yet");
        }
        if let Some(span) = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_))) {
            self.refuse(span, "a `readonly` class is not converted yet: each of its properties becomes `{ get; }`");
        }
        if !metadata.template_type_extends_count.is_empty() || !metadata.template_type_implements_count.is_empty() {
            self.refuse(class.name.span, "`@extends` and `@implements` with type arguments are not converted yet");
        }

        self.attributes(&class.attribute_lists);
        let header_start = class.modifiers.first().map_or(class.class.span, HasSpan::span).start.offset;
        self.edits.insert(header_start, "public ");
        if let Some(parameters) = self.type_parameters(metadata, class.name.span)
            && !parameters.is_empty()
        {
            self.edits.insert(class.name.span.end.offset, parameters);
        }

        // `extends A implements B, C` is the header `: A, B, C` (spec section 22).
        let mut parents: Vec<&Identifier<'_>> = Vec::new();
        if let Some(extends) = &class.extends {
            self.edits.replace(extends.extends.span, ":");
            parents.extend(extends.types.iter());
        }
        if let Some(implements) = &class.implements {
            match parents.last() {
                Some(last) => {
                    self.edits.replace_range(last.span().end.offset, implements.implements.span.end.offset, ",");
                }
                None => self.edits.replace(implements.implements.span, ":"),
            }
            parents.extend(implements.types.iter());
        }
        for parent in parents {
            self.rename_class(parent);
        }
        self.docblock(class.span());

        let is_final = has(&class.modifiers, |modifier| matches!(modifier, Modifier::Final(_))).is_some();
        let set_by_constructor = set_by_constructor(class);
        self.class = Some(Class { name, metadata, is_final, set_by_constructor });
        for member in &class.members {
            match member {
                ClassLikeMember::Method(method) => self.method(method),
                ClassLikeMember::Property(Property::Plain(property)) => self.property(property),
                ClassLikeMember::Property(_) => {
                    self.refuse(
                        member.span(),
                        "a property with hooks is not converted yet: its hooks become accessors",
                    );
                }
                ClassLikeMember::Constant(_) => self.refuse(member.span(), "a class constant is not converted yet"),
                ClassLikeMember::TraitUse(_) => {
                    self.refuse(
                        member.span(),
                        "a trait a class uses is not converted yet: it joins the class's header",
                    );
                }
                _ => self.refuse(member.span(), "this member has no PHP# form"),
            }
        }
        self.class = None;
    }

    /// PHP's `#[…]` attributes as PHP#'s `[…]` (spec section 1). Each stays, with its arguments.
    pub(crate) fn attributes(&mut self, lists: &Sequence<'_, AttributeList<'_>>) {
        for list in lists {
            self.edits.replace(list.hash_left_bracket, "[");
            for attribute in &list.attributes {
                self.rename_class(&attribute.name);
                let Some(arguments) = &attribute.argument_list else {
                    continue;
                };
                for argument in &arguments.arguments {
                    match argument {
                        PartialArgument::Positional(argument) if argument.ellipsis.is_none() => {
                            self.expression(argument.value, None);
                        }
                        PartialArgument::Named(argument) => self.expression(argument.value, None),
                        _ => self.refuse(
                            argument.span(),
                            "a spread or placeholder argument of an attribute has no PHP# form",
                        ),
                    }
                }
            }
        }
    }

    /// Takes each tag whose type the declaration now writes out of the docblock right before `declaration`, and the
    /// whole docblock when nothing else is left in it. Its prose and every other tag stay.
    pub(crate) fn docblock(&mut self, declaration: Span) {
        let Some(docblock) = self.docblock_before(declaration) else {
            return;
        };

        let text = String::from_utf8_lossy(docblock.1).into_owned();
        let is_written = |line: &str| {
            let content = line.trim().trim_start_matches("/**").trim_start_matches('*').trim();
            content.split_whitespace().next().is_some_and(|tag| WRITTEN_TAGS.contains(&tag))
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut kept: Vec<&str> = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            if is_written(lines[index]) {
                // A tag's text runs on over the lines up to the next tag or the end, as a long `@param` does.
                index += 1;
                while index < lines.len() && !is_tag_or_end(lines[index]) {
                    index += 1;
                }
                continue;
            }
            kept.push(lines[index]);
            index += 1;
        }
        if kept.len() == lines.len() {
            return;
        }

        let is_empty = |line: &str| {
            let content = line.trim().trim_start_matches("/**").trim_end_matches("*/").trim_start_matches('*').trim();
            content.is_empty()
        };
        if kept.iter().all(|line| is_empty(line)) {
            self.edits.delete(docblock.0.start.offset, self.skip_whitespace(docblock.0.end.offset));
            return;
        }
        // A blank line left right before the closing `*/` goes too.
        while kept.len() > 2 && is_empty(kept[kept.len() - 2]) && !kept[kept.len() - 2].contains("/**") {
            kept.remove(kept.len() - 2);
        }
        self.edits.replace(docblock.0, kept.join("\n"));
    }

    /// The docblock right before `declaration`, with nothing but whitespace between them, and its text.
    pub(crate) fn docblock_before(&self, declaration: Span) -> Option<(Span, &[u8])> {
        let start = declaration.start.offset;
        self.input
            .program
            .trivia
            .iter()
            .rev()
            .find(|trivia| {
                trivia.kind.is_docblock()
                    && trivia.span.end.offset <= start
                    && self.source[trivia.span.end.offset as usize..start as usize].iter().all(u8::is_ascii_whitespace)
            })
            .map(|trivia| (trivia.span, trivia.value))
    }

    /// The offset `public ` goes at for a member whose modifiers name no visibility, which PHP makes public.
    fn missing_visibility(modifiers: &Sequence<'_, Modifier<'_>>, keyword: Span) -> Option<u32> {
        let has_visibility = modifiers
            .iter()
            .any(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)));

        (!has_visibility).then(|| modifiers.first().map_or(keyword, HasSpan::span).start.offset)
    }

    /// A property, written again whole, as a field when it is private or protected storage, or a property with
    /// accessors otherwise. Two names in one declaration become one declaration each, on its own line (spec section
    /// 6).
    fn property(&mut self, property: &PlainProperty<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let metadata = class.metadata;
        if let Some(span) = has(&property.modifiers, |modifier| matches!(modifier, Modifier::Static(_))) {
            self.refuse(span, "a static property is not converted yet");
            return;
        }
        if let Some(span) = has(&property.modifiers, |modifier| modifier.is_write_visibility()) {
            self.refuse(span, "a property with a visibility of its own for `set` is not converted yet");
            return;
        }
        let visibility = match property.modifiers.iter().find(|modifier| modifier.is_read_visibility()) {
            Some(Modifier::Protected(_)) => "protected",
            Some(Modifier::Private(_)) => "private",
            _ => "public",
        };
        let is_readonly = has(&property.modifiers, |modifier| matches!(modifier, Modifier::Readonly(_))).is_some();

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
            let variable = item.variable();
            // A type the codex marks `inferred` here is inherited: declared in the parent, which the override rules
            // need.
            let Some(declared) = metadata.properties.get(&word(variable.name)) else {
                self.refuse(variable.span, "the analysis holds no property of this name");
                continue;
            };
            let union = declared.type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone());
            let Some(written) = self.type_text(&union, variable.span) else {
                continue;
            };
            let value = match item {
                PropertyItem::Concrete(concrete) => {
                    self.expression(concrete.value, Some(&union));
                    format!(" = {}", self.render(concrete.value))
                }
                // PHP gives an untyped property null, which PHP# writes, since a member has no implicit value (spec
                // section 6).
                PropertyItem::Abstract(_) if property.hint.is_none() => " = null".to_owned(),
                PropertyItem::Abstract(_)
                    if self.class.as_ref().is_some_and(|class| {
                        class.set_by_constructor.iter().any(|set| set.as_slice() == variable.name)
                    }) =>
                {
                    String::new()
                }
                // PHP leaves a typed property unset until code outside the class sets it, where PHP# sets every
                // property before the object is used.
                PropertyItem::Abstract(_) => {
                    let name = String::from_utf8_lossy(variable.name);
                    self.refuse(
                        variable.span,
                        format!(
                            "`{name}` has no initial value, and the constructor doesn't set it: a PHP# property is set before the object is used (spec section 6)"
                        ),
                    );
                    continue;
                }
            };
            let name = Self::variable_name(variable.name);
            // A method the class declares is refused where it is declared, and one it inherits clashes here.
            let method = ascii_lowercase_word(&variable.name[1..]);
            if metadata.appearing_method_ids.contains_key(&method) && !metadata.methods.contains(&method) {
                self.refuse(
                    variable.span,
                    format!("`{name}` names a property and a method, and a PHP# class has one member of each name (spec section 14)"),
                );
                continue;
            }
            // A property that redeclares its parent's, as one that changes its initial value does, replaces it, which
            // only a property with accessors marked `override` does: a field replaces nothing (spec section 22).
            let overrides =
                metadata.overridden_property_ids.get(&word(variable.name)).is_some_and(|parents| !parents.is_empty());
            let accessors = accessors(visibility == "public" || overrides, is_readonly);
            let opening = if overrides { "override " } else { "" };
            // A property's accessors end it, unless an initial value follows them (spec section 6).
            let end = if accessors.is_empty() || !value.is_empty() { ";" } else { "" };
            declarations.push(format!("{attributes}{visibility} {opening}{written} {name}{accessors}{value}{end}"));
        }

        let indentation = self.indentation(span.start.offset);
        self.edits.replace(span, declarations.join(&format!("\n{indentation}")));
        self.docblock(span);
    }

    fn method(&mut self, method: &MethodDeclaration<'_>) {
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
        if let Some(span) = has(&method.modifiers, |modifier| matches!(modifier, Modifier::Static(_))) {
            self.refuse(span, "a static method is not converted yet");
            return;
        }
        let is_constructor = name.eq_ignore_ascii_case(b"__construct");
        if name.starts_with(b"__") && !is_constructor {
            self.refuse(method.name.span, format!("the magic method `{shown}` is not converted yet"));
            return;
        }
        if is_constructor && metadata.direct_parent_class.is_some() {
            self.refuse(
                method.name.span,
                "a subclass's constructor is not converted yet: it calls its parent's with `: super(…)` (spec section 9)",
            );
            return;
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

        self.attributes(&method.attribute_lists);
        if let Some(at) = Self::missing_visibility(&method.modifiers, function.span) {
            self.edits.insert(at, "public ");
        }

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
            let opening = if overrides {
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
        for (index, parameter) in method.parameter_list.parameters.iter().enumerate() {
            let metadata = declared.parameters.get(index).and_then(|parameter| parameter.type_metadata.as_ref());
            self.parameter(parameter, metadata.map(|metadata| metadata.type_union.clone()));
        }

        let returning = (!is_constructor).then(|| {
            declared.return_type_metadata.as_ref().map_or_else(get_mixed, |metadata| metadata.type_union.clone())
        });
        match &method.body {
            MethodBody::Concrete(block) => {
                let resolution = declared.type_resolution_context.clone().unwrap_or_default();
                self.method = Some(Method { returning, resolution, parameters, locals: Locals::default() });
                self.block(block);
                self.declare_locals();
                self.method = None;
            }
            MethodBody::Abstract(_) => {}
            MethodBody::Expression(_) => self.refuse(method.body.span(), "an expression body is PHP# only"),
        }
        self.docblock(method.span());
    }

    fn parameter(&mut self, parameter: &FunctionLikeParameter<'_>, declared: Option<TUnion>) {
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
            if let Some(span) = has(&parameter.modifiers, |modifier| modifier.is_write_visibility()) {
                self.refuse(span, "a property with a visibility of its own for `set` is not converted yet");
                return;
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
            let accessors = accessors(is_public, readonly.is_some());
            if !accessors.is_empty() {
                self.edits.insert(parameter.variable.span.end.offset, accessors);
            }
        }

        if let Some(default) = &parameter.default_value {
            self.expression(default.value, Some(&union));
        }
    }
}

/// Each property, as `$name`, that `class`'s constructor sets with `$this->name = …` in a statement of its own body,
/// which runs on every path through it.
fn set_by_constructor(class: &mago_syntax::cst::Class<'_>) -> Vec<Vec<u8>> {
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

/// Whether a docblock line starts a tag or closes the docblock, which ends the text of the tag before it.
fn is_tag_or_end(line: &str) -> bool {
    let content = line.trim().trim_start_matches('*').trim();

    content.starts_with('@') || line.trim().starts_with("*/") || line.trim().is_empty()
}
