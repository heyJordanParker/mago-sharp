use mago_bytes::BytesDisplay;
use mago_names::binding::Binding;
use mago_names::binding::BindingError;
use mago_names::binding::Local;
use mago_names::binding::LocalKind;
use mago_names::scope::php_name;
use mago_php_version::PHPVersion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Break;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Continue;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::Function;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Global;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::Method;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Namespace;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::Program;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::PropertyItem;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Terminator;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::UnaryPostfix;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Use;
use mago_syntax::cst::UseItem;
use mago_syntax::cst::UseItems;
use mago_syntax::cst::Variable;
use mago_syntax::cst::While;
use mago_syntax::cst::WhileBody;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::internal::consts::RESERVED_CLASS_NAMES;
use crate::internal::consts::RESERVED_KEYWORDS;
use crate::internal::consts::SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED;
use crate::internal::context::Context;

/// The PHP superglobals. A PHP# local or parameter of one of these names would read or replace it.
const SUPERGLOBALS: [&[u8]; 9] =
    [b"GLOBALS", b"_SERVER", b"_GET", b"_POST", b"_FILES", b"_COOKIE", b"_SESSION", b"_REQUEST", b"_ENV"];

/// Checks a PHP# file against the slice: the only constructs a `.sharp` file may use, and the contract the
/// engine's lowering implements.
///
/// - At file level: `namespace`, `import`, `class`, `interface` and `enum`. A file has at most one namespace, named
///   and written without braces.
/// - A class: attributes, an optional `public`, `abstract` or `final`, a name, an optional `: Base, Interface` header,
///   constants, fields, properties and methods, with no other modifiers, `extends` or `implements`. The engine tells
///   the base class from the interfaces when it links the class.
/// - An interface: an optional `public`, a name, an optional `: Interface` header, methods and properties, with no
///   attributes, other modifiers or `extends`. An interface method has parameters, a return type and no body, as spec
///   section 29 writes `Money quote(Cart cart);`, or a default body, a block or `=> expr;`, as spec section 22 writes
///   `string designKey() => …;`. A modifier on it is an error, because every interface method is public. A property is
///   `T p { get; }` or `T p { get; set; }` without a modifier or an initial value, or a computed property without a
///   modifier, whose expression is its default body. A default body is checked as a class method's body.
/// - An enum: attributes, an optional `public`, a name, an optional `: string, Interface` header, constants, cases and
///   methods, with no other modifiers or `implements`. A leading `int` or `string` in the header is the backing type,
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
///   An attribute target, as in `[return: NotNull]`, is a parse error.
/// - A constant: `public`, `protected` or `private`, an optional type from the types below but `void`, one name, and a
///   value as a parameter default is.
/// - A field: `private` or `protected`, an optional `static`, a type, one name and an optional initial value. A
///   `public` field is an error, because spec section 6 has no public fields.
/// - A static field or property: an initial value that is constant, which PHP stores as its default, and a `set`
///   accessor, because PHP has no `readonly` static property.
/// - An initial value: any expression a method body has, without `this`, which is an error. A constant initial value,
///   as a parameter default is, becomes the member's default, and any other runs at the start of the constructor.
/// - An auto-property: `public`, `protected` or `private`, an optional `static`, a type, one name, the accessors
///   `get;` and an optional `set;` that may take an access modifier narrower than the property's, and an optional
///   initial value after the accessors, which is a field's. A property without `get`, an accessor declared twice, or
///   a `set` access modifier as wide as the property's is an error, as in C#. A get-only property runs as `readonly`,
///   and the analyzer reports every write to it that `readonly` refuses.
/// - A computed property: `public`, `protected` or `private`, a type, one name and an expression body, as in
///   `public string slug => Str.slug(name);`. Its expression is a method body's expression and runs on each read. A
///   `static` computed property is not supported yet, because PHP has no hooks on a static property.
/// - A method: `public`, `protected` or `private`, an optional `static`, `virtual` or `override`, parameters, a return
///   type and a body, or `abstract` and no body. The analyzer decides `virtual` and `override`, as spec section 22
///   writes them. Its name does not start with `__`, which PHP reserves for magic methods, and is not its class's name,
///   compared ignoring case, which PHP# gives to the constructor, nor, compared ignoring case, a property's of its
///   class.
/// - The constructor: a method named exactly after its class, without a return type and not `static`. A method
///   without a return type named otherwise is an error. A constructor parameter with an access modifier declares a
///   member: a field when `private` or `protected` without accessors, and a property with accessors, which follow the
///   auto-property rules. A `public` parameter without accessors is an error, as spec section 9 says.
/// - A parameter: always a type, a name and an optional default, and neither variadic nor by reference. A default is
///   a constant expression: a literal, a constant, `typeof(X)`, a list or map literal of them, or the operators below
///   on them, without `++` and `--`. An optional parameter before a required one is an error, because PHP would make
///   it required.
/// - Types: `int`, `float`, `bool`, `string`, a class written by its short name, `List<T>` and `Map<TKey, TValue>`
///   of these, function types `Function<R(P1, P2)>` of these, and `void` as a return type, a function type's too. A
///   `Map`'s key is `int` or `string`. PHP's own check reports a `void` parameter. Each of them is nullable when
///   written with `?` after it, as in `int?`, and PHP's own check reports `void?`.
/// - A method body: a block, or an expression body, `=> expr;`, which returns the expression, or runs it as a
///   statement in a `void` method and the constructor, as in C#.
/// - In a method body: blocks, expression statements, `return`, `let` and `const` declarations, `if` with `else if`
///   and `else`, `while`, `do … while`, `for` with a `let` or `const` counter or with expressions, `for … of` over a
///   value or a key and value, `break` and `continue` without a level, and `try` with `catch` clauses and `finally`.
///   The body of `if`, `else` and each loop is a block in braces. A catch clause names one or more classes separated
///   by `|`, and an optional variable written without `$`, which lives until the clause's block ends. A local statement can have its type written, as in `Money? total = null;` or
///   `const int base = 2;`, from the types above but `void`.
/// - Writes: `=`, compound assignment, `++` and `--` write only a local, a parameter, a member written
///   `object.name` or, when static, `Class.name`, or an index of one of them written `target[key]`. The analyzer,
///   which knows the types, allows an index write only on a `Map`, and a read not under `??` or `?.` only on a `List`,
///   as spec section 12 decides.
/// - A constant, an enum case or a static member is reached through its class name, as in `Report.count`, and the
///   engine looks up which one it is when it runs. A bare constant or static member name is an error that names the
///   class. A constant expression, which is a parameter default, an attribute argument, a constant's value, a
///   constant initial value or an enum case's value, reads only a constant or an enum case this way, as PHP does: a
///   static field or property there is an error when this file declares it, and the analyzer reports it otherwise.
/// - In expressions: literals, list literals `[a, b]`, map literals `["key": value]` and `[:]`, index reads
///   `value[key]`, templates, parentheses, bare names, assignment, the operators below, and method calls and property
///   reads written with `.` or `?.` and a member name, `new Class(...)` on a class written by its short name, calls of
///   a function by its bare name, each with positional and named arguments, `throw`, which is an expression as in PHP,
///   `typeof(X)` on a class written by its short name, without a member read or called on it, and
///   `super.method(...)`, which calls the parent's method. `?.` never follows a class. A function is the global
///   function of that name, PHP's own or one a library or the app declares, as spec sections 8 and 29 keep them, and
///   the engine calls the global one. A string literal's `\u{...}` escapes are valid codepoints, as PHP requires. A
///   `"…"` string never interpolates, and a template, `` `Order ${number}` ``, interpolates any expression of this
///   list in each `${…}` and takes JavaScript's escapes, as spec section 18 writes them.
/// - Lambdas, as spec section 3 writes them: `x => x.id`, `(a, b) => a + b` and `() => { … }`, whose body is an
///   expression or a block of a method body. A parameter is a method's, with its type optional. A lambda captures the
///   variable itself, except a loop variable that code changes, whose capture is not supported yet. A call of a local
///   by its bare name, `f(x)`, calls the lambda the local holds.
/// - Operators: `+ - * / % **`, `== != === !== < > <= >=`, `&& || !`, `??`, unary `-` and `+`, `++` and `--`, and
///   `= += -= *= /= **= ??=`.
/// - The ternary `c ? a : b` in a method body, as spec section 21 writes it. PHP's `a ?: b` is an error, and a ternary as the condition
///   of another needs parentheses, as in PHP 8.
/// - Casts: `(int)`, `(float)` and `(string)` in a method body, as spec section 24 writes them. PHP's other casts and
///   its cast aliases, such as `(bool)` and `(integer)`, are errors.
/// - A bare `Int` or `Float` before `.` is the class `Sharp\Int` or `Sharp\Float` of the engine's standard library,
///   unless the file imports the name, so `Int.parse(text)` and `Float.tryParse(text)` call it. PHP reserves both
///   names, so no file declares a class of either.
///
/// The check runs on every node the checking walk enters, and refuses any node, or any position of a node, that this
/// list does not name. It reports each refusal once, at its outermost node, and skips the nodes inside it. It does not
/// run on a file with a parse error, which is the one error to fix first. The constructs PHP# never has, such as `$`
/// variables, `global` and top-level functions, keep their own errors.
///
/// Five more refusals need inferred types or the codebase, so the analyzer makes them as its part of this contract:
/// - `+` that may join a string with any other value, which spec section 18 makes an error, in
///   `analyze_arithmetic_operation`. `+` on two strings joins them.
/// - a condition of `if`, `while`, `do … while`, `for` or `? :`, or an operand of `&&`, `||` or `!`, that is not
///   `bool`, which spec section 21 makes an error, in `Context::report_non_bool_condition`.
/// - a cast of a value that is not an `int` or a `float`, which spec section 24 makes an error, in `UnaryPrefix`'s
///   `analyze`.
/// - a call that resolves to a namespaced function, in `report_namespaced_function_call`.
/// - a full name in code, such as `App.Shared.Money.of(1)`, which spec section 23 keeps in `import` lines, in
///   `report_full_name`. One file cannot tell it from a class and its member, such as `Status.Active`.
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
    /// An interface method's parameters, return type and default body, whose block or expression is the method body's.
    Signature,
    /// An enum and its members.
    Enum,
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
    /// An attribute list: its attributes' names and arguments.
    Attribute,
    /// A constant expression: a parameter default, an attribute argument, a class constant's value or an enum case's
    /// value.
    Constant,
}

