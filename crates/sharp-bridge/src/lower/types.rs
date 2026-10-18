use mago_analyzer::CodebasePatternNames;
use mago_analyzer::artifacts::AnalysisArtifacts;
use mago_analyzer::artifacts::CallTarget;
use mago_analyzer::effects::Effects;
use mago_codex::defaults_trait_name;
use mago_codex::identifier::function_like::FunctionLikeIdentifier;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::class_like::ParsedField;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::ModuleMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::metadata::property::PropertyMetadata;
use mago_codex::misc::GenericParent;
use mago_codex::ttype::SharpType;
use mago_codex::ttype::TType;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::callable::TCallable;
use mago_codex::ttype::atomic::callable::parameter::TCallableParameter;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::atomic::scalar::class_like_string::TClassLikeString;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::comparator::union_comparator::can_expression_types_be_identical;
use mago_codex::ttype::get_array_parameters;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_sharp_type;
use mago_codex::ttype::is_written_in_sharp;
use mago_codex::ttype::template::variance::Variance;
use mago_codex::ttype::union::TUnion;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::NullSafeFunctionCall;
use mago_syntax::cst::PropertyAccess;
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
    /// The effects of every PHP# body, which a function's type lists.
    effects: &'analysis Effects,
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
    Trait,
    /// `backed` is whether each case has a backing value.
    Enum {
        backed: bool,
    },
    Constant,
    EnumCase,
    /// The one object of a plain case of an enum with a case that carries data, spec section 20, which the static
    /// `instance()` of the case's class gives. The declaration's class is the case's class.
    CaseObject,
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
        effects: &'analysis Effects,
        inline_forms: &'analysis InlineForms,
    ) -> Self {
        Self { file, names, artifacts, codebase, effects, inline_forms }
    }

    pub(crate) fn names(&self) -> &ResolvedNames<'analysis> {
        &self.names
    }

    /// What the PHP of a pattern depends on, as the analysis answered it.
    pub(crate) fn pattern_names(&self) -> CodebasePatternNames<'_, 'analysis> {
        CodebasePatternNames { names: &self.names, codebase: self.codebase, artifacts: self.artifacts }
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

    /// Whether `value`, a call of plain PHP or a read of a property plain PHP declares, may be an `int`-keyed array
    /// whose keys have gaps, which the analysis reads as a `List`, as `get_sharp_received_type` says: plain PHP declares
    /// a keyed array, and the analysis types the value as a `List`. A node the lowering builds has no type.
    pub(crate) fn arrives_as_list(&self, value: &Expression) -> bool {
        let is_list =
            |atomic: &TAtomic| matches!(atomic, TAtomic::Array(TArray::List(list)) if list.tuple_names.is_none());
        let declared = match value.unparenthesized() {
            Expression::Call(_) => self
                .plain_php_call(value)
                .and_then(|(_, metadata)| metadata.return_type_metadata.as_ref().map(|declared| &declared.type_union)),
            Expression::Access(Access::Property(_)) => self.property_type(value),
            _ => unreachable!("the lowering asks this only of a plain PHP call or property read"),
        };

        declared.is_some_and(|declared| {
            declared.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_))))
        }) && self.artifacts.get_expression_type(value).is_some_and(|r#type| r#type.types.iter().any(is_list))
    }

    /// Whether the collection `expression` may hold a `Map`, whose element a loop reads as its entry, spec section 17.
    pub(crate) fn holds_map(&self, expression: &Expression) -> bool {
        self.expression_type(expression).types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_))))
    }

    /// The position of the element `access` reads by its name in the tuple its object holds, spec section 30, or `None`
    /// when the object holds no tuple. The checker refuses a name the tuple does not write.
    pub(crate) fn tuple_element_index(&self, access: &PropertyAccess) -> Option<usize> {
        let ClassLikeMemberSelector::Identifier(name) = &access.property else {
            unreachable!("check_slice refuses the member name `{}`", access.property);
        };
        // A class a static member is read on has no type.
        let TAtomic::Array(TArray::List(list)) = self.artifacts.get_expression_type(access.object)?.types.first()?
        else {
            return None;
        };

        list.tuple_elements()?
            .iter()
            .position(|(_, element)| element.is_some_and(|element| element.as_bytes() == name.value))
    }

    /// The declaration the fully qualified class name `class` resolves to.
    #[must_use]
    pub fn class_declaration(&self, class: &[u8]) -> Declaration {
        let metadata = self.codebase.get_class_like(class).unwrap_or_else(|| {
            unreachable!("the checker refuses the unknown class `{}`", String::from_utf8_lossy(class))
        });

        let kind = if metadata.kind.is_interface() {
            DeclarationKind::Interface
        } else if metadata.kind.is_trait() {
            DeclarationKind::Trait
        } else if metadata.kind.is_enum() {
            DeclarationKind::Enum { backed: metadata.enum_type.is_some() }
        } else {
            DeclarationKind::Class
        };

        Declaration { kind, class: metadata.original_name, name: metadata.original_name, public: true }
    }

    /// The traits the fully qualified class-like name `class` uses itself, spec section 22: the traits its `header`
    /// names, then the traits of default bodies [`CodebaseMetadata::get_defaults_traits`] gives it. A subclass does not
    /// use its parent's traits again, as `ClassLikeMetadata::used_traits` lists them.
    #[must_use]
    pub fn used_traits(&self, class: &[u8], header: Option<&Inheritance>) -> Vec<Word> {
        let mut traits: Vec<Word> = header
            .into_iter()
            .flat_map(Inheritance::names)
            .filter_map(|name| self.codebase.get_trait(self.names.get(&name)))
            .map(|header_trait| header_trait.original_name)
            .collect();
        let metadata = self.scanned(class);
        traits.extend(self.codebase.get_defaults_traits(metadata).iter().map(|defaults| defaults.original_name));

        traits
    }

    /// The metadata of the fully qualified class-like name `class`, which the bridge lowers.
    fn scanned(&self, class: &[u8]) -> &'analysis ClassLikeMetadata {
        let Some(metadata) = self.codebase.get_class_like(class) else {
            unreachable!(
                "the codex scans each class-like the bridge lowers, `{}` included",
                String::from_utf8_lossy(class)
            );
        };

        metadata
    }

    /// The computed properties with a default body, spec section 22, that the fully qualified class-like name `class`
    /// gives itself: each one an interface whose trait [`CodebaseMetadata::get_defaults_traits`] gives it declares and
    /// the class does not, by its name with `$` and its interface's full name.
    #[must_use]
    pub fn default_body_properties(&self, class: &[u8]) -> Vec<(Word, Word)> {
        let metadata = self.scanned(class);
        let interfaces = self.defaults_interfaces(metadata);
        let mut properties = Vec::new();
        for (_, interface) in &interfaces {
            for (name, property) in &interface.properties {
                if property.hooks.values().any(|accessor| !accessor.is_abstract)
                    && metadata.declaring_property_ids.get(name) != Some(&metadata.name)
                    && Self::replacing(&interfaces, interface, |other| {
                        other.declaring_property_ids.get(name) == Some(&other.name)
                    })
                    .is_none()
                {
                    properties.push((*name, interface.original_name));
                }
            }
        }

        properties
    }

    /// The default bodies the fully qualified class-like name `class` uses itself, spec section 22: each method of the
    /// traits [`CodebaseMetadata::get_defaults_traits`] gives it, with its trait, its interface's full name, and the
    /// trait of a sub-interface whose body replaces it, in declaration order.
    #[must_use]
    pub fn default_bodies(&self, class: &[u8]) -> Vec<(Word, Word, Word, Option<Word>)> {
        let metadata = self.scanned(class);
        let interfaces = self.defaults_interfaces(metadata);

        interfaces
            .iter()
            .flat_map(|(defaults, interface)| {
                // A computed property's default body is already the protected `designKey@App\HasDesign`.
                defaults.methods.iter().filter(|method| !method.as_bytes().contains(&b'@')).filter_map(|method| {
                    let body = self.codebase.get_method(defaults.name.as_bytes(), method.as_bytes())?;
                    let replacing = Self::replacing(&interfaces, interface, |other| {
                        self.codebase
                            .get_method(
                                defaults_trait_name(other.original_name.as_bytes()).as_bytes(),
                                method.as_bytes(),
                            )
                            .is_some()
                    });

                    Some((defaults.original_name, interface.original_name, body.original_name, replacing))
                })
            })
            .collect()
    }

    /// The traits of default bodies `metadata` uses, each beside its interface.
    fn defaults_interfaces(
        &self,
        metadata: &ClassLikeMetadata,
    ) -> Vec<(&'analysis ClassLikeMetadata, &'analysis ClassLikeMetadata)> {
        self.codebase
            .get_defaults_traits(metadata)
            .into_iter()
            .filter_map(|defaults| Some((defaults, self.codebase.get_defaults_interface(defaults)?)))
            .collect()
    }

    /// The trait of the sub-interface among `interfaces` whose own body, which `gives` tells, replaces `interface`'s
    /// default body, as the most specific interface's does in Java, C# and Kotlin.
    fn replacing(
        interfaces: &[(&ClassLikeMetadata, &ClassLikeMetadata)],
        interface: &ClassLikeMetadata,
        gives: impl Fn(&ClassLikeMetadata) -> bool,
    ) -> Option<Word> {
        interfaces
            .iter()
            .find(|(_, other)| other.all_parent_interfaces.contains(&interface.name) && gives(other))
            .map(|(defaults, _)| defaults.original_name)
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

    /// How many type parameters the fully qualified class name `class` declares when it is a PHP# generic class, whose
    /// static members take its type arguments as their leading own ones. None for any other class.
    pub(crate) fn static_type_parameters(&self, class: &[u8]) -> Option<usize> {
        let metadata = self.codebase.get_class_like(class)?;

        (metadata.flags.is_sharp() && !metadata.template_types.is_empty()).then_some(metadata.template_types.len())
    }

    /// The variance of the type parameters of the fully qualified class name `class` when it is a PHP# class-like and
    /// one of them is `out` or `in`, spec section 11.1: `+` for `out`, `-` for `in` and `=` for neither, in declaration
    /// order, which the engine reads where it checks a value against a generic type.
    pub(crate) fn variance(&self, class: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_class_like(class)?;
        if !metadata.flags.is_sharp() || metadata.template_variance.iter().all(Variance::is_invariant) {
            return None;
        }

        Some(
            metadata
                .template_variance
                .iter()
                .map(|variance| match variance {
                    Variance::Covariant => '+',
                    Variance::Contravariant => '-',
                    Variance::Invariant => '=',
                    Variance::Bivariant => unreachable!("a PHP# type parameter is `out`, `in` or neither"),
                })
                .collect(),
        )
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
    /// reads them, as the prelude's `Sharp\Internal\ListMethods` and `Sharp\Internal\MapMethods` declare a list's and a
    /// map's. A method of any project class, PHP# or plain PHP, may run a PHP# method that overrides or implements it,
    /// so a call through a plain PHP `@template` interface or parent carries them, and a plain PHP method ignores them.
    /// The class that declares the method decides, so `super.m()` carries them through a plain PHP parent that inherits
    /// a PHP# `m`.
    pub(crate) fn call_type_arguments(&self, call: &Expression) -> Option<String> {
        let span = call.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;
        let callee = self.call_target(call).class;
        if self.codebase.get_class_like(callee.as_bytes()).is_none_or(|callee| callee.flags.is_built_in()) {
            return None;
        }

        Some(self.argument_texts(arguments))
    }

    /// The type arguments the access `access` of a generic extension member gives the static method it runs as, spec
    /// section 26: those the checker inferred from the receiver for the extension block's type parameters, then the
    /// member's own, written as [`Self::type_arguments`] writes those of a `new`. None for a member that isn't generic.
    pub(crate) fn extension_type_arguments(&self, access: &Expression) -> Option<String> {
        let span = access.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;

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

    /// The type arguments the static property access `access` reads a generic PHP# class's own statics with, spec
    /// section 11, or the read of a plain case of a generic enum gives the case, whose object is one of its statics,
    /// spec section 31, as the checker found them, written as [`Self::type_arguments`] writes those of a `new`. None
    /// for a static property of any other class or any other case.
    pub(crate) fn statics_type_arguments(&self, access: &impl HasSpan) -> Option<String> {
        let span = access.span();
        let arguments = self.artifacts.inferred_type_arguments.get(&(span.start.offset, span.end.offset))?;

        Some(self.argument_texts(arguments))
    }

    /// The type a PHP# `is`, `as`, `match` arm, catch clause or `typeof` reads while the code runs, a type parameter or a
    /// class with type arguments, as the analysis records it for the type the code writes, written as
    /// [`Self::type_arguments`] writes a type. None for a class without type arguments.
    pub(crate) fn tested_type(&self, name: &impl HasSpan) -> Option<String> {
        let tested = self.artifacts.get_tested_type(name)?;

        Some(text(tested, self.codebase, Parameter::Index))
    }

    /// Whether the test at `name` is of `List<Any?>` and the analysis knows `value` is a `List` or null, as a list
    /// pattern's first test on a `List?` is, so the test asks only whether `value` is not null.
    pub(crate) fn tests_only_for_null(&self, value: &Expression, name: &impl HasSpan) -> bool {
        let is_list = |atomic: &TAtomic| matches!(atomic, TAtomic::Array(TArray::List(_)));
        let tests_any_list = self.artifacts.get_tested_type(name).is_some_and(|tested| {
            tested.types.iter().all(|atomic| {
                matches!(atomic, TAtomic::Array(TArray::List(list))
                    if list.element_type.is_mixed() && list.known_elements.is_none() && !list.non_empty)
            })
        });

        tests_any_list
            && self.expression_type(value).types.iter().all(|atomic| is_list(atomic) || matches!(atomic, TAtomic::Null))
    }

    /// The classes the type argument of the type parameter `name`, written as a class in `TItem.member`, can be: the
    /// classes of its bound, as [`receiver_classes`] gives them. None for a class.
    pub(crate) fn type_parameter_classes(&self, name: &impl HasSpan) -> Option<Vec<&'analysis [u8]>> {
        receiver_classes(self.artifacts.get_tested_type(name)?)
    }

    /// The type texts of `arguments`, joined by `, `, as a `SHARP_TYPE_ARGS` spells them.
    pub(crate) fn argument_texts(&self, arguments: &[TUnion]) -> String {
        arguments.iter().map(|argument| text(argument, self.codebase, Parameter::Index)).collect::<Vec<_>>().join(", ")
    }

    /// The metadata of the method `method` of the fully qualified class name `class` that the engine reads on a call:
    /// the bounds of its own type parameters, which a call from plain PHP gives it, and a type text list with one entry
    /// per parameter that a call from plain PHP checks its argument against, [`Self::checked_text`] or `Any?`. Each half
    /// is None when the method declares no type parameter, or no parameter needs a check. A static method of a generic
    /// class, a named constructor among them, has no object to read its class's type arguments from, so its class's
    /// type parameters lead its own, with the class's bounds.
    pub(crate) fn method_metadata(&self, class: &[u8], method: &[u8]) -> (Option<String>, Option<String>) {
        let Some(metadata) = self.codebase.get_method(class, method) else {
            return (None, None);
        };
        let is_static = metadata.method_metadata.as_ref().is_some_and(|method| method.is_static);

        self.function_metadata(metadata, if is_static { self.bounds(class) } else { None })
    }

    /// [`Self::method_metadata`] of the lambda at `span`, which plain PHP calls as a `Closure`. A lambda declares no
    /// type parameter, so it has no bounds.
    pub(crate) fn lambda_metadata(&self, span: Span) -> (Option<String>, Option<String>) {
        self.codebase
            .get_closure_at(self.file, span)
            .map_or((None, None), |metadata| self.function_metadata(metadata, None))
    }

    /// Whether the analysis widened the parameter whose name spans `name`: it erases to a narrower type than a parameter
    /// its method overrides or implements, as `string value` of a `Behavior<string>`'s `set`, so PHP would refuse the
    /// class when it links. Its declaration keeps its PHP type, which reflection reports and every call checks under
    /// the caller's `strict_types`, and carries `ZEND_PARAM_WIDENED`, so inheritance skips its variance check.
    pub(crate) fn is_widened(&self, name: Span) -> bool {
        self.artifacts.widened_parameters.contains(&(name.start.offset, name.end.offset))
    }

    /// The whole type of the lambda written at `lambda`, which `is` tests a closure of it by: [`Self::function_type`] of
    /// the parameter and result types the analysis gave it, written or inferred.
    pub(crate) fn lambda_type(&self, lambda: Span) -> FunctionType {
        let Some(TAtomic::Callable(TCallable::Signature(signature))) =
            self.artifacts.get_expression_type(&lambda).and_then(|r#type| r#type.types.first())
        else {
            unreachable!("the analysis types a lambda as its signature, not the lambda at {lambda:?}");
        };
        let parameters: Vec<Option<&TUnion>> =
            signature.get_parameters().iter().map(TCallableParameter::get_type_signature).collect();

        self.function_type(
            &FunctionLikeIdentifier::for_closure(self.file, lambda),
            &parameters,
            signature.get_return_type(),
        )
    }

    /// The whole type of the method `class::method`, which `is` tests a closure of it by: [`Self::function_type`] of its
    /// declared parameter types and its result type.
    pub(crate) fn method_type(&self, class: &[u8], method: &[u8]) -> FunctionType {
        let function = FunctionLikeIdentifier::Method(word(class), word(method));
        let Some(metadata) = self.codebase.get_method(class, method) else {
            unreachable!("the codebase holds every method a PHP# file declares, not {}", function.as_string());
        };
        let parameters: Vec<Option<&TUnion>> = metadata
            .parameters
            .iter()
            .map(|parameter| parameter.type_metadata.as_ref().map(|r#type| &r#type.type_union))
            .collect();
        let result = metadata.return_type_metadata.as_ref().map(|r#type| &r#type.type_union);

        self.function_type(&function, &parameters, result)
    }

    /// The type text of the result of the method `class::method`, as [`FunctionType::result`] writes it.
    pub(crate) fn method_result(&self, class: &[u8], method: &[u8]) -> Option<String> {
        let Some(metadata) = self.codebase.get_method(class, method) else {
            unreachable!(
                "the codebase holds every method a PHP# file declares, not {}",
                String::from_utf8_lossy(method)
            );
        };

        self.result_text(metadata.return_type_metadata.as_ref().map(|r#type| &r#type.type_union))
    }

    /// The [`FunctionType`] of the PHP# function `function`, with `parameters` and `result`.
    fn function_type(
        &self,
        function: &FunctionLikeIdentifier,
        parameters: &[Option<&TUnion>],
        result: Option<&TUnion>,
    ) -> FunctionType {
        FunctionType {
            name: function.as_string(),
            text: self.function_type_text(function, parameters, result),
            result: self.result_text(result),
        }
    }

    /// The type text of `result`, each class's type parameter as `$i` and the function's own as `#i`, or `None` when it
    /// is missing or PHP# can't write it.
    fn result_text(&self, result: Option<&TUnion>) -> Option<String> {
        result.filter(|result| is_written_in_sharp(result)).map(|result| text(result, self.codebase, Parameter::Index))
    }

    /// The type text `Function<R(P1, P2) uses E1, E2>` of the PHP# function `function`, with `parameters` and `result`,
    /// each class's type parameter as `$i` and the function's own as `#i`, and the effects it reaches, sorted. A type
    /// it leaves out is `Any?`. `None`, so a closure of it is of no function type, when no function type can say its
    /// type: when the effects solve finds an effect no `uses` lists, as for plain PHP no `extern` declares, or a type is
    /// one PHP# can't write, as the `never` of a lambda that always throws.
    fn function_type_text(
        &self,
        function: &FunctionLikeIdentifier,
        parameters: &[Option<&TUnion>],
        result: Option<&TUnion>,
    ) -> Option<String> {
        if !parameters.iter().copied().chain([result]).flatten().all(is_written_in_sharp) {
            return None;
        }
        let mut effects: Vec<String> = self
            .effects
            .function_effects(function)?
            .into_iter()
            .map(|effect| class_text(effect, self.codebase))
            .collect();
        effects.sort_unstable();
        effects.dedup();
        let written = |r#type: Option<&TUnion>| {
            r#type.map_or_else(|| "Any?".to_owned(), |r#type| text(r#type, self.codebase, Parameter::Index))
        };
        let parameters: Vec<String> = parameters.iter().map(|parameter| written(*parameter)).collect();

        Some(format!("Function<{}({}){}>", written(result), parameters.join(", "), uses_text(&effects)))
    }

    /// The metadata of a function the engine reads on a call, whose own bounds follow `class_bounds`.
    fn function_metadata(
        &self,
        metadata: &FunctionLikeMetadata,
        class_bounds: Option<String>,
    ) -> (Option<String>, Option<String>) {
        let bounds: Vec<String> = class_bounds
            .into_iter()
            .chain(
                metadata
                    .template_types
                    .values()
                    .map(|template| text(&template.constraint, self.codebase, Parameter::Bound)),
            )
            .collect();
        let parameters: Vec<Option<String>> = metadata
            .parameters
            .iter()
            .map(|parameter| self.checked_text(&parameter.type_metadata.as_ref()?.type_union, true))
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
    /// spec section 12 lets in, wherever it goes next. A collection's elements are tested only in the result of the
    /// project's own plain PHP: PHP's internal functions and the prelude's stubs have types the engine guarantees.
    ///
    /// A call whose result no PHP# code checked, as [`Self::unchecked_result_call`] finds it, checks its result when the
    /// result holds a `float`, an `Any` or an `Object`, spec section 24: plain PHP's `int` becomes the `float`, and its
    /// null or non-object is refused where it enters.
    pub(crate) fn call_check(&self, call: &Expression) -> Option<(String, String)> {
        self.call_check_as(call, self.artifacts.get_expression_type(call)?)
    }

    /// [`Self::call_check`], with the result checked as `r#type`: the `Map` a place declares, which keeps the keys of a
    /// plain PHP array the checker reads as a `List`, as the place makes it a `Map` of them.
    pub(crate) fn call_check_as(&self, call: &Expression, r#type: &TUnion) -> Option<(String, String)> {
        if let Some(name) = self.unchecked_result_call(call) {
            return needs_result_check(r#type)
                .then(|| (text(r#type, self.codebase, Parameter::Index), format!("{name}()")));
        }
        let (name, metadata) = self.plain_php_call(call)?;
        let text = self.checked_text(r#type, metadata.flags.is_user_defined())?;

        Some((text, name))
    }

    /// The check a call into plain PHP takes where its result, an object of a plain PHP class that is a `Traversable`,
    /// as a generator, an `IteratorAggregate` or Laravel's `LazyCollection` is, enters a place of the type `declared`
    /// that holds an `Iterable<T>`: `declared`'s text, whose `Iterable<T>` checks each element as it is read, spec
    /// section 12, and the call's name. Until it is held as an `Iterable<T>`, the result keeps its own class and
    /// methods, so [`Self::call_check`] gives the call alone no check.
    pub(crate) fn lazy_call_check(&self, call: &Expression, declared: &TUnion) -> Option<(String, String)> {
        let (name, _) = self.plain_php_call(call)?;
        let is_lazy_source = |atomic: &TAtomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => {
                self.codebase.get_class_like(object.name.as_bytes()).is_some_and(|class| !class.flags.is_sharp())
                    && self.codebase.is_instance_of(object.name.as_bytes(), b"Traversable")
            }
            atomic => atomic.is_null(),
        };
        let holds_iterable = declared.types.iter().any(|atomic| {
            matches!(atomic, TAtomic::Iterable(iterable) if iterable.holds_map_entries && !iterable.get_value_type().is_mixed())
        });
        let result = self.artifacts.get_expression_type(call)?;

        (holds_iterable && result.types.iter().all(is_lazy_source) && is_written_in_sharp(declared))
            .then(|| (text(declared, self.codebase, Parameter::Index), name))
    }

    /// The check each positional argument of `list` takes where it enters an `Iterable<T>` parameter of the PHP#
    /// function, method or constructor `callee`, as [`Self::lazy_call_check`] says. The engine checks the arguments of a
    /// call from plain PHP only, so a call from PHP# checks a lazy source where it passes one.
    pub(crate) fn lazy_argument_checks(
        &self,
        callee: &FunctionLikeMetadata,
        list: &ArgumentList,
    ) -> Vec<Option<(String, String)>> {
        list.arguments
            .iter()
            .zip(&callee.parameters)
            .map(|(argument, parameter)| match argument {
                Argument::Positional(positional) if positional.ellipsis.is_none() => {
                    self.lazy_call_check(positional.value, &parameter.type_metadata.as_ref()?.type_union)
                }
                _ => None,
            })
            .collect()
    }

    /// The PHP# function or method the checker resolved `call` to.
    pub(crate) fn sharp_callee(&self, call: &Expression) -> Option<&'analysis FunctionLikeMetadata> {
        let mut callees = self.artifacts.get_callees(call);
        let (Some(CallTarget::FunctionLike { callee, .. }), None) = (callees.next(), callees.next()) else {
            return None;
        };

        self.codebase.get_function_like(callee).filter(|metadata| metadata.flags.is_sharp())
    }

    /// Whether the list the spread `spread` spreads may hold a value of `parameter`'s type, and so may fill it, as a
    /// list of `Any` or of the parameter's own type does. Only a `List` spreads into a call, spec section 7.
    pub(crate) fn spread_can_fill(&self, spread: &Expression, parameter: &FunctionLikeParameterMetadata) -> bool {
        let Some(metadata) = &parameter.type_metadata else {
            unreachable!("semantics refuses a PHP# parameter without a type");
        };
        let elements = self.element_type(self.expression_type(spread));

        can_expression_types_be_identical(self.codebase, &elements, &metadata.type_union, false, false)
    }

    /// The method `method` of the fully qualified class name `class` runs, where it declares it or inherits it, when
    /// PHP# declares it: `__construct` for `new`, a named constructor, or the static method an extension's member runs
    /// as.
    pub(crate) fn sharp_method(&self, class: &[u8], method: &[u8]) -> Option<&'analysis FunctionLikeMetadata> {
        self.codebase.get_declaring_method(class, method).filter(|metadata| metadata.flags.is_sharp())
    }

    /// The name PHP's return check gives `call`, as `Lib\Pager::orders()`, and the metadata of the plain PHP function or
    /// method the checker resolved it to.
    fn plain_php_call(&self, call: &Expression) -> Option<(String, &'analysis FunctionLikeMetadata)> {
        let (callee, metadata) = self.artifacts.get_callees(call).find_map(|target| match target {
            CallTarget::FunctionLike { callee, .. } | CallTarget::MagicMethod { callee, .. } => self
                .codebase
                .get_function_like(callee)
                .filter(|metadata| !metadata.flags.is_sharp())
                .map(|metadata| (callee, metadata)),
            CallTarget::Property { .. } => None,
        })?;
        let name = match callee {
            FunctionLikeIdentifier::Method(class, _) => {
                let class = self.codebase.get_class_like(class.as_bytes()).map_or(*class, |class| class.original_name);
                format!("{class}::{}", metadata.original_name)
            }
            _ => metadata.original_name.to_string(),
        };

        Some((format!("{name}()"), metadata))
    }

    /// The name of the call `call` when plain PHP may give its result, which no PHP# code checked: a function value's
    /// call, which may run a plain PHP closure, a PHP# interface method's, which a plain PHP class may implement, as
    /// `App\Source::read`, and a method with no PHP# body given a function value, as `map` of `numbers.map(f)`: the
    /// engine runs the function and keeps what it returns, which no PHP# return check tested.
    fn unchecked_result_call(&self, call: &Expression) -> Option<String> {
        let Expression::Call(invocation) = call else {
            return None;
        };
        if let Some(function) = self.function_value_name(call) {
            return Some(function);
        }
        let (callee, class) = self.artifacts.get_callees(call).find_map(|target| match target {
            CallTarget::FunctionLike { callee: callee @ FunctionLikeIdentifier::Method(class, _), .. } => {
                Some((callee, class))
            }
            _ => None,
        })?;
        let class = self.codebase.get_class_like(class.as_bytes())?;
        let method = self.codebase.get_function_like(callee)?;
        if class.flags.is_sharp() && class.kind.is_interface() {
            return Some(format!("{}::{}", class.original_name, method.original_name));
        }
        let takes_function_value = invocation.get_argument_list().arguments.iter().any(|argument| {
            !matches!(argument.value(), Expression::ArrowFunction(_) | Expression::Closure(_))
                && self
                    .artifacts
                    .get_expression_type(argument.value())
                    .is_some_and(|r#type| r#type.types.iter().all(|atomic| matches!(atomic, TAtomic::Callable(_))))
        });

        (!method.flags.is_user_defined() && takes_function_value).then(|| method.original_name.to_string())
    }

    /// The name of the function value the call `call` runs, as the file writes it: the local of `f(2)` and `f?.(2)`,
    /// the expression of `steps[0](2)` and `this.make()?.(2)`, or the property of `holder.onTotal(2)`. None for a call
    /// of a function or a method.
    fn function_value_name(&self, call: &Expression) -> Option<String> {
        match call {
            Expression::Call(
                Call::Function(FunctionCall { function, .. })
                | Call::NullSafeFunction(NullSafeFunctionCall { function, .. }),
            ) => self
                .artifacts
                .get_expression_type(function)
                .is_some_and(|r#type| {
                    r#type.types.iter().all(|atomic| matches!(atomic, TAtomic::Callable(_) | TAtomic::Null))
                })
                .then(|| {
                    let span = function.span();
                    let written = &self.file.contents[span.start.offset as usize..span.end.offset as usize];

                    String::from_utf8_lossy(written).into_owned()
                }),
            _ => self.artifacts.get_callees(call).find_map(|target| match target {
                CallTarget::Property { property, .. } => {
                    let property = property.as_bytes();

                    Some(String::from_utf8_lossy(property.strip_prefix(b"$").unwrap_or(property)).into_owned())
                }
                _ => None,
            }),
        }
    }

    /// The type text the property `property` of the fully qualified class name `class` checks a value from plain PHP
    /// against, as a parameter checks an argument: [`Self::checked_text`] of its type, a collection's elements included.
    /// PHP's typed property check tests it on every write plain PHP makes, through reflection too, whatever the
    /// property's visibility.
    pub(crate) fn property_check(&self, class: &[u8], property: &[u8]) -> Option<String> {
        let metadata = self.codebase.get_declaring_property(class, &[b"$", property].concat())?;
        let r#type = &metadata.type_metadata.as_ref()?.type_union;

        // A property of a PHP enum, which only PHP# declares, a computed one, carries its type text always, which tells
        // the engine the enum may hold it, spec section 31, and so does one that runs with a wider plain PHP type.
        let is_enum = self.codebase.get_class_like(class).is_some_and(|class| class.kind.is_enum());
        let always = is_enum || self.narrowed_root_type(class, property).is_some();

        self.checked_text(r#type, true).or_else(|| always.then(|| type_text(r#type, self.codebase)))
    }

    /// `r#type` as the type text an entry check tests a value from plain PHP against, when part of it is an `Any`, which
    /// refuses the null PHP's `mixed` takes, a PHP# class with a type argument the check can test, a plain PHP trait,
    /// whose PHP type is `object`, a class value of a generic class, since PHP's own `string` type checks no class
    /// value, a type parameter of the class, or, with `collections`, a collection whose elements the check can test,
    /// since PHP's own `array` type checks none. None for a type only a plain PHP docblock writes, such as
    /// `array<array-key, int>`, since the engine reads the check as PHP#. None otherwise: plain PHP's generics are
    /// erased, since an object of a plain PHP generic class carries no type arguments, and a call from plain PHP gives a
    /// method's own type parameters none, so one nested in a type matches any type argument, and a value whose type is
    /// one of them is checked as its bound.
    fn checked_text(&self, r#type: &TUnion, collections: bool) -> Option<String> {
        let is_method_parameter = |r#type: &TUnion| {
            r#type.types.iter().filter(|atomic| !atomic.is_null()).all(|atomic| {
                matches!(
                    atomic,
                    TAtomic::GenericParameter(parameter)
                        if matches!(parameter.defining_entity, GenericParent::FunctionLike(_))
                )
            })
        };
        // A plain PHP trait is PHP's `object`, spec section 22, alone or in an intersection, so the check tests that a
        // class uses it.
        let names_trait = |atomic: &TAtomic| {
            let TAtomic::Object(TObject::Named(object)) = atomic else {
                return false;
            };

            std::iter::once(atomic).chain(object.get_intersection_types().unwrap_or_default()).any(|part| {
                matches!(part, TAtomic::Object(TObject::Named(part)) if self.codebase.get_trait(part.name.as_bytes()).is_some())
            })
        };
        // A call from plain PHP gives a method's own type parameters no type arguments, so a value whose type is one of
        // them checks its bound, spec section 11. One nested in a type matches any type argument.
        let bound_of = |atomic: &TAtomic| match atomic {
            TAtomic::GenericParameter(parameter)
                if matches!(parameter.defining_entity, GenericParent::FunctionLike(_)) =>
            {
                Some(parameter.constraint.types.to_vec())
            }
            _ => None,
        };
        let bounded;
        let r#type = if r#type.types.iter().any(|atomic| bound_of(atomic).is_some()) {
            bounded = TUnion::from_vec(
                r#type
                    .types
                    .iter()
                    .flat_map(|atomic| bound_of(atomic).unwrap_or_else(|| vec![atomic.clone()]))
                    .collect(),
            );
            &bounded
        } else {
            r#type
        };
        // An `Any` refuses null, which PHP's `mixed` takes, spec section 24, so only an `Any?` constrains nothing.
        let constrains = |r#type: &TUnion| !(is_method_parameter(r#type) || r#type.is_mixed() && r#type.can_be_null());
        let is_sharp_generic = |object: &TNamedObject| {
            object
                .get_type_parameters()
                .is_some_and(|arguments| arguments.iter().any(|argument| !is_method_parameter(argument)))
                && self.codebase.get_class_like(object.name.as_bytes()).is_some_and(|class| class.flags.is_sharp())
        };
        let checked = collections
            && r#type.types.iter().any(|atomic| match get_sharp_type(atomic) {
                Some(SharpType::List(element) | SharpType::Set(element) | SharpType::Iterable(element)) => {
                    constrains(element)
                }
                Some(SharpType::Map(key, value)) => constrains(key) || constrains(value),
                // PHP's `array` type checks neither a tuple's length nor its elements.
                Some(SharpType::Tuple(_)) => true,
                _ => false,
            })
            || r#type.types.iter().any(|atomic| match atomic {
                TAtomic::Mixed(mixed) => mixed.is_non_null(),
                TAtomic::Object(TObject::Named(object)) => is_sharp_generic(object) || names_trait(atomic),
                // A `Class<PaginatedList>` names no type arguments, so PHP's `string` type is the whole of its check. A
                // `Class<int>` or a `Class<int|string>` takes only the text `typeof` gives its type, which PHP's `string`
                // type doesn't check.
                TAtomic::Scalar(TScalar::ClassLikeString(
                    class_value @ TClassLikeString::OfType { constraint, .. },
                )) => match class_value.constraint() {
                    Some(TAtomic::Object(TObject::Named(object))) => {
                        is_sharp_generic(object)
                            && !object
                                .get_type_parameters()
                                .is_some_and(|arguments| arguments.iter().any(TUnion::from_unspecified_template))
                    }
                    Some(TAtomic::Object(_) | TAtomic::GenericParameter(_)) => false,
                    _ => !constraint.is_mixed() && is_written_in_sharp(constraint),
                },
                TAtomic::GenericParameter(parameter) => {
                    matches!(parameter.defining_entity, GenericParent::ClassLike(_))
                }
                _ => false,
            });

        (checked && is_written_in_sharp(r#type)).then(|| text(r#type, self.codebase, Parameter::Index))
    }

    /// The constructor `parent::__construct` reaches from the fully qualified class name `class`, as
    /// [`CodebaseMetadata::get_parent_constructor`] finds it.
    #[must_use]
    pub fn parent_constructor(&self, class: &[u8]) -> Option<&'analysis FunctionLikeMetadata> {
        self.codebase.get_parent_constructor(class)
    }

    /// The parameters of the main constructor the fully qualified class name `class` declares, which the container
    /// fills, spec section 32.
    #[must_use]
    pub fn constructor_parameters(&self, class: &[u8]) -> &'analysis [FunctionLikeParameterMetadata] {
        self.codebase.get_method(class, b"__construct").map_or(&[], |constructor| constructor.parameters.as_slice())
    }

    /// The module the scanner recorded at `span`, with its bindings in the order the module declares them.
    #[must_use]
    pub fn module(&self, span: Span) -> &'analysis ModuleMetadata {
        self.codebase
            .modules
            .iter()
            .find(|module| module.span == span)
            .unwrap_or_else(|| unreachable!("the scanner records every module of a file the checker accepted"))
    }

    /// The parameters of the lambda a binding's expression runs as, named `lambda` by the scanner.
    #[must_use]
    pub fn binding_parameters(&self, lambda: Word) -> &'analysis [FunctionLikeParameterMetadata] {
        self.codebase.get_closure(&lambda).map_or(&[], |lambda| lambda.parameters.as_slice())
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

        if let Some(case) = self.enum_case_class(class, member) {
            Some(Declaration { kind: DeclarationKind::CaseObject, class: case, name: word(b"instance"), public: true })
        } else if self.codebase.get_enum_case(class, member).is_some() {
            Some(declared(DeclarationKind::EnumCase, true))
        } else if let Some(constant) = self.codebase.get_class_constant(class, member) {
            Some(declared(DeclarationKind::Constant, constant.visibility.is_public()))
        } else {
            self.property_declaration(class, member).or_else(|| self.method_declaration(class, member))
        }
    }

    /// The full name of the class the case `case` of the enum whose full name is `class` runs as, when a case of the
    /// enum carries data, spec section 20: `App\PaymentResult\Paid` for `App\PaymentResult` and `Paid`.
    #[must_use]
    pub fn enum_case_class(&self, class: &[u8], case: &[u8]) -> Option<Word> {
        let metadata = self.codebase.get_class_like(&[class, b"\\", case].concat())?;

        (metadata.flags.is_enum_case_class()
            && metadata.direct_parent_class.is_some_and(|parent| parent.as_bytes().eq_ignore_ascii_case(class)))
        .then_some(metadata.original_name)
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

    /// The extension member, spec section 26, that the method call or property read `access` runs, as the analysis
    /// resolved it: the public static method of its static class, which takes the receiver first. None for any other
    /// call or read, and for a call through the class, as `WalletExtensions.total(wallet)`, which is that static call
    /// already, as in C#.
    #[must_use]
    pub fn extension(&self, access: &Expression) -> Option<Declaration> {
        if let Expression::Call(Call::Method(call)) = access
            && self.names.static_call_class(call).is_some()
        {
            return None;
        }

        let mut callees = self.artifacts.get_callees(access);
        let (Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. }), None) =
            (callees.next(), callees.next())
        else {
            return None;
        };
        let metadata = self.codebase.get_method(class.as_bytes(), method.as_bytes())?;

        (metadata.flags.is_extension_method() || metadata.flags.is_extension_property()).then(|| Declaration {
            kind: DeclarationKind::StaticMethod,
            class: self.class_declaration(class.as_bytes()).name,
            name: metadata.original_name,
            public: true,
        })
    }

    /// Whether the read `access` is the value of the extension method it reaches, spec section 14.3, which the analysis
    /// resolved as it resolves a call, and not a computed property.
    pub(crate) fn is_extension_method_value(&self, access: &Expression) -> bool {
        matches!(access, Expression::Access(_))
            && matches!(
                self.artifacts.get_callees(access).next(),
                Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. })
                    if self.codebase.get_method(class.as_bytes(), method.as_bytes())
                        .is_some_and(|metadata| metadata.flags.is_extension_method())
            )
    }

    /// The type of the first parameter of the function the method value `value` is, as the checker typed it. None for a
    /// function that takes none.
    pub(crate) fn value_parameter_type(&self, value: &Expression) -> Option<TUnion> {
        let Some(TAtomic::Callable(TCallable::Signature(signature))) =
            self.expression_type(value).types.iter().find(|atomic| atomic.is_callable())
        else {
            unreachable!("the analysis types a method value as its signature, not the value at {:?}", value.span());
        };

        Some(signature.get_parameters().first()?.get_type_signature().cloned().unwrap_or_else(get_mixed))
    }

    /// The parameters of the function the method value `value` is, as the checker typed it, each named as the method
    /// it runs names it, without an extension method's receiver. A plain PHP library that reads a callback's
    /// parameters sees them, as it sees a first-class callable's.
    pub(crate) fn value_parameters(&self, value: &Expression) -> Vec<ValueParameter> {
        let Some(TAtomic::Callable(TCallable::Signature(signature))) =
            self.expression_type(value).types.iter().find(|atomic| atomic.is_callable())
        else {
            unreachable!("the analysis types a method value as its signature, not the value at {:?}", value.span());
        };
        let names: Vec<&[u8]> = signature
            .get_source()
            .and_then(|source| self.codebase.get_function_like(&source))
            .map(|method| {
                let receiver = usize::from(method.flags.is_extension_method());
                method
                    .parameters
                    .iter()
                    .skip(receiver)
                    .map(|parameter| {
                        let name = parameter.name.0.as_bytes();

                        name.strip_prefix(b"$").unwrap_or(name)
                    })
                    .collect()
            })
            .unwrap_or_default();

        signature
            .get_parameters()
            .iter()
            .enumerate()
            .map(|(index, parameter)| {
                let name =
                    names.get(index).map_or_else(|| format!("argument{index}").into_bytes(), |name| name.to_vec());
                ValueParameter {
                    // The value's own closure holds the receiver as `$receiver`.
                    name: if name == b"receiver" { b"receiver#1".to_vec() } else { name },
                    r#type: parameter.get_type_signature().cloned().unwrap_or_else(get_mixed),
                    variadic: parameter.is_variadic(),
                    optional: parameter.has_default(),
                }
            })
            .collect()
    }

    /// Whether the read `read` of a plain PHP class's member gives the value of its method where the class also
    /// declares a property of that name, spec section 14.3, as the analysis resolved it where the code takes the read
    /// as a function the property's type does not fit.
    pub(crate) fn reads_method_over_property(&self, read: &Expression) -> bool {
        matches!(read, Expression::Access(_))
            && matches!(
                self.artifacts.get_callees(read).next(),
                Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(..), .. })
            )
    }

    /// The full name of the plain PHP function the read `read` gives the value of, spec section 14.3, as
    /// `global.ucfirst` and `Pdf.render` read one, as the analysis resolved it. None for any other read.
    pub(crate) fn function_value(&self, read: &Expression) -> Option<Word> {
        match self.artifacts.get_callees(read).next() {
            Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Function(function), .. })
                if matches!(read, Expression::Access(_)) =>
            {
                Some(*function)
            }
            _ => None,
        }
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

    /// The backing type of the backed enum `class`, if it has one.
    pub(crate) fn backing_type(&self, class: &[u8]) -> Option<&'analysis TAtomic> {
        self.codebase.get_class_like(class)?.enum_type.as_ref()
    }

    /// Whether the class-like `class` has the `parse` the codex gives a struct or a PHP# class.
    pub(crate) fn parses(&self, class: &[u8]) -> bool {
        self.codebase.parses(class)
    }

    /// The fields the `parse` of the struct or PHP# class `class` reads, as the codex gives them.
    pub(crate) fn struct_parse_fields(&self, class: &[u8]) -> Vec<ParsedField<'analysis>> {
        self.codebase.struct_parse_fields(class)
    }

    /// `r#type` as the type text generics R3c's walk reads, php-sharp's decision 72: a class by its full dotted name as
    /// it is declared, `int`, `float`, `string`, `bool`, `Any`, `List<T>`, `Set<T>`, `Map<K, V>`, a class's type
    /// parameter by its index, `$0`, and `T?`, `A|B` and `(A|B)?` with union members sorted by their text. These are
    /// the types a struct's `parse` reads, so a type that holds a function, which no input holds, has none.
    pub(crate) fn type_text(&self, r#type: &TUnion) -> Option<String> {
        if let Some(TAtomic::Mixed(mixed)) = r#type.types.iter().find(|atomic| atomic.is_mixed()) {
            return Some(if mixed.is_non_null() { "Any" } else { "Any?" }.to_owned());
        }

        let mut members = r#type
            .types
            .iter()
            .filter(|atomic| !atomic.is_null())
            .map(|atomic| self.atomic_text(atomic))
            .collect::<Option<Vec<String>>>()?;
        members.sort_unstable();
        members.dedup();

        Some(match (r#type.has_null(), members.as_slice()) {
            (false, _) => members.join("|"),
            (true, [member]) => format!("{member}?"),
            (true, _) => format!("({})?", members.join("|")),
        })
    }

    /// One member of a union as [`Types::type_text`] writes it.
    fn atomic_text(&self, atomic: &TAtomic) -> Option<String> {
        Some(match atomic {
            TAtomic::Scalar(TScalar::Integer(_)) => "int".to_owned(),
            TAtomic::Scalar(TScalar::Float(_)) => "float".to_owned(),
            TAtomic::Scalar(TScalar::String(_)) => "string".to_owned(),
            TAtomic::Scalar(TScalar::Bool(_)) => "bool".to_owned(),

            TAtomic::Object(TObject::Enum(r#enum)) => self.class_text(r#enum.name),
            TAtomic::Object(TObject::Named(object)) => self.class_text(object.name),
            TAtomic::Array(array) => {
                let (key, element) = self.array_parameters(array);
                match array {
                    TArray::List(_) => format!("List<{}>", self.type_text(&element)?),
                    TArray::Set(_) => format!("Set<{}>", self.type_text(&element)?),
                    TArray::Keyed(_) => format!("Map<{}, {}>", self.type_text(&key)?, self.type_text(&element)?),
                }
            }
            // A generic type's field of its type parameter reads as the type argument `parse` runs with: `$0`.
            TAtomic::GenericParameter(_) => text(&TUnion::from_atomic(atomic.clone()), self.codebase, Parameter::Index),
            _ => return None,
        })
    }

    /// The full dotted name of the class `name`, as it is declared.
    fn class_text(&self, name: Word) -> String {
        let declared = self.codebase.get_class_like(name.as_bytes()).map_or(name, |class| class.original_name);

        declared.as_str_lossy().trim_start_matches('\\').replace('\\', ".")
    }

    /// The key type and the element type of a collection.
    pub(crate) fn array_parameters(&self, array: &TArray) -> (TUnion, TUnion) {
        get_array_parameters(array, self.codebase)
    }

    /// The type of the elements a value of `r#type` holds, the `T` of a `List<T>`, leaving out `null`. The caller asks
    /// only about a value the checker typed as a collection: a collection method's receiver, an operand of a
    /// collection's `==`, or a spread.
    pub(crate) fn element_type(&self, r#type: &TUnion) -> TUnion {
        let mut element_type = None;
        for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
            let TAtomic::Array(array) = atomic else {
                unreachable!("the checker types the value as a collection, not as {atomic:?}");
            };
            let (_, value) = get_array_parameters(array, self.codebase);
            element_type = Some(add_optional_union_type(value, element_type.as_ref(), self.codebase));
        }

        element_type.unwrap_or_else(|| unreachable!("the checker types the value as a collection"))
    }

    /// The collection, `List`, `Map` or `Set`, and the name in lowercase of the collection method the analyzer checked
    /// the method call `call` against, null-safe or not, or that the read `call` gives the value of, spec section
    /// 14.3: a method of `Sharp\Internal\ListMethods`,
    /// `Sharp\Internal\MapMethods` or `Sharp\Internal\SetMethods`. None for a call of any other method.
    pub(crate) fn collection_method(&self, call: &Expression) -> Option<(&'static str, Vec<u8>)> {
        let mut callees = self.artifacts.get_callees(call);
        let (Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. }), None) =
            (callees.next(), callees.next())
        else {
            return None;
        };
        let methods: [(&str, &[u8]); 3] = [
            ("List", b"Sharp\\Internal\\ListMethods"),
            ("Map", b"Sharp\\Internal\\MapMethods"),
            ("Set", b"Sharp\\Internal\\SetMethods"),
        ];
        let collection = methods
            .into_iter()
            .find_map(|(collection, methods)| class.as_bytes().eq_ignore_ascii_case(methods).then_some(collection))?;

        Some((collection, method.as_bytes().to_ascii_lowercase()))
    }

    /// The name in lowercase of the `List` method the method call `call` runs on a value that may also be a `Map` or a
    /// `Set`: the analyzer checked it against `Sharp\Internal\ListMethods` and the other collections' method of that
    /// name. None for a call on one collection, or on anything else.
    pub(crate) fn list_method_on_several_collections(&self, call: &Expression) -> Option<Vec<u8>> {
        let (mut list, mut others) = (None, false);
        for callee in self.artifacts.get_callees(call) {
            let CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, method), .. } = callee else {
                return None;
            };
            if class.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\ListMethods") {
                list = Some(method.as_bytes().to_ascii_lowercase());
            } else if class.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\MapMethods")
                || class.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\SetMethods")
            {
                others = true;
            } else {
                return None;
            }
        }

        list.filter(|_| others)
    }

    /// Whether the method call `call`, null-safe or not, runs a `Map` method that leaves its receiver as it is, which
    /// `Sharp\Internal\MapMethods` declares mutation-free.
    pub(crate) fn reads_map(&self, call: &Expression) -> bool {
        let Some(("Map", method)) = self.collection_method(call) else {
            return false;
        };

        self.codebase
            .get_method(b"Sharp\\Internal\\MapMethods", &method)
            .is_some_and(|metadata| metadata.flags.is_mutation_free())
    }

    /// Whether the method call `call`, null-safe or not, runs a `List` method that changes its receiver, one
    /// `Sharp\Internal\ListMethods` does not declare mutation-free, as `add` and `remove`.
    pub(crate) fn changes_list(&self, call: &Expression) -> bool {
        let Some(("List", method)) = self.collection_method(call) else {
            return false;
        };

        self.codebase
            .get_method(b"Sharp\\Internal\\ListMethods", &method)
            .is_some_and(|metadata| !metadata.flags.is_mutation_free())
    }

    /// Whether the method call `call`, null-safe or not, runs a lazy operation of a PHP# `Iterable<T>`, spec section 12,
    /// which `Sharp\Internal\IterableMethods` declares and the engine's `Sharp\Sequence` runs.
    pub(crate) fn is_lazy_operation(&self, call: &Expression) -> bool {
        matches!(
            self.artifacts.get_callees(call).next(),
            Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Method(class, _), .. })
                if class.as_bytes().eq_ignore_ascii_case(b"Sharp\\Internal\\IterableMethods")
        )
    }

    /// The plain PHP function the method call `call` runs, as `Pdf.render(html)` and `global.now()` call one, spec
    /// section 23, with the name its declaration writes. None for a call of a method.
    pub(crate) fn namespace_function(&self, call: &Expression) -> Option<Word> {
        let Some(CallTarget::FunctionLike { callee: FunctionLikeIdentifier::Function(function), .. }) =
            self.artifacts.get_callees(call).next()
        else {
            return None;
        };

        Some(self.codebase.get_function(function.as_bytes()).map_or(*function, |metadata| metadata.original_name))
    }

    /// Whether `function`, a plain PHP function, is one of PHP's built-ins whose declared return type holds its failure,
    /// `false` or `null`, as `hex2bin` returns `string|false`. PHP# reads the failure from that value, so PHP's warning
    /// for the same failure only repeats it, and the call runs without it. A built-in that takes a function, as
    /// `call_user_func` and `array_reduce` do, runs that function's code inside the call, whose warnings are no failure of
    /// the built-in, so it keeps its warnings, as a plain PHP library's own function does.
    pub(crate) fn returns_its_failure(&self, function: Word) -> bool {
        self.codebase.get_function(function.as_bytes()).is_some_and(|metadata| {
            metadata.flags.is_built_in()
                && !metadata.parameters.iter().any(|parameter| {
                    [parameter.get_type_declaration_metadata(), parameter.get_type_metadata()]
                        .into_iter()
                        .flatten()
                        .any(|declared| declared.type_union.has_callable())
                })
                && metadata
                    .return_type_declaration_metadata
                    .as_ref()
                    .or(metadata.return_type_metadata.as_ref())
                    .is_some_and(|returned| {
                        returned.type_union.types.iter().any(|atomic| match atomic {
                            TAtomic::Null | TAtomic::Mixed(_) => true,
                            TAtomic::Scalar(scalar) => scalar.is_false() || scalar.is_general_bool(),
                            _ => false,
                        })
                    })
        })
    }

    /// The full name of the plain PHP constant the read `read` reaches through its namespace, as `global.PHP_EOL` and
    /// `Pdf.DEFAULT_DPI` read one, spec section 23. None for any other read.
    pub(crate) fn namespace_constant(&self, read: &Expression) -> Option<Word> {
        self.artifacts.get_namespace_constant(read)
    }

    /// The parameters of the plain PHP function or method the call `call` runs, as the analysis resolved it, which a
    /// `Map` or a `Set` crosses into as a PHP array where the parameter's declared type names `array`. None for a PHP#
    /// one, which takes the object.
    pub(crate) fn call_parameters(&self, call: &Expression) -> Option<Vec<PlainParameter<'analysis>>> {
        let mut callees = self.artifacts.get_callees(call);
        let (Some(CallTarget::FunctionLike { callee, .. }), None) = (callees.next(), callees.next()) else {
            return None;
        };
        let owner = match callee {
            FunctionLikeIdentifier::Function(name) | FunctionLikeIdentifier::Method(name, _) => name.as_bytes(),
            FunctionLikeIdentifier::Closure(_) => return None,
        };

        plain_parameters(owner, self.codebase.get_function_like(callee))
    }

    /// The parameters of the plain PHP closure the expression `function` gives, which the call `function(…)` runs, as
    /// the signature of its type gives them, as [`Self::call_parameters`] gives a call's. None for a function value
    /// whose signature PHP# wrote, which takes the object, and for a value of any other type, such as an object whose
    /// `__invoke` the call runs, which [`Self::call_parameters`] finds.
    pub(crate) fn closure_parameters(&self, function: &Expression) -> Option<Vec<PlainParameter<'analysis>>> {
        let [TAtomic::Callable(TCallable::Signature(signature))] =
            self.artifacts.get_expression_type(function)?.types.as_ref()
        else {
            return None;
        };

        signature.effects.is_none().then(|| signature.get_parameters().iter().map(PlainParameter::Callable).collect())
    }

    /// The parameters of the constructor `new` runs for the fully qualified class name `class`, as
    /// [`Self::call_parameters`] gives a call's.
    pub(crate) fn new_parameters(&self, class: &[u8]) -> Option<Vec<PlainParameter<'analysis>>> {
        plain_parameters(class, self.codebase.get_declaring_method(class, b"__construct"))
    }

    /// The parameters of the constructor `parent::__construct` reaches from the fully qualified class name `class`, as
    /// [`Self::call_parameters`] gives a call's.
    pub(crate) fn parent_constructor_parameters(&self, class: &[u8]) -> Option<Vec<PlainParameter<'analysis>>> {
        plain_parameters(b"", self.codebase.get_parent_constructor(class))
    }

    /// The declared type of the property `target` writes, `object.property` or `Class.property`, as the class, or the
    /// first class its object can be, declares it. None for any other target.
    pub(crate) fn property_type(&self, target: &Expression) -> Option<&'analysis TUnion> {
        let (class, property) = self.accessed_property(target)?;

        self.codebase.get_property(class, &property)?.type_metadata.as_ref().map(|declared| &declared.type_union)
    }

    /// Whether `target` reads a property plain PHP declares, which can hold a PHP array where PHP# reads a `Map` or a
    /// `Set`.
    pub(crate) fn plain_property(&self, target: &Expression) -> bool {
        self.accessed_property(target)
            .and_then(|(class, property)| self.codebase.get_declaring_property_class(class, &property))
            .and_then(|declaring| self.codebase.get_class_like(declaring.as_bytes()))
            .is_some_and(|declaring| !declaring.flags.is_sharp())
    }

    /// The class and the `$`-prefixed name of the property `target` reads, `object.property` or `Class.property`, as
    /// the class, or the first class its object can be, holds it.
    fn accessed_property(&self, target: &Expression) -> Option<(&'analysis [u8], Vec<u8>)> {
        let Expression::Access(Access::Property(
            access @ PropertyAccess { object, property: ClassLikeMemberSelector::Identifier(name), .. },
        )) = target.unparenthesized()
        else {
            return None;
        };
        let class = match self.names.static_property_class(access) {
            Some(class) => self.names.get(&class.name),
            None => *receiver_classes(self.artifacts.get_expression_type(object)?)?.first()?,
        };

        Some((class, [b"$", name.value].concat()))
    }

    /// Whether the property `property` of an object of `r#type` has hooks in a class the object can be. PHP gives no
    /// reference to a hooked property, so a write into the `Map` it holds cannot go through one.
    pub(crate) fn property_has_hooks(&self, r#type: &TUnion, property: &[u8]) -> bool {
        let variable = [b"$", property].concat();

        receiver_classes(r#type).into_iter().flatten().any(|class| {
            self.codebase.get_declaring_property(class, &variable).is_some_and(|metadata| !metadata.hooks.is_empty())
        })
    }

    /// Whether the static property `property` of `class` runs accessors: a computed one, or one with an accessor body or
    /// an observer. PHP has no hooks on a static, so its class holds them as the hidden static methods `name#get` and
    /// `name#set`, which every read and write of it calls.
    pub(crate) fn is_static_accessor(&self, class: &[u8], property: &[u8]) -> bool {
        let variable = [b"$", property].concat();

        self.codebase
            .get_declaring_property(class, &variable)
            .is_some_and(|metadata| metadata.flags.is_static() && !metadata.hooks.is_empty())
    }

    /// Whether the static property `property` of `class` has an initial value that runs as code, which a throw leaves
    /// unset, spec section 6.
    pub(crate) fn has_runtime_initial_value(&self, class: &[u8], property: &[u8]) -> bool {
        let variable = [b"$", property].concat();

        self.codebase
            .get_declaring_property(class, &variable)
            .is_some_and(|metadata| metadata.flags.has_runtime_initial_value())
    }

    /// The property `class` declares or inherits by the name `property`, if any.
    fn property_declaration(&self, class: &[u8], property: &[u8]) -> Option<Declaration> {
        let variable = [b"$", property].concat();
        let metadata = self.codebase.get_declaring_property(class, &variable)?;
        let kind = if metadata.flags.is_static() {
            DeclarationKind::StaticProperty
        } else {
            DeclarationKind::Property {
                typed: self.root_property(class, &variable).1.type_declaration_metadata.is_some(),
            }
        };

        Some(Declaration {
            kind,
            class: self.codebase.get_declaring_property_class(class, &variable).unwrap_or_else(|| word(class)),
            name: word(property),
            public: metadata.read_visibility.is_public(),
        })
    }

    /// The type the root declaration of the property `property` of the fully qualified class name `class` writes, when
    /// that root is a typed plain PHP property and `class` declares the property with a narrower type, spec section
    /// 6.1. PHP keeps a property's type through every override as the class links, so the property runs with it, and
    /// its own type checks each write. None for any other property, which runs with the type it writes.
    pub(crate) fn narrowed_root_type(&self, class: &[u8], property: &[u8]) -> Option<&'analysis TUnion> {
        let variable = [b"$", property].concat();
        let own =
            &self.codebase.get_declaring_property(class, &variable)?.type_declaration_metadata.as_ref()?.type_union;
        let (root_class, root) = self.root_property(class, &variable);
        let root_type = &root.type_declaration_metadata.as_ref()?.type_union;
        let is_plain = !self.codebase.get_class_like(root_class.as_bytes())?.flags.is_sharp();

        (is_plain
            && !union_comparator::is_contained_by(
                self.codebase,
                root_type,
                own,
                false,
                false,
                false,
                &mut ComparisonResult::default(),
            ))
        .then_some(root_type)
    }

    /// The root declaration of the property `variable` that `class` declares or inherits, the one in the class whose
    /// parent does not declare it, with that class.
    fn root_property(&self, class: &[u8], variable: &[u8]) -> (Word, &'analysis PropertyMetadata) {
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

        let metadata = self
            .codebase
            .get_declaring_property(root.as_bytes(), variable)
            .unwrap_or_else(|| unreachable!("`{root}` declares the property"));

        (root, metadata)
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
                class_value @ (TClassLikeString::Generic { .. } | TClassLikeString::OfType { .. }),
            )) => match class_value.constraint() {
                Some(TAtomic::Object(TObject::Named(object))) => Some(object.name.as_bytes()),
                Some(TAtomic::Object(TObject::Enum(object))) => Some(object.name.as_bytes()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<_>>()?;

    (!classes.is_empty()).then_some(classes)
}

/// The parameters of `function`, which the function or class `owner` names, when plain PHP declares it. PHP# declares
/// what the `Sharp\` namespace holds, its collection stubs among them, though their files are plain PHP.
fn plain_parameters<'function>(
    owner: &[u8],
    function: Option<&'function FunctionLikeMetadata>,
) -> Option<Vec<PlainParameter<'function>>> {
    let function = function?;
    let sharp =
        function.flags.is_sharp() || owner.get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sharp\\"));

    (!sharp).then(|| function.parameters.iter().map(PlainParameter::Declared).collect())
}

