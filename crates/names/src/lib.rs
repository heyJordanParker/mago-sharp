use std::sync::LazyLock;

use foldhash::HashMap;
use mago_span::Span;

use mago_span::HasPosition;
use mago_span::Position;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::GenericClass;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::TypeParameter;
use mago_syntax::cst::TypeParameterBound;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::UnionHint;
use mago_syntax::cst::built_in_generic_arity;
use mago_syntax::walker::MutWalker;
use mago_syntax::walker::Walker;
use mago_syntax::walker::walk_property_hook_list_mut;

use crate::binding::Binding;
use crate::binding::BindingError;
use crate::binding::Local;

pub mod binding;
pub mod kind;
pub mod resolver;
pub mod scope;

mod internal;
mod sharp_classes;

pub use sharp_classes::EMIT_EFFECT;
pub use sharp_classes::PROPERTY_BEHAVIOR;
pub use sharp_classes::STANDARD_ROOT_CLASSES;

/// The attribute that makes a body-less static method the call of the PHP function its argument names, spec section 23,
/// as `[Import("levenshtein")]`.
pub const IMPORT: &str = "Sharp\\Interop\\Import";

/// The attribute that declares a global function of the name its argument gives, which calls the static method it marks,
/// spec section 23, as `[Export("auth_redirect")]`.
pub const EXPORT: &str = "Sharp\\Interop\\Export";

/// The mark of an attribute class whose framework reaches by reflection the member its attribute marks, or each public
/// member of the class it marks.
///
/// Spec section 5: as .NET's `[DynamicallyAccessedMembers]` says a member is reached, so the member counts as used, and
/// is never too wide.
pub const DYNAMICALLY_ACCESSED: &str = "Sharp\\DynamicallyAccessed";

/// The method of a class that runs as a program, spec section 27.
///
/// PHP runs the `.sharp` file of a class that declares `public int main()` or `public void main()` as its script, and
/// the engine builds the class and calls it.
pub const PROGRAM_ENTRY: &str = "main";

/// The mark of a parameter whose argument a method writes as JSON text.
///
/// The standard library marks `Json.encode`'s, so the checker refuses a `Map` key JSON can't write. The checker reads
/// the mark, so it copies no library name.
pub const ENCODED_AS_JSON: &str = "Sharp\\Internal\\EncodedAsJson";

/// The mark of a method without a body whose implementations may have any effect.
///
/// The code that calls the method carries the implementation's effects, as for `Dispatcher.dispatch` and
/// `Behavior<T>`'s `get` and `set`.
pub const ACCEPTS_ANY_EFFECT: &str = "Sharp\\Internal\\AcceptsAnyEffect";

/// The attribute the standard library marks the method with whose call starts a listener of its first type argument,
/// as `Events.on`.
pub const STARTS_LISTENER: &str = "Sharp\\Internal\\StartsListener";

/// The mark of a generic method that creates the attributes of its type argument, as `Class<T>.attributes<T>()`.
///
/// A call whose type argument is a PHP# class is pure, spec section 1: every PHP# attribute's constructor and initial
/// values have no effects.
pub const CREATES_ATTRIBUTES: &str = "Sharp\\Internal\\CreatesAttributes";

/// The mark of a method that runs the initial value of a property, as `Property.defaultValue()`.
///
/// A call has the effects of every initial value the program declares that is not constant, spec section 29.
pub const RUNS_INITIAL_VALUES: &str = "Sharp\\Internal\\RunsInitialValues";

/// The mark of the interface every `emit` hands its event to, as `Dispatcher`.
///
/// The app's binding of the interface is its dispatcher, spec section 15. The checker, the compile command and the
/// lowering of `emit` read the mark, so none copies the library's name.
pub const DISPATCHES_EMITS: &str = "Sharp\\Internal\\DispatchesEmits";

/// The mark of a generic method that parses a value into its type argument.
///
/// The method runs the type's `parse`, as `Json.decode<T>` does, so each call's type argument gets the struct parse
/// check.
pub const PARSES_TYPE_ARGUMENT: &str = "Sharp\\Internal\\ParsesTypeArgument";

