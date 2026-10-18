use std::collections::BTreeMap;

use mago_bytes::BytesDisplay;
use mago_casing::is_camel_case;
use mago_casing::is_class_case;
use mago_casing::is_constant_case;
use mago_casing::to_camel_case;
use mago_casing::to_class_case;
use mago_casing::to_constant_case;
use mago_database::file::FolderNamespace;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::MethodParts;
use mago_names::binding::SUPERGLOBALS;
use mago_names::binding::php_method_name;
use mago_names::binding::php_operator_name;
use mago_names::scope::php_name;
use mago_reporting::Annotation;
use mago_reporting::AnnotationKind;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::Array;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::BinaryPattern;
use mago_syntax::cst::BindingValue;
use mago_syntax::cst::Block;
use mago_syntax::cst::Break;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::ComparisonPattern;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Construct;
use mago_syntax::cst::ConstructorInitializer;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCaseDataItem;
use mago_syntax::cst::Event;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extension;
use mago_syntax::cst::Extern;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::GenericHint;
use mago_syntax::cst::Global;
use mago_syntax::cst::Hint;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::Leave;
use mago_syntax::cst::ListPatternElement;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::LocalTarget;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::NamedConstructor;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::NullSafeMethodCall;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::Operator;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::PatternMatch;
use mago_syntax::cst::PatternMatchArm;
use mago_syntax::cst::PatternMatchArmBody;
use mago_syntax::cst::Program;
use mago_syntax::cst::PropertiesPattern;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyBehaviorList;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::ReadonlyHint;
use mago_syntax::cst::Return;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Struct;
use mago_syntax::cst::Terminator;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TryFinallyClause;
use mago_syntax::cst::TupleHint;
use mago_syntax::cst::TypeOf;
use mago_syntax::cst::TypeParameter;
use mago_syntax::cst::TypeParameterBound;
use mago_syntax::cst::TypeParameterList;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UnionHint;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::WhileBody;
use mago_syntax::cst::Yield;
use mago_syntax::cst::YieldFrom;
use mago_syntax::cst::YieldPair;
use mago_syntax::cst::YieldSpread;
use mago_syntax::cst::YieldValue;
use mago_syntax::cst::built_in_generic_arity;
use mago_syntax::error::ParseError;
use mago_syntax::token::GetPrecedence;
use mago_syntax::token::Precedence;
use mago_syntax::token::is_sharp_keyword;
use mago_syntax::utils::pattern::has_type_test;
use mago_syntax::walker::Walker;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_syntax_core::utils::is_part_of_identifier;
use mago_syntax_core::utils::is_start_of_identifier;

use crate::internal::checker::class_like::check_law_parameters;
use crate::internal::consts::RESERVED_CLASS_NAMES;
use crate::internal::context::Context;

/// How PHP# writes PHP's `mixed`. The parser reads both as `Hint::Mixed`, and only `Any` is PHP#.
const ANY: &[u8] = b"Any";

/// How PHP# writes PHP's `object`, spec section 24. The parser reads both as `Hint::Object`, and only `Object` is PHP#.
const OBJECT: &[u8] = b"Object";

/// Checks a PHP# file against the slice: the only constructs a `.sharp` file may use, and the contract the
/// engine's lowering implements.
///
/// - At file level: `namespace`, `import`, `class`, `interface` and `enum`. A file has at most one namespace, named
///   and written without braces.
/// - A class: attributes, an optional `public`, `abstract` or `final`, a name, optional type parameters, an optional
///   `: Base, Interface` header, constants, fields, properties, methods, named constructors, operators, laws and, in a
///   static class, extensions, with no other modifiers, `extends` or `implements`. The engine tells the base class from
///   the interfaces when it links the class.
/// - An extension, spec section 26: one receiver parameter and the methods and computed properties it adds, each with
///   only an access modifier. `check_extension` holds its rules.
/// - A member without an access modifier is `private`, spec section 5, outside an interface.
/// - A named constructor, spec section 9.1: an optional access modifier, its class's name, a `.` and its own name, parameters
///   that declare no property, `: this(…)` without `this` in its arguments, and a body. `check_named_constructor`
///   decides each part, and an interface, an enum and a static class declare none.
/// - A law, spec section 28, in a class or an enum: `law`, a name, parameters checked as a method's and an expression
///   body. `check_members` refuses a default and a variadic parameter, because a law's parameters range over every
///   value.
/// - A static class, `public static class Text`, as spec sections 26 and 29 write it: `static` and an optional
///   `public`, no header and no constructor, and only constants and static members. The bridge lowers it to a final
///   PHP class.
/// - An interface: attributes, an optional `public`, a name, optional type parameters, an optional `: Interface` header,
///   constants, methods, properties and operators, with no other modifiers, `extends`, fields or laws. A constant
///   follows a class constant's rules. An interface method has optional type parameters, parameters, a return type
///   and no body, as spec section 29 writes `Money quote(Cart cart);`, or a default body, a block or `=> expr;`, as
///   spec section 22 writes `string designKey() => …;`, checked as a class method's body. A property is written
///   `int orderId { get; }` or `List<string> claims { get; set; }`, and a computed property's expression is its
///   default body. An interface holds no storage, so a property with an initial value is an error, and so is one with
///   an accessor body, which the computed form writes. Every interface member is public, written bare, so an access
///   modifier on one is an error, and so is any other modifier on a method.
/// - Type parameters, as spec section 11 declares them: on a class, an interface and a method, as in
///   `<out TItem : DatabaseEntity & Shareable, TKey>`. A name is `T`, or `T` and an uppercase ASCII letter, as in
///   `TItem`, and any other name is an error, as C# names them. A list that declares a name twice is an error. `in` and
///   `out` on a method's type parameter are errors, because only a class or an interface declares variance, spec
///   section 11.1. A bound is `Object`, a class or an interface, a generic class type, as in `Comparable<TItem>`,
///   several classes and interfaces joined with `&`, or another type parameter alone, as in `<TSub : T>`, and any
///   other bound is an error. A type parameter is in scope in its class's or interface's
///   whole declaration, its header included, and in its method's signature and body, as the binder decides. A
///   constant uses none of its class's type parameters, because every `Box<…>` shares it. A static field, property or
///   method uses them, because each type argument has its own, as `Box<int>.count` reaches it. The analyzer checks
///   variance, bounds and inference.
/// - An enum: attributes, an optional `public`, a name, an optional `: string, Interface` header, constants, cases,
///   methods, operators and laws, with no other modifiers or `implements`. A leading `int` or `string` in the header is
///   the backing type, and every class name is an interface. A constant follows a class constant's rules. A case has
///   attributes as a class has them, and is `case Active;` or, in a backed enum, `case Active = "a";`, whose value is
///   a constant expression. A case named `class`, compared ignoring case, is an error, as in PHP. A method follows a
///   class's method rules, but an enum has no constructor, so a method without a return type is an error. PHP's
///   `check_enum` reports a property and any other backing type, and the analyzer a class name in the header.
/// - A header of a class, an interface or an enum that names one type twice is an error, and so is an enum header that
///   names `UnitEnum` or `BackedEnum`, because the engine refuses both when it declares the class. A header names each
///   class and interface by its short name, and a generic one with its type arguments, as in `: PaginatedList<Order>`,
///   whose type arguments are a parameter's types. Any other type, a type parameter, `List`, `Map` and `Class`
///   included, is an error that says a header names classes and interfaces.
/// - Attributes: on a class, an interface, an enum, an enum case, an event, a constant, a method, an interface method, a
///   field, a property, an accessor, a parameter and a lambda with its parameters in parentheses, written `[Name]` or
///   `[Name(arguments)]`, several in one list, as in `[Field("Name"), Searchable]`, or in several lists. A list may
///   open with a target, as in `[param: Trimmed]`, which `check_attribute_target` checks, and an observer, which runs
///   inside `set`, takes none, as `check_accessors` decides. An argument is positional or named, and is a
///   constant expression, list and map literals included, or a `new` of a class by its name
///   whose arguments are such values, as `check_attribute_object` decides. Anything else is an error that names the
///   rule. PHP's own check reports a spread argument. An attribute on a type parameter is a parse error.
/// - A constant: an optional `public`, `protected` or `private`, an optional type from the types below but `void`, one
///   name, and a constant expression as its value. Several names in one declaration are an error.
/// - A field: an optional `private` or `protected`, an optional `static`, a type, one name and an optional initial
///   value. A `public` field is an error, because spec section 6 has no public fields, and so are several names.
/// - An override: a field written `override`, which replaces a plain PHP parent's property, spec section 6.1. It may
///   be `public`, as the parent's property is, and has a constant initial value. The analyzer checks the parent.
/// - A static field or property: an initial value, and a `set` accessor, because PHP has no `readonly` static
///   property.
/// - An initial value: any expression a method body has, without `this`, which is an error. A constant initial value,
///   as a parameter default is, becomes the member's default, and any other runs at the start of the constructor, or
///   a static's when a request first uses its class's statics.
/// - A property: an optional `public`, `protected` or `private`, an optional `static`, a type, one name, the accessors `get` and
///   an optional `set` that may take an access modifier narrower than the property's, and an optional initial value
///   after the accessors, which is a field's. A property without `get`, an accessor declared twice, or a `set` access
///   modifier as wide as the property's is an error, as in C#. A get-only property is set only where `readonly`
///   allows, and the analyzer reports every other write to it.
/// - An accessor: `get;` or `set;`, which is an auto accessor, or a body, `=> expr;` or a block, which is a method
///   body's. A body names the property's storage `field`, and a `set` body names the incoming value `value`. A
///   property has storage when an accessor is auto or a body uses `field`, and a property without storage takes no
///   initial value and is not declared by a constructor parameter, because PHP runs it as a virtual property. A `get`
///   block returns a value and a `set` block returns none, as php-src types its hooks. `field` in a lambda is an
///   error, because PHP runs a lambda as a function of its own, where `$this->name` calls the accessor again.
///   `this.name` or `this?.name` inside `name`'s own accessor is an error that names `field`, because PHP reads the
///   storage there, while C# calls the accessor again. PHP has no hooks on a static property, so a static property's
///   accessor bodies run as hidden static methods of its class. An initial value fills the storage without running a
///   `set` body.
/// - Observers, spec section 6.2: `willSet` and `didSet` with a body, beside a `set` accessor, run on every write
///   once the object is built, so never on an initial value or a write while a constructor runs. `willSet` sees
///   `value` and reads the value it replaces in `field`, and `didSet` sees `field`. `willSet` neither returns nor
///   writes `value`, and `didSet` returns no value.
/// - Behaviors, spec section 6.4: `via` and class names after the accessors of an instance auto-property with
///   `get; set;`, and the analyzer checks each is a `Behavior` for the property's type.
/// - A computed property: `public`, `protected` or `private`, an optional `static` or `lazy`, a type, one name and an
///   expression body, as in `public string slug => Str.slug(name);`. Its expression is a method body's expression and
///   runs on each read, or once when `lazy`. A `static` one runs as a hidden static method of its class, because PHP
///   has no hooks on a static property.
/// - A method: `public`, `protected` or `private`, an optional `static`, `virtual` or `override`, `final` beside
///   `override`, optional type parameters, parameters, a return type and a body, or `abstract` and no body. The analyzer
///   decides where `virtual` and `override` replace a method, as spec section 22 writes them, and
///   `check_method_modifiers` refuses one that can't matter. Its name does not start with `__`, which PHP reserves for
///   magic methods, and is not its class's name,
///   compared ignoring case, which PHP# gives to the constructor, nor, compared ignoring case, a property's of its
///   class.
/// - An operator, as spec section 19 writes `public static Money operator +(Money a, Money b) => …;`: `public static`,
///   a return type, `operator`, one of `+ - * / % **`, unary `-`, `==` and `<=>`, parameters and a method body.
///   `!=` derives from `==`, and `< > <= >=` from `<=>`, so declaring one is an error that names its source. `==`,
///   `<=>` and the binary arithmetic operators take two parameters, unary `-` one, and one of them is the declaring
///   class. `==` returns `bool`, and `<=>` `int`. A class declares each operator once, because each runs as one
///   static method named after .NET's, such as `op_Addition`. An enum declares an operator as a class does, and an
///   interface as `static int operator <=>(Priced a, Priced b)`, `static` alone, as its other members are written bare.
/// - An `extern` method, `public static extern string slug(string title);`, whose body is native, compiled into the
///   engine, as spec section 29 writes it: a `public static` method of a static class, with no body. Only the
///   standard library declares one, and the analyzer refuses one anywhere else.
/// - The constructor: a method named exactly after its class, without a return type and not `static`. A method
///   without a return type named otherwise is an error. A constructor parameter with an access modifier declares a
///   member: a field when `private` or `protected` without accessors, and a property with accessors, which follow the
///   auto-property rules. A `public` parameter without accessors is an error, as spec section 9 says. The constructor
///   alone may be `required`, which keeps it callable as `new Self(…)` on every subclass, spec section 25. The parser
///   reports `required` on a method or an operator. A class has one constructor, and `check_members`
///   reports a second one, because PHP# has no overloading by argument types. A parameter cannot be `lazy`, because a
///   lazy property computes its own value. The constructor passes
///   control to its base class with `: super(…)`, whose arguments are a method body's expressions without `this`,
///   because they run before the object exists, as C#'s CS0027 says. `: this(…)` belongs to a named constructor, so on
///   the main one it is an error. The analyzer checks the arguments against the base class's constructor. A
///   constructor runs once, when `new` creates the object, so a call of a member named `__construct` is an error.
/// - A parameter: always a type, a name and an optional default, and never by reference. The parser reports a
///   parameter written as a computed property, because a parameter receives its value. A default is any expression
///   a method body has, which runs each time a call leaves the argument out, spec section 7. A parameter written
///   `int ...values` is variadic, as spec section 7 writes it, and PHP's own checks report a variadic parameter that is not the last or has a default. `check_parameter_list` reports a
///   variadic parameter that declares a member or has a `void` type in a `.sharp` file only, as upstream Mago reports
///   neither. An optional parameter before a required one is an error, because PHP would make it required. A
///   variadic parameter is not a required one, as in PHP. A default of `Position.current()` of the standard library is
///   the caller's position, spec section 27, which a PHP# call passes. A default that reads one part of it, as
///   `Position.current().line`, is an error, because the parameter takes the whole position.
/// - Types: `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter in scope written by its short name,
///   generic types of these, function types `Function<R(P1, P2)>` of these, tuple types `[Money total, int count]` of
///   these but `void`, spec section 30, whose element names are optional and past four of which `check_tuple`
///   suggests a struct, and `void` as a return type, a function
///   type's too. A generic type is `List<T>` and `Map<TKey, TValue>` of spec section 12, `Class<T>` of section 25,
///   whose type argument is any one type, a nullable or union type included, as `typeof` takes it, and a class with
///   one or more type arguments of section 11, as in `PaginatedList<Order>`, whose arity and bounds the analyzer
///   checks. A type argument is any of
///   these types but `void`, and a type parameter takes none. A `Map`'s key is `int` or `string`, or a type the
///   analyzer finds an `int` or `string` backing value for. PHP's own check reports a `void` parameter and a `void`
///   field. `never` is a method's return type alone, spec section 7. Each of them is nullable when written with `?`
///   after it, as in `int?` or `Map<string, Any?>`, and PHP's own check reports `void?` and `never?`. PHP's `mixed` is an error, because spec section 24 writes it `Any?`. A union of them but
///   `void`, written inline as spec section 24 writes it, as in `int|string` or `List<int>|string`, goes wherever a
///   type goes, and each member takes the type arguments it takes outside a union. PHP's own check reports `void`,
///   `never` and a nullable type, as in `int?|string`, in a union, and `check_union` reports a type written twice, which the
///   engine refuses. A union holds null only when written in parentheses with `?` after it, as in `(int|string)?`,
///   which the engine compiles as `int|string|null`.
///   `check_union` reports `null` written in a union, as in `int|null`, with its nullable form. PHP's own check reports
///   a nullable union inside another union, and a single type in parentheses, as in `(Calc)?`, which PHP# writes
///   `Calc?`. `(int)?` is a parse error, as PHP lexes `(int)` as a cast. A member of a nullable type has no implicit
///   starting value: a field or a property with storage writes `= null` as any other initial value, or the
///   constructor sets it, and the analyzer reports a path through the constructor that leaves it unset. A property
///   without storage holds nothing to start with.
/// - `Self`, written exactly so, is the class a static method is called on, spec section 25, and PHP's `static`. It is
///   a method's return type only, as in Swift, nullable as in `Self?`, in a union as in `Self|int`, or a type argument
///   there as in `List<Self>`, and any other type position, a function type's included, is an error. The lexer reads
///   `Self` and `self` as one keyword, and every other spelling is PHP's `self`, which PHP# removes: `self` and
///   `static`, as a type or in an expression, are errors that name the class, the enum or `Self`.
/// - A method body: a block, or an expression body, `=> expr;`, which returns the expression, or runs it as a
///   statement in a `void` method and the constructor, as in C#.
/// - In a method body: blocks, expression statements, `return`, `let` and `const` declarations, `if` with `else if`
///   and `else`, `while`, `do … while`, `for` with a `let` or `const` counter or with expressions, `for … of` over a
///   value or a key and value, `break` and `continue` without a level, and `try` with `catch` clauses and `finally`.
///   The body of `if`, `else` and each loop is a block in braces, the guard clause
///   `if (!tenant.active) { throw new InactiveTenant(tenant); }` too. A catch clause names one or more classes separated
///   by `|`, and an optional variable written without `$`, which lives until the clause's block ends. A local
///   statement can have its type written, as in `Money? total = null;` or `const int base = 2;`, from the types
///   above but `void`, or unpack a tuple into a local per element with `let` or `const`, as in
///   `const [total, count] = summary;`, spec section 30. An expression statement whose operator is binary, as in `flags | MASK;`, is an error that
///   names its compound assignment or its result, because the result is never used, except `??` and an assignment
///   that PHP's `and`, `or` and `xor` bind looser than.
/// - `yield value;` and `yield ...other;`, spec section 12: statements of a method that returns `Iterable<T>` or
///   `Iterable<T>?`, which PHP compiles as a generator too, outside any lambda in it. `yield` gives no value back, so
///   any other use of it is an error, and so is `return value;` in a method that yields, as C#'s CS1622. PHP's
///   `yield from`, `yield key => value` and bare `yield` are errors that name the PHP# form.
/// - Writes: `=`, compound assignment, `++` and `--` write only a local, a parameter, a member written
///   `object.name` or `object?.name`, which spec section 14.4 writes only when every `?.` receiver on its left is not
///   null, or, when static, `Class.name`, or an index of one of them written `target[key]`. The analyzer,
///   which knows the types, allows an index write only on a `Map`, and a read not under `??` or `?.` only on a `List`,
///   as spec section 12 decides.
/// - A constant, an enum case or a static member is reached through its class name, as in `Report.count`, and the
///   engine looks up which one it is when it runs. A bare constant or static member name is an error that names the
///   class. A constant expression, which is an attribute argument, a constant's value, a constant initial value, an
///   enum case's value or a pattern's value, reads only a constant or an enum case this way, as PHP does: a static
///   field or property there is an error when this file declares it, and the analyzer reports it otherwise. A
///   pattern's value reads no local, parameter or `this`, as spec section 21 says: their values are compared in `when`.
/// - A constant expression is built from constants: literals, constants, enum cases, a backed case's `value` and a
///   plain case's `name`, `typeof(X)`, list and map literals, indexes, templates, the ternary, `match` with constant
///   arms, `(int)`, `(float)` and `(string)`, the operators below, and lambdas that capture nothing. Any other
///   expression runs code, as a call and `new` do, so it is an error that says to use a static field, or `when` in a
///   pattern, and a lambda that captures a local or `this` is an error that names what it captures. An attribute
///   argument also creates an object of constants.
/// - In expressions: literals, list literals `[a, b]` and `[...a, b]`, map literals `["key": value]` and `[:]`, index reads
///   `value[key]`, templates, parentheses, bare names, assignment, the operators below, and method calls and property
///   reads written with `.` or `?.` and a member name, `new Class(...)` and `new Class.name(...)` on a class written by
///   its short name, `new Self(...)` in a class whose constructor is `required`, calls of a function by its bare name,
///   `super.method(...)`, which calls the parent's method, and `Self.method(...)`, which calls a static method of the
///   class a static method is called on, each with positional, named and spread arguments, as in
///   `Money.sum(...prices)` and `max(...prices)`, `throw`, which is an expression as in PHP, `exit(code)` and
///   `exit()`, which spec section 8 keeps as PHP 8.4's built-in function, and `typeof(X)` on a class written by its
///   short name, without a member read or called on it. `new` and each method call, `super.` and
///   `Self.` calls included, take type arguments, as in `new PaginatedList<Order>(rows)` and
///   `Json.decode<WebhookPayload>(body)`, each a method body's type but `void`. `new Self(...)` in an enum is an error,
///   because an enum has no constructor. `Self.name` read or written is not supported yet, and `super`
///   is only the object of a method call, spec section 22.
///   PHP's own check reports a positional argument after a spread and a spread after a named argument, and
///   `check_function_call` reports `assert` with a spread as its only argument, after which PHP adds a positional
///   description. `?.` never follows a class. A function is the global function of that name, PHP's own or one a
///   library or the app declares, as spec sections 8 and 29 keep them, and the engine calls the global one. A string
///   literal's `\u{...}` escapes are valid codepoints, as PHP requires. A `"…"` string never interpolates, and a
///   template, `` `Order ${number}` ``, interpolates any expression of this list in each `${…}` and takes
///   JavaScript's escapes, as spec section 18 writes them.
/// - Lambdas, as spec section 3 writes them: `x => x.id`, `(a, b) => a + b` and `() => { … }`, whose body is an
///   expression or a block of a method body. A parameter is a method's, with its type optional, so the last one can
///   be variadic, as in `(int ...values) => count(values)`. A lambda captures the variable itself, and a loop
///   variable is each pass's own, spec section 3. A call of a local by its bare name, `f(x)`,
///   calls the lambda the local holds, with positional, named and spread arguments.
/// - Operators: `+ - * / % **`, `== != === !== < > <= >=`, `&& || !`, `??`, unary `-` and `+`, `++` and `--`, and
///   `= += -= *= /= **= ??=`. Spec section 19 puts `<`, `<=`, `>`, `>=`, `is` and `as` in one row and `==`, `!=`,
///   `===` and `<=>` in the next, and an operator of either row that takes an operand of its own row without
///   parentheses, as in `a < b < c` or `a is T is U`, is an error that names the groupings to write. So is a comparison
///   in the value of a comparison or value pattern, as in `x is > a < b`.
/// - No `@` before a value: `@` before a name makes a keyword a name, spec section 2, and a PHP built-in's failure is
///   the value it returns, which the bridge runs without PHP's warning.
/// - The ternary `c ? a : b` in a method body, as spec section 21 writes it. PHP's `a ?: b` is an error, and
///   `a ? b : c ? d : e` is `a ? b : (c ? d : e)`, as in C#.
/// - Pattern matching in a method body, as spec section 21 writes it: `x is pattern`, `x as T` to a type that is not
///   nullable or `void`, and `match`, whose arms are `pattern => value`, `pattern when condition => value` and one
///   `default => value`. A `match` needs `default` unless its arms name only `Class.y` values or `null`, as a `match`
///   on an enum does, and the analyzer reports the cases such a `match` misses. A `match` statement's arm may be a
///   block. A pattern is a type with an optional name, as in `int count`, a case of an enum with data with its fields
///   by position, as in `PaymentResult.Paid(string transactionId)`, a value, a comparison such as `< 10`, a
///   properties pattern such as `{ total: > 0 }`, a list pattern such as `[Line first, ...]`, or patterns joined by
///   `and`, `or` and `not`. A type pattern is never nullable, and a name under `or` or `not` is an error, as C#'s
///   CS8780, except under the `not` that starts the pattern of `is`. `not` beside `or` is an error that names both
///   readings, as decision 045 decides, and `!x is T`, which reads as `(!x) is T`, is an error that writes
///   `x is not T`, as decision 044 decides. A list pattern has one rest.
/// - What reads a type argument while the code runs, which R3 compiles, passes this check: a type parameter in a
///   pattern, a `match` arm, `as` or a catch clause, `typeof` and `new` of a type parameter, a static member reached
///   through a type parameter, as in `TItem.make()` and `TItem.LIMIT`, and a type with type arguments in a pattern or
///   `as`, as in `value is List<int>`. The analyzer refuses what no test can read, as a plain PHP class's type
///   arguments. A named constructor through a type parameter, as in `new TItem.from(…)`, is not supported yet.
/// - Casts: `(int)`, `(float)` and `(string)` in a method body, as spec section 24 writes them. PHP's other casts and
///   its cast aliases, such as `(bool)` and `(integer)`, are errors. A cast is lowercase only, so the lexer reads
///   `(Int)` as the class `Int` in parentheses, as in `typeof(Int)`.
/// - A bare `Int`, `Float`, `Bool`, `Position`, `Environment` or `List` names the class `Sharp\<Name>` of the
///   standard library wherever a class is named, unless the file imports or declares a class-like of that name, spec
///   section 23. So `Int.parse(text)`, `Bool.tryParse(value)`, `Position.current()`, `new Environment()` and
///   `List.wrap(value)` call it. PHP reserves `Int`, `Float`, `Bool` and
///   `List`, so no file declares or imports a class of those, except the standard library's own `Sharp.Int`,
///   `Sharp.Float` and `Sharp.Bool`: a file whose namespace is exactly `Sharp` declares them, as the engine allows,
///   and any file may import them.
///
/// The check runs on every node the checking walk enters, and refuses any node, or any position of a node, that this
/// list does not name. It reports each refusal once, at its outermost node, and skips the nodes inside the refusal's
/// span. A node refused at a part, such as a method at its name or a cast at its operator, keeps its other parts
/// checked. It does not run on a file with a parse error, which is the one error to fix first, except a keyword in
/// another case, which reads as the keyword and leaves the tree whole. A node outside this list is a form
/// `is_undecided` names, which is not supported yet, a PHP# form that breaks a rule of its place, which `broken_rule`
/// names, or a form PHP# never writes, which fails as a typo does, at the first token PHP# has no place for. Such
/// forms include `$` variables, `global`, `echo`, `print`, `switch`, the word operators `and`, `or` and `xor`,
/// superglobals, magic constants and other `__Something__` names, every cast but `(int)`, `(float)` and `(string)`, and
/// a method name that starts with `__`.
///
/// Twenty-two more refusals need inferred types or the codebase, so the analyzer makes them as its part of this
/// contract:
/// - `new` on a static class, in `analyze_class_instantiation`, and a class that extends one, in
///   `check_class_like_extends`, because the static class may be another file's.
/// - an `extern` method anywhere but a standard library file whose class is under `Sharp`, in `Method`'s `analyze`,
///   `@` in any file that is not the standard library's, in `UnaryPrefix`'s `analyze`, and `namespace Sharp` or a
///   namespace under it, which alone declares `Sharp.Int`, `Sharp.Float` and `Sharp.Bool`, in any file that is not the
///   standard library's, in `report_standard_library_namespace`, because only the analyzer knows the file's package.
///   The nearest `composer.json` above a file names it.
/// - a bare call that finds no function while the enclosing class declares a method of that name, in
///   `bare_member_call`, because only the codebase knows which functions exist.
/// - a bare call of a plain PHP function, `exit` included, which spec section 23 makes an error, in
///   `report_bare_function_call`, because only the codebase knows the standard library member that replaces it.
/// - `+` that may join a string with any other value, which spec section 18 makes an error, in
///   `analyze_arithmetic_operation`. `+` on two strings joins them.
/// - a condition of `if`, `while`, `do … while`, `for` or `? :`, or an operand of `&&`, `||` or `!`, that is not
///   `bool`, which spec section 21 makes an error, in `Context::report_non_bool_condition`.
/// - a cast of a value that is not an `int` or a `float`, which spec section 24 makes an error, in `UnaryPrefix`'s
///   `analyze`.
/// - a spread of a value that is not a list, which spec section 7 spreads, in `report_non_list_spread`.
/// - a named argument that names a variadic parameter or no parameter, on a method or a plain PHP function that
///   takes any number of arguments too, which spec section 16 makes an error, in `analyze_invocation`.
/// - a default or an initial value that does not fit its declared type, such as `null` for an `int` or `false` for
///   an `int|string`, which the engine refuses, in `check_parameter_default_value` and the property's
///   `analyze_default_value`. A nullable type, as in `int? total = null`, holds `null`.
/// - a constructor of any descendant of a class whose constructor is `required` that changes its parent
///   constructor's parameters or adds one without a default, which spec section 25 makes an error, in
///   `validate_method_signature_compatibility`, as PHP's `@consistent-constructor` does for a child.
/// - an override of a method with a variadic parameter that is not variadic, which spec section 7 makes an error and
///   the engine refuses, in `validate_method_signature_compatibility`.
/// - a full name in code, such as `App.Shared.Money.of(1)`, which spec section 23 keeps in `import` lines, in
///   `report_full_name`. One file cannot tell it from a class and its member, such as `Status.Active`.
/// - an `operator ==` in a class that neither declares nor inherits a `public int hash()`, in `Operator`'s `analyze`,
///   because the parent may be another file's.
/// - an operator a parent already declares, in `check_inherited_operators`, because the parent may be another file's.
/// - `: super(…)` arguments that do not fit the base class's constructor, or any argument when no constructor exists
///   above the class, in `analyze_constructor_initializer`, because the base class may be another file's.
/// - a constructor without `: super(…)` in a class whose base class has a constructor, in `check_super_call`, which
///   the interim ruling on an implicit `super()` makes an error.
/// - a class without a constructor whose initial values run in the constructor PHP# gives it, under a base class
///   whose constructor takes parameters, in `check_super_call`, which names the constructor to declare.
#[inline]
pub fn check_slice(node: Node<'_, '_>, context: &mut Context<'_, '_, '_>) {
    let place = match context.slice_places.last() {
        None => enter(node, Place::File, context),
        Some(Some(place)) => enter(node, *place, context),
        Some(None) => None,
    };

    context.slice_places.push(place);
}

/// Where a node of a PHP# file sits, for the parts of the slice that depend on it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    /// The file's statements, its namespace and its imports.
    File,
    /// A class and its members.
    Class,
    /// An interface and its members.
    Interface,
    /// An interface method's parameters and return type.
    Signature,
    /// An enum and its members.
    Enum,
    /// The types of a class, interface, enum or event header, written after `:`.
    Header,
    /// The type parameters of a class, an interface or a method: each one's variance, name and bound.
    TypeParameter,
    /// An event, spec section 15: its modifiers, parameters and header.
    Event,
    /// An enum case that carries data, spec section 20: its fields.
    Case,
    /// A struct, spec section 10: its modifiers, backing type and header. Its members are a class's.
    Struct,
    /// A module and its members.
    Module,
    /// An extension block of a static class: its receiver and its members.
    Extension,
    /// A field or a property, both of which the CST calls a property: its modifiers, type and name.
    FieldOrProperty,
    /// A class constant: its modifiers, type and name.
    ClassConstant,
    /// A method's modifiers, parameters and return type.
    Method,
    /// One parameter.
    Parameter,
    /// A method body.
    Body,
    /// A lambda's parameters, whose body is the method body's.
    Lambda,
    /// `new` and the class it creates, whose arguments are the method body's.
    Instantiation,
    /// A call of a global function and the function's name, whose arguments are the method body's.
    FunctionCall,
    /// A catch clause, whose block is the method body's.
    TryCatchClause,
    /// `super.method(...)`, whose method and arguments are the method body's.
    SuperCall,
    /// `Self.method(...)`, whose method and arguments are the method body's.
    SelfCall,
    /// An expression of a method body reported over one of its parts, at this span: the walk skips that part and
    /// checks the rest as the method body, such as the value of a write whose target the slice refuses.
    RefusedPart(Span),
    /// An attribute list: its attributes' names and arguments.
    Attribute,
    /// A constant expression: an attribute argument, a class constant's value, an enum case's value, a pattern's value
    /// or a field's constant initial value.
    Constant,
    /// A pattern of `is` or of a `match` arm, whose values are constant expressions, as in C#.
    Pattern,
}

