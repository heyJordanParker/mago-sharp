//! Inline forms: a standard library method whose body is one expression runs at its call as that expression, with
//! the call's receiver and arguments in the expression's slots, the way a compiler inlines a one-line method. An
//! `extern` method's body is the call of its native function, so its call runs as that call.
//!
//! The orchestrator takes forms only from the standard library's files, those `File::is_standard_library` marks, and
//! lowers each of them with an empty [`InlineForms`], so in v1 one library form inlines no other, but the native call
//! of an `extern` method of its own class.

use std::collections::HashMap;

use mago_analyzer::artifacts::MemberCall;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_span::HasSpan;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::ArrayElement;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::Conditional;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::IfBody;
use mago_syntax::cst::Literal;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Pattern;
use mago_syntax::cst::Property;
use mago_syntax::cst::Return;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;
use mago_word::Word;
use xxhash_rust::xxh3::Xxh3;

use super::Lines;
use super::Lowering;
use super::NULL;
use super::ZEND_ARRAY_SYNTAX_SHORT;
use super::types::Declaration;
use super::types::DeclarationKind;
use super::types::KeyOrder;
use super::types::receiver_classes;
use crate::lower::checked::CheckedProgram;
use crate::sharp_kind::SHARP_AST_ARRAY;
use crate::sharp_kind::SHARP_AST_ARRAY_ELEM;
use crate::sharp_kind::SHARP_AST_CALLABLE_CONVERT;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_node;
use crate::sharp_value;
use crate::store_text;
use crate::unit::Read;
use crate::unit::form_fingerprint;

/// The prelude's mark of an engine function's parameter that takes the type arguments of the `Map` or the `Set` the
/// inlined call gives.
pub(crate) const TAKES_RESULT_TYPE: &[u8] = b"Sharp\\Internal\\TakesResultType";

/// The prelude's mark of an engine function's parameter that takes the `operator <=>` the checker chose for the keys the
/// inlined call orders by.
pub(crate) const TAKES_KEY_ORDER: &[u8] = b"Sharp\\Internal\\TakesKeyOrder";

/// The name of a form's slot for what a [`TAKES_RESULT_TYPE`] parameter takes, which no PHP# name can be.
pub(crate) const RESULT_TYPE_SLOT: &[u8] = b"result#type";

/// The name of a form's slot for what a [`TAKES_KEY_ORDER`] parameter takes, which no PHP# name can be.
pub(crate) const KEY_ORDER_SLOT: &[u8] = b"key#order";

/// A standard library method's body, lowered, with a slot where it reads its receiver or a parameter.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InlineForm {
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<u8>,
    root: u32,
    /// The node each value replaces: the receiver first for an instance method, then each parameter in declaration
    /// order.
    slots: Vec<u32>,
    /// For each slot, the node of the parameter's default when it is a literal, which a call that leaves the argument
    /// out passes, as `trim()` passes `trim(string? characters = null)` its `null`.
    defaults: Vec<Option<u32>>,
    /// How many slots the form reads in declaration order, the receiver first, before its first call. Their values run
    /// in the order the call runs them, so a value in a later slot must run nothing when read.
    in_order: usize,
    /// The node the type arguments of the collection the inlined call gives replace, [`RESULT_TYPE_SLOT`].
    result_type: Option<u32>,
    /// The node the `operator <=>` of the keys the inlined call orders by replaces, [`KEY_ORDER_SLOT`].
    key_order: Option<u32>,
    /// The plain PHP function the body calls, when the body is that one call.
    php_call: Option<PhpCall>,
    /// The arguments the form runs for only, each a literal that decides a condition of the body.
    fixed: Vec<FixedArgument>,
}

/// The plain PHP function a form's body is one call of, as `count` is for a `List`'s `count()`, whose body is
/// `global.count(items)`, and what the body passes it, in the function's order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PhpCall {
    /// The function's name as its declaration writes it.
    pub function: Word,
    pub arguments: Vec<PhpCallArgument>,
}

/// What a form's body passes the PHP function it calls, in one of the function's parameters.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PhpCallArgument {
    /// The value of the form's slot at this index: the receiver first for an instance or extension member, then each
    /// parameter in declaration order.
    Slot(usize),
    /// A literal, as its token's text: `true`, `512`, and a string as its value in double quotes, its escapes read.
    Literal(Vec<u8>),
    /// A plain PHP constant, by its name.
    Constant(Word),
}

impl PhpCallArgument {
    /// `literal` as a form passes it, so a PHP call's own literal and a form's compare equal when PHP reads them as one
    /// value: an integer by its value, a string by its bytes once its escapes are read, and a keyword in lowercase.
    #[must_use]
    pub fn literal(literal: &Literal<'_>) -> Self {
        Self::Literal(match literal {
            Literal::String(string) => [b"\"".as_slice(), string.value.unwrap_or(string.raw), b"\""].concat(),
            Literal::Integer(integer) => {
                integer.value.map_or_else(|| integer.raw.to_vec(), |value| value.to_string().into_bytes())
            }
            Literal::Float(float) => float.value.to_string().into_bytes(),
            Literal::True(_) => b"true".to_vec(),
            Literal::False(_) => b"false".to_vec(),
            Literal::Null(_) => b"null".to_vec(),
        })
    }
}

