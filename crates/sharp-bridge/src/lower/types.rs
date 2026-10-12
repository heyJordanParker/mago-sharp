use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_analyzer::artifacts::CallTarget;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ParsedField;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::union::TUnion;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Call;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_word::Word;
use mago_word::word;

use super::inline::InlineForm;
use super::inline::InlineForms;
use super::inline::key;

/// The checker's types for one file. The lowering reads a type only through these queries, so `--assert-types` can
/// turn each answer it used into a runtime guard.
pub struct Types<'analysis> {
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
        names: ResolvedNames<'analysis>,
        artifacts: &'analysis AnalysisArtifacts,
        codebase: &'analysis CodebaseMetadata,
        inline_forms: &'analysis InlineForms,
    ) -> Self {
        Self { names, artifacts, codebase, inline_forms }
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
        let declared = |kind, public| Declaration { kind, class: word(class), name: word(member), public };

        if self.codebase.get_enum_case(class, member).is_some() {
            declared(DeclarationKind::EnumCase, true)
        } else if let Some(constant) = self.codebase.get_class_constant(class, member) {
            declared(DeclarationKind::Constant, constant.visibility.is_public())
        } else if let Some(declaration) = self.property_declaration(class, member) {
            declaration
        } else if let Some(declaration) = self.method_declaration(class, member) {
            declaration
        } else if self.codebase.method_exists(class, b"__get") {
            declared(DeclarationKind::Property { typed: false }, true)
        } else {
            unreachable!(
                "the checker refuses `{}.{}`, which names no member",
                String::from_utf8_lossy(class),
                String::from_utf8_lossy(member)
            )
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

    /// The name and type of each property `==` compares on the struct `class`, in the order the struct declares them.
    pub(crate) fn struct_properties(&self, class: &[u8]) -> Vec<(&'analysis [u8], &'analysis TUnion)> {
        let metadata = self
            .codebase
            .get_class_like(class)
            .unwrap_or_else(|| unreachable!("the codex registers the struct `{}`", String::from_utf8_lossy(class)));

        metadata
            .struct_properties()
            .into_iter()
            .map(|property| {
                let name = property.name.0.as_bytes();
                let Some(r#type) = property.type_metadata.as_ref().or(property.type_declaration_metadata.as_ref())
                else {
                    unreachable!("semantics refuses a PHP# property without a type");
                };

                (name.strip_prefix(b"$").unwrap_or(name), &r#type.type_union)
            })
            .collect()
    }

    /// Whether the class-like `class` is a PHP# struct.
    pub(crate) fn is_struct(&self, class: &[u8]) -> bool {
        self.codebase.get_class_like(class).is_some_and(|metadata| metadata.flags.is_struct())
    }

    /// The backing type of the backed enum or the backed struct `class`, if it has one.
    pub(crate) fn backing_type(&self, class: &[u8]) -> Option<&'analysis TAtomic> {
        self.codebase.get_class_like(class)?.enum_type.as_ref()
    }

    /// The fields the struct `class`'s `parse` reads, as the codex gives them.
    pub(crate) fn struct_parse_fields(&self, class: &[u8]) -> Vec<ParsedField<'analysis>> {
        self.codebase.struct_parse_fields(class)
    }

    /// The key type and the element type of a collection.
    pub(crate) fn array_parameters(&self, array: &TArray) -> (TUnion, TUnion) {
        get_array_parameters(array, self.codebase)
    }

    /// The backing type of the struct `class`, or none when `class` is no struct or declares no backing type.
    pub(crate) fn struct_backing_type(&self, class: &[u8]) -> Option<&'analysis TAtomic> {
        self.codebase.get_class_like(class).filter(|metadata| metadata.flags.is_struct())?.enum_type.as_ref()
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
/// type is no class, such as a collection.
pub(crate) fn receiver_classes(r#type: &TUnion) -> Option<Vec<&[u8]>> {
    let classes: Vec<&[u8]> = r#type
        .types
        .iter()
        .filter(|atomic| !atomic.is_null())
        .map(|atomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => Some(object.name.as_bytes()),
            TAtomic::Object(TObject::Enum(object)) => Some(object.name.as_bytes()),
            _ => None,
        })
        .collect::<Option<_>>()?;

    (!classes.is_empty()).then_some(classes)
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
