use std::collections::HashSet;

use mago_allocator::LocalArena;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::union::TUnion;
use mago_names::INLINED_COLLECTION_METHODS;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
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
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::ArrowFunction;
use mago_syntax::cst::Assignment;
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::AttributeList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Block;
use mago_syntax::cst::Call;
use mago_syntax::cst::Class;
use mago_syntax::cst::ClassLikeConstant;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Closure;
use mago_syntax::cst::CompositeString;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Construct;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::ExitConstruct;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOfTarget;
use mago_syntax::cst::ForOfVariable;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Hint;
use mago_syntax::cst::If;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Inheritance;
use mago_syntax::cst::Instantiation;
use mago_syntax::cst::Interface;
use mago_syntax::cst::InterpolatedString;
use mago_syntax::cst::Literal;
use mago_syntax::cst::LiteralStringPart;
use mago_syntax::cst::LocalDeclaration;
use mago_syntax::cst::MatchArm;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::NamedArgument;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::NullSafePropertyAccess;
use mago_syntax::cst::NullableHint;
use mago_syntax::cst::Operator;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PartialArgumentList;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::PropertyHook;
use mago_syntax::cst::PropertyHookBody;
use mago_syntax::cst::PropertyHookConcreteBody;
use mago_syntax::cst::PropertyHookConcreteExpressionBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Try;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::Variable;
use mago_syntax::cst::WhileBody;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::utils::pattern::php_shape;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_syntax_core::utils::parse_literal_integer_as_float;
use mago_word::Word;

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
use crate::sharp_kind::SHARP_AST_ISSET;
use crate::sharp_kind::SHARP_AST_MAGIC_CONST;
use crate::sharp_kind::SHARP_AST_MATCH;
use crate::sharp_kind::SHARP_AST_MATCH_ARM;
use crate::sharp_kind::SHARP_AST_MATCH_ARM_LIST;
use crate::sharp_kind::SHARP_AST_METHOD;
use crate::sharp_kind::SHARP_AST_METHOD_CALL;
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
use crate::sharp_kind::SHARP_AST_SILENCE;
use crate::sharp_kind::SHARP_AST_STATIC_CALL;
use crate::sharp_kind::SHARP_AST_STATIC_PROP;
use crate::sharp_kind::SHARP_AST_STMT_LIST;
use crate::sharp_kind::SHARP_AST_THROW;
use crate::sharp_kind::SHARP_AST_TRY;
use crate::sharp_kind::SHARP_AST_TYPE;
use crate::sharp_kind::SHARP_AST_TYPE_UNION;
use crate::sharp_kind::SHARP_AST_UNARY_MINUS;
use crate::sharp_kind::SHARP_AST_UNARY_OP;
use crate::sharp_kind::SHARP_AST_UNARY_PLUS;
use crate::sharp_kind::SHARP_AST_UNPACK;
use crate::sharp_kind::SHARP_AST_UNSET;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_kind::SHARP_AST_WHILE;
use crate::sharp_kind::SHARP_AST_ZVAL;
use crate::sharp_node;
use crate::sharp_str;
use crate::sharp_value;
use crate::store_text;
use crate::unit::Read;

pub(crate) mod checked;
pub(crate) mod inline;
pub(crate) mod types;

use types::DeclarationKind;
use types::Types;
use types::agreed_kind;
use types::class_value_classes;
use types::receiver_classes;

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
const ZEND_ACC_READONLY: u32 = 1 << 7;
const ZEND_ACC_PROTECTED_SET: u32 = 1 << 11;
const ZEND_ACC_PRIVATE_SET: u32 = 1 << 12;
const ZEND_ACC_ENUM: u32 = 1 << 28;
const ZEND_TYPE_NULLABLE: u32 = 1 << 8;
const IS_STATIC: u32 = 15;
const ZEND_PARAM_VARIADIC: u32 = 1 << 4;
const ZEND_PARENTHESIZED_CONDITIONAL: u32 = 1;
const ZEND_BIND_REF: u32 = 1;
const IS_LONG: u32 = 4;
const IS_DOUBLE: u32 = 5;
const IS_STRING: u32 = 6;
const IS_ARRAY: u32 = 7;
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
    /// Any other value, which lives nowhere, so a change changes a copy.
    Value(Reread<'receiver, 'ast>),
}

