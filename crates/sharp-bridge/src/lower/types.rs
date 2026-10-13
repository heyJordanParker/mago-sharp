use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_analyzer::artifacts::CallTarget;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::get_sole_backed_enum;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Call;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::word;

use super::inline::InlineForm;
use super::inline::InlineForms;
use super::inline::key;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
    /// The file, which names the lambdas it declares.
    file: &'analysis File,
    /// The file's resolved names, which name the class of a `Class.m()` call.
    names: ResolvedNames<'analysis>,
    artifacts: &'analysis AnalysisArtifacts,
    codebase: &'analysis CodebaseMetadata,
    inline_forms: &'analysis InlineForms,
}

/// The declaration a class or member name resolves to, as the checker found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Declaration {
    pub kind: DeclarationKind,
    /// The class that declares it: a class itself, or the class an inherited member is declared in.
    pub class: Word,
    /// The member's name, or a class's own name.
    pub name: Word,
    /// Whether code outside the class may use it. A class is public.
    pub public: bool,
}

/// What a declaration is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclarationKind {
    Class,
    Interface,
    /// `backed` is whether each case has a backing value, which a `Map` keyed by the enum holds for the case.
    Enum {
        backed: bool,
    },
    Constant,
    EnumCase,
    StaticProperty,
    /// `typed` is whether the property's root declaration, the one at the bottom of its chain of overrides, has a type.
    /// PHP refuses a type on an override of an untyped property, so every override above an untyped root runs untyped.
    Property {
        typed: bool,
    },
    StaticMethod,
    /// `overridable` is whether a subclass can declare its own body, which keeps a call from being inlined.
    Method {
        overridable: bool,
    },
}

impl<'analysis> Types<'analysis> {
    pub(crate) fn new(
        file: &'analysis File,
        names: ResolvedNames<'analysis>,
        artifacts: &'analysis AnalysisArtifacts,
        codebase: &'analysis CodebaseMetadata,
        inline_forms: &'analysis InlineForms,
    ) -> Self {
        Self { file, names, artifacts, codebase, inline_forms }
    }

