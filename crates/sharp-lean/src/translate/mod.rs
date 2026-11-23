//! A checked PHP# file as the Lean declarations of the code laws reach and of its own laws.
//!
//! The translation reads the same `CheckedProgram` the bridge lowers to the `.sharpc` file: one checked tree, two back
//! ends. It translates only what a law reaches, and records each construct Lean does not model where it stands, so a
//! law that reaches it is refused with the construct named.

mod body;
mod loops;
mod machine;
pub(crate) mod names;
mod pattern;
pub(crate) mod proof;
mod search;

use std::borrow::Cow;

use mago_analyzer::smaller::Bound;
use mago_analyzer::smaller::Smaller;
use mago_analyzer::smaller::Step;
use mago_codex::scanner::get_union_from_hint;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_database::file::FileId;
use mago_names::binding::php_method_name;
use mago_names::binding::php_operator_name;
use mago_sharp_bridge::CheckedProgram;
use mago_sharp_bridge::DeclarationKind;
use mago_span::HasSpan;
use mago_span::Span;

use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extension;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Law as LawNode;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Operator;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use crate::issues;
use crate::reach::Reach;
use crate::translate::body::Body;
use crate::translate::body::indented;
use crate::translate::machine::case_class;
use crate::translate::proof::Fact;
use crate::translate::proof::Proof;
use crate::translate::proof::Prover;

/// The Lean translation of one accepted `.sharp` file: its laws and the declarations laws reach in it.
#[derive(Debug)]
pub struct Translation {
    pub(crate) file: FileId,
    /// Workspace-relative, with `/` separators.
    pub(crate) name: Vec<u8>,
    /// The Lean name of the file's first class-like, which names its generated module, and that name's span.
    pub(crate) class: Option<(String, Span)>,
    pub(crate) laws: Vec<Law>,
    pub(crate) declarations: Vec<Declaration>,
    /// Each member PHP# calls on a value, by its Lean name, and whether it is a property: `m.add(x)` and `m.total`. A goal
    /// names any other member on its class, as `Money.refundable(a, b)`.
    pub(crate) receivers: Vec<(String, bool)>,
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
    /// Its theorem, `<statement>.proof`.
    pub(crate) proof: Proof,
    /// The search for the small values that break it, spec section 28, which the runner elaborates and runs only when
    /// its proof stays open. None for a law with no search.
    pub(crate) breaking: Option<Breaking>,
    /// For a PHP# implementer's proof of an interface's law, spec sections 22 and 28, what an open proof reports on the
    /// implementer: "`MemoryLedger.credit` breaks `Ledger.creditAdds`.". None for any other law.
    pub(crate) broken: Option<String>,
}

/// A law's search for the small values that break it, spec section 28: the Lean text of `<statement>.«$claim»` and
/// `<statement>.«$breaking»`, which no generated module holds, so a law whose proof closes never elaborates it, and
/// what that text uses and what of it Lean cannot model.
#[derive(Debug, Clone)]
pub(crate) struct Breaking {
    pub(crate) text: String,
    pub(crate) uses: Vec<Use>,
    pub(crate) unmodeled: Vec<Unmodeled>,
}

/// A law's statement and what its proof needs of it.
struct LawStatement {
    text: String,
    uses: Vec<Use>,
    unmodeled: Vec<Unmodeled>,
    /// Each parameter as the binder `(name : Type)`.
    binders: Vec<String>,
    /// Each parameter's name.
    parameters: Vec<String>,
    /// Whether it states that each value it takes keeps its class-like's precondition.
    preconditions: bool,
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
    pub(crate) role: Role,
    /// What a method makes smaller on each call it makes of itself or of a method that calls it back.
    pub(crate) measure: Option<Measure>,
    /// A type's Lean text where its own text has a problem and laws only name it: the values of an interface of its
    /// name, of which Lean knows nothing, then its coercions.
    pub(crate) opaque: Option<String>,
}

/// What a declaration is in Lean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    /// A structure or an inductive: a cycle of them is a type that holds itself.
    Type,
    /// A class's `__construct`, which builds its structure: a cycle of them builds a type that holds itself.
    Constructor,
    /// A method, a law's statement or a parameter's default, which computes: a cycle of them is a recursion.
    Definition,
}

/// A declaration another one uses.
#[derive(Debug, Clone)]
pub(crate) struct Use {
    pub(crate) key: String,
    /// The file that declares it, when the analysis knows it.
    pub(crate) file: Option<FileId>,
    /// As messages name it: `Money.add`.
    pub(crate) place: String,
    /// The smaller value the use passes, when it is a call of a method of the same class.
    pub(crate) smaller: Option<Smaller>,
    /// Whether it only names a type, as a parameter's or a field's declared type does, and reads nothing inside the
    /// type's values, so the type's own problems are not the user's.
    pub(crate) names_only: bool,
}

/// The parameter a recursive method's calls each make smaller, by the same step, and the `termination_by` that tells
/// Lean.
#[derive(Debug, Clone)]
pub(crate) struct Measure {
    pub(crate) parameter: usize,
    pub(crate) step: Step,
    pub(crate) termination: String,
}

impl Measure {
    /// The measure of a method whose parameters Lean names `parameters`, from the calls among `uses` that pass a
    /// smaller value, when they all make the same parameter smaller by the same step.
    fn of(uses: &[Use], parameters: &[String]) -> Option<Measure> {
        let mut calls = uses.iter().filter_map(|used| used.smaller);
        let first = calls.next()?;
        if !calls.all(|call| call.parameter == first.parameter && call.step == first.step) {
            return None;
        }

        Some(Self::new(first.parameter, first.step, parameters))
    }

    /// The measure of a method or a law whose parameters Lean names `parameters`, whose calls of itself each make the
    /// parameter at `place` smaller by `step`.
    pub(crate) fn new(place: usize, step: Step, parameters: &[String]) -> Measure {
        let parameter = &parameters[place];
        let bound = |bound: Bound| match bound {
            Bound::Literal(value) => format!("({value} : int).toInt"),
            Bound::Parameter(index) => format!("{}.toInt", parameters[index]),
        };
        let tested = |bound: Bound| match bound {
            Bound::Literal(_) => String::new(),
            Bound::Parameter(index) => format!(" {}", parameters[index]),
        };
        let (measure, smaller) = match step {
            Step::Rest => (format!("{parameter}.length"), "smaller_value".to_owned()),
            Step::Down(below) => {
                (format!("({parameter}.toInt - {}).toNat", bound(below)), format!("smaller_down{}", tested(below)))
            }
            Step::Up(above) => {
                (format!("({} - {parameter}.toInt).toNat", bound(above)), format!("smaller_up{}", tested(above)))
            }
        };

        Measure {
            parameter: place,
            step,
            termination: format!("termination_by {measure}\ndecreasing_by all_goals {smaller}"),
        }
    }
}

/// A construct the translation does not model.
#[derive(Debug, Clone)]
pub(crate) struct Unmodeled {
    pub(crate) span: Span,
    /// What it is: `a float`.
    pub(crate) what: String,
    /// The declaration that holds it, as messages name it: `Price.ratio`.
    pub(crate) place: String,
    pub(crate) reason: Cow<'static, str>,
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
            class: None,
            laws: Vec::new(),
            declarations: Vec::new(),
            receivers: Vec::new(),
        },
    };

    for statement in &checked.program().statements {
        translator.statement(statement);
    }

    translator.translation
}