/// Decides one node at its place. Returns the place of its children when the slice has the node, and `None` when
/// the node is reported. A refusal skips the node's children only when its span covers them: a node refused at a
/// part, such as a method at its name or a cast at its operator, keeps the place it would have had, so the walk still
/// checks the rest.
fn enter(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    use Place::Attribute;
    use Place::Body;
    use Place::Class;
    use Place::ClassConstant;
    use Place::Constant;
    use Place::Enum;
    use Place::FieldOrProperty;
    use Place::File;
    use Place::FunctionCall;
    use Place::Header;
    use Place::Instantiation;
    use Place::Interface;
    use Place::Lambda;
    use Place::Method;
    use Place::Parameter;
    use Place::SelfCall;
    use Place::Signature;
    use Place::SuperCall;
    use Place::TryCatchClause;

    if let Place::RefusedPart(part) = place {
        return if node.span() == part { None } else { enter(node, Body, context) };
    }

    if let Some(target) = write_target(node) {
        // PHP# never has `$` variables, and the walk reports a write to one at the variable, and a superglobal at its
        // name.
        if !matches!(target, Expression::Variable(_))
            && !is_slice_target(target, context)
            && superglobal_span(target, context).is_none()
        {
            let targets = "PHP# writes to a local, a parameter or a member written `object.name`.";
            let skipped = |array: &Array| {
                array.elements.iter().find_map(|element| match element {
                    ArrayElement::Missing(missing) => Some(missing.span()),
                    _ => None,
                })
            };
            match target {
                Expression::List(_) => report_unexpected(target.span(), context),
                // A skipped element, as in `[, a]`, is no PHP# syntax anywhere, so it fails as a typo does at its `,`.
                Expression::Array(array) if let Some(missing) = skipped(array) => report_unexpected(missing, context),
                // A tuple unpacks into new locals, each element named, spec section 30.
                Expression::Array(_) => {
                    let written = BytesDisplay(context.get_code_snippet(target));
                    context.report(
                        Issue::error(format!(
                            "`{written}` can't be written to: unpack a tuple into new locals with `const {written} = …`."
                        ))
                        .with_annotation(Annotation::primary(target.span()).with_message("Written here."))
                        .with_note(targets),
                    );
                }
                _ => context.report(
                    Issue::error(format!("`{}` can't be written to.", BytesDisplay(context.get_code_snippet(target))))
                        .with_annotation(Annotation::primary(target.span()).with_message("Written here."))
                        .with_note(targets),
                ),
            }

            return Some(Place::RefusedPart(target.span()));
        }

        if let Expression::Access(Access::Property(property)) = target {
            check_member_access(property.object, &property.property, context);
        }
    }

    match (node, place) {
        (Node::Statement(statement), File) if !is_module_file_statement(statement, context) => {
            let path: &[u8] = &context.source_file.name;
            let file = BytesDisplay(path.rsplit(|&byte| byte == b'/').next().unwrap_or(path));
            context.report(
                Issue::error(format!("{file} holds only its folder's module."))
                    .with_annotation(Annotation::primary(statement.span()).with_message(format!("Declared in {file}.")))
                    .with_note(format!("A namespace's `{file}` holds its namespace, its imports and one `module {{ … }}`.")),
            );

            None
        }
        (Node::Statement(Statement::Namespace(namespace)), File) if !is_slice_namespace(namespace, context.program) => {
            let message = if namespace.name.is_none() {
                "A namespace has the name its folder gives it, as in `namespace App.Tenant;`."
            } else if !matches!(namespace.body, NamespaceBody::Implicit(_)) {
                "A namespace is written without braces, as in `namespace App.Tenant;`."
            } else {
                "A file has one namespace: move the code after this one to a file in its own namespace's folder."
            };
            context.report(
                Issue::error(message)
                    .with_annotation(Annotation::primary(namespace.r#namespace.span).with_message("Written here."))
                    .with_note("A file's namespace is the one its folder names, written once before its imports."),
            );

            Some(File)
        }
        (Node::Hint(hint), _) if let Some(refusal) = static_member_use(hint, context) => {
            report_static_member_use(hint.span(), refusal, context);

            None
        }
        // PHP's `declare` is no PHP# syntax, at file level or in a body.
        (Node::Statement(Statement::Declare(_)), _) => {
            report_unexpected(node.span(), context);

            None
        }
        // Spec section 27: a `.sharp` file declares one type, so code at file level is never valid.
        (Node::Statement(statement), File) if runs_code(statement) => {
            let help = if matches!(statement, Statement::Noop(_)) { "Delete it." } else { "Move it into a method." };
            context.report(
                Issue::error("A statement cannot sit at file level in PHP#.")
                    .with_annotation(Annotation::primary(statement.span()).with_message("Written at file level."))
                    .with_note("A `.sharp` file holds its namespace, its imports and one type, and code runs in that type's methods.")
                    .with_help(help),
            );

            None
        }
        // A dotted name is a namespace's, an import's, or a case type's, as in `PaymentResult.Paid`.
        (
            Node::Keyword(_)
            | Node::LocalIdentifier(_)
            | Node::Identifier(Identifier::Local(_) | Identifier::Dotted(_))
            | Node::DottedIdentifier(_),
            _,
        ) => Some(place),
        // A missing terminator ends the statement a leave runs, as in `a ?? return`, and is otherwise a parse error.
        (Node::Terminator(Terminator::Semicolon(_) | Terminator::Missing(_)) | Node::MissingTerminator(_), _) => {
            Some(place)
        }
        (Node::Extern(r#extern), File) if r#extern.event.is_some() => check_extern_event(r#extern, context),
        (Node::UseItem(item), File) if imports_error(item) => {
            context.report(
                Issue::error("`Error` is PHP#'s interface of every failure, so no import names it.")
                    .with_annotation(Annotation::primary(item.span()).with_message("Imported here."))
                    .with_note(
                        "A bare `Error` always names the standard library's interface. Catch a bug as `Throwable`, and rename another library's `Error` with `as`.",
                    ),
            );

            Some(File)
        }
        (
            Node::Program(_)
            | Node::Statement(
                Statement::Namespace(_)
                | Statement::Use(_)
                | Statement::Extern(_)
                | Statement::Module(_)
                | Statement::Class(_)
                | Statement::Interface(_)
                | Statement::Enum(_)
                | Statement::Event(_)
                | Statement::Struct(_),
            )
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
            // A braced namespace's body, after its namespace was refused.
            | Node::Block(_)
            | Node::Use(_)
            | Node::Extern(_)
            | Node::UseItems(UseItems::Sequence(_))
            | Node::UseItemSequence(_)
            | Node::UseItem(_)
            | Node::UseItemAlias(_),
            File,
        ) => Some(File),
        (Node::Class(class), File) => {
            report_methods_named_as_properties(ClassLike::of_class(class), context);
            if class.modifiers.contains_static() {
                check_static_class(class, context);
            }
            for member in &class.members {
                if let ClassLikeMember::NamedConstructor(constructor) = member {
                    check_named_constructor(constructor, &class.name, class.modifiers.contains_abstract(), context);
                }
            }

            Some(Class)
        }
        (Node::Struct(r#struct), File) => {
            report_methods_named_as_properties(ClassLike::of_struct(r#struct), context);
            check_struct_declaration(r#struct, context);
            for member in &r#struct.members {
                if let ClassLikeMember::NamedConstructor(constructor) = member {
                    check_named_constructor(constructor, &r#struct.name, false, context);
                }
            }

            Some(Place::Struct)
        }
        // `check_struct_declaration` reports every modifier but `public`. A struct's members are a class's.
        (Node::Modifier(modifier), Place::Struct) => matches!(modifier, Modifier::Public(_)).then_some(Place::Struct),
        (Node::AttributeList(list), Place::Struct) => check_attribute_target(list, context),
        (Node::ClassLikeMember(_), Place::Struct) => enter(node, Class, context),
        // The walk checks a class's, an interface's, an enum's and an event's name in `check_class_name`.
        (Node::Interface(_), File) => Some(Interface),
        (Node::Enum(r#enum), File) => {
            check_case_class_names(r#enum, context);
            // Spec section 20: a backed enum runs as a PHP enum, which carries no type arguments.
            if let (Some(list), Some(backing)) = (&r#enum.type_parameters, &r#enum.backing_type_hint) {
                context.report(
                    Issue::error("A backed enum takes no type parameters: it runs as a PHP enum, which holds none.")
                        .with_annotation(Annotation::primary(list.span()).with_message("Declared here."))
                        .with_annotation(Annotation::secondary(backing.span()).with_message("Backed here."))
                        .with_note("Remove the backing type to give the enum type parameters, as in `enum Option<T>`."),
                );
            }

            Some(Enum)
        }
        (Node::Event(_), File) => Some(Place::Event),
        (Node::Modifier(Modifier::Public(_) | Modifier::Internal(_)), Place::Event) => Some(Place::Event),
        (Node::Modifier(modifier), Place::Event) => {
            context.report(
                Issue::error("An event takes only `public` or `internal`.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier."))
                    .with_note("An event runs as a final class, and its parameters are its public get-only properties."),
            );

            None
        }
        (Node::FunctionLikeParameter(parameter), Place::Event) => {
            check_property_parameter(parameter, Place::Event, context)
        }
        (Node::Module(module), File) => check_module_file_name(module, context),
        (Node::ModuleMember(_), Place::Module) => Some(Place::Module),
        // The header names the module it overrides, which `check_module_file_name` checked.
        (Node::ModuleParent(_), Place::Module) => None,
        // A binding's parameters are checked as a method's, and its expression as a method's expression body. The
        // analyzer checks the expression as the lambda it runs as.
        (Node::Binding(_), Place::Module) => Some(Method),
        (Node::BindingValue(BindingValue::Expression(_)), Method) => Some(Body),
        (Node::BindingTarget(_) | Node::BindingValue(_) | Node::BindingClasses(_), Method) => Some(Method),
        (Node::FunctionLikeParameterList(parameters), Method | Signature | Place::Event | Place::Case) => {
            report_optional_before_required(parameters, context);

            Some(place)
        }
        (Node::Modifier(Modifier::Lazy(lazy)), Parameter) => {
            context.report(
                Issue::error(
                    "A parameter cannot be `lazy`: a lazy property computes its own value, and a parameter receives one.",
                )
                .with_annotation(Annotation::primary(lazy.span).with_message("Written here.")),
            );

            None
        }
        // `is_slice_property` keeps `lazy` to a computed property.
        (Node::Modifier(Modifier::Lazy(_)), FieldOrProperty) => Some(FieldOrProperty),
        (Node::Modifier(Modifier::Lazy(lazy)), _) => {
            context.report(
                Issue::error("Only a property is `lazy`, written as in `public lazy Plan plan => this.load();`.")
                    .with_annotation(Annotation::primary(lazy.span).with_message("Written here.")),
            );

            None
        }
        // `check_class` reports `protected` and `private` on a class, and PHP's own check `abstract` with `final`.
        // `check_static_class` reports `abstract`, `final` and `foreign` on a static class.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Abstract(_)
                | Modifier::Final(_)
                | Modifier::Static(_)
                | Modifier::Internal(_)
                | Modifier::Foreign(_),
            ),
            Class,
        ) => Some(Class),
        (Node::Inheritance(inheritance), Class | Interface | Enum | Place::Event | Place::Struct) => {
            check_header(inheritance, place, context);

            Some(Header)
        }
        // A header names each class and interface by its short name, and a generic one with type arguments, which are
        // a parameter's types.
        (Node::Hint(Hint::Identifier(name)), Header) if !context.names.is_type_parameter(name) => Some(Header),
        (Node::Hint(hint @ Hint::Generic(generic)), Header)
            if is_slice_type(hint) && built_in_generic_arity(generic.name.value).is_none() =>
        {
            Some(Parameter)
        }
        (Node::TypeParameterList(list), Class | Interface | Method | Signature | Place::Struct) => {
            check_type_parameters(list, place, context);

            Some(Place::TypeParameter)
        }
        (Node::TypeParameterList(list), Enum) => {
            check_type_parameters(list, place, context);

            Some(Place::TypeParameter)
        }
        (Node::TypeParameter(_), Place::TypeParameter) => Some(Place::TypeParameter),
        (Node::TypeParameterBound(bound), Place::TypeParameter) => {
            if is_slice_bound(&bound.hint, context) {
                return Some(Place::TypeParameter);
            }

            let message = if matches!(bound.hint, Hint::Union(_)) {
                "A bound can't be a union: join several bounds with `&`."
            } else if joins_type_parameter(&bound.hint, context) {
                "A type parameter is a bound alone, as in `<TSub : T>`: it joins no other bound with `&`."
            } else {
                "A bound is `Object`, a class, an interface or a type parameter, as in `<TItem : DatabaseEntity>`."
            };
            context.report(
                Issue::error(message)
                    .with_annotation(Annotation::primary(bound.hint.span()).with_message("Bound written here."))
                    .with_note(supported(Place::TypeParameter)),
            );

            None
        }
        // `is_slice_bound` decided the bound, and a generic class's type arguments in it are a parameter's types.
        (
            Node::Hint(Hint::Object(_) | Hint::Identifier(_) | Hint::Intersection(_)) | Node::IntersectionHint(_),
            Place::TypeParameter,
        ) => {
            Some(Place::TypeParameter)
        }
        (Node::Hint(Hint::Generic(_)), Place::TypeParameter) => Some(Parameter),
        (Node::Modifier(Modifier::Public(_) | Modifier::Internal(_)), Interface | Enum) => Some(place),
        // Spec section 13: a class keeps its values with get-only properties.
        (Node::Modifier(Modifier::Readonly(readonly)), Class | Interface | Enum) => {
            context.report(
                Issue::error("A class is never `readonly`: declare each of its properties get-only, with `{ get; }`.")
                    .with_annotation(Annotation::primary(readonly.span).with_message("Written here.")),
            );

            None
        }
        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Interface) => Some(Interface),
        // Spec section 22: a computed property's expression is its default body, as a method's body is, and runs on
        // each read of the object it is read on, so an interface property takes no `static` or `lazy`.
        (Node::ClassLikeMember(ClassLikeMember::Property(Property::Computed(property))), Interface) => {
            for modifier in property
                .modifiers
                .iter()
                .filter(|modifier| !modifier.is_visibility() && !matches!(modifier, Modifier::Override(_)))
            {
                context.report(
                    Issue::error("An interface property takes no modifier but `override`: its default body runs on each read.")
                        .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier.")),
                );
            }
            check_member_name(&property.modifiers, property.variable.name, property.variable.span, "Property", context);

            Some(FieldOrProperty)
        }
        // An interface states a property its classes give, as `OrderEvent` states `orderId`, spec section 15.
        (Node::ClassLikeMember(ClassLikeMember::Property(hooked @ Property::Hooked(property))), Interface)
            if is_interface_property(property) =>
        {
            let variable = hooked.first_variable();
            check_member_name(hooked.modifiers(), variable.name, variable.span, "Property", context);

            Some(FieldOrProperty)
        }
        // Spec section 22: an interface holds no storage, so a property it states takes no initial value, and its
        // default body is a computed property's expression.
        (Node::ClassLikeMember(ClassLikeMember::Property(Property::Hooked(property))), Interface)
            if let Some(initial_value) = &property.initial_value =>
        {
            let name = BytesDisplay(property.item.variable().name);
            context.report(
                Issue::error("An interface holds no storage, so it gives no initial value.")
                    .with_annotation(Annotation::primary(initial_value.span()).with_message("Written here."))
                    .with_help(format!(
                        "Remove it, and give `{name}` its value in each class that implements the interface."
                    )),
            );

            None
        }
        (Node::ClassLikeMember(ClassLikeMember::Property(Property::Hooked(property))), Interface)
            if let Some(body) = property
                .hook_list
                .hooks
                .iter()
                .find(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_))) =>
        {
            let hint = property.hint.as_ref().map(|hint| String::from_utf8_lossy(context.get_code_snippet(hint)));
            let declared = format!("{} {}", hint.unwrap_or_default(), BytesDisplay(property.item.variable().name));
            context.report(
                Issue::error(format!(
                    "An interface property's default body is an expression: write `{declared} => …;`."
                ))
                .with_annotation(Annotation::primary(body.span()).with_message("Written here.")),
            );

            None
        }
        // Spec section 22: every field is declared in the class that owns it.
        (Node::ClassLikeMember(ClassLikeMember::Property(field @ Property::Plain(_))), Interface) => {
            let interface = BytesDisplay(enclosing_class_like_name(context.program, field.span()).unwrap_or_default());
            let variable = field.first_variable().name;
            let name = BytesDisplay(variable.strip_prefix(b"$").unwrap_or(variable));
            context.report(
                Issue::error(format!("An interface holds no fields: the class that implements `{interface}` declares `{name}`."))
                    .with_annotation(Annotation::primary(field.span()).with_message("Declared in the interface.")),
            );

            None
        }
        // An interface member is public, written bare. A method's modifiers are in its signature, and an interface
        // property, which `is_interface_property` keeps whatever its access modifier, walks as a property.
        (
            Node::Modifier(
                modifier @ (Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Internal(_)),
            ),
            Signature | FieldOrProperty | ClassConstant,
        ) if is_in_interface(context.program, modifier.span()) => {
            report_interface_member_access(modifier, context);

            None
        }
        // Spec section 22: a sub-interface's body replaces its parent's default body with `override`.
        (Node::Modifier(Modifier::Override(_)), Signature) => None,
        (Node::Modifier(modifier), Signature) => {
            context.report(
                Issue::error("An interface method takes no modifier but `override`: every interface method is public.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier.")),
            );

            None
        }
        (Node::Method(method), Interface) => match is_slice_signature(method) {
            Ok(()) => {
                check_method_name(method, context);

                Some(Signature)
            }
            Err(issue) => report_refusal(*issue, method.span(), Signature, context),
        },
        // A body, a block or an expression, is the method's default body. The analyzer's `check_listener` refuses an
        // interface method that listens.
        (
            Node::FunctionLikeReturnTypeHint(_) | Node::On(_) | Node::MethodBody(_) | Node::MethodAbstractBody(_),
            Signature,
        ) => Some(Signature),
        (
            Node::AttributeList(list),
            Class | Interface | Enum | Place::Event | ClassConstant | Method | Signature | FieldOrProperty | Parameter
            | Lambda,
        ) => check_attribute_target(list, context),
        (
            Node::Attribute(_)
            | Node::PartialArgumentList(_)
            | Node::PartialArgument(PartialArgument::Positional(_) | PartialArgument::Named(_)),
            Attribute,
        ) => Some(Attribute),
        // PHP refuses a spread in attribute arguments, and `check_attribute_list` reports it, as `check_attribute_object`
        // does in a `new`'s arguments.
        (Node::PositionalArgument(argument), Attribute) => argument.ellipsis.is_none().then_some(Constant),
        (Node::NamedArgument(_), Attribute) => Some(Constant),
        (Node::Expression(Expression::Instantiation(instantiation)), Constant) if is_attribute_argument(context) => {
            check_attribute_object(instantiation, context)
        }
        (
            Node::Instantiation(_)
            | Node::Expression(Expression::Identifier(Identifier::Local(_)))
            | Node::ArgumentList(_)
            | Node::Argument(_),
            Attribute,
        ) => Some(Attribute),

        (Node::ClassLikeMember(ClassLikeMember::Method(_) | ClassLikeMember::Law(_)), Class | Enum) => Some(place),
        (Node::ClassLikeMember(ClassLikeMember::Extension(extension)), Class | Interface | Enum) => {
            if place == Class && enclosing_class(context.program, extension.span()).is_some_and(|class| class.is_static) {
                check_extension(extension, context);

                return Some(Class);
            }
            context.report(
                Issue::error("An extension belongs in a static class, as in `public static class StringExtensions`.")
                    .with_annotation(Annotation::primary(extension.extension.span).with_message("Declared here.")),
            );

            None
        }
        // The receiver is checked as a method's parameter, and a method or a computed property as a member of the
        // static class. `check_extension` reported every other member.
        (Node::Extension(_), Class) => Some(Place::Extension),
        (Node::TypeParameterList(list), Place::Extension) => {
            check_type_parameters(list, Place::Extension, context);

            Some(Place::TypeParameter)
        }
        (Node::FunctionLikeParameterList(_), Place::Extension) => Some(Method),
        (Node::ClassLikeMember(ClassLikeMember::Method(method)), Place::Extension)
            if let Some(readonly) = &method.readonly
                && let Some(extension) = enclosing_extension(context.program, method.span()) =>
        {
            let receiver = extension.parameter_list.parameters.first().map_or_else(String::new, |receiver| {
                String::from_utf8_lossy(context.get_code_snippet(receiver)).into_owned()
            });
            context.report(
                Issue::error(format!(
                    "An extension member has no `this`: write `readonly` before its receiver's type, as in `extension(readonly {receiver})`."
                ))
                .with_annotation(Annotation::primary(readonly.span).with_message("Written here.")),
            );

            enter(node, Class, context)
        }
        (
            Node::ClassLikeMember(ClassLikeMember::Method(_) | ClassLikeMember::Property(Property::Computed(_))),
            Place::Extension,
        ) => enter(node, Class, context),
        (Node::ClassLikeMember(_), Place::Extension) => None,
        // A law's parameters and body are checked as a method's. `check_members` refuses a default and a variadic one.
        (Node::Law(_), Class | Enum | Place::Module) => Some(Method),
        (Node::ClassLikeMember(ClassLikeMember::Operator(_) | ClassLikeMember::NamedConstructor(_)), Class) => {
            Some(Class)
        }
        (Node::ClassLikeMember(ClassLikeMember::Operator(_)), Interface | Enum) => Some(place),
        // A named constructor's parts are a method's, and `check_named_constructor` decides its class, name, modifiers
        // and `: this(…)` when the walk enters its class.
        (Node::NamedConstructor(_), Class) => Some(Method),
        (Node::ClassLikeMember(ClassLikeMember::NamedConstructor(constructor)), Interface | Enum) => {
            let message = if place == Interface {
                "An interface has no constructor: a named constructor creates an object of its own class."
            } else {
                "An enum has no constructor: its cases are its only values."
            };
            context.report(
                Issue::error(message)
                    .with_annotation(Annotation::primary(constructor.name.span).with_message("Declared here.")),
            );

            None
        }
        // An operator's parts are a method's, and `check_operator` decides its modifiers and its symbol, the one binary
        // operator a method's parts hold.
        (Node::Operator(operator), Class | Interface | Enum) => {
            check_operator(operator, context);
            // Spec section 13: the language runs an operator on readonly values, so its operands are readonly already.
            for parameter in &operator.parameter_list.parameters {
                if let Some(Hint::Readonly(readonly)) = &parameter.hint {
                    let written = format!(
                        "{} {}",
                        String::from_utf8_lossy(context.get_code_snippet(readonly.hint)),
                        BytesDisplay(parameter.variable.name)
                    );
                    context.report(
                        Issue::error(format!("An operator's operands are readonly by definition: write `{written}`."))
                            .with_annotation(Annotation::primary(readonly.readonly.span).with_message("Written here.")),
                    );
                }
            }

            Some(Method)
        }
        (Node::BinaryOperator(_), Method) => Some(Method),
        // Spec section 22: an enum's computed property is checked as a class's. `check_enum` reports any other property
        // in an enum, and a backing type other than `int` or `string`.
        (Node::ClassLikeMember(ClassLikeMember::Property(property @ Property::Computed(_))), Enum)
            if !property.modifiers().contains_static() =>
        {
            enter(node, Class, context)
        }
        (Node::ClassLikeMember(ClassLikeMember::Property(_)) | Node::EnumBackingTypeHint(_), Enum) => None,
        (Node::EnumCase(case), Enum) if case.item.name().value.eq_ignore_ascii_case(b"class") => {
            context.report(
                Issue::error("An enum case cannot be named `class`: PHP reserves `class` for the class name.")
                    .with_annotation(Annotation::primary(case.item.name().span).with_message("Declared here.")),
            );

            Some(Enum)
        }
        (Node::EnumCase(case), Enum) => {
            check_declared_name(case.item.name().value, case.item.name().span, "Enum case", NameCase::Pascal, context);

            Some(Enum)
        }
        (
            Node::ClassLikeMember(ClassLikeMember::EnumCase(_)) | Node::EnumCaseItem(_) | Node::EnumCaseUnitItem(_),
            Enum,
        ) => Some(Enum),
        // A case's data are its fields, which run as the public get-only properties of the case's class, as an
        // event's parameters do.
        (Node::EnumCaseDataItem(_), Enum) => Some(Place::Case),
        (Node::FunctionLikeParameter(parameter), Place::Case) => check_property_parameter(parameter, Place::Case, context),
        (Node::EnumCaseBackedItem(_), Enum) => Some(Constant),
        // Spec section 13: a field or a property keeps its value with `{ get; }`, and only a computed property's type,
        // the value each read gives, takes `readonly`.
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class)
            if !matches!(property, Property::Computed(_))
                && let Some(readonly) = storage_readonly(property.modifiers(), property.hint()) =>
        {
            let visibility = property.modifiers().get_first_visibility();
            report_readonly_storage(
                "A field or property",
                readonly,
                visibility,
                property.hint(),
                property.first_variable().name,
                context,
            );

            None
        }
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class) => {
            match is_slice_property(property) {
                Ok(()) => {
                    let variable = property.first_variable();
                    let kind = if matches!(property, Property::Plain(_)) { "Field" } else { "Property" };
                    check_member_name(property.modifiers(), variable.name, variable.span, kind, context);
                    report_static_without_initial_value(property, context);

                    Some(FieldOrProperty)
                }
                // A refused initial value is skipped with its property, because the walk would check it as code and
                // name other fixes, such as setting the member in the constructor.
                Err(issue) => report_refusal(
                    *issue,
                    property.initial_value().map_or(property.span(), |value| value.span()),
                    FieldOrProperty,
                    context,
                ),
            }
        }
        (Node::ClassLikeMember(ClassLikeMember::Constant(constant)), Class | Interface | Enum) => {
            match is_slice_constant(constant) {
                Ok(()) => {
                    let name = &constant.first_item().name;
                    // PHP reserves `X::class` for the class's name, so it loads no constant named `class` in any case.
                    if name.value.eq_ignore_ascii_case(b"class")
                        && let Some(class) = enclosing_class_like_name(context.program, constant.span())
                    {
                        context.report(
                            Issue::error(format!(
                                "`{}` can't name a constant: `{}.class` already names the class.",
                                BytesDisplay(name.value),
                                BytesDisplay(class)
                            ))
                            .with_annotation(Annotation::primary(name.span).with_message("Declared here.")),
                        );
                    } else {
                        check_declared_name(name.value, name.span, "Constant", NameCase::UpperSnake, context);
                    }

                    Some(ClassConstant)
                }
                Err(issue) => report_refusal(*issue, constant.span(), ClassConstant, context),
            }
        }
        (
            Node::ClassLikeConstant(_)
            | Node::Modifier(
                Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_),
            ),
            ClassConstant,
        ) => Some(ClassConstant),
        (Node::ClassLikeConstantItem(_), ClassConstant) => Some(Constant),
        (Node::HookedProperty(property), FieldOrProperty) => {
            let variable = property.item.variable();
            // `is_interface_property` took an interface property's accessors.
            if !is_in_interface(context.program, property.span()) {
                check_accessors(&property.modifiers, &property.hook_list, variable, context);
            }
            check_accessor_bodies(&property.hook_list, variable.name, context);
            if let Some(initial_value) = &property.initial_value
                && !context.names.has_storage(&property.hook_list)
            {
                report_no_storage(variable, initial_value.value.span(), context);
            }
            if let Some(behaviors) = &property.behaviors {
                check_behaviors(&property.modifiers, &property.hook_list, property.span(), behaviors, context);
            }

            Some(FieldOrProperty)
        }
        // `check_accessors` reported each accessor outside the slice. An accessor body is a method body.
        (Node::PropertyHookList(_), FieldOrProperty | Parameter) => Some(place),
        // `is_interface_property` took an interface property's accessors. An access modifier on one is an interface
        // member's, and the class gives the narrower `set`.
        (Node::PropertyHook(accessor), FieldOrProperty) if is_in_interface(context.program, accessor.span()) => {
            for modifier in &accessor.modifiers {
                report_interface_accessor_access(accessor, modifier, context);
            }

            accessor.modifiers.is_empty().then_some(place)
        }
        (Node::PropertyHook(accessor), FieldOrProperty | Parameter) => is_slice_accessor(accessor).then_some(place),
        (
            Node::PropertyHookBody(_) | Node::PropertyHookAbstractBody(_) | Node::PropertyHookConcreteBody(_),
            FieldOrProperty | Parameter,
        ) => Some(place),
        // A computed property's expression runs on each read, as a method body does.
        (Node::ComputedProperty(_), FieldOrProperty) => Some(FieldOrProperty),
        (Node::Block(_) | Node::PropertyHookConcreteExpressionBody(_), FieldOrProperty | Parameter) => Some(Body),
        // A constant initial value is the member's default, which PHP evaluates as a constant expression. One that
        // reads a static member, `Class.name`, runs as code, as `Property::initial_value_runs_as_code` decides.
        (Node::Expression(value), FieldOrProperty) => {
            let uses_this = report_this_in_initial_value(node, context);
            let place = if !uses_this
                && value.is_constant(&context.version, false)
                && !is_initial_value_run_as_code(value, context)
            {
                Constant
            } else {
                Body
            };

            enter(node, place, context)
        }
        (
            Node::Property(_)
            | Node::PlainProperty(_)
            | Node::PropertyItem(_)
            | Node::PropertyAbstractItem(_)
            | Node::PropertyConcreteItem(_)
            | Node::DirectVariable(_)
            // `is_slice_property` refuses `override`, `virtual` and `abstract` on a field, and `required` on a member
            // without `init` or `set`.
            | Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Internal(_)
                | Modifier::Static(_)
                | Modifier::Abstract(_)
                | Modifier::Virtual(_)
                | Modifier::Override(_)
                | Modifier::Required(_),
            ),
            FieldOrProperty,
        ) => Some(FieldOrProperty),
        (Node::Hint(hint), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body | Place::Pattern)
            if let Some(issue) = type_written_another_way(hint, place, context) =>
        {
            context.report(issue);

            None
        }
        // Spec section 13: `readonly` before a type a value has. The analyzer refuses it on a type whose class PHP# does
        // not check, or that never changes, an enum's or a struct's.
        (Node::Hint(Hint::Readonly(_)), FieldOrProperty | Method | Signature | Parameter | Body) => Some(place),
        (Node::ReadonlyHint(readonly), FieldOrProperty | Method | Signature | Parameter | Body) => {
            check_readonly_type(readonly, context).then_some(place)
        }
        (Node::Hint(Hint::Readonly(readonly)), Place::Pattern) => {
            let written = String::from_utf8_lossy(context.get_code_snippet(readonly.hint)).into_owned();
            context.report(
                Issue::error(format!("`is` keeps a value readonly: write `is {written}`."))
                    .with_annotation(Annotation::primary(readonly.readonly.span).with_message("Written here.")),
            );

            None
        }
        // Spec section 2: a constant takes any type, and its value, which is known before the code runs, decides what
        // it holds, as a collection literal or `typeof(Report)`.
        (Node::GenericHint(generic), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_generic(generic, place, context)
        }
        // A generic type's type arguments, and those of `new` and of a method call, are never `void`. Nor are they
        // `never` in a return type, which is a type only as the whole return type, spec section 7, and which
        // `type_written_another_way` refuses everywhere else.
        (
            Node::TypeArgumentList(list),
            FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body | Instantiation,
        ) => {
            if let Some(void) = list.arguments.iter().find(|argument| matches!(argument, Hint::Void(_))) {
                context.report(
                    Issue::error("A type argument cannot be `void`: `void` is only a return type.")
                        .with_annotation(Annotation::primary(void.span()).with_message("Written here.")),
                );

                return None;
            }
            if matches!(place, Method | Signature) {
                let refusals: Vec<Issue> = list
                    .arguments
                    .iter()
                    .filter_map(|argument| match argument {
                        Hint::Never(_) => type_written_another_way(argument, Parameter, context),
                        _ => None,
                    })
                    .collect();
                for issue in refusals {
                    context.report(issue);
                }
            }

            Some(if place == Instantiation { Body } else { place })
        }
        // A function type's parts are checked as a parameter's type is, so its return type may be `void`, which
        // `is_slice_type` refuses for its parameters, and `Self` in it is an error, because `Self` is a method's return
        // type only.
        (Node::FunctionHint(function), FieldOrProperty | ClassConstant | Method | Parameter | Body) => {
            for void in function.parameters.iter().filter(|parameter| matches!(parameter, Hint::Void(_))) {
                context.report(
                    Issue::error(
                        "A function type takes no `void` parameter: `void` is only a return type, and `Function<int()>` takes nothing.",
                    )
                    .with_annotation(Annotation::primary(void.span()).with_message("Written here.")),
                );
            }

            Some(Parameter)
        }
        // A tuple's element types are checked as a parameter's type is, so `Self` in one is an error.
        (Node::TupleHint(tuple), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_tuple(tuple, context)
        }
        (Node::TupleElementHint(_), Parameter) => Some(Parameter),
        // The effects an `extern`, a function type or a method without a body lists, spec section 29, which the
        // analyzer's `check_uses` decides, because an effect is a `foreign` class any file may declare.
        (Node::Uses(_), File | Parameter | Method | Signature) => Some(place),
        // Spec section 13: `readonly` after the parameters keeps `this` readonly, so a method with no `this` to keep, and
        // one the language already keeps readonly, takes none.
        (Node::Method(method), Class | Enum)
            if let Some(readonly) = &method.readonly
                && let Some(message) = needless_readonly_marker(method, place) =>
        {
            context.report(
                Issue::error(message).with_annotation(Annotation::primary(readonly.span).with_message("Written here.")),
            );

            Some(Method)
        }
        (Node::Method(method), Enum)
            if method.return_type_hint.is_none()
                && enclosing_class(context.program, method.span())
                    .is_some_and(|r#enum| r#enum.name.value.eq_ignore_ascii_case(method.name.value)) =>
        {
            report_refusal(
                Issue::error("An enum has no constructor: its cases are its only values.")
                    .with_annotation(
                        Annotation::primary(method.name.span).with_message("Declared without a return type here."),
                    )
                    .with_help("Give the method a return type, as in `public string label()`."),
                method.span(),
                Method,
                context,
            )
        }
        (Node::Method(method), Class | Enum) => match is_slice_method(method, context.program) {
            Ok(()) => {
                check_method_name(method, context);
                check_method_uses(method, context);
                check_abstract_access(method, context);
                check_iterator_returns(method, context);
                check_method_modifiers(method, context);
                check_extern(method, context)
            }
            Err(issue) => report_refusal(*issue, method.span(), Method, context),
        },

        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Internal(_)
                | Modifier::Static(_)
                | Modifier::Abstract(_)
                | Modifier::Virtual(_)
                | Modifier::Override(_)
                // `check_method_modifiers` decides where `final` and `virtual` can matter.
                | Modifier::Final(_)
                // The parser keeps `required` on the constructor only.
                | Modifier::Required(_)
                // `check_extern` decides where an `extern` method goes.
                | Modifier::Extern(_),
            )
            | Node::FunctionLikeReturnTypeHint(_)
            // The analyzer's `check_listener` decides which methods listen.
            | Node::On(_)
            | Node::MethodBody(_)
            // PHP's own check reports a method without a body that is not `abstract`, and one with a body that is.
            | Node::MethodAbstractBody(_),
            Method,
        ) => Some(Method),
        (Node::ConstructorInitializer(initializer), Method)
            if initializer.is_super() == is_named_constructor_initializer(initializer, context.program) =>
        {
            let message = if initializer.is_super() {
                "A named constructor passes control to the main constructor with `: this(…)`."
            } else {
                "The main constructor passes control to its base class with `: super(…)`. Only a named constructor calls `: this(…)`."
            };
            context.report(
                Issue::error(message)
                    .with_annotation(Annotation::primary(initializer.keyword.span).with_message("Written here.")),
            );

            None
        }
        // The arguments of `: super(…)` and `: this(…)` are a method body's expressions, without `this`.
        (Node::ConstructorInitializer(initializer), Method) => {
            report_this_in_initializer(initializer, context);

            Some(Body)
        }
        // Spec section 13: a parameter that declares a member keeps its value with `{ get; }`.
        (Node::FunctionLikeParameter(parameter), Method)
            if parameter.modifiers.contains_visibility()
                && let Some(readonly) = storage_readonly(&parameter.modifiers, parameter.hint.as_ref()) =>
        {
            let visibility = parameter.modifiers.get_first_visibility();
            report_readonly_storage(
                "A constructor parameter that declares a member",
                readonly,
                visibility,
                parameter.hint.as_ref(),
                parameter.variable.name,
                context,
            );

            None
        }
        // Outside a constructor, PHP's own check reports a parameter with an access modifier.
        (Node::FunctionLikeParameter(parameter), Method)
            if parameter.hooks.is_none()
                && let Some(access) = parameter
                    .modifiers
                    .iter()
                    .find(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Internal(_)))
                && is_constructor_parameter(parameter, context.program) =>
        {
            let level = BytesDisplay(access.get_keyword().value);

            report_refusal(
                Issue::error(format!(
                    "A constructor parameter declared `{level}` needs accessors: it is a property, as in `{level} int id {{ get; }}`."
                ))
                .with_annotation(Annotation::primary(access.span()).with_message(format!("Declared `{level}` here.")))
                .with_note("A field is `private` or `protected`, so a parameter without accessors is one."),
                parameter.span(),
                Parameter,
                context,
            )
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda)
            if let Some(ampersand) = parameter.ampersand =>
        {
            report_refusal(unexpected(ampersand, context), parameter.span(), Parameter, context)
        }
        // Spec section 9: a member declared on a constructor parameter is a property like one in the class body, with
        // storage the argument goes into.
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda) => {
            if let Some(accessors) = &parameter.hooks
                && parameter.modifiers.contains_visibility()
            {
                check_accessors(&parameter.modifiers, accessors, &parameter.variable, context);
                check_accessor_bodies(accessors, parameter.variable.name, context);
                if !context.names.has_storage(accessors) {
                    report_no_storage(&parameter.variable, parameter.variable.span, context);
                }
                if let Some(behaviors) = &parameter.behaviors {
                    check_behaviors(&parameter.modifiers, accessors, parameter.span(), behaviors, context);
                }
            }

            Some(Parameter)
        }
        (Node::Hint(Hint::Void(_)), Body) => {
            context.report(
                Issue::error("A local cannot be `void`: `void` is only a return type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Declared `void` here.")),
            );

            None
        }
        (
            Node::Hint(Hint::Self_(keyword)),
            FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body | Place::Pattern,
        ) => check_self_type(keyword, place, context).then_some(place),
        (
            Node::Hint(Hint::Static(keyword)),
            FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body | Place::Pattern,
        ) => {
            report_unexpected(keyword.span, context);

            None
        }
        // `check_hint` reports a type in parentheses that is not a union.
        (
            Node::Hint(Hint::Nullable(NullableHint { hint: Hint::Parenthesized(parenthesized), .. })),
            FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body,
        ) => {
            if let Hint::Union(union) = parenthesized.hint {
                check_union(union, place, context);
            }

            None
        }
        (Node::Hint(hint), FieldOrProperty | Method | Signature | Parameter | Body) if is_slice_type(hint) => {
            Some(place)
        }
        // Spec section 9: every call passes a parameter without a default, a constructor's or a method's, so `required`
        // adds nothing. It declares no member either, so it is this one error.
        (Node::Modifier(modifier @ Modifier::Required(_)), Parameter) => {
            context.report(
                Issue::error("A parameter is required unless it has a default: remove `required`.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Written here.")),
            );

            None
        }
        // Spec section 7: `never` is a method's return type, which `type_written_another_way` refuses elsewhere, and
        // PHP's own check reports `never?` and `never` in a union.
        (Node::Hint(Hint::Never(_)), Method | Signature) => Some(place),
        (Node::Hint(Hint::Union(union)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_union(union, place, context);

            None
        }
        (Node::Hint(hint), ClassConstant) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => {
            Some(ClassConstant)
        }
        (Node::NullableHint(_), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            Some(place)
        }
        (Node::DirectVariable(_), Parameter) => Some(Parameter),
        // An access modifier declares a member, and `check_accessors` checked its accessors. PHP reports `static`,
        // `final` and `abstract` on a parameter, a member declared outside the constructor, and accessors without one.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Internal(_)
                | Modifier::Static(_)
                | Modifier::Final(_)
                | Modifier::Abstract(_),
            ),
            Parameter,
        ) => Some(Parameter),
        // Spec section 27: as a default, `Position.current()` is the caller's position, which a PHP# call passes.
        (Node::FunctionLikeParameterDefaultValue(default), Parameter)
            if let Expression::Call(Call::Method(call)) = default.value
                && context.names.is_current_position(call) =>
        {
            None
        }
        (Node::FunctionLikeParameterDefaultValue(default), Parameter)
            if !Node::Expression(default.value)
                .filter_map(|node| match node {
                    Node::MethodCall(call) if context.names.is_current_position(call) => Some(()),
                    _ => None,
                })
                .is_empty() =>
        {
            context.report(
                Issue::error("Take the whole position: `Position caller = Position.current()`.")
                    .with_annotation(
                        Annotation::primary(default.value.span()).with_message("Computed from the position."),
                    )
                    .with_note(
                        "As a default, `Position.current()` gives the caller's position, and the parameter takes all of it, as one value.",
                    ),
            );

            None
        }
        // Spec section 9: a parameter's default is any expression, which runs each time a call leaves the argument out.
        (Node::FunctionLikeParameterDefaultValue(default), Parameter) => {
            check_default_reads(default.value, context);

            Some(Body)
        }
        // An interface method's body is its default body, spec section 22, checked as a class method's body.
        (Node::Block(_), Method | Signature | Body) => Some(Body),
        (Node::MethodExpressionBody(_), Method | Signature) => Some(Body),
        (Node::TryCatchClause(_), Body) => Some(TryCatchClause),
        // A generic class's type arguments in a catch clause are a parameter's types, alone or in a union.
        (Node::Hint(Hint::Generic(_)), TryCatchClause) => Some(Parameter),
        (Node::Hint(Hint::Union(_)) | Node::UnionHint(_), TryCatchClause) => Some(TryCatchClause),
        // `check_try` reports a catch type that is not a class, and `check_try_catch_clause` the variable's name.
        (Node::Hint(_) | Node::DirectVariable(_), TryCatchClause) => None,
        (Node::Block(_), TryCatchClause) => Some(Body),

        // The block is checked as a body, and no jump leaves it.
        (Node::TryFinallyClause(finally), Body) => {
            report_jumps_out_of_finally(finally, context);

            Some(Body)
        }
        (Node::Statement(statement), Body) if let Some((body, fix)) = body_without_braces(statement, context) => {
            context.report(
                Issue::error("PHP# writes the body of `if`, `else` and each loop in braces.")
                    .with_annotation(Annotation::primary(body).with_message("Written without braces."))
                    .with_help(format!("Write `{fix}`.")),
            );

            None
        }
        // `??` runs its right side only for null, so `value ?? throw …` is a statement with an effect, and so is an
        // assignment PHP's `and`, `or` and `xor` bind looser than, as in `a = b or c`.
        (Node::ExpressionStatement(statement), Body)
            if let Expression::Binary(binary) = statement.expression
                && !binary.operator.is_null_coalesce()
                && ![binary.lhs, binary.rhs].iter().any(|operand| matches!(operand, Expression::Assignment(_))) =>
        {
            report_no_effect(binary, context);

            None
        }
        (
            Node::Statement(
                Statement::Block(_)
                | Statement::Expression(_)
                | Statement::Return(_)
                | Statement::Emit(_)
                | Statement::LocalDeclaration(_)
                | Statement::If(_)
                | Statement::While(_)
                | Statement::DoWhile(_)
                | Statement::For(_)
                | Statement::ForOf(_)
                | Statement::Try(_)
                | Statement::Break(Break { level: None, .. })
                | Statement::Continue(Continue { level: None, .. }),
            )
            | Node::Try(_)
            | Node::Break(_)
            | Node::Continue(_)
            | Node::ExpressionStatement(_)
            | Node::Return(_)
            | Node::Emit(_)
            | Node::LocalDeclaration(_)
            | Node::LocalTarget(_)
            | Node::LocalUnpack(_)
            | Node::If(_)
            | Node::IfBody(IfBody::Statement(_))
            | Node::IfStatementBody(_)
            | Node::IfStatementBodyElseClause(_)
            | Node::While(_)
            | Node::WhileBody(WhileBody::Statement(_))
            | Node::DoWhile(_)
            | Node::For(_)
            | Node::ForBody(ForBody::Statement(_))
            | Node::ForOf(_)
            | Node::ForOfTarget(_)
            | Node::ForOfUnpackTarget(_)
            | Node::ForOfVariable(_),
            Body,
        ) => Some(Body),
        // Spec section 8 makes output a function call, so PHP's `echo` and `print` are no PHP# syntax. `exit` is PHP's
        // function `exit`, which the analyzer refuses bare as it refuses any bare call of a plain PHP function, spec
        // section 23.
        (Node::Statement(Statement::Echo(_)) | Node::Expression(Expression::Construct(Construct::Print(_))), Body) => {
            report_unexpected(node.span(), context);

            None
        }
        (
            Node::Expression(Expression::Construct(Construct::Exit(_)))
            | Node::Construct(Construct::Exit(_))
            | Node::ExitConstruct(_),
            Body,
        ) => Some(Body),

        (
            Node::Expression(expression @ (Expression::Binary(_) | Expression::Is(_) | Expression::As(_))),
            Body | Constant,
        ) if let Some(groupings) = comparison_chain(expression, context) => {
            report_comparison_chain(expression.span(), &groupings, context);

            None
        }
        (Node::Statement(Statement::PatternMatch(pattern_match)), Body) => {
            check_pattern_match(pattern_match, false, context).then_some(Body)
        }
        // A `match` on constants with constant arms is known before the code runs.
        (Node::Expression(Expression::PatternMatch(pattern_match)), Body | Constant) => {
            check_pattern_match(pattern_match, true, context).then_some(place)
        }
        (Node::Is(is), Body)
            if let Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) = is.value =>
        {
            let operand = BytesDisplay(context.get_code_snippet(operand));
            let pattern = BytesDisplay(context.get_code_snippet(is.pattern));
            context.report(
                Issue::error(format!(
                    "Write `{operand} is not {pattern}`: `!` applies to `{operand}` before `is` tests it."
                ))
                .with_annotation(Annotation::primary(is.span()).with_message("`!` takes the value before `is` tests it."))
                .with_note("`is` binds with the comparisons, so `!entity is HasDesign` reads as `(!entity) is HasDesign`, and a negative test is written `is not`."),
            );

            None
        }
        (Node::Is(is), Body) if report_chained_pattern(is.span(), is.pattern, context) => None,
        (Node::PatternMatchPatternArm(arm), Body | Constant)
            if report_chained_pattern(arm.pattern.span(), arm.pattern, context) =>
        {
            None
        }
        (Node::Is(is), Body) => {
            // `is not T name` declares `name` where the test is false, so the outermost `not` hides no variable.
            let tested = if let Pattern::Not(not) = is.pattern { not.pattern } else { is.pattern };
            report_hidden_pattern_variables(tested, false, context);

            Some(Body)
        }
        (Node::PatternMatchPatternArm(arm), Body | Constant) => {
            report_hidden_pattern_variables(arm.pattern, false, context);

            Some(place)
        }
        (Node::As(r#as), Body) => match r#as.hint {
            // Spec section 13: a conversion of a readonly value is readonly already, as a narrowing pattern is.
            Hint::Readonly(readonly) => {
                let written = String::from_utf8_lossy(context.get_code_snippet(readonly.hint)).into_owned();
                context.report(
                    Issue::error(format!("`as` keeps a value readonly: write `as {written}`."))
                        .with_annotation(Annotation::primary(readonly.readonly.span).with_message("Written here.")),
                );

                None
            }
            // Spec section 14.4: a `?` that cannot matter is an error, and the help names the type without it. A union
            // with `null` is a nullable type written another way, as `int|null` is `int?`.
            Hint::Nullable(_) | Hint::Void(_) => {
                let without = match r#as.hint {
                    Hint::Nullable(nullable) => {
                        Some(BytesDisplay(context.get_code_snippet(nullable.hint.span())).to_string())
                    }
                    _ => None,
                };
                report_nullable_as(r#as.hint, without, context);

                None
            }
            Hint::Union(union) if let Some(without) = written_without_null(union, context) => {
                report_nullable_as(r#as.hint, Some(without), context);

                None
            }
            // Every type in the slice has a test. The walk refuses a type outside it, and `Self`, as anywhere in a
            // body, and in a union too, as `check_union` checks each member.
            _ => Some(Body),
        },
        (
            Node::Expression(Expression::Is(_) | Expression::As(_))
            | Node::PatternMatch(_)
            | Node::PatternMatchArm(_)
            | Node::PatternMatchDefaultArm(_)
            | Node::MatchGuard(_)
            | Node::PatternMatchArmBody(_),
            Body,
        ) => Some(Body),
        (
            Node::PatternMatch(_)
            | Node::PatternMatchArm(_)
            | Node::PatternMatchDefaultArm(_)
            | Node::MatchGuard(_)
            | Node::PatternMatchArmBody(_),
            Constant,
        ) => Some(Constant),
        // Spec section 21: a pattern takes types and constants only, and a local's value is compared in `when`, which
        // the analyzer's error writes with the value's type. The binder binds a bare name of a type pattern to the
        // local of that name. The value is no constant expression, so the walk stops here.
        (
            Node::Pattern(Pattern::Type(TypePattern {
                hint: Hint::Identifier(name),
                fields: None,
                properties: None,
                variable: None,
            })),
            Body | Constant | Place::Pattern,
        ) if reads_code(Node::Identifier(name), context) => None,
        (Node::Pattern(Pattern::Value(value)), Body | Constant | Place::Pattern)
            if reads_code(Node::Expression(value), context) =>
        {
            None
        }
        (Node::ComparisonPattern(comparison), Place::Pattern) if reads_code(Node::Expression(comparison.value), context) => {
            None
        }
        (Node::Pattern(_), Body | Constant | Place::Pattern) => Some(Place::Pattern),
        // Spec section 21: a pattern creates a variable only through a typed declaration. Its properties are still
        // checked.
        (Node::PropertiesPattern(PropertiesPattern { variable: Some(variable), .. }), Place::Pattern) => {
            context.report(
                Issue::error(
                    "A pattern creates a variable only through a typed declaration: write the type, as in `Response { status: 200 } r`.",
                )
                .with_annotation(Annotation::primary(variable.span).with_message("Declared without a type.")),
            );

            Some(Place::Pattern)
        }
        // A union with `null` is a nullable type written another way, as `int|null` is `int?`.
        (Node::Hint(hint @ (Hint::Nullable(_) | Hint::Union(_))), Place::Pattern)
            if !matches!(hint, Hint::Union(union) if written_without_null(union, context).is_none()) =>
        {
            context.report(
                Issue::error("A type pattern is never nullable: null never matches a type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                    .with_help("Test for null with `x == null`, or join both with `or`, as in `x is int or null`."),
            );

            None
        }
        (Node::Hint(Hint::Void(_)), Place::Pattern) => {
            context.report(
                Issue::error("A pattern's type is never `void`: `void` is only a return type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here.")),
            );

            None
        }
        // A generic class's type arguments and a function type's parameter and result types are types, as in a body.
        (Node::Hint(hint), Place::Pattern) if is_slice_type(hint) && has_type_test(hint) => Some(Body),
        // A union's members are types, which `check_union` checks as in a body.
        (Node::Hint(hint @ Hint::Union(union)), Place::Pattern) if has_type_test(hint) => {
            check_union(union, Body, context);

            None
        }
        (Node::BinaryPattern(binary), Place::Pattern)
            if !binary.is_and() && [binary.left, binary.right].iter().any(|side| matches!(side, Pattern::Not(_))) =>
        {
            report_not_beside_or(binary, context);

            None
        }
        // Spec section 21: a list pattern has one rest, anywhere among its elements.
        (Node::ListPattern(list), Place::Pattern) => {
            let rests = list.elements.iter().filter_map(|element| match element {
                ListPatternElement::Rest(rest) => Some(rest),
                ListPatternElement::Pattern(_) => None,
            });
            for rest in rests.skip(1) {
                context.report(
                    Issue::error("A list pattern has one rest: the elements its other patterns leave.")
                        .with_annotation(Annotation::primary(rest.span()).with_message("A second rest.")),
                );
            }

            Some(Place::Pattern)
        }
        // A named rest is a `List` of the elements the others leave, whose type the walk checks as a method body's.
        (Node::RestPattern(rest), Place::Pattern) => {
            let (Some(hint), Some(variable)) = (rest.hint, rest.variable) else {
                return None;
            };
            if !matches!(hint, Hint::Generic(generic) if generic.name.value == b"List") {
                context.report(
                    Issue::error("A rest is a `List` of the elements the others leave, as in `...List<Line> rest`.")
                        .with_annotation(Annotation::primary(hint.span()).with_message("Written here.")),
                );

                return None;
            }
            check_local_name(variable.value, variable.span, "pattern variable", context);

            Some(Body)
        }
        (
            Node::TypePattern(_)
            | Node::ComparisonPattern(_)
            | Node::NotPattern(_)
            | Node::BinaryPattern(_)
            | Node::ParenthesizedPattern(_)
            | Node::FieldsPattern(_)
            | Node::PropertiesPattern(_)
            | Node::PropertyPattern(_)
            | Node::ListPatternElement(_)
            | Node::BinaryOperator(_),
            Place::Pattern,
        ) => Some(Place::Pattern),
        // A value or a comparison's value is a constant value, as in C#.
        (Node::Expression(_), Place::Pattern) => enter(node, Constant, context),

        // PHP refuses the file at compile time, so the engine would too.
        (Node::LiteralString(string), Body | Constant) if string.value.is_none() => {
            context.report(
                Issue::error("Invalid UTF-8 codepoint escape sequence.")
                    .with_annotation(Annotation::primary(string.span).with_message("Escape written here."))
                    .with_note("A `\\u{...}` escape holds hex digits for a codepoint up to `10FFFF`."),
            );

            None
        }
        // Spec sections 27 and 29 remove PHP's superglobals and its `__Something__` names, written bare as well. A
        // method body walks an index of a superglobal and reports the superglobal at its name. A constant expression
        // has no index, so it reports the superglobal over the whole index.
        (Node::Expression(expression @ (Expression::ConstantAccess(_) | Expression::Variable(_))), Body | Constant)
        | (Node::Expression(expression @ Expression::ArrayAccess(_)), Constant)
            if let Some(span) = superglobal_span(expression, context) =>
        {
            report_unexpected(span, context);

            None
        }
        (Node::Expression(expression @ Expression::ConstantAccess(_)), Body | Constant)
            if let Some(name) = magic_name(expression, context) =>
        {
            report_unexpected(name.span, context);

            None
        }
        (Node::Expression(Expression::MagicConstant(constant)), Body | Constant) => {
            report_unexpected(constant.value().span, context);

            None
        }
        // A constant's value writes nothing, so `++` and `--` run code there, as any other form outside it does.
        (
            Node::Expression(
                expression @ Expression::UnaryPrefix(UnaryPrefix {
                    operator: UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_),
                    ..
                }),
            ),
            Constant,
        ) => {
            report_call_in_constant(expression.span(), context);

            None
        }
        (Node::UnaryPrefix(unary_prefix), Body | Constant) if unary_prefix.operator.is_cast() => {
            check_cast(unary_prefix, context);

            Some(place)
        }
        // Spec section 21: `is` tests a value against a type, and PHP's `instanceof` is no PHP# syntax. The value is
        // still checked, and the class is the test's.
        (Node::Binary(binary), Body | Constant) if matches!(binary.operator, BinaryOperator::Instanceof(_)) => {
            report_unexpected(binary.operator.span(), context);
            enter_tree(Node::Expression(binary.lhs), place, context);

            None
        }
        (
            Node::Expression(
                Expression::Literal(_)
                | Expression::ConstantAccess(_)
                | Expression::Parenthesized(_)
                | Expression::Binary(_)
                | Expression::UnaryPrefix(_),
            )
            | Node::Literal(_)
            | Node::LiteralInteger(_)
            | Node::LiteralFloat(_)
            | Node::LiteralString(_)
            | Node::Parenthesized(_)
            | Node::Binary(_)
            | Node::UnaryPrefix(_),
            Body | Constant,
        ) => Some(place),
        (
            Node::Expression(Expression::Array(_))
            | Node::Array(_)
            | Node::ArrayElement(ArrayElement::Value(_) | ArrayElement::KeyValue(_) | ArrayElement::Variadic(_))
            | Node::ValueArrayElement(_)
            | Node::KeyValueArrayElement(_)
            | Node::VariadicArrayElement(_),
            Body | Constant,
        ) => Some(place),
        (Node::Expression(Expression::ArrayAccess(_)) | Node::ArrayAccess(_), Body | Constant) => Some(place),
        // `is_slice_target` passes PHP's `x[] = v` to the analyzer, which refuses it, and refuses `x[]` read as a value.
        (Node::Expression(Expression::ArrayAppend(_)) | Node::ArrayAppend(_), Body) => Some(Body),
        (Node::ConstantAccess(_), Body) => Some(Body),
        // `typeof(Self)` is the class a static method is called on, as `Self` is in `new Self(…)`.
        (Node::TypeOf(TypeOf { r#type: Hint::Self_(keyword), .. }), Body) if is_sharp_self(keyword) => None,
        (Node::TypeOf(TypeOf { r#type: Hint::Identifier(name), .. }), Constant)
            if context.names.is_type_parameter(name) =>
        {
            report_type_argument_in_constant(node.span(), context);

            None
        }
        // `typeof(X)` of a class is `X::class`, which PHP takes as a constant expression too. Its type is checked as a
        // local's type is.
        (Node::Expression(Expression::TypeOf(_)), Body | Constant) => Some(place),
        (Node::TypeOf(_), Body | Constant) => Some(Body),
        // `nameof(X.y)` is the string `'y'`, a constant expression too.
        (Node::Expression(Expression::NameOf(_)) | Node::NameOf(_), Body | Constant) => Some(place),
        // The parser builds a class with type arguments only before `.`, as `Counter<Order>.count`. Only a class takes
        // them, so a type parameter, a local or a constant written so is refused, as C# refuses it (CS0307).
        (Node::GenericClass(generic_class), Body)
            if let Expression::ConstantAccess(class) = generic_class.class
                && context.names.binding(&class.name) != Some(Binding::Class) =>
        {
            let name = BytesDisplay(class.name.value());
            let message = if context.names.is_type_parameter(&class.name) {
                format!("A type parameter takes no type arguments: write `{name}`.")
            } else {
                format!("`{name}` is not a class, so it takes no type arguments.")
            };
            context.report(
                Issue::error(message)
                    .with_annotation(Annotation::primary(generic_class.type_arguments.span()).with_message("Written here.")),
            );

            None
        }
        (Node::Expression(Expression::GenericClass(_)) | Node::GenericClass(_), Body) => Some(Body),
        (Node::ConstantAccess(constant), Constant)
            if context.names.binding(&constant.name) == Some(Binding::Constant) =>
        {
            Some(Constant)
        }
        // A `match` in a constant value binds its pattern's variables from constants, so they are constants too.
        (Node::ConstantAccess(constant), Constant)
            if let Some(Binding::Local(Local { declaration, .. })) = context.names.binding(&constant.name)
                && context.ancestors.iter().any(|ancestor| {
                    matches!(ancestor, Node::Expression(Expression::PatternMatch(pattern_match))
                        if pattern_match.span().contains(&declaration.start))
                }) =>
        {
            Some(Constant)
        }
        // The object decides a member of PHP's `self` or `static`, and of a superglobal or a `__Something__` name,
        // which binds as a class before `.`.
        (Node::Expression(Expression::Access(Access::Property(access))), Constant)
            if matches!(access.object, Expression::Self_(_) | Expression::Static(_))
                || superglobal_span(access.object, context).is_some()
                || magic_name(access.object, context).is_some() =>
        {
            enter(Node::Expression(access.object), Constant, context)
        }
        // The engine reads `Class.name` in a constant expression as the class constant or enum case, since PHP
        // cannot read a static property there. The analyzer reports a static property of a class this file does
        // not declare.
        (Node::Expression(Expression::Access(Access::Property(access))), Constant)
            if let Some(class) = context.names.static_property_class(access)
                && context.names.is_type_parameter(&class.name) =>
        {
            report_type_argument_in_constant(access.span(), context);

            None
        }
        (Node::Expression(Expression::Access(Access::Property(access))), Constant)
            if let Some(class) = context.names.static_property_class(access)
                && let ClassLikeMemberSelector::Identifier(member) = &access.property =>
        {
            report_static_member_in_constant(class, member, access.span(), context);

            None
        }
        (Node::Expression(Expression::Conditional(_)), Body | Constant) => Some(place),
        (Node::Conditional(conditional), Body | Constant) => {
            check_conditional(conditional, context);

            Some(place)
        }
        // The parser reads a template, and only a template, into an interpolated string.
        (
            Node::Expression(Expression::CompositeString(CompositeString::Interpolated(_)))
            | Node::CompositeString(CompositeString::Interpolated(_))
            | Node::InterpolatedString(_)
            | Node::StringPart(StringPart::Literal(_) | StringPart::BracedExpression(_))
            | Node::LiteralStringPart(_)
            | Node::BracedExpressionStringPart(_),
            Body | Constant,
        ) => Some(place),
        // A case's `value` and `name` are known before the code runs, as PHP reads `Plan::Free->value` in a constant
        // expression. A case is capitalized, as in `Plan.Free`, and so is a constant that holds one, as in
        // `Billing.DEFAULT_PLAN`.
        (
            Node::Expression(Expression::Access(Access::Property(PropertyAccess {
                object:
                    Expression::Access(Access::Property(PropertyAccess {
                        property: ClassLikeMemberSelector::Identifier(case),
                        ..
                    })),
                property: ClassLikeMemberSelector::Identifier(member),
                ..
            }))),
            Constant,
        ) if case.value.first().is_some_and(u8::is_ascii_uppercase) && matches!(member.value, b"value" | b"name") => {
            Some(Constant)
        }
        (
            Node::Access(Access::Property(_))
            | Node::PropertyAccess(_)
            | Node::ClassLikeMemberSelector(ClassLikeMemberSelector::Identifier(_)),
            Constant,
        ) => Some(Constant),
        // Spec section 14.2: a lambda is a bare arrow, after one name or a parenthesized parameter list, and its body
        // is an expression or a block. The parser reads it into an arrow function or a closure without a keyword.
        (Node::Expression(Expression::ArrowFunction(_) | Expression::Closure(_)), Body) => Some(Body),
        // A lambda in a constant value captures nothing, so its value is known before the code runs.
        (Node::Expression(lambda @ (Expression::ArrowFunction(_) | Expression::Closure(_))), Constant) => {
            (!report_lambda_capture(lambda, context)).then_some(Body)
        }
        (Node::ArrowFunction(_) | Node::Closure(_), Body) => Some(Lambda),
        (Node::FunctionLikeParameterList(_), Lambda) => Some(Lambda),
        (Node::Block(_), Lambda) => Some(Body),
        (Node::Expression(_), Lambda) => enter(node, Body, context),
        // `check_yield` reports a `yield` at its keyword, so its value is still checked as the method body's.
        (Node::Expression(Expression::Yield(r#yield)), Body) => {
            check_yield(r#yield, context);

            Some(Body)
        }
        (Node::Yield(_) | Node::YieldValue(_) | Node::YieldSpread(_) | Node::YieldFrom(_) | Node::YieldPair(_), Body) => {
            Some(Body)
        }
        (Node::BinaryOperator(operator), Body | Constant) if is_slice_binary_operator(operator) => Some(place),
        // Spec section 21 removes PHP's word operators, so `=` can no longer bind before `and`. The operands are still
        // checked.
        (
            Node::BinaryOperator(
                operator @ (BinaryOperator::LowAnd(_) | BinaryOperator::LowOr(_) | BinaryOperator::LowXor(_)),
            ),
            Body | Constant,
        ) => {
            report_unexpected(operator.span(), context);

            Some(place)
        }
        // `check_cast` decided the cast at its `UnaryPrefix`.
        (Node::UnaryPrefixOperator(operator), Body | Constant) if operator.is_cast() => Some(place),
        (Node::UnaryPrefixOperator(operator), Body | Constant) if is_slice_prefix_operator(operator, place) => {
            Some(place)
        }

        (
            Node::Expression(
                Expression::Assignment(_)
                | Expression::UnaryPostfix(_)
                | Expression::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::NullSafeFunction(_))
                | Expression::Access(Access::Property(_) | Access::NullSafeProperty(_))
                | Expression::Throw(_)
                | Expression::Leave(_)
                | Expression::Propagate(_),
            )
            | Node::Throw(_)
            | Node::Leave(_)
            | Node::Propagate(_)
            | Node::Assignment(_)
            | Node::UnaryPostfix(_)
            | Node::UnaryPostfixOperator(UnaryPostfixOperator::PostIncrement(_) | UnaryPostfixOperator::PostDecrement(_))
            | Node::Call(Call::Method(_) | Call::NullSafeMethod(_) | Call::NullSafeFunction(_))
            | Node::NullSafeFunctionCall(_)
            | Node::Access(Access::Property(_) | Access::NullSafeProperty(_))
            | Node::ClassLikeMemberSelector(ClassLikeMemberSelector::Identifier(_))
            | Node::ArgumentList(_)
            | Node::Argument(_)
            | Node::NamedArgument(_),
            Body,
        ) => Some(Body),
        (Node::Expression(Expression::Instantiation(instantiation)), Body)
            if matches!(
                instantiation.class,
                Expression::Identifier(Identifier::Local(_))
                    | Expression::Self_(_)
                    | Expression::Static(_)
                    | Expression::Parenthesized(_)
            ) =>
        {
            check_instantiation(instantiation, context)
        }
        // `check_instantiation` reported PHP's `self` and `static`.
        (
            Node::Instantiation(_)
            | Node::ConstructorName(_)
            | Node::Expression(
                Expression::Identifier(Identifier::Local(_)) | Expression::Self_(_) | Expression::Static(_),
            ),
            Instantiation,
        ) => Some(Instantiation),
        // `new (type)(…)` creates an object from the class value in the parentheses, spec section 25.
        (Node::Expression(Expression::Parenthesized(_)), Instantiation) => Some(Body),
        (Node::ArgumentList(_) | Node::ObjectInitializer(_), Instantiation) => Some(Body),
        // Spec section 16: `with` copies an object, and an object initializer names the values it sets.
        (
            Node::Expression(Expression::With(_)) | Node::With(_) | Node::ObjectInitializer(_) | Node::NamedValue(_),
            Body,
        ) => Some(Body),
        (Node::AssignmentOperator(operator), Body) if is_slice_assignment_operator(operator) => Some(Body),
        (Node::PositionalArgument(_), Body) => Some(Body),
        // A constructor runs once, when `new` creates the object, so no code calls one as a method, as in C#.
        (Node::MethodCall(MethodCall { method, .. }) | Node::NullSafeMethodCall(NullSafeMethodCall { method, .. }), Body)
            if matches!(method, ClassLikeMemberSelector::Identifier(name) if name.value.eq_ignore_ascii_case(b"__construct")) =>
        {
            context.report(
                Issue::error(
                    "A constructor runs once, when `new` creates the object: pass the base class's arguments with `: super(…)`.",
                )
                .with_annotation(Annotation::primary(method.span()).with_message("Called here.")),
            );

            None
        }
        // `super.label()` calls the parent's method, and `super<HasLabel>.label()` the interface `HasLabel`'s default
        // body, spec section 22. Neither `super` is a value of its own.
        (Node::MethodCall(call), Body) if matches!(call.object, Expression::Parent(_) | Expression::SuperOf(_)) => {
            Some(SuperCall)
        }
        (Node::Expression(Expression::SuperOf(_)), SuperCall) => None,
        (Node::Expression(expression @ Expression::SuperOf(_)), Body)
        | (Node::Expression(expression @ Expression::Parent(Keyword { value: b"super", .. })), Body) => {
            context.report(
                Issue::error("`super` only calls a method, as in `super.label()` or `super<HasLabel>.label()`.")
                    .with_annotation(Annotation::primary(expression.span()).with_message("Written here.")),
            );

            None
        }
        // `Self.make()` calls a static method of the class a static method is called on, spec section 25.
        (Node::MethodCall(call), Body) if let Expression::Self_(keyword) = call.object => {
            if !is_sharp_self(keyword) {
                report_unexpected(keyword.span, context);
            }

            Some(SelfCall)
        }
        (Node::Expression(Expression::Parent(_)), SuperCall) | (Node::Expression(Expression::Self_(_)), SelfCall) => None,
        (Node::ClassLikeMemberSelector(_) | Node::TypeArgumentList(_) | Node::ArgumentList(_), SuperCall | SelfCall) => {
            enter(node, Body, context)
        }
        (Node::MethodCall(call), Body) => {
            check_member_access(call.object, &call.method, context);

            Some(Body)
        }
        (Node::PropertyAccess(access), Body) => {
            check_member_access(access.object, &access.property, context);

            Some(Body)
        }
        (Node::NullSafeMethodCall(call), Body) => check_null_safe_object(call.span(), call.object, &call.method, context),
        (Node::NullSafePropertyAccess(access), Body) => {
            check_null_safe_object(access.span(), access.object, &access.property, context)
        }

        // PHP# never has `$` variables. A `$` variable inside a construct the walk refuses adds no second error.
        (Node::Expression(Expression::Variable(variable)), Body | Constant) => {
            report_unexpected(variable.span(), context);

            None
        }
        // Spec section 25 removes PHP's `self` and `static`. `Self` is no value of its own.
        (Node::Expression(Expression::Self_(keyword)), Body | Constant) if !is_sharp_self(keyword) => {
            report_unexpected(keyword.span, context);

            None
        }
        (Node::Expression(Expression::Static(keyword)), Body | Constant) => {
            report_unexpected(keyword.span, context);

            None
        }
        // PHP# never has top-level functions or `global`: `check_function` and `check_global` report them. A function,
        // at file level or in a body, is reported at its name, so its parameters and body are checked as a method's.
        // `global` holds only the names it declares.
        (Node::Statement(Statement::Function(_)), File | Body) => Some(File),
        (Node::Function(_), File) => Some(Method),
        (Node::Statement(Statement::Global(_)), Body) => None,
        // Spec sections 8 and 29: plain PHP functions are called by their bare names. The analyzer refuses a call
        // that resolves to a namespaced function. Spec section 14: a local holding a function is called the same way.
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if let Expression::Identifier(name @ Identifier::Local(_)) = function_call.function
                && matches!(context.names.binding(name), None | Some(Binding::Local(_))) =>
        {
            Some(FunctionCall)
        }
        // Spec section 14: any other expression that gives a function is called the same way, as `steps[0](1)` and
        // `this.make()(1)`, and as `f?.(x)` calls it.
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
        | (Node::Call(Call::Function(function_call)), Body)
        | (Node::FunctionCall(function_call), Body)
            if !matches!(function_call.function, Expression::Identifier(_)) =>
        {
            Some(Body)
        }
        (
            Node::Call(Call::Function(_))
            | Node::FunctionCall(_)
            | Node::Expression(Expression::Identifier(Identifier::Local(_))),
            FunctionCall,
        ) => Some(FunctionCall),
        (Node::ArgumentList(_), FunctionCall) => Some(Body),

        (Node::Expression(_), Constant) if is_attribute_argument(context) => {
            report_attribute_argument(node.span(), context);

            None
        }
        _ => {
            report_unsupported(node, place, context);

            None
        }
    }
}

/// Spec section 1: an attribute list's target, as `param:` in `[param: Trimmed]`, names a part of its declaration, as
/// C# allows: `type:` on a type, `event:` on an event, `method:` on a method, an operator, a named constructor, a lambda
/// or an accessor, `param:` on a parameter and on `set` or `init`, `field:` on a field, a constant, an enum case and a
/// property with storage, `property:` on a property, and `property:` and `field:` on a parameter that declares a
/// property: a promoted one, an event's or a data case's. An attribute always describes a declaration, so C#'s
/// `return:` is no PHP# syntax.
fn check_attribute_target(list: &AttributeList, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    /// The declaration a list is on, and its targets.
    type Declaration = (&'static str, &'static [&'static str]);

    let Some(target) = &list.target else {
        return Some(Place::Attribute);
    };
    if target.name.value == b"return" {
        report_unexpected(target.name.span, context);

        return None;
    }
    let written = BytesDisplay(target.name.value);
    // The declaration that holds the list. The walk keeps no member on its ancestors, so the file names it.
    let holds = |lists: &Sequence<AttributeList>| lists.iter().any(|held| held.span() == list.span());
    let declaration = Node::Program(context.program).filter_map(|node| -> Option<Declaration> {
        match node {
            Node::Class(class) if holds(&class.attribute_lists) => Some(("a class", &["type"])),
            Node::Interface(interface) if holds(&interface.attribute_lists) => Some(("an interface", &["type"])),
            Node::Enum(r#enum) if holds(&r#enum.attribute_lists) => Some(("an enum", &["type"])),
            Node::Struct(r#struct) if holds(&r#struct.attribute_lists) => Some(("a struct", &["type"])),
            Node::Event(event) if holds(&event.attribute_lists) => Some(("an event", &["type", "event"])),
            Node::Method(method) if holds(&method.attribute_lists) => Some(("a method", &["method"])),
            Node::Operator(operator) if holds(&operator.attribute_lists) => Some(("an operator", &["method"])),
            Node::NamedConstructor(constructor) if holds(&constructor.attribute_lists) => {
                Some(("a named constructor", &["method"]))
            }
            Node::Closure(Closure { attribute_lists, .. })
            | Node::ArrowFunction(ArrowFunction { attribute_lists, .. })
                if holds(attribute_lists) =>
            {
                Some(("a lambda", &["method"]))
            }
            Node::ClassLikeConstant(constant) if holds(&constant.attribute_lists) => Some(("a constant", &["field"])),
            Node::EnumCase(case) if holds(&case.attribute_lists) => Some(("an enum case", &["field"])),
            Node::Property(Property::Plain(field)) if holds(&field.attribute_lists) => Some(("a field", &["field"])),
            Node::Property(Property::Hooked(property)) if holds(&property.attribute_lists) => Some(
                if context.names.has_storage(&property.hook_list)
                    && !property.modifiers.contains_abstract()
                    && !is_in_interface(context.program, property.span())
                {
                    ("a property", &["property", "field"])
                } else {
                    ("a property without storage", &["property"])
                },
            ),
            Node::Property(Property::Computed(property)) if holds(&property.attribute_lists) => {
                Some(("a property without storage", &["property"]))
            }
            Node::FunctionLikeParameter(parameter) if holds(&parameter.attribute_lists) => {
                Some(if declares_property(parameter, context.program) {
                    ("a parameter that declares a property", &["param", "property", "field"])
                } else {
                    ("a parameter", &["param"])
                })
            }
            Node::PropertyHook(accessor) if holds(&accessor.attribute_lists) => Some(if accessor.is_setter() {
                ("`set` and `init`", &["method", "param"])
            } else {
                ("`get`", &["method"])
            }),
            _ => None,
        }
    });
    let Some((declaration, targets)) = declaration.into_iter().next() else {
        context.report(
            Issue::error(format!("`{written}:` names no part of this declaration."))
                .with_annotation(Annotation::primary(target.span()).with_message("Written here."))
                .with_help("Write the attribute with no target."),
        );

        return None;
    };

    if !targets.iter().any(|named| named.as_bytes() == target.name.value) {
        let targets: Vec<String> = targets.iter().map(|named| format!("`{named}:`")).collect();
        context.report(
            Issue::error(format!(
                "`{written}:` names no part of {declaration}, whose targets are {}.",
                targets.join(", ")
            ))
            .with_annotation(Annotation::primary(target.span()).with_message("Written here.")),
        );

        return None;
    }

    Some(Place::Attribute)
}

/// Whether `parameter` declares a property as well: a promoted constructor parameter, or a parameter of an event or of
/// a case that carries data, each of which is a public property of its class, spec sections 15 and 20.
fn declares_property(parameter: &FunctionLikeParameter, program: &Program) -> bool {
    let start = parameter.span().start;

    parameter.is_promoted_property()
        || !Node::Program(program)
            .filter_map(|node| match node {
                Node::Event(Event { parameter_list, .. })
                | Node::EnumCaseDataItem(EnumCaseDataItem { parameter_list, .. }) => {
                    parameter_list.span().contains(&start).then_some(())
                }
                _ => None,
            })
            .is_empty()
}

/// Spec section 1: an attribute argument may create an object of a class it names, from arguments that are constants or
/// more such objects, as PHP and Java's nested annotations take, and PHP creates it each time it reads the attribute. A
/// named constructor, an object initializer and a spread run code there. PHP creates the object with no PHP# code to
/// give it type arguments, so a generic class's object is an error.
fn check_attribute_object(instantiation: &Instantiation, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    if let Some(type_arguments) = &instantiation.type_arguments {
        context.report(
            Issue::error(format!(
                "An attribute argument creates its objects without type arguments, so it cannot `new` a generic class, as `{}` does.",
                BytesDisplay(context.get_code_snippet(instantiation))
            ))
            .with_annotation(Annotation::primary(type_arguments.span()).with_message("Written here.")),
        );

        return None;
    }

    let names_class = matches!(instantiation.class, Expression::Identifier(Identifier::Local(class))
        if !context.names.is_type_parameter(class));
    if !names_class
        || instantiation.constructor.is_some()
        || instantiation.initializer.is_some()
        || instantiation.argument_list.is_none()
    {
        report_attribute_argument(instantiation.span(), context);

        return None;
    }

    let spreads: Vec<Span> = instantiation
        .argument_list
        .iter()
        .flat_map(|list| list.arguments.iter())
        .filter_map(|argument| match argument {
            Argument::Positional(positional) if positional.ellipsis.is_some() => Some(positional.span()),
            _ => None,
        })
        .collect();
    for spread in &spreads {
        report_attribute_argument(*spread, context);
    }

    spreads.is_empty().then_some(Place::Attribute)
}

/// Whether the constant value the walk is in is an attribute argument itself, and not a pattern's value or a lambda's
/// body inside one.
fn is_attribute_argument(context: &Context<'_, '_, '_>) -> bool {
    context
        .slice_places
        .iter()
        .rev()
        .flatten()
        .find(|place| matches!(place, Place::Attribute | Place::Body | Place::Pattern))
        == Some(&Place::Attribute)
}

fn report_attribute_argument(span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!(
            "An attribute argument is a constant or a `new` of constants, and `{}` is neither.",
            BytesDisplay(context.get_code_snippet(span))
        ))
        .with_annotation(Annotation::primary(span).with_message("Written here.")),
    );
}

/// Whether the slice has a method, with the refusal at its name when it does not. The constructor of spec section 9 is
/// the one method without a return type, named after its class, and a static constructor is not supported yet.
/// `enter` reports a `__Something__` name, which ends as well as starts with `__`.
fn is_slice_method(method: &Method, program: &Program) -> Result<(), Box<Issue>> {
    let class_name = enclosing_class(program, method.span()).map(|class| class.name.value);
    let refusal = |message: String, note: String| {
        Err(Box::new(
            Issue::error(message)
                .with_annotation(Annotation::primary(method.name.span).with_message("Declared here."))
                .with_note(note),
        ))
    };

    if method.return_type_hint.is_none() && class_name != Some(method.name.value) {
        refusal(
            "A PHP# method needs a return type: only the constructor, named after its class, has none.".to_owned(),
            "A method is written `public int total()`, and the constructor `public Report()`.".to_owned(),
        )
    } else if method.return_type_hint.is_none() && method.is_static() {
        Err(Box::new(
            Issue::error("A static constructor is not supported yet in PHP#.")
                .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                .with_note("The main constructor of a PHP# class is not `static`."),
        ))
    } else if method.return_type_hint.is_some()
        && let Some(class_name) = class_name.filter(|class_name| class_name.eq_ignore_ascii_case(method.name.value))
    {
        let class_name = BytesDisplay(class_name);
        refusal(
            format!("Only the constructor is named after its class: rename `{}`.", BytesDisplay(method.name.value)),
            format!("The constructor has no return type, as in `public {class_name}()`."),
        )
    } else {
        Ok(())
    }
}

/// Reports a method's `final` or `virtual` that can't change what a subclass may do, spec section 22. A PHP# method is
/// closed unless it is `virtual`, so `final` marks only an override, and `virtual` is an error on a method no subclass
/// replaces: a private method, a constructor, an abstract method or an override, which are open already, and every
/// method of a class-like no class extends. `check_extension` reports every modifier of an extension member.
fn check_method_modifiers(method: &Method, context: &mut Context<'_, '_, '_>) {
    let start = method.span().start;
    if enclosing_class(context.program, method.span()).is_some_and(|class| {
        class
            .members
            .iter()
            .any(|member| matches!(member, ClassLikeMember::Extension(extension) if extension.span().contains(&start)))
    }) {
        return;
    }

    let sealed_by = file_statements(context.program).find_map(|statement| match statement {
        Statement::Class(class)
            if class.span().contains(&start)
                && (class.modifiers.contains_final() || class.modifiers.contains_static()) =>
        {
            let modifier = if class.modifiers.contains_final() { "final" } else { "static" };
            Some(format!("`{}` is `{modifier}`", BytesDisplay(class.name.value)))
        }
        Statement::Enum(r#enum) if r#enum.span().contains(&start) => {
            Some(format!("`{}` is an enum", BytesDisplay(r#enum.name.value)))
        }
        _ => None,
    });

    let is_override = method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Override(_)));
    for modifier in &method.modifiers {
        let message = match modifier {
            Modifier::Final(_) if !is_override => {
                "`final` marks only an override: a PHP# method is already closed unless it is `virtual`.".to_owned()
            }
            Modifier::Final(_) | Modifier::Virtual(_) if let Some(sealed_by) = &sealed_by => {
                let word = BytesDisplay(modifier.get_keyword().value);
                format!("{sealed_by}, so no subclass replaces its methods, and `{word}` can't matter.")
            }
            Modifier::Virtual(_) if method.modifiers.contains_private() => {
                "A `private` method is never replaced, so `virtual` can't matter.".to_owned()
            }
            Modifier::Virtual(_) if method.return_type_hint.is_none() => {
                "A constructor is never replaced, so `virtual` can't matter.".to_owned()
            }
            Modifier::Virtual(_) if is_override => {
                "An override is already open to the subclasses below it, so `virtual` can't matter.".to_owned()
            }
            Modifier::Virtual(_) if method.modifiers.contains_abstract() => {
                "An `abstract` method is already open to the subclasses below it, so `virtual` can't matter.".to_owned()
            }
            _ => continue,
        };

        context.report(
            Issue::error(message).with_annotation(Annotation::primary(modifier.span()).with_message("Remove it.")),
        );
    }
}

/// Whether the slice has an interface method, with the refusal at its name when it does not. `enter` reports a
/// modifier, a body and a name that starts with `__`.
fn is_slice_signature(method: &Method) -> Result<(), Box<Issue>> {
    if method.return_type_hint.is_none() {
        Err(Box::new(
            Issue::error("An interface method needs a return type in PHP#.")
                .with_annotation(Annotation::primary(method.name.span).with_message("Declared here."))
                .with_note("An interface method is written `float area();`, with its return type first."),
        ))
    } else {
        Ok(())
    }
}

/// Checks an operator a class, an enum or an interface declares, spec section 19: a symbol a class can declare,
/// `public static`, or `static` alone in an interface, whose members are written bare, the number of parameters its
/// symbol takes with one of them the declaring type itself, `bool` from `==` and `int` from `<=>`, and one declaration
/// of it in its type. A refused symbol is the one error, because the other rules depend on it. The analyzer checks
/// that a type that declares `==` has a `hash()`.
fn check_operator(operator: &Operator, context: &mut Context<'_, '_, '_>) {
    let start = operator.span().start;
    let Some((owner, members, kind)) = file_statements(context.program).find_map(|statement| match statement {
        Statement::Interface(interface) if interface.span().contains(&start) => {
            Some((&interface.name, &interface.members, "interface"))
        }
        Statement::Enum(r#enum) if r#enum.span().contains(&start) => Some((&r#enum.name, &r#enum.members, "enum")),
        Statement::Class(class) if class.span().contains(&start) => Some((&class.name, &class.members, "class")),
        Statement::Struct(r#struct) if r#struct.span().contains(&start) => {
            Some((&r#struct.name, &r#struct.members, "class"))
        }
        _ => None,
    }) else {
        return;
    };
    let interface = kind == "interface";
    let names = context.names;
    let class_name = BytesDisplay(owner.value);
    let symbol = BytesDisplay(operator.symbol.as_bytes());
    if interface {
        for modifier in operator.modifiers.iter().filter(|modifier| modifier.is_visibility()) {
            report_interface_member_access(modifier, context);
        }
    }
    let mut report = |message: String, span: Span| {
        context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message("Declared here.")));
    };

    let Some(name) = php_operator_name(&operator.symbol, operator.parameter_list.parameters.len()) else {
        let reason = match operator.symbol {
            BinaryOperator::NotEqual(_) | BinaryOperator::AngledNotEqual(_) => "it is derived from `==`".to_owned(),
            BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_) => "it is derived from `<=>`".to_owned(),
            _ => format!("only {} can", Operator::DECLARABLE),
        };

        return report(format!("`operator {symbol}` cannot be declared: {reason}."), operator.symbol.span());
    };

    if interface {
        if !operator
            .modifiers
            .iter()
            .all(|modifier| modifier.is_visibility() || matches!(modifier, Modifier::Static(_)))
            || !operator.modifiers.contains_static()
        {
            report(
                format!(
                    "An interface operator is `static`, as in `static {class_name} operator +({class_name} a, {class_name} b)`."
                ),
                operator.operator.span,
            );
        }
    } else if operator.modifiers.len() != 2
        || !operator.modifiers.contains_static()
        || operator.modifiers.get_public().is_none()
    {
        report(
            format!(
                "An operator is `public static`, as in `public static {class_name} operator +({class_name} a, {class_name} b)`."
            ),
            operator.operator.span,
        );
    }

    let parameters = &operator.parameter_list.parameters;
    let class_fqcn = names.get(owner);
    if matches!(operator.symbol, BinaryOperator::Subtraction(_)) && !matches!(parameters.len(), 1 | 2) {
        report(
            "`operator -` takes one parameter, to negate, or two, to subtract.".to_owned(),
            operator.parameter_list.span(),
        );
    } else if !matches!(operator.symbol, BinaryOperator::Subtraction(_)) && parameters.len() != 2 {
        report(format!("`operator {symbol}` takes two parameters."), operator.parameter_list.span());
    } else if !parameters
        .iter()
        .any(|parameter| parameter.hint.as_ref().is_some_and(|hint| is_class_type(hint, class_fqcn, names)))
    {
        report(
            format!("One parameter of `operator {symbol}` is `{class_name}`, the {kind} that declares it."),
            operator.parameter_list.span(),
        );
    }

    let return_type = &operator.return_type_hint.hint;
    match operator.symbol {
        BinaryOperator::Equal(_) if !matches!(return_type, Hint::Bool(_)) => {
            report("`operator ==` returns `bool`.".to_owned(), return_type.span());
        }
        BinaryOperator::Spaceship(_) if !matches!(return_type, Hint::Integer(_)) => {
            report("`operator <=>` returns `int`.".to_owned(), return_type.span());
        }
        _ => {}
    }

    let declared_before = members
        .iter()
        .filter_map(|member| match member {
            ClassLikeMember::Operator(earlier) => Some(earlier),
            _ => None,
        })
        .take_while(|earlier| earlier.span() != operator.span())
        .any(|earlier| php_operator_name(&earlier.symbol, earlier.parameter_list.parameters.len()) == Some(name));
    if declared_before {
        let unary = if name == b"op_UnaryNegation" { "Unary " } else { "" };
        report(format!("{unary}`operator {symbol}` is declared twice in `{class_name}`."), operator.operator.span);
    }
}

/// Whether a parameter's type is the class `class_fqcn`, written by its name, nullable or not, or as `Self`.
fn is_class_type(hint: &Hint, class_fqcn: &[u8], names: &ResolvedNames) -> bool {
    match hint {
        Hint::Nullable(nullable) => is_class_type(nullable.hint, class_fqcn, names),
        Hint::Identifier(identifier) => {
            names.resolve(identifier).is_some_and(|name| name.eq_ignore_ascii_case(class_fqcn))
        }
        Hint::Self_(_) => true,
        _ => false,
    }
}

/// Reports what a static class cannot hold, as spec sections 26 and 29 write it: a modifier but `public` and `static`,
/// a header, a constructor, or a member that is not static. A constant is static already, and `check_extern` decides
/// an `extern` method.
fn check_static_class(class: &Class, context: &mut Context<'_, '_, '_>) {
    let mut report = |message: String, span: Span, written: &str| {
        context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message(written)));
    };

    for modifier in &class.modifiers {
        if let Modifier::Abstract(keyword) | Modifier::Final(keyword) | Modifier::Foreign(keyword) = modifier {
            report(
                format!("A static class takes only `public` and `static`: remove `{}`.", BytesDisplay(keyword.value)),
                keyword.span,
                "Written here.",
            );
        }
    }

    if let Some(inheritance) = &class.inheritance {
        report(
            "A static class cannot extend a class or implement an interface.".to_owned(),
            inheritance.span(),
            "Named here.",
        );
    }

    let non_static =
        |name: &[u8]| format!("A static class holds only static members: make `{}` static.", BytesDisplay(name));
    for member in &class.members {
        match member {
            ClassLikeMember::Method(method)
                if method.return_type_hint.is_none() && method.name.value == class.name.value =>
            {
                report("A static class has no constructor.".to_owned(), method.name.span, "Declared here.");
            }
            ClassLikeMember::NamedConstructor(constructor) => {
                report("A static class has no constructor.".to_owned(), constructor.name.span, "Declared here.");
            }
            ClassLikeMember::Method(method) if !method.is_static() && !is_extern(method) => {
                report(non_static(method.name.value), method.name.span, "Declared without `static` here.");
            }
            ClassLikeMember::Property(property) if !property.modifiers().contains_static() => {
                let variable = property.first_variable();
                report(non_static(variable.name), variable.span, "Declared without `static` here.");
            }
            _ => {}
        }
    }
}

/// Checks an extension block's type parameters, spec section 26. A call infers them from its receiver, so each one
/// appears in the receiver's type, as C#'s CS9295 requires. Each member runs as a static method whose type parameters
/// are the block's and then its own, so a member can't declare one of the block's names again.
fn check_extension_type_parameters(
    extension: &Extension,
    type_parameters: &TypeParameterList,
    context: &mut Context<'_, '_, '_>,
) {
    if let [receiver] = extension.parameter_list.parameters.as_slice() {
        let receiver_type = receiver.hint.as_ref();
        let used: Vec<Span> = receiver_type.map_or_else(Vec::new, |hint| {
            Node::Hint(hint).filter_map(|node| match node {
                Node::Identifier(name) => match context.names.binding(*name) {
                    Some(Binding::TypeParameter { declaration }) => Some(declaration),
                    _ => None,
                },
                _ => None,
            })
        });
        let written = receiver_type.map_or(&b""[..], |hint| context.get_code_snippet(hint));
        for parameter in type_parameters.parameters.iter().filter(|parameter| !used.contains(&parameter.name.span)) {
            context.report(
                Issue::error(format!(
                    "The receiver `{}` doesn't use `{}`: a call infers an extension block's type parameters from its receiver, so each one appears there.",
                    BytesDisplay(written),
                    BytesDisplay(parameter.name.value)
                ))
                .with_annotation(Annotation::primary(parameter.name.span).with_message("Declared here."))
                .with_help("Declare it on the member that uses it, as in `public List<TOut> converted<TOut>(…)`."),
            );
        }
    }

    for member in &extension.members {
        let ClassLikeMember::Method(Method { type_parameters: Some(own), .. }) = member else {
            continue;
        };
        for parameter in own.parameters.iter() {
            let Some(block) = type_parameters.parameters.iter().find(|block| block.name.value == parameter.name.value)
            else {
                continue;
            };
            context.report(
                Issue::error(format!(
                    "`{}` is already a type parameter of the extension block: give the member's another name.",
                    BytesDisplay(parameter.name.value)
                ))
                .with_annotation(Annotation::primary(parameter.name.span).with_message("Declared again here."))
                .with_annotation(Annotation::secondary(block.name.span).with_message("Declared by the block here.")),
            );
        }
    }
}

/// Reports what spec section 10 refuses in a struct: a modifier but `public`, `readonly` above all because every
/// struct is read-only, a `set` accessor, and a member modifier only a subclass would need.
fn check_struct_declaration(r#struct: &Struct, context: &mut Context<'_, '_, '_>) {
    let mut report = |issue: Issue| context.report(issue);

    for modifier in &r#struct.modifiers {
        let issue = match modifier {
            Modifier::Public(_) => continue,
            Modifier::Readonly(_) => Issue::error("Every struct is already read-only: write `struct`.")
                .with_annotation(Annotation::primary(modifier.span()).with_message("Remove `readonly`.")),
            _ => Issue::error(format!(
                "A struct takes only `public`: remove `{}`.",
                BytesDisplay(modifier.get_keyword().value)
            ))
            .with_annotation(Annotation::primary(modifier.span()).with_message("Written here."))
            .with_note("A struct is a value: it has objects, and no subclasses."),
        };
        report(issue);
    }

    let parameters = r#struct.members.iter().flat_map(|member| match member {
        ClassLikeMember::Method(method) => method.parameter_list.parameters.as_slice(),
        _ => &[],
    });
    let accessor_lists = r#struct
        .members
        .iter()
        .filter_map(|member| match member {
            ClassLikeMember::Property(Property::Hooked(property)) => {
                Some((&property.hook_list, property.item.variable().name))
            }
            _ => None,
        })
        .chain(parameters.clone().filter_map(|parameter| Some((parameter.hooks.as_ref()?, parameter.variable.name))));
    for (accessors, name) in accessor_lists {
        for setter in accessors.hooks.iter().filter(|accessor| accessor.name.value == b"set") {
            report(
                Issue::error(format!(
                    "A struct never changes, so `{}` takes `init`, not `set`.",
                    BytesDisplay(name.strip_prefix(b"$").unwrap_or(name))
                ))
                .with_annotation(Annotation::primary(setter.name.span).with_message("Written here."))
                .with_note("A struct's property is set only while the struct is created."),
            );
        }
    }

    let member_modifiers = r#struct
        .members
        .iter()
        .flat_map(|member| match member {
            ClassLikeMember::Method(method) => method.modifiers.as_slice(),
            ClassLikeMember::Property(property) => property.modifiers().as_slice(),
            ClassLikeMember::Constant(constant) => constant.modifiers.as_slice(),
            ClassLikeMember::NamedConstructor(constructor) => constructor.modifiers.as_slice(),
            _ => &[],
        })
        .chain(parameters.flat_map(|parameter| parameter.modifiers.as_slice()));
    for modifier in member_modifiers {
        if matches!(
            modifier,
            Modifier::Protected(_) | Modifier::Virtual(_) | Modifier::Abstract(_) | Modifier::Override(_)
        ) {
            report(
                Issue::error("A struct has no subclasses, so write `private`.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Written here.")),
            );
        }
    }
}

/// Whether `hint` is a class value's type, `Class<T>`, nullable or not.
fn is_class_value_type(hint: &Hint) -> bool {
    match hint {
        Hint::Generic(generic) => generic.name.value == b"Class",
        Hint::Nullable(nullable) => is_class_value_type(nullable.hint),
        _ => false,
    }
}

/// Checks an extension block of a static class, spec section 26: one receiver, a parameter with no modifier, default
/// or `...`, and members that are methods or computed properties of the receiver, each with only an access modifier,
/// naming the receiver and never `this`. A static class has no constructor, in an extension either. A class value's
/// members are `Class`'s own and the statics of the class it holds, spec section 25, so only the standard library
/// extends `Class`.
fn check_extension(extension: &Extension, context: &mut Context<'_, '_, '_>) {
    let receivers = &extension.parameter_list.parameters;
    let refused_receiver = match receivers.as_slice() {
        [receiver] => {
            (!receiver.modifiers.is_empty() || receiver.default_value.is_some() || receiver.ellipsis.is_some())
                .then(|| receiver.span())
        }
        _ => Some(extension.parameter_list.span()),
    };
    if let Some(span) = refused_receiver {
        context.report(
            Issue::error(
                "An extension's receiver is one parameter, with no modifier, default or `...`, as in `extension(string value)`.",
            )
            .with_annotation(Annotation::primary(span).with_message("Written here.")),
        );
    }
    if let Some(type_parameters) = extension.type_parameters {
        check_extension_type_parameters(extension, type_parameters, context);
    }
    if let [FunctionLikeParameter { hint: Some(hint), .. }] = receivers.as_slice()
        && is_class_value_type(hint)
        && !is_standard_library_namespace(context.program)
    {
        context.report(
            Issue::error(
                "An extension can't add members to `Class`: a class value's members are `Class`'s own and the statics of the class it holds.",
            )
            .with_annotation(Annotation::primary(hint.span()).with_message("Receiver written here."))
            .with_help("Write a static method that takes the class value, as in `public static string describe<T>(Class<T> type)`."),
        );
    }

    let receiver = receivers.first().map_or(b"value".as_slice(), |receiver| receiver.variable.name);
    let this = format!("An extension member names its receiver: write `{}`, not `this`.", BytesDisplay(receiver));
    let class = enclosing_class(context.program, extension.span()).map(|class| class.name.value);
    for member in &extension.members {
        let (modifiers, body) = match member {
            ClassLikeMember::Method(method)
                if method.return_type_hint.is_none() && class == Some(method.name.value) =>
            {
                context.report(
                    Issue::error("A static class has no constructor.")
                        .with_annotation(Annotation::primary(method.name.span).with_message("Declared here.")),
                );

                continue;
            }
            ClassLikeMember::Method(method) => (&method.modifiers, Node::MethodBody(&method.body)),
            ClassLikeMember::Property(Property::Computed(property)) => {
                (&property.modifiers, Node::PropertyHookConcreteExpressionBody(&property.body))
            }
            member => {
                context.report(
                    Issue::error(
                        "An extension adds only methods and computed properties, as in `public bool isBlank => value == \"\";`.",
                    )
                    .with_annotation(Annotation::primary(member.span()).with_message("Declared here.")),
                );

                continue;
            }
        };
        // Spec section 5's `internal` is an access modifier.
        for modifier in modifiers
            .iter()
            .filter(|modifier| !modifier.is_read_visibility() && !matches!(modifier, Modifier::Internal(_)))
        {
            context.report(
                Issue::error(format!(
                    "An extension member takes only an access modifier: remove `{}`.",
                    BytesDisplay(modifier.get_keyword().value)
                ))
                .with_annotation(Annotation::primary(modifier.span()).with_message("Written here.")),
            );
        }
        report_this(body, &this, None, context);
    }
}

/// Whether a method is `extern`: its body is native, compiled into the engine, as spec section 29 writes it.
fn is_extern(method: &Method) -> bool {
    method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_)))
}

/// Refuses every name in the `uses` of a method with a body, spec section 29: the checker works a body's effects out
/// from its code, so only a method without a body states the effects its implementations may have. A method is pure
/// without `uses`, so its `none` says to delete it. A function parameter in `uses`, as the old
/// `charge(…) uses prepare => …` wrote it, is the parser's error.
fn check_method_uses(method: &Method, context: &mut Context<'_, '_, '_>) {
    if method.is_abstract() {
        return;
    }

    for name in MethodParts::of_method(method).uses_effects() {
        if name.value == b"none" {
            context.report(
                Issue::error("A method without `uses` is pure already, so delete `uses none`.")
                    .with_annotation(Annotation::primary(name.span).with_message("Written here.")),
            );

            continue;
        }

        context.report(
            Issue::error(format!(
                "A method with a body takes no effect in `uses`, and it names `{}`.",
                BytesDisplay(name.value)
            ))
            .with_annotation(Annotation::primary(name.span).with_message("Written here."))
            .with_note("Its effects come from its code.")
            .with_help("Remove it from `uses`, or declare the method without a body."),
        );
    }
}

/// Refuses an abstract method that is `private`, written or by spec section 5's default: a subclass implements it, so
/// it is `public` or `protected`, as C# refuses a `private` one. PHP's own engine refuses it only when it runs.
fn check_abstract_access(method: &Method, context: &mut Context<'_, '_, '_>) {
    if !method.modifiers.contains_abstract()
        || method
            .modifiers
            .iter()
            .any(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Internal(_)))
    {
        return;
    }

    context.report(
        Issue::error(
            "A subclass implements an abstract method, so it is `public` or `protected`: write `protected abstract`.",
        )
        .with_annotation(Annotation::primary(method.name.span).with_message("Declared here.")),
    );
}

