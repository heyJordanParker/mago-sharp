//! Inline forms: a standard library method whose body is one expression runs at its call as that expression, with
//! the call's receiver and arguments in the expression's slots, the way a compiler inlines a one-line method. An
//! `extern` method's body is the call of its native function, so its call runs as that call.
//!
//! The orchestrator takes forms only from the standard library's files, those `File::is_standard_library` marks, and
//! lowers each of them with an empty [`InlineForms`], so in v1 one library form inlines no other, but the native call
//! of an `extern` method of its own class.

use std::collections::HashMap;

use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_names::binding::php_operator_name;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Binary;
use mago_syntax::cst::BinaryOperator;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionLikeParameter;
use mago_syntax::cst::Hint;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Property;
use mago_syntax::cst::Return;
use mago_syntax::cst::Statement;
use mago_syntax::cst::UnaryPrefix;
use mago_syntax::cst::UnaryPrefixOperator;

use super::Lines;
use super::Lowering;
use super::NULL;
use super::types::Declaration;
use super::types::DeclarationKind;
use super::types::receiver_classes;
use crate::lower::checked::CheckedProgram;
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
#[derive(Debug, Clone)]
pub struct InlineForm {
    nodes: Vec<sharp_node>,
    children: Vec<u32>,
    texts: Vec<u8>,
    root: u32,
    /// The node each value replaces: the receiver first for an instance method, then each parameter in declaration
    /// order.
    slots: Vec<u32>,
    /// Whether the form reads every slot in declaration order, the receiver first, before its first call, so the
    /// values run in the order the call runs them.
    in_order: bool,
    /// The node the type arguments of the collection the inlined call gives replace, [`RESULT_TYPE_SLOT`].
    result_type: Option<u32>,
    /// The node the `operator <=>` of the keys the inlined call orders by replaces, [`KEY_ORDER_SLOT`].
    key_order: Option<u32>,
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
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        form_fingerprint(&self.nodes, &self.children, &self.texts)
    }
}

/// The inline forms a file's lowering may inline, by the name [`key`] gives the method each runs.
#[derive(Debug, Default)]
pub struct InlineForms(HashMap<Vec<u8>, InlineForm>);