/// The instances each translated class, enum and sealed interface derives for the translated code and the library's
/// lemmas. No `Repr`, which only prints a value, and no proof prints one.
const DERIVED: &str = "  deriving DecidableEq, Inhabited";

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
            Statement::Class(class) => self.class(&class.name, class.inheritance.as_ref(), &class.members, false),
            Statement::Struct(r#struct) => {
                self.class(&r#struct.name, r#struct.inheritance.as_ref(), &r#struct.members, true);
            }
            Statement::Enum(r#enum) => self.r#enum(r#enum),
            Statement::Interface(interface) => self.interface(interface),
            Statement::Module(module) => self.module(module),
            _ => {}
        }
    }

    /// A module's laws, named by its class-like `<Namespace>.Module`, as a class's laws are named by the class. Only a
    /// folder's base module states laws, so a module that overrides another has none to translate.
    fn module(&mut self, module: &Module) {
        if module.parent.is_some() {
            return;
        }

        let full = self.checked.names().get(&module.module);
        self.name_module(full, module.module.span);

        let lean = names::full_name(full);
        let short = names::short_name(full);
        for member in &module.members {
            if let ModuleMember::Law(law) = member {
                self.law(full, &lean, &short, law);
            }
        }
    }

    /// Names the file's generated module after its first class-like.
    fn name_module(&mut self, full: &[u8], span: Span) {
        if self.translation.class.is_none() {
            self.translation.class = Some((names::full_name(full), span));
        }
    }

    /// A class, or a struct, which is the `final` class it lowers to, spec section 10, with the `==` it gets.
    fn class(
        &mut self,
        name: &LocalIdentifier,
        inheritance: Option<&Inheritance>,
        members: &Sequence<'_, ClassLikeMember<'_>>,
        r#struct: bool,
    ) {
        let full = self.checked.names().get(name);
        self.name_module(full, name.span);
        if !self.states_laws(members) && !self.reach.reaches(full, b"") {
            return;
        }

        let lean = names::full_name(full);
        let short = names::short_name(full);
        let mut body = Body::new(self.checked, self.reach, full, short.clone());
        // Spec section 28: the structure holds the fields of the PHP# class the class extends, which holds those of its
        // own PHP# parent. A plain PHP parent's state is no field: a law carries the value and reads none of it.
        let parent = inheritance
            .iter()
            .flat_map(|inheritance| inheritance.names())
            .map(|parent| self.checked.names().get(&parent))
            .find(|parent| self.checked.types().class_declaration(word(parent)).kind == DeclarationKind::Class)
            .filter(|parent| self.reach.class(parent).is_some_and(|reached| reached.sharp));

        let constructor = members.iter().find_map(|member| match member {
            ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => Some(method),
            _ => None,
        });
        let mut fields: Vec<Field> = Vec::new();
        for member in members {
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
                ClassLikeMember::Property(property) => {
                    let name = property.first_variable().name;
                    let name = name.strip_prefix(b"$").unwrap_or(name);
                    if self.reach.is_stored_field(full, name) {
                        fields.push(Field {
                            name,
                            hint: property.hint(),
                            value: property.initial_value().map_or(FieldValue::Absent, FieldValue::Initial),
                        });
                    }
                }
                _ => {}
            }
        }

        let parent_field = parent.map(|parent| (body.parent_field(parent), body.own_type(name.span, parent)));
        let extends = parent_field.as_ref().map(|(_, r#type)| format!(" extends {type}")).unwrap_or_default();
        let mut lines = vec![format!("protected structure {lean}{extends} where")];
        if fields.iter().any(|field| field.name == b"mk") {
            lines.push("  make ::".to_owned());
        }
        let mut types = Vec::new();
        for field in &fields {
            let r#type = body.declared_type(field.hint, name.span);
            if names::RESERVED.contains(&String::from_utf8_lossy(field.name).as_ref()) {
                body.unmodeled(
                    name.span,
                    format!("the property {short}.{}", String::from_utf8_lossy(field.name)),
                    issues::RESERVED_NAME,
                );
            }
            lines.push(format!("  protected {} : {type}", names::identifier(field.name)));
            types.push(r#type);
        }
        lines.push(DERIVED.to_owned());
        let coercions = self.coercions(&mut body, &lean, self.own_parents(full, inheritance));
        self.declare_type(body, lean.clone(), name.span, lines, coercions);

        // Spec section 20.1: an implementer of a sealed interface is a value of the interface, which reads each of its
        // properties through the interface's accessor, and each property a PHP# parent declares through the accessor of
        // the parent's value. Each other value of the interface gives the property's default, which no read sees, since
        // the checker allows the read only where the value is the implementer.
        let own =
            fields.iter().zip(&types).map(|(field, r#type)| (String::from_utf8_lossy(field.name), r#type.clone()));
        for (field, r#type) in parent_field.map(|(field, r#type)| (field.into(), r#type)).into_iter().chain(own) {
            let mut body = Body::new(self.checked, self.reach, full, format!("{short}.{field}"));
            let Some((interface, constructor)) = body.sealed(name.span, full) else {
                break;
            };
            let key = names::member(&constructor, field.as_bytes());
            body.reads(full);
            let mut lines = vec![
                format!("protected def {key} : {interface} → {type}"),
                format!("  | {constructor} held => held.{}", names::identifier(field.as_bytes())),
            ];
            lines.extend(default_arms(&body, full, &body.values(full, Some(&constructor))));
            self.translation.receivers.push((key.clone(), true));
            self.declare(body, key, name.span, lines.join("\n"), Role::Definition);
        }

        self.constructor(full, &lean, &short, (name.span, parent), constructor, &fields, &types);
        self.valid(full, &lean, &short, name.span, constructor, &fields);
        self.members(full, &lean, &short, members.as_slice());
        if r#struct {
            self.value_equality(full, &lean, &short, name.span, members, |body| {
                fields_equality(body, full, name.span, &fields)
            });
        }
    }

    /// The `==` a struct, or an enum whose cases run as classes, gets when it declares none, spec section 19, as its
    /// `op_Equality` of `a` and `b`, the terms `equal` gives joined by `&&`. A struct compares each field with that
    /// field's own `==`, as [`fields_equality`] gives it, and an enum compares the case and its fields, as Lean's `==`
    /// on its inductive type does. One that declares `operator ==` runs its own, as [`Translator::operator`] gives it.
    fn value_equality(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        span: Span,
        members: &Sequence<'_, ClassLikeMember<'_>>,
        equal: impl FnOnce(&mut Body<'_, '_>) -> Vec<String>,
    ) {
        let Some(name) = php_operator_name(&BinaryOperator::Equal(span), 2) else {
            unreachable!("`==` runs as a static method of its class");
        };
        let declared = members.iter().any(|member| {
            matches!(member, ClassLikeMember::Operator(operator) if matches!(operator.symbol, BinaryOperator::Equal(_)))
        });
        if declared || !self.reach.reaches(full, name) {
            return;
        }

        let mut body = Body::new(self.checked, self.reach, full, format!("{short}.operator =="));
        let r#type = body.class_type(span, full);
        let equal = equal(&mut body);
        let (mut lines, uses, unmodeled) = body.finish();
        lines.push(format!("return {}", if equal.is_empty() { "true".to_owned() } else { equal.join(" && ") }));

        self.define(
            names::member(lean, name),
            span,
            &[format!("(a b : {type})")],
            "Bool",
            (lines, uses, unmodeled),
            None,
        );
    }

    /// `Class.__construct`, a `@[simp]` definition that builds the structure: the PHP# `parent`'s value from the
    /// constructor `: super(…)` calls, each promoted parameter, and each other property's initial value. A body that
    /// only checks the arguments and throws runs first, so its checks are a precondition of the value, spec section 28,
    /// and any other body is not modeled.
    #[allow(clippy::too_many_arguments)]
    fn constructor(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        (span, parent): (Span, Option<&[u8]>),
        constructor: Option<&Method>,
        fields: &[Field],
        types: &[String],
    ) {
        let key = names::member(lean, b"__construct");
        let mut body = Body::new(self.checked, self.reach, full, format!("new {short}"));
        let mut parameters = Vec::new();
        if let Some(constructor) = constructor {
            self.defaults(full, &key, &format!("new {short}"), &constructor.parameter_list);
            for parameter in constructor.parameter_list.parameters.iter() {
                let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());
                let name = parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name);
                parameters.push(format!("({} : {type})", names::identifier(name)));
            }
            if let MethodBody::Concrete(block) = &constructor.body {
                if block.statements.iter().all(checks_and_throws) {
                    body.statements(&block.statements);
                } else {
                    body.unmodeled(
                        block.span(),
                        format!("the body of the constructor of {short}"),
                        issues::CONSTRUCTOR_BODY,
                    );
                }
            }
        }

        let mut values = Vec::new();
        if let Some(parent) = parent {
            let initializer = constructor.and_then(|constructor| constructor.initializer.as_ref());
            let built = body.construct(
                initializer.map_or(span, HasSpan::span),
                parent,
                initializer.map(|initializer| &initializer.argument_list),
            );
            values.push(format!("{} := {built}", names::identifier(body.parent_field(parent).as_bytes())));
        }
        for (field, r#type) in fields.iter().zip(types) {
            let value = match field.value {
                FieldValue::Parameter => names::identifier(field.name),
                FieldValue::Initial(value) => body.value(value),
                FieldValue::Absent if r#type.starts_with("(Option") => "none".to_owned(),
                FieldValue::Absent => "default".to_owned(),
            };
            values.push(format!("{} := {value}", names::identifier(field.name)));
        }

        body.reads(full);
        let (mut lines, uses, unmodeled) = body.finish();
        lines.push(format!("return {{ {} }}", values.join(", ")));
        let text = format!(
            "@[simp] protected def {key} {} : M {lean} := do\n{}",
            parameters.join(" "),
            indented(&lines, 2).collect::<Vec<_>>().join("\n")
        );
        self.translation.declarations.push(Declaration {
            key,
            span,
            text,
            uses,
            unmodeled,
            role: Role::Constructor,
            measure: None,
            opaque: None,
        });
    }

    /// `Class.«$valid»`, what every value of the class keeps, spec section 28, when the reach finds it keeps anything:
    /// its constructor's checks, as the constructor returning on the value's own fields, when the body only checks, and
    /// what each value its fields hold keeps. It is a `simp` rewrite, as the constructor is, so a law's automatic steps
    /// read the precondition of a value a field holds as they read the value's own.
    fn valid(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        span: Span,
        constructor: Option<&Method>,
        fields: &[Field],
    ) {
        if !self.reach.is_valid(full) {
            return;
        }
        let key = names::member(lean, b"$valid");
        let mut body = Body::new(self.checked, self.reach, full, format!("{short}.valid"));
        let mut parts = Vec::new();
        let mut uses = Vec::new();
        if let Some(constructor) = constructor
            && let MethodBody::Concrete(block) = &constructor.body
            && !block.statements.is_empty()
            && block.statements.iter().all(checks_and_throws)
        {
            let construct = names::member(lean, b"__construct");
            let arguments: Vec<String> = constructor
                .parameter_list
                .parameters
                .iter()
                .map(|parameter| format!(" value.{}", parameter_name(parameter)))
                .collect();
            parts.push(format!("∃ built, {construct}{} = pure built", arguments.concat()));
            uses.push(Use {
                key: construct,
                file: Some(self.translation.file),
                place: format!("new {short}"),
                smaller: None,
                names_only: false,
            });
        }
        for field in fields {
            parts.extend(body.valid(field.hint, &format!("value.{}", names::identifier(field.name))));
        }
        if parts.is_empty() {
            parts.push("True".to_owned());
        }
        let text = format!("@[simp] protected def {key} (value : {lean}) : Prop :=\n  {}", parts.join(" ∧\n  "));
        let (_, body_uses, unmodeled) = body.finish();
        uses.extend(body_uses);
        self.translation.declarations.push(Declaration {
            key,
            span,
            text,
            uses,
            unmodeled,
            role: Role::Definition,
            measure: None,
            opaque: None,
        });
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
        let mut lines = vec![format!("protected inductive {lean} where")];
        let mut values = Vec::new();
        let mut cases = Vec::new();
        let mut fields = Vec::new();
        for member in &r#enum.members {
            let ClassLikeMember::EnumCase(case) = member else {
                continue;
            };
            let name = case.item.name();
            cases.push(names::member(&lean, name.value));
            if names::RESERVED.contains(&String::from_utf8_lossy(name.value).as_ref()) {
                body.unmodeled(
                    name.span,
                    format!("the case {short}.{}", String::from_utf8_lossy(name.value)),
                    issues::RESERVED_NAME,
                );
            }
            match &case.item {
                // Spec section 20: a case that carries data is a constructor of its fields.
                EnumCaseItem::Data(item) => {
                    let mut constructor = vec![format!("  | protected {}", names::identifier(name.value))];
                    let count = item.parameter_list.parameters.len();
                    for (place, field) in item.parameter_list.parameters.iter().enumerate() {
                        let r#type = field.hint.as_ref().map_or_else(|| "Unit".to_owned(), |hint| body.r#type(hint));
                        constructor.push(format!("({} : {type})", names::identifier(field.variable.name)));
                        fields.push((name.value, field.variable.name, place, count, r#type));
                    }
                    lines.push(constructor.join(" "));
                }
                EnumCaseItem::Backed(item) => {
                    lines.push(format!("  | protected {}", names::identifier(name.value)));
                    values.push((names::identifier(name.value), body.value(item.value)));
                }
                EnumCaseItem::Unit(_) => lines.push(format!("  | protected {}", names::identifier(name.value))),
            }
        }
        lines.push(DERIVED.to_owned());
        let coercions = self.coercions(&mut body, &lean, self.own_parents(full, r#enum.inheritance.as_ref()));
        self.declare_type(body, lean.clone(), r#enum.name.span, lines, coercions);
        self.class_values(full, &lean, &short, r#enum.name.span);

        // Spec section 20: a field of a case that carries data reads as the constructor's field, where a test named the
        // case. Each other value gives the field's default, which no such read sees. The value of an enum that implements
        // a sealed interface is the interface's, spec section 20.1. A proof reads the field through `«$held»`, the
        // constructor's equation, rather than unfolding the read, which for a machine of one case is a projection the
        // automatic steps can't relate to the case that built the value.
        for (case, field, place, count, r#type) in fields {
            let key = names::member(&names::member(&lean, case), field);
            let mut binders = vec!["_"; count];
            binders[place] = "held";
            let values: Vec<String> = (0..count).map(|value| format!("«${value}»")).collect();
            let mut body =
                Body::new(self.checked, self.reach, full, format!("{short}.{}", String::from_utf8_lossy(case)));
            let domain = body.class_type(r#enum.name.span, full);
            let pattern =
                body.held(r#enum.name.span, full, format!("(.{} {})", names::identifier(case), binders.join(" ")));
            let built =
                body.held(r#enum.name.span, full, format!("(.{} {})", names::identifier(case), values.join(" ")));
            let covers = body.held(r#enum.name.span, full, format!("({} ..)", names::member(&lean, case)));
            let mut lines = vec![format!("protected def {key} : {domain} → {type}"), format!("  | {pattern} => held")];
            lines.extend(default_arms(&body, full, &[covers]));
            lines.extend([
                String::new(),
                format!(
                    "@[simp, grind =] protected theorem {key}.«$held» : ∀ {}, {key} {built} = {} := fun {} => rfl",
                    values.join(" "),
                    values[place],
                    vec!["_"; count].join(" ")
                ),
            ]);
            self.translation.receivers.push((key.clone(), true));
            self.declare(body, key, r#enum.name.span, lines.join("\n"), Role::Definition);
        }

        // Spec section 28: a value of an enum whose cases carry data keeps what each value its case's fields hold keeps.
        if self.reach.is_valid(full) {
            let key = names::member(&lean, b"$valid");
            let mut body = Body::new(self.checked, self.reach, full, format!("{short}.valid"));
            let domain = body.class_type(r#enum.name.span, full);
            let mut lines = vec![format!("@[simp] protected def {key} : {domain} → Prop")];
            let mut covers = Vec::new();
            for member in &r#enum.members {
                let ClassLikeMember::EnumCase(case) = member else {
                    continue;
                };
                let name = case.item.name().value;
                let hints: Vec<Option<&Hint>> = match &case.item {
                    EnumCaseItem::Data(item) => {
                        item.parameter_list.parameters.iter().map(|field| field.hint.as_ref()).collect()
                    }
                    _ => Vec::new(),
                };
                let binders: Vec<String> = (0..hints.len()).map(|place| format!("«${place}»")).collect();
                let mut parts: Vec<String> =
                    hints.iter().zip(&binders).filter_map(|(hint, binder)| body.valid(*hint, binder)).collect();
                if parts.is_empty() {
                    parts.push("True".to_owned());
                }
                let written = format!("(.{} {})", names::identifier(name), binders.join(" "));
                let pattern = body.held(r#enum.name.span, full, written);
                lines.push(format!("  | {pattern} => {}", parts.join(" ∧ ")));
                covers.push(body.held(r#enum.name.span, full, format!("({} ..)", names::member(&lean, name))));
            }
            lines.extend(default_arms(&body, full, &covers));
            self.declare(body, key, r#enum.name.span, lines.join("\n"), Role::Definition);
        }

        if let Some(backing) = &r#enum.backing_type_hint {
            let mut body = Body::new(self.checked, self.reach, full, format!("{short}.value"));
            let r#type = body.r#type(&backing.hint);
            let domain = body.class_type(r#enum.name.span, full);
            let mut lines = vec![format!("protected def {} : {domain} → {type}", names::member(&lean, b"value"))];
            let mut covers = Vec::new();
            for (case, value) in &values {
                lines.push(format!("  | {} => {value}", body.held(r#enum.name.span, full, format!(".{case}"))));
                covers.push(body.held(r#enum.name.span, full, format!("({lean}.{case} ..)")));
            }
            lines.extend(default_arms(&body, full, &covers));
            self.translation.receivers.push((names::member(&lean, b"value"), true));
            self.declare(body, names::member(&lean, b"value"), r#enum.name.span, lines.join("\n"), Role::Definition);

            // `from` and `tryFrom` give the case whose value matches. For any other value `from` throws `ValueError`, as
            // the engine does, and `tryFrom` gives null.
            for (method, nullable) in [(b"from".as_slice(), false), (b"tryFrom".as_slice(), true)] {
                if !self.reach.reaches(full, method) {
                    continue;
                }
                let place = format!("{short}.{}", String::from_utf8_lossy(method));
                let mut body = Body::new(self.checked, self.reach, full, place);
                let class = body.class_type(r#enum.name.span, full);
                let returns = if nullable { format!("(Option {class})") } else { class };
                let parameter = format!("(value : {})", body.r#type(&backing.hint));
                let cases: Vec<String> = values
                    .iter()
                    .map(|(case, _)| body.held(r#enum.name.span, full, format!("{lean}.{case}")))
                    .collect();
                let (mut lines, uses, unmodeled) = body.finish();
                let mut chosen = if nullable { "pure none" } else { "throw Throwable.ValueError" }.to_owned();
                for (case, (_, value)) in cases.into_iter().zip(&values).rev() {
                    let found = if nullable { format!("(some {case})") } else { case };
                    chosen = format!("if value == {value} then pure {found} else {chosen}");
                }
                lines.push(chosen);
                self.define(
                    names::member(&lean, method),
                    r#enum.name.span,
                    &[parameter],
                    &returns,
                    (lines, uses, unmodeled),
                    None,
                );
            }
        }
        // An enum whose cases carry no data has `cases()`, the list of its cases in the order it declares them.
        if self.reach.reaches(full, b"cases") {
            let mut body = Body::new(self.checked, self.reach, full, format!("{short}.cases"));
            let returns = format!("(List {})", body.class_type(r#enum.name.span, full));
            let cases: Vec<String> = cases.into_iter().map(|case| body.held(r#enum.name.span, full, case)).collect();
            let (mut lines, uses, unmodeled) = body.finish();
            lines.push(format!("return [{}]", cases.join(", ")));
            self.define(
                names::member(&lean, b"cases"),
                r#enum.name.span,
                &[],
                &returns,
                (lines, uses, unmodeled),
                None,
            );
        }

        self.members(full, &lean, &short, r#enum.members.as_slice());
        if r#enum.runs_as_classes() {
            self.value_equality(full, &lean, &short, r#enum.name.span, &r#enum.members, |_| {
                vec!["(a == b)".to_owned()]
            });
        }
        // Spec section 20.2: the class of a machine's case holds the members of its block, each a definition on the
        // machine's inductive type, as `Shop.OrderStatus.Draft.pay`.
        for member in &r#enum.members {
            if let ClassLikeMember::EnumCase(case) = member {
                let class = case_class(full, case.item.name().value);
                let place = format!("{short}.{}", String::from_utf8_lossy(case.item.name().value));
                self.members(&class, &names::full_name(&class), &place, case.members());
            }
        }
        if let Some(machine) = self.checked.types().machine(word(full)) {
            self.invariants(r#enum, machine, full, &lean, &short);
        }
    }

    /// An interface as a type Lean knows nothing about, `Interface "App.Shop.Payment"`, so a proof holds for every class
    /// that implements it. A call of its methods is not translated, since the implementation that runs is chosen at run time.
    /// A sealed interface is the closed set of its implementers, spec section 20.1: an inductive type with a constructor
    /// for each, which holds the implementer's own value, and every value of an implementer is the interface's. One that
    /// no class implements has no implementer to tell apart, and is as opaque as any other.
    fn interface(&mut self, interface: &Interface) {
        let full = self.checked.names().get(&interface.name);
        self.name_module(full, interface.name.span);
        if !self.reach.reaches(full, b"") {
            return;
        }

        let lean = names::full_name(full);
        let short = names::short_name(full);
        let mut body = Body::new(self.checked, self.reach, full, short.clone());
        let implementers = self.reach.implementers(full).unwrap_or_default();
        let lines = if implementers.is_empty() {
            vec![format!("protected abbrev {lean} : Type := Interface \"{lean}\"")]
        } else {
            let mut constructors = vec![format!("protected inductive {lean} where")];
            for implementer in implementers {
                let held = body.own_type(interface.name.span, implementer.as_bytes());
                let constructor = names::identifier(names::short_name(implementer.as_bytes()).as_bytes());
                constructors.push(format!("  | protected {constructor} (value : {held})"));
            }
            constructors.push(DERIVED.to_owned());
            constructors
        };
        // Spec section 20.1: an implementer's value is the sealed interface's, so the interface's declaration holds
        // the coercion to each interface an implementer's header names.
        let mut parents = self.header(interface.inheritance.as_ref());
        for implementer in implementers {
            parents.extend(
                self.reach
                    .interfaces(implementer.as_bytes())
                    .iter()
                    .map(|parent| (parent.as_bytes(), interface.name.span)),
            );
        }
        let coercions = self.coercions(&mut body, &lean, parents);
        self.declare_type(body, lean.clone(), interface.name.span, lines, coercions);
        self.class_values(full, &lean, &short, interface.name.span);
        self.members(full, &lean, &short, interface.members.as_slice());
    }

    /// `Class.«$Class»`, the class of each value of the class-like `full` as a law reads a `Class<T>`, spec section 25,
    /// with one constructor for each class a value runs as, and `Class.«$classOf»`, the class a value runs as, which
    /// `Class.of` gives, when `full` is a family of classes, as [`Body::classes`] gives them.
    fn class_values(&mut self, full: &[u8], lean: &str, short: &str, span: Span) {
        let body = Body::new(self.checked, self.reach, full, format!("{short}.class"));
        let Some(classes) = body.classes(full) else {
            return;
        };
        let r#type = names::member(lean, b"$Class");
        let mut lines = vec![format!("protected inductive {type} where")];
        lines.extend(classes.iter().map(|(constructor, _)| format!("  | protected {constructor}")));
        lines.push(DERIVED.to_owned());
        self.declare_type(body, r#type, span, lines, Vec::new());

        let mut body = Body::new(self.checked, self.reach, full, format!("{short}.class"));
        let domain = body.class_type(span, full);
        let r#type = body.class_value_type(full);
        let key = names::member(lean, b"$classOf");
        let mut lines = vec![format!("protected def {key} : {domain} → {type}")];
        lines.extend(classes.iter().map(|(constructor, pattern)| format!("  | {pattern} => .{constructor}")));
        self.declare(body, key, span, lines.join("\n"), Role::Definition);
    }

    /// A `Coe` from the class-like `lean` to each interface of `parents` that a law reaches, once each, so a value of it
    /// passes where the interface is expected. Each parent is a full name and where it is named. The interface's value
    /// is `upcast`'s, of which Lean knows nothing, as of every interface value. They follow the class-like's own
    /// declaration, so every declaration that uses it can use them. A sealed interface needs none, since an
    /// implementer's value is the interface's.
    fn coercions<'name>(
        &self,
        body: &mut Body<'_, '_>,
        lean: &str,
        parents: impl IntoIterator<Item = (&'name [u8], Span)>,
    ) -> Vec<String> {
        let mut coerced: Vec<&[u8]> = Vec::new();
        let mut coercions = Vec::new();
        for (parent, span) in parents {
            let translated = self.reach.class(parent).is_some_and(|reached| reached.sharp);
            if self.checked.types().class_declaration(word(parent)).kind != DeclarationKind::Interface
                || !translated
                || !self.reach.reaches(parent, b"")
                || self.reach.implementers(parent).is_some()
                || coerced.iter().any(|known| self.reach.same_class(known, parent))
            {
                continue;
            }

            let interface = body.class_type(span, parent);
            coercions.push(format!(
                "protected instance {lean}.«$coe{}» : Coe {lean} {interface} := ⟨upcast⟩",
                coerced.len()
            ));
            coerced.push(parent);
        }

        coercions
    }

    /// The interfaces whose coercions the declaration of the class or enum `full` holds: those its header names, or
    /// none for an implementer of a sealed interface, whose value is the interface's, spec section 20.1, so the
    /// interface's declaration holds them.
    fn own_parents(&self, full: &[u8], inheritance: Option<&Inheritance>) -> Vec<(&[u8], Span)> {
        if self.reach.sealed_interfaces(full).is_empty() { self.header(inheritance) } else { Vec::new() }
    }

    /// Each class-like the header `inheritance` names, by its full name and where the header names it.
    fn header(&self, inheritance: Option<&Inheritance>) -> Vec<(&[u8], Span)> {
        inheritance
            .iter()
            .flat_map(|inheritance| inheritance.names())
            .map(|name| (self.checked.names().get(&name), name.span()))
            .collect()
    }

    /// The reached methods, computed properties, extension members and constants, the reader of each property whose
    /// value each implementation gives, and every law, of a class-like. A machine's invariant is proved as
    /// [`Translator::invariants`] says, so it is no law here.
    fn members(&mut self, full: &[u8], lean: &str, short: &str, members: &[ClassLikeMember<'_>]) {
        for member in members {
            match member {
                ClassLikeMember::Property(property) => {
                    self.reader(full, lean, short, property);
                    self.computed_property(full, lean, short, None, property);
                }
                ClassLikeMember::Method(method)
                    if php_method_name(method) != b"__construct" && self.reach.reaches(full, method.name.value) =>
                {
                    self.method(full, lean, short, method, None, None);
                    for interface in self.reach.law_interfaces(full) {
                        if self.reach.method(interface.as_bytes(), method.name.value).is_some() {
                            self.method(full, lean, short, method, None, Some(interface));
                        }
                    }
                }
                ClassLikeMember::Operator(operator) => {
                    if let Some(name) = php_operator_name(&operator.symbol, operator.parameter_list.parameters.len())
                        && self.reach.reaches(full, name)
                    {
                        self.operator(full, lean, short, operator, name);
                    }
                }
                ClassLikeMember::Extension(extension) => self.extension(full, lean, short, extension),
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
                            Some(r#type) => format!("protected def {key} : {type} := {value}"),
                            None => format!("protected def {key} := {value}"),
                        };
                        self.declare(body, key, item.name.span, text, Role::Definition);
                    }
                }
                ClassLikeMember::Law(law) if self.invariant_case(full, law).is_none() => {
                    self.law(full, lean, short, law);
                }
                _ => {}
            }
        }
    }

    /// The reader of `property` when each implementation gives its value, as an interface's, an abstract one and an
    /// overridable computed one do: `opaque Class.property : Class → T`, which each implementation answers, so a law
    /// knows of the value only what holds for every implementation.
    fn reader(&mut self, full: &[u8], lean: &str, short: &str, property: &Property) {
        let variable = property.first_variable();
        let name = variable.name.strip_prefix(b"$").unwrap_or(variable.name);
        if !self.reach.is_abstract_property(full, name) {
            return;
        }

        let key = names::member(lean, name);
        let place = format!("{short}.{}", String::from_utf8_lossy(name));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        if names::RESERVED.contains(&String::from_utf8_lossy(name).as_ref()) {
            body.unmodeled(variable.span, format!("the property {place}"), issues::RESERVED_NAME);
        }
        let receiver = body.class_type(variable.span, full);
        let r#type = body.declared_type(property.hint(), variable.span);
        self.translation.receivers.push((key.clone(), true));
        let text = format!("protected opaque {key} : {receiver} → {type}");
        self.declare(body, key, variable.span, text, Role::Definition);
    }

    /// A method, or an extension's method, spec section 26, which runs as a static method that takes `receiver` first.
    /// With `reading`, the method as the proof that its class keeps that interface's laws reads it, spec sections 22 and
    /// 28: each value of the interface is one of the class, under the name [`Body::read_method`] gives it. An
    /// interface's method is a value Lean knows nothing about but what the interface's laws say.
    fn method(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        method: &Method,
        receiver: Option<&FunctionLikeParameter>,
        reading: Option<Word>,
    ) {
        let place = format!("{short}.{}", String::from_utf8_lossy(method.name.value));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        let key = match reading {
            Some(interface) => {
                body.reads_as(interface, word(full));
                let read = body.read_method(full, &String::from_utf8_lossy(method.name.value));
                names::member(lean, read.as_bytes())
            }
            None => {
                self.defaults(full, &names::member(lean, method.name.value), &place, &method.parameter_list);
                names::member(lean, method.name.value)
            }
        };
        if names::RESERVED.contains(&String::from_utf8_lossy(method.name.value).as_ref()) {
            body.unmodeled(method.name.span, format!("the method {place}"), issues::RESERVED_NAME);
        }
        if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) {
            body.unmodeled(method.name.span, format!("the native method {place}"), issues::EXTERN);
        }

        let mut parameters = Vec::new();
        if receiver.is_none() && !method.is_static() {
            parameters.push(format!("(this : {})", body.class_type(method.name.span, full)));
        }
        if receiver.is_some() || !method.is_static() {
            self.translation.receivers.push((key.clone(), false));
        }
        body.takes(receiver.into_iter().chain(method.parameter_list.parameters.iter()));
        for parameter in receiver.into_iter().chain(method.parameter_list.parameters.iter()) {
            parameters.push(binder(&mut body, parameter));
        }
        // An extension's receiver comes before the arguments a call passes, so only a method's own calls recurse.
        if receiver.is_none() {
            body.recurse_on(method.parameter_list.parameters.iter());
        }
        let void = method.return_type_hint.as_ref().is_none_or(|hint| matches!(hint.hint, Hint::Void(_)));
        let returns =
            method.return_type_hint.as_ref().map_or_else(|| "Unit".to_owned(), |hint| body.r#type(&hint.hint));

        let translated = match &method.body {
            MethodBody::Concrete(block) => body.block_body(block, void),
            MethodBody::Expression(expression) => body.expression_body(expression.expression),
            MethodBody::Abstract(_)
                if self.checked.types().class_declaration(word(full)).kind == DeclarationKind::Interface =>
            {
                let text = format!("protected opaque {key} {} : M {returns}", parameters.join(" "));
                self.declare(body, key, method.name.span, text, Role::Definition);
                return;
            }
            MethodBody::Abstract(abstract_body) => {
                body.unmodeled(abstract_body.span(), format!("the abstract method {place}"), issues::OVERRIDABLE);
                body.finish()
            }
        };
        let names: Vec<String> = method.parameter_list.parameters.iter().map(parameter_name).collect();
        let measure = receiver.is_none().then(|| Measure::of(&translated.1, &names)).flatten();
        self.define(key, method.name.span, &parameters, &returns, translated, measure);
    }

    /// Each parameter default of the method `key`, spec section 7, as the constant [`names::default`] names, which a
    /// call that leaves its argument out passes. A default a law can't read refuses only a law that leaves it out.
    fn defaults(&mut self, full: &[u8], key: &str, place: &str, parameters: &FunctionLikeParameterList) {
        for (position, parameter) in parameters.parameters.iter().enumerate() {
            let Some(default) = &parameter.default_value else {
                continue;
            };
            let name =
                String::from_utf8_lossy(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name));
            let mut body = Body::new(self.checked, self.reach, full, format!("the default of `{name}` in {place}"));
            let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());
            let value = body.pure_value(default.value);
            let key = names::default(key, position);
            self.declare(
                body,
                key.clone(),
                parameter.span(),
                format!("protected def {key} : {type} := {value}"),
                Role::Definition,
            );
        }
    }

    /// An operator a class declares, spec section 19, as the static method `name` it runs as, which takes its operands.
    fn operator(&mut self, full: &[u8], lean: &str, short: &str, operator: &Operator, name: &[u8]) {
        let place = format!("{short}.operator {}", String::from_utf8_lossy(operator.symbol.as_bytes()));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        body.takes(operator.parameter_list.parameters.iter());
        let parameters: Vec<String> =
            operator.parameter_list.parameters.iter().map(|parameter| binder(&mut body, parameter)).collect();
        let returns = body.r#type(&operator.return_type_hint.hint);
        let translated = match &operator.body {
            MethodBody::Concrete(block) => body.block_body(block, false),
            MethodBody::Expression(expression) => body.expression_body(expression.expression),
            MethodBody::Abstract(abstract_body) => {
                body.unmodeled(abstract_body.span(), format!("the abstract operator {place}"), issues::OVERRIDABLE);
                body.finish()
            }
        };
        self.define(names::member(lean, name), operator.operator.span, &parameters, &returns, translated, None);
    }

    /// The reached members of an extension, spec section 26, each the static method it runs as.
    fn extension(&mut self, full: &[u8], lean: &str, short: &str, extension: &Extension) {
        let [receiver] = extension.parameter_list.parameters.as_slice() else {
            unreachable!("semantics refuses an extension without exactly one receiver");
        };

        for member in &extension.members {
            match member {
                ClassLikeMember::Method(method) if self.reach.reaches(full, method.name.value) => {
                    self.method(full, lean, short, method, Some(receiver), None);
                }
                ClassLikeMember::Property(property) => {
                    self.computed_property(full, lean, short, Some(receiver), property);
                }
                _ => {}
            }
        }
    }

    /// A computed property, spec section 6, which runs its `get` body as a method of no arguments: of `this`, or, for an
    /// extension's, spec section 26, a static method that takes only `receiver`.
    fn computed_property(
        &mut self,
        full: &[u8],
        lean: &str,
        short: &str,
        receiver: Option<&FunctionLikeParameter>,
        property: &Property,
    ) {
        let variable = property.first_variable();
        let name = variable.name.strip_prefix(b"$").unwrap_or(variable.name);
        if self.reach.method(full, name).is_none() {
            return;
        }

        let place = format!("{short}.{}", String::from_utf8_lossy(name));
        let mut body = Body::new(self.checked, self.reach, full, place.clone());
        if names::RESERVED.contains(&String::from_utf8_lossy(name).as_ref()) {
            body.unmodeled(variable.span, format!("the property {place}"), issues::RESERVED_NAME);
        }
        let parameter = match receiver {
            Some(receiver) => {
                body.takes([receiver]);
                binder(&mut body, receiver)
            }
            None => format!("(this : {})", body.class_type(variable.span, full)),
        };
        self.translation.receivers.push((names::member(lean, name), true));
        let returns = body.declared_type(property.hint(), variable.span);
        let translated = match property {
            Property::Computed(computed) => body.expression_body(computed.body.expression),
            Property::Hooked(hooked) => {
                match hooked.hook_list.hooks.iter().find(|hook| hook.name.value == b"get").map(|hook| &hook.body) {
                    Some(PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block))) => {
                        body.block_body(block, false)
                    }
                    Some(PropertyHookBody::Concrete(PropertyHookConcreteBody::Expression(get))) => {
                        body.expression_body(get.expression)
                    }
                    _ => return,
                }
            }
            Property::Plain(_) => return,
        };
        self.define(names::member(lean, name), variable.span, &[parameter], &returns, translated, None);
    }

    /// Declares the [`definition`] of `key`.
    fn define(
        &mut self,
        key: String,
        span: Span,
        parameters: &[String],
        returns: &str,
        translated: (Vec<String>, Vec<Use>, Vec<Unmodeled>),
        measure: Option<Measure>,
    ) {
        self.translation.declarations.push(definition(key, span, parameters, returns, translated, measure));
    }

    /// A law of the class-like `full`: its statement, as [`Translator::law_statement`] gives it, which assumes each law of
    /// an interface a parameter's type names, as [`Translator::facts`] gives them, and its theorem, which its body
    /// proves, from [`Prover`]. An interface's law has no theorem: each PHP# implementer proves it, as
    /// [`Translator::obligation`] says.
    fn law(&mut self, full: &[u8], lean: &str, short: &str, law: &LawNode) {
        let name = String::from_utf8_lossy(law.name.value).into_owned();
        let key = names::member(lean, law.name.value);
        let place = format!("{short}.{name}");
        let span = Span::between(law.law.span(), law.name.span);
        // Spec sections 22 and 28: an interface's law states what each PHP# implementer proves, and has no proof of
        // its own.
        if self.checked.types().class_declaration(word(full)).kind == DeclarationKind::Interface {
            let statement = self.law_statement(full, &key, &place, law, None, &[]);
            self.push_statement(key, span, statement);
            for implementer in self.reach.law_implementers(full).to_vec() {
                self.obligation(full, short, law, &name, implementer);
            }
            return;
        }

        let facts = self.facts(law);
        let mut statement = self.law_statement(full, &key, &place, law, None, &facts);
        // The theorem lives beside the statement, so the generated module holds what its proof uses too.
        let mut proof = Prover::new(self.checked, self.reach, full, place, law, key.clone()).assuming(facts).prove(
            &statement.binders,
            &statement.parameters,
            statement.preconditions,
        );
        statement.uses.append(&mut proof.uses);
        statement.unmodeled.append(&mut proof.unmodeled);
        let breaking = self.breaking(full, lean, short, law);
        self.translation.laws.push(Law { name, statement: key.clone(), span, proof, breaking, broken: None });
        self.push_statement(key, span, statement);
    }

    /// The proof that the PHP# class `implementer` keeps `law`, a law of the interface `full`, spec sections 22 and 28:
    /// `law`'s statement with each value of the interface read as one of `implementer`, as [`Body::reads_as`] says,
    /// named `<implementer>.«creditAdds$Ledger»` and reported on `implementer`'s name.
    fn obligation(&mut self, full: &[u8], short: &str, law: &LawNode, name: &str, implementer: Word) {
        let Some(reached) = self.reach.class(implementer.as_bytes()) else {
            return;
        };
        let implementer_short = names::short_name(reached.name.as_bytes());
        let key = names::member(&names::full_name(reached.name.as_bytes()), format!("{name}${short}").as_bytes());
        let place = format!("{implementer_short}.{name}");
        let reading = (word(full), reached.name);
        let mut statement = self.law_statement(full, &key, &place, law, Some(reading), &[]);
        let mut proof = Prover::new(self.checked, self.reach, full, place, law, key.clone())
            .reading(reading.0, reading.1)
            .prove(&statement.binders, &statement.parameters, statement.preconditions);
        statement.uses.append(&mut proof.uses);
        statement.unmodeled.append(&mut proof.unmodeled);

        let read = format!("${short}»");
        let mut methods: Vec<String> = Vec::new();
        for used in statement.uses.iter().filter(|used| used.key.ends_with(&read)) {
            let method = format!("`{}`", used.place);
            if !methods.contains(&method) {
                methods.push(method);
            }
        }
        let broken = match methods.as_slice() {
            [] => format!("`{implementer_short}` breaks `{short}.{name}`."),
            [method] => format!("{method} breaks `{short}.{name}`."),
            [rest @ .., last] => format!("{} and {last} break `{short}.{name}`.", rest.join(", ")),
        };
        self.translation.laws.push(Law {
            name: name.to_owned(),
            statement: key.clone(),
            span: reached.span,
            proof,
            breaking: None,
            broken: Some(broken),
        });
        self.push_statement(key, reached.span, statement);
    }

    /// The laws a law over a value of an interface takes as facts, spec section 28: each law of the interface a
    /// parameter's type names, which each PHP# implementer proves.
    fn facts(&self, law: &LawNode) -> Vec<Fact> {
        let mut facts: Vec<Fact> = Vec::new();
        for parameter in &law.parameter_list.parameters {
            let Some(Hint::Identifier(identifier)) = &parameter.hint else {
                continue;
            };
            let interface = self.checked.names().get(identifier);
            let Some(reached) = self.reach.class(interface) else {
                continue;
            };
            for (law_name, arity) in self.reach.interface_laws(interface) {
                let statement = names::member(&names::full_name(reached.name.as_bytes()), law_name.as_bytes());
                if facts.iter().any(|fact| fact.statement == statement) {
                    continue;
                }
                let arguments: Vec<String> = (1..=*arity).map(|place| format!("«$a{place}»")).collect();
                let premise = if arguments.is_empty() {
                    statement.clone()
                } else {
                    format!("(∀ {}, {statement} {})", arguments.join(" "), arguments.join(" "))
                };
                facts.push(Fact {
                    name: format!("«$h{}»", facts.len() + 1),
                    statement,
                    premise,
                    file: Some(reached.file),
                    place: format!(
                        "{}.{}",
                        names::short_name(reached.name.as_bytes()),
                        String::from_utf8_lossy(law_name.as_bytes())
                    ),
                });
            }
        }

        facts
    }

    /// The statement `protected def <key> (a : A) : Prop := [fact →] [condition = pure true →] [A.«$valid» a →] holds
    /// do …` of `law`, a law of `full`: the law holds for every value of its parameters that meets its `when` condition
    /// and keeps its class-like's precondition, as each value of it does, given each of `facts`, unless its claim
    /// finishes with `false`. A parameter of one case of a machine holds a value of that case alone that keeps the
    /// case's invariants, as [`machine::HeldCase::premises`] states it. The condition and the claim compute ints
    /// exactly, as [`Body::computes_exactly`] says.
    /// With `reading`, each value of an interface is read as one of an implementer, as [`Body::reads_as`] says.
    fn law_statement(
        &self,
        full: &[u8],
        key: &str,
        place: &str,
        law: &LawNode,
        reading: Option<(Word, Word)>,
        facts: &[Fact],
    ) -> LawStatement {
        let body_of = || {
            let mut body = Body::for_law(self.checked, self.reach, full, place.to_owned(), law);
            if let Some((interface, implementer)) = reading {
                body.reads_as(interface, implementer);
            }
            body
        };
        let mut body = body_of();
        let binders: Vec<String> =
            law.parameter_list.parameters.iter().map(|parameter| binder(&mut body, parameter)).collect();
        let parameters: Vec<String> =
            law.parameter_list.parameters.iter().map(|parameter| parameter_name(parameter)).collect();

        let mut text = format!("protected def {key} {} : Prop :=\n", binders.join(" "));
        let (mut uses, mut unmodeled) = (Vec::new(), Vec::new());
        for fact in facts {
            text.push_str("  ");
            text.push_str(&fact.premise);
            text.push_str(" →\n");
            uses.push(Use {
                key: fact.statement.clone(),
                file: fact.file,
                place: fact.place.clone(),
                smaller: None,
                names_only: false,
            });
        }
        if let Some(condition) = &law.condition {
            let mut computed = body_of();
            computed.computes_exactly();
            let (lines, terms, condition_uses, condition_unmodeled) = computed.terms(&[], &[condition.expression]);
            text.push_str("  ((do\n");
            text.extend(indented(&lines, 6).map(|line| format!("{line}\n")));
            text.push_str("      return (");
            text.push_str(&terms[0]);
            text.push_str(")) : M Bool) = pure true →\n");
            uses.extend(condition_uses);
            unmodeled.extend(condition_unmodeled);
        }
        // Spec section 28: each value the law takes keeps the precondition every value of its class-like keeps, and a
        // value of one case of a machine is that case's, keeping its invariants, spec section 20.2.
        let mut preconditions = Vec::new();
        for parameter in law.parameter_list.parameters.iter() {
            let value = parameter_name(parameter);
            if let Some(held) = self.held_case(parameter) {
                preconditions.extend(held.premises(&value));
                uses.extend(held.uses);
            }
            preconditions.extend(body.valid(parameter.hint.as_ref(), &value));
        }
        text.extend(preconditions.iter().map(|valid| format!("  {valid} →\n")));
        body.computes_exactly();
        let (lines, claim_uses, claim_unmodeled) = body.expression_body(law.claim);
        text.push_str("  holds do\n");
        text.push_str(&indented(&lines, 4).collect::<Vec<_>>().join("\n"));
        uses.extend(claim_uses);
        unmodeled.extend(claim_unmodeled);

        LawStatement { text, uses, unmodeled, binders, parameters, preconditions: !preconditions.is_empty() }
    }

    /// Declares the law statement `statement` under `key`.
    fn push_statement(&mut self, key: String, span: Span, statement: LawStatement) {
        self.translation.declarations.push(Declaration {
            key,
            span,
            text: statement.text,
            uses: statement.uses,
            unmodeled: statement.unmodeled,
            role: Role::Definition,
            measure: None,
            opaque: None,
        });
    }

    fn states_laws(&self, members: &Sequence<'_, ClassLikeMember<'_>>) -> bool {
        members.iter().any(|member| matches!(member, ClassLikeMember::Law(_)))
    }

    fn declare(&mut self, body: Body<'_, '_>, key: String, span: Span, text: String, role: Role) {
        let (_, uses, unmodeled) = body.finish();
        self.translation.declarations.push(Declaration {
            key,
            span,
            text,
            uses,
            unmodeled,
            role,
            measure: None,
            opaque: None,
        });
    }

    /// The class-like type `key`, of its Lean `lines` and then the `coercions` to the interfaces it implements, and its
    /// opaque text, which a law that only names the type proves with when its own text has a problem.
    fn declare_type(
        &mut self,
        body: Body<'_, '_>,
        key: String,
        span: Span,
        lines: Vec<String>,
        coercions: Vec<String>,
    ) {
        let (_, uses, unmodeled) = body.finish();
        let opaque = std::iter::once(format!("protected abbrev {key} : Type := Interface \"{key}\""))
            .chain(coercions.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        self.translation.declarations.push(Declaration {
            key,
            span,
            text: lines.into_iter().chain(coercions).collect::<Vec<_>>().join("\n"),
            uses,
            unmodeled,
            role: Role::Type,
            measure: None,
            opaque: Some(opaque),
        });
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

/// The arm of a definition over `class`'s Lean type that gives `default` for each value `covers` leaves out, one for
/// each constructor, as [`Body::constructors`] gives them, so no value is left to a `_` arm.
fn default_arms(body: &Body<'_, '_>, class: &[u8], covers: &[String]) -> Vec<String> {
    body.constructors(class)
        .into_iter()
        .filter(|constructor| !covers.contains(constructor))
        .map(|other| format!("  | {other} => default"))
        .collect()
}

/// Whether each of the struct `full`'s `fields` holds equal values in `a` and `b`, by that field's own `==`, spec
/// section 19, each a term of `body`, whose refusals stand at `span`.
fn fields_equality(body: &mut Body<'_, '_>, full: &[u8], span: Span, fields: &[Field]) -> Vec<String> {
    let mut equal = Vec::new();
    for field in fields {
        let Some(hint) = field.hint else {
            equal.push(body.unmodeled(span, "a declaration without a type".to_owned(), issues::MISSING_TYPE));
            continue;
        };
        let field_type = get_union_from_hint(
            hint,
            Some(word(full)),
            body.checked.names(),
            &TypeResolutionContext::new(),
            Dialect::Sharp,
        );
        equal.push(body.fields_equal(span, full, field.name, &field_type));
    }

    equal
}

/// `protected def key (parameters) : M returns := do …`, a definition a law unfolds, from its translated body.
fn definition(
    key: String,
    span: Span,
    parameters: &[String],
    returns: &str,
    (lines, uses, unmodeled): (Vec<String>, Vec<Use>, Vec<Unmodeled>),
    measure: Option<Measure>,
) -> Declaration {
    let text = format!(
        "protected def {key} {} : M {returns} := do\n{}",
        parameters.join(" "),
        indented(&lines, 2).collect::<Vec<_>>().join("\n")
    );

    Declaration { key, span, text, uses, unmodeled, role: Role::Definition, measure, opaque: None }
}

/// `parameter` as the binder `(name : Type)`, with its Lean type as `body` translates it.
fn binder(body: &mut Body<'_, '_>, parameter: &FunctionLikeParameter) -> String {
    let r#type = body.declared_type(parameter.hint.as_ref(), parameter.span());

    format!("({} : {type})", parameter_name(parameter))
}

/// `parameter`'s name as Lean writes it.
fn parameter_name(parameter: &FunctionLikeParameter) -> String {
    names::identifier(parameter.variable.name.strip_prefix(b"$").unwrap_or(parameter.variable.name))
}

/// Whether `statement` only checks a condition and throws, as `if (nanosecond < 0) { throw new ValueError(…); }` does,
/// so a constructor body of such statements is a precondition of the value it builds, spec section 28.
fn checks_and_throws(statement: &Statement) -> bool {
    let Statement::If(r#if) = statement else {
        return false;
    };
    let IfBody::Statement(body) = &r#if.body else {
        return false;
    };

    body.else_if_clauses.is_empty() && body.else_clause.is_none() && throws(body.statement)
}

/// Whether `statement` is a `throw`, alone or as the one statement of a block.
fn throws(statement: &Statement) -> bool {
    match statement {
        Statement::Expression(statement) => matches!(statement.expression, Expression::Throw(_)),
        Statement::Block(block) => matches!(block.statements.as_slice(), [statement] if throws(statement)),
        _ => false,
    }
}