/// The place of a method's parts, refusing an `extern` method that is not `static`, in a static or `foreign` class,
/// with no body. A `foreign` class's native body is the one part of an effect that plain PHP cannot reach, as
/// `Environment` reads the command line. The refusal is at its name, so its other parts, a body written by mistake
/// among them, stay checked. Only the standard library declares native bodies, but the engine compiles the library's
/// files from `vendor/` as any other, so the analyzer, which knows a file's package, refuses one anywhere else.
fn check_extern(method: &Method, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    if !is_extern(method)
        || (enclosing_class(context.program, method.span()).is_some_and(|class| class.is_static || class.is_foreign)
            && method.is_static()
            && matches!(method.body, MethodBody::Abstract(_)))
    {
        return Some(Place::Method);
    }

    report_refusal(
        Issue::error("An `extern` method is `static`, in a static or `foreign` class, with no body.")
            .with_annotation(Annotation::primary(method.name.span).with_message("Declared `extern` here.")),
        method.span(),
        Place::Method,
        context,
    )
}

/// Whether an import names PHP's engine class `Error`, or gives the file the name `Error`, which a `.sharp` file keeps
/// for the standard library's interface, spec section 31.
fn imports_error(item: &UseItem) -> bool {
    let name = item.name.value();
    let last = name.rsplit(|byte| matches!(byte, b'.' | b'\\')).next().unwrap_or(name);
    let given = item.alias.as_ref().map_or(last, |alias| alias.identifier.value);

    given == b"Error" || name.strip_prefix(b"\\").unwrap_or(name) == b"Error"
}