/// An argument a form fixes, which a call passes as a literal that decides a condition of the method's body.
///
/// The form runs only for a call that passes a literal in the slot `slot` that equals `literal`, when `equal`, or one
/// that doesn't, otherwise, as a text to replace other than `""` decides `replace`'s `old != ""`, so the form holds the
/// branch it takes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FixedArgument {
    /// The receiver first for an instance or extension member, then each parameter in declaration order.
    pub slot: usize,
    /// The literal, as [`PhpCallArgument::literal`] writes it.
    pub literal: Vec<u8>,
    pub equal: bool,
}

impl FixedArgument {
    /// Whether `argument` is a literal the form runs for.
    #[must_use]
    pub fn holds(&self, argument: &Expression<'_>) -> bool {
        matches!(argument, Expression::Literal(literal) if matches!(
            PhpCallArgument::literal(literal),
            PhpCallArgument::Literal(text) if (text == self.literal) == self.equal
        ))
    }
}

/// What a fold knows of a method's parameters: the literal default of each one a call leaves out, by name, the names
/// of the rest, whether a comparison of one of the rest with a literal holds, and the arguments the fold fixes so, by
/// name, as [`FixedArgument`] states them.
struct Known<'known, 'ast> {
    defaults: &'known [(&'known [u8], &'known Expression<'ast>)],
    kept: Vec<&'known [u8]>,
    holds: bool,
    fixed: Vec<(Vec<u8>, Vec<u8>, bool)>,
}

/// The value an inline form's slot takes.
pub(super) enum InlineReceiver<'value, 'ast> {
    /// A value the form reads where its slot is.
    Expression(&'value Expression<'ast>),
    /// A place already lowered, which reading runs nothing, as the place a `ref` receiver passes.
    Place(u32),
}

impl InlineForm {
    /// The xxh3-64 of the form's nodes, children and texts, which `Reads::inlined` stores. The root, the slots and
    /// the order follow from the nodes. The nodes carry no line, because each inlined node takes the line of its call.
    /// The arguments the form fixes decide which calls it inlines and follow from no node, so they count too, in the
    /// little-endian bytes the unit's own fingerprints read, which a cache written on another machine reads alike.
    #[must_use]
    #[allow(clippy::little_endian_bytes)]
    pub fn fingerprint(&self) -> u64 {
        let nodes = form_fingerprint(&self.nodes, &self.children, &self.texts);
        if self.fixed.is_empty() {
            return nodes;
        }
        let mut hasher = Xxh3::new();
        hasher.update(&nodes.to_le_bytes());
        for fixed in &self.fixed {
            hasher.update(&fixed.slot.to_le_bytes());
            hasher.update(&fixed.literal.len().to_le_bytes());
            hasher.update(&fixed.literal);
            hasher.update(&[u8::from(fixed.equal)]);
        }

        hasher.digest()
    }

    /// The arguments the form runs for only, each a literal that decides a condition of the body.
    #[must_use]
    pub fn fixed(&self) -> &[FixedArgument] {
        &self.fixed
    }

    /// The plain PHP function the form's body is one call of, and what it passes it.
    #[must_use]
    pub const fn php_call(&self) -> Option<&PhpCall> {
        self.php_call.as_ref()
    }

    /// How many slots the form reads in declaration order, the receiver first, before its first call. A value in a
    /// later slot runs out of the order the call writes it in.
    #[must_use]
    pub const fn in_order(&self) -> usize {
        self.in_order
    }
}

/// The inline forms a file's lowering may inline, by the name [`key`] gives the method each runs.
#[derive(Debug, Default)]
pub struct InlineForms(HashMap<Vec<u8>, InlineForm>);

impl InlineForms {
    pub(crate) fn get(&self, name: &[u8]) -> Option<&InlineForm> {
        self.0.get(name)
    }

    /// Each form, with the class it runs on and the member it runs, as [`key`] gives them. A member has a form for each
    /// call that leaves out parameters, as [`folded_key`] names it.
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[u8], &InlineForm)> {
        self.0.iter().filter_map(|(name, form)| {
            let separator = name.windows(2).position(|pair| pair == b"::")?;
            let member = &name[separator + 2..];
            let member = member.iter().position(|byte| *byte == b'#').map_or(member, |arity| &member[..arity]);

            Some((&name[..separator], member, form))
        })
    }
}

impl FromIterator<(Vec<u8>, InlineForm)> for InlineForms {
    fn from_iter<I>(forms: I) -> Self
    where
        I: IntoIterator<Item = (Vec<u8>, InlineForm)>,
    {
        Self(forms.into_iter().collect())
    }
}

/// The name of the PHP# class `class`'s method `method`, as `Sharp\Text::startsWith`: the class and the method exact,
/// as the codebase keys a PHP# class and its methods, spec section 24.
pub(crate) fn key(class: &[u8], method: &[u8]) -> Vec<u8> {
    [class, b"::", method].concat()
}