impl InlineForms {
    pub(crate) fn get(&self, name: &[u8]) -> Option<&InlineForm> {
        self.0.get(name)
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

/// The name of `class`'s method `method`, as `sharp\text::shout`: both lowercase, as PHP matches them.
pub(crate) fn key(class: &[u8], method: &[u8]) -> Vec<u8> {
    [class.to_ascii_lowercase().as_slice(), b"::", method.to_ascii_lowercase().as_slice()].concat()
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
                        .method_form(name, method, None)
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
                    forms.extend(
                        lowering()
                            .method_form(name, method, None)
                            .filter(|_| public(method.name.value))
                            .map(|form| (key(name, method.name.value), form)),
                    );
                }
                ClassLikeMember::Extension(extension) => {
                    let [receiver] = extension.parameter_list.parameters.as_slice() else {
                        unreachable!("semantics refuses an extension without exactly one receiver");
                    };
                    for member in &extension.members {
                        let (member, form) = match member {
                            ClassLikeMember::Method(method) => {
                                (method.name.value, lowering().method_form(name, method, Some(receiver)))
                            }
                            ClassLikeMember::Property(Property::Computed(property)) => (
                                property.variable.name,
                                lowering().form(
                                    name,
                                    property.variable.name,
                                    &[receiver],
                                    Body::Expression(property.body.expression),
                                    true,
                                ),
                            ),
                            _ => unreachable!("semantics refuses the extension member `{member}`"),
                        };
                        forms.extend(form.filter(|_| public(member)).map(|form| (key(name, member), form)));
                    }
                }
                _ => {}
            }
        }
    }

    forms
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
    fn method_form(
        self,
        class: &[u8],
        method: &Method,
        receiver: Option<&FunctionLikeParameter>,
    ) -> Option<InlineForm> {
        let is_void = method.return_type_hint.as_ref().is_some_and(|hint| matches!(hint.hint, Hint::Void(_)));
        let body = match &method.body {
            MethodBody::Expression(body) if method.returns_value() || is_void => Body::Expression(body.expression),
            MethodBody::Concrete(block) => {
                let mut statements = block.statements.iter();
                match (statements.next(), statements.next()) {
                    (Some(Statement::Return(Return { value: Some(value), .. })), None) => Body::Expression(value),
                    _ => return None,
                }
            }
            MethodBody::Abstract(_)
                if method.returns_value()
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
            {
                Body::Native(method)
            }
            _ => return None,
        };
        let parameters: Vec<&FunctionLikeParameter> =
            receiver.into_iter().chain(&method.parameter_list.parameters).collect();

        self.form(class, method.name.value, &parameters, body, receiver.is_some())
    }

    /// The form of `class`'s member `name`, which takes `parameters`, the receiver first for an `extension` member, and
    /// runs `body`, lowered into this empty lowering, when the inlining rule takes it: the body's steps are ones the
    /// rule takes.
    fn form(
        mut self,
        class: &[u8],
        name: &[u8],
        parameters: &[&FunctionLikeParameter],
        body: Body,
        extension: bool,
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
        let (root, in_order) = match body {
            Body::Expression(expression) => {
                let mut steps = Vec::new();
                if !self.steps(expression, &names, &mut steps) {
                    return None;
                }

                let reads: Vec<usize> = steps
                    .iter()
                    .filter_map(|step| match step {
                        Step::Read(slot) => Some(*slot),
                        Step::Call => None,
                    })
                    .collect();
                let mut each_once = reads.clone();
                each_once.sort_unstable();
                if each_once != (0..names.len()).collect::<Vec<_>>() {
                    return None;
                }

                let in_order =
                    reads == each_once && steps[..reads.len()].iter().all(|step| matches!(step, Step::Read(_)));
                (self.expression(expression), in_order)
            }
            // The native call reads each parameter once, in order, and then calls.
            Body::Native(method) => (self.native_call(method, class), true),
        };
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
            in_order,
            result_type,
            key_order,
        })
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
    /// each parameter one positional argument. A form that reads its slots out of order, or after a call, takes only
    /// values that do nothing when read, since inlined they run in the form's order instead of the call's.
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
        )
    }

    /// The call `call` of `declaration` on `receiver`, its first slot, with `arguments`, as the method's inline form on
    /// the call's line, under the rule [`Self::inlined_call`] states. A form that calls an engine function whose
    /// parameter takes what the call gives, [`TAKES_RESULT_TYPE`] or [`TAKES_KEY_ORDER`], gets it from `call`. Each
    /// argument goes in as an element of the type `element` does, where the receiver is a collection plain PHP declares.
    pub(super) fn inlined(
        &mut self,
        call: &Expression,
        declaration: &Declaration,
        receiver: Option<InlineReceiver>,
        arguments: Option<&ArgumentList>,
        element: Option<&TUnion>,
    ) -> Option<u32> {
        let form = self.types.inline_form(declaration)?;
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
        // A place is lowered already, and reading it runs nothing.
        let runs_code =
            |value: &InlineReceiver| matches!(value, InlineReceiver::Expression(value) if !self.is_pure(value));
        if values.len() != form.slots.len() || (!form.in_order && values.iter().any(runs_code)) {
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
        let result_type = form.result_type.map(|_| self.result_type_argument(call, line));
        let key_order = form.key_order.map(|_| self.key_order_argument(call, line));
        values.extend(result_type.into_iter().chain(key_order));

        Some(self.copied(declaration, form, &values, line))
    }

    /// The member `declaration` an operation runs, as `list[i] = v` runs a `List`'s `set`, with `values` lowered
    /// already in its slots, in order, as the member's inline form on `line`. None when the member has no form, or one
    /// that reads its slots out of order or takes what a call gives.
    pub(super) fn inlined_operation(&mut self, declaration: &Declaration, values: &[u32], line: u32) -> Option<u32> {
        let form = self.types.inline_form(declaration)?;
        if values.len() != form.slots.len() || !form.in_order || form.result_type.is_some() || form.key_order.is_some()
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

    /// The type arguments of the `Map` or the `Set` `call` gives, as the engine function's [`TAKES_RESULT_TYPE`]
    /// parameter takes them.
    pub(super) fn result_type_argument(&mut self, call: &Expression, line: u32) -> u32 {
        let Some(text) = self.table_type_arguments(self.types.expression_type(call)) else {
            unreachable!("the checker refuses a type argument PHP# can't write, as `never` of `map(x => throw …)`");
        };

        self.type_text(line, &text)
    }

    /// The `operator <=>` the checker chose for the keys `call` orders by, spec section 19, as the engine function's
    /// [`TAKES_KEY_ORDER`] parameter takes it: its first-class callable, or null for keys of a type that declares none.
    pub(super) fn key_order_argument(&mut self, call: &Expression, line: u32) -> u32 {
        match self.types.sorting_operator(call).zip(php_operator_name(&BinaryOperator::Spaceship(Span::zero()), 2)) {
            Some((owner, name)) => {
                let callable = self.node(SHARP_AST_CALLABLE_CONVERT, 0, line, &[]);

                self.operator_method(line, owner, name, callable)
            }
            None => self.zval(line, sharp_value::SHARP_NULL, |_| {}),
        }
    }

    /// Whether reading `expression` runs nothing: a literal, a name, a plain PHP constant read through its namespace, a
    /// lambda, `typeof(X)` or `nameof(X.y)`.
    pub(super) fn is_pure(&self, expression: &Expression) -> bool {
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
            Expression::Access(Access::Property(_)) => self.types.namespace_constant(expression).is_some(),
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
