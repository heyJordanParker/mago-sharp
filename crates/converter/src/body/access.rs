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

use crate::Converter;

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
        if metadata.flags.is_built_in() {
            return self.refuse(
                expression.span(),
                format!("`{name}` is PHP's own constant, which is not converted yet: the standard library names it"),
            );
        }
        let Some(text) = self.through_namespace(expression.span(), &name, metadata.span) else {
            return;
        };
        self.edits.replace(expression.span(), text);
    }

    /// `__DIR__`, `__FILE__` and `__LINE__` as what `Position.current()` gives (spec section 27). PHP's other magic
    /// constants name a function, a class or a namespace PHP's way, which `Position` doesn't.
    pub(super) fn magic_constant(&mut self, constant: &MagicConstant<'_>) {
        let member = match constant {
            MagicConstant::Directory(_) => "directory",
            MagicConstant::File(_) => "file",
            MagicConstant::Line(_) => "line",
            _ => {
                let shown = String::from_utf8_lossy(self.text(constant.span())).into_owned();
                return self.refuse(
                    constant.span(),
                    format!("`{shown}` has no PHP# form: `Position` gives a file, a directory, a line, a column and a dotted function name (spec section 27)"),
                );
            }
        };
        self.edits.replace(constant.span(), format!("Position.current().{member}"));
    }

    /// `name`, a library's function or constant declared at `declared`, read through its namespace: its own, or the
    /// namespace of the library's classes beside it when it is in PHP's root namespace (spec section 23, decision 083).
    pub(super) fn through_namespace(&mut self, span: Span, name: &str, declared: Span) -> Option<String> {
        let (namespace, member) = match name.rsplit_once('\\') {
            Some((namespace, member)) => (namespace.to_owned(), member.to_owned()),
            None => match self.input.codebase.folder_namespaces(declared).first() {
                Some(namespace) => (namespace.as_str_lossy().into_owned(), name.to_owned()),
                None => {
                    self.refuse(
                        span,
                        format!("`{name}` is declared in PHP's root namespace beside no class of its library, so it has no namespace to be reached through (spec section 23)"),
                    );
                    return None;
                }
            },
        };
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
                self.untyped(access.object, "a property access");
                self.edits.replace(access.arrow, ".");
                self.member(&access.property);
            }
            Access::NullSafeProperty(access) => {
                self.untyped(access.object, "a property access");
                self.edits.replace(access.question_mark_arrow, "?.");
                self.member(&access.property);
            }
            Access::StaticProperty(access) => {
                self.expression(access.class, None);
                self.edits.replace(access.double_colon, ".");
                match &access.property {
                    Variable::Direct(variable) => self.edits.replace(variable.span, Self::variable_name(variable.name)),
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