/// The inline form of each public method of `checked` the inlining rule takes, with its name.
///
/// A method's body is one expression built only from calls, literals and reads of its receiver and parameters, each
/// read exactly once, or it is its native function's call. The expression's call of an `extern` method of the same
/// class, a private one among them, runs as that method's native call, so a copy in another class reaches it.
#[must_use]
pub fn inline_forms(checked: &CheckedProgram<'_>) -> Vec<(Vec<u8>, InlineForm)> {
    let lines = Lines::new(&checked.file().contents);
    let mut forms = Vec::new();
    let classes = Node::Program(checked.program()).filter_map(|node| match node {
        Node::Class(class) => Some(*class),
        _ => None,
    });
    for class in classes {
        let name = checked.names().get(&class.name);
        let natives: InlineForms = class
            .members
            .iter()
            .filter_map(|member| match member {
                ClassLikeMember::Method(method)
                    if method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
                {
                    Lowering::new(&lines, checked.names(), checked.types())
                        .method_form(name, method, None, &[], true)
                        .map(|form| (key(name, method.name.value), form))
                }
                _ => None,
            })
            .collect();
        let types = checked.types().with_inline_forms(&natives);
        let public = |member: &[u8]| checked.types().member_declaration(name, member).public;
        let lowering = || Lowering::new(&lines, checked.names(), &types);
        for member in &class.members {
            match member {
                ClassLikeMember::Method(method) => {
                    let built = method_forms(&lowering, name, method, None);
                    if !built.is_empty() && public(method.name.value) {
                        forms.extend(built);
                    }
                }
                ClassLikeMember::Extension(extension) => {
                    let [receiver] = extension.parameter_list.parameters.as_slice() else {
                        unreachable!("semantics refuses an extension without exactly one receiver");
                    };
                    for member in &extension.members {
                        match member {
                            ClassLikeMember::Method(method) => {
                                let built = method_forms(&lowering, name, method, Some(receiver));
                                if !built.is_empty() && public(method.name.value) {
                                    forms.extend(built);
                                }
                            }
                            ClassLikeMember::Property(Property::Computed(property)) => {
                                let form = lowering().form(
                                    name,
                                    property.variable.name,
                                    &[receiver],
                                    Body::Expression(property.body.expression),
                                    true,
                                    Vec::new(),
                                );
                                forms.extend(
                                    form.filter(|_| public(property.variable.name))
                                        .map(|form| (key(name, property.variable.name), form)),
                                );
                            }
                            _ => unreachable!("semantics refuses the extension member `{member}`"),
                        }
                    }
                }
                _ => {}
            }
        }
    }

    forms
}

/// The name of the form of `class`'s method `method` for a call that passes `arity` values, the receiver counted, and
/// leaves out the parameters after them, as `Sharp\Text\StringMembers::startsWith#2`. No PHP# name holds `#`.
pub(crate) fn folded_key(class: &[u8], method: &[u8], arity: usize) -> Vec<u8> {
    [&key(class, method), b"#".as_slice(), arity.to_string().as_bytes()].concat()
}

/// The forms of `class`'s method `method`, each with its name: the form of a call that passes every argument, and the
/// form of each call that leaves out some of the last parameters whose default is a literal. Such a call takes those
/// defaults, so its form folds every branch they decide, as `startsWith(prefix)` leaves out `ignoreCase` and runs
/// `str_starts_with` alone. A form folds each comparison of a parameter the call passes with a literal as it holds,
/// or else as it fails, and runs only for a call whose literal decides it so, as `replace(" ", "-")` runs
/// `str_replace` where `old != ""` holds.
fn method_forms<'lowering, 'arena, F>(
    lowering: &F,
    class: &[u8],
    method: &Method,
    receiver: Option<&FunctionLikeParameter>,
) -> Vec<(Vec<u8>, InlineForm)>
where
    'arena: 'lowering,
    F: Fn() -> Lowering<'lowering, 'arena>,
{
    let mut defaults: Vec<(&[u8], &Expression)> = method
        .parameter_list
        .parameters
        .iter()
        .rev()
        .map_while(|parameter| Some((parameter.variable.name, literal_default(parameter)?)))
        .collect();
    defaults.reverse();

    (0..=defaults.len())
        .filter_map(|omitted| {
            let defaults = &defaults[defaults.len() - omitted..];
            let form = lowering()
                .method_form(class, method, receiver, defaults, true)
                .or_else(|| lowering().method_form(class, method, receiver, defaults, false))?;
            let name = if omitted == 0 {
                key(class, method.name.value)
            } else {
                folded_key(class, method.name.value, form.slots.len())
            };

            Some((name, form))
        })
        .collect()
}

/// The name and the literal a comparison of `lhs` with `rhs` holds, on either side.
fn name_and_literal<'value, 'ast>(
    lhs: &'value Expression<'ast>,
    rhs: &'value Expression<'ast>,
) -> Option<(&'value ConstantAccess<'ast>, &'value Literal<'ast>)> {
    match (lhs, rhs) {
        (Expression::ConstantAccess(name), Expression::Literal(literal))
        | (Expression::Literal(literal), Expression::ConstantAccess(name)) => Some((name, literal)),
        _ => None,
    }
}

/// The default of `parameter` when it is a literal.
fn literal_default<'body, 'ast>(parameter: &'body FunctionLikeParameter<'ast>) -> Option<&'body Expression<'ast>> {
    parameter
        .default_value
        .as_ref()
        .map(|default| default.value)
        .filter(|value| matches!(value, Expression::Literal(_)))
}

/// What a library member's body runs: one expression, or the call of an `extern` method's native function.
#[derive(Clone, Copy)]
enum Body<'body, 'ast> {
    Expression(&'body Expression<'ast>),
    Native(&'body Method<'ast>),
}

/// What a form's body runs, in the order it runs it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A read of the slot at this index.
    Read(usize),
    Call,
}

