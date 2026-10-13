use mago_bytes::BytesDisplay;
use mago_names::ResolvedNames;
use mago_names::STANDARD_EFFECTS;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::binding::php_method_name;
use mago_names::binding::php_operator_name;
use mago_names::scope::php_name;
use mago_names::short_name;
use mago_php_version::PHPVersion;
use mago_reporting::Annotation;
use mago_reporting::AnnotationKind;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::BinaryPattern;
use mago_syntax::cst::Block;
use mago_syntax::cst::Break;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ComparisonPattern;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Construct;
use mago_syntax::cst::ConstructorInitializer;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DeclareBody;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extern;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Global;
use mago_syntax::cst::Hint;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::MagicConstant;
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
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Terminator;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TypePattern;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::UnionHint;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Uses;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::WhileBody;
use mago_syntax::token::GetPrecedence;
use mago_syntax::token::Precedence;
use mago_syntax::token::is_sharp_keyword;
use mago_syntax::utils::pattern::has_type_test;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::internal::checker::class_like::check_law_parameters;
use crate::internal::consts::RESERVED_CLASS_NAMES;
use crate::internal::context::Context;

/// The PHP superglobals, which spec section 29 removes. A PHP# local or parameter of one of these names would read or
/// replace it.
const SUPERGLOBALS: [&[u8]; 9] =
    [b"GLOBALS", b"_SERVER", b"_GET", b"_POST", b"_FILES", b"_COOKIE", b"_SESSION", b"_REQUEST", b"_ENV"];

/// How PHP# writes PHP's `mixed`. The parser reads both as `Hint::Mixed`, and only `Any` is PHP#.
const ANY: &[u8] = b"Any";