/// The whole type of a PHP# method or lambda, which `is` tests a closure of it by.
#[derive(Debug)]
pub(crate) struct FunctionType {
    /// The function, `Class::method` or the lambda's `{closure:…}` name.
    pub(crate) name: String,
    /// Its type text, `Function<R(P1, P2) uses E1, E2>`, or `None` when no function type can say its type.
    pub(crate) text: Option<String>,
    /// The type text of its result, which a generator's elements and a map stage's are read from whatever it calls,
    /// spec section 12, or `None` when PHP# can't write it.
    pub(crate) result: Option<String>,
}

/// A parameter of plain PHP code a call runs, which a `Map` or a `Set` crosses into: a declared function's, method's or
/// constructor's, or one of the signature of a plain PHP closure a local holds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PlainParameter<'analysis> {
    Declared(&'analysis FunctionLikeParameterMetadata),
    Callable(&'analysis TCallableParameter),
}

impl<'analysis> PlainParameter<'analysis> {
    /// The name a named argument gives the parameter, without its `$`.
    pub(crate) fn name(self) -> Option<&'analysis [u8]> {
        let name = match self {
            Self::Declared(parameter) => parameter.get_name(),
            Self::Callable(parameter) => parameter.get_name()?,
        };

        name.0.as_bytes().get(1..)
    }

    /// The type the parameter declares.
    pub(crate) fn declared_type(self) -> Option<&'analysis TUnion> {
        match self {
            Self::Declared(parameter) => parameter.get_type_metadata().map(|declared| &declared.type_union),
            Self::Callable(parameter) => parameter.get_type_signature(),
        }
    }

    pub(crate) fn is_variadic(self) -> bool {
        match self {
            Self::Declared(parameter) => parameter.flags.is_variadic(),
            Self::Callable(parameter) => parameter.is_variadic(),
        }
    }

    pub(crate) fn is_by_reference(self) -> bool {
        match self {
            Self::Declared(parameter) => parameter.flags.is_by_reference(),
            Self::Callable(parameter) => parameter.is_by_reference(),
        }
    }
}