    pub(crate) fn names(&self) -> &ResolvedNames<'analysis> {
        &self.names
    }

    /// The type the analysis gave `expression`.
    #[must_use]
    pub fn expression_type(&self, expression: &Expression) -> &'analysis TUnion {
        self.artifacts.get_expression_type(expression).unwrap_or_else(|| {
            unreachable!(
                "the analysis types every expression of a file the checker accepted, not {:?}",
                expression.span()
            )
        })
    }

    /// The declaration the fully qualified class name `class` resolves to.
    #[must_use]
    pub fn class_declaration(&self, class: &[u8]) -> Declaration {
        let metadata = self.codebase.get_class_like(class).unwrap_or_else(|| {
            unreachable!("the checker refuses the unknown class `{}`", String::from_utf8_lossy(class))
        });

        let kind = if metadata.kind.is_interface() {
            DeclarationKind::Interface
        } else if metadata.kind.is_enum() {
            DeclarationKind::Enum { backed: metadata.enum_type.is_some() }
        } else {
            DeclarationKind::Class
        };

        Declaration { kind, class: metadata.original_name, name: metadata.original_name, public: true }
    }

    /// The bounds of the fully qualified class name `class` when it is a PHP# generic class, whose objects carry their
    /// type arguments: the [`type_text`] of each type parameter's bound in declaration order, joined by `, `. They are
    /// the type arguments of an object plain PHP creates, so a bound that names a type parameter of the class writes it
    /// as `Any?`: `Comparable<Any?>` for `Sorted<TItem : Comparable<TItem>>`.
    pub(crate) fn bounds(&self, class: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_class_like(class)?;
        if !metadata.flags.is_sharp() || metadata.template_types.is_empty() {
            return None;
        }

        let bounds: Vec<String> =
            metadata.template_types.values().map(|template| type_text(&template.constraint, self.codebase)).collect();

        Some(bounds.join(", "))
    }

    /// The type arguments the header of the fully qualified class name `class` gives each generic parent and
    /// interface, as a type text list of those classes with their type arguments, each type parameter of `class`
    /// written as `$` and its index, and sorted by their text: `App.PaginatedList<App.Order>` for
    /// `OrderPage : PaginatedList<Order>`, and `App.Base<List<$0>>` for `Sub<T> : Base<List<T>>`. None when the header
    /// gives none.
    pub(crate) fn header(&self, class: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_class_like(class)?;
        let mut ancestors: Vec<String> = metadata
            .template_extended_offsets
            .iter()
            .map(|(ancestor, arguments)| {
                let arguments: Vec<String> =
                    arguments.iter().map(|argument| text(argument, self.codebase, Parameter::Index)).collect();

                format!("{}<{}>", class_text(*ancestor, self.codebase), arguments.join(", "))
            })
            .collect();
        ancestors.sort_unstable();

        (!ancestors.is_empty()).then(|| ancestors.join(", "))
    }

    /// The type arguments the `new` at `span` gives the fully qualified class name `class`, as the checker found them,
    /// when it is a PHP# generic class: the type text of each in declaration order, joined by `, `. A type parameter of
    /// the class the `new` is in is written as `$` and its index, which the engine replaces with `this`'s type argument
    /// at that index, and one of the method it is in as `#` and its index, which the engine replaces with the method's
    /// own type argument. A lambda captures its method's type arguments, so a `new` in it writes them the same way.
    pub(crate) fn type_arguments(&self, class: &[u8], span: Span) -> Option<String> {
        self.bounds(class)?;
        let arguments =
            self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset)).unwrap_or_else(|| {
                unreachable!("the analysis records the type arguments of every generic `new`, not {span:?}")
            });

        Some(self.argument_texts(arguments))
    }

    /// The type arguments the generic method call at `span` gives the method, as [`Self::type_arguments`] writes those
    /// of a `new`. None when the method declares no type parameter, or a built-in class declares it, whose method never
    /// reads them, as the prelude's `Sharp\ListMethods` and `Sharp\MapMethods` declare a list's and a map's. A method
    /// of any project class, PHP# or plain PHP, may run a PHP# method that overrides or implements it, so a call through
    /// a plain PHP `@template` interface or parent carries them, and a plain PHP method ignores them. The class that
    /// declares the method decides, so `super.m()` carries them through a plain PHP parent that inherits a PHP# `m`.
    pub(crate) fn call_type_arguments(&self, call: &Expression) -> Option<String> {
        let span = call.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;
        let callee = self.call_target(call).class;
        if self.codebase.get_class_like(callee.as_bytes()).is_none_or(|callee| callee.flags.is_built_in()) {
            return None;
        }

        Some(self.argument_texts(arguments))
    }

    /// The type arguments the method value `value` of a PHP# generic method takes from the function type it goes
    /// into, as the checker found them, written as [`Self::type_arguments`] writes those of a `new`. None for the value
    /// of a method that isn't generic.
    pub(crate) fn method_value_type_arguments(&self, value: &Expression) -> Option<String> {
        let span = value.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;

        Some(self.argument_texts(arguments))
    }

    fn argument_texts(&self, arguments: &[TUnion]) -> String {
        arguments.iter().map(|argument| text(argument, self.codebase, Parameter::Index)).collect::<Vec<_>>().join(", ")
    }

    /// The metadata of the method `method` of the fully qualified class name `class` that the engine reads on a call:
    /// the bounds of its own type parameters, which a call from plain PHP gives it, and a type text list with one entry
    /// per parameter that a call from plain PHP checks its argument against, [`Self::checked_text`] or `Any?`. Each half
    /// is None when the method declares no type parameter, or no parameter needs a check.
    pub(crate) fn method_metadata(&self, class: &[u8], method: &[u8]) -> (Option<String>, Option<String>) {
        self.codebase.get_method(class, method).map_or((None, None), |metadata| self.function_metadata(metadata))
    }

    /// [`Self::method_metadata`] of the lambda at `span`, which plain PHP calls as a `Closure`. A lambda declares no
    /// type parameter, so it has no bounds.
    pub(crate) fn lambda_metadata(&self, span: Span) -> (Option<String>, Option<String>) {
        self.codebase.get_closure_at(self.file, span).map_or((None, None), |metadata| self.function_metadata(metadata))
    }

    fn function_metadata(&self, metadata: &FunctionLikeMetadata) -> (Option<String>, Option<String>) {
        let bounds: Vec<String> = metadata
            .template_types
            .values()
            .map(|template| text(&template.constraint, self.codebase, Parameter::Bound))
            .collect();
        let parameters: Vec<Option<String>> = metadata
            .parameters
            .iter()
            .map(|parameter| parameter.type_metadata.as_ref().and_then(|r#type| self.checked_text(&r#type.type_union)))
            .collect();

        (
            (!bounds.is_empty()).then(|| bounds.join(", ")),
            parameters.iter().any(Option::is_some).then(|| {
                parameters
                    .into_iter()
                    .map(|parameter| parameter.unwrap_or_else(|| "Any?".to_owned()))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        )
    }

    /// The type text a call from PHP# into plain PHP checks its result against, as a parameter checks an argument from
    /// plain PHP, and the call as PHP's return check names it, `Lib\Pager::orders()`: [`Self::checked_text`] of
    /// the call's type, when the checker resolved `call` to a plain PHP function or method. The checker reads a plain
    /// PHP return type that names a PHP# generic class from a docblock it can't prove, so the engine tests the value
    /// spec section 12 lets in, wherever it goes next.
    pub(crate) fn call_check(&self, call: &Expression) -> Option<(String, String)> {
        let (callee, metadata) = self.artifacts.get_callees(call).find_map(|target| match target {
            CallTarget::FunctionLike { callee, .. } | CallTarget::MagicMethod { callee, .. } => self
                .codebase
                .get_function_like(callee)
                .filter(|metadata| !metadata.flags.is_sharp())
                .map(|metadata| (callee, metadata)),
            CallTarget::Property { .. } => None,
        })?;
        let text = self.checked_text(self.artifacts.get_expression_type(call)?)?;
        let name = match callee {
            FunctionLikeIdentifier::Method(class, _) => {
                let class = self.codebase.get_class_like(class.as_bytes()).map_or(*class, |class| class.original_name);
                format!("{class}::{}", metadata.original_name)
            }
            _ => metadata.original_name.to_string(),
        };

        Some((text, format!("{name}()")))
    }

    /// The type text the property `property` of the fully qualified class name `class` checks a value from plain PHP
    /// against, as a parameter checks an argument: [`Self::checked_text`] of its type, when plain PHP can write it. That
    /// is a property PHP# writes as public or protected and not read-only.
    pub(crate) fn property_check(&self, class: &[u8], property: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_property(class, &[b"$", property].concat())?;
        if metadata.write_visibility.is_private() || metadata.flags.is_readonly() {
            return None;
        }

        self.checked_text(&metadata.type_metadata.as_ref()?.type_union)
    }

    /// `r#type` as the type text an entry check tests a value from plain PHP against, when part of it is a PHP# class
    /// with a type argument the check can test, or a type parameter of the class. None otherwise: plain PHP's generics
    /// are erased, since an object of a plain PHP generic class carries no type arguments, and a call from plain PHP
    /// gives a method's own type parameters none, so they match any value.
    fn checked_text(&self, r#type: &TUnion) -> Option<String> {
        let is_method_parameter = |r#type: &TUnion| {
            r#type.types.iter().filter(|atomic| !atomic.is_null()).all(|atomic| {
                matches!(
                    atomic,
                    TAtomic::GenericParameter(parameter)
                        if matches!(parameter.defining_entity, GenericParent::FunctionLike(_))
                )
            })
        };
        let checked = r#type.types.iter().any(|atomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => {
                object
                    .get_type_parameters()
                    .is_some_and(|arguments| arguments.iter().any(|argument| !is_method_parameter(argument)))
                    && self.codebase.get_class_like(object.name.as_bytes()).is_some_and(|class| class.flags.is_sharp())
            }
            TAtomic::GenericParameter(parameter) => matches!(parameter.defining_entity, GenericParent::ClassLike(_)),
            _ => false,
        });

        checked.then(|| text(r#type, self.codebase, Parameter::Index))
    }

    /// The fully qualified name of the one backed enum every value of `r#type` but `null` is a case of, as
    /// [`get_sole_backed_enum`] finds it.
    pub(crate) fn backed_enum(&self, r#type: &TUnion) -> Option<&'analysis [u8]> {
        get_sole_backed_enum(r#type, self.codebase, Dialect::Sharp)
            .map(|backed_enum| backed_enum.original_name.as_bytes())
    }

    /// The constructor `parent::__construct` reaches from the fully qualified class name `class`, as
    /// [`CodebaseMetadata::get_parent_constructor`] finds it.
    #[must_use]
    pub fn parent_constructor(&self, class: &[u8]) -> Option<&'analysis FunctionLikeMetadata> {
        self.codebase.get_parent_constructor(class)
    }

    /// The declaration `member` of the fully qualified class name `class` resolves to when code reads it: an enum case,
    /// a constant, a property, then a method, which the read takes as a first-class callable. Only when the class
    /// declares none of them is it a property its `__get` serves. A call is [`Self::call_target`]'s.
    #[must_use]
    pub fn member_declaration(&self, class: &[u8], member: &[u8]) -> Declaration {
        self.declared_member(class, member).unwrap_or_else(|| {
            if !self.codebase.method_exists(class, b"__get") {
                unreachable!(
                    "the checker refuses `{}.{}`, which names no member",
                    String::from_utf8_lossy(class),
                    String::from_utf8_lossy(member)
                )
            }

            Declaration {
                kind: DeclarationKind::Property { typed: false },
                class: word(class),
                name: word(member),
                public: true,
            }
        })
    }

    /// The member `member` of the fully qualified class name `class` declares or inherits, as
    /// [`Self::member_declaration`] resolves it, leaving out a property its `__get` serves.
    pub(crate) fn declared_member(&self, class: &[u8], member: &[u8]) -> Option<Declaration> {
        let declared = |kind, public| Declaration { kind, class: word(class), name: word(member), public };

        if self.codebase.get_enum_case(class, member).is_some() {
            Some(declared(DeclarationKind::EnumCase, true))
        } else if let Some(constant) = self.codebase.get_class_constant(class, member) {
            Some(declared(DeclarationKind::Constant, constant.visibility.is_public()))
        } else {
            self.property_declaration(class, member).or_else(|| self.method_declaration(class, member))
        }
    }

    /// The declaration the method call `call`, null-safe or not, runs, as the analysis resolved it when it checked the
    /// call: a method, a method the class's `__call` or `__callStatic` serves, or a property holding a function, as spec
    /// section 14 calls one. Every class the receiver can be has the same kind of member, and the declaration is the
    /// first the analysis recorded. A call of a property also records what the function it holds runs, such as an
    /// object's `__invoke`, for the effects check, and lowers as the property alone.
    #[must_use]
    pub fn call_target(&self, call: &Expression) -> Declaration {
        let targets: Vec<&CallTarget> = self.artifacts.get_callees(call).collect();
        let calls_property = targets.iter().any(|target| matches!(target, CallTarget::Property { .. }));
        let declarations: Vec<Declaration> = targets
            .into_iter()
            .filter(|target| !calls_property || matches!(target, CallTarget::Property { .. }))
            .map(|target| self.target_declaration(target))
            .collect();
        let Some(&declaration) = declarations.first() else {
            unreachable!(
                "the analysis records what each call of a file the checker accepted runs, not {:?}",
                call.span()
            );
        };
        agreed_kind(declarations.iter().map(|declaration| declaration.kind));

        declaration
    }

    /// The full name of the constant the read `constant` reaches, as the analysis found it: the constant of that name
    /// in the file's namespace, or else the global one.
    #[must_use]
    pub fn constant_target(&self, constant: &ConstantAccess) -> Word {
        self.codebase
            .get_constant_or_global(self.names.get(constant), constant.name.value())
            .unwrap_or_else(|| unreachable!("the checker refuses the undefined constant `{}`", constant.name))
            .name
    }

    /// The inline form of the standard library method `declaration` names, if it has one.
    pub(crate) fn inline_form(&self, declaration: &Declaration) -> Option<&'analysis InlineForm> {
        if !matches!(declaration.kind, DeclarationKind::Method { .. } | DeclarationKind::StaticMethod) {
            return None;
        }

        self.inline_forms.get(&key(declaration.class.as_bytes(), declaration.name.as_bytes()))
    }

    /// The declaration of what the analysis recorded a method call runs. A method `__call` or `__callStatic` serves is
    /// public and, as any subclass may declare it, overridable.
    fn target_declaration(&self, target: &CallTarget) -> Declaration {
        let method = |class: Word, method: Word| {
            self.method_declaration(class.as_bytes(), method.as_bytes())
                .unwrap_or_else(|| unreachable!("the analysis resolved `{class}.{method}()` from the codebase"))
        };

        match *target {
            CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, name), .. } => method(class, name),
            CallTarget::MagicMethod {
                callee: FunctionLikeIdentifier::Method(magic_class, magic),
                class,
                method: name,
            } => {
                let kind = match method(magic_class, magic).kind {
                    DeclarationKind::StaticMethod => DeclarationKind::StaticMethod,
                    _ => DeclarationKind::Method { overridable: true },
                };

                Declaration { kind, class, name, public: true }
            }
            CallTarget::Property { class, property } => property
                .as_bytes()
                .strip_prefix(b"$")
                .and_then(|property| self.property_declaration(class.as_bytes(), property))
                .unwrap_or_else(|| unreachable!("the analysis resolved the property `{class}.{property}`")),
            target => unreachable!("a method call runs a method or a property's function, not {target:?}"),
        }
    }

    /// The type of the keys a value of `r#type` holds, or none when part of it is no `Map` or `List`.
    pub(crate) fn map_key_type(&self, r#type: &TUnion) -> Option<TUnion> {
        let mut key_type = None;
        for atomic in r#type.types.iter() {
            let TAtomic::Array(array) = atomic else {
                return None;
            };
            let (key, _) = get_array_parameters(array, self.codebase);
            key_type = Some(add_optional_union_type(key, key_type.as_ref(), self.codebase));
        }

        key_type
    }

    /// The type of the elements a value of `r#type` holds, the `T` of a `List<T>`, leaving out `null`. The caller asks
    /// only about a value the checker typed as a collection.
    pub(crate) fn element_type(&self, r#type: &TUnion) -> TUnion {
        let mut element_type = None;
        for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
            let TAtomic::Array(array) = atomic else {
                unreachable!("the checker runs a collection method only on a collection, not on {atomic:?}");
            };
            let (_, value) = get_array_parameters(array, self.codebase);
            element_type = Some(add_optional_union_type(value, element_type.as_ref(), self.codebase));
        }

        element_type.unwrap_or_else(|| unreachable!("a collection method's receiver is a collection"))
    }

    /// The positions of the arguments of the method call `call`, null-safe or not, that go in as its receiver's key.
    /// The analyzer checks a call on a `Map` against `Sharp\MapMethods<K, V>`, whose `K` the `Map`'s key type fills
    /// (spec section 12), so an argument goes in as a key when the call runs a `MapMethods` method and its parameter
    /// is a template parameter bounded by `array-key`, as in `get(K $key)`. The method is the one the analyzer checked
    /// the call against, which a `List` emptied by `[]` keeps. The caller asks only about a call on a value.
    pub(crate) fn key_arguments(&self, call: &Expression) -> Vec<usize> {
        let arguments = match call {
            Expression::Call(Call::Method(call)) => &call.argument_list,
            Expression::Call(Call::NullSafeMethod(call)) => &call.argument_list,
            _ => unreachable!("only a method call has arguments a receiver takes as its key"),
        };
        let mut callees = self.artifacts.get_callees(call);
        let (Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. }), None) =
            (callees.next(), callees.next())
        else {
            return Vec::new();
        };
        if !class.as_bytes().eq_ignore_ascii_case(b"Sharp\\MapMethods") {
            return Vec::new();
        }
        let metadata = self
            .codebase
            .get_method(class.as_bytes(), method.as_bytes())
            .unwrap_or_else(|| unreachable!("the analyzer resolved `Map.{method}()` from the codebase"));

        arguments
            .arguments
            .iter()
            .enumerate()
            .filter(|(position, argument)| {
                let parameter = match argument {
                    Argument::Positional(argument) if argument.ellipsis.is_none() => metadata.parameters.get(*position),
                    Argument::Positional(_) => None,
                    Argument::Named(argument) => metadata
                        .parameters
                        .iter()
                        .find(|parameter| parameter.name.0.as_bytes().strip_prefix(b"$") == Some(argument.name.value)),
                };

                parameter.is_some_and(takes_key)
            })
            .map(|(position, _)| position)
            .collect()
    }

    /// The collection, `List`, `Map` or `Set`, and the name in lowercase of the collection method the analyzer checked
    /// the method call `call` against, null-safe or not: a method of `Sharp\ListMethods`, `Sharp\MapMethods` or
    /// `Sharp\SetMethods`. None for a call of any other method.
    pub(crate) fn collection_method(&self, call: &Expression) -> Option<(&'static str, Vec<u8>)> {
        let mut callees = self.artifacts.get_callees(call);
        let (Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. }), None) =
            (callees.next(), callees.next())
        else {
            return None;
        };
        let methods: [(&str, &[u8]); 3] =
            [("List", b"Sharp\\ListMethods"), ("Map", b"Sharp\\MapMethods"), ("Set", b"Sharp\\SetMethods")];
        let collection = methods
            .into_iter()
            .find_map(|(collection, methods)| class.as_bytes().eq_ignore_ascii_case(methods).then_some(collection))?;

        Some((collection, method.as_bytes().to_ascii_lowercase()))
    }

    /// The property `class` declares or inherits by the name `property`, if any.
    fn property_declaration(&self, class: &[u8], property: &[u8]) -> Option<Declaration> {
        let variable = [b"$", property].concat();
        let metadata = self.codebase.get_declaring_property(class, &variable)?;
        let kind = if metadata.flags.is_static() {
            DeclarationKind::StaticProperty
        } else {
            DeclarationKind::Property {
                typed: self.root_property(class, &variable).type_declaration_metadata.is_some(),
            }
        };

        Some(Declaration {
            kind,
            class: self.codebase.get_declaring_property_class(class, &variable).unwrap_or_else(|| word(class)),
            name: word(property),
            public: metadata.read_visibility.is_public(),
        })
    }

    /// The root declaration of the property `variable` that `class` declares or inherits: the one in the class whose
    /// parent does not declare it.
    fn root_property(&self, class: &[u8], variable: &[u8]) -> &'analysis PropertyMetadata {
        let declaring = |class: &[u8]| self.codebase.get_declaring_property_class(class, variable);
        let mut root =
            declaring(class).unwrap_or_else(|| unreachable!("`{}` declares or inherits the property", word(class)));
        while let Some(parent) = self
            .codebase
            .get_class_like(root.as_bytes())
            .and_then(|metadata| metadata.direct_parent_class)
            .and_then(|parent| declaring(parent.as_bytes()))
        {
            root = parent;
        }

        self.codebase
            .get_declaring_property(root.as_bytes(), variable)
            .unwrap_or_else(|| unreachable!("`{root}` declares the property"))
    }

    /// The class declaring the static method `name` a PHP# operator on operands of the types `operands` runs as, as the
    /// checker chose it: the one the left operand's class declares or inherits, else the right one's, as `App\Money`
    /// for `op_Addition` on two `Order`s that inherit it. Operands with no instance of one class, enums aside, run none.
    pub(crate) fn operator_class(&self, name: &[u8], operands: &[&TUnion]) -> Option<Word> {
        operands.iter().find_map(|operand| {
            let classes = receiver_classes(operand)?;
            let [class] = classes.as_slice() else {
                return None;
            };
            if matches!(self.class_declaration(class).kind, DeclarationKind::Enum { .. }) {
                return None;
            }
            let method = self.method_declaration(class, name)?;

            (method.kind == DeclarationKind::StaticMethod).then(|| self.class_declaration(method.class.as_bytes()).name)
        })
    }

    /// The method `class` declares or inherits by the name `method`, if any.
    fn method_declaration(&self, class: &[u8], method: &[u8]) -> Option<Declaration> {
        let metadata = self.codebase.get_declaring_method(class, method)?.method_metadata.as_ref()?;
        let kind = if metadata.is_static {
            DeclarationKind::StaticMethod
        } else {
            DeclarationKind::Method { overridable: !metadata.is_final }
        };
        let declaring =
            self.codebase.get_declaring_method_identifier(&MethodIdentifier::new(word(class), word(method)));

        Some(Declaration {
            kind,
            class: declaring.get_class_name(),
            name: word(method),
            public: metadata.visibility.is_public(),
        })
    }
}