/// The attribute the standard library marks a method with that a law's proof reads as the Lean function the
/// attribute's argument names, folded over the call's arguments, as `Math.max` is Lean's `max`.
pub const LEAN_FUNCTION: &str = "Sharp\\Internal\\LeanFunction";

/// The built-in bound of every type that has `==`, spec section 11, as a `.sharp` file writes it.
///
/// It resolves as a default class to the prelude's `Sharp\Equatable`, a bound alone that the engine declares nothing
/// for, so the checker reserves the name and the scanner reads it as no type the running code checks.
pub const EQUATABLE: &str = "Equatable";

/// How PHP# writes PHP's `object`, spec section 24.
///
/// The parser reads both as `Hint::Object`, and only `Object` is PHP#.
pub const OBJECT: &[u8] = b"Object";

/// The class-likes that a bare name in a `.sharp` file names by default, spec section 23, and that an import or a
/// class-like of the file's namespace wins over, as in Kotlin and Java: every class-like the standard library or the
/// prelude declares directly in `Sharp`, `Error` among them, then the [standard root classes](default_root_classes).
pub(crate) fn default_classes() -> impl Iterator<Item = &'static str> {
    sharp_classes::SHARP_CLASSES.into_iter().chain(default_root_classes())
}

/// The [standard classes](STANDARD_ROOT_CLASSES) a bare name names by default, spec section 23.
///
/// Each is in PHP's root namespace. One of a namespace, as `Dom\HTMLDocument`, is named through its import.
pub fn default_root_classes() -> impl Iterator<Item = &'static str> {
    STANDARD_ROOT_CLASSES.into_iter().filter(|class| !class.contains('\\'))
}

/// Each [default class-like](default_classes) by its short name, the first of them for a name two share.
static DEFAULT_CLASSES: LazyLock<HashMap<&'static [u8], &'static [u8]>> = LazyLock::new(|| {
    let mut classes = HashMap::default();
    for class in default_classes() {
        classes.entry(default_short_name(class)).or_insert(class.as_bytes());
    }

    classes
});

/// The default class-like whose short name is `name`, as a `.sharp` file writes it: `Sharp\Math` for `Math`, and
/// `ValueError` for `ValueError`. A lowercase `int` is the built-in type, so the name is compared exactly.
pub(crate) fn default_class(name: &[u8]) -> Option<&'static [u8]> {
    DEFAULT_CLASSES.get(name).copied()
}

/// The standard effect whose short name is `name`, which a bare name in a `uses` of a `.sharp` file names by default:
/// `Sharp\Time\Clock` for `Clock`, spec section 29, or the [effect of `emit`](EMIT_EFFECT), which no `effect` class
/// is.
pub(crate) fn standard_effect(name: &[u8]) -> Option<&'static [u8]> {
    sharp_classes::STANDARD_EFFECTS
        .into_iter()
        .chain([EMIT_EFFECT])
        .find(|effect| default_short_name(effect) == name)
        .map(str::as_bytes)
}

/// Whether a plain PHP class-like whose short name is `name` wins over a default class-like or a standard effect in its
/// namespace, compared ignoring case as PHP compares class names.
pub(crate) fn shadows_a_default_class(name: &[u8]) -> bool {
    default_classes()
        .chain(sharp_classes::STANDARD_EFFECTS)
        .any(|class| default_short_name(class).eq_ignore_ascii_case(name))
}

/// The name a `.sharp` file writes the default class-like `class` by: `Math` for `Sharp\Math`.
pub(crate) fn default_short_name(class: &str) -> &[u8] {
    class.rsplit('\\').next().unwrap_or(class).as_bytes()
}

