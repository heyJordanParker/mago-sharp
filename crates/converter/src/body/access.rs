//! Member and constant access: `->` and `::` become `.`, `?->` becomes `?.` (spec section 4), and `X::class` becomes
//! `typeof(X)` (spec section 25).

use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::MagicConstant;
use mago_syntax::cst::Variable;

use super::Kind;
use crate::Converter;
use crate::library::short;

impl Converter<'_, '_> {
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
                let function = self.method.as_ref().and_then(|method| method.name.as_deref());
                let function = function.map(|name| String::from_utf8_lossy(name).into_owned());
                let text = match constant {
                    MagicConstant::Namespace(_) => class.map(|class| {
                        class.rsplit_once('\\').map_or_else(String::new, |(namespace, _)| namespace.to_owned())
                    }),
                    // A class file declares no trait, and PHP gives `''` outside one.
                    MagicConstant::Trait(_) => class.map(|_| String::new()),
                    MagicConstant::Function(_) => function,
                    MagicConstant::Method(_) => class.zip(function).map(|(class, method)| format!("{class}::{method}")),
                    _ => None,
                };
                let Some(text) = text else {
                    let shown = String::from_utf8_lossy(self.text(constant.span())).into_owned();
                    let in_lambda = self.method.as_ref().is_some_and(|method| method.captures.is_some());
                    return self.refuse(
                        constant.span(),
                        if in_lambda {
                            format!("`{shown}` in a lambda gives PHP's name of the lambda, which holds the line it starts on, and the conversion moves lines")
                        } else {
                            format!("`{shown}` outside a method, or in a property accessor, is not converted yet")
                        },
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
                self.expression(access.class, None);
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
                let names_class = matches!(
                    access.class,
                    Expression::Identifier(_) | Expression::Self_(_) | Expression::Static(_) | Expression::Parent(_)
                );
                if constant.value.eq_ignore_ascii_case(b"class") && !names_class {
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
                self.expression(access.class, None);
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
}