/// The fully qualified names of the classes a value of `r#type` can be, leaving out `null`, or none when part of the
/// type is no class, such as a collection. A value of a type parameter can be the classes of its bound.
pub(crate) fn receiver_classes(r#type: &TUnion) -> Option<Vec<&[u8]>> {
    Some(receiver_intersections(r#type)?.into_iter().map(|classes| classes[0]).collect())
}

/// The classes a value of `r#type` can be, as [`receiver_classes`] gives them, each followed by the classes it is
/// intersected with, as a type parameter bounded by `Entity & Shareable` is.
pub(crate) fn receiver_intersections(r#type: &TUnion) -> Option<Vec<Vec<&[u8]>>> {
    fn class(object: &TObject) -> Option<&[u8]> {
        match object {
            TObject::Named(object) => Some(object.name.as_bytes()),
            TObject::Enum(object) => Some(object.name.as_bytes()),
            _ => None,
        }
    }

    let mut receivers = Vec::new();
    for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
        match atomic {
            TAtomic::Object(object) if class(object).is_some() => {
                let parts = object.get_intersection_types().unwrap_or_default().iter().filter_map(|part| match part {
                    TAtomic::Object(part) => class(part),
                    _ => None,
                });
                receivers.push(class(object).into_iter().chain(parts).collect());
            }
            TAtomic::GenericParameter(parameter) => receivers.extend(receiver_intersections(&parameter.constraint)?),
            _ => return None,
        }
    }

    (!receivers.is_empty()).then_some(receivers)
}

/// The fully qualified names of the classes a class value of `r#type` holds, leaving out `null`, or none when part of
/// the type is no class value. A `Class<T>` value holds `T` or a subclass, which has the same kind of member.
pub(crate) fn class_value_classes(r#type: &TUnion) -> Option<Vec<&[u8]>> {
    let classes: Vec<&[u8]> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| match atomic {
            TAtomic::Scalar(TScalar::ClassLikeString(TClassLikeString::Literal { value })) => Some(value.as_bytes()),
            TAtomic::Scalar(TScalar::ClassLikeString(
                TClassLikeString::Generic { constraint, .. } | TClassLikeString::OfType { constraint, .. },
            )) => match constraint.as_ref() {
                TAtomic::Object(TObject::Named(object)) => Some(object.name.as_bytes()),
                TAtomic::Object(TObject::Enum(object)) => Some(object.name.as_bytes()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<_>>()?;

    (!classes.is_empty()).then_some(classes)
}

/// Whether `parameter` takes a collection's key: its type is a template parameter bounded by `array-key`.
fn takes_key(parameter: &FunctionLikeParameterMetadata) -> bool {
    parameter.type_metadata.as_ref().is_some_and(|r#type| {
        matches!(r#type.type_union.types.as_ref(), [TAtomic::GenericParameter(template)] if template.constraint.is_array_key())
    })
}

/// The kind of member every class a receiver can be declares, which the checker requires to be one kind. Its details,
/// such as whether a property is typed, may differ between the classes.
pub(crate) fn agreed_kind(mut kinds: impl Iterator<Item = DeclarationKind>) -> DeclarationKind {
    let kind = kinds.next().unwrap_or_else(|| unreachable!("a receiver's type names at least one class"));
    if kinds.any(|other| std::mem::discriminant(&other) != std::mem::discriminant(&kind)) {
        unreachable!("the checker refuses a member whose kind differs across the receiver's classes");
    }

    kind
}

/// How a type text writes a type parameter.
#[derive(Clone, Copy)]
enum Parameter {
    /// As `Any?`, which stands in for a type parameter no type argument is given for, as in a bound that names a
    /// type parameter of its own class: `Comparable<Any?>` for `TItem : Comparable<TItem>`.
    Any,
    /// A class's as `$` and its index among its class's type parameters, `$0` for the first, and a method's as `#` and
    /// its index among the method's.
    Index,
    /// A class's as [`Parameter::Index`] writes it, and a method's as its bound, which a call from plain PHP gives it.
    Bound,
}

/// `r#type` in the one PHP# spelling the engine parses, the type text of php-sharp's decision 72: a class by its full
/// dotted name as it is declared, the built-in types, `List<T>`, `Map<K, V>`, `Set<T>`, `Iterable<T>`, `Class<T>`,
/// `Function<R(P1, P2)>`, a type parameter as `Any?`, `T?`, `(A|B)?`, `A & B`, and an intersection in parentheses
/// inside a union or a nullable type, `(A & B)|C` and `(A & B)?`. Union and intersection members are sorted by their
/// text, and a space comes only after a comma and around `&`.
pub(crate) fn type_text(r#type: &TUnion, codebase: &CodebaseMetadata) -> String {
    text(r#type, codebase, Parameter::Any)
}

/// [`type_text`], with each type parameter of a class written as `parameter` says.
fn text(r#type: &TUnion, codebase: &CodebaseMetadata, parameter: Parameter) -> String {
    if let Some(TAtomic::Mixed(mixed)) = r#type.types.iter().find(|atomic| atomic.is_mixed()) {
        return if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned();
    }

    let mut members: Vec<(String, bool)> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| intersection_text(atomic, codebase, parameter))
        .collect();
    members.sort_unstable();
    members.dedup();
    let grouped =
        |(text, intersection): &(String, bool)| if *intersection { format!("({text})") } else { text.clone() };
    let union = || members.iter().map(grouped).collect::<Vec<_>>().join("|");

    match (r#type.has_null(), members.as_slice()) {
        (false, [(member, _)]) => member.clone(),
        (false, _) => union(),
        (true, []) => "null".to_owned(),
        (true, [member]) => format!("{}?", grouped(member)),
        (true, _) => format!("({})?", union()),
    }
}

/// `atomic` and each type it intersects, `A & B` with its members sorted by their text, and whether it intersects any.
fn intersection_text(atomic: &TAtomic, codebase: &CodebaseMetadata, parameter: Parameter) -> (String, bool) {
    let Some(intersected) = atomic.get_intersection_types().filter(|intersected| !intersected.is_empty()) else {
        return (atomic_text(atomic, codebase, parameter), false);
    };

    let mut members: Vec<String> =
        std::iter::once(atomic).chain(intersected).map(|member| atomic_text(member, codebase, parameter)).collect();
    members.sort_unstable();
    members.dedup();

    (members.join(" & "), true)
}

fn atomic_text(atomic: &TAtomic, codebase: &CodebaseMetadata, parameter: Parameter) -> String {
    let type_text = |r#type: &TUnion| text(r#type, codebase, parameter);
    let list = |types: &mut dyn Iterator<Item = &TUnion>| types.map(type_text).collect::<Vec<_>>().join(", ");
    let parameter_text =
        |name: Word, defining_entity: GenericParent, bound: &dyn Fn() -> String| match (parameter, defining_entity) {
            (Parameter::Any, _) => "Any?".to_owned(),
            (Parameter::Index | Parameter::Bound, GenericParent::ClassLike(class)) => {
                let index = codebase
                    .get_class_like(class.as_bytes())
                    .and_then(|class| class.template_types.get_index_of(&name))
                    .unwrap_or_else(|| unreachable!("a class declares each of its type parameters, not `{name}`"));

                format!("${index}")
            }
            (Parameter::Index, GenericParent::FunctionLike((class, method))) => {
                let index = codebase
                    .get_method(class.as_bytes(), method.as_bytes())
                    .and_then(|method| method.template_types.get_index_of(&name))
                    .unwrap_or_else(|| unreachable!("a method declares each of its type parameters, not `{name}`"));

                format!("#{index}")
            }
            (Parameter::Bound, GenericParent::FunctionLike(_)) => bound(),
        };

    let Some(sharp_type) = get_sharp_type(atomic) else {
        unreachable!("the analyzer refuses a type PHP# can't write, not `{}`", atomic.get_id())
    };

    match sharp_type {
        SharpType::Int => "int".to_owned(),
        SharpType::Float => "float".to_owned(),
        SharpType::Bool => "bool".to_owned(),
        SharpType::String => "string".to_owned(),
        SharpType::Void => "void".to_owned(),
        SharpType::Object => "Object".to_owned(),
        SharpType::Enum(name) | SharpType::Class(name, []) => class_text(name, codebase),
        SharpType::Class(name, arguments) => format!("{}<{}>", class_text(name, codebase), list(&mut arguments.iter())),
        SharpType::List(element_type) => format!("List<{}>", type_text(element_type)),
        SharpType::Set(element_type) => format!("Set<{}>", type_text(element_type)),
        SharpType::Map(key, value) => format!("Map<{}>", list(&mut [key, value].into_iter())),
        SharpType::Iterable(value) => format!("Iterable<{}>", type_text(value)),
        SharpType::Parameter(generic) => {
            parameter_text(generic.parameter_name, generic.defining_entity, &|| type_text(&generic.constraint))
        }
        SharpType::ClassValue(class_value) => {
            let class = match class_value {
                TClassLikeString::Literal { value } => class_text(*value, codebase),
                TClassLikeString::OfType { constraint, .. } => atomic_text(constraint, codebase, parameter),
                TClassLikeString::Generic { parameter_name, defining_entity, constraint, .. } => {
                    parameter_text(*parameter_name, *defining_entity, &|| atomic_text(constraint, codebase, parameter))
                }
                TClassLikeString::Any { .. } => "Object".to_owned(),
            };

            format!("Class<{class}>")
        }
        SharpType::Function(signature) => {
            let written = |r#type: Option<&TUnion>| r#type.map_or_else(|| "Any?".to_owned(), type_text);
            let parameters: Vec<String> =
                signature.get_parameters().iter().map(|parameter| written(parameter.get_type_signature())).collect();

            format!("Function<{}({})>", written(signature.get_return_type()), parameters.join(", "))
        }
    }
}

/// The full dotted name of the class `name`, as it is declared.
fn class_text(name: Word, codebase: &CodebaseMetadata) -> String {
    let declared = codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name);

    declared.as_str_lossy().trim_start_matches('\\').replace('\\', ".")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mago_codex::metadata::CodebaseMetadata;
    use mago_codex::misc::GenericParent;
    use mago_codex::ttype::TType;
    use mago_codex::ttype::atomic::TAtomic;
    use mago_codex::ttype::atomic::array::TArray;
    use mago_codex::ttype::atomic::callable::TCallable;
    use mago_codex::ttype::atomic::callable::TCallableSignature;
    use mago_codex::ttype::atomic::callable::parameter::TCallableParameter;
    use mago_codex::ttype::atomic::generic::TGenericParameter;
    use mago_codex::ttype::atomic::iterable::TIterable;
    use mago_codex::ttype::atomic::mixed::TMixed;
    use mago_codex::ttype::atomic::object::TObject;
    use mago_codex::ttype::atomic::object::named::TNamedObject;
    use mago_codex::ttype::atomic::scalar::TScalar;
    use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
    use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeStringKind;
    use mago_codex::ttype::get_bool;
    use mago_codex::ttype::get_float;
    use mago_codex::ttype::get_int;
    use mago_codex::ttype::get_keyed_array;
    use mago_codex::ttype::get_list;
    use mago_codex::ttype::get_mixed;
    use mago_codex::ttype::get_null;
    use mago_codex::ttype::get_object;
    use mago_codex::ttype::get_string;
    use mago_codex::ttype::get_void;
    use mago_codex::ttype::union::TUnion;
    use mago_codex::ttype::wrap_atomic;
    use mago_word::word;

    use super::Parameter;
    use super::receiver_intersections;
    use super::type_text;

    fn text(r#type: &TUnion) -> String {
        type_text(r#type, &CodebaseMetadata::default())
    }

    fn class(name: &str, arguments: Vec<TUnion>) -> TUnion {
        let arguments = (!arguments.is_empty()).then_some(arguments);

        wrap_atomic(TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(word(name), arguments))))
    }

    fn set(element: TUnion) -> TUnion {
        wrap_atomic(TAtomic::Array(TArray::Set(Arc::new(element))))
    }

    fn union(members: &[TUnion]) -> TUnion {
        TUnion::from_vec(members.iter().flat_map(|member| member.types.iter().cloned()).collect())
    }

    fn function(return_type: TUnion, parameters: &[TUnion]) -> TUnion {
        let parameters = parameters
            .iter()
            .map(|parameter| TCallableParameter::new(Some(Arc::new(parameter.clone())), false, false, false))
            .collect();

        wrap_atomic(TAtomic::Callable(TCallable::Signature(
            TCallableSignature::new(false, true)
                .with_parameters(parameters)
                .with_return_type(Some(Arc::new(return_type))),
        )))
    }

    #[test]
    fn a_class_is_its_full_dotted_name_and_the_built_in_types_are_their_names() {
        assert_eq!(text(&class("App\\Order", vec![])), "App.Order");
        assert_eq!(text(&class("Order", vec![])), "Order");
        assert_eq!(text(&get_int()), "int");
        assert_eq!(text(&get_float()), "float");
        assert_eq!(text(&get_bool()), "bool");
        assert_eq!(text(&get_string()), "string");
        assert_eq!(text(&get_void()), "void");
        assert_eq!(text(&get_object()), "Object");
        assert_eq!(text(&wrap_atomic(TAtomic::Mixed(TMixed::new().with_is_non_null(true)))), "Any");
        assert_eq!(text(&get_mixed()), "Any?");
    }

    #[test]
    fn a_generic_type_writes_its_arguments_nested_with_a_space_after_each_comma() {
        let order = class("App\\Order", vec![]);
        let page = class("App\\PaginatedList", vec![get_keyed_array(get_string(), get_list(order.clone()))]);

        assert_eq!(text(&page), "App.PaginatedList<Map<string, List<App.Order>>>");
        assert_eq!(text(&class("App\\Pair", vec![order.clone(), get_int()])), "App.Pair<App.Order, int>");
        assert_eq!(text(&class("App\\Box", vec![set(get_int())])), "App.Box<Set<int>>");
        assert_eq!(text(&set(order.clone())), "Set<App.Order>");
        assert_eq!(text(&wrap_atomic(TAtomic::Iterable(TIterable::of_value(Arc::new(order))))), "Iterable<App.Order>");
    }

    #[test]
    fn a_nullable_type_ends_in_a_question_mark_and_a_nullable_union_is_in_parentheses() {
        assert_eq!(text(&get_int().as_nullable()), "int?");
        assert_eq!(text(&class("App\\Order", vec![]).as_nullable()), "App.Order?");
        assert_eq!(text(&union(&[get_string(), get_int()]).as_nullable()), "(int|string)?");
        assert_eq!(text(&get_list(get_int().as_nullable())), "List<int?>");
    }

    #[test]
    fn union_members_are_sorted_by_their_text() {
        let union = union(&[get_string(), class("App\\Zone", vec![]), get_int(), class("App\\Area", vec![])]);

        assert_eq!(text(&union), "App.Area|App.Zone|int|string");
    }

    #[test]
    fn an_intersection_joins_its_members_sorted_and_stands_in_parentheses_in_a_union_or_a_nullable_type() {
        let mut shared = TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\Shareable"))));
        shared.add_intersection_type(TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\DatabaseEntity")))));
        let shared = wrap_atomic(shared);

        assert_eq!(text(&shared), "App.DatabaseEntity & App.Shareable");
        assert_eq!(text(&get_list(shared.clone())), "List<App.DatabaseEntity & App.Shareable>");
        assert_eq!(text(&union(&[get_int(), shared.clone()])), "(App.DatabaseEntity & App.Shareable)|int");
        assert_eq!(
            text(&union(&[class("App\\Zone", vec![]), shared.clone()])),
            "(App.DatabaseEntity & App.Shareable)|App.Zone"
        );
        assert_eq!(text(&shared.clone().as_nullable()), "(App.DatabaseEntity & App.Shareable)?");
        assert_eq!(text(&union(&[get_int(), shared]).as_nullable()), "((App.DatabaseEntity & App.Shareable)|int)?");
    }

    #[test]
    fn a_function_type_writes_its_return_type_then_its_parameter_types() {
        let order = class("App\\Order", vec![]);

        assert_eq!(text(&function(get_bool(), &[order, get_int()])), "Function<bool(App.Order, int)>");
        assert_eq!(text(&function(get_void(), &[])), "Function<void()>");
        assert_eq!(
            text(&function(get_list(get_int()).as_nullable(), &[get_string().as_nullable()])),
            "Function<List<int>?(string?)>"
        );
    }

    #[test]
    fn a_type_parameter_is_any_and_a_class_value_names_its_class() {
        let parameter = TGenericParameter::new(
            word("TItem"),
            Arc::new(get_mixed()),
            GenericParent::ClassLike(word("App\\PaginatedList")),
        );
        let class_value = TClassLikeString::literal(word("App\\Order"));

        assert_eq!(text(&wrap_atomic(TAtomic::GenericParameter(parameter))), "Any?");
        assert_eq!(text(&wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(class_value)))), "Class<App.Order>");
    }

    /// A class value bounded by a class names it, one of a type parameter writes the type parameter, and one of any
    /// class names `Object`. A type that is only `null` is `null`.
    #[test]
    fn a_class_value_names_its_bound_its_type_parameter_or_object() {
        let order = TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\Order"))));
        let class_value = |class_value| text(&wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(class_value))));
        let of_type = TClassLikeString::of_type(TClassLikeStringKind::Class, order.clone());
        let generic = TClassLikeString::generic(
            TClassLikeStringKind::Class,
            word("TItem"),
            GenericParent::ClassLike(word("App\\PaginatedList")),
            order,
        );

        assert_eq!(class_value(of_type), "Class<App.Order>");
        assert_eq!(class_value(generic), "Class<Any?>");
        assert_eq!(class_value(TClassLikeString::class_string()), "Class<Object>");
        assert_eq!(text(&get_null()), "null");
    }

    /// In a bound, a method's own type parameter is its bound, which a call from plain PHP gives it.
    #[test]
    fn a_bound_writes_a_method_type_parameter_as_its_bound() {
        let parameter = TGenericParameter::new(
            word("TOther"),
            Arc::new(class("App\\Order", vec![])),
            GenericParent::FunctionLike((word("App\\Report"), word("pick"))),
        );
        let bound = class("App\\Comparable", vec![wrap_atomic(TAtomic::GenericParameter(parameter))]);

        assert_eq!(super::text(&bound, &CodebaseMetadata::default(), Parameter::Bound), "App.Comparable<App.Order>");
    }

    /// A receiver's intersection names its classes, and a part that is no class, as a PHP `@template` it is joined
    /// with, names none.
    #[test]
    fn a_receiver_intersection_names_only_its_classes() {
        let mut receiver = TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\Order"))));
        receiver.add_intersection_type(TAtomic::GenericParameter(TGenericParameter::new(
            word("T"),
            Arc::new(get_mixed()),
            GenericParent::ClassLike(word("App\\Box")),
        )));

        assert_eq!(receiver_intersections(&wrap_atomic(receiver)), Some(vec![vec![b"App\\Order".as_slice()]]));
    }
}
