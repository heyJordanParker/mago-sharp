//! A checked PHP# file as the Lean declarations of the code laws reach, and of its own laws.
//!
//! The translation reads the same `CheckedProgram` the bridge lowers to the `.sharpc` file: one checked tree, two back
//! ends. It translates only what a law reaches, and records each construct Lean does not model where it stands, so a
//! law that reaches it is refused with the construct named.

mod body;
pub(crate) mod names;

use std::path::PathBuf;

use mago_database::file::FileId;
use mago_names::binding::php_method_name;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::DeclarationKind;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;

use crate::issues;
use crate::reach::Reach;
use crate::translate::body::Body;
use crate::translate::body::indented;

/// The Lean translation of one accepted `.sharp` file: its laws, and the declarations laws reach in it.
#[derive(Debug)]
pub struct Translation {
    pub(crate) file: FileId,
    /// Workspace-relative, with `/` separators.
    pub(crate) name: Vec<u8>,
    pub(crate) path: Option<PathBuf>,
    /// The Lean name of the file's first class-like, which names its generated module, and that name's span.
    pub(crate) class: Option<(String, Span)>,
    pub(crate) laws: Vec<Law>,
    pub(crate) declarations: Vec<Declaration>,
}

/// A law the file states.
#[derive(Debug, Clone)]
pub(crate) struct Law {
    /// As PHP# writes it: `addKeepsCurrency`.
    pub(crate) name: String,
    /// The Lean name of its statement: `App.Shared.Money.addKeepsCurrency`.
    pub(crate) statement: String,
    /// From `law` to the law's name, where its errors are reported.
    pub(crate) span: Span,
}

/// One Lean declaration: a structure, an inductive, a definition or a law's statement.
#[derive(Debug, Clone)]
pub(crate) struct Declaration {
    /// Its Lean name.
    pub(crate) key: String,
    pub(crate) span: Span,
    pub(crate) text: String,
    pub(crate) uses: Vec<Use>,
    pub(crate) unmodeled: Vec<Unmodeled>,
    /// Whether a proposed proof names it to `simp`: a definition other than a constructor, which is `@[simp]` already.
    pub(crate) unfolds: bool,
}

/// A declaration another one uses.
#[derive(Debug, Clone)]
pub(crate) struct Use {
    pub(crate) key: String,
    /// The file that declares it, when the analysis knows it.
    pub(crate) file: Option<FileId>,
    /// As messages name it: `Money.add`.
    pub(crate) place: String,
}

/// A construct the translation does not model.
#[derive(Debug, Clone)]
pub(crate) struct Unmodeled {
    pub(crate) span: Span,
    /// What it is: `a float`.
    pub(crate) what: String,
    /// The declaration that holds it, as messages name it: `Price.ratio`.
    pub(crate) place: String,
    pub(crate) reason: &'static str,
}

/// Translates the laws of `checked` and every declaration in it that a law of the project reaches.
#[must_use]
pub fn translate(checked: &CheckedProgram<'_>, reach: &Reach) -> Translation {
    let file = checked.file();
    let mut translator = Translator {
        checked,
        reach,
        translation: Translation {
            file: file.id,
            name: file.name.to_vec(),
            path: file.path.clone(),
            class: None,
            laws: Vec::new(),
            declarations: Vec::new(),
        },
    };

    for statement in &checked.program().statements {
        translator.statement(statement);
    }

    translator.translation
}

struct Translator<'translator, 'program> {
    checked: &'translator CheckedProgram<'program>,
    reach: &'translator Reach,
    translation: Translation,
}

