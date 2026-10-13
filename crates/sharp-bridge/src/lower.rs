use std::borrow::Cow;
use std::collections::HashSet;

use mago_allocator::LocalArena;
use mago_database::file::File;
use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_names::binding::php_method_name;
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
use mago_syntax::cst::AssignmentOperator;
use mago_syntax::cst::AttributeList;
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
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Enum;
use mago_syntax::cst::EnumCase;
use mago_syntax::cst::EnumCaseItem;
use mago_syntax::cst::Expression;
use mago_syntax::cst::For;
use mago_syntax::cst::ForBody;
use mago_syntax::cst::ForOfTarget;
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
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::ModifierSequenceExt;
use mago_syntax::cst::NamedArgument;
use mago_syntax::cst::NamespaceBody;
use mago_syntax::cst::Node;
use mago_syntax::cst::PartialArgument;
use mago_syntax::cst::PartialArgumentList;
use mago_syntax::cst::PositionalArgument;
use mago_syntax::cst::Property;
use mago_syntax::cst::PropertyHookConcreteExpressionBody;
use mago_syntax::cst::PropertyHookList;
use mago_syntax::cst::Sequence;
use mago_syntax::cst::Statement;
use mago_syntax::cst::StringPart;
use mago_syntax::cst::Try;
use mago_syntax::cst::TryCatchClause;
use mago_syntax::cst::UnaryPostfixOperator;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_syntax::cst::WhileBody;
use mago_syntax::dialect::Dialect;
use mago_syntax::parser::parse_file_with_dialect;
use mago_syntax::settings::ParserSettings;
use mago_syntax::utils::interface::DEFAULTS_TRAIT_SHORT_NAME;
use mago_syntax_core::stack::ensure_sufficient_stack;
use mago_syntax_core::utils::parse_literal_integer_as_float;

use crate::Diagnostic;
use crate::Unit;
use crate::lower::checked::CheckError;
use crate::lower::checked::CheckedProgram;
use crate::lower::checked::check;
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
use crate::sharp_kind::SHARP_AST_STATIC_CALL;
use crate::sharp_kind::SHARP_AST_STATIC_PROP;
use crate::sharp_kind::SHARP_AST_STMT_LIST;
use crate::sharp_kind::SHARP_AST_THROW;
use crate::sharp_kind::SHARP_AST_TRY;
use crate::sharp_kind::SHARP_AST_TYPE;
use crate::sharp_kind::SHARP_AST_UNARY_MINUS;
use crate::sharp_kind::SHARP_AST_UNARY_OP;
use crate::sharp_kind::SHARP_AST_UNARY_PLUS;
use crate::sharp_kind::SHARP_AST_UNSET;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_kind::SHARP_AST_WHILE;
use crate::sharp_kind::SHARP_AST_ZVAL;
use crate::sharp_node;
use crate::sharp_severity;
use crate::sharp_str;
use crate::sharp_value;
use crate::store_text;

mod checked;

/// The values php-src gives the attrs the lowering emits, from `zend_compile.h` and `zend_vm_opcodes.h`.
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
const ZEND_ACC_ENUM: u32 = 1 << 28;
const ZEND_TYPE_NULLABLE: u32 = 1 << 8;
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
const ZEND_POW: u32 = 12;
const ZEND_BOOL_NOT: u32 = 14;
const ZEND_IS_IDENTICAL: u32 = 16;
const ZEND_IS_NOT_IDENTICAL: u32 = 17;
const ZEND_IS_EQUAL: u32 = 18;
const ZEND_IS_NOT_EQUAL: u32 = 19;
const ZEND_IS_SMALLER: u32 = 20;
const ZEND_IS_SMALLER_OR_EQUAL: u32 = 21;
/// php-sharp's own attr from `zend_compile.h`: a class constant fetch that falls back to the static property. It sits
/// above the fetch flags a constant expression passes in the same attr.
const ZEND_FETCH_CLASS_MEMBER_SYNTAX: u32 = 1 << 15;
/// php-sharp's own class flag from `zend_compile.h`: the class's parent, if any, is in its interface list.
const ZEND_ACC_PARENT_IN_INTERFACES: u32 = 1 << 31;

/// A null child.
const NULL: u32 = u32::MAX;