/// Decides an `extern event`, which marks a plain PHP class as an event, spec section 15: it names a class, whose
/// effects its `uses` lists.
fn check_extern_event(r#extern: &Extern, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let Identifier::Dotted(target) = &r#extern.target else {
        return Some(Place::File);
    };

    let issue =
        Issue::error(format!("An `extern event` names a class, and `{}` names a member.", BytesDisplay(target.value)))
            .with_annotation(Annotation::primary(target.span).with_message("Named here."))
            .with_help("Name the event's class, as in `extern event JobFailed;`.");

    report_refusal(issue, r#extern.span(), Place::File, context)
}

/// Decides a parameter of an event, spec section 15, or a field of a case that carries data, spec section 20, at
/// `place`, which runs as a public get-only property of the class the event or the case runs as: a type, a name and
/// an optional default. PHP refuses a variadic promoted property, and the declaration decides the property's access,
/// so a modifier, accessors and `...` are refused. The rest is checked as a method's parameter.
fn check_property_parameter(
    parameter: &FunctionLikeParameter,
    place: Place,
    context: &mut Context<'_, '_, '_>,
) -> Option<Place> {
    let (part, owner) =
        if place == Place::Case { ("A case's field", "case") } else { ("An event's parameter", "event") };
    let remove = |span: Span, message: String| {
        Issue::error(message).with_annotation(Annotation::primary(span).with_message("Remove this."))
    };
    let issue = if let Some(modifier) = parameter.modifiers.first() {
        // The issue covers the modifier, not the parameter, so the walk stops here: it would refuse `readonly` again.
        context.report(remove(
            modifier.span(),
            format!("{part} takes no modifier: it is a public get-only property of the {owner}."),
        ));

        return None;
    } else if let Some(accessors) = &parameter.hooks {
        remove(accessors.span(), format!("{part} takes no accessors: it is a get-only property of the {owner}."))
    } else if let Some(ellipsis) = parameter.ellipsis {
        remove(ellipsis, format!("{part} cannot be variadic: it is one property of the {owner}."))
    } else {
        return enter(Node::FunctionLikeParameter(parameter), Place::Method, context);
    };

    report_refusal(issue, parameter.span(), Place::Parameter, context)
}

/// Whether the slice has an interface property: a type, a name and `{ get; }` or `{ get; set; }`, which PHP runs as a
/// public interface property its classes give, as an event's parameters give `OrderEvent`'s `orderId`, spec section
/// 15. A class gives one stated with `set` a `set` too, which the analyzer checks. Each accessor may take attributes,
/// which PHP keeps on its abstract hook, spec section 1. An access modifier, on the property or on an accessor, keeps
/// it in the slice, and `enter` reports the modifier where the walk reaches it.
fn is_interface_property(property: &HookedProperty) -> bool {
    let is_access = |modifier: &Modifier| {
        matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_))
    };
    let is_stated = |accessor: &PropertyHook, name: &[u8]| {
        accessor.name.value == name
            && accessor.ampersand.is_none()
            && accessor.modifiers.iter().all(is_access)
            && accessor.parameter_list.is_none()
            && matches!(accessor.body, PropertyHookBody::Abstract(_))
    };

    property.var.is_none()
        && property.modifiers.iter().all(is_access)
        && property.hint.is_some()
        && matches!(property.item, PropertyItem::Abstract(_))
        && property.initial_value.is_none()
        && match property.hook_list.hooks.as_slice() {
            [get] => is_stated(get, b"get"),
            [get, set] => is_stated(get, b"get") && is_stated(set, b"set"),
            _ => false,
        }
}

/// Reports an access modifier on an interface property's accessor, as on any interface member. A narrower `set` is
/// the class's: the interface states the property `{ get; }`, and the class gives it with the narrower `set`.
fn report_interface_accessor_access(accessor: &PropertyHook, modifier: &Modifier, context: &mut Context<'_, '_, '_>) {
    let keyword = BytesDisplay(modifier.get_keyword().value);
    let help = if accessor.is_setter() {
        format!("Declare `{{ get; }}` in the interface, and `{{ get; {keyword} set; }}` in the class.")
    } else {
        format!("Remove `{keyword}`.")
    };

    context.report(
        Issue::error("An interface member is always public, so it takes no access modifier.")
            .with_annotation(Annotation::primary(modifier.span()).with_message("Written here."))
            .with_help(help),
    );
}

/// Reports an access modifier on an interface member. A member is public, written bare, as TypeScript and Swift
/// write it.
fn report_interface_member_access(modifier: &Modifier, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("An interface member is always public, so it takes no access modifier.")
            .with_annotation(Annotation::primary(modifier.span()).with_message("Written here."))
            .with_help(format!("Remove `{}`.", BytesDisplay(modifier.get_keyword().value))),
    );
}

/// Reports what the engine refuses in a header when it declares the class: a name the header already holds, and in an
/// enum, `UnitEnum` or `BackedEnum`, which the engine adds to every enum or every backed one. Names compare as PHP's
/// do, resolved and ignoring case. A generic type counts by its name, and `enter` refuses any type in a header that
/// is not a class or a generic class type, `List`, `Map` and `Class` included, which have no resolved name.
fn check_header(inheritance: &Inheritance, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut named: Vec<&[u8]> = Vec::new();
    for name in inheritance.names() {
        let Some(resolved) = context.names.resolve(&name) else {
            continue;
        };
        let issue = if place == Place::Enum
            && (resolved.eq_ignore_ascii_case(b"UnitEnum") || resolved.eq_ignore_ascii_case(b"BackedEnum"))
        {
            Issue::error(
                "Every enum implements `UnitEnum`, and every backed enum `BackedEnum`, so an enum header never names them.",
            )
            .with_annotation(Annotation::primary(name.span()).with_message("Remove this name."))
        } else if named.iter().any(|earlier| earlier.eq_ignore_ascii_case(resolved)) {
            Issue::error(format!("This header names `{}` twice.", BytesDisplay(name.value())))
                .with_annotation(Annotation::primary(name.span()).with_message("Remove this name."))
        } else {
            named.push(resolved);

            continue;
        };

        context.report(issue.with_note("The engine refuses this header when it declares the class."));
    }
}

/// Reports each optional parameter before the last required one, which PHP makes required with a deprecation. A
/// variadic parameter is not a required one, as in PHP.
fn report_optional_before_required(parameters: &FunctionLikeParameterList, context: &mut Context<'_, '_, '_>) {
    let Some(last_required) = parameters
        .parameters
        .iter()
        .rev()
        .find(|parameter| parameter.default_value.is_none() && parameter.ellipsis.is_none())
    else {
        return;
    };

    for parameter in parameters.parameters.iter().take_while(|parameter| parameter.span() != last_required.span()) {
        if parameter.default_value.is_some() {
            context.report(
                Issue::error(format!(
                    "The optional parameter `{}` comes before the required parameter `{}`: PHP would make it required.",
                    BytesDisplay(parameter.variable.name),
                    BytesDisplay(last_required.variable.name)
                ))
                .with_annotation(Annotation::primary(parameter.span()).with_message("Optional here."))
                .with_help("Move the optional parameter after the required ones, or give the required one a default."),
            );
        }
    }
}