impl Translator<'_, '_> {
    fn statement(&mut self, statement: &Statement) {
        match statement {
            Statement::Namespace(namespace) => {
                let statements = match &namespace.body {
                    NamespaceBody::Implicit(body) => &body.statements,
                    NamespaceBody::BraceDelimited(block) => &block.statements,
                };
                for statement in statements {
                    self.statement(statement);
                }
            }
            Statement::Class(class) => self.class(class),
            Statement::Enum(r#enum) => self.r#enum(r#enum),
            Statement::Interface(interface) => {
                let full = self.checked.names().get(&interface.name);
                self.name_module(full, interface.name.span);
            }
            _ => {}
        }
    }

    /// Names the file's generated module after its first class-like.
    fn name_module(&mut self, full: &[u8], span: Span) {
        if self.translation.class.is_none() {
            self.translation.class = Some((names::full_name(full), span));
        }
    }

    fn class(&mut self, class: &Class) {
        let full = self.checked.names().get(&class.name);
        self.name_module(full, class.name.span);
        if !self.states_laws(&class.members) && !self.reach.reaches(full, b"") {
            return;
        }

        let lean = names::full_name(full);
        let short = names::short_name(full);
        let mut body = Body::new(self.checked, self.reach, full, short.clone());
        for name in class.inheritance.iter().flat_map(|inheritance| inheritance.types.iter()) {
            let parent = self.checked.names().get(name);
            if self.checked.types().class_declaration(parent).kind == DeclarationKind::Class {
                body.unmodeled(
                    class.name.span,
                    format!("the class {short}, which extends {}", names::short_name(parent)),
                    issues::INHERITANCE,
                );
            }
        }

        let constructor = class.members.iter().find_map(|member| match member {
            ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => Some(method),
            _ => None,
        });
        let mut fields: Vec<Field> = Vec::new();
        for member in &class.members {
            match member {
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    for parameter in
                        method.parameter_list.parameters.iter().filter(|parameter| parameter.is_promoted_property())
                    {
                        fields.push(Field {
                            name: parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name),
                            hint: parameter.hint.as_ref(),
                            value: FieldValue::Parameter,
                        });
                    }
                }
                ClassLikeMember::Property(property) if is_stored(property) => {
                    let name = property.first_variable().name;
                    fields.push(Field {
                        name: name.strip_prefix(b"$").unwrap_or(name),
                        hint: property.hint(),
                        value: property.initial_value().map_or(FieldValue::Absent, FieldValue::Initial),
                    });
                }
                _ => {}
            }
        }

        let mut lines = vec![format!("structure {lean} where")];
        if fields.iter().any(|field| field.name == b"mk") {
            lines.push("  make ::".to_owned());
        }
        let mut types = Vec::new();
        for field in &fields {
            let r#type = body.declared_type(field.hint, class.name.span);
            if names::RESERVED.contains(&String::from_utf8_lossy(field.name).as_ref()) {
                body.unmodeled(
                    class.name.span,
                    format!("the property {short}.{}", String::from_utf8_lossy(field.name)),
                    issues::RESERVED_NAME,
                );
            }
            lines.push(format!("  {} : {type}", names::identifier(field.name)));
            types.push(r#type);
        }
        lines.push("  deriving Repr, DecidableEq, Inhabited".to_owned());
        self.declare(body, lean.clone(), class.name.span, lines.join("\n"), false);

        self.constructor(full, &lean, &short, class.name.span, constructor, &fields, &types);
        self.members(full, &lean, &short, &class.members);
    }