/// Runs the PHP# file at `path` through the parser, the binder and the semantic checks, and lowers it into the tree
/// php-src builds for the equivalent PHP. Any error returns diagnostics and no nodes. The file is PHP# whatever its
/// name, because `ext/sharp` decided that before calling.
pub(crate) fn lower(path: Vec<u8>, source: Vec<u8>) -> Box<Unit> {
    let file = File::ephemeral(Cow::Owned(path), Cow::Owned(source));
    let lines = Lines::new(&file.contents);
    let arena = LocalArena::new();
    let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
    let checked = match check(&arena, &file, program) {
        Ok(checked) => checked,
        Err(CheckError::Parse(errors)) => {
            return Unit::failed(
                errors
                    .iter()
                    .map(|error| {
                        lines.diagnostic(Some(error.span()), sharp_severity::SHARP_PARSE_ERROR, error.to_string())
                    })
                    .collect(),
            );
        }
        Err(CheckError::Compile(errors)) => {
            return Unit::failed(
                errors
                    .into_iter()
                    .map(|issue| {
                        lines.diagnostic(issue.primary_span(), sharp_severity::SHARP_COMPILE_ERROR, issue.message)
                    })
                    .collect(),
            );
        }
    };

    Lowering::new(&lines, checked.names()).program(&checked)
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

    /// A diagnostic at the start of `span`, with a 1-based line and byte column. Without a span it is at line 0,
    /// column 0, which the ABI defines as no position.
    fn diagnostic(&self, span: Option<Span>, severity: sharp_severity, message: String) -> Diagnostic {
        let (line, column) = span.map_or((0, 0), |span| {
            let line = self.line(span.start.offset);

            (line, span.start.offset - self.0[line as usize - 1] + 1)
        });

        Diagnostic { line, column, severity, message }
    }
}

/// Lowers one checked file. Every node is pushed after its children, and each node's children are contiguous.
struct Lowering<'lowering, 'arena> {
    lines: &'lowering Lines,
    names: &'lowering ResolvedNames<'arena>,
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: LocalArena,
    /// The declaration offsets of the locals a lambda captures by reference: those code writes.
    by_reference: HashSet<u32>,
    /// How many loop bodies hold the statement being lowered, inside the innermost method or lambda.
    loop_depth: u32,
    /// Whether the lowering is inside a constant expression, which PHP evaluates without opcodes.
    in_constant_expression: bool,
}

impl<'lowering, 'arena> Lowering<'lowering, 'arena> {
    fn new(lines: &'lowering Lines, names: &'lowering ResolvedNames<'arena>) -> Self {
        Self {
            lines,
            names,
            nodes: Vec::new(),
            children: Vec::new(),
            texts: LocalArena::new(),
            by_reference: HashSet::default(),
            loop_depth: 0,
            in_constant_expression: false,
        }
    }

    /// Lowers a constant expression: a constant's value, a default or an attribute's arguments.
    fn constant_expression(&mut self, lower: impl FnOnce(&mut Self) -> u32) -> u32 {
        self.in_constant_expression = true;
        let index = lower(self);
        self.in_constant_expression = false;

        index
    }

    /// `declare(strict_types=1);` first, then the namespaces and classes. Imports are not lowered: every class name
    /// in the tree is fully qualified.
    fn program(mut self, checked: &CheckedProgram) -> Box<Unit> {
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

        Unit::boxed(self.nodes, self.children, root, Vec::new(), self.texts)
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
            Statement::Use(_) => {}
            Statement::Class(class) => statements.push(self.class(class)),
            Statement::Interface(interface) => {
                statements.push(self.interface(interface));
                statements.extend(self.defaults(interface));
            }
            Statement::Enum(r#enum) => statements.push(self.r#enum(r#enum)),
            _ => unreachable!("check_slice refuses the file statement `{statement}`"),
        }
    }

