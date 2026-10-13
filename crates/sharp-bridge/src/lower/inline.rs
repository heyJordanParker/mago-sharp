//! Inline forms: a standard library method whose body is one expression runs at its call as that expression, with
//! the call's receiver and arguments in the expression's slots, the way a compiler inlines a one-line method. An
//! `extern` method's body is the call of its native function, so its call runs as that call.
//!
//! The orchestrator takes forms only from the standard library's files, those `File::is_standard_library` marks, and
//! lowers each of them with an empty [`InlineForms`], so in v1 one library form never inlines another.

use std::collections::HashMap;

use mago_names::binding::Binding;
use mago_syntax::cst::Argument;
use mago_syntax::cst::ArgumentList;
use mago_syntax::cst::Call;
use mago_syntax::cst::ClassLikeMember;
use mago_syntax::cst::Expression;
use mago_syntax::cst::FunctionCall;
use mago_syntax::cst::Method;
use mago_syntax::cst::MethodBody;
use mago_syntax::cst::MethodCall;
use mago_syntax::cst::Modifier;
use mago_syntax::cst::Node;
use mago_syntax::cst::Return;
use mago_syntax::cst::Statement;

use super::Lines;
use super::Lowering;
use super::NULL;
use super::ZEND_NAME_FQ;
use super::is_current_position;
use super::set_parameter;
use super::types::DeclarationKind;
use super::types::receiver_classes;
use crate::lower::checked::CheckedProgram;
use crate::sharp_kind::SHARP_AST_CONST;
use crate::sharp_kind::SHARP_AST_VAR;
use crate::sharp_node;
use crate::store_text;
use crate::unit::Read;
use crate::unit::form_fingerprint;

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

/// The inline form of each method of `checked` the inlining rule takes, with its name.
///
/// A method's body is one expression built only from calls, literals and reads of its receiver and parameters, each
/// read exactly once, or it is its native function's call.
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
        for member in &class.members {
            if let ClassLikeMember::Method(method) = member
                && let Some(form) = Lowering::new(&lines, checked.names(), checked.types()).form(name, method)
            {
                forms.push((key(name, method.name.value), form));
            }
        }
    }

    forms
}

/// What a form's body runs, in the order it runs it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A read of the slot at this index.
    Read(usize),
    Call,
}