impl Lowering<'_, '_> {
    /// The form of `class`'s method `method`, when the inlining rule takes it. A method an extension adds takes its
    /// `receiver` first.
    ///
    /// The body is one expression, or, for an `extern` method, the call of its native function. A `void` method's
    /// expression body is a call, whose value no caller reads. A constructor, which declares no return type, has none.
    ///
    /// A call that leaves out the last parameters takes their literal defaults, `defaults` by name, so the body is what
    /// remains once every branch they decide is folded, and the form has no slot for them. A condition that compares
    /// a parameter the call passes with a literal folds as it `holds`, and the form runs only for a call whose literal
    /// argument decides it so, as [`FixedArgument`] states.
    fn method_form(
        self,
        class: &[u8],
        method: &Method,
        receiver: Option<&FunctionLikeParameter>,
        defaults: &[(&[u8], &Expression)],
        holds: bool,
    ) -> Option<InlineForm> {
        let parameters: Vec<&FunctionLikeParameter> =
            receiver.into_iter().chain(&method.parameter_list.parameters).collect();
        let kept = parameters.len() - defaults.len();
        let mut known = Known {
            defaults,
            kept: parameters[..kept].iter().map(|parameter| parameter.variable.name).collect(),
            holds,
            fixed: Vec::new(),
        };
        let is_void = method.return_type_hint.as_ref().is_some_and(|hint| matches!(hint.hint, Hint::Void(_)));
        let body = match &method.body {
            MethodBody::Expression(body) if method.returns_value() || is_void => {
                Body::Expression(self.folded(body.expression, &mut known))
            }
            MethodBody::Concrete(block) => {
                // The body's first statement after the fold is the `return` that runs, and no statement after it does.
                let mut statements = Vec::new();
                self.folded_statements(block.statements.iter(), &mut known, &mut statements);
                match statements.first() {
                    Some(Statement::Return(Return { value: Some(value), .. })) => {
                        Body::Expression(self.folded(value, &mut known))
                    }
                    _ => return None,
                }
            }
            MethodBody::Abstract(_)
                if defaults.is_empty()
                    && method.returns_value()
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
            {
                Body::Native(method)
            }
            _ => return None,
        };

        let fixed = known.fixed;
        self.form(class, method.name.value, &parameters[..kept], body, receiver.is_some(), fixed)
    }

    /// `expression` with each `?:` whose condition `known` decides folded to the branch it takes.
    fn folded<'body, 'ast>(
        &self,
        mut expression: &'body Expression<'ast>,
        known: &mut Known,
    ) -> &'body Expression<'ast> {
        loop {
            expression = match expression {
                Expression::Parenthesized(parenthesized)
                    if matches!(parenthesized.expression, Expression::Conditional(_)) =>
                {
                    parenthesized.expression
                }
                Expression::Conditional(Conditional { condition, then: Some(then), r#else, .. }) => {
                    match self.decided(condition, known) {
                        Some(true) => then,
                        Some(false) => r#else,
                        None => return expression,
                    }
                }
                _ => return expression,
            };
        }
    }

    /// Appends `statements` to `folded`, each `if` whose condition `known` decides replaced by the statements of the
    /// branch it takes. PHP# has no `elseif`: its `else if` is an `else` that holds an `if`, which this folds in turn.
    fn folded_statements<'body, 'ast>(
        &self,
        statements: impl IntoIterator<Item = &'body Statement<'ast>>,
        known: &mut Known,
        folded: &mut Vec<&'body Statement<'ast>>,
    ) where
        'ast: 'body,
    {
        for statement in statements {
            let Statement::If(r#if) = statement else {
                folded.push(statement);
                continue;
            };
            let IfBody::Statement(body) = &r#if.body else {
                unreachable!("check_slice refuses a colon-delimited `if`");
            };
            let Some(taken) = self.decided(r#if.condition, known) else {
                folded.push(statement);
                continue;
            };
            let branch =
                if taken { Some(body.statement) } else { body.else_clause.as_ref().map(|clause| clause.statement) };
            match branch {
                Some(Statement::Block(block)) => self.folded_statements(block.statements.iter(), known, folded),
                Some(branch) => self.folded_statements([branch], known, folded),
                None => {}
            }
        }
    }