/// Checks a PHP# file against the slice: the only constructs a `.sharp` file may use, and the contract the
/// engine's lowering implements.
///
/// - At file level: `namespace`, `import`, `class`, `interface` and `enum`. A file has at most one namespace, named
///   and written without braces.
/// - A class: attributes, an optional `public`, `abstract` or `final`, a name, an optional `: Base, Interface` header,
///   constants, fields, properties, methods, named constructors, operators and laws, with no other modifiers, `extends`
///   or `implements`. The engine tells the base class from the interfaces when it links the class.
/// - A named constructor, spec section 9.1: an access modifier, its class's name, a `.` and its own name, parameters
///   that declare no property, `: this(…)` without `this` in its arguments, and a body. `check_named_constructor`
///   decides each part, and an interface, an enum and a static class declare none.
/// - A law, spec section 28, in a class or an enum: `law`, a name, parameters checked as a method's and an expression
///   body. `check_members` refuses a default and a variadic parameter, because a law's parameters range over every
///   value.
/// - A static class, `public static class Text`, as spec sections 26 and 29 write it: `static` and an optional
///   `public`, no header and no constructor, and only constants and static members. The bridge lowers it to a final
///   PHP class.
/// - An interface: an optional `public`, a name, an optional `: Interface` header, methods and get-only properties,
///   with no attributes, other modifiers or `extends`. An interface method has parameters, a return type and no body,
///   as spec section 29 writes `Money quote(Cart cart);`, and a property is written `int orderId { get; }`. Every
///   interface member is public, written bare, so an access modifier on one is an error, and so is any other modifier
///   on a method.
/// - An enum: attributes, an optional `public`, a name, an optional `: string, Interface` header, constants, cases,
///   methods and laws, with no other modifiers or `implements`. A leading `int` or `string` in the header is the backing type,
///   and every class name is an interface. A constant follows a class constant's rules. A case has attributes as a
///   class has them, and is `case Active;` or, in a backed enum, `case Active = "a";`, whose value is a constant
///   expression. A case named `class`, compared ignoring case, is an error, as in PHP. A method follows a class's
///   method rules, but an enum has no constructor, so a method without a return type is an error. PHP's `check_enum`
///   reports a property and any other backing type, and the analyzer a class name in the header.
/// - A header of a class, an interface or an enum that names one type twice is an error, and so is an enum header that
///   names `UnitEnum` or `BackedEnum`, because the engine refuses both when it declares the class.
/// - Attributes: on a class, an enum, an enum case, a method, a field, a property and a parameter, written `[Name]` or
///   `[Name(arguments)]`, several in one list, as in `[Field("Name"), Searchable]`, or in several lists. An argument
///   is positional or named, and is a constant expression as a parameter default is, list and map literals included.
///   PHP's own check reports a spread argument. An attribute target, as in `[return: NotNull]`, is a parse error.
/// - A constant: `public`, `protected` or `private`, an optional type from the types below but `void`, one name, and a
///   value as a parameter default is.
/// - A field: `private` or `protected`, an optional `static`, a type, one name and an optional initial value. A
///   `public` field is an error, because spec section 6 has no public fields.
/// - An override: a field written `override`, which replaces a plain PHP parent's property, spec section 6.1. It may
///   be `public`, as the parent's property is, and has a constant initial value. The analyzer checks the parent.
/// - A static field or property: an initial value that is constant, which PHP stores as its default, and a `set`
///   accessor, because PHP has no `readonly` static property.
/// - An initial value: any expression a method body has, without `this`, which is an error. A constant initial value,
///   as a parameter default is, becomes the member's default, and any other runs at the start of the constructor.
/// - A property: `public`, `protected` or `private`, an optional `static`, a type, one name, the accessors `get` and
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
///   storage there, while C# calls the accessor again. A static property with a body is not supported yet, because
///   PHP has no hooks on a static property, and neither is a non-constant initial value on a property whose `set` has
///   a body, which would run the body in the constructor.
/// - A computed property: `public`, `protected` or `private`, a type, one name and an expression body, as in
///   `public string slug => Str.slug(name);`. Its expression is a method body's expression and runs on each read. A
///   `static` computed property is not supported yet, because PHP has no hooks on a static property.
/// - A method: `public`, `protected` or `private`, an optional `static`, `virtual` or `override`, parameters, a return
///   type and a body, or `abstract` and no body. The analyzer decides `virtual` and `override`, as spec section 22
///   writes them. Its name does not start with `__`, which PHP reserves for magic methods, and is not its class's name,
///   compared ignoring case, which PHP# gives to the constructor, nor, compared ignoring case, a property's of its
///   class.
/// - An operator, as spec section 19 writes `public static Money operator +(Money a, Money b) => …;`: `public static`,
///   a return type, `operator`, one of `+ - * / % **`, unary `-`, `==` and `<=>`, parameters and a method body.
///   `!=` derives from `==`, and `< > <= >=` from `<=>`, so declaring one is an error that names its source. `==`,
///   `<=>` and the binary arithmetic operators take two parameters, unary `-` one, and one of them is the declaring
///   class. `==` returns `bool`, and `<=>` `int`. A class declares each operator once, because each runs as one
///   static method named after .NET's, such as `op_Addition`. An interface or an enum declares no operator.
/// - An `extern` method, `public static extern string slug(string title);`, whose body is native, compiled into the
///   engine, as spec section 29 writes it: a `public static` method of a static class, with no body. Only the
///   standard library declares one, and the analyzer refuses one anywhere else.
/// - The constructor: a method named exactly after its class, without a return type and not `static`. A method
///   without a return type named otherwise is an error. A constructor parameter with an access modifier declares a
///   member: a field when `private` or `protected` without accessors, and a property with accessors, which follow the
///   auto-property rules. A `public` parameter without accessors is an error, as spec section 9 says. The constructor
///   alone may be `required`, which keeps it callable as `new Self(…)` on every subclass, spec section 25. The parser
///   reports `required` on any other member as not supported yet. A class has one constructor, and `check_members`
///   reports a second one, because PHP# has no overloading by argument types. A parameter cannot be `lazy`, because a
///   lazy property computes its own value, and `lazy` anywhere else is not supported yet. The constructor passes
///   control to its base class with `: super(…)`, whose arguments are a method body's expressions without `this`,
///   because they run before the object exists, as C#'s CS0027 says. `: this(…)` belongs to a named constructor, so on
///   the main one it is an error. The analyzer checks the arguments against the base class's constructor. A
///   constructor runs once, when `new` creates the object, so a call of a member named `__construct` is an error.
/// - A parameter: always a type, a name and an optional default, and never by reference. The parser reports a
///   parameter written as a computed property, because a parameter receives its value. A default is a constant
///   expression: a literal, a constant, `typeof(X)`, a list or map literal of them, or the operators below on them,
///   without `++` and `--`. A parameter written `int ...values` is variadic, as spec section 7 writes it, and PHP's
///   own checks report a variadic parameter that is not the last or has a default. `check_parameter_list` reports a
///   variadic parameter that declares a member or has a `void` type in a `.sharp` file only, as upstream Mago reports
///   neither. An optional parameter before a required one is an error, because PHP would make it required. A
///   variadic parameter is not a required one, as in PHP. A default of `Position.current()` of the standard library is
///   not supported yet, because as a default it gives the caller's position, spec section 27, which waits for typed
///   compilation.
/// - Types: `int`, `float`, `bool`, `string`, `Any`, a class written by its short name, `List<T>` and
///   `Map<TKey, TValue>` of these, function types `Function<R(P1, P2)>` of these, and `void` as a return type, a
///   function type's too. A `Map`'s key is `int` or `string`. PHP's own check reports a `void` parameter and a `void`
///   field. Each of them is nullable when written with `?` after it, as in `int?` or `Map<string, Any?>`, and PHP's
///   own check reports `void?`. PHP's `mixed` is an error, because spec section 24 writes it `Any?`. A union of them but
///   `void`, written inline as spec section 24 writes it, as in `int|string` or `List<int>|string`, goes wherever a
///   type goes. PHP's own check reports `void` and a nullable type, as in `int?|string`, in a union, and
///   `check_union` reports a type written twice, which the engine refuses. A union holds null only when written in
///   parentheses with `?` after it, as in `(int|string)?`, which the engine compiles as `int|string|null`.
///   `check_union` reports `null` written in a union, as in `int|null`, as not supported yet. PHP's own check reports
///   a nullable union inside another union, and a single type in parentheses, as in `(Calc)?`, which PHP# writes
///   `Calc?`. `(int)?` is a parse error, as PHP lexes `(int)` as a cast. A field, or a property with storage and `set`,
///   of a nullable type or a nullable union starts as null without an initial value, as in C# and Swift, so
///   `private int? total;` holds null until it is written, and the engine gives it the default `null`. A get-only
///   property with storage of one without an initial value is not supported yet, because it is set only where
///   `readonly` allows, and `readonly` takes no default. A property without storage holds nothing to start with.
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
///   above but `void`. An expression statement whose operator is binary, as in `flags | MASK;`, is an error that
///   names its compound assignment or its result, because the result is never used, except `??` and an assignment
///   that PHP's `and`, `or` and `xor` bind looser than.
/// - Writes: `=`, compound assignment, `++` and `--` write only a local, a parameter, a member written
///   `object.name` or, when static, `Class.name`, or an index of one of them written `target[key]`. The analyzer,
///   which knows the types, allows an index write only on a `Map`, and a read not under `??` or `?.` only on a `List`,
///   as spec section 12 decides.
/// - A constant, an enum case or a static member is reached through its class name, as in `Report.count`, and the
///   engine looks up which one it is when it runs. A bare constant or static member name is an error that names the
///   class. A constant expression, which is a parameter default, an attribute argument, a constant's value, a
///   constant initial value or an enum case's value, reads only a constant or an enum case this way, as PHP does: a
///   static field or property there is an error when this file declares it, and the analyzer reports it otherwise.
/// - In expressions: literals, list literals `[a, b]` and `[...a, b]`, map literals `["key": value]` and `[:]`, index reads
///   `value[key]`, templates, parentheses, bare names, assignment, the operators below, and method calls and property
///   reads written with `.` or `?.` and a member name, `new Class(...)` and `new Class.name(...)` on a class written by
///   its short name, `new Self(...)` in a class whose constructor is `required`, calls of a function by its bare name,
///   `super.method(...)`, which calls the parent's method, and `Self.method(...)`, which calls a static method of the
///   class a static method is called on, each with positional, named and spread arguments, as in
///   `Money.sum(...prices)` and `max(...prices)`, `throw`, which is an expression as in PHP, `exit(code)` and
///   `exit()`, which spec section 8 keeps as PHP 8.4's built-in function, and `typeof(X)` on a class written by its
///   short name, without a member read or called on it. `new Self(...)` in an enum is an error, because an enum has
///   no constructor. `Self.name` read or written is not supported yet, as `super.name` is not. PHP's own check
///   reports a positional argument after a spread and a spread after a named argument, and `check_function_call`
///   reports `assert` with a spread as its only argument, after which PHP adds a positional description. `?.` never
///   follows a class. A function is the global function of that name, PHP's own or one a library or the app
///   declares, as spec sections 8 and 29 keep them, and the engine calls the global one. A string literal's `\u{...}`
///   escapes are valid codepoints, as PHP requires. A `"…"` string never interpolates, and a template,
///   `` `Order ${number}` ``, interpolates any expression of this list in each `${…}` and takes JavaScript's escapes,
///   as spec section 18 writes them.
/// - Lambdas, as spec section 3 writes them: `x => x.id`, `(a, b) => a + b` and `() => { … }`, whose body is an
///   expression or a block of a method body. A parameter is a method's, with its type optional, so the last one can
///   be variadic, as in `(int ...values) => count(values)`. A lambda captures the variable itself, except a loop
///   variable that code changes, whose capture is not supported yet. A call of a local by its bare name, `f(x)`,
///   calls the lambda the local holds, with positional, named and spread arguments.
/// - Operators: `+ - * / % **`, `== != === !== < > <= >=`, `&& || !`, `??`, unary `-` and `+`, `++` and `--`, and
///   `= += -= *= /= **= ??=`. Spec section 19 puts `<`, `<=`, `>`, `>=`, `is` and `as` in one row and `==`, `!=`,
///   `===` and `<=>` in the next, and an operator of either row that takes an operand of its own row without
///   parentheses, as in `a < b < c` or `a is T is U`, is an error that names the groupings to write. So is a comparison
///   in the value of a comparison or value pattern, as in `x is > a < b`.
/// - `@`, which hides PHP's warnings, in a method body of a file whose namespace is `Sharp` or below it, where the
///   standard library writes it before a method throws its own exception. Anywhere else it is an error, and the
///   analyzer refuses it in any file that is not the standard library's.
/// - The ternary `c ? a : b` in a method body, as spec section 21 writes it. PHP's `a ?: b` is an error, and a
///   ternary as the condition of another needs parentheses, as in PHP 8.
/// - Pattern matching in a method body, as spec section 21 writes it: `x is pattern`, `x as T` to a type that is not
///   nullable or `void`, and `match`, whose arms are `pattern => value`, `pattern when condition => value` and one
///   `default => value`. A `match` needs `default` unless its arms name only `Class.y` values or `null`, as a `match`
///   on an enum does, and the analyzer reports the cases such a `match` misses. A `match` statement's arm may be a
///   block. A pattern is a type with an optional name, as in `int count`, a value, a comparison such as `< 10`, a
///   properties pattern such as `{ total: > 0 }`, or patterns joined by `and`, `or` and `not`. A type pattern is never
///   nullable, and a name under `or` or `not` is an error, as C#'s CS8780, except under the `not` that starts the
///   pattern of `is`. `not` beside `or` is an error that names both readings, as decision 045 decides, and `!x is T`,
///   which reads as `(!x) is T`, is an error that writes `x is not T`, as decision 044 decides. The parser reports list
///   patterns, and enum case patterns with fields or a name, as not supported yet.
/// - Casts: `(int)`, `(float)` and `(string)` in a method body, as spec section 24 writes them. PHP's other casts and
///   its cast aliases, such as `(bool)` and `(integer)`, are errors. A cast is lowercase only, so the lexer reads
///   `(Int)` as the class `Int` in parentheses, as in `typeof(Int)`.
/// - A bare `Int`, `Float`, `Bool`, `Position`, `Environment`, `List` or `Replaces` names the class `Sharp\<Name>` of
///   the standard library wherever a class is named, unless the file imports or declares a class-like of that name,
///   spec section 23. So `Int.parse(text)`, `Bool.tryParse(value)`, `Position.current()`, `new Environment()` and
///   `List.wrap(value)` call it, and `[Replaces("date")]` is its attribute. PHP reserves `Int`, `Float`, `Bool` and
///   `List`, so no file declares or imports a class of those, except the standard library's own `Sharp.Int`,
///   `Sharp.Float` and `Sharp.Bool`: a file whose namespace is exactly `Sharp` declares them, as the engine allows,
///   and any file may import them.
///
/// The check runs on every node the checking walk enters, and refuses any node, or any position of a node, that this
/// list does not name. It reports each refusal once, at its outermost node, and skips the nodes inside the refusal's
/// span. A node refused at a part, such as a method at its name or a cast at its operator, keeps its other parts
/// checked, so every PHP form PHP# removes is refused by name wherever it is written. It does not run on a file with a
/// parse error, which is the one error to fix first. The constructs PHP# never has, such as `$`
/// variables, `global` and top-level functions, keep their own errors. So do the PHP forms PHP# removes, each an error
/// that names its replacement. In a method body, they are `echo`, `print`, `die`, and `exit` without parentheses or
/// with a string literal or a template as its first argument. In a method body and a constant expression, they are
/// superglobals, read or written, magic constants and other `__Something__` names, `(array)` and `(object)`. A
/// `__Something__` name is an error when it is declared too, as a class, interface, enum, enum case, method, property,
/// constant, parameter or local. A method name that starts but does not end with `__`, as PHP's magic methods do,
/// stays not supported yet.
///
/// Twenty-two more refusals need inferred types or the codebase, so the analyzer makes them as its part of this
/// contract:
/// - `new` on a static class, in `analyze_class_instantiation`, and a class that extends one, in
///   `check_class_like_extends`, because the static class may be another file's.
/// - an `extern` method anywhere but a standard library file whose class is under `Sharp`, in `Method`'s `analyze`,
///   `@` in any file that is not the standard library's, in `UnaryPrefix`'s `analyze`, and a class or enum named
///   `Sharp.Int`, `Sharp.Float` or `Sharp.Bool` in any file that is not the standard library's, in
///   `report_sharp_type_class_outside_library`, because only the analyzer knows the file's package. The nearest
///   `composer.json` above a file names it.
/// - a bare call that finds no function while the enclosing class declares a method of that name, in
///   `bare_member_call`, because only the codebase knows which functions exist. A bare call is the global function's.
/// - `+` that may join a string with any other value, which spec section 18 makes an error, in
///   `analyze_arithmetic_operation`. `+` on two strings joins them.
/// - a condition of `if`, `while`, `do … while`, `for` or `? :`, or an operand of `&&`, `||` or `!`, that is not
///   `bool`, which spec section 21 makes an error, in `Context::report_non_bool_condition`.
/// - a cast of a value that is not an `int` or a `float`, which spec section 24 makes an error, in `UnaryPrefix`'s
///   `analyze`.
/// - an `exit` argument that is not an `int`, which spec section 8 makes an error, in `ExitConstruct`'s `analyze`.
///   `check_slice` refuses a string literal or template argument, because the engine runs `.sharp` files without the
///   analyzer.
/// - a call that resolves to a namespaced function, in `report_namespaced_function_call`.
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
///   whose constructor takes parameters, in `check_super_call`, which is not supported yet.
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
    /// An event, spec section 15: its modifiers, parameters and header.
    Event,
    /// A module and its members.
    Module,
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
    /// A constant expression: a parameter default, an attribute argument, a class constant's value or an enum case's
    /// value.
    Constant,
    /// A pattern of `is` or of a `match` arm, whose values are the method body's.
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
            if let Expression::List(list) = target {
                report_removed_construct(list.list.value, target.span(), context);
            } else {
                report_not_supported(
                    target.span(),
                    "write target",
                    "PHP# writes to a local, a parameter or a member written `object.name`.",
                    context,
                );
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
            report_not_supported(
                namespace.r#namespace.span,
                "namespace",
                "A PHP# file has at most one namespace, written `namespace App.Tenant;` before its imports.",
                context,
            );

            Some(File)
        }
        (Node::Statement(Statement::Declare(declare)), _) => {
            let head = BytesDisplay(context.get_code_snippet(declare.declare.span.join(declare.right_parenthesis)));
            let issue = if declare.items.iter().all(|item| item.name.value.eq_ignore_ascii_case(b"strict_types")) {
                Issue::error(format!("PHP# is always strict: remove `{head};`."))
                    .with_note("Every `.sharp` file runs with `strict_types=1`.")
            } else {
                let help = match declare.body {
                    DeclareBody::Statement(Statement::Noop(_)) => "Delete it.",
                    _ => "Write its body's code without the `declare` around it.",
                };

                Issue::error(format!("PHP# has no `{head}`: a `.sharp` file runs with PHP's default settings."))
                    .with_help(help)
            };
            context.report(issue.with_annotation(Annotation::primary(declare.span()).with_message("Written here.")));

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
        (Node::Keyword(_) | Node::LocalIdentifier(_) | Node::Identifier(Identifier::Local(_)), _) => Some(place),
        (Node::Terminator(Terminator::Semicolon(_)), _) => Some(place),
        (Node::Extern(r#extern), File) if r#extern.event.is_some() => check_extern_event(r#extern, context),

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
                | Statement::Event(_),
            )
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
            // A braced namespace's body, after its namespace was refused.
            | Node::Block(_)
            | Node::Identifier(Identifier::Dotted(_))
            | Node::DottedIdentifier(_)
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
                    check_named_constructor(constructor, class, context);
                }
            }

            Some(Class)
        }
        // The walk checks a class's, an interface's, an enum's and an event's name in `check_class_name`.
        (Node::Interface(_), File) => Some(Interface),
        (Node::Enum(_), File) => Some(Enum),
        (Node::Event(_), File) => Some(Place::Event),
        (Node::Modifier(Modifier::Public(_)), Place::Event) => Some(Place::Event),
        // Spec section 5's `internal` is not supported yet on any declaration.
        (Node::Modifier(modifier), Place::Event) if !matches!(modifier, Modifier::Internal(_)) => {
            context.report(
                Issue::error("An event takes only `public`.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier."))
                    .with_note("An event runs as a final class, and its parameters are its public get-only properties."),
            );

            None
        }
        (Node::FunctionLikeParameter(parameter), Place::Event) => check_event_parameter(parameter, context),
        (Node::Module(module), File) => check_module_file_name(module, context),
        (Node::ModuleMember(_), Place::Module) => Some(Place::Module),
        // The header names the module it overrides, which `check_module_file_name` checked.
        (Node::ModuleParent(_), Place::Module) => None,
        // A binding's parameters and value are checked as a method's, and the analyzer checks its value as the
        // lambda it runs as.
        (Node::Binding(_), Place::Module) => Some(Method),
        (Node::BindingTarget(_) | Node::BindingValue(_) | Node::BindingClasses(_), Method) => Some(Method),
        (Node::FunctionLikeParameterList(parameters), Method | Signature | Place::Event) => {
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
        (Node::Modifier(Modifier::Lazy(lazy)), _) => {
            context.report(
                Issue::error("`lazy` is not supported yet in PHP#.")
                    .with_annotation(Annotation::primary(lazy.span).with_message("Not supported yet."))
                    .with_note("A lazy property computes its value on the first read, which the engine cannot run yet."),
            );

            None
        }
        // `check_class` reports `protected` and `private` on a class, and PHP's own check `abstract` with `final`.
        // `check_static_class` reports `abstract` and `final` on a static class.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Abstract(_)
                | Modifier::Final(_)
                | Modifier::Static(_),
            ),
            Class,
        ) => Some(Class),
        (Node::Inheritance(inheritance), Class | Interface | Enum | Place::Event) => {
            check_header(inheritance, place, context);

            Some(place)
        }
        (Node::Modifier(Modifier::Public(_)), Interface | Enum) => Some(place),
        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Interface) => Some(Interface),
        // An interface states a property its classes give, as `OrderEvent` states `orderId`, spec section 15.
        (Node::ClassLikeMember(ClassLikeMember::Property(Property::Hooked(property))), Interface)
            if is_interface_property(property) =>
        {
            Some(FieldOrProperty)
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
            Signature | FieldOrProperty,
        ) if is_in_interface(context.program, modifier.span()) => {
            report_interface_member_access(modifier, context);

            None
        }
        (Node::Modifier(modifier), Signature) => {
            context.report(
                Issue::error("An interface method takes no modifier: every interface method is public.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier.")),
            );

            None
        }
        (Node::Method(method), Interface) => match is_slice_signature(method) {
            Ok(()) => {
                check_declared_name(method.name.value, method.name.span, context);

                Some(Signature)
            }
            Err((message, help)) => report_refusal(
                Issue::error(message)
                    .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                    .with_note(help),
                method.span(),
                Signature,
                context,
            ),
        },
        // A body's block is not in the place, so `MethodBody` admits only `MethodAbstractBody`. The analyzer's
        // `check_listener` refuses an interface method that listens.
        (
            Node::FunctionLikeReturnTypeHint(_) | Node::On(_) | Node::MethodBody(_) | Node::MethodAbstractBody(_),
            Signature,
        ) => Some(Signature),
        (Node::AttributeList(_), Class | Enum | Method | FieldOrProperty | Parameter) => Some(Attribute),
        (
            Node::Attribute(_)
            | Node::PartialArgumentList(_)
            | Node::PartialArgument(PartialArgument::Positional(_) | PartialArgument::Named(_)),
            Attribute,
        ) => Some(Attribute),
        // PHP refuses a spread in attribute arguments, and `check_attribute_list` reports it.
        (Node::PositionalArgument(argument), Attribute) => argument.ellipsis.is_none().then_some(Constant),
        (Node::NamedArgument(_), Attribute) => Some(Constant),

        (Node::ClassLikeMember(ClassLikeMember::Method(_) | ClassLikeMember::Law(_)), Class | Enum) => Some(place),
        // A law's parameters and body are checked as a method's. `check_members` refuses a default and a variadic one.
        (Node::Law(_), Class | Enum | Place::Module) => Some(Method),
        (Node::ClassLikeMember(ClassLikeMember::Operator(_) | ClassLikeMember::NamedConstructor(_)), Class) => {
            Some(Class)
        }
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
        (Node::Operator(operator), Class) => {
            check_operator(operator, context);

            Some(Method)
        }
        (Node::BinaryOperator(_), Method) => Some(Method),
        // `check_enum` reports a property in an enum, and a backing type other than `int` or `string`.
        (Node::ClassLikeMember(ClassLikeMember::Property(_)) | Node::EnumBackingTypeHint(_), Enum) => None,
        (Node::EnumCase(case), Enum) if case.item.name().value.eq_ignore_ascii_case(b"class") => {
            context.report(
                Issue::error("An enum case cannot be named `class`: PHP reserves `class` for the class name.")
                    .with_annotation(Annotation::primary(case.item.name().span).with_message("Declared here.")),
            );

            Some(Enum)
        }
        (Node::EnumCase(case), Enum) => {
            check_declared_name(case.item.name().value, case.item.name().span, context);

            Some(Enum)
        }
        (
            Node::ClassLikeMember(ClassLikeMember::EnumCase(_)) | Node::EnumCaseItem(_) | Node::EnumCaseUnitItem(_),
            Enum,
        ) => Some(Enum),
        (Node::EnumCaseBackedItem(_), Enum) => Some(Constant),
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class) => {
            match is_slice_property(property, &context.version, context.names) {
                Ok(()) => {
                    let variable = property.first_variable();
                    check_declared_name(variable.name, variable.span, context);

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
        (Node::ClassLikeMember(ClassLikeMember::Constant(constant)), Class | Enum) => match is_slice_constant(constant) {
            Ok(()) => {
                let name = &constant.first_item().name;
                check_declared_name(name.value, name.span, context);
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
                }

                Some(ClassConstant)
            }
            Err(issue) => report_refusal(*issue, constant.span(), ClassConstant, context),
        },
        (
            Node::ClassLikeConstant(_)
            | Node::Modifier(Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)),
            ClassConstant,
        ) => Some(ClassConstant),
        (Node::ClassLikeConstantItem(_), ClassConstant) => Some(Constant),
        (Node::HookedProperty(property), FieldOrProperty) => {
            let variable = property.item.variable();
            check_accessors(&property.modifiers, &property.hook_list, variable.span, context);
            check_accessor_bodies(&property.hook_list, variable.name, context);
            if let Some(initial_value) = &property.initial_value
                && !context.names.has_storage(&property.hook_list)
            {
                report_no_storage(variable, initial_value.value.span(), "its initial value", context);
            }

            Some(FieldOrProperty)
        }
        // `check_accessors` reported each accessor outside the slice. An accessor body is a method body.
        (Node::PropertyHookList(_), FieldOrProperty | Parameter) => Some(place),
        (Node::PropertyHook(accessor), FieldOrProperty | Parameter) => is_slice_accessor(accessor).then_some(place),
        (
            Node::PropertyHookBody(_) | Node::PropertyHookAbstractBody(_) | Node::PropertyHookConcreteBody(_),
            FieldOrProperty | Parameter,
        ) => Some(place),
        // A computed property's expression runs on each read, as a method body does.
        (Node::ComputedProperty(_), FieldOrProperty) => Some(FieldOrProperty),
        (Node::Block(_) | Node::PropertyHookConcreteExpressionBody(_), FieldOrProperty | Parameter) => Some(Body),
        // A constant initial value is the member's default, which PHP evaluates as a constant expression.
        (Node::Expression(value), FieldOrProperty) => {
            let uses_this = report_this_in_initial_value(node, context);
            let place = if !uses_this && value.is_constant(&context.version, false) { Constant } else { Body };

            enter(node, place, context)
        }
        (
            Node::Property(_)
            | Node::PlainProperty(_)
            | Node::PropertyItem(_)
            | Node::PropertyAbstractItem(_)
            | Node::PropertyConcreteItem(_)
            | Node::DirectVariable(_)
            // `is_slice_property` refuses `override` on a member that is not a field.
            | Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Override(_),
            ),
            FieldOrProperty,
        ) => Some(FieldOrProperty),
        (Node::Hint(Hint::Mixed(mixed)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body)
            if mixed.value != ANY =>
        {
            context.report(
                Issue::error("PHP# has no `mixed`: write `Any?`, or `Any` for a value that is never null.")
                    .with_annotation(Annotation::primary(mixed.span).with_message("Written here."))
                    .with_note("`Any` holds a value of any type but null, and `Any?` also allows null."),
            );

            None
        }
        (Node::GenericHint(generic), FieldOrProperty | Method | Signature | Parameter | Body) => {
            // The analyzer refuses a named key type without an `int` or `string` backing value. A key type outside the
            // slice, nullable or not, is refused once, by the walk, as that type.
            if let [key, _] = generic.arguments.as_slice()
                && !matches!(key, Hint::Integer(_) | Hint::String(_) | Hint::Identifier(_))
                && is_slice_type(match key {
                    Hint::Nullable(nullable) => nullable.hint,
                    key => key,
                })
            {
                return report_refusal(
                    Issue::error(
                        "A `Map`'s keys are `int`, `string` or a type with an `int` or `string` backing value.",
                    )
                    .with_annotation(Annotation::primary(key.span()).with_message("Key type written here.")),
                    generic.span(),
                    place,
                    context,
                );
            }

            Some(place)
        }
        // A function type's parts are checked as a parameter's type is, so its return type may be `void`, which
        // `is_slice_type` refuses for its parameters, and `Self` in it is an error, because `Self` is a method's return
        // type only.
        (Node::FunctionHint(_), FieldOrProperty | Method | Parameter | Body) => Some(Parameter),
        // The effects an `extern` or a function type lists, spec section 29.
        (Node::Uses(uses), File | Parameter) => {
            check_uses(uses, context);

            Some(place)
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
                // The constructor, the one method without a return type, carries its class's name, which
                // `check_class_name` reports.
                if method.return_type_hint.is_some() {
                    check_declared_name(method.name.value, method.name.span, context);
                }

                check_extern(method, context)
            }
            Err((message, help)) => report_refusal(
                Issue::error(message)
                    .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                    .with_note(help),
                method.span(),
                Method,
                context,
            ),
        },

        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Abstract(_)
                | Modifier::Virtual(_)
                | Modifier::Override(_)
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
        // Outside a constructor, PHP's own check reports a parameter with an access modifier.
        (Node::FunctionLikeParameter(parameter), Method)
            if parameter.hooks.is_none()
                && let Some(public) = parameter.modifiers.get_public()
                && is_constructor_parameter(parameter, context.program) =>
        {
            report_refusal(
                Issue::error(
                    "A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`.",
                )
                .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                .with_note("A `public` parameter without accessors is an error, as a public field is."),
                parameter.span(),
                Parameter,
                context,
            )
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda)
            if let Some(ampersand) = parameter.ampersand =>
        {
            report_refusal(
                Issue::error("A by-reference parameter is not supported yet in PHP#.")
                    .with_annotation(Annotation::primary(ampersand).with_message("Not supported yet."))
                    .with_note("The engine passes every PHP# argument by value."),
                parameter.span(),
                Parameter,
                context,
            )
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda) => {
            if let Some(accessors) = &parameter.hooks
                && parameter.modifiers.contains_visibility()
            {
                check_accessors(&parameter.modifiers, accessors, parameter.variable.span, context);
                check_accessor_bodies(accessors, parameter.variable.name, context);
                if !context.names.has_storage(accessors) {
                    report_no_storage(&parameter.variable, parameter.variable.span, "the constructor to set", context);
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
        (Node::Hint(Hint::Self_(keyword)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            check_self_type(keyword, place, context).then_some(place)
        }
        (Node::Hint(Hint::Static(keyword)), FieldOrProperty | ClassConstant | Method | Signature | Parameter | Body) => {
            report_php_static(keyword, context);

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
                | Modifier::Static(_)
                | Modifier::Final(_)
                | Modifier::Abstract(_),
            ),
            Parameter,
        ) => Some(Parameter),
        (Node::FunctionLikeParameterDefaultValue(default), Parameter)
            if let Expression::Call(Call::Method(call)) = default.value
                && let Some(class) = context.names.static_call_class(call)
                && context.names.get(&class.name).eq_ignore_ascii_case(b"Sharp\\Position")
                && let ClassLikeMemberSelector::Identifier(method) = &call.method
                && method.value.eq_ignore_ascii_case(b"current") =>
        {
            context.report(
                Issue::error("A `Position.current()` default is not supported yet in PHP#.")
                    .with_annotation(Annotation::primary(call.span()).with_message("Not supported yet."))
                    .with_note(
                        "As a parameter's default, `Position.current()` gives the caller's position, which waits for typed compilation.",
                    ),
            );

            None
        }
        (Node::FunctionLikeParameterDefaultValue(_), Parameter) => Some(Constant),
        (Node::Block(_), Method | Body) => Some(Body),
        (Node::MethodExpressionBody(_), Method) => Some(Body),
        (Node::TryCatchClause(_), Body) => Some(TryCatchClause),
        // `check_try` reports a catch type that is not a class, and `check_try_catch_clause` the variable's name.
        (Node::Hint(_) | Node::DirectVariable(_), TryCatchClause) => None,
        (Node::Block(_), TryCatchClause) => Some(Body),

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
            | Node::TryFinallyClause(_)
            | Node::Break(_)
            | Node::Continue(_)
            | Node::ExpressionStatement(_)
            | Node::Return(_)
            | Node::Emit(_)
            | Node::LocalDeclaration(_)
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
            | Node::ForOfKeyValueTarget(_)
            | Node::ForOfVariable(_),
            Body,
        ) => Some(Body),
        // Spec section 8 makes output and `exit` function calls, and keeps `exit(code)` as PHP 8.4's built-in
        // function. The analyzer refuses an `exit` argument that is not an `int`.
        (Node::Statement(Statement::Echo(_)) | Node::Expression(Expression::Construct(Construct::Print(_))), Body) => {
            let form: &[u8] = if matches!(node, Node::Statement(_)) { b"echo" } else { b"print" };
            report_removed_construct(form, node.span(), context);

            None
        }
        (Node::Expression(Expression::Construct(Construct::Exit(exit))), Body) if exit.arguments.is_none() => {
            context.report(
                Issue::error("PHP# calls `exit` as a function: write `exit(0)`.")
                    .with_annotation(Annotation::primary(exit.exit.span).with_message("Written here."))
                    .with_note("`exit` is PHP 8.4's built-in function in PHP#."),
            );

            None
        }
        // The engine runs a `.sharp` file without the analyzer, so a string literal or template argument, in any
        // number of parentheses, is refused here.
        (Node::Expression(Expression::Construct(Construct::Exit(exit))), Body)
            if exit.arguments.as_ref().and_then(|arguments| arguments.arguments.first()).is_some_and(|argument| {
                let mut value = argument.value();
                while let Expression::Parenthesized(parenthesized) = value {
                    value = parenthesized.expression;
                }

                matches!(value, Expression::Literal(Literal::String(_)) | Expression::CompositeString(_))
            }) =>
        {
            report_removed_construct(b"die", node.span(), context);

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
        (Node::Expression(Expression::PatternMatch(pattern_match)), Body) => {
            check_pattern_match(pattern_match, true, context).then_some(Body)
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
        (Node::PatternMatchPatternArm(arm), Body) if report_chained_pattern(arm.pattern.span(), arm.pattern, context) => {
            None
        }
        (Node::Is(is), Body) => {
            // `is not T name` declares `name` where the test is false, so the outermost `not` hides no variable.
            let tested = if let Pattern::Not(not) = is.pattern { not.pattern } else { is.pattern };
            report_hidden_pattern_variables(tested, false, context);

            Some(Body)
        }
        (Node::PatternMatchPatternArm(arm), Body) => {
            report_hidden_pattern_variables(arm.pattern, false, context);

            Some(Body)
        }
        (Node::As(r#as), Body) => match r#as.hint {
            Hint::Nullable(_) | Hint::Void(_) => {
                context.report(
                    Issue::error("`as` converts to a type that is not nullable or `void`.")
                        .with_annotation(Annotation::primary(r#as.hint.span()).with_message("Written here."))
                        .with_note("`as T` already gives `T?`: the value as a `T`, or null when it is not one."),
                );

                None
            }
            // A type outside the slice, and `Self`, are refused by the walk, as anywhere in a body.
            hint if !has_type_test(hint) && (is_slice_type(hint) || matches!(hint, Hint::Union(_))) => {
                report_not_supported(
                    hint.span(),
                    "type",
                    "`as` converts to `int`, `float`, `string`, `bool` or a class.",
                    context,
                );

                None
            }
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
        (Node::Pattern(_), Body | Place::Pattern) => Some(Place::Pattern),
        (Node::Hint(Hint::Nullable(_)), Place::Pattern) => {
            context.report(
                Issue::error("A type pattern is never nullable: null never matches a type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Written here."))
                    .with_help("Test for null with `x == null`, or join both with `or`, as in `x is int or null`."),
            );

            None
        }
        (Node::Hint(hint), Place::Pattern) if is_slice_type(hint) && has_type_test(hint) => Some(Place::Pattern),
        (Node::BinaryPattern(binary), Place::Pattern)
            if !binary.is_and() && [binary.left, binary.right].iter().any(|side| matches!(side, Pattern::Not(_))) =>
        {
            report_not_beside_or(binary, context);

            None
        }
        (
            Node::TypePattern(_)
            | Node::ComparisonPattern(_)
            | Node::NotPattern(_)
            | Node::BinaryPattern(_)
            | Node::ParenthesizedPattern(_)
            | Node::PropertiesPattern(_)
            | Node::PropertyPattern(_)
            | Node::BinaryOperator(_),
            Place::Pattern,
        ) => Some(Place::Pattern),
        // A value or a comparison's value is a method body's expression.
        (Node::Expression(_), Place::Pattern) => enter(node, Body, context),

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
            report_superglobal(span, context);

            None
        }
        (Node::Expression(expression @ Expression::ConstantAccess(_)), Body | Constant)
            if let Some(name) = magic_name(expression, context) =>
        {
            report_magic_constant(name.value, name.span, None, context);

            None
        }
        (Node::Expression(Expression::MagicConstant(constant)), Body | Constant) => {
            report_magic_constant(constant.value().value, constant.value().span, Some(constant), context);

            None
        }
        (Node::UnaryPrefix(unary_prefix), Body | Constant) if unary_prefix.operator.is_cast() => {
            check_cast(unary_prefix, place, context);

            Some(place)
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
        (Node::Expression(Expression::ArrayAccess(_)) | Node::ArrayAccess(_), Body) => Some(Body),
        (Node::ConstantAccess(_), Body) => Some(Body),
        // `typeof(X)` is `X::class`, which PHP takes as a constant expression too.
        (Node::Expression(Expression::TypeOf(_)) | Node::TypeOf(_), Body | Constant) => Some(place),
        (Node::ConstantAccess(constant), Constant)
            if context.names.binding(&constant.name) == Some(Binding::Constant) =>
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
                && let ClassLikeMemberSelector::Identifier(member) = &access.property =>
        {
            report_static_member_in_constant(class, member, access.span(), context);

            None
        }
        (Node::Expression(Expression::Conditional(_)), Body) => Some(Body),
        (Node::Conditional(conditional), Body) => Some(check_conditional(conditional, context)),
        // Spec section 14.2: a lambda is a bare arrow, after one name or a parenthesized parameter list, and its body
        // is an expression or a block. The parser reads it into an arrow function or a closure without a keyword.
        (Node::Expression(Expression::ArrowFunction(_) | Expression::Closure(_)), Body) => Some(Body),
        (Node::ArrowFunction(_) | Node::Closure(_), Body) => {
            check_lambda_captures(node, context);

            Some(Lambda)
        }
        (Node::FunctionLikeParameterList(_), Lambda) => Some(Lambda),
        (Node::Block(_), Lambda) => Some(Body),
        (Node::Expression(_), Lambda) => enter(node, Body, context),
        (Node::BinaryOperator(operator), Body | Constant) if is_slice_binary_operator(operator) => Some(place),
        // `check_cast` decided the cast at its `UnaryPrefix`.
        (Node::UnaryPrefixOperator(operator), Body | Constant) if operator.is_cast() => Some(place),
        (Node::UnaryPrefixOperator(operator @ UnaryPrefixOperator::ErrorControl(_)), Body | Constant)
            if !is_standard_library_namespace(context.program) =>
        {
            context.report(
                Issue::error(
                    "`@` hides PHP's warnings, and only the standard library uses it: handle the failure where it happens.",
                )
                .with_annotation(Annotation::primary(operator.span()).with_message("Written here.")),
            );

            None
        }
        (Node::UnaryPrefixOperator(operator), Body | Constant) if is_slice_prefix_operator(operator, place) => {
            Some(place)
        }

        (
            Node::Expression(
                Expression::Assignment(_)
                | Expression::UnaryPostfix(_)
                | Expression::Call(Call::Method(_) | Call::NullSafeMethod(_))
                | Expression::Access(Access::Property(_) | Access::NullSafeProperty(_))
                | Expression::Throw(_)
                // The parser reads a template, and only a template, into an interpolated string.
                | Expression::CompositeString(CompositeString::Interpolated(_)),
            )
            | Node::Throw(_)
            | Node::CompositeString(CompositeString::Interpolated(_))
            | Node::InterpolatedString(_)
            | Node::StringPart(StringPart::Literal(_) | StringPart::BracedExpression(_))
            | Node::LiteralStringPart(_)
            | Node::BracedExpressionStringPart(_)
            | Node::Assignment(_)
            | Node::UnaryPostfix(_)
            | Node::UnaryPostfixOperator(UnaryPostfixOperator::PostIncrement(_) | UnaryPostfixOperator::PostDecrement(_))
            | Node::Call(Call::Method(_) | Call::NullSafeMethod(_))
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
                Expression::Identifier(Identifier::Local(_)) | Expression::Self_(_) | Expression::Static(_)
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
        (Node::ArgumentList(_), Instantiation) => Some(Body),
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
        // `super.label()` calls the parent's method, spec section 22. `super` is no value of its own.
        (Node::MethodCall(call), Body) if matches!(call.object, Expression::Parent(_)) => Some(SuperCall),
        // `Self.make()` calls a static method of the class a static method is called on, spec section 25.
        (Node::MethodCall(call), Body) if let Expression::Self_(keyword) = call.object => {
            if !is_sharp_self(keyword) {
                report_php_self(keyword, true, context);
            }

            Some(SelfCall)
        }
        (Node::Expression(Expression::Parent(_)), SuperCall) | (Node::Expression(Expression::Self_(_)), SelfCall) => None,
        (Node::ClassLikeMemberSelector(_) | Node::ArgumentList(_), SuperCall | SelfCall) => {
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
            check_variable(variable, context);

            None
        }
        // Spec section 25 removes PHP's `self` and `static`. `Self` is no value of its own.
        (Node::Expression(Expression::Self_(keyword)), Body | Constant) if !is_sharp_self(keyword) => {
            report_php_self(keyword, false, context);

            None
        }
        (Node::Expression(Expression::Static(keyword)), Body | Constant) => {
            report_php_static(keyword, context);

            None
        }
        // PHP# never has top-level functions, `global`, `compact()` or `extract()`: `check_function`, `check_global` and
        // `check_function_call` report them. A function, at file level or in a body, is reported at its name, so its
        // parameters and body are checked as a method's. `global` holds only the names it declares, and `compact()`
        // and `extract()` are reported over the whole call.
        (Node::Statement(Statement::Function(_)), File | Body) => Some(File),
        (Node::Function(_), File) => Some(Method),
        (Node::Statement(Statement::Global(_)), Body) => None,
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if let Expression::Identifier(name) = function_call.function
                && is_compact_or_extract(name.last_segment()) =>
        {
            None
        }
        // Spec sections 8 and 29: plain PHP functions are called by their bare names. The analyzer refuses a call
        // that resolves to a namespaced function. Spec section 14: a local holding a function is called the same way.
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if let Expression::Identifier(name @ Identifier::Local(_)) = function_call.function
                && matches!(context.names.binding(name), None | Some(Binding::Local(_))) =>
        {
            Some(FunctionCall)
        }
        (
            Node::Call(Call::Function(_))
            | Node::FunctionCall(_)
            | Node::Expression(Expression::Identifier(Identifier::Local(_))),
            FunctionCall,
        ) => Some(FunctionCall),
        (Node::ArgumentList(_), FunctionCall) => Some(Body),

        _ => {
            report_unsupported(node, place, context);

            None
        }
    }
}

/// Whether the slice has a method, with the refusal's message and note when it does not. Each rule keeps out a method
/// whose meaning a later slice gives it: PHP's magic methods, PHP#'s `private` default of spec section 5, and a static
/// constructor. The constructor of spec section 9 is the one method without a return type, named after its class.
/// `enter` reports a `__Something__` name, which ends as well as starts with `__`.
fn is_slice_method(method: &Method, program: &Program) -> Result<(), (&'static str, &'static str)> {
    let class_name = enclosing_class(program, method.span()).map(|class| class.name.value);
    let refusal = if method.return_type_hint.is_none() && class_name != Some(method.name.value) {
        (
            "A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "A method is written `public int total()`, and the constructor `public Report()`.",
        )
    } else if method.return_type_hint.is_none() && method.is_static() {
        ("A static constructor is not supported yet in PHP#.", "The main constructor of a PHP# class is not `static`.")
    } else if method.name.value.starts_with(b"__") && !is_magic_name(method.name.value) {
        (
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        )
    } else if !method.modifiers.iter().any(|modifier| {
        matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_))
    }) {
        (
            "A method without `public`, `protected` or `private` is not supported yet in PHP#.",
            "A PHP# member without an access modifier is `private`, while PHP makes it `public`.",
        )
    } else if method.return_type_hint.is_some()
        && class_name.is_some_and(|class_name| class_name.eq_ignore_ascii_case(method.name.value))
    {
        (
            "A method named after its class is not supported yet in PHP#.",
            "In PHP#, a method named after its class is the class's constructor.",
        )
    } else {
        return Ok(());
    };

    Err(refusal)
}

/// Whether the slice has an interface method, with the refusal's message and note when it does not. `enter` reports a
/// modifier, a body and a `__Something__` name.
fn is_slice_signature(method: &Method) -> Result<(), (&'static str, &'static str)> {
    if method.return_type_hint.is_none() {
        Err((
            "An interface method needs a return type in PHP#.",
            "An interface method is written `float area();`, with its return type first.",
        ))
    } else if method.name.value.starts_with(b"__") && !is_magic_name(method.name.value) {
        Err((
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        ))
    } else {
        Ok(())
    }
}

/// Checks an operator a class declares, spec section 19: a symbol a class can declare, `public static`, the number of
/// parameters its symbol takes with one of them the class itself, `bool` from `==` and `int` from `<=>`, and one
/// declaration of it in its class. A refused symbol is the one error, because the other rules depend on it. The
/// analyzer checks that a class that declares `==` has a `hash()`.
fn check_operator(operator: &Operator, context: &mut Context<'_, '_, '_>) {
    let Some(class) = enclosing_class(context.program, operator.span()) else {
        return;
    };
    let names = context.names;
    let class_name = BytesDisplay(class.name.value);
    let symbol = BytesDisplay(operator.symbol.as_bytes());
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

    if operator.modifiers.len() != 2
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
    let class_fqcn = names.get(class.name);
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
            format!("One parameter of `operator {symbol}` is `{class_name}`, the class that declares it."),
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

    let declared_before = class
        .members
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
        if let Modifier::Abstract(keyword) | Modifier::Final(keyword) = modifier {
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

/// Whether a method is `extern`: its body is native, compiled into the engine, as spec section 29 writes it.
fn is_extern(method: &Method) -> bool {
    method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_)))
}

/// The place of a method's parts, refusing an `extern` method that is not `public static`, in a static class, with no
/// body. The refusal is at its name, so its other parts, a body written by mistake among them, stay checked. Only the
/// standard library declares native bodies, but the engine compiles the library's files from `vendor/` as any other,
/// so the analyzer, which knows a file's package, refuses one anywhere else.
fn check_extern(method: &Method, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    if !is_extern(method)
        || (enclosing_class(context.program, method.span()).is_some_and(|class| class.is_static)
            && method.modifiers.contains_public()
            && method.is_static()
            && matches!(method.body, MethodBody::Abstract(_)))
    {
        return Some(Place::Method);
    }

    report_refusal(
        Issue::error("An `extern` method is `public static`, in a static class, with no body.")
            .with_annotation(Annotation::primary(method.name.span).with_message("Declared `extern` here.")),
        method.span(),
        Place::Method,
        context,
    )
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

/// Decides an event's parameter, which runs as a public get-only property of the event, spec section 15: a type, a
/// name and an optional default. PHP refuses a variadic promoted property, and the event decides the property's
/// access, so a modifier, accessors and `...` are refused. The rest is checked as a method's parameter.
fn check_event_parameter(parameter: &FunctionLikeParameter, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let remove = |span: Span, message: &str| {
        Issue::error(message).with_annotation(Annotation::primary(span).with_message("Remove this."))
    };
    let issue = if let Some(modifier) = parameter.modifiers.first() {
        // The issue covers the modifier, not the parameter, so the walk stops here: it would refuse `readonly` and
        // `internal` again as not supported yet.
        context.report(remove(
            modifier.span(),
            "An event's parameter takes no modifier: it is a public get-only property of the event.",
        ));

        return None;
    } else if let Some(accessors) = &parameter.hooks {
        remove(accessors.span(), "An event's parameter takes no accessors: it is a get-only property of the event.")
    } else if let Some(ellipsis) = parameter.ellipsis {
        remove(ellipsis, "An event's parameter cannot be variadic: it is one property of the event.")
    } else if let Some(Hint::Generic(set) | Hint::Nullable(NullableHint { hint: Hint::Generic(set), .. })) =
        &parameter.hint
        && set.name.value == b"Set"
    {
        Issue::error("A `Set` parameter of an event is not supported yet in PHP#.")
            .with_annotation(Annotation::primary(set.span()).with_message("Not supported yet."))
            .with_note("An event's parameter is its property, and an event cannot yet make its argument a `Set`.")
    } else {
        return enter(Node::FunctionLikeParameter(parameter), Place::Method, context);
    };

    report_refusal(issue, parameter.span(), Place::Parameter, context)
}

/// Whether the slice has an interface property: a type, a name and `{ get; }`, which PHP runs as a public interface
/// property its classes give, as an event's parameters give `OrderEvent`'s `orderId`, spec section 15. An access
/// modifier keeps it in the slice, and `enter` reports the modifier where the walk reaches it.
fn is_interface_property(property: &HookedProperty) -> bool {
    property.attribute_lists.is_empty()
        && property.var.is_none()
        && property.modifiers.iter().all(|modifier| {
            matches!(
                modifier,
                Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Internal(_)
            )
        })
        && property.hint.is_some()
        && matches!(property.item, PropertyItem::Abstract(_))
        && property.initial_value.is_none()
        && matches!(
            property.hook_list.hooks.as_slice(),
            [accessor] if accessor.name.value == b"get" && is_slice_accessor(accessor) && matches!(accessor.body, PropertyHookBody::Abstract(_))
        )
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

/// Reports each name in a `uses` that is no effect, spec section 29. A name is an effect when it resolves to one of the
/// [`STANDARD_EFFECTS`], written bare or imported, and names compare as PHP's class names do, ignoring case.
fn check_uses(uses: &Uses, context: &mut Context<'_, '_, '_>) {
    for effect in &uses.names {
        let resolved = context.get_name(effect.span.start);
        if STANDARD_EFFECTS.iter().any(|standard| standard.as_bytes().eq_ignore_ascii_case(resolved)) {
            continue;
        }

        let [rest @ .., last] = &STANDARD_EFFECTS;
        let rest = rest.iter().map(short_name).collect::<Vec<_>>().join(", ");
        let message =
            format!("`{}` is not an effect: `uses` takes {rest} or {}.", BytesDisplay(effect.value), short_name(last));

        context.report(
            Issue::error(message).with_annotation(Annotation::primary(effect.span).with_message("Written here.")),
        );
    }
}

/// Reports what the engine refuses in a header when it declares the class: a name the header already holds, and in an
/// enum, `UnitEnum` or `BackedEnum`, which the engine adds to every enum or every backed one. Names compare as PHP's
/// do, resolved and ignoring case.
fn check_header(inheritance: &Inheritance, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut named: Vec<&[u8]> = Vec::new();
    for name in &inheritance.types {
        let resolved = context.get_name(name.span().start);
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
/// A static member takes only a constant initial value, which PHP stores as its default, because any other runs in
/// the constructor. A static property has `set`, because PHP has no `readonly` static property.
///
/// A field written `override` replaces a plain PHP parent's property, spec section 6.1. It takes the parent's access
/// level, which may be `public` and which the analyzer checks, and a constant initial value, its new default, because
/// the parent's constructor may read the property before this class's constructor sets any other.
fn is_slice_property(property: &Property, version: &PHPVersion, names: &ResolvedNames) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Issue::error(message)
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(note)
    };
    let no_access_modifier = "A PHP# member without an access modifier is `private`, while PHP makes it `public`.";

    if let Some(r#static) = property.modifiers().get_static() {
        if let Property::Hooked(hooked) = property
            && hooked.hook_list.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
        {
            return Err(Box::new(not_supported(
                r#static.span(),
                "A static property with an accessor body is not supported yet in PHP#.",
                "PHP has no hooks on a static property.",
            )));
        }

        if let Property::Hooked(auto_property) = property
            && auto_property.hook_list.is_get_only()
        {
            return Err(Box::new(not_supported(
                property.first_variable().span,
                "A get-only static property is not supported yet in PHP#.",
                "A get-only property runs as `readonly`, and PHP has no `readonly` static property.",
            )));
        }

        if let Property::Hooked(auto_property) = property
            && let Some(init) = auto_property.hook_list.setter().filter(|setter| setter.name.value == b"init")
        {
            return Err(Box::new(
                Issue::error("A static property has no `init`: `init` sets a property while its object is created.")
                    .with_annotation(Annotation::primary(init.span()).with_message("Declared here."))
                    .with_help("Write `set;`, or `private set;` to set it only inside its class."),
            ));
        }

        if let Some(value) = property.initial_value()
            && !value.is_constant(version, false)
        {
            return Err(Box::new(not_supported(
                value.span(),
                "A static member's initial value that is not constant is not supported yet in PHP#.",
                "An initial value that is not constant runs in the constructor, which a static member does not wait for.",
            )));
        }
    }

    let is_override = property.modifiers().iter().any(|modifier| matches!(modifier, Modifier::Override(_)));
    if is_override && !matches!(property, Property::Plain(_)) {
        return Err(Box::new(not_supported(
            property.first_variable().span,
            "Overriding a property is not supported yet in PHP#.",
            "`override` takes a field that replaces a plain PHP parent's property, as in `protected override string table = \"orders\";`.",
        )));
    }

    match property {
        Property::Plain(field) => {
            if let Some(public) = field.modifiers.get_public()
                && !is_override
            {
                Err(Box::new(
                    Issue::error("A PHP# field cannot be `public`: a field is `private` or `protected`.")
                        .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                        .with_help("Declare a property, as in `public int views { get; set; }`, to make it public."),
                ))
            } else if is_override && property.initial_value().is_none() {
                Err(Box::new(
                    Issue::error(
                        "An override needs an initial value: it replaces the default of the parent's property.",
                    )
                    .with_annotation(
                        Annotation::primary(property.first_variable().span)
                            .with_message("Declared without a value here."),
                    )
                    .with_help("Write the new default, as in `protected override string table = \"orders\";`."),
                ))
            } else if is_override
                && let Some(value) = property.initial_value()
                && !value.is_constant(version, false)
            {
                Err(Box::new(
                    Issue::error(
                        "The initial value of an override must be constant: the parent's constructor may read it before this class's code runs.",
                    )
                    .with_annotation(Annotation::primary(value.span()).with_message("Not constant."))
                    .with_help("Write a constant value, such as a literal or a list of literals."),
                ))
            } else if field.items.len() > 1 {
                Err(Box::new(not_supported(
                    field.span(),
                    "A field declaring several names is not supported yet in PHP#.",
                    supported(Place::FieldOrProperty),
                )))
            } else {
                Ok(())
            }
        }
        Property::Hooked(auto_property) => {
            if !writes_access(&auto_property.modifiers) {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A property without `public`, `protected` or `private` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
            } else if let PropertyItem::Concrete(item) = &auto_property.item {
                Err(Box::new(not_supported(
                    item.span(),
                    "An initial value before the accessors is not supported yet in PHP#.",
                    "A PHP# property writes its initial value after its accessors: `public int views { get; set; } = 0;`.",
                )))
            } else if let Some(initial_value) = &auto_property.initial_value
                && !initial_value.value.is_constant(version, false)
                && let Some(setter) = auto_property
                    .hook_list
                    .setter()
                    .filter(|setter| matches!(setter.body, PropertyHookBody::Concrete(_)))
            {
                let setter = BytesDisplay(setter.name.value);

                Err(Box::new(not_supported(
                    initial_value.value.span(),
                    &format!(
                        "An initial value that is not constant, on a property whose `{setter}` has a body, is not supported yet in PHP#."
                    ),
                    &format!(
                        "A constant initial value is the property's default, while any other would run the `{setter}` body at the start of the constructor."
                    ),
                )))
            } else if auto_property.initial_value.is_none()
                && matches!(auto_property.hint, Some(Hint::Nullable(_)))
                && auto_property.hook_list.is_get_only()
                && names.has_storage(&auto_property.hook_list)
            {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A get-only nullable property without an initial value is not supported yet in PHP#.",
                    "A get-only property runs as PHP's `readonly`, which takes no default, so it cannot start as null: give it an initial value, as in `public int? total { get; } = null;`, or a `set` accessor.",
                )))
            } else {
                Ok(())
            }
        }
        Property::Computed(computed) => {
            if !writes_access(&computed.modifiers) {
                Err(Box::new(not_supported(
                    computed.variable.span,
                    "A property without `public`, `protected` or `private` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
            } else if let Some(r#static) = computed.modifiers.get_static() {
                Err(Box::new(not_supported(
                    r#static.span(),
                    "A static computed property is not supported yet in PHP#.",
                    "PHP has no hooks on a static property, so a computed property is an instance property.",
                )))
            } else {
                Ok(())
            }
        }
    }
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

/// Whether the slice has a class constant, with the refusal when it does not: an access modifier and one name.
/// Whether a member writes its access, spec section 5's `internal` included, which the walk refuses on its own as not
/// supported yet.
fn writes_access(modifiers: &Sequence<Modifier>) -> bool {
    modifiers.contains_visibility() || modifiers.iter().any(|modifier| matches!(modifier, Modifier::Internal(_)))
}

fn is_slice_constant(constant: &ClassLikeConstant) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Box::new(
            Issue::error(message)
                .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
                .with_note(note),
        )
    };

    if constant.items.len() > 1 {
        Err(not_supported(
            constant.span(),
            "A constant declaring several names is not supported yet in PHP#.",
            supported(Place::ClassConstant),
        ))
    } else if !writes_access(&constant.modifiers) {
        Err(not_supported(
            constant.first_item().name.span,
            "A constant without `public`, `protected` or `private` is not supported yet in PHP#.",
            "A PHP# member without an access modifier is `private`, while PHP makes it `public`.",
        ))
    } else {
        Ok(())
    }
}

/// Checks the accessors of a property, declared in the class body or on a constructor parameter: `get` once, and an
/// optional `set` or `init` once, which may take an access modifier narrower than the property's, as in C#. `init`
/// sets the property only while its object is created, spec section 6.1. Each is `;`, an expression body `=> expr;`
/// or a block. An access modifier on `get` and a parameter list on `set` are not supported yet.
fn check_accessors(
    modifiers: &Sequence<Modifier>,
    accessors: &PropertyHookList,
    name: Span,
    context: &mut Context<'_, '_, '_>,
) {
    let reach = modifiers.get_first_read_visibility().map_or(0, visibility_reach);
    let mut declared: Vec<&[u8]> = Vec::new();

    for accessor in &accessors.hooks {
        let name = accessor.name.value;
        if !is_slice_accessor(accessor) {
            report_not_supported(
                accessor.span(),
                "accessor",
                "A PHP# property's accessors are `get` and `set` or `init`, each written `;`, `=> expr;` or with a block body, and `set` and `init` may take `private` or `protected`.",
                context,
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
        } else if accessor.modifiers.first().is_some_and(|modifier| visibility_reach(modifier) >= reach) {
            let setter = BytesDisplay(name);

            context.report(
                Issue::error(format!("The `{setter}` accessor of a PHP# property must be narrower than the property."))
                    .with_annotation(Annotation::primary(accessor.span()).with_message("Declared here."))
                    .with_help(format!(
                        "Write `private {setter};` or `protected {setter};` narrower than the property, or `{setter};`."
                    )),
            );
        }

        declared.push(name);
    }

    if !declared.contains(&b"get".as_slice()) {
        context.report(
            Issue::error("A PHP# property needs a `get` accessor.")
                .with_annotation(Annotation::primary(name).with_message("Declared here."))
                .with_help("Add `get;`, as in `public int views { get; set; }`."),
        );
    }
}

/// Whether the slice has an accessor: `get`, `set` or `init` without attributes, `&` or a parameter list, where only
/// `set` and `init` take an access modifier, `private`, `protected` or `public`, which `check_accessors` compares with
/// the property's.
fn is_slice_accessor(accessor: &PropertyHook) -> bool {
    accessor.attribute_lists.is_empty()
        && accessor.ampersand.is_none()
        && accessor.parameter_list.is_none()
        && match accessor.modifiers.as_slice() {
            [] => accessor.name.value == b"get" || accessor.is_setter(),
            [Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)] => accessor.is_setter(),
            _ => false,
        }
}

/// Reports a property without storage that something sets: an initial value, or a constructor that receives it.
fn report_no_storage(property: &DirectVariable, span: Span, set_by: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!(
            "The property `{}` has no storage for {set_by}: give it an auto accessor, such as `get;`, or use `field` in an accessor body.",
            BytesDisplay(property.name)
        ))
        .with_annotation(Annotation::primary(span).with_message("Set here."))
        .with_note("Its accessors compute every read and write, as PHP's virtual property does."),
    );
}

/// Checks the bodies of a property's accessors: the returns PHP's hooks allow, `field` in a lambda, and the property
/// read through `this` inside its own accessor.
fn check_accessor_bodies(accessors: &PropertyHookList, property: &[u8], context: &mut Context<'_, '_, '_>) {
    for accessor in accessors.hooks.iter().filter(|accessor| is_slice_accessor(accessor)) {
        if let PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block)) = &accessor.body {
            check_accessor_returns(accessor.name.value, block, context);
        }

        check_accessor_node(Node::PropertyHookBody(&accessor.body), property, false, context);
    }
}

