//! Properties as fields or properties with accessors (spec section 6), and methods with their return types, `virtual`
//! and `override` (spec sections 7 and 22).

use mago_codex::ttype::TType;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_span::HasSpan;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::Method as MethodDeclaration;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::PlainProperty;
use mago_syntax::cst::PropertyItem;
use mago_word::ascii_lowercase_word;
use mago_word::word;

use crate::Converter;
use crate::Method;
use crate::body::Locals;

use super::accessors;
use super::has;

impl Converter<'_, '_> {
    /// A property, written again whole, as a field when it is private or protected storage, or a property with
    /// accessors otherwise. Two names in one declaration become one declaration each, on its own line (spec section
    /// 6).
    pub(super) fn property(&mut self, property: &PlainProperty<'_>) {
        let Some(class) = self.class.as_ref() else {
            return;
        };
        let metadata = class.metadata;
        let is_static = has(&property.modifiers, |modifier| matches!(modifier, Modifier::Static(_))).is_some();
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
                // A static holds a value of its type before any code reads it (spec section 6).
                PropertyItem::Abstract(_) if is_static => {
                    let name = String::from_utf8_lossy(variable.name);
                    self.refuse(
                        variable.span,
                        format!("`{name}` is static and has no initial value: a PHP# static holds a value of its type before any code reads it (spec section 6)"),
                    );
                    continue;
                }
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
            let accessors = accessors(visibility == "public" || overrides, is_readonly, &property.modifiers);
            let opening = match (is_static, overrides) {
                (true, true) => "static override ",
                (true, false) => "static ",
                (false, true) => "override ",
                (false, false) => "",
            };
            // A property's accessors end it, unless an initial value follows them (spec section 6).
            let end = if accessors.is_empty() || !value.is_empty() { ";" } else { "" };
            declarations.push(format!("{attributes}{visibility} {opening}{written} {name}{accessors}{value}{end}"));
        }

        let indentation = self.indentation(span.start.offset);
        self.edits.replace(span, declarations.join(&format!("\n{indentation}")));
        self.docblock(span);
    }

    /// A class constant as `public const int LIMIT = 3;`, one declaration per name, with the type its declaration
    /// writes or its value has (spec section 2). PHP's `final` goes, since a PHP# subclass declares no constant of its
    /// parent's.
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
        let mut declarations = Vec::new();
        for item in constant.items.iter() {
            let name = String::from_utf8_lossy(item.name.value).into_owned();
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
        if name.starts_with(b"__") && !is_constructor {
            self.refuse(method.name.span, format!("the magic method `{shown}` is not converted yet"));
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
                self.method =
                    Some(Method { returning, resolution, parameters, locals: Locals::default(), super_call: None });
                if is_constructor && let Some(parent) = metadata.direct_parent_class {
                    self.super_call(method, block, parent.as_bytes());
                }
                self.block(block);
                self.declare_locals();
                self.method = None;
            }
            MethodBody::Abstract(_) => {}
            MethodBody::Expression(_) => self.refuse(method.body.span(), "an expression body is PHP# only"),
        }
        self.docblock(method.span());
    }
}