/// Whether the bound of `parameter`, a type parameter of `list`, leads back to it.
///
/// It leads back through the bounds of the other type parameters of `list` it names, as `<T : T>` and
/// `<TA : TB, TB : TA>` do. C# refuses such a circular constraint (CS0454), and so does PHP#, spec section 11.
#[must_use]
pub fn bounds_itself(list: &TypeParameterList<'_>, parameter: &TypeParameter<'_>) -> bool {
    let mut current = parameter;
    for _ in 0..list.parameters.len() {
        let Some(TypeParameterBound { hint: Hint::Identifier(Identifier::Local(bound)), .. }) = &current.bound else {
            return false;
        };
        if bound.value == parameter.name.value {
            return true;
        }
        let Some(next) = list.parameters.iter().find(|other| other.name.value == bound.value) else {
            return false;
        };
        current = next;
    }

    false
}

/// Collects each name of a PHP# type parameter the code it walks writes, as the binder bound it.
///
/// It collects `TError` in a body's `catch (TError error)`, or `TOther` in a bound's `List<TOther>`.
pub struct TypeParametersNamed<'names, 'arena>(pub &'names ResolvedNames<'arena>);

impl<'ast, 'arena> Walker<'ast, 'arena, Vec<&'arena [u8]>> for TypeParametersNamed<'_, '_> {
    fn walk_in_local_identifier(&self, identifier: &'ast LocalIdentifier<'arena>, names: &mut Vec<&'arena [u8]>) {
        if self.0.is_type_parameter(identifier) {
            names.push(identifier.value);
        }
    }
}

/// The last segment of the full name `name`, as a PHP# import writes it.
#[must_use]
pub fn short_name(name: impl AsRef<[u8]>) -> String {
    let name = name.as_ref();

    String::from_utf8_lossy(name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name)).into_owned()
}

/// The member `member_name` of the class `class_name` as PHP# names it: `Box.put`.
///
/// C# names a member the same way in its messages. A method, a property, a constant and an enum case read alike, so a
/// property's `$` is dropped: `Box.total`.
#[must_use]
pub fn display_sharp_member(class_name: impl AsRef<[u8]>, member_name: impl std::fmt::Display) -> String {
    format!("{}.{}", short_name(class_name), member_name.to_string().trim_start_matches('$'))
}

/// A resolved name's end offset, and its fully qualified name with whether an import gave it.
type Resolved<'arena> = (u32, (&'arena [u8], bool));

/// Stores the results of a name resolution pass over a PHP program.
///
/// Maps the start byte offset of every identifier in the source to a tuple of
/// `(end offset, resolved fully qualified name, was-imported flag)`. Storing the end
/// offset alongside the start lets callers answer "what name is at this cursor offset?"
/// without re-scanning the source for identifier boundaries.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ResolvedNames<'arena> {
    /// Internal map: start offset -> (end offset, (resolved FQN, imported flag)).
    names: HashMap<u32, Resolved<'arena>>,

    /// Start offset of every name of `names` that holds the PHP a PHP# form runs as, which the checker, the analyzer
    /// and the bridge read, where the source names nothing, as at `Starter` in the pattern `Plan.Starter` and at the
    /// type parameter `T`. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "foldhash::HashSet::is_empty"))]
    unwritten: foldhash::HashSet<u32>,

    /// Start offset of every bare PHP# name, and of every PHP# type name that names a type parameter -> what it refers
    /// to. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    bindings: HashMap<u32, Binding>,

    /// The PHP# scope rules the bare names break, in source order. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Vec::is_empty"))]
    binding_errors: Vec<BindingError>,

    /// Start offset of every PHP# lambda -> the names and locals declared outside it that it uses, in first-use order.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    captures: HashMap<u32, Vec<(&'arena [u8], Local)>>,

    /// Declaration start offset of every PHP# local that code outside a lambda writes after its declaration -> the
    /// offset after which no such write runs: the end of its last write, or of the outermost loop that repeats the
    /// write and not the declaration.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    written_locals: HashMap<u32, u32>,

    /// Declaration start offset of every PHP# local that a lambda capturing it writes -> the start offset of the first
    /// such lambda in the file. That write may run at any time.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "HashMap::is_empty"))]
    assigning_lambdas: HashMap<u32, u32>,

    /// Declaration start offset of every PHP# local that code writes after its declaration, in a lambda or not, with a
    /// write it makes: every write [`Self::is_written`] counts but a call the binder takes as one from its name alone.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "foldhash::HashSet::is_empty"))]
    assigned_locals: foldhash::HashSet<u32>,

    /// The full name of each class-like of the file's namespace that wins over a default class-like the file names,
    /// whether the namespace holds it or not. Empty for PHP.
    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "foldhash::HashSet::is_empty"))]
    namespace_classes: foldhash::HashSet<&'arena [u8]>,
}