    /// A class sets the initial values PHP takes as no default, those that are not constant or belong to a `readonly`
    /// property, at the start of its constructor, in declaration order. A class without a constructor gets a public
    /// one that spans the class.
    fn class(&mut self, class: &Class) -> u32 {
        let mut initial_values = Vec::new();
        for member in &class.members {
            if let ClassLikeMember::Property(property) = member
                && let Some(value) = property.initial_value()
                && !is_default(property, value)
            {
                initial_values.push(self.initial_value(property.first_variable(), value));
            }
        }

        let mut members = Vec::new();
        let mut has_constructor = false;
        for member in &class.members {
            members.push(match member {
                ClassLikeMember::Method(method) if php_method_name(method) == b"__construct" => {
                    has_constructor = true;

                    self.method(method, modifier_flags(&method.modifiers), &initial_values)
                }
                ClassLikeMember::Method(method) => self.method(method, modifier_flags(&method.modifiers), &[]),
                ClassLikeMember::Property(property) => self.property(property),
                ClassLikeMember::Constant(constant) => self.constant(constant),
                _ => unreachable!("check_slice refuses the class member `{member}`"),
            });
        }

        if !has_constructor && !initial_values.is_empty() {
            let line = self.line(class.left_brace);
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
        let (header, parent_in_interfaces) = match &class.inheritance {
            Some(inheritance) => (self.name_list(inheritance), ZEND_ACC_PARENT_IN_INTERFACES),
            None => (NULL, 0),
        };

        self.declaration(
            SHARP_AST_CLASS,
            class_flags(&class.modifiers) | parent_in_interfaces,
            class.class.span,
            class.right_brace,
            class.name.value,
            &[NULL, header, members, attributes, NULL],
        )
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

    /// An interface is a class declaration with `ZEND_ACC_INTERFACE` whose members are all abstract and `public`, as
    /// php-src's grammar builds `interface Measured { public function area(): float; public int $sides { get; } }`.
    /// Its default bodies are in the trait `defaults` lowers.
    fn interface(&mut self, interface: &Interface) -> u32 {
        let mut members = Vec::new();
        for member in &interface.members {
            members.push(match member {
                ClassLikeMember::Method(method) => self.signature(method),
                ClassLikeMember::Property(property) => self.abstract_property(property),
                _ => unreachable!("check_slice refuses the interface member `{member}`"),
            });
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

    /// The trait that holds an interface's default bodies, as php-src's grammar builds `trait Defaults { … }`, on the
    /// interface's lines. Its name is `HasDesign\Defaults` for the interface `HasDesign`, which the engine declares in
    /// the file's namespace, so every name in a body resolves as it does in the interface. Each default method is a
    /// `public` method with its body, and each computed property a `public` property with its `get` hook, in source
    /// order. An interface without a default body has no trait.
    fn defaults(&mut self, interface: &Interface) -> Option<u32> {
        let mut members = Vec::new();
        for member in &interface.members {
            match member {
                ClassLikeMember::Method(method) if !matches!(method.body, MethodBody::Abstract(_)) => {
                    members.push(self.method(method, ZEND_ACC_PUBLIC, &[]));
                }
                ClassLikeMember::Property(property @ Property::Computed(computed)) => {
                    let hooks = self.get_hook(&computed.body);
                    members.push(self.property_group(property, ZEND_ACC_PUBLIC, hooks));
                }
                _ => {}
            }
        }

        if members.is_empty() {
            return None;
        }

        let members = self.node(SHARP_AST_STMT_LIST, 0, self.line(interface.left_brace), &members);
        let name = [interface.name.value, b"\\", DEFAULTS_TRAIT_SHORT_NAME.as_bytes()].concat();

        Some(self.declaration(
            SHARP_AST_CLASS,
            ZEND_ACC_TRAIT,
            interface.interface.span,
            interface.right_brace,
            &name,
            &[NULL, NULL, members, NULL, NULL],
        ))
    }

    /// An enum is a final class with the enum flag, its header as its interface list and its backing type as its last
    /// child, as php-src's grammar builds `enum Status: string implements HasLabel`. An enum has no parent, so its
    /// header needs no mark.
    fn r#enum(&mut self, r#enum: &Enum) -> u32 {
        let mut members = Vec::new();
        for member in &r#enum.members {
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
            EnumCaseItem::Backed(item) => self.constant_expression(|lowering| lowering.expression(item.value)),
        };
        let attributes = self.attributes(&case.attribute_lists, None);

        self.node(SHARP_AST_ENUM_CASE, 0, line, &[name, value, NULL, attributes])
    }

    /// A method is a `function` with its return type after its parameters, and with `flags`. Its first line is where
    /// PHP writes `function`: the return type, or the name of the constructor, which runs as `__construct` and has
    /// no return type. The constructor's body starts with the class's initial values that are not constant, and an
    /// abstract method has no statement list.
    fn method(&mut self, method: &Method, flags: u32, initial_values: &[u32]) -> u32 {
        if flags & (ZEND_ACC_PUBLIC | ZEND_ACC_PROTECTED | ZEND_ACC_PRIVATE) == 0 {
            unreachable!("check_slice refuses a method without an access modifier");
        }

        self.method_declaration(method, flags, |lowering| {
            let mut statements = initial_values.to_vec();
            match &method.body {
                MethodBody::Concrete(block) => {
                    for statement in &block.statements {
                        statements.push(lowering.statement(statement));
                    }

                    lowering.node(SHARP_AST_STMT_LIST, 0, lowering.line(block), &statements)
                }
                MethodBody::Expression(body) => {
                    let line = lowering.line(body);
                    let expression = lowering.expression(body.expression);
                    statements.push(if method.returns_value() {
                        lowering.node(SHARP_AST_RETURN, 0, line, &[expression])
                    } else {
                        expression
                    });

                    lowering.node(SHARP_AST_STMT_LIST, 0, line, &statements)
                }
                MethodBody::Abstract(_) => NULL,
            }
        })
    }

    /// An interface's method is `public` with no statement list, as php-src's grammar builds
    /// `public function designKey(): string;`. Its default body, if any, is in the trait `defaults` lowers.
    fn signature(&mut self, method: &Method) -> u32 {
        self.method_declaration(method, ZEND_ACC_PUBLIC, |_| NULL)
    }

    /// A method's declaration with `flags`: its parameters, the statement list `body` lowers after them, its return
    /// type and its attributes.
    fn method_declaration(&mut self, method: &Method, flags: u32, body: impl FnOnce(&mut Self) -> u32) -> u32 {
        let mut parameters = Vec::new();
        for parameter in &method.parameter_list.parameters {
            parameters.push(self.parameter(parameter));
        }

        let parameters = self.node(SHARP_AST_PARAM_LIST, 0, self.line(&method.parameter_list), &parameters);
        let body = body(self);
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

    fn parameter(&mut self, parameter: &FunctionLikeParameter) -> u32 {
        let Some(hint) = &parameter.hint else {
            unreachable!("semantics refuses a method parameter without a type");
        };
        let hint = self.hint(hint);

        self.parameter_of_type(parameter, hint)
    }

    /// A parameter with its lowered type, or with a null type, as a lambda's parameter can be.
    fn parameter_of_type(&mut self, parameter: &FunctionLikeParameter, hint: u32) -> u32 {
        let name = self.string(0, self.line(parameter.variable.span), parameter.variable.name);
        let default = parameter
            .default_value
            .as_ref()
            .map_or(NULL, |default| self.constant_expression(|lowering| lowering.expression(default.value)));
        let accessor_flags =
            parameter.hooks.as_ref().map_or(0, |accessors| accessor_flags(&parameter.modifiers, accessors));
        let flags = modifier_flags(&parameter.modifiers) | accessor_flags;
        let attributes = self.attributes(&parameter.attribute_lists, None);

        self.node(SHARP_AST_PARAM, flags, self.line(parameter), &[hint, name, default, attributes, NULL, NULL])
    }

    /// A field, an auto-property or a computed property is a property group of one property, as php-src's grammar
    /// builds `private int $count = 0;`, `public private(set) int $views = 0;`, `public readonly int $id;` and
    /// `public string $slug { get => expr; }`. A constant initial value is its default, unless the property is
    /// `readonly`.
    fn property(&mut self, property: &Property) -> u32 {
        let (accessor_flags, hooks) = match property {
            Property::Plain(_) => (0, NULL),
            Property::Hooked(auto_property) => {
                (accessor_flags(&auto_property.modifiers, &auto_property.hook_list), NULL)
            }
            Property::Computed(computed) => (0, self.get_hook(&computed.body)),
        };

        self.property_group(property, modifier_flags(property.modifiers()) | accessor_flags, hooks)
    }

    /// An interface's property is `public`, and each accessor is a hook without a body, as php-src's grammar builds
    /// `public string $designColumn { get; }`. A computed property is `{ get; }` here, and its default body is in the
    /// trait `defaults` lowers.
    fn abstract_property(&mut self, property: &Property) -> u32 {
        let mut hooks = Vec::new();
        let line = match property {
            Property::Hooked(auto_property) => {
                for accessor in &auto_property.hook_list.hooks {
                    hooks.push(self.abstract_hook(accessor.name.value, accessor.body.span()));
                }

                self.line(auto_property.hook_list.left_brace)
            }
            Property::Computed(computed) => {
                hooks.push(self.abstract_hook(b"get", computed.body.arrow));

                self.line(computed.body.arrow)
            }
            Property::Plain(_) => unreachable!("check_slice refuses a field in an interface"),
        };
        let hooks = self.node(SHARP_AST_STMT_LIST, 0, line, &hooks);

        self.property_group(property, ZEND_ACC_PUBLIC, hooks)
    }

    /// A hook without a body, as php-src's grammar builds `get;`, on the line of `at`, the token after its name.
    fn abstract_hook(&mut self, name: &[u8], at: Span) -> u32 {
        self.declaration(SHARP_AST_PROPERTY_HOOK, 0, at, at, name, &[NULL, NULL, NULL, NULL, NULL])
    }

    /// A property group of one property with `flags` and the hook list `hooks`, or null.
    fn property_group(&mut self, property: &Property, flags: u32, hooks: u32) -> u32 {
        let attribute_lists = match property {
            Property::Plain(field) => &field.attribute_lists,
            Property::Hooked(auto_property) => &auto_property.attribute_lists,
            Property::Computed(computed) => &computed.attribute_lists,
        };
        let Some(hint) = property.hint() else {
            unreachable!("the PHP# parser gives every field and property its type");
        };
        let hint = self.hint(hint);
        let variable = property.first_variable();
        let line = self.line(variable);
        let name = self.string(0, line, variable.name);
        let default = match property.initial_value() {
            Some(value) if is_default(property, value) => {
                self.constant_expression(|lowering| lowering.expression(value))
            }
            _ => NULL,
        };
        let element = self.node(SHARP_AST_PROP_ELEM, 0, line, &[name, default, NULL, hooks]);
        let declaration = self.node(SHARP_AST_PROP_DECL, 0, line, &[element]);
        let attributes = self.attributes(attribute_lists, None);

        self.node(SHARP_AST_PROP_GROUP, flags, self.line(property), &[hint, declaration, attributes])
    }

    /// A computed property's `=> expr;` is a hook list of one `get` hook whose body is the short body php-src's
    /// grammar builds for `get => expr;`, on the arrow's lines.
    fn get_hook(&mut self, body: &PropertyHookConcreteExpressionBody) -> u32 {
        let line = self.line(body.arrow);
        let expression = self.expression(body.expression);
        let short_body = self.node(SHARP_AST_PROPERTY_HOOK_SHORT_BODY, 0, line, &[expression]);
        let hook = self.declaration(
            SHARP_AST_PROPERTY_HOOK,
            0,
            body.arrow,
            body,
            b"get",
            &[NULL, NULL, short_body, NULL, NULL],
        );

        self.node(SHARP_AST_STMT_LIST, 0, line, &[hook])
    }

    /// A declaration's attributes are one `ATTRIBUTE_LIST` with an `ATTRIBUTE_GROUP` per `[...]`, as php-src's grammar
    /// builds `#[...]`, or null without attributes. Each attribute names its class by its full name. A method's
    /// `override` adds a last group of `#[\Override]`.
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
                let arguments = attribute
                    .argument_list
                    .as_ref()
                    .map_or(NULL, |list| self.constant_expression(|lowering| lowering.attribute_arguments(list)));

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
        let value = self.constant_expression(|lowering| lowering.expression(item.value));
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

    /// A built-in type is written unqualified, and a class by its full name. A `List` or `Map` is a PHP array, so its
    /// type is `array`, as php-src's grammar builds it. A function type runs as PHP's `\Closure`. A nullable type is
    /// its type with `ZEND_TYPE_NULLABLE`, as php-src's grammar builds `?int`.
    fn hint(&mut self, hint: &Hint) -> u32 {
        match hint {
            Hint::Integer(name) | Hint::Float(name) | Hint::Bool(name) | Hint::String(name) | Hint::Void(name) => {
                self.string(ZEND_NAME_NOT_FQ, self.line(name.span), name.value)
            }
            Hint::Identifier(class) => self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class)),
            Hint::Generic(generic) => self.node(SHARP_AST_TYPE, IS_ARRAY, self.line(generic), &[]),
            Hint::Function(function) => self.string(ZEND_NAME_FQ, self.line(function), b"Closure"),
            Hint::Nullable(nullable) => {
                let index = self.hint(nullable.hint);
                self.nodes[index as usize].attr |= ZEND_TYPE_NULLABLE;

                index
            }
            _ => unreachable!("check_slice refuses the type `{hint}`"),
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
                    ForOfTarget::Value(value) => (NULL, self.variable(value.span, value.value)),
                    ForOfTarget::KeyValue(pair) => {
                        (self.variable(pair.key.span, pair.key.value), self.variable(pair.value.span, pair.value.value))
                    }
                };
                let body = self.loop_body(for_of.body);

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

    fn catch(&mut self, clause: &TryCatchClause) -> u32 {
        let mut classes = Vec::new();
        self.catch_classes(&clause.hint, &mut classes);
        let line = self.nodes[classes[0] as usize].line;
        let classes = self.node(SHARP_AST_NAME_LIST, 0, line, &classes);
        let variable =
            clause.variable.as_ref().map_or(NULL, |variable| self.string(0, self.line(variable.span), variable.name));
        let block = self.block(&clause.block);

        self.node(SHARP_AST_CATCH, 0, line, &[classes, variable, block])
    }

    /// The classes a catch clause names, each by its full name, in the order they are written.
    fn catch_classes(&mut self, hint: &Hint, classes: &mut Vec<u32>) {
        match hint {
            Hint::Identifier(class) => classes.push(self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class))),
            Hint::Union(union) => {
                self.catch_classes(union.left, classes);
                self.catch_classes(union.right, classes);
            }
            _ => unreachable!("semantics refuses the catch type `{hint}`"),
        }
    }