/// Decides one node at its place. Returns the place of its children when the slice has the node, and `None` when
/// the node is reported.
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
    use Place::Signature;
    use Place::SuperCall;
    use Place::TryCatchClause;

    if let Some(target) = write_target(node) {
        // PHP# never has `$` variables, and `check_variable` reports a write to one.
        if !matches!(target, Expression::Variable(_)) && !is_slice_target(target, context) {
            report_not_supported(
                target.span(),
                "write target",
                "PHP# writes to a local, a parameter or a member written `object.name`.",
                context,
            );

            return None;
        }

        if let Expression::Access(Access::Property(property)) = target {
            check_member_access(property.object, &property.property, context);
        }
    }

    match (node, place) {
        (Node::Statement(Statement::Namespace(namespace)), File) if !is_slice_namespace(namespace, context.program) => {
            report_not_supported(
                namespace.r#namespace.span,
                "namespace",
                "A PHP# file has at most one namespace, written `namespace App.Tenant;` before its imports.",
                context,
            );

            None
        }
        (Node::Keyword(_) | Node::LocalIdentifier(_) | Node::Identifier(Identifier::Local(_)), _) => Some(place),
        (Node::Terminator(Terminator::Semicolon(_)), _) => Some(place),

        (
            Node::Program(_)
            | Node::Statement(
                Statement::Namespace(_)
                | Statement::Use(_)
                | Statement::Class(_)
                | Statement::Interface(_)
                | Statement::Enum(_),
            )
            | Node::Namespace(_)
            | Node::NamespaceBody(_)
            | Node::NamespaceImplicitBody(_)
            | Node::Block(_)
            | Node::Identifier(Identifier::Dotted(_))
            | Node::DottedIdentifier(_)
            | Node::Use(_)
            | Node::UseItems(UseItems::Sequence(_))
            | Node::UseItemSequence(_)
            | Node::UseItem(_),
            File,
        ) => Some(File),
        (Node::Class(class), File) => {
            report_methods_named_as_properties(
                ClassLike { name: &class.name, span: class.span(), members: &class.members },
                context,
            );

            Some(Class)
        }
        (Node::Interface(interface), File) => {
            report_methods_named_as_properties(
                ClassLike { name: &interface.name, span: interface.span(), members: &interface.members },
                context,
            );

            Some(Interface)
        }
        (Node::Enum(_), File) => Some(Enum),
        (Node::FunctionLikeParameterList(parameters), Method | Signature) => {
            report_optional_before_required(parameters, context);

            Some(place)
        }
        // `check_class` reports `protected` and `private` on a class, and PHP's own check `abstract` with `final`.
        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Abstract(_)
                | Modifier::Final(_),
            ),
            Class,
        ) => Some(Class),
        (Node::Inheritance(inheritance), Class | Interface | Enum) => {
            check_header(inheritance, place, context);

            Some(place)
        }
        (Node::Modifier(Modifier::Public(_)), Interface | Enum) => Some(place),
        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Interface) => Some(Interface),
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Interface)
            if is_slice_interface_property(property) =>
        {
            Some(FieldOrProperty)
        }
        (Node::Modifier(modifier), Signature) => {
            context.report(
                Issue::error("An interface method takes no modifier: every interface method is public.")
                    .with_annotation(Annotation::primary(modifier.span()).with_message("Remove this modifier.")),
            );

            None
        }
        (Node::Method(method), Interface) => match is_slice_signature(method) {
            Ok(()) => Some(Signature),
            Err((message, help)) => {
                context.report(
                    Issue::error(message)
                        .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                        .with_note(help),
                );

                None
            }
        },
        (
            Node::FunctionLikeReturnTypeHint(_) | Node::MethodBody(_) | Node::MethodAbstractBody(_),
            Signature,
        ) => Some(Signature),
        (Node::AttributeList(_), Class | Enum | Method | FieldOrProperty | Parameter) => Some(Attribute),
        (
            Node::Attribute(_)
            | Node::PartialArgumentList(_)
            | Node::PartialArgument(PartialArgument::Positional(_) | PartialArgument::Named(_)),
            Attribute,
        ) => Some(Attribute),
        (Node::PositionalArgument(argument), Attribute) if argument.ellipsis.is_none() => Some(Constant),
        (Node::NamedArgument(_), Attribute) => Some(Constant),

        (Node::ClassLikeMember(ClassLikeMember::Method(_)), Class | Enum) => Some(place),
        // `check_enum` reports a property in an enum, and a backing type other than `int` or `string`.
        (Node::ClassLikeMember(ClassLikeMember::Property(_)) | Node::EnumBackingTypeHint(_), Enum) => None,
        (Node::EnumCase(case), Enum) if case.item.name().value.eq_ignore_ascii_case(b"class") => {
            context.report(
                Issue::error("An enum case cannot be named `class`: PHP reserves `class` for the class name.")
                    .with_annotation(Annotation::primary(case.item.name().span).with_message("Declared here.")),
            );

            Some(Enum)
        }
        (
            Node::ClassLikeMember(ClassLikeMember::EnumCase(_))
            | Node::EnumCase(_)
            | Node::EnumCaseItem(_)
            | Node::EnumCaseUnitItem(_),
            Enum,
        ) => Some(Enum),
        (Node::EnumCaseBackedItem(_), Enum) => Some(Constant),
        (Node::ClassLikeMember(ClassLikeMember::Property(property)), Class) => {
            match is_slice_property(property, &context.version) {
                Ok(()) => Some(FieldOrProperty),
                Err(issue) => {
                    context.report(*issue);

                    None
                }
            }
        }
        (Node::ClassLikeMember(ClassLikeMember::Constant(constant)), Class | Enum) => match is_slice_constant(constant) {
            Ok(()) => Some(ClassConstant),
            Err(issue) => {
                context.report(*issue);

                None
            }
        },
        (
            Node::ClassLikeConstant(_)
            | Node::Modifier(Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)),
            ClassConstant,
        ) => Some(ClassConstant),
        (Node::Hint(hint), ClassConstant) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => {
            Some(ClassConstant)
        }
        (Node::NullableHint(_), ClassConstant) => Some(ClassConstant),
        (Node::ClassLikeConstantItem(_), ClassConstant) => Some(Constant),
        (Node::HookedProperty(property), FieldOrProperty) => {
            check_accessors(&property.modifiers, &property.hook_list, property.item.variable().span, context);

            Some(FieldOrProperty)
        }
        // `check_accessors` checked the accessors. An initial value is a method body's expression without `this`.
        (Node::PropertyHookList(_), FieldOrProperty) => None,
        // A computed property's expression runs on each read, as a method body does.
        (Node::ComputedProperty(_), FieldOrProperty) => Some(FieldOrProperty),
        (Node::PropertyHookConcreteExpressionBody(_), FieldOrProperty) => Some(Body),
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
            | Node::Modifier(
                Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_) | Modifier::Static(_),
            ),
            FieldOrProperty,
        ) => Some(FieldOrProperty),
        (Node::Hint(hint), FieldOrProperty) if is_slice_type(hint) && !matches!(hint, Hint::Void(_)) => Some(FieldOrProperty),
        (Node::GenericHint(generic), FieldOrProperty | Method | Signature | Parameter | Body) => {
            if let [key, _] = generic.arguments.as_slice()
                && !matches!(key, Hint::Integer(_) | Hint::String(_))
            {
                context.report(
                    Issue::error("A `Map`'s keys are `int` or `string`, as a PHP array's keys are.")
                        .with_annotation(Annotation::primary(key.span()).with_message("Key type written here.")),
                );

                return None;
            }

            Some(place)
        }
        // A function type's return type may be `void`, as a method's may.
        (Node::FunctionHint(_), FieldOrProperty | Method | Parameter | Body) => Some(Method),
        (Node::Method(method), Enum)
            if method.return_type_hint.is_none()
                && enclosing_class(context.program, method.span())
                    .is_some_and(|r#enum| r#enum.name.value.eq_ignore_ascii_case(method.name.value)) =>
        {
            context.report(
                Issue::error("An enum has no constructor: its cases are its only values.")
                    .with_annotation(
                        Annotation::primary(method.name.span).with_message("Declared without a return type here."),
                    )
                    .with_help("Give the method a return type, as in `public string label()`."),
            );

            None
        }
        (Node::Method(method), Class | Enum) => match is_slice_method(method, context.program) {
            Ok(()) => Some(Method),
            Err((message, help)) => {
                context.report(
                    Issue::error(message)
                        .with_annotation(Annotation::primary(method.name.span).with_message("Not supported yet."))
                        .with_note(help),
                );

                None
            }
        },

        (
            Node::Modifier(
                Modifier::Public(_)
                | Modifier::Protected(_)
                | Modifier::Private(_)
                | Modifier::Static(_)
                | Modifier::Abstract(_)
                | Modifier::Virtual(_)
                | Modifier::Override(_),
            )
            | Node::FunctionLikeReturnTypeHint(_)
            | Node::MethodBody(_)
            // PHP's own check reports a method without a body that is not `abstract`, and one with a body that is.
            | Node::MethodAbstractBody(_),
            Method,
        ) => Some(Method),
        (Node::FunctionLikeParameter(parameter), Method)
            if parameter.hooks.is_none()
                && let Some(public) = parameter.modifiers.get_public() =>
        {
            context.report(
                Issue::error(
                    "A `public` constructor parameter needs accessors: a public member is a property, as in `public int id { get; }`.",
                )
                .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                .with_note("Spec section 9 makes a `public` parameter without accessors an error, as a public field is."),
            );

            None
        }
        (Node::FunctionLikeParameter(parameter), Method | Signature | Lambda) => match is_slice_parameter(parameter) {
            Ok(()) => {
                if let Some(accessors) = &parameter.hooks
                    && parameter.modifiers.contains_visibility()
                {
                    check_accessors(&parameter.modifiers, accessors, parameter.variable.span, context);
                }

                Some(Parameter)
            }
            Err((span, message, help)) => {
                context.report(
                    Issue::error(message)
                        .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
                        .with_note(help),
                );

                None
            }
        },
        (Node::Hint(Hint::Void(_)), Body) => {
            context.report(
                Issue::error("A local cannot be `void`: `void` is only a return type.")
                    .with_annotation(Annotation::primary(node.span()).with_message("Declared `void` here.")),
            );

            None
        }
        (Node::Hint(hint), Method | Signature | Parameter | Body) if is_slice_type(hint) => Some(place),
        (Node::NullableHint(_), Method | Signature | Parameter | Body) => Some(place),
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
        (Node::PropertyHookList(_), Parameter) => None,
        (Node::FunctionLikeParameterDefaultValue(_), Parameter) => Some(Constant),
        (Node::Block(_), Method | Signature | Body) => Some(Body),
        (Node::MethodExpressionBody(_), Method | Signature) => Some(Body),
        (Node::TryCatchClause(_), Body) => Some(TryCatchClause),
        // `check_try` reports a catch type that is not a class, and `check_try_catch_clause` the variable's name.
        (Node::Hint(_) | Node::DirectVariable(_), TryCatchClause) => None,
        (Node::Block(_), TryCatchClause) => Some(Body),

        (Node::Statement(statement), Body) if !has_braces(statement) => {
            report_not_supported(
                statement.span(),
                "statement without braces",
                "PHP# writes the body of `if`, `else` and each loop as a block in braces.",
                context,
            );

            None
        }
        (
            Node::Statement(
                Statement::Block(_)
                | Statement::Expression(_)
                | Statement::Return(_)
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
            | Node::ForOfKeyValueTarget(_),
            Body,
        ) => Some(Body),

        // PHP refuses the file at compile time, so the engine would too.
        (Node::LiteralString(string), Body | Constant) if string.value.is_none() => {
            context.report(
                Issue::error("Invalid UTF-8 codepoint escape sequence.")
                    .with_annotation(Annotation::primary(string.span).with_message("Escape written here."))
                    .with_note("A `\\u{...}` escape holds hex digits for a codepoint up to `10FFFF`."),
            );

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
            | Node::ArrayElement(ArrayElement::Value(_) | ArrayElement::KeyValue(_))
            | Node::ValueArrayElement(_)
            | Node::KeyValueArrayElement(_),
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
        (Node::Expression(Expression::Conditional(conditional)), Body) => {
            check_conditional(conditional, context).then_some(Body)
        }
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
        (Node::Conditional(_), Body) => Some(Body),
        (Node::BinaryOperator(operator), Body | Constant) if is_slice_binary_operator(operator) => Some(place),
        (Node::UnaryPrefixOperator(operator), Body | Constant) if operator.is_cast() => {
            check_cast(operator, place, context)
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
            if matches!(instantiation.class, Expression::Identifier(Identifier::Local(_))) =>
        {
            if instantiation.argument_list.is_some() {
                return Some(Instantiation);
            }

            report_not_supported(
                instantiation.span(),
                "`new` without arguments",
                "PHP# creates an object with `new Class(arguments)`, parentheses included.",
                context,
            );

            None
        }
        (Node::Instantiation(_) | Node::Expression(Expression::Identifier(Identifier::Local(_))), Instantiation) => {
            Some(Instantiation)
        }
        (Node::ArgumentList(_), Instantiation) => Some(Body),
        (Node::AssignmentOperator(operator), Body) if is_slice_assignment_operator(operator) => Some(Body),
        (Node::PositionalArgument(argument), Body) if argument.ellipsis.is_none() => Some(Body),
        // `super.label()` calls the parent's method, spec section 22. `super` is no value of its own.
        (Node::MethodCall(call), Body) if matches!(call.object, Expression::Parent(_)) => Some(SuperCall),
        (Node::Expression(Expression::Parent(_)), SuperCall) => None,
        (Node::ClassLikeMemberSelector(_) | Node::ArgumentList(_), SuperCall) => enter(node, Body, context),
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
        // PHP# never has top-level functions, `global`, `compact()`, `extract()` or a member called without `this.`:
        // `check_function`, `check_global` and `check_function_call` report them.
        (Node::Statement(Statement::Function(_)), File) | (Node::Statement(Statement::Global(_)), Body) => None,
        (Node::Expression(Expression::Call(Call::Function(function_call))), Body)
            if is_checked_function_call(function_call, context) =>
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
fn is_slice_method(method: &Method, program: &Program) -> Result<(), (&'static str, &'static str)> {
    let class_name = enclosing_class(program, method.span()).map(|class| class.name.value);
    let refusal = if method.return_type_hint.is_none() && class_name != Some(method.name.value) {
        (
            "A PHP# method needs a return type: only the constructor, named after its class, has none.",
            "A method is written `public int total()`, and the constructor `public Report()`.",
        )
    } else if method.return_type_hint.is_none() && method.is_static() {
        ("A static constructor is not supported yet in PHP#.", "The main constructor of a PHP# class is not `static`.")
    } else if method.name.value.starts_with(b"__") {
        (
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        )
    } else if !method
        .modifiers
        .iter()
        .any(|modifier| matches!(modifier, Modifier::Public(_) | Modifier::Protected(_) | Modifier::Private(_)))
    {
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
/// modifier. A body is the method's default body, checked as a class method's body.
fn is_slice_signature(method: &Method) -> Result<(), (&'static str, &'static str)> {
    if method.return_type_hint.is_none() {
        Err((
            "An interface method needs a return type in PHP#.",
            "An interface method is written `float area();`, with its return type first.",
        ))
    } else if method.name.value.starts_with(b"__") {
        Err((
            "This method name is not supported yet in PHP#.",
            "PHP reserves method names that start with `__` for its magic methods.",
        ))
    } else {
        Ok(())
    }
}

/// Whether the slice has an interface property: an abstract `T p { get; }` or `T p { get; set; }`, whose accessors
/// `check_accessors` decides, or a computed `T p => expr;`, whose expression is its default body. It has no attributes,
/// no modifier, because every interface member is public, no accessor modifier or body, and no initial value, because
/// the class that implements the interface declares the storage.
fn is_slice_interface_property(property: &Property) -> bool {
    match property {
        Property::Hooked(property) => {
            property.attribute_lists.is_empty()
                && property.modifiers.is_empty()
                && matches!(property.item, PropertyItem::Abstract(_))
                && property.initial_value.is_none()
                && property.hook_list.hooks.iter().all(|accessor| {
                    accessor.modifiers.is_empty() && matches!(accessor.body, PropertyHookBody::Abstract(_))
                })
        }
        Property::Computed(property) => property.attribute_lists.is_empty() && property.modifiers.is_empty(),
        Property::Plain(_) => false,
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

/// Reports each optional parameter before the last required one, which PHP makes required with a deprecation.
fn report_optional_before_required(parameters: &FunctionLikeParameterList, context: &mut Context<'_, '_, '_>) {
    let Some(last_required) = parameters.parameters.iter().rev().find(|parameter| parameter.default_value.is_none())
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

/// Whether the slice has a parameter, with the refusal's span, message and note when it does not.
fn is_slice_parameter(parameter: &FunctionLikeParameter) -> Result<(), (Span, &'static str, &'static str)> {
    if let Some(ellipsis) = parameter.ellipsis {
        Err((ellipsis, "This variadic parameter is not supported yet in PHP#.", supported(Place::Parameter)))
    } else if let Some(ampersand) = parameter.ampersand {
        Err((
            ampersand,
            "A by-reference parameter is not supported yet in PHP#.",
            "The engine passes every PHP# argument by value.",
        ))
    } else {
        Ok(())
    }
}

/// Whether the slice has a field or a property, with the refusal when it does not. Spec section 6 makes a field
/// storage, which is `private` or `protected`, and a property the API, whose accessors `check_accessors` decides.
///
/// A static member takes only a constant initial value, which PHP stores as its default, because any other runs in
/// the constructor. A static property has `set`, because PHP has no `readonly` static property.
fn is_slice_property(property: &Property, version: &PHPVersion) -> Result<(), Box<Issue>> {
    let not_supported = |span: Span, message: &str, note: &str| {
        Issue::error(message)
            .with_annotation(Annotation::primary(span).with_message("Not supported yet."))
            .with_note(note)
    };
    let no_access_modifier = "A PHP# member without an access modifier is `private`, while PHP makes it `public`.";

    if property.modifiers().contains_static() {
        if let Property::Hooked(auto_property) = property
            && auto_property.hook_list.is_get_only()
        {
            return Err(Box::new(not_supported(
                property.first_variable().span,
                "A get-only static property is not supported yet in PHP#.",
                "A get-only property runs as `readonly`, and PHP has no `readonly` static property.",
            )));
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

    match property {
        Property::Plain(field) => {
            if let Some(public) = field.modifiers.get_public() {
                Err(Box::new(
                    Issue::error("A PHP# field cannot be `public`: a field is `private` or `protected`.")
                        .with_annotation(Annotation::primary(public.span()).with_message("Declared `public` here."))
                        .with_help("Declare a property, as in `public int views { get; set; }`, to make it public."),
                ))
            } else if !field.modifiers.contains_protected() && !field.modifiers.contains_private() {
                Err(Box::new(not_supported(
                    property.first_variable().span,
                    "A field without `private` or `protected` is not supported yet in PHP#.",
                    no_access_modifier,
                )))
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
            if !auto_property.modifiers.contains_visibility() {
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
            } else {
                Ok(())
            }
        }
        Property::Computed(computed) => {
            if !computed.modifiers.contains_visibility() {
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
    } else if !constant.modifiers.contains_visibility() {
        Err(not_supported(
            constant.first_item().name.span,
            "A constant without `public`, `protected` or `private` is not supported yet in PHP#.",
            "A PHP# member without an access modifier is `private`, while PHP makes it `public`.",
        ))
    } else {
        Ok(())
    }
}

/// Checks the accessors of an auto-property, declared in the class body or on a constructor parameter: `get;` once,
/// and an optional `set;` once, which may take an access modifier narrower than the property's, as in C#. Accessor
/// bodies, `init` and an access modifier on `get` are not supported yet.
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
        let is_auto = accessor.attribute_lists.is_empty()
            && accessor.ampersand.is_none()
            && accessor.parameter_list.is_none()
            && matches!(accessor.body, PropertyHookBody::Abstract(_))
            && accessor.modifiers.len() <= 1
            && accessor.modifiers.iter().all(|modifier| {
                matches!(modifier, Modifier::Protected(_) | Modifier::Private(_) | Modifier::Public(_))
            });

        if !is_auto || !(name == b"get" || name == b"set") || (name == b"get" && !accessor.modifiers.is_empty()) {
            report_not_supported(
                accessor.span(),
                "accessor",
                "A PHP# property's accessors are `get;` and `set;`, and `set` may take `private` or `protected`.",
                context,
            );
        } else if declared.contains(&name) {
            context.report(
                Issue::error("A PHP# property declares each accessor once.")
                    .with_annotation(Annotation::primary(accessor.name.span).with_message("Declared again here.")),
            );
        } else if accessor.modifiers.first().is_some_and(|modifier| visibility_reach(modifier) >= reach) {
            context.report(
                Issue::error("The `set` accessor of a PHP# property must be narrower than the property.")
                    .with_annotation(Annotation::primary(accessor.span()).with_message("Declared here."))
                    .with_help("Write `private set;` or `protected set;` narrower than the property, or `set;`."),
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

/// Reports each `this` in an initial value. A constant initial value is the member's default, and any other runs at
/// the start of the constructor in declaration order, before the members declared after it are set, so it cannot read
/// the object, as in C#. Returns whether it reported one.
fn report_this_in_initial_value(node: Node<'_, '_>, context: &mut Context<'_, '_, '_>) -> bool {
    let mut reported = false;
    if let Node::ConstantAccess(access) = node
        && context.names.binding(&access.name) == Some(Binding::This)
    {
        context.report(
            Issue::error(
                "An initial value cannot use `this`: it runs before the constructor body, while the object is built.",
            )
            .with_annotation(Annotation::primary(access.span()).with_message("Used here."))
            .with_help("Set the member in the constructor body instead."),
        );
        reported = true;
    }

    ensure_sufficient_stack(|| node.visit_children(|child| reported |= report_this_in_initial_value(child, context)));

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

/// Whether the slice has a ternary, reporting it when it does not. PHP#'s ternary is `c ? a : b`, and PHP's `a ?: b`
/// does not exist, as spec section 21 says. A ternary as the condition of another needs parentheses, as in PHP 8.
fn check_conditional(conditional: &Conditional, context: &mut Context<'_, '_, '_>) -> bool {
    if conditional.then.is_none() {
        context.report(
            Issue::error("PHP# has no `?:`: write `a ?? b` to replace null, or `c ? a : b` with a `bool` condition.")
                .with_annotation(
                    Annotation::primary(Span::between(conditional.question_mark, conditional.colon))
                        .with_message("Written here."),
                )
                .with_note("`?:` tests truthiness, so it replaces `\"\"` and `\"0\"` along with null."),
        );

        return false;
    }

    if let Expression::Conditional(nested @ Conditional { then: Some(_), .. }) = conditional.condition {
        context.report(
            Issue::error(
                "Unparenthesized `a ? b : c ? d : e` is not supported. Use either `(a ? b : c) ? d : e` or `a ? b : (c ? d : e)`.",
            )
            .with_annotation(Annotation::primary(nested.span()).with_message("Nested here.")),
        );

        return false;
    }

    true
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

/// Whether every body of an `if`, `else` or loop is a block in braces. An `else` may also hold the next `if`.
fn has_braces(statement: &Statement) -> bool {
    let is_block = |statement: &Statement| matches!(statement, Statement::Block(_));

    match statement {
        Statement::If(If { body: IfBody::Statement(body), .. }) => {
            is_block(body.statement)
                && body
                    .else_clause
                    .as_ref()
                    .is_none_or(|clause| is_block(clause.statement) || matches!(clause.statement, Statement::If(_)))
        }
        Statement::While(While { body: WhileBody::Statement(body), .. })
        | Statement::For(For { body: ForBody::Statement(body), .. }) => is_block(body),
        Statement::ForOf(for_of) => is_block(for_of.body),
        Statement::DoWhile(do_while) => is_block(do_while.statement),
        _ => true,
    }
}

/// Whether the slice can write an expression: a local, a parameter, a member written `object.name`, or an index of one
/// of them, as in `counts["a"]`. A member written by its bare name passes, because `check_constant_access` reports it
/// with the name to write instead.
fn is_slice_target(target: &Expression, context: &Context<'_, '_, '_>) -> bool {
    match target {
        Expression::ConstantAccess(access) => {
            matches!(context.names.binding(&access.name), Some(Binding::Local(_) | Binding::Member))
        }
        Expression::Access(Access::Property(_)) => true,
        Expression::ArrayAccess(access) => is_slice_target(access.array, context),
        _ => false,
    }
}

/// Whether the slice has a type: the built-in types of spec section 24, or a class written by its short name, or a
/// nullable type, or `List<T>` or `Map<TKey, TValue>` of spec section 12, or a function type of spec section 14.1
/// without a `void` parameter, whose inner types the walk checks next.
fn is_slice_type(hint: &Hint) -> bool {
    match hint {
        Hint::Integer(_)
        | Hint::Float(_)
        | Hint::Bool(_)
        | Hint::String(_)
        | Hint::Void(_)
        | Hint::Identifier(Identifier::Local(_))
        | Hint::Nullable(_) => true,
        Hint::Generic(generic) => {
            let arguments = generic.arguments.len();

            ((generic.name.value == b"List" && arguments == 1) || (generic.name.value == b"Map" && arguments == 2))
                && !generic.arguments.iter().any(|argument| matches!(argument, Hint::Void(_)))
        }
        Hint::Function(function) => !function.parameters.iter().any(|parameter| matches!(parameter, Hint::Void(_))),
        _ => false,
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
        | BinaryOperator::And(_)
        | BinaryOperator::Or(_)
        | BinaryOperator::NullCoalesce(_) => true,
        BinaryOperator::BitwiseAnd(_)
        | BinaryOperator::BitwiseOr(_)
        | BinaryOperator::BitwiseXor(_)
        | BinaryOperator::LeftShift(_)
        | BinaryOperator::RightShift(_)
        | BinaryOperator::AngledNotEqual(_)
        | BinaryOperator::Spaceship(_)
        | BinaryOperator::StringConcat(_)
        | BinaryOperator::Instanceof(_)
        | BinaryOperator::LowAnd(_)
        | BinaryOperator::LowOr(_)
        | BinaryOperator::LowXor(_) => false,
    }
}

/// Whether the slice has a prefix operator at a place. A parameter default has no `++` or `--`. Every operator is
/// named, so a new one does not compile until it is decided.
fn is_slice_prefix_operator(operator: &UnaryPrefixOperator, place: Place) -> bool {
    match operator {
        UnaryPrefixOperator::Negation(_) | UnaryPrefixOperator::Plus(_) | UnaryPrefixOperator::Not(_) => true,
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
        | UnaryPrefixOperator::VoidCast(..)
        | UnaryPrefixOperator::BitwiseNot(_) => false,
    }
}

/// Decides a cast at a place, reporting it when the slice does not have it. Spec section 24 keeps `(int)`, `(float)`
/// and `(string)`, which the slice has in a method body, and removes PHP's other casts and its cast aliases. Every
/// cast is named, so a new one does not compile until it is decided.
fn check_cast(operator: &UnaryPrefixOperator, place: Place, context: &mut Context<'_, '_, '_>) -> Option<Place> {
    let compare = "compare the value instead, as in `count > 0` or `flag == \"1\"`.";
    let numbers_only = "`(int)`, `(float)` and `(string)` convert between numbers only.";
    let (cast, instead) = match operator {
        UnaryPrefixOperator::IntCast(..) | UnaryPrefixOperator::FloatCast(..) | UnaryPrefixOperator::StringCast(..)
            if place == Place::Body =>
        {
            return Some(Place::Body);
        }
        UnaryPrefixOperator::IntCast(..)
        | UnaryPrefixOperator::FloatCast(..)
        | UnaryPrefixOperator::StringCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::VoidCast(..) => {
            report_not_supported(operator.span(), "operator", supported(place), context);

            return None;
        }
        UnaryPrefixOperator::BoolCast(..) => ("(bool)", compare),
        UnaryPrefixOperator::BooleanCast(..) => ("(boolean)", compare),
        UnaryPrefixOperator::ArrayCast(..) => ("(array)", numbers_only),
        UnaryPrefixOperator::ObjectCast(..) => ("(object)", numbers_only),
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

    context.report(
        Issue::error(format!("PHP# has no `{cast}`: {instead}"))
            .with_annotation(Annotation::primary(operator.span()).with_message("Written here."))
            .with_note("Spec section 24 keeps `(int)`, `(float)` and `(string)` between numbers, and removes PHP's other casts and its cast aliases."),
    );

    None
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
        | AssignmentOperator::Exponentiation(_)
        | AssignmentOperator::Coalesce(_) => true,
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_) => false,
    }
}

fn report_unsupported(node: Node<'_, '_>, place: Place, context: &mut Context<'_, '_, '_>) {
    let construct = match node {
        Node::Statement(_) => "statement",
        Node::Expression(_) | Node::ConstantAccess(_) => "expression",
        Node::BinaryOperator(_) | Node::UnaryPrefixOperator(_) | Node::AssignmentOperator(_) => "operator",
        Node::Hint(_) | Node::NullableHint(_) => "type",
        Node::Modifier(_) => "modifier",
        Node::IfStatementBodyElseIfClause(_) => {
            return report_not_supported(node.span(), "`elseif`", "PHP# writes `else if`.", context);
        }
        Node::Extends(_) => "`extends` clause",
        Node::Implements(_) => "`implements` clause",
        Node::ClassLikeMember(_) => "class member",
        Node::ClassLikeMemberSelector(_) => "member name",
        Node::PositionalArgument(_) => "spread argument",
        _ => "construct",
    };

    report_not_supported(node.span(), construct, supported(place), context);
}

/// What the slice has at a place, as the note of a refusal there.
const fn supported(place: Place) -> &'static str {
    match place {
        Place::File => "At file level, PHP# supports `namespace`, `import`, `class`, `interface` and `enum`.",
        Place::Class => {
            "A PHP# class has attributes, an optional `public`, `abstract` or `final`, a name, an optional `: Base, Interface` header, constants, fields, properties and methods, with no other modifiers, `extends` or `implements`."
        }
        Place::Interface => {
            "A PHP# interface has an optional `public`, a name, an optional `: Interface` header, methods, properties `T p { get; }` or `T p { get; set; }`, and computed properties `T p => expr;`, each without a modifier, and no attributes, other modifiers, `extends`, constants or fields."
        }
        Place::Signature => {
            "A PHP# interface method has no modifier, parameters, a return type of `int`, `float`, `bool`, `string`, `void`, a class, `List<T>` or `Map<TKey, TValue>`, and no body or a default body."
        }
        Place::Enum => {
            "A PHP# enum has attributes, an optional `public`, a name, an optional `: string, Interface` header whose `int` or `string` comes first, constants, cases and methods, with no other modifiers or `implements`."
        }
        Place::FieldOrProperty => {
            "A PHP# field is `private` or `protected`, and a property has the accessors `get;` and an optional `set;`. Both may be `static`, and have a type of `int`, `float`, `bool`, `string`, a class, `List<T>` or `Map<TKey, TValue>`, a name, and an optional initial value."
        }
        Place::ClassConstant => {
            "A PHP# constant has `public`, `protected` or `private`, an optional type of `int`, `float`, `bool`, `string` or a class, nullable as in `int?` or not, one name, and a constant value."
        }
        Place::Method => {
            "A PHP# method takes `public`, `protected`, `private`, `static`, `abstract`, `virtual` and `override`, parameters, and a return type of `int`, `float`, `bool`, `string`, `void`, a class, `List<T>` or `Map<TKey, TValue>`, each but `void` nullable as in `int?`."
        }
        Place::Parameter => {
            "A PHP# parameter has a type of `int`, `float`, `bool`, `string`, a class, `List<T>` or `Map<TKey, TValue>`, nullable as in `int?` or not, a name, and an optional default."
        }
        Place::Lambda => {
            "A PHP# lambda is a bare arrow after one name or parenthesized parameters, each with an optional type, and its body is an expression or a block, as in `(a, b) => a + b`."
        }
        Place::Body | Place::Instantiation | Place::FunctionCall | Place::TryCatchClause | Place::SuperCall => {
            "In a method body, PHP# supports blocks, expression statements, `return`, `let` and `const` and typed locals, `if` with `else if` and `else`, `while`, `do … while`, `for`, `for … of`, `break` and `continue` without a level, and `try` with `catch` and `finally`, with literals, list and map literals, index reads, templates, parentheses, bare names, assignment, arithmetic, comparison and logical operators, `??`, the ternary `c ? a : b`, `(int)`, `(float)` and `(string)`, `++` and `--`, method calls and property reads written with `.` or `?.`, `new Class(...)`, calls of global functions, `throw`, `typeof(Class)` and `super.method(...)`."
        }
        Place::Attribute => {
            "A PHP# attribute is a class name with optional positional and named arguments, as in `[Field(\"Name\", searchable: true)]`."
        }
        Place::Constant => {
            "A parameter default, an attribute argument, a constant's value or an enum case's value is a literal, a constant, `typeof(Class)`, a list or map literal, or arithmetic, comparison, logical and `??` operators on them."
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

#[inline]
pub fn check_local_declaration(local_declaration: &LocalDeclaration, context: &mut Context<'_, '_, '_>) {
    check_local_name(local_declaration.name.value, local_declaration.name.span, "local", context);
}

#[inline]
pub fn check_for_of(for_of: &ForOf, context: &mut Context<'_, '_, '_>) {
    for name in for_of.target.names() {
        check_local_name(name.value, name.span, "loop variable", context);
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

/// Checks the name of a PHP# class or enum against the names the engine reserves, beyond the keywords the PHP checks
/// reject.
#[inline]
pub fn check_class_name(class_name: &LocalIdentifier, context: &mut Context<'_, '_, '_>) {
    let is_keyword = RESERVED_KEYWORDS
        .iter()
        .chain(&SOFT_RESERVED_KEYWORDS_MINUS_SYMBOL_ALLOWED)
        .any(|keyword| keyword.eq_ignore_ascii_case(class_name.value));

    if is_reserved_class_name(class_name.value) && !is_keyword {
        let name = BytesDisplay(class_name.value);

        context.report(
            Issue::error(format!("Cannot use `{name}` as a class name: it is reserved."))
                .with_annotation(Annotation::primary(class_name.span).with_message("Class declared here."))
                .with_note("PHP reserves this name for a type."),
        );
    }
}

/// Checks the classes and imports of a PHP# file against each other, as the engine does when it compiles the file.
#[inline]
pub fn check_declarations(program: &Program, context: &mut Context<'_, '_, '_>) {
    let (classes, imports) = declarations(program);
    let namespace = first_namespace(program).and_then(|namespace| namespace.name.as_ref()).map(php_name);

    for (index, import) in imports.iter().enumerate() {
        let short_name = import.name.last_segment();
        let full_name = BytesDisplay(import.name.value());

        if is_reserved_class_name(short_name) {
            let short_name = BytesDisplay(short_name);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{short_name}`: PHP reserves `{short_name}` for a type."
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported here."))
                .with_help("Import a class with another name."),
            );
        }

        if let Some(earlier) =
            imports[..index].iter().find(|earlier| earlier.name.last_segment().eq_ignore_ascii_case(short_name))
        {
            let earlier_full_name = BytesDisplay(earlier.name.value());

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{}`: `{earlier_full_name}` is already imported as `{}`.",
                    BytesDisplay(short_name),
                    BytesDisplay(earlier.name.last_segment()),
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported again here."))
                .with_annotation(Annotation::secondary(earlier.name.span()).with_message("First imported here."))
                .with_note("Class names are case-insensitive."),
            );
        }

        // Importing the class the file declares names that class, which the engine accepts.
        if let Some(class) = classes.iter().find(|class| class.name.value.eq_ignore_ascii_case(short_name))
            && !names_class(&php_name(&import.name), namespace.as_deref(), class.name.value)
        {
            let short_name = BytesDisplay(short_name);
            let class_name = BytesDisplay(class.name.value);

            context.report(
                Issue::error(format!(
                    "Cannot import `{full_name}` as `{short_name}`: this file declares a class named `{class_name}`."
                ))
                .with_annotation(Annotation::primary(import.name.span()).with_message("Imported here."))
                .with_annotation(Annotation::secondary(class.name.span).with_message("Class declared here.")),
            );
        }
    }
}

#[inline]
pub fn check_constant_access(constant_access: &ConstantAccess, context: &mut Context<'_, '_, '_>) {
    if context.names.binding(&constant_access.name) == Some(Binding::Member) {
        report_bare_member(constant_access.span(), constant_access.name.value(), context);
    }
}

#[inline]
pub fn check_function_call(function_call: &FunctionCall, context: &mut Context<'_, '_, '_>) {
    let Expression::Identifier(identifier) = function_call.function else {
        return;
    };

    if context.names.binding(identifier) == Some(Binding::Member) {
        report_bare_member(identifier.span(), identifier.value(), context);

        return;
    }

    let function = identifier.last_segment();
    if is_compact_or_extract(function) {
        let function = BytesDisplay(function);

        context.report(
            Issue::error(format!("`{function}()` is not part of PHP#."))
                .with_annotation(Annotation::primary(function_call.span()).with_message("Called here."))
                .with_help("PHP# variables are never created or read by name at runtime."),
        );
    }
}

/// Returns true when `check_function_call` reports the call: a bare member, `compact()` or `extract()`.
fn is_checked_function_call(function_call: &FunctionCall, context: &Context<'_, '_, '_>) -> bool {
    let Expression::Identifier(identifier) = function_call.function else {
        return false;
    };

    context.names.binding(identifier) == Some(Binding::Member) || is_compact_or_extract(identifier.last_segment())
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
/// A chain rooted at a class reaches its constants, enum cases and static members. One file cannot tell a full name,
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

    if let Expression::TypeOf(type_of) = root {
        context.report(
            Issue::error(format!(
                "Reading a member of `typeof({})` is not supported yet.",
                BytesDisplay(type_of.class.value())
            ))
            .with_annotation(
                Annotation::primary(Span::between(object.span(), member.span())).with_message("Read here."),
            )
            .with_note("The engine runs `typeof(X)` as the class name `X::class`, which has no members yet."),
        );
    }
}

/// A class, an enum or an interface a PHP# file declares. The slice's class rules treat them alike.
#[derive(Clone, Copy)]
struct ClassLike<'ast, 'arena> {
    name: &'ast LocalIdentifier<'arena>,
    span: Span,
    members: &'ast Sequence<'arena, ClassLikeMember<'arena>>,
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
        Statement::Class(class) => {
            classes.push(ClassLike { name: &class.name, span: class.span(), members: &class.members });
        }
        Statement::Enum(r#enum) => {
            classes.push(ClassLike { name: &r#enum.name, span: r#enum.span(), members: &r#enum.members });
        }
        Statement::Use(Use { items: UseItems::Sequence(sequence), .. }) => imports.extend(sequence.items.iter()),
        _ => {}
    }
}

/// Checks the name of a PHP# local or parameter, which runs as the PHP variable of the same name.
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
    }
}

fn is_reserved_class_name(name: &[u8]) -> bool {
    RESERVED_CLASS_NAMES.iter().any(|reserved| reserved.eq_ignore_ascii_case(name))
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

/// Checks the object of `object?.member`. A class is never null, so `?.` after a class is an error that names `.`.
fn check_null_safe_object(
    access: Span,
    object: &Expression,
    member: &ClassLikeMemberSelector,
    context: &mut Context<'_, '_, '_>,
) -> Option<Place> {
    if let Expression::ConstantAccess(class) = object
        && context.names.binding(&class.name) == Some(Binding::Class)
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

/// The class of the static method whose body holds `span`, which has no `this`.
fn enclosing_static_method_class<'ast, 'arena>(
    program: &'ast Program<'arena>,
    span: Span,
) -> Option<ClassLike<'ast, 'arena>> {
    let class = enclosing_class(program, span)?;

    class
        .members
        .iter()
        .any(|member| {
            matches!(member, ClassLikeMember::Method(method)
                if method.span().contains(&span.start)
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Static(_))))
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

/// The class or enum of a PHP# file whose body holds `span`.
fn enclosing_class<'ast, 'arena>(program: &'ast Program<'arena>, span: Span) -> Option<ClassLike<'ast, 'arena>> {
    declarations(program).0.into_iter().find(|class| class.span.contains(&span.start))
}