    /// `Class.__construct`, a `@[simp]` definition that builds the structure: from each promoted parameter, and from each
    /// other property's initial value. A constructor with a body of its own is not modeled.
    #[allow(clippy::too_many_arguments)]
    fn constructor(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        span: Span,
        constructor: Option<&Method>,
        fields: &[Field],
        types: &[String],
    ) {
        let key = names::member(lean, b"__construct");
        let mut body = Body::new(self.checked, self.reach, full, format!("new {short}"));
        let mut parameters = Vec::new();
        if let Some(constructor) = constructor {
            for parameter in constructor.parameter_list.parameters.iter() {
                let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());
                let name = parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name);
                parameters.push(format!("({} : {type})", names::identifier(name)));
            }
            if let MethodBody::Concrete(block) = &constructor.body
                && !block.statements.is_empty()
            {
                body.unmodeled(
                    block.span(),
                    format!("the body of the constructor of {short}"),
                    issues::CONSTRUCTOR_BODY,
                );
            }
        }

        let mut values = Vec::new();
        for (field, r#type) in fields.iter().zip(types) {
            let value = match field.value {
                FieldValue::Parameter => names::identifier(field.name),
                FieldValue::Initial(value) => body.value(value),
                FieldValue::Absent if r#type.starts_with("(Option") => "none".to_owned(),
                FieldValue::Absent => "default".to_owned(),
            };
            values.push(format!("{} := {value}", names::identifier(field.name)));
        }

        let (mut lines, uses, unmodeled) = body.finish();
        lines.push(format!("return {{ {} }}", values.join(", ")));
        let text = format!(
            "@[simp] def {key} {} : M {lean} := do\n{}",
            parameters.join(" "),
            indented(&lines, 2).collect::<Vec<_>>().join("\n")
        );
        self.translation.declarations.push(Declaration { key, span, text, uses, unmodeled, unfolds: false });
    }

    fn r#enum(&mut self, r#enum: &Enum) {
        let full = self.checked.names().get(&r#enum.name);
        self.name_module(full, r#enum.name.span);
        if !self.states_laws(&r#enum.members) && !self.reach.reaches(full, b"") {
            return;
        }

        let lean = names::full_name(full);
        let short = names::short_name(full);
        let mut body = Body::new(self.checked, self.reach, full, short.clone());
        let mut lines = vec![format!("inductive {lean} where")];
        let mut values = Vec::new();
        for member in &r#enum.members {
            let ClassLikeMember::EnumCase(case) = member else {
                continue;
            };
            let name = case.item.name();
            if names::RESERVED.contains(&String::from_utf8_lossy(name.value).as_ref()) {
                body.unmodeled(
                    name.span,
                    format!("the case {short}.{}", String::from_utf8_lossy(name.value)),
                    issues::RESERVED_NAME,
                );
            }
            lines.push(format!("  | {}", names::identifier(name.value)));
            if let EnumCaseItem::Backed(item) = &case.item {
                values.push((names::identifier(name.value), body.value(item.value)));
            }
        }
        lines.push("  deriving Repr, DecidableEq, Inhabited".to_owned());
        self.declare(body, lean.clone(), r#enum.name.span, lines.join("\n"), false);

        if let Some(backing) = &r#enum.backing_type_hint {
            let mut body = Body::new(self.checked, self.reach, full, format!("{short}.value"));
            let r#type = body.r#type(&backing.hint);
            let mut lines = vec![format!("def {} : {lean} → {type}", names::member(&lean, b"value"))];
            for (case, value) in values {
                lines.push(format!("  | .{case} => {value}"));
            }
            self.declare(body, names::member(&lean, b"value"), r#enum.name.span, lines.join("\n"), true);
        }

        self.members(full, &lean, &short, &r#enum.members);
    }

    /// The reached methods and constants, and every law, of a class-like.
    fn members(&mut self, full: &[u8], lean: &str, short: &str, members: &Sequence<'_, ClassLikeMember<'_>>) {
        for member in members {
            match member {
                ClassLikeMember::Method(method)
                    if php_method_name(method) != b"__construct" && self.reach.reaches(full, method.name.value) =>
                {
                    self.method(full, lean, short, method);
                }
                ClassLikeMember::Constant(constant) => {
                    for item in constant.items.iter().filter(|item| self.reach.reaches(full, item.name.value)) {
                        let key = names::member(lean, item.name.value);
                        let mut body = Body::new(
                            self.checked,
                            self.reach,
                            full,
                            format!("{short}.{}", String::from_utf8_lossy(item.name.value)),
                        );
                        let r#type = constant.hint.as_ref().map(|hint| body.r#type(hint));
                        let value = body.pure_value(item.value);
                        let text = match r#type {
                            Some(r#type) => format!("def {key} : {type} := {value}"),
                            None => format!("def {key} := {value}"),
                        };
                        self.declare(body, key, item.name.span, text, true);
                    }
                }
                ClassLikeMember::Law(law) => self.law(full, lean, short, law),
                _ => {}
            }
        }
    }

    fn method(&mut self, full: &[u8], lean: &str, short: &str, method: &Method) {
        let key = names::member(lean, method.name.value);
        let place = format!("{short}.{}", String::from_utf8_lossy(method.name.value));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        if names::RESERVED.contains(&String::from_utf8_lossy(method.name.value).as_ref()) {
            body.unmodeled(method.name.span, format!("the method {place}"), issues::RESERVED_NAME);
        }
        if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) {
            body.unmodeled(method.name.span, format!("the native method {place}"), issues::EXTERN);
        }

        let mut parameters = Vec::new();
        if !method.is_static() {
            parameters.push(format!("(this : {lean})"));
        }
        for parameter in method.parameter_list.parameters.iter() {
            let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());
            if matches!(parameter.hint, Some(Hint::Nullable(_))) {
                body.nullable_parameter(parameter.variable.span.start.offset);
            }
            let name = parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name);
            parameters.push(format!("({} : {type})", names::identifier(name)));
        }
        let void = method.return_type_hint.as_ref().is_none_or(|hint| matches!(hint.hint, Hint::Void(_)));
        let returns =
            method.return_type_hint.as_ref().map_or_else(|| "Unit".to_owned(), |hint| body.r#type(&hint.hint));

        let (lines, uses, unmodeled) = match &method.body {
            MethodBody::Concrete(block) => body.block_body(block, void),
            MethodBody::Expression(expression) => body.expression_body(expression.expression),
            MethodBody::Abstract(abstract_body) => {
                body.unmodeled(abstract_body.span(), format!("the abstract method {place}"), issues::OVERRIDABLE);
                body.finish()
            }
        };
        let text = format!(
            "def {key} {} : M {returns} := do\n{}",
            parameters.join(" "),
            indented(&lines, 2).collect::<Vec<_>>().join("\n")
        );
        self.translation.declarations.push(Declaration {
            key,
            span: method.name.span,
            text,
            uses,
            unmodeled,
            unfolds: true,
        });
    }

    /// `@[law] def Class.law : Prop := ∀ (a : A), holds do …`: the law holds unless its body finishes with `false`.
    fn law(&mut self, full: &[u8], lean: &str, short: &str, law: &LawNode) {
        let name = String::from_utf8_lossy(law.name.value).into_owned();
        let key = names::member(lean, law.name.value);
        let mut body = Body::new(self.checked, self.reach, full, format!("{short}.{name}"));
        let mut binders = Vec::new();
        for parameter in law.parameter_list.parameters.iter() {
            let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());
            if matches!(parameter.hint, Some(Hint::Nullable(_))) {
                body.nullable_parameter(parameter.variable.span.start.offset);
            }
            let parameter = parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name);
            binders.push(format!("({} : {type})", names::identifier(parameter)));
        }

        let (lines, uses, unmodeled) = body.expression_body(law.body.expression);
        let statement =
            if binders.is_empty() { "holds do".to_owned() } else { format!("∀ {}, holds do", binders.join(" ")) };
        let text = format!(
            "@[law] def {key} : Prop :=\n  {statement}\n{}",
            indented(&lines, 4).collect::<Vec<_>>().join("\n")
        );
        let span = Span::between(law.law.span(), law.name.span);
        self.translation.laws.push(Law { name, statement: key.clone(), span });
        self.translation.declarations.push(Declaration { key, span, text, uses, unmodeled, unfolds: true });
    }

    fn states_laws(&self, members: &Sequence<'_, ClassLikeMember<'_>>) -> bool {
        members.iter().any(|member| matches!(member, ClassLikeMember::Law(_)))
    }

    fn declare(&mut self, body: Body<'_, '_>, key: String, span: Span, text: String, unfolds: bool) {
        let (_, uses, unmodeled) = body.finish();
        self.translation.declarations.push(Declaration { key, span, text, uses, unmodeled, unfolds });
    }
}

/// A property of a class's structure, and where its value comes from when the constructor builds it.
struct Field<'field, 'arena> {
    name: &'field [u8],
    hint: Option<&'field Hint<'arena>>,
    value: FieldValue<'field, 'arena>,
}

enum FieldValue<'field, 'arena> {
    Parameter,
    Initial(&'field mago_syntax::cst::Expression<'arena>),
    Absent,
}

/// Whether `property` holds a value: it is not static and has no accessor body.
fn is_stored(property: &Property) -> bool {
    if property.modifiers().iter().any(|modifier| matches!(modifier, Modifier::Static(_))) {
        return false;
    }

    match property {
        Property::Plain(_) => true,
        Property::Hooked(hooked) => {
            hooked.hook_list.hooks.iter().all(|hook| matches!(hook.body, PropertyHookBody::Abstract(_)))
        }
        Property::Computed(_) => false,
    }
}