/// One side of an `==` the lowering writes: an expression of the source, or the hidden variable `name` holding a value
/// of `r#type`, read on the line of `span`, as the element and the value a collection search compares.
enum Side<'side, 'ast> {
    Expression(&'side Expression<'ast>),
    Hidden { span: Span, name: Vec<u8>, r#type: &'side TUnion },
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
    /// The name of the property whose accessor body is being lowered, which `field` reads and writes.
    property: Vec<u8>,
    /// Each inline form the lowering copied, with its fingerprint, as often as it copied it.
    inlined: Vec<Read>,
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
            property: Vec::new(),
            inlined: Vec::new(),
        }
    }

    /// `declare(strict_types=1);` first, then the namespaces and classes. Imports are not lowered: every class name
    /// in the tree is fully qualified.
    fn program(mut self, checked: &CheckedProgram) -> Unit {
        for lambda in Node::Program(checked.program()).filter_map(|node| match node {
            Node::ArrowFunction(arrow_function) => Some(arrow_function.span()),
            Node::Closure(closure) => Some(closure.span()),
            _ => None,
        }) {
            for (_, local) in self.names.captures(&lambda) {
                if self.names.is_written(local) {
                    self.by_reference.insert(local.declaration.start.offset);
                }
            }
        }

        let mut statements = vec![self.strict_types()];
        for statement in &checked.program().statements {
            self.file_statement(statement, &mut statements);
        }

        let root = self.node(SHARP_AST_STMT_LIST, 0, 1, &statements);
        self.nodes[root as usize].end_line = self.lines.last();
        self.inlined.sort();
        self.inlined.dedup();

        Unit { nodes: self.nodes, children: self.children, root, texts: self.texts, inlined: self.inlined }
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
            // never run.
            Statement::Use(_) | Statement::Extern(_) | Statement::Module(_) => {}
            Statement::Class(class) => statements.push(self.class(class)),
            Statement::Interface(interface) => statements.push(self.interface(interface)),
            Statement::Enum(r#enum) => statements.push(self.r#enum(r#enum)),
            _ => unreachable!("check_slice refuses the file statement `{statement}`"),
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
        let mut initial_values = Vec::new();
        for member in &class.members {
            if let ClassLikeMember::Property(property) = member
                && property.initial_value_runs_in_constructor()
                && let Some(value) = property.initial_value()
            {
                initial_values.push(self.initial_value(property.first_variable(), value));
            }
        }

        let (parent, interfaces, parent_name) = match &class.inheritance {
            Some(inheritance) => self.class_header(inheritance),
            None => (NULL, NULL, None),
        };
        let mut members = Vec::new();
        let mut has_constructor = false;
        // A law, spec section 28, is checked and never runs, so the engine gets no node for it.
        for member in class.members.iter().filter(|member| !matches!(member, ClassLikeMember::Law(_))) {
            let member = match member {
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    has_constructor = true;
                    for parameter in &method.parameter_list.parameters {
                        if parameter.is_promoted_property() && set_parameter(parameter).is_some() {
                            members.push(self.promoted_set_property(parameter));
                        }
                    }

                    self.method(method, modifier_flags(&method.modifiers), &initial_values)
                }
                ClassLikeMember::Method(method)
                    if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
                {
                    let call = self.native_call(method, self.names.get(&class.name));
                    let body = if method.returns_value() {
                        self.node(SHARP_AST_RETURN, 0, self.line(method.name.span), &[call])
                    } else {
                        call
                    };

                    self.method(method, modifier_flags(&method.modifiers), &[body])
                }
                ClassLikeMember::Method(method) => self.method(method, modifier_flags(&method.modifiers), &[]),
                ClassLikeMember::Operator(operator) => self.operator(operator),
                ClassLikeMember::Property(property) => self.property(property, parent_name),
                ClassLikeMember::Constant(constant) => self.constant(constant),
                _ => unreachable!("check_slice refuses the class member `{member}`"),
            };
            members.push(member);
        }

        if !has_constructor && !initial_values.is_empty() {
            let line = self.line(class.left_brace);
            if self.types.parent_constructor(self.class).is_some() {
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[]);
                initial_values.push(self.parent_construct(line, arguments));
            }
            let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[]);
            let body = self.node(SHARP_AST_STMT_LIST, 0, line, &initial_values);

            members.push(self.declaration(
                SHARP_AST_METHOD,
                ZEND_ACC_PUBLIC,
                class.class.span,
                class.right_brace,
                b"__construct",
                &[parameters, NULL, body, NULL, NULL],
            ));
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(class.left_brace), &members);
        let attributes = self.attributes(&class.attribute_lists, None);

        self.declaration(
            SHARP_AST_CLASS,
            class_flags(&class.modifiers),
            class.class.span,
            class.right_brace,
            class.name.value,
            &[parent, interfaces, members, attributes, NULL],
        )
    }

    /// A class header's names as PHP's `extends` name and `implements` name list, with the parent's full name: the name
    /// the checker found to be a class is the parent, and the rest are interfaces. Either is null when the header names
    /// none.
    fn class_header(&mut self, inheritance: &Inheritance) -> (u32, u32, Option<&'lowering [u8]>) {
        let mut parent = NULL;
        let mut parent_name = None;
        let mut interfaces = Vec::new();
        for name in &inheritance.types {
            let full_name = self.names.get(name);
            let index = self.string(ZEND_NAME_FQ, self.line(name), full_name);
            match self.types.class_declaration(full_name).kind {
                DeclarationKind::Class => (parent, parent_name) = (index, Some(full_name)),
                DeclarationKind::Interface => interfaces.push(index),
                kind => unreachable!("the checker refuses a {kind:?} in a class header"),
            }
        }

        let interfaces = if interfaces.is_empty() {
            NULL
        } else {
            self.node(SHARP_AST_NAME_LIST, 0, self.line(inheritance), &interfaces)
        };

        (parent, interfaces, parent_name)
    }

    /// A header's names, which PHP compiles as the interface list.
    fn name_list(&mut self, inheritance: &Inheritance) -> u32 {
        let names: Vec<u32> = inheritance
            .types
            .iter()
            .map(|name| self.string(ZEND_NAME_FQ, self.line(name), self.names.get(name)))
            .collect();

        self.node(SHARP_AST_NAME_LIST, 0, self.line(inheritance), &names)
    }

    /// An interface is a class declaration with `ZEND_ACC_INTERFACE`, and its methods are `public`, as php-src's
    /// grammar builds `interface Measured { public function area(): float; }`.
    fn interface(&mut self, interface: &Interface) -> u32 {
        self.class = self.names.get(&interface.name);
        let mut members = Vec::new();
        for member in &interface.members {
            let ClassLikeMember::Method(method) = member else {
                unreachable!("check_slice refuses the interface member `{member}`");
            };

            members.push(self.method(method, ZEND_ACC_PUBLIC, &[]));
        }

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

    /// An enum is a final class with the enum flag, its header as its interface list and its backing type as its last
    /// child, as php-src's grammar builds `enum Status: string implements HasLabel`. An enum has no parent, so its
    /// header needs no mark.
    fn r#enum(&mut self, r#enum: &Enum) -> u32 {
        self.class = self.names.get(&r#enum.name);
        let mut members = Vec::new();
        // A law, spec section 28, is checked and never runs, so the engine gets no node for it.
        for member in r#enum.members.iter().filter(|member| !matches!(member, ClassLikeMember::Law(_))) {
            members.push(match member {
                ClassLikeMember::Method(method) => self.method(method, modifier_flags(&method.modifiers), &[]),
                ClassLikeMember::EnumCase(case) => self.enum_case(case),
                ClassLikeMember::Constant(constant) => self.constant(constant),
                _ => unreachable!("check_slice refuses the enum member `{member}`"),
            });
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(r#enum.left_brace), &members);
        let attributes = self.attributes(&r#enum.attribute_lists, None);
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
        };
        let attributes = self.attributes(&case.attribute_lists, None);

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
        if flags & (ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED | ZEND_ACC_PRIVATE) == 0 {
            unreachable!("check_slice refuses a method without an access modifier");
        }

        self.enter(method.name.value);
        let mut parameters = Vec::new();
        for parameter in &method.parameter_list.parameters {
            parameters.push(self.parameter(parameter));
        }

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, self.line(&method.parameter_list), &parameters);
        let mut statements = if matches!(method.body, MethodBody::Abstract(_)) && first_statements.is_empty() {
            Vec::new()
        } else {
            self.set_parameters(&method.parameter_list)
        };
        statements.extend_from_slice(first_statements);
        if let Some(initializer) = &method.initializer
            && self.types.parent_constructor(self.class).is_some()
        {
            let arguments = self.arguments(&initializer.argument_list, &[]);
            statements.push(self.parent_construct(self.line(initializer), arguments));
        }
        let body = self.method_body(&method.body, method.returns_value(), statements);
        let (start, return_type) = match &method.return_type_hint {
            Some(return_type_hint) => (return_type_hint.span(), self.hint(&return_type_hint.hint)),
            None => (method.name.span, NULL),
        };
        let r#override = method.modifiers.iter().find(|modifier| matches!(modifier, Modifier::Override(_)));
        let attributes = self.attributes(&method.attribute_lists, r#override);

        self.declaration(
            SHARP_AST_METHOD,
            flags,
            start,
            method.body.span(),
            php_method_name(method),
            &[parameters, NULL, body, return_type, attributes],
        )
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
                let expression = self.expression(body.expression);
                statements.push(if returns_value {
                    self.node(SHARP_AST_RETURN, 0, line, &[expression])
                } else {
                    expression
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

                    self.parameter_of_type(parameter, hint)
                }
                _ => self.parameter(parameter),
            });
        }

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, self.line(&operator.parameter_list), &parameters);
        let mut prologue = self.set_parameters(&operator.parameter_list);
        if lifted {
            prologue.push(self.null_lifting(&operator.parameter_list));
        }
        let body = self.method_body(&operator.body, true, prologue);
        let return_type = self.hint(&operator.return_type_hint.hint);
        let attributes = self.attributes(&operator.attribute_lists, None);

        self.declaration(
            SHARP_AST_METHOD,
            modifier_flags(&operator.modifiers),
            &operator.return_type_hint,
            operator.body.span(),
            name,
            &[parameters, NULL, body, return_type, attributes],
        )
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

    /// A variadic parameter carries `ZEND_PARAM_VARIADIC`, as php-src's grammar builds `int ...$values`.
    fn parameter(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("semantics refuses a method parameter without a type");
        };
        let hint = self.hint(hint);

        self.parameter_of_type(parameter, hint)
    }

    /// A parameter with its lowered type, or with a null type, as a lambda's parameter can be. A promoted `Set`
    /// parameter is a plain one, beside the property [`Self::promoted_set_property`] declares.
    fn parameter_of_type(&mut self, parameter: &FunctionLikeParameter, hint: u32) -> u32 {
        let name = self.string(0, self.line(parameter.variable.span), parameter.variable.name);
        let default = parameter.default_value.as_ref().map_or(NULL, |default| self.expression(default.value));
        let (member_flags, hooks) =
            if set_parameter(parameter).is_some() { (0, NULL) } else { self.member_flags_and_hooks(parameter) };
        let variadic_flag = if parameter.is_variadic() { ZEND_PARAM_VARIADIC } else { 0 };
        let attributes = self.attributes(&parameter.attribute_lists, None);

        self.node(
            SHARP_AST_PARAM,
            member_flags | variadic_flag,
            self.line(parameter),
            &[hint, name, default, attributes, NULL, hooks],
        )
    }

    /// The flags and the hooks of the property a constructor parameter promotes, or none for any other parameter.
    fn member_flags_and_hooks(&mut self, parameter: &FunctionLikeParameter) -> (u32, u32) {
        let (accessor_flags, hooks) = match &parameter.hooks {
            Some(accessors) => (
                accessor_flags(&parameter.modifiers, accessors, self.names),
                self.hooks(parameter.variable.name, accessors),
            ),
            None => (0, NULL),
        };

        (modifier_flags(&parameter.modifiers) | accessor_flags, hooks)
    }

    /// The property a promoted `Set` parameter declares, with the visibility, accessors and attributes it is written
    /// with and no default. PHP sets a promoted property from the argument before the constructor's body runs, and a
    /// get-only one only once, so the constructor sets this property to the `Set` of the argument instead.
    fn promoted_set_property(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("a `Set` parameter is written with its type");
        };
        let hint = self.hint(hint);
        let line = self.line(parameter.variable.span);
        let name = self.string(0, line, parameter.variable.name);
        let (flags, hooks) = self.member_flags_and_hooks(parameter);
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, NULL, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let attributes = self.attributes(&parameter.attribute_lists, None);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(parameter), &[hint, declaration, attributes])
    }

    /// A field, a property with accessors or a computed property is a property group of one property, as php-src's
    /// grammar builds `private int $count = 0;`, `public private(set) int $views = 0;`, `public readonly int $id;`,
    /// `public string $name { get => $this->name; }` and `public string $slug { get => expr; }`. A field with no access
    /// modifier is `private`, while PHP makes it `public`. A constant initial value is its default, unless the property
    /// is `readonly`. A member that starts as null without an initial value
    /// takes the default `null` on the line of its name, as php-src builds `private ?int $total = null;`. An override is
    /// a field with `#[\Override]`. PHP refuses a type on a property whose parent's property has none, so an override of
    /// a property of `parent` whose root declaration has no type has none either, down the whole chain (decision 028).
    fn property(&mut self, property: &Property, parent: Option<&[u8]>) -> u32 {
        let (accessor_flags, attribute_lists, hooks) = match property {
            Property::Plain(field) => (0, &field.attribute_lists, NULL),
            Property::Hooked(hooked) => (
                accessor_flags(&hooked.modifiers, &hooked.hook_list, self.names),
                &hooked.attribute_lists,
                self.hooks(hooked.item.variable().name, &hooked.hook_list),
            ),
            Property::Computed(computed) => {
                let hook = self.accessor_bodies(computed.variable.name, |lowering| {
                    let body = lowering.short_body(b"get", &computed.body);

                    lowering.hook(b"get", computed.body.arrow, &computed.body, body)
                });

                (
                    0,
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
        let hint = match (r#override, parent) {
            (Some(_), Some(parent))
                if self.types.member_declaration(parent, variable.name).kind
                    == (DeclarationKind::Property { typed: false }) =>
            {
                NULL
            }
            _ => self.hint(hint),
        };
        let line = self.line(variable);
        let name = self.string(0, line, variable.name);
        let default = match property.initial_value() {
            Some(value) if !property.initial_value_runs_in_constructor() => self.expression(value),
            None if is_null_by_default(property, self.names) => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
            _ => NULL,
        };
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, default, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let access = if property.modifiers().contains_visibility() { 0 } else { ZEND_ACC_PRIVATE };
        let flags = modifier_flags(property.modifiers()) | access | accessor_flags;
        let attributes = self.attributes(attribute_lists, r#override);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(property), &[hint, declaration, attributes])
    }

    /// A property's hook list, or null when no accessor has a body: an auto-property is plain storage. Each body is a
    /// hook. An auto accessor beside a body that uses `field` is PHP's backing store, so it is no hook, and beside
    /// bodies that never use `field` it is the hook over the storage, so PHP keeps the property backed, as C# does.
    fn hooks(&mut self, property: &[u8], accessors: &PropertyHookList) -> u32 {
        if !accessors.hooks.iter().any(|accessor| matches!(accessor.body, PropertyHookBody::Concrete(_))) {
            return NULL;
        }

        let uses_field = self.names.uses_field(accessors);
        self.accessor_bodies(property, |lowering| {
            let mut hooks = Vec::new();
            for accessor in &accessors.hooks {
                let body = match &accessor.body {
                    PropertyHookBody::Concrete(PropertyHookConcreteBody::Block(block)) => lowering.block(block),
                    PropertyHookBody::Concrete(PropertyHookConcreteBody::Expression(body)) => {
                        lowering.short_body(accessor.name.value, body)
                    }
                    PropertyHookBody::Abstract(_) if uses_field => continue,
                    PropertyHookBody::Abstract(_) => lowering.storage_body(accessor),
                };
                hooks.push(lowering.hook(accessor.name.value, accessor.name, accessor, body));
            }

            lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(accessors), &hooks)
        })
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

    /// A hook named `get` or `set` with its body, as php-src's grammar declares every hook.
    fn hook(&mut self, name: &[u8], start: impl HasSpan, end: impl HasSpan, body: u32) -> u32 {
        self.declaration(SHARP_AST_PROPERTY_HOOK, 0, start, end, name, &[NULL, NULL, body, NULL, NULL])
    }

    /// The body of `get => expr;`, the short body php-src's grammar builds, on the arrow's line. A `set` with an
    /// expression body is the statement list `set { expr; }`, because PHP stores the result of `set => expr;`.
    fn short_body(&mut self, accessor: &[u8], body: &PropertyHookConcreteExpressionBody) -> u32 {
        let line = self.line(body.arrow);
        let expression = self.expression(body.expression);
        let kind = if accessor == b"get" { SHARP_AST_PROPERTY_HOOK_SHORT_BODY } else { SHARP_AST_STMT_LIST };

        self.node(kind, 0, line, &[expression])
    }

    /// An auto accessor's body over the storage: `get => $this->name;` or `set { $this->name = $value; }`.
    fn storage_body(&mut self, accessor: &PropertyHook) -> u32 {
        let line = self.line(accessor);
        let storage = self.storage(line);
        if accessor.name.value == b"get" {
            self.node(SHARP_AST_PROPERTY_HOOK_SHORT_BODY, 0, line, &[storage])
        } else {
            let value = self.variable(accessor.name.span, b"value");
            let assignment = self.node(SHARP_AST_ASSIGN, 0, line, &[storage, value]);

            self.node(SHARP_AST_STMT_LIST, 0, line, &[assignment])
        }
    }

    /// `field`: `$this->name` of the property whose accessor body is being lowered, which PHP reads and writes as the
    /// storage inside the property's own hook.
    fn storage(&mut self, line: u32) -> u32 {
        let this = self.string(0, line, b"this");
        let this = self.node(SHARP_AST_VAR, 0, line, &[this]);
        let name = store_text(&mut self.texts, &self.property);
        let name = self.zval(line, sharp_value::SHARP_STRING, |node| node.text = name);

        self.node(SHARP_AST_PROP, 0, line, &[this, name])
    }

    /// A declaration's attributes are one `ATTRIBUTE_LIST` with an `ATTRIBUTE_GROUP` per `[...]`, as php-src's grammar
    /// builds `#[...]`, or null without attributes. Each attribute names its class by its full name. A method's or a
    /// field's `override` adds a last group of `#[\Override]`.
    fn attributes(&mut self, lists: &Sequence<AttributeList>, r#override: Option<&Modifier>) -> u32 {
        let line = match (lists.first(), r#override) {
            (Some(first), _) => self.line(first),
            (None, Some(r#override)) => self.line(r#override),
            (None, None) => return NULL,
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

        self.node(SHARP_AST_ATTRIBUTE_LIST, 0, line, &groups)
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
            modifier_flags(&constant.modifiers),
            self.line(constant),
            &[declaration, NULL, hint],
        )
    }

    /// `$this->name = value;` on the line of the member's name.
    fn initial_value(&mut self, variable: &DirectVariable, value: &Expression) -> u32 {
        let line = self.line(variable);
        let this = self.variable(variable.span, b"this");
        let name = self.string(0, line, variable.name);
        let property = self.node(SHARP_AST_PROP, 0, line, &[this, name]);
        let value = self.expression(value);

        self.node(SHARP_AST_ASSIGN, 0, line, &[property, value])
    }

    /// A built-in type is written unqualified, and a class by its full name. `Self` is a `TYPE` node of `IS_STATIC`, as
    /// php-src's grammar builds `static`. A `List` or `Map` is a PHP array, so its type is `array`, as php-src's grammar
    /// builds it. A function type runs as PHP's `\Closure`. `Any` and `Any?` are PHP's `mixed`, which already holds
    /// null. Any other nullable type is its type with `ZEND_TYPE_NULLABLE`, as
    /// php-src's grammar builds `?int`.
    fn hint(&mut self, hint: &Hint) -> u32 {
        match hint {
            Hint::Integer(name) | Hint::Float(name) | Hint::Bool(name) | Hint::String(name) | Hint::Void(name) => {
                self.string(ZEND_NAME_NOT_FQ, self.line(name.span), name.value)
            }
            Hint::Mixed(any) => self.string(ZEND_NAME_NOT_FQ, self.line(any.span), b"mixed"),
            Hint::Identifier(class) => self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)),
            Hint::Self_(keyword) => self.node(SHARP_AST_TYPE, IS_STATIC, self.line(keyword), &[]),
            Hint::Generic(generic) => self.node(SHARP_AST_TYPE, IS_ARRAY, self.line(generic), &[]),
            Hint::Function(function) => self.string(ZEND_NAME_FQ, self.line(function), b"Closure"),
            Hint::Nullable(NullableHint { hint: any @ Hint::Mixed(_), .. }) => self.hint(any),
            Hint::Nullable(NullableHint { question_mark, hint: Hint::Parenthesized(parenthesized) }) => {
                self.union(parenthesized.hint, Some(*question_mark))
            }
            Hint::Nullable(nullable) => {
                let index = self.hint(nullable.hint);
                self.nodes[index as usize].attr |= ZEND_TYPE_NULLABLE;

                index
            }
            Hint::Union(_) => self.union(hint, None),
            _ => unreachable!("check_slice refuses the type `{hint}`"),
        }
    }

    /// A union is one `TYPE_UNION` list of its types in the order they are written, on the line of its first type, as
    /// php-src's `union_type` rule builds `int|string`. A union in parentheses with `?` after it, `(int|string)?`,
    /// ends its list with the name `null` on the line of the `?`, as php-src builds `int|string|null`.
    fn union(&mut self, union: &Hint, question_mark: Option<Span>) -> u32 {
        let mut types = Vec::new();
        for member in union_members(union) {
            types.push(self.hint(member));
        }
        if let Some(question_mark) = question_mark {
            types.push(self.string(ZEND_NAME_NOT_FQ, self.line(question_mark), b"null"));
        }
        let line = self.nodes[types[0] as usize].line;

        self.node(SHARP_AST_TYPE_UNION, 0, line, &types)
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
            Statement::Return(r#return) => {
                let value = r#return.value.map_or(NULL, |value| self.expression(value));

                self.node(SHARP_AST_RETURN, 0, self.line(r#return), &[value])
            }
            Statement::LocalDeclaration(local) => self.local(local),
            Statement::If(r#if) => self.r#if(r#if),
            Statement::For(r#for) => self.r#for(r#for),
            Statement::ForOf(for_of) => {
                let collection = self.expression(for_of.expression);
                let (key, value) = match &for_of.target {
                    ForOfTarget::Value(value) => (NULL, self.variable(value.name.span, value.name.value)),
                    ForOfTarget::KeyValue(pair) => (
                        self.variable(pair.key.name.span, pair.key.name.value),
                        self.variable(pair.value.name.span, pair.value.name.value),
                    ),
                };
                let mut body = self.loop_body(for_of.body);

                // A `Map` keyed by a backed enum holds each key as its backing value, so the key reads back as its
                // case. PHP stores an all-digit `string` key as an `int`, so a `Map<string, V>` key reads back through
                // `(string)`, as spec section 12 reads it. Both follow the `Map`'s key type, written on the key or not.
                if let ForOfTarget::KeyValue(pair) = &for_of.target
                    && let Some(key_type) = self.types.map_key_type(self.types.expression_type(for_of.expression))
                    && let Some(read_back) = self.key_read_back(&pair.key, &key_type)
                {
                    let line = self.line(&pair.key);
                    let key = self.variable(pair.key.name.span, pair.key.name.value);
                    let assignment = self.node(SHARP_AST_ASSIGN, 0, line, &[key, read_back]);

                    body = self.node(SHARP_AST_STMT_LIST, 0, line, &[assignment, body]);
                }

                self.node(SHARP_AST_FOREACH, 0, self.line(for_of), &[collection, value, key, body])
            }
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

    fn loop_body(&mut self, body: &Statement) -> u32 {
        self.loop_depth += 1;
        let body = self.statement(body);
        self.loop_depth -= 1;

        body
    }

    /// A `let` or `const` local is the assignment of its value to its variable. Spec section 3 gives each loop pass
    /// its own local, and PHP reuses one variable, so a local in a loop body that a lambda captures by reference is
    /// `{ unset($local); $local = value; }`, and the lambda of each pass keeps its own.
    fn local(&mut self, local: &LocalDeclaration) -> u32 {
        let line = self.line(local);
        let variable = self.variable(local.name.span, local.name.value);
        let value = self.expression(local.value);
        let assignment = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, value]);
        if self.loop_depth == 0 || !self.by_reference.contains(&local.name.span.start.offset) {
            return assignment;
        }

        let variable = self.variable(local.name.span, local.name.value);
        let unset = self.node(SHARP_AST_UNSET, 0, line, &[variable]);
        let unset = self.node(SHARP_AST_STMT_LIST, 0, line, &[unset]);

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
        let mut catches = Vec::new();
        for clause in &r#try.catch_clauses {
            catches.push(self.catch(clause));
        }

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
        let variable =
            clause.variable.as_ref().map_or(NULL, |variable| self.string(0, self.line(variable.span), variable.name));
        let block = self.block(&clause.block);

        self.node(SHARP_AST_CATCH, 0, line, &[classes, variable, block])
    }

    fn expression(&mut self, expression: &Expression) -> u32 {
        let line = self.line(expression);
        if let Some((_, name)) = self.null_safe_receivers.iter().find(|(span, _)| *span == expression.span()) {
            let name = name.clone();

            return self.variable(expression.span(), &name);
        }
        if let Some((link, receiver)) = self.untested_link(expression) {
            return self.null_safe_chain(expression, link, receiver);
        }

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
            Expression::ConstantAccess(name) => self.name(name),
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

                let (kind, attr) = prefix_kind(&unary.operator);
                let operand = if unary.operator.is_increment_or_decrement() {
                    self.target(unary.operand)
                } else {
                    self.expression(unary.operand)
                };

                self.node(kind, attr, line, &[operand])
            }
            Expression::UnaryPostfix(unary) => {
                let kind = match unary.operator {
                    UnaryPostfixOperator::PostIncrement(_) => SHARP_AST_POST_INC,
                    UnaryPostfixOperator::PostDecrement(_) => SHARP_AST_POST_DEC,
                };
                let operand = self.target(unary.operand);

                self.node(kind, 0, line, &[operand])
            }
            Expression::Assignment(assignment) => {
                // A compound assignment to an instance assigns what the operator its class declares returns.
                if let Some(operator) = compound_operator(&assignment.operator)
                    && let Some(name) = php_operator_name(&operator, 2)
                    && let Some(class) = self.types.operator_class(
                        name,
                        &[self.types.expression_type(assignment.lhs), self.types.expression_type(assignment.rhs)],
                    )
                {
                    return self.compound_assignment(line, assignment, |this, target, value| {
                        this.operator_call(line, class, name, &[target, value])
                    });
                }

                match (&assignment.operator, self.operand_types_of(assignment)) {
                    (AssignmentOperator::Addition(_), Operands::Strings) => {
                        let lhs = self.target(assignment.lhs);
                        let rhs = self.expression(assignment.rhs);

                        self.node(SHARP_AST_ASSIGN_OP, ZEND_CONCAT, line, &[lhs, rhs])
                    }
                    (AssignmentOperator::Division(_), Operands::Ints) => {
                        self.compound_assignment(line, assignment, |this, target, value| {
                            this.intdiv(line, target, value)
                        })
                    }
                    (operator, _) => {
                        let (kind, attr) = assignment_kind(operator);
                        let lhs = self.target(assignment.lhs);
                        let rhs = self.expression(assignment.rhs);

                        self.node(kind, attr, line, &[lhs, rhs])
                    }
                }
            }
            Expression::Call(Call::Method(call)) => self.method_call(expression, call),
            Expression::Call(Call::Function(FunctionCall {
                function: Expression::Identifier(function),
                argument_list,
            })) => {
                // A local holds a function, so `f(x)` calls it as PHP's `$f($x)`, and any other name is the global
                // function's.
                let function = match self.names.binding(function) {
                    Some(Binding::Local(_)) => self.variable(function.span(), function.value()),
                    _ => self.string(ZEND_NAME_FQ, self.line(function), function.value()),
                };
                let arguments = self.arguments(argument_list, &[]);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            Expression::Construct(Construct::Exit(ExitConstruct { arguments: Some(arguments), .. })) => {
                let function = self.string(ZEND_NAME_FQ, line, b"exit");
                let arguments = self.arguments(arguments, &[]);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            Expression::Instantiation(Instantiation {
                class: Expression::Identifier(class),
                argument_list: Some(arguments),
                ..
            }) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));
                let arguments = self.arguments(arguments, &[]);

                self.node(SHARP_AST_NEW, 0, line, &[class, arguments])
            }
            // `new Self(…)` is `new static(…)`, the name php-src's grammar writes with `ZEND_NAME_NOT_FQ`.
            Expression::Instantiation(Instantiation {
                class: Expression::Self_(keyword),
                argument_list: Some(arguments),
                ..
            }) => {
                let class = self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"static");
                let arguments = self.arguments(arguments, &[]);

                self.node(SHARP_AST_NEW, 0, line, &[class, arguments])
            }
            // `Class.y`, and `y` read through a class value, is the fetch of the member the checker found on the class:
            // a constant or enum case, a static property, or a static method as a first-class callable.
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let full_name = self.names.get(&class.name);
                    let class = self.string(ZEND_NAME_FQ, self.line(class), full_name);

                    self.static_member(line, class, &[full_name], &access.property)
                }
                None => match self.class_value(access.object) {
                    Some((class, classes)) => self.static_member(line, class, &classes, &access.property),
                    None => {
                        let is_method = self.is_method_value(access.object, &access.property);
                        let object = self.expression(access.object);
                        let member = self.member(&access.property);

                        if is_method {
                            self.method_value(line, object, member)
                        } else {
                            self.node(SHARP_AST_PROP, 0, line, &[object, member])
                        }
                    }
                },
            },
            // A link whose chain's conditional tests the receiver, which `untested_link` found, is the static call
            // through a class value, the property call, or the plain method call.
            Expression::Call(Call::NullSafeMethod(call)) => {
                if let Some(lowered) = self.collection_call(expression, call.object, &call.argument_list, true) {
                    return lowered;
                }

                let tested = self.tested_links.contains(&expression.span());
                if tested && let Some((class, _)) = self.class_value(call.object) {
                    let method = self.member(&call.method);
                    let arguments = self.arguments(&call.argument_list, &[]);

                    return self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments]);
                }

                let is_property_call = tested && self.is_property_call(expression, call.object);
                let object = if tested { self.expression(call.object) } else { self.null_safe_object(call.object) };
                let method = self.member(&call.method);
                let keys = self.types.key_arguments(expression);
                let arguments = self.arguments(&call.argument_list, &keys);

                if is_property_call {
                    let property = self.node(SHARP_AST_PROP, 0, line, &[object, method]);

                    self.node(SHARP_AST_CALL, 0, line, &[property, arguments])
                } else if tested {
                    self.node(SHARP_AST_METHOD_CALL, 0, line, &[object, method, arguments])
                } else {
                    self.node(SHARP_AST_NULLSAFE_METHOD_CALL, 0, line, &[object, method, arguments])
                }
            }
            // A link whose chain's conditional tests the receiver, which `untested_link` found, is the static member
            // read through a class value, the method value, or the plain property read.
            Expression::Access(Access::NullSafeProperty(access)) => {
                let tested = self.tested_links.contains(&expression.span());
                if tested && let Some((class, classes)) = self.class_value(access.object) {
                    return self.static_member(line, class, &classes, &access.property);
                }

                let is_method = tested && self.is_method_value(access.object, &access.property);
                let object = if tested { self.expression(access.object) } else { self.null_safe_object(access.object) };
                let member = self.member(&access.property);

                if is_method {
                    self.method_value(line, object, member)
                } else if tested {
                    self.node(SHARP_AST_PROP, 0, line, &[object, member])
                } else {
                    self.node(SHARP_AST_NULLSAFE_PROP, 0, line, &[object, member])
                }
            }
            Expression::TypeOf(type_of) => {
                let class = self.string(ZEND_NAME_FQ, self.line(type_of.class), self.names.get(&type_of.class));

                self.node(SHARP_AST_CLASS_NAME, 0, line, &[class])
            }
            Expression::Throw(throw) => {
                let exception = self.expression(throw.exception);

                self.node(SHARP_AST_THROW, 0, line, &[exception])
            }
            Expression::CompositeString(CompositeString::Interpolated(template)) => self.template(template),
            Expression::ArrowFunction(arrow_function) => self.arrow_function(arrow_function),
            Expression::Closure(closure) => self.closure(closure),
            Expression::Array(array) if self.is_set(expression) => self.set_literal(array),
            Expression::Array(array) => self.array(array),
            Expression::ArrayAccess(access) => {
                let value = self.expression(access.array);
                let key = self.key(access.index);

                self.node(SHARP_AST_DIM, 0, line, &[value, key])
            }
            _ => unreachable!("check_slice refuses the expression `{expression}`"),
        })
    }

    /// The fetch of the static `member` of `class`, whose node is `class` and whose possible classes are `classes`: a
    /// class constant fetch for a constant or an enum case, a static property fetch, or a first-class callable of the
    /// static method.
    fn static_member(&mut self, line: u32, class: u32, classes: &[&[u8]], member: &ClassLikeMemberSelector) -> u32 {
        let ClassLikeMemberSelector::Identifier(name) = member else {
            unreachable!("check_slice refuses the member name `{member}`");
        };
        let kind = agreed_kind(classes.iter().map(|class| self.types.member_declaration(class, name.value).kind));
        let member = self.member(member);

        match kind {
            DeclarationKind::Constant | DeclarationKind::EnumCase => {
                self.node(SHARP_AST_CLASS_CONST, 0, line, &[class, member])
            }
            DeclarationKind::StaticProperty => self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, member]),
            DeclarationKind::StaticMethod => {
                let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

                self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, member, callable])
            }
            kind => unreachable!("a class's member read names a static member, not a {kind:?}"),
        }
    }

    /// The class node of a static member read or call through `object` and the classes it can be, when `object` is a
    /// class value: `typeof(X)` is the name `X`, and any other class value is the value, as in PHP's `$type::y`.
    fn class_value(&mut self, object: &Expression) -> Option<(u32, Vec<&'lowering [u8]>)> {
        let classes = class_value_classes(self.types.expression_type(object))?;
        let class = match object {
            Expression::TypeOf(type_of) => {
                self.string(ZEND_NAME_FQ, self.line(type_of.class), self.names.get(&type_of.class))
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
    /// kind on every class the receiver can be.
    fn is_method_value(&self, object: &Expression, member: &ClassLikeMemberSelector) -> bool {
        let (Some(classes), ClassLikeMemberSelector::Identifier(name)) =
            (receiver_classes(self.types.expression_type(object)), member)
        else {
            return false;
        };
        let kind = agreed_kind(classes.into_iter().map(|class| self.types.member_declaration(class, name.value).kind));

        matches!(kind, DeclarationKind::Method { .. } | DeclarationKind::StaticMethod)
    }

    /// Whether the method call `call` on `object` runs the function the property of that name holds.
    fn is_property_call(&self, call: &Expression, object: &Expression) -> bool {
        receiver_classes(self.types.expression_type(object)).is_some()
            && matches!(self.types.call_target(call).kind, DeclarationKind::Property { .. })
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
    /// `rhs`'s: the `op_Equality` the class of an instance side declares, which lifts a null side itself, an int and a
    /// float as two floats, and `===` otherwise. `== null` tests for null and runs no operator. A collection search
    /// compares each element with its value through it, so a search never compares differently from `==`.
    fn equality(&mut self, line: u32, lhs: Side, rhs: Side, negated: bool) -> u32 {
        let (lhs_type, rhs_type) = (self.side_type(&lhs), self.side_type(&rhs));
        if let Some((class, name)) = self.declared_equality(lhs_type, rhs_type) {
            let lhs = self.side(&lhs);
            let rhs = self.side(&rhs);
            let call = self.operator_call(line, class, name, &[lhs, rhs]);

            return if negated { self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[call]) } else { call };
        }
        if mixes_numbers(lhs_type, rhs_type) {
            return self.float_equality(line, &lhs, &rhs, negated);
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
            Side::Hidden { r#type, .. } => r#type,
        }
    }

    /// A read of `side`: the expression, lowered, or its hidden variable.
    fn side(&mut self, side: &Side) -> u32 {
        match side {
            Side::Expression(expression) => self.expression(expression),
            Side::Hidden { span, name, .. } => self.variable(*span, name),
        }
    }

    fn side_span(side: &Side) -> Span {
        match side {
            Side::Expression(expression) => expression.span(),
            Side::Hidden { span, .. } => *span,
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
            None => (self.target(assignment.lhs), self.expression(assignment.lhs)),
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

    /// `\Class::name(arguments)`, the static method `class` declares to run an operator.
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

    /// `$object->member(...)`, the method as a first-class callable.
    fn method_value(&mut self, line: u32, object: u32, member: u32) -> u32 {
        let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

        self.node(SHARP_AST_METHOD_CALL, 0, line, &[object, member, callable])
    }

    /// The highest null-safe link in `chain` that PHP's `?->` cannot write, a method value, a property call, or any
    /// link below a collection method call that runs as PHP on the array, with its receiver, when `chain` is a member
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
                Expression::Access(Access::Property(access)) => access.object,
                Expression::Call(Call::Method(call)) => {
                    below_php_call = below_php_call || self.php_collection_method(link).is_some();

                    call.object
                }
                Expression::ArrayAccess(access) => access.array,
                Expression::Parenthesized(parenthesized) => parenthesized.expression,
                Expression::Access(Access::NullSafeProperty(access)) => {
                    if !self.tested_links.contains(&link.span())
                        && (below_php_call
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
                            || self.is_class_value(call.object)
                            || self.is_property_call(link, call.object))
                    {
                        return Some((link, call.object));
                    }

                    call.object
                }
                _ => return None,
            };
        }
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

    /// The object of `?.`. An index there reads a missing key as null, as `??` does, so `x[k]?.name` is
    /// `($x[$k] ?? null)?->name`, where a bare `x[k]` throws on a missing key (spec section 12).
    fn null_safe_object(&mut self, object: &Expression) -> u32 {
        let value = self.expression(object);
        if !matches!(object.unparenthesized(), Expression::ArrayAccess(_)) {
            return value;
        }

        let line = self.line(object);
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_COALESCE, 0, line, &[value, null])
    }

    /// Spec section 3: a lambda captures the variable itself. PHP's `fn` captures by value what its body reads, which
    /// is the variable itself when nothing writes it, so a lambda with an expression body that captures no written
    /// local is an `ARROW_FUNC`, as php-src's grammar builds `fn`, with its expression as its body. Any other is a
    /// `CLOSURE` whose body returns the expression, as is one with a `Set` parameter, which `fn` has no statement
    /// before its expression to make a `Set`.
    fn arrow_function(&mut self, arrow_function: &ArrowFunction) -> u32 {
        let lambda = arrow_function.span();
        if !self.names.captures(&lambda).iter().any(|(_, local)| self.names.is_written(local))
            && !arrow_function.parameter_list.parameters.iter().any(|parameter| set_parameter(parameter).is_some())
        {
            let parameters = self.lambda_parameters(&arrow_function.parameter_list);
            let body = self.lambda_body(|lowering| lowering.expression(arrow_function.expression));

            return self.declaration(
                SHARP_AST_ARROW_FUNC,
                0,
                lambda,
                arrow_function.expression,
                b"",
                &[parameters, NULL, body, NULL, NULL],
            );
        }

        let parameters = self.lambda_parameters(&arrow_function.parameter_list);
        let uses = self.closure_uses(lambda);
        let body = self.lambda_body(|lowering| {
            let line = lowering.line(arrow_function.expression);
            let mut statements = lowering.set_parameters(&arrow_function.parameter_list);
            let value = lowering.expression(arrow_function.expression);
            statements.push(lowering.node(SHARP_AST_RETURN, 0, line, &[value]));

            lowering.node(SHARP_AST_STMT_LIST, 0, line, &statements)
        });

        self.declaration(SHARP_AST_CLOSURE, 0, lambda, lambda, b"", &[parameters, uses, body, NULL, NULL])
    }

    /// A lambda with a block body is a `CLOSURE`, as php-src's grammar builds `function () use (…) { … }`, which makes
    /// each `Set` parameter a `Set` first.
    fn closure(&mut self, closure: &Closure) -> u32 {
        let lambda = closure.span();
        let parameters = self.lambda_parameters(&closure.parameter_list);
        let uses = self.closure_uses(lambda);
        let body = self.lambda_body(|lowering| {
            let mut statements = lowering.set_parameters(&closure.parameter_list);
            for statement in &closure.body.statements {
                statements.push(lowering.statement(statement));
            }

            lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(&closure.body), &statements)
        });

        self.declaration(SHARP_AST_CLOSURE, 0, lambda, lambda, b"", &[parameters, uses, body, NULL, NULL])
    }

    /// A lambda's parameters, each with its type or a null type, as PHP writes a parameter without one.
    fn lambda_parameters(&mut self, list: &FunctionLikeParameterList) -> u32 {
        let mut parameters = Vec::new();
        for parameter in &list.parameters {
            let hint = parameter.hint.as_ref().map_or(NULL, |hint| self.hint(hint));
            parameters.push(self.parameter_of_type(parameter, hint));
        }

        self.node(SHARP_AST_PARAM_LIST, 0, self.line(list), &parameters)
    }

    /// The `use` list of a `CLOSURE`: each local the lambda captures, in first-use order, by reference with
    /// `ZEND_BIND_REF` when code writes it and by value otherwise, or null when it captures none.
    fn closure_uses(&mut self, lambda: Span) -> u32 {
        let line = self.line(lambda);
        let mut uses = Vec::new();
        for &(name, local) in self.names.captures(&lambda) {
            let attr = if self.names.is_written(&local) { ZEND_BIND_REF } else { 0 };
            uses.push(self.string(attr, line, name));
        }

        if uses.is_empty() { NULL } else { self.node(SHARP_AST_CLOSURE_USES, 0, line, &uses) }
    }

    /// Lowers a lambda's body, which runs in a frame of its own, outside any loop of the method around it.
    fn lambda_body(&mut self, lower: impl FnOnce(&mut Self) -> u32) -> u32 {
        let loop_depth = std::mem::replace(&mut self.loop_depth, 0);
        let body = lower(self);
        self.loop_depth = loop_depth;

        body
    }

    /// A list or map literal is an `ARRAY` with `ZEND_ARRAY_SYNTAX_SHORT`, as php-src's grammar builds `[…]`. Each
    /// element is an `ARRAY_ELEM` of its value and its key or null, and a spread is an `UNPACK` of its value.
    fn array(&mut self, array: &Array) -> u32 {
        if array.elements.iter().any(|element| self.is_map_spread(element)) {
            return self.map_with_spreads(array);
        }

        let elements: Vec<u32> = array.elements.iter().map(|element| self.array_element(element)).collect();

        self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, self.line(array), &elements)
    }

    fn array_element(&mut self, element: &ArrayElement) -> u32 {
        let (kind, value_and_key) = match element {
            ArrayElement::Value(element) => (SHARP_AST_ARRAY_ELEM, vec![self.expression(element.value), NULL]),
            ArrayElement::KeyValue(element) => {
                (SHARP_AST_ARRAY_ELEM, vec![self.expression(element.value), self.key(element.key)])
            }
            ArrayElement::Variadic(element) => (SHARP_AST_UNPACK, vec![self.expression(element.value)]),
            ArrayElement::Missing(_) => unreachable!("check_slice refuses a missing literal element"),
        };

        self.node(kind, 0, self.line(element), &value_and_key)
    }

    /// A key going into a `Map`, in a literal, an index or an argument a `Map` method takes as a key, or an element
    /// going into a `Set` as its key.
    fn key(&mut self, key: &Expression) -> u32 {
        let lowered = self.expression(key);

        self.stored_key(key, lowered)
    }

    /// `lowered`, a read of `key`, as the key a `Map` or a `Set` holds. One keyed by a backed enum holds each case as its
    /// backing value, so a case goes in as its `->value`.
    fn stored_key(&mut self, key: &Expression, lowered: u32) -> u32 {
        if self.backed_enum(self.types.expression_type(key)).is_none() {
            return lowered;
        }

        let line = self.line(key);
        let value = self.string(0, line, b"value");

        self.node(SHARP_AST_PROP, 0, line, &[lowered, value])
    }

    /// The loop key `key` of a `Map` keyed by `key_type`, read back as the value the `Map` was given: the case of a
    /// backed enum through its `from`, or a `string` through `(string)`. None when the stored key is that value.
    fn key_read_back(&mut self, key: &ForOfVariable, key_type: &TUnion) -> Option<u32> {
        let line = self.line(key);
        if let Some(class) = self.backed_enum(key_type) {
            let stored_key = self.variable(key.name.span, key.name.value);
            let class = self.string(ZEND_NAME_FQ, line, class);
            let from = self.string(0, line, b"from");
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[stored_key]);

            return Some(self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, from, arguments]));
        }
        if !key_type.is_string() {
            return None;
        }

        let stored_key = self.variable(key.name.span, key.name.value);

        Some(self.node(SHARP_AST_CAST, IS_STRING, line, &[stored_key]))
    }

    /// The backed enum every value of `r#type` is a case of, if there is one.
    fn backed_enum<'r#type>(&self, r#type: &'r#type TUnion) -> Option<&'r#type [u8]> {
        let classes = receiver_classes(r#type)?;
        let class = classes[0];

        (classes.iter().all(|other| other.eq_ignore_ascii_case(class))
            && self.types.class_declaration(class).kind == DeclarationKind::Enum { backed: true })
        .then_some(class)
    }

    /// Whether `element` spreads a `Map`, which keeps its keys where PHP's `...` renumbers int keys.
    fn is_map_spread(&self, element: &ArrayElement) -> bool {
        matches!(element, ArrayElement::Variadic(spread)
            if !self.types.expression_type(spread.value).types.iter().all(|atomic| atomic.is_list() || atomic.is_never()))
    }

    /// A `Map` literal with a spread is `\array_replace` of its parts in order, each spread `Map` and each run of
    /// entries as a literal, so every key stays and a later one wins (decision 031).
    fn map_with_spreads(&mut self, array: &Array) -> u32 {
        let line = self.line(array);
        let mut parts = Vec::new();
        let mut entries = Vec::new();
        for element in &array.elements {
            match element {
                ArrayElement::Variadic(spread) => {
                    if !entries.is_empty() {
                        parts.push(self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &entries));
                        entries.clear();
                    }
                    parts.push(self.expression(spread.value));
                }
                ArrayElement::KeyValue(_) => entries.push(self.array_element(element)),
                _ => unreachable!("the checker refuses a `List` part in a `Map` literal"),
            }
        }
        if !entries.is_empty() {
            parts.push(self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &entries));
        }

        let function = self.string(ZEND_NAME_FQ, line, b"array_replace");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &parts);

        self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
    }

    /// Whether the analysis gave `expression` a `Set` type, as it gives a list literal where a `Set` is declared.
    fn is_set(&self, expression: &Expression) -> bool {
        self.types
            .expression_type(expression)
            .types
            .iter()
            .any(|atomic| matches!(atomic, TAtomic::Array(TArray::Set(_))))
    }

    /// Spec section 12 stores a `Set` as the array of its elements, each under its key, so a `Set` literal is
    /// `["vip" => "vip"]`. One with a spread is `\Sharp\Set::from` of the list literal.
    fn set_literal(&mut self, array: &Array) -> u32 {
        let line = self.line(array);
        if array.elements.iter().any(|element| matches!(element, ArrayElement::Variadic(_))) {
            let list = self.array(array);

            return self.set_from(line, list);
        }

        let mut entries = Vec::new();
        for element in &array.elements {
            let ArrayElement::Value(element) = element else {
                unreachable!("the checker refuses a `Map` literal where a `Set` is declared");
            };
            let mut value = self.reread(element.value, "element");
            let key = self.set_key(&mut value);
            let read = self.read(&mut value);
            self.release(value);
            entries.push(self.node(SHARP_AST_ARRAY_ELEM, 0, self.line(element), &[read, key]));
        }

        self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &entries)
    }

    /// `\Sharp\Set::from(value)`, the `Set` of the elements of the array `value`.
    fn set_from(&mut self, line: u32, value: u32) -> u32 {
        let class = self.string(ZEND_NAME_FQ, line, b"Sharp\\Set");
        let method = self.string(0, line, b"from");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);

        self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, arguments])
    }

    /// `$p = \Sharp\Set::from($p);` for each `Set` parameter, since plain PHP and a `List` argument pass any array of
    /// the elements, and `$p = \array_map(\Sharp\Set::from(...), $p);` for a variadic one, which keeps each element's
    /// position or name. A promoted one is the property its constructor declares, `$this->p = \Sharp\Set::from($p);`.
    /// A nullable one stays null.
    fn set_parameters(&mut self, parameters: &FunctionLikeParameterList) -> Vec<u32> {
        let mut statements = Vec::new();
        for parameter in &parameters.parameters {
            let Some(nullable) = set_parameter(parameter) else {
                continue;
            };
            let line = self.line(parameter);
            let variable = &parameter.variable;
            let set = if parameter.is_variadic() {
                let to_set = if nullable {
                    let name = self.string(0, line, b"set");
                    let element = self.node(SHARP_AST_PARAM, 0, line, &[NULL, name, NULL, NULL, NULL, NULL]);
                    let elements = self.node(SHARP_AST_PARAM_LIST, 0, line, &[element]);
                    let set = self.set_of(variable.span, b"set", true);

                    self.declaration(
                        SHARP_AST_ARROW_FUNC,
                        0,
                        parameter,
                        parameter,
                        b"",
                        &[elements, NULL, set, NULL, NULL],
                    )
                } else {
                    let class = self.string(ZEND_NAME_FQ, line, b"Sharp\\Set");
                    let method = self.string(0, line, b"from");
                    let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

                    self.node(SHARP_AST_STATIC_CALL, 0, line, &[class, method, callable])
                };
                let function = self.string(ZEND_NAME_FQ, line, b"array_map");
                let values = self.variable(variable.span, variable.name);
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[to_set, values]);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            } else {
                self.set_of(variable.span, variable.name, nullable)
            };
            let place = self.parameter_place(parameter);
            statements.push(self.node(SHARP_AST_ASSIGN, 0, line, &[place, set]));
        }

        statements
    }

    /// `\Sharp\Set::from($name)`, or `$name === null ? null : \Sharp\Set::from($name)` when it is `nullable`.
    fn set_of(&mut self, span: Span, name: &[u8], nullable: bool) -> u32 {
        let line = self.line(span);
        let value = self.variable(span, name);
        let set = self.set_from(line, value);
        if !nullable {
            return set;
        }

        let value = self.variable(span, name);
        let is_null = self.is_null(line, value);
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_CONDITIONAL, 0, line, &[is_null, null, set])
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
    /// change reaches a property through the property, which runs its hooks, and a value that lives nowhere changes as
    /// a copy. A null-safe call tests its receiver first and gives null for null.
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
            ("List", b"contains" | b"indexof") => {
                let element_type = self.types.element_type(self.types.expression_type(receiver));

                self.element_search(call.span(), &mut collection, &element_type, element(), method == b"indexof")
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
            ("Set", b"contains") => {
                let set = self.read_collection(&mut collection, line, false);
                let key = self.key(element());
                let entry = self.node(SHARP_AST_DIM, 0, line, &[set, key]);

                self.node(SHARP_AST_ISSET, 0, line, &[entry])
            }
            ("Set", b"add") => {
                let mut element = self.reread(element(), "element");
                let held = self.read_collection(&mut collection, line, false);
                let key = self.set_key(&mut element);
                let entry = self.node(SHARP_AST_DIM, 0, line, &[held, key]);
                let held = self.node(SHARP_AST_ISSET, 0, line, &[entry]);
                let absent = self.node(SHARP_AST_UNARY_OP, ZEND_BOOL_NOT, line, &[held]);
                let place = self.read_collection(&mut collection, line, false);
                let key = self.set_key(&mut element);
                let value = self.read(&mut element);
                self.release(element);
                let entry = self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[value, key]);
                let entries = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[entry]);
                let added = self.node(SHARP_AST_ASSIGN_OP, ZEND_ADD, line, &[place, entries]);

                self.node(SHARP_AST_AND, 0, line, &[absent, added])
            }
            ("Set", b"remove") => {
                let mut element = self.reread(element(), "element");
                let held = self.read_collection(&mut collection, line, false);
                let key = self.set_key(&mut element);
                let entry = self.node(SHARP_AST_DIM, 0, line, &[held, key]);
                let held = self.node(SHARP_AST_ISSET, 0, line, &[entry]);
                let place = self.read_collection(&mut collection, line, false);
                let delete = self.string(0, line, b"delete");
                let key = self.set_key(&mut element);
                self.release(element);
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[key]);
                let deleted = self.node(SHARP_AST_METHOD_CALL, 0, line, &[place, delete, arguments]);
                let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
                let deleted = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[deleted, null]);

                self.node(SHARP_AST_AND, 0, line, &[held, deleted])
            }
            ("Set", b"clear") => {
                let place = self.read_collection(&mut collection, line, false);
                if matches!(collection, CollectionReceiver::Value(Reread { hidden: None, .. })) {
                    // The receiver lives nowhere, so clearing it changes nothing anyone reads.
                    place
                } else {
                    let empty = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &[]);

                    self.node(SHARP_AST_ASSIGN, 0, line, &[place, empty])
                }
            }
            (_, b"count") | ("Set", b"tolist") => {
                let function = if method == b"count" { b"count".as_slice() } else { b"array_values" };
                let array = self.read_collection(&mut collection, line, false);
                let function = self.string(ZEND_NAME_FQ, line, function);
                let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[array]);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            ("Set", b"filter") => {
                let set = self.read_collection(&mut collection, line, false);
                let filter = self.string(0, line, b"filterValues");
                let arguments = self.arguments(arguments, &[]);

                self.node(SHARP_AST_METHOD_CALL, 0, line, &[set, filter, arguments])
            }
            ("Map", b"filter") => self.read_collection(&mut collection, line, false),
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

    /// A `List`'s `contains(value)`, or its `indexOf(value)` when `index` holds: whether an element of `element_type`
    /// equals `value` by spec section 19's `==`, or the index of the first one that does, or null. Where `==` on them is
    /// `===` and the value runs nothing, so it may run before the receiver, it is PHP's strict search,
    /// `\in_array($value, $list, true)`, or `\array_search` with its false read as null through a hidden `$index#N`,
    /// since index 0 is falsy. Otherwise it runs [`Self::equality`] on each element:
    /// `\array_find_key($list, (fn ($value#N) => fn ($element#M) => $element#M == $value#N)($value))`, which reads
    /// the receiver before the value, as the call does, and the value once.
    fn element_search(
        &mut self,
        call: Span,
        collection: &mut CollectionReceiver,
        element_type: &TUnion,
        value: &Expression,
        index: bool,
    ) -> u32 {
        let line = self.line(call);
        let value_type = self.types.expression_type(value);
        let identical =
            self.declared_equality(element_type, value_type).is_none() && !mixes_numbers(element_type, value_type);
        // `\in_array` and `\array_search` read the value before the list, so they run only where neither side runs code.
        let receiver_runs_nothing = match collection {
            CollectionReceiver::Place(_) => true,
            CollectionReceiver::Property(object, _) | CollectionReceiver::Value(object) => {
                self.is_pure(object.expression)
            }
        };
        if identical && self.is_pure(value) && receiver_runs_nothing {
            let hidden = index.then(|| {
                self.temporaries += 1;

                format!("index#{}", self.temporaries).into_bytes()
            });
            let value = self.expression(value);
            let list = self.read_collection(collection, line, false);
            let strict = self.zval(line, sharp_value::SHARP_TRUE, |_| {});
            let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[value, list, strict]);
            let Some(hidden) = hidden else {
                let function = self.string(ZEND_NAME_FQ, line, b"in_array");

                return self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
            };
            let function = self.string(ZEND_NAME_FQ, line, b"array_search");
            let found = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
            let variable = self.variable(call, &hidden);
            let found = self.node(SHARP_AST_ASSIGN, 0, line, &[variable, found]);
            let missed = self.zval(line, sharp_value::SHARP_FALSE, |_| {});
            let missed = self.node(SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL, line, &[found, missed]);
            let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});
            let index = self.variable(call, &hidden);
            self.temporaries -= 1;

            return self.node(SHARP_AST_CONDITIONAL, ZEND_PARENTHESIZED_CONDITIONAL, line, &[missed, null, index]);
        }

        self.temporaries += 2;
        let value_name = format!("value#{}", self.temporaries - 1).into_bytes();
        let element_name = format!("element#{}", self.temporaries).into_bytes();
        let list = self.read_collection(collection, line, false);
        let element = Side::Hidden { span: call, name: element_name.clone(), r#type: element_type };
        let held = Side::Hidden { span: call, name: value_name.clone(), r#type: value_type };
        let equality = self.equality(line, element, held, false);
        let compare = self.hidden_arrow_function(call, &element_name, equality);
        let compare_with = self.hidden_arrow_function(call, &value_name, compare);
        let value = self.expression(value);
        let value = self.node(SHARP_AST_ARG_LIST, 0, line, &[value]);
        let predicate = self.node(SHARP_AST_CALL, 0, line, &[compare_with, value]);
        self.temporaries -= 2;

        let function = self.string(ZEND_NAME_FQ, line, b"array_find_key");
        let arguments = self.node(SHARP_AST_ARG_LIST, 0, line, &[list, predicate]);
        let found = self.node(SHARP_AST_CALL, 0, line, &[function, arguments]);
        if index {
            return found;
        }
        let null = self.zval(line, sharp_value::SHARP_NULL, |_| {});

        self.node(SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL, line, &[found, null])
    }

    /// `fn ($parameter) => body`, an arrow function of one untyped hidden parameter, on the line of `span`.
    fn hidden_arrow_function(&mut self, span: Span, parameter: &[u8], body: u32) -> u32 {
        let line = self.line(span);
        let name = self.string(0, line, parameter);
        let parameter = self.node(SHARP_AST_PARAM, 0, line, &[NULL, name, NULL, NULL, NULL, NULL]);
        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, line, &[parameter]);

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
        let reads_twice = matches!((collection, method.as_slice()), ("Set", b"add" | b"remove") | ("List", b"last"));

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
            _ => CollectionReceiver::Value(hold(self, receiver)),
        }
    }

    /// A read of the collection a method call runs on, which is also where the method writes it. The read that tests
    /// a null-safe call's receiver reads a property with `?->`.
    fn read_collection(&mut self, collection: &mut CollectionReceiver, line: u32, null_safe: bool) -> u32 {
        match collection {
            CollectionReceiver::Place(place) => self.target(place),
            CollectionReceiver::Property(object, property) => {
                let object = self.read(object);
                let property = self.member(property);
                let kind = if null_safe { SHARP_AST_NULLSAFE_PROP } else { SHARP_AST_PROP };

                self.node(kind, 0, line, &[object, property])
            }
            CollectionReceiver::Value(value) => self.read(value),
        }
    }

    fn release_collection(&mut self, collection: CollectionReceiver) {
        match collection {
            CollectionReceiver::Place(_) => {}
            CollectionReceiver::Property(object, _) => self.release(object),
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
        let Some(name) = value.hidden.clone() else {
            return self.expression(value.expression);
        };
        let variable = self.variable(value.expression.span(), &name);
        if value.assigned {
            return variable;
        }

        value.assigned = true;
        let lowered = self.expression(value.expression);

        self.node(SHARP_AST_ASSIGN, 0, self.line(value.expression), &[variable, lowered])
    }

    /// A read of the element `value` as the key a `Set` holds it under.
    fn set_key(&mut self, value: &mut Reread) -> u32 {
        let read = self.read(value);

        self.stored_key(value.expression, read)
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
    /// four instructions. Any other value is itself.
    fn template_value(&mut self, expression: &Expression) -> u32 {
        let value = self.expression(expression);
        if !self.types.expression_type(expression).is_bool() {
            return value;
        }

        let line = self.line(expression);
        let texts = [b"false".as_slice(), b"true"].map(|text| {
            let text = self.string(0, line, text);

            self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[text, NULL])
        });
        let texts = self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &texts);

        self.node(SHARP_AST_DIM, 0, line, &[texts, value])
    }

    /// What an assignment, a compound assignment, `++` or `--` writes: a local or parameter, `object.name`,
    /// `Class.name`, which php-src's grammar builds as the static property `Class::$name`, or an index of one of them,
    /// as php-src's `variable` rule takes them.
    fn target(&mut self, target: &Expression) -> u32 {
        match target {
            Expression::ConstantAccess(name)
                if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::Field)) =>
            {
                self.name(name)
            }
            // The hidden variable of an `is`, `as` or `match`, which `php_shape` writes.
            Expression::Variable(Variable::Direct(_)) => self.expression(target),
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let line = self.line(target);
                    let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));
                    let property = self.member(&access.property);

                    self.node(SHARP_AST_STATIC_PROP, 0, line, &[class, property])
                }
                None => self.expression(target),
            },
            Expression::ArrayAccess(access) => {
                let value = self.target(access.array);
                let key = self.key(access.index);

                self.node(SHARP_AST_DIM, 0, self.line(target), &[value, key])
            }
            _ => unreachable!("check_slice refuses writing to `{target}`"),
        }
    }

    /// An `is`, `as` or `match` is the PHP it runs as, which the analyzer analyzes too. Its hidden variables are
    /// numbered after those of the forms around it, which may still hold their values.
    fn pattern(&mut self, node: Node<'_, '_>) -> u32 {
        let arena = LocalArena::new();
        let names = self.names;
        let is_local = |span: Span| matches!(names.binding(&span), Some(Binding::Local(_)));
        let Some(PhpShape { php, temporaries, .. }) = php_shape(&arena, node, self.temporaries, &is_local) else {
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

    /// A local, a parameter or `this` is a PHP variable of the same name, and `field` is the storage. Any other bare
    /// name outside a call is a constant, which the engine looks up in the namespace, then globally, as PHP does for
    /// an unqualified name.
    fn name(&mut self, name: &ConstantAccess) -> u32 {
        let line = self.line(name);

        match self.names.binding(&name.name) {
            Some(Binding::Local(_) | Binding::This) => self.variable(name.span(), name.name.value()),
            Some(Binding::Field) => self.storage(line),
            Some(Binding::Constant) => {
                let constant = self.string(ZEND_NAME_NOT_FQ, line, name.name.value());

                self.node(SHARP_AST_CONST, 0, line, &[constant])
            }
            binding => unreachable!("check_slice refuses the name `{}` bound as {binding:?}", name.name),
        }
    }

    fn variable(&mut self, span: Span, name: &[u8]) -> u32 {
        let line = self.line(span);
        let name = self.string(0, line, name);

        self.node(SHARP_AST_VAR, 0, line, &[name])
    }

    /// `Class.m()` is a static call on the class's full name, `super.m()` one on `parent`, and `Self.m()` one on
    /// `static`, except the standard library's `Position.current()`, which is the position it is written at. Any other
    /// `object.m()` is an instance call, or a call of the function in the property `m` when the checker found that
    /// property, as spec section 14 calls one. A call of a standard library method with an inline form runs that form.
    fn method_call(&mut self, expression: &Expression, call: &MethodCall) -> u32 {
        if let Some(class) = self.names.static_call_class(call)
            && is_current_position(self.names.get(&class.name), call)
        {
            return self.current_position(class, call);
        }
        if let Some(lowered) = self.collection_call(expression, call.object, &call.argument_list, false) {
            return lowered;
        }
        if let Some(inlined) = self.inlined_call(expression, call) {
            return inlined;
        }

        let line = self.line(call);
        let (kind, object) = match (self.names.static_call_class(call), call.object) {
            (Some(class), _) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));

                (SHARP_AST_STATIC_CALL, class)
            }
            (None, Expression::Parent(keyword)) => {
                (SHARP_AST_STATIC_CALL, self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"parent"))
            }
            (None, Expression::Self_(keyword)) => {
                (SHARP_AST_STATIC_CALL, self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"static"))
            }
            (None, object) => match self.class_value(object) {
                Some((class, _)) => (SHARP_AST_STATIC_CALL, class),
                None => (SHARP_AST_METHOD_CALL, self.expression(object)),
            },
        };
        let method = self.member(&call.method);
        let keys = if kind == SHARP_AST_METHOD_CALL { self.types.key_arguments(expression) } else { Vec::new() };
        let arguments = self.arguments(&call.argument_list, &keys);

        if kind == SHARP_AST_METHOD_CALL && self.is_property_call(expression, call.object) {
            let property = self.node(SHARP_AST_PROP, 0, line, &[object, method]);

            return self.node(SHARP_AST_CALL, 0, line, &[property, arguments]);
        }

        self.node(kind, 0, line, &[object, method, arguments])
    }

    /// Spec section 27: `Position.current()` in a body is the position where it is written, a new `Position` of the
    /// file, the line and byte column of `Position`, and the function, as
    /// `new \Sharp\Position(__FILE__, 9, 22, 'App.Tenant.Report.run')`. The engine compiles the file under its source's
    /// absolute path, which `__FILE__` reads where the file runs, so the compiled file holds no path.
    fn current_position(&mut self, class: &ConstantAccess, call: &MethodCall) -> u32 {
        let (line, column) = self.lines.line_and_column(class.span().start.offset);
        let name = self.string(ZEND_NAME_FQ, line, b"Sharp\\Position");
        let file = self.node(SHARP_AST_MAGIC_CONST, SHARP_T_FILE, line, &[]);
        let line_number = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = i64::from(line));
        let column = self.zval(line, sharp_value::SHARP_LONG, |node| node.long_value = i64::from(column));
        let function = self.function.clone();
        let function = self.string(0, line, &function);
        let arguments =
            self.node(SHARP_AST_ARG_LIST, 0, self.line(&call.argument_list), &[file, line_number, column, function]);

        self.node(SHARP_AST_NEW, 0, self.line(call), &[name, arguments])
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

    /// A call's arguments, the one at each position in `keys` going in as a key, as `Map.get`'s does.
    fn arguments(&mut self, list: &ArgumentList, keys: &[usize]) -> u32 {
        let mut arguments = Vec::new();
        for (position, argument) in list.arguments.iter().enumerate() {
            let is_key = keys.contains(&position);
            arguments.push(match argument {
                Argument::Positional(positional) if is_key => self.key(positional.value),
                Argument::Positional(positional) => self.positional_argument(positional),
                Argument::Named(named) => self.named_argument(named, is_key),
            });
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    /// An attribute's arguments, which the parser reads as a partial argument list without placeholders.
    fn attribute_arguments(&mut self, list: &PartialArgumentList) -> u32 {
        let mut arguments = Vec::new();
        for argument in &list.arguments {
            arguments.push(match argument {
                PartialArgument::Positional(positional) => self.positional_argument(positional),
                PartialArgument::Named(named) => self.named_argument(named, false),
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

    fn named_argument(&mut self, argument: &NamedArgument, is_key: bool) -> u32 {
        let line = self.line(argument.name.span);
        let name = self.string(0, line, argument.name.value);
        let value = if is_key { self.key(argument.value) } else { self.expression(argument.value) };

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

/// Whether `call`, a static call on `class`, is the standard library's `Position.current()`. PHP compares class and
/// method names ignoring case.
fn is_current_position(class: &[u8], call: &MethodCall) -> bool {
    class.eq_ignore_ascii_case(b"Sharp\\Position")
        && matches!(call.method, ClassLikeMemberSelector::Identifier(method) if method.value.eq_ignore_ascii_case(b"current"))
        && call.argument_list.arguments.is_empty()
}

/// The flags of a member's modifiers. Every modifier is named, so a new one does not compile until it is decided.
fn modifier_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) => ZEND_ACC_PUBLIC,
            Modifier::Protected(_) => ZEND_ACC_PROTECTED,
            Modifier::Private(_) => ZEND_ACC_PRIVATE,
            Modifier::Static(_) => ZEND_ACC_STATIC,
            Modifier::Abstract(_) => ZEND_ACC_ABSTRACT,
            // PHP methods are open to overriding, and `method` lowers `override` to `#[\Override]`. PHP has no
            // `required` constructor, and the checker proves every subclass keeps one `new Self(…)` can call. `method`
            // gives an `extern` method the body that calls its native function.
            Modifier::Virtual(_) | Modifier::Override(_) | Modifier::Required(_) | Modifier::Extern(_) => 0,
            Modifier::Final(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_)
            | Modifier::Lazy(_) => unreachable!("check_slice refuses the modifier `{modifier}`"),
        };
    }

    flags
}

/// The flags of a class's modifiers. `public` adds none, because every PHP class is public. A static class, whose
/// members are all static, runs as a final PHP class.
fn class_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) => 0,
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

/// Whether `parameter` is a `Set`, as `Some` of whether it is nullable.
fn set_parameter(parameter: &FunctionLikeParameter) -> Option<bool> {
    match &parameter.hint {
        Some(Hint::Generic(generic)) if generic.name.value == b"Set" => Some(false),
        Some(Hint::Nullable(NullableHint { hint: Hint::Generic(generic), .. })) if generic.name.value == b"Set" => {
            Some(true)
        }
        _ => None,
    }
}

/// Whether a member without an initial value starts as null, as in C# and Swift: a field, or a property with storage
/// and `set`, of a nullable type. A get-only property with storage is set only where `readonly` allows, and
/// `readonly` takes no default, so the checker refuses one of a nullable type without an initial value. A property
/// without storage is virtual, and PHP refuses a default on a virtual property.
fn is_null_by_default(property: &Property, names: &ResolvedNames) -> bool {
    matches!(property.hint(), Some(Hint::Nullable(_)))
        && match property {
            Property::Plain(_) => true,
            Property::Hooked(hooked) => !hooked.hook_list.is_get_only() && names.has_storage(&hooked.hook_list),
            Property::Computed(_) => false,
        }
}

/// The flags a property's accessors add: `readonly` for a get-only auto-property, which spec section 6.1 sets in the
/// constructor, or the set visibility php-src writes `private(set)` or `protected(set)` from the `set` accessor's
/// access modifier. A get-only property whose body uses `field` takes `protected(set)`, the set visibility of
/// `readonly`, and one without storage takes none, because PHP refuses a set visibility on a read-only virtual
/// property. PHP drops a set visibility equal to the property's own, so a private property, or a protected one with
/// `protected(set)`, takes none, and the flags carry only what the engine keeps.
fn accessor_flags(modifiers: &Sequence<Modifier>, accessors: &PropertyHookList, names: &ResolvedNames) -> u32 {
    if accessors.is_readonly() {
        return ZEND_ACC_READONLY;
    }

    let flags = match accessors.hooks.iter().find(|accessor| accessor.name.value == b"set") {
        None if names.uses_field(accessors) => ZEND_ACC_PROTECTED_SET,
        None => 0,
        Some(set) => match set.modifiers.first() {
            None => 0,
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

/// Whether `==` compares a value of `lhs` with one of `rhs` as an int and a float: both are numbers, `null` aside, and
/// one may hold an int where one may hold a float. The checker refuses every other pair of two different number kinds.
fn mixes_numbers(lhs: &TUnion, rhs: &TUnion) -> bool {
    let [Some([lhs_int, lhs_float]), Some([rhs_int, rhs_float])] = [lhs, rhs].map(number_kinds) else {
        return false;
    };

    (lhs_int || rhs_int) && (lhs_float || rhs_float)
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
        UnaryPrefixOperator::ErrorControl(_) => (SHARP_AST_SILENCE, 0),
        UnaryPrefixOperator::Reference(_)
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
        let checked = CheckedProgram::unchecked(
            &file,
            program,
            NameResolver::new(&arena).resolve(program),
            &artifacts,
            &codebase,
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