/// php-src gives a `get` hook the property's type and a `set` hook `void`, so a `get` block returns a value and a
/// `set` or `init` block, which runs as the `set` hook, returns none. A lambda's returns are its own.
fn check_accessor_returns(accessor: &[u8], block: &Block, context: &mut Context<'_, '_, '_>) {
    for r#return in mago_syntax::utils::find_returns_in_block(block) {
        match (accessor, &r#return.value) {
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

/// How far a visibility modifier reaches: `private` least, then `protected`, then `public`.
const fn visibility_reach(modifier: &Modifier) -> u8 {
    match modifier {
        Modifier::Public(_) | Modifier::PublicSet(_) => 2,
        Modifier::Protected(_) | Modifier::ProtectedSet(_) => 1,
        _ => 0,
    }
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

/// Checks a ternary, and returns the place of its parts. PHP#'s ternary is `c ? a : b`, and PHP's `a ?: b` does not
/// exist, as spec section 21 says, so `?:` is reported between its two characters. A ternary as the condition of
/// another needs parentheses, as in PHP 8, so it is reported over that condition, which the walk then skips.
fn check_conditional(conditional: &Conditional, context: &mut Context<'_, '_, '_>) -> Place {
    if conditional.then.is_none() {
        context.report(
            Issue::error("PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.")
                .with_annotation(
                    Annotation::primary(Span::between(conditional.question_mark, conditional.colon))
                        .with_message("Written here."),
                )
                .with_note("`?:` tests truthiness, so it replaces `\"\"` and `\"0\"` along with null."),
        );
    } else if let Expression::Conditional(nested @ Conditional { then: Some(_), .. }) = conditional.condition {
        context.report(
            Issue::error(
                "Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`.",
            )
            .with_annotation(Annotation::primary(nested.span()).with_message("Nested here.")),
        );

        return Place::RefusedPart(nested.span());
    }

    Place::Body
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
            Pattern::Type(_) => {}
            Pattern::Not(not) => chained_values(not.pattern, values),
            Pattern::Binary(binary) => {
                chained_values(binary.left, values);
                chained_values(binary.right, values);
            }
            Pattern::Parenthesized(parenthesized) => chained_values(parenthesized.pattern, values),
            Pattern::Properties(properties) => {
                for property in &properties.properties {
                    chained_values(property.pattern, values);
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

/// The first body of an `if`, `else` or loop that is not a block in braces, with the block to write in its place. An
/// `else` may also hold the next `if`, whose body is a block too. A colon-delimited body, from its `:` to its `endif;`,
/// `endwhile;` or `endfor;`, is written as blocks whole, its `elseif` clauses as `else if`. A lone `;` body is the
/// empty block.
fn body_without_braces(statement: &Statement, context: &Context<'_, '_, '_>) -> Option<(Span, String)> {
    let unbraced = |body: &Statement| match body {
        Statement::Block(_) => None,
        Statement::Noop(_) => Some((body.span(), "{ }".to_owned())),
        _ => Some((body.span(), format!("{{ {} }}", BytesDisplay(context.get_code_snippet(body))))),
    };
    let block = |statements: &Sequence<Statement>| {
        let written: Vec<String> =
            statements.iter().map(|statement| BytesDisplay(context.get_code_snippet(statement)).to_string()).collect();

        format!("{{ {} }}", written.join(" "))
    };

    match statement {
        Statement::If(If { body: IfBody::Statement(body), .. }) => {
            unbraced(body.statement).or_else(|| match body.else_clause.as_ref()?.statement {
                Statement::If(If { body: IfBody::Statement(next), .. }) => unbraced(next.statement),
                Statement::If(_) => None,
                next => unbraced(next),
            })
        }
        Statement::If(If { body: IfBody::ColonDelimited(body), .. }) => {
            let mut parts = vec![block(&body.statements)];
            parts.extend(body.else_if_clauses.iter().map(|clause| {
                let condition = BytesDisplay(context.get_code_snippet(clause.condition));

                format!("else if ({condition}) {}", block(&clause.statements))
            }));
            parts.extend(body.else_clause.iter().map(|clause| format!("else {}", block(&clause.statements))));

            Some((body.colon.join(body.terminator.span()), parts.join(" ")))
        }
        Statement::While(While { body: WhileBody::ColonDelimited(body), .. }) => {
            Some((body.colon.join(body.terminator.span()), block(&body.statements)))
        }
        Statement::For(For { body: ForBody::ColonDelimited(body), .. }) => {
            Some((body.colon.join(body.terminator.span()), block(&body.statements)))
        }
        Statement::While(While { body: WhileBody::Statement(body), .. })
        | Statement::For(For { body: ForBody::Statement(body), .. }) => unbraced(body),
        Statement::ForOf(for_of) => unbraced(for_of.body),
        Statement::DoWhile(do_while) => unbraced(do_while.statement),
        _ => None,
    }
}

/// Checks the arms of a `match`, and returns whether its children can be checked. Spec section 21 needs a `default`
/// arm on a value that is not an enum, and section 20 lets a `match` on an enum leave it out when its arms cover every
/// case. Only the analyzer knows the value's type, so a `match` whose arms all name `Class.y` values or `null` may
/// leave out `default`, and the analyzer reports the cases it misses. A `match` without arms covers no case, so it needs
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
    match pattern {
        Pattern::Type(TypePattern { variable: Some(variable), .. }) if hidden => context.report(
            Issue::error(format!(
                "`{}` is declared under `or` or `not`, where the pattern can match without a value for it.",
                BytesDisplay(variable.value)
            ))
            .with_annotation(Annotation::primary(variable.span).with_message("Declared here."))
            .with_help("Declare the variable outside `or` and `not`, or test the value again where you use it."),
        ),
        Pattern::Type(_) | Pattern::Value(_) | Pattern::Comparison(_) => {}
        Pattern::Not(not) => report_hidden_pattern_variables(not.pattern, true, context),
        Pattern::Binary(binary) => {
            let hidden = hidden || !binary.is_and();
            report_hidden_pattern_variables(binary.left, hidden, context);
            report_hidden_pattern_variables(binary.right, hidden, context);
        }
        Pattern::Parenthesized(parenthesized) => {
            report_hidden_pattern_variables(parenthesized.pattern, hidden, context)
        }
        Pattern::Properties(properties) => {
            for property in &properties.properties {
                report_hidden_pattern_variables(property.pattern, hidden, context);
            }
        }
    }
}

/// Whether the slice can write an expression: a local, a parameter, `field`, a member written `object.name`, or an
/// index of one of them, as in `counts["a"]`. A member written by its bare name passes, because
/// `check_constant_access` reports it with the name to write instead.
fn is_slice_target(target: &Expression, context: &Context<'_, '_, '_>) -> bool {
    match target {
        Expression::ConstantAccess(access) => {
            matches!(context.names.binding(&access.name), Some(Binding::Local(_) | Binding::Field | Binding::Member))
        }
        Expression::Access(Access::Property(_)) => true,
        Expression::ArrayAccess(access) => is_slice_target(access.array, context),
        _ => false,
    }
}

/// Whether the slice has a type: the built-in types of spec section 24, or a class written by its short name, or a
/// nullable type, or `List<T>`, `Map<TKey, TValue>` or `Set<T>` of spec section 12, or a function type of spec section
/// 14.1 without a `void` parameter, whose inner types the walk checks next. A `Set` holds an `int`, a `string` or a
/// named type, which the analyzer accepts only as a backed enum, because its elements run as the keys of a PHP array.
fn is_slice_type(hint: &Hint) -> bool {
    match hint {
        Hint::Integer(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::String(_)
        | Hint::Void(_)
        | Hint::Identifier(Identifier::Local(_))
        | Hint::Nullable(_) => true,
        Hint::Mixed(any) => any.value == ANY,
        Hint::Generic(generic) => match (generic.name.value, generic.arguments.as_slice()) {
            (b"List", [element]) => !matches!(element, Hint::Void(_)),
            (b"Map", [key, value]) => !matches!(key, Hint::Void(_)) && !matches!(value, Hint::Void(_)),
            (b"Set", [element]) => {
                matches!(element, Hint::Integer(_) | Hint::String(_) | Hint::Identifier(Identifier::Local(_)))
            }
            _ => false,
        },
        Hint::Function(function) => !function.parameters.iter().any(|parameter| matches!(parameter, Hint::Void(_))),
        _ => false,
    }
}

/// Checks a union type. A type written twice is compared as the engine compares it: a class by its full name,
/// ignoring case.
fn check_union(union: &UnionHint, place: Place, context: &mut Context<'_, '_, '_>) {
    let mut members = Vec::new();
    union_members(union, &mut members);

    for (index, member) in members.iter().enumerate() {
        let is_slice_member = match member {
            Hint::Null(_) => {
                report_not_supported(
                    member.span(),
                    "union that holds null",
                    "PHP# writes a union that holds null in parentheses with `?` after it, as in `(int|string)?`.",
                    context,
                );

                false
            }
            Hint::Void(_) | Hint::Nullable(_) => false,
            Hint::Self_(keyword) => check_self_type(keyword, place, context),
            Hint::Static(keyword) => {
                report_php_static(keyword, context);

                false
            }
            _ if is_slice_type(member) => true,
            _ => {
                report_not_supported(member.span(), "type", supported(place), context);

                false
            }
        };

        if !is_slice_member {
            continue;
        }
        let Some(first) = members[..index].iter().find(|earlier| is_same_type(earlier, member, context)) else {
            continue;
        };

        let written = BytesDisplay(context.get_code_snippet(*member));
        context.report(
            Issue::error(format!("Duplicate type `{written}` is redundant."))
                .with_annotation(Annotation::primary(member.span()).with_message("Written again here."))
                .with_annotation(Annotation::secondary(first.span()).with_message("First written here."))
                .with_help("Remove the second one, as PHP refuses a union that names a type twice."),
        );
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
        report_php_self(keyword, is_return_type, context);
    } else if !is_return_type {
        context.report(
            Issue::error("`Self` is only a return type in PHP#.")
                .with_annotation(Annotation::primary(keyword.span).with_message("Written here."))
                .with_note("`Self` is the class a static method is called on, which a method returns."),
        );
    }

    is_sharp_self(keyword) && is_return_type
}

/// Reports PHP's `self`, which spec section 25 removes: a class writes its own name, or `Self` where `self_fits`.
fn report_php_self(keyword: &Keyword, self_fits: bool, context: &mut Context<'_, '_, '_>) {
    let class = enclosing_class_like_name(context.program, keyword.span)
        .map_or(String::new(), |name| format!(", `{}`,", BytesDisplay(name)));
    let or_self = if self_fits { ", or `Self` for the class a static method is called on" } else { "" };

    context.report(
        Issue::error(format!(
            "PHP# has no `self`: write the class's own name{class} for the declaring class{or_self}."
        ))
        .with_annotation(Annotation::primary(keyword.span).with_message("Written here.")),
    );
}

/// Reports PHP's `static`, which spec section 25 writes `Self`.
fn report_php_static(keyword: &Keyword, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("PHP# writes `Self` for PHP's `static`.")
            .with_annotation(Annotation::primary(keyword.span).with_message("Written here.")),
    );
}

/// Decides `new` on a class written by its short name, on `Self`, or on PHP's `self` or `static`. `new Self(…)` needs
/// the class's constructor marked `required`, spec section 25, because `Self` can be any subclass. PHP's `self` and
/// `static` are reported at the keyword, so the arguments are still checked.
fn check_instantiation(instantiation: &Instantiation, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    match instantiation.class {
        Expression::Self_(keyword) if !is_sharp_self(keyword) => {
            report_php_self(keyword, true, context);

            return Some(Place::Instantiation);
        }
        Expression::Static(keyword) => {
            report_php_static(keyword, context);

            return Some(Place::Instantiation);
        }
        _ => {}
    }

    if instantiation.argument_list.is_none() {
        report_not_supported(
            instantiation.span(),
            "`new` without arguments",
            "PHP# creates an object with `new Class(arguments)`, parentheses included.",
            context,
        );

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
fn check_named_constructor(constructor: &NamedConstructor, class: &Class, context: &mut Context<'_, '_, '_>) {
    let class_name = BytesDisplay(class.name.value);
    let name = BytesDisplay(constructor.name.value);
    let mut report = |message: String, span: Span, written: &str| {
        context.report(Issue::error(message).with_annotation(Annotation::primary(span).with_message(written)));
    };

    if class.modifiers.contains_abstract() {
        report(
            "An abstract class has no objects of its own, so it declares no named constructor.".to_owned(),
            constructor.name.span,
            "Declared here.",
        );
    }

    if constructor.class.value != class.name.value {
        report(
            format!("A named constructor is declared on its own class, as in `{class_name}.{name}`."),
            constructor.class.span,
            "Written here.",
        );
    }

    if constructor.name.value.eq_ignore_ascii_case(class.name.value) {
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

    // Spec section 5's `internal` is an access, which the walk refuses as not supported yet.
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

    if !constructor.modifiers.iter().any(is_access) {
        report(
            "A named constructor without `public`, `protected` or `private` is not supported yet in PHP#.".to_owned(),
            constructor.name.span,
            "Declared here.",
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

fn is_same_type(first: &Hint, second: &Hint, context: &Context<'_, '_, '_>) -> bool {
    match (first, second) {
        (Hint::Identifier(first), Hint::Identifier(second)) => {
            context.names.get(first).eq_ignore_ascii_case(context.names.get(second))
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

/// Whether the slice has a prefix operator at a place. A parameter default has no `++`, `--` or `@`, and `enter`
/// refuses `@` outside the namespace `Sharp` before it calls this. Every operator is named, so a new one does not
/// compile until it is decided.
fn is_slice_prefix_operator(operator: &UnaryPrefixOperator, place: Place) -> bool {
    match operator {
        UnaryPrefixOperator::Negation(_)
        | UnaryPrefixOperator::Plus(_)
        | UnaryPrefixOperator::Not(_)
        | UnaryPrefixOperator::BitwiseNot(_) => true,
        UnaryPrefixOperator::PreIncrement(_)
        | UnaryPrefixOperator::PreDecrement(_)
        | UnaryPrefixOperator::ErrorControl(_) => place == Place::Body,
        UnaryPrefixOperator::Reference(_)
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

/// Checks a cast at a place, reporting it at its operator when the slice does not have it. Spec section 24 keeps
/// `(int)`, `(float)` and `(string)`, which the slice has in a method body, and removes PHP's other casts and its cast
/// aliases. The refusal of `(array)` names an operand that is a local or a parameter, as the spec's `(array)row` does.
/// Every cast is named, so a new one does not compile until it is decided.
fn check_cast(unary_prefix: &UnaryPrefix, place: Place, context: &mut Context<'_, '_, '_>) {
    let operator = &unary_prefix.operator;
    let compare = "compare the value instead, as in `count > 0` or `flag == \"1\"`.";
    let parse_or_wrap;
    let (cast, instead) = match operator {
        UnaryPrefixOperator::IntCast(..) | UnaryPrefixOperator::FloatCast(..) | UnaryPrefixOperator::StringCast(..)
            if place == Place::Body =>
        {
            return;
        }
        UnaryPrefixOperator::IntCast(..)
        | UnaryPrefixOperator::FloatCast(..)
        | UnaryPrefixOperator::StringCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::VoidCast(..) => {
            return report_not_supported(operator.span(), "operator", supported(place), context);
        }
        UnaryPrefixOperator::BoolCast(..) => ("(bool)", compare),
        UnaryPrefixOperator::BooleanCast(..) => ("(boolean)", compare),
        UnaryPrefixOperator::ArrayCast(..) => {
            let value = match unary_prefix.operand {
                Expression::ConstantAccess(ConstantAccess { name: name @ Identifier::Local(local) })
                    if matches!(context.names.binding(name), Some(Binding::Local(_))) =>
                {
                    local.value
                }
                _ => b"value".as_slice(),
            };
            let value = BytesDisplay(value);
            parse_or_wrap =
                format!("write a struct's `parse({value})` for an object, or `List.wrap({value})` for a value.");

            ("(array)", parse_or_wrap.as_str())
        }
        UnaryPrefixOperator::ObjectCast(..) => ("(object)", "write a `Map` literal, or a struct."),
        UnaryPrefixOperator::IntegerCast(..) => ("(integer)", "write `(int)`."),
        UnaryPrefixOperator::DoubleCast(..) => ("(double)", "write `(float)`."),
        UnaryPrefixOperator::RealCast(..) => ("(real)", "write `(float)`."),
        UnaryPrefixOperator::BinaryCast(..) => ("(binary)", "write `(string)`."),
        UnaryPrefixOperator::ErrorControl(_)
        | UnaryPrefixOperator::Reference(_)
        | UnaryPrefixOperator::BitwiseNot(_)
        | UnaryPrefixOperator::Not(_)
        | UnaryPrefixOperator::PreIncrement(_)
        | UnaryPrefixOperator::PreDecrement(_)
        | UnaryPrefixOperator::Plus(_)
        | UnaryPrefixOperator::Negation(_) => unreachable!("`enter` calls `check_cast` only for a cast"),
    };

    report_removed(
        operator.span(),
        cast.as_bytes(),
        instead,
        "PHP# keeps `(int)`, `(float)` and `(string)` between numbers, and removes PHP's other casts and its cast aliases.",
        context,
    );
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

/// Reports a PHP form that PHP# removes, with what to write instead and why.
fn report_removed(span: Span, form: &[u8], instead: &str, note: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("PHP# has no `{}`: {instead}", BytesDisplay(form)))
            .with_annotation(Annotation::primary(span).with_message("Written here."))
            .with_note(note),
    );
}

/// The PHP constructs spec sections 2, 8 and 29 remove that start with a word: the word, what PHP# writes instead,
/// and why. A `.sharp` file reads the word as a plain name, so the construct written as a call, or a bare `die`,
/// reaches the checker as a call of a function, or a read of a constant, that no code declares.
const REMOVED_CONSTRUCTS: [(&[u8], &str, &str); 8] = [
    (b"echo", "write `printf` or `fwrite`.", "Output is a function call in PHP#."),
    (b"print", "write `printf` or `fwrite`.", "Output is a function call in PHP#."),
    (
        b"die",
        "write the message to STDERR, then `exit(1)`.",
        "`die(\"…\")` and `exit(\"…\")` print the message and exit with status 0, which reports success.",
    ),
    (b"isset", "test it against null, as in `value != null`.", "A PHP# value is never undefined."),
    (
        b"empty",
        "compare the value itself, as in `items.count() == 0`.",
        "PHP's `empty` is true for `0`, `\"0\"`, `\"\"`, `[]` and null alike.",
    ),
    (
        b"unset",
        "a local ends at the `}` that closes its block, and `map.remove(key)` removes a Map's entry.",
        "A PHP# local never becomes undefined.",
    ),
    (b"array", "write a list literal, as in `[1, 2]`.", "PHP# writes a list or a map literal in square brackets."),
    (
        b"list",
        "read each element into its own local, as in `const first = pair[0];`.",
        "PHP# reads a list's elements by index.",
    ),
];

/// Reports the PHP construct `word` starts, when spec sections 2, 8 and 29 remove it.
fn report_removed_construct(word: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    if let Some((_, instead, note)) =
        REMOVED_CONSTRUCTS.iter().find(|(construct, ..)| construct.eq_ignore_ascii_case(word))
    {
        report_removed(span, word, instead, note, context);
    }
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

/// Reports a declared name written `__Something__`, which spec section 27 removes as it removes reading one.
fn check_declared_name(name: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    if is_magic_name(name) {
        report_magic_constant(name, span, None, context);
    }
}

/// Reports a PHP superglobal, written with `$` or bare, which spec section 29 removes.
fn report_superglobal(span: Span, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error("PHP# has no superglobals; take a Request")
            .with_annotation(Annotation::primary(span).with_message("Written here."))
            .with_note(
                "Request data arrives as an object, such as a framework's `Request`, and the process environment as `Environment`.",
            ),
    );
}

/// Reports a PHP magic constant, or another `__Something__` name when `constant` is `None`, which spec section 27
/// replaces with `Position`. The error names it as written.
fn report_magic_constant(name: &[u8], span: Span, constant: Option<&MagicConstant>, context: &mut Context<'_, '_, '_>) {
    let instead = match constant {
        Some(MagicConstant::Directory(_)) => "write `Position.current().directory`.",
        Some(MagicConstant::File(_)) => "write `Position.current().file`.",
        Some(MagicConstant::Line(_)) => "write `Position.current().line`.",
        Some(MagicConstant::Function(_) | MagicConstant::Method(_)) => "write `Position.current().function`.",
        Some(MagicConstant::Class(_)) => "write `typeof(Class)` with the class's name.",
        Some(MagicConstant::Namespace(_)) => "write `Position.current().function`, which starts with the namespace.",
        Some(MagicConstant::Trait(_) | MagicConstant::Property(_)) | None => {
            "`Position.current()` gives the file, directory, line, column and function."
        }
    };

    report_removed(
        span,
        name,
        instead,
        "PHP# removes PHP's magic constants and every other `__Something__` name: `Position` says where code sits in its source.",
        context,
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

fn report_unsupported(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    let construct = match node {
        Node::Statement(_) => "statement",
        Node::Expression(_) | Node::ConstantAccess(_) => "expression",
        Node::BinaryOperator(_) | Node::UnaryPrefixOperator(_) | Node::AssignmentOperator(_) => "operator",
        Node::Hint(_) | Node::NullableHint(_) => "type",
        Node::Pattern(_) => "pattern",
        Node::Modifier(_) => "modifier",
        Node::IfStatementBodyElseIfClause(_) => {
            return report_not_supported(node.span(), "`elseif`", "PHP# writes `else if`.", context);
        }
        Node::Extends(_) => "`extends` clause",
        Node::Implements(_) => "`implements` clause",
        Node::ClassLikeMember(_) => "class member",
        Node::ClassLikeMemberSelector(_) => "member name",
        _ => "construct",
    };

    report_not_supported(node.span(), construct, supported(place), context);
}

/// What the slice has at a place, as the note of a refusal there.
const fn supported(place: Place) -> &'static str {
    match place {
        Place::File => "At file level, PHP# supports `namespace`, `import`, `class`, `interface`, `enum` and `event`.",
        Place::Class => {
            "A PHP# class has attributes, an optional `public`, `abstract` or `final`, a name, an optional `: Base, Interface` header, constants, fields, properties, methods, named constructors, operators and laws, with no other modifiers, `extends` or `implements`."
        }
        Place::Interface => {
            "A PHP# interface has an optional `public`, a name, an optional `: Interface` header, methods and get-only properties, as in `int orderId { get; }`, with no attributes, other modifiers, `extends`, constants or other properties."
        }
        Place::Signature => {
            "A PHP# interface method has no modifier, parameters, a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, a class, `List<T>`, `Map<TKey, TValue>`, `Set<T>` or `Self`, each but `void` nullable as in `int?`, or a union of them but `void`, as in `int|string`, and no body."
        }
        Place::Enum => {
            "A PHP# enum has attributes, an optional `public`, a name, an optional `: string, Interface` header whose `int` or `string` comes first, constants, cases, methods and laws, with no other modifiers or `implements`."
        }
        Place::Event => {
            "A PHP# event has an optional `public`, a name, parameters, each a type and a name with an optional default, and an optional `: Interface` header, as in `public event OrderPaid(int orderId) : OrderEvent;`."
        }
        Place::Module => "A PHP# module holds service bindings and laws.",
        Place::FieldOrProperty => {
            "A PHP# field is `private` or `protected`, and a property has the accessors `get` and an optional `set` or `init`, each `;`, `=> expr;` or a block. Both may be `static`, and have a type of `int`, `float`, `bool`, `string`, `Any`, a class, `List<T>`, `Map<TKey, TValue>`, `Set<T>` or `Function<R(P)>`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional initial value."
        }
        Place::ClassConstant => {
            "A PHP# constant has `public`, `protected` or `private`, an optional type of `int`, `float`, `bool`, `string`, `Any` or a class, nullable as in `int?` or not, or a union of them as in `int|string`, one name, and a constant value."
        }
        Place::Method => {
            "A PHP# method takes `public`, `protected`, `private`, `static`, `abstract`, `virtual` and `override`, a constructor also `required`, parameters, and a return type of `int`, `float`, `bool`, `string`, `Any`, `void`, a class, `List<T>`, `Map<TKey, TValue>`, `Set<T>`, `Function<R(P)>` or `Self`, each but `void` nullable as in `int?`, or a union of them but `void`, as in `int|string`, and an optional `on` clause after the parameters, as in `on OrderPaid, OrderRefunded`."
        }
        Place::Parameter => {
            "A PHP# parameter has a type of `int`, `float`, `bool`, `string`, `Any`, a class, `List<T>`, `Map<TKey, TValue>`, `Set<T>` or `Function<R(P)>`, nullable as in `int?` or not, or a union of them as in `int|string`, a name, and an optional default. The last parameter can be variadic, as in `int ...values`."
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
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const` and typed locals, `if` with `else if` and `else`, `while`, `do … while`, `for`, `for … of`, `break` and `continue` without a level, and `try` with `catch` and `finally`, with literals, list and map literals, index reads, templates, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `??`, the ternary `c ? a : b`, `(int)`, `(float)` and `(string)`, `++` and `--`, method calls and property reads written with `.` or `?.`, lambdas as in `x => x.id`, `new Class(...)`, `new Class.name(...)` and, with a `required` constructor, `new Self(...)`, calls of global functions and of a local that holds a lambda, `super.method(...)` and `Self.method(...)`, each with positional, named and spread arguments as in `max(...prices)`, `throw`, `exit(code)`, `typeof(Class)`, `is`, `as` and `match`."
        }
        Place::Attribute => {
            "A PHP# attribute is a class name with optional positional and named arguments, as in `[Field(\"Name\", searchable: true)]`."
        }
        Place::Constant => {
            "A parameter default, an attribute argument, a constant's value or an enum case's value is a literal, a constant, `typeof(Class)`, a list or map literal, or arithmetic, comparison, logical and `??` operators on them."
        }
        Place::Pattern => {
            "A PHP# pattern is a type of `int`, `float`, `string`, `bool` or a class with an optional name, as in `int count`, a value, a comparison such as `< 10`, a properties pattern such as `{ total: > 0 }`, or patterns joined by `and`, `or` and `not`."
        }
    }
}

fn report_not_supported(span: Span, construct: &str, supported: &str, context: &mut Context<'_, '_, '_>) {
    context.report(
        Issue::error(format!("This {construct} is not supported yet in PHP#."))
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(supported),
    );
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
    check_local_name(local_declaration.name.value, local_declaration.name.span, "local", context);

    if local_declaration.hint.is_some() {
        return;
    }

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
    let name = BytesDisplay(local_declaration.name.value);

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
        report_dollar_variable(parameter.variable.name, parameter.variable.span, context);
    } else {
        check_local_name(parameter.variable.name, parameter.variable.span, "parameter", context);
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
        };

        context.report(issue);
    }
}

/// Checks the name of a PHP# class, interface, enum or event against the type names the engine reserves, against the
/// `__Something__` names spec section 27 removes, and against spec section 24's capital letter. A name gets the first
/// of these errors only. Any other word names a type, a PHP keyword PHP# doesn't use included, and so does a
/// capitalized PHP# keyword, since PHP#'s keywords are lowercase but `Self`, which the engine reserves. The standard
/// library's `Sharp.Int`, `Sharp.Float` and `Sharp.Bool` are the engine's one exception, and the analyzer refuses
/// them in any file that is not the standard library's.
#[inline]
pub fn check_class_name(class_name: &LocalIdentifier, context: &mut Context<'_, '_, '_>) {
    if is_reserved_class_name(class_name.value) && !is_sharp_type_class(context.get_name(class_name.span.start)) {
        let name = BytesDisplay(class_name.value);

        context.report(
            Issue::error(format!("Cannot use `{name}` as a class name: it is reserved."))
                .with_annotation(Annotation::primary(class_name.span).with_message("Class declared here."))
                .with_note("PHP# reserves this name for a type."),
        );
    } else if is_magic_name(class_name.value) {
        check_declared_name(class_name.value, class_name.span, context);
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
#[inline]
pub fn check_declarations(program: &Program, context: &mut Context<'_, '_, '_>) {
    let (classes, imports) = declarations(program);
    let name = first_namespace(program).and_then(|namespace| namespace.name.as_ref());

    if let Some(name) = name {
        let mut start = 0;

        for part in name.value().split(|&byte| byte == b'.') {
            let end = start + part.len() as u32;

            check_capitalized(part, name.span().subspan(start, end), context);
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
        // A bare `die`, which no local names, is PHP's construct, and `TRUE` is `true` written in another case.
        Some(Binding::Constant) if !is_in_file_level_code(context) => {
            let name = constant_access.name.value();
            match ["true", "false", "null"].into_iter().find(|literal| literal.as_bytes().eq_ignore_ascii_case(name)) {
                Some(literal) => context.report(
                    Issue::error(format!("Write `{literal}` in lowercase."))
                        .with_annotation(Annotation::primary(constant_access.span()).with_message("Written here.")),
                ),
                None => report_removed_construct(name, constant_access.span(), context),
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

    let function = identifier.last_segment();
    if context.names.binding(identifier).is_none() && !is_in_file_level_code(context) {
        report_removed_construct(function, function_call.span(), context);
    }

    if is_compact_or_extract(function) {
        let function = BytesDisplay(function);

        context.report(
            Issue::error(format!("`{function}()` is not part of PHP#."))
                .with_annotation(Annotation::primary(function_call.span()).with_message("Called here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        );
    } else if function.eq_ignore_ascii_case(b"assert")
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

fn is_compact_or_extract(function: &[u8]) -> bool {
    function.eq_ignore_ascii_case(b"compact") || function.eq_ignore_ascii_case(b"extract")
}

#[inline]
pub fn check_function(function: &Function, context: &mut Context<'_, '_, '_>) {
    let name = BytesDisplay(function.name.value);

    context.report(
        Issue::error(format!("PHP# has no top-level functions: move `{name}` into a class as a static method."))
            .with_annotation(Annotation::primary(function.name.span).with_message("Function declared here.")),
    );
}

#[inline]
pub fn check_global(global: &Global, context: &mut Context<'_, '_, '_>) {
    if is_in_file_level_code(context) {
        return;
    }

    context.report(
        Issue::error("`global` is not part of PHP#.")
            .with_annotation(Annotation::primary(global.global.span).with_message("Used here."))
            .with_help("Pass the value in as a parameter instead."),
    );
}

fn check_variable(variable: &Variable, context: &mut Context<'_, '_, '_>) {
    match variable {
        Variable::Direct(direct) => report_dollar_variable(direct.name, direct.span, context),
        Variable::Indirect(_) | Variable::Nested(_) => context.report(
            Issue::error("Variable variables are not part of PHP#.")
                .with_annotation(Annotation::primary(variable.span()).with_message("Used here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        ),
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

/// A class or an enum a PHP# file declares. The slice's class rules treat both alike.
#[derive(Clone, Copy)]
struct ClassLike<'ast, 'arena> {
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    members: &'ast Sequence<'arena, ClassLikeMember<'arena>>,
    is_static: bool,
}

impl<'ast, 'arena> ClassLike<'ast, 'arena> {
    fn of_class(class: &'ast Class<'arena>) -> Self {
        Self {
            name: &class.name,
            span: class.span(),
            members: &class.members,
            is_static: class.modifiers.contains_static(),
        }
    }
}

/// The classes, enums and imports a PHP# file declares at its top level and in its first namespace, in source order.
fn declarations<'ast, 'arena>(
    program: &'ast Program<'arena>,
) -> (Vec<ClassLike<'ast, 'arena>>, Vec<&'ast UseItem<'arena>>) {
    let mut classes = Vec::new();
    let mut imports = Vec::new();
    for statement in &program.statements {
        collect_declarations(statement, &mut classes, &mut imports);
    }

    // The slice refuses a second namespace, so its declarations are not the file's.
    for statement in first_namespace(program).into_iter().flat_map(|namespace| namespace.statements().iter()) {
        collect_declarations(statement, &mut classes, &mut imports);
    }

    (classes, imports)
}

fn collect_declarations<'ast, 'arena>(
    statement: &'ast Statement<'arena>,
    classes: &mut Vec<ClassLike<'ast, 'arena>>,
    imports: &mut Vec<&'ast UseItem<'arena>>,
) {
    match statement {
        Statement::Class(class) => classes.push(ClassLike::of_class(class)),
        Statement::Enum(r#enum) => {
            classes.push(ClassLike {
                name: &r#enum.name,
                span: r#enum.span(),
                members: &r#enum.members,
                is_static: false,
            });
        }
        Statement::Use(Use { items: UseItems::Sequence(sequence), .. }) => imports.extend(sequence.items.iter()),
        _ => {}
    }
}

/// Checks the name of a PHP# local or parameter, which runs as the PHP variable of the same name, and a
/// `__Something__` name, which spec section 27 removes.
fn check_local_name(name: &[u8], span: Span, kind: &str, context: &mut Context<'_, '_, '_>) {
    if name == b"this" {
        context.report(
            Issue::error(format!("Cannot name a {kind} `this`: `this` is the object the method runs on."))
                .with_annotation(Annotation::primary(span).with_message("Declared here."))
                .with_note(format!("A PHP# {kind} runs as a PHP variable of the same name, and PHP forbids `$this`.")),
        );
    } else if SUPERGLOBALS.contains(&name) {
        let name = BytesDisplay(name);

        context.report(
            Issue::error(format!("`{name}` is the name of a PHP superglobal: rename this {kind}."))
                .with_annotation(Annotation::primary(span).with_message("Declared here."))
                .with_note(format!("A PHP# {kind} runs as a PHP variable of the same name, which would be `${name}`.")),
        );
    } else if is_sharp_keyword(name) {
        let renamed = format!("{}Value", name.to_ascii_lowercase().escape_ascii());
        let name = BytesDisplay(name);

        context.report(
            Issue::error(format!("`{name}` is a PHP# keyword, so it can't name a {kind}."))
                .with_annotation(Annotation::primary(span).with_message("Declared here."))
                .with_help(format!("Rename it, as in `{renamed}`.")),
        );
    } else {
        check_declared_name(name, span, context);
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

fn report_dollar_variable(name: &[u8], span: Span, context: &mut Context<'_, '_, '_>) {
    let bare = BytesDisplay(name.strip_prefix(b"$").unwrap_or(name));

    context.report(
        Issue::error(format!("PHP# variables have no `$`: write `{bare}`."))
            .with_annotation(Annotation::primary(span).with_message("Written with `$` here.")),
    );
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

/// The class of the static method or the law whose body holds `span`, which has no `this`.
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

/// Refuses a lambda that captures a loop variable that code changes. Spec section 3 gives each loop pass its own
/// variable, and a lambda captures the variable itself, so the engine would capture a changing one by reference, and
/// every pass's lambda would share the one PHP variable the loop reuses.
fn check_lambda_captures(lambda: Node<'_, '_>, context: &mut Context<'_, '_, '_>) {
    let captures = context.names.captures(&lambda.span()).to_vec();
    for (_, local) in captures {
        if !context.names.is_written(&local) || !is_loop_variable(context.program, &local) {
            continue;
        }

        let uses = lambda.filter_map(|node| match node {
            Node::ConstantAccess(access) if context.names.binding(&access.name) == Some(Binding::Local(local)) => {
                Some(access.span())
            }
            _ => None,
        });
        report_not_supported(
            uses.first().copied().unwrap_or_else(|| lambda.span()),
            "capture of a loop variable that changes",
            "Each loop pass has its own loop variable, which the engine cannot give a lambda yet when code changes it. Copy it into a `const` in the loop body, and capture that.",
            context,
        );
    }
}

/// Whether `local` is the counter a `for` declares or a variable a `for … of` declares.
fn is_loop_variable(program: &Program, local: &Local) -> bool {
    let declares = |node: &Node<'_, '_>| match node {
        Node::For(r#for) => {
            r#for.declaration.as_ref().is_some_and(|declaration| declaration.name.span == local.declaration)
        }
        Node::ForOf(for_of) => for_of.target.names().into_iter().any(|name| name.span == local.declaration),
        _ => false,
    };

    !Node::Program(program).filter_map(|node| declares(node).then_some(())).is_empty()
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
    let declared = declarations(context.program).0.into_iter().any(|declared| {
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
        .with_note("A default, a constant's value and a constant initial value read constants and enum cases only."),
    );
}

/// The class or enum of a PHP# file whose body holds `span`, in any of the file's namespaces, because the walk checks
/// the classes of a namespace it refuses too.
fn enclosing_class<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<ClassLike<'ast, 'arena>> {
    let mut classes = Vec::new();
    for statement in file_statements(program) {
        collect_declarations(statement, &mut classes, &mut Vec::new());
    }

    classes.into_iter().find(|class| class.span.contains(&span.start))
}

/// The name of the class, interface or enum of a PHP# file whose body holds `span`, in any of the file's namespaces.
fn enclosing_class_like_name<'arena>(program: &Program<'arena>, span: Span) -> Option<&'arena [u8]> {
    file_statements(program).find_map(|statement| match statement {
        Statement::Class(class) if class.span().contains(&span.start) => Some(class.name.value),
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