impl<'arena> ResolvedNames<'arena> {
    /// Returns the total number of resolved names stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Returns `true` if no resolved names are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Checks if a resolved name exists at the given source `Position`.
    #[must_use]
    pub fn contains(&self, position: &Position) -> bool {
        self.names.contains_key(&position.offset)
    }

    /// Gets the resolved name identifier for the given source position.
    ///
    /// # Panics
    ///
    /// Panics if no resolved name is found at the specified `position`.
    /// Use `contains` first if unsure.
    #[allow(clippy::expect_used)]
    pub fn get<T>(&self, position: &T) -> &'arena [u8]
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name).expect("resolved name not found at position")
    }

    /// Attempts to resolve the name at the given source position.
    ///
    /// Returns `Some(&str)` if a resolved name exists at the position, or `None` otherwise.
    pub fn resolve<T>(&self, position: &T) -> Option<&'arena [u8]>
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).map(|(_, (name, _))| *name)
    }

    /// Checks if the name resolved at the given position originated from an explicit
    /// `use` alias or construct.
    ///
    /// Returns `false` if the name was resolved relative to the namespace, is a
    /// definition, or if no name is found at the position.
    pub fn is_imported<T>(&self, position: &T) -> bool
    where
        T: HasPosition,
    {
        self.names.get(&position.offset()).is_some_and(|(_, (_, imported))| *imported)
    }

    /// Returns the resolved entry whose source range covers the given byte offset.
    ///
    /// Identifier ranges in `ResolvedNames` never overlap, so at most one entry can
    /// match. Returns `Some((start, end, fqn, imported))` for the covering entry, or
    /// `None` if the offset falls outside every tracked identifier.
    #[must_use]
    pub fn at_offset(&self, offset: u32) -> Option<(u32, u32, &'arena [u8], bool)> {
        self.names.iter().find_map(|(&start, &(end, (name, imported)))| {
            if start <= offset && offset < end { Some((start, end, name, imported)) } else { None }
        })
    }

    /// Iterates over every name the source writes as `(start, end, fqn, imported)`, as a reader of the source sees
    /// it: an extension host, or `mago cst --names`. In a PHP# file that is the name each form means, the same name the
    /// PHP that means it gives: `App\Plan` alone for the pattern `Plan.Starter`, as for `Plan::Starter`, and no name
    /// for a local or a type parameter. The other methods read the PHP each form runs as.
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32, &'arena [u8], bool)> + '_ {
        self.names
            .iter()
            .filter(|(start, _)| !self.unwritten.contains(*start))
            .map(|(&start, &(end, (name, imported)))| (start, end, name, imported))
    }

    /// Byte ranges `(start, end)` of every entry whose resolved name matches
    /// `fqcn`, compared case-insensitively (PHP name resolution is). This
    /// matches aliased uses too: `use Bar as Qux; Qux\G` resolves to `Bar\G`, so
    /// searching for `Bar\G` finds it where a raw text scan would not.
    ///
    /// `exclude_offset` drops the entry starting at that offset, e.g. to omit a
    /// declaration from its own reference list; pass `None` to keep every match.
    #[must_use]
    pub fn references_to(&self, fqcn: &[u8], exclude_offset: Option<u32>) -> Vec<(u32, u32)> {
        self.iter()
            .filter(|(start, _, _, _)| exclude_offset.is_none_or(|offset| offset != *start))
            .filter_map(
                |(start, end, name, _)| if eq_ignore_ascii_case(name, fqcn) { Some((start, end)) } else { None },
            )
            .collect()
    }

    /// Returns what the bare PHP# name starting at the given position refers to, or the type parameter a PHP# type name
    /// there names.
    ///
    /// Returns `None` for every name in a PHP file.
    pub fn binding<T>(&self, position: &T) -> Option<Binding>
    where
        T: HasPosition,
    {
        self.bindings.get(&position.offset()).copied()
    }

    /// Returns whether the PHP# type name starting at the given position names a type parameter.
    ///
    /// Returns `false` for every name in a PHP file.
    pub fn is_type_parameter<T>(&self, name: &T) -> bool
    where
        T: HasPosition,
    {
        matches!(self.binding(name), Some(Binding::TypeParameter { .. }))
    }

    /// Returns whether PHP# refuses the bound of `parameter`, a type parameter of `list`, spec section 11.
    ///
    /// It refuses one that [is no bound](Self::is_bound), as a union, `int`, `Shareable?` or `TItem & Shareable`, and
    /// one that [leads back to `parameter`](bounds_itself), as in `<T : T>` and `<TA : TB, TB : TA>`.
    ///
    /// The checker and the scanner both read this, so the bound the checker refuses is the bound the scanner reads as
    /// the refused type.
    #[must_use]
    pub fn is_refused_bound(
        &self,
        list: &TypeParameterList<'_>,
        parameter: &TypeParameter<'_>,
        in_standard_library: bool,
    ) -> bool {
        parameter
            .bound
            .as_ref()
            .is_some_and(|bound| !self.is_bound(&bound.hint, in_standard_library) || bounds_itself(list, parameter))
    }

    /// Returns whether `hint` is a bound PHP# writes, spec section 11.
    ///
    /// It is a type parameter alone, or [a class bound](Self::is_class_bound). `in_standard_library` also accepts the
    /// `int|float` the standard library's `sumOf` bounds its result by.
    #[must_use]
    pub fn is_bound(&self, hint: &Hint<'_>, in_standard_library: bool) -> bool {
        match hint {
            Hint::Identifier(name @ Identifier::Local(_)) if self.is_type_parameter(name) => true,
            Hint::Union(UnionHint { left: Hint::Integer(_), right: Hint::Float(_), .. }) => in_standard_library,
            hint => self.is_class_bound(hint),
        }
    }

    /// Returns whether `hint` is a class bound, spec section 11.
    ///
    /// It is `Object`, or a class or an interface, by name or as a generic class type, or several of them joined with
    /// `&`, as C#'s `class` constraint joins interfaces. `Equatable` is a name too. A type parameter is none of them.
    #[must_use]
    pub fn is_class_bound(&self, hint: &Hint<'_>) -> bool {
        match hint {
            Hint::Object(object) => object.value == OBJECT,
            Hint::Intersection(intersection) => {
                self.is_class_bound(intersection.left) && self.is_class_bound(intersection.right)
            }
            hint => self.joins_classes(hint),
        }
    }

    /// Returns whether `hint`, which [is no bound](Self::is_bound), is one once the parentheses it writes are gone, as
    /// `(Shareable)`, `(TItem)` and `(Shareable) & DatabaseEntity` are, so the parentheses are its one error, spec
    /// section 11.
    #[must_use]
    pub fn is_bound_in_parentheses(&self, hint: &Hint<'_>, in_standard_library: bool) -> bool {
        match hint {
            Hint::Parenthesized(parenthesized) => {
                self.is_bound(parenthesized.hint, in_standard_library)
                    || self.is_bound_in_parentheses(parenthesized.hint, in_standard_library)
            }
            Hint::Intersection(_) => self.joins_classes_in_parentheses(hint),
            _ => false,
        }
    }

    /// Whether `hint` [joins classes](Self::joins_classes), each of them in parentheses or not.
    fn joins_classes_in_parentheses(&self, hint: &Hint<'_>) -> bool {
        match hint {
            Hint::Parenthesized(parenthesized) => self.joins_classes_in_parentheses(parenthesized.hint),
            Hint::Intersection(intersection) => {
                self.joins_classes_in_parentheses(intersection.left)
                    && self.joins_classes_in_parentheses(intersection.right)
            }
            member => self.joins_classes(member),
        }
    }

    /// Whether `hint` is a class or an interface, by name or as a generic class type, or several of them joined with
    /// `&`.
    fn joins_classes(&self, hint: &Hint<'_>) -> bool {
        match hint {
            Hint::Identifier(name @ Identifier::Local(_)) => !self.is_type_parameter(name),
            // A dotted name is a case type or a full name, which the analyzer refuses with the import to add, spec
            // section 23.
            Hint::Identifier(Identifier::Dotted(_)) => true,
            Hint::Generic(generic) => {
                built_in_generic_arity(generic.name.value).is_none() && !self.is_type_parameter(&generic.name)
            }
            Hint::Intersection(intersection) => {
                self.joins_classes(intersection.left) && self.joins_classes(intersection.right)
            }
            _ => false,
        }
    }

    /// Returns the class of a PHP# static call, `Class.method()`: the object of `call` when the binder bound it to a
    /// class, or to a type parameter, which stands for the class of the type argument the code runs with. Returns
    /// `None` for an instance call and for every call in a PHP file.
    ///
    /// The checker and the engine both read this, so they never disagree on a static call.
    #[must_use]
    pub fn static_call_class<'ast>(&self, call: &MethodCall<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(call.object)
    }

    /// Returns whether `call` is the standard library's `Position.current()`, spec section 27: a static call of
    /// `Sharp\Position`'s `current` without arguments, both names compared ignoring case, as PHP compares them.
    /// Returns `false` for every call in a PHP file.
    ///
    /// The scanner, the checker and the engine all read this, so they never disagree on the caller's position.
    #[must_use]
    pub fn is_current_position(&self, call: &MethodCall<'_>) -> bool {
        self.static_call_class(call).is_some_and(|class| self.get(&class.name).eq_ignore_ascii_case(b"Sharp\\Position"))
            && matches!(&call.method, ClassLikeMemberSelector::Identifier(method) if method.value.eq_ignore_ascii_case(b"current"))
            && call.argument_list.arguments.is_empty()
    }

    /// Returns what a parameter's `default` takes of the caller's position, spec section 27: `Some(None)` for
    /// `Position.current()`, the whole position, and `Some(Some(field))` for one field of it, as
    /// `Position.current().line`, as C#'s `[CallerLineNumber]` and Swift's `#line` take the line alone. `None` for any
    /// other default, and for every default in a PHP file.
    ///
    /// The scanner, the checker and the engine all read this, so they never disagree on a caller's position default.
    #[must_use]
    pub fn caller_position_field<'ast>(&self, default: &Expression<'ast>) -> Option<Option<&'ast [u8]>> {
        match default {
            Expression::Call(Call::Method(call)) if self.is_current_position(call) => Some(None),
            Expression::Access(Access::Property(PropertyAccess {
                object: Expression::Call(Call::Method(call)),
                property: ClassLikeMemberSelector::Identifier(field),
                ..
            })) if self.is_current_position(call) => Some(Some(field.value)),
            _ => None,
        }
    }

    /// Returns the class of a PHP# static property access, `Class.name`: the object of `access` when the binder bound
    /// it to a class or to a type parameter, as [`Self::static_call_class`] does. Returns `None` for an instance
    /// property and for every access in a PHP file.
    ///
    /// The checker, the analyzer and the engine all read this, so they never disagree on a static property.
    #[must_use]
    pub fn static_property_class<'ast>(&self, access: &PropertyAccess<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        self.class_object(access.object)
    }

    /// Returns whether an accessor body of a PHP# property names `field` outside a lambda, as php-src finds
    /// `$this->name` in a hook outside a closure, nested blocks included. A lambda is a function of its own, where PHP
    /// calls the accessor again. Returns `false` for every property in a PHP file.
    ///
    /// The checker, the analyzer and the engine all read this, so they never disagree on a backed property.
    #[must_use]
    pub fn uses_field(&self, accessors: &PropertyHookList<'_>) -> bool {
        let mut field_use = FieldUse { names: self, lambdas: 0, found: false };
        walk_property_hook_list_mut(&mut field_use, accessors, &mut ());

        field_use.found
    }

    /// Returns whether a PHP# property has storage: an auto accessor, `get;` or `set;`, or a body that
    /// [uses `field`](Self::uses_field). A property without storage runs as PHP's virtual property.
    ///
    /// The checker and the analyzer both read this, so they never disagree on a virtual property.
    #[must_use]
    pub fn has_storage(&self, accessors: &PropertyHookList<'_>) -> bool {
        accessors.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Abstract(_)))
            || self.uses_field(accessors)
    }

    /// The class name of a member's object: a bare name bound to a class or a type parameter, or the class of
    /// `Counter<Order>`, whose type arguments a caller that reads them takes from the object.
    fn class_object<'ast>(&self, object: &'ast Expression<'ast>) -> Option<&'ast ConstantAccess<'ast>> {
        match object {
            Expression::ConstantAccess(access)
                if matches!(self.binding(&access.name), Some(Binding::Class | Binding::TypeParameter { .. })) =>
            {
                Some(access)
            }
            Expression::GenericClass(GenericClass { class, .. }) => self.class_object(class),
            _ => None,
        }
    }

    /// Returns the PHP# scope rules the bare names break, in source order.
    #[must_use]
    pub fn binding_errors(&self) -> &[BindingError] {
        &self.binding_errors
    }

    /// Returns the name and local of each local declared outside the PHP# lambda starting at the given position that
    /// the lambda uses, in the order it first uses them. A lambda inside another captures what it uses for both.
    pub fn captures<T>(&self, lambda: &T) -> &[(&'arena [u8], Local)]
    where
        T: HasPosition,
    {
        self.captures.get(&lambda.offset()).map_or(&[], Vec::as_slice)
    }

    /// Returns whether code writes the PHP# local after its declaration: with `=`, a compound assignment, `++` or
    /// `--` on it or an index of it, or a call of one of the project's
    /// [`ChangingMembers`](crate::resolver::ChangingMembers) on it, as `add`. The binder knows no types, so a call of a
    /// method of one of those names on an object writes its local too, which captures it by reference, as harmless.
    #[must_use]
    pub fn is_written(&self, local: &Local) -> bool {
        let declaration = local.declaration.start.offset;

        self.written_locals.contains_key(&declaration) || self.assigning_lambdas.contains_key(&declaration)
    }

    /// Returns whether code writes the PHP# local after its declaration with a write it makes: `=`, a compound
    /// assignment, `++` or `--` on it or an index of it, in a lambda or not. A call of one of the
    /// [`ChangingMembers`](crate::resolver::ChangingMembers) writes the local without assigning it, since the binder
    /// takes it as a write from its name alone.
    #[must_use]
    pub fn is_assigned(&self, local: &Local) -> bool {
        self.assigned_locals.contains(&local.declaration.start.offset)
    }

    /// Returns the start offset of a lambda that captures the PHP# local and writes it, so the write may run at any
    /// time, as Kotlin's "local variable that is captured by a changing closure" is: the first such lambda in the file.
    #[must_use]
    pub fn lambda_that_assigns(&self, local: &Local) -> Option<u32> {
        self.assigning_lambdas.get(&local.declaration.start.offset).copied()
    }

    /// Returns whether a write of the PHP# local can run after the code at `offset`: a write after it, a write in a
    /// loop that repeats both, or a write in a lambda, as TypeScript 5.4 decides whether a closure keeps a narrowing.
    #[must_use]
    pub fn is_written_after(&self, local: &Local, offset: u32) -> bool {
        self.lambda_that_assigns(local).is_some()
            || self.written_locals.get(&local.declaration.start.offset).is_some_and(|settled| *settled > offset)
    }

    /// Returns the full name of each class-like of the PHP# file's namespace that would win over a default class-like
    /// the file names, spec section 23, as `App\Cache` for a bare `Cache` in the namespace `App`. A bare name means
    /// that class-like when the project declares it, so declaring or removing one changes what the file means.
    pub fn namespace_classes(&self) -> impl Iterator<Item = &'arena [u8]> + '_ {
        self.namespace_classes.iter().copied()
    }

    pub(crate) fn bind(&mut self, span: Span, binding: Binding) {
        self.bindings.insert(span.start.offset, binding);
    }

    pub(crate) fn capture(&mut self, lambda: u32, name: &'arena [u8], local: Local) {
        let captures = self.captures.entry(lambda).or_default();
        if !captures.iter().any(|(_, captured)| *captured == local) {
            captures.push((name, local));
        }
    }

    /// Records a write of `local` whose effect may run until `settled`.
    pub(crate) fn write(&mut self, local: Local, settled: u32) {
        let last = self.written_locals.entry(local.declaration.start.offset).or_default();
        *last = (*last).max(settled);
    }

    /// Records a write of `local` in the lambda that starts at `lambda`, which may run at any time.
    pub(crate) fn write_in_lambda(&mut self, local: Local, lambda: u32) {
        self.assigning_lambdas.entry(local.declaration.start.offset).or_insert(lambda);
    }

    /// Records that a write the code makes assigns `local`, see [`Self::is_assigned`].
    pub(crate) fn assign(&mut self, local: Local) {
        self.assigned_locals.insert(local.declaration.start.offset);
    }

    pub(crate) fn add_namespace_class(&mut self, name: &'arena [u8]) {
        self.namespace_classes.insert(name);
    }

    pub(crate) fn report_binding_error(&mut self, error: BindingError) {
        self.binding_errors.push(error);
    }

    /// Inserts a resolution result into the map (intended for internal use).
    ///
    /// The full source span of the identifier is stored, so [`at_offset`](Self::at_offset)
    /// and other range-based lookups work without re-scanning the source.
    pub(crate) fn insert_at(&mut self, span: Span, name: &'arena [u8], imported: bool) {
        self.names.insert(span.start.offset, (span.end.offset, (name, imported)));
    }

    /// Inserts a name the PHP a PHP# form runs as reads at `span`, where the source names nothing.
    pub(crate) fn insert_lowered_at(&mut self, span: Span, name: &'arena [u8], imported: bool) {
        self.insert_at(span, name, imported);
        self.unwritten.insert(span.start.offset);
    }
}

/// Finds `field` in accessor bodies outside the lambdas they hold, for [`ResolvedNames::uses_field`].
struct FieldUse<'names, 'arena> {
    names: &'names ResolvedNames<'arena>,
    /// How many lambdas hold the node being walked.
    lambdas: u32,
    found: bool,
}

impl<'ast, 'arena> MutWalker<'ast, 'arena, ()> for FieldUse<'_, '_> {
    fn walk_in_arrow_function(&mut self, _arrow_function: &'ast ArrowFunction<'arena>, _context: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_arrow_function(&mut self, _arrow_function: &'ast ArrowFunction<'arena>, _context: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_closure(&mut self, _closure: &'ast Closure<'arena>, _context: &mut ()) {
        self.lambdas += 1;
    }

    fn walk_out_closure(&mut self, _closure: &'ast Closure<'arena>, _context: &mut ()) {
        self.lambdas -= 1;
    }

    fn walk_in_constant_access(&mut self, constant_access: &'ast ConstantAccess<'arena>, _context: &mut ()) {
        self.found |= self.lambdas == 0 && self.names.binding(&constant_access.name) == Some(Binding::Field);
    }
}

/// Case-insensitive byte equality, routing equal-length inputs through
/// `mago_word`'s SIMD prefix comparison.
#[inline]
fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && mago_word::starts_with_ignore_case(a, b)
}