/// A parameter of the function a method value is, as [`Types::value_parameters`] gives it.
pub(crate) struct ValueParameter {
    pub(crate) name: Vec<u8>,
    pub(crate) r#type: TUnion,
    pub(crate) variadic: bool,
    pub(crate) optional: bool,
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
/// dotted name as it is declared, the built-in types, `List<T>`, `Map<K, V>`, `Set<T>`, `Iterable<T>`, a tuple as
/// `[A, B]`, `Class<T>`,
/// `Function<R(P1, P2)>`, a type parameter as `Any?`, `T?`, `(A|B)?`, `A & B`, and an intersection in parentheses
/// inside a union or a nullable type, `(A & B)|C` and `(A & B)?`. Union and intersection members are sorted by their
/// text, and a space comes only after a comma and around `&`.
pub(crate) fn type_text(r#type: &TUnion, codebase: &CodebaseMetadata) -> String {
    text(r#type, codebase, Parameter::Any)
}

/// Whether `r#type` holds, itself or as a collection's element, key or value, a part an entry check acts on for a
/// value plain PHP gives: a `float`, which an `int` becomes, an `Any`, which refuses null, or an `Object`, which
/// refuses a value that is no object.
fn needs_result_check(r#type: &TUnion) -> bool {
    r#type.types.iter().any(|atomic| match get_sharp_type(atomic) {
        Some(SharpType::List(element) | SharpType::Set(element) | SharpType::Iterable(element)) => {
            needs_result_check(element)
        }
        Some(SharpType::Map(key, value)) => needs_result_check(key) || needs_result_check(value),
        Some(SharpType::Float | SharpType::Object) => true,
        _ => matches!(atomic, TAtomic::Mixed(mixed) if mixed.is_non_null()),
    })
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
        // A generic class written without type arguments, as `Class<Box>` names it, gets the ones the analysis fills in
        // from its bounds, and its text is the class alone, which takes the class with any type arguments.
        SharpType::Class(name, arguments) if arguments.iter().all(TUnion::from_unspecified_template) => {
            class_text(name, codebase)
        }
        SharpType::Class(name, arguments) => format!("{}<{}>", class_text(name, codebase), list(&mut arguments.iter())),
        SharpType::List(element_type) => format!("List<{}>", type_text(element_type)),
        SharpType::Set(element_type) => format!("Set<{}>", type_text(element_type)),
        SharpType::Map(key, value) => format!("Map<{}>", list(&mut [key, value].into_iter())),
        SharpType::Iterable(value) => format!("Iterable<{}>", type_text(value)),
        // A tuple's element names are the checker's, so the engine reads only its element types.
        SharpType::Tuple(tuple) => {
            format!("[{}]", list(&mut tuple.tuple_elements().into_iter().flatten().map(|(element, _)| element)))
        }
        SharpType::Parameter(generic) => {
            parameter_text(generic.parameter_name, generic.defining_entity, &|| type_text(&generic.constraint))
        }
        SharpType::ClassValue(class_value) => {
            let class = match class_value {
                TClassLikeString::Literal { value } => class_text(*value, codebase),
                TClassLikeString::OfType { constraint, .. } => text(constraint, codebase, parameter),
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
            let mut effects: Vec<String> =
                signature.effects.iter().flatten().map(|effect| class_text(*effect, codebase)).collect();
            effects.sort_unstable();
            effects.dedup();

            format!(
                "Function<{}({}){}>",
                written(signature.get_return_type()),
                parameters.join(", "),
                uses_text(&effects)
            )
        }
    }
}