/// Whether the slice has a field or a property, with the refusal when it does not. Spec section 6 makes a field
/// storage, which is `private` or `protected`, `private` when it has no modifier, and a property the API, whose
/// accessors `check_accessors` decides.
///
/// A static property has `set`, because PHP has no `readonly` static property.
///
/// An override is a property, spec section 6.1, so a field takes no `override`, and no `virtual` or `abstract` either:
/// a subclass overrides a `protected` field with a property already. An abstract property declares accessors with no
/// body and no initial value. The analyzer checks an override against its parent.
///
/// A `lazy` property is computed, spec section 6.5: `lazy T name => expr;` is its only form.
fn is_slice_property(property: &Property) -> Result<(), Box<Issue>> {
    if property.modifiers().contains_static() {
        if let Property::Hooked(auto_property) = property
            && let Some(init) = auto_property.hook_list.setter().filter(|setter| setter.name.value == b"init")
        {
            return Err(Box::new(
                Issue::error("A static property has no `init`: `init` sets a property while its object is created.")
                    .with_annotation(Annotation::primary(init.span()).with_message("Declared here."))
                    .with_help("Write `set;`, or `private set;` to set it only inside its class."),
            ));
        }
    }

    // Language rule 9: a required property is set where its object is created, so it takes `init` or `set`.
    if let Some(required) = property.modifiers().iter().find(|modifier| matches!(modifier, Modifier::Required(_)))
        && !matches!(property, Property::Hooked(hooked) if hooked.hook_list.setter().is_some())
    {
        let name = property.first_variable().name;

        return Err(Box::new(
            Issue::error(format!(
                "`{}` is required, so it is set when its object is created: give it `init` or `set`.",
                BytesDisplay(name.strip_prefix(b"$").unwrap_or(name))
            ))
            .with_annotation(Annotation::primary(required.span()).with_message("Written here.")),
        ));
    }

    let name = BytesDisplay(property.first_variable().name);
    let modifier = |found: fn(&Modifier) -> bool| property.modifiers().iter().find(|modifier| found(modifier));
    if let Some(lazy) = modifier(|modifier| matches!(modifier, Modifier::Lazy(_)))
        && !matches!(property, Property::Computed(_))
    {
        return Err(Box::new(
            Issue::error(format!(
                "A `lazy` property computes its value on the first read: write `lazy` with `=>`, as in `lazy Plan {name} => this.load();`."
            ))
            .with_annotation(Annotation::primary(lazy.span()).with_message("Written here."))
            .with_note("It takes no accessors and no initial value."),
        ));
    }

    if let Property::Plain(_) = property {
        if let Some(r#override) = modifier(|modifier| matches!(modifier, Modifier::Override(_))) {
            return Err(Box::new(
                Issue::error(format!(
                    "An override is a property: give `{name}` accessors, as in `{name} {{ get; set; }}`."
                ))
                .with_annotation(Annotation::primary(r#override.span()).with_message("A field written `override`."))
                .with_note(
                    "PHP# has no public fields, and a subclass replaces a parent's property or field with a property.",
                ),
            ));
        }
        if let Some(r#virtual) = modifier(|modifier| matches!(modifier, Modifier::Virtual(_))) {
            return Err(Box::new(
                Issue::error(
                    "A field takes no `virtual`: a subclass overrides a `protected` field with a property already.",
                )
                .with_annotation(Annotation::primary(r#virtual.span()).with_message("Written here.")),
            ));
        }
        if let Some(r#abstract) = modifier(|modifier| matches!(modifier, Modifier::Abstract(_))) {
            return Err(Box::new(
                Issue::error(
                    "A field cannot be `abstract`: declare an abstract property, as in `public abstract int total { get; }`.",
                )
                .with_annotation(Annotation::primary(r#abstract.span()).with_message("Written here.")),
            ));
        }
    } else if let Some(r#abstract) = modifier(|modifier| matches!(modifier, Modifier::Abstract(_))) {
        let has_body = match property {
            Property::Hooked(hooked) => {
                hooked.hook_list.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
            }
            _ => true,
        };
        if has_body {
            return Err(Box::new(
                Issue::error("An abstract property has no body: write its accessors with none, as in `{ get; }`.")
                    .with_annotation(Annotation::primary(r#abstract.span()).with_message("Declared `abstract` here.")),
            ));
        }
        if property.initial_value().is_some() {
            return Err(Box::new(
                Issue::error("An abstract property has no initial value: the class that implements it gives one.")
                    .with_annotation(Annotation::primary(r#abstract.span()).with_message("Declared `abstract` here.")),
            ));
        }
    }

    match property {
        Property::Plain(field) => {
            if let Some(access) =
                field.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Internal(_)))
            {
                let level = BytesDisplay(access.get_keyword().value);

                Err(Box::new(
                    Issue::error(format!("A PHP# field cannot be `{level}`: a field is `private` or `protected`."))
                        .with_annotation(
                            Annotation::primary(access.span()).with_message(format!("Declared `{level}` here.")),
                        )
                        .with_help(format!(
                            "Declare a property, as in `{level} int views {{ get; set; }}`, to make it {level}."
                        )),
                ))
            } else if field.items.len() > 1 {
                Err(Box::new(several_names(field.span(), "field")))
            } else {
                Ok(())
            }
        }
        Property::Hooked(auto_property) => {
            if let PropertyItem::Concrete(item) = &auto_property.item {
                Err(Box::new(
                    Issue::error(
                        "A property's initial value comes last: write it after the accessors, as in `public int views { get; set; } = 0;`.",
                    )
                    .with_annotation(Annotation::primary(item.span()).with_message("Written here.")),
                ))
            } else {
                Ok(())
            }
        }
        Property::Computed(_) => Ok(()),
    }
}

/// The error for a field or a constant, the `kind`, at `span` that declares several names. One member per declaration
/// keeps a member's type, initial value and attributes in one place.
fn several_names(span: Span, kind: &str) -> Issue {
    Issue::error(format!("A {kind} declares one name: declare each {kind} on its own line."))
        .with_annotation(Annotation::primary(span).with_message("Several names here."))
}

/// Reports a static field or a static property with storage that has no initial value, spec section 6. No constructor
/// sets a static, so it would be read before anything set it, and a `T?` one has no implicit `null` either.
fn report_static_without_initial_value(property: &Property, context: &mut Context<'_, '_, '_>) {
    // An untyped member is the parser's error.
    if property.modifiers().get_static().is_none()
        || property.initial_value().is_some()
        || property.hint().is_none()
        || matches!(property, Property::Computed(_))
        || matches!(property, Property::Hooked(hooked) if !context.names.has_storage(&hooked.hook_list))
    {
        return;
    }

    let variable = property.first_variable();
    context.report(
        Issue::error(format!("Give `{}` an initial value: no constructor sets a static.", BytesDisplay(variable.name)))
            .with_annotation(Annotation::primary(variable.span).with_message("Declared without a value here.")),
    );
}

/// Reports each method named as a property of its class, compared ignoring case as PHP finds a method. Spec section 14
/// calls a property holding a function as `order.priceOf(line)`, and the engine calls the property only when the
/// class has no method of that name, so one name declares one member, as in C#.
fn report_methods_named_as_properties(class: ClassLike, context: &mut Context<'_, '_, '_>) {
    let mut properties: Vec<&DirectVariable> = Vec::new();
    for member in class.members {
        match member {
            ClassLikeMember::Property(property) => properties.extend(property.variables()),
            ClassLikeMember::Method(method) => properties.extend(
                method
                    .parameter_list
                    .parameters
                    .iter()
                    .filter(|parameter| parameter.is_promoted_property())
                    .map(|parameter| &parameter.variable),
            ),
            _ => {}
        }
    }

    for member in class.members {
        let ClassLikeMember::Method(method) = member else {
            continue;
        };
        // A constructor is written with its class's name, and PHP names it `__construct`, so it shares no name.
        if php_method_name(method) == b"__construct" {
            continue;
        }

        if let Some(property) = properties.iter().find(|property| property.name.eq_ignore_ascii_case(method.name.value))
        {
            context.report(
                Issue::error(format!(
                    "The class `{}` declares a method and a property named `{}`.",
                    BytesDisplay(class.name.value),
                    BytesDisplay(method.name.value),
                ))
                .with_annotation(Annotation::primary(method.name.span).with_message("The method is declared here."))
                .with_annotation(Annotation::secondary(property.span).with_message("The property is declared here."))
                .with_help("Rename one of them: `x.name(…)` calls the method, or the function the property holds."),
            );
        }
    }
}

/// Whether the slice has a class constant, with the error when it does not: a constant declares one name.
fn is_slice_constant(constant: &ClassLikeConstant) -> Result<(), Box<Issue>> {
    if constant.items.len() > 1 { Err(Box::new(several_names(constant.span(), "constant"))) } else { Ok(()) }
}

/// Checks the accessors of a property, declared in the class body or on a constructor parameter: `get` once, and an
/// optional `set` or `init` once, which may take an access modifier narrower than the property's, as in C#. `init`
/// sets the property only while its object is created, spec section 6.1. Each is `;`, an expression body `=> expr;`
/// or a block. Only `set` and `init` carry an access level, so one on `get` is an error, and `report_accessor` names
/// what to write for any other accessor `is_slice_accessor` refuses.
fn check_accessors(
    modifiers: &Sequence<Modifier>,
    accessors: &PropertyHookList,
    variable: &DirectVariable,
    context: &mut Context<'_, '_, '_>,
) {
    let access = modifiers.iter().find(|modifier| {
        matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_))
    });
    let mut declared: Vec<&[u8]> = Vec::new();

    for accessor in &accessors.hooks {
        let name = accessor.name.value;
        if let [level @ (Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_))] =
            accessor.modifiers.as_slice()
            && name == b"get"
        {
            let property = BytesDisplay(variable.name);
            context.report(
                Issue::error(format!(
                    "Only `set` and `init` take an access level: `get` reads `{property}` at the property's own level."
                ))
                .with_annotation(Annotation::primary(level.span()).with_message("Written here."))
                .with_help(format!(
                    "Write `get;`, and to read `{property}` only inside the class, write a method that reads it there, such as a `private` method that returns `this.{property}`."
                )),
            );
        } else if !is_slice_accessor(accessor) {
            report_accessor(accessor, context);
        } else if accessor.is_observer()
            && let (Some(first), Some(last)) = (accessor.attribute_lists.first(), accessor.attribute_lists.last())
        {
            let observer = BytesDisplay(name);
            context.report(
                Issue::error(format!(
                    "`{observer}` runs inside the `set` accessor, so it has no place of its own for an attribute."
                ))
                .with_annotation(Annotation::primary(first.span().join(last.span())).with_message("Written here."))
                .with_help("Put the attribute on `set`."),
            );
        } else if accessor.is_observer() && !accessors.hooks.iter().any(|hook| hook.name.value == b"set") {
            let observer = BytesDisplay(name);
            context.report(
                Issue::error(format!("`{observer}` needs a `set` accessor: it runs on each write through `set`."))
                    .with_annotation(Annotation::primary(accessor.name.span).with_message("Declared here."))
                    .with_help(format!(
                        "Write `set;` beside it, as in `public string name {{ get; set; {observer} => this.touch(); }} = \"\";`."
                    )),
            );
        } else if declared.contains(&name) {
            context.report(
                Issue::error("A PHP# property declares each accessor once.")
                    .with_annotation(Annotation::primary(accessor.name.span).with_message("Declared again here.")),
            );
        } else if accessor.is_setter() && declared.iter().any(|declared| *declared == b"set" || *declared == b"init") {
            context.report(
                Issue::error("A PHP# property is written by `set` or by `init`, not both.")
                    .with_annotation(Annotation::primary(accessor.name.span).with_message("Declared here."))
                    .with_help(
                        "Keep `init` to set it only while the object is created, or `set` to set it at any time.",
                    ),
            );
        } else if accessor.modifiers.first().is_some_and(|modifier| !is_narrower(modifier, access)) {
            let setter = BytesDisplay(name);

            context.report(
                Issue::error(format!("The `{setter}` accessor of a PHP# property must be narrower than the property."))
                    .with_annotation(Annotation::primary(accessor.span()).with_message("Declared here."))
                    .with_help(format!(
                        "Write `private {setter};`, or `protected {setter};` or `internal {setter};` under a public property, narrower than the property, or `{setter};`."
                    )),
            );
        }

        declared.push(name);
    }

    if !declared.contains(&b"get".as_slice()) {
        context.report(
            Issue::error("A PHP# property needs a `get` accessor.")
                .with_annotation(Annotation::primary(variable.span).with_message("Declared here."))
                .with_help("Add `get;`, as in `public int views { get; set; }`."),
        );
    }
}

/// Whether the slice has an accessor: `get`, `set` or `init`, or an observer with a body, `willSet` or `didSet`
/// (spec section 6.2), with optional attributes and without `&` or a parameter list, where only `set` and `init` take an access
/// modifier, `private`, `protected`, `internal` or `public`, which `check_accessors` compares with the property's.
fn is_slice_accessor(accessor: &PropertyHook) -> bool {
    accessor.ampersand.is_none()
        && accessor.parameter_list.is_none()
        && match accessor.modifiers.as_slice() {
            [] if accessor.is_observer() => matches!(accessor.body, PropertyHookBody::Concrete(_)),
            [] => accessor.name.value == b"get" || accessor.is_setter(),
            [Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_)] => {
                accessor.is_setter()
            }
            _ => false,
        }
}

/// Reports an accessor `is_slice_accessor` refuses, naming what PHP# writes instead. PHP's `&get` and an accessor of any
/// other name are no PHP# syntax.
fn report_accessor(accessor: &PropertyHook, context: &mut Context<'_, '_, '_>) {
    const ACCESSORS: &str = "A PHP# property's accessors are `get` and `set` or `init`, each with optional attributes and written `;`, `=> expr;` or with a block body, and `set` and `init` may take `private`, `protected` or `internal`. The observers `willSet` and `didSet` take a body.";

    let name = BytesDisplay(accessor.name.value);
    let other_modifier = accessor.modifiers.iter().find(|modifier| {
        !matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_))
    });
    let message = if let Some(ampersand) = accessor.ampersand {
        return report_unexpected(ampersand, context);
    } else if accessor.parameter_list.is_some() && accessor.is_setter() {
        format!("`{name}` takes no parameter list: its body reads the incoming value as `value`.")
    } else if accessor.parameter_list.is_some() {
        format!("`{name}` takes no parameter list.")
    } else if let Some(modifier) = other_modifier {
        format!("An accessor takes no `{}`: remove it.", BytesDisplay(modifier.get_keyword().value))
    } else if accessor.is_observer() && !accessor.modifiers.is_empty() {
        format!("`{name}` takes no access level: it runs inside `set`.")
    } else if accessor.is_observer() {
        format!("`{name}` needs a body, as in `{name} => this.log(field);`.")
    } else {
        return report_unexpected(accessor.name.span, context);
    };

    context.report(
        Issue::error(message)
            .with_annotation(Annotation::primary(accessor.span()).with_message("Written here."))
            .with_note(ACCESSORS),
    );
}

/// Spec section 6.4: a behavior runs around a property's storage, once per object, so `via` goes on an instance
/// auto-property with `get; set;` and no accessor body, which a class declares, never an interface. Each behavior is
/// a class PHP# creates with `new`, so it names one, with its type arguments when it is generic. The property, at
/// `span`, is declared in the class body or on a constructor parameter, with `modifiers` and `accessors`.
fn check_behaviors(
    modifiers: &Sequence<Modifier>,
    accessors: &PropertyHookList,
    span: Span,
    behaviors: &PropertyBehaviorList,
    context: &mut Context<'_, '_, '_>,
) {
    for behavior in behaviors.behaviors.iter() {
        let names_class = match behavior {
            Hint::Identifier(_) => true,
            Hint::Generic(generic) => built_in_generic_arity(generic.name.value).is_none(),
            _ => false,
        };
        if !names_class {
            context.report(
                Issue::error(format!(
                    "`{}` is no class: `via` names a behavior class, as `via Trimmed` or `via Logged<string>`.",
                    BytesDisplay(context.get_code_snippet(behavior))
                ))
                .with_annotation(Annotation::primary(behavior.span()).with_message("Named here.")),
            );
        }
    }

    let accessors = &accessors.hooks;
    let is_auto = |name: &[u8]| {
        accessors
            .iter()
            .any(|accessor| accessor.name.value == name && matches!(accessor.body, PropertyHookBody::Abstract(_)))
    };
    let refusal = if is_in_interface(context.program, span) || modifiers.contains_abstract() {
        Some((
            "A property without storage takes no `via`: the class that stores the property names its behaviors.",
            None,
        ))
    } else if modifiers.contains_static() {
        Some(("A static property takes no `via`: each object keeps its own behaviors.", None))
    } else if accessors.len() != 2 || !is_auto(b"get") || !is_auto(b"set") {
        Some((
            "`via` goes on an auto-property with `get; set;`: its behaviors run around the storage.",
            Some("Write the change in a behavior's `get` or `set`, or drop `via` and write an accessor body."),
        ))
    } else {
        None
    };
    let Some((message, help)) = refusal else {
        return;
    };

    let mut issue =
        Issue::error(message).with_annotation(Annotation::primary(behaviors.via).with_message("Written here."));
    if let Some(help) = help {
        issue = issue.with_help(help);
    }
    context.report(issue);
}

/// Reports a property without storage that has an initial value.
fn report_no_storage(property: &DirectVariable, span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!(
            "The property `{}` has no storage for its initial value: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
            BytesDisplay(property.name)
        ))
        .with_annotation(Annotation::primary(span).with_message("Set here."))
        .with_note("Its accessors compute every read and write, as PHP's virtual property does."),
    );
}

/// Checks the bodies of a property's accessors: the returns PHP's hooks allow, `field` in a lambda, and the property
/// read through `this` inside its own accessor.
fn check_accessor_bodies(accessors: &PropertyHookList, property: &[u8], context: &mut Context<'_, '_, '_>) {
    let observed_after = accessors.hooks.iter().any(|accessor| accessor.name.value == b"didSet");
    for accessor in accessors.hooks.iter().filter(|accessor| is_slice_accessor(accessor)) {
        if let PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block)) = &accessor.body {
            check_accessor_returns(accessor.name.value, observed_after, block, context);
        }

        check_accessor_node(Node::PropertyHookBody(&accessor.body), property, false, context);
        if accessor.name.value == b"willSet" {
            check_will_set(Node::PropertyHookBody(&accessor.body), accessor.name.span, context);
        }
    }
}

