//! Member and constant access: `->` and `::` become `.`, `?->` becomes `?.` (spec section 4), `X::class` becomes
//! `typeof(X)`, and a static is reached through its class's name or a type parameter (spec sections 11 and 25).

use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::TType;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::get_mixed;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MagicConstant;
use mago_syntax::cst::StaticPropertyAccess;
use mago_syntax::cst::Variable;

use super::Kind;
use super::builtins::runs_nothing;
use super::is_primary;
use super::natively_checked;
use crate::Converter;
use crate::library::short;

impl<'input> Converter<'input, '_> {
    /// A constant: a library's, through its namespace as its functions are (spec section 23), or PHP's own, which the
    /// standard library names.
    pub(super) fn constant(&mut self, expression: &Expression<'_>, constant: &ConstantAccess<'_>) {
        let codebase = self.input.codebase;
        let written = constant.name.value();
        let Some(metadata) = codebase
            .get_constant(self.input.names.get(&constant.name))
            .or_else(|| (!written.contains(&b'\\')).then(|| codebase.get_constant(written)).flatten())
        else {
            return self.refuse(expression.span(), "the analysis holds no constant of this name");
        };
        let name = metadata.name.as_str_lossy().into_owned();
        // PHP# code reads PHP's own constant through the standard library constant whose value it is, as `Int.MAX`
        // reads `PHP_INT_MAX` (spec section 23).
        if metadata.flags.is_built_in() {
            let Some((class, member)) = self.input.library.constant(metadata.name.as_bytes()) else {
                return self.refuse(
                    expression.span(),
                    format!("`{name}` has no PHP# form: no standard library member reads it (decision 083)"),
                );
            };
            return match self.imports.name_of(&class.as_str_lossy()) {
                Ok(class) => self.edits.replace(expression.span(), format!("{class}.{member}")),
                Err(reason) => self.refuse(expression.span(), reason),
            };
        }
        let Some(text) = self.through_namespace(expression.span(), &name, metadata.span) else {
            return;
        };
        self.edits.replace(expression.span(), text);
    }

    /// `__DIR__` and `__FILE__` as what `Position.current()` gives (spec section 27), and `__CLASS__` as `self::class`
    /// is, `typeof` of the class. `__METHOD__`, `__FUNCTION__`, `__NAMESPACE__` and `__TRAIT__` as the text PHP compiles
    /// in their place, which the file's own declarations fix and the conversion keeps. `__LINE__` gives the line of the
    /// PHP file, which the conversion moves, and so does the name PHP gives a lambda, which `__FUNCTION__` and
    /// `__METHOD__` give in one.
    pub(super) fn magic_constant(&mut self, constant: &MagicConstant<'_>) {
        let member = match constant {
            MagicConstant::Directory(_) => "directory",
            MagicConstant::File(_) => "file",
            MagicConstant::Line(_) => {
                return self.refuse(
                    constant.span(),
                    "`__LINE__` gives the line of the PHP file, and the conversion moves lines, so `Position.current().line` would give another one",
                );
            }
            MagicConstant::Class(_) if let Some(class) = self.class.as_ref() => {
                let class = format!("typeof({})", class.name);
                return self.edits.replace(constant.span(), class);
            }
            _ => {
                let class = self.class.as_ref().map(|class| class.metadata.original_name.as_str_lossy().into_owned());
                // PHP names a method by its name, a property's accessor as `$total::get`, and nothing outside a
                // function, as in a property's default.
                let function = match &self.method {
                    None => Some(String::new()),
                    Some(method) => match (&method.name, &method.storage, &method.captures) {
                        (Some(name), _, _) => Some(String::from_utf8_lossy(name).into_owned()),
                        (None, Some(property), None) => Some(format!(
                            "${}::{}",
                            String::from_utf8_lossy(property),
                            if method.returning.is_some() { "get" } else { "set" }
                        )),
                        _ => None,
                    },
                };
                let outside = self.method.is_none();
                let text = match constant {
                    MagicConstant::Namespace(_) => class.map(|class| {
                        class.rsplit_once('\\').map_or_else(String::new, |(namespace, _)| namespace.to_owned())
                    }),
                    // A class file declares no trait, and PHP gives `''` outside one.
                    MagicConstant::Trait(_) => class.map(|_| String::new()),
                    MagicConstant::Function(_) => function,
                    MagicConstant::Method(_) if outside => Some(String::new()),
                    MagicConstant::Method(_) => class.zip(function).map(|(class, method)| format!("{class}::{method}")),
                    _ => None,
                };
                let Some(text) = text else {
                    let shown = String::from_utf8_lossy(self.text(constant.span())).into_owned();
                    return self.refuse(
                        constant.span(),
                        format!("`{shown}` in a lambda gives PHP's name of the lambda, which holds the line it starts on, and the conversion moves lines"),
                    );
                };
                return self.edits.replace(constant.span(), format!("\"{}\"", text.replace('\\', "\\\\")));
            }
        };
        self.edits.replace(constant.span(), format!("Position.current().{member}"));
    }

    /// `name`, a library's function or constant declared at `declared`, read through its namespace: its own, or, in
    /// PHP's root namespace, the one an `extern namespace` line gives its folder, which the conversion adds when the
    /// project has none (spec section 23).
    pub(super) fn through_namespace(&mut self, span: Span, name: &str, declared: Span) -> Option<String> {
        let (namespace, member) = match name.rsplit_once('\\') {
            Some((namespace, member)) => (namespace.to_owned(), member.to_owned()),
            None => match self.imports.root_namespace(name, declared) {
                Ok(namespace) => (namespace, name.to_owned()),
                Err(reason) => {
                    self.refuse(span, reason);
                    return None;
                }
            },
        };
        // `Text.str` names the class `Text\Str` where one exists, since a class's name has no case (spec section 23).
        if let Some(class) = self.input.codebase.get_class_like(format!("{namespace}\\{member}").as_bytes()) {
            let class = class.original_name.as_str_lossy().into_owned();
            self.refuse(
                span,
                format!("`{name}` is reached through its namespace as `{}.{member}`, which names the class `{class}`, since PHP finds a class in any case (spec section 23)", short(&namespace)),
            );
            return None;
        }
        let alias = self.imports.import(&namespace);

        Some(format!("{alias}.{member}"))
    }

    pub(super) fn access(&mut self, expression: &Expression<'_>, access: &Access<'_>) {
        match access {
            // PHP's `$this->name` in the accessor of `name` is its storage, PHP#'s `field` (spec section 6).
            Access::Property(access)
                if matches!(access.object, Expression::Variable(Variable::Direct(this)) if this.name == b"$this")
                    && matches!(&access.property, ClassLikeMemberSelector::Identifier(name)
                        if self.method.as_ref().and_then(|method| method.storage.as_deref()) == Some(name.value)) =>
            {
                self.edits.replace(expression.span(), "field");
            }
            Access::Property(access) => {
                self.member_of(access.object, "a property access");
                self.edits.replace(access.arrow, ".");
                self.member(&access.property);
            }
            Access::NullSafeProperty(access) => {
                self.member_of(access.object, "a property access");
                self.edits.replace(access.question_mark_arrow, "?.");
                self.member(&access.property);
            }
            Access::StaticProperty(access) => {
                if !self.static_class(access.class) {
                    return self.held_static_read(expression, access);
                }
                self.edits.replace(access.double_colon, ".");
                match &access.property {
                    // The name its class declares, which no renamed local of the method changes.
                    Variable::Direct(variable) => {
                        let name = String::from_utf8_lossy(&variable.name[1..]).into_owned();
                        self.edits.replace(variable.span, name);
                    }
                    _ => self.refuse(
                        access.property.span(),
                        "a property named by a variable has no PHP# form (spec section 25)",
                    ),
                }
            }
            Access::ClassConstant(access) => {
                let ClassLikeConstantSelector::Identifier(constant) = &access.constant else {
                    return self.refuse(expression.span(), "a constant named by an expression has no PHP# form");
                };
                if constant.value.eq_ignore_ascii_case(b"class") && !names_class(access.class) {
                    // `$order::class` is the class of an object, `Class.of(order)`, as `typeof` takes a type only
                    // (spec section 25). PHP's on `null` throws, and PHP#'s takes an object.
                    if !self.is(access.class, &[Kind::Object, Kind::Enum]) {
                        let shown = self.shown(access.class);
                        return self.refuse(
                            expression.span(),
                            format!("`::class` of `{shown}` has no PHP# form: `Class.of` gives the class of an object, and PHP's on `null` throws (spec section 25)"),
                        );
                    }
                    self.untyped(access.class, "`Class.of`");
                    let object = self.render(access.class);
                    return self.edits.replace(expression.span(), format!("Class.of({object})"));
                }
                if !self.static_class(access.class) {
                    return self.refuse(expression.span(), self.held_static(expression));
                }
                if constant.value.eq_ignore_ascii_case(b"class") {
                    // `X::class` is the class value `typeof(X)` (spec section 25).
                    let class = self.render(access.class);
                    self.edits.replace(expression.span(), format!("typeof({class})"));
                } else {
                    self.edits.replace(access.double_colon, ".");
                }
            }
        }
    }

    pub(super) fn member(&mut self, selector: &ClassLikeMemberSelector<'_>) {
        if !matches!(selector, ClassLikeMemberSelector::Identifier(_)) {
            self.refuse(selector.span(), "a member named by a variable has no PHP# form (spec section 25)");
        }
    }

    /// Writes `class`, which a static member is reached through, as PHP# reaches one, and gives whether it could: a
    /// class's name, `self`, `static` and `parent` as they convert, and a value of `class-string<T>` as `T`, as
    /// `T.find(7)`, which reaches the statics of the type argument the code runs with (spec section 11). PHP# reaches a
    /// static through no other class held at run time.
    pub(super) fn static_class(&mut self, class: &Expression<'_>) -> bool {
        if names_class(class) {
            self.expression(class, None);
            return true;
        }
        let Some(parameter) = self.class_parameter(class) else {
            return false;
        };
        self.edits.replace(class.span(), parameter);

        true
    }

    /// The name of `T`, a type parameter of the converted class or method, where `class` is a value of
    /// `class-string<T>` whose read runs nothing, so `T` names the class it holds.
    fn class_parameter(&self, class: &Expression<'_>) -> Option<String> {
        if !runs_nothing(class) {
            return None;
        }
        let [TAtomic::Scalar(TScalar::ClassLikeString(held))] = self.type_of(class)?.types.as_ref() else {
            return None;
        };
        let TAtomic::GenericParameter(parameter) = held.get_object_type(self.input.codebase) else {
            return None;
        };

        self.declares(&parameter).then(|| parameter.parameter_name.as_str_lossy().into_owned())
    }

    /// Whether `place` is a static property reached through a class held at run time, as `$kind::$count`, which PHP#
    /// reads and writes through reflection.
    pub(super) fn held_static_place(&self, place: &Expression<'_>) -> bool {
        matches!(place, Expression::Access(Access::StaticProperty(access))
            if !names_class(access.class) && self.class_parameter(access.class).is_none())
    }

    /// The reflection of the static property `access` names, through the class value it is reached through, as
    /// `kind.properties().first(property => property.name == "count")`, which finds the property of the class the
    /// value holds, as PHP's `$kind::$count` does (spec section 25), with the property's declaration. A refusal at
    /// `span` where the value may hold something other than a class value, or the analysis finds no such property.
    fn held_property(
        &mut self,
        span: Span,
        access: &StaticPropertyAccess<'_>,
    ) -> Option<(String, &'input PropertyMetadata)> {
        let Variable::Direct(variable) = &access.property else {
            self.refuse(access.property.span(), "a property named by a variable has no PHP# form (spec section 25)");
            return None;
        };
        let shown = String::from_utf8_lossy(self.text(span)).into_owned();
        if !self.is(access.class, &[Kind::Class]) || !runs_nothing(access.class) {
            let held = self.shown(access.class);
            self.refuse(
                span,
                format!("`{shown}` reaches a static through `{held}`, which holds no class value a second read gives again, and has no PHP# form: {HELD_CLASS}"),
            );
            return None;
        }
        let codebase = self.input.codebase;
        let Some(declared) = self
            .held_class(access.class)
            .and_then(|class| codebase.get_declaring_property(class.as_bytes(), variable.name))
        else {
            let held = self.shown(access.class);
            self.refuse(
                span,
                format!("`{shown}` reaches a static through a class held at run time, which PHP# reads through reflection, and the analysis finds no static property `{}` of `{held}` to type it by (spec section 25)", String::from_utf8_lossy(variable.name)),
            );
            return None;
        };
        self.expression(access.class, None);
        let class = self.render(access.class);
        let class = if is_primary(access.class) { class } else { format!("({class})") };
        let found = self.unused_name("property", &[]);
        let name = String::from_utf8_lossy(&variable.name[1..]);

        Some((format!("{class}.properties().first({found} => {found}.name == \"{name}\")"), declared))
    }

    /// `$kind::$count`, a static property of a class held at run time, as the read through reflection PHP# makes it
    /// with, `….get(null)`. `get` gives `Any?`, which [`Converter::checked`] takes into the type the analysis gives the
    /// read (spec section 21).
    fn held_static_read(&mut self, expression: &Expression<'_>, access: &StaticPropertyAccess<'_>) {
        let Some((property, declared)) = self.held_property(expression.span(), access) else {
            return;
        };
        self.edits.replace(expression.span(), format!("{property}.get(null)"));
        let Some(read) = self.type_of(expression).filter(|read| !read.has_mixed()) else {
            return;
        };
        let natively = natively_checked(declared.type_declaration_metadata.as_ref(), read, self.input.codebase);
        if self.checked(expression, &get_mixed(), read, "Value", natively) {
            // `as` and `??` bind looser than the operators around a read.
            let checked = self.render(expression);
            self.edits.replace(expression.span(), format!("({checked})"));
        } else {
            let shown = String::from_utf8_lossy(self.text(expression.span())).into_owned();
            self.refuse(
                expression.span(),
                format!("`{shown}` reaches a static through a class held at run time, which PHP# reads through reflection, whose `get` gives `Any?`, and no PHP# test gives it the `{}` the property holds (spec section 21)", read.get_id()),
            );
        }
    }

    /// `$kind::$count = $value;`, a write of a static property of a class held at run time, as the statement PHP#
    /// writes it through reflection with, `….set(null, value)`, which checks the value against the property's type,
    /// as PHP's write does (spec section 25).
    pub(super) fn held_static_write(&mut self, statement: &Expression<'_>, lhs: &Expression<'_>, rhs: &Expression<'_>) {
        let Expression::Access(Access::StaticProperty(access)) = lhs else {
            return;
        };
        let Some((property, declared)) = self.held_property(lhs.span(), access) else {
            return;
        };
        let place = declared.type_metadata.as_ref().map(|metadata| metadata.type_union.clone());
        self.expression(rhs, place.as_ref());
        let value = self.render(rhs);
        self.edits.replace(statement.span(), format!("{property}.set(null, {value})"));
    }

    /// Why `access`, a static property or constant reached through a class held at run time, as `$class::$count`, has
    /// no PHP# form.
    fn held_static(&self, access: &Expression<'_>) -> String {
        let shown = String::from_utf8_lossy(self.text(access.span()));

        format!("`{shown}` reaches a static through a class held at run time, which has no PHP# form: {HELD_CLASS}")
    }
}

/// Why a static reached through a class held at run time has no PHP# form but a call through reflection.
pub(super) const HELD_CLASS: &str = "PHP# reaches a static through its class's name or a type parameter, and calls a static method of a class held at run time through reflection (spec sections 11 and 25)";

/// Whether `class`, which a static member is reached through, names one: a class's name, `self`, `static` or `parent`.
pub(super) fn names_class(class: &Expression<'_>) -> bool {
    matches!(class, Expression::Identifier(_) | Expression::Self_(_) | Expression::Static(_) | Expression::Parent(_))
}