    fn expression(&mut self, expression: &Expression) -> u32 {
        let line = self.line(expression);

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
            Expression::Binary(binary) => {
                let (kind, attr) = binary_kind(binary.operator);
                let lhs = self.expression(binary.lhs);
                let rhs = self.expression(binary.rhs);

                self.node(kind, attr, line, &[lhs, rhs])
            }
            Expression::UnaryPrefix(unary) => {
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
                let (kind, attr) = assignment_kind(&assignment.operator);
                let lhs = self.target(assignment.lhs);
                let rhs = self.expression(assignment.rhs);

                self.node(kind, attr, line, &[lhs, rhs])
            }
            Expression::Call(Call::Method(call)) => self.method_call(call),
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
                let arguments = self.arguments(argument_list);

                self.node(SHARP_AST_CALL, 0, line, &[function, arguments])
            }
            Expression::Instantiation(Instantiation {
                class: Expression::Identifier(class),
                argument_list: Some(arguments),
                ..
            }) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(class));
                let arguments = self.arguments(arguments);

                self.node(SHARP_AST_NEW, 0, line, &[class, arguments])
            }
            // Spec section 4 looks up the kind of `Class.y` when it runs, so the read is a class constant fetch
            // marked to fall back to the static property of the same name. A constant expression reads only
            // constants and enum cases, as PHP's does, so its fetch is unmarked.
            Expression::Access(Access::Property(access)) => match self.names.static_property_class(access) {
                Some(class) => {
                    let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));
                    let member = self.member(&access.property);
                    let attr = if self.in_constant_expression { 0 } else { ZEND_FETCH_CLASS_MEMBER_SYNTAX };

                    self.node(SHARP_AST_CLASS_CONST, attr, line, &[class, member])
                }
                None => {
                    let object = self.expression(access.object);
                    let property = self.member(&access.property);

                    self.node(SHARP_AST_PROP, 0, line, &[object, property])
                }
            },
            Expression::Call(Call::NullSafeMethod(call)) => {
                let object = self.null_safe_object(call.object);
                let method = self.member(&call.method);
                let arguments = self.arguments(&call.argument_list);

                self.node(SHARP_AST_NULLSAFE_METHOD_CALL, 0, line, &[object, method, arguments])
            }
            Expression::Access(Access::NullSafeProperty(access)) => {
                let object = self.null_safe_object(access.object);
                let property = self.member(&access.property);

                self.node(SHARP_AST_NULLSAFE_PROP, 0, line, &[object, property])
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
            Expression::Array(array) => self.array(array),
            Expression::ArrayAccess(access) => {
                let value = self.expression(access.array);
                let key = self.expression(access.index);

                self.node(SHARP_AST_DIM, 0, line, &[value, key])
            }
            _ => unreachable!("check_slice refuses the expression `{expression}`"),
        })
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
    /// `CLOSURE` whose body returns the expression.
    fn arrow_function(&mut self, arrow_function: &ArrowFunction) -> u32 {
        let lambda = arrow_function.span();
        if !self.names.captures(&lambda).iter().any(|(_, local)| self.names.is_written(local)) {
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
            let value = lowering.expression(arrow_function.expression);
            let r#return = lowering.node(SHARP_AST_RETURN, 0, line, &[value]);

            lowering.node(SHARP_AST_STMT_LIST, 0, line, &[r#return])
        });

        self.declaration(SHARP_AST_CLOSURE, 0, lambda, lambda, b"", &[parameters, uses, body, NULL, NULL])
    }

    /// A lambda with a block body is a `CLOSURE`, as php-src's grammar builds `function () use (…) { … }`.
    fn closure(&mut self, closure: &Closure) -> u32 {
        let lambda = closure.span();
        let parameters = self.lambda_parameters(&closure.parameter_list);
        let uses = self.closure_uses(lambda);
        let body = self.lambda_body(|lowering| lowering.block(&closure.body));

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
    /// element is an `ARRAY_ELEM` of its value and its key or null.
    fn array(&mut self, array: &Array) -> u32 {
        let mut elements = Vec::new();
        for element in &array.elements {
            let value_and_key = match element {
                ArrayElement::Value(element) => [self.expression(element.value), NULL],
                ArrayElement::KeyValue(element) => [self.expression(element.value), self.expression(element.key)],
                ArrayElement::Variadic(_) | ArrayElement::Missing(_) => {
                    unreachable!("check_slice refuses a spread or missing literal element")
                }
            };

            elements.push(self.node(SHARP_AST_ARRAY_ELEM, 0, self.line(element), &value_and_key));
        }

        self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, self.line(array), &elements)
    }

    /// A template without `${…}` is its text, as php-src's grammar builds a string without interpolation. Any other
    /// is an `ENCAPS_LIST` of its text that is not empty and its expressions, on the line of its first part.
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
                            children.push(self.expression(interpolation.expression));
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

    /// What an assignment, a compound assignment, `++` or `--` writes: a local or parameter, `object.name`,
    /// `Class.name`, which php-src's grammar builds as the static property `Class::$name`, or an index of one of them,
    /// as php-src's `variable` rule takes them.
    fn target(&mut self, target: &Expression) -> u32 {
        match target {
            Expression::ConstantAccess(name) if matches!(self.names.binding(&name.name), Some(Binding::Local(_))) => {
                self.variable(name.span(), name.name.value())
            }
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
                let key = self.expression(access.index);

                self.node(SHARP_AST_DIM, 0, self.line(target), &[value, key])
            }
            _ => unreachable!("check_slice refuses writing to `{target}`"),
        }
    }

    /// A local, a parameter or `this` is a PHP variable of the same name. Any other bare name outside a call is a
    /// constant, which the engine looks up in the namespace, then globally, as PHP does for an unqualified name.
    fn name(&mut self, name: &ConstantAccess) -> u32 {
        let line = self.line(name);

        match self.names.binding(&name.name) {
            Some(Binding::Local(_) | Binding::This) => self.variable(name.span(), name.name.value()),
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

    /// `Class.m()` is a static call on the class's full name, and `super.m()` one on `parent`. Any other `object.m()`
    /// is an instance call.
    fn method_call(&mut self, call: &MethodCall) -> u32 {
        let line = self.line(call);
        let (kind, object) = match (self.names.static_call_class(call), call.object) {
            (Some(class), _) => {
                let class = self.string(ZEND_NAME_FQ, self.line(class), self.names.get(&class.name));

                (SHARP_AST_STATIC_CALL, class)
            }
            (None, Expression::Parent(keyword)) => {
                (SHARP_AST_STATIC_CALL, self.string(ZEND_NAME_NOT_FQ, self.line(keyword), b"parent"))
            }
            (None, object) => (SHARP_AST_METHOD_CALL, self.expression(object)),
        };
        let method = self.member(&call.method);
        let arguments = self.arguments(&call.argument_list);

        self.node(kind, 0, line, &[object, method, arguments])
    }

    fn member(&mut self, member: &ClassLikeMemberSelector) -> u32 {
        let ClassLikeMemberSelector::Identifier(name) = member else {
            unreachable!("check_slice refuses the member name `{member}`");
        };

        self.string(0, self.line(name.span), name.value)
    }

    fn arguments(&mut self, list: &ArgumentList) -> u32 {
        let mut arguments = Vec::new();
        for argument in &list.arguments {
            arguments.push(match argument {
                Argument::Positional(positional) => self.positional_argument(positional),
                Argument::Named(named) => self.named_argument(named),
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
                PartialArgument::Named(named) => self.named_argument(named),
                _ => unreachable!("check_slice refuses a placeholder argument"),
            });
        }

        self.node(SHARP_AST_ARG_LIST, 0, self.line(list), &arguments)
    }

    fn positional_argument(&mut self, argument: &PositionalArgument) -> u32 {
        if argument.ellipsis.is_some() {
            unreachable!("check_slice refuses a spread argument");
        }

        self.expression(argument.value)
    }

    fn named_argument(&mut self, argument: &NamedArgument) -> u32 {
        let line = self.line(argument.name.span);
        let name = self.string(0, line, argument.name.value);
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
        let name = store_text(&self.texts, name);

        let node = &mut self.nodes[index as usize];
        node.end_line = end_line;
        node.text = name;

        index
    }

    fn string(&mut self, attr: u32, line: u32, text: &[u8]) -> u32 {
        let text = store_text(&self.texts, text);

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
            // PHP methods are open to overriding, and `method` lowers `override` to `#[\Override]`.
            Modifier::Virtual(_) | Modifier::Override(_) => 0,
            Modifier::Final(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_) => unreachable!("check_slice refuses the modifier `{modifier}`"),
        };
    }

    flags
}