/// php-src gives a `get` hook the property's type and a `set` hook `void`, so a `get` block returns a value and a
/// `set` or `init` block, which runs as the `set` hook, returns none. The observers run inside that `set` hook, before
/// and after the write (spec section 6.2), so `willSet` cannot return, as that would stop the write, and neither can
/// a `set` body that `didSet`, `observed_after`, runs after. A lambda's returns are its own.
fn check_accessor_returns(accessor: &[u8], observed_after: bool, block: &Block, context: &mut Context<'_, '_, '_>) {
    for r#return in mago_syntax::utils::find_returns_in_block(block) {
        match (accessor, &r#return.value) {
            (b"willSet", _) => context.report(
                Issue::error("`willSet` cannot return: the write runs after it.")
                    .with_annotation(Annotation::primary(r#return.span()).with_message("Returns here."))
                    .with_help("Let the body run to its end. To refuse a value, throw instead."),
            ),
            (b"set", _) if observed_after => context.report(
                Issue::error("A `set` accessor beside `didSet` cannot return: `didSet` runs after it.")
                    .with_annotation(Annotation::primary(r#return.span()).with_message("Returns here."))
                    .with_help("Let the body run to its end, so `didSet` runs after each write."),
            ),
            (b"didSet", Some(value)) => context.report(
                Issue::error("`didSet` must not return a value.")
                    .with_annotation(Annotation::primary(value.span()).with_message("Returned here."))
                    .with_note("It runs inside the `set` hook, which PHP gives the return type `void`."),
            ),
            (b"get", None) => context.report(
                Issue::error("A `get` accessor must return a value.")
                    .with_annotation(Annotation::primary(r#return.span()).with_message("Returns no value."))
                    .with_note("PHP gives a `get` hook the property's type.")
                    .with_help("Return the property's value, such as `return field;`."),
            ),
            (b"set", Some(value)) | (b"init", Some(value)) => context.report(
                Issue::error(if accessor == b"init" {
                    "An `init` accessor must not return a value."
                } else {
                    "A `set` accessor must not return a value."
                })
                .with_annotation(Annotation::primary(value.span()).with_message("Returned here."))
                .with_note("PHP gives a `set` hook the return type `void`.")
                .with_help("Write the value with `field = value;`, then `return;` if the body ends early."),
            ),
            _ => {}
        }
    }
}

/// A lambda runs as a function of its own, so `$this->name` there calls the accessor again, in PHP as in C#. In the
/// accessor itself PHP reads and writes the storage through `$this->name` and `$this?->name`, where C# calls the
/// accessor again, so the accessor writes `field`, and a lambda cannot.
fn check_accessor_node(node: Node<'_, '_>, property: &[u8], in_lambda: bool, context: &mut Context<'_, '_, '_>) {
    let in_lambda = in_lambda || matches!(node, Node::ArrowFunction(_) | Node::Closure(_));
    let member = match node {
        Node::PropertyAccess(access) => Some((access.object, &access.property)),
        Node::NullSafePropertyAccess(access) => Some((access.object, &access.property)),
        _ => None,
    };

    match node {
        Node::ConstantAccess(access) if in_lambda && context.names.binding(&access.name) == Some(Binding::Field) => {
            context.report(
                Issue::error(
                    "`field` cannot be used in a lambda: PHP would call the accessor again instead of reading the storage.",
                )
                .with_annotation(Annotation::primary(access.span()).with_message("Used here."))
                .with_help("Copy `field` into a local before the lambda, and use the local inside it."),
            );
        }
        _ if !in_lambda
            && let Some((object, selector)) = member
            && matches!(object.unparenthesized(), Expression::ConstantAccess(object) if context.names.binding(&object.name) == Some(Binding::This))
            && matches!(selector, ClassLikeMemberSelector::Identifier(member) if member.value == property) =>
        {
            let property = BytesDisplay(property);
            context.report(
                Issue::error(format!("Write `field` instead of `this.{property}` inside `{property}`'s own accessor."))
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                    .with_note("PHP reads and writes the storage there, while C# would call the accessor again."),
            );
        }
        _ => {}
    }

    ensure_sufficient_stack(|| node.visit_children(|child| check_accessor_node(child, property, in_lambda, context)));
}

/// Spec section 6.2: `willSet` runs before the write and cannot change it, so it never writes `value`, the local its
/// name declares at `value`.
fn check_will_set(node: Node<'_, '_>, value: Span, context: &mut Context<'_, '_, '_>) {
    let target = match node {
        Node::Assignment(assignment) => Some(assignment.lhs),
        Node::UnaryPrefix(UnaryPrefix {
            operator: UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_),
            operand,
        })
        | Node::UnaryPostfix(UnaryPostfix { operand, .. }) => Some(*operand),
        _ => None,
    };

    if let Some(Expression::ConstantAccess(target)) = target
        && matches!(context.names.binding(&target.name), Some(Binding::Local(local)) if local.declaration == value)
    {
        context.report(
            Issue::error("`willSet` cannot change `value`: the write after it stores `value` as written.")
                .with_annotation(Annotation::primary(target.span()).with_message("Changed here."))
                .with_help("Change the value in a `set` body, as in `set => field = value.trim();`."),
        );
    }

    ensure_sufficient_stack(|| node.visit_children(|child| check_will_set(child, value, context)));
}

/// Reports each `this` in an initial value. A constant initial value is the member's default, and any other runs at
/// the start of the constructor in declaration order, before the members declared after it are set, so it cannot read
/// the object, as in C#. Returns whether it reported one.
fn report_this_in_initial_value(node: Node<'_, '_>, context: &mut Context<'_, '_, '_>) -> bool {
    report_this(
        node,
        "An initial value cannot use `this`: it runs before the constructor body, while the object is built.",
        Some("Set the member in the constructor body instead."),
        context,
    )
}

/// Reports each `this` in the arguments of `: super(…)` or `: this(…)`. They run before the object exists, as C#'s
/// CS0027 says: `: this(…)` builds the object, and `: super(…)` runs before the class's own constructor body.
fn report_this_in_initializer(initializer: &ConstructorInitializer, context: &mut Context<'_, '_, '_>) {
    report_this(
        Node::ArgumentList(&initializer.argument_list),
        "`this` is not available before the constructor runs: pass a parameter instead.",
        None,
        context,
    );
}

/// Whether `node`, the value of a pattern, reads a local, a parameter, `this` or a bare member. Spec section 21 takes
/// types and constants only in a pattern. The analyzer reports a local, a parameter and `this` with the `when` that
/// compares it, typed as the value it compares, and `check_constant_access` reports a bare member, which says to write
/// `this.`. A lambda reads its own parameters, and `report_lambda_capture` reports what it captures.
fn reads_code(node: Node<'_, '_>, context: &Context<'_, '_, '_>) -> bool {
    match node {
        Node::Identifier(name)
            if matches!(context.names.binding(name), Some(Binding::Local(_) | Binding::This | Binding::Member)) =>
        {
            return true;
        }
        Node::Expression(Expression::ArrowFunction(_) | Expression::Closure(_)) => return false,
        _ => {}
    }

    let mut reads = false;
    ensure_sufficient_stack(|| node.visit_children(|child| reads |= reads_code(child, context)));

    reads
}

/// Reports each `this` in `node` with `message` and `help`, and returns whether it reported one.
fn report_this(node: Node<'_, '_>, message: &str, help: Option<&str>, context: &mut Context<'_, '_, '_>) -> bool {
    let mut reported = false;
    if let Node::ConstantAccess(access) = node
        && context.names.binding(&access.name) == Some(Binding::This)
    {
        let issue =
            Issue::error(message).with_annotation(Annotation::primary(access.span()).with_message("Used here."));
        context.report(match help {
            Some(help) => issue.with_help(help),
            None => issue,
        });
        reported = true;
    }

    ensure_sufficient_stack(|| node.visit_children(|child| reported |= report_this(child, message, help, context)));

    reported
}

/// Reports each local of the code around a lambda in a constant value that the lambda captures, and each `this` in it,
/// since a constant's value is known before that code runs. Returns whether it reported one.
fn report_lambda_capture(lambda: &Expression, context: &mut Context<'_, '_, '_>) -> bool {
    let locals: Vec<String> =
        context.names.captures(&lambda.span()).iter().map(|(name, _)| format!("`{}`", BytesDisplay(name))).collect();
    if !locals.is_empty() {
        context.report(
            Issue::error(format!(
                "The lambda captures {}, and a lambda in a constant value captures nothing.",
                locals.join(" and ")
            ))
            .with_annotation(Annotation::primary(lambda.span()).with_message("Captured here."))
            .with_help("Take each value the lambda uses as a parameter."),
        );
    }

    let this = report_this(
        Node::Expression(lambda),
        "The lambda captures `this`, and a lambda in a constant value captures nothing.",
        Some("Take the object as a parameter."),
        context,
    );

    !locals.is_empty() || this
}

/// Reports the expression at `call` in a constant value, which runs code where the value is known before the code runs,
/// as a call, a `new`, a property read and `this` do: a static field holds a value code computes, and a pattern's `when`
/// compares with one. An attribute argument names what it takes in `report_attribute_argument`.
fn report_call_in_constant(call: Span, context: &mut Context<'_, '_, '_>) {
    if is_attribute_argument(context) {
        return report_attribute_argument(call, context);
    }
    let written = String::from_utf8_lossy(context.get_code_snippet(call)).into_owned();
    let instead = if context.slice_places.contains(&Some(Place::Pattern)) {
        "compare with it in `when`."
    } else {
        "use a static field."
    };

    context.report(
        Issue::error(format!("A constant's value is known before the code runs, and `{written}` runs code: {instead}"))
            .with_annotation(Annotation::primary(call).with_message("Runs code here.")),
    );
}

/// How far a visibility modifier reaches: `private` least, then `protected`, then `public`.
/// Whether the access modifier `setter` reaches strictly less than `property`, the property's own access modifier, as
/// C# requires of an accessor's. `internal` and `protected` reach different code, a namespace and the subclasses, so
/// neither is narrower than the other, and `private` is narrower than both.
const fn is_narrower(setter: &Modifier, property: Option<&Modifier>) -> bool {
    matches!(
        (property, setter),
        (Some(Modifier::Public(_)), Modifier::Internal(_) | Modifier::Protected(_) | Modifier::Private(_))
            | (Some(Modifier::Internal(_) | Modifier::Protected(_)), Modifier::Private(_))
    )
}

/// Whether the slice has a namespace: the file's first, with a name and no braces.
fn is_slice_namespace(namespace: &Namespace, program: &Program) -> bool {
    namespace.name.is_some()
        && matches!(namespace.body, NamespaceBody::Implicit(_))
        && first_namespace(program).is_some_and(|first| first.span() == namespace.span())
}

/// The first namespace of a PHP# file, the only one the slice has.
fn first_namespace<'ast, 'arena>(program: &'ast Program<'arena>) -> Option<&'ast Namespace<'arena>> {
    program.statements.iter().find_map(|statement| match statement {
        Statement::Namespace(namespace) => Some(namespace),
        _ => None,
    })
}

/// Whether a PHP# file's namespace is `Sharp` or below it, compared ignoring case as PHP compares namespaces. The
/// engine compiles the standard library from `vendor/` knowing only the file, so its namespace is all the slice knows
/// of the library.
fn is_standard_library_namespace(program: &Program) -> bool {
    first_namespace(program).and_then(|namespace| namespace.name.as_ref()).map(php_name).is_some_and(|name| {
        name.get(..5).is_some_and(|root| root.eq_ignore_ascii_case(b"Sharp"))
            && matches!(name.get(5), None | Some(b'\\'))
    })
}

/// The statements at file level of a PHP# file, and those of each of its namespaces, the ones the slice refuses too.
fn file_statements<'ast, 'arena>(program: &'ast Program<'arena>) -> impl Iterator<Item = &'ast Statement<'arena>> {
    program.statements.iter().flat_map(|statement| match statement {
        Statement::Namespace(namespace) => namespace.statements().as_slice(),
        statement => std::slice::from_ref(statement),
    })
}

/// Checks a ternary. PHP#'s ternary is `c ? a : b`, and PHP's `a ?: b` does not exist, as spec section 21 says, so `?:`
/// is no PHP# syntax, at its `?`. The parser reads `a ? b : c ? d : e` as `a ? b : (c ? d : e)`, so a ternary is the
/// condition of another only in parentheses.
fn check_conditional(conditional: &Conditional, context: &mut Context<'_, '_, '_>) {
    if conditional.then.is_none() {
        report_unexpected(conditional.question_mark, context);
    }
}

/// The row of spec section 19's precedence table a comparison or an equality sits on, with the operand it tests and,
/// for a binary operator, the operand it tests against. `is` and `as` test against a pattern and a type, which hold
/// no comparison of their own.
fn comparison_operands<'ast, 'arena>(
    expression: &'ast Expression<'arena>,
) -> Option<(Precedence, &'ast Expression<'arena>, Option<&'ast Expression<'arena>>)> {
    let (row, tested, against) = match expression {
        Expression::Binary(binary) => (binary.operator.precedence(), binary.lhs, Some(binary.rhs)),
        Expression::Is(is) => (Precedence::Comparison, is.value, None),
        Expression::As(r#as) => (Precedence::Comparison, r#as.value, None),
        _ => return None,
    };

    matches!(row, Precedence::Comparison | Precedence::Equality).then_some((row, tested, against))
}

/// The groupings to choose from when a comparison or an equality takes an operand of its own row without parentheses,
/// which spec section 19 makes an error, as in `a < b < c`: the first operator grouped first, then the second. A
/// grouping that would put a value inside a pattern or a type, as `a is (T is U)` would, is left out.
fn comparison_chain(expression: &Expression, context: &Context<'_, '_, '_>) -> Option<Vec<String>> {
    let (row, tested, against) = comparison_operands(expression)?;
    let span = expression.span();
    let written = |span: Span| BytesDisplay(context.get_code_snippet(span)).to_string();

    if let Some(against) = against
        && let Some((inner_row, inner_tested, _)) = comparison_operands(against)
        && inner_row == row
    {
        let middle = inner_tested.end_position();

        return Some(vec![
            format!("({}){}", written(span.to_end(middle)), written(span.from_start(middle))),
            format!("{}({})", written(span.to_end(against.start_position())), written(against.span())),
        ]);
    }

    let (inner_row, _, inner_against) = comparison_operands(tested)?;
    if inner_row != row {
        return None;
    }

    let grouped = format!("({}){}", written(tested.span()), written(span.from_start(tested.end_position())));
    let other = inner_against.map(|inner_against| {
        let middle = inner_against.start_position();

        format!("{}({})", written(span.to_end(middle)), written(span.from_start(middle)))
    });

    Some(std::iter::once(grouped).chain(other).collect())
}

/// Reports a test whose pattern holds a comparison in the value of a comparison or value pattern, as `x is > 1 < 2`,
/// which chains because the value reads up to the comparison row. The grouping is the whole test as written, each such
/// value in parentheses. Returns whether it reported.
fn report_chained_pattern(test: Span, pattern: &Pattern, context: &mut Context<'_, '_, '_>) -> bool {
    fn chained_values<'ast, 'arena>(pattern: &'ast Pattern<'arena>, values: &mut Vec<&'ast Expression<'arena>>) {
        match pattern {
            Pattern::Comparison(ComparisonPattern { value, .. }) | Pattern::Value(value) => {
                if comparison_operands(value).is_some() {
                    values.push(value);
                }
            }
            Pattern::Type(TypePattern { fields, properties, .. }) => {
                for field in fields.iter().flat_map(|fields| fields.fields.iter()) {
                    chained_values(field, values);
                }
                for property in properties.iter().flat_map(|properties| properties.properties.iter()) {
                    chained_values(property.pattern, values);
                }
            }
            Pattern::Properties(properties) => {
                for property in &properties.properties {
                    chained_values(property.pattern, values);
                }
            }
            Pattern::Not(not) => chained_values(not.pattern, values),
            Pattern::Binary(binary) => {
                chained_values(binary.left, values);
                chained_values(binary.right, values);
            }
            Pattern::Parenthesized(parenthesized) => chained_values(parenthesized.pattern, values),
            Pattern::List(list) => {
                for element in &list.elements {
                    if let ListPatternElement::Pattern(pattern) = element {
                        chained_values(pattern, values);
                    }
                }
            }
        }
    }

    let mut values = Vec::new();
    chained_values(pattern, &mut values);
    if values.is_empty() {
        return false;
    }

    let written = |span: Span| BytesDisplay(context.get_code_snippet(span)).to_string();
    let mut grouping = String::new();
    let mut written_to = test.start;
    for value in values {
        grouping.push_str(&written(Span::new(test.file_id, written_to, value.start_position())));
        grouping.push('(');
        grouping.push_str(&written(value.span()));
        grouping.push(')');
        written_to = value.end_position();
    }
    grouping.push_str(&written(test.from_start(written_to)));
    report_comparison_chain(test, &[grouping], context);

    true
}

fn report_comparison_chain(span: Span, groupings: &[String], context: &mut Context<'_, '_, '_>) {
    let groupings: Vec<String> = groupings.iter().map(|grouping| format!("`{grouping}`")).collect();

    context.report(
        Issue::error(format!("Comparisons do not chain: write {}.", groupings.join(" or ")))
            .with_annotation(Annotation::primary(span).with_message("Chained here."))
            .with_note("PHP#'s precedence table puts `<`, `<=`, `>`, `>=`, `is` and `as` in one row and `==`, `!=`, `===` and `<=>` in the next, and an operator takes an operand of its own row only in parentheses."),
    );
}

/// Reports `not` beside `or` in a pattern, which decision 045 makes an error, naming both readings: `not` over the
/// whole `or`, and each `not` over its own side.
fn report_not_beside_or(binary: &BinaryPattern, context: &mut Context<'_, '_, '_>) {
    let written = |pattern: &Pattern| BytesDisplay(context.get_code_snippet(pattern)).to_string();
    let (whole, own): (Vec<String>, Vec<String>) = [binary.left, binary.right]
        .into_iter()
        .map(|side| match side {
            Pattern::Not(not) => (written(not.pattern), format!("({})", written(side))),
            _ => (written(side), written(side)),
        })
        .unzip();
    let or = BytesDisplay(binary.operator.value);

    context.report(
        Issue::error(format!("Write `not ({} {or} {})`, or `{} {or} {}`.", whole[0], whole[1], own[0], own[1]))
            .with_annotation(Annotation::primary(binary.span()).with_message("`not` beside `or`."))
            .with_note("In a pattern, `not` beside `or` needs parentheses. C# reads `not Paid or Refunded` as `(not Paid) or Refunded`, where `or Refunded` adds nothing."),
    );
}

/// Reports an expression statement whose top operator is a binary one other than `??`, without an assignment for an
/// operand: its result is never used. An arithmetic or bitwise operator on a write target names its compound
/// assignment.
fn report_no_effect(binary: &Binary, context: &mut Context<'_, '_, '_>) {
    let fix =
        if (binary.operator.is_arithmetic() || binary.operator.is_bitwise()) && is_slice_target(binary.lhs, context) {
            format!(
                "write `{} {}= {}` to keep its result",
                BytesDisplay(context.get_code_snippet(binary.lhs)),
                BytesDisplay(binary.operator.as_bytes()),
                BytesDisplay(context.get_code_snippet(binary.rhs))
            )
        } else {
            format!("use the result of `{}`, or remove the statement", BytesDisplay(context.get_code_snippet(binary)))
        };

    context.report(
        Issue::error(format!("This statement has no effect: {fix}."))
            .with_annotation(Annotation::primary(binary.span()).with_message("Its result is never used.")),
    );
}

/// The expression a node writes: the left side of an assignment, or the operand of `++` or `--`.
fn write_target<'ast, 'arena>(node: Node<'ast, 'arena>) -> Option<&'ast Expression<'arena>> {
    match node {
        Node::Assignment(assignment) => Some(assignment.lhs),
        Node::UnaryPrefix(unary_prefix) if unary_prefix.operator.is_increment_or_decrement() => {
            Some(unary_prefix.operand)
        }
        Node::UnaryPostfix(unary_postfix) => Some(unary_postfix.operand),
        _ => None,
    }
}

/// Whether a statement runs code, as a method body's statements do, rather than declaring something. PHP's `foreach`
/// is a parse error in a `.sharp` file, and `unset(…)` is a call there.
const fn runs_code(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Block(_)
            | Statement::Goto(_)
            | Statement::Label(_)
            | Statement::Try(_)
            | Statement::For(_)
            | Statement::ForOf(_)
            | Statement::While(_)
            | Statement::DoWhile(_)
            | Statement::Continue(_)
            | Statement::Break(_)
            | Statement::Switch(_)
            | Statement::If(_)
            | Statement::Return(_)
            | Statement::Expression(_)
            | Statement::Echo(_)
            | Statement::Emit(_)
            | Statement::Global(_)
            | Statement::Static(_)
            | Statement::LocalDeclaration(_)
            | Statement::PatternMatch(_)
            | Statement::Noop(_)
    )
}

/// Whether the walk is inside a statement at file level that runs code, which the file-level rule refuses whole, so
/// a refusal of a part of it would report the statement twice.
fn is_in_file_level_code(context: &Context<'_, '_, '_>) -> bool {
    let mut ancestors =
        context.ancestors.iter().skip_while(|node| matches!(node, Node::Statement(Statement::Namespace(_))));

    matches!(ancestors.next(), Some(Node::Statement(statement)) if runs_code(statement))
}

/// Reports each `break` and `continue`, plain or after `??`, that leaves the `finally` block `finally`: one that no loop
/// inside the block holds, outside the lambdas and classes the block declares. A `finally` block runs while its `try`
/// statement leaves, so it cannot jump out, as in C# (CS0157), and the engine refuses the jump when it loads the file.
fn report_jumps_out_of_finally(finally: &TryFinallyClause<'_>, context: &mut Context<'_, '_, '_>) {
    let mut jumps = FinallyJumps::default();
    FinallyJumpWalker.walk_block(&finally.block, &mut jumps);

    for (span, jump) in jumps.leaving {
        context.report(
            Issue::error(format!("`{jump}` cannot leave a `finally` block."))
                .with_annotation(Annotation::primary(span).with_message("Leaves the `finally` block."))
                .with_note("A `finally` block runs while its `try` statement leaves, so it cannot jump out, as in C#.")
                .with_help("Set a local in the `finally` block, and test it after the `try` statement."),
        );
    }
}

/// The jumps of a `finally` block that leave it, with how each is written, and how many loops, lambdas and classes
/// inside the block hold the node the walk is at.
#[derive(Default)]
struct FinallyJumps {
    inner: usize,
    leaving: Vec<(Span, &'static str)>,
}

/// Collects the `FinallyJumps` of a `finally` block.
struct FinallyJumpWalker;

impl FinallyJumpWalker {
    /// Whether `node` holds its own `break` and `continue`: a loop, a lambda or a class.
    fn holds_jumps(node: Node<'_, '_>) -> bool {
        matches!(
            node,
            Node::While(_)
                | Node::DoWhile(_)
                | Node::For(_)
                | Node::ForOf(_)
                | Node::Closure(_)
                | Node::ArrowFunction(_)
                | Node::AnonymousClass(_)
        )
    }
}

impl<'ast, 'arena> Walker<'ast, 'arena, FinallyJumps> for FinallyJumpWalker {
    fn walk_in_node(&self, node: Node<'ast, 'arena>, jumps: &mut FinallyJumps) {
        if Self::holds_jumps(node) {
            jumps.inner += 1;
        }
        if jumps.inner > 0 {
            return;
        }

        match node {
            Node::Statement(Statement::Break(r#break)) => jumps.leaving.push((r#break.span(), "break")),
            Node::Statement(Statement::Continue(r#continue)) => jumps.leaving.push((r#continue.span(), "continue")),
            Node::Leave(Leave::Break(r#break)) => jumps.leaving.push((r#break.span(), "?? break")),
            Node::Leave(Leave::Continue(r#continue)) => jumps.leaving.push((r#continue.span(), "?? continue")),
            _ => {}
        }
    }

    fn walk_out_node(&self, node: Node<'ast, 'arena>, jumps: &mut FinallyJumps) {
        if Self::holds_jumps(node) {
            jumps.inner -= 1;
        }
    }
}

/// The first body of an `if`, `else` or loop that is not a block in braces, with the block to write in its place. An
/// `else` may also hold the next `if`, whose body is a block too. A lone `;` body is the empty block. PHP's
/// colon-delimited body is no PHP# syntax, so the walk reports it as a typo at its `:`.
fn body_without_braces(statement: &Statement, context: &Context<'_, '_, '_>) -> Option<(Span, String)> {
    let unbraced = |body: &Statement| match body {
        Statement::Block(_) => None,
        Statement::Noop(_) => Some((body.span(), "{ }".to_owned())),
        _ => Some((body.span(), format!("{{ {} }}", BytesDisplay(context.get_code_snippet(body))))),
    };

    match statement {
        Statement::If(If { body: IfBody::Statement(body), .. }) => {
            unbraced(body.statement).or_else(|| match body.else_clause.as_ref()?.statement {
                Statement::If(If { body: IfBody::Statement(next), .. }) => unbraced(next.statement),
                Statement::If(_) => None,
                next => unbraced(next),
            })
        }
        Statement::While(While { body: WhileBody::Statement(body), .. })
        | Statement::For(For { body: ForBody::Statement(body), .. }) => unbraced(body),
        Statement::ForOf(for_of) => unbraced(for_of.body),
        Statement::DoWhile(do_while) => unbraced(do_while.statement),
        _ => None,
    }
}

/// Checks the arms of a `match`, and returns whether its children can be checked. Spec section 21 needs a `default`
/// arm on a value that is not an enum or a sealed interface, and section 20 lets a `match` on one leave it out when its
/// arms cover every case. Only the analyzer knows the value's type, so a `match` whose arms all name `Class.y` values,
/// classes or `null` may leave out `default`, and the analyzer reports the cases it misses. A `match` without arms covers no case, so it needs
/// `default`. A block arm is only in a `match` statement. The engine takes one `default` arm, as PHP's `match` does.
fn check_pattern_match(pattern_match: &PatternMatch, is_expression: bool, context: &mut Context<'_, '_, '_>) -> bool {
    let mut defaults = pattern_match.arms.iter().filter(|arm| arm.is_default());
    match defaults.next() {
        None if pattern_match.arms.is_empty()
            || !pattern_match.arms.iter().all(|arm| names_class_values(arm, context)) =>
        {
            context.report(
                Issue::error("A `match` needs a `default` arm.")
                    .with_annotation(
                        Annotation::primary(pattern_match.r#match.span).with_message("This `match` has none."),
                    )
                    .with_note("Only a `match` on an enum may leave out `default`, when its arms cover every case.")
                    .with_help("Add `default => …` as the last arm."),
            );

            return false;
        }
        None => {}
        Some(default) => {
            if let Some(again) = defaults.next() {
                context.report(
                    Issue::error("A `match` has one `default` arm.")
                        .with_annotation(Annotation::primary(again.span()).with_message("Written again here."))
                        .with_annotation(Annotation::secondary(default.span()).with_message("First written here.")),
                );

                return false;
            }
        }
    }

    let block = pattern_match.arms.iter().find_map(|arm| match arm.body() {
        PatternMatchArmBody::Block(block) if is_expression => Some(block),
        _ => None,
    });
    if let Some(block) = block {
        context.report(
            Issue::error("A block arm is only in a `match` statement: a `match` that gives a value gives an expression in each arm.")
                .with_annotation(Annotation::primary(block.span()).with_message("Block written here."))
                .with_help("Start the statement with `match`, or give a value in this arm."),
        );

        return false;
    }

    true
}

/// Whether an arm's pattern names only `Class.y` values or `null`, joined by `or`, as the arms of a `match` on an enum
/// do.
fn names_class_values(arm: &PatternMatchArm, context: &Context<'_, '_, '_>) -> bool {
    fn is_class_value(pattern: &Pattern, context: &Context<'_, '_, '_>) -> bool {
        match pattern {
            Pattern::Value(Expression::Access(Access::Property(access))) => {
                context.names.static_property_class(access).is_some()
            }
            Pattern::Value(Expression::Literal(Literal::Null(_))) => true,
            // A class, as a case of an enum with data or an implementer of a sealed interface, spec section 20.
            Pattern::Type(TypePattern { hint: Hint::Identifier(_), .. }) => true,
            Pattern::Binary(binary) if !binary.is_and() => {
                is_class_value(binary.left, context) && is_class_value(binary.right, context)
            }
            Pattern::Parenthesized(parenthesized) => is_class_value(parenthesized.pattern, context),
            _ => false,
        }
    }

    match arm {
        PatternMatchArm::Pattern(arm) => is_class_value(arm.pattern, context),
        PatternMatchArm::Default(_) => true,
    }
}

/// Reports each variable a pattern declares under `or` or `not`, which C# refuses as error CS8780: the variable would
/// have no value where the pattern matches. `hidden` is whether an enclosing pattern already hides it.
fn report_hidden_pattern_variables(pattern: &Pattern, hidden: bool, context: &mut Context<'_, '_, '_>) {
    let report_properties = |properties: &PropertiesPattern, context: &mut Context<'_, '_, '_>| {
        for property in &properties.properties {
            report_hidden_pattern_variables(property.pattern, hidden, context);
        }
    };
    let report_variable = |variable: &LocalIdentifier, context: &mut Context<'_, '_, '_>| {
        if hidden {
            context.report(
                Issue::error(format!(
                    "`{}` is declared under `or` or `not`, where the pattern can match without a value for it.",
                    BytesDisplay(variable.value)
                ))
                .with_annotation(Annotation::primary(variable.span).with_message("Declared here."))
                .with_help("Declare the variable outside `or` and `not`, or test the value again where you use it."),
            );
        }
    };

    match pattern {
        Pattern::Type(TypePattern { fields, properties, variable, .. }) => {
            if let Some(variable) = variable {
                report_variable(variable, context);
            }
            for field in fields.iter().flat_map(|fields| fields.fields.iter()) {
                report_hidden_pattern_variables(field, hidden, context);
            }
            if let Some(properties) = properties {
                report_properties(properties, context);
            }
        }
        Pattern::List(list) => {
            for element in &list.elements {
                match element {
                    ListPatternElement::Pattern(pattern) => report_hidden_pattern_variables(pattern, hidden, context),
                    ListPatternElement::Rest(rest) => {
                        if let Some(variable) = &rest.variable {
                            report_variable(variable, context);
                        }
                    }
                }
            }
        }
        Pattern::Value(_) | Pattern::Comparison(_) => {}
        Pattern::Not(not) => report_hidden_pattern_variables(not.pattern, true, context),
        Pattern::Binary(binary) => {
            let hidden = hidden || !binary.is_and();
            report_hidden_pattern_variables(binary.left, hidden, context);
            report_hidden_pattern_variables(binary.right, hidden, context);
        }
        Pattern::Parenthesized(parenthesized) => {
            report_hidden_pattern_variables(parenthesized.pattern, hidden, context)
        }
        Pattern::Properties(properties) => report_properties(properties, context),
    }
}

/// Whether the slice can write an expression: a local, a parameter, `field`, a member written `object.name` or
/// `object?.name`, or an index of one of them, as in `counts["a"]`, or PHP's append to one, as in `items[]`. A member
/// written by its bare name passes, because `check_constant_access` reports it with the name to write instead.
fn is_slice_target(target: &Expression, context: &Context<'_, '_, '_>) -> bool {
    match target {
        Expression::ConstantAccess(access) => {
            matches!(context.names.binding(&access.name), Some(Binding::Local(_) | Binding::Field | Binding::Member))
        }
        Expression::Access(Access::Property(_) | Access::NullSafeProperty(_)) => true,
        Expression::ArrayAccess(access) => is_slice_target(access.array, context),
        // The analyzer refuses `x[] = v` with the PHP# form for the collection it writes, as `add` for a `List`.
        Expression::ArrayAppend(append) => is_slice_target(append.array, context),
        _ => false,
    }
}

/// Whether `hint` keys a `Map` and is a `Set`'s element by its kind alone, spec section 12: a type with `==` and
/// `hash()` with no code, a collection of such types, a union of them, or a named type, which the analyzer accepts when
/// it is an enum, a struct, a class that declares `operator ==`, or a type parameter bounded by one.
fn is_key_hint(hint: &Hint) -> bool {
    match hint {
        Hint::Integer(_) | Hint::Float(_) | Hint::Bool(_) | Hint::String(_) | Hint::Identifier(_) => true,
        Hint::Nullable(nullable) => is_key_hint(nullable.hint),
        // Spec section 19 compares a union by the type of the value it holds, so each of its types gives it both.
        Hint::Union(union) => is_key_hint(union.left) && is_key_hint(union.right),
        // Spec section 19 compares two collections by their elements, which `sharp_hash` hashes, so a collection whose
        // every type argument has `==` and `hash()` has them too.
        Hint::Generic(collection) if matches!(collection.name.value, b"List" | b"Set" | b"Map") => {
            collection.type_arguments.arguments.iter().all(is_key_hint)
        }
        // A generic struct or class, as `Pair<int, string>`, is a named type, whose `==` and `hash()` the analyzer checks.
        Hint::Generic(generic) => built_in_generic_arity(generic.name.value).is_none(),
        _ => false,
    }
}

/// Whether the slice has a type: the built-in types of spec section 24, or a class or a type parameter written by its
/// short name, or a case of an enum written through the enum, as `PaymentResult.Paid` of spec section 20, or a
/// nullable type, or `List<T>`, `Map<TKey, TValue>` or `Set<T>` of spec section 12, `Class<T>` of section 25 or a class
/// with type arguments of section 11, or a function type of spec section 14.1, whose inner types the walk checks next,
/// refusing a `void` type argument or parameter and a wrong count of type arguments as such. The analyzer checks a
/// generic class's arity and refuses a dotted type that names no case.
fn is_slice_type(hint: &Hint) -> bool {
    match hint {
        Hint::Integer(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::String(_)
        | Hint::Void(_)
        | Hint::Identifier(Identifier::Local(_) | Identifier::Dotted(_))
        | Hint::Nullable(_) => true,
        Hint::Mixed(any) => any.value == ANY,
        Hint::Object(object) => object.value == OBJECT,
        Hint::Generic(_) | Hint::Tuple(_) | Hint::Function(_) => true,
        _ => false,
    }
}

/// The error for a type PHP# writes another way, spec section 24: the built-in types are lowercase, every other type is
/// capitalized, as `Any` and `Object`, a lone `null` is written with `?` after a type, and PHP's `mixed`, `object`,
/// `array`, `callable`, `iterable`, `parent`, `true` and `false` are no PHP# syntax. `never` is a type only as a
/// method's return type, spec section 7, at `place` [`Place::Method`] or [`Place::Signature`]. `None` for a type
/// written as PHP# writes it.
fn type_written_another_way(hint: &Hint, place: Place, context: &Context<'_, '_, '_>) -> Option<Issue> {
    let written = match hint {
        Hint::Identifier(Identifier::Local(name)) => name.value,
        Hint::Void(name)
        | Hint::Float(name)
        | Hint::Bool(name)
        | Hint::Integer(name)
        | Hint::String(name)
        | Hint::Object(name)
        | Hint::Mixed(name)
        | Hint::Iterable(name) => name.value,
        Hint::Null(keyword)
        | Hint::True(keyword)
        | Hint::False(keyword)
        | Hint::Array(keyword)
        | Hint::Callable(keyword)
        | Hint::Parent(keyword) => keyword.value,
        Hint::Never(keyword) if !matches!(place, Place::Method | Place::Signature) => keyword.value,
        _ => return None,
    };
    let lowercase = written.to_ascii_lowercase();
    let message = match lowercase.as_slice() {
        b"int" | b"float" | b"bool" | b"string" | b"void" if written != lowercase.as_slice() => {
            format!("PHP# writes the type `{}`.", BytesDisplay(&lowercase))
        }
        // PHP's `object` is lowercase, so another case is PHP#'s `Object`.
        b"object" if written != OBJECT && written != b"object" => "PHP# writes `Object`.".to_owned(),
        b"object" | b"mixed" | b"true" | b"false" | b"array" | b"callable" | b"parent"
            if written != OBJECT || lowercase != b"object" =>
        {
            return Some(unexpected(hint.span(), context));
        }
        b"iterable" if written == b"iterable" => return Some(unexpected(hint.span(), context)),
        b"any" if written != ANY => "PHP# writes `Any`.".to_owned(),
        b"null" => "A type holds null only with `?` after it: write the type, as in `int?`.".to_owned(),
        b"never" => "PHP# has no `never` type outside a return type.".to_owned(),
        b"iterable" => "`Iterable` takes 1 type argument: the element.".to_owned(),
        _ => return None,
    };

    Some(Issue::error(message).with_annotation(Annotation::primary(hint.span()).with_message("Written here.")))
}

/// Decides a tuple type, spec section 30: its elements are values, so none is `void`, and past four elements the
/// checker suggests a struct, whose fields have names where the code reads them.
fn check_tuple(tuple: &TupleHint, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let count = tuple.elements.len();
    if count > 4 {
        context.report(
            Issue::warning(format!("A tuple of {count} elements is hard to read: declare a struct, whose fields have names."))
                .with_annotation(Annotation::primary(tuple.span()).with_message("Written here."))
                .with_help("A struct reads each value by its field's name, as in `public struct Totals { public Money total { get; init; } }`."),
        );
    }
    if let Some(void) = tuple.elements.iter().find(|element| matches!(element.hint, Hint::Void(_))) {
        context.report(
            Issue::error("A tuple's element cannot be `void`: `void` is only a return type.")
                .with_annotation(Annotation::primary(void.hint.span()).with_message("Written here.")),
        );

        return None;
    }

    Some(Place::Parameter)
}

/// Refuses the expression at `span` in a constant value: it reads a type argument, which only the running code has.
fn report_type_argument_in_constant(span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!(
            "`{}` is not a constant: it reads the type argument the code runs with.",
            BytesDisplay(context.get_code_snippet(span))
        ))
        .with_annotation(Annotation::primary(span).with_message("Written here.")),
    );
}

/// The refusal of `hint` in a constant of its class, when `hint` names one of the class's type parameters or is `Self`,
/// which carries them all. `None` for any other type.
fn static_member_use(hint: &Hint<'_>, context: &Context<'_, '_, '_>) -> Option<String> {
    match hint {
        Hint::Identifier(name) => {
            let Some(Binding::TypeParameter { declaration }) = context.names.binding(name) else {
                return None;
            };
            let class = static_member_class(hint.span(), context)?;

            class.type_parameters?.span().contains(&declaration.start).then(|| {
                format!(
                    "A constant can't use `{}`, because every `{}<…>` shares it.",
                    BytesDisplay(name.value()),
                    BytesDisplay(class.name.value)
                )
            })
        }
        Hint::Self_(keyword) if is_sharp_self(keyword) => static_self_use(hint.span(), context),
        _ => None,
    }
}

/// The refusal of PHP#'s `Self` at `span` in a constant's type in a generic class: `Self` carries the class's type
/// parameters, which no constant names. A constant's value writes no `new`, so `new Self(…)` never reaches here.
fn static_self_use(span: Span, context: &Context<'_, '_, '_>) -> Option<String> {
    let class = static_member_class(span, context)?;
    let names: Vec<String> = class
        .type_parameters?
        .parameters
        .iter()
        .map(|parameter| format!("`{}`", BytesDisplay(parameter.name.value)))
        .collect();
    let carried = match names.split_last()? {
        (last, []) => last.clone(),
        (last, rest) => format!("{} and {last}", rest.join(", ")),
    };

    Some(format!(
        "A constant can't use `Self`, because every `{}<…>` shares it and `Self` carries {carried}.",
        BytesDisplay(class.name.value)
    ))
}

/// The class of the constant that holds `span`. A static method or property of a generic class has the type arguments
/// it is reached through, spec section 11, and a constant has none.
fn static_member_class<'ast, 'arena>(
    span: Span,
    context: &Context<'_, 'ast, 'arena>,
) -> Option<ClassLike<'ast, 'arena>> {
    let class = enclosing_class(context.program, span)?;
    let in_constant = class.members.iter().any(|member| match member {
        ClassLikeMember::Constant(constant) => constant.span().contains(&span.start),
        _ => false,
    });

    in_constant.then_some(class)
}

/// Reports `refusal`, which `static_member_use` or `static_self_use` words, at `span`.
fn report_static_member_use(span: Span, refusal: String, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(refusal).with_annotation(Annotation::primary(span).with_message("Used in a static member.")),
    );
}

/// Reports each type parameter of a list whose name is not `T`, or `T` and an uppercase letter, as C# names them, a
/// name the list declares twice, a bound that leads back to its type parameter, as C# refuses a circular constraint
/// (CS0454), and `in` or `out` on a method's or an enum's, because only a class or an interface declares variance,
/// spec section 11.1.
fn check_type_parameters(list: &TypeParameterList, place: Place, context: &mut Context<'_, '_, '_>) {
    for (index, parameter) in list.parameters.iter().enumerate() {
        let name = parameter.name.value;
        if let Some(bound) = &parameter.bound
            && bounds_itself(list, parameter)
        {
            context.report(
                Issue::error(format!(
                    "A type parameter can't bound itself: the bound of `{0}` leads back to `{0}`.",
                    BytesDisplay(name)
                ))
                .with_annotation(Annotation::primary(bound.hint.span()).with_message("Bound written here.")),
            );
        }
        if !matches!(name, [b'T'] | [b'T', b'A'..=b'Z', ..]) {
            context.report(
                Issue::error("A type parameter's name is `T`, or `T` and a capital letter, as in `TItem`.")
                    .with_annotation(Annotation::primary(parameter.name.span).with_message("Named here.")),
            );
        }

        if let Some(first) = list.parameters.iter().take(index).find(|earlier| earlier.name.value == name) {
            context.report(
                Issue::error(format!("This type parameter list declares `{}` twice.", BytesDisplay(name)))
                    .with_annotation(Annotation::primary(parameter.name.span).with_message("Declared again here."))
                    .with_annotation(Annotation::secondary(first.name.span).with_message("First declared here.")),
            );
        }

        if matches!(place, Place::Method | Place::Signature | Place::Extension | Place::Enum)
            && let Some(variance) = parameter.variance
        {
            let owner = match place {
                Place::Extension => "An extension block's",
                Place::Enum => "An enum's",
                _ => "A method's",
            };
            context.report(
                Issue::error(format!(
                    "{owner} type parameter takes no `{}`: only a class or an interface declares variance, so remove it.",
                    BytesDisplay(variance.value)
                ))
                .with_annotation(Annotation::primary(variance.span).with_message("Written here.")),
            );
        }
    }
}

/// Whether the bound of `parameter` names it, through the bounds of the other type parameters of `list` it names, as
/// `<T : T>` and `<TA : TB, TB : TA>` do.
fn bounds_itself(list: &TypeParameterList, parameter: &TypeParameter) -> bool {
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

/// Whether a type parameter's bound is another type parameter, as in `<TSub : T>`, or a class bound, as spec section
/// 11 writes it.
fn is_slice_bound(hint: &Hint, context: &Context<'_, '_, '_>) -> bool {
    is_type_parameter_bound(hint, context) || is_class_bound(hint, context)
}

/// Whether `hint` is a type parameter, a bound that stands alone, as Java's `<U extends T>` does.
fn is_type_parameter_bound(hint: &Hint, context: &Context<'_, '_, '_>) -> bool {
    matches!(hint, Hint::Identifier(name @ Identifier::Local(_)) if context.names.is_type_parameter(name))
}

/// Whether `hint` joins a type parameter with other bounds by `&`, as in `<TL : Order & TA>`.
fn joins_type_parameter(hint: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match hint {
        Hint::Intersection(intersection) => {
            joins_type_parameter(intersection.left, context) || joins_type_parameter(intersection.right, context)
        }
        hint => is_type_parameter_bound(hint, context),
    }
}

/// Whether `hint` is `Object`, a class or an interface, a generic class type, or several of them joined with `&`.
/// Mago's own check of an intersection refuses `Object` in one.
fn is_class_bound(hint: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match hint {
        Hint::Object(object) => object.value == OBJECT,
        Hint::Identifier(name @ Identifier::Local(_)) => !context.names.is_type_parameter(name),
        // A dotted name is a case type or a full name, which the analyzer refuses with the import to add, spec section
        // 23.
        Hint::Identifier(Identifier::Dotted(_)) => true,
        Hint::Generic(generic) => {
            built_in_generic_arity(generic.name.value).is_none() && !context.names.is_type_parameter(&generic.name)
        }
        Hint::Intersection(intersection) => {
            is_class_bound(intersection.left, context) && is_class_bound(intersection.right, context)
        }
        _ => false,
    }
}

/// Checks the type arguments a generic type's name takes: their count for `List`, `Set` and `Map`, spec section 12, and
/// `Class<T>`, section 25, a `Map`'s key, `Class<T>`'s class, and none on a type parameter. The analyzer checks a
/// generic class's arity and bounds. Returns the place of the type arguments, or none when the walk skips them.
fn check_generic(generic: &GenericHint, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    if context.names.is_type_parameter(&generic.name) {
        context.report(
            Issue::error(format!(
                "A type parameter takes no type arguments: write `{}`.",
                BytesDisplay(generic.name.value)
            ))
            .with_annotation(Annotation::primary(generic.span()).with_message("Written here.")),
        );

        return None;
    }

    let written = generic.type_arguments.arguments.len();
    if let Some(arity) = built_in_generic_arity(generic.name.value).filter(|arity| *arity != written) {
        let roles = match generic.name.value {
            b"Map" => "the key and the value",
            b"Class" => "the class",
            _ => "the element",
        };
        let plural = if arity == 1 { "" } else { "s" };
        context.report(
            Issue::error(format!(
                "`{}` takes {arity} type argument{plural}: {roles}.",
                BytesDisplay(generic.name.value)
            ))
            .with_annotation(Annotation::primary(generic.span()).with_message("Written here.")),
        );

        return None;
    }

    let key = match (generic.name.value, generic.type_arguments.arguments.as_slice()) {
        (b"Map", [key, _]) => Some((key, "A `Map`'s key")),
        (b"Set", [element]) => Some((element, "A `Set`'s element")),
        _ => None,
    };
    match (generic.name.value, generic.type_arguments.arguments.as_slice()) {
        // A `Set`'s element takes exactly what a `Map`'s key takes. The analyzer refuses a named key type without `==`
        // and `hash()`, with the check a `Map` literal's key and an inferred key keep, and a class value keys one as
        // its class name. A key type outside the slice, nullable or not, is refused once, by the walk, as that type.
        _ if let Some((key, rule)) = key
            && !is_key_hint(key)
            && !matches!(key, Hint::Generic(class) if class.name.value == b"Class")
            && is_slice_type(match key {
                Hint::Nullable(nullable) => nullable.hint,
                key => key,
            }) =>
        {
            report_refusal(
                Issue::error(format!(
                    "{rule} is anything with `==` and `hash()`: an `int`, a `float`, a `string`, a `bool`, `null`, an enum, a struct, a `List`, `Set` or `Map` of them, or a class that declares `operator ==`."
                ))
                .with_annotation(Annotation::primary(key.span()).with_message("Key type written here.")),
                generic.span(),
                place,
                context,
            )
        }
        _ => Some(place),
    }
}

/// Checks a union type. Each member in the slice is decided with the parts inside it as it is outside a union, so a
/// generic member takes the type arguments it takes alone. Two members that overlap are refused, as [`overlap`] finds
/// them: one type written twice, two collections, as a type is one collection, and a collection beside an `Iterable`.
fn check_union(union: &UnionHint, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut members = Vec::new();
    union_members(union, &mut members);

    for (index, member) in members.iter().enumerate() {
        let is_slice_member = match member {
            Hint::Null(_) => {
                report_null_in_union(member, &members, context);

                false
            }
            Hint::Void(_) | Hint::Never(_) | Hint::Nullable(_) => false,
            Hint::Self_(keyword) => check_self_type(keyword, place, context),
            Hint::Static(keyword) => {
                report_unexpected(keyword.span, context);

                false
            }
            _ if is_slice_type(member) => true,
            _ => {
                match type_written_another_way(member, place, context) {
                    Some(issue) => context.report(issue),
                    None => report_unsupported(Node::Hint(member), place, context),
                }

                false
            }
        };

        if !is_slice_member {
            continue;
        }
        enter_tree(Node::Hint(member), place, context);
        let Some((first, overlap)) =
            members[..index].iter().find_map(|earlier| Some((earlier, overlap(earlier, member, context)?)))
        else {
            continue;
        };

        let written = |hint: &Hint| BytesDisplay(context.get_code_snippet(hint)).to_string();
        let issue = match overlap {
            Overlap::Written => Issue::error(format!("Duplicate type `{}` is redundant.", written(member)))
                .with_annotation(Annotation::primary(member.span()).with_message("Written again here."))
                .with_annotation(Annotation::secondary(first.span()).with_message("First written here."))
                .with_help("Remove the second one, as PHP refuses a union that names a type twice."),
            Overlap::Array => {
                Issue::error(format!("A type can't be both a `{}` and a `{}`.", written(first), written(member)))
                    .with_annotation(Annotation::primary(member.span()).with_message("The second collection."))
                    .with_annotation(Annotation::secondary(first.span()).with_message("The first collection."))
                    .with_help(format!(
                        "Take the `{}` and the `{}` as two parameters, or as an enum with one case that holds each.",
                        written(first),
                        written(member)
                    ))
            }
            Overlap::Iterated { collection, iterable } => Issue::error(format!(
                "`{}` is already an `{}`, so the union is `{}`.",
                written(collection),
                written(iterable),
                written(iterable)
            ))
            .with_annotation(Annotation::primary(member.span()).with_message("Written here."))
            .with_annotation(Annotation::secondary(first.span()).with_message("Holds it already."))
            .with_help(format!("Write `{}` alone.", written(iterable))),
        };
        context.report(issue);
    }
}

/// Reports `null` written in a union, spec section 24: a union holds null only through `?`, so the error names the
/// union of the other `members` in parentheses with `?` after it, or `T?` when one type is left.
fn report_null_in_union(null: &Hint, members: &[&Hint], context: &mut Context<'_, '_, '_>) {
    let rest: Vec<String> = members
        .iter()
        .filter(|member| !matches!(member, Hint::Null(_)))
        .map(|member| BytesDisplay(context.get_code_snippet(*member)).to_string())
        .collect();
    let message = match rest.as_slice() {
        [single] => format!("A type that holds null is written `{single}?`."),
        _ => format!("A union that holds null is written `({})?`.", rest.join("|")),
    };

    context
        .report(Issue::error(message).with_annotation(Annotation::primary(null.span()).with_message("Written here.")));
}

/// The union `union` written without its `null`, as `int|string` for `int|string|null`, or None when it holds no `null`.
fn written_without_null(union: &UnionHint, context: &Context<'_, '_, '_>) -> Option<String> {
    let mut members = Vec::new();
    union_members(union, &mut members);
    if !members.iter().any(|member| matches!(member, Hint::Null(_))) {
        return None;
    }

    let rest: Vec<String> = members
        .iter()
        .filter(|member| !matches!(member, Hint::Null(_)))
        .map(|member| BytesDisplay(context.get_code_snippet(*member)).to_string())
        .collect();

    Some(rest.join("|"))
}

/// Reports `as` to `hint`, a nullable type or `void`, spec section 14.4: `as T` already gives `T?`. `without` is the
/// type written without its `?` or `null`, which the help names.
fn report_nullable_as(hint: &Hint, without: Option<String>, context: &mut Context<'_, '_, '_>) {
    let issue = Issue::error("`as` converts to a type that is not nullable or `void`.")
        .with_annotation(Annotation::primary(hint.span()).with_message("Written here."))
        .with_note("`as T` already gives `T?`: the value as a `T`, or null when it is not one.");

    context.report(match without {
        Some(without) => issue.with_help(format!("Write `as {without}`.")),
        None => issue,
    });
}

/// How two members of a union overlap, so the union is refused.
enum Overlap<'hint, 'arena> {
    /// One type written twice.
    Written,
    /// Two of `List`, `Map` and `Set`: a type is one collection, as in C#, Java, Kotlin and Swift.
    Array,
    /// A collection beside an `Iterable` of its element type, which already holds it.
    Iterated { collection: &'hint Hint<'arena>, iterable: &'hint Hint<'arena> },
}

/// How the union members `first` and `second` overlap, if they do. Type arguments reach the running program, so a
/// generic class with other type arguments is another type.
fn overlap<'hint, 'arena>(
    first: &'hint Hint<'arena>,
    second: &'hint Hint<'arena>,
    context: &Context<'_, '_, '_>,
) -> Option<Overlap<'hint, 'arena>> {
    if is_same_type(first, second, context) {
        return Some(Overlap::Written);
    }

    let (Hint::Generic(first_generic), Hint::Generic(second_generic)) = (first, second) else {
        return None;
    };
    let iterates = |iterable: &GenericHint, element: &Hint| match (
        iterable.name.value,
        iterable.type_arguments.arguments.as_slice(),
    ) {
        (b"Iterable", [iterated]) => is_same_type(iterated, element, context),
        _ => false,
    };

    match (collection_element(first_generic), collection_element(second_generic)) {
        (Some(_), Some(_)) => Some(Overlap::Array),
        (Some(element), None) if iterates(second_generic, element) => {
            Some(Overlap::Iterated { collection: first, iterable: second })
        }
        (None, Some(element)) if iterates(first_generic, element) => {
            Some(Overlap::Iterated { collection: second, iterable: first })
        }
        _ => None,
    }
}

/// The element type of a `List`, `Set` or `Map`, a map's being its value type, as a loop over it reads them.
fn collection_element<'hint, 'arena>(generic: &'hint GenericHint<'arena>) -> Option<&'hint Hint<'arena>> {
    match (generic.name.value, generic.type_arguments.arguments.as_slice()) {
        (b"List" | b"Set", [element]) | (b"Map", [_, element]) => Some(element),
        _ => None,
    }
}

/// Decides a node and every node inside it at a place, as the checking walk does, for a node whose parent's check
/// decides it and so keeps the walk out of it.
fn enter_tree(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    if let Some(inner) = enter(node, place, context) {
        ensure_sufficient_stack(|| node.visit_children(|child| enter_tree(child, inner, context)));
    }
}

/// Whether a `self` keyword is PHP#'s `Self`, written exactly so. The lexer reads `Self` and PHP's `self` as one
/// keyword, and any other spelling is PHP's `self`.
fn is_sharp_self(keyword: &Keyword) -> bool {
    keyword.value == b"Self"
}

/// Checks `Self` or PHP's `self` written as a type at a place, and returns whether the slice has it. Spec section 25
/// makes `Self` a return type only, as in Swift.
fn check_self_type(keyword: &Keyword, place: Place, context: &mut Context<'_, '_, '_>) -> bool {
    let is_return_type = matches!(place, Place::Method | Place::Signature);
    if !is_sharp_self(keyword) {
        report_unexpected(keyword.span, context);
    } else if !is_return_type {
        context.report(
            Issue::error("`Self` is only a return type in PHP#.")
                .with_annotation(Annotation::primary(keyword.span).with_message("Written here."))
                .with_note("`Self` is the class a static method is called on, which a method returns."),
        );
    }

    is_sharp_self(keyword) && is_return_type
}

/// Decides `new` on a class written by its short name, on `Self`, or on PHP's `self` or `static`. `new Self(…)` needs
/// the class's constructor marked `required`, spec section 25, because `Self` can be any subclass, and names no type
/// arguments, spec section 11, because `Self` is the class with its own type parameters, which a static member of a
/// generic class can't name. PHP's `self` and `static`, and `Self` in such a static member, are reported at the
/// keyword, so the arguments are still checked. `new` of a type parameter creates its type argument while the code
/// runs, which carries its own type arguments, so it names none.
fn check_instantiation(instantiation: &Instantiation, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    match instantiation.class {
        Expression::Self_(keyword) if !is_sharp_self(keyword) => {
            report_unexpected(keyword.span, context);

            return Some(Place::Instantiation);
        }
        Expression::Static(keyword) => {
            report_unexpected(keyword.span, context);

            return Some(Place::Instantiation);
        }
        // A named constructor through a type parameter reads the type argument while the code runs, which the engine
        // can't do yet.
        Expression::Identifier(class)
            if context.names.is_type_parameter(class)
                && let Some(constructor) = &instantiation.constructor =>
        {
            context.report(
                Issue::error(format!(
                    "A named constructor through a type parameter, as in `new {}.{}(…)`, is not supported yet in PHP#.",
                    BytesDisplay(context.get_code_snippet(class)),
                    BytesDisplay(constructor.name.value)
                ))
                .with_annotation(Annotation::primary(instantiation.span()).with_message("Not supported yet.")),
            );

            return None;
        }
        Expression::Identifier(class)
            if context.names.is_type_parameter(class)
                && let Some(type_arguments) = &instantiation.type_arguments =>
        {
            let name = BytesDisplay(context.get_code_snippet(class));
            context.report(
                Issue::error(format!("`{name}` already carries the type arguments of the type it stands for."))
                    .with_annotation(Annotation::primary(type_arguments.span()).with_message("Written here."))
                    .with_help(format!("Write `new {name}(…)`.")),
            );
        }
        Expression::Self_(_) if let Some(type_arguments) = &instantiation.type_arguments => {
            context.report(
                Issue::error("`Self` already carries its class's type parameters.")
                    .with_annotation(Annotation::primary(type_arguments.span()).with_message("Written here."))
                    .with_help("Write `new Self(…)`."),
            );
        }
        Expression::Parenthesized(_) if let Some(constructor) = &instantiation.constructor => {
            context.report(
                Issue::error("`new` through a class value calls the class's constructor, so it names no other one.")
                    .with_annotation(Annotation::primary(constructor.span()).with_message("Named here."))
                    .with_help("Call a named constructor on its class by name, as in `new Report.fromJson(…)`."),
            );

            return None;
        }
        _ => {}
    }

    if instantiation.argument_list.is_none() && instantiation.initializer.is_none() {
        // `new App.Shop.Order()` reads as the named constructor `Shop` of `App` and a call of `Order` on it. Only the
        // codebase tells a full name from a member, so the analyzer refuses the chain, spec section 23.
        if instantiation.constructor.is_some() && is_member_chain_object(instantiation, context) {
            return Some(Place::Instantiation);
        }

        // PHP# writes `new Class(arguments)`, parentheses included, so the token after the class is unexpected.
        report_unexpected(instantiation.span().from_start(instantiation.span().end), context);

        return None;
    }

    if matches!(instantiation.class, Expression::Self_(_)) && context.slice_places.contains(&Some(Place::Enum)) {
        context.report(
            Issue::error("An enum has no constructor: its cases are its only values.")
                .with_annotation(Annotation::primary(instantiation.span()).with_message("Created here.")),
        );

        return None;
    }

    // A named constructor belongs to the class that declares it, spec section 9.1, and `Self` can be any subclass. The
    // file shows the class only when it declares the named constructor, and the analyzer names any other.
    if let (Expression::Self_(_), Some(constructor), Some(arguments)) =
        (instantiation.class, &instantiation.constructor, &instantiation.argument_list)
    {
        let Some(class) = enclosing_class(context.program, instantiation.span()).filter(|class| {
            class.members.iter().any(|member| {
                matches!(member, ClassLikeMember::NamedConstructor(declared)
                    if declared.name.value.eq_ignore_ascii_case(constructor.name.value))
            })
        }) else {
            return Some(Place::Instantiation);
        };
        let fix = format!(
            "new {}.{}{}",
            BytesDisplay(class.name.value),
            BytesDisplay(constructor.name.value),
            BytesDisplay(context.get_code_snippet(arguments))
        );
        context.report(
            Issue::error("`Self` cannot name a named constructor: a named constructor is not inherited, and `Self` can be any subclass.")
                .with_annotation(Annotation::primary(instantiation.span()).with_message("Created here."))
                .with_help(format!("Name its class: write `{fix}`.")),
        );

        return None;
    }

    if matches!(instantiation.class, Expression::Self_(_))
        && let Some(class) = enclosing_class(context.program, instantiation.span())
        && !has_required_constructor(&class)
    {
        let name = BytesDisplay(class.name.value);
        context.report(
            Issue::error(
                "`new Self(…)` needs a `required` constructor: `Self` can be any subclass, so every subclass must keep a constructor that `new Self(…)` can call.",
            )
            .with_annotation(Annotation::primary(instantiation.span()).with_message("Created here."))
            .with_help(format!("Declare the constructor of `{name}` `required`, as in `public required {name}(…)`.")),
        );

        return None;
    }

    Some(Place::Instantiation)
}

/// Whether `instantiation` is the object a member read or a method call reads through, as the `new App.Shop` of
/// `new App.Shop.Order()` is.
fn is_member_chain_object(instantiation: &Instantiation, context: &Context<'_, '_, '_>) -> bool {
    // The check of an expression runs before the walk enters it, so the last ancestor is its parent.
    let object = match context.ancestors.last() {
        Some(Node::Expression(Expression::Call(Call::Method(call)))) => call.object,
        Some(Node::Expression(Expression::Access(Access::Property(access)))) => access.object,
        _ => return false,
    };

    object.span() == instantiation.span()
}

/// Whether a class's constructor is marked `required`.
fn has_required_constructor(class: &ClassLike) -> bool {
    class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Method(method)
            if php_method_name(method) == b"__construct"
                && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Required(_))))
    })
}

/// Checks a named constructor of `class`, spec section 9.1: its class has objects of its own, it is declared on its own
/// class's name, as in `Artifact.fromJson`, takes only an access modifier, has a name that is neither its class's nor
/// one PHP reserves, and passes control to the main constructor with `: this(…)`. `check_members` reports a name
/// another member of the class has, as PHP's duplicate-method check, since the named constructor runs as the static
/// method of its name.
fn check_named_constructor(
    constructor: &NamedConstructor,
    class: &LocalIdentifier,
    is_abstract: bool,
    context: &mut Context<'_, '_, '_>,
) {
    let class_name = BytesDisplay(class.value);
    let name = BytesDisplay(constructor.name.value);
    let mut report = |message: String, span: Span, written: &str| {
        context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message(written)));
    };

    if is_abstract {
        report(
            "An abstract class has no objects of its own, so it declares no named constructor.".to_owned(),
            constructor.name.span,
            "Declared here.",
        );
    }

    if constructor.class.value != class.value {
        report(
            format!("A named constructor is declared on its own class, as in `{class_name}.{name}`."),
            constructor.class.span,
            "Written here.",
        );
    }

    if constructor.name.value.eq_ignore_ascii_case(class.value) {
        report(
            format!(
                "A named constructor cannot have its class's name: `new {class_name}(…)` calls the main constructor."
            ),
            constructor.name.span,
            "Declared here.",
        );
    } else if constructor.name.value.starts_with(b"__") {
        report(
            "A named constructor's name cannot start with `__`: PHP reserves those names for its magic methods."
                .to_owned(),
            constructor.name.span,
            "Declared here.",
        );
    }

    // Spec section 5's `internal` is an access.
    let is_access = |modifier: &Modifier| modifier.is_read_visibility() || matches!(modifier, Modifier::Internal(_));
    for modifier in constructor.modifiers.iter().filter(|modifier| !is_access(modifier)) {
        report(
            format!(
                "A named constructor takes only `public`, `protected` or `private`: remove `{}`.",
                BytesDisplay(modifier.get_keyword().value)
            ),
            modifier.span(),
            "Written here.",
        );
    }

    if constructor.initializer.is_none() {
        report(
            "A named constructor passes control to the main constructor with `: this(…)`.".to_owned(),
            constructor.name.span,
            "Declared without `: this(…)` here.",
        );
    }

    for modifier in constructor.parameter_list.parameters.iter().flat_map(|parameter| parameter.modifiers.iter()) {
        report(
            "A named constructor's parameter declares no property: only the main constructor's parameters do."
                .to_owned(),
            modifier.span(),
            "Written here.",
        );
    }

    if let MethodBody::Abstract(body) = &constructor.body {
        report("A named constructor needs a body, even an empty `{ }`.".to_owned(), body.span(), "Written here.");
    }
}

/// Whether `: this(…)` or `: super(…)` belongs to a named constructor rather than to the main one.
fn is_named_constructor_initializer(initializer: &ConstructorInitializer, program: &Program) -> bool {
    enclosing_class(program, initializer.span()).is_some_and(|class| {
        class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::NamedConstructor(constructor)
                if constructor.initializer.as_ref().is_some_and(|written| written.span() == initializer.span()))
        })
    })
}

/// Whether a parameter belongs to a class's constructor, the one method where an access modifier declares a property.
fn is_constructor_parameter(parameter: &FunctionLikeParameter, program: &Program) -> bool {
    enclosing_class(program, parameter.span()).is_some_and(|class| {
        class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Method(method)
                if php_method_name(method) == b"__construct"
                    && method.parameter_list.span().contains(&parameter.span().start))
        })
    })
}