    /// Whether `condition` holds when each parameter `known` gives a default holds it: a `bool` parameter itself, `!`,
    /// `==`, `===`, `!=` or `!==` between a parameter and a literal, and `is` of a type that holds no `null` on a
    /// parameter whose default is `null`. A comparison of a parameter the call passes with a literal holds as `known`
    /// says, and `known` fixes the call's argument to a literal that decides it so. None when it reads anything else.
    fn decided(&self, condition: &Expression, known: &mut Known) -> Option<bool> {
        match condition {
            Expression::Parenthesized(parenthesized) => self.decided(parenthesized.expression, known),
            Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) => {
                self.decided(operand, known).map(|holds| !holds)
            }
            Expression::Binary(Binary { lhs, operator, rhs }) => {
                let equal = matches!(operator, BinaryOperator::Equal(_) | BinaryOperator::Identical(_));
                if !equal && !matches!(operator, BinaryOperator::NotEqual(_) | BinaryOperator::NotIdentical(_)) {
                    return None;
                }
                let (default, literal) = match (self.default_of(lhs, known), self.default_of(rhs, known)) {
                    (Some(default), None) => (default, *rhs),
                    (None, Some(default)) => (default, *lhs),
                    (None, None) => return self.fixed(lhs, rhs, equal, known),
                    (Some(_), Some(_)) => return None,
                };
                let (Expression::Literal(default), Expression::Literal(literal)) = (default, literal) else {
                    return None;
                };

                Some((PhpCallArgument::literal(default) == PhpCallArgument::literal(literal)) == equal)
            }
            // `null` is no value of a type that holds no `null`, as `limit is int count` fails for `int? limit = null`.
            Expression::Is(is) => match (self.default_of(is.value, known)?, is.pattern) {
                (Expression::Literal(Literal::Null(_)), Pattern::Type(pattern))
                    if matches!(
                        pattern.hint,
                        Hint::Integer(_)
                            | Hint::String(_)
                            | Hint::Float(_)
                            | Hint::Bool(_)
                            | Hint::Array(_)
                            | Hint::Object(_)
                    ) =>
                {
                    Some(false)
                }
                _ => None,
            },
            // A condition is a `bool`, whose literal default is `true` or `false`.
            value => Some(matches!(self.default_of(value, known)?, Expression::Literal(Literal::True(_)))),
        }
    }

    /// Decides `lhs == rhs`, or `lhs != rhs` when not `equal`, where one side reads a parameter the call passes and the
    /// other is a literal, as `known` says such a comparison does, and fixes the call's argument to a literal that
    /// decides it so. None for any other operands.
    fn fixed(&self, lhs: &Expression, rhs: &Expression, equal: bool, known: &mut Known) -> Option<bool> {
        let (name, literal) = name_and_literal(lhs, rhs)?;
        let parameter = name.name.value();
        if !matches!(self.names.binding(&name.name), Some(Binding::Local(_))) || !known.kept.contains(&parameter) {
            return None;
        }
        let PhpCallArgument::Literal(literal) = PhpCallArgument::literal(literal) else {
            unreachable!("a literal is passed as a literal");
        };
        known.fixed.push((parameter.to_vec(), literal, equal == known.holds));

        Some(known.holds)
    }

    /// The default of the parameter `expression` reads, when `known` gives one. A name in a method is its parameter
    /// whenever one is named so, since no block inside redeclares a name, spec section 3.
    fn default_of<'default, 'ast>(
        &self,
        expression: &Expression,
        known: &Known<'default, 'ast>,
    ) -> Option<&'default Expression<'ast>> {
        let Expression::ConstantAccess(name) = expression else {
            return None;
        };

        known.defaults.iter().find(|(parameter, _)| *parameter == name.name.value()).map(|(_, default)| *default)
    }

    /// The form of `class`'s member `name`, which takes `parameters`, the receiver first for an `extension` member, and
    /// runs `body`, lowered into this empty lowering, when the inlining rule takes it: the body's steps are ones the
    /// rule takes. The form runs only for a call that passes the arguments `fixed` names by parameter, as
    /// [`FixedArgument`] states.
    fn form(
        mut self,
        class: &[u8],
        name: &[u8],
        parameters: &[&FunctionLikeParameter],
        body: Body,
        extension: bool,
        fixed: Vec<(Vec<u8>, Vec<u8>, bool)>,
    ) -> Option<InlineForm> {
        // A form runs without the method's statements before its body, which make each `Map` and `Set` a parameter
        // holds the object. An extension's receiver needs none: a PHP# caller's is the object already, and the call
        // crosses one plain PHP gives, as a method call's receiver.
        if parameters.iter().enumerate().any(|(index, parameter)| {
            parameter.is_variadic() || (self.crossing_type(parameter).is_some() && !(extension && index == 0))
        }) {
            return None;
        }
        self.forming = true;

        let receiver = match self.types.member_declaration(class, name).kind {
            DeclarationKind::Method { overridable: false } => true,
            DeclarationKind::StaticMethod => false,
            _ => return None,
        };

        let names: Vec<&[u8]> = receiver
            .then_some(b"this".as_slice())
            .into_iter()
            .chain(parameters.iter().map(|parameter| parameter.variable.name))
            .collect();
        let fixed: Vec<FixedArgument> = fixed
            .into_iter()
            .map(|(parameter, literal, equal)| {
                let slot = names.iter().position(|name| *name == parameter.as_slice()).unwrap_or_else(|| {
                    unreachable!("the fold fixes only a parameter of the method, which names a slot")
                });

                FixedArgument { slot, literal, equal }
            })
            .collect();
        let (root, in_order) = match body {
            Body::Expression(expression) => {
                let mut steps = Vec::new();
                if !self.steps(expression, &names, &mut steps) {
                    return None;
                }

                let mut each_once: Vec<usize> = steps
                    .iter()
                    .filter_map(|step| match step {
                        Step::Read(slot) => Some(*slot),
                        Step::Call => None,
                    })
                    .collect();
                each_once.sort_unstable();
                if each_once != (0..names.len()).collect::<Vec<_>>() {
                    return None;
                }

                let in_order = steps.iter().zip(0..).take_while(|(step, slot)| **step == Step::Read(*slot)).count();
                (self.expression(expression), in_order)
            }
            // The native call reads each parameter once, in order, and then calls.
            Body::Native(method) => (self.native_call(method, class), names.len()),
        };
        let php_call = match body {
            Body::Expression(expression) => self.php_call(expression, &names),
            Body::Native(_) => None,
        };
        // A literal runs nothing, so a call that leaves its argument out runs the form with it in any slot.
        let defaults: Vec<Option<u32>> = receiver
            .then_some(None)
            .into_iter()
            .chain(parameters.iter().map(
                |parameter| match parameter.default_value.as_ref().map(|default| default.value) {
                    Some(value @ Expression::Literal(_)) => Some(self.expression(value)),
                    _ => None,
                },
            ))
            .collect();
        let mut slots = vec![NULL; names.len()];
        let (mut result_type, mut key_order) = (None, None);
        for (index, node) in self.nodes.iter().enumerate() {
            if node.kind == SHARP_AST_VAR {
                let name = self.nodes[self.children[node.first_child as usize] as usize].text;
                let name = &self.texts[name.offset as usize..(name.offset + name.len) as usize];
                match name {
                    RESULT_TYPE_SLOT => result_type = Some(index as u32),
                    KEY_ORDER_SLOT => key_order = Some(index as u32),
                    // A body whose lowering holds a value in a variable of its own, as a plain PHP `List` renumbered
                    // where it arrives does, runs as a call, since a form reads only its slots.
                    _ => {
                        let slot = names.iter().position(|slot| *slot == name)?;
                        slots[slot] = index as u32;
                    }
                }
            }
        }
        for node in &mut self.nodes {
            node.line = 0;
        }

        Some(InlineForm {
            nodes: self.nodes,
            children: self.children,
            texts: self.texts,
            root,
            slots,
            defaults,
            in_order,
            result_type,
            key_order,
            php_call,
            fixed,
        })
    }

    /// The plain PHP function `expression` is one call of, through its namespace as `global.count(items)` calls it,
    /// and what it passes it: a slot `names` names, a literal, or a plain PHP constant read through its namespace. The
    /// inlining rule took `expression`, so no argument spreads, and a named argument passes no plain value.
    fn php_call(&self, expression: &Expression, names: &[&[u8]]) -> Option<PhpCall> {
        let Expression::Call(Call::Method(call)) = expression else {
            return None;
        };
        let function = self.types.namespace_function(expression)?;
        let arguments = call
            .argument_list
            .arguments
            .iter()
            .map(|argument| {
                let Argument::Positional(argument) = argument else {
                    return None;
                };
                match argument.value {
                    Expression::ConstantAccess(name)
                        if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::This)) =>
                    {
                        names.iter().position(|slot| *slot == name.name.value()).map(PhpCallArgument::Slot)
                    }
                    Expression::Literal(literal) => Some(PhpCallArgument::literal(literal)),
                    value => self.types.namespace_constant(value).map(PhpCallArgument::Constant),
                }
            })
            .collect::<Option<Vec<_>>>()?;

        Some(PhpCall { function, arguments })
    }

    /// Appends what `expression` runs to `steps`, and whether the inlining rule takes all of it: a literal, a read of
    /// one of the slots `names` names, a read of a plain PHP constant through its namespace, as the standard library's
    /// `global.PHP_EOL` reads one, and a call of a plain PHP function through its namespace, as its `global.trim(x)`
    /// calls one, or of a method whose receiver it takes. Both name their constant or function in full, so a copy in another namespace reaches
    /// the same one. A bare constant is the library file's own, which a copy in another namespace would not find.
    fn steps(&self, expression: &Expression, names: &[&[u8]], steps: &mut Vec<Step>) -> bool {
        match expression {
            Expression::Parenthesized(parenthesized) => self.steps(parenthesized.expression, names, steps),
            Expression::Literal(_) => true,
            Expression::ConstantAccess(name)
                if matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::This)) =>
            {
                let Some(slot) = names.iter().position(|slot| *slot == name.name.value()) else {
                    return false;
                };
                steps.push(Step::Read(slot));

                true
            }
            Expression::Access(Access::Property(_)) => self.types.namespace_constant(expression).is_some(),
            Expression::Call(Call::Method(call)) if self.types.namespace_function(expression).is_some() => {
                self.argument_steps(&call.argument_list, names, steps) && self.call_step(steps)
            }
            Expression::Call(Call::Method(call)) => self.method_call_steps(expression, call, names, steps),
            // `!`, `==` and `!=` run what their operands run, as `isEmpty()`'s `global.count(items) == 0` does.
            Expression::UnaryPrefix(UnaryPrefix { operator: UnaryPrefixOperator::Not(_), operand }) => {
                self.steps(operand, names, steps)
            }
            Expression::Binary(Binary {
                lhs,
                operator: BinaryOperator::Equal(_) | BinaryOperator::NotEqual(_),
                rhs,
            }) => self.steps(lhs, names, steps) && self.steps(rhs, names, steps),
            _ => false,
        }
    }

    /// The steps of `Class.m()` or `object.m()`: the receiver's, then the arguments', then the call. `Position.current()`
    /// is the position in the library's method, which a copy in the caller would not give. A private method is the
    /// library class's own, which a copy in another class would not reach, unless it is an `extern` method of the same
    /// class, whose call the form runs as its native call.
    fn method_call_steps(
        &self,
        expression: &Expression,
        call: &MethodCall,
        names: &[&[u8]],
        steps: &mut Vec<Step>,
    ) -> bool {
        match self.names.static_call_class(call) {
            Some(class) if self.names.is_type_parameter(&class.name) || self.names.is_current_position(call) => {
                return false;
            }
            Some(_) => {}
            None if self.takes_receiver(call.object) && self.steps(call.object, names, steps) => {}
            None => return false,
        }
        let declaration = self.types.call_target(expression);

        (declaration.public || self.types.inline_form(&declaration).is_some())
            && matches!(declaration.kind, DeclarationKind::Method { .. } | DeclarationKind::StaticMethod)
            && self.argument_steps(&call.argument_list, names, steps)
            && self.call_step(steps)
    }

    /// Whether the inlining rule takes `object` as a call's receiver: a value of classes. `Self` and `super`, which the
    /// analysis gives no type, name another class once the form is copied, and a class value is no value of classes.
    fn takes_receiver(&self, object: &Expression) -> bool {
        !matches!(object, Expression::Parent(_) | Expression::Self_(_))
            && receiver_classes(self.types.expression_type(object)).is_some()
    }

    fn argument_steps(&self, list: &ArgumentList, names: &[&[u8]], steps: &mut Vec<Step>) -> bool {
        list.arguments.iter().all(|argument| match argument {
            Argument::Positional(argument) => argument.ellipsis.is_none() && self.steps(argument.value, names, steps),
            Argument::Named(argument) => self.steps(argument.value, names, steps),
        })
    }

    fn call_step(&self, steps: &mut Vec<Step>) -> bool {
        steps.push(Step::Call);

        true
    }

    /// The call `call` as its standard library method's inline form, when the method has one and the call passes
    /// each parameter one positional argument or leaves out one whose default is a literal. A slot the form reads out of order, or after a call, takes only a value
    /// that does nothing when read, since inlined it runs in the form's order instead of the call's.
    pub(super) fn inlined_call(&mut self, expression: &Expression, call: &MethodCall) -> Option<u32> {
        // A type parameter's class is a subclass of its bound when the code runs, which may declare its own method.
        let receiver = match self.names.static_call_class(call) {
            Some(class) if self.names.is_type_parameter(&class.name) => return None,
            Some(_) => None,
            None if self.takes_receiver(call.object) => Some(call.object),
            None => return None,
        };
        let declaration = self.types.call_target(expression);

        self.inlined(
            expression,
            &declaration,
            receiver.map(InlineReceiver::Expression),
            Some(&call.argument_list),
            None,
            None,
        )
    }

    /// The call `call` of `declaration` on `receiver`, its first slot, with `arguments`, as the method's inline form on
    /// the call's line, under the rule [`Self::inlined_call`] states. A form that calls an engine function whose
    /// parameter takes what the call gives, [`TAKES_RESULT_TYPE`] or [`TAKES_KEY_ORDER`], gets it from `call`, or from
    /// `member`, where the call runs it of several. Each argument goes in as an element of the type `element` does,
    /// where the receiver is a collection plain PHP declares.
    pub(super) fn inlined(
        &mut self,
        call: &Expression,
        declaration: &Declaration,
        receiver: Option<InlineReceiver>,
        arguments: Option<&ArgumentList>,
        element: Option<&TUnion>,
        member: Option<&MemberCall>,
    ) -> Option<u32> {
        let line = self.line(call);
        let receivers = usize::from(receiver.is_some());
        let mut values: Vec<InlineReceiver> = receiver.into_iter().collect();
        for argument in arguments.into_iter().flat_map(|list| list.arguments.iter()) {
            let Argument::Positional(argument) = argument else {
                return None;
            };
            if argument.ellipsis.is_some() {
                return None;
            }

            values.push(InlineReceiver::Expression(argument.value));
        }
        let form = self.types.inline_form_passing(declaration, values.len())?;
        // A form that fixes an argument folded the branch its literal takes, which another value may not take.
        let fixes = form.fixed.iter().all(|fixed| {
            matches!(values.get(fixed.slot), Some(InlineReceiver::Expression(argument)) if fixed.holds(argument))
        });
        if !fixes {
            return None;
        }
        // An argument the call leaves out is the parameter's default, which the form holds when it is a literal.
        let defaults: Vec<u32> =
            form.defaults.get(values.len()..).unwrap_or_default().iter().copied().collect::<Option<_>>()?;
        for default in defaults {
            values.push(InlineReceiver::Place(self.copy(form, default, &[], line)));
        }
        // A place is lowered already, and reading it runs nothing.
        let runs_code =
            |value: &InlineReceiver| matches!(value, InlineReceiver::Expression(value) if !self.is_pure(value));
        if values.len() != form.slots.len() || values.iter().skip(form.in_order).any(runs_code) {
            return None;
        }

        let mut values: Vec<u32> = values
            .into_iter()
            .enumerate()
            .map(|(position, value)| match value {
                InlineReceiver::Expression(value) if position >= receivers => self.member_argument(value, element),
                InlineReceiver::Expression(value) => self.read_receiver(value),
                InlineReceiver::Place(place) => place,
            })
            .collect();
        let result_type = form.result_type.map(|_| self.result_type_argument(call, member, line));
        let key_order = form.key_order.map(|_| self.key_order_argument(call, line));
        values.extend(result_type.into_iter().chain(key_order));

        Some(self.copied(declaration, form, &values, line))
    }

    /// The member `declaration` an operation runs, as `list[i] = v` runs a `List`'s `set`, with `values` lowered
    /// already in its slots, in order, as the member's inline form on `line`. None when the member has no form, or one
    /// that reads its slots out of order, takes what a call gives, or fixes an argument, which a lowered value is not.
    pub(super) fn inlined_operation(&mut self, declaration: &Declaration, values: &[u32], line: u32) -> Option<u32> {
        let form = self.types.inline_form(declaration)?;
        if values.len() != form.slots.len()
            || form.in_order != form.slots.len()
            || form.result_type.is_some()
            || form.key_order.is_some()
            || !form.fixed.is_empty()
        {
            return None;
        }

        Some(self.copied(declaration, form, values, line))
    }

    /// `form`, the inline form of `declaration`, with `values` in its slots on `line`, which the unit names among the
    /// forms it inlines.
    fn copied(&mut self, declaration: &Declaration, form: &InlineForm, values: &[u32], line: u32) -> u32 {
        self.inlined.push(Read {
            name: key(declaration.class.as_bytes(), declaration.name.as_bytes()),
            fingerprint: form.fingerprint(),
        });

        self.copy(form, form.root, values, line)
    }

    /// The type arguments of the `Map` or the `Set` `call` gives, or `member` gives, where the call runs it of several,
    /// as the engine function's [`TAKES_RESULT_TYPE`] parameter takes them.
    pub(super) fn result_type_argument(&mut self, call: &Expression, member: Option<&MemberCall>, line: u32) -> u32 {
        let Some(text) = self.table_type_arguments(self.types.call_type(call, member)) else {
            unreachable!("the checker refuses a type argument PHP# can't write, as `never` of `map(x => throw …)`");
        };

        self.type_text(line, &text)
    }

    /// The `operator <=>` the checker chose for the keys `call` orders by, spec section 19, as the engine function's
    /// [`TAKES_KEY_ORDER`] parameter takes it: its first-class callable, the list of each element's for a tuple key,
    /// spec section 12, or null for keys of a type that declares none.
    pub(super) fn key_order_argument(&mut self, call: &Expression, line: u32) -> u32 {
        let order = self.types.key_order(call);

        self.key_order_value(&order, line)
    }

    /// `order` as the engine function's [`TAKES_KEY_ORDER`] parameter takes it, on `line`.
    fn key_order_value(&mut self, order: &KeyOrder, line: u32) -> u32 {
        match order {
            KeyOrder::Operator(owner, name) => {
                let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

                self.operator_method(line, *owner, name, callable)
            }
            KeyOrder::Elements(orders) => {
                let elements: Vec<u32> = orders
                    .iter()
                    .map(|order| {
                        let value = self.key_order_value(order, line);

                        self.node(SHARP_AST_ARRAY_ELEM, 0, line, &[value, NULL])
                    })
                    .collect();

                self.node(SHARP_AST_ARRAY, ZEND_ARRAY_SYNTAX_SHORT, line, &elements)
            }
            KeyOrder::Plain => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
        }
    }

    /// Whether reading `expression` runs nothing: a literal, a collection literal of such values, a name, a plain PHP
    /// constant read through its namespace, a class's constant or enum case, a lambda, `typeof(X)` or `nameof(X.y)`, or
    /// a value a hidden variable holds already, which reads the variable.
    pub(super) fn is_pure(&self, expression: &Expression) -> bool {
        if self.held_receivers.iter().any(|(span, _)| *span == expression.span()) {
            return true;
        }
        match expression {
            Expression::Parenthesized(parenthesized) => self.is_pure(parenthesized.expression),
            Expression::Literal(_)
            | Expression::ArrowFunction(_)
            | Expression::Closure(_)
            | Expression::TypeOf(_)
            | Expression::NameOf(_) => true,
            Expression::ConstantAccess(name) => {
                matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::This | Binding::Constant))
            }
            // A collection literal of values that run nothing runs nothing. A spread reads a sequence, which may run, and
            // the checker refuses an element left out.
            Expression::Array(array) => array.elements.iter().all(|element| match element {
                ArrayElement::KeyValue(element) => self.is_pure(element.key) && self.is_pure(element.value),
                ArrayElement::Value(element) => self.is_pure(element.value),
                ArrayElement::Variadic(_) | ArrayElement::Missing(_) => false,
            }),
            Expression::Access(Access::Property(access)) => {
                self.types.namespace_constant(expression).is_some()
                    || matches!(&access.property, ClassLikeMemberSelector::Identifier(name)
                    if self.static_classes(access).is_some_and(|classes| classes.iter().all(|class| matches!(
                        self.types.member_declaration(class, name.value).kind,
                        DeclarationKind::Constant | DeclarationKind::EnumCase
                    ))))
            }
            _ => false,
        }
    }

    /// `form`'s node `node` and its children, on `line`, with each slot replaced by its value in `values`, followed by
    /// the value of its result type's slot and then of its key order's, as the form has them.
    fn copy(&mut self, form: &InlineForm, node: u32, values: &[u32], line: u32) -> u32 {
        if node == NULL {
            return NULL;
        }
        if let Some(slot) = form.slots.iter().position(|&slot| slot == node) {
            return values[slot];
        }
        let result_type = form.slots.len();
        if form.result_type == Some(node) {
            return values[result_type];
        }
        if form.key_order == Some(node) {
            return values[result_type + usize::from(form.result_type.is_some())];
        }

        let source = form.nodes[node as usize];
        let first = source.first_child as usize;
        let children: Vec<u32> = form.children[first..first + source.child_count as usize]
            .iter()
            .map(|&child| self.copy(form, child, values, line))
            .collect();
        let index = self.node(source.kind, source.attr, line, &children);
        let text = &form.texts[source.text.offset as usize..(source.text.offset + source.text.len) as usize];
        let text = store_text(&mut self.texts, text);
        let copied = &mut self.nodes[index as usize];
        copied.value = source.value;
        copied.long_value = source.long_value;
        copied.double_value = source.double_value;
        copied.text = text;

        index
    }
}