/// The flags of a class's modifiers. `public` adds none, because every PHP class is public.
fn class_flags(modifiers: &Sequence<Modifier>) -> u32 {
    let mut flags = 0;
    for modifier in modifiers {
        flags |= match modifier {
            Modifier::Public(_) => 0,
            Modifier::Abstract(_) => ZEND_ACC_EXPLICIT_ABSTRACT_CLASS,
            Modifier::Final(_) => ZEND_ACC_FINAL,
            Modifier::Protected(_)
            | Modifier::Private(_)
            | Modifier::Static(_)
            | Modifier::Readonly(_)
            | Modifier::PublicSet(_)
            | Modifier::ProtectedSet(_)
            | Modifier::PrivateSet(_)
            | Modifier::Virtual(_)
            | Modifier::Override(_) => unreachable!("check_slice refuses the class modifier `{modifier}`"),
        };
    }

    flags
}

/// Whether PHP takes an initial value as the property's default: a constant expression without `new`, on a property
/// that is not `readonly`, which takes no default.
fn is_default(property: &Property, value: &Expression) -> bool {
    !matches!(property, Property::Hooked(auto_property) if auto_property.hook_list.is_get_only())
        && value.is_constant(&PHPVersion::PHP85, false)
}

/// The flags an auto-property's accessors add: `readonly` for a get-only property, which spec section 6.1 sets in
/// the constructor, or the set visibility php-src writes `private(set)` or `protected(set)` from the `set` accessor's
/// access modifier. A private property needs no set visibility.
fn accessor_flags(modifiers: &Sequence<Modifier>, accessors: &PropertyHookList) -> u32 {
    if accessors.is_get_only() {
        return ZEND_ACC_READONLY;
    }

    let set = accessors.hooks.iter().find(|accessor| accessor.name.value == b"set");
    let flags = match set.and_then(|set| set.modifiers.first()) {
        None => 0,
        Some(Modifier::Protected(_)) => ZEND_ACC_PROTECTED_SET,
        Some(Modifier::Private(_)) => ZEND_ACC_PRIVATE_SET,
        Some(modifier) => unreachable!("check_slice refuses the accessor modifier `{modifier}`"),
    };

    if modifiers.contains_private() { 0 } else { flags }
}