/// The members of a union, in the order they are written. The parser nests a union to the right.
fn union_members<'ast, 'arena>(union: &'ast UnionHint<'arena>, members: &mut Vec<&'ast Hint<'arena>>) {
    for side in [union.left, union.right] {
        match side {
            Hint::Union(inner) => union_members(inner, members),
            member => members.push(member),
        }
    }
}

/// Whether `first` and `second` write one type, as the engine compares them: a class by its full name, ignoring case,
/// and a generic type by its name and each type argument.
fn is_same_type(first: &Hint, second: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match (first, second) {
        (Hint::Identifier(first), Hint::Identifier(second)) => {
            context.names.get(first).eq_ignore_ascii_case(context.names.get(second))
        }
        (Hint::Generic(first), Hint::Generic(second)) => {
            let (first_name, second_name) = (
                context.names.resolve(&first.name).unwrap_or(first.name.value),
                context.names.resolve(&second.name).unwrap_or(second.name.value),
            );
            let (first_arguments, second_arguments) =
                (&first.type_arguments.arguments, &second.type_arguments.arguments);

            first_name.eq_ignore_ascii_case(second_name)
                && first_arguments.len() == second_arguments.len()
                && first_arguments
                    .iter()
                    .zip(second_arguments.iter())
                    .all(|(first, second)| is_same_type(first, second, context))
        }
        _ => std::mem::discriminant(first) == std::mem::discriminant(second),
    }
}

/// Whether the slice has a binary operator. Every operator is named, so a new one does not compile until it is
/// decided.
const fn is_slice_binary_operator(operator: &BinaryOperator) -> bool {
    match operator {
        BinaryOperator::Addition(_)
        | BinaryOperator::Subtraction(_)
        | BinaryOperator::Multiplication(_)
        | BinaryOperator::Division(_)
        | BinaryOperator::Modulo(_)
        | BinaryOperator::Exponentiation(_)
        | BinaryOperator::Equal(_)
        | BinaryOperator::NotEqual(_)
        | BinaryOperator::Identical(_)
        | BinaryOperator::NotIdentical(_)
        | BinaryOperator::LessThan(_)
        | BinaryOperator::LessThanOrEqual(_)
        | BinaryOperator::GreaterThan(_)
        | BinaryOperator::GreaterThanOrEqual(_)
        | BinaryOperator::Spaceship(_)
        | BinaryOperator::And(_)
        | BinaryOperator::Or(_)
        | BinaryOperator::NullCoalesce(_)
        | BinaryOperator::BitwiseAnd(_)
        | BinaryOperator::BitwiseOr(_)
        | BinaryOperator::BitwiseXor(_)
        | BinaryOperator::LeftShift(_)
        | BinaryOperator::RightShift(_) => true,
        BinaryOperator::AngledNotEqual(_)
        | BinaryOperator::StringConcat(_)
        | BinaryOperator::Instanceof(_)
        | BinaryOperator::LowAnd(_)
        | BinaryOperator::LowOr(_)
        | BinaryOperator::LowXor(_) => false,
    }
}

/// Whether the slice has a prefix operator at a place. A parameter default has no `++` or `--`, and `enter` refuses
/// `@` before it calls this. Every operator is named, so a new one does not compile until it is decided.
fn is_slice_prefix_operator(operator: &UnaryPrefixOperator, place: Place) -> bool {
    match operator {
        UnaryPrefixOperator::Negation(_)
        | UnaryPrefixOperator::Plus(_)
        | UnaryPrefixOperator::Not(_)
        | UnaryPrefixOperator::BitwiseNot(_) => true,
        UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) => place == Place::Body,
        UnaryPrefixOperator::ErrorControl(_)
        | UnaryPrefixOperator::Reference(_)
        | UnaryPrefixOperator::ArrayCast(..)
        | UnaryPrefixOperator::BoolCast(..)
        | UnaryPrefixOperator::BooleanCast(..)
        | UnaryPrefixOperator::DoubleCast(..)
        | UnaryPrefixOperator::RealCast(..)
        | UnaryPrefixOperator::FloatCast(..)
        | UnaryPrefixOperator::IntCast(..)
        | UnaryPrefixOperator::IntegerCast(..)
        | UnaryPrefixOperator::ObjectCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::StringCast(..)
        | UnaryPrefixOperator::BinaryCast(..)
        | UnaryPrefixOperator::VoidCast(..) => false,
    }
}

/// Checks a cast, reporting it at its operator when the slice does not have it. Spec section 24 keeps `(int)`,
/// `(float)` and `(string)`, in a method body and a constant value alike, and PHP's other casts and its cast aliases
/// are no PHP# syntax.
fn check_cast(unary_prefix: &UnaryPrefix, context: &mut Context<'_, '_, '_>) {
    let operator = &unary_prefix.operator;
    if !matches!(
        operator,
        UnaryPrefixOperator::IntCast(..) | UnaryPrefixOperator::FloatCast(..) | UnaryPrefixOperator::StringCast(..)
    ) {
        report_unexpected(operator.span(), context);
    }
}

/// Reports a node's refusal, and returns the place of its children: none when the refusal's primary span covers
/// `skipped`, the node or the part of it that holds its code, so the walk skips the children, and `place` when the
/// refusal covers another part, such as a name or a modifier, so the walk still checks the rest.
fn report_refusal(issue: Issue, skipped: Span, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let covers = issue.annotations.iter().any(|annotation| {
        matches!(annotation.kind, AnnotationKind::Primary) && annotation.span.join(skipped) == annotation.span
    });
    context.report(issue);

    (!covers).then_some(place)
}

/// Reports a form outside the PHP# grammar as the parser reports a typo, at the token `span` starts with.
fn report_unexpected(span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(unexpected(span, context));
}

/// The error the parser gives a typo at the token `span` starts with, which every form outside the PHP# grammar gets.
fn unexpected(span: Span, context: &Context<'_, '_, '_>) -> Issue {
    Issue::from(&ParseError::unexpected_in_sharp(span.file_id, &context.source_file.contents, span.start.offset))
}

/// The span of the PHP superglobal an expression names, written bare as `_SERVER` or with `$`, through any index of
/// it, as in `_SESSION["user"]`. A bare name is the superglobal when it binds as a constant, or as a class before `.`
/// as in `_SERVER.x`, so a local named `_GET`, which `check_local_name` reports, is not.
fn superglobal_span(expression: &Expression, context: &Context<'_, '_, '_>) -> Option<Span> {
    match expression {
        Expression::ArrayAccess(access) => superglobal_span(access.array, context),
        Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
            if SUPERGLOBALS.contains(&local.value)
                && matches!(context.names.binding(name), Some(Binding::Constant | Binding::Class)) =>
        {
            Some(local.span)
        }
        Expression::Variable(Variable::Direct(direct))
            if direct.name.strip_prefix(b"$").is_some_and(|name| SUPERGLOBALS.contains(&name)) =>
        {
            Some(direct.span)
        }
        _ => None,
    }
}

/// The bare name an expression reads when it is written `__Something__`, the form of PHP's magic constants, which spec
/// section 27 removes. It binds as a constant, or as a class before `.` as in `__Foo__.bar`.
fn magic_name<'ast, 'arena>(
    expression: &'ast Expression<'arena>,
    context: &Context<'_, '_, '_>,
) -> Option<&'ast LocalIdentifier<'arena>> {
    match expression {
        Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
            if is_magic_name(local.value)
                && matches!(context.names.binding(name), Some(Binding::Constant | Binding::Class)) =>
        {
            Some(local)
        }
        _ => None,
    }
}

/// Whether a name is written `__Something__`, with at least one byte between the underscores.
fn is_magic_name(name: &[u8]) -> bool {
    name.len() > 4 && name.starts_with(b"__") && name.ends_with(b"__")
}

/// Reports a method whose name starts with `__`, as PHP's magic methods and `__Something__` names do, which is no PHP#
/// syntax. The constructor, the one method without a return type, carries its class's name, which `check_class_name`
/// reports. The analyzer warns on a method name out of camel case, since only it knows whether a parent, a plain PHP
/// one included, gives the method its name.
fn check_method_name(method: &Method, context: &mut Context<'_, '_, '_>) {
    if method.return_type_hint.is_some() && method.name.value.starts_with(b"__") {
        report_unexpected(method.name.span, context);
    }
}

/// Checks the name of a field or a property, the `kind`, in camel case. A member written `override` keeps its parent's
/// name, so its case is its parent's choice.
fn check_member_name(
    modifiers: &Sequence<Modifier>,
    name: &[u8],
    span: Span,
    kind: &str,
    context: &mut Context<'_, '_, '_>,
) {
    if is_magic_name(name) || !modifiers.iter().any(|modifier| matches!(modifier, Modifier::Override(_))) {
        check_declared_name(name, span, kind, NameCase::Camel, context);
    }
}

/// The case spec section 24 names a kind of declaration in, as TypeScript does.
#[derive(Clone, Copy)]
enum NameCase {
    /// Namespaces, types, type parameters, events and enum cases.
    Pascal,
    /// Methods, properties, fields, parameters and locals.
    Camel,
    /// Constants.
    UpperSnake,
}

/// Reports a declared name written `__Something__`, which spec section 27 removes as it removes reading one, and warns
/// on a name that breaks the case spec section 24 gives its `kind`, as "Method name `TotalIn` should be in camel case."
fn check_declared_name(name: &[u8], span: Span, kind: &str, case: NameCase, context: &mut Context<'_, '_, '_>) {
    if is_magic_name(name) {
        return report_unexpected(span, context);
    }

    let (holds, written, fixed) = match case {
        NameCase::Pascal => (is_class_case(name), "Pascal case", to_class_case(name)),
        NameCase::Camel => (is_camel_case(name), "camel case", to_camel_case(name)),
        NameCase::UpperSnake => (is_constant_case(name), "upper snake case", to_constant_case(name)),
    };
    if holds {
        return;
    }

    context.report(
        Issue::warning(format!("{kind} name `{}` should be in {written}.", BytesDisplay(name)))
            .with_annotation(Annotation::primary(span).with_message("Named here."))
            .with_help(format!("Rename it `{}`.", BytesDisplay(&fixed))),
    );
}

/// Whether the slice has an assignment operator. Every operator is named, so a new one does not compile until it
/// is decided.
const fn is_slice_assignment_operator(operator: &AssignmentOperator) -> bool {
    match operator {
        AssignmentOperator::Assign(_)
        | AssignmentOperator::Addition(_)
        | AssignmentOperator::Subtraction(_)
        | AssignmentOperator::Multiplication(_)
        | AssignmentOperator::Division(_)
        | AssignmentOperator::Modulo(_)
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Coalesce(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_) => true,
        AssignmentOperator::Concat(_) => false,
    }
}

/// Reports a node the slice does not have at its place. A form [`is_undecided`] names is not supported yet, a PHP#
/// form that breaks a rule of its place gets the error [`broken_rule`] gives, any other expression in a constant value
/// runs code, as `new Money(5)` and `Rates.current()` do there, and any other node is no PHP# syntax, which fails as a
/// typo does, at the first token PHP# has no place for.
fn report_unsupported(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    let construct = match node {
        Node::Statement(_) => "statement",
        Node::Expression(_) | Node::ConstantAccess(_) => "expression",
        Node::BinaryOperator(_) | Node::UnaryPrefixOperator(_) | Node::AssignmentOperator(_) => "operator",
        Node::Hint(_) | Node::NullableHint(_) => "type",
        Node::ClassLikeMember(_) => "class member",
        _ => "construct",
    };

    if is_undecided(node, place, context) {
        context.report(not_supported_yet(node.span(), construct, supported(place)));
    } else if let Some(issue) = broken_rule(node, place, context) {
        context.report(issue);
    } else if let Some(span) = unexpected_part(node) {
        report_unexpected(span, context);
    } else if let (Node::Expression(_) | Node::ConstantAccess(_), Place::Constant) = (node, place) {
        report_call_in_constant(node.span(), context);
    } else {
        report_unexpected(first_unexpected_token(node), context);
    }
}

/// Where a node PHP# never writes leaves the grammar: at its first token, or, for a declaration, at the first token
/// after the attributes and modifiers PHP# writes before it, as `trait` in `[Deprecated] public trait Audit`.
fn first_unexpected_token(node: Node<'_, '_>) -> Span {
    let children = node.children();
    match children.first() {
        Some(Node::AttributeList(_) | Node::Modifier(_)) => children
            .iter()
            .find(|child| !matches!(child, Node::AttributeList(_) | Node::Modifier(_)))
            .map_or_else(|| node.span(), HasSpan::span),
        // A statement holds its declaration, which spans the same code.
        Some(&declaration) if children.len() == 1 && declaration.span() == node.span() => {
            first_unexpected_token(declaration)
        }
        _ => node.span(),
    }
}

/// The token PHP# has no place for inside a form that starts with PHP# syntax, at any place: the `2` of `break 2`, the
/// `|>` of a pipe, and the first placeholder of a partial application, as `?` in `max(?, 1)` and `...` in
/// `Order.total(...)`.
fn unexpected_part(node: Node<'_, '_>) -> Option<Span> {
    match node {
        Node::Statement(
            Statement::Break(Break { level: Some(level), .. })
            | Statement::Continue(Continue { level: Some(level), .. }),
        ) => Some(level.span()),
        Node::Expression(Expression::Pipe(pipe)) => Some(pipe.operator.span()),
        Node::Expression(Expression::PartialApplication(application)) => {
            application.get_argument_list().arguments.iter().find_map(|argument| match argument {
                PartialArgument::Placeholder(placeholder) => Some(placeholder.span),
                PartialArgument::NamedPlaceholder(placeholder) => Some(placeholder.question_mark),
                PartialArgument::VariadicPlaceholder(placeholder) => Some(placeholder.span),
                PartialArgument::Positional(_) | PartialArgument::Named(_) => None,
            })
        }
        _ => None,
    }
}

/// Whether a node the slice does not have is a form PHP# has not decided, or that a later change builds, so it is not
/// supported yet: a member of `Self` read or written, as in `Self.count`, an interface property in another form than
/// `{ get; }`, `{ get; set; }` and `=> expr;`, as `{ get; init; }`, and an intersection type.
fn is_undecided(node: Node<'_, '_>, place: Place, context: &Context<'_, '_, '_>) -> bool {
    match (node, place) {
        // The check of an expression runs before the walk enters it, so the last ancestor is its parent.
        (Node::Expression(Expression::Self_(keyword)), _) => {
            is_sharp_self(keyword)
                && matches!(context.ancestors.last(), Some(Node::Expression(Expression::Access(Access::Property(access))))
                    if access.object.span() == keyword.span)
        }
        (Node::ClassLikeMember(ClassLikeMember::Property(_)), Place::Interface) => true,
        (Node::Hint(Hint::Intersection(_)) | Node::IntersectionHint(_), _) => true,
        (Node::Hint(Hint::Parenthesized(parenthesized)), _) => matches!(parenthesized.hint, Hint::Intersection(_)),
        _ => false,
    }
}

/// The error for a PHP# form that breaks a rule of the place it is written at, as a modifier the declaration doesn't
/// take, or `None` for a node that is no such form.
fn broken_rule(node: Node<'_, '_>, place: Place, context: &Context<'_, '_, '_>) -> Option<Issue> {
    let written = |span: Span| BytesDisplay(context.get_code_snippet(span)).to_string();
    let span = node.span();
    let error = |message: String| {
        Issue::error(message).with_annotation(Annotation::primary(span).with_message("Written here."))
    };

    Some(match (node, place) {
        // PHP's `public(set)`, `protected(set)` and `private(set)` are no PHP# syntax.
        (
            Node::Modifier(modifier),
            Place::Class
            | Place::Interface
            | Place::Enum
            | Place::ClassConstant
            | Place::FieldOrProperty
            | Place::Method
            | Place::Parameter,
        ) if !matches!(modifier, Modifier::PublicSet(_) | Modifier::ProtectedSet(_) | Modifier::PrivateSet(_)) => {
            let owner = match place {
                Place::Class => "A class",
                Place::Interface => "An interface",
                Place::Enum => "An enum",
                Place::ClassConstant => "A constant",
                Place::FieldOrProperty => "A field or property",
                Place::Method => "A method",
                _ => "A parameter",
            };

            error(format!("{owner} takes no `{}`: remove it.", BytesDisplay(modifier.get_keyword().value)))
                .with_note(supported(place))
        }
        (Node::Hint(Hint::Parenthesized(parenthesized)), _) => error(format!(
            "Write `{}` without parentheses: they go only around a union before `?`, as in `(int|string)?`.",
            written(parenthesized.hint.span())
        )),
        (Node::Hint(hint), Place::Header) => error(format!(
            "A header names classes and interfaces, and `{}` is neither: list a class or an interface, as in `: DatabaseEntity, Linkable`.",
            written(hint.span())
        ))
        .with_note(supported(place)),
        (Node::Statement(Statement::Extern(_)), Place::Body) => {
            error("An `extern` is declared at file level, after the imports: move it out of the method.".to_owned())
        }
        (Node::Statement(Statement::Use(_)), Place::Body) => {
            error("An `import` goes at the top of the file, after the namespace: move it out of the method.".to_owned())
        }
        (
            Node::Statement(
                Statement::Class(Class { name, .. })
                | Statement::Interface(Interface { name, .. })
                | Statement::Enum(Enum { name, .. })
                | Statement::Event(Event { name, .. })
                | Statement::Struct(Struct { name, .. }),
            ),
            Place::Body,
        ) => error(format!(
            "A method can't declare a type inside it: declare `{}` in its own file.",
            BytesDisplay(name.value)
        )),
        (Node::ClassLikeMember(ClassLikeMember::Law(law)), Place::Interface) => error(format!(
            "An interface has no laws: state `{}` in a class or an enum, whose values it is about.",
            BytesDisplay(law.name.value)
        ))
        .with_note(supported(place)),
        (Node::ClassLikeMember(ClassLikeMember::EnumCase(case)), Place::Class) => {
            error(format!("A class has no cases: declare `{}` in an enum.", BytesDisplay(case.item.name().value)))
                .with_note(supported(place))
        }
        (Node::Expression(Expression::Self_(keyword)), Place::Body | Place::Constant) if is_sharp_self(keyword) => {
            error("`Self` is no value: write `typeof(Self)` for the class a static method is called on.".to_owned())
        }
        _ => return None,
    })
}

/// What the slice has at a place, as the note of a refusal there.
fn supported(place: Place) -> &'static str {
    match place {
        Place::File => {
            "At file level, PHP# supports `namespace`, `import`, `class`, `struct`, `interface`, `enum` and `event`."
        }
        Place::Class => {
            "A PHP# class has attributes, an optional `public`, `abstract` or `final`, a name, optional type parameters as in `<out TItem : DatabaseEntity>`, an optional `: Base, Interface` header, constants, fields, properties, methods, named constructors, operators, laws and, in a static class, extensions, with no other modifiers, `extends` or `implements`."
        }
        Place::Interface => {
            "A PHP# interface has attributes, an optional `public`, a name, optional type parameters as in `<in TItem>`, an optional `: Interface` header, constants, methods, properties as in `int orderId { get; }` and `string label => …;`, and operators as in `static int operator <=>(Priced a, Priced b)`, with no other modifiers or `extends`."
        }
        Place::Signature => {
            "A PHP# interface method has attributes, no modifier, optional type parameters, parameters, a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, `never`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, or `Self`, each but `void` and `never` nullable as in `int?`, or a union of them but `void` and `never`, as in `int|string`, and no body or a default body."
        }
        Place::Enum => {
            "A PHP# enum has attributes, an optional `public`, a name, optional type parameters as in `<T, TError : Error>` when it has no backing type, an optional `: string, Interface` header whose `int` or `string` comes first, constants, cases, computed properties, methods, operators and laws, with no other modifiers or `implements`. A case of a backed enum has a value, as in `case None = \"none\";`, and a case of any other enum may carry data, as in `case Paid(string transactionId);`."
        }
        Place::Case => unreachable!("`enter` takes a case's name, its field list and each of its fields"),
        Place::Header => {
            "A PHP# header names each base class and interface by its short name, as in `: DatabaseEntity, Linkable`, and a generic one with its type arguments, as in `: PaginatedList<Order>`."
        }
        Place::TypeParameter => {
            "A PHP# type parameter is an optional `in` or `out`, a name that starts with `T`, and an optional bound of `Object`, of another type parameter, or of classes and interfaces joined with `&`, as in `<out TItem : DatabaseEntity & Shareable>`."
        }
        Place::Event => {
            "A PHP# event has an optional `public`, a name, parameters, each a type and a name with an optional default, and an optional `: Interface` header, as in `public event OrderPaid(int orderId) : OrderEvent;`."
        }
        Place::Struct => unreachable!("the walk enters every part of a struct"),
        Place::Module => unreachable!("a module holds only its header, bindings and laws, and `enter` takes each one"),
        Place::Extension => unreachable!("`enter` places an extension's keyword, receiver and every member of it"),
        Place::FieldOrProperty => {
            "A PHP# field is `private` or `protected`, and a property has the accessors `get` and an optional `set` or `init`, each `;`, `=> expr;` or a block. Both may be `static`, and have a type of `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, `Function<R(P)>`, or a tuple as in `[Money, int]`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional initial value."
        }
        Place::ClassConstant => {
            "A PHP# constant has attributes, `public`, `protected` or `private`, an optional type of `int`, `float`, `bool`, `string`, `Any`, a class, a generic type as in `Map<string, int>` or `Class<Report>`, a function type as in `Function<int(int)>` or a tuple as in `[int, string]`, nullable as in `int?` or not, or a union of them as in `int|string`, one name, and a constant value."
        }
        Place::Method => {
            "A PHP# method takes `public`, `protected`, `private`, `static`, `abstract`, `virtual` and `override`, a constructor also `required`, optional type parameters as in `<TItem : DatabaseEntity>`, parameters, and a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, `never`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, `Function<R(P)>`, a tuple as in `[Money, int]` or `Self`, each but `void` and `never` nullable as in `int?`, or a union of them but `void` and `never`, as in `int|string`, and an optional `on` clause after the parameters, as in `on OrderPaid, OrderRefunded`."
        }
        Place::Parameter => {
            "A PHP# parameter has a type of `int`, `float`, `bool`, `string`, `Any`, a class or a type parameter, a generic type as in `List<T>` or `PaginatedList<Order>`, `Function<R(P)>`, or a tuple as in `[Money, int]`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional default. The last parameter can be variadic, as in `int ...values`."
        }
        Place::Lambda => {
            "A PHP# lambda is a bare arrow after one name or parenthesized parameters, each with an optional type, and its body is an expression or a block, as in `(a, b) => a + b`. The last parameter can be variadic, as in `(int ...values) => count(values)`."
        }
        Place::Body
        | Place::Instantiation
        | Place::FunctionCall
        | Place::TryCatchClause
        | Place::SuperCall
        | Place::SelfCall
        | Place::RefusedPart(_) => {
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const`, which may unpack a tuple, and typed locals, `if` with `else if` and `else`, `while`, `do … while`, `for`, `for … of`, `break` and `continue` without a level, `try` with `catch` and `finally`, and `yield value;` and `yield ...other;` in a method that returns `Iterable<T>`, with literals, list and map literals, index reads, templates, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `??`, the ternary `c ? a : b`, `(int)`, `(float)` and `(string)`, `++` and `--`, method calls, with optional type arguments as in `Json.decode<WebhookPayload>(body)`, and property reads written with `.` or `?.`, lambdas as in `x => x.id`, `new Class(...)` with optional type arguments as in `new PaginatedList<Order>(rows)`, `new Class.name(...)` and, with a `required` constructor, `new Self(...)` and `new (type)(...)` of a class value, calls of global functions and of a local that holds a lambda, `super.method(...)` and `Self.method(...)`, each with positional, named and spread arguments as in `max(...prices)`, `throw`, `try` before a `Result`, `exit(code)`, `typeof(Class)`, `nameof(Class.property)`, `is`, `as` and `match`."
        }
        Place::Attribute => {
            "A PHP# attribute is a class name with optional positional and named arguments, as in `[Field(\"Name\", searchable: true)]`."
        }
        Place::Constant => {
            "A constant's value, an attribute argument, an enum case's value or a pattern's value is built from constants: literals, constants, enum cases, a backed case's `value` and the `name` of a case of an enum whose cases carry no data, `typeof(Class)`, list and map literals, indexes, templates, the ternary `c ? a : b`, `match`, `(int)`, `(float)` and `(string)`, and arithmetic, comparison, logical and `??` operators. All but a pattern's value take lambdas that capture nothing, and an attribute argument takes `new` of a class with such arguments, as in `[Rule(new Range(1, 10))]`."
        }
        Place::Pattern => {
            "A PHP# pattern is a type of `int`, `float`, `string`, `bool`, `Any`, `Object`, a class, a collection as in `List<int>`, a class value as in `Class<Order>`, a function type as in `Function<int(int)>`, or a union of them, a tuple among them, as in `int|[int, string]`, with an optional name, as in `int count`, a case with its fields, as in `PaymentResult.Paid(string transactionId)`, a class with properties, as in `Paid { amount: > 0 } big`, a constant, a comparison such as `< 10`, a properties pattern such as `{ total: > 0 }`, a list pattern such as `[Line first, ...List<Line> rest]`, or patterns joined by `and`, `or` and `not`."
        }
    }
}

/// The error for a construct PHP# has not decided, or that a later change builds, with what PHP# has where it is
/// written.
fn not_supported_yet(span: Span, construct: &str, supported: &str) -> Issue {
    Issue::error(format!("This {construct} is not supported yet in PHP#."))
        .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
        .with_note(supported)
}

#[inline]
pub fn check_assignment(assignment: &Assignment, context: &mut Context<'_, '_, '_>) {
    check_const_write(assignment.lhs, "assign to", context);
}

#[inline]
pub fn check_unary_prefix(unary_prefix: &UnaryPrefix, context: &mut Context<'_, '_, '_>) {
    match unary_prefix.operator {
        UnaryPrefixOperator::PreIncrement(_) => check_const_write(unary_prefix.operand, "increment", context),
        UnaryPrefixOperator::PreDecrement(_) => check_const_write(unary_prefix.operand, "decrement", context),
        _ => {}
    }
}

#[inline]
pub fn check_unary_postfix(unary_postfix: &UnaryPostfix, context: &mut Context<'_, '_, '_>) {
    match unary_postfix.operator {
        UnaryPostfixOperator::PostIncrement(_) => check_const_write(unary_postfix.operand, "increment", context),
        UnaryPostfixOperator::PostDecrement(_) => check_const_write(unary_postfix.operand, "decrement", context),
    }
}

/// Reports a write, such as `assign to` or `increment`, to a local declared with `const`.
///
/// A write after the local's block closed reports only its scope error.
fn check_const_write(target: &Expression, write: &str, context: &mut Context<'_, '_, '_>) {
    let Expression::ConstantAccess(target) = target else {
        return;
    };

    let span = target.name.span();
    let out_of_scope = context
        .names
        .binding_errors()
        .iter()
        .any(|error| matches!(error, BindingError::OutOfScope { name, .. } if *name == span));
    if out_of_scope {
        return;
    }

    if let Some(Binding::Local(local @ Local { kind: LocalKind::Const, .. })) = context.names.binding(&target.name) {
        let name = BytesDisplay(target.name.value());

        context.report(
            Issue::error(format!("Cannot {write} `{name}`: it is declared with `const`."))
                .with_annotation(Annotation::primary(target.span()).with_message("Changed here."))
                .with_annotation(Annotation::secondary(local.declaration).with_message("Declared with `const` here."))
                .with_help(format!("Declare `{name}` with `let` to change it.")),
        );
    }
}

/// Checks a local's name, and reports an empty literal, or a `let` that starts as `null`, declared without a type,
/// which has no type to give the local. Swift refuses an empty collection literal or `nil` without a type for the same
/// reason.
#[inline]
pub fn check_local_declaration(local_declaration: &LocalDeclaration, context: &mut Context<'_, '_, '_>) {
    for name in local_declaration.target.names() {
        check_local_name(name.value, name.span, "local", context);
    }

    // An unpacked tuple gives each local the type of its element.
    let (None, LocalTarget::Name(name)) = (local_declaration.hint, &local_declaration.target) else {
        return;
    };

    let (start, hint, value) = match local_declaration.value {
        Expression::Array(literal) if literal.elements.is_empty() && literal.colon.is_some() => {
            ("An empty literal", "Map<TKey, TValue>", "[:]")
        }
        Expression::Array(literal) if literal.elements.is_empty() => ("An empty literal", "List<T>", "[]"),
        // A `const` never takes another value, so `null` is all of its type.
        Expression::Literal(Literal::Null(_)) if !local_declaration.is_const() => ("A null start", "T?", "null"),
        _ => return,
    };

    let keyword = if local_declaration.is_const() { "const " } else { "" };
    let name = BytesDisplay(name.value);

    context.report(
        Issue::error(format!("{start} needs a type: write `{keyword}{hint} {name} = {value}`.")).with_annotation(
            Annotation::primary(local_declaration.value.span()).with_message("Declared without a type."),
        ),
    );
}

#[inline]
pub fn check_for_of(for_of: &ForOf, context: &mut Context<'_, '_, '_>) {
    for name in for_of.target.names() {
        check_local_name(name.value, name.span, "loop variable", context);
    }
}

#[inline]
pub fn check_type_pattern(type_pattern: &TypePattern, context: &mut Context<'_, '_, '_>) {
    if let Some(variable) = &type_pattern.variable {
        check_local_name(variable.value, variable.span, "pattern variable", context);
    }
}

#[inline]
pub fn check_try_catch_clause(try_catch_clause: &TryCatchClause, context: &mut Context<'_, '_, '_>) {
    if let Some(variable) = &try_catch_clause.variable
        && !variable.name.starts_with(b"$")
    {
        check_local_name(variable.name, variable.span, "catch variable", context);
    }
}

#[inline]
pub fn check_parameter(parameter: &FunctionLikeParameter, context: &mut Context<'_, '_, '_>) {
    if parameter.variable.name.starts_with(b"$") {
        report_unexpected(parameter.variable.span, context);
    } else {
        // A parameter with an access level or accessors also declares the property of its name.
        let kind = if parameter.is_promoted_property() { "property" } else { "parameter" };
        check_local_name(parameter.variable.name, parameter.variable.span, kind, context);
    }
}

/// Reports the PHP# scope rules the binder found broken.
#[inline]
pub fn check_binding_errors(context: &mut Context<'_, '_, '_>) {
    for error in context.names.binding_errors() {
        let issue = match *error {
            BindingError::OutOfScope {
                name,
                local: Local { declaration, kind: LocalKind::Pattern { test, negated } },
            } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));
                let test_text = BytesDisplay(context.get_code_snippet(test));
                let holds = if negated { "false" } else { "true" };

                Issue::error(format!("`{name_text}` exists only where `{test_text}` is {holds}."))
                    .with_annotation(Annotation::primary(name).with_message("Used here."))
                    .with_annotation(Annotation::secondary(declaration).with_message("Declared here."))
                    .with_help(format!(
                        "A pattern variable holds a value only where its pattern matches. Test the value again here, or declare a local for `{name_text}` before the test."
                    ))
            }
            BindingError::OutOfScope { name, local } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));

                Issue::error(format!("`{name_text}` is used after the block that declares it closes."))
                    .with_annotation(Annotation::primary(name).with_message("Used here."))
                    .with_annotation(Annotation::secondary(local.declaration).with_message("Declared here."))
                    .with_help(format!(
                        "A local lives until the `}}` that closes its block. Declare `{name_text}` before the block to use it after."
                    ))
            }
            BindingError::Redeclared { name, earlier } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));

                Issue::error(format!("`{name_text}` is already declared in an enclosing block of this method."))
                    .with_annotation(Annotation::primary(name).with_message("Declared again here."))
                    .with_annotation(Annotation::secondary(earlier.declaration).with_message("First declared here."))
                    .with_help("Rename one of the two locals. A block cannot redeclare a name its enclosing blocks declare, as in C#'s rule CS0136.")
            }
            BindingError::LocalAsType { name, local } => {
                let name_text = BytesDisplay(context.get_code_snippet(name));

                Issue::error(format!("`typeof` takes a type, and `{name_text}` is a local: write `Class.of({name_text})`."))
                    .with_annotation(Annotation::primary(name).with_message("Written here."))
                    .with_annotation(Annotation::secondary(local.declaration).with_message("Declared here."))
                    .with_note("`Class.of(value)` gives the class of the object a value holds, which may be a subclass of its type.")
            }
        };

        context.report(issue);
    }
}

/// Checks the name of a PHP# class, interface, enum or event against the type names the engine reserves, against the
/// `__Something__` names spec section 27 removes, and against spec section 24's capital letter. A name gets the first
/// of these errors only. Any other word names a type, a PHP keyword PHP# doesn't use included, and so does a
/// capitalized PHP# keyword, since PHP#'s keywords are lowercase but `Self`, which the engine reserves. The standard
/// library's `Sharp.Int`, `Sharp.Float` and `Sharp.Bool` are the engine's one exception, and the analyzer refuses
/// them in any file that is not the standard library's. A naming warning names the declaration as `kind`, `Type` or
/// `Event`.
#[inline]
pub fn check_class_name(class_name: &LocalIdentifier, kind: &str, context: &mut Context<'_, '_, '_>) {
    if is_reserved_class_name(class_name.value) && !is_sharp_type_class(context.get_name(class_name.span.start)) {
        let name = BytesDisplay(class_name.value);

        context.report(
            Issue::error(format!("Cannot use `{name}` as a class name: it is reserved."))
                .with_annotation(Annotation::primary(class_name.span).with_message("Class declared here."))
                .with_note("PHP# reserves this name for a type."),
        );
    } else if is_magic_name(class_name.value) || class_name.value.first().is_some_and(u8::is_ascii_uppercase) {
        check_declared_name(class_name.value, class_name.span, kind, NameCase::Pascal, context);
    } else {
        check_capitalized(class_name.value, class_name.span, context);
    }
}

/// Reports a type name or a namespace part a PHP# file gives that does not start with a capital letter, A to Z, as
/// spec section 24 writes every type but the built-in ones, and spec section 23 every part of a `namespace` line. The
/// parser reads the same letter to tell a type with type arguments, `Box<int>`, from a comparison.
fn check_capitalized(name: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    if name.first().is_some_and(u8::is_ascii_uppercase) {
        return;
    }

    let name = BytesDisplay(name);

    context.report(
        Issue::error(format!(
            "`{name}` must start with a capital letter: PHP# capitalizes every namespace and every type except the built-in ones."
        ))
        .with_annotation(Annotation::primary(span).with_message("Named here.")),
    );
}

