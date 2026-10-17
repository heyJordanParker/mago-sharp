use std::collections::HashMap;
use std::collections::HashSet;

use mago_allocator::LocalArena;
use mago_codex::defaults_trait_name;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::BindingValueMetadata;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::metadata::module::service_id;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::scanner::get_union_from_hint;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::iterable::TIterable;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::atomic::reference::TReference;
use mago_codex::ttype::atomic::scalar::TScalar;
use mago_codex::ttype::resolution::TypeResolutionContext;
use mago_codex::ttype::union::TUnion;
use mago_names::INLINED_COLLECTION_METHODS;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::LocalKind;
use mago_names::binding::SUPERGLOBALS;
use mago_names::binding::php_local_name;
use mago_names::binding::php_method_name;
use mago_names::binding::php_operator_name;
use mago_names::scope::php_name;
use mago_php_version::PHPVersion;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Array;
use mago_syntax::cst::ArrayAccess;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::BindingValue;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::ComputedProperty;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Construct;
use mago_syntax::cst::ConstructorInitializer;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Event;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Extension;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOf;
use mago_syntax::cst::ForOfTarget;
use mago_syntax::cst::ForOfVariable;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Hint;
use mago_syntax::cst::HookedProperty;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::InterpolatedString;
use mago_syntax::cst::Leave;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralStringPart;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::LocalTarget;
use mago_syntax::cst::MatchArm;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::NamedArgument;
use mago_syntax::cst::NamedConstructor;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::ObjectInitializer;
use mago_syntax::cst::Operator;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PartialArgumentList;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Propagate;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyBehaviorList;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookConcreteExpressionBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::Return;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Struct;
use mago_syntax::cst::Try;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::TypeOf;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
use mago_syntax::cst::WhileBody;
use mago_syntax::cst::With;
use mago_syntax::cst::Yield;
use mago_syntax::cst::YieldValue;
use mago_syntax::cst::built_in_generic_arity;
use mago_syntax::utils::block_has_yield;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::utils::pattern::php_shape;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_syntax_core::utils::parse_literal_integer_as_float;
use mago_word::Word;
use mago_word::word;
use xxhash_rust::xxh3::xxh3_64;

use crate::Unit;
use crate::kind::SHARP_T_FILE;
use crate::lower::checked::CheckedProgram;
use crate::sharp_kind;
use crate::sharp_kind::SHARP_AST_AND;
use crate::sharp_kind::SHARP_AST_ARG_LIST;
use crate::sharp_kind::SHARP_AST_ARRAY;
use crate::sharp_kind::SHARP_AST_ARRAY_ELEM;
use crate::sharp_kind::SHARP_AST_ARROW_FUNC;
use crate::sharp_kind::SHARP_AST_ASSIGN;
use crate::sharp_kind::SHARP_AST_ASSIGN_COALESCE;
use crate::sharp_kind::SHARP_AST_ASSIGN_OP;
use crate::sharp_kind::SHARP_AST_ASSIGN_REF;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE_GROUP;
use crate::sharp_kind::SHARP_AST_ATTRIBUTE_LIST;
use crate::sharp_kind::SHARP_AST_BINARY_OP;
use crate::sharp_kind::SHARP_AST_BREAK;
use crate::sharp_kind::SHARP_AST_CALL;
use crate::sharp_kind::SHARP_AST_CALLABLE_CONVERT;
use crate::sharp_kind::SHARP_AST_CAST;
use crate::sharp_kind::SHARP_AST_CATCH;
use crate::sharp_kind::SHARP_AST_CATCH_LIST;
use crate::sharp_kind::SHARP_AST_CLASS;
use crate::sharp_kind::SHARP_AST_CLASS_CONST;
use crate::sharp_kind::SHARP_AST_CLASS_CONST_DECL;
use crate::sharp_kind::SHARP_AST_CLASS_CONST_GROUP;
use crate::sharp_kind::SHARP_AST_CLASS_NAME;
use crate::sharp_kind::SHARP_AST_CLOSURE;
use crate::sharp_kind::SHARP_AST_CLOSURE_USES;
use crate::sharp_kind::SHARP_AST_COALESCE;
use crate::sharp_kind::SHARP_AST_CONDITIONAL;
use crate::sharp_kind::SHARP_AST_CONST;
use crate::sharp_kind::SHARP_AST_CONST_DECL;
use crate::sharp_kind::SHARP_AST_CONST_ELEM;
use crate::sharp_kind::SHARP_AST_CONST_ENUM_INIT;
use crate::sharp_kind::SHARP_AST_CONTINUE;
use crate::sharp_kind::SHARP_AST_DECLARE;
use crate::sharp_kind::SHARP_AST_DIM;
use crate::sharp_kind::SHARP_AST_DO_WHILE;
use crate::sharp_kind::SHARP_AST_ENCAPS_LIST;
use crate::sharp_kind::SHARP_AST_ENUM_CASE;
use crate::sharp_kind::SHARP_AST_EXPR_LIST;
use crate::sharp_kind::SHARP_AST_FOR;
use crate::sharp_kind::SHARP_AST_FOREACH;
use crate::sharp_kind::SHARP_AST_GREATER;
use crate::sharp_kind::SHARP_AST_GREATER_EQUAL;
use crate::sharp_kind::SHARP_AST_IF;
use crate::sharp_kind::SHARP_AST_IF_ELEM;
use crate::sharp_kind::SHARP_AST_INSTANCEOF;
use crate::sharp_kind::SHARP_AST_MAGIC_CONST;
use crate::sharp_kind::SHARP_AST_MATCH;
use crate::sharp_kind::SHARP_AST_MATCH_ARM;
use crate::sharp_kind::SHARP_AST_MATCH_ARM_LIST;
use crate::sharp_kind::SHARP_AST_METHOD;
use crate::sharp_kind::SHARP_AST_METHOD_CALL;
use crate::sharp_kind::SHARP_AST_METHOD_REFERENCE;
use crate::sharp_kind::SHARP_AST_NAME_LIST;
use crate::sharp_kind::SHARP_AST_NAMED_ARG;
use crate::sharp_kind::SHARP_AST_NAMESPACE;
use crate::sharp_kind::SHARP_AST_NEW;
use crate::sharp_kind::SHARP_AST_NULLSAFE_METHOD_CALL;
use crate::sharp_kind::SHARP_AST_NULLSAFE_PROP;
use crate::sharp_kind::SHARP_AST_OR;
use crate::sharp_kind::SHARP_AST_PARAM;
use crate::sharp_kind::SHARP_AST_PARAM_LIST;
use crate::sharp_kind::SHARP_AST_POST_DEC;
use crate::sharp_kind::SHARP_AST_POST_INC;
use crate::sharp_kind::SHARP_AST_PRE_DEC;
use crate::sharp_kind::SHARP_AST_PRE_INC;
use crate::sharp_kind::SHARP_AST_PROP;
use crate::sharp_kind::SHARP_AST_PROP_DECL;
use crate::sharp_kind::SHARP_AST_PROP_ELEM;
use crate::sharp_kind::SHARP_AST_PROP_GROUP;
use crate::sharp_kind::SHARP_AST_PROPERTY_HOOK;
use crate::sharp_kind::SHARP_AST_PROPERTY_HOOK_SHORT_BODY;
use crate::sharp_kind::SHARP_AST_RETURN;
use crate::sharp_kind::SHARP_AST_SHARP_CLASS_METADATA;
use crate::sharp_kind::SHARP_AST_SHARP_IS_TYPE;
use crate::sharp_kind::SHARP_AST_SHARP_TYPE_ARGS;
use crate::sharp_kind::SHARP_AST_SHARP_TYPE_OF;
use crate::sharp_kind::SHARP_AST_SILENCE;
use crate::sharp_kind::SHARP_AST_STATIC_CALL;
use crate::sharp_kind::SHARP_AST_STATIC_PROP;
use crate::sharp_kind::SHARP_AST_STMT_LIST;
use crate::sharp_kind::SHARP_AST_THROW;
use crate::sharp_kind::SHARP_AST_TRAIT_ADAPTATIONS;
use crate::sharp_kind::SHARP_AST_TRAIT_ALIAS;
use crate::sharp_kind::SHARP_AST_TRAIT_PRECEDENCE;
use crate::sharp_kind::SHARP_AST_TRY;
use crate::sharp_kind::SHARP_AST_TYPE;
use crate::sharp_kind::SHARP_AST_TYPE_INTERSECTION;
use crate::sharp_kind::SHARP_AST_TYPE_UNION;
use crate::sharp_kind::SHARP_AST_UNARY_MINUS;
use crate::sharp_kind::SHARP_AST_UNARY_OP;
use crate::sharp_kind::SHARP_AST_UNARY_PLUS;
use crate::sharp_kind::SHARP_AST_UNPACK;
use crate::sharp_kind::SHARP_AST_UNSET;
use crate::sharp_kind::SHARP_AST_USE_TRAIT;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_kind::SHARP_AST_VERIFY_TYPE_ARGS;
use crate::sharp_kind::SHARP_AST_WHILE;
use crate::sharp_kind::SHARP_AST_YIELD;
use crate::sharp_kind::SHARP_AST_ZVAL;
use crate::sharp_node;
use crate::sharp_str;
use crate::sharp_value;
use crate::store_text;
use crate::unit::Read;

pub(crate) mod checked;
pub(crate) mod inline;
pub(crate) mod types;

use types::Declaration;
use types::DeclarationKind;
use types::FunctionType;
use types::PlainParameter;
use types::Types;
use types::agreed_kind;
use types::class_value_classes;
use types::receiver_classes;
use types::receiver_intersections;

/// The values php-src gives the attrs the lowering emits, from `zend_compile.h` and `zend_vm_opcodes.h`. The tokens
/// it emits are generated in `kind.rs`.
const ZEND_NAME_FQ: u32 = 0;
const ZEND_NAME_NOT_FQ: u32 = 1;
const ZEND_ACC_PUBLIC: u32 = 1 << 0;
const ZEND_ACC_PROTECTED: u32 = 1 << 1;
const ZEND_ACC_PRIVATE: u32 = 1 << 2;
const ZEND_ACC_STATIC: u32 = 1 << 4;
const ZEND_ACC_FINAL: u32 = 1 << 5;
const ZEND_ACC_ABSTRACT: u32 = 1 << 6;
const ZEND_ACC_EXPLICIT_ABSTRACT_CLASS: u32 = 1 << 6;
const ZEND_ACC_INTERFACE: u32 = 1 << 0;
const ZEND_ACC_TRAIT: u32 = 1 << 1;
const ZEND_ACC_READONLY: u32 = 1 << 7;
const ZEND_ACC_PROTECTED_SET: u32 = 1 << 11;
const ZEND_ACC_PRIVATE_SET: u32 = 1 << 12;
const ZEND_ACC_FIELD: u32 = 1 << 15;
const ZEND_ACC_GENERATOR: u32 = 1 << 24;
const ZEND_ACC_ENUM: u32 = 1 << 28;
const ZEND_ACC_SHARP_HIDDEN: u32 = 1 << 13;
const ZEND_SHARP_TYPE_OF_LIST: u32 = 1;
const ZEND_SHARP_NEW_CLASS_VALUE: u32 = 1;
const ZEND_TYPE_NULLABLE: u32 = 1 << 8;
const IS_STATIC: u32 = 15;
const ZEND_PARAM_VARIADIC: u32 = 1 << 4;
const ZEND_PARAM_WIDENED: u32 = 1 << 6;
const ZEND_PARENTHESIZED_CONDITIONAL: u32 = 1;
const ZEND_BIND_REF: u32 = 1;
/// The hidden local that holds a generic method's own type arguments, which a closure in the method captures.
const TYPE_ARGUMENTS: &[u8] = b"\0<sharp>\0types";
/// The hidden static that keeps each type argument's own static fields of a generic class.
const STATICS: &[u8] = b"\0<sharp>\0statics";
/// The hidden static method that runs the initial values of one type argument's entry, which the engine calls when it
/// adds the entry.
const STATICS_INIT: &[u8] = b"#init";
const IS_LONG: u32 = 4;
const IS_DOUBLE: u32 = 5;
const IS_STRING: u32 = 6;
const IS_ARRAY: u32 = 7;
const IS_OBJECT: u32 = 8;
const ZEND_ARRAY_SYNTAX_SHORT: u32 = 3;
const ZEND_ADD: u32 = 1;
const ZEND_SUB: u32 = 2;
const ZEND_MUL: u32 = 3;
const ZEND_DIV: u32 = 4;
const ZEND_MOD: u32 = 5;
const ZEND_SL: u32 = 6;
const ZEND_SR: u32 = 7;
const ZEND_CONCAT: u32 = 8;
const ZEND_BW_OR: u32 = 9;
const ZEND_BW_AND: u32 = 10;
const ZEND_BW_XOR: u32 = 11;
const ZEND_POW: u32 = 12;
const ZEND_BW_NOT: u32 = 13;
const ZEND_BOOL_NOT: u32 = 14;
const ZEND_IS_IDENTICAL: u32 = 16;
const ZEND_IS_NOT_IDENTICAL: u32 = 17;
const ZEND_IS_EQUAL: u32 = 18;
const ZEND_IS_NOT_EQUAL: u32 = 19;
const ZEND_IS_SMALLER: u32 = 20;
const ZEND_IS_SMALLER_OR_EQUAL: u32 = 21;
const ZEND_SPACESHIP: u32 = 170;

/// A null child.
const NULL: u32 = u32::MAX;

/// What the operands of an operator are, where spec sections 19 and 24 make the operator differ from PHP's.
#[derive(Clone, Copy)]
enum Operands {
    Strings,
    Ints,
    Other,
}

/// Lowers a program the checker accepted into the tree php-src builds for the equivalent PHP.
#[must_use]
pub fn lower(checked: &CheckedProgram<'_>) -> Unit {
    let lines = Lines::new(&checked.file().contents);

    Lowering::new(&lines, checked.names(), checked.types()).program(checked)
}

/// The offset each line starts at, counted as the Zend scanner counts: `\n`, `\r\n` and a lone `\r` each end a line.
struct Lines(Vec<u32>);

impl Lines {
    fn new(source: &[u8]) -> Self {
        let mut starts = vec![0];
        for (index, &byte) in source.iter().enumerate() {
            if byte == b'\n' || (byte == b'\r' && source.get(index + 1) != Some(&b'\n')) {
                starts.push(index as u32 + 1);
            }
        }

        Self(starts)
    }

    /// The 1-based line `offset` is on.
    fn line(&self, offset: u32) -> u32 {
        self.0.partition_point(|&start| start <= offset) as u32
    }

    /// The file's last line, the one after its last line ending.
    fn last(&self) -> u32 {
        self.0.len() as u32
    }

    /// The 1-based line and byte column `offset` is at.
    fn line_and_column(&self, offset: u32) -> (u32, u32) {
        let line = self.line(offset);

        (line, offset - self.0[line as usize - 1] + 1)
    }
}

/// A value the PHP of a `Set` or a collection method reads more than once, and the hidden variable that holds it, if
/// any.
struct Reread<'value, 'ast> {
    expression: &'value Expression<'ast>,
    hidden: Option<Vec<u8>>,
    /// Whether a read assigned the hidden variable.
    assigned: bool,
}

impl<'value, 'ast> Reread<'value, 'ast> {
    /// `expression`, lowered again at each read.
    fn once(expression: &'value Expression<'ast>) -> Self {
        Self { expression, hidden: None, assigned: false }
    }
}

/// The receiver of a collection method call that runs as the PHP on its array.
enum CollectionReceiver<'receiver, 'ast> {
    /// A local, `field` or a static property, which lowering again reads and writes the same collection.
    Place(&'receiver Expression<'ast>),
    /// A property of an object, which the method reads again.
    Property(Reread<'receiver, 'ast>, &'receiver ClassLikeMemberSelector<'ast>),
    /// An index of a `List`, a place by spec section 12, which a change reaches only through a `Sharp\Collection`
    /// method: the engine reads the element of a method call's receiver index through the list's collection.
    Index(Box<CollectionReceiver<'receiver, 'ast>>, Reread<'receiver, 'ast>),
    /// Any other value, which lives nowhere, so a change changes a copy.
    Value(Reread<'receiver, 'ast>),
}

/// A write to the element of a `List` at an index.
enum ListWrite<'write, 'ast> {
    /// `list[i] = v` or `list[i] op= v`.
    Assignment(&'write Assignment<'ast>),
    /// `++list[i]`, `--list[i]`, `list[i]++` or `list[i]--`.
    Step { decrement: bool, postfix: bool },
}

/// What an access does with the place it reaches: reads its value, alone or before it writes it, as a compound
/// assignment, `++` and `--` do, or writes it alone, as `=` and `??=` do.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlaceUse {
    Read,
    Write,
}

/// What a `List`'s search by `==` gives: whether an element equals the value, the index of the first one that does, or
/// whether `remove` removed it.
#[derive(Clone, Copy)]
enum Search {
    Contains,
    IndexOf,
    Remove,
}

/// One side of an `==` the lowering writes: an expression of the source, the hidden variable `name` holding a value
/// of `r#type`, read on the line of `span`, as the element and the value a collection search compares, or the
/// `property` of type `r#type` of the hidden variable `object`, as a struct's `==` compares each of its properties.
enum Side<'side, 'ast> {
    Expression(&'side Expression<'ast>),
    Hidden {
        span: Span,
        name: Vec<u8>,
        r#type: &'side TUnion,
    },
    Property {
        span: Span,
        object: &'static [u8],
        property: &'side [u8],
        r#type: &'side TUnion,
    },
    /// The element of the hidden collection `array` under the hidden key `key`.
    Index {
        span: Span,
        array: Vec<u8>,
        key: Vec<u8>,
        r#type: &'side TUnion,
    },
}

/// A place a struct's `parse` reads a value from or writes one to: a hidden variable, or an element of one.
#[derive(Clone)]
struct Place {
    variable: Vec<u8>,
    index: Index,
}

#[derive(Clone)]
enum Index {
    None,
    Text(Vec<u8>),
    Variable(Vec<u8>),
    /// `$variable[]`, a new element at the end.
    Append,
}

impl Place {
    fn variable(name: &[u8]) -> Self {
        Self { variable: name.to_vec(), index: Index::None }
    }

    fn element(array: &[u8], index: Index) -> Self {
        Self { variable: array.to_vec(), index }
    }
}

/// How a `Map` takes back a key plain PHP's array holds, spec section 12.
#[derive(Clone, Copy)]
enum KeyCrossing<'class> {
    /// The key as PHP holds it.
    AsHeld,
    /// An `int` key back as its `string`, for a key type that holds `string` and no `int`.
    Text,
    /// A backing value back as its case of this backed enum.
    Case(&'class [u8]),
}

/// A part of the path a problem of `parse` is recorded under: text, or the key a hidden variable holds.
#[derive(Clone)]
enum PathPart {
    Text(Vec<u8>),
    Variable(Vec<u8>),
}

/// Whether a missing key leaves a field of `r#type` null, as spec section 10 reads a `T?` or `Any?` parameter.
fn takes_null(r#type: &TUnion) -> bool {
    r#type.is_nullable()
        || r#type.types.iter().any(|atomic| matches!(atomic, TAtomic::Mixed(mixed) if !mixed.is_non_null()))
}

/// Lowers one checked file. Every node is pushed after its children, and each node's children are contiguous.
struct Lowering<'lowering, 'arena> {
    lines: &'lowering Lines,
    names: &'lowering ResolvedNames<'arena>,
    types: &'lowering Types<'lowering>,
    /// The full name of the class-like being lowered, as PHP writes it.
    class: &'arena [u8],
    /// The full dotted name of the method or property being lowered, which `Position.current()` gives as its `function`.
    function: Vec<u8>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<u8>,
    /// How many hidden variables the pattern forms and null-safe chains being lowered hold.
    temporaries: u32,
    /// The receivers of the null-safe chains being lowered that their hidden variable holds, by span, with its name.
    null_safe_receivers: Vec<(Span, Vec<u8>)>,
    /// The null-safe links whose chain's conditional already tests the receiver, so each is its plain form.
    tested_links: Vec<Span>,
    /// The declaration offsets of the locals a lambda captures by reference: those code writes.
    by_reference: HashSet<u32>,
    /// How many loop bodies hold the statement being lowered, inside the innermost method or lambda.
    loop_depth: u32,
    captures_type_arguments: bool,
    /// How many type parameters the generic class has whose static member is being lowered. Its static method has no
    /// `this` to read the class's type arguments from, so it takes them as its leading own ones.
    static_type_parameters: Option<usize>,
    /// The name of the property whose accessor body is being lowered, which `field` reads and writes.
    property: Vec<u8>,
    /// Each inline form the lowering copied, with its fingerprint, as often as it copied it.
    inlined: Vec<Read>,
    /// Each function type the lowering spelled for a method or lambda of the file, with its text's fingerprint.
    function_types: Vec<Read>,
    /// The declared return type of the method being lowered, which a value it returns crosses into. A lambda declares
    /// none.
    returns: Option<TUnion>,
    /// The declared type of each typed local lowered so far, by the offset of its declaration, which a reference
    /// parameter's local crosses back into.
    local_types: HashMap<u32, TUnion>,
    /// Whether the constructor being lowered promotes a `Map` or a `Set`, so it declares each property it promotes
    /// beside it, in written order, and sets each one in its first statements.
    promoted_beside: bool,
    /// The type PHP writes for each type parameter, by the offset of its declared name: its bound, or `mixed` without
    /// one.
    type_parameters: HashMap<u32, PhpType<'arena>>,
    /// Whether the file declares a test module, so each of its classes starts and ends its tests.
    tested: bool,
}

impl<'lowering, 'arena> Lowering<'lowering, 'arena> {
    fn new(
        lines: &'lowering Lines,
        names: &'lowering ResolvedNames<'arena>,
        types: &'lowering Types<'lowering>,
    ) -> Self {
        Self {
            lines,
            names,
            types,
            class: b"",
            function: Vec::new(),
            nodes: Vec::new(),
            children: Vec::new(),
            texts: Vec::new(),
            temporaries: 0,
            null_safe_receivers: Vec::new(),
            tested_links: Vec::new(),
            by_reference: HashSet::default(),
            loop_depth: 0,
            captures_type_arguments: false,
            static_type_parameters: None,
            property: Vec::new(),
            inlined: Vec::new(),
            function_types: Vec::new(),
            returns: None,
            local_types: HashMap::default(),
            promoted_beside: false,
            type_parameters: HashMap::default(),
            tested: false,
        }
    }

    /// `declare(strict_types=1);` first, then the namespaces and classes. Imports are not lowered: every class name
    /// in the tree is fully qualified.
    fn program(mut self, checked: &CheckedProgram<'arena>) -> Unit {
        for node in Node::Program(checked.program()).filter_map(|node| {
            matches!(node, Node::ArrowFunction(_) | Node::Closure(_) | Node::TypeParameter(_)).then_some(*node)
        }) {
            let lambda = match node {
                Node::TypeParameter(parameter) => {
                    let php_type = parameter.bound.as_ref().map_or(PhpType::Mixed, |bound| self.erase(&bound.hint));
                    self.type_parameters.insert(parameter.name.span.start.offset, php_type);

                    continue;
                }
                lambda => lambda.span(),
            };

            for (_, local) in self.names.captures(&lambda) {
                if self.names.is_written(local) {
                    self.by_reference.insert(local.declaration.start.offset);
                }
            }
        }

        let modules = Node::Program(checked.program()).filter_map(|node| match *node {
            Node::Module(module) => Some(module),
            _ => None,
        });
        self.tested = modules.first().is_some_and(|first| self.types.module(first.span()).kind == ModuleKind::Test);
        let mut statements = vec![self.strict_types()];
        for statement in &checked.program().statements {
            self.file_statement(statement, &mut statements);
        }
        let mut bindings = Vec::new();
        for module in &modules {
            self.module_bindings(module, &mut bindings);
        }
        // A test module applies with no bindings too, so its tests run without the environment modules.
        if let Some(first) = modules.first()
            && (self.tested || !bindings.is_empty())
        {
            let line = self.line(*first);
            let table = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &bindings);
            if self.tested {
                statements.extend(self.test_module(checked, line, table));
            } else {
                statements.push(self.node(SHARP_AST_RETURN, 0, line, &[table]));
            }
        }

        let root = self.node(SHARP_AST_STMT_LIST, 0, 1, &statements);
        self.nodes[root as usize].end_line = self.lines.last();
        self.inlined.sort();
        self.inlined.dedup();
        self.function_types.sort();

        Unit {
            nodes: self.nodes,
            children: self.children,
            root,
            texts: self.texts,
            inlined: self.inlined,
            function_types: self.function_types,
        }
    }

    fn strict_types(&mut self) -> u32 {
        let name = self.string(0, 1, b"strict_types");
        let value = self.zval(1, sharp_value::SHARP_LONG, |node| node.long_value = 1);
        let element = self.node(SHARP_AST_CONST_ELEM, 0, 1, &[name, value, NULL]);
        let list = self.node(SHARP_AST_CONST_DECL, 0, 1, &[element]);

        self.node(SHARP_AST_DECLARE, 0, 1, &[list, NULL])
    }

    fn file_statement(&mut self, statement: &Statement, statements: &mut Vec<u32>) {
        match statement {
            Statement::Namespace(namespace) => {
                let (Some(name), NamespaceBody::Implicit(body)) = (&namespace.name, &namespace.body) else {
                    unreachable!("check_slice refuses a namespace without a name or with braces");
                };

                let name = self.string(0, self.line(name), &php_name(name));
                statements.push(self.node(SHARP_AST_NAMESPACE, 0, self.line(namespace), &[name, NULL]));
                for statement in &body.statements {
                    self.file_statement(statement, statements);
                }
            }
            // An `extern` declaration tells only the checker what plain PHP does, and a module's laws are checked and
            // never run. A module's bindings are the table the file returns last, in `program`.
            Statement::Use(_) | Statement::Extern(_) | Statement::Module(_) => {}
            Statement::Class(class) => statements.push(self.class(class)),
            Statement::Interface(interface) => {
                statements.push(self.interface(interface));
                statements.extend(self.defaults(interface));
            }
            Statement::Enum(r#enum) if r#enum.runs_as_classes() => self.data_enum(r#enum, statements),
            Statement::Enum(r#enum) => statements.push(self.r#enum(r#enum)),
            Statement::Event(event) => statements.push(self.event(event)),
            Statement::Struct(r#struct) => statements.push(self.r#struct(r#struct)),
            _ => unreachable!("check_slice refuses the file statement `{statement}`"),
        }
    }

    /// Spec section 32: a test file's module, the call `\Sharp\Internal\testModule('App\Shop\CheckoutTest', table)`,
    /// which hands the container the module's bindings for the class the file declares, so the container uses them
    /// while a test of that class runs. A file declares one type, spec section 27, and a file whose type is no class
    /// has no test to run them in, so it calls nothing.
    fn test_module(&mut self, checked: &CheckedProgram, line: u32, table: u32) -> Option<u32> {
        let names = Node::Program(checked.program()).filter_map(|node| match *node {
            Node::Class(class) => Some(self.names.get(&class.name)),
            _ => None,
        });
        let class = self.string(0, line, names.first()?);
        let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\testModule");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[class, table]);

        Some(self.node(SHARP_AST_CALL, 0, line, &[function, arguments]))
    }

    /// Spec section 32: each test of a class a test file declares runs with the file's module, in PHPUnit's own
    /// process or a child one, because PHPUnit runs the methods `Before` and `After` mark wherever it runs the test.
    /// The class gets `__sharpStartTest`, which calls `\Sharp\Internal\startTest(static::class)` before every other
    /// `Before` method and `setUp`, and `__sharpEndTest`, which calls `\Sharp\Internal\endTest()` after every other
    /// `After` method and `tearDown`, by their priorities. PHP# refuses a member name that starts with `__`, so no
    /// member of the class can take them.
    fn test_hooks(&mut self, class: &Class) -> [u32; 2] {
        let line = self.line(class.class.span);
        let test_class = self.string(ZEND_NAME_NOT_FQ, line, b"static");
        let test_class = self.node(SHARP_AST_CLASS_NAME, 0, line, &[test_class]);
        let start = self.test_hook(
            class,
            b"__sharpStartTest",
            (b"PHPUnit\\Framework\\Attributes\\Before", i64::MAX),
            b"Sharp\\Internal\\startTest",
            &[test_class],
        );
        let end = self.test_hook(
            class,
            b"__sharpEndTest",
            (b"PHPUnit\\Framework\\Attributes\\After", i64::MIN),
            b"Sharp\\Internal\\endTest",
            &[],
        );

        [start, end]
    }

    /// A public `void` method of `class` named `name`, which PHPUnit's attribute runs at the priority `marked` gives,
    /// and which calls the function `function` with `arguments`.
    fn test_hook(
        &mut self,
        class: &Class,
        name: &[u8],
        marked: (&[u8], i64),
        function: &[u8],
        arguments: &[u32],
    ) -> u32 {
        let line = self.line(class.class.span);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, arguments);
        let function = self.string(ZEND_NAME_FQ, line, function);
        let call = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[call]);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let return_type = self.string(ZEND_NAME_NOT_FQ, line, b"void");
        let (attribute, priority) = marked;
        let attribute = self.string(ZEND_NAME_FQ, line, attribute);
        let priority = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = priority);
        let priority = self.node(SHARP_AST_ARG_LIST, 0, line, &[priority]);
        let attribute = self.node(SHARP_AST_ATTRIBUTE, 0, line, &[attribute, priority]);
        let group = self.node(SHARP_AST_ATTRIBUTE_GROUP, 0, line, &[attribute]);
        let attributes = self.node(SHARP_AST_ATTRIBUTE_LIST, 0, line, &[group]);

        self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC,
            class.class.span,
            class.right_brace,
            name,
            &[parameters, NULL, body, return_type, attributes],
        )
    }

    /// Spec section 32: a file whose base or environment modules bind services returns their bindings, which
    /// `Sharp\Container` reads. A module of laws alone returns nothing. Each binding is a list of the namespace whose
    /// classes it serves, its service's id, the class it serves or null, its lifetime, and its value: a class's full
    /// name, a list of them, or a static `fn` over the binding's parameters that returns its expression.
    fn module_bindings(&mut self, module: &Module, bindings: &mut Vec<u32>) {
        let metadata = self.types.module(module.span());
        let written = module.members.iter().filter_map(|member| match member {
            ModuleMember::Binding(binding) => Some(*binding),
            ModuleMember::Law(_) => None,
        });
        for (binding, binding_metadata) in written.zip(&metadata.bindings) {
            let line = self.line(binding);
            let namespace = self.string(0, line, metadata.namespace.as_bytes());
            let service = self.string(0, line, service_id(&binding_metadata.service.type_union).as_bytes());
            let target = match binding_metadata.target {
                Some(target) => self.string(0, line, target.as_bytes()),
                None => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
            };
            let lifetime = self.string(0, line, binding_metadata.lifetime.as_str().as_bytes());
            let value = match (&binding.value, &binding_metadata.value) {
                (_, BindingValueMetadata::Class(class, _)) => self.string(0, line, class.as_bytes()),
                (_, BindingValueMetadata::Classes(classes)) => {
                    let mut elements = Vec::new();
                    for (class, span) in classes {
                        let line = self.line(*span);
                        let class = self.string(0, line, class.as_bytes());
                        elements.push(self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[class, NULL]));
                    }

                    self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements)
                }
                (BindingValue::Expression(expression), BindingValueMetadata::Expression(lambda)) => {
                    let services = self.types.binding_parameters(*lambda);
                    let mut parameters = Vec::new();
                    for (index, parameter) in
                        binding.parameter_list.iter().flat_map(|list| &list.parameters).enumerate()
                    {
                        parameters.push(self.parameter(parameter, services.get(index)));
                    }
                    let parameter_line = binding.parameter_list.as_ref().map_or(line, |list| self.line(list));
                    let parameters = self.node(SHARP_AST_PARAM_LIST, 0, parameter_line, &parameters);
                    // A binding declares no type parameters, so its lambda captures no type arguments.
                    let (body, _) = self.lambda_body(|lowering| lowering.expression(expression));

                    self.declaration(
                        SHARP_AST_ARROW_FUNC,
                        ZEND_ACC_STATIC,
                        binding,
                        expression,
                        b"",
                        &[parameters, NULL, body, NULL, NULL],
                    )
                }
                _ => unreachable!("the scanner records a binding's expression as the lambda it runs as"),
            };

            let fields: Vec<u32> = [namespace, service, target, lifetime, value]
                .into_iter()
                .map(|field| self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[field, NULL]))
                .collect();
            let entry = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &fields);
            bindings.push(self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[entry, NULL]));
        }
    }

    /// A class sets the initial values PHP takes as no default, those that are not constant or belong to a `readonly`
    /// property, at the start of its constructor, in declaration order. A class without a constructor gets a public
    /// one that spans the class, and calls `parent::__construct()` after them when a constructor exists above the
    /// class, which the checker allows only when that constructor takes no parameters. The property of each promoted
    /// `Set` parameter comes just before the constructor.
    fn class(&mut self, class: &Class) -> u32 {
        self.class = self.names.get(&class.name);
        self.enter(class.name.value);
        let (parent, interfaces, parent_name) = match &class.inheritance {
            Some(inheritance) => self.class_header(inheritance),
            None => (NULL, NULL, None),
        };
        let mut members =
            self.class_members(&class.members, parent_name, class.class.span, class.left_brace, class.right_brace);
        members.extend(self.parse_methods(&class.members, class.name.span));
        members.extend(self.string_conversion(&class.members, class.right_brace));
        if self.tested {
            members.extend(self.test_hooks(class));
        }
        members.extend(self.generic_statics(&class.name, &class.members));
        members.extend(self.class_metadata(self.class, self.line(class.name)));
        members.splice(0..0, self.trait_members(class.inheritance.as_ref(), class.left_brace));
        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(class.left_brace), &members);
        let attributes = self.attributes(&class.attribute_lists, None, None);

        self.declaration(
            SHARP_AST_CLASS,
            class_flags(&class.modifiers),
            class.class.span,
            class.right_brace,
            class.name.value,
            &[parent, interfaces, members, attributes, NULL],
        )
    }

    /// Spec section 18: plain PHP converts an object of a class that declares `toString()` to the text a PHP#
    /// template shows, so the class gets PHP's `__toString()`, which returns `$this->toString()`, as Java's and
    /// Kotlin's string conversion calls `toString()`. PHP# refuses a member name that starts with `__`, so no member
    /// of the class can take it. None for a class that declares no `toString()`, which inherits its parent's.
    fn string_conversion(&mut self, members: &Sequence<ClassLikeMember>, right_brace: Span) -> Option<u32> {
        let to_string = members.iter().find_map(|member| match member {
            ClassLikeMember::Method(method) if method.name.value == b"toString" => Some(method),
            _ => None,
        })?;
        let line = self.line(to_string.name.span);
        let this = self.variable(to_string.name.span, b"this");
        let method = self.string(0, line, b"toString");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
        let call = self.node(SHARP_AST_METHOD_CALL, 0, line, &[this, method, arguments]);
        let result = self.node(SHARP_AST_RETURN, 0, line, &[call]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[result]);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let return_type = self.string(ZEND_NAME_NOT_FQ, line, b"string");

        Some(self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC,
            to_string.name.span,
            right_brace,
            b"__toString",
            &[parameters, NULL, body, return_type, NULL],
        ))
    }

    /// A struct is a final class, spec section 10, whose members are a class's, with its header's interfaces and
    /// `\Sharp\Struct` as its interface list, its header's traits as its `use`, and, unless it declares `operator ==`, the `op_Equality` that compares it by value, as
    /// php-src's grammar builds
    /// `final class Username implements \App\HasLabel, \Sharp\Struct`. The engine lets only a `.sharp` class implement
    /// `\Sharp\Struct`, and hashes a `Map` key or a `Set` element by its fields only for one that does.
    fn r#struct(&mut self, r#struct: &Struct) -> u32 {
        self.class = self.names.get(&r#struct.name);
        self.enter(r#struct.name.value);
        let mut members = self.class_members(
            &r#struct.members,
            None,
            r#struct.r#struct.span,
            r#struct.left_brace,
            r#struct.right_brace,
        );
        let declares_equality = r#struct.members.iter().any(|member| {
            matches!(member, ClassLikeMember::Operator(operator) if matches!(operator.symbol, BinaryOperator::Equal(_)))
        });
        if !declares_equality {
            members.push(self.value_equality(r#struct.name.span, &[self.class]));
        }
        members.extend(self.parse_methods(&r#struct.members, r#struct.name.span));
        members.extend(self.string_conversion(&r#struct.members, r#struct.right_brace));
        members.extend(self.generic_statics(&r#struct.name, &r#struct.members));
        members.extend(self.class_metadata(self.class, self.line(r#struct.name.span)));
        members.splice(0..0, self.trait_members(r#struct.inheritance.as_ref(), r#struct.left_brace));
        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(r#struct.left_brace), &members);
        let attributes = self.attributes(&r#struct.attribute_lists, None, None);
        let mut interfaces = Vec::new();
        // A header trait is the `use` above, spec section 22.
        for name in r#struct.inheritance.iter().flat_map(Inheritance::names) {
            let full_name = self.names.get(&name);
            if self.types.class_declaration(full_name).kind != DeclarationKind::Trait {
                interfaces.push(self.string(ZEND_NAME_FQ, self.line(name), full_name));
            }
        }
        let line = self.line(r#struct.name.span);
        interfaces.push(self.string(ZEND_NAME_FQ, line, b"Sharp\\Struct"));
        let header = self.node(SHARP_AST_NAME_LIST, 0, line, &interfaces);

        self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_FINAL,
            r#struct.r#struct.span,
            r#struct.right_brace,
            r#struct.name.value,
            &[NULL, header, members, attributes, NULL],
        )
    }

    /// The `==` of a struct, spec section 10, or of an enum with a case that carries data, section 20, as
    /// `public static function op_Equality(mixed $a, mixed $b): bool`, spanning the name at `span`. `classes` holds the
    /// struct, or the class of each case. Two instances of one of them are equal when each property equals its pair by
    /// the property's own `==`, joined by `&&`, so two of a plain case are always equal. Any other pair is equal only
    /// when identical, so null equals only null and a value of another type or case is unequal:
    /// `if ($a instanceof \App\Point && $b instanceof \App\Point) { return …; } return $a === $b;`.
    fn value_equality(&mut self, span: Span, classes: &[&[u8]]) -> u32 {
        let name = php_operator_name(&BinaryOperator::Equal(span), 2)
            .unwrap_or_else(|| unreachable!("`==` runs as `op_Equality`"));
        self.enter(name);
        let line = self.line(span);
        let mut parameters = Vec::new();
        for operand in [b"a", b"b"] {
            let hint = self.string(ZEND_NAME_NOT_FQ, line, b"mixed");
            let name = self.string(0, line, operand);
            parameters.push(self.node(SHARP_AST_PARAM, 0, line, &[hint, name, NULL, NULL, NULL, NULL]));
        }
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);

        let mut statements = Vec::new();
        for class in classes {
            let instances = [b"a", b"b"].map(|operand| {
                let value = self.variable(span, operand);
                let class = self.string(ZEND_NAME_FQ, line, class);

                self.node(SHARP_AST_INSTANCEOF, 0, line, &[value, class])
            });
            let both = self.node(SHARP_AST_AND, 0, line, &instances);

            let mut equal = None;
            for (property, r#type) in self.types.struct_properties(class) {
                let side = |object| Side::Property { span, object, property, r#type };
                let property_equal = self.equality(line, side(b"a"), side(b"b"), false);
                equal = Some(match equal {
                    Some(earlier) => self.node(SHARP_AST_AND, 0, line, &[earlier, property_equal]),
                    None => property_equal,
                });
            }
            let equal = equal.unwrap_or_else(|| self.zval(line, sharp_value::SHARP_TRUE, |_| {}));
            let r#return = self.node(SHARP_AST_RETURN, 0, line, &[equal]);
            let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[r#return]);
            let branch = self.node(SHARP_AST_IF_ELEM, 0, line, &[both, body]);
            statements.push(self.node(SHARP_AST_IF, 0, line, &[branch]));
        }
        let a = self.variable(span, b"a");
        let b = self.variable(span, b"b");
        let identical = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[a, b]);
        statements.push(self.node(SHARP_AST_RETURN, 0, line, &[identical]));
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let return_type = self.string(ZEND_NAME_NOT_FQ, line, b"bool");

        self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC | ZEND_ACC_STATIC,
            span,
            span,
            name,
            &[parameters, NULL, body, return_type, NULL],
        )
    }

    /// The `parse` and `tryParse` of the struct or PHP# class being lowered, whose `members` are declared and whose name
    /// is at `span`, where the codex gives it them, spec section 10. No input holds a function, so a type with a
    /// function field, also inside a collection, has neither, and the checker refuses its call.
    fn parse_methods(&mut self, members: &Sequence<ClassLikeMember>, span: Span) -> Vec<u32> {
        if !self.types.parses(self.class) {
            return Vec::new();
        }
        let Some(parse) = self.parse(members, span) else {
            return Vec::new();
        };

        vec![parse, self.try_parse(span)]
    }

    /// A struct's or a PHP# class's `parse`, spec section 10, as
    /// `public static function parse(array|object $value): \App\Point`, spanning the type's name, at `span`, with
    /// `members` its declared members. It reads each field `struct_parse_fields` gives from the input, a `Map`, the PHP
    /// array plain PHP passes for one, or an object's public properties, records each bad value's problem under its
    /// path in `$fields#2` through generics R3c's walk, and throws one `\Sharp\InvalidFields` when any is bad. Otherwise
    /// it creates the struct through its main constructor, with the arguments spread by name, so a missing key takes its
    /// parameter's default, then writes each property read. A field whose type has no type text, a function, gives no
    /// `parse`:
    ///
    /// ```php
    /// $input#1 = $value instanceof \Sharp\Map ? \iterator_to_array($value) : (\is_object($value) ? \get_object_vars($value) : $value);
    /// $fields#2 = [];
    /// $arguments#3 = [];
    /// $properties#4 = [];
    /// if (\array_key_exists('customerId', $input#1)) { …the read of an int… } else { $fields#2['customerId'] = 'missing'; }
    /// if ($fields#2 !== []) { throw new \Sharp\InvalidFields($fields#2); }
    /// $object#5 = new \App\RenewRequest(...$arguments#3);
    /// foreach ($properties#4 as $name#6 => $property#7) { $object#5->$name#6 = $property#7; }
    /// return $object#5;
    /// ```
    fn parse(&mut self, members: &Sequence<ClassLikeMember>, span: Span) -> Option<u32> {
        let fields = self.types.struct_parse_fields(self.class);
        let texts = fields.iter().map(|field| self.types.type_text(field.r#type)).collect::<Option<Vec<_>>>()?;
        self.enter(b"parse");
        // A generic type's `parse` takes its type arguments as a static method does, spec section 11.
        self.static_type_parameters = self.types.static_type_parameters(self.class);
        let line = self.line(span);
        let taken = self.temporaries;
        let [input, problems, arguments, properties, object, name, property] =
            ["input", "fields", "arguments", "properties", "object", "name", "property"].map(|base| self.hidden(base));
        let value = Place::variable(b"value");

        let mut statements = Vec::new();
        let is_map = self.instance_of(span, &value, b"Sharp\\Map");
        let entries = self.global_call_of(span, b"iterator_to_array", &value);
        let is_object = self.global_call_of(span, b"is_object", &value);
        let properties_of = self.global_call_of(span, b"get_object_vars", &value);
        let itself = self.place(span, &value);
        let read =
            self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[is_object, properties_of, itself]);
        let read = self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_map, entries, read]);
        statements.push(self.assign(span, &Place::variable(&input), read));
        for collected in [&problems, &arguments, &properties] {
            let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);
            statements.push(self.assign(span, &Place::variable(collected), empty));
        }
        for (field, text) in fields.iter().zip(&texts) {
            let collected = if field.is_parameter { &arguments } else { &properties };
            let target = Place::element(collected, Index::Text(field.name.to_vec()));
            let source = Place::element(&input, Index::Text(field.key.to_vec()));
            let hint = written_hint(members, field.name);
            let read = self.parse_field(span, field.r#type, text, hint, &source, field.key, &target, &problems);
            let key = self.string(0, line, field.key);
            let input_read = self.variable(span, &input);
            let present = self.global_call(line, b"array_key_exists", &[key, input_read]);
            let mut branches = vec![(present, read)];
            if !field.has_default {
                let missing = if takes_null(field.r#type) {
                    let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
                    self.assign(span, &target, null)
                } else {
                    let missing = self.string(0, line, b"missing");
                    self.assign(span, &Place::element(&problems, Index::Text(field.key.to_vec())), missing)
                };
                branches.push((NULL, missing));
            }
            statements.push(self.if_chain(span, &branches));
        }

        let any_bad = self.not_empty(span, &problems);
        let exception = self.string(ZEND_NAME_FQ, line, b"Sharp\\InvalidFields");
        let problems = self.variable(span, &problems);
        let problems = self.node(SHARP_AST_ARG_LIST, 0, line, &[problems]);
        let created = self.node(SHARP_AST_NEW, 0, line, &[exception, problems]);
        let throw = self.node(SHARP_AST_THROW, 0, line, &[created]);
        statements.push(self.if_chain(span, &[(any_bad, throw)]));

        let class = self.string(ZEND_NAME_FQ, line, self.class);
        let spread = self.variable(span, &arguments);
        let spread = self.node(SHARP_AST_UNPACK, 0, line, &[spread]);
        let spread = self.node(SHARP_AST_ARG_LIST, 0, line, &[spread]);
        let mut created = self.node(SHARP_AST_NEW, 0, line, &[class, spread]);
        if let Some(count) = self.static_type_parameters {
            let own = self.type_text(line, &type_parameters_text(count));
            created = self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[created, own]);
        }
        statements.push(self.assign(span, &Place::variable(&object), created));
        let object_read = self.variable(span, &object);
        let property_name = self.variable(span, &name);
        let written = self.node(SHARP_AST_PROP, 0, line, &[object_read, property_name]);
        let property_read = self.variable(span, &property);
        let write = self.node(SHARP_AST_ASSIGN, 0, line, &[written, property_read]);
        let write = self.node(SHARP_AST_STMT_LIST, 0, line, &[write]);
        let each = [self.variable(span, &properties), self.variable(span, &property), self.variable(span, &name)];
        statements.push(self.node(SHARP_AST_FOREACH, 0, line, &[each[0], each[1], each[2], write]));
        let object_read = self.variable(span, &object);
        statements.push(self.node(SHARP_AST_RETURN, 0, line, &[object_read]));
        self.temporaries = taken;

        let parameters = self.parse_parameters(span);
        self.static_type_parameters = None;
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let return_type = self.string(ZEND_NAME_FQ, line, self.class);

        Some(self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC | ZEND_ACC_STATIC,
            span,
            span,
            b"parse",
            &[parameters, NULL, body, return_type, NULL],
        ))
    }

    /// A struct's or a PHP# class's `tryParse`, spec section 10: its `parse`, giving null where `parse` throws
    /// `\Sharp\InvalidFields`.
    ///
    /// ```php
    /// public static function tryParse(array|object $value): ?\App\Point
    /// {
    ///     try { return \App\Point::parse($value); } catch (\Sharp\InvalidFields) { return null; }
    /// }
    /// ```
    fn try_parse(&mut self, span: Span) -> u32 {
        self.enter(b"tryParse");
        self.static_type_parameters = self.types.static_type_parameters(self.class);
        let line = self.line(span);
        let class = self.string(ZEND_NAME_FQ, line, self.class);
        let method = self.string(0, line, b"parse");
        let mut arguments = vec![self.variable(span, b"value")];
        if let Some(count) = self.static_type_parameters {
            arguments.push(self.type_arguments_argument(line, &type_parameters_text(count)));
        }
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);
        let parsed = self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments]);
        let parsed = self.node(SHARP_AST_RETURN, 0, line, &[parsed]);
        let parsed = self.node(SHARP_AST_STMT_LIST, 0, line, &[parsed]);
        let exception = self.string(ZEND_NAME_FQ, line, b"Sharp\\InvalidFields");
        let exception = self.node(SHARP_AST_NAME_LIST, 0, line, &[exception]);
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
        let null = self.node(SHARP_AST_RETURN, 0, line, &[null]);
        let null = self.node(SHARP_AST_STMT_LIST, 0, line, &[null]);
        let catch = self.node(SHARP_AST_CATCH, 0, line, &[exception, NULL, null]);
        let catches = self.node(SHARP_AST_CATCH_LIST, 0, line, &[catch]);
        let r#try = self.node(SHARP_AST_TRY, 0, line, &[parsed, catches, NULL]);

        let parameters = self.parse_parameters(span);
        self.static_type_parameters = None;
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[r#try]);
        let return_type = self.string(ZEND_NAME_FQ, line, self.class);
        self.nodes[return_type as usize].attr |= ZEND_TYPE_NULLABLE;

        self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC | ZEND_ACC_STATIC,
            span,
            span,
            b"tryParse",
            &[parameters, NULL, body, return_type, NULL],
        )
    }

    /// `(array|object $value)`, what `parse` and `tryParse` take: a `Map<string, Any?>` or an object. A generic type's
    /// ends with the bounds of its type parameters, as any static method of it does.
    fn parse_parameters(&mut self, span: Span) -> u32 {
        let line = self.line(span);
        let array = self.node(SHARP_AST_TYPE, IS_ARRAY, line, &[]);
        let object = self.string(ZEND_NAME_NOT_FQ, line, b"object");
        let hint = self.node(SHARP_AST_TYPE_UNION, 0, line, &[array, object]);
        let name = self.string(0, line, b"value");
        let mut parameters = vec![self.node(SHARP_AST_PARAM, 0, line, &[hint, name, NULL, NULL, NULL, NULL])];
        parameters.extend(self.parameter_metadata((self.types.bounds(self.class), None), line));

        self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters)
    }

    /// The read of a field present in the input, from `source` into `target`. Each nested struct is built from its
    /// value through its own `parse`, and each backed enum case from its backing value, then generics R3c's walk checks
    /// the whole value against `r#type`, adding each bad value's problem to `fields` under its path, which starts at
    /// `key`. The walk takes only a struct object where a struct is expected, so where a nested `parse` already moved
    /// its problems under a path, the walk's own problem at that path is dropped. Once the walk accepts the value, each
    /// `Set` in it holds its elements under their keys. `Any?` takes the value as it is. `hint` is the type as the
    /// field writes it, which orders a union of structs.
    ///
    /// ```php
    /// $read#N = $input#1['addresses'];
    /// $failed#N = [];
    /// …each nested struct and enum case built in $read#N…
    /// if (\Sharp\Internal\accepts('List<App.Address>', $read#N, 'addresses', $fields#2)) { …each Set built… }
    /// foreach ($failed#N as $path#N) { unset($fields#2[$path#N]); }
    /// $arguments#3['addresses'] = $read#N;
    /// ```
    #[allow(clippy::too_many_arguments)]
    fn parse_field(
        &mut self,
        span: Span,
        r#type: &TUnion,
        text: &str,
        hint: Option<&Hint>,
        source: &Place,
        key: &[u8],
        target: &Place,
        fields: &[u8],
    ) -> u32 {
        let line = self.line(span);
        if r#type.types.iter().any(|atomic| matches!(atomic, TAtomic::Mixed(mixed) if !mixed.is_non_null())) {
            let value = self.place(span, source);

            return self.assign(span, target, value);
        }

        let [read, failed] = ["read", "failed"].map(|base| self.hidden(base));
        let value = self.place(span, source);
        let mut statements = vec![self.assign(span, &Place::variable(&read), value)];
        let mut moves = false;
        let path = [PathPart::Text(key.to_vec())];
        let built = self.parse_build(span, r#type, hint, &read, &path, fields, &failed, &mut moves);
        if moves {
            let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);
            statements.push(self.assign(span, &Place::variable(&failed), empty));
        }
        statements.extend(built);

        let arguments = [
            self.type_text(line, text),
            self.variable(span, &read),
            self.string(0, line, key),
            self.variable(span, fields),
        ];
        let accepts = self.global_call(line, b"Sharp\\Internal\\accepts", &arguments);
        statements.push(match self.crossing_array(r#type) {
            Some(_) => {
                let place = self.variable(span, &read);
                let first = self.variable(span, &read);
                let crossed = self.crossed(span, r#type, &read, first);
                let crossed = self.node(SHARP_AST_ASSIGN, 0, line, &[place, crossed]);

                self.if_chain(span, &[(accepts, crossed)])
            }
            None => accepts,
        });
        if moves {
            let path = self.hidden("path");
            let slot = self.place(span, &Place::element(fields, Index::Variable(path.clone())));
            let unset = self.node(SHARP_AST_UNSET, 0, line, &[slot]);
            let unset = self.node(SHARP_AST_STMT_LIST, 0, line, &[unset]);
            let each = [self.variable(span, &failed), self.variable(span, &path)];
            statements.push(self.node(SHARP_AST_FOREACH, 0, line, &[each[0], each[1], NULL, unset]));
        }
        let read_value = self.variable(span, &read);
        statements.push(self.assign(span, target, read_value));

        self.node(SHARP_AST_STMT_LIST, 0, line, &statements)
    }

    /// The statements that build, in the value of `variable`, each struct and backed enum case `r#type` holds from the
    /// value plain PHP gives, at `path`: a struct through its own `parse` from a `Map` or an object, and a case through
    /// `tryFrom` from its backing value, inside a `List`, `Set` or `Map` element by element. A value that builds
    /// nothing stays as it is, for the walk to report. Where the value can only be the one struct, that struct's
    /// problems move under `path`, which `failed` records, and `moves` becomes true. Where it may be one of several, the
    /// first that builds is taken, in the order `hint`, the type as the source writes it, names them, as serde's
    /// untagged enums try their variants. The checker's union holds its members sorted, so only `hint` keeps that order.
    ///
    /// ```php
    /// if (!$read#N instanceof \App\Address && (\is_array($read#N) || \is_object($read#N))) {
    ///     try { $read#N = \App\Address::parse($read#N); }
    ///     catch (\Sharp\InvalidFields $inner#N) {
    ///         foreach ($inner#N->fields as $key#N => $problem#N) { $fields#2[path . '.' . $key#N] = $problem#N; }
    ///         $failed#N[] = path;
    ///     }
    /// }
    /// if (\is_string($read#N) && ($case#N = \App\Status::tryFrom($read#N)) !== null) { $read#N = $case#N; }
    /// if (\is_array($read#N)) { foreach ($read#N as $key#N => $element#N) { …$element#N built… $read#N[$key#N] = $element#N; } }
    /// ```
    #[allow(clippy::too_many_arguments)]
    fn parse_build(
        &mut self,
        span: Span,
        r#type: &TUnion,
        hint: Option<&Hint>,
        variable: &[u8],
        path: &[PathPart],
        fields: &[u8],
        failed: &[u8],
        moves: &mut bool,
    ) -> Vec<u32> {
        let line = self.line(span);
        let value = Place::variable(variable);
        let members: Vec<&TAtomic> = r#type.types.iter().filter(|atomic| !atomic.is_null()).collect();
        let written = hint.map(written_members).unwrap_or_default();
        let mut statements = Vec::new();

        let mut structs: Vec<&TNamedObject> = members
            .iter()
            .filter_map(|atomic| match atomic {
                TAtomic::Object(TObject::Named(object)) if self.types.parses(object.name.as_bytes()) => Some(object),
                _ => None,
            })
            .collect();
        structs.sort_by_key(|class| {
            written.iter().position(|member| {
                matches!(member, Hint::Identifier(name) if self.names.get(name).eq_ignore_ascii_case(class.name.as_bytes()))
            })
        });
        if !structs.is_empty() {
            let readable =
                [self.global_call_of(span, b"is_array", &value), self.global_call_of(span, b"is_object", &value)];
            let mut condition = self.node(SHARP_AST_OR, 0, line, &readable);
            for class in &structs {
                let is_struct = self.instance_of(span, &value, class.name.as_bytes());
                let is_not = self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[is_struct]);
                condition = self.node(SHARP_AST_AND, 0, line, &[is_not, condition]);
            }
            let built = if let ([class], [_]) = (structs.as_slice(), members.as_slice()) {
                *moves = true;
                self.parse_nested(span, class, variable, path, fields, failed)
            } else {
                let mut otherwise = self.node(SHARP_AST_STMT_LIST, 0, line, &[]);
                for class in structs.iter().rev() {
                    let parsed = self.nested_parse(span, class, variable);
                    let exception = self.string(ZEND_NAME_FQ, line, b"Sharp\\InvalidFields");
                    let exception = self.node(SHARP_AST_NAME_LIST, 0, line, &[exception]);
                    let catch = self.node(SHARP_AST_CATCH, 0, line, &[exception, NULL, otherwise]);
                    let catches = self.node(SHARP_AST_CATCH_LIST, 0, line, &[catch]);
                    let r#try = self.node(SHARP_AST_TRY, 0, line, &[parsed, catches, NULL]);
                    otherwise = self.node(SHARP_AST_STMT_LIST, 0, line, &[r#try]);
                }

                otherwise
            };
            statements.push(self.if_chain(span, &[(condition, built)]));
        }

        for atomic in &members {
            let TAtomic::Object(TObject::Enum(r#enum)) = atomic else {
                continue;
            };
            let class = r#enum.name.as_bytes();
            let Some(backing) = self.types.backing_type(class) else {
                continue;
            };
            let case = self.hidden("case");
            let backs: &[u8] = if backing.is_int() { b"is_int" } else { b"is_string" };
            let backs = self.global_call_of(span, backs, &value);
            let found = self.try_from(span, class, &value);
            let case_read = self.variable(span, &case);
            let found = self.node(SHARP_AST_ASSIGN, 0, line, &[case_read, found]);
            let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
            let found = self.node(SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL, line, &[found, null]);
            let named = self.node(SHARP_AST_AND, 0, line, &[backs, found]);
            let case_read = self.variable(span, &case);
            let from_value = self.assign(span, &value, case_read);
            statements.push(self.if_chain(span, &[(named, from_value)]));
        }

        for atomic in &members {
            let TAtomic::Array(array) = atomic else {
                continue;
            };
            let (_, element_type) = self.types.array_parameters(array);
            if !self.builds(&element_type) {
                continue;
            }
            let [key, element] = ["key", "element"].map(|base| self.hidden(base));
            let (open, close): (&[u8], &[u8]) =
                if matches!(array, TArray::Keyed(_)) { (b"[\"", b"\"]") } else { (b"[", b"]") };
            let element_path = [
                path,
                &[PathPart::Text(open.to_vec()), PathPart::Variable(key.clone()), PathPart::Text(close.to_vec())],
            ]
            .concat();
            let element_hint = written.iter().find_map(|member| element_hint(member, array));
            let mut each =
                self.parse_build(span, &element_type, element_hint, &element, &element_path, fields, failed, moves);
            let element_read = self.variable(span, &element);
            each.push(self.assign(span, &Place::element(variable, Index::Variable(key.clone())), element_read));
            let each = self.node(SHARP_AST_STMT_LIST, 0, line, &each);
            let parts = [self.place(span, &value), self.variable(span, &element), self.variable(span, &key)];
            let each = self.node(SHARP_AST_FOREACH, 0, line, &[parts[0], parts[1], parts[2], each]);
            let is_array = self.global_call_of(span, b"is_array", &value);
            statements.push(self.if_chain(span, &[(is_array, each)]));
        }

        statements
    }

    /// `try { $read#N = \App\Address::parse($read#N); }`, the struct `class` built from the value of `variable`, a
    /// generic one with the type arguments the field gives it, as `\App\Pair::parse($read#N, <int, string>)`.
    fn nested_parse(&mut self, span: Span, class: &TNamedObject, variable: &[u8]) -> u32 {
        let line = self.line(span);
        let name = self.string(ZEND_NAME_FQ, line, class.name.as_bytes());
        let method = self.string(0, line, b"parse");
        let mut arguments = vec![self.variable(span, variable)];
        if let Some(type_arguments) = class.get_type_parameters() {
            let text = self.types.argument_texts(type_arguments);
            arguments.push(self.type_arguments_argument(line, &text));
        }
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);
        let parsed = self.node(SHARP_AST_STATIC_CALL, 0, line, &[name, method, arguments]);
        let parsed = self.assign(span, &Place::variable(variable), parsed);

        self.node(SHARP_AST_STMT_LIST, 0, line, &[parsed])
    }

    /// The struct `class` built from the value of `variable` through its own `parse`, each of its problems moved
    /// under `path`, as `addresses[1].city`, and `path` recorded in `failed`.
    fn parse_nested(
        &mut self,
        span: Span,
        class: &TNamedObject,
        variable: &[u8],
        path: &[PathPart],
        fields: &[u8],
        failed: &[u8],
    ) -> u32 {
        let line = self.line(span);
        let [inner, key, problem] = ["inner", "key", "problem"].map(|base| self.hidden(base));
        let parsed = self.nested_parse(span, class, variable);
        let inner_path = [path, &[PathPart::Text(b".".to_vec()), PathPart::Variable(key.clone())]].concat();
        let moved = self.parse_problem(span, &inner_path, fields, Place::variable(&problem));
        let moved = self.node(SHARP_AST_STMT_LIST, 0, line, &[moved]);
        let inner_read = self.variable(span, &inner);
        let property = self.string(0, line, b"fields");
        let problems = self.node(SHARP_AST_PROP, 0, line, &[inner_read, property]);
        let each = [self.variable(span, &problem), self.variable(span, &key)];
        let each = self.node(SHARP_AST_FOREACH, 0, line, &[problems, each[0], each[1], moved]);
        let path = self.path(span, path);
        let recorded = self.assign(span, &Place::element(failed, Index::Append), path);
        let caught = self.node(SHARP_AST_STMT_LIST, 0, line, &[each, recorded]);
        let exception = self.string(ZEND_NAME_FQ, line, b"Sharp\\InvalidFields");
        let exception = self.node(SHARP_AST_NAME_LIST, 0, line, &[exception]);
        let inner = self.string(0, line, &inner);
        let catch = self.node(SHARP_AST_CATCH, 0, line, &[exception, inner, caught]);
        let catches = self.node(SHARP_AST_CATCH_LIST, 0, line, &[catch]);

        self.node(SHARP_AST_TRY, 0, line, &[parsed, catches, NULL])
    }

    /// Whether `r#type` holds a struct, a PHP# class or a backed enum, at any depth, which `parse_build` builds from a
    /// value.
    fn builds(&self, r#type: &TUnion) -> bool {
        r#type.types.iter().any(|atomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => self.types.parses(object.name.as_bytes()),
            TAtomic::Object(TObject::Enum(r#enum)) => self.types.backing_type(r#enum.name.as_bytes()).is_some(),
            TAtomic::Array(array) => self.builds(&self.types.array_parameters(array).1),
            _ => false,
        })
    }

    /// `\App\Status::tryFrom($value)`, its value cast to a `string` for a `string`-backed enum, as PHP holds an
    /// all-digit `string` key as an `int`.
    fn try_from(&mut self, span: Span, class: &[u8], source: &Place) -> u32 {
        let line = self.line(span);
        let mut value = self.place(span, source);
        if !self.types.backing_type(class).is_some_and(TAtomic::is_int) {
            value = self.node(SHARP_AST_CAST, IS_STRING, line, &[value]);
        }
        let name = self.string(ZEND_NAME_FQ, line, class);
        let method = self.string(0, line, b"tryFrom");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[name, method, arguments])
    }

    /// `$fields[path] = $problem;`, a problem a nested struct's `parse` reported, recorded under `path`.
    fn parse_problem(&mut self, span: Span, path: &[PathPart], fields: &[u8], problem: Place) -> u32 {
        let line = self.line(span);
        let problem = self.place(span, &problem);
        let path = self.path(span, path);
        let fields = self.variable(span, fields);
        let slot = self.node(SHARP_AST_DIM, 0, line, &[fields, path]);

        self.node(SHARP_AST_ASSIGN, 0, line, &[slot, problem])
    }

    /// `$value instanceof \Class`.
    fn instance_of(&mut self, span: Span, source: &Place, class: &[u8]) -> u32 {
        let line = self.line(span);
        let value = self.place(span, source);
        let class = self.string(ZEND_NAME_FQ, line, class);

        self.node(SHARP_AST_INSTANCEOF, 0, line, &[value, class])
    }

    /// `\function($value)`, a global PHP function called on a read of `source`.
    fn global_call_of(&mut self, span: Span, function: &[u8], source: &Place) -> u32 {
        let value = self.place(span, source);

        self.global_call(self.line(span), function, &[value])
    }

    /// `$variable !== []`.
    fn not_empty(&mut self, span: Span, variable: &[u8]) -> u32 {
        let line = self.line(span);
        let read = self.variable(span, variable);
        let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);

        self.node(SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL, line, &[read, empty])
    }

    /// A read of `place`.
    fn place(&mut self, span: Span, place: &Place) -> u32 {
        let line = self.line(span);
        let variable = self.variable(span, &place.variable);
        let index = match &place.index {
            Index::None => return variable,
            Index::Text(text) => self.string(0, line, text),
            Index::Variable(name) => self.variable(span, name),
            Index::Append => NULL,
        };

        self.node(SHARP_AST_DIM, 0, line, &[variable, index])
    }

    /// `place = value;`.
    fn assign(&mut self, span: Span, place: &Place, value: u32) -> u32 {
        let place = self.place(span, place);

        self.node(SHARP_AST_ASSIGN, 0, self.line(span), &[place, value])
    }

    /// The text of `parts` joined by `.`, as `'addresses' . '[' . $key#N . ']'`.
    fn path(&mut self, span: Span, parts: &[PathPart]) -> u32 {
        let line = self.line(span);
        let mut joined: Option<u32> = None;
        for part in parts {
            let part = match part {
                PathPart::Text(text) => self.string(0, line, text),
                PathPart::Variable(name) => self.variable(span, name),
            };
            joined = Some(match joined {
                Some(earlier) => self.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[earlier, part]),
                None => part,
            });
        }

        joined.unwrap_or_else(|| unreachable!("a path names its field"))
    }

    /// An `if` of `branches`, each a condition and its statement, the last one `else` when its condition is `NULL`.
    fn if_chain(&mut self, span: Span, branches: &[(u32, u32)]) -> u32 {
        let line = self.line(span);
        let mut elements = Vec::new();
        for &(condition, statement) in branches {
            let statements = self.node(SHARP_AST_STMT_LIST, 0, line, &[statement]);
            elements.push(self.node(SHARP_AST_IF_ELEM, 0, line, &[condition, statements]));
        }

        self.node(SHARP_AST_IF, 0, line, &elements)
    }

    /// The name of a hidden variable, `{base}#N`, which no other hidden variable of the method being lowered holds.
    fn hidden(&mut self, base: &str) -> Vec<u8> {
        self.temporaries += 1;

        format!("{base}#{}", self.temporaries).into_bytes()
    }

    /// The members of a class or a struct, each lowered, with the constructor the class needs when it declares none
    /// and has initial values to set, spanning `keyword` to `right_brace` on the line of `left_brace`. `parent_name` is
    /// the full name of the parent a class's header names.
    fn class_members(
        &mut self,
        class_members: &Sequence<ClassLikeMember>,
        parent_name: Option<&'lowering [u8]>,
        keyword: Span,
        left_brace: Span,
        right_brace: Span,
    ) -> Vec<u32> {
        // Each behavior is created before any initial value, which its property's `set` may run through.
        let mut initial_values = Vec::new();
        for member in class_members {
            if let ClassLikeMember::Property(Property::Hooked(property)) = member
                && let Some(behaviors) = &property.behaviors
            {
                initial_values.extend(self.behavior_creations(property.item.variable(), behaviors));
            }
        }
        for member in class_members {
            if let ClassLikeMember::Property(property) = member
                && property.initial_value_runs_in_constructor()
                && let Some(value) = property.initial_value()
            {
                initial_values.push(self.initial_value(property, value));
            }
        }

        let mut members = Vec::new();
        let mut has_constructor = false;
        let keeps_statics = self.types.bounds(self.class).is_some();
        // A law, spec section 28, is checked and never runs, so the engine gets no node for it.
        for member in class_members.iter().filter(|member| !matches!(member, ClassLikeMember::Law(_))) {
            let member = match member {
                // A generic class keeps its static fields in `statics_store`, and its statics with accessors' storage.
                ClassLikeMember::Property(property)
                    if keeps_statics && property.modifiers().contains_static() && !runs_static_accessors(property) =>
                {
                    continue;
                }
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    has_constructor = true;
                    let promoted =
                        || method.parameter_list.parameters.iter().filter(|parameter| parameter.is_promoted_property());
                    // A superglobal's name runs hidden as a parameter, and stays its own as the property.
                    self.promoted_beside = promoted().any(|parameter| {
                        collection_hint(parameter.hint.as_ref()).is_some()
                            || runs_its_accessors(parameter)
                            || SUPERGLOBALS.contains(&parameter.variable.name)
                    });
                    if self.promoted_beside {
                        for parameter in promoted() {
                            members.push(self.promoted_property(parameter));
                            if let Some(behaviors) = &parameter.behaviors {
                                members.extend(self.behavior_slots(&parameter.variable, behaviors));
                            }
                        }
                    }
                    let constructor =
                        self.method(method, modifier_flags_or_private(&method.modifiers), &initial_values);
                    self.promoted_beside = false;

                    constructor
                }
                ClassLikeMember::Method(method)
                    if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
                {
                    let call = self.native_call(method, self.class);
                    let body = if method.returns_value() {
                        self.node(SHARP_AST_RETURN, 0, self.line(method.name.span), &[call])
                    } else {
                        call
                    };

                    self.method(method, modifier_flags_or_private(&method.modifiers), &[body])
                }
                ClassLikeMember::Method(method) => {
                    self.method(method, modifier_flags_or_private(&method.modifiers), &[])
                }
                ClassLikeMember::Operator(operator) => self.operator(operator),
                ClassLikeMember::NamedConstructor(
                    constructor @ NamedConstructor { initializer: Some(initializer), .. },
                ) => self.named_constructor(constructor, initializer),
                ClassLikeMember::Property(property) if runs_static_accessors(property) => {
                    members.extend(self.static_accessors(property, parent_name));

                    continue;
                }
                ClassLikeMember::Property(property) => {
                    if let Property::Computed(computed) = property
                        && is_lazy(&computed.modifiers)
                    {
                        members.push(self.lazy_flag(computed));
                    }
                    if let Property::Hooked(HookedProperty { item, behaviors: Some(behaviors), .. }) = property {
                        members.extend(self.behavior_slots(item.variable(), behaviors));
                    }

                    self.property(property, parent_name)
                }
                ClassLikeMember::Constant(constant) => self.constant(constant),
                ClassLikeMember::Extension(extension) => {
                    for member in &extension.members {
                        members.push(self.extension_member(extension, member));
                    }

                    continue;
                }
                _ => unreachable!("check_slice refuses the class member `{member}`"),
            };
            members.push(member);
        }

        if !has_constructor && !initial_values.is_empty() {
            let line = self.line(left_brace);
            if self.types.parent_constructor(self.class).is_some() {
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
                initial_values.push(self.parent_construct(line, arguments));
            }
            let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
            let body = self.node(SHARP_AST_STMT_LIST, 0, line, &initial_values);

            members.push(self.declaration(
                SHARP_AST_METHOD,
                ZEND_ACC_PUBLIC,
                keyword,
                right_brace,
                b"__construct",
                &[parameters, NULL, body, NULL, NULL],
            ));
        }
        members.extend(self.copy_resets(class_members, parent_name.is_some(), keyword, right_brace));
        if !keeps_statics {
            members.extend(self.static_initial_values(class_members));
        }

        members
    }

    /// The hidden `public function __clone(): void` of a class whose copy must not share what its original holds, spec
    /// sections 6.4, 6.5 and 16. A copy, made with `with` or with plain PHP's `clone`, gets a new instance of each
    /// behavior, and computes each `lazy` property again on its first read, as Kotlin's `copy()` does. It runs the
    /// parent's `__clone` first when the parent has one. None for a class with neither.
    fn copy_resets(
        &mut self,
        class_members: &Sequence<ClassLikeMember>,
        has_parent: bool,
        keyword: Span,
        right_brace: Span,
    ) -> Option<u32> {
        let line = self.line(right_brace);
        let mut statements = Vec::new();
        for member in class_members {
            match member {
                ClassLikeMember::Property(Property::Hooked(property)) => {
                    if let Some(behaviors) = &property.behaviors {
                        statements.extend(self.behavior_creations(property.item.variable(), behaviors));
                    }
                }
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    for parameter in method.parameter_list.parameters.iter() {
                        if let Some(behaviors) =
                            parameter.behaviors.as_ref().filter(|_| parameter.is_promoted_property())
                        {
                            statements.extend(self.behavior_creations(&parameter.variable, behaviors));
                        }
                    }
                }
                ClassLikeMember::Property(Property::Computed(computed))
                    if is_lazy(&computed.modifiers) && !computed.modifiers.contains_static() =>
                {
                    let flag = self.lazy_flag_fetch(line, computed.variable.name);
                    let r#false = self.zval(line, sharp_value::SHARP_FALSE, |_| {});
                    statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[flag, r#false]));
                }
                _ => {}
            }
        }
        if statements.is_empty() {
            return None;
        }

        // `if (\method_exists(parent::class, '__clone')) { parent::__clone(); }`: a parent's own copy runs first.
        if has_parent {
            let parent = self.string(ZEND_NAME_NOT_FQ, line, b"parent");
            let class = self.node(SHARP_AST_CLASS_NAME, 0, line, &[parent]);
            let name = self.string(0, line, b"__clone");
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[class, name]);
            let function = self.string(ZEND_NAME_FQ, line, b"method_exists");
            let exists = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
            let parent = self.string(ZEND_NAME_NOT_FQ, line, b"parent");
            let method = self.string(0, line, b"__clone");
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
            let call = self.node(SHARP_AST_STATIC_CALL, 0, line, &[parent, method, arguments]);
            let call = self.node(SHARP_AST_STMT_LIST, 0, line, &[call]);
            let branch = self.node(SHARP_AST_IF_ELEM, 0, line, &[exists, call]);
            statements.insert(0, self.node(SHARP_AST_IF, 0, line, &[branch]));
        }
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let void = self.string(ZEND_NAME_NOT_FQ, line, b"void");

        Some(self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC,
            keyword,
            right_brace,
            b"__clone",
            &[parameters, NULL, body, void, NULL],
        ))
    }

    /// The [`Self::statics_store`] and [`Self::statics_init`] of a generic PHP# class or struct named `name` with
    /// `members` that declares static fields, spec section 11, or none.
    fn generic_statics(&mut self, name: &LocalIdentifier, members: &Sequence<ClassLikeMember>) -> Vec<u32> {
        if self.types.bounds(self.class).is_none() {
            return Vec::new();
        }
        let Some(store) = self.statics_store(name, members) else {
            return Vec::new();
        };

        [Some(store), self.statics_init(name, members)].into_iter().flatten().collect()
    }

    /// The hidden static a generic PHP# class or struct, named `name` with `members`, keeps each type argument's own
    /// static fields in, spec section 11, as
    /// `public static $"\0<sharp>\0statics" = ["" => ["count" => 0, "items" => []]];` with `ZEND_ACC_SHARP_HIDDEN`:
    /// under the key "" the defaults, each constant initial value, which `\Sharp\Internal\statics()` copies for each
    /// type argument text on its first use. An initial value that runs as code is set by the class's
    /// [`Self::statics_init`]. None for a class without static fields.
    fn statics_store(&mut self, name: &LocalIdentifier, members: &Sequence<ClassLikeMember>) -> Option<u32> {
        let line = self.line(name);
        let mut stores = false;
        let mut fields = Vec::new();
        for member in members {
            let ClassLikeMember::Property(property) = member else {
                continue;
            };
            let Some(value) = property.initial_value().filter(|_| property.modifiers().contains_static()) else {
                continue;
            };
            stores = true;
            if property.initial_value_runs_after_class() {
                continue;
            }

            let variable = property.first_variable();
            let field_line = self.line(variable);
            let value = self.expression(value);
            let name = self.string(0, field_line, variable.name);
            fields.push(self.node(SHARP_AST_ARRAY_ELEM, 0, field_line, &[value, name]));
        }
        if !stores {
            return None;
        }

        Some(self.statics_property(line, &fields))
    }

    /// The hidden static of [`Self::statics_store`], with `fields`, each an array element of a field's initial value
    /// under its name, as its defaults.
    fn statics_property(&mut self, line: u32, fields: &[u32]) -> u32 {
        let defaults = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, fields);
        let empty = self.string(0, line, b"");
        let defaults = self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[defaults, empty]);
        let store = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[defaults]);
        let name = self.string(0, line, STATICS);
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, store, NULL, NULL]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);

        self.node(
            SHARP_AST_PROP_GROUP,
            ZEND_ACC_PUBLIC | ZEND_ACC_STATIC | ZEND_ACC_SHARP_HIDDEN,
            line,
            &[NULL, declaration, NULL],
        )
    }

    /// The hidden static method of a generic PHP# class or struct named `name` with `members` that runs each initial
    /// value that runs as code, in declaration order, which `\Sharp\Internal\statics` calls when it adds a type
    /// argument's entry, as C# runs a closed type's static initializer on its first use: `private static function
    /// '#init'($"\0<sharp>\0types") { \Sharp\Internal\statics(\App\Counter::class, <the text of #0>)['made'] = …; }`.
    /// It takes the type arguments in the hidden local a method keeps its own in, which the engine fills from the
    /// entry's key. A throw leaves the entry's other fields as they are and runs nothing again, as a class's `#statics`
    /// does. None for a class whose initial values are all constant.
    fn statics_init(&mut self, name: &LocalIdentifier, members: &Sequence<ClassLikeMember>) -> Option<u32> {
        let initial_values: Vec<&Property> = members
            .iter()
            .filter_map(|member| match member {
                ClassLikeMember::Property(property) if property.initial_value_runs_after_class() => Some(property),
                _ => None,
            })
            .collect();
        if initial_values.is_empty() {
            return None;
        }

        let line = self.line(name);
        let count = self.types.static_type_parameters(self.class);
        self.static_type_parameters = count;
        let own = type_parameters_text(
            count.unwrap_or_else(|| unreachable!("a class keeps statics per type argument only when it is generic")),
        );
        let mut writes = Vec::new();
        for property in initial_values {
            let variable = property.first_variable();
            let Some(value) = property.initial_value() else {
                unreachable!("a static whose initial value runs after its class has one");
            };
            let field_line = self.line(variable);
            let value = self.expression(value);
            let field = self.statics_field(field_line, self.class, &own, variable.name, PlaceUse::Write);
            writes.push(self.node(SHARP_AST_ASSIGN, 0, field_line, &[field, value]));
        }
        self.static_type_parameters = None;

        let type_arguments = self.string(0, line, TYPE_ARGUMENTS);
        let parameter = self.node(SHARP_AST_PARAM, 0, line, &[NULL, type_arguments, NULL, NULL, NULL, NULL]);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[parameter]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &writes);

        Some(self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PRIVATE | ZEND_ACC_STATIC,
            name.span,
            name.span,
            STATICS_INIT,
            &[parameters, NULL, body, NULL, NULL],
        ))
    }

    /// The field `name` of the entry of the type arguments `type_arguments` among the statics of the generic PHP# class
    /// `class` that declares it, as `place_use` uses it: `\Sharp\Internal\statics(\App\Counter::class,
    /// <the text of App.Order>)['count']`, which returns the entry by reference, so every write is PHP's own array
    /// write, spec section 11. A throw in a static's initial value that runs as code leaves it out of the entry, so a use
    /// that reads such a static names it, `\Sharp\Internal\statics(\App\Counter::class, <…>, 'made')['made']`, and
    /// the engine throws PHP's own error for a typed static read before it is set, as in a class that isn't generic. A
    /// write alone sets the field.
    fn statics_field(
        &mut self,
        line: u32,
        class: &[u8],
        type_arguments: &str,
        name: &[u8],
        place_use: PlaceUse,
    ) -> u32 {
        let class_name = self.string(ZEND_NAME_FQ, line, class);
        let mut arguments =
            vec![self.node(SHARP_AST_CLASS_NAME, 0, line, &[class_name]), self.type_list_text(line, type_arguments)];
        if place_use == PlaceUse::Read && self.types.has_runtime_initial_value(class, name) {
            arguments.push(self.string(0, line, name));
        }
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);
        let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\statics");
        let entry = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
        let field = self.string(0, line, name);

        self.node(SHARP_AST_DIM, 0, line, &[entry, field])
    }

    /// The type text `$0, $1, …` of the type parameters of the generic class being lowered, which a static member reads
    /// as its own leading type arguments, as a `SHARP_TYPE_OF` list: the text of the type arguments the code runs with.
    fn own_type_arguments(&mut self, line: u32) -> u32 {
        let count = self.types.static_type_parameters(self.class).unwrap_or_else(|| {
            unreachable!("a static member reads its own type arguments only in a generic PHP# class")
        });

        self.type_list_text(line, &type_parameters_text(count))
    }

    /// The text of the type list `text` the code runs with, as the engine spells it, which `SHARP_TYPE_OF` gives for
    /// the whole list.
    fn type_list_text(&mut self, line: u32, text: &str) -> u32 {
        let text = self.type_text(line, text);

        self.node(SHARP_AST_SHARP_TYPE_OF, ZEND_SHARP_TYPE_OF_LIST, line, &[text])
    }

    /// The metadata of the PHP# class-like `class`, which the engine keeps with the class: the type arguments its
    /// header gives each generic parent and interface, the bounds of its type parameters, and their variance, each a
    /// text or null when it has none. None when it has neither a header nor bounds. A generic class also declares from
    /// it the hidden slot that holds each object's type arguments, which start as its bounds.
    fn class_metadata(&mut self, class: &[u8], line: u32) -> Option<u32> {
        let header = self.types.header(class);
        let bounds = self.types.bounds(class);
        if header.is_none() && bounds.is_none() {
            return None;
        }

        let header = header.map_or(NULL, |header| self.string(0, line, header.as_bytes()));
        let bounds = bounds.map_or(NULL, |bounds| self.string(0, line, bounds.as_bytes()));
        let variance = self.types.variance(class).map_or(NULL, |variance| self.string(0, line, variance.as_bytes()));

        Some(self.node(SHARP_AST_SHARP_CLASS_METADATA, 0, line, &[header, bounds, variance]))
    }

    /// A class header's names as PHP's `extends` name and `implements` name list, with the parent's full name: the name
    /// the checker found to be a class is the parent, the interfaces are the list, and a trait is in the class's `use`.
    /// Either is null when the header names none.
    fn class_header(&mut self, inheritance: &Inheritance) -> (u32, u32, Option<&'lowering [u8]>) {
        let mut parent = NULL;
        let mut parent_name = None;
        for name in inheritance.names() {
            let full_name = self.names.get(&name);
            if self.types.class_declaration(full_name).kind == DeclarationKind::Class {
                parent = self.string(ZEND_NAME_FQ, self.line(name), full_name);
                parent_name = Some(full_name);
            }
        }

        (parent, self.name_list(inheritance), parent_name)
    }

    /// A header's interfaces, which PHP compiles as the interface list, or null when it names none. A class's base
    /// class is its parent, and a trait is in its `use`.
    fn name_list(&mut self, inheritance: &Inheritance) -> u32 {
        let mut names = Vec::new();
        for name in inheritance.names() {
            let full_name = self.names.get(&name);
            match self.types.class_declaration(full_name).kind {
                DeclarationKind::Interface => names.push(self.string(ZEND_NAME_FQ, self.line(name), full_name)),
                DeclarationKind::Class | DeclarationKind::Trait => {}
                kind => unreachable!("the checker refuses a {kind:?} in a header"),
            }
        }

        if names.is_empty() {
            return NULL;
        }

        self.node(SHARP_AST_NAME_LIST, 0, self.line(inheritance), &names)
    }

    /// An interface is a class declaration with `ZEND_ACC_INTERFACE` whose methods are `public` and abstract, as
    /// php-src's grammar builds `interface Measured { public function area(): float; }`. Its default bodies are in the
    /// trait `defaults` lowers.
    fn interface(&mut self, interface: &Interface) -> u32 {
        self.class = self.names.get(&interface.name);
        let mut members = Vec::new();
        for member in &interface.members {
            if let ClassLikeMember::Property(Property::Hooked(property)) = member {
                members.push(self.interface_property(property));
                continue;
            }
            if let ClassLikeMember::Property(Property::Computed(computed)) = member {
                members.push(self.defaulted_property(computed.variable.name, computed.span(), NULL));
                continue;
            }
            let ClassLikeMember::Method(method) = member else {
                unreachable!("check_slice refuses the interface member `{member}`");
            };

            members.push(self.signature(method));
        }

        members.extend(self.class_metadata(self.class, self.line(interface.name)));
        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(interface.left_brace), &members);
        let header = interface.inheritance.as_ref().map_or(NULL, |inheritance| self.name_list(inheritance));

        self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_INTERFACE,
            interface.interface.span,
            interface.right_brace,
            interface.name.value,
            &[NULL, header, members, NULL, NULL],
        )
    }

    /// The trait that holds an interface's default bodies, spec section 22, on the interface's lines, as php-src's
    /// grammar builds `trait Defaults { … }`. It is named `HasDesign\Defaults` for the interface `HasDesign`, and the
    /// engine declares it in the file's namespace, so every name in a body resolves as it does in the interface. Each
    /// default body is a method, in source order: a method's is `public`, and a computed property's is
    /// `protected function 'designKey@App\HasDesign'(): string`, which the property of each class that gets it runs. A
    /// generic interface's trait has the header `App.Keyed<$0>`. An interface without a default body has no trait.
    fn defaults(&mut self, interface: &Interface) -> Option<u32> {
        self.class = self.names.get(&interface.name);
        let mut members = Vec::new();
        let interface_name = word(self.class);
        for member in &interface.members {
            match member {
                ClassLikeMember::Method(method) if !matches!(method.body, MethodBody::Abstract(_)) => {
                    members.push(self.method(method, ZEND_ACC_PUBLIC, &[]));
                }
                ClassLikeMember::Property(Property::Computed(computed)) => {
                    let line = self.line(&computed.body);
                    let value = self.accessor_bodies(computed.variable.name, |lowering| {
                        lowering.expression(computed.body.expression)
                    });
                    let value = self.node(SHARP_AST_RETURN, 0, line, &[value]);
                    let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[value]);
                    let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
                    let return_type = computed.hint.as_ref().map_or(NULL, |hint| self.hint(hint));
                    let name = super_alias(interface_name, word(computed.variable.name));
                    members.push(self.declaration(
                        SHARP_AST_METHOD,
                        ZEND_ACC_PROTECTED,
                        computed.span(),
                        &computed.body,
                        &name,
                        &[parameters, NULL, body, return_type, NULL],
                    ));
                }
                _ => {}
            }
        }

        if members.is_empty() {
            return None;
        }

        // A generic interface's trait names the interface with its type parameters as its header, `Demo.Held<$0>`, so
        // the engine counts a body's `$i` among the interface's type arguments the class gives.
        if let Some(parameters) = &interface.type_parameters {
            let line = self.line(interface.name);
            let indexes = type_parameters_text(parameters.parameters.len());
            let header = format!("{}<{indexes}>", interface_name.as_str_lossy().replace('\\', "."));
            let header = self.string(0, line, header.as_bytes());
            members.push(self.node(SHARP_AST_SHARP_CLASS_METADATA, 0, line, &[header, NULL, NULL]));
        }
        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(interface.left_brace), &members);
        let name = defaults_trait_name(interface.name.value);

        Some(self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_TRAIT,
            interface.interface.span,
            interface.right_brace,
            name.as_bytes(),
            &[NULL, NULL, members, NULL, NULL],
        ))
    }

    /// The `use` a class, a struct, an enum or an event starts with, as php-src's grammar builds
    /// `use \Lib\HasSlug, \App\HasDesign\Defaults;`: the traits [`Types::used_traits`] lists, or null when it lists none.
    /// It gives each default body a protected alias, as `\App\HasLabel\Defaults::label as protected 'label@App\HasLabel';`,
    /// which `super<HasLabel>.label()` calls in the class and in each subclass, whose own override replaces only
    /// `label`. The property of each computed property with a default body the class does not declare follows it.
    fn trait_members(&mut self, header: Option<&Inheritance>, span: Span) -> Vec<u32> {
        let line = self.line(span);
        let traits = self.types.used_traits(self.class, header);
        if traits.is_empty() {
            return Vec::new();
        }

        let names: Vec<u32> = traits.iter().map(|name| self.string(ZEND_NAME_FQ, line, name.as_bytes())).collect();
        let names = self.node(SHARP_AST_NAME_LIST, 0, line, &names);
        let mut adaptations = Vec::new();
        for (defaults, interface, method, replacing) in self.types.default_bodies(self.class) {
            let reference = |lowering: &mut Self, defaults: Word| {
                let defaults = lowering.string(ZEND_NAME_FQ, line, defaults.as_bytes());
                let name = lowering.string(0, line, method.as_bytes());

                lowering.node(SHARP_AST_METHOD_REFERENCE, 0, line, &[defaults, name])
            };
            // A sub-interface's body replaces this one, `ShowsMore\Defaults::label insteadof Shows\Defaults`.
            if let Some(replacing) = replacing {
                let winner = reference(self, replacing);
                let replaced = self.string(ZEND_NAME_FQ, line, defaults.as_bytes());
                let replaced = self.node(SHARP_AST_NAME_LIST, 0, line, &[replaced]);
                adaptations.push(self.node(SHARP_AST_TRAIT_PRECEDENCE, 0, line, &[winner, replaced]));
            }
            let reference = reference(self, defaults);
            let alias = self.string(0, line, &super_alias(interface, method));
            adaptations.push(self.node(SHARP_AST_TRAIT_ALIAS, ZEND_ACC_PROTECTED, line, &[reference, alias]));
        }
        let adaptations =
            if adaptations.is_empty() { NULL } else { self.node(SHARP_AST_TRAIT_ADAPTATIONS, 0, line, &adaptations) };
        let mut members = vec![self.node(SHARP_AST_USE_TRAIT, 0, line, &[names, adaptations])];
        for (property, interface) in self.types.default_body_properties(self.class) {
            let name = property.as_bytes().strip_prefix(b"$").unwrap_or(property.as_bytes());
            let this = self.variable(span, b"this");
            let method = self.string(0, line, &super_alias(interface, word(name)));
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
            let call = self.node(SHARP_AST_METHOD_CALL, 0, line, &[this, method, arguments]);
            let body = self.node(SHARP_AST_PROPERTY_HOOK_SHORT_BODY, 0, line, &[call]);
            members.push(self.defaulted_property(name, span, body));
        }

        members
    }

    /// An interface property is `public` with the abstract hook `get`, as php-src's grammar builds
    /// `public int $orderId { get; }`. PHP has no `readonly` interface property, and a `readonly` property implements
    /// it.
    fn interface_property(&mut self, property: &HookedProperty) -> u32 {
        let variable = property.item.variable();
        let line = self.line(variable);
        let hint = property.hint.as_ref().map_or(NULL, |hint| self.hint(hint));
        let name = self.string(0, line, variable.name);
        let hooks = self.abstract_hooks(&property.hook_list);
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, NULL, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);

        self.node(SHARP_AST_PROP_GROUP, ZEND_ACC_PUBLIC, line, &[hint, declaration, NULL])
    }

    /// A computed property with a default body, spec section 22: in its interface, `public mixed $designKey { get; }`,
    /// and in a class that gets the body, `body`, `public mixed $designKey { get => $this->{'designKey@App\HasDesign'}(); }`,
    /// which runs the trait's method that holds it. Its PHP type is `mixed`, since the class's file has no type to
    /// write, so an override, or a plain PHP class, writes the property with its own type, as a `get` hook narrows it,
    /// and the trait's method checks the value the body gives. Its type text, [`Lowering::property_hint`], tells the
    /// engine that an enum may hold it, spec section 31.
    fn defaulted_property(&mut self, name: &[u8], span: Span, body: u32) -> u32 {
        let line = self.line(span);
        let mixed = self.php_type(&PhpType::Mixed, line);
        let hint = self.property_hint(mixed, name, line);
        let name = self.string(0, line, name);
        let hook = self.hook(b"get", span, span, NULL, body);
        let hooks = self.node(SHARP_AST_STMT_LIST, 0, line, &[hook]);
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, NULL, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);

        self.node(SHARP_AST_PROP_GROUP, ZEND_ACC_PUBLIC, line, &[hint, declaration, NULL])
    }

    /// An event is a final class, whose constructor promotes each parameter to a `public readonly` property, with its
    /// header as its interface list, as php-src's grammar builds
    /// `final class OrderPaid implements \App\OrderEvent { public function __construct(public readonly int $orderId) {} }`.
    /// The constructor spans the event's name and parameters.
    fn event(&mut self, event: &Event) -> u32 {
        self.class = self.names.get(&event.name);
        let line = self.line(&event.parameter_list);
        let mut members = self.trait_members(event.inheritance.as_ref(), event.parameter_list.span());
        members.push(self.promoted_constructor(event.name.span, &event.parameter_list));
        members.extend(self.class_metadata(self.class, self.line(event.name)));
        let members = self.node(SHARP_AST_STMT_LIST, 0, line, &members);
        let header = event.inheritance.as_ref().map_or(NULL, |inheritance| self.name_list(inheritance));

        self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_FINAL,
            event.event.span,
            event.terminator,
            event.name.value,
            &[NULL, header, members, NULL, NULL],
        )
    }

    /// The public constructor of an event or a case that carries data, from `name` to the end of `parameters`, which
    /// promotes each parameter to a `public readonly` property and has no statement.
    fn promoted_constructor(&mut self, name: Span, parameter_list: &FunctionLikeParameterList) -> u32 {
        self.enter(b"__construct");
        let mut parameters = Vec::new();
        for parameter in &parameter_list.parameters {
            let index = self.parameter(parameter, None);
            self.nodes[index as usize].attr |= ZEND_ACC_PUBLIC | ZEND_ACC_READONLY;
            parameters.push(index);
        }

        let line = self.line(parameter_list);
        let metadata = self.types.method_metadata(self.class, b"__construct");
        parameters.extend(self.parameter_metadata(metadata, line));
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[]);

        self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC,
            name,
            parameter_list,
            b"__construct",
            &[parameters, NULL, body, NULL, NULL],
        )
    }

    /// Spec section 20: an enum with a case that carries data is an abstract class, with the enum's header as its
    /// interface list, the enum's constants and methods, and the `op_Equality` that compares two of the same case
    /// field by field, as `abstract class PaymentResult implements \App\HasLabel`. Each case is a final subclass named
    /// in the enum's namespace, declared after it, that implements `\Sharp\Struct`, so a `Map` or a `Set` hashes it by
    /// its fields as it hashes a struct. A case that carries data promotes its fields in its constructor, as
    /// `final class Paid extends \App\PaymentResult implements \Sharp\Struct { public function __construct(public readonly string $transactionId) {} }`
    /// in the namespace `App\PaymentResult`. A plain case is a case with no fields, so a copy `unserialize` makes is
    /// equal to it, and its `instance()` makes its one object once and gives it:
    /// `final class Open extends \App\CheckoutState implements \Sharp\Struct { private static $instance;
    /// private function __construct() {} public static function instance(): self { return self::$instance ??= new self(); } }`.
    fn data_enum(&mut self, r#enum: &Enum, statements: &mut Vec<u32>) {
        self.class = self.names.get(&r#enum.name);
        let enum_name = self.class;
        let mut members = Vec::new();
        // A law, spec section 28, is checked and never runs, so the engine gets no node for it.
        for member in &r#enum.members {
            members.push(match member {
                ClassLikeMember::Method(method) => {
                    self.method(method, modifier_flags_or_private(&method.modifiers), &[])
                }
                ClassLikeMember::Constant(constant) => self.constant(constant),
                ClassLikeMember::Property(property) if runs_static_accessors(property) => {
                    members.extend(self.static_accessors(property, None));

                    continue;
                }
                ClassLikeMember::Property(property @ Property::Computed(_)) => self.property(property, None),
                ClassLikeMember::EnumCase(_) | ClassLikeMember::Law(_) => continue,
                _ => unreachable!("check_slice refuses the enum member `{member}`"),
            });
        }
        members.extend(self.class_metadata(self.class, self.line(r#enum.name)));
        let cases: Vec<Vec<u8>> = r#enum
            .members
            .iter()
            .filter_map(|member| match member {
                ClassLikeMember::EnumCase(case) => Some([enum_name, b"\\", case.item.name().value].concat()),
                _ => None,
            })
            .collect();
        let cases: Vec<&[u8]> = cases.iter().map(Vec::as_slice).collect();
        members.push(self.value_equality(r#enum.name.span, &cases));
        members.splice(0..0, self.trait_members(r#enum.inheritance.as_ref(), r#enum.left_brace));

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(r#enum.left_brace), &members);
        let attributes = self.attributes(&r#enum.attribute_lists, None, None);
        let header = r#enum.inheritance.as_ref().map_or(NULL, |inheritance| self.name_list(inheritance));
        statements.push(self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_EXPLICIT_ABSTRACT_CLASS,
            r#enum.r#enum.span,
            r#enum.right_brace,
            r#enum.name.value,
            &[NULL, header, members, attributes, NULL],
        ));

        for member in &r#enum.members {
            let ClassLikeMember::EnumCase(case) = member else {
                continue;
            };
            let name = case.item.name();
            let line = self.line(name.span);
            let case_class = [enum_name, b"\\", name.value].concat();
            let mut members = match &case.item {
                EnumCaseItem::Data(item) => vec![self.promoted_constructor(name.span, &item.parameter_list)],
                EnumCaseItem::Unit(_) | EnumCaseItem::Backed(_) => self.case_object(&case_class, name.span),
            };
            // A case of a generic enum passes its type arguments to the enum, as `Ok<T, TError>` to `Result<T, TError>`.
            members.extend(self.class_metadata(&case_class, line));
            let members = self.node(SHARP_AST_STMT_LIST, 0, line, &members);
            let marker = self.string(ZEND_NAME_FQ, line, b"Sharp\\Struct");
            let header = self.node(SHARP_AST_NAME_LIST, 0, line, &[marker]);
            let parent = self.string(ZEND_NAME_FQ, line, enum_name);
            let attributes = self.attributes(&case.attribute_lists, None, None);
            let class_name = [r#enum.name.value, b"\\", name.value].concat();

            statements.push(self.declaration(
                SHARP_AST_CLASS,
                ZEND_ACC_FINAL,
                case.case.span,
                case.terminator.span(),
                &class_name,
                &[parent, header, members, attributes, NULL],
            ));
        }
    }

    /// The members of the class `class` of a plain case of an enum that runs as classes, at `name`: the private static
    /// property `instance`, a private constructor, and the public static `instance()`, which makes the case's one
    /// object on the first call and gives it. A case of a generic enum has one object for each list of type arguments,
    /// spec section 31, which it keeps as a static field of its own, spec section 11: `instance()` takes them as its
    /// leading own type arguments, as any static method of a generic class does, and makes the object with them,
    /// `\Sharp\Internal\statics(Case::class, '#0')['instance'] ??= new self<#0>()`.
    fn case_object(&mut self, class: &[u8], name: Span) -> Vec<u32> {
        self.enter(b"instance");
        let line = self.line(name);
        let type_arguments = self
            .types
            .static_type_parameters(class)
            .map(|count| (0..count).map(|index| format!("#{index}")).collect::<Vec<_>>().join(", "));
        let (property, instance) = match &type_arguments {
            Some(type_arguments) => {
                let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
                let property_name = self.string(0, line, b"instance");
                let field = self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[null, property_name]);
                let property = self.statics_property(line, &[field]);

                let declaring = self.string(ZEND_NAME_FQ, line, class);
                let declaring = self.node(SHARP_AST_CLASS_NAME, 0, line, &[declaring]);
                let text = self.type_text(line, type_arguments);
                let key = self.node(SHARP_AST_SHARP_TYPE_OF, ZEND_SHARP_TYPE_OF_LIST, line, &[text]);
                let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\statics");
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[declaring, key]);
                let entry = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
                let property_name = self.string(0, line, b"instance");

                (property, self.node(SHARP_AST_DIM, 0, line, &[entry, property_name]))
            }
            None => {
                let property_name = self.string(0, line, b"instance");
                let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[property_name, NULL, NULL, NULL]);
                let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
                let property = self.node(
                    SHARP_AST_PROP_GROUP,
                    ZEND_ACC_PRIVATE | ZEND_ACC_STATIC,
                    line,
                    &[NULL, declaration, NULL],
                );
                let class = self.string(ZEND_NAME_NOT_FQ, line, b"self");
                let property_name = self.string(0, line, b"instance");

                (property, self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, property_name]))
            }
        };

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[]);
        let constructor = self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PRIVATE,
            name,
            name,
            b"__construct",
            &[parameters, NULL, body, NULL, NULL],
        );

        let metadata = self.types.method_metadata(class, b"instance");
        let class = self.string(ZEND_NAME_NOT_FQ, line, b"self");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
        let object = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);
        let object = self.with_type_arguments(line, object, type_arguments);
        let made = self.node(SHARP_AST_ASSIGN_COALESCE, 0, line, &[instance, object]);
        let returned = self.node(SHARP_AST_RETURN, 0, line, &[made]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[returned]);
        // A generic case's `instance()` receives its class's type arguments, as any static method of a generic class.
        let metadata: Vec<u32> = self.parameter_metadata(metadata, line).into_iter().collect();
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &metadata);
        let return_type = self.string(ZEND_NAME_NOT_FQ, line, b"self");
        let instance = self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PUBLIC | ZEND_ACC_STATIC,
            name,
            name,
            b"instance",
            &[parameters, NULL, body, return_type, NULL],
        );

        vec![property, constructor, instance]
    }

    /// An enum is a final class with the enum flag, its header as its interface list and its backing type as its last
    /// child, as php-src's grammar builds `enum Status: string implements HasLabel`. An enum has no parent, so its
    /// header needs no mark.
    fn r#enum(&mut self, r#enum: &Enum) -> u32 {
        self.class = self.names.get(&r#enum.name);
        let mut members = Vec::new();
        // A law, spec section 28, is checked and never runs, so the engine gets no node for it.
        for member in r#enum.members.iter().filter(|member| !matches!(member, ClassLikeMember::Law(_))) {
            members.push(match member {
                ClassLikeMember::Method(method) => {
                    self.method(method, modifier_flags_or_private(&method.modifiers), &[])
                }
                ClassLikeMember::EnumCase(case) => self.enum_case(case),
                ClassLikeMember::Constant(constant) => self.constant(constant),
                ClassLikeMember::Property(property) if runs_static_accessors(property) => {
                    members.extend(self.static_accessors(property, None));

                    continue;
                }
                // Spec section 31: a computed property has no storage, which a PHP# enum's property may lack.
                ClassLikeMember::Property(property @ Property::Computed(_)) => self.property(property, None),
                _ => unreachable!("check_slice refuses the enum member `{member}`"),
            });
        }

        members.extend(self.class_metadata(self.class, self.line(r#enum.name)));
        members.splice(0..0, self.trait_members(r#enum.inheritance.as_ref(), r#enum.left_brace));
        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(r#enum.left_brace), &members);
        let attributes = self.attributes(&r#enum.attribute_lists, None, None);
        let header = r#enum.inheritance.as_ref().map_or(NULL, |inheritance| self.name_list(inheritance));
        let backing_type = r#enum.backing_type_hint.as_ref().map_or(NULL, |backing_type| self.hint(&backing_type.hint));

        self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_ENUM | ZEND_ACC_FINAL,
            r#enum.r#enum.span,
            r#enum.right_brace,
            r#enum.name.value,
            &[NULL, header, members, attributes, backing_type],
        )
    }

    /// A case is its name, its value or null, a null doc comment and its attributes, on the line of its name, as
    /// php-src's grammar builds `case Active = "active";`. A value is a constant expression.
    fn enum_case(&mut self, case: &EnumCase) -> u32 {
        let name = case.item.name();
        let line = self.line(name.span);
        let name = self.string(0, line, name.value);
        let value = match &case.item {
            EnumCaseItem::Unit(_) => NULL,
            EnumCaseItem::Backed(item) => self.expression(item.value),
            EnumCaseItem::Data(_) => unreachable!("`data_enum` lowers an enum with a case that carries data"),
        };
        let attributes = self.attributes(&case.attribute_lists, None, None);

        self.node(SHARP_AST_ENUM_CASE, 0, line, &[name, value, NULL, attributes])
    }

    /// A method is a `function` with its return type after its parameters, and with `flags`. Its first line is where
    /// PHP writes `function`: the return type, or the name of the constructor, which runs as `__construct` and has
    /// no return type. Its body starts with `first_statements`: the class's initial values that are not constant in
    /// the constructor, or the call of an `extern` method's native function, which is that method's whole body. Any
    /// other abstract method has no statement list. A method with statements makes each `Set` parameter a `Set` before
    /// them. A constructor's `: super(…)` runs after them as `parent::__construct(…)`, and does nothing when no
    /// constructor exists above the class.
    fn method(&mut self, method: &Method, flags: u32, first_statements: &[u32]) -> u32 {
        self.method_declaration(method, flags, Some(first_statements))
    }

    /// An interface's method is `public` with no statement list, as php-src's grammar builds
    /// `public function designKey(): string;`, spec section 22. Its default body is in the trait `defaults` lowers.
    fn signature(&mut self, method: &Method) -> u32 {
        self.method_declaration(method, ZEND_ACC_PUBLIC, None)
    }

    /// A method with `flags`, whose body starts with `first_statements`, or which has no body when they are `None`.
    fn method_declaration(&mut self, method: &Method, flags: u32, first_statements: Option<&[u32]>) -> u32 {
        self.enter(method.name.value);
        if flags & ZEND_ACC_STATIC != 0 {
            self.static_type_parameters = self.types.static_type_parameters(self.class);
        }
        // Spec section 32: the container builds a class through its main constructor.
        let services =
            if php_method_name(method) == b"__construct" { self.types.constructor_parameters(self.class) } else { &[] };
        let mut parameters = Vec::new();
        for (index, parameter) in method.parameter_list.parameters.iter().enumerate() {
            parameters.push(self.parameter(parameter, services.get(index)));
        }
        let line = self.line(&method.parameter_list);
        let metadata = self.types.method_metadata(self.class, php_method_name(method));
        parameters.extend(self.parameter_metadata(metadata, line));
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let body = match first_statements {
            None => NULL,
            Some(first_statements) => {
                let mut statements = if matches!(method.body, MethodBody::Abstract(_)) && first_statements.is_empty() {
                    Vec::new()
                } else {
                    self.collection_parameters(&method.parameter_list.parameters)
                };
                statements.extend_from_slice(first_statements);
                if let Some(initializer) = &method.initializer
                    && self.types.parent_constructor(self.class).is_some()
                {
                    let parameters = self.types.parent_constructor_parameters(self.class);
                    let callee =
                        self.types.parent_constructor(self.class).filter(|constructor| constructor.flags.is_sharp());
                    let line = self.line(initializer);
                    let construct = self.call(
                        Some(initializer.keyword.span),
                        &initializer.argument_list,
                        None,
                        parameters,
                        callee,
                        |this, arguments| this.parent_construct(line, arguments),
                    );
                    statements.push(construct);
                }
                self.returns = method.return_type_hint.as_ref().map(|return_type| self.hint_type(&return_type.hint));
                let body = self.method_body(&method.body, method.returns_value(), statements);
                self.returns = None;

                body
            }
        };
        let (start, return_type) = match &method.return_type_hint {
            Some(return_type_hint) => (return_type_hint.span(), self.hint(&return_type_hint.hint)),
            None => (method.name.span, NULL),
        };
        // Spec section 14.3: a method named without parentheses is a function value, a closure of the method.
        let return_type = if php_method_name(method) == b"__construct" {
            return_type
        } else {
            let function_type = self.types.method_type(self.class, php_method_name(method));

            self.function_type(return_type, function_type, self.line(start))
        };
        self.static_type_parameters = None;
        // A sub-interface's body that replaces its parent's default body runs as a method of its trait of default
        // bodies, which has no parent, so only the interface's signature carries `#[\Override]`.
        let r#override =
            method.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Override(_))).filter(|_| {
                first_statements.is_none()
                    || self.types.class_declaration(self.class).kind != DeclarationKind::Interface
            });
        let attributes = self.attributes(&method.attribute_lists, r#override, None);

        self.declaration(
            SHARP_AST_METHOD,
            flags | generator_flag(&method.body),
            start,
            method.body.span(),
            php_method_name(method),
            &[parameters, NULL, body, return_type, attributes],
        )
    }

    /// The metadata that ends the parameter list of a function a call gives type arguments, or whose argument a call
    /// from plain PHP is checked against: `metadata`, its bounds and the type each parameter is checked against, as
    /// [`Types::method_metadata`] gives them. None when it has neither.
    fn parameter_metadata(&mut self, metadata: (Option<String>, Option<String>), line: u32) -> Option<u32> {
        let (bounds, checks) = metadata;
        if bounds.is_none() && checks.is_none() {
            return None;
        }

        // A static method's reads its class's type parameters as its own leading ones, as its checks do.
        let bounds = bounds.map_or(NULL, |bounds| self.type_text(line, &bounds));
        let checks = checks.map_or(NULL, |checks| self.type_text(line, &checks));

        Some(self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[bounds, checks]))
    }

    /// `parent::__construct(…)` with `arguments`, the call `: super(…)` runs as, a static call of `parent` as
    /// `super.m()`'s is.
    fn parent_construct(&mut self, line: u32, arguments: u32) -> u32 {
        let class = self.string(ZEND_NAME_NOT_FQ, line, b"parent");
        let method = self.string(0, line, b"__construct");

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments])
    }

    /// A method's statement list: `statements`, then its block's statements, or its expression body, which it returns
    /// when it `returns_value`. An abstract method without `statements` has none.
    fn method_body(&mut self, body: &MethodBody, returns_value: bool, mut statements: Vec<u32>) -> u32 {
        match body {
            MethodBody::Concrete(block) => {
                for statement in &block.statements {
                    statements.push(self.statement(statement));
                }

                self.node(SHARP_AST_STMT_LIST, 0, self.line(block), &statements)
            }
            MethodBody::Expression(body) => {
                let line = self.line(body);
                statements.push(if returns_value {
                    let returns = self.returns.clone();
                    let expression = self.crossed_value(body.expression, returns.as_ref());

                    self.node(SHARP_AST_RETURN, 0, line, &[expression])
                } else {
                    self.expression(body.expression)
                });

                self.node(SHARP_AST_STMT_LIST, 0, line, &statements)
            }
            MethodBody::Abstract(_) if statements.is_empty() => NULL,
            MethodBody::Abstract(body) => self.node(SHARP_AST_STMT_LIST, 0, self.line(body), &statements),
        }
    }

    /// An operator is the public static method it runs as, named after .NET's operator method, as php-src's grammar
    /// builds `public static function op_Addition(\App\Money $a, \App\Money $b): \App\Money`. `==` is lifted over null,
    /// as C# lifts it: its parameters are nullable, and its body starts with
    /// `if ($a === null || $b === null) { return $a === $b; }`, so null equals only null. Its body makes each `Set`
    /// parameter a `Set` before that, as a method's does.
    fn operator(&mut self, operator: &Operator) -> u32 {
        let Some(name) = php_operator_name(&operator.symbol, operator.parameter_list.parameters.len()) else {
            unreachable!("check_slice refuses the operator `{}`", operator.symbol);
        };
        let lifted = matches!(operator.symbol, BinaryOperator::Equal(_));

        self.enter(name);
        let mut parameters = Vec::new();
        for parameter in &operator.parameter_list.parameters {
            parameters.push(match &parameter.hint {
                Some(hint) if lifted => {
                    let hint = self.nullable_hint(hint);

                    self.parameter_of_type(parameter, hint, None)
                }
                _ => self.parameter(parameter, None),
            });
        }

        let line = self.line(&operator.parameter_list);
        let metadata = self.types.method_metadata(self.class, name);
        parameters.extend(self.parameter_metadata(metadata, line));
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let mut prologue = self.collection_parameters(&operator.parameter_list.parameters);
        if lifted {
            prologue.push(self.null_lifting(&operator.parameter_list));
        }
        let body = self.method_body(&operator.body, true, prologue);
        let return_type = self.hint(&operator.return_type_hint.hint);
        let attributes = self.attributes(&operator.attribute_lists, None, None);

        self.declaration(
            SHARP_AST_METHOD,
            modifier_flags(&operator.modifiers),
            &operator.return_type_hint,
            operator.body.span(),
            name,
            &[parameters, NULL, body, return_type, attributes],
        )
    }

    /// An extension's member is the public static method it runs as, spec section 26, which takes the receiver first:
    /// `public string slug(string separator)` in `extension(string value)` runs as php-src's grammar builds
    /// `public static function slug(string $value, string $separator): string`. A computed property is the one that
    /// takes only the receiver and returns its expression: `public bool isBlank => …;` runs as
    /// `public static function isBlank(string $value): bool { return …; }`.
    fn extension_member(&mut self, extension: &Extension, member: &ClassLikeMember) -> u32 {
        let [receiver] = extension.parameter_list.parameters.as_slice() else {
            unreachable!("semantics refuses an extension without exactly one receiver");
        };

        match member {
            ClassLikeMember::Method(method) => {
                self.enter(method.name.value);
                let (parameters, statements) = self.extension_parameters(
                    method.name.value,
                    std::iter::once(receiver).chain(&method.parameter_list.parameters),
                );
                let body = self.method_body(&method.body, method.returns_value(), statements);
                let (start, return_type) = match &method.return_type_hint {
                    Some(return_type_hint) => (return_type_hint.span(), self.hint(&return_type_hint.hint)),
                    None => unreachable!("semantics refuses an extension method without a return type"),
                };
                // An extension member carries its result type, which a generator's elements are read from, spec
                // section 12, and no function type, since no function value names it.
                let result = self.types.method_result(self.class, method.name.value);
                let return_type = self.carried_type(return_type, result, self.line(start));
                let attributes = self.attributes(&method.attribute_lists, None, None);

                self.declaration(
                    SHARP_AST_METHOD,
                    modifier_flags(&method.modifiers) | ZEND_ACC_STATIC | generator_flag(&method.body),
                    start,
                    method.body.span(),
                    method.name.value,
                    &[parameters, NULL, body, return_type, attributes],
                )
            }
            ClassLikeMember::Property(Property::Computed(property)) => {
                let Some(hint) = &property.hint else {
                    unreachable!("semantics refuses a computed property without a type");
                };
                self.enter(property.variable.name);
                let (parameters, mut statements) = self.extension_parameters(property.variable.name, [receiver]);
                let line = self.line(&property.body);
                let value = self.expression(property.body.expression);
                statements.push(self.node(SHARP_AST_RETURN, 0, line, &[value]));
                let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
                let return_type = self.hint(hint);
                let attributes = self.attributes(&property.attribute_lists, None, None);

                self.declaration(
                    SHARP_AST_METHOD,
                    modifier_flags(&property.modifiers) | ZEND_ACC_STATIC,
                    hint,
                    &property.body,
                    property.variable.name,
                    &[parameters, NULL, body, return_type, attributes],
                )
            }
            _ => unreachable!("semantics refuses the extension member `{member}`"),
        }
    }

    /// The parameter list of the extension member `name`, the receiver first, ending with the metadata a call from
    /// plain PHP is checked against, as a method's does, with the statements that make each `Map` or `Set` a PHP array
    /// gives the object, as a method's body starts with.
    fn extension_parameters<'parameter, 'ast>(
        &mut self,
        name: &[u8],
        parameters: impl IntoIterator<Item = &'parameter FunctionLikeParameter<'ast>> + Clone,
    ) -> (u32, Vec<u32>)
    where
        'ast: 'parameter,
    {
        let mut lowered = Vec::new();
        let mut line = None;
        for parameter in parameters.clone() {
            line.get_or_insert(self.line(parameter));
            lowered.push(self.parameter(parameter, None));
        }
        let Some(line) = line else {
            unreachable!("an extension's member takes its receiver");
        };
        let metadata = self.types.method_metadata(self.class, name);
        lowered.extend(self.parameter_metadata(metadata, line));

        (self.node(SHARP_AST_PARAM_LIST, 0, line, &lowered), self.collection_parameters(parameters))
    }

    /// A named constructor is the static method it runs as, spec section 9.1, as php-src's grammar builds
    /// `public static function fromJson(string $json): \Demo\Artifact { ${'this#1'} = new \Demo\Artifact(…);
    /// \Sharp\Internal\build(${'this#1'}, function ($json) { … }, $json); return ${'this#1'}; }`: its `: this(…)`
    /// creates the object with the main constructor, and its body is a closure that `\Sharp\Internal\build` binds to
    /// that object, as `Closure::call` does, so it runs as a method body on a real `$this`, where `super.m()` and
    /// `return;` run as they do in a method. The object is still being built while the body runs, so its writes run no
    /// observer, spec section 6.2. A generic class's static method takes the class's type arguments as its own, ends
    /// its parameter list with their bounds, and gives them to the object.
    fn named_constructor(&mut self, constructor: &NamedConstructor, initializer: &ConstructorInitializer) -> u32 {
        self.enter(constructor.name.value);
        self.static_type_parameters = self.types.static_type_parameters(self.class);
        let mut parameters = Vec::new();
        for parameter in &constructor.parameter_list.parameters {
            parameters.push(self.parameter(parameter, None));
        }
        let line = self.line(&constructor.parameter_list);
        let metadata = self.types.method_metadata(self.class, constructor.name.value);
        parameters.extend(self.parameter_metadata(metadata, line));
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let mut statements = self.collection_parameters(&constructor.parameter_list.parameters);
        let line = self.line(initializer);
        let object = self.variable(initializer.keyword.span, b"this#1");
        let class = self.string(ZEND_NAME_FQ, line, self.class);
        let callee = self.types.sharp_method(self.class, b"__construct");
        let arguments = self.call(
            Some(initializer.keyword.span),
            &initializer.argument_list,
            None,
            None,
            callee,
            |_, arguments| arguments,
        );
        let mut new = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);
        if let Some(count) = self.static_type_parameters {
            let own = self.type_text(line, &type_parameters_text(count));
            new = self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[new, own]);
        }
        statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[object, new]));
        self.static_type_parameters = None;

        // The closure takes the method's parameters untyped, as the method already checked and converted each one.
        let line = self.line(&constructor.body);
        let mut closure_parameters = Vec::new();
        let mut arguments = vec![self.variable(constructor.body.span(), b"this#1")];
        for parameter in &constructor.parameter_list.parameters {
            let parameter_line = self.line(parameter);
            let name = self.local_name(0, parameter_line, parameter.variable.name);
            let variadic_flag = if parameter.is_variadic() { ZEND_PARAM_VARIADIC } else { 0 };
            closure_parameters.push(self.node(
                SHARP_AST_PARAM,
                variadic_flag,
                parameter_line,
                &[NULL, name, NULL, NULL, NULL, NULL],
            ));

            let argument = self.variable(parameter.variable.span, parameter.variable.name);
            arguments.push(if parameter.is_variadic() {
                self.node(SHARP_AST_UNPACK, 0, parameter_line, &[argument])
            } else {
                argument
            });
        }

        let closure_parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &closure_parameters);
        let body = self.method_body(&constructor.body, false, Vec::new());
        let closure = self.declaration(
            SHARP_AST_CLOSURE,
            0,
            &constructor.body,
            &constructor.body,
            b"",
            &[closure_parameters, NULL, body, NULL, NULL],
        );
        arguments.insert(1, closure);
        statements.push(self.global_call(line, b"Sharp\\Internal\\build", &arguments));

        let end = self.lines.line(constructor.body.span().end.offset);
        let object = self.variable(constructor.body.span(), b"this#1");
        statements.push(self.node(SHARP_AST_RETURN, 0, end, &[object]));
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let return_type = self.string(ZEND_NAME_FQ, self.line(constructor.class), self.class);
        let attributes = self.attributes(&constructor.attribute_lists, None, None);

        self.declaration(
            SHARP_AST_METHOD,
            modifier_flags_or_private(&constructor.modifiers) | ZEND_ACC_STATIC,
            constructor.class,
            constructor.body.span(),
            constructor.name.value,
            &[parameters, NULL, body, return_type, attributes],
        )
    }

    /// `new` on line `line`: `NEW` of the class by its full name, and `new Self(…)` is `new static(…)`, the name
    /// php-src's grammar writes with `ZEND_NAME_NOT_FQ`. `new Artifact.fromJson(…)` calls the static method the named
    /// constructor runs as, spec section 9.1. An object initializer then sets its values on the new object, spec
    /// section 16, and `new Point { x: 1 }` creates it with no arguments.
    ///
    /// A PHP# generic class's `new` carries the type arguments the checker found in a `SHARP_TYPE_ARGS` over the `NEW`,
    /// and its named constructor takes them after its arguments, as a generic call does. `new TItem(…)` creates the type
    /// argument the code runs with: a `NEW` that names no class, under the `SHARP_TYPE_ARGS` that spells the type. `new
    /// (type)(…)` creates an object from the class value `type`, spec section 25, under the type arguments it names,
    /// or with those the value carries. In a generic class `new Self(…)` gives the new object `this`'s type arguments: a
    /// `SHARP_TYPE_ARGS` without a text.
    fn instantiation(&mut self, line: u32, instantiation: &Instantiation) -> u32 {
        let class_name = match instantiation.class {
            Expression::Identifier(class) if !self.names.is_type_parameter(class) => Some(self.names.get(class)),
            _ => None,
        };
        let type_arguments = class_name.and_then(|name| self.types.type_arguments(name, instantiation.span()));
        let class_value = match instantiation.class {
            Expression::Parenthesized(value) => Some(self.expression(value.expression)),
            _ => None,
        };
        let arguments = match &instantiation.argument_list {
            Some(arguments) => match (class_name, &instantiation.constructor) {
                (Some(name), None) => {
                    let parameters = self.types.new_parameters(name);
                    let callee = self.types.sharp_method(name, b"__construct");

                    self.call(Some(instantiation.span()), arguments, None, parameters, callee, |_, arguments| arguments)
                }
                (_, Some(constructor)) => {
                    let callee = class_name.and_then(|name| self.types.sharp_method(name, constructor.name.value));

                    self.call(
                        Some(instantiation.span()),
                        arguments,
                        type_arguments.clone(),
                        None,
                        callee,
                        |_, arguments| arguments,
                    )
                }
                (None, None) => self.arguments(arguments),
            },
            None => self.node(SHARP_AST_ARG_LIST, 0, line, &[]),
        };
        let created = match (instantiation.class, &instantiation.constructor) {
            (Expression::Identifier(class), Some(constructor)) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));
                let method = self.string(0, self.line(constructor), constructor.name.value);

                self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments])
            }
            (Expression::Identifier(class), None) if class_name.is_none() => {
                let tested = self
                    .types
                    .tested_type(class)
                    .unwrap_or_else(|| unreachable!("a type parameter's type is recorded"));
                let new = self.node(SHARP_AST_NEW, 0, line, &[NULL, arguments]);
                let tested = self.type_text(line, &tested);

                self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[new, tested])
            }
            (Expression::Identifier(class), None) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));
                let new = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);

                self.with_type_arguments(line, new, type_arguments)
            }
            (Expression::Parenthesized(value), None) => {
                let class = class_value.unwrap_or_else(|| unreachable!("a parenthesized class is lowered above"));
                let type_arguments = instantiation.type_arguments.as_ref().and_then(|_| {
                    let classes = class_value_classes(self.types.expression_type(value.expression))?;

                    self.types.type_arguments(classes.first()?, instantiation.span())
                });

                match type_arguments {
                    Some(type_arguments) => {
                        let new = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);

                        self.with_type_arguments(line, new, Some(type_arguments))
                    }
                    None => self.node(SHARP_AST_NEW, ZEND_SHARP_NEW_CLASS_VALUE, line, &[class, arguments]),
                }
            }
            (Expression::Self_(keyword), _) => {
                let class = self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"static");
                let new = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);

                if self.types.bounds(self.class).is_some() {
                    self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[new, NULL])
                } else {
                    new
                }
            }
            _ => unreachable!("check_slice refuses `new` of a class written any other way"),
        };
        let Some(initializer) = &instantiation.initializer else {
            return created;
        };

        // `\Sharp\Internal\build(new \App\Point(), function ($value#1) { $this->x = $value#1; return $this; }, 1)`: the
        // object is built once the initializer ends, so its writes run no observer, spec section 6.2.
        self.bound_to(instantiation.span(), created, initializer, true, |lowering, span, values| {
            let line = lowering.line(span);
            let mut statements = Vec::new();
            for (name, value) in values {
                let this = lowering.variable(span, b"this");
                let name = lowering.string(0, line, name);
                let property = lowering.node(SHARP_AST_PROP, 0, line, &[this, name]);
                statements.push(lowering.node(SHARP_AST_ASSIGN, 0, line, &[property, value]));
            }
            let this = lowering.variable(span, b"this");
            statements.push(lowering.node(SHARP_AST_RETURN, 0, line, &[this]));

            statements
        })
    }

    /// The `NEW` node `new` under the `SHARP_TYPE_ARGS` that spells `type_arguments`, or `new` alone without them.
    fn with_type_arguments(&mut self, line: u32, new: u32, type_arguments: Option<String>) -> u32 {
        let Some(type_arguments) = type_arguments else {
            return new;
        };
        let text = self.type_text(line, &type_arguments);

        self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[new, text])
    }

    /// `point with { y: 9 }` copies the object, spec section 16, as PHP 8.5's `clone` with values does:
    /// `(function ($value#1) { return \clone($this, ['y' => $value#1]); })->call($point, 9)`. `clone` writes each
    /// value through the property's accessor, after the copy's `__clone`, while the engine marks the copy as still
    /// being built, so the writes run no observer, spec section 6.2.
    fn with(&mut self, with: &With) -> u32 {
        let object = self.expression(with.object);

        self.bound_to(with.span(), object, &with.initializer, false, |lowering, span, values| {
            let line = lowering.line(span);
            let mut entries = Vec::new();
            for (name, value) in values {
                let key = lowering.string(0, line, name);
                entries.push(lowering.node(SHARP_AST_ARRAY_ELEM, 0, line, &[value, key]));
            }
            let this = lowering.variable(span, b"this");
            let values = lowering.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &entries);
            let function = lowering.string(ZEND_NAME_FQ, line, b"clone");
            let arguments = lowering.node(SHARP_AST_ARG_LIST, 0, line, &[this, values]);
            let copy = lowering.node(SHARP_AST_CALL, 0, line, &[function, arguments]);

            vec![lowering.node(SHARP_AST_RETURN, 0, line, &[copy])]
        })
    }

    /// Runs the statements `body` writes, as a closure bound to `object`, the object `new` or `with` at `span` creates,
    /// spec section 16: `(function ($value#1, …) { … })->call($object, …)`, or, when the closure `builds` the object,
    /// `\Sharp\Internal\build($object, function ($value#1, …) { … }, …)`, which binds it the same way while the object
    /// is still being built, so its writes run no observer, spec section 6.2. The closure runs with the object's
    /// class's own access, so a write runs an `init` or `set` accessor wherever the code is, as a named constructor's
    /// body runs (`Lowering::named_constructor`). Each value of `initializer` runs in the caller, in order after
    /// `object`, as an argument, and `body` reads it through the hidden parameter it gets beside the property's name.
    fn bound_to(
        &mut self,
        span: Span,
        object: u32,
        initializer: &ObjectInitializer,
        builds: bool,
        body: impl FnOnce(&mut Self, Span, Vec<(&[u8], u32)>) -> Vec<u32>,
    ) -> u32 {
        let line = self.line(span);
        let temporaries = self.temporaries;
        let mut parameters = Vec::new();
        let mut arguments = vec![object];
        let mut values = Vec::new();
        for value in initializer.values.iter() {
            arguments.push(self.expression(value.value));

            self.temporaries += 1;
            let hidden = format!("value#{}", self.temporaries).into_bytes();
            let name = self.string(0, line, &hidden);
            parameters.push(self.node(SHARP_AST_PARAM, 0, line, &[NULL, name, NULL, NULL, NULL, NULL]));
            values.push((value.name.value, self.variable(span, &hidden)));
        }
        self.temporaries = temporaries;

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let statements = body(self, span, values);
        let statements = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let closure =
            self.declaration(SHARP_AST_CLOSURE, 0, span, span, b"", &[parameters, NULL, statements, NULL, NULL]);
        if builds {
            arguments.insert(1, closure);
            return self.global_call(line, b"Sharp\\Internal\\build", &arguments);
        }
        let call = self.string(0, line, b"call");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);

        self.node(SHARP_AST_METHOD_CALL, 0, line, &[closure, call, arguments])
    }

    /// A type that also holds null: `?T`, as php-src's grammar builds `?\App\Money`, a union with `null` last, or the
    /// type itself when it holds null already, as `T?` and `Any` do.
    fn nullable_hint(&mut self, hint: &Hint) -> u32 {
        match hint {
            Hint::Nullable(_) | Hint::Mixed(_) => self.hint(hint),
            Hint::Union(_) => self.union(hint, Some(hint.span())),
            _ => {
                let index = self.hint(hint);
                self.nodes[index as usize].attr |= ZEND_TYPE_NULLABLE;

                index
            }
        }
    }

    /// `if ($a === null || $b === null) { return $a === $b; }` over the two parameters of `operator ==`, on the line of
    /// its parameter list.
    fn null_lifting(&mut self, parameters: &FunctionLikeParameterList) -> u32 {
        let [a, b] = parameters.parameters.as_slice() else {
            unreachable!("check_slice refuses an `operator ==` without two parameters");
        };
        let line = self.line(parameters);

        let a_value = self.variable(a.variable.span, a.variable.name);
        let a_is_null = self.is_null(line, a_value);
        let b_value = self.variable(b.variable.span, b.variable.name);
        let b_is_null = self.is_null(line, b_value);
        let either_is_null = self.node(SHARP_AST_OR, 0, line, &[a_is_null, b_is_null]);

        let a_value = self.variable(a.variable.span, a.variable.name);
        let b_value = self.variable(b.variable.span, b.variable.name);
        let both_are_null = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[a_value, b_value]);
        let r#return = self.node(SHARP_AST_RETURN, 0, line, &[both_are_null]);
        let statements = self.node(SHARP_AST_STMT_LIST, 0, line, &[r#return]);
        let branch = self.node(SHARP_AST_IF_ELEM, 0, line, &[either_is_null, statements]);

        self.node(SHARP_AST_IF, 0, line, &[branch])
    }

    /// The call an `extern` method's body runs, as php-src's grammar builds `\Sharp\Internal\Text\Text\slug($title)`:
    /// a call of the native function the engine registers under `Sharp\Internal`, then the class's full name after
    /// `Sharp\`, then the method's name. It passes each parameter on, a variadic one as a spread. The method returns it
    /// unless it is `void`, and a caller inlines it as the method's form.
    fn native_call(&mut self, method: &Method, class: &[u8]) -> u32 {
        let Some(class_in_library) = class
            .split_at_checked(b"Sharp\\".len())
            .and_then(|(root, rest)| root.eq_ignore_ascii_case(b"Sharp\\").then_some(rest))
        else {
            unreachable!("the checker refuses an `extern` method outside the standard library's `Sharp` classes");
        };
        let line = self.line(method.name.span);
        let function = [b"Sharp\\Internal\\".as_slice(), class_in_library, b"\\", method.name.value].concat();
        let function = self.string(ZEND_NAME_FQ, line, &function);

        let mut arguments = Vec::new();
        for parameter in &method.parameter_list.parameters {
            let value = self.variable(parameter.variable.span, parameter.variable.name);
            arguments.push(if parameter.is_variadic() {
                self.node(SHARP_AST_UNPACK, 0, line, &[value])
            } else {
                value
            });
        }

        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);

        self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
    }

    /// A variadic parameter carries `ZEND_PARAM_VARIADIC`, as php-src's grammar builds `int ...$values`, and a widened
    /// one `ZEND_PARAM_WIDENED` (`Types::is_widened`). A parameter the container fills, whose type is `service`,
    /// carries the service's id when its PHP type drops it.
    fn parameter(&mut self, parameter: &FunctionLikeParameter, service: Option<&FunctionLikeParameterMetadata>) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("semantics refuses a method parameter without a type");
        };
        let hint_index = self.hint(hint);
        let service = service.and_then(|service| dropped_service(hint, service));
        let node = self.parameter_of_type(parameter, hint_index, service);
        if self.types.is_widened(parameter.variable.span) {
            self.nodes[node as usize].attr |= ZEND_PARAM_WIDENED;
        }

        node
    }

    /// A parameter with its lowered type, or with a null type, as a lambda's parameter can be, and with the id of the
    /// service the container gives it when its PHP type drops it. A `Map` or a `Set` parameter also takes the PHP array
    /// plain PHP passes, `\Sharp\Map|array`, which [`Self::collection_parameters`] turns into the object. A promoted one
    /// is a plain parameter, beside the property [`Self::promoted_property`] declares, when [`Self::promoted_beside`]
    /// holds.
    fn parameter_of_type(&mut self, parameter: &FunctionLikeParameter, hint: u32, service: Option<Word>) -> u32 {
        let hint = match collection_hint(parameter.hint.as_ref()) {
            Some(nullable) if hint != NULL => self.table_or_array(hint, nullable),
            _ => hint,
        };
        let line = self.line(parameter.variable.span);
        let name = self.local_name(0, line, parameter.variable.name);
        let default = parameter.default_value.as_ref().map_or(NULL, |default| self.default_value(default.value));
        let (member_flags, hint) = if parameter.is_promoted_property() && !self.promoted_beside {
            (self.member_flags(parameter), self.property_hint(hint, parameter.variable.name, line))
        } else {
            (0, hint)
        };
        let variadic_flag = if parameter.is_variadic() { ZEND_PARAM_VARIADIC } else { 0 };
        let service = service.map(|service| self.service_attribute(line, service));
        let attributes = self.attributes(&parameter.attribute_lists, None, service);

        self.node(
            SHARP_AST_PARAM,
            member_flags | variadic_flag,
            self.line(parameter),
            &[hint, name, default, attributes, NULL, NULL],
        )
    }

    /// A parameter's default, `value`. Spec section 9: it is any expression, which runs each time a call leaves the
    /// argument out. PHP takes a constant expression as a default, so one that is not lowers to a static closure called
    /// with no arguments, `(static function () { return \time(); })()`, which the engine runs as the default.
    fn default_value(&mut self, value: &Expression) -> u32 {
        let (lowered, _) = self.lambda_body(|lowering| lowering.expression(value));
        if self.is_constant_expression(lowered) {
            return lowered;
        }
        let line = self.line(value);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let returned = self.node(SHARP_AST_RETURN, 0, line, &[lowered]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[returned]);
        let closure = self.declaration(
            SHARP_AST_CLOSURE,
            ZEND_ACC_STATIC,
            value,
            value,
            b"",
            &[parameters, NULL, body, NULL, NULL],
        );
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);

        self.node(SHARP_AST_CALL, 0, line, &[closure, arguments])
    }

    /// Whether the lowered expression `node` is one PHP takes as a constant expression, as php-src's
    /// `zend_is_allowed_in_const_expr` says, leaving out the closures and first-class callables a default never is.
    fn is_constant_expression(&self, node: u32) -> bool {
        if node == NULL {
            return true;
        }
        let node = &self.nodes[node as usize];
        let constant = matches!(
            node.kind,
            SHARP_AST_ZVAL
                | SHARP_AST_BINARY_OP
                | SHARP_AST_GREATER
                | SHARP_AST_GREATER_EQUAL
                | SHARP_AST_AND
                | SHARP_AST_OR
                | SHARP_AST_UNARY_OP
                | SHARP_AST_UNARY_PLUS
                | SHARP_AST_UNARY_MINUS
                | SHARP_AST_CAST
                | SHARP_AST_CONDITIONAL
                | SHARP_AST_DIM
                | SHARP_AST_ARRAY
                | SHARP_AST_ARRAY_ELEM
                | SHARP_AST_UNPACK
                | SHARP_AST_CONST
                | SHARP_AST_CLASS_CONST
                | SHARP_AST_CLASS_NAME
                | SHARP_AST_MAGIC_CONST
                | SHARP_AST_COALESCE
                | SHARP_AST_CONST_ENUM_INIT
                | SHARP_AST_NEW
                | SHARP_AST_ARG_LIST
                | SHARP_AST_NAMED_ARG
                | SHARP_AST_PROP
                | SHARP_AST_NULLSAFE_PROP
        );
        let children = node.first_child as usize..(node.first_child + node.child_count) as usize;

        constant && children.into_iter().all(|child| self.is_constant_expression(self.children[child]))
    }

    /// `\Sharp\Map|array`, or `\Sharp\Set|array`, of the lowered `Map` or `Set` type `hint`, with `null` when it is
    /// nullable: what a parameter takes where plain PHP passes a PHP array.
    fn table_or_array(&mut self, hint: u32, nullable: bool) -> u32 {
        self.nodes[hint as usize].attr &= !ZEND_TYPE_NULLABLE;
        let line = self.nodes[hint as usize].line;
        let mut types = vec![hint, self.node(SHARP_AST_TYPE, IS_ARRAY, line, &[])];
        if nullable {
            types.push(self.string(ZEND_NAME_NOT_FQ, line, b"null"));
        }

        self.node(SHARP_AST_TYPE_UNION, 0, line, &types)
    }

    /// The flags of the property a constructor parameter promotes. Spec section 9 gives such a property access levels
    /// and no accessor bodies, so it has no hooks. One without accessors is a field, which reflection tells from a
    /// property by its flag.
    fn member_flags(&self, parameter: &FunctionLikeParameter) -> u32 {
        let accessor_flags = match &parameter.hooks {
            Some(accessors) => accessor_flags(&parameter.modifiers, accessors, self.names),
            None => ZEND_ACC_FIELD,
        };

        modifier_flags(&parameter.modifiers) | accessor_flags
    }

    /// `hint`, the lowered type of the property `property`, wrapped around the type text [`Types::property_check`]
    /// gives it, which PHP keeps on the property and checks a write from plain PHP against in its typed property
    /// check. `hint` alone when the type needs no text, or when the property has no type.
    fn property_hint(&mut self, hint: u32, property: &[u8], line: u32) -> u32 {
        match self.types.property_check(self.class, property) {
            Some(text) if hint != NULL => {
                let text = self.string(0, line, text.as_bytes());

                self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[hint, text])
            }
            _ => hint,
        }
    }

    /// The property a promoted parameter declares, with the visibility, accessors, accessor bodies, observers,
    /// behaviors and attributes it is written with, and no default. PHP sets a promoted property from the argument
    /// before the constructor's body runs, and a get-only one only once, so a constructor that promotes a `Map` or a
    /// `Set`, or a property whose writes run through its accessors, declares each property it promotes this way, in its
    /// written order, and sets each from its argument once the behaviors exist, spec section 9: the `Map` or `Set` one
    /// to the object of the argument, and each other through its `set`.
    fn promoted_property(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("semantics refuses a method parameter without a type");
        };
        let hooks = match (&parameter.hooks, &parameter.behaviors) {
            (Some(accessors), Some(behaviors)) => self.behavior_hooks(&parameter.variable, accessors, behaviors),
            (accessors, _) => self.hooks(&parameter.variable, accessors.as_ref(), Some(hint)),
        };
        let line = self.line(parameter.variable.span);
        let hint = self.property_type(hint, parameter.variable.name, line);
        let name = self.string(0, line, parameter.variable.name);
        let flags = self.member_flags(parameter);
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, NULL, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let crossing = self.crossing_attribute(parameter.hint.as_ref());
        let attributes = self.attributes(&parameter.attribute_lists, None, crossing);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(parameter), &[hint, declaration, attributes])
    }

    /// A field, a property with accessors or a computed property is a property group of one property, as php-src's
    /// grammar builds `private int $count = 0;`, `public private(set) int $views = 0;`, `public readonly int $id;`,
    /// `public string $name { get => $this->name; }` and `public string $slug { get => expr; }`. A field with no access
    /// modifier is `private`, while PHP makes it `public`. A constant initial value is its default, unless the property
    /// is `readonly`, and a member without one has none, as a nullable one has no implicit `null`. An override is a
    /// property with `#[\Override]`, and an auto-property override lowers as a field does. PHP refuses a type on a property whose parent's property has none, so an override of
    /// a property of `parent` whose root declaration has no type has none either, down the whole chain (decision 028).
    fn property(&mut self, property: &Property, parent: Option<&[u8]>) -> u32 {
        let (accessor_flags, attribute_lists, hooks) = match property {
            // Reflection tells a field from a property by its flag, spec section 6.1.
            Property::Plain(field) => (ZEND_ACC_FIELD, &field.attribute_lists, NULL),
            // An abstract property runs as PHP's, with abstract hooks and no `readonly`, as an interface property does.
            Property::Hooked(hooked) if hooked.modifiers.contains_abstract() => {
                let flags = accessor_flags(&hooked.modifiers, &hooked.hook_list, self.names) & !ZEND_ACC_READONLY;

                (flags, &hooked.attribute_lists, self.abstract_hooks(&hooked.hook_list))
            }
            // A static property's accessors are its class's hidden static methods, which alone reach its storage, so
            // the storage is private whatever the property's access level, and plain PHP reaches it only through a
            // method.
            Property::Hooked(hooked) if runs_static_accessors(property) => (0, &hooked.attribute_lists, NULL),
            Property::Hooked(hooked @ HookedProperty { behaviors: Some(behaviors), .. }) => (
                accessor_flags(&hooked.modifiers, &hooked.hook_list, self.names),
                &hooked.attribute_lists,
                self.behavior_hooks(hooked.item.variable(), &hooked.hook_list, behaviors),
            ),
            Property::Hooked(hooked) => (
                accessor_flags(&hooked.modifiers, &hooked.hook_list, self.names),
                &hooked.attribute_lists,
                self.hooks(hooked.item.variable(), Some(&hooked.hook_list), property.hint()),
            ),
            Property::Computed(computed) => {
                let lazy = is_lazy(&computed.modifiers);
                let hook = self.accessor_bodies(computed.variable.name, |lowering| {
                    let body =
                        if lazy { lowering.lazy_body(computed) } else { lowering.short_body(b"get", &computed.body) };

                    lowering.hook(b"get", computed.body.arrow, &computed.body, NULL, body)
                });

                (
                    // A lazy property keeps its value in its own storage, which only its `get` hook writes.
                    if lazy { ZEND_ACC_PRIVATE_SET } else { 0 },
                    &computed.attribute_lists,
                    self.node(SHARP_AST_STMT_LIST, 0, self.line(computed.body.arrow), &[hook]),
                )
            }
        };
        let Some(hint) = property.hint() else {
            unreachable!("the PHP# parser gives every field and property its type");
        };
        let variable = property.first_variable();
        let r#override = property.modifiers().iter().find(|modifier| matches!(modifier, Modifier::Override(_)));
        let line = self.line(variable);
        let hint = match (r#override, parent) {
            (Some(_), Some(parent))
                if self.types.member_declaration(parent, variable.name).kind
                    == (DeclarationKind::Property { typed: false }) =>
            {
                NULL
            }
            _ => self.property_type(hint, variable.name, line),
        };
        let name = self.string(0, line, variable.name);
        let default = match property.initial_value() {
            Some(value)
                if !property.initial_value_runs_in_constructor() && !property.initial_value_runs_after_class() =>
            {
                self.expression(value)
            }
            _ => NULL,
        };
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, default, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let access_flags = ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED | ZEND_ACC_PRIVATE;
        let flags = modifier_flags(property.modifiers()) | accessor_flags;
        let flags = if runs_static_accessors(property) { flags & !access_flags } else { flags };
        // A member with no access modifier is private, spec section 5.
        let access = if flags & access_flags == 0 { ZEND_ACC_PRIVATE } else { 0 };
        let flags = flags | access;
        let crossing = self.crossing_attribute(property.hint());
        let attributes = self.attributes(attribute_lists, r#override, crossing);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(property), &[hint, declaration, attributes])
    }

    /// A property's hook list, or null when it needs none: an auto-property is plain storage. Each body is a hook. An
    /// auto accessor beside a body that uses `field` is PHP's backing store, so it is no hook, and beside bodies that
    /// never use `field` it is the hook over the storage, so PHP keeps the property backed, as C# does. The observers
    /// are no hooks of their own: they run inside the `set` hook, which an auto `set` beside one always has. The `set`
    /// hook of a `Map` or a `Set`, whose type `holds` names, also takes the PHP array plain PHP writes, and the body
    /// first crosses it, as a parameter's is.
    fn hooks(&mut self, variable: &DirectVariable, accessors: Option<&PropertyHookList>, holds: Option<&Hint>) -> u32 {
        let Some(accessors) = accessors.filter(|accessors| {
            accessors.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
        }) else {
            return NULL;
        };

        let uses_field = self.names.uses_field(accessors);
        let named = |name: &[u8]| accessors.hooks.iter().find(|accessor| accessor.name.value == name);
        let (will_set, did_set) = (named(b"willSet"), named(b"didSet"));
        self.accessor_bodies(variable.name, |lowering| {
            let mut hooks = Vec::new();
            for accessor in accessors.hooks.iter().filter(|accessor| !accessor.is_observer()) {
                let observed = accessor.is_setter() && (will_set.is_some() || did_set.is_some());
                let mut body = match &accessor.body {
                    PropertyHookBody::Concrete(body) => lowering.accessor_body(accessor.name.value, body),
                    PropertyHookBody::Abstract(_) if uses_field && !observed => continue,
                    PropertyHookBody::Abstract(_) => lowering.storage_body(accessor),
                };
                if observed {
                    body = lowering.observed(accessor, will_set, body, did_set, true);
                }
                let mut parameters = NULL;
                if accessor.is_setter()
                    && let Some(hint) = holds
                    && let Some(nullable) = collection_hint(holds)
                {
                    let (span, line) = (accessor.name.span, lowering.line(accessor));
                    let lowered = lowering.hint(hint);
                    let accepted = lowering.table_or_array(lowered, nullable);
                    let value = lowering.string(0, line, b"value");
                    let parameter = lowering.node(SHARP_AST_PARAM, 0, line, &[accepted, value, NULL, NULL, NULL, NULL]);
                    parameters = lowering.node(SHARP_AST_PARAM_LIST, 0, line, &[parameter]);
                    let r#type = lowering.hint_type(hint);
                    let first = lowering.variable(span, b"value");
                    let crossed = lowering.crossed(span, &r#type, b"value", first);
                    let place = lowering.variable(span, b"value");
                    let crossing = lowering.node(SHARP_AST_ASSIGN, 0, line, &[place, crossed]);
                    body = lowering.node(SHARP_AST_STMT_LIST, 0, line, &[crossing, body]);
                }
                // An `init` body is the `set` hook, and the property's set visibility keeps it to object creation.
                let name = if accessor.is_setter() { b"set".as_slice() } else { accessor.name.value };
                hooks.push(lowering.hook(name, accessor.name, accessor, parameters, body));
            }

            lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(accessors), &hooks)
        })
    }

    /// The hooks of an interface property or an abstract one: each accessor is a hook with no body, as php-src's
    /// grammar builds `{ get; set; }`. An `init` is the `set` hook, as it is on a property with a body.
    fn abstract_hooks(&mut self, accessors: &PropertyHookList) -> u32 {
        let mut hooks = Vec::new();
        for accessor in &accessors.hooks {
            let name = if accessor.is_setter() { b"set".as_slice() } else { accessor.name.value };
            hooks.push(self.hook(name, accessor.name, accessor, NULL, NULL));
        }

        self.node(SHARP_AST_STMT_LIST, 0, self.line(accessors), &hooks)
    }

    /// `lowered`, the lowered value, checked against the type text `text` once it enters PHP# from plain PHP, with
    /// `target` naming what takes it, as `local counter`, for the engine's TypeError.
    fn verified(&mut self, line: u32, lowered: u32, text: &str, target: &[u8]) -> u32 {
        let text = self.string(0, line, text.as_bytes());
        let target = self.string(0, line, target);

        self.node(SHARP_AST_VERIFY_TYPE_ARGS, 0, line, &[lowered, text, target])
    }

    /// Lowers a property's accessor bodies, in which `field` is the property's storage and `Position.current()` names
    /// the property. The member around the property is the function again after them.
    fn accessor_bodies(&mut self, property: &[u8], lower: impl FnOnce(&mut Self) -> u32) -> u32 {
        let function = std::mem::take(&mut self.function);
        self.property = property.to_vec();
        self.enter(property);
        let index = lower(self);
        self.property.clear();
        self.function = function;

        index
    }

    /// A hook named `get` or `set` with its parameters, null for the one PHP gives it, and its body, as php-src's grammar
    /// declares every hook.
    fn hook(&mut self, name: &[u8], start: impl HasSpan, end: impl HasSpan, parameters: u32, body: u32) -> u32 {
        self.declaration(SHARP_AST_PROPERTY_HOOK, 0, start, end, name, &[parameters, NULL, body, NULL, NULL])
    }

    /// The body of `get => expr;`, the short body php-src's grammar builds, on the arrow's line. A `set` with an
    /// expression body is the statement list `set { expr; }`, because PHP stores the result of `set => expr;`.
    /// The body an accessor named `accessor` writes: a block, or `=> expr;`.
    fn accessor_body(&mut self, accessor: &[u8], body: &PropertyHookConcreteBody) -> u32 {
        match body {
            PropertyHookConcreteBody::Block(block) => self.block(block),
            PropertyHookConcreteBody::Expression(body) => self.short_body(accessor, body),
        }
    }

    /// The `set` hook of a property with observers, spec section 6.2: `willSet`, then `write`, the `set` body or the
    /// storage write, then `didSet`. An instance property's observers run only once its object is built, so they wait
    /// on what `\Sharp\Internal\building` says: `$built#1 = !\Sharp\Internal\building($this);
    /// if ($built#1) { willSet } write; if ($built#1) { didSet }`. A static's run on every write, as its initial value
    /// never writes through it.
    fn observed(
        &mut self,
        setter: &PropertyHook,
        will_set: Option<&PropertyHook>,
        write: u32,
        did_set: Option<&PropertyHook>,
        instance: bool,
    ) -> u32 {
        let line = self.line(setter);
        let span = setter.name.span;
        let mut statements = Vec::new();
        if instance {
            let this = self.variable(span, b"this");
            let building = self.global_call(line, b"Sharp\\Internal\\building", &[this]);
            let built = self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[building]);
            let place = self.variable(span, b"built#1");
            statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[place, built]));
        }
        let observer = |lowering: &mut Self, observer: &PropertyHook| {
            let PropertyHookBody::Concrete(body) = &observer.body else {
                unreachable!("semantics refuses an observer without a body");
            };
            let body = lowering.accessor_body(observer.name.value, body);
            if !instance {
                return body;
            }
            let built = lowering.variable(span, b"built#1");

            lowering.if_chain(span, &[(built, body)])
        };
        statements.extend(will_set.map(|will_set| observer(self, will_set)));
        statements.push(write);
        statements.extend(did_set.map(|did_set| observer(self, did_set)));

        self.node(SHARP_AST_STMT_LIST, 0, line, &statements)
    }

    fn short_body(&mut self, accessor: &[u8], body: &PropertyHookConcreteExpressionBody) -> u32 {
        let line = self.line(body.arrow);
        let expression = self.expression(body.expression);
        let kind = if accessor == b"get" { SHARP_AST_PROPERTY_HOOK_SHORT_BODY } else { SHARP_AST_STMT_LIST };

        self.node(kind, 0, line, &[expression])
    }

    /// The `get` body of `lazy T name => expr;`, spec section 6.5: it runs `expr` on the first read and keeps the value
    /// in the property's storage, and whether it ran in the hidden flag [`Lowering::lazy_flag`] declares, which a
    /// copy's `__clone` clears and a null value can't stand in for:
    /// `get { if (!$this->{'name#lazy'}) { $this->name = expr; $this->{'name#lazy'} = true; } return $this->name; }`.
    /// A throw in `expr` keeps nothing, so the next read runs it again.
    fn lazy_body(&mut self, computed: &ComputedProperty) -> u32 {
        let line = self.line(computed.body.arrow);
        let storage = self.storage(line, PlaceUse::Write);
        let value = self.expression(computed.body.expression);
        let keep = self.node(SHARP_AST_ASSIGN, 0, line, &[storage, value]);
        let flag = self.lazy_flag_fetch(line, computed.variable.name);
        let r#true = self.zval(line, sharp_value::SHARP_TRUE, |_| {});
        let ran = self.node(SHARP_AST_ASSIGN, 0, line, &[flag, r#true]);
        let first_read = self.node(SHARP_AST_STMT_LIST, 0, line, &[keep, ran]);
        let flag = self.lazy_flag_fetch(line, computed.variable.name);
        let not_ran = self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[flag]);
        let branch = self.node(SHARP_AST_IF_ELEM, 0, line, &[not_ran, first_read]);
        let r#if = self.node(SHARP_AST_IF, 0, line, &[branch]);
        let storage = self.storage(line, PlaceUse::Read);
        let r#return = self.node(SHARP_AST_RETURN, 0, line, &[storage]);

        self.node(SHARP_AST_STMT_LIST, 0, line, &[r#if, r#return])
    }

    /// The hidden flag of an instance `lazy` property, `private bool ${'name#lazy'} = false;`, which its `get` hook sets
    /// once the value is kept. No PHP# name has a `#`, so it names no member a program declares.
    fn lazy_flag(&mut self, computed: &ComputedProperty) -> u32 {
        let line = self.line(&computed.variable);
        let hint = self.string(ZEND_NAME_NOT_FQ, line, b"bool");
        let name = self.string(0, line, &lazy_flag_name(computed.variable.name));
        let r#false = self.zval(line, sharp_value::SHARP_FALSE, |_| {});
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, r#false, NULL, NULL]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);

        self.node(SHARP_AST_PROP_GROUP, ZEND_ACC_PRIVATE, line, &[hint, declaration, NULL])
    }

    /// The hidden private properties that hold the behaviors of the property `variable`, spec section 6.4, one per
    /// behavior, `private \App\Trimmed $name#via0;`, named as PHP source can't spell, as the hidden local `this#1` is.
    fn behavior_slots(&mut self, variable: &DirectVariable, behaviors: &PropertyBehaviorList) -> Vec<u32> {
        let line = self.line(variable);
        let mut slots = Vec::new();
        for (index, behavior) in behaviors.behaviors.iter().enumerate() {
            let hint = self.string(ZEND_NAME_FQ, line, self.behavior_class(behavior));
            let name = self.string(0, line, &behavior_slot_name(variable.name, index));
            let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, NULL, NULL, NULL]);
            let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
            slots.push(self.node(SHARP_AST_PROP_GROUP, ZEND_ACC_PRIVATE, line, &[hint, declaration, NULL]));
        }

        slots
    }

    /// `$this->{'name#via0'} = new \App\Trimmed();` for each behavior of the property `variable`, which the constructor
    /// runs before any initial value, spec section 6.4: one instance per object, with no arguments. A generic behavior's
    /// `new` carries its type arguments, as a generic `new` does.
    fn behavior_creations(&mut self, variable: &DirectVariable, behaviors: &PropertyBehaviorList) -> Vec<u32> {
        let line = self.line(variable);
        let mut creations = Vec::new();
        for (index, behavior) in behaviors.behaviors.iter().enumerate() {
            let slot = self.behavior_slot(line, variable.name, index);
            let name = self.behavior_class(behavior);
            let class = self.string(ZEND_NAME_FQ, line, name);
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
            let created = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);
            let type_arguments = self.types.type_arguments(name, behavior.span());
            let created = self.with_type_arguments(line, created, type_arguments);
            creations.push(self.node(SHARP_AST_ASSIGN, 0, line, &[slot, created]));
        }

        creations
    }

    /// The full name of the class the behavior `behavior` names, with or without type arguments.
    fn behavior_class(&self, behavior: &Hint) -> &'lowering [u8] {
        match behavior {
            Hint::Identifier(identifier) => self.names.get(identifier),
            Hint::Generic(generic) => self.names.get(&generic.name),
            _ => unreachable!("semantics refuses a behavior that names no class"),
        }
    }

    /// `$this->{'name#viaN'}`, the slot of the behavior at `index` of the property `property`.
    fn behavior_slot(&mut self, line: u32, property: &[u8], index: usize) -> u32 {
        let this = self.string(0, line, b"this");
        let this = self.node(SHARP_AST_VAR, 0, line, &[this]);
        let name = self.string(0, line, &behavior_slot_name(property, index));

        self.node(SHARP_AST_PROP, 0, line, &[this, name])
    }

    /// The hooks of the property `variable` with `behaviors`, spec section 6.4. `set` runs the written value through
    /// each behavior's `set` in the order they are written, and stores the result. `get` runs the storage through each
    /// behavior's `get` the other way, so the first behavior wraps the rest:
    /// `get => $this->{'name#via0'}->get($this->{'name#via1'}->get($this->name));` and
    /// `set { $this->name = $this->{'name#via1'}->set($this->{'name#via0'}->set($value)); }`.
    fn behavior_hooks(
        &mut self,
        variable: &DirectVariable,
        accessors: &PropertyHookList,
        behaviors: &PropertyBehaviorList,
    ) -> u32 {
        let (Some(getter), Some(setter)) =
            (accessors.hooks.iter().find(|accessor| accessor.name.value == b"get"), accessors.setter())
        else {
            unreachable!("semantics gives `via` only to a property with `get; set;`");
        };
        let count = behaviors.behaviors.len();

        self.accessor_bodies(variable.name, |lowering| {
            let line = lowering.line(variable);
            let call = |lowering: &mut Self, index: usize, accessor: &[u8], value: u32| {
                let slot = lowering.behavior_slot(line, variable.name, index);
                let method = lowering.string(0, line, accessor);
                let arguments = lowering.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

                lowering.node(SHARP_AST_METHOD_CALL, 0, line, &[slot, method, arguments])
            };

            let mut read = lowering.storage(line, PlaceUse::Read);
            for index in (0..count).rev() {
                read = call(lowering, index, b"get", read);
            }
            let read = lowering.node(SHARP_AST_PROPERTY_HOOK_SHORT_BODY, 0, line, &[read]);
            let get = lowering.hook(b"get", getter.name, getter, NULL, read);

            let mut written = lowering.variable(setter.name.span, b"value");
            for index in 0..count {
                written = call(lowering, index, b"set", written);
            }
            let storage = lowering.storage(line, PlaceUse::Write);
            let store = lowering.node(SHARP_AST_ASSIGN, 0, line, &[storage, written]);
            let store = lowering.node(SHARP_AST_STMT_LIST, 0, line, &[store]);
            let set = lowering.hook(b"set", setter.name, setter, NULL, store);

            lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(accessors), &[get, set])
        })
    }

    /// `$this->{'name#lazy'}`, the hidden flag of the instance `lazy` property `name`.
    fn lazy_flag_fetch(&mut self, line: u32, property: &[u8]) -> u32 {
        let this = self.string(0, line, b"this");
        let this = self.node(SHARP_AST_VAR, 0, line, &[this]);
        let name = self.string(0, line, &lazy_flag_name(property));

        self.node(SHARP_AST_PROP, 0, line, &[this, name])
    }

    /// An auto accessor's body over the storage: `get => $this->name;` or [`Self::storage_set`].
    fn storage_body(&mut self, accessor: &PropertyHook) -> u32 {
        if accessor.name.value == b"get" {
            let line = self.line(accessor);
            let storage = self.storage(line, PlaceUse::Read);
            self.node(SHARP_AST_PROPERTY_HOOK_SHORT_BODY, 0, line, &[storage])
        } else {
            self.storage_set(accessor.name.span)
        }
    }

    /// `set { $this->name = $value; }`, a `set` body over the storage, on the line of `span`.
    fn storage_set(&mut self, span: Span) -> u32 {
        let line = self.line(span);
        let storage = self.storage(line, PlaceUse::Write);
        let value = self.variable(span, b"value");
        let assignment = self.node(SHARP_AST_ASSIGN, 0, line, &[storage, value]);

        self.node(SHARP_AST_STMT_LIST, 0, line, &[assignment])
    }

    /// `field`: `$this->name` of the property whose accessor body is being lowered, which PHP reads and writes as the
    /// storage inside the property's own hook. In a generic class, a static's storage is used as `place_use` says, as
    /// [`Self::statics_field`] writes it.
    fn storage(&mut self, line: u32, place_use: PlaceUse) -> u32 {
        // A static property's storage is its class's own static, `self::$name`, or in a generic class the field of the
        // entry of the accessor's own type arguments, which are the class's.
        if self.types.is_static_accessor(self.class, &self.property) {
            if let Some(count) = self.static_type_parameters {
                let property = self.property.clone();

                return self.statics_field(line, self.class, &type_parameters_text(count), &property, place_use);
            }
            let name = store_text(&mut self.texts, &self.property);
            let name = self.zval(line, sharp_value::SHARP_STRING, |node| node.text = name);
            let class = self.string(ZEND_NAME_NOT_FQ, line, b"self");

            return self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, name]);
        }

        let this = self.string(0, line, b"this");
        let this = self.node(SHARP_AST_VAR, 0, line, &[this]);
        let name = store_text(&mut self.texts, &self.property);
        let name = self.zval(line, sharp_value::SHARP_STRING, |node| node.text = name);

        self.node(SHARP_AST_PROP, 0, line, &[this, name])
    }

    /// A declaration's attributes are one `ATTRIBUTE_LIST` with an `ATTRIBUTE_GROUP` per `[...]`, as php-src's grammar
    /// builds `#[...]`, or null without attributes. Each attribute names its class by its full name. A method's or a
    /// field's `override` adds a group of `#[\Override]`, and the group the lowering adds, `added`, comes last: a
    /// property's [`Self::crossing_attribute`] or a parameter's [`Self::service_attribute`].
    fn attributes(
        &mut self,
        lists: &Sequence<AttributeList>,
        r#override: Option<&Modifier>,
        added: Option<u32>,
    ) -> u32 {
        let line = match (lists.first(), r#override, added) {
            (Some(first), _, _) => self.line(first),
            (None, Some(r#override), _) => self.line(r#override),
            (None, None, Some(added)) => self.nodes[added as usize].line,
            (None, None, None) => return NULL,
        };

        let mut groups = Vec::new();
        for list in lists {
            let mut attributes = Vec::new();
            for attribute in &list.attributes {
                let line = self.line(attribute.name);
                let name = self.string(ZEND_NAME_FQ, line, self.names.get(&attribute.name));
                let arguments = attribute.argument_list.as_ref().map_or(NULL, |list| self.attribute_arguments(list));

                attributes.push(self.node(SHARP_AST_ATTRIBUTE, 0, line, &[name, arguments]));
            }

            groups.push(self.node(SHARP_AST_ATTRIBUTE_GROUP, 0, self.line(list), &attributes));
        }

        if let Some(r#override) = r#override {
            let line = self.line(r#override);
            let name = self.string(ZEND_NAME_FQ, line, b"Override");
            let attribute = self.node(SHARP_AST_ATTRIBUTE, 0, line, &[name, NULL]);
            groups.push(self.node(SHARP_AST_ATTRIBUTE_GROUP, 0, line, &[attribute]));
        }
        groups.extend(added);

        self.node(SHARP_AST_ATTRIBUTE_LIST, 0, line, &groups)
    }

    /// The `#[\Sharp\Crossing(plan)]` group of a property of a `Map` or a `Set` written `hint`, with the plan
    /// [`Self::crossing_plan`] gives it, by which the engine crosses a PHP array plain PHP writes there. None for a
    /// property of any other type.
    fn crossing_attribute(&mut self, hint: Option<&Hint>) -> Option<u32> {
        let hint = hint.filter(|hint| collection_hint(Some(hint)).is_some())?;
        let r#type = self.hint_type(hint);
        let line = self.line(hint);
        let plan = self.crossing_plan(line, &r#type);

        Some(self.attribute_group(line, b"Sharp\\Crossing", plan))
    }

    /// The `#[\Sharp\Internal\Service('…')]` group, on line `line`, of a parameter the container fills whose PHP type
    /// drops the service `id` it names.
    fn service_attribute(&mut self, line: u32, id: Word) -> u32 {
        let id = self.string(0, line, id.as_bytes());

        self.attribute_group(line, b"Sharp\\Internal\\Service", id)
    }

    /// `#[\Class(argument)]`, a group of one attribute of the class `class` with one argument.
    fn attribute_group(&mut self, line: u32, class: &[u8], argument: u32) -> u32 {
        let name = self.string(ZEND_NAME_FQ, line, class);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[argument]);
        let attribute = self.node(SHARP_AST_ATTRIBUTE, 0, line, &[name, arguments]);

        self.node(SHARP_AST_ATTRIBUTE_GROUP, 0, line, &[attribute])
    }

    /// A constant is a class constant group of one constant, as php-src's grammar builds `public const int MAX = 3;`:
    /// the constant list, no attributes, then the type.
    fn constant(&mut self, constant: &ClassLikeConstant) -> u32 {
        let item = constant.first_item();
        let line = self.line(item.name);
        let name = self.string(0, line, item.name.value);
        let value = self.expression(item.value);
        let element = self.node(SHARP_AST_CONST_ELEM, 0, line, &[name, value, NULL]);
        let declaration = self.node(SHARP_AST_CLASS_CONST_DECL, 0, line, &[element]);
        let hint = constant.hint.as_ref().map_or(NULL, |hint| self.hint(hint));

        self.node(
            SHARP_AST_CLASS_CONST_GROUP,
            modifier_flags_or_private(&constant.modifiers),
            self.line(constant),
            &[declaration, NULL, hint],
        )
    }

    /// The hidden method that sets each static whose initial value isn't constant, as PHP gives a static only a constant
    /// default: `private static function #statics(): void { self::$a = …; }`. The engine runs it once each request,
    /// when it first builds the class's statics, after their constant defaults (`zend_class_init_sharp_statics`), so a
    /// preloaded class gets the values too. Each value fills the storage, so it runs neither a `set` body nor an
    /// observer, spec section 6.2. None when each static's initial value is its default.
    fn static_initial_values(&mut self, members: &Sequence<ClassLikeMember>) -> Option<u32> {
        let properties: Vec<&Property> = members
            .iter()
            .filter_map(|member| match member {
                ClassLikeMember::Property(property) if property.initial_value_runs_after_class() => Some(property),
                _ => None,
            })
            .collect();
        let first = properties.first()?;
        let line = self.line(first.first_variable());
        let mut statements = Vec::new();
        for property in &properties {
            let variable = property.first_variable();
            let Some(value) = property.initial_value() else {
                unreachable!("a static whose initial value runs after its class has one");
            };
            let line = self.line(variable);
            let class = self.string(ZEND_NAME_NOT_FQ, line, b"self");
            let value = self.expression(value);
            let member = self.string(0, line, variable.name);
            let target = self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, member]);
            statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[target, value]));
        }
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let void = self.string(ZEND_NAME_NOT_FQ, line, b"void");
        let span = first.span();

        Some(self.declaration(
            SHARP_AST_METHOD,
            ZEND_ACC_PRIVATE | ZEND_ACC_STATIC,
            span,
            span,
            b"#statics",
            &[parameters, NULL, body, void, NULL],
        ))
    }

    /// The members a static property with accessors runs as, decision 3: PHP has no hooks on a static, so its storage,
    /// when it has one, is a plain static only its class writes, and its accessors are the hidden static methods
    /// `name#get` and `name#set`, which every PHP# read and write calls, as [`Lowering::accessor_call`] writes them.
    /// `name#get` returns the `get` body's value, or the storage for `get;`. `name#set` runs the observers around the
    /// `set` body, or around the storage write for `set;`, as an instance property's `set` hook does. In a generic class
    /// each type argument has its own: both take the class's type arguments as their own, as any static method of the
    /// class does, and the storage is a field of their entry of `statics_store`.
    fn static_accessors(&mut self, property: &Property, parent: Option<&[u8]>) -> Vec<u32> {
        let mut members = Vec::new();
        let generic = self.types.static_type_parameters(self.class);
        let accessors = match property {
            Property::Hooked(hooked) => {
                if self.names.has_storage(&hooked.hook_list) && generic.is_none() {
                    members.push(self.property(property, parent));
                }
                Some(&hooked.hook_list)
            }
            _ => None,
        };
        self.static_type_parameters = generic;
        let variable = property.first_variable();
        let Some(hint) = property.hint() else {
            unreachable!("the PHP# parser gives every field and property its type");
        };
        let read_visibility = modifier_flags(property.modifiers()) & (ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED);
        let read_visibility = if read_visibility == 0 { ZEND_ACC_PRIVATE } else { read_visibility };

        let get = self.accessor_bodies(variable.name, |lowering| {
            let line = lowering.line(variable);
            let returned = |lowering: &mut Self, value: u32| {
                let r#return = lowering.node(SHARP_AST_RETURN, 0, line, &[value]);
                lowering.node(SHARP_AST_STMT_LIST, 0, line, &[r#return])
            };
            let getter =
                accessors.and_then(|accessors| accessors.hooks.iter().find(|accessor| accessor.name.value == b"get"));
            let body = match (property, getter.map(|getter| &getter.body)) {
                (Property::Computed(computed), _) if is_lazy(&computed.modifiers) => {
                    let value = lowering.static_lazy_value(computed);
                    returned(lowering, value)
                }
                (Property::Computed(computed), _) => {
                    let value = lowering.expression(computed.body.expression);
                    returned(lowering, value)
                }
                (_, Some(PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block)))) => lowering.block(block),
                (_, Some(PropertyHookBody::Concrete(PropertyHookConcreteBody::Expression(body)))) => {
                    let value = lowering.expression(body.expression);
                    returned(lowering, value)
                }
                _ => {
                    let storage = lowering.storage(line, PlaceUse::Read);
                    returned(lowering, storage)
                }
            };
            let return_type = lowering.hint(hint);

            lowering.accessor_method(variable, b"get", read_visibility, &[], body, return_type)
        });
        members.push(get);

        let Some((accessors, setter)) = accessors.and_then(|accessors| Some((accessors, accessors.setter()?))) else {
            self.static_type_parameters = None;

            return members;
        };
        let named = |name: &[u8]| accessors.hooks.iter().find(|accessor| accessor.name.value == name);
        let (will_set, did_set) = (named(b"willSet"), named(b"didSet"));
        let write_visibility = match setter.modifiers.first() {
            Some(Modifier::Private(_)) => ZEND_ACC_PRIVATE,
            Some(Modifier::Protected(_)) => ZEND_ACC_PROTECTED,
            // PHP has no namespace reach, so the analyzer checks `internal set`, as on an instance property.
            _ => read_visibility,
        };
        let set = self.accessor_bodies(variable.name, |lowering| {
            let line = lowering.line(setter);
            let write = match &setter.body {
                PropertyHookBody::Concrete(body) => lowering.accessor_body(setter.name.value, body),
                PropertyHookBody::Abstract(_) => lowering.storage_set(setter.name.span),
            };
            let body = if will_set.is_some() || did_set.is_some() {
                lowering.observed(setter, will_set, write, did_set, false)
            } else {
                write
            };
            let accepted = lowering.hint(hint);
            let value = lowering.string(0, line, b"value");
            let parameter = lowering.node(SHARP_AST_PARAM, 0, line, &[accepted, value, NULL, NULL, NULL, NULL]);

            lowering.accessor_method(variable, b"set", write_visibility, &[parameter], body, NULL)
        });
        members.push(set);
        self.static_type_parameters = None;

        members
    }

    /// The value a `static lazy` property's `name#get` returns, spec section 6.5:
    /// `\Sharp\Internal\keep('App\Report::name', static fn () => body)`, which runs the body on the first read and keeps
    /// its value for a singleton's lifetime. In a generic class each type argument keeps its own, under a key with the
    /// text of `name#get`'s own type arguments, `'App\Counter<' . <App.Order> . '>::name'`, and a body that reads them
    /// is a closure that captures them.
    fn static_lazy_value(&mut self, computed: &ComputedProperty) -> u32 {
        let line = self.line(computed.body.arrow);
        let key = match self.static_type_parameters {
            None => self.string(0, line, &[self.class, b"::", computed.variable.name].concat()),
            Some(_) => {
                let class = self.string(0, line, &[self.class, b"<"].concat());
                let own = self.own_type_arguments(line);
                let name = self.string(0, line, &[b">::", computed.variable.name].concat());
                let key = self.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[class, own]);

                self.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[key, name])
            }
        };
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
        let (value, captures_type_arguments) =
            self.lambda_body(|lowering| lowering.expression(computed.body.expression));
        let compute = if captures_type_arguments {
            let uses = self.string(0, line, TYPE_ARGUMENTS);
            let uses = self.node(SHARP_AST_CLOSURE_USES, 0, line, &[uses]);
            let value = self.node(SHARP_AST_RETURN, 0, line, &[value]);
            let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[value]);

            self.declaration(
                SHARP_AST_CLOSURE,
                ZEND_ACC_STATIC,
                computed.body.arrow,
                computed.body.expression,
                b"",
                &[parameters, uses, body, NULL, NULL],
            )
        } else {
            self.declaration(
                SHARP_AST_ARROW_FUNC,
                ZEND_ACC_STATIC,
                computed.body.arrow,
                computed.body.expression,
                b"",
                &[parameters, NULL, value, NULL, NULL],
            )
        };
        let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\keep");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[key, compute]);

        self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
    }

    /// The class and the name of `target` when it is `Class.name` for a static property that runs accessors, whose
    /// reads and writes call its hidden methods, as [`Lowering::static_accessors`] writes them.
    fn static_accessor<'ast>(&self, target: &Expression<'ast>) -> Option<(&'ast ConstantAccess<'ast>, &'ast [u8])> {
        let Expression::Access(Access::Property(
            access @ PropertyAccess { property: ClassLikeMemberSelector::Identifier(name), .. },
        )) = target.unparenthesized()
        else {
            return None;
        };
        let class = self.names.static_property_class(access)?;
        let classes =
            self.types.type_parameter_classes(&class.name).unwrap_or_else(|| vec![self.names.get(&class.name)]);

        classes.iter().any(|class| self.types.is_static_accessor(class, name.value)).then_some((class, name.value))
    }

    /// `Class::{'name#get'}()` or `Class::{'name#set'}(value)`: the call of the hidden method `accessor` of the static
    /// property `name` of `class`, which the access at `span` reaches, with `arguments`, and last the access's type
    /// arguments when `class` is a generic PHP# class, whose type arguments each have their own static.
    fn accessor_call(
        &mut self,
        class: &ConstantAccess,
        name: &[u8],
        accessor: &[u8],
        arguments: &[u32],
        span: Span,
    ) -> u32 {
        let line = self.line(class);
        let (class, _) = self.static_class(class);
        let method = self.string(0, line, &[name, b"#", accessor].concat());
        let mut arguments = arguments.to_vec();
        if let Some(type_arguments) = self.types.statics_type_arguments(&span) {
            arguments.push(self.type_arguments_argument(line, &type_arguments));
        }
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments])
    }

    /// A write of `value` to the static property `name` of `class`, which runs accessors, at `span`:
    /// `Class::{'name#set'}($value#N = value) ?? $value#N`. The hidden setter returns nothing, so the write's value is
    /// the value written, as an assignment's is.
    fn accessor_write(&mut self, class: &ConstantAccess, name: &[u8], value: u32, span: Span) -> u32 {
        let line = self.line(span);
        let hidden = self.hidden("value");
        let variable = self.variable(span, &hidden);
        let held = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, value]);
        let call = self.accessor_call(class, name, b"set", &[held], span);
        let written = self.variable(span, &hidden);
        self.temporaries -= 1;

        self.node(SHARP_AST_COALESCE, 0, line, &[call, written])
    }

    /// `++` or `--`, by `operation`, `ZEND_ADD` or `ZEND_SUB`, of the static property `name` of `class`, which runs
    /// accessors, at `span`. A prefix one is the value written. A postfix one is the value read:
    /// `Class::{'name#set'}(($old#N = Class::{'name#get'}()) + 1) ?? $old#N`.
    fn accessor_step(&mut self, class: &ConstantAccess, name: &[u8], operation: u32, postfix: bool, span: Span) -> u32 {
        let line = self.line(span);
        let one = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = 1);
        let read = self.accessor_call(class, name, b"get", &[], span);
        if !postfix {
            let value = self.node(SHARP_AST_BINARY_OP, operation, line, &[read, one]);

            return self.accessor_write(class, name, value, span);
        }

        let hidden = self.hidden("old");
        let old = self.variable(span, &hidden);
        let held = self.node(SHARP_AST_ASSIGN, 0, line, &[old, read]);
        let value = self.node(SHARP_AST_BINARY_OP, operation, line, &[held, one]);
        let call = self.accessor_call(class, name, b"set", &[value], span);
        let old = self.variable(span, &hidden);
        self.temporaries -= 1;

        self.node(SHARP_AST_COALESCE, 0, line, &[call, old])
    }

    /// The hidden static method `name#get` or `name#set` of the static property `variable`, with the visibility
    /// `visibility` and `parameters`, spanning the property's name. In a generic class its parameter list ends with
    /// the class's bounds, as any static method of the class does.
    fn accessor_method(
        &mut self,
        variable: &DirectVariable,
        accessor: &[u8],
        visibility: u32,
        parameters: &[u32],
        body: u32,
        return_type: u32,
    ) -> u32 {
        let line = self.line(variable);
        let mut parameters = parameters.to_vec();
        if self.static_type_parameters.is_some() {
            let bounds = self.types.bounds(self.class);
            parameters.extend(self.parameter_metadata((bounds, None), line));
        }
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters);
        let name = [variable.name, b"#", accessor].concat();

        self.declaration(
            SHARP_AST_METHOD,
            visibility | ZEND_ACC_STATIC,
            variable.span,
            variable.span,
            &name,
            &[parameters, NULL, body, return_type, NULL],
        )
    }

    /// The write of `property`'s initial value `value` at the start of the constructor, on the line of its name. It
    /// fills the storage, so it runs neither a `set` body nor an observer, spec section 6: PHP's
    /// `(new \ReflectionProperty(self::class, 'name'))->setRawValue($this, value);` when the property has a `set` hook,
    /// and `$this->name = value;`, which runs the behaviors' `set`, otherwise.
    fn initial_value(&mut self, property: &Property, value: &Expression) -> u32 {
        let variable = property.first_variable();
        let line = self.line(variable);
        let name = self.string(0, line, variable.name);
        let value = self.expression(value);
        let this = self.variable(variable.span, b"this");
        let runs_set_hook = matches!(property, Property::Hooked(hooked) if hooked.behaviors.is_none()
        && hooked.hook_list.hooks.iter().any(|accessor| {
            accessor.is_observer() || (accessor.is_setter() && matches!(accessor.body, PropertyHookBody::Concrete(_)))
        }));
        if !runs_set_hook {
            let property = self.node(SHARP_AST_PROP, 0, line, &[this, name]);

            return self.node(SHARP_AST_ASSIGN, 0, line, &[property, value]);
        }

        let own = self.string(ZEND_NAME_NOT_FQ, line, b"self");
        let class = self.node(SHARP_AST_CLASS_NAME, 0, line, &[own]);
        let reflection = self.string(ZEND_NAME_FQ, line, b"ReflectionProperty");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[class, name]);
        let reflection = self.node(SHARP_AST_NEW, 0, line, &[reflection, arguments]);
        let method = self.string(0, line, b"setRawValue");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[this, value]);

        self.node(SHARP_AST_METHOD_CALL, 0, line, &[reflection, method, arguments])
    }

    /// A type is the type PHP writes for it once generics are erased, as [`Lowering::erase`] gives it.
    ///
    /// A union is one `TYPE_UNION` list of its types in the order they are written, on the line of its first type, as
    /// php-src's `union_type` rule builds `int|string`. A type another type of the union holds is left out, because PHP
    /// refuses a union that names a type twice or an intersection beside one of its classes, so a union that holds
    /// `mixed` is `mixed`. One type left is that type alone.
    ///
    /// `?` after a type, or after a union in parentheses, adds null, which `mixed` already holds, and PHP refuses
    /// `?mixed`. One type takes `ZEND_TYPE_NULLABLE`, as php-src's grammar builds `?int`. An intersection, which PHP
    /// refuses after `?`, or several types end the `TYPE_UNION` list with the name `null` on the line of the `?`, as
    /// php-src builds `int|string|null` and the DNF type `(A&B)|null`.
    fn hint(&mut self, hint: &Hint) -> u32 {
        // Spec section 13: the checker keeps a readonly value read only, so the engine runs its type.
        let hint = without_readonly(hint);
        let (union, question_mark) = match hint {
            Hint::Nullable(NullableHint { question_mark, hint: Hint::Parenthesized(parenthesized) }) => {
                (parenthesized.hint, Some(*question_mark))
            }
            Hint::Nullable(nullable) => (nullable.hint, Some(nullable.question_mark)),
            _ => (hint, None),
        };

        self.union(union, question_mark)
    }

    /// The type `union`, a union or one type, with null added when `question_mark` is the `?` that adds it.
    fn union(&mut self, union: &Hint, question_mark: Option<Span>) -> u32 {
        let mut types: Vec<(PhpType, u32)> = Vec::new();
        for member in union_members(union) {
            let php_type = self.erase(member);
            if types.iter().any(|(kept, _)| kept.holds(&php_type)) {
                continue;
            }

            types.retain(|(kept, _)| !php_type.holds(kept));
            types.push((php_type, self.line(member)));
        }

        let mut nodes: Vec<u32> = types.iter().map(|(php_type, line)| self.php_type(php_type, *line)).collect();
        let question_mark = question_mark.filter(|_| !matches!(types.as_slice(), [(PhpType::Mixed, _)]));
        if let [single] = nodes[..] {
            if question_mark.is_none() {
                return single;
            }

            if self.nodes[single as usize].kind != SHARP_AST_TYPE_INTERSECTION {
                self.nodes[single as usize].attr |= ZEND_TYPE_NULLABLE;

                return single;
            }
        }

        if let Some(question_mark) = question_mark {
            nodes.push(self.string(ZEND_NAME_NOT_FQ, self.line(question_mark), b"null"));
        }
        let line = self.nodes[nodes[0] as usize].line;

        self.node(SHARP_AST_TYPE_UNION, 0, line, &nodes)
    }

    /// The type PHP writes for a type that is not a union, with generics erased. A built-in type is its name, `Object`
    /// is `object`, and `Any` is `mixed`. A class is its full name. `Self` is `static`. A `List` is a PHP array, so its type is `array`, and a
    /// `Map` or a `Set` is the engine's `\Sharp\Map` or `\Sharp\Set`, and an `Iterable` is PHP's `iterable`. A generic class type is its class, and `Class<T>` is `string`, because plain PHP receives a class
    /// value as its class-name string (spec section 25). A function type runs as PHP's `\Closure`. A type parameter is
    /// its bound, or `mixed` without one, as the PHP twin written with `@template` declares it, and the classes a bound
    /// joins with `&` are one intersection. A plain PHP trait is `object`, because no value is `instanceof` a trait, and
    /// its PHP# type text checks it, spec section 22: in an intersection it leaves PHP's type to the classes beside it.
    fn erase<'hint>(&self, hint: &'hint Hint<'_>) -> PhpType<'hint>
    where
        'arena: 'hint,
    {
        match hint {
            Hint::Integer(name)
            | Hint::Float(name)
            | Hint::Bool(name)
            | Hint::String(name)
            | Hint::Void(name)
            | Hint::Never(name) => PhpType::Named(name.value),
            Hint::Object(_) => PhpType::Named(b"object"),
            Hint::Mixed(_) => PhpType::Mixed,
            Hint::Identifier(name) => match self.names.binding(name) {
                Some(Binding::TypeParameter { declaration }) => {
                    match self.type_parameters.get(&declaration.start.offset) {
                        Some(php_type) => php_type.clone(),
                        None => unreachable!("the binder binds `{name}` to a type parameter the file declares"),
                    }
                }
                _ if self.types.class_declaration(self.names.get(name)).kind == DeclarationKind::Trait => {
                    PhpType::Keyword(IS_OBJECT)
                }
                _ => PhpType::Classes(vec![self.names.get(name)]),
            },
            // A plain PHP trait leaves the intersection, and the PHP# type text checks it.
            Hint::Intersection(intersection) => match (self.erase(intersection.left), self.erase(intersection.right)) {
                (PhpType::Classes(mut classes), PhpType::Classes(right)) => {
                    classes.extend(right);

                    PhpType::Classes(classes)
                }
                (classes @ PhpType::Classes(_), PhpType::Keyword(IS_OBJECT))
                | (PhpType::Keyword(IS_OBJECT), classes @ (PhpType::Classes(_) | PhpType::Keyword(IS_OBJECT))) => {
                    classes
                }
                _ => unreachable!("check_slice joins only classes and traits with `&`"),
            },
            Hint::Self_(_) => PhpType::Keyword(IS_STATIC),
            Hint::Generic(generic) if built_in_generic_arity(generic.name.value).is_none() => {
                PhpType::Classes(vec![self.names.get(&generic.name)])
            }
            Hint::Generic(generic) if generic.name.value == b"Class" => PhpType::Named(b"string"),
            Hint::Generic(generic) if generic.name.value == b"Map" => PhpType::Classes(vec![b"Sharp\\Map"]),
            Hint::Generic(generic) if generic.name.value == b"Set" => PhpType::Classes(vec![b"Sharp\\Set"]),
            Hint::Generic(generic) if generic.name.value == b"Iterable" => PhpType::Named(b"iterable"),
            // Spec section 30: a tuple runs as a PHP list.
            Hint::Generic(_) | Hint::Tuple(_) => PhpType::Keyword(IS_ARRAY),
            Hint::Function(_) => PhpType::Classes(vec![b"Closure"]),
            _ => unreachable!("check_slice refuses the type `{hint}`"),
        }
    }

    /// The PHP type of the property `property`, which PHP# declares with the type `hint`, with the type text the engine
    /// checks each write against where PHP's type checks less. An override of a typed plain PHP property runs with the
    /// type the parent declares, spec section 6.1, and its text checks its own type.
    fn property_type(&mut self, hint: &Hint, property: &[u8], line: u32) -> u32 {
        let php_type = match self.types.narrowed_root_type(self.class, property) {
            Some(root) => self.declared_type(root, line),
            None => self.hint(hint),
        };

        self.property_hint(php_type, property, line)
    }

    /// The node of `r#type`, a type plain PHP declares, so it holds only PHP's own types: each as [`Self::php_type`]
    /// writes it, in a union when there are several.
    fn declared_type(&mut self, r#type: &TUnion, line: u32) -> u32 {
        let mut nodes = Vec::new();
        for atomic in r#type.types.iter() {
            let php_type = match atomic {
                TAtomic::Null => PhpType::Named(b"null"),
                TAtomic::Mixed(_) => PhpType::Mixed,
                TAtomic::Array(_) => PhpType::Keyword(IS_ARRAY),
                TAtomic::Iterable(_) => PhpType::Named(b"iterable"),
                TAtomic::Object(TObject::Any) => PhpType::Named(b"object"),
                TAtomic::Object(TObject::Enum(r#enum)) => PhpType::Classes(vec![r#enum.name.as_bytes()]),
                TAtomic::Object(TObject::Named(object)) => PhpType::Classes(
                    std::iter::once(object.name.as_bytes())
                        .chain(object.get_intersection_types().unwrap_or_default().iter().filter_map(
                            |part| match part {
                                TAtomic::Object(TObject::Named(part)) => Some(part.name.as_bytes()),
                                _ => None,
                            },
                        ))
                        .collect(),
                ),
                TAtomic::Scalar(scalar) if scalar.is_true() => PhpType::Named(b"true"),
                TAtomic::Scalar(scalar) if scalar.is_false() => PhpType::Named(b"false"),
                TAtomic::Scalar(scalar) if scalar.is_general_bool() => PhpType::Named(b"bool"),
                TAtomic::Scalar(scalar) if scalar.is_int() => PhpType::Named(b"int"),
                TAtomic::Scalar(scalar) if scalar.is_float() => PhpType::Named(b"float"),
                TAtomic::Scalar(scalar) if scalar.is_string() => PhpType::Named(b"string"),
                _ => unreachable!("PHP declares no property of the type `{}`", atomic.get_id()),
            };
            nodes.push(self.php_type(&php_type, line));
        }

        match nodes[..] {
            [single] => single,
            _ => self.node(SHARP_AST_TYPE_UNION, 0, line, &nodes),
        }
    }

    /// The node of a type PHP writes, on `line`: a built-in type by its name with `ZEND_NAME_NOT_FQ`, a keyword type as
    /// a `TYPE` node of its type code, a class by its full name with `ZEND_NAME_FQ`, and several classes as the
    /// `TYPE_INTERSECTION` list php-src's `intersection_type` rule builds for `A&B`.
    fn php_type(&mut self, php_type: &PhpType, line: u32) -> u32 {
        match php_type {
            PhpType::Mixed => self.string(ZEND_NAME_NOT_FQ, line, b"mixed"),
            PhpType::Named(name) => self.string(ZEND_NAME_NOT_FQ, line, name),
            PhpType::Keyword(code) => self.node(SHARP_AST_TYPE, *code, line, &[]),
            PhpType::Classes(classes) => {
                let classes: Vec<u32> = classes.iter().map(|class| self.string(ZEND_NAME_FQ, line, class)).collect();
                match classes[..] {
                    [class] => class,
                    _ => self.node(SHARP_AST_TYPE_INTERSECTION, 0, line, &classes),
                }
            }
        }
    }

    fn block(&mut self, block: &Block) -> u32 {
        let mut statements = Vec::new();
        for statement in &block.statements {
            statements.push(self.statement(statement));
        }

        self.node(SHARP_AST_STMT_LIST, 0, self.line(block), &statements)
    }

    fn statement(&mut self, statement: &Statement) -> u32 {
        ensure_sufficient_stack(|| match statement {
            Statement::Block(block) => self.block(block),
            Statement::Expression(statement) => self.expression(statement.expression),
            Statement::Return(r#return) => self.r#return(r#return),
            // `emit e;` is `\Sharp\Internal\emit($e);`, which runs every listener of the event before it returns.
            Statement::Emit(emit) => {
                let line = self.line(emit);
                let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\emit");
                let event = self.expression(emit.value);
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[event]);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            Statement::LocalDeclaration(local) => self.local(local),
            Statement::If(r#if) => self.r#if(r#if),
            Statement::For(r#for) => self.r#for(r#for),
            Statement::ForOf(for_of) => self.for_of(for_of),
            Statement::While(r#while) => {
                let WhileBody::Statement(body) = &r#while.body else {
                    unreachable!("check_slice refuses a colon-delimited `while`");
                };
                let condition = self.expression(r#while.condition);
                let body = self.loop_body(body);

                self.node(SHARP_AST_WHILE, 0, self.line(r#while), &[condition, body])
            }
            Statement::DoWhile(do_while) => {
                let body = self.loop_body(do_while.statement);
                let condition = self.expression(do_while.condition);

                self.node(SHARP_AST_DO_WHILE, 0, self.line(do_while), &[body, condition])
            }
            Statement::Try(r#try) => self.r#try(r#try),
            Statement::PatternMatch(_) => self.pattern(Node::Statement(statement)),
            Statement::Break(r#break) => self.node(SHARP_AST_BREAK, 0, self.line(r#break), &[NULL]),
            Statement::Continue(r#continue) => self.node(SHARP_AST_CONTINUE, 0, self.line(r#continue), &[NULL]),
            _ => unreachable!("check_slice refuses the statement `{statement}`"),
        })
    }

    fn r#return(&mut self, r#return: &Return) -> u32 {
        let returns = self.returns.clone();
        let value = r#return.value.map_or(NULL, |value| self.crossed_value(value, returns.as_ref()));

        self.node(SHARP_AST_RETURN, 0, self.line(r#return), &[value])
    }

    /// A `for … of` is the `foreach` of its collection. A loop variable is each element, spec sections 17 and 30, and a
    /// `Map`'s element is its entry, so one variable over a `Map` is `[$key, $value]` built from both, which a
    /// `Sharp\Map` and a PHP array give alike: `foreach ($plans as $key => $value) { $entry = [$key, $value]; … }`.
    /// `[a, b]` reads a `Map`'s key and value, and unpacks each two-element tuple of any other collection, as
    /// `foreach ($rows as [$a, $b])`.
    fn for_of(&mut self, for_of: &ForOf) -> u32 {
        let line = self.line(for_of);
        let holds_map = self.types.holds_map(for_of.expression);
        // Spec section 12: PHP holds an all-digit `string` key as an `int`, so a plain PHP array keyed by `string` or by
        // a backed enum becomes the `\Sharp\Map` first, which takes each key back, whichever variables the loop takes.
        let r#type = self.types.expression_type(for_of.expression).clone();
        let collection = if holds_map && self.may_be_php_array(for_of.expression) && self.takes_keys_back(&r#type) {
            self.crossed_value(for_of.expression, Some(&r#type))
        } else {
            self.expression(for_of.expression)
        };
        let variable =
            |this: &mut Self, variable: &ForOfVariable| this.variable(variable.name.span, variable.name.value);
        let taken = self.temporaries;
        let (key, value, entry) = match &for_of.target {
            ForOfTarget::Value(entry) if holds_map => {
                let span = entry.name.span;
                let (key_name, value_name) = (self.hidden("key"), self.hidden("value"));
                let elements = [&key_name, &value_name].map(|name| {
                    let part = self.variable(span, name);

                    self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[part, NULL])
                });
                let tuple = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements);
                let entry = variable(self, entry);
                let entry = self.node(SHARP_AST_ASSIGN, 0, line, &[entry, tuple]);

                (self.variable(span, &key_name), self.variable(span, &value_name), Some(entry))
            }
            ForOfTarget::Value(value) => (NULL, variable(self, value), None),
            ForOfTarget::Unpack(unpack) if holds_map => {
                (variable(self, &unpack.first), variable(self, &unpack.second), None)
            }
            ForOfTarget::Unpack(unpack) => {
                let elements = [&unpack.first, &unpack.second].map(|part| {
                    let part = variable(self, part);

                    self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[part, NULL])
                });

                (NULL, self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements), None)
            }
        };
        let mut body = self.loop_body(for_of.body);
        if let Some(entry) = entry {
            body = self.node(SHARP_AST_STMT_LIST, 0, line, &[entry, body]);
        }
        self.temporaries = taken;

        self.node(SHARP_AST_FOREACH, 0, line, &[collection, value, key, body])
    }

    fn loop_body(&mut self, body: &Statement) -> u32 {
        self.loop_depth += 1;
        let body = self.statement(body);
        self.loop_depth -= 1;

        body
    }

    /// A `let` or `const` local is the assignment of its value to its variable, and an unpacked tuple, spec section 30,
    /// is PHP's list assignment of a tuple's elements, `[$total, $count] = $summary;`, as a tuple is a PHP list. Spec
    /// section 3 gives each loop pass its own local, and PHP reuses one variable, so a local in a loop body that a
    /// lambda captures by reference is `{ unset($local); $local = value; }`, and the lambda of each pass keeps its own.
    fn local(&mut self, local: &LocalDeclaration) -> u32 {
        let line = self.line(local);
        let target = match &local.target {
            LocalTarget::Name(name) => self.variable(name.span, name.value),
            LocalTarget::Unpack(unpack) => {
                let elements: Vec<u32> = unpack
                    .names
                    .iter()
                    .map(|name| {
                        let variable = self.variable(name.span, name.value);

                        self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[variable, NULL])
                    })
                    .collect();

                self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements)
            }
        };
        // A local declared without a type that holds a `Map` or a `Set` takes the type of its value, as a typed local
        // does: a PHP array plain PHP gives becomes the object here, so a method that changes it changes the local, and
        // a read of the local needs no crossing.
        let declared = match local.hint {
            Some(hint) => Some(self.hint_type(hint)),
            None => Some(self.types.expression_type(local.value).clone())
                .filter(|inferred| self.crossing_array(inferred).is_some()),
        };
        let value = self.crossed_value(local.value, declared.as_ref());
        if let Some(declared) = declared {
            self.local_types.insert(local.target.span().start.offset, declared);
        }
        let assignment = self.node(SHARP_AST_ASSIGN, 0, line, &[target, value]);
        if self.loop_depth == 0 {
            return assignment;
        }
        let captured: Vec<&LocalIdentifier> = local
            .target
            .names()
            .into_iter()
            .filter(|name| self.by_reference.contains(&name.span.start.offset))
            .collect();
        if captured.is_empty() {
            return assignment;
        }

        let unsets: Vec<u32> = captured
            .into_iter()
            .map(|name| {
                let variable = self.variable(name.span, name.value);

                self.node(SHARP_AST_UNSET, 0, line, &[variable])
            })
            .collect();
        let unset = self.node(SHARP_AST_STMT_LIST, 0, line, &unsets);

        self.node(SHARP_AST_STMT_LIST, 0, line, &[unset, assignment])
    }

    /// Each part of the header is an `EXPR_LIST`, or null when it is empty. A `let` or `const` counter is the
    /// first part.
    fn r#for(&mut self, r#for: &For) -> u32 {
        let ForBody::Statement(body) = &r#for.body else {
            unreachable!("check_slice refuses a colon-delimited `for`");
        };

        let mut initializations = Vec::new();
        if let Some(declaration) = &r#for.declaration {
            initializations.push(self.local(declaration));
        }
        for initialization in &r#for.initializations {
            initializations.push(self.expression(initialization));
        }

        let initializations = self.expression_list(&initializations);
        let conditions: Vec<u32> = r#for.conditions.iter().map(|condition| self.expression(condition)).collect();
        let conditions = self.expression_list(&conditions);
        let increments: Vec<u32> = r#for.increments.iter().map(|increment| self.expression(increment)).collect();
        let increments = self.expression_list(&increments);
        let body = self.loop_body(body);

        self.node(SHARP_AST_FOR, 0, self.line(r#for), &[initializations, conditions, increments, body])
    }

    /// An `EXPR_LIST` at the line of its first expression, or null when there are no expressions.
    fn expression_list(&mut self, expressions: &[u32]) -> u32 {
        let Some(&first) = expressions.first() else {
            return NULL;
        };

        let line = self.nodes[first as usize].line;

        self.node(SHARP_AST_EXPR_LIST, 0, line, expressions)
    }

    /// An `IF` list with one `IF_ELEM` per branch, and a null condition for `else`. php-src's grammar reads
    /// `else if` as an `else` whose statement is the next `if`.
    fn r#if(&mut self, r#if: &If) -> u32 {
        let IfBody::Statement(body) = &r#if.body else {
            unreachable!("check_slice refuses a colon-delimited `if`");
        };

        let condition = self.expression(r#if.condition);
        let statement = self.statement(body.statement);
        let mut branches = vec![self.node(SHARP_AST_IF_ELEM, 0, self.line(r#if.condition), &[condition, statement])];
        if let Some(else_clause) = &body.else_clause {
            let statement = self.statement(else_clause.statement);
            branches.push(self.node(SHARP_AST_IF_ELEM, 0, self.line(else_clause.statement), &[NULL, statement]));
        }

        self.node(SHARP_AST_IF, 0, self.line(r#if), &branches)
    }

    /// A `TRY` takes the try block, a `CATCH_LIST` and the finally block or null. php-src's grammar takes a node's
    /// line from its first child, so a `TRY` is on the line of its block and a `CATCH` on the line of its first class.
    fn r#try(&mut self, r#try: &Try) -> u32 {
        let block = self.block(&r#try.block);
        let tests_type_arguments = r#try.catch_clauses.iter().any(|clause| {
            union_members(&clause.hint).into_iter().any(|member| self.types.tested_type(member).is_some())
        });
        let catches = if tests_type_arguments {
            vec![self.tested_catch(r#try)]
        } else {
            r#try.catch_clauses.iter().map(|clause| self.catch(clause)).collect()
        };

        let catches = self.node(SHARP_AST_CATCH_LIST, 0, self.line(r#try.block.right_brace), &catches);
        let finally = r#try.finally_clause.as_ref().map_or(NULL, |finally| self.block(&finally.block));
        let line = self.nodes[block as usize].line;

        self.node(SHARP_AST_TRY, 0, line, &[block, catches, finally])
    }

    /// The classes a catch clause names are each its full name, in the order they are written.
    fn catch(&mut self, clause: &TryCatchClause) -> u32 {
        let mut classes = Vec::new();
        for class in union_members(&clause.hint) {
            let Hint::Identifier(class) = class else {
                unreachable!("semantics refuses the catch type `{class}`");
            };

            classes.push(self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)));
        }
        let line = self.nodes[classes[0] as usize].line;
        let classes = self.node(SHARP_AST_NAME_LIST, 0, line, &classes);
        let variable = clause
            .variable
            .as_ref()
            .map_or(NULL, |variable| self.local_name(0, self.line(variable.span), variable.name));
        let block = self.block(&clause.block);

        self.node(SHARP_AST_CATCH, 0, line, &[classes, variable, block])
    }

    /// The one `catch (Throwable)` a `try` runs as when a catch clause names a type parameter or a generic class, whose
    /// type arguments PHP's `catch` can't test. The caught value goes into the hidden variable `caught#N`, each clause
    /// tests it in the order the code writes them, as `is` does, and the first that matches gives its variable the
    /// value and runs its block. When none matches, the value is thrown again, so a `try` around this one catches it.
    fn tested_catch(&mut self, r#try: &Try) -> u32 {
        self.temporaries += 1;
        let caught = format!("caught#{}", self.temporaries).into_bytes();
        let line = self.line(
            &r#try.catch_clauses.first().unwrap_or_else(|| unreachable!("a catch clause tests type arguments")).hint,
        );

        let mut branches = Vec::new();
        for clause in &r#try.catch_clauses {
            let clause_line = self.line(&clause.hint);
            let mut tests = Vec::new();
            for member in union_members(&clause.hint) {
                let value = self.variable(member.span(), &caught);
                tests.push(match self.types.tested_type(member) {
                    Some(tested) => {
                        let tested = self.type_text(clause_line, &tested);

                        self.node(SHARP_AST_SHARP_IS_TYPE, 0, clause_line, &[value, tested])
                    }
                    None => {
                        let Hint::Identifier(class) = member else {
                            unreachable!("semantics refuses the catch type `{member}`");
                        };
                        let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));

                        self.node(SHARP_AST_INSTANCEOF, 0, clause_line, &[value, class])
                    }
                });
            }
            let condition = tests
                .into_iter()
                .reduce(|left, right| self.node(SHARP_AST_OR, 0, clause_line, &[left, right]))
                .unwrap_or_else(|| unreachable!("a catch clause names a type"));

            let body = match &clause.variable {
                Some(variable) => {
                    let target = self.variable(variable.span, variable.name);
                    let value = self.variable(variable.span, &caught);
                    let assignment = self.node(SHARP_AST_ASSIGN, 0, self.line(variable.span), &[target, value]);
                    let block = self.block(&clause.block);

                    self.node(SHARP_AST_STMT_LIST, 0, clause_line, &[assignment, block])
                }
                None => self.block(&clause.block),
            };
            branches.push(self.node(SHARP_AST_IF_ELEM, 0, clause_line, &[condition, body]));
        }

        let value = self.variable(r#try.block.right_brace, &caught);
        let rethrow = self.node(SHARP_AST_THROW, 0, line, &[value]);
        let rethrow = self.node(SHARP_AST_STMT_LIST, 0, line, &[rethrow]);
        branches.push(self.node(SHARP_AST_IF_ELEM, 0, line, &[NULL, rethrow]));
        let tests = self.node(SHARP_AST_IF, 0, line, &branches);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[tests]);

        let throwable = self.string(ZEND_NAME_FQ, line, b"Throwable");
        let classes = self.node(SHARP_AST_NAME_LIST, 0, line, &[throwable]);
        let variable = self.string(0, line, &caught);
        self.temporaries -= 1;

        self.node(SHARP_AST_CATCH, 0, line, &[classes, variable, body])
    }

    /// A call into plain PHP is checked as [`Types::call_check`] says, wherever its value goes, and plain PHP's value is
    /// renumbered where the checker reads it as a `List`, as [`Self::renumbered`] says.
    fn expression(&mut self, expression: &Expression) -> u32 {
        self.arriving(expression, Arrival::Read)
    }

    /// `expression`, as [`Self::expression`] lowers it where PHP# reads it, and as `arrival` says where plain PHP's
    /// array the checker reads as a `List` goes elsewhere.
    fn arriving(&mut self, expression: &Expression, arrival: Arrival) -> u32 {
        let line = self.line(expression);
        if let Some((_, name)) = self.null_safe_receivers.iter().find(|(span, _)| *span == expression.span()) {
            let name = name.clone();

            return self.variable(expression.span(), &name);
        }
        if let Some((link, receiver)) = self.untested_link(expression) {
            return self.null_safe_chain(expression, link, receiver);
        }

        let lowered = self.unchecked_expression(line, expression);
        let arrives_as_list = self.plain_value(expression) && self.types.arrives_as_list(expression);
        let (lowered, check) = match arrival {
            Arrival::Read if arrives_as_list => {
                (self.renumbered(expression, lowered), self.types.call_check(expression))
            }
            Arrival::Map(map) if arrives_as_list => (lowered, self.types.call_check_as(expression, map)),
            Arrival::Plain if arrives_as_list => (lowered, None),
            _ => (lowered, self.types.call_check(expression)),
        };
        match check {
            Some((text, target)) => self.verified(line, lowered, &text, target.as_bytes()),
            None => lowered,
        }
    }

    /// `lowered`, the value of `value`, renumbered where plain PHP gives it and the checker reads it as a `List`, spec
    /// section 12: plain PHP's `array<int, T>` arrives as a `List<T>`, as [`Types::arrives_as_list`] says, and its
    /// keys may have gaps. `\array_values` returns a list without gaps as it is. A value plain PHP declares a list is
    /// one already.
    ///
    /// ```php
    /// \is_array($renumbered#1 = \App\Users::all()) ? \array_values($renumbered#1) : $renumbered#1
    /// ```
    fn renumbered(&mut self, value: &Expression, lowered: u32) -> u32 {
        let span = value.span();
        let line = self.line(span);
        let taken = self.temporaries;
        let name = self.hidden("renumbered");
        let place = self.variable(span, &name);
        let first = self.node(SHARP_AST_ASSIGN, 0, line, &[place, lowered]);
        let is_array = self.global_call(line, b"is_array", &[first]);
        let read = self.variable(span, &name);
        let values = self.global_call(line, b"array_values", &[read]);
        let itself = self.variable(span, &name);
        self.temporaries = taken;

        self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_array, values, itself])
    }

    fn unchecked_expression(&mut self, line: u32, expression: &Expression) -> u32 {
        ensure_sufficient_stack(|| match expression {
            Expression::Literal(literal) => self.literal(literal),
            Expression::Parenthesized(parenthesized) => {
                let index = self.expression(parenthesized.expression);
                if let Expression::Conditional(_) = parenthesized.expression {
                    self.nodes[index as usize].attr = ZEND_PARENTHESIZED_CONDITIONAL;
                }

                index
            }
            Expression::Conditional(Conditional { condition, then: Some(then), r#else, .. }) => {
                let condition = self.expression(condition);
                let then = self.expression(then);
                let r#else = self.expression(r#else);

                self.node(SHARP_AST_CONDITIONAL, 0, line, &[condition, then, r#else])
            }
            Expression::ConstantAccess(name) => self.name(name, PlaceUse::Read),
            Expression::Is(_) | Expression::As(_) | Expression::PatternMatch(_) => {
                self.pattern(Node::Expression(expression))
            }
            // Only the PHP of a pattern form has a `$` variable: a pattern's name, or a variable PHP# cannot name.
            Expression::Variable(Variable::Direct(variable)) => {
                self.variable(variable.span, variable.name.strip_prefix(b"$").unwrap_or(variable.name))
            }
            Expression::Binary(Binary {
                lhs,
                operator: BinaryOperator::Instanceof(_),
                rhs: Expression::Identifier(class),
            }) => {
                let value = self.expression(lhs);
                // `is List<Any?>` on a `List` or null, as a list pattern's first test on a `List?`, is the null check,
                // and PHP's `\is_array` gives it without walking the elements.
                if self.types.tests_only_for_null(lhs, class) {
                    let line = self.line(class);
                    let function = self.string(ZEND_NAME_FQ, line, b"is_array");
                    let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

                    return self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
                }
                // A test of a type parameter or of a class with type arguments reads them while the code runs.
                if let Some(tested) = self.types.tested_type(class) {
                    let tested = self.type_text(line, &tested);

                    return self.node(SHARP_AST_SHARP_IS_TYPE, 0, line, &[value, tested]);
                }
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));

                self.node(SHARP_AST_INSTANCEOF, 0, line, &[value, class])
            }
            Expression::Match(r#match) => {
                let subject = self.expression(r#match.expression);
                let mut arms = Vec::new();
                for arm in r#match.arms.iter() {
                    let (conditions, value) = match arm {
                        MatchArm::Expression(arm) => {
                            let conditions: Vec<u32> =
                                arm.conditions.iter().map(|condition| self.expression(condition)).collect();

                            (self.expression_list(&conditions), arm.expression)
                        }
                        MatchArm::Default(arm) => (NULL, arm.expression),
                    };
                    let value = self.expression(value);
                    arms.push(self.node(SHARP_AST_MATCH_ARM, 0, self.line(arm), &[conditions, value]));
                }

                let arms = self.node(SHARP_AST_MATCH_ARM_LIST, 0, self.line(r#match.left_brace), &arms);

                self.node(SHARP_AST_MATCH, 0, line, &[subject, arms])
            }
            // Spec section 24: `+` on two strings joins them, and `/` on two ints divides toward zero. Section 19
            // orders two strings by their bytes and compares an int with a float as two floats, and an operator on
            // instances runs the one their class declares.
            Expression::Binary(binary) => {
                if matches!(binary.operator, BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_)) {
                    let negated = binary.operator.is_negated_equality();

                    return self.equality(line, Side::Expression(binary.lhs), Side::Expression(binary.rhs), negated);
                }
                if let Some(call) = self.declared_operator(line, binary) {
                    return call;
                }

                let operands = match binary.operator {
                    BinaryOperator::Addition(_) | BinaryOperator::Division(_) => {
                        self.operand_types(binary.lhs, binary.rhs)
                    }
                    BinaryOperator::LessThan(_)
                    | BinaryOperator::LessThanOrEqual(_)
                    | BinaryOperator::GreaterThan(_)
                    | BinaryOperator::GreaterThanOrEqual(_)
                    | BinaryOperator::Spaceship(_)
                        if self.orders_strings(binary) =>
                    {
                        Operands::Strings
                    }
                    _ => Operands::Other,
                };

                let lhs = self.expression(binary.lhs);
                let rhs = self.expression(binary.rhs);

                match (binary.operator, operands) {
                    (BinaryOperator::Addition(_), Operands::Strings) => {
                        self.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[lhs, rhs])
                    }
                    (BinaryOperator::Division(_), Operands::Ints) => self.intdiv(line, lhs, rhs),
                    (_, Operands::Strings) => self.ordinal(binary, line, lhs, rhs),
                    _ => {
                        let (kind, attr) = binary_kind(binary);

                        self.node(kind, attr, line, &[lhs, rhs])
                    }
                }
            }
            Expression::UnaryPrefix(unary) => {
                if let UnaryPrefixOperator::Negation(operator) = unary.operator
                    && let Some(name) = php_operator_name(&BinaryOperator::Subtraction(operator), 1)
                    && let Some(class) = self.types.operator_class(name, &[self.types.expression_type(unary.operand)])
                {
                    let operand = self.expression(unary.operand);

                    return self.operator_call(line, class, name, &[operand]);
                }

                if let Expression::ArrayAccess(access) = unary.operand
                    && unary.operator.is_increment_or_decrement()
                    && self.is_list(access.array)
                {
                    let decrement = matches!(unary.operator, UnaryPrefixOperator::PreDecrement(_));

                    return self.list_write(line, access, ListWrite::Step { decrement, postfix: false });
                }
                if let UnaryPrefixOperator::PreIncrement(_) | UnaryPrefixOperator::PreDecrement(_) = unary.operator
                    && let Some((class, name)) = self.static_accessor(unary.operand)
                {
                    let operation = if matches!(unary.operator, UnaryPrefixOperator::PreIncrement(_)) {
                        ZEND_ADD
                    } else {
                        ZEND_SUB
                    };

                    return self.accessor_step(class, name, operation, false, unary.operand.span());
                }

                let (kind, attr) = prefix_kind(&unary.operator);
                let operand = if unary.operator.is_increment_or_decrement() {
                    self.target(unary.operand, PlaceUse::Read)
                } else {
                    self.expression(unary.operand)
                };

                self.node(kind, attr, line, &[operand])
            }
            Expression::UnaryPostfix(unary) => {
                if let Expression::ArrayAccess(access) = unary.operand
                    && self.is_list(access.array)
                {
                    let decrement = matches!(unary.operator, UnaryPostfixOperator::PostDecrement(_));

                    return self.list_write(line, access, ListWrite::Step { decrement, postfix: true });
                }
                if let Some((class, name)) = self.static_accessor(unary.operand) {
                    let operation = if matches!(unary.operator, UnaryPostfixOperator::PostIncrement(_)) {
                        ZEND_ADD
                    } else {
                        ZEND_SUB
                    };

                    return self.accessor_step(class, name, operation, true, unary.operand.span());
                }

                let kind = match unary.operator {
                    UnaryPostfixOperator::PostIncrement(_) => SHARP_AST_POST_INC,
                    UnaryPostfixOperator::PostDecrement(_) => SHARP_AST_POST_DEC,
                };
                let operand = self.target(unary.operand, PlaceUse::Read);

                self.node(kind, 0, line, &[operand])
            }
            Expression::Assignment(assignment) => {
                // A static that runs accessors is written through its hidden `name#set`.
                if let Some((class, name)) = self.static_accessor(assignment.lhs) {
                    let span = assignment.lhs.span();
                    match assignment.operator {
                        AssignmentOperator::Assign(_) => {
                            let declared = self.types.property_type(assignment.lhs).cloned();
                            let value = self.crossed_value(assignment.rhs, declared.as_ref());

                            return self.accessor_write(class, name, value, span);
                        }
                        AssignmentOperator::Coalesce(_) => {
                            let read = self.accessor_call(class, name, b"get", &[], span);
                            let value = self.expression(assignment.rhs);
                            let write = self.accessor_write(class, name, value, span);

                            return self.node(SHARP_AST_COALESCE, 0, line, &[read, write]);
                        }
                        // A compound assignment reads and writes it in `compound_assignment`.
                        _ => {}
                    }
                }

                if let Expression::ArrayAccess(access) = assignment.lhs
                    && self.is_list(access.array)
                {
                    return self.list_write(line, access, ListWrite::Assignment(assignment));
                }
                if let Expression::ArrayAccess(access) = assignment.lhs
                    && match access.array.unparenthesized() {
                        array if self.static_accessor(array).is_some() => true,
                        Expression::ArrayAccess(element) => self.is_list(element.array),
                        // `Class.name` is a static property, which PHP gives no hooks.
                        Expression::Access(Access::Property(
                            property @ PropertyAccess {
                                object,
                                property: ClassLikeMemberSelector::Identifier(name),
                                ..
                            },
                        )) if self.names.static_property_class(property).is_none() => {
                            self.types.property_has_hooks(self.types.expression_type(object), name.value)
                        }
                        _ => false,
                    }
                {
                    return self.element_write(line, assignment, access);
                }

                // A compound assignment to an instance assigns what the operator its class declares returns.
                if self.declared_compound_operator(assignment).is_some() {
                    return self.compound_assignment(line, assignment, |this, target, value| {
                        this.compound_value(line, assignment, target, value)
                    });
                }

                match (&assignment.operator, self.operand_types_of(assignment)) {
                    (AssignmentOperator::Addition(_), Operands::Strings)
                        if self.static_accessor(assignment.lhs).is_some() =>
                    {
                        self.compound_assignment(line, assignment, |this, read, value| {
                            this.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[read, value])
                        })
                    }
                    (AssignmentOperator::Addition(_), Operands::Strings) => {
                        let lhs = self.target(assignment.lhs, PlaceUse::Read);
                        let rhs = self.expression(assignment.rhs);

                        self.node(SHARP_AST_ASSIGN_OP, ZEND_CONCAT, line, &[lhs, rhs])
                    }
                    (AssignmentOperator::Division(_), Operands::Ints) => {
                        self.compound_assignment(line, assignment, |this, target, value| {
                            this.compound_value(line, assignment, target, value)
                        })
                    }
                    (operator, _) if self.static_accessor(assignment.lhs).is_some() => {
                        let (_, attr) = assignment_kind(operator);

                        self.compound_assignment(line, assignment, |this, read, value| {
                            this.node(SHARP_AST_BINARY_OP, attr, line, &[read, value])
                        })
                    }
                    (operator, _) => {
                        let (kind, attr) = assignment_kind(operator);
                        // `=` and `??=` set the target without reading it, as PHP's do.
                        let place_use = match operator {
                            AssignmentOperator::Assign(_) | AssignmentOperator::Coalesce(_) => PlaceUse::Write,
                            _ => PlaceUse::Read,
                        };
                        let lhs = self.target(assignment.lhs, place_use);
                        let rhs = match operator {
                            AssignmentOperator::Assign(_) => {
                                let declared = self
                                    .types
                                    .property_type(assignment.lhs)
                                    .cloned()
                                    .or_else(|| self.typed_local(assignment.lhs).map(|(_, declared)| declared));

                                self.crossed_value(assignment.rhs, declared.as_ref())
                            }
                            _ => self.expression(assignment.rhs),
                        };

                        self.node(kind, attr, line, &[lhs, rhs])
                    }
                }
            }
            Expression::Call(Call::Method(call)) => self.method_call(expression, call),
            // `f?.(x)` whose conditional `null_safe_chain` built already tests `f`: the call of what `f` holds.
            Expression::Call(Call::NullSafeFunction(call)) => {
                let function = self.expression(call.function);
                let arguments = self.arguments(&call.argument_list);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            Expression::Call(Call::Function(FunctionCall {
                function: call_function @ Expression::Identifier(function),
                argument_list,
            })) => {
                // A local holds a function, so `f(x)` calls it as PHP's `$f($x)`, with the parameters of the closure
                // it holds, or of the `__invoke` of the object it holds. Any other name is the PHP function a pattern's
                // PHP shape calls, such as `is_string`, since the checker refuses a bare call in code.
                let (function, parameters) = match self.names.binding(function) {
                    Some(Binding::Local(_)) => (
                        self.variable(function.span(), function.value()),
                        self.types.closure_parameters(call_function).or_else(|| self.types.call_parameters(expression)),
                    ),
                    _ => (self.string(ZEND_NAME_FQ, self.line(function), function.value()), None),
                };

                // A call through a function value passes no caller's position, spec section 27, as a function type
                // has no parameter for it.
                let callee = self.types.sharp_callee(expression);
                self.call(None, argument_list, None, parameters, callee, |this, arguments| {
                    this.node(SHARP_AST_CALL, 0, line, &[function, arguments])
                })
            }
            Expression::Construct(Construct::Exit(_)) => {
                unreachable!("the checker refuses a bare call of PHP's `exit`, spec section 23")
            }
            Expression::Instantiation(instantiation) => self.instantiation(line, instantiation),
            Expression::With(with) => self.with(with),
            // `global.PHP_EOL` and `Pdf.DEFAULT_DPI` read the plain PHP constant the checker found, spec section 23.
            Expression::Access(Access::Property(_))
                if let Some(constant) = self.types.namespace_constant(expression) =>
            {
                let constant = self.string(ZEND_NAME_FQ, line, constant.as_bytes());

                self.node(SHARP_AST_CONST, 0, line, &[constant])
            }
            // Spec section 30: a named element of a tuple, which runs as a PHP list, is the index read of its position.
            Expression::Access(Access::Property(access))
                if let Some(index) = self.types.tuple_element_index(access) =>
            {
                let object = self.expression(access.object);
                let index = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = index as i64);

                self.node(SHARP_AST_DIM, 0, line, &[object, index])
            }
            // `Class.y`, and `y` read through a class value, is the fetch of the member the checker found on the class:
            // a constant or enum case, a static property, or a static method as a first-class callable.
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let (class, classes) = self.static_class(class);

                    self.static_member(expression, class, &classes, &access.property)
                }
                None if let Some(extension) = self.types.extension(expression) => {
                    self.extension_access(expression, &extension, access.object, None)
                }
                None => match self.class_value(access.object) {
                    Some((class, classes)) => self.static_member(expression, class, &classes, &access.property),
                    None => {
                        let is_method = self.is_method_value(access.object, &access.property);
                        let object = self.expression(access.object);
                        let member = self.member(&access.property);

                        if is_method {
                            self.method_value(expression, SHARP_AST_METHOD_CALL, object, member)
                        } else {
                            self.node(SHARP_AST_PROP, 0, line, &[object, member])
                        }
                    }
                },
            },
            // A link whose chain's conditional tests the receiver, which `untested_link` found, is the static call
            // through a class value, the extension's member, the lazy operation, the property call, or the plain method call.
            Expression::Call(Call::NullSafeMethod(call)) => {
                if let Some(lowered) = self.collection_call(expression, call.object, &call.argument_list, true) {
                    return lowered;
                }

                let tested = self.tested_links.contains(&expression.span());
                if tested && let Some(extension) = self.types.extension(expression) {
                    return self.extension_access(expression, &extension, call.object, Some(&call.argument_list));
                }
                if tested && self.types.is_lazy_operation(expression) {
                    return self.lazy_operation(line, call.object, &call.method, &call.argument_list);
                }
                if tested && let Some((class, _)) = self.class_value(call.object) {
                    let method = self.member(&call.method);
                    let type_arguments = self.types.call_type_arguments(expression);
                    let parameters = self.types.call_parameters(expression);
                    let callee = self.types.sharp_callee(expression);

                    return self.call(
                        Some(call.span()),
                        &call.argument_list,
                        type_arguments,
                        parameters,
                        callee,
                        |this, arguments| this.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments]),
                    );
                }

                let is_property_call = tested && self.is_property_call(expression, call.object);
                let object = if self.reads_plain_map(expression, call.object) {
                    let r#type = self.types.expression_type(call.object).clone();

                    self.crossed_value(call.object, Some(&r#type))
                } else if tested {
                    self.expression(call.object)
                } else {
                    self.null_safe_object(call.object)
                };
                let method = self.member(&call.method);
                if self.types.collection_method(expression).is_some() {
                    let arguments = self.collection_arguments(&call.argument_list);
                    let kind = if tested { SHARP_AST_METHOD_CALL } else { SHARP_AST_NULLSAFE_METHOD_CALL };

                    return self.node(kind, 0, line, &[object, method, arguments]);
                }
                let type_arguments = self.types.call_type_arguments(expression);
                let parameters = self.types.call_parameters(expression);
                let callee = self.types.sharp_callee(expression);

                self.call(
                    Some(call.span()),
                    &call.argument_list,
                    type_arguments,
                    parameters,
                    callee,
                    |this, arguments| {
                        if is_property_call {
                            let property = this.node(SHARP_AST_PROP, 0, line, &[object, method]);

                            this.node(SHARP_AST_CALL, 0, line, &[property, arguments])
                        } else if tested {
                            this.node(SHARP_AST_METHOD_CALL, 0, line, &[object, method, arguments])
                        } else {
                            this.node(SHARP_AST_NULLSAFE_METHOD_CALL, 0, line, &[object, method, arguments])
                        }
                    },
                )
            }
            // A link whose chain's conditional tests the receiver, which `untested_link` found, is the static member
            // read through a class value, the extension's member, the method value, or the plain property read.
            Expression::Access(Access::NullSafeProperty(access)) => {
                let tested = self.tested_links.contains(&expression.span());
                if tested && let Some(extension) = self.types.extension(expression) {
                    return self.extension_access(expression, &extension, access.object, None);
                }
                if tested && let Some((class, classes)) = self.class_value(access.object) {
                    return self.static_member(expression, class, &classes, &access.property);
                }

                let is_method = tested && self.is_method_value(access.object, &access.property);
                let object = if tested { self.expression(access.object) } else { self.null_safe_object(access.object) };
                let member = self.member(&access.property);

                if is_method {
                    self.method_value(expression, SHARP_AST_METHOD_CALL, object, member)
                } else if tested {
                    self.node(SHARP_AST_PROP, 0, line, &[object, member])
                } else {
                    self.node(SHARP_AST_NULLSAFE_PROP, 0, line, &[object, member])
                }
            }
            // `typeof` of a type the code runs with spells it then: a type parameter's type argument, a generic PHP#
            // class with its type arguments, and every type that is no class, as `int` or `List<Order>`.
            Expression::TypeOf(type_of) if let Some(tested) = self.types.tested_type(&type_of.r#type) => {
                let tested = self.type_text(line, &tested);

                self.node(SHARP_AST_SHARP_TYPE_OF, 0, line, &[tested])
            }
            Expression::TypeOf(type_of) => {
                let class = self.type_of_class(type_of);

                self.node(SHARP_AST_CLASS_NAME, 0, line, &[class])
            }
            // The checker proved the class has the property, so the running program needs only its name.
            Expression::NameOf(name_of) => self.string(0, line, name_of.property.value),
            Expression::Throw(throw) => {
                let exception = self.expression(throw.exception);

                self.node(SHARP_AST_THROW, 0, line, &[exception])
            }
            // `yield value;` is php-src's `yield $value;`, a `YIELD` of its value and no key, and `yield ...other;` is
            // `foreach ($other as $element#N) { yield $element#N; }`, so each element takes the generator's next
            // automatic key, where `yield from` would give the keys of `other`. `check_slice` keeps both as statements
            // of a method that returns `Iterable<T>`, so the loop stands where the statement does.
            Expression::Yield(Yield::Value(YieldValue { value: Some(value), .. })) => {
                let value = self.expression(value);

                self.node(SHARP_AST_YIELD, 0, line, &[value, NULL])
            }
            // Spec section 12: `yield ...plans` gives a `Map`'s entries.
            Expression::Yield(Yield::Spread(spread)) => {
                let iterator = if self.types.holds_map(spread.iterator) {
                    self.map_entries(spread.iterator)
                } else {
                    self.expression(spread.iterator)
                };
                self.temporaries += 1;
                let element = format!("element#{}", self.temporaries).into_bytes();
                let value = self.variable(spread.span(), &element);
                let r#yield = self.variable(spread.span(), &element);
                let r#yield = self.node(SHARP_AST_YIELD, 0, line, &[r#yield, NULL]);
                let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[r#yield]);
                self.temporaries -= 1;

                self.node(SHARP_AST_FOREACH, 0, line, &[iterator, value, NULL, body])
            }
            // Spec section 14.4: the engine compiles the statement of `a ?? return`, `continue` or `break` in the
            // middle of the expression, after the ZEND_SHARP_LEAVE that frees what the expression holds.
            Expression::Leave(Leave::Return(r#return)) => self.r#return(r#return),
            Expression::Propagate(propagate) => self.propagate(expression, propagate),
            Expression::Leave(Leave::Continue(_)) => self.node(SHARP_AST_CONTINUE, 0, line, &[NULL]),
            Expression::Leave(Leave::Break(_)) => self.node(SHARP_AST_BREAK, 0, line, &[NULL]),
            Expression::CompositeString(CompositeString::Interpolated(template)) => self.template(template),
            Expression::ArrowFunction(arrow_function) => self.arrow_function(arrow_function),
            Expression::Closure(closure) => self.closure(closure),
            Expression::Array(array) if self.is_set(expression) => self.set_literal(array),
            Expression::Array(array) if self.is_map(expression, array) => self.map_literal(array),
            Expression::Array(array) => self.array(array),
            Expression::ArrayAccess(access) => {
                let value = self.expression(access.array);
                let key = self.expression(access.index);

                self.node(SHARP_AST_DIM, 0, line, &[value, key])
            }
            _ => unreachable!("check_slice refuses the expression `{expression}`"),
        })
    }

    /// The fetch the read `read` makes of the static `member` of `class`, whose node is `class` and whose possible
    /// classes are `classes`: a class constant fetch for a constant or an enum case, a static property fetch, or the
    /// static method's value.
    fn static_member(
        &mut self,
        read: &Expression,
        class: u32,
        classes: &[&[u8]],
        member: &ClassLikeMemberSelector,
    ) -> u32 {
        let ClassLikeMemberSelector::Identifier(name) = member else {
            unreachable!("check_slice refuses the member name `{member}`");
        };
        let kind = agreed_kind(classes.iter().map(|class| self.types.member_declaration(class, name.value).kind));
        let line = self.line(read);
        if kind == DeclarationKind::CaseObject {
            let case = self.types.member_declaration(classes[0], name.value);
            let case_class = self.string(ZEND_NAME_FQ, line, case.class.as_bytes());
            let instance = self.string(0, line, case.name.as_bytes());
            // A plain case of a generic enum is the object of the type arguments the read gives it, spec section 31.
            let type_arguments = self.types.statics_type_arguments(read);
            let type_arguments: Vec<u32> =
                type_arguments.iter().map(|text| self.type_arguments_argument(line, text)).collect();
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &type_arguments);

            return self.node(SHARP_AST_STATIC_CALL, 0, line, &[case_class, instance, arguments]);
        }
        let member = self.member(member);

        match kind {
            DeclarationKind::Constant | DeclarationKind::EnumCase => {
                self.node(SHARP_AST_CLASS_CONST, 0, line, &[class, member])
            }
            DeclarationKind::StaticProperty => {
                self.static_property(read, class, classes, name.value, member, line, PlaceUse::Read)
            }
            DeclarationKind::StaticMethod => self.method_value(read, SHARP_AST_STATIC_CALL, class, member),
            kind => unreachable!("a class's member read names a static member, not a {kind:?}"),
        }
    }

    /// The static property `name` of `class`, whose node is `class` and whose possible classes are `classes`, that the
    /// read or write `access` reaches as `member`: PHP's `Class::$name`, or for a static field of a generic PHP# class
    /// the field of the entry of the access's type arguments, used as `place_use` says, as [`Self::statics_field`]
    /// writes it, spec section 11.
    fn static_property(
        &mut self,
        access: &Expression,
        class: u32,
        classes: &[&[u8]],
        name: &[u8],
        member: u32,
        line: u32,
        place_use: PlaceUse,
    ) -> u32 {
        let type_arguments = self.types.statics_type_arguments(access);
        // A static that runs accessors is read through its hidden `name#get`, `Class::{'name#get'}()`.
        if classes.iter().any(|class| self.types.is_static_accessor(class, name)) {
            let method = self.string(0, line, &[name, b"#get"].concat());
            let arguments: Vec<u32> = type_arguments
                .map(|type_arguments| self.type_arguments_argument(line, &type_arguments))
                .into_iter()
                .collect();
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);

            return self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments]);
        }
        let Some(type_arguments) = type_arguments else {
            return self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, member]);
        };

        let declaring = self.types.member_declaration(classes[0], name).class;
        let declaring = self.types.class_declaration(declaring.as_bytes()).name;

        self.statics_field(line, declaring.as_bytes(), &type_arguments, name, place_use)
    }

    /// The class name node of `typeof(X)` of a class `X`, which PHP writes `X::class`: the name `X`, without the type
    /// arguments of a plain PHP class, which carries none, or `static` for `Self`, the class a static method is called on.
    fn type_of_class(&mut self, type_of: &TypeOf) -> u32 {
        match &type_of.r#type {
            Hint::Identifier(class) => self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)),
            Hint::Generic(generic) => self.string(ZEND_NAME_FQ, self.line(generic.name), self.names.get(&generic.name)),
            Hint::Self_(keyword) => self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"static"),
            r#type => unreachable!("the analyzer records a tested type for `typeof({type})`"),
        }
    }

    /// The class node of a static member read or call through `object` and the classes it can be, when `object` is a
    /// class value: `typeof(X)` of a class is the name `X`, and any other class value is the value, as in PHP's
    /// `$type::y`.
    fn class_value(&mut self, object: &Expression) -> Option<(u32, Vec<&'lowering [u8]>)> {
        let classes = class_value_classes(self.types.expression_type(object))?;
        let class = match object {
            Expression::TypeOf(type_of) if self.types.tested_type(&type_of.r#type).is_none() => {
                self.type_of_class(type_of)
            }
            _ => self.expression(object),
        };

        Some((class, classes))
    }

    /// Whether `object` is a class value, whose members are its class's static members.
    fn is_class_value(&self, object: &Expression) -> bool {
        class_value_classes(self.types.expression_type(object)).is_some()
    }

    /// Whether `object.member` reads a method, which the read takes as a first-class callable. The member is the same
    /// kind on every class the receiver can be. A class intersected with others takes the member the first of them
    /// declares, as the checker's `resolve_method_value` does.
    fn is_method_value(&self, object: &Expression, member: &ClassLikeMemberSelector) -> bool {
        let (Some(receivers), ClassLikeMemberSelector::Identifier(name)) =
            (receiver_intersections(self.types.expression_type(object)), member)
        else {
            return false;
        };
        let kind = agreed_kind(receivers.into_iter().map(|classes| {
            classes
                .iter()
                .find_map(|class| self.types.declared_member(class, name.value))
                .unwrap_or_else(|| self.types.member_declaration(classes[0], name.value))
                .kind
        }));

        matches!(kind, DeclarationKind::Method { .. } | DeclarationKind::StaticMethod)
    }

    /// Whether the method call `call` on `object` runs the function the property of that name holds.
    fn is_property_call(&self, call: &Expression, object: &Expression) -> bool {
        receiver_classes(self.types.expression_type(object)).is_some()
            && matches!(self.types.call_target(call).kind, DeclarationKind::Property { .. })
    }

    /// A lazy operation of an `Iterable<T>`, spec section 12, on the engine's `Sharp\Sequence`, as
    /// `\Sharp\Sequence::from($object)->method(arguments)`. An `object` that is itself a lazy operation, null-safe or
    /// not, is already a `Sequence`, so a chain reads its first receiver through one `from`.
    fn lazy_operation(
        &mut self,
        line: u32,
        object: &Expression,
        method: &ClassLikeMemberSelector,
        arguments: &ArgumentList,
    ) -> u32 {
        let is_sequence =
            matches!(object.unparenthesized(), Expression::Call(Call::Method(_) | Call::NullSafeMethod(_)))
                && self.types.is_lazy_operation(object.unparenthesized());
        let mut sequence = self.expression(object);
        if !is_sequence {
            sequence = self.sequence_from(line, sequence);
        }
        let method = self.member(method);
        let arguments = self.arguments(arguments);

        self.node(SHARP_AST_METHOD_CALL, 0, line, &[sequence, method, arguments])
    }

    /// The `Map` `value` as the `Iterable<[TKey, TValue]>` of its entries, spec section 12, a `Sequence` of the object a
    /// PHP array plain PHP gave becomes first: `\Sharp\Sequence::from($plans)`.
    fn map_entries(&mut self, value: &Expression) -> u32 {
        let line = self.line(value.span());
        let own = self.types.expression_type(value).clone();
        let map = self.crossed_value(value, Some(&own));

        self.sequence_from(line, map)
    }

    /// `$name = $name instanceof \Sharp\Map ? \Sharp\Sequence::from($name) : $name;`, the statement that gives the
    /// parameter `parameter` a `Map` argument's entries, when it is a PHP# `Iterable<T>`, which holds a `Map` by its
    /// entries, spec section 12. A plain PHP caller's `Map` is a checking `Sequence` of them already.
    fn map_entries_parameter(&mut self, parameter: &FunctionLikeParameter) -> Option<u32> {
        let hint = parameter.hint.as_ref()?;
        if parameter.is_variadic() || !holds_entries(&self.hint_type(hint)) {
            return None;
        }

        let line = self.line(parameter);
        let variable = &parameter.variable;
        let [tested, source, kept, place] = [(); 4].map(|()| self.variable(variable.span, variable.name));
        let class = self.string(ZEND_NAME_FQ, line, b"Sharp\\Map");
        let is_map = self.node(SHARP_AST_INSTANCEOF, 0, line, &[tested, class]);
        let entries = self.sequence_from(line, source);
        let value = self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_map, entries, kept]);

        Some(self.node(SHARP_AST_ASSIGN, 0, line, &[place, value]))
    }

    /// `\Sharp\Sequence::from($source)`, the `Iterable<T>` that reads `source` lazily, spec section 12: a `Map`'s
    /// entries `[key, value]`, and every other collection's elements.
    fn sequence_from(&mut self, line: u32, source: u32) -> u32 {
        let class = self.string(ZEND_NAME_FQ, line, b"Sharp\\Sequence");
        let from = self.string(0, line, b"from");
        let source = self.node(SHARP_AST_ARG_LIST, 0, line, &[source]);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, from, source])
    }

    /// Whether both operands of an operator are strings or both ints, the two cases where spec section 24's operators
    /// differ from PHP's.
    fn operand_types(&self, lhs: &Expression, rhs: &Expression) -> Operands {
        let (lhs, rhs) = (self.types.expression_type(lhs), self.types.expression_type(rhs));
        if lhs.is_any_string() && rhs.is_any_string() {
            Operands::Strings
        } else if lhs.is_int() && rhs.is_int() {
            Operands::Ints
        } else {
            Operands::Other
        }
    }

    /// The operand types of `+=` and `/=`, the compound assignments whose operator differs from PHP's.
    fn operand_types_of(&self, assignment: &Assignment) -> Operands {
        match assignment.operator {
            AssignmentOperator::Addition(_) | AssignmentOperator::Division(_) => {
                self.operand_types(assignment.lhs, assignment.rhs)
            }
            _ => Operands::Other,
        }
    }

    /// Whether the ordering `binary` compares strings. The checker orders a string only against a string, and never a
    /// side that may be `null`, so a string on either side means strings on both.
    fn orders_strings(&self, binary: &Binary) -> bool {
        [binary.lhs, binary.rhs]
            .into_iter()
            .any(|operand| self.types.expression_type(operand).types.iter().any(TAtomic::is_any_string))
    }

    /// `\strcmp(lhs, rhs) <op> 0`, the ordering `binary` names of two strings by their bytes, so `"10" < "9"`, where
    /// PHP's `<` compares two numeric strings as numbers.
    fn ordinal(&mut self, binary: &Binary, line: u32, lhs: u32, rhs: u32) -> u32 {
        let function = self.string(ZEND_NAME_FQ, line, b"strcmp");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[lhs, rhs]);
        let comparison = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
        let zero = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = 0);
        let (kind, attr) = binary_kind(binary);

        self.node(kind, attr, line, &[comparison, zero])
    }

    /// `lhs == rhs`, or `lhs != rhs` when `negated`, as spec section 19 compares a value of `lhs`'s type with one of
    /// `rhs`'s: an unchecked `Any?` with a value [`compares_unchecked`] names, spec section 24, two class values by
    /// their whole type, as C# compares two `Type`s, and a value of a type parameter by its type argument's `==`, spec
    /// section 31, through `\Sharp\Internal\equals`, the `op_Equality` the class of an instance side declares, which
    /// lifts a null side itself, an int and a float as two floats, and `===` otherwise. `== null` tests for null and runs
    /// no operator. A collection search compares each element with its value through it, so a search never compares
    /// differently from `==`.
    fn equality(&mut self, line: u32, lhs: Side, rhs: Side, negated: bool) -> u32 {
        let (lhs_type, rhs_type) = (self.side_type(&lhs), self.side_type(&rhs));
        if compares_unchecked(lhs_type, rhs_type)
            || compares_unchecked(rhs_type, lhs_type)
            || [lhs_type, rhs_type].into_iter().any(|r#type| holds_class_value(r#type) || holds_type_parameter(r#type))
        {
            let function = self.string(ZEND_NAME_FQ, line, b"Sharp\\Internal\\equals");
            let lhs = self.side(&lhs);
            let rhs = self.side(&rhs);
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[lhs, rhs]);
            let call = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);

            return if negated { self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[call]) } else { call };
        }
        if let Some((class, name)) = self.declared_equality(lhs_type, rhs_type) {
            let lhs = self.side(&lhs);
            let rhs = self.side(&rhs);
            let call = self.operator_call(line, class, name, &[lhs, rhs]);

            return if negated { self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[call]) } else { call };
        }
        if mixes_numbers(lhs_type, rhs_type) {
            return self.float_equality(line, &lhs, &rhs, negated);
        }
        // The engine compares two `Map`s or two `Set`s by PHP#'s `==`, regardless of order, and an object with null as
        // unequal.
        if holds_table(lhs_type) || holds_table(rhs_type) {
            let lhs = self.side(&lhs);
            let rhs = self.side(&rhs);
            let attr = if negated { ZEND_IS_NOT_EQUAL } else { ZEND_IS_EQUAL };

            return self.node(SHARP_AST_BINARY_OP, attr, line, &[lhs, rhs]);
        }
        if !self.compares_identically(lhs_type, rhs_type) {
            return self.collection_equality(line, &lhs, &rhs, negated);
        }

        let lhs = self.side(&lhs);
        let rhs = self.side(&rhs);
        let attr = if negated { ZEND_IS_NOT_IDENTICAL } else { ZEND_IS_IDENTICAL };

        self.node(SHARP_AST_BINARY_OP, attr, line, &[lhs, rhs])
    }

    /// The class whose `op_Equality` `==` runs on a value of `lhs_type` and one of `rhs_type`, as the checker chose it,
    /// with the method's name, or none when `==` on them is `===` or two floats. `== null` runs none.
    fn declared_equality(&self, lhs_type: &TUnion, rhs_type: &TUnion) -> Option<(Word, &'static [u8])> {
        if lhs_type.is_null() || rhs_type.is_null() {
            return None;
        }
        let name = php_operator_name(&BinaryOperator::Equal(Span::zero()), 2)?;

        Some((self.types.operator_class(name, &[lhs_type, rhs_type])?, name))
    }

    /// The type of the value `side` holds.
    fn side_type<'side>(&self, side: &Side<'side, '_>) -> &'side TUnion
    where
        'lowering: 'side,
    {
        match side {
            Side::Expression(expression) => self.types.expression_type(expression),
            Side::Hidden { r#type, .. } | Side::Property { r#type, .. } | Side::Index { r#type, .. } => r#type,
        }
    }

    /// Whether `==` on a value of `lhs_type` and one of `rhs_type` is PHP's `===`, spec section 19: true for every value
    /// but a `List`, and for two lists whose elements compare so. Elements compared by an `op_Equality`, as floats, or
    /// as `Map`s or `Set`s need their own `==`.
    fn compares_identically(&self, lhs_type: &TUnion, rhs_type: &TUnion) -> bool {
        let (Some(lhs_lists), Some(rhs_lists)) = (collection_kind(lhs_type), collection_kind(rhs_type)) else {
            return true;
        };
        let (lhs_element, rhs_element) = (self.types.element_type(lhs_type), self.types.element_type(rhs_type));

        lhs_lists
            && rhs_lists
            && self.declared_equality(&lhs_element, &rhs_element).is_none()
            && !mixes_numbers(&lhs_element, &rhs_element)
            && self.compares_identically(&lhs_element, &rhs_element)
    }

    /// `lhs == rhs` of two `List`s, spec section 19, which holds when both hold as many elements and each element of
    /// `lhs` equals the one at its index in `rhs` by the element's own `==`:
    /// `(fn ($lhs#1, $rhs#2) => \count($lhs#1) === \count($rhs#2) && \array_all($lhs#1, fn ($element#3, $key#4) =>
    /// \array_key_exists($key#4, $rhs#2) && $element#3 == $rhs#2[$key#4]))(lhs, rhs)`. When a side may be null, null
    /// equals only null first: `$lhs#1 === null || $rhs#2 === null ? $lhs#1 === $rhs#2 : …`. `!=` is its `!`.
    fn collection_equality(&mut self, line: u32, lhs: &Side, rhs: &Side, negated: bool) -> u32 {
        let span = Self::side_span(lhs);
        let (lhs_type, rhs_type) = (self.side_type(lhs), self.side_type(rhs));
        let (lhs_element, rhs_element) = (self.types.element_type(lhs_type), self.types.element_type(rhs_type));
        let nullable = lhs_type.can_be_null() || rhs_type.can_be_null();

        let taken = self.temporaries;
        let [lhs_name, rhs_name, element_name, key_name] = [("lhs", 1), ("rhs", 2), ("element", 3), ("key", 4)]
            .map(|(name, n)| format!("{name}#{}", taken + n).into_bytes());
        self.temporaries += 4;

        let element = Side::Hidden { span, name: element_name.clone(), r#type: &lhs_element };
        let other = Side::Index { span, array: rhs_name.clone(), key: key_name.clone(), r#type: &rhs_element };
        let element_equal = self.equality(line, element, other, false);
        let key = self.variable(span, &key_name);
        let rhs_array = self.variable(span, &rhs_name);
        let has_key = self.global_call(line, b"array_key_exists", &[key, rhs_array]);
        let element_matches = self.node(SHARP_AST_AND, 0, line, &[has_key, element_equal]);
        let predicate = self.hidden_arrow_function(span, &[&element_name, &key_name], element_matches);

        let lhs_array = self.variable(span, &lhs_name);
        let every = self.global_call(line, b"array_all", &[lhs_array, predicate]);
        let lhs_count = self.variable(span, &lhs_name);
        let lhs_count = self.global_call(line, b"count", &[lhs_count]);
        let rhs_count = self.variable(span, &rhs_name);
        let rhs_count = self.global_call(line, b"count", &[rhs_count]);
        let same_count = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[lhs_count, rhs_count]);
        let mut equal = self.node(SHARP_AST_AND, 0, line, &[same_count, every]);
        if nullable {
            let is_null = |lowering: &mut Self, name: &[u8]| {
                let variable = lowering.variable(span, name);
                let null = lowering.zval(line, sharp_value::SHARP_NULL, |_| {});

                lowering.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[variable, null])
            };
            let either_null = [is_null(self, &lhs_name), is_null(self, &rhs_name)];
            let either_null = self.node(SHARP_AST_OR, 0, line, &either_null);
            let both = [self.variable(span, &lhs_name), self.variable(span, &rhs_name)];
            let identical = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &both);
            equal = self.node(SHARP_AST_CONDITIONAL, 0, line, &[either_null, identical, equal]);
        }
        let compare = self.hidden_arrow_function(span, &[&lhs_name, &rhs_name], equal);
        self.temporaries -= 4;

        let operands = [self.side(lhs), self.side(rhs)];
        let operands = self.node(SHARP_AST_ARG_LIST, 0, line, &operands);
        let call = self.node(SHARP_AST_CALL, 0, line, &[compare, operands]);

        if negated { self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[call]) } else { call }
    }

    /// `\function(arguments)`, a call of the global PHP function `function`.
    fn global_call(&mut self, line: u32, function: &[u8], arguments: &[u32]) -> u32 {
        let function = self.string(ZEND_NAME_FQ, line, function);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, arguments);

        self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
    }

    /// A read of `side`: the expression, lowered, its hidden variable, or its hidden variable's property.
    fn side(&mut self, side: &Side) -> u32 {
        match side {
            Side::Expression(expression) => self.expression(expression),
            Side::Hidden { span, name, .. } => self.variable(*span, name),
            Side::Property { span, object, property, .. } => {
                let line = self.line(*span);
                let object = self.variable(*span, object);
                let property = self.string(0, line, property);

                self.node(SHARP_AST_PROP, 0, line, &[object, property])
            }
            Side::Index { span, array, key, .. } => {
                let array = self.variable(*span, array);
                let key = self.variable(*span, key);

                self.node(SHARP_AST_DIM, 0, self.line(*span), &[array, key])
            }
        }
    }

    fn side_span(side: &Side) -> Span {
        match side {
            Side::Expression(expression) => expression.span(),
            Side::Hidden { span, .. } | Side::Property { span, .. } | Side::Index { span, .. } => *span,
        }
    }

    /// `lhs == rhs` of an int and a float as two floats, `(float) $count === $ratio`, as spec section 19 compares
    /// numbers by value. A side that may hold an int is cast. When a side may be null, both sides go into hidden
    /// `$operand#N`s, each running once, and null equals only null:
    /// `(($operand#1 = lhs) === null) === (($operand#2 = rhs) === null) && ($operand#1 === null || (float) $operand#1
    /// === $operand#2)`, so `(float) null`, which is `0.0`, never compares. `!=` is its `!`. The names are taken
    /// before the operands are lowered, so an equality inside an operand takes the next ones.
    fn float_equality(&mut self, line: u32, lhs: &Side, rhs: &Side, negated: bool) -> u32 {
        let (lhs_type, rhs_type) = (self.side_type(lhs), self.side_type(rhs));
        let [cast_lhs, cast_rhs] = [lhs_type, rhs_type].map(|r#type| number_kinds(r#type).is_some_and(|[int, _]| int));
        if !lhs_type.is_nullable() && !rhs_type.is_nullable() {
            let lhs = self.side(lhs);
            let lhs = self.float(line, lhs, cast_lhs);
            let rhs = self.side(rhs);
            let rhs = self.float(line, rhs, cast_rhs);
            let attr = if negated { ZEND_IS_NOT_IDENTICAL } else { ZEND_IS_IDENTICAL };

            return self.node(SHARP_AST_BINARY_OP, attr, line, &[lhs, rhs]);
        }

        self.temporaries += 2;
        let lhs_name = format!("operand#{}", self.temporaries - 1).into_bytes();
        let rhs_name = format!("operand#{}", self.temporaries).into_bytes();
        let (lhs_span, rhs_span) = (Self::side_span(lhs), Self::side_span(rhs));
        let lhs = self.side(lhs);
        let rhs = self.side(rhs);

        let lhs_variable = self.variable(lhs_span, &lhs_name);
        let lhs_stored = self.node(SHARP_AST_ASSIGN, 0, line, &[lhs_variable, lhs]);
        let lhs_is_null = self.is_null(line, lhs_stored);
        let rhs_variable = self.variable(rhs_span, &rhs_name);
        let rhs_stored = self.node(SHARP_AST_ASSIGN, 0, line, &[rhs_variable, rhs]);
        let rhs_is_null = self.is_null(line, rhs_stored);
        let same_nulls = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[lhs_is_null, rhs_is_null]);

        let lhs_read = self.variable(lhs_span, &lhs_name);
        let both_null = self.is_null(line, lhs_read);
        let lhs_read = self.variable(lhs_span, &lhs_name);
        let lhs_float = self.float(line, lhs_read, cast_lhs);
        let rhs_read = self.variable(rhs_span, &rhs_name);
        let rhs_float = self.float(line, rhs_read, cast_rhs);
        let same_floats = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[lhs_float, rhs_float]);
        let same_values = self.node(SHARP_AST_OR, 0, line, &[both_null, same_floats]);
        let equality = self.node(SHARP_AST_AND, 0, line, &[same_nulls, same_values]);
        self.temporaries -= 2;

        if negated { self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[equality]) } else { equality }
    }

    /// `value === null`.
    fn is_null(&mut self, line: u32, value: u32) -> u32 {
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[value, null])
    }

    /// `value` cast to float when `cast` holds, as `(float) value`.
    fn float(&mut self, line: u32, value: u32, cast: bool) -> u32 {
        if cast { self.node(SHARP_AST_CAST, IS_DOUBLE, line, &[value]) } else { value }
    }

    /// `\intdiv(lhs, rhs)`, which divides two ints toward zero.
    fn intdiv(&mut self, line: u32, lhs: u32, rhs: u32) -> u32 {
        let function = self.string(ZEND_NAME_FQ, line, b"intdiv");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[lhs, rhs]);

        self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
    }

    /// A write into a `Map` that is the element of a `List`, a place by spec section 12, or that a hooked property
    /// holds, which PHP gives no reference to. Only the engine's collection of the element or the property reaches where it is
    /// stored, through the property's hooks: `list[i][key] = value` is `$list[$i]->put($key, $value)`, which gives the
    /// value, and `list[i][key] ??= value` is `$list[$i][$key] ?? $list[$i]->put($key, $value)`, which holds the index
    /// and the key that run something in hidden variables. `this.hooked[key] = value` is
    /// `$this->hooked->put($key, $value)`. The checker refuses every other operator there, since it reads the element
    /// of the `Map` bare.
    fn element_write(&mut self, line: u32, assignment: &Assignment, access: &ArrayAccess) -> u32 {
        let coalesce = match assignment.operator {
            AssignmentOperator::Assign(_) => false,
            AssignmentOperator::Coalesce(_) => true,
            _ => unreachable!("the checker refuses `{}` on a `Map` element, which reads it bare", assignment.operator),
        };
        let mut element = self.collection_receiver(access.array, coalesce);
        let mut key = if coalesce { self.reread(access.index, "key") } else { Reread::once(access.index) };
        let held = coalesce.then(|| {
            let element = self.read_collection(&mut element, line, false);
            let key = self.read(&mut key);

            self.node(SHARP_AST_DIM, 0, line, &[element, key])
        });
        let place = self.read_collection(&mut element, line, false);
        let read = self.read(&mut key);
        // The variables stay taken until the value is lowered, as in a compound assignment.
        let value = self.expression(assignment.rhs);
        self.release(key);
        self.release_collection(element);
        let put = self.string(0, line, b"put");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[read, value]);
        let put = self.node(SHARP_AST_METHOD_CALL, 0, line, &[place, put, arguments]);

        match held {
            Some(held) => self.node(SHARP_AST_COALESCE, 0, line, &[held, put]),
            None => put,
        }
    }

    /// A write to the element of a `List` at an index, which spec section 12 makes replace the element and throw
    /// `OutOfRangeException` past the end, where PHP's write would append or leave a gap. Every form runs the engine's
    /// `Sharp\Collection::set` on the list, which reaches a local, a property, a hooked property's `set` and the element
    /// of another `List`, and gives the value it stores: `list[i] = v` is `$list->set($i, $v)`, and `list[i] op= v`
    /// and `++list[i]` are `$list->set($i, $list[$i] op v)`, whose list and index run once, in hidden variables when
    /// they run something. `list[i]++` is `$list->set($i, ($old#N = $list[$i]) + 1) ? $old#N : $old#N`, which gives
    /// the element before the write, as PHP has no expression that runs one and gives another. The checker refuses
    /// `??=`, which reads the index quietly.
    fn list_write(&mut self, line: u32, access: &ArrayAccess, write: ListWrite) -> u32 {
        let reads = !matches!(write, ListWrite::Assignment(Assignment { operator: AssignmentOperator::Assign(_), .. }));
        let mut list = self.collection_receiver(access.array, reads);
        let mut index = if reads { self.reread(access.index, "index") } else { Reread::once(access.index) };
        let place = self.read_collection(&mut list, line, false);
        let written = self.read(&mut index);
        let mut element = |lowering: &mut Self| {
            let list = lowering.read_collection(&mut list, line, false);
            let index = lowering.read(&mut index);

            lowering.node(SHARP_AST_DIM, 0, line, &[list, index])
        };
        let mut old = None;
        // The variables stay taken until the value is lowered, as in a compound assignment.
        let value = match write {
            ListWrite::Assignment(assignment @ Assignment { operator: AssignmentOperator::Assign(_), .. }) => {
                self.expression(assignment.rhs)
            }
            ListWrite::Assignment(assignment) => {
                let element = element(self);
                let value = self.expression(assignment.rhs);

                self.compound_value(line, assignment, element, value)
            }
            ListWrite::Step { decrement, postfix } => {
                let mut element = element(self);
                if postfix {
                    self.temporaries += 1;
                    let name = format!("old#{}", self.temporaries).into_bytes();
                    let variable = self.variable(access.span(), &name);
                    element = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, element]);
                    old = Some(name);
                }
                let one = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = 1);

                self.node(SHARP_AST_BINARY_OP, if decrement { ZEND_SUB } else { ZEND_ADD }, line, &[element, one])
            }
        };
        if old.is_some() {
            self.temporaries -= 1;
        }
        self.release(index);
        self.release_collection(list);
        let set = self.string(0, line, b"set");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[written, value]);
        let set = self.node(SHARP_AST_METHOD_CALL, 0, line, &[place, set, arguments]);
        let Some(old) = old else {
            return set;
        };
        let [given, otherwise] = [(); 2].map(|()| self.variable(access.span(), &old));

        self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[set, given, otherwise])
    }

    /// The class and the PHP name of the operator a compound assignment runs on instances, as `op_Addition` for `+=`,
    /// or none when its operands are no instances whose class declares it.
    fn declared_compound_operator(&self, assignment: &Assignment) -> Option<(Word, &'static [u8])> {
        let operator = compound_operator(&assignment.operator)?;
        let name = php_operator_name(&operator, 2)?;
        let types = self.types;
        let class = types
            .operator_class(name, &[types.expression_type(assignment.lhs), types.expression_type(assignment.rhs)])?;

        Some((class, name))
    }

    /// What the compound assignment `assignment` writes, from the lowered `read` of its target and its lowered `value`:
    /// the operator its operands' class declares, and otherwise spec section 24's: `+` on two strings joins them, and
    /// `/` on two ints divides toward zero.
    fn compound_value(&mut self, line: u32, assignment: &Assignment, read: u32, value: u32) -> u32 {
        if let Some((class, name)) = self.declared_compound_operator(assignment) {
            return self.operator_call(line, class, name, &[read, value]);
        }

        match (&assignment.operator, self.operand_types_of(assignment)) {
            (AssignmentOperator::Addition(_), Operands::Strings) => {
                self.node(SHARP_AST_BINARY_OP, ZEND_CONCAT, line, &[read, value])
            }
            (AssignmentOperator::Division(_), Operands::Ints) => self.intdiv(line, read, value),
            (AssignmentOperator::Coalesce(_), _) => {
                unreachable!("the checker refuses `??=` on a `List` index, which reads it quietly")
            }
            (operator, _) => {
                let (_, operation) = assignment_kind(operator);

                self.node(SHARP_AST_BINARY_OP, operation, line, &[read, value])
            }
        }
    }

    /// `target op= value` as `target = operation(target, value)`, for an operator that differs from PHP's: `/=` on ints
    /// runs `\intdiv`, and `+=` on an instance runs the `op_Addition` its class declares. The target's receiver runs
    /// once: one that is not a local or `this` goes into a hidden `$receiver#N`, which the read sets and the write
    /// reads. php-src refuses an assignment as the object of a written property, and reads a variable it writes through
    /// after the value, so `$receiver#N->total = \intdiv(($receiver#N = $this->next())->total, 2)` runs the receiver
    /// once. The variable stays taken until the value is lowered, so a compound assignment in the value takes another.
    fn compound_assignment(
        &mut self,
        line: u32,
        assignment: &Assignment,
        operation: impl FnOnce(&mut Self, u32, u32) -> u32,
    ) -> u32 {
        // A static that runs accessors reads through `name#get` and writes through `name#set`.
        if let Some((class, name)) = self.static_accessor(assignment.lhs) {
            let read = self.accessor_call(class, name, b"get", &[], assignment.lhs.span());
            let value = self.expression(assignment.rhs);
            let result = operation(self, read, value);

            return self.accessor_write(class, name, result, assignment.lhs.span());
        }

        let receiver = match assignment.lhs {
            Expression::Access(Access::Property(access))
                if self.names.static_property_class(access).is_none() && !self.is_local_or_this(access.object) =>
            {
                Some(access)
            }
            _ => None,
        };

        let (target, read) = match receiver {
            Some(access) => {
                self.temporaries += 1;
                let name = format!("receiver#{}", self.temporaries).into_bytes();
                let variable = self.variable(access.object.span(), &name);
                let member = self.member(&access.property);
                let target = self.node(SHARP_AST_PROP, 0, line, &[variable, member]);

                let variable = self.variable(access.object.span(), &name);
                let object = self.expression(access.object);
                let object = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, object]);
                let member = self.member(&access.property);
                let read = self.node(SHARP_AST_PROP, 0, line, &[object, member]);

                (target, read)
            }
            None => (self.target(assignment.lhs, PlaceUse::Write), self.expression(assignment.lhs)),
        };
        let value = self.expression(assignment.rhs);
        let result = operation(self, read, value);
        if receiver.is_some() {
            self.temporaries -= 1;
        }

        self.node(SHARP_AST_ASSIGN, 0, line, &[target, result])
    }

    /// The call of the static method a PHP# operator on instances runs, as `\App\Money::op_Addition($a, $b)` for
    /// `a + b`: an ordering compares what `op_Comparison` returns with 0, and `==` and `!=` are [`Self::equality`]'s.
    /// Operands with no instance run none.
    fn declared_operator(&mut self, line: u32, binary: &Binary) -> Option<u32> {
        let declared = match binary.operator {
            BinaryOperator::LessThan(span)
            | BinaryOperator::LessThanOrEqual(span)
            | BinaryOperator::GreaterThan(span)
            | BinaryOperator::GreaterThanOrEqual(span)
            | BinaryOperator::Spaceship(span) => BinaryOperator::Spaceship(span),
            operator if operator.is_arithmetic() => operator,
            _ => return None,
        };
        let name = php_operator_name(&declared, 2)?;
        let types = self.types;
        let class =
            types.operator_class(name, &[types.expression_type(binary.lhs), types.expression_type(binary.rhs)])?;

        let lhs = self.expression(binary.lhs);
        let rhs = self.expression(binary.rhs);
        let call = self.operator_call(line, class, name, &[lhs, rhs]);

        Some(match binary.operator {
            BinaryOperator::LessThan(_)
            | BinaryOperator::LessThanOrEqual(_)
            | BinaryOperator::GreaterThan(_)
            | BinaryOperator::GreaterThanOrEqual(_) => {
                let zero = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = 0);
                let (kind, attr) = binary_kind(binary);

                self.node(kind, attr, line, &[call, zero])
            }
            _ => call,
        })
    }

    /// `\Class::name(arguments)`, the static method `class` declares to run an operator or an extension's member.
    fn operator_call(&mut self, line: u32, class: Word, name: &[u8], arguments: &[u32]) -> u32 {
        let class = self.string(ZEND_NAME_FQ, line, class.as_bytes());
        let method = self.string(0, line, name);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, arguments);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments])
    }

    /// Whether `expression` is a local, a parameter or `this`, which reading twice runs nothing twice.
    fn is_local_or_this(&self, expression: &Expression) -> bool {
        matches!(
            expression,
            Expression::ConstantAccess(name)
                if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::This))
        )
    }

    /// The value `value` reads of the method `member` of `target`, an object for a `METHOD_CALL` `call` or a class for
    /// a `STATIC_CALL` one. A method that isn't generic is PHP's first-class callable, `$object->member(...)`. A PHP#
    /// generic method's would run with no type arguments, so it is a closure that calls the method with those the
    /// checker took from the value's function type, on `target` read once where the value is read:
    /// `(function ($receiver) { return function (...$arguments) use ($receiver) { return
    /// $receiver->member(...$arguments, <T>); }; })($object)`.
    fn method_value(&mut self, value: &Expression, call: sharp_kind, target: u32, member: u32) -> u32 {
        let line = self.line(value);
        let Some(type_arguments) = self.types.method_value_type_arguments(value) else {
            let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

            return self.node(call, 0, line, &[target, member, callable]);
        };

        let names_method_type_arguments = type_arguments.contains('#');
        let text = self.type_text(line, &type_arguments);
        let type_arguments = self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[NULL, text]);
        let arguments = self.variable(value.span(), b"arguments");
        let spread = self.node(SHARP_AST_UNPACK, 0, line, &[arguments]);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[spread, type_arguments]);
        let receiver = self.variable(value.span(), b"receiver");
        let method_call = self.node(call, 0, line, &[receiver, member, arguments]);
        let captured: &[&[u8]] =
            if names_method_type_arguments { &[b"receiver", TYPE_ARGUMENTS] } else { &[b"receiver"] };
        let caller = self.returning_closure(value, b"arguments", ZEND_PARAM_VARIADIC, captured, method_call);
        let captured: &[&[u8]] = if names_method_type_arguments { &[TYPE_ARGUMENTS] } else { &[] };
        let reader = self.returning_closure(value, b"receiver", 0, captured, caller);
        let target = self.node(SHARP_AST_ARG_LIST, 0, line, &[target]);

        self.node(SHARP_AST_CALL, 0, line, &[reader, target])
    }

    /// A `CLOSURE` the lowering writes, with the one parameter `parameter` and its `flags`, that captures each local
    /// of `captured` by value and returns `value`.
    fn returning_closure(
        &mut self,
        span: impl HasSpan,
        parameter: &[u8],
        flags: u32,
        captured: &[&[u8]],
        value: u32,
    ) -> u32 {
        let line = self.line(&span);
        let name = self.string(0, line, parameter);
        let parameter = self.node(SHARP_AST_PARAM, flags, line, &[NULL, name, NULL, NULL, NULL, NULL]);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[parameter]);
        let mut uses = Vec::new();
        for captured in captured {
            uses.push(self.string(0, line, captured));
        }
        let uses = if uses.is_empty() { NULL } else { self.node(SHARP_AST_CLOSURE_USES, 0, line, &uses) };
        let value = self.node(SHARP_AST_RETURN, 0, line, &[value]);
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &[value]);

        self.declaration(SHARP_AST_CLOSURE, 0, &span, &span, b"", &[parameters, uses, body, NULL, NULL])
    }

    /// The highest null-safe link in `chain` that PHP's `?->` cannot write, a method value, a property call, an
    /// extension's member, a lazy operation of an `Iterable`, a call of a function that may be null, `f?.(x)`, or any link below a collection method call that runs as PHP on the array or below an
    /// extension's member, which runs as a static call, with its receiver, when `chain` is a member
    /// read, a call or an index whose chain holds one no conditional tests yet. A null there would reach the PHP
    /// function, where `?->` skips only the rest of its own chain. The walk stops at a receiver a hidden variable
    /// holds, whose links its own conditional tests.
    fn untested_link<'chain, 'ast>(
        &self,
        chain: &'chain Expression<'ast>,
    ) -> Option<(&'chain Expression<'ast>, &'chain Expression<'ast>)> {
        let mut link = chain;
        let mut below_php_call = false;
        loop {
            if self.null_safe_receivers.iter().any(|(span, _)| *span == link.span()) {
                return None;
            }

            link = match link {
                Expression::Access(Access::Property(access)) => {
                    below_php_call = below_php_call || self.types.extension(link).is_some();

                    access.object
                }
                Expression::Call(Call::Method(call)) => {
                    below_php_call = below_php_call
                        || self.php_collection_method(link).is_some()
                        || self.types.extension(link).is_some();

                    call.object
                }
                Expression::ArrayAccess(access) => access.array,
                Expression::Parenthesized(parenthesized) => parenthesized.expression,
                Expression::Access(Access::NullSafeProperty(access)) => {
                    if !self.tested_links.contains(&link.span())
                        && (below_php_call
                            || self.types.extension(link).is_some()
                            || self.is_class_value(access.object)
                            || self.is_method_value(access.object, &access.property))
                    {
                        return Some((link, access.object));
                    }

                    access.object
                }
                Expression::Call(Call::NullSafeMethod(call)) => {
                    if !self.tested_links.contains(&link.span())
                        && (below_php_call
                            || self.types.extension(link).is_some()
                            || self.types.is_lazy_operation(link)
                            || self.is_class_value(call.object)
                            || self.is_property_call(link, call.object))
                    {
                        return Some((link, call.object));
                    }

                    call.object
                }
                // PHP has no `?->` that calls a function, spec section 14.4's `f?.(x)`.
                Expression::Call(Call::NullSafeFunction(call)) => {
                    if !self.tested_links.contains(&link.span()) {
                        return Some((link, call.function));
                    }

                    call.function
                }
                _ => return None,
            };
        }
    }

    /// `try result`, spec section 31, as `($try#N = result) instanceof \Sharp\Result\Err ? return new
    /// \Sharp\Result\Err($try#N->error) : $try#N->value`. The returned `Err` holds the same error with the type arguments
    /// of the function's `Result`, which the checker recorded at `expression`, and the engine compiles the `return` in
    /// the conditional as a leave, which frees what the expression holds.
    fn propagate(&mut self, expression: &Expression, propagate: &Propagate) -> u32 {
        const ERR: &[u8] = b"Sharp\\Result\\Err";

        let line = self.line(expression);
        self.temporaries += 1;
        let name = format!("try#{}", self.temporaries).into_bytes();
        let variable = self.variable(propagate.value.span(), &name);
        let value = self.expression(propagate.value);
        let held = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, value]);
        let class = self.string(ZEND_NAME_FQ, line, ERR);
        let failed = self.node(SHARP_AST_INSTANCEOF, 0, line, &[held, class]);

        let variable = self.variable(propagate.value.span(), &name);
        let property = self.string(0, line, b"error");
        let error = self.node(SHARP_AST_PROP, 0, line, &[variable, property]);
        let class = self.string(ZEND_NAME_FQ, line, ERR);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[error]);
        let new = self.node(SHARP_AST_NEW, 0, line, &[class, arguments]);
        let type_arguments = self.types.type_arguments(ERR, expression.span());
        let new = self.with_type_arguments(line, new, type_arguments);
        let returned = self.node(SHARP_AST_RETURN, 0, line, &[new]);

        let variable = self.variable(propagate.value.span(), &name);
        let property = self.string(0, line, b"value");
        let value = self.node(SHARP_AST_PROP, 0, line, &[variable, property]);
        self.temporaries -= 1;

        self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[failed, returned, value])
    }

    /// A chain whose `untested` null-safe link PHP's `?->` cannot write, as
    /// `($nullsafe#N = receiver) === null ? null : chain`, with the chain reading `$nullsafe#N` in place of the
    /// receiver. The conditional wraps the whole chain, which a null receiver skips as `?->` does. A local receiver is
    /// tested and read as itself.
    fn null_safe_chain(&mut self, chain: &Expression, untested: &Expression, receiver: &Expression) -> u32 {
        let line = self.line(chain);
        let is_local = self.is_local_or_this(receiver);
        let tested = if is_local {
            self.null_safe_object(receiver)
        } else {
            self.temporaries += 1;
            let name = format!("nullsafe#{}", self.temporaries).into_bytes();
            let variable = self.variable(receiver.span(), &name);
            let value = self.null_safe_object(receiver);
            self.null_safe_receivers.push((receiver.span(), name));

            self.node(SHARP_AST_ASSIGN, 0, line, &[variable, value])
        };
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
        let condition = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[tested, null]);

        self.tested_links.push(untested.span());
        let rest = self.expression(chain);
        self.tested_links.pop();
        if !is_local {
            self.null_safe_receivers.pop();
            self.temporaries -= 1;
        }

        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[condition, null, rest])
    }

    /// The object of `?.`. A `Map` index there reads a missing key as null, which `?.` skips, where a bare read throws
    /// on a missing key (spec section 12), so it is `($x[$k] ?? null)?->name`. A `List` index is the bare index itself,
    /// which throws on a missing index: `?.` skips only a null element, and a change after it reaches the element the
    /// list stores, as `rows[0]?.add(5)` does.
    fn null_safe_object(&mut self, object: &Expression) -> u32 {
        let value = self.expression(object);
        let Expression::ArrayAccess(access) = object.unparenthesized() else {
            return value;
        };
        if self.is_list(access.array) {
            return value;
        }

        let line = self.line(object);
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_COALESCE, 0, line, &[value, null])
    }

    /// Spec section 3: a lambda captures the variable itself. PHP's `fn` captures by value what its body reads, which
    /// is the variable itself when nothing writes it, so a lambda with an expression body that captures no written
    /// local is an `ARROW_FUNC`, as php-src's grammar builds `fn`, with its expression as its body. Any other is a
    /// `CLOSURE` whose body returns the expression, as is one that captures its method's type arguments, and one with a
    /// `Set` parameter, which `fn` has no statement before its expression to make a `Set`.
    fn arrow_function(&mut self, arrow_function: &ArrowFunction) -> u32 {
        let lambda = arrow_function.span();
        let parameters = self.lambda_parameters(lambda, &arrow_function.parameter_list);
        let ((mut statements, value, return_type), captures_type_arguments) = self.lambda_body(|lowering| {
            (
                lowering.collection_parameters(&arrow_function.parameter_list.parameters),
                lowering.expression(arrow_function.expression),
                lowering.lambda_type(lambda),
            )
        });
        if statements.is_empty()
            && !captures_type_arguments
            && !self.names.captures(&lambda).iter().any(|(_, local)| self.names.is_written(local))
        {
            return self.declaration(
                SHARP_AST_ARROW_FUNC,
                0,
                lambda,
                arrow_function.expression,
                b"",
                &[parameters, NULL, value, return_type, NULL],
            );
        }

        let uses = self.closure_uses(lambda, captures_type_arguments);
        let line = self.line(arrow_function.expression);
        statements.push(self.node(SHARP_AST_RETURN, 0, line, &[value]));
        let body = self.node(SHARP_AST_STMT_LIST, 0, line, &statements);

        self.declaration(SHARP_AST_CLOSURE, 0, lambda, lambda, b"", &[parameters, uses, body, return_type, NULL])
    }

    /// A lambda with a block body is a `CLOSURE`, as php-src's grammar builds `function () use (…) { … }`, which makes
    /// each `Set` parameter a `Set` first.
    fn closure(&mut self, closure: &Closure) -> u32 {
        let lambda = closure.span();
        let parameters = self.lambda_parameters(lambda, &closure.parameter_list);
        let ((body, return_type), captures_type_arguments) = self.lambda_body(|lowering| {
            let mut statements = lowering.collection_parameters(&closure.parameter_list.parameters);
            for statement in &closure.body.statements {
                statements.push(lowering.statement(statement));
            }

            (
                lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(&closure.body), &statements),
                lowering.lambda_type(lambda),
            )
        });
        let uses = self.closure_uses(lambda, captures_type_arguments);

        self.declaration(SHARP_AST_CLOSURE, 0, lambda, lambda, b"", &[parameters, uses, body, return_type, NULL])
    }

    /// The result type of the lambda at `lambda`, which has no PHP type, with its whole type, as
    /// [`Self::function_type`] gives it. A type that names the method's own type parameter as `#i` makes the lambda
    /// capture the method's type arguments, as its body's would.
    fn lambda_type(&mut self, lambda: Span) -> u32 {
        let function_type = self.types.lambda_type(lambda);

        self.function_type(NULL, function_type, self.line(lambda))
    }

    /// The result type of a PHP# method or lambda declaration: a `SHARP_TYPE_ARGS` of `php_type` and `function_type`'s
    /// text, which `is` tests a closure of it by, or of its result type alone when no function type takes a closure of
    /// it, or `php_type` alone when PHP# can't write either. The unit lists the function type's fingerprint, or zero
    /// without one, since its effects come from other files' bodies.
    fn function_type(&mut self, php_type: u32, function_type: FunctionType, line: u32) -> u32 {
        let FunctionType { name, text, result } = function_type;
        self.function_types.push(Read {
            name: name.into_bytes(),
            fingerprint: text.as_ref().map_or(0, |text| xxh3_64(text.as_bytes())),
        });
        // A function whose effects no function type can say carries its result type alone, which a generator's
        // elements and a map stage's are read from, spec section 12.
        self.carried_type(php_type, text.or(result), line)
    }

    /// A `SHARP_TYPE_ARGS` of `php_type` and the type text `carried`, or `php_type` alone without one.
    fn carried_type(&mut self, php_type: u32, carried: Option<String>, line: u32) -> u32 {
        let Some(carried) = carried else {
            return php_type;
        };
        let text = self.type_text(line, &carried);

        self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[php_type, text])
    }

    /// The parameters of the lambda at `lambda`, each with its type or a null type, as PHP writes a parameter without
    /// one, and the metadata plain PHP's call of it is checked against, as a method's.
    fn lambda_parameters(&mut self, lambda: Span, list: &FunctionLikeParameterList) -> u32 {
        let mut parameters = Vec::new();
        for parameter in &list.parameters {
            let hint = parameter.hint.as_ref().map_or(NULL, |hint| self.hint(hint));
            parameters.push(self.parameter_of_type(parameter, hint, None));
        }
        let line = self.line(list);
        let metadata = self.types.lambda_metadata(lambda);
        parameters.extend(self.parameter_metadata(metadata, line));

        self.node(SHARP_AST_PARAM_LIST, 0, line, &parameters)
    }

    /// The `use` list of a `CLOSURE`: each local the lambda captures, in first-use order, by reference with
    /// `ZEND_BIND_REF` when code writes it and by value otherwise, then by value the method's hidden local of its type
    /// arguments when the lambda captures them, or null when it captures none.
    fn closure_uses(&mut self, lambda: Span, captures_type_arguments: bool) -> u32 {
        let line = self.line(lambda);
        let mut uses = Vec::new();
        for &(name, local) in self.names.captures(&lambda) {
            let attr = if self.names.is_written(&local) { ZEND_BIND_REF } else { 0 };
            uses.push(self.local_name(attr, line, name));
        }
        if captures_type_arguments {
            uses.push(self.string(0, line, TYPE_ARGUMENTS));
        }

        if uses.is_empty() { NULL } else { self.node(SHARP_AST_CLOSURE_USES, 0, line, &uses) }
    }

    /// Lowers a lambda's body, which runs in a frame of its own, outside any loop of the method around it. It also
    /// gives whether the body, or a lambda in it, wrote a type text that names the method's own type parameter as
    /// `#i`, so the lambda captures the method's type arguments, as each lambda around it does.
    fn lambda_body<T>(&mut self, lower: impl FnOnce(&mut Self) -> T) -> (T, bool) {
        let loop_depth = std::mem::replace(&mut self.loop_depth, 0);
        let returns = self.returns.take();
        let around = std::mem::take(&mut self.captures_type_arguments);
        let body = lower(self);
        self.loop_depth = loop_depth;
        self.returns = returns;
        let captures_type_arguments = self.captures_type_arguments;
        self.captures_type_arguments |= around;

        (body, captures_type_arguments)
    }

    /// A list literal is an `ARRAY` with `ZEND_ARRAY_SYNTAX_SHORT`, as php-src's grammar builds `[…]`. Each element is
    /// an `ARRAY_ELEM` of its value and null, and a spread is an `UNPACK` of its value.
    fn array(&mut self, array: &Array) -> u32 {
        let elements: Vec<u32> = array.elements.iter().map(|element| self.array_element(element)).collect();

        self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, self.line(array), &elements)
    }

    fn array_element(&mut self, element: &ArrayElement) -> u32 {
        let line = self.line(element);
        match element {
            ArrayElement::Value(element) => {
                let value = self.expression(element.value);

                self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[value, NULL])
            }
            ArrayElement::Variadic(element) => {
                let value = self.expression(element.value);

                self.node(SHARP_AST_UNPACK, 0, line, &[value])
            }
            _ => unreachable!("the checker refuses a `Map` entry in a `List` literal"),
        }
    }

    /// A `Map` literal is the `\Sharp\Map` of its entries, key then value, in the order the literal writes them, so a
    /// later key replaces an earlier one: `new \Sharp\Map([$k1, $v1, $k2, $v2])`, which a property default may hold. A
    /// spread puts the other `Map`'s entries in place,
    /// `...\array_merge([], ...\array_map(null, $map->keys(), \iterator_to_array($map, false)))`, which reads a `Map`
    /// that runs something once, through a hidden `$spread#N`. `[:]` is `new \Sharp\Map()`.
    fn map_literal(&mut self, array: &Array) -> u32 {
        let line = self.line(array);
        if array.elements.is_empty() {
            return self.collection_new(line, b"Sharp\\Map", None);
        }

        let mut keys_and_values = Vec::new();
        for element in &array.elements {
            match element {
                ArrayElement::KeyValue(entry) => {
                    let key = self.expression(entry.key);
                    keys_and_values.push(self.node(SHARP_AST_ARRAY_ELEM, 0, self.line(entry), &[key, NULL]));
                    let value = self.expression(entry.value);
                    keys_and_values.push(self.node(SHARP_AST_ARRAY_ELEM, 0, self.line(entry), &[value, NULL]));
                }
                ArrayElement::Variadic(spread) => {
                    let line = self.line(spread);
                    let mut map = self.reread(spread.value, "spread");
                    let receiver = self.read(&mut map);
                    let keys = self.string(0, line, b"keys");
                    let no_arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
                    let keys = self.node(SHARP_AST_METHOD_CALL, 0, line, &[receiver, keys, no_arguments]);
                    let map_read = self.read(&mut map);
                    self.release(map);
                    let renumbered = self.zval(line, sharp_value::SHARP_FALSE, |_| {});
                    let values = self.global_call(line, b"iterator_to_array", &[map_read, renumbered]);
                    let pair = self.zval(line, sharp_value::SHARP_NULL, |_| {});
                    let pairs = self.global_call(line, b"array_map", &[pair, keys, values]);
                    let pairs = self.node(SHARP_AST_UNPACK, 0, line, &[pairs]);
                    let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);
                    let entries = self.global_call(line, b"array_merge", &[empty, pairs]);
                    keys_and_values.push(self.node(SHARP_AST_UNPACK, 0, line, &[entries]));
                }
                _ => unreachable!("the checker refuses a `List` element in a `Map` literal"),
            }
        }
        let keys_and_values = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &keys_and_values);

        self.collection_new(line, b"Sharp\\Map", Some(keys_and_values))
    }

    /// `new \Sharp\Map(argument)` or `new \Sharp\Set(argument)`, as `class` names it, or without an argument.
    fn collection_new(&mut self, line: u32, class: &[u8], argument: Option<u32>) -> u32 {
        let class = self.string(ZEND_NAME_FQ, line, class);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, argument.as_slice());

        self.node(SHARP_AST_NEW, 0, line, &[class, arguments])
    }

    /// Whether the analysis gave `expression` a `Set` type, as it gives a list literal where a `Set` is declared.
    fn is_set(&self, expression: &Expression) -> bool {
        self.types
            .expression_type(expression)
            .types
            .iter()
            .any(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_))))
    }

    /// Whether the literal `array` of `expression` is a `Map`: `[:]`, or one the analysis gave a `Map` type, as it gives
    /// a literal of entries. `[]` is the empty `List`, though the analysis types it as the empty array of either.
    fn is_map(&self, expression: &Expression, array: &Array) -> bool {
        array.colon.is_some()
            || (!array.elements.is_empty()
                && self
                    .types
                    .expression_type(expression)
                    .types
                    .iter()
                    .any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_)))))
    }

    /// Whether the analysis gave `expression` a `List` type, whose index spec section 12 makes a place.
    fn is_list(&self, expression: &Expression) -> bool {
        self.types
            .expression_type(expression)
            .types
            .iter()
            .any(|atomic| matches!(atomic, TAtomic::Array(TArray::List(_))))
    }

    /// A `Set` literal is the `\Sharp\Set` of the list literal of its elements, which holds each once:
    /// `new \Sharp\Set(["vip", ...$more])`, which a property default may hold. A spread `Set` gives its elements, as a
    /// loop over it does.
    fn set_literal(&mut self, array: &Array) -> u32 {
        let line = self.line(array);
        let list = self.array(array);

        self.collection_new(line, b"Sharp\\Set", Some(list))
    }

    /// `$p = \is_array($p) ? new \Sharp\Set($p) : $p;` for each parameter whose type holds a `Map` or a `Set`, at any
    /// depth, since plain PHP passes a PHP array where PHP# passes the object, and
    /// `$p = \array_map(fn ($collection#N) => …, $p);` for a variadic one, which keeps each argument's position or name.
    /// A promoted one is the property its constructor declares, `$this->p = …`. Null and the object stay as they are.
    fn collection_parameters<'parameter, 'ast>(
        &mut self,
        parameters: impl IntoIterator<Item = &'parameter FunctionLikeParameter<'ast>>,
    ) -> Vec<u32>
    where
        'ast: 'parameter,
    {
        let mut statements = Vec::new();
        for parameter in parameters {
            if self.promoted_beside
                && parameter.is_promoted_property()
                && let Some(behaviors) = &parameter.behaviors
            {
                statements.extend(self.behavior_creations(&parameter.variable, behaviors));
            }
            let Some(r#type) = self.crossing_type(parameter) else {
                statements.extend(self.map_entries_parameter(parameter));
                if self.promoted_beside && parameter.is_promoted_property() {
                    let place = self.parameter_place(parameter);
                    let value = self.variable(parameter.variable.span, parameter.variable.name);
                    statements.push(self.node(SHARP_AST_ASSIGN, 0, self.line(parameter), &[place, value]));
                }
                continue;
            };
            let line = self.line(parameter);
            let variable = &parameter.variable;
            let taken = self.temporaries;
            let crossed = if parameter.is_variadic() {
                let each = self.hidden("collection");
                let first = self.variable(variable.span, &each);
                let crossed = self.crossed(variable.span, &r#type, &each, first);
                let crossed = self.hidden_arrow_function(variable.span, &[&each], crossed);
                let values = self.variable(variable.span, variable.name);

                self.global_call(line, b"array_map", &[crossed, values])
            } else {
                let first = self.variable(variable.span, variable.name);

                self.crossed(variable.span, &r#type, variable.name, first)
            };
            self.temporaries = taken;
            let place = self.parameter_place(parameter);
            statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[place, crossed]));
        }

        statements
    }

    /// The backed enum `r#type` is, which a `Map` keyed by it takes plain PHP's backing values for. A type the analysis
    /// gives names it as an enum, and one a hint declares as the class name it writes.
    fn backed_enum<'r#type>(&self, r#type: &'r#type TUnion) -> Option<&'r#type [u8]> {
        let name = match r#type.types.as_ref() {
            [TAtomic::Object(TObject::Enum(r#enum))] => r#enum.name.as_bytes(),
            [TAtomic::Reference(TReference::Symbol { name, .. })] => name.as_bytes(),
            _ => return None,
        };

        self.types.backing_type(name).is_some().then_some(name)
    }

    /// Whether `r#type` is a `Map` keyed by a backed enum, which reaches a plain PHP array with each case's backing value
    /// as its key, spec section 12.
    fn keyed_by_backed_enum(&self, r#type: &TUnion) -> bool {
        r#type.types.iter().any(|atomic| match atomic {
            TAtomic::Array(map @ TArray::Keyed(_)) => {
                let (key_type, _) = self.types.array_parameters(map);

                key_type.types.iter().all(|key| {
                    matches!(key, TAtomic::Object(TObject::Enum(r#enum)) if self.types.backing_type(r#enum.name.as_bytes()).is_some())
                })
            }
            _ => false,
        })
    }

    /// The PHP array plain PHP takes for `value`, a `Map` keyed by a backed enum, with each case's backing value as its
    /// key, or a PHP array or null as it is: `(fn ($map#N) => \is_object($map#N)
    /// ? \array_combine(\array_column($map#N->keys(), 'value'), $map#N->values()) : $map#N)($value)`.
    fn backed_key_array(&mut self, span: Span, value: u32) -> u32 {
        let line = self.line(span);
        let taken = self.temporaries;
        let map = self.hidden("map");
        let [tested, key_source, value_source, kept] = [(); 4].map(|()| self.variable(span, &map));
        let is_object = self.global_call(line, b"is_object", &[tested]);
        let [keys, values] = [(key_source, b"keys".as_slice()), (value_source, b"values")].map(|(map, method)| {
            let method = self.string(0, line, method);
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);

            self.node(SHARP_AST_METHOD_CALL, 0, line, &[map, method, arguments])
        });
        let value_name = self.string(0, line, b"value");
        let backing = self.global_call(line, b"array_column", &[keys, value_name]);
        let array = self.global_call(line, b"array_combine", &[backing, values]);
        let body = self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_object, array, kept]);
        let crossing = self.hidden_arrow_function(span, &[&map], body);
        self.temporaries = taken;
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

        self.node(SHARP_AST_CALL, 0, line, &[crossing, arguments])
    }

    /// `\App\Status::from($value)`, its value cast to a `string` for a `string`-backed enum, as PHP holds an all-digit
    /// `string` key as an `int`.
    fn backed_case(&mut self, line: u32, class: &[u8], mut value: u32) -> u32 {
        if !self.types.backing_type(class).is_some_and(TAtomic::is_int) {
            value = self.node(SHARP_AST_CAST, IS_STRING, line, &[value]);
        }
        let name = self.string(ZEND_NAME_FQ, line, class);
        let method = self.string(0, line, b"from");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[name, method, arguments])
    }

    /// The declared type of `parameter` where it holds a `Map` or a `Set`, at any depth, which
    /// [`Self::collection_parameters`] makes the objects of what plain PHP passes.
    pub(super) fn crossing_type(&self, parameter: &FunctionLikeParameter) -> Option<TUnion> {
        let r#type = self.hint_type(parameter.hint.as_ref()?);

        self.crossing_array(&r#type).is_some().then_some(r#type)
    }

    /// The type `hint` declares, as the checker reads it, a type parameter in it as a class of its name, which holds no
    /// `Map` or `Set`.
    fn hint_type(&self, hint: &Hint) -> TUnion {
        let class = (!self.class.is_empty()).then(|| word(self.class));

        get_union_from_hint(hint, class, self.names, &TypeResolutionContext::new())
    }

    /// The one collection `r#type` holds where the value holds a `Map` or a `Set`, at any depth, for [`Self::crossed`]
    /// to build. Where the value may be one of several collections, the array alone doesn't say which, and none is.
    fn crossing_array<'r#type>(&self, r#type: &'r#type TUnion) -> Option<&'r#type TArray> {
        let mut arrays = r#type.types.iter().filter_map(|atomic| match atomic {
            TAtomic::Array(array) => Some(array),
            _ => None,
        });
        let (Some(array), None) = (arrays.next(), arrays.next()) else {
            return None;
        };
        let holds_collection =
            !matches!(array, TArray::List(_)) || self.crossing_array(&self.types.array_parameters(array).1).is_some();

        holds_collection.then_some(array)
    }

    /// The value of the variable `name`, of `r#type`, with each `Map` and `Set` it holds made the object where plain PHP
    /// gave a PHP array, spec section 12: the one conversion where plain PHP's value enters PHP#. `first` is the first
    /// read of `name`, which may assign it. The caller asks only about a type [`Self::crossing_array`] finds a
    /// collection in. A `Map` is `\Sharp\Map::from($name)` and a `Set` is `new \Sharp\Set($name)`. PHP holds an
    /// all-digit `string` key as an `int`, so a `Map` whose key type holds `string` and no `int` takes each key back as
    /// its `string`:
    /// `new \Sharp\Map(\array_merge([], ...\array_map(fn ($key#N, $value#M) => [(string) $key#N, $value#M],
    /// \array_keys($name), $name)))`, and a `Map` keyed by a backed enum takes each backing value back as its case,
    /// `\App\Status::from($key#N)`, as plain PHP passes it. The elements cross first,
    /// `\array_map(fn ($element#N) => …, $name)`.
    ///
    /// ```php
    /// \is_array($name) ? \Sharp\Map::from($name) : $name
    /// ```
    fn crossed(&mut self, span: Span, r#type: &TUnion, name: &[u8], first: u32) -> u32 {
        let Some(array) = self.crossing_array(r#type) else {
            unreachable!("the caller crosses only a value that holds a `Map` or a `Set`");
        };
        let line = self.line(span);
        let (key_type, element_type) = self.types.array_parameters(array);
        let taken = self.temporaries;
        let mut values = self.variable(span, name);
        if self.crossing_array(&element_type).is_some() {
            let element = self.hidden("element");
            let element_read = self.variable(span, &element);
            let each = self.crossed(span, &element_type, &element, element_read);
            let each = self.hidden_arrow_function(span, &[&element], each);
            values = self.global_call(line, b"array_map", &[each, values]);
        }
        let built = match (array, self.key_crossing(&key_type)) {
            (TArray::List(_), _) => values,
            (TArray::Set(_), _) => self.collection_new(line, b"Sharp\\Set", Some(values)),
            (TArray::Keyed(_), crossing @ (KeyCrossing::Text | KeyCrossing::Case(_))) => {
                let [key, value] = ["key", "value"].map(|base| self.hidden(base));
                let key_read = self.variable(span, &key);
                let key_text = match crossing {
                    KeyCrossing::Case(class) => self.backed_case(line, class, key_read),
                    _ => self.node(SHARP_AST_CAST, IS_STRING, line, &[key_read]),
                };
                let key_text = self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[key_text, NULL]);
                let value_read = self.variable(span, &value);
                let value_read = self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[value_read, NULL]);
                let entry = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[key_text, value_read]);
                let entry = self.hidden_arrow_function(span, &[&key, &value], entry);
                let keys = self.variable(span, name);
                let keys = self.global_call(line, b"array_keys", &[keys]);
                let entries = self.global_call(line, b"array_map", &[entry, keys, values]);
                let entries = self.node(SHARP_AST_UNPACK, 0, line, &[entries]);
                let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);
                let keys_and_values = self.global_call(line, b"array_merge", &[empty, entries]);

                self.collection_new(line, b"Sharp\\Map", Some(keys_and_values))
            }
            (TArray::Keyed(_), KeyCrossing::AsHeld) => {
                let class = self.string(ZEND_NAME_FQ, line, b"Sharp\\Map");
                let from = self.string(0, line, b"from");
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[values]);

                self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, from, arguments])
            }
        };
        self.temporaries = taken;
        let is_array = self.global_call(line, b"is_array", &[first]);
        let itself = self.variable(span, name);

        self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_array, built, itself])
    }

    /// Whether `r#type` is a `Map` whose keys [`Self::crossed`] takes back from a PHP array, as [`Self::key_crossing`]
    /// says: one keyed by `string` and no `int`, or by a backed enum.
    fn takes_keys_back(&self, r#type: &TUnion) -> bool {
        self.crossing_array(r#type).is_some_and(|array| {
            matches!(array, TArray::Keyed(_))
                && !matches!(self.key_crossing(&self.types.array_parameters(array).0), KeyCrossing::AsHeld)
        })
    }

    /// How a `Map` keyed by `key_type` takes back a key plain PHP's array holds, as [`Self::crossed`] and
    /// [`Self::crossing_plan`] both cross it.
    fn key_crossing<'r#type>(&self, key_type: &'r#type TUnion) -> KeyCrossing<'r#type> {
        match self.backed_enum(key_type) {
            Some(class) => KeyCrossing::Case(class),
            None if key_type.has_string() && !key_type.has_int() => KeyCrossing::Text,
            None => KeyCrossing::AsHeld,
        }
    }

    /// The plan `#[\Sharp\Crossing]` gives the engine, so a PHP array plain PHP writes to a `Map` or `Set` property of
    /// `r#type` crosses as [`Self::crossed`] crosses a parameter: `['map', key, values]`, `['set', elements]` or
    /// `['list', elements]`. The key is null for a key as PHP holds it, `'string'` for an `int` key back as its
    /// `string`, or the backed enum whose case each backing value is, and an inner plan is null where that part holds
    /// no `Map` or `Set`.
    fn crossing_plan(&mut self, line: u32, r#type: &TUnion) -> u32 {
        let Some(array) = self.crossing_array(r#type) else {
            return self.zval(line, sharp_value::SHARP_NULL, |_| {});
        };
        let (key_type, element_type) = self.types.array_parameters(array);
        let inner = self.crossing_plan(line, &element_type);
        let mut parts = match array {
            TArray::List(_) => vec![self.string(0, line, b"list")],
            TArray::Set(_) => vec![self.string(0, line, b"set")],
            TArray::Keyed(_) => {
                let kind = self.string(0, line, b"map");
                let key = match self.key_crossing(&key_type) {
                    KeyCrossing::AsHeld => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
                    KeyCrossing::Text => self.string(0, line, b"string"),
                    KeyCrossing::Case(class) => self.string(0, line, class),
                };

                vec![kind, key]
            }
        };
        parts.push(inner);
        let elements: Vec<u32> =
            parts.into_iter().map(|part| self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[part, NULL])).collect();

        self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements)
    }

    /// Where `parameter` holds its value in the body: its variable, or its property when it is promoted.
    fn parameter_place(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let variable = &parameter.variable;
        if !parameter.is_promoted_property() {
            return self.variable(variable.span, variable.name);
        }

        let line = self.line(variable.span);
        let this = self.variable(variable.span, b"this");
        let property = self.string(0, line, variable.name);

        self.node(SHARP_AST_PROP, 0, line, &[this, property])
    }

    /// The PHP a call of one of the [`INLINED_COLLECTION_METHODS`] runs on the array, or none for any other call. A
    /// change reaches a property through the property, which runs its hooks, an element of a `List` through a
    /// `Sharp\Collection` method, and a value that lives nowhere changes as a copy. A null-safe call tests its
    /// receiver first and gives null for null, which a missing key of a `Map` reads as.
    fn collection_call(
        &mut self,
        call: &Expression,
        receiver: &Expression,
        arguments: &ArgumentList,
        null_safe: bool,
    ) -> Option<u32> {
        let (collection_name, method, reads_twice) = self.php_collection_method(call)?;
        let line = self.line(call);
        let element = || {
            arguments
                .arguments
                .first()
                .map(Argument::value)
                .unwrap_or_else(|| unreachable!("the checker refuses a collection method call without its element"))
        };

        let mut collection = self.collection_receiver(receiver, reads_twice || null_safe);
        let test = null_safe.then(|| {
            let receiver = self.read_collection(&mut collection, line, true);

            self.is_null(line, receiver)
        });
        let lowered = match (collection_name, method.as_slice()) {
            ("List", b"contains" | b"indexof" | b"remove") => {
                let element_type = self.types.element_type(self.types.expression_type(receiver));
                let search = match method.as_slice() {
                    b"contains" => Search::Contains,
                    b"indexof" => Search::IndexOf,
                    _ => Search::Remove,
                };

                self.element_search(call.span(), &mut collection, &element_type, element(), search)
            }
            ("List", b"last") => {
                let list = self.read_collection(&mut collection, line, false);
                let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);
                let is_empty = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[list, empty]);
                let class = self.string(ZEND_NAME_FQ, line, b"OutOfRangeException");
                let message = self.string(0, line, b"The list is empty.");
                let message = self.node(SHARP_AST_ARG_LIST, 0, line, &[message]);
                let exception = self.node(SHARP_AST_NEW, 0, line, &[class, message]);
                let throw = self.node(SHARP_AST_THROW, 0, line, &[exception]);
                let list = self.read_collection(&mut collection, line, false);
                let function = self.string(ZEND_NAME_FQ, line, b"array_last");
                let list = self.node(SHARP_AST_ARG_LIST, 0, line, &[list]);
                let element = self.node(SHARP_AST_CALL, 0, line, &[function, list]);

                self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[is_empty, throw, element])
            }
            ("List", b"clear") => {
                let place = self.read_collection(&mut collection, line, false);
                match collection {
                    CollectionReceiver::Value(_) => {
                        unreachable!("the checker refuses a change to a value with no place")
                    }
                    CollectionReceiver::Index(..) => {
                        let clear = self.string(0, line, b"clear");
                        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);

                        self.node(SHARP_AST_METHOD_CALL, 0, line, &[place, clear, arguments])
                    }
                    _ => {
                        let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);

                        self.node(SHARP_AST_ASSIGN, 0, line, &[place, empty])
                    }
                }
            }
            ("List", b"count") => {
                let list = self.read_collection(&mut collection, line, false);

                self.global_call(line, b"count", &[list])
            }
            // `getOrNull(i)` is `($list[$i] ?? null)`, the read that gives null past either end.
            ("List", b"getornull") => {
                let list = self.read_collection(&mut collection, line, false);
                let index = self.expression(element());
                let read = self.node(SHARP_AST_DIM, 0, line, &[list, index]);
                let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

                self.node(SHARP_AST_COALESCE, 0, line, &[read, null])
            }
            // `toSet()` is the `Set` a literal of the elements makes.
            ("List", b"toset") => {
                let list = self.read_collection(&mut collection, line, false);

                self.collection_new(line, b"Sharp\\Set", Some(list))
            }
            _ => unreachable!(
                "INLINED_COLLECTION_METHODS names `({collection_name}, {})`, which has no PHP here",
                String::from_utf8_lossy(&method)
            ),
        };
        self.release_collection(collection);

        Some(match test {
            Some(test) => {
                let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

                self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[test, null, lowered])
            }
            None => lowered,
        })
    }

    /// A `List`'s `contains(value)`, `indexOf(value)` or `remove(value)`, as `search` names it: whether an element of
    /// `element_type` equals `value` by spec section 19's `==`, the index of the first one that does or null, or whether
    /// `remove` removed it. Where `==` on them is `===` and neither the receiver nor the value runs code, so the value
    /// may be read first, the search is PHP's strict `\in_array($value, $list, true)` or
    /// `\array_search($value, $list, true)`. Otherwise it runs [`Self::equality`] on each element:
    /// `\array_find_key($list, (fn ($value#N) => fn ($element#M) => $element#M == $value#N)($value))`, which reads
    /// the receiver before the value, as the call does, and the value once. An index the search found goes into a
    /// hidden `$index#N`, since index 0 is falsy: `indexOf` reads `\array_search`'s false as null, and `remove` gives
    /// `($index#N = …) !== false && $list->removeAt($index#N) === null`, so `Sharp\Collection::removeAt` removes the
    /// element, renumbers the list and writes it back where it lives.
    fn element_search(
        &mut self,
        call: Span,
        collection: &mut CollectionReceiver,
        element_type: &TUnion,
        value: &Expression,
        search: Search,
    ) -> u32 {
        let line = self.line(call);
        let value_type = self.types.expression_type(value);
        // `\in_array` and `\array_search` read the value before the list, so they run only where neither side runs code.
        let strict = self.declared_equality(element_type, value_type).is_none()
            && !mixes_numbers(element_type, value_type)
            && self.compares_identically(element_type, value_type)
            && !holds_class_value(element_type)
            && !holds_class_value(value_type)
            && self.is_pure(value)
            && self.runs_nothing(collection);
        let hidden = (matches!(search, Search::Remove) || (strict && matches!(search, Search::IndexOf))).then(|| {
            self.temporaries += 1;

            format!("index#{}", self.temporaries).into_bytes()
        });
        let found = if strict {
            let value = self.expression(value);
            let list = self.read_collection(collection, line, false);
            let strict = self.zval(line, sharp_value::SHARP_TRUE, |_| {});
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value, list, strict]);
            let function = if matches!(search, Search::Contains) { b"in_array".as_slice() } else { b"array_search" };
            let function = self.string(ZEND_NAME_FQ, line, function);

            self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
        } else {
            self.temporaries += 2;
            let value_name = format!("value#{}", self.temporaries - 1).into_bytes();
            let element_name = format!("element#{}", self.temporaries).into_bytes();
            let list = self.read_collection(collection, line, false);
            let element = Side::Hidden { span: call, name: element_name.clone(), r#type: element_type };
            let held = Side::Hidden { span: call, name: value_name.clone(), r#type: value_type };
            let equality = self.equality(line, element, held, false);
            let compare = self.hidden_arrow_function(call, &[&element_name], equality);
            let compare_with = self.hidden_arrow_function(call, &[&value_name], compare);
            let value = self.expression(value);
            let value = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);
            let predicate = self.node(SHARP_AST_CALL, 0, line, &[compare_with, value]);
            self.temporaries -= 2;

            let function = self.string(ZEND_NAME_FQ, line, b"array_find_key");
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[list, predicate]);

            self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
        };
        let Some(hidden) = hidden else {
            if strict || matches!(search, Search::IndexOf) {
                return found;
            }
            let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

            return self.node(SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL, line, &[found, null]);
        };

        let variable = self.variable(call, &hidden);
        let found = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, found]);
        let missed = if strict { sharp_value::SHARP_FALSE } else { sharp_value::SHARP_NULL };
        let missed = self.zval(line, missed, |_| {});
        let lowered = if matches!(search, Search::Remove) {
            let found = self.node(SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL, line, &[found, missed]);
            let list = self.read_collection(collection, line, false);
            let remove_at = self.string(0, line, b"removeAt");
            let index = self.variable(call, &hidden);
            let index = self.node(SHARP_AST_ARG_LIST, 0, line, &[index]);
            let removed = self.node(SHARP_AST_METHOD_CALL, 0, line, &[list, remove_at, index]);
            let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
            let removed = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[removed, null]);

            self.node(SHARP_AST_AND, 0, line, &[found, removed])
        } else {
            let missed = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[found, missed]);
            let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
            let index = self.variable(call, &hidden);

            self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[missed, null, index])
        };
        self.temporaries -= 1;

        lowered
    }

    /// Whether reading the receiver `collection` runs no code: a local, a field or a static property, or a property of
    /// or a value that [`Self::is_pure`] reads.
    fn runs_nothing(&self, collection: &CollectionReceiver) -> bool {
        match collection {
            CollectionReceiver::Place(_) => true,
            CollectionReceiver::Property(object, _) | CollectionReceiver::Value(object) => {
                self.is_pure(object.expression)
            }
            CollectionReceiver::Index(list, index) => self.runs_nothing(list) && self.is_pure(index.expression),
        }
    }

    /// `fn ($parameter, …) => body`, an arrow function of untyped hidden parameters, on the line of `span`.
    fn hidden_arrow_function(&mut self, span: Span, parameters: &[&[u8]], body: u32) -> u32 {
        let line = self.line(span);
        let mut lowered = Vec::new();
        for parameter in parameters {
            let name = self.string(0, line, parameter);
            lowered.push(self.node(SHARP_AST_PARAM, 0, line, &[NULL, name, NULL, NULL, NULL, NULL]));
        }
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &lowered);

        self.declaration(SHARP_AST_ARROW_FUNC, 0, span, span, b"", &[parameters, NULL, body, NULL, NULL])
    }

    /// The collection, `List`, `Map` or `Set`, and the name in lowercase of the method the method call `call` runs as
    /// PHP on the array, one of the [`INLINED_COLLECTION_METHODS`], and whether that PHP reads the receiver twice. None
    /// for a call of any other method.
    fn php_collection_method(&self, call: &Expression) -> Option<(&'static str, Vec<u8>, bool)> {
        let (collection, method) = self.types.collection_method(call)?;
        if !INLINED_COLLECTION_METHODS.iter().any(|(inlined_collection, inlined)| {
            *inlined_collection == collection && inlined.as_bytes().eq_ignore_ascii_case(&method)
        }) {
            return None;
        }
        let reads_twice = matches!(method.as_slice(), b"remove" | b"last");

        Some((collection, method, reads_twice))
    }

    /// The receiver of a collection method call. A method that reads it `twice` holds a value or an object that runs
    /// something in a hidden variable.
    fn collection_receiver<'receiver, 'ast>(
        &mut self,
        receiver: &'receiver Expression<'ast>,
        twice: bool,
    ) -> CollectionReceiver<'receiver, 'ast> {
        let receiver = receiver.unparenthesized();
        let hold = |lowering: &mut Self, value: &'receiver Expression<'ast>| {
            if twice { lowering.reread(value, "receiver") } else { Reread::once(value) }
        };

        match receiver {
            Expression::ConstantAccess(name)
                if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::Field)) =>
            {
                CollectionReceiver::Place(receiver)
            }
            Expression::Access(Access::Property(access)) if self.names.static_property_class(access).is_some() => {
                CollectionReceiver::Place(receiver)
            }
            Expression::Access(Access::Property(PropertyAccess { object, property, .. }))
            | Expression::Access(Access::NullSafeProperty(NullSafePropertyAccess { object, property, .. }))
                if !self.is_class_value(object) =>
            {
                CollectionReceiver::Property(hold(self, object), property)
            }
            Expression::ArrayAccess(access) if self.is_list(access.array) => {
                let list = self.collection_receiver(access.array, twice);
                let index = if twice { self.reread(access.index, "index") } else { Reread::once(access.index) };

                CollectionReceiver::Index(Box::new(list), index)
            }
            _ => CollectionReceiver::Value(hold(self, receiver)),
        }
    }

    /// A read of the collection a method call runs on, which is also where the method writes it. The read that tests
    /// a null-safe call's receiver reads a property with `?->`, and a `List` index bare, as `null_safe_object` does
    /// (spec section 12).
    fn read_collection(&mut self, collection: &mut CollectionReceiver, line: u32, null_safe: bool) -> u32 {
        match collection {
            CollectionReceiver::Place(place) => self.target(place, PlaceUse::Write),
            CollectionReceiver::Property(object, property) => {
                let object = self.read(object);
                let property = self.member(property);
                let kind = if null_safe { SHARP_AST_NULLSAFE_PROP } else { SHARP_AST_PROP };

                self.node(kind, 0, line, &[object, property])
            }
            CollectionReceiver::Index(list, index) => {
                let list = self.read_collection(list, line, false);
                let index = self.read(index);

                self.node(SHARP_AST_DIM, 0, line, &[list, index])
            }
            CollectionReceiver::Value(value) if null_safe => self.read_as(value, Self::null_safe_object),
            CollectionReceiver::Value(value) => self.read(value),
        }
    }

    /// Frees the hidden variables of `collection`, the last ones taken first.
    fn release_collection(&mut self, collection: CollectionReceiver) {
        match collection {
            CollectionReceiver::Place(_) => {}
            CollectionReceiver::Property(object, _) => self.release(object),
            CollectionReceiver::Index(list, index) => {
                self.release(index);
                self.release_collection(*list);
            }
            CollectionReceiver::Value(value) => self.release(value),
        }
    }

    /// `value`, which the PHP of a `Set` or a collection method reads more than once. One that reads the same and runs
    /// nothing when lowered again is lowered at each read. Any other is held in the hidden variable `name#N`, which
    /// its first read assigns.
    fn reread<'value, 'ast>(&mut self, value: &'value Expression<'ast>, name: &str) -> Reread<'value, 'ast> {
        if self.is_pure(value) || value.is_constant(&PHPVersion::PHP85, false) {
            return Reread::once(value);
        }

        self.temporaries += 1;

        Reread { expression: value, hidden: Some(format!("{name}#{}", self.temporaries).into_bytes()), assigned: false }
    }

    /// A read of `value`, whose first read assigns its hidden variable.
    fn read(&mut self, value: &mut Reread) -> u32 {
        self.read_as(value, Self::expression)
    }

    /// A read of `value` as `lower` lowers it, whose first read assigns its hidden variable.
    fn read_as(&mut self, value: &mut Reread, lower: fn(&mut Self, &Expression) -> u32) -> u32 {
        let Some(name) = value.hidden.clone() else {
            return lower(self, value.expression);
        };
        let variable = self.variable(value.expression.span(), &name);
        if value.assigned {
            return variable;
        }

        value.assigned = true;
        let lowered = lower(self, value.expression);

        self.node(SHARP_AST_ASSIGN, 0, self.line(value.expression), &[variable, lowered])
    }

    /// Frees the hidden variable of `value`, the last one taken, for the forms after it.
    fn release(&mut self, value: Reread) {
        if value.hidden.is_some() {
            self.temporaries -= 1;
        }
    }

    /// A template without `${…}` is its text, as php-src's grammar builds a string without interpolation. Any other
    /// is an `ENCAPS_LIST` of its text that is not empty and its values, on the line of its first part.
    fn template(&mut self, template: &InterpolatedString) -> u32 {
        match template.parts.as_slice() {
            [] => self.string(0, self.line(template), b""),
            [StringPart::Literal(text)] => self.template_text(text),
            parts => {
                let mut children = Vec::new();
                for part in parts {
                    match part {
                        StringPart::Literal(text) if text.value == Some(b"") => {}
                        StringPart::Literal(text) => children.push(self.template_text(text)),
                        StringPart::BracedExpression(interpolation) => {
                            children.push(self.template_value(interpolation.expression));
                        }
                        StringPart::Expression(_) => unreachable!("the parser reads only `${{…}}` in a template"),
                    }
                }

                let line = self.nodes[children[0] as usize].line;

                self.node(SHARP_AST_ENCAPS_LIST, 0, line, &children)
            }
        }
    }

    fn template_text(&mut self, text: &LiteralStringPart) -> u32 {
        let Some(value) = text.value else {
            unreachable!("the parser refuses an escape JavaScript refuses");
        };

        self.string(0, self.line(text), value)
    }

    /// The value a template's `${…}` shows. Spec section 18 shows a `bool` as `true` or `false`, where PHP's string
    /// conversion gives `1` or nothing, so a `bool` is `["false", "true"][value]`: PHP indexes by `false` as 0 and
    /// `true` as 1, and the optimizer keeps the read as one `FETCH_DIM_R` of a constant array, where a conditional takes
    /// four instructions. An object shows what its `toString()` returns, `$value->toString()`, which an enum case
    /// needs, since PHP gives an enum no string conversion. Any other value is itself.
    fn template_value(&mut self, expression: &Expression) -> u32 {
        let value = self.expression(expression);
        let r#type = self.types.expression_type(expression);
        let line = self.line(expression);
        if !r#type.types.is_empty() && r#type.types.iter().all(TAtomic::is_object_type) {
            let method = self.string(0, line, b"toString");
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);

            return self.node(SHARP_AST_METHOD_CALL, 0, line, &[value, method, arguments]);
        }
        if !r#type.is_bool() {
            return value;
        }

        let texts = [b"false".as_slice(), b"true"].map(|text| {
            let text = self.string(0, line, text);

            self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[text, NULL])
        });
        let texts = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &texts);

        self.node(SHARP_AST_DIM, 0, line, &[texts, value])
    }

    /// What an assignment, a compound assignment, `++` or `--` writes: a local or parameter, `object.name`,
    /// `Class.name`, which php-src's grammar builds as the static property `Class::$name`, or an index of one of them,
    /// as php-src's `variable` rule takes them. `place_use` says whether the write reads the target first, as a
    /// compound assignment, `++` and `--` do, which a generic class's static checks.
    fn target(&mut self, target: &Expression, place_use: PlaceUse) -> u32 {
        match target {
            Expression::ConstantAccess(name)
                if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::Field)) =>
            {
                self.name(name, place_use)
            }
            // The hidden variable of an `is`, `as` or `match`, which `php_shape` writes.
            Expression::Variable(Variable::Direct(_)) => self.expression(target),
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let ClassLikeMemberSelector::Identifier(name) = &access.property else {
                        unreachable!("check_slice refuses the member name `{}`", access.property);
                    };
                    let line = self.line(target);
                    let (class, classes) = self.static_class(class);
                    let property = self.member(&access.property);

                    self.static_property(target, class, &classes, name.value, property, line, place_use)
                }
                None => self.expression(target),
            },
            Expression::ArrayAccess(access) => {
                let value = self.target(access.array, place_use);
                let key = self.expression(access.index);

                self.node(SHARP_AST_DIM, 0, self.line(target), &[value, key])
            }
            _ => unreachable!("check_slice refuses writing to `{target}`"),
        }
    }

    /// An `is`, `as` or `match` is the PHP it runs as, which the analyzer analyzes too. Its hidden variables are
    /// numbered after those of the forms around it, which may still hold their values.
    fn pattern(&mut self, node: Node<'_, '_>) -> u32 {
        let arena = LocalArena::new();
        let names = self.types.pattern_names();
        let Some(PhpShape { php, temporaries, .. }) = php_shape(&arena, node, self.temporaries, &names) else {
            unreachable!("check_slice refuses the pattern forms that have no PHP");
        };

        self.temporaries += temporaries;
        let index = match php {
            Node::Expression(expression) => self.expression(expression),
            Node::Statement(statement) => self.statement(statement),
            _ => unreachable!("`php_shape` builds an expression or a statement"),
        };
        self.temporaries -= temporaries;

        index
    }

    /// A local, a parameter or `this` is a PHP variable of the same name, and `field` is the storage, used as
    /// `place_use` says, as [`Self::storage`] reads it. Any other bare name outside a call is a constant, which the
    /// engine looks up in the namespace, then globally, as PHP does for an unqualified name.
    fn name(&mut self, name: &ConstantAccess, place_use: PlaceUse) -> u32 {
        let line = self.line(name);

        match self.names.binding(&name.name) {
            Some(Binding::Local(_)) => self.variable(name.span(), name.name.value()),
            Some(Binding::This) => self.variable(name.span(), b"this"),
            Some(Binding::Field) => self.storage(line, place_use),
            Some(Binding::Constant) => {
                let constant = self.string(ZEND_NAME_NOT_FQ, line, name.name.value());

                self.node(SHARP_AST_CONST, 0, line, &[constant])
            }
            binding => unreachable!("check_slice refuses the name `{}` bound as {binding:?}", name.name),
        }
    }

    fn variable(&mut self, span: Span, name: &[u8]) -> u32 {
        let line = self.line(span);
        let name = self.local_name(0, line, name);

        self.node(SHARP_AST_VAR, 0, line, &[name])
    }

    /// The name of the PHP variable the PHP# local or parameter `name` runs as, with `attr`, as a parameter, a `use`
    /// of a closure or a `catch` clause writes it: a superglobal's name runs hidden, [`php_local_name`].
    fn local_name(&mut self, attr: u32, line: u32, name: &[u8]) -> u32 {
        self.string(attr, line, &php_local_name(name))
    }

    /// `Class.m()` is a static call on the class's full name, `super.m()` one on `parent`, and `Self.m()` one on
    /// `static`, except the standard library's `Position.current()`, which is the position it is written at. Any other
    /// `object.m()` is an instance call, or a call of the function in the property `m` when the checker found that
    /// property, as spec section 14 calls one. A call of a standard library method with an inline form runs that form.
    /// `Pdf.render(html)` and `global.now()` call the plain PHP function the checker found, spec section 23, which
    /// takes each `Map` and `Set` as [`Self::call`] crosses them.
    fn method_call(&mut self, expression: &Expression, call: &MethodCall) -> u32 {
        if let Expression::SuperOf(_) = call.object {
            return self.super_of_call(expression, call);
        }
        if let Some(function) = self.types.namespace_function(expression) {
            let line = self.line(call);
            let quiet = self.types.returns_its_failure(function);
            let function = self.string(ZEND_NAME_FQ, line, function.as_bytes());

            let (parameters, callee) = (self.types.call_parameters(expression), self.types.sharp_callee(expression));

            // A built-in's failure is read from its value, so its warning for the same failure is silenced.
            return self.call(Some(call.span()), &call.argument_list, None, parameters, callee, |this, arguments| {
                let call = this.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
                if quiet { this.node(SHARP_AST_SILENCE, 0, line, &[call]) } else { call }
            });
        }
        if self.names.is_current_position(call) {
            return self.position(call.object.span());
        }
        if self.types.is_lazy_operation(expression) {
            return self.lazy_operation(self.line(call), call.object, &call.method, &call.argument_list);
        }
        if let Some(lowered) = self.collection_call(expression, call.object, &call.argument_list, false) {
            return lowered;
        }
        if let Some(extension) = self.types.extension(expression) {
            return self.extension_access(expression, &extension, call.object, Some(&call.argument_list));
        }
        if let Some(inlined) = self.inlined_call(expression, call) {
            return inlined;
        }
        // Spec section 20: `PaymentResult.Paid(…)` creates the case, an object of its class, and a case of a generic
        // enum gets the type arguments the call fixed, spec section 31.
        if let Some(class) = self.names.static_call_class(call)
            && let ClassLikeMemberSelector::Identifier(method) = &call.method
            && let Some(case) = self.types.enum_case_class(self.names.get(&class.name), method.value)
        {
            let name = self.string(ZEND_NAME_FQ, self.line(method), case.as_bytes());
            let arguments = self.arguments(&call.argument_list);
            let new = self.node(SHARP_AST_NEW, 0, self.line(call), &[name, arguments]);
            let type_arguments = self.types.call_type_arguments(expression);

            return self.with_type_arguments(self.line(call), new, type_arguments);
        }

        let line = self.line(call);
        let (kind, object) = match (self.names.static_call_class(call), call.object) {
            (Some(class), _) => (SHARP_AST_STATIC_CALL, self.static_class(class).0),
            (None, Expression::Parent(keyword)) => {
                (SHARP_AST_STATIC_CALL, self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"parent"))
            }
            (None, Expression::Self_(keyword)) => {
                (SHARP_AST_STATIC_CALL, self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"static"))
            }
            (None, object) => match self.class_value(object) {
                Some((class, _)) => (SHARP_AST_STATIC_CALL, class),
                None if self.reads_plain_map(expression, object) => {
                    let r#type = self.types.expression_type(object).clone();

                    (SHARP_AST_METHOD_CALL, self.crossed_value(object, Some(&r#type)))
                }
                None => (SHARP_AST_METHOD_CALL, self.receiver(object)),
            },
        };
        let method = self.member(&call.method);
        if self.types.collection_method(expression).is_some() {
            let arguments = self.collection_arguments(&call.argument_list);

            return self.node(kind, 0, line, &[object, method, arguments]);
        }
        let property_call = kind == SHARP_AST_METHOD_CALL && self.is_property_call(expression, call.object);
        let type_arguments = self.types.call_type_arguments(expression);
        let parameters = self.types.call_parameters(expression);
        let callee = self.types.sharp_callee(expression);

        self.call(Some(call.span()), &call.argument_list, type_arguments, parameters, callee, |this, arguments| {
            if property_call {
                let property = this.node(SHARP_AST_PROP, 0, line, &[object, method]);

                return this.node(SHARP_AST_CALL, 0, line, &[property, arguments]);
            }

            this.node(kind, 0, line, &[object, method, arguments])
        })
    }

    /// The object of the instance call on `object`. A static field of a generic class, which `Sharp\Internal\statics`
    /// returns by reference, is bound by reference to the hidden variable `receiver#N` first, so a collection method
    /// that changes it changes the field where it lives, as it does a property, instead of a copy. That holds for one
    /// read through the class's name and one read through a class value alike.
    fn receiver(&mut self, object: &Expression) -> u32 {
        let lowered = self.expression(object);
        let is_static_field = matches!(
            object,
            Expression::Access(Access::Property(access))
                if self.names.static_property_class(access).is_some() || self.is_class_value(access.object)
        );
        if !is_static_field || self.types.statics_type_arguments(object).is_none() {
            return lowered;
        }

        let name = format!("receiver#{}", self.temporaries + 1);
        let variable = self.variable(object.span(), name.as_bytes());

        self.node(SHARP_AST_ASSIGN_REF, 0, self.line(object), &[variable, lowered])
    }

    /// The class node of `Class.member` and the classes it can be: the class's full name, or, for a type parameter, the
    /// class of the type argument the code runs with, as `typeof(TItem)` gives it, which its bound's classes hold.
    fn static_class(&mut self, class: &ConstantAccess) -> (u32, Vec<&'lowering [u8]>) {
        let line = self.line(class);
        if let Some(classes) = self.types.type_parameter_classes(&class.name) {
            let tested = self
                .types
                .tested_type(&class.name)
                .unwrap_or_else(|| unreachable!("a type parameter's type is recorded"));
            let tested = self.type_text(line, &tested);

            return (self.node(SHARP_AST_SHARP_TYPE_OF, 0, line, &[tested]), classes);
        }
        let full_name = self.names.get(&class.name);

        (self.string(ZEND_NAME_FQ, line, full_name), vec![full_name])
    }

    /// `super<HasLabel>.label()`, spec section 22: a call on `$this` of the protected alias of the `HasLabel\Defaults`
    /// method that the class's `use` declares, `$this->{'label@App\HasLabel'}()`, as PHP runs a trait's method that the
    /// class replaced only through an alias.
    fn super_of_call(&mut self, expression: &Expression, call: &MethodCall) -> u32 {
        let Expression::SuperOf(super_of) = call.object else {
            unreachable!("`super_of_call` lowers a call on `super<…>`");
        };
        let line = self.line(call);
        let interface = word(self.names.get(&super_of.interface));
        let method = self.types.call_target(expression).name;
        let object = self.variable(super_of.span(), b"this");
        let method = self.string(0, self.line(&call.method), &super_alias(interface, method));
        let type_arguments = self.types.call_type_arguments(expression);
        let parameters = self.types.call_parameters(expression);
        let callee = self.types.sharp_callee(expression);

        self.call(Some(call.span()), &call.argument_list, type_arguments, parameters, callee, |this, arguments| {
            this.node(SHARP_AST_METHOD_CALL, 0, line, &[object, method, arguments])
        })
    }

    /// The member `extension`, which an extension adds, spec section 26, called with `arguments` or read on `receiver`:
    /// the call of the static method it runs as, `\App\TextExtensions::slug($title, '-')`, or the library's inline
    /// form of it, as `trim($name)` for `name.trim()`. The checker chose it from the type the code shows, so it runs
    /// on any value of that type, an object of a subclass with its own member of that name too. A call passes the
    /// caller's position, as [`Self::caller_positions`] says, and a generic member's call ends with its type
    /// arguments, as any generic call does.
    fn extension_access(
        &mut self,
        access: &Expression,
        extension: &Declaration,
        receiver: &Expression,
        arguments: Option<&ArgumentList>,
    ) -> u32 {
        let line = self.line(access);
        if let Some(inlined) = self.inlined(extension, Some(receiver), arguments, line) {
            return inlined;
        }

        let mut values = vec![self.expression(receiver)];
        for argument in arguments.into_iter().flat_map(|list| list.arguments.iter()) {
            values.push(match argument {
                Argument::Positional(positional) => self.positional_argument(positional),
                Argument::Named(named) => self.named_argument(named),
            });
        }
        // The receiver is the static method's first parameter.
        if let Some(list) = arguments
            && let Some(callee) = self.types.sharp_method(extension.class.as_bytes(), extension.name.as_bytes())
        {
            values.extend(self.caller_positions(access.span(), list, callee, 1));
        }
        if let Some(type_arguments) = self.types.extension_type_arguments(access) {
            let text = self.type_text(line, &type_arguments);
            values.push(self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[NULL, text]));
        }

        self.operator_call(line, extension.class, extension.name.as_bytes(), &values)
    }

    /// Spec section 27: the position `at` starts at, a new `Position` of the file, the line and byte column, and the
    /// function, as `new \Sharp\Position(__FILE__, 9, 22, 'App.Tenant.Report.run')`. `Position.current()` in a body is
    /// the position of its `Position`, and a parameter's default of it is the position of its own `Position` too, which
    /// a plain PHP caller gets. A PHP# call passes its own start instead, as [`Self::sharp_arguments`] says. The engine
    /// compiles the file under its source's absolute path, which `__FILE__` reads where the file runs, so the compiled
    /// file holds no path.
    fn position(&mut self, at: Span) -> u32 {
        let (line, column) = self.lines.line_and_column(at.start.offset);
        let name = self.string(ZEND_NAME_FQ, line, b"Sharp\\Position");
        let file = self.node(SHARP_AST_MAGIC_CONST, SHARP_T_FILE, line, &[]);
        let line_number = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = i64::from(line));
        let column = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = i64::from(column));
        let function = self.function.clone();
        let function = self.string(0, line, &function);
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[file, line_number, column, function]);

        self.node(SHARP_AST_NEW, 0, line, &[name, arguments])
    }

    /// Makes `name`, a member of the class-like being lowered, the function `Position.current()` gives, by its full
    /// dotted name.
    fn enter(&mut self, name: &[u8]) {
        self.function.clear();
        self.function.extend(self.class.iter().map(|&byte| if byte == b'\\' { b'.' } else { byte }));
        self.function.push(b'.');
        self.function.extend_from_slice(name);
    }

    fn member(&mut self, member: &ClassLikeMemberSelector) -> u32 {
        let ClassLikeMemberSelector::Identifier(name) = member else {
            unreachable!("check_slice refuses the member name `{member}`");
        };

        self.string(0, self.line(name.span), name.value)
    }

    /// The arguments of a call of a `List`, `Map` or `Set` method, which runs as the `Sharp\Collection` method that
    /// serves every collection. A method of one parameter names it after what its collection takes, `key` or
    /// `value`, where `Sharp\Collection` has one name, so a named argument goes in by position. `set` and `insert`,
    /// the methods of two parameters, name them as `Sharp\Collection` does.
    fn collection_arguments(&mut self, list: &ArgumentList) -> u32 {
        let [Argument::Named(named)] = list.arguments.as_slice() else {
            return self.arguments(list);
        };
        let value = self.expression(named.value);

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &[value])
    }

    fn arguments(&mut self, list: &ArgumentList) -> u32 {
        let arguments = self.argument_nodes(list);

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    /// The arguments of `list` as the PHP# function, method or constructor `callee` takes them, each lazy plain PHP
    /// source checked where it enters an `Iterable<T>` parameter, as [`Types::lazy_argument_checks`] says, then the
    /// caller's positions [`Self::caller_positions`] passes.
    fn sharp_arguments(&mut self, site: Option<Span>, list: &ArgumentList, callee: &FunctionLikeMetadata) -> Vec<u32> {
        let checks = self.types.lazy_argument_checks(callee, list);
        let mut arguments: Vec<u32> = list
            .arguments
            .iter()
            .enumerate()
            .map(|(position, argument)| match (argument, checks.get(position)) {
                (Argument::Positional(positional), Some(Some((text, target)))) => {
                    let line = self.line(positional.value);
                    let value = self.expression(positional.value);

                    self.verified(line, value, text, target.as_bytes())
                }
                (Argument::Positional(positional), _) => self.positional_argument(positional),
                (Argument::Named(named), _) => self.named_argument(named),
            })
            .collect();
        if let Some(site) = site {
            arguments.extend(self.caller_positions(site, list, callee, 0));
        }

        arguments
    }

    /// Spec section 27: each parameter of `callee` whose default is `Position.current()` that the call starting at
    /// `site` leaves out takes the call's position, as the named argument
    /// `caller: new \Sharp\Position(__FILE__, 12, 9, 'App.Reports.run')`. `leading` counts the parameters the call
    /// fills before `list`, as an extension's receiver. A spread fills each parameter from its own on, so a parameter a
    /// spread's list may hold a value for keeps its default, the declaration's position, since passing it too would
    /// pass it twice.
    fn caller_positions(
        &mut self,
        site: Span,
        list: &ArgumentList,
        callee: &FunctionLikeMetadata,
        leading: usize,
    ) -> Vec<u32> {
        let positional = leading
            + list
                .arguments
                .iter()
                .take_while(
                    |argument| matches!(argument, Argument::Positional(positional) if positional.ellipsis.is_none()),
                )
                .count();
        let mut positions = Vec::new();
        for parameter in
            callee.parameters.iter().skip(positional).filter(|parameter| parameter.flags.is_caller_position())
        {
            let name = &parameter.name.0.as_bytes()[1..];
            let given = list.arguments.iter().any(|argument| match argument {
                Argument::Named(named) => named.name.value == name,
                Argument::Positional(positional) => {
                    positional.ellipsis.is_some() && self.types.spread_can_fill(positional.value, parameter)
                }
            });
            if given {
                continue;
            }
            let line = self.line(site);
            let name = self.string(0, line, name);
            let position = self.position(site);
            positions.push(self.node(SHARP_AST_NAMED_ARG, 0, line, &[name, position]));
        }

        positions
    }

    fn argument_nodes(&mut self, list: &ArgumentList) -> Vec<u32> {
        list.arguments
            .iter()
            .map(|argument| match argument {
                Argument::Positional(positional) => self.positional_argument(positional),
                Argument::Named(named) => self.named_argument(named),
            })
            .collect()
    }

    /// The last argument of a call of a generic method, the type arguments `text` the checker found, a
    /// `SHARP_TYPE_ARGS` without a `new`, which the engine resolves right before the call.
    fn type_arguments_argument(&mut self, line: u32, text: &str) -> u32 {
        let text = self.type_text(line, text);

        self.node(SHARP_AST_SHARP_TYPE_ARGS, 0, line, &[NULL, text])
    }

    /// The call `call` builds from its lowered arguments, which starts at `site`, or none for a call through a function
    /// value, where `parameters` are the parameters of the plain PHP function, method or constructor it runs, or none
    /// for a PHP# one, and `type_arguments` the type arguments of a
    /// generic method, which go last, as [`Self::type_arguments_argument`] writes them. A `Map` or a `Set` crosses into
    /// a plain PHP parameter whose declared type names `array` as a PHP array, spec section 12, and a `Set` without its
    /// positions: `\is_object($m) ? \iterator_to_array($m) : $m`, so null and a PHP array pass as they are. Any other
    /// parameter, such as `mixed`, `iterable`, `object` or an interface, takes the object, and so does any parameter
    /// when the value is itself what plain PHP gave. A local passed to a reference parameter crosses back after the
    /// call, and before it too when the parameter names `array`, `[$m = …, \parse_str($query, $m), $m = …][1]`, as
    /// [`Self::crossed`] makes the object of what plain PHP wrote.
    fn call(
        &mut self,
        site: Option<Span>,
        list: &ArgumentList,
        type_arguments: Option<String>,
        parameters: Option<Vec<PlainParameter>>,
        callee: Option<&FunctionLikeMetadata>,
        call: impl FnOnce(&mut Self, u32) -> u32,
    ) -> u32 {
        let line = self.line(list);
        let (mut arguments, written) = match (parameters, callee) {
            (Some(parameters), _) => self.crossing_arguments(list, &parameters),
            (None, Some(callee)) => (self.sharp_arguments(site, list, callee), Vec::new()),
            (None, None) => (self.argument_nodes(list), Vec::new()),
        };
        if let Some(type_arguments) = type_arguments {
            arguments.push(self.type_arguments_argument(line, &type_arguments));
        }
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &arguments);
        let call = call(self, arguments);
        if written.is_empty() {
            return call;
        }

        let mut steps = Vec::new();
        for (span, name, declared, takes_array) in &written {
            let (span, name) = (*span, *name);
            if let Some(set) = table_kind(declared).filter(|_| *takes_array) {
                let array = if self.keyed_by_backed_enum(declared) {
                    let map = self.variable(span, name);

                    self.backed_key_array(span, map)
                } else {
                    let [first, second, third] = [(); 3].map(|()| self.variable(span, name));

                    self.array_from(line, set, [first, second, third])
                };
                let place = self.variable(span, name);
                let before = self.node(SHARP_AST_ASSIGN, 0, line, &[place, array]);
                steps.push(self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[before, NULL]));
            }
        }
        let position = steps.len();
        steps.push(self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[call, NULL]));
        for (span, name, declared, _) in written {
            let first = self.variable(span, name);
            let crossed = self.crossed(span, &declared, name, first);
            let place = self.variable(span, name);
            let after = self.node(SHARP_AST_ASSIGN, 0, line, &[place, crossed]);
            steps.push(self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[after, NULL]));
        }
        let steps = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &steps);
        let position = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = position as i64);

        self.node(SHARP_AST_DIM, 0, line, &[steps, position])
    }

    /// The arguments of `list` as a call of plain PHP whose parameters are `parameters` takes them, each `Map` or `Set`
    /// crossed as [`Self::call`] says, with each typed local a reference parameter writes: its span, its name, its
    /// declared type, and whether the parameter names `array`.
    #[allow(clippy::type_complexity)]
    fn crossing_arguments<'value>(
        &mut self,
        list: &ArgumentList<'value>,
        parameters: &[PlainParameter],
    ) -> (Vec<u32>, Vec<(Span, &'value [u8], TUnion, bool)>) {
        let line = self.line(list);
        let mut arguments = Vec::new();
        let mut written = Vec::new();
        for (position, argument) in list.arguments.iter().enumerate() {
            let (value, spread) = match argument {
                Argument::Positional(positional) => (positional.value, positional.ellipsis.is_some()),
                Argument::Named(named) => (named.value, false),
            };
            let parameter = match argument {
                Argument::Positional(_) => parameters
                    .get(position)
                    .or_else(|| parameters.last().filter(|parameter| parameter.is_variadic()))
                    .copied(),
                Argument::Named(named) => {
                    parameters.iter().copied().find(|parameter| parameter.name() == Some(named.name.value))
                }
            };
            let takes_array = parameter
                .and_then(PlainParameter::declared_type)
                .is_some_and(|declared| declared.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(_))));
            let crossed = match (takes_array, spread, self.typed_local(value)) {
                (_, false, Some((name, declared))) if parameter.is_some_and(PlainParameter::is_by_reference) => {
                    if self.crossing_array(&declared).is_some() {
                        written.push((value.span(), name, declared, takes_array));
                    }

                    None
                }
                (false, _, _) => None,
                (true, _, _) if self.plain_value(value) => None,
                (true, true, _) => self
                    .crossing_array(self.types.expression_type(value))
                    .map(|list| self.types.array_parameters(list).1)
                    .and_then(|element| self.array_of_each(value, &element)),
                (true, false, _) if self.keyed_by_backed_enum(self.types.expression_type(value)) => {
                    let map = self.expression(value);

                    Some(self.backed_key_array(value.span(), map))
                }
                (true, false, _) => table_kind(self.types.expression_type(value)).map(|set| self.array_of(value, set)),
            };
            let value = crossed.unwrap_or_else(|| self.arriving(value, Arrival::Plain));
            arguments.push(match argument {
                Argument::Positional(_) if spread => self.node(SHARP_AST_UNPACK, 0, line, &[value]),
                Argument::Positional(_) => value,
                Argument::Named(named) => {
                    let name = self.string(0, self.line(named.name.span), named.name.value);

                    self.node(SHARP_AST_NAMED_ARG, 0, line, &[name, value])
                }
            });
        }

        (arguments, written)
    }

    /// The name and the declared type of the typed local `value` reads, where it reads one.
    fn typed_local<'value>(&self, value: &Expression<'value>) -> Option<(&'value [u8], TUnion)> {
        let Expression::ConstantAccess(access) = value.unparenthesized() else {
            return None;
        };
        let Some(Binding::Local(local)) = self.names.binding(&access.name) else {
            return None;
        };

        Some((access.name.value(), self.local_types.get(&local.declaration.start.offset)?.clone()))
    }

    /// The PHP array of the `Map` or `Set` `value` gives, as [`Self::call`] passes it to plain PHP. `\iterator_to_array`
    /// takes a PHP array too, so only a value that may be null reads itself more than once, as an inline form, which
    /// reads each parameter once, needs.
    fn array_of(&mut self, value: &Expression, set: bool) -> u32 {
        let line = self.line(value);
        if !self.types.expression_type(value).is_nullable() {
            let value = self.expression(value);

            return self.iterator_to_array(line, set, value);
        }

        let mut value = self.reread(value, "argument");
        let reads = [self.read(&mut value), self.read(&mut value), self.read(&mut value)];
        self.release(value);

        self.array_from(line, set, reads)
    }

    /// `\array_map(fn ($element#N) => …, $list)`, the list `value` gives with each `Map` or `Set` of `element_type` its
    /// PHP array, as [`Self::call`] spreads it into plain PHP. None when `element_type` is no `Map` or `Set`.
    fn array_of_each(&mut self, value: &Expression, element_type: &TUnion) -> Option<u32> {
        let set = table_kind(element_type)?;
        let span = value.span();
        let line = self.line(span);
        let taken = self.temporaries;
        let element = self.hidden("element");
        let each = if self.keyed_by_backed_enum(element_type) {
            let map = self.variable(span, &element);

            self.backed_key_array(span, map)
        } else {
            let reads = [(); 3].map(|()| self.variable(span, &element));

            self.array_from(line, set, reads)
        };
        let each = self.hidden_arrow_function(span, &[&element], each);
        self.temporaries = taken;
        let list = self.expression(value);

        Some(self.global_call(line, b"array_map", &[each, list]))
    }

    /// `\is_object($m) ? \iterator_to_array($m) : $m` over the three reads of one value, and
    /// `\iterator_to_array($s, false)` for a `Set`, whose positions are no keys.
    fn array_from(&mut self, line: u32, set: bool, [first, second, third]: [u32; 3]) -> u32 {
        let is_object = self.global_call(line, b"is_object", &[first]);
        let array = self.iterator_to_array(line, set, second);

        self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_object, array, third])
    }

    /// `\iterator_to_array($value)`, and `\iterator_to_array($value, false)` for a `Set`.
    fn iterator_to_array(&mut self, line: u32, set: bool, value: u32) -> u32 {
        if !set {
            return self.global_call(line, b"iterator_to_array", &[value]);
        }

        let renumbered = self.zval(line, sharp_value::SHARP_FALSE, |_| {});

        self.global_call(line, b"iterator_to_array", &[value, renumbered])
    }

    /// `value` as it enters a place of the type `declared`, a method's return, a typed local or a typed property, with
    /// each `Map` and `Set` the type holds made the object where plain PHP gave a PHP array, spec section 12:
    /// `\is_array($crossing#N = value) ? \Sharp\Map::from($crossing#N) : $crossing#N`. A value
    /// [`Self::may_be_php_array`] rules out is the object already, and a place without a declared type takes the value
    /// as it is. `[]` entering a `Set` place is the empty `Set`, which the analysis types as the empty array of any
    /// collection. A plain PHP lazy source a call gives is checked element by element where it enters an `Iterable<T>`
    /// place, as [`Types::lazy_call_check`] says.
    fn crossed_value(&mut self, value: &Expression, declared: Option<&TUnion>) -> u32 {
        if let (Expression::Array(array), Some(true)) = (value.unparenthesized(), declared.and_then(table_kind)) {
            return self.set_literal(array);
        }
        if declared.is_some_and(holds_entries) && self.types.holds_map(value) {
            return self.map_entries(value);
        }
        if let Some((text, target)) = declared.and_then(|declared| self.types.lazy_call_check(value, declared)) {
            let line = self.line(value);
            let lowered = self.expression(value);

            return self.verified(line, lowered, &text, target.as_bytes());
        }
        let Some(r#type) =
            declared.filter(|declared| self.may_be_php_array(value) && self.crossing_array(declared).is_some())
        else {
            return self.expression(value);
        };

        let span = value.span();
        let taken = self.temporaries;
        let name = self.hidden("crossing");
        let place = self.variable(span, &name);
        let arrival = match self.crossing_array(r#type) {
            Some(TArray::Keyed(_)) => Arrival::Map(r#type),
            _ => Arrival::Read,
        };
        let lowered = self.arriving(value, arrival);
        let first = self.node(SHARP_AST_ASSIGN, 0, self.line(span), &[place, lowered]);
        let crossed = self.crossed(span, r#type, &name, first);
        self.temporaries = taken;

        crossed
    }

    /// Whether the method call `call` runs a `Map` method that reads its receiver `object`, as [`Types::reads_map`]
    /// says, on a value that can be a PHP array, as [`Self::may_be_php_array`] says. That PHP array becomes the
    /// `\Sharp\Map` first, as [`Self::crossed_value`] makes it where a typed local holds it, so its keys keep their
    /// `string`s. A method that changes its receiver still runs on the PHP array, which the engine refuses, since a
    /// `Map` made of a copy of it would drop the change.
    fn reads_plain_map(&self, call: &Expression, object: &Expression) -> bool {
        self.types.reads_map(call)
            && self.may_be_php_array(object)
            && self.crossing_array(self.types.expression_type(object)).is_some()
    }

    /// Whether `value` can be a PHP array PHP# types as a `Map` or a `Set`: what plain PHP gives, a call of it or a
    /// property it declares, and what a local declared without a type holds. A literal, a `new`, a call of PHP#, a
    /// property PHP# declares, a parameter and a typed local hold the object, as does an element of any of them.
    fn may_be_php_array(&self, value: &Expression) -> bool {
        match value.unparenthesized() {
            Expression::Array(_) | Expression::Instantiation(_) | Expression::Literal(_) => false,
            Expression::ArrayAccess(access) => self.may_be_php_array(access.array),
            Expression::Conditional(conditional) => {
                self.may_be_php_array(conditional.then.unwrap_or(conditional.condition))
                    || self.may_be_php_array(conditional.r#else)
            }
            Expression::Call(_) | Expression::Access(Access::Property(_)) => self.plain_value(value),
            Expression::ConstantAccess(access) => match self.names.binding(&access.name) {
                Some(Binding::Local(local)) => local.kind != LocalKind::Parameter && self.typed_local(value).is_none(),
                _ => true,
            },
            _ => true,
        }
    }

    /// Whether plain PHP gives `value` itself, the result of a call of plain PHP or a property plain PHP declares,
    /// which holds a PHP array where PHP# reads a `Map` or a `Set`.
    fn plain_value(&self, value: &Expression) -> bool {
        match value.unparenthesized() {
            Expression::Call(_) => self.types.call_parameters(value).is_some(),
            Expression::Access(Access::Property(_)) => self.types.plain_property(value),
            _ => false,
        }
    }

    /// An attribute's arguments, which the parser reads as a partial argument list without placeholders.
    fn attribute_arguments(&mut self, list: &PartialArgumentList) -> u32 {
        let mut arguments = Vec::new();
        for argument in &list.arguments {
            arguments.push(match argument {
                PartialArgument::Positional(positional) => self.positional_argument(positional),
                PartialArgument::Named(named) => self.named_argument(named),
                _ => unreachable!("check_slice refuses a placeholder argument"),
            });
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    /// A spread argument is an `UNPACK` of its value, as php-src's grammar builds `...$list`.
    fn positional_argument(&mut self, argument: &PositionalArgument) -> u32 {
        let value = self.expression(argument.value);
        if argument.ellipsis.is_none() {
            return value;
        }

        self.node(SHARP_AST_UNPACK, 0, self.nodes[value as usize].line, &[value])
    }

    /// A named argument, `name: value`. Only a PHP# parameter can have a superglobal's name, which runs hidden, so its
    /// label does too.
    fn named_argument(&mut self, argument: &NamedArgument) -> u32 {
        let line = self.line(argument.name.span);
        let name = self.local_name(0, line, argument.name.value);
        let value = self.expression(argument.value);

        self.node(SHARP_AST_NAMED_ARG, 0, line, &[name, value])
    }

    /// An integer literal too large for `int` is a float, as PHP reads it.
    fn literal(&mut self, literal: &Literal) -> u32 {
        let line = self.line(literal);

        match literal {
            Literal::String(string) => {
                let Some(value) = string.value else {
                    unreachable!("semantics refuses an invalid codepoint escape");
                };

                self.string(0, line, value)
            }
            Literal::Integer(integer) => match integer.value.and_then(|value| i64::try_from(value).ok()) {
                Some(value) => self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = value),
                None => {
                    #[allow(clippy::expect_used)]
                    let value =
                        parse_literal_integer_as_float(integer.raw).expect("the lexer reads only valid integers");

                    self.zval(line, sharp_value::SHARP_DOUBLE, |node| node.double_value = value)
                }
            },
            Literal::Float(float) => {
                let value = float.value.into_inner();

                self.zval(line, sharp_value::SHARP_DOUBLE, |node| node.double_value = value)
            }
            Literal::True(_) => self.zval(line, sharp_value::SHARP_TRUE, |_| {}),
            Literal::False(_) => self.zval(line, sharp_value::SHARP_FALSE, |_| {}),
            Literal::Null(_) => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
        }
    }

    fn declaration(
        &mut self,
        kind: sharp_kind,
        flags: u32,
        start: impl HasSpan,
        end: impl HasSpan,
        name: &[u8],
        children: &[u32],
    ) -> u32 {
        let index = self.node(kind, flags, self.line(start), children);
        let end_line = self.lines.line(end.span().end.offset);
        let name = store_text(&mut self.texts, name);

        let node = &mut self.nodes[index as usize];
        node.end_line = end_line;
        node.text = name;

        index
    }

    /// A type text the engine reads while the code runs. One that names a `#i` makes the lambda around it capture its
    /// method's type arguments. In a static member of a generic class, the class's type parameter `$i` is the static
    /// method's `#i`, and its own `#i` comes after the class's.
    fn type_text(&mut self, line: u32, text: &str) -> u32 {
        let text = match self.static_type_parameters {
            Some(count) => static_type_text(text, count),
            None => text.to_owned(),
        };
        self.captures_type_arguments |= text.contains('#');

        self.string(0, line, text.as_bytes())
    }

    fn string(&mut self, attr: u32, line: u32, text: &[u8]) -> u32 {
        let text = store_text(&mut self.texts, text);

        self.zval(line, sharp_value::SHARP_STRING, |node| {
            node.attr = attr;
            node.text = text;
        })
    }

    fn zval(&mut self, line: u32, value: sharp_value, set: impl FnOnce(&mut sharp_node)) -> u32 {
        let index = self.node(SHARP_AST_ZVAL, 0, line, &[]);

        let node = &mut self.nodes[index as usize];
        node.value = value;
        set(node);

        index
    }

    fn node(&mut self, kind: sharp_kind, attr: u32, line: u32, children: &[u32]) -> u32 {
        let first_child = self.children.len() as u32;
        self.children.extend_from_slice(children);
        self.nodes.push(sharp_node {
            kind,
            attr,
            line,
            end_line: 0,
            first_child,
            child_count: children.len() as u32,
            value: sharp_value::SHARP_NULL,
            long_value: 0,
            double_value: 0.0,
            text: sharp_str::EMPTY,
        });

        (self.nodes.len() - 1) as u32
    }

    fn line(&self, node: impl HasSpan) -> u32 {
        self.lines.line(node.span().start.offset)
    }
}

/// The type the struct field `name` writes: its main constructor parameter's, or its property's.
fn written_hint<'hint, 'arena>(
    members: &'hint Sequence<'arena, ClassLikeMember<'arena>>,
    name: &[u8],
) -> Option<&'hint Hint<'arena>> {
    let named = |variable: &DirectVariable| variable.name.strip_prefix(b"$").unwrap_or(variable.name) == name;

    members.iter().find_map(|member| match member {
        ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => method
            .parameter_list
            .parameters
            .iter()
            .find(|parameter| named(&parameter.variable))
            .and_then(|parameter| parameter.hint.as_ref()),
        ClassLikeMember::Property(property) if named(property.first_variable()) => property.hint(),
        _ => None,
    })
}

/// The members of a written type in the order they are written, seen through `?` and parentheses.
fn written_members<'hint, 'arena>(hint: &'hint Hint<'arena>) -> Vec<&'hint Hint<'arena>> {
    match hint {
        Hint::Nullable(nullable) => written_members(nullable.hint),
        Hint::Parenthesized(parenthesized) => written_members(parenthesized.hint),
        _ => union_members(hint),
    }
}

/// The written element type of `member` when it is the collection `array`: a `List`'s or a `Set`'s one type
/// argument, or a `Map`'s value type.
fn element_hint<'hint, 'arena>(member: &'hint Hint<'arena>, array: &TArray) -> Option<&'hint Hint<'arena>> {
    let Hint::Generic(generic) = member else {
        return None;
    };
    let (name, index): (&[u8], usize) = match array {
        TArray::List(_) => (b"List", 0),
        TArray::Set(_) => (b"Set", 0),
        TArray::Keyed(_) => (b"Map", 1),
    };

    (generic.name.value == name).then(|| generic.type_arguments.arguments.iter().nth(index)).flatten()
}

/// The type PHP writes for a PHP# type that is not a union, once generics are erased.
#[derive(Clone)]
enum PhpType<'name> {
    /// `mixed`, which holds every value.
    Mixed,
    /// A built-in type php-src's grammar reads as a name, such as `int` or `string`.
    Named(&'name [u8]),
    /// A type php-src's grammar reads as a keyword and builds as a `TYPE` node of its type code: `IS_ARRAY` for
    /// `array`, or `IS_STATIC` for `static`.
    Keyword(u32),
    /// One class, or the classes of an intersection, each by its full name.
    Classes(Vec<&'name [u8]>),
}

impl PhpType<'_> {
    /// Whether every value of `other` is a value of this type, so a union that holds this type needs no `other`. Names
    /// are compared as the engine compares them, ignoring case. No union holds two keyword types: the checker refuses
    /// two collections, which share PHP's `array`, and `Self` twice.
    fn holds(&self, other: &PhpType) -> bool {
        match (self, other) {
            (PhpType::Mixed, _) => true,
            (PhpType::Named(name), PhpType::Named(other)) => name.eq_ignore_ascii_case(other),
            (PhpType::Classes(classes), PhpType::Classes(others)) => {
                classes.iter().all(|class| others.iter().any(|other| class.eq_ignore_ascii_case(other)))
            }
            _ => false,
        }
    }
}

/// The types of a union in the order they are written, or the one type that is not a union. The parser nests a
/// union to the right.
fn union_members<'hint, 'arena>(hint: &'hint Hint<'arena>) -> Vec<&'hint Hint<'arena>> {
    match hint {
        Hint::Union(union) => {
            let mut members = union_members(union.left);
            members.extend(union_members(union.right));

            members
        }
        _ => vec![hint],
    }
}

/// The name of the protected alias a class gives the default body of `method` that `interface` gives, spec section 22,
/// as `label@App\HasLabel`. No PHP# member name holds `@`, so it can't clash with one.
fn super_alias(interface: Word, method: Word) -> Vec<u8> {
    [method.as_bytes(), b"@", interface.as_bytes()].concat()
}

/// Whether a static property runs accessors, decision 3: a computed one, or one with an accessor body or an observer.
/// [`Lowering::static_accessors`] lowers it.
fn runs_static_accessors(property: &Property) -> bool {
    property.modifiers().contains_static()
        && match property {
            Property::Computed(_) => true,
            Property::Hooked(hooked) => {
                hooked.hook_list.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
            }
            Property::Plain(_) => false,
        }
}

/// Whether a property is `lazy`, spec section 6.5.
fn is_lazy(modifiers: &Sequence<Modifier>) -> bool {
    modifiers.iter().any(|modifier| matches!(modifier, Modifier::Lazy(_)))
}

/// `name#lazy`, the hidden flag of the instance `lazy` property `name`.
fn lazy_flag_name(property: &[u8]) -> Vec<u8> {
    [property, b"#lazy"].concat()
}

/// `name#viaN`, the hidden slot of the behavior at `index` of the property `name`, spec section 6.4.
fn behavior_slot_name(property: &[u8], index: usize) -> Vec<u8> {
    [property, format!("#via{index}").as_bytes()].concat()
}

/// The flags of a class member's modifiers, with `ZEND_ACC_PRIVATE` when they name no access: a PHP# member written
/// without an access modifier is `private`, spec section 5, where PHP makes it `public`.
fn modifier_flags_or_private(modifiers: &Sequence<Modifier>) -> u32 {
    let flags = modifier_flags(modifiers);

    if flags & (ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED | ZEND_ACC_PRIVATE) == 0 {
        flags | ZEND_ACC_PRIVATE
    } else {
        flags
    }
}

/// `ZEND_ACC_GENERATOR` for a method whose body yields, as php-src's grammar flags a function whose body holds a
/// `yield`, and zero for any other.
fn generator_flag(body: &MethodBody) -> u32 {
    match body {
        MethodBody::Concrete(block) if block_has_yield(block) => ZEND_ACC_GENERATOR,
        _ => 0,
    }
}

/// The flags of a member's modifiers. Every modifier is named, so a new one does not compile until it is decided.
fn modifier_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            // PHP has no namespace reach, so the analyzer checks spec section 5's `internal`, and it runs as public.
            Modifier::Public(_) | Modifier::Internal(_) => ZEND_ACC_PUBLIC,
            Modifier::Protected(_) => ZEND_ACC_PROTECTED,
            Modifier::Private(_) => ZEND_ACC_PRIVATE,
            Modifier::Static(_) => ZEND_ACC_STATIC,
            Modifier::Abstract(_) => ZEND_ACC_ABSTRACT,
            // The checker keeps `final` on an override only, spec section 22, where it stops the override chain.
            Modifier::Final(_) => ZEND_ACC_FINAL,
            // PHP methods are open to overriding, and `method` lowers `override` to `#[\Override]`. PHP has no
            // `required` constructor, and the checker proves every subclass keeps one `new Self(…)` can call. `method`
            // gives an `extern` method the body that calls its native function. `property` gives a `lazy` property
            // the `get` hook that keeps its value.
            Modifier::Virtual(_)
            | Modifier::Override(_)
            | Modifier::Required(_)
            | Modifier::Extern(_)
            | Modifier::Lazy(_) => 0,
            Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_)
            | Modifier::Foreign(_) => unreachable!("check_slice refuses the modifier `{modifier}`"),
        };
    }

    flags
}

/// The type text `$0, $1, …` of a class-like's `count` type parameters, each by its index.
fn type_parameters_text(count: usize) -> String {
    (0..count).map(|index| format!("${index}")).collect::<Vec<_>>().join(", ")
}

/// `text`, a type text in a static member of a class with `count` type parameters, with the class's `$i` written as the
/// static method's `#i` and the method's own `#i` as `#` and `count + i`. A type text writes `$` and `#` only before an
/// index.
fn static_type_text(text: &str, count: usize) -> String {
    let mut written = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['$', '#']) {
        written.push_str(&rest[..at]);
        let digits = rest[at + 1..].find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len() - at - 1);
        let Ok(index) = rest[at + 1..at + 1 + digits].parse::<usize>() else {
            unreachable!("an index follows `{}`", &rest[at..=at]);
        };
        let shift = if rest.as_bytes()[at] == b'#' { count } else { 0 };
        written.push('#');
        written.push_str(&(index + shift).to_string());
        rest = &rest[at + 1 + digits..];
    }
    written.push_str(rest);

    written
}

/// The flags of a class's modifiers. `public` and `internal` add none, because every PHP class is public and the
/// analyzer checks spec section 5's namespace reach, and `foreign` adds none, because only the checker reads it. A
/// static class, whose members are all static, runs as a final PHP class.
fn class_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) | Modifier::Internal(_) | Modifier::Foreign(_) => 0,
            Modifier::Abstract(_) => ZEND_ACC_EXPLICIT_ABSTRACT_CLASS,
            Modifier::Final(_) | Modifier::Static(_) => ZEND_ACC_FINAL,
            Modifier::Protected(_)
            | Modifier::Private(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_)
            | Modifier::Virtual(_)
            | Modifier::Override(_)
            | Modifier::Required(_)
            | Modifier::Extern(_)
            | Modifier::Lazy(_) => unreachable!("check_slice refuses the class modifier `{modifier}`"),
        };
    }

    flags
}

/// The id of the service the container gives a parameter written `hint` whose metadata is `service`, when its PHP type
/// drops the service, spec section 32: a collection of objects, as `List<PriceRule>` runs as `array`. The engine reads
/// any other parameter's class from its PHP type, and a collection of values is no service. Neither is a type that
/// names a type parameter, as `List<TItem>` does, whose objects are of a type each type argument fixes.
fn dropped_service(hint: &Hint, service: &FunctionLikeParameterMetadata) -> Option<Word> {
    let r#type = &service.type_metadata.as_ref()?.type_union;
    let collection = matches!(
        without_readonly(hint),
        Hint::Generic(_) | Hint::Nullable(NullableHint { hint: Hint::Generic(_), .. })
    );
    let nodes = r#type.get_all_child_nodes();
    let of_objects = nodes.iter().any(|node| matches!(node, TypeRef::Atomic(TAtomic::Object(_))));
    let is_open = nodes.iter().any(|node| matches!(node, TypeRef::Atomic(TAtomic::GenericParameter(_))));

    (collection && of_objects && !is_open).then(|| service_id(r#type))
}

/// Where plain PHP's array, which the checker reads as a `List`, goes, as [`Lowering::arriving`] lowers it.
#[derive(Clone, Copy)]
enum Arrival<'place> {
    /// PHP# reads it, so it is renumbered, spec section 12.
    Read,
    /// It enters a place that declares this `Map`, which makes a `Map` of its keys, and is checked as that `Map`.
    Map(&'place TUnion),
    /// It is an argument of plain PHP, which takes it as plain PHP gave it.
    Plain,
}

/// Whether a value of `r#type` is a `Set`, true, or a `Map`, false, where it may be one.
fn table_kind(r#type: &TUnion) -> Option<bool> {
    r#type.types.iter().find_map(|atomic| match atomic {
        TAtomic::Array(TArray::Set(_)) => Some(true),
        TAtomic::Array(TArray::Keyed(_)) => Some(false),
        _ => None,
    })
}

/// Whether `r#type` is a PHP# `Iterable<T>`, which holds a `Map` by its entries, spec section 12, whose `T` can be an
/// entry: a tuple, `Any` or a type parameter. The checker refuses a `Map` for any other `T`.
fn holds_entries(r#type: &TUnion) -> bool {
    r#type.types.iter().any(|atomic| match atomic {
        TAtomic::Iterable(iterable @ TIterable { holds_map_entries: true, .. }) => {
            iterable.get_value_type().types.iter().any(|value| {
                value.is_mixed() || matches!(value, TAtomic::GenericParameter(_) | TAtomic::Array(TArray::List(_)))
            })
        }
        _ => false,
    })
}

/// Whether the property a constructor parameter declares runs its writes through its accessors: an accessor body, an
/// observer or a behavior, spec section 9, which the constructor's write of the argument runs.
fn runs_its_accessors(parameter: &FunctionLikeParameter) -> bool {
    parameter.behaviors.is_some()
        || parameter.hooks.as_ref().is_some_and(|accessors| {
            accessors.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_)))
        })
}

/// Whether `hint`, a parameter's or a property's type, is a `Map` or a `Set`, which plain PHP gives as a PHP array, as
/// `Some` of whether it is nullable.
fn collection_hint(hint: Option<&Hint>) -> Option<bool> {
    let (generic, nullable) = match hint.map(without_readonly) {
        Some(Hint::Generic(generic)) => (generic, false),
        Some(Hint::Nullable(NullableHint { hint: Hint::Generic(generic), .. })) => (generic, true),
        _ => return None,
    };

    matches!(generic.name.value, b"Map" | b"Set").then_some(nullable)
}

/// The type `hint` writes after its `readonly`, spec section 13, which the engine runs as the type.
fn without_readonly<'hint, 'arena>(hint: &'hint Hint<'arena>) -> &'hint Hint<'arena> {
    match hint {
        Hint::Readonly(readonly) => readonly.hint,
        hint => hint,
    }
}

/// The flags a property's accessors add: `readonly` for a get-only auto-property, which spec section 6.1 sets in the
/// constructor, or the set visibility php-src writes `private(set)` or `protected(set)` from the `set` or `init`
/// accessor's access modifier, and `protected(set)` for an `init` without one. A get-only property whose body uses
/// `field` takes `protected(set)`, the set visibility of `readonly`, and one without storage takes none, because PHP
/// refuses a set visibility on a read-only virtual property. PHP drops a set visibility equal to the property's own,
/// so a private property, or a protected one with `protected(set)`, takes none, and the flags carry only what the
/// engine keeps.
fn accessor_flags(modifiers: &Sequence<Modifier>, accessors: &PropertyHookList, names: &ResolvedNames) -> u32 {
    // PHP has no `readonly` static, so a get-only static auto-property is `private(set)`, which its class's initializer
    // writes once, and the analyzer refuses every other write.
    if accessors.is_readonly() && modifiers.contains_static() {
        return if modifiers.contains_private() { 0 } else { ZEND_ACC_PRIVATE_SET };
    }
    if accessors.is_readonly() {
        return ZEND_ACC_READONLY;
    }

    let flags = match accessors.setter() {
        None if names.uses_field(accessors) => ZEND_ACC_PROTECTED_SET,
        None => 0,
        Some(setter) => match setter.modifiers.first() {
            // `init` sets the property while the object is created, in the constructors of its class and its
            // subclasses, so it runs as `protected(set)`, and the analyzer refuses every write after that.
            None | Some(Modifier::Internal(_)) if setter.name.value == b"init" => ZEND_ACC_PROTECTED_SET,
            // PHP has no namespace reach, so the analyzer checks spec section 5's `internal set`, and other classes
            // of the namespace write the property as PHP's public set.
            None | Some(Modifier::Internal(_)) => 0,
            Some(Modifier::Protected(_)) => ZEND_ACC_PROTECTED_SET,
            Some(Modifier::Private(_)) => ZEND_ACC_PRIVATE_SET,
            Some(modifier) => unreachable!("check_slice refuses the accessor modifier `{modifier}`"),
        },
    };

    if modifiers.contains_private() || (modifiers.contains_protected() && flags == ZEND_ACC_PROTECTED_SET) {
        0
    } else {
        flags
    }
}

/// Whether every value of `r#type` but `null` is a collection, and then whether each is a `List`, or `None` when part of
/// it is no collection.
fn collection_kind(r#type: &TUnion) -> Option<bool> {
    let mut lists = None;
    for atomic in r#type.types.iter().filter(|atomic| !atomic.is_null()) {
        let TAtomic::Array(array) = atomic else {
            return None;
        };
        lists = Some(lists.unwrap_or(true) && matches!(array, TArray::List(_)));
    }

    lists
}

/// Whether a value of `r#type` may be a `Map` or a `Set`, which the engine holds as an object.
fn holds_table(r#type: &TUnion) -> bool {
    r#type.types.iter().any(|atomic| matches!(atomic, TAtomic::Array(TArray::Keyed(_) | TArray::Set(_))))
}

/// Whether `==` compares a value of `lhs` with one of `rhs` as an int and a float: both are numbers, `null` aside, and
/// one may hold an int where one may hold a float. The checker refuses every other pair of two different number kinds.
fn mixes_numbers(lhs: &TUnion, rhs: &TUnion) -> bool {
    let [Some([lhs_int, lhs_float]), Some([rhs_int, rhs_float])] = [lhs, rhs].map(number_kinds) else {
        return false;
    };

    (lhs_int || rhs_int) && (lhs_float || rhs_float)
}

/// Whether a value of `union` may be a class value, which `==` compares by its whole type, type arguments included.
fn holds_class_value(union: &TUnion) -> bool {
    union.types.iter().any(|atomic| matches!(atomic, TAtomic::Scalar(TScalar::ClassLikeString(_))))
}

/// Whether a value of `union` may be a value of a type parameter, whose own `==` only the running code knows.
fn holds_type_parameter(union: &TUnion) -> bool {
    union.types.iter().any(|atomic| matches!(atomic, TAtomic::GenericParameter(_)))
}

/// Whether `==` on an unchecked `Any?` of `any` and a value of `other` needs the engine's `==`, spec section 24: a
/// number equals an `int` or a `float` of the same value, and a collection, a struct or an object compares by value. A
/// string, a `bool` or `null` equals only the same one, which PHP's `===` tests.
fn compares_unchecked(any: &TUnion, other: &TUnion) -> bool {
    any.is_mixed() && !other.types.iter().all(|atomic| atomic.is_any_string() || atomic.is_bool() || atomic.is_null())
}

/// The kinds of number the values of `union` are, `null` aside, as `[may hold an int, may hold a float]`, or `None`
/// when a value is no number. A type parameter counts as its constraint.
fn number_kinds(union: &TUnion) -> Option<[bool; 2]> {
    let mut kinds = [false, false];
    for atomic in union.types.iter() {
        match atomic {
            TAtomic::Null => {}
            TAtomic::GenericParameter(parameter) => {
                let [int, float] = number_kinds(&parameter.constraint)?;
                kinds = [kinds[0] || int, kinds[1] || float];
            }
            atomic if atomic.is_int() => kinds[0] = true,
            atomic if atomic.is_float() => kinds[1] = true,
            _ => return None,
        }
    }

    (kinds != [false, false]).then_some(kinds)
}

/// The binary operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided. Spec section 19 compares values strictly, and the checker refuses every pair
/// `===` would compare differently, so `==` and `!=` are `===` and `!==`.
fn binary_kind(binary: &Binary) -> (sharp_kind, u32) {
    match binary.operator {
        BinaryOperator::Addition(_) => (SHARP_AST_BINARY_OP, ZEND_ADD),
        BinaryOperator::Subtraction(_) => (SHARP_AST_BINARY_OP, ZEND_SUB),
        BinaryOperator::Multiplication(_) => (SHARP_AST_BINARY_OP, ZEND_MUL),
        BinaryOperator::Division(_) => (SHARP_AST_BINARY_OP, ZEND_DIV),
        BinaryOperator::Modulo(_) => (SHARP_AST_BINARY_OP, ZEND_MOD),
        BinaryOperator::Exponentiation(_) => (SHARP_AST_BINARY_OP, ZEND_POW),
        BinaryOperator::BitwiseOr(_) => (SHARP_AST_BINARY_OP, ZEND_BW_OR),
        BinaryOperator::BitwiseAnd(_) => (SHARP_AST_BINARY_OP, ZEND_BW_AND),
        BinaryOperator::BitwiseXor(_) => (SHARP_AST_BINARY_OP, ZEND_BW_XOR),
        BinaryOperator::LeftShift(_) => (SHARP_AST_BINARY_OP, ZEND_SL),
        BinaryOperator::RightShift(_) => (SHARP_AST_BINARY_OP, ZEND_SR),
        BinaryOperator::Equal(_) | BinaryOperator::Identical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL),
        BinaryOperator::NotEqual(_) | BinaryOperator::NotIdentical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL),
        BinaryOperator::LessThan(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER),
        BinaryOperator::LessThanOrEqual(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER_OR_EQUAL),
        BinaryOperator::GreaterThan(_) => (SHARP_AST_GREATER, 0),
        BinaryOperator::GreaterThanOrEqual(_) => (SHARP_AST_GREATER_EQUAL, 0),
        BinaryOperator::Spaceship(_) => (SHARP_AST_BINARY_OP, ZEND_SPACESHIP),
        // check_slice refuses `and`, so only the `when` of a `match` arm runs as it.
        BinaryOperator::And(_) | BinaryOperator::LowAnd(_) => (SHARP_AST_AND, 0),
        BinaryOperator::Or(_) => (SHARP_AST_OR, 0),
        BinaryOperator::NullCoalesce(_) => (SHARP_AST_COALESCE, 0),
        BinaryOperator::AngledNotEqual(_)
        | BinaryOperator::StringConcat(_)
        | BinaryOperator::Instanceof(_)
        | BinaryOperator::LowOr(_)
        | BinaryOperator::LowXor(_) => unreachable!("check_slice refuses the operator `{}`", binary.operator),
    }
}

/// The prefix operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided.
fn prefix_kind(operator: &UnaryPrefixOperator) -> (sharp_kind, u32) {
    match operator {
        UnaryPrefixOperator::Negation(_) => (SHARP_AST_UNARY_MINUS, 0),
        UnaryPrefixOperator::Plus(_) => (SHARP_AST_UNARY_PLUS, 0),
        UnaryPrefixOperator::Not(_) => (SHARP_AST_UNARY_OP, ZEND_BOOL_NOT),
        UnaryPrefixOperator::BitwiseNot(_) => (SHARP_AST_UNARY_OP, ZEND_BW_NOT),
        UnaryPrefixOperator::PreIncrement(_) => (SHARP_AST_PRE_INC, 0),
        UnaryPrefixOperator::PreDecrement(_) => (SHARP_AST_PRE_DEC, 0),
        UnaryPrefixOperator::IntCast(..) => (SHARP_AST_CAST, IS_LONG),
        UnaryPrefixOperator::FloatCast(..) => (SHARP_AST_CAST, IS_DOUBLE),
        UnaryPrefixOperator::StringCast(..) => (SHARP_AST_CAST, IS_STRING),
        UnaryPrefixOperator::ErrorControl(_)
        | UnaryPrefixOperator::Reference(_)
        | UnaryPrefixOperator::ArrayCast(..)
        | UnaryPrefixOperator::BoolCast(..)
        | UnaryPrefixOperator::BooleanCast(..)
        | UnaryPrefixOperator::DoubleCast(..)
        | UnaryPrefixOperator::RealCast(..)
        | UnaryPrefixOperator::IntegerCast(..)
        | UnaryPrefixOperator::ObjectCast(..)
        | UnaryPrefixOperator::UnsetCast(..)
        | UnaryPrefixOperator::BinaryCast(..)
        | UnaryPrefixOperator::VoidCast(..) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

/// The operator a compound assignment applies, which an instance runs as the one its class declares: `+=` applies `+`.
const fn compound_operator(operator: &AssignmentOperator) -> Option<BinaryOperator<'static>> {
    Some(match *operator {
        AssignmentOperator::Addition(span) => BinaryOperator::Addition(span),
        AssignmentOperator::Subtraction(span) => BinaryOperator::Subtraction(span),
        AssignmentOperator::Multiplication(span) => BinaryOperator::Multiplication(span),
        AssignmentOperator::Division(span) => BinaryOperator::Division(span),
        AssignmentOperator::Modulo(span) => BinaryOperator::Modulo(span),
        AssignmentOperator::Exponentiation(span) => BinaryOperator::Exponentiation(span),
        _ => return None,
    })
}

/// The assignment operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one
/// does not compile until it is decided.
fn assignment_kind(operator: &AssignmentOperator) -> (sharp_kind, u32) {
    match operator {
        AssignmentOperator::Assign(_) => (SHARP_AST_ASSIGN, 0),
        AssignmentOperator::Addition(_) => (SHARP_AST_ASSIGN_OP, ZEND_ADD),
        AssignmentOperator::Subtraction(_) => (SHARP_AST_ASSIGN_OP, ZEND_SUB),
        AssignmentOperator::Multiplication(_) => (SHARP_AST_ASSIGN_OP, ZEND_MUL),
        AssignmentOperator::Division(_) => (SHARP_AST_ASSIGN_OP, ZEND_DIV),
        AssignmentOperator::Modulo(_) => (SHARP_AST_ASSIGN_OP, ZEND_MOD),
        AssignmentOperator::Exponentiation(_) => (SHARP_AST_ASSIGN_OP, ZEND_POW),
        AssignmentOperator::BitwiseOr(_) => (SHARP_AST_ASSIGN_OP, ZEND_BW_OR),
        AssignmentOperator::BitwiseAnd(_) => (SHARP_AST_ASSIGN_OP, ZEND_BW_AND),
        AssignmentOperator::BitwiseXor(_) => (SHARP_AST_ASSIGN_OP, ZEND_BW_XOR),
        AssignmentOperator::LeftShift(_) => (SHARP_AST_ASSIGN_OP, ZEND_SL),
        AssignmentOperator::RightShift(_) => (SHARP_AST_ASSIGN_OP, ZEND_SR),
        AssignmentOperator::Coalesce(_) => (SHARP_AST_ASSIGN_COALESCE, 0),
        AssignmentOperator::Concat(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use mago_analyzer::artifacts::AnalysisArtifacts;
    use mago_analyzer::effects::Effects;
    use mago_codex::metadata::CodebaseMetadata;
    use mago_database::file::File;
    use mago_names::resolver::NameResolver;
    use mago_syntax::dialect::Dialect;
    use mago_syntax::parser::parse_file_with_dialect;
    use mago_syntax::settings::ParserSettings;

    use super::inline::InlineForms;
    use super::*;

    /// Lowers a class holding `method`, skipping the checks that would refuse it. A construct the checks refuse that
    /// reaches the lowering is a checker bug, so the lowering panics on it.
    fn lower_method(method: &str) {
        let source = format!("class Report\n{{\n    {method}\n}}\n");
        let file = File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Owned(source.into_bytes()));
        let arena = LocalArena::new();
        let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
        let (artifacts, codebase, forms) = (AnalysisArtifacts::new(), CodebaseMetadata::new(), InlineForms::default());
        let effects = Effects::solve(&codebase, &[]);
        let checked = CheckedProgram::unchecked(
            &file,
            program,
            NameResolver::new(&arena).resolve(program),
            &artifacts,
            &codebase,
            &effects,
            &forms,
        );

        let _ = lower(&checked);
    }

    #[test]
    #[should_panic(expected = "check_slice refuses writing to `ConstantAccess`")]
    fn an_assignment_to_a_constant_panics() {
        lower_method("public void run() { PHP_INT_MAX = 1; }");
    }

    /// `??=` reads no operand type, which the unchecked file has none of. An arithmetic one asks whether an instance
    /// runs a declared operator first.
    #[test]
    #[should_panic(expected = "check_slice refuses writing to `ConstantAccess`")]
    fn a_compound_assignment_to_a_constant_panics() {
        lower_method("public void run() { PHP_INT_MAX ??= 1; }");
    }

    #[test]
    #[should_panic(expected = "check_slice refuses writing to `ConstantAccess`")]
    fn an_increment_of_a_constant_panics() {
        lower_method("public void run() { PHP_INT_MAX++; }");
    }

    #[test]
    #[should_panic(expected = "semantics refuses a method parameter without a type")]
    fn a_parameter_without_a_type_panics() {
        lower_method("public void run($extra) {}");
    }
}