/// The `uses` of a function type text, ` uses E1, E2`, for `effects`, which are sorted, or nothing for none.
fn uses_text(effects: &[String]) -> String {
    if effects.is_empty() { String::new() } else { format!(" uses {}", effects.join(", ")) }
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
    use mago_codex::ttype::get_nullable_int;
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

    /// A class value bounded by a class names it, one of a union or a nullable type writes that whole type, one of a
    /// type parameter writes the type parameter, and one of any class names `Object`. A type that is only `null` is
    /// `null`.
    #[test]
    fn a_class_value_names_its_bound_its_type_parameter_or_object() {
        let order = TAtomic::Object(TObject::Named(TNamedObject::new(word("App\\Order"))));
        let class_value = |class_value| text(&wrap_atomic(TAtomic::Scalar(TScalar::ClassLikeString(class_value))));
        let of_type = TClassLikeString::of_type(TClassLikeStringKind::Class, order.clone());
        let held =
            |held: TUnion| TClassLikeString::OfType { kind: TClassLikeStringKind::Class, constraint: Arc::new(held) };
        let generic = TClassLikeString::generic(
            TClassLikeStringKind::Class,
            word("TItem"),
            GenericParent::ClassLike(word("App\\PaginatedList")),
            order,
        );

        assert_eq!(class_value(of_type), "Class<App.Order>");
        assert_eq!(class_value(held(union(&[get_string(), get_int()]))), "Class<int|string>");
        assert_eq!(class_value(held(get_nullable_int())), "Class<int?>");
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