/// Checks the namespace, classes and imports of a PHP# file: each part of the namespace starts with a capital letter,
/// and the classes and imports name no class twice, which the engine also checks when it compiles the file. An import's
/// namespace keeps its letters, because it names a namespace another file declares, often a plain PHP one.
///
/// Spec section 5: a namespace's inside is its folder, so a file an `autoload` PSR-4 folder holds declares the
/// namespace that folder maps its folder to, and no file declares its way into a namespace. A test folder, one
/// `autoload-dev` maps, may declare the namespace it tests.
#[inline]
pub fn check_declarations(program: &Program, context: &mut Context<'_, '_, '_>) {
    let Declarations { types, classes, imports } = declarations(program);
    check_one_type(&types, context);

    let name = first_namespace(program).and_then(|namespace| namespace.name.as_ref());

    if let Some(FolderNamespace::Namespace(folder)) = &context.source_file.folder_namespace
        && name.is_none_or(|name| name.value() != folder.as_bytes())
    {
        // A folder whose name no namespace can take, as `it's`, gives the file a namespace no file can declare.
        let unnamed = folder.split('.').find(|part| {
            !part.as_bytes().first().is_some_and(is_start_of_identifier)
                || !part.as_bytes().iter().all(is_part_of_identifier)
        });
        let issue = match unnamed {
            Some(part) => Issue::error(format!(
                "This file's folder `{part}` can't name a namespace: a namespace's inside is its folder, and a namespace takes letters, digits and `_`."
            ))
            .with_help(format!("Rename the folder `{part}` to the namespace it holds, as in `Store`.")),
            None => Issue::error(format!(
                "This file's folder holds the namespace `{folder}`, so the file declares `namespace {folder};`."
            )),
        }
        .with_note("A namespace's inside is its folder, so no file declares its way into another namespace.");

        context.report(match name {
            Some(name) => issue.with_annotation(Annotation::primary(name.span()).with_message("Declared here.")),
            None => issue.with_annotation(
                Annotation::primary(Span::new(context.source_file.id, Position::new(0), Position::new(0)))
                    .with_message("The file declares no namespace."),
            ),
        });
    }

    if let Some(name) = name {
        let mut start = 0;

        for part in name.value().split(|&byte| byte == b'.') {
            let end = start + part.len() as u32;

            let span = name.span().subspan(start, end);
            if part.first().is_some_and(u8::is_ascii_uppercase) {
                check_declared_name(part, span, "Namespace", NameCase::Pascal, context);
            } else {
                check_capitalized(part, span, context);
            }
            start = end + 1;
        }
    }

    let namespace = name.map(php_name);

    for (index, import) in imports.iter().enumerate() {
        let imported_name = imported_name(import);
        let full_name = BytesDisplay(import.name.value());
        let rename = format!(
            "Rename the import with `as`, as in `import {full_name} as Other{};`.",
            BytesDisplay(import.name.last_segment())
        );

        // A standard library type class imported under its own name is the type the name already gives.
        if is_reserved_class_name(imported_name)
            && !(imported_name.eq_ignore_ascii_case(import.name.last_segment())
                && is_sharp_type_class(&php_name(&import.name)))
        {
            let imported_name = BytesDisplay(imported_name);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{imported_name}`: PHP# reserves `{imported_name}` for a type."
                ))
                .with_annotation(Annotation::primary(import.span()).with_message("Imported here."))
                .with_help(rename.clone()),
            );
        } else if let Some(alias) = &import.alias {
            check_capitalized(alias.identifier.value, alias.identifier.span, context);
        }

        if let Some(earlier) =
            imports[..index].iter().find(|earlier| self::imported_name(earlier).eq_ignore_ascii_case(imported_name))
        {
            let earlier_full_name = BytesDisplay(earlier.name.value());

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{}`: `{earlier_full_name}` is already imported as `{}`.",
                    BytesDisplay(imported_name),
                    BytesDisplay(self::imported_name(earlier)),
                ))
                .with_annotation(Annotation::primary(import.span()).with_message("Imported again here."))
                .with_annotation(Annotation::secondary(earlier.span()).with_message("First imported here."))
                .with_note("Class names are case-insensitive.")
                .with_help(rename.clone()),
            );
        }

        // Importing the class the file declares names that class, which the engine accepts.
        if let Some(class) = classes.iter().find(|class| class.name.value.eq_ignore_ascii_case(imported_name))
            && !names_class(&php_name(&import.name), namespace.as_deref(), class.name.value)
        {
            let imported_name = BytesDisplay(imported_name);
            let class_name = BytesDisplay(class.name.value);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{imported_name}`: this file declares a class named `{class_name}`."
                ))
                .with_annotation(Annotation::primary(import.span()).with_message("Imported here."))
                .with_annotation(Annotation::secondary(class.name.span).with_message("Class declared here."))
                .with_help(rename),
            );
        }
    }
}

/// Spec section 27: a `.sharp` file that declares a type declares one, named like the file, case included, as Java
/// names a public class's file, so a reader finds a type by its namespace and its file. A type is a class, interface,
/// enum, struct or event. An `extern` line and a module are none, so an `extern` declaration file and
/// a `Module.sharp` hold no type, and `is_module_file_statement` refuses any type a module file declares.
///
/// The type named like the file is the file's own, and each other type moves to its own file. When none is, the
/// first type is the file's own, which takes the file's name only when that name can name a type.
fn check_one_type(types: &[&LocalIdentifier], context: &mut Context<'_, '_, '_>) {
    if Module::is_declared_in(context.source_file) || Module::environment_of(context.source_file).is_some() {
        return;
    }

    let file = context.source_file.name.rsplit(|&byte| byte == b'/').next().unwrap_or_default();
    let stem = file.strip_suffix(b".sharp").unwrap_or(file);
    let Some(own) = types.iter().find(|name| name.value == stem).or_else(|| types.first()) else {
        return;
    };

    if own.value != stem {
        let (file, name) = (BytesDisplay(file), BytesDisplay(own.value));
        let names_a_type = stem.first().is_some_and(u8::is_ascii_uppercase) && stem.iter().all(is_part_of_identifier);
        let or_the_type = if names_a_type { format!(", or the type `{}`", BytesDisplay(stem)) } else { String::new() };
        context.report(
            Issue::error(format!("`{file}` declares `{name}`: rename the file `{name}.sharp`{or_the_type}."))
                .with_annotation(Annotation::primary(own.span).with_message("Declared here.")),
        );
    }

    for other in types.iter().filter(|other| other.span != own.span) {
        let name = BytesDisplay(other.value);
        context.report(
            Issue::error(format!("A `.sharp` file declares one type: move `{name}` to `{name}.sharp`."))
                .with_annotation(Annotation::primary(other.span).with_message("A second type."))
                .with_annotation(Annotation::secondary(own.span).with_message("The file's type.")),
        );
    }
}

/// Checks a module's laws, spec section 28: each law's parameters as a class law's, and no two laws of one name,
/// compared ignoring case, as a class's methods and laws are. Only a folder's `Module.sharp` states laws, because an
/// environment or test module only overrides services, spec section 32.
#[inline]
pub fn check_module(module: &Module, context: &mut Context<'_, '_, '_>) {
    let mut stated: Vec<&LocalIdentifier> = Vec::new();
    for member in &module.members {
        let ModuleMember::Law(law) = member else {
            continue;
        };
        if module.parent.is_some() {
            context.report(
                Issue::error("A module that overrides another states no laws: state it in its folder's Module.sharp.")
                    .with_annotation(Annotation::primary(law.span()).with_message("Stated here.")),
            );
            continue;
        }
        check_law_parameters(law, context);

        if let Some(earlier) = stated.iter().find(|earlier| earlier.value.eq_ignore_ascii_case(law.name.value)) {
            context.report(
                Issue::error(format!("The module already states a law named `{}`.", BytesDisplay(earlier.value)))
                    .with_annotation(Annotation::primary(law.name.span).with_message("Stated again here."))
                    .with_annotation(Annotation::secondary(earlier.span).with_message("First stated here."))
                    .with_note("Law names are case-insensitive."),
            );
        } else {
            stated.push(&law.name);
        }
    }
}

/// Decides a module by its file and its header, spec section 32:
/// - a folder's `Module.sharp` holds its base module, which overrides nothing;
/// - a `Module.<Environment>.sharp` holds an environment module, which overrides its own folder's module;
/// - any other file holds a test module, which names the module it overrides.
fn check_module_file_name(module: &Module, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let issue = match (
        &module.parent,
        Module::is_declared_in(context.source_file),
        Module::environment_of(context.source_file),
    ) {
        (None, true, _) => return Some(Place::Module),
        (Some(parent), true, _) => {
            Issue::error("A folder's Module.sharp overrides no other module: remove its header.")
                .with_annotation(Annotation::primary(parent.span()).with_message("Written here."))
        }
        (None, false, Some(_)) => {
            let namespace = first_namespace(context.program).and_then(|namespace| namespace.name.as_ref());
            let header = namespace.map_or_else(String::new, |name| format!(" : {}", BytesDisplay(name.value())));
            Issue::error(format!("An environment module overrides its folder's module: write `module{header}`."))
                .with_annotation(Annotation::primary(module.module.span).with_message("Declared here."))
        }
        (Some(parent), false, Some(_)) => {
            let namespace = first_namespace(context.program).and_then(|namespace| namespace.name.as_ref());
            match namespace {
                Some(namespace) if context.names.get(namespace) == context.names.get(&parent.name) => {
                    return Some(Place::Module);
                }
                Some(namespace) => Issue::error(format!(
                    "An environment module overrides its own folder's module: write `module : {}`.",
                    BytesDisplay(namespace.value())
                ))
                .with_annotation(Annotation::primary(parent.name.span()).with_message("Names another module.")),
                None => Issue::error(
                    "An environment module overrides its own folder's module, so its file needs a namespace.",
                )
                .with_annotation(Annotation::primary(parent.name.span()).with_message("Names a module here.")),
            }
        }
        (Some(_), false, None) => return Some(Place::Module),
        (None, false, None) => Issue::error("A module is declared in its folder's Module.sharp.")
            .with_annotation(Annotation::primary(module.module.span).with_message("Declared here."))
            .with_help("A test module names the module it overrides, as in `module : App.Shop`."),
    };
    context.report(issue);

    None
}

/// Whether `statement` may stand at the top of the checked file: any statement outside a module file, and in
/// `Module.sharp` or `Module.<Environment>.sharp` its namespace, its imports and its first module, spec section 32.
fn is_module_file_statement(statement: &Statement, context: &Context<'_, '_, '_>) -> bool {
    if !Module::is_declared_in(context.source_file) && Module::environment_of(context.source_file).is_none() {
        return true;
    }

    match statement {
        Statement::Namespace(_) | Statement::Use(_) => true,
        Statement::Module(module) => file_statements(context.program)
            .find_map(|statement| match statement {
                Statement::Module(first) => Some(first.span()),
                _ => None,
            })
            .is_some_and(|first| first == module.span()),
        _ => false,
    }
}

/// The name an import gives its class in the file: the name after `as`, or else the last segment of the class name.
fn imported_name<'arena>(import: &UseItem<'arena>) -> &'arena [u8] {
    import.alias.as_ref().map_or_else(|| import.name.last_segment(), |alias| alias.identifier.value)
}

#[inline]
pub fn check_constant_access(constant_access: &ConstantAccess, context: &mut Context<'_, '_, '_>) {
    match context.names.binding(&constant_access.name) {
        Some(Binding::Member) => report_bare_member(constant_access.span(), constant_access.name.value(), context),
        // `TRUE` is `true` written in another case.
        Some(Binding::Constant) if !is_in_file_level_code(context) => {
            let name = constant_access.name.value();
            if let Some(literal) =
                ["true", "false", "null"].into_iter().find(|literal| literal.as_bytes().eq_ignore_ascii_case(name))
            {
                context.report(
                    Issue::error(format!("Write `{literal}` in lowercase."))
                        .with_annotation(Annotation::primary(constant_access.span()).with_message("Written here.")),
                );
            }
        }
        _ => {}
    }
}

#[inline]
pub fn check_function_call(function_call: &FunctionCall, context: &mut Context<'_, '_, '_>) {
    let Expression::Identifier(identifier) = function_call.function else {
        return;
    };

    if identifier.last_segment().eq_ignore_ascii_case(b"assert")
        && let [Argument::Positional(argument)] = function_call.argument_list.arguments.as_slice()
        && argument.ellipsis.is_some()
    {
        context.report(
            Issue::error("Cannot use positional argument after argument unpacking.")
                .with_annotation(
                    Annotation::primary(argument.span()).with_message("The only argument of `assert` is a spread."),
                )
                .with_note("PHP compiles `assert` with one argument by adding its text as a positional description.")
                .with_help("Pass the condition without `...`, or pass the description by name after the spread."),
        );
    }
}

#[inline]
pub fn check_function(function: &Function, context: &mut Context<'_, '_, '_>) {
    report_unexpected(function.name.span, context);
}

#[inline]
pub fn check_global(global: &Global, context: &mut Context<'_, '_, '_>) {
    if !is_in_file_level_code(context) {
        report_unexpected(global.global.span, context);
    }
}

/// Checks `object.member` and `object.member()` once per chain of property reads, from its outermost access. The
/// member name spans of the accesses it checks go into the context's `slice_members`, because each access has its own
/// member name and an access's own span grows with the chain before it.
///
/// A chain rooted at a class reaches its constants, enum cases and static members, and one rooted at a class value,
/// `typeof(X)` or a local holding one, reaches those of the class it holds. One file cannot tell a full name,
/// `App.Status`, from a class and its member, `Status.Active`, so the analyzer reports a full name.
fn check_member_access(object: &Expression, member: &ClassLikeMemberSelector, context: &mut Context<'_, '_, '_>) {
    if !context.slice_members.insert(member.span()) {
        return;
    }

    let mut root = object;
    while let Expression::Access(Access::Property(property)) = root
        && let ClassLikeMemberSelector::Identifier(name) = &property.property
    {
        context.slice_members.insert(name.span);
        root = property.object;
    }
}

/// A class, a struct or an enum a PHP# file declares. The slice's class rules treat them alike, and a struct and an
/// enum declare no type parameters.
#[derive(Clone, Copy)]
struct ClassLike<'ast, 'arena> {
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    type_parameters: Option<&'ast TypeParameterList<'arena>>,
    members: &'ast Sequence<'arena, ClassLikeMember<'arena>>,
    is_static: bool,
    is_foreign: bool,
}

impl<'ast, 'arena> ClassLike<'ast, 'arena> {
    fn of_class(class: &'ast Class<'arena>) -> Self {
        Self {
            name: &class.name,
            span: class.span(),
            type_parameters: class.type_parameters.as_ref(),
            members: &class.members,
            is_static: class.modifiers.contains_static(),
            is_foreign: class.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Foreign(_))),
        }
    }

    fn of_struct(r#struct: &'ast Struct<'arena>) -> Self {
        Self {
            name: &r#struct.name,
            span: r#struct.span(),
            type_parameters: r#struct.type_parameters.as_ref(),
            members: &r#struct.members,
            is_static: false,
            is_foreign: false,
        }
    }
}

/// What a PHP# file declares at its top level and in its first namespace, each in source order.
#[derive(Default)]
struct Declarations<'ast, 'arena> {
    /// The name of each type: a class, interface, enum, struct or event.
    types: Vec<&'ast LocalIdentifier<'arena>>,
    /// Each class, struct and enum.
    classes: Vec<ClassLike<'ast, 'arena>>,
    imports: Vec<&'ast UseItem<'arena>>,
}

impl<'ast, 'arena> Declarations<'ast, 'arena> {
    fn collect(&mut self, statement: &'ast Statement<'arena>) {
        match statement {
            Statement::Class(class) => {
                self.types.push(&class.name);
                self.classes.push(ClassLike::of_class(class));
            }
            Statement::Struct(r#struct) => {
                self.types.push(&r#struct.name);
                self.classes.push(ClassLike::of_struct(r#struct));
            }
            Statement::Enum(r#enum) => {
                self.types.push(&r#enum.name);
                self.classes.push(ClassLike {
                    name: &r#enum.name,
                    span: r#enum.span(),
                    type_parameters: None,
                    members: &r#enum.members,
                    is_static: false,
                    is_foreign: false,
                });
            }
            Statement::Interface(interface) => self.types.push(&interface.name),
            Statement::Event(event) => self.types.push(&event.name),
            Statement::Use(Use { items: UseItems::Sequence(sequence), .. }) => {
                self.imports.extend(sequence.items.iter())
            }
            _ => {}
        }
    }
}

fn declarations<'ast, 'arena>(program: &'ast Program<'arena>) -> Declarations<'ast, 'arena> {
    let mut declarations = Declarations::default();
    // The slice refuses a second namespace, so its declarations are not the file's.
    let namespaced = first_namespace(program).into_iter().flat_map(|namespace| namespace.statements().iter());
    for statement in program.statements.iter().chain(namespaced) {
        declarations.collect(statement);
    }

    declarations
}

/// Checks the name of a PHP# local or parameter, and a `__Something__` name, which spec section 27 removes. A keyword
/// names one only with `@` before it, spec section 2. The name of `@event` is `event`, and its span holds the `@` too,
/// and the name of `@this` keeps its `@`.
fn check_local_name(name: &[u8], span: Span, kind: &str, context: &mut Context<'_, '_, '_>) {
    let marked = span.length() as usize > name.len();
    if is_sharp_keyword(name) && !marked {
        let name = BytesDisplay(name);

        context.report(
            Issue::error(format!("`{name}` is a keyword; write `@{name}`."))
                .with_annotation(Annotation::primary(span).with_message("Declared here.")),
        );
    } else {
        let mut kind = kind.to_owned();
        kind[..1].make_ascii_uppercase();

        check_declared_name(name.strip_prefix(b"@").unwrap_or(name), span, &kind, NameCase::Camel, context);
    }
}

/// Refuses, in an enum that runs as classes, spec sections 20 and 31, a case named as a type PHP# reserves, since each
/// case runs as a class of its name, and a method named as a case that carries data, ignoring case as PHP does, since
/// `PaymentResult.Paid(…)` creates that case.
fn check_case_class_names(r#enum: &mago_syntax::cst::Enum, context: &mut Context<'_, '_, '_>) {
    if !r#enum.runs_as_classes() {
        return;
    }

    for member in &r#enum.members {
        let ClassLikeMember::EnumCase(case) = member else {
            continue;
        };
        let name = case.item.name();
        if is_reserved_class_name(name.value) {
            context.report(
                Issue::error(format!(
                    "A case of an enum whose cases carry data or type arguments is a class, so it can't be named `{}`.",
                    BytesDisplay(name.value)
                ))
                .with_annotation(Annotation::primary(name.span).with_message("Case declared here."))
                .with_note("PHP# reserves this name for a type."),
            );
        }

        let method = r#enum.members.iter().find_map(|member| match member {
            ClassLikeMember::Method(method) if method.name.value.eq_ignore_ascii_case(name.value) => Some(method),
            _ => None,
        });
        if let (mago_syntax::cst::EnumCaseItem::Data(_), Some(method)) = (&case.item, method) {
            context.report(
                Issue::error(format!(
                    "The method `{}` shares its name with the case `{}`, which `{}.{}(…)` creates.",
                    BytesDisplay(method.name.value),
                    BytesDisplay(name.value),
                    BytesDisplay(r#enum.name.value),
                    BytesDisplay(name.value),
                ))
                .with_annotation(Annotation::primary(method.name.span).with_message("Method declared here."))
                .with_annotation(Annotation::secondary(name.span).with_message("Case declared here."))
                .with_help("Rename the method."),
            );
        }
    }
}

/// PHP# reserves PHP's type names and `Any`, its name for `mixed`.
fn is_reserved_class_name(name: &[u8]) -> bool {
    RESERVED_CLASS_NAMES.iter().chain(&[ANY]).any(|reserved| reserved.eq_ignore_ascii_case(name))
}

/// Whether a PHP name is one of the standard library's classes that carry a reserved name, spec section 24, written
/// exactly as the engine's `zend_is_sharp_type_class` compares it.
fn is_sharp_type_class(full_name: &[u8]) -> bool {
    matches!(full_name, b"Sharp\\Int" | b"Sharp\\Float" | b"Sharp\\Bool")
}

/// Returns true when the PHP name `full_name` is the class `class_name` declared in `namespace`.
fn names_class(full_name: &[u8], namespace: Option<&[u8]>, class_name: &[u8]) -> bool {
    let (import_namespace, import_class) = match full_name.iter().rposition(|&byte| byte == b'\\') {
        Some(separator) => (&full_name[..separator], &full_name[separator + 1..]),
        None => (&full_name[..0], full_name),
    };

    import_class.eq_ignore_ascii_case(class_name)
        && import_namespace.eq_ignore_ascii_case(namespace.unwrap_or_default())
}

/// Checks the object of `object?.member`. A class is never null, so `?.` after a class is an error that names `.`. A
/// superglobal or a `__Something__` name binds as a class there too, and the walk refuses it at the object.
fn check_null_safe_object(
    access: Span,
    object: &Expression,
    member: &ClassLikeMemberSelector,
    context: &mut Context<'_, '_, '_>,
) -> Option<Place> {
    if let Expression::ConstantAccess(class) = object
        && context.names.binding(&class.name) == Some(Binding::Class)
        && superglobal_span(object, context).is_none()
        && magic_name(object, context).is_none()
        && let ClassLikeMemberSelector::Identifier(member) = member
    {
        let class = BytesDisplay(class.name.value());

        context.report(
            Issue::error(format!(
                "`{class}` is a class, which is never null: write `{class}.{}`.",
                BytesDisplay(member.value)
            ))
            .with_annotation(Annotation::primary(access).with_message("Null-safe access written here.")),
        );

        return None;
    }

    Some(Place::Body)
}

fn report_bare_member(span: Span, name: &[u8], context: &mut Context<'_, '_, '_>) {
    let is_case =
        |member: &ClassLikeMember| matches!(member, ClassLikeMember::EnumCase(case) if case.item.name().value == name);
    let class = enclosing_class(context.program, span);
    let message = if let Some(r#enum) = class.filter(|class| class.members.iter().any(is_case)) {
        format!(
            "Write `{}.{}`: an enum case is reached through its enum's name.",
            BytesDisplay(r#enum.name.value),
            BytesDisplay(name)
        )
    } else if let Some(class) = class.filter(|class| is_static_member(*class, name)) {
        format!(
            "Write `{}.{}`: a static member is reached through its class name.",
            BytesDisplay(class.name.value),
            BytesDisplay(name)
        )
    } else {
        let (name, call) = match enclosing_class_method(context.program, span, name) {
            Some(method) => (BytesDisplay(method.name.value), "()"),
            None => (BytesDisplay(name), ""),
        };

        match enclosing_static_method_class(context.program, span) {
            Some(class) => format!(
                "Write `{}.{name}{call}`: a static method reaches the members of its class through the class name.",
                BytesDisplay(class.name.value)
            ),
            None => format!("Write `this.{name}{call}`: members of the same object are always written with `this.`."),
        }
    };

    context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message("Used here.")));
}

/// The class of the static method, the static property or the law whose body holds `span`, which has no `this`.
fn enclosing_static_method_class<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
) -> Option<ClassLike<'ast, 'arena>> {
    let class = enclosing_class(program, span)?;

    class
        .members
        .iter()
        .any(|member| match member {
            ClassLikeMember::Method(method) => {
                method.span().contains(&span.start)
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Static(_)))
            }
            ClassLikeMember::Property(property) => {
                property.span().contains(&span.start) && property.modifiers().contains_static()
            }
            ClassLikeMember::Law(law) => law.span().contains(&span.start),
            _ => false,
        })
        .then_some(class)
}

/// The method named `name` of the class whose body holds `span`. Method names are case-insensitive.
fn enclosing_class_method<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
    name: &[u8],
) -> Option<&'ast Method<'arena>> {
    enclosing_class(program, span)?.members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if method.name.value.eq_ignore_ascii_case(name) => Some(method),
        _ => None,
    })
}

/// Decides `yield`, spec section 12. `yield value;` and `yield ...other;` are statements of a method that returns
/// `Iterable<T>`, outside any lambda in it, because `yield` gives no value back. PHP's `yield from other`,
/// `yield key => value` and a bare `yield` are no PHP# syntax. Each error is reported at the `yield` keyword, so the
/// value is still checked.
fn check_yield(r#yield: &Yield, context: &mut Context<'_, '_, '_>) {
    let span = r#yield.span();
    let keyword = match r#yield {
        Yield::From(YieldFrom { r#yield, .. })
        | Yield::Pair(YieldPair { r#yield, .. })
        | Yield::Value(YieldValue { r#yield, value: None }) => return report_unexpected(r#yield.span, context),
        Yield::Value(YieldValue { r#yield, .. }) | Yield::Spread(YieldSpread { r#yield, .. }) => r#yield.span,
    };

    let Some(method) = enclosing_iterator_method(context.program, span) else {
        return context.report(
            Issue::error("`yield` needs a method that returns `Iterable<T>`.")
                .with_annotation(Annotation::primary(keyword).with_message("Written here."))
                .with_note("A method that returns `Iterable<T>` produces its values with `yield value;` and `yield ...other;`. A lambda never yields."),
        );
    };

    let is_statement = !Node::Method(method)
        .filter_map(|node| match node {
            Node::ExpressionStatement(statement) if statement.expression.span() == span => Some(()),
            _ => None,
        })
        .is_empty();
    if !is_statement {
        context.report(
            Issue::error("`yield` gives no value back: write it as a statement, as in `yield value;`.")
                .with_annotation(Annotation::primary(keyword).with_message("Used as a value here."))
                .with_note("PHP's `send()` and two-way generators are not part of PHP#."),
        );
    }
}

/// The method that returns `Iterable<T>` and whose body holds `span` outside any lambda in it, in a class or in one of
/// its extension blocks.
fn enclosing_iterator_method<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<&'ast Method<'arena>> {
    let members = match enclosing_extension(program, span) {
        Some(extension) => &extension.members,
        None => enclosing_class(program, span)?.members,
    };
    let method = members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if method.span().contains(&span.start) => Some(method),
        _ => None,
    })?;
    let in_lambda = !Node::Method(method)
        .filter_map(|node| {
            (matches!(node, Node::ArrowFunction(_) | Node::Closure(_)) && node.span().contains(&span.start))
                .then_some(())
        })
        .is_empty();

    (returns_iterable(method) && !in_lambda).then_some(method)
}

/// Whether a method's declared return type is `Iterable<T>`. A method that yields gives one, which is never null, so
/// `Iterable<T>?` is not one.
fn returns_iterable(method: &Method) -> bool {
    method.return_type_hint.as_ref().is_some_and(
        |return_type| matches!(&return_type.hint, Hint::Generic(generic) if generic.name.value == b"Iterable"),
    )
}

/// Reports `return value;` in a method that returns `Iterable<T>` and yields, outside the lambdas in it. Its values
/// are the ones it yields, spec section 12, as C#'s CS1622 says: "Cannot return a value from an iterator". A bare
/// `return;` ends it.
fn check_iterator_returns(method: &Method, context: &mut Context<'_, '_, '_>) {
    if !returns_iterable(method) {
        return;
    }

    let mut lambdas = Vec::new();
    let mut yields = Vec::new();
    let mut returns = Vec::new();
    for node in Node::Method(method).filter_map(|node| match node {
        Node::ArrowFunction(_) | Node::Closure(_) | Node::Yield(_) | Node::Return(Return { value: Some(_), .. }) => {
            Some(*node)
        }
        _ => None,
    }) {
        match node {
            Node::Yield(_) => yields.push(node.span()),
            Node::Return(_) => returns.push(node.span()),
            _ => lambdas.push(node.span()),
        }
    }

    let outside_lambdas = |span: &Span| !lambdas.iter().any(|lambda| lambda.contains(&span.start));
    if !yields.iter().any(outside_lambdas) {
        return;
    }

    for span in returns.into_iter().filter(outside_lambdas) {
        context.report(
            Issue::error("A method that yields cannot return a value: write `return;` to end it.")
                .with_annotation(Annotation::primary(span).with_message("Returns a value here."))
                .with_note("A method that yields produces only the values it yields, as in C#."),
        );
    }
}

/// Reports each read of `this`, `field`, a parameter or a type parameter in `default`, a parameter's default, outside a
/// lambda's own parameters. Spec section 9: the default runs before the method, each time a call leaves the argument
/// out, so no object, argument or type argument is there yet. A bare member read already has its own error.
fn check_default_reads(default: &Expression, context: &mut Context<'_, '_, '_>) {
    // A name's binding is kept at its first byte, where the expressions it starts begin too. The walk gives a node's
    // children before it, so the first node at that byte is the smallest, the name.
    let mut reads = BTreeMap::<u32, Span>::new();
    for span in Node::Expression(default).filter_map(|node| Some(node.span())) {
        let unavailable = match context.names.binding(&span.start) {
            Some(Binding::This | Binding::Field | Binding::TypeParameter { .. }) => true,
            Some(Binding::Local(local)) => !default.span().contains(&local.declaration.start),
            _ => false,
        };
        if unavailable {
            reads.entry(span.start.offset).or_insert(span);
        }
    }
    for read in reads.into_values() {
        context.report(
            Issue::error(format!(
                "A parameter's default runs before the method, so `{}` is not there yet.",
                BytesDisplay(context.get_code_snippet(read))
            ))
            .with_annotation(Annotation::primary(read).with_message("Read here."))
            .with_help("Compute the value in the method from a nullable parameter, as in `int? limit = null`."),
        );
    }
}

/// Whether `name` is a constant, or a static field or property, of `class`.
fn is_static_member(class: ClassLike, name: &[u8]) -> bool {
    is_static_property(class, name)
        || class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Constant(constant)
                if constant.items.iter().any(|item| item.name.value == name))
        })
}

/// Whether `name` is a static field or property of `class`.
fn is_static_property(class: ClassLike, name: &[u8]) -> bool {
    class.members.iter().any(|member| {
        matches!(member, ClassLikeMember::Property(property)
            if property.modifiers().contains_static()
                && property.variables().iter().any(|variable| variable.name == name))
    })
}

/// Reports `Class.name` in a constant expression when this file declares `name` as a static field or property of
/// `Class`, because PHP cannot read a static property there.
fn report_static_member_in_constant(
    class: &ConstantAccess,
    member: &LocalIdentifier,
    span: Span,
    context: &mut Context<'_, '_, '_>,
) {
    let declared = declarations(context.program).classes.into_iter().any(|declared| {
        declared.name.value.eq_ignore_ascii_case(class.name.value()) && is_static_property(declared, member.value)
    });
    if !declared {
        return;
    }

    context.report(
        Issue::error(format!(
            "`{}.{}` is a static member, which a constant value cannot read.",
            BytesDisplay(class.name.value()),
            BytesDisplay(member.value)
        ))
        .with_annotation(Annotation::primary(span).with_message("Read here."))
        .with_note(
            "A default, a constant's value, a constant initial value and a pattern read constants and enum cases only.",
        ),
    );
}

/// The class or enum of a PHP# file whose body holds `span`, in any of the file's namespaces, because the walk checks
/// the classes of a namespace it refuses too.
fn enclosing_class<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<ClassLike<'ast, 'arena>> {
    let mut declarations = Declarations::default();
    for statement in file_statements(program) {
        declarations.collect(statement);
    }

    declarations.classes.into_iter().find(|class| class.span.contains(&span.start))
}

/// Whether `value` is the initial value of a member that runs as code, a static member's once the class is built and
/// an instance member's in the constructor, which `Property::initial_value_runs_as_code` decides for the checker, the
/// analyzer and the engine alike.
fn is_initial_value_run_as_code(value: &Expression, context: &Context<'_, '_, '_>) -> bool {
    enclosing_class(context.program, value.span()).is_some_and(|class| {
        class.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Property(property)
                if property.initial_value().is_some_and(|initial_value| initial_value.span() == value.span())
                    && property.initial_value_runs_as_code())
        })
    })
}

/// The name of the class, struct, interface or enum of a PHP# file whose body holds `span`, in any of the file's
/// namespaces.
fn enclosing_class_like_name<'arena>(program: &Program<'arena>, span: Span) -> Option<&'arena [u8]> {
    file_statements(program).find_map(|statement| match statement {
        Statement::Class(class) if class.span().contains(&span.start) => Some(class.name.value),
        Statement::Struct(r#struct) if r#struct.span().contains(&span.start) => Some(r#struct.name.value),
        Statement::Interface(interface) if interface.span().contains(&span.start) => Some(interface.name.value),
        Statement::Enum(r#enum) if r#enum.span().contains(&span.start) => Some(r#enum.name.value),
        _ => None,
    })
}

/// Whether `span` is in an interface of a PHP# file, in any of the file's namespaces.
fn is_in_interface(program: &Program, span: Span) -> bool {
    file_statements(program)
        .any(|statement| matches!(statement, Statement::Interface(interface) if interface.span().contains(&span.start)))
}

/// The extension block of a PHP# file's static class whose body holds `span`, in any of the file's namespaces.
fn enclosing_extension<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<&'ast Extension<'arena>> {
    file_statements(program).find_map(|statement| match statement {
        Statement::Class(class) => class.members.iter().find_map(|member| match member {
            ClassLikeMember::Extension(extension) if extension.span().contains(&span.start) => Some(extension),
            _ => None,
        }),
        _ => None,
    })
}

/// The `readonly` written on a field's, a property's or a member-declaring parameter's storage, spec section 13: as
/// its modifier, or before its type.
fn storage_readonly(modifiers: &Sequence<'_, Modifier<'_>>, hint: Option<&Hint<'_>>) -> Option<Span> {
    modifiers.iter().find(|modifier| matches!(modifier, Modifier::Readonly(_))).map(HasSpan::span).or(match hint {
        Some(Hint::Readonly(readonly)) => Some(readonly.readonly.span),
        _ => None,
    })
}

/// Reports `readonly` on storage, spec section 13, with the get-only property that keeps the value, written with the
/// member's own access modifier, type and name.
fn report_readonly_storage(
    member: &str,
    readonly: Span,
    visibility: Option<&Modifier<'_>>,
    hint: Option<&Hint<'_>>,
    name: &[u8],
    context: &mut Context<'_, '_, '_>,
) {
    let visibility = visibility.map_or("private", |visibility| match visibility {
        Modifier::Public(_) => "public",
        Modifier::Protected(_) => "protected",
        _ => "private",
    });
    let written = match hint {
        Some(Hint::Readonly(readonly)) => Some(readonly.hint),
        hint => hint,
    };
    let written = written
        .map_or_else(String::new, |hint| format!("{} ", String::from_utf8_lossy(context.get_code_snippet(hint))));
    let name = BytesDisplay(name.strip_prefix(b"$").unwrap_or(name));
    context.report(
        Issue::error(format!(
            "{member} is never `readonly`: declare a get-only property, as in `{visibility} {written}{name} {{ get; }}`."
        ))
        .with_annotation(Annotation::primary(readonly).with_message("Written here."))
        .with_note("`readonly` makes a value only read. A get-only property already cannot be written."),
    );
}

/// Checks `readonly` before a type, spec section 13: before a type whose value never changes it changes nothing, and
/// `Any` can hold a plain PHP object, which PHP# does not check. Returns whether the type passes.
fn check_readonly_type(readonly: &ReadonlyHint<'_>, context: &mut Context<'_, '_, '_>) -> bool {
    let value = match readonly.hint {
        Hint::Nullable(nullable) => nullable.hint,
        hint => hint,
    };
    let message = match value {
        Hint::Readonly(_) => "`readonly` is written once.".to_owned(),
        Hint::Mixed(_) => {
            "`readonly` holds only types PHP# checks, and `Any` can hold a plain PHP object: write a PHP# type."
                .to_owned()
        }
        Hint::Integer(_) | Hint::Float(_) | Hint::Bool(_) | Hint::String(_) | Hint::Void(_) | Hint::Never(_) => {
            let written = String::from_utf8_lossy(context.get_code_snippet(readonly.hint)).into_owned();
            let article = if matches!(value, Hint::Integer(_)) { "An" } else { "A" };

            format!("{article} `{written}` never changes, so `readonly` changes nothing: write `{written}`.")
        }
        _ => return true,
    };
    let span = match value {
        Hint::Readonly(inner) => inner.readonly.span,
        _ => readonly.readonly.span,
    };
    context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message("Written here.")));

    false
}

/// Why the `readonly` after `method`'s parameters, spec section 13, can't matter in `place`: the method has no `this`
/// to keep, or the language keeps it readonly already. None when it keeps `this` readonly.
fn needless_readonly_marker(method: &Method<'_>, place: Place) -> Option<&'static str> {
    if method.is_static() {
        return Some("A static method has no `this` to keep readonly: remove `readonly`.");
    }
    if php_method_name(method) == b"__construct" {
        return Some("A constructor sets `this`, so it is never readonly: remove `readonly`.");
    }
    if place == Place::Enum {
        return Some("An enum case never changes, so an enum method needs no `readonly`: remove it.");
    }

    match method.name.value {
        _ if !MethodParts::of_method(method).is_hash_or_to_string() => None,
        b"hash" => Some("`hash()` is readonly by definition: remove `readonly`."),
        _ => Some("`toString()` is readonly by definition: remove `readonly`."),
    }
}