impl Lowering<'_, '_> {
    /// The form of `class`'s method `method`, lowered into this empty lowering, when the inlining rule takes it.
    ///
    /// The body is one expression whose steps the rule takes, or, for an `extern` method, the call of its native
    /// function. `None` for an expression means the native call.
    fn form(mut self, class: &[u8], method: &Method) -> Option<InlineForm> {
        let expression = match &method.body {
            MethodBody::Expression(body) if method.returns_value() => Some(body.expression),
            MethodBody::Concrete(block) => {
                let mut statements = block.statements.iter();
                match (statements.next(), statements.next()) {
                    (Some(Statement::Return(Return { value: Some(value), .. })), None) => Some(*value),
                    _ => return None,
                }
            }
            MethodBody::Abstract(_)
                if method.returns_value()
                    && method.modifiers.iter().any(|modifier| matches!(modifier, Modifier::Extern(_))) =>
            {
                None
            }
            _ => return None,
        };
        // A form runs without the method's statements before its body, which make each `Set` parameter a `Set`.
        let parameters = &method.parameter_list.parameters;
        if parameters.iter().any(|parameter| parameter.is_variadic() || set_parameter(parameter).is_some()) {
            return None;
        }

        let declaration = self.types.member_declaration(class, method.name.value);
        let receiver = match declaration.kind {
            DeclarationKind::Method { overridable: false } => true,
            DeclarationKind::StaticMethod => false,
            _ => return None,
        };
        if !declaration.public {
            return None;
        }

        let names: Vec<&[u8]> = receiver
            .then_some(b"this".as_slice())
            .into_iter()
            .chain(parameters.iter().map(|parameter| parameter.variable.name))
            .collect();
        let (root, in_order) = match expression {
            Some(expression) => {
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
            None => (self.native_call(method, class), true),
        };
        let mut slots = vec![NULL; names.len()];
        for (index, node) in self.nodes.iter().enumerate() {
            if node.kind == SHARP_AST_VAR {
                let name = self.nodes[self.children[node.first_child as usize] as usize].text;
                let name = &self.texts[name.offset as usize..(name.offset + name.len) as usize];
                let slot = names.iter().position(|slot| *slot == name).unwrap_or_else(|| {
                    unreachable!("a form reads only its slots, not `{}`", String::from_utf8_lossy(name))
                });
                slots[slot] = index as u32;
            }
        }
        // A form leaves the library file's namespace when it is copied, so each global constant it reads is named in
        // full, where the caller's own namespace would be looked up first.
        let constants: Vec<usize> = self
            .nodes
            .iter()
            .filter(|node| node.kind == SHARP_AST_CONST)
            .map(|node| self.children[node.first_child as usize] as usize)
            .collect();
        for constant in constants {
            self.nodes[constant].attr = ZEND_NAME_FQ;
        }
        for node in &mut self.nodes {
            node.line = 0;
        }

        Some(InlineForm { nodes: self.nodes, children: self.children, texts: self.texts, root, slots, in_order })
    }

    /// Appends what `expression` runs to `steps`, and whether the inlining rule takes all of it: a literal, a read of
    /// one of the slots `names` names, and a call of a global function or a method whose receiver it takes.
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
            Expression::ConstantAccess(name) if self.names.binding(&name.name) == Some(Binding::Constant) => {
                !self.types.constant_target(name).as_bytes().contains(&b'\\')
            }
            Expression::Call(Call::Function(FunctionCall {
                function: Expression::Identifier(function),
                argument_list,
            })) if !matches!(self.names.binding(function), Some(Binding::Local(_))) => {
                self.argument_steps(argument_list, names, steps) && self.call_step(steps)
            }
            Expression::Call(Call::Method(call)) => self.method_call_steps(expression, call, names, steps),
            _ => false,
        }
    }

    /// The steps of `Class.m()` or `object.m()`: the receiver's, then the arguments', then the call. `Position.current()`
    /// is the position in the library's method, which a copy in the caller would not give.
    fn method_call_steps(
        &self,
        expression: &Expression,
        call: &MethodCall,
        names: &[&[u8]],
        steps: &mut Vec<Step>,
    ) -> bool {
        match self.names.static_call_class(call) {
            Some(class) if is_current_position(self.names.get(&class.name), call) => return false,
            Some(_) => {}
            None if self.takes_receiver(call.object) && self.steps(call.object, names, steps) => {}
            None => return false,
        }
        let declaration = self.types.call_target(expression);

        declaration.public
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
        let receiver = match self.names.static_call_class(call) {
            Some(_) => None,
            None if self.takes_receiver(call.object) => Some(call.object),
            None => return None,
        };
        let declaration = self.types.call_target(expression);
        let form = self.types.inline_form(&declaration)?;
        let mut values: Vec<&Expression> = receiver.into_iter().collect();
        for argument in &call.argument_list.arguments {
            let Argument::Positional(argument) = argument else {
                return None;
            };
            if argument.ellipsis.is_some() {
                return None;
            }

            values.push(argument.value);
        }
        if values.len() != form.slots.len() || !(form.in_order || values.iter().all(|value| self.is_pure(value))) {
            return None;
        }

        let line = self.line(call);
        let values: Vec<u32> = values.into_iter().map(|value| self.expression(value)).collect();
        self.inlined.push(Read {
            name: key(declaration.class.as_bytes(), declaration.name.as_bytes()),
            fingerprint: form.fingerprint(),
        });

        Some(self.copy(form, form.root, &values, line))
    }

    /// Whether reading `expression` runs nothing: a literal, a name, a lambda or `typeof(X)`.
    pub(super) fn is_pure(&self, expression: &Expression) -> bool {
        match expression {
            Expression::Parenthesized(parenthesized) => self.is_pure(parenthesized.expression),
            Expression::Literal(_) | Expression::ArrowFunction(_) | Expression::Closure(_) | Expression::TypeOf(_) => {
                true
            }
            Expression::ConstantAccess(name) => {
                matches!(self.names.binding(&name.name), Some(Binding::Local(_) | Binding::This | Binding::Constant))
            }
            _ => false,
        }
    }

    /// `form`'s node `node` and its children, on `line`, with each slot replaced by its value in `values`.
    fn copy(&mut self, form: &InlineForm, node: u32, values: &[u32], line: u32) -> u32 {
        if node == NULL {
            return NULL;
        }
        if let Some(slot) = form.slots.iter().position(|&slot| slot == node) {
            return values[slot];
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