/// The binary operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided.
fn binary_kind(operator: BinaryOperator) -> (sharp_kind, u32) {
    match operator {
        BinaryOperator::Addition(_) => (SHARP_AST_BINARY_OP, ZEND_ADD),
        BinaryOperator::Subtraction(_) => (SHARP_AST_BINARY_OP, ZEND_SUB),
        BinaryOperator::Multiplication(_) => (SHARP_AST_BINARY_OP, ZEND_MUL),
        BinaryOperator::Division(_) => (SHARP_AST_BINARY_OP, ZEND_DIV),
        BinaryOperator::Modulo(_) => (SHARP_AST_BINARY_OP, ZEND_MOD),
        BinaryOperator::Exponentiation(_) => (SHARP_AST_BINARY_OP, ZEND_POW),
        BinaryOperator::Equal(_) => (SHARP_AST_BINARY_OP, ZEND_IS_EQUAL),
        BinaryOperator::NotEqual(_) => (SHARP_AST_BINARY_OP, ZEND_IS_NOT_EQUAL),
        BinaryOperator::Identical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_IDENTICAL),
        BinaryOperator::NotIdentical(_) => (SHARP_AST_BINARY_OP, ZEND_IS_NOT_IDENTICAL),
        BinaryOperator::LessThan(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER),
        BinaryOperator::LessThanOrEqual(_) => (SHARP_AST_BINARY_OP, ZEND_IS_SMALLER_OR_EQUAL),
        BinaryOperator::GreaterThan(_) => (SHARP_AST_GREATER, 0),
        BinaryOperator::GreaterThanOrEqual(_) => (SHARP_AST_GREATER_EQUAL, 0),
        BinaryOperator::And(_) => (SHARP_AST_AND, 0),
        BinaryOperator::Or(_) => (SHARP_AST_OR, 0),
        BinaryOperator::NullCoalesce(_) => (SHARP_AST_COALESCE, 0),
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
        | BinaryOperator::LowXor(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

/// The prefix operators of the slice, as php-src's grammar builds them. Every operator is named, so a new one does
/// not compile until it is decided.
fn prefix_kind(operator: &UnaryPrefixOperator) -> (sharp_kind, u32) {
    match operator {
        UnaryPrefixOperator::Negation(_) => (SHARP_AST_UNARY_MINUS, 0),
        UnaryPrefixOperator::Plus(_) => (SHARP_AST_UNARY_PLUS, 0),
        UnaryPrefixOperator::Not(_) => (SHARP_AST_UNARY_OP, ZEND_BOOL_NOT),
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
        | UnaryPrefixOperator::VoidCast(..)
        | UnaryPrefixOperator::BitwiseNot(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
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
        AssignmentOperator::Exponentiation(_) => (SHARP_AST_ASSIGN_OP, ZEND_POW),
        AssignmentOperator::Coalesce(_) => (SHARP_AST_ASSIGN_COALESCE, 0),
        AssignmentOperator::Modulo(_)
        | AssignmentOperator::Concat(_)
        | AssignmentOperator::BitwiseAnd(_)
        | AssignmentOperator::BitwiseOr(_)
        | AssignmentOperator::BitwiseXor(_)
        | AssignmentOperator::LeftShift(_)
        | AssignmentOperator::RightShift(_) => unreachable!("check_slice refuses the operator `{operator}`"),
    }
}

#[cfg(test)]
mod tests {
    use mago_names::resolver::NameResolver;

    use super::*;
    use crate::catch_panic;

    /// Lowers a class holding `method`, skipping the semantic checks that would refuse it, and asserts the result is
    /// one internal error and no nodes.
    fn assert_internal_error(method: &str) {
        let source = format!("class Report\n{{\n    {method}\n}}\n");

        let unit = catch_panic(|| {
            let file = File::ephemeral(Cow::Borrowed(b"src/Report.sharp"), Cow::Owned(source.into_bytes()));
            let arena = LocalArena::new();
            let program = parse_file_with_dialect(&arena, &file, Dialect::Sharp, ParserSettings::default());
            let checked = CheckedProgram::unchecked(program, NameResolver::new(&arena).resolve(program));

            Lowering::new(&Lines::new(&file.contents), checked.names()).program(&checked)
        });

        assert_eq!(unit.abi.node_count, 0, "{method}");
        assert_eq!(unit.diagnostics.len(), 1, "{method}");
        assert!(unit.diagnostics[0].message.bytes().starts_with(b"internal error in the PHP# front end: "), "{method}");
    }

    #[test]
    fn a_write_to_anything_but_a_local_or_a_member_returns_an_internal_error_and_no_nodes() {
        assert_internal_error("public void run() { PHP_INT_MAX = 1; }");
        assert_internal_error("public void run() { PHP_INT_MAX += 1; }");
        assert_internal_error("public void run() { PHP_INT_MAX++; }");
    }

    #[test]
    fn a_parameter_without_a_type_returns_an_internal_error_and_no_nodes() {
        assert_internal_error("public void run($extra) {}");
    }
}
