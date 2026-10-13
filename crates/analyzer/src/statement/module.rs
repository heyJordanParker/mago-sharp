//! Spec section 32: a module binds the services the container builds, and the checker proves the container can build
//! every one. As in .NET's scope validation and Dagger's graph check, it refuses a service nothing binds, a service
//! that needs itself, and a `scoped` service a `singleton` would keep after its scope ends.

use foldhash::HashSet;
use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::BindingMetadata;
use mago_codex::metadata::module::BindingValueMetadata;
use mago_codex::metadata::module::Lifetime;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::metadata::module::ModuleMetadata;
use mago_codex::metadata::parameter::FunctionLikeParameterMetadata;
use mago_codex::reference::ReferenceOrigin;
use mago_codex::ttype::TType;
use mago_codex::ttype::TypeRef;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::comparator::ComparisonResult;
use mago_codex::ttype::comparator::union_comparator;
use mago_codex::ttype::get_named_object;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BindingValue;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::TokenSeparatedSequence;
use mago_word::Word;
use mago_word::ascii_lowercase_word;
use mago_word::empty_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::report_undefined_type;
use crate::statement::function_like::report_undefined_types;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;

/// Checks each binding of `module` where it is written, then walks the services each one needs for a cycle and for a
/// `scoped` service a `singleton` holds.
pub(crate) fn analyze_bindings<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    artifacts: &mut AnalysisArtifacts,
    module: &Module<'arena>,
    metadata: &'ctx ModuleMetadata,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let written = module.members.iter().filter_map(|member| match member {
        ModuleMember::Binding(binding) => Some(binding),
        ModuleMember::Law(_) => None,
    });
    let origin = ReferenceOrigin::File(word(&context.source_file.name));
    let mut bound: Vec<((Word, Option<Word>), Span)> = Vec::new();
    for (binding, binding_metadata) in written.zip(&metadata.bindings) {
        let Some(service) = service_type(&binding_metadata.service.type_union) else {
            report_undefined_types(context, &binding_metadata.service.type_union, binding.service.span());
            continue;
        };

        let key = (service_key(service), binding_metadata.target.map(|target| ascii_lowercase_word(target.as_bytes())));
        if let Some((_, first)) = bound.iter().find(|(earlier, _)| *earlier == key) {
            report_duplicate_binding(context, service, binding_metadata, *first);
        } else {
            bound.push((key, binding_metadata.span));
        }

        if let (Some(target), Some(written_target)) = (binding_metadata.target, &binding.target)
            && !context.codebase.get_class_like(target.as_bytes()).is_some_and(|class| class.kind.is_class())
        {
            report_undefined_type(context, target, written_target.class.span());
        }

        match &binding_metadata.value {
            BindingValueMetadata::Class(class, span) => check_class_value(context, service, *class, *span),
            BindingValueMetadata::Classes(classes) => {
                let Some(element) = list_element(service) else {
                    context.collector.report_with_code(
                        IssueCode::InvalidBinding,
                        Issue::error(format!(
                            "A list of classes binds a `List` service, and `{}` is no `List`.",
                            display_sharp_type(context, service)
                        ))
                        .with_annotation(Annotation::primary(binding.value.span()).with_message("A list of classes."))
                        .with_help("Bind the service to one class, or bind a `List` of it."),
                    );
                    continue;
                };
                for (class, span) in classes {
                    check_class_value(context, element, *class, *span);
                }
            }
            BindingValueMetadata::Expression(lambda) => {
                let BindingValue::Expression(expression) = binding.value else {
                    continue;
                };
                let Some(lambda) = context.codebase.get_closure(lambda) else {
                    continue;
                };
                let empty;
                let parameter_list = match &binding.parameter_list {
                    Some(parameter_list) => parameter_list,
                    None => {
                        empty = FunctionLikeParameterList {
                            left_parenthesis: binding.equals,
                            parameters: TokenSeparatedSequence::empty(),
                            right_parenthesis: binding.equals,
                        };
                        &empty
                    }
                };
                let mut scope = mago_codex::context::ScopeContext::new(origin);
                scope.set_function_like(Some(lambda));
                scope.set_static(true);
                let mut block_context = BlockContext::new(scope, context.settings.register_super_globals);
                analyze_function_like(
                    context,
                    artifacts,
                    &mut block_context,
                    lambda,
                    parameter_list,
                    None,
                    FunctionLikeBody::Expression(expression),
                    None,
                )?;
            }
        }
    }

    // An environment or test module also changes what the base modules' bindings reach, so each base binding it
    // leaves in place is walked again over its services.
    let services = Services::new(context.codebase, metadata);
    let base = context
        .codebase
        .modules
        .iter()
        .filter(|module| metadata.kind != ModuleKind::Base && module.kind == ModuleKind::Base);
    let starts = std::iter::once(metadata)
        .chain(base)
        .flat_map(|module| module.bindings.iter().map(move |binding| (module, binding)))
        .filter(|(module, binding)| {
            std::ptr::eq(*module, metadata)
                || services
                    .binding_for(module.namespace.as_bytes(), &binding.service.type_union, binding.target)
                    .is_some_and(|(_, found)| std::ptr::eq(found, *binding))
        });
    let mut reported: Vec<(IssueCode, *const BindingMetadata)> = Vec::new();
    for (module, binding) in starts {
        let mut walk = Walk::new(&services, binding);
        walk.dependencies_of(Node::Binding(module, binding));
        for class in &walk.classes {
            artifacts.symbol_references.add_reference(origin, (*class, empty_word()), true);
        }
        let mut first = |code: IssueCode, at: &BindingMetadata| {
            let key = (code, std::ptr::from_ref(at));
            !reported.contains(&key) && {
                reported.push(key);
                true
            }
        };
        if let Some((cycle, at)) = walk.cycle
            && first(IssueCode::ServiceCycle, at)
        {
            report_cycle(context, binding, &cycle, at);
        }
        if let Some((captive, at)) = walk.captive
            && first(IssueCode::CaptiveService, at)
        {
            report_captive(context, binding, captive, at);
        }
    }

    Ok(())
}

/// Spec section 32: the container builds a class through its main constructor, so each parameter whose type only a
/// binding can build, an interface or an abstract class, needs a module of the class's namespace, or of a namespace
/// above it, to bind it. The standard library's classes come from the host.
pub(crate) fn check_constructor_services<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata)
where
    A: Arena,
{
    if !class.kind.is_class() || class.flags.is_abstract() || class.flags.is_event() {
        return;
    }
    let Some(constructor) = context.codebase.get_declaring_method(class.name.as_bytes(), b"__construct") else {
        return;
    };

    let services = Services::base(context.codebase);
    for parameter in &constructor.parameters {
        let Some(service) = parameter_service(parameter) else {
            continue;
        };
        if !needs_binding(context.codebase, service)
            || services.binding_for(&namespace_of(class.name), service, Some(class.name)).is_some()
        {
            continue;
        }

        let class_name = display_class_like_name(context, class.original_name);
        let service_name = display_sharp_type(context, service);
        context.collector.report_with_code(
            IssueCode::MissingService,
            Issue::error(format!(
                "`{class_name}` needs a `{service_name}`, and no module binds one: the container can't build `{class_name}`."
            ))
            .with_annotation(Annotation::primary(parameter.span).with_message("Needed here."))
            .with_help(format!(
                "Bind `{service_name}` in a module of `{class_name}`'s namespace or one above it, as in `singleton {service_name} = …;`."
            )),
        );
    }
}

/// The modules one set of services reads: every folder's base module, and the environment or test module whose
/// bindings win over them.
struct Services<'ctx> {
    codebase: &'ctx CodebaseMetadata,
    overrides: Option<&'ctx ModuleMetadata>,
}

impl<'ctx> Services<'ctx> {
    fn base(codebase: &'ctx CodebaseMetadata) -> Self {
        Self { codebase, overrides: None }
    }

    /// The set of services `module` belongs to: the base set for a base module, and the base set under `module`'s own
    /// bindings for an environment or test module.
    fn new(codebase: &'ctx CodebaseMetadata, module: &'ctx ModuleMetadata) -> Self {
        Self { codebase, overrides: (module.kind != ModuleKind::Base).then_some(module) }
    }

    /// The binding that serves `service` to `requester`, a class of `namespace`: the nearest namespace's module first,
    /// an override before its base module, and in one module a binding `for` the requester before one for any class.
    fn binding_for(
        &self,
        namespace: &[u8],
        service: &TUnion,
        requester: Option<Word>,
    ) -> Option<(&'ctx ModuleMetadata, &'ctx BindingMetadata)> {
        let key = service_key(service);
        let mut namespace = namespace;
        loop {
            let modules = self
                .overrides
                .into_iter()
                .chain(self.codebase.modules.iter().filter(|module| module.kind == ModuleKind::Base));
            for module in modules.filter(|module| module.namespace.as_bytes() == namespace) {
                let serves = |binding: &&BindingMetadata| service_key(&binding.service.type_union) == key;
                let targeted = module.bindings.iter().filter(serves).find(|binding| {
                    binding
                        .target
                        .zip(requester)
                        .is_some_and(|(target, requester)| target.as_bytes().eq_ignore_ascii_case(requester.as_bytes()))
                });
                if let Some(binding) =
                    targeted.or_else(|| module.bindings.iter().filter(serves).find(|binding| binding.target.is_none()))
                {
                    return Some((module, binding));
                }
            }

            if namespace.is_empty() {
                return None;
            }
            namespace = namespace.iter().rposition(|byte| *byte == b'\\').map_or(&[], |end| &namespace[..end]);
        }
    }
}

/// A service the walk reaches: a binding, or a concrete class nothing binds, which the container builds through its
/// main constructor every time.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Node<'ctx> {
    Binding(&'ctx ModuleMetadata, &'ctx BindingMetadata),
    Class(Word),
}

/// A walk of the services one binding needs, which records every class it reads, the first cycle back to that
/// binding, and the first `scoped` service a `singleton` binding reaches without passing another `singleton`. Each
/// problem carries the binding it is reported at: the start for a base module, and the last binding of the
/// environment or test module on the way for an override, whose walks of base bindings report only what it changed.
struct Walk<'services, 'ctx> {
    services: &'services Services<'ctx>,
    start: &'ctx BindingMetadata,
    path: Vec<Word>,
    bindings: Vec<&'ctx BindingMetadata>,
    seen: HashSet<(Word, bool, bool)>,
    classes: Vec<Word>,
    cycle: Option<(Vec<Word>, &'ctx BindingMetadata)>,
    captive: Option<(&'ctx BindingMetadata, &'ctx BindingMetadata)>,
}

impl<'services, 'ctx> Walk<'services, 'ctx> {
    fn new(services: &'services Services<'ctx>, start: &'ctx BindingMetadata) -> Self {
        Self {
            services,
            start,
            path: Vec::new(),
            bindings: vec![start],
            seen: HashSet::default(),
            classes: Vec::new(),
            cycle: None,
            captive: None,
        }
    }

    /// Visits each service `node` needs. A `singleton` start binding holds what it reaches through transient
    /// services and unbound classes.
    fn dependencies_of(&mut self, node: Node<'ctx>) {
        let held = self.start.lifetime == Lifetime::Singleton;
        self.visit_parameters_of(node, held);
    }

    /// The binding a problem the walk finds now is reported at, after `reached` when it reaches one.
    fn reported_at(&self, reached: Option<&'ctx BindingMetadata>) -> Option<&'ctx BindingMetadata> {
        let Some(overrides) = self.services.overrides else {
            return Some(self.start);
        };

        reached
            .into_iter()
            .chain(self.bindings.iter().rev().copied())
            .find(|binding| overrides.bindings.iter().any(|own| std::ptr::eq(own, *binding)))
    }

    /// Visits the services each class `node` builds takes, with that class on the path a cycle names.
    fn visit_parameters_of(&mut self, node: Node<'ctx>, held: bool) {
        for (namespace, requester, parameters) in self.parameters_of(node) {
            self.path.extend(requester);
            for parameter in parameters {
                if let Some(service) = parameter_service(parameter) {
                    self.visit(&namespace, requester, service, held);
                }
            }
            if requester.is_some() {
                self.path.pop();
            }
        }
    }

    fn visit(&mut self, namespace: &[u8], requester: Option<Word>, service: &TUnion, held: bool) {
        let node = match self.services.binding_for(namespace, service, requester) {
            Some((module, binding)) => Node::Binding(module, binding),
            None => match single_class(self.services.codebase, service) {
                Some(class) if is_buildable(class) => Node::Class(class.name),
                _ => return,
            },
        };

        if let Node::Binding(_, binding) = node {
            if std::ptr::eq(binding, self.start) {
                if self.cycle.is_none()
                    && let Some(at) = self.reported_at(None)
                {
                    self.cycle = Some((self.path.clone(), at));
                }
                return;
            }
            if held
                && binding.lifetime == Lifetime::Scoped
                && self.captive.is_none()
                && let Some(at) = self.reported_at(Some(binding))
            {
                self.captive = Some((binding, at));
            }
        }

        let name = match node {
            Node::Binding(..) => service_key(service),
            Node::Class(class) => class,
        };
        // A singleton past the start keeps its own services, and its own walk checks them.
        let held = held && !matches!(node, Node::Binding(_, binding) if binding.lifetime == Lifetime::Singleton);
        if self.seen.insert((name, held, self.reported_at(None).is_some())) {
            if let Node::Binding(_, binding) = node {
                self.bindings.push(binding);
            }
            self.visit_parameters_of(node, held);
            if let Node::Binding(..) = node {
                self.bindings.pop();
            }
        }
    }

    /// The parameters the container fills to build `node`, each list with the namespace its services resolve from
    /// and the class they serve: a class's main constructor, or a binding expression's own parameters, which resolve
    /// from the module that holds the binding.
    fn parameters_of(
        &mut self,
        node: Node<'ctx>,
    ) -> Vec<(Vec<u8>, Option<Word>, &'ctx [FunctionLikeParameterMetadata])> {
        let codebase = self.services.codebase;
        let classes: Vec<Word> = match node {
            Node::Class(class) => vec![class],
            Node::Binding(_, BindingMetadata { value: BindingValueMetadata::Class(class, _), .. }) => vec![*class],
            Node::Binding(_, BindingMetadata { value: BindingValueMetadata::Classes(classes), .. }) => {
                classes.iter().map(|(class, _)| *class).collect()
            }
            Node::Binding(module, BindingMetadata { value: BindingValueMetadata::Expression(lambda), .. }) => {
                return codebase
                    .get_closure(lambda)
                    .map(|lambda| vec![(module.namespace.as_bytes().to_vec(), None, lambda.parameters.as_slice())])
                    .unwrap_or_default();
            }
        };

        classes
            .into_iter()
            .filter_map(|class| {
                let metadata = codebase.get_class_like(class.as_bytes())?;
                self.classes.push(metadata.name);
                let constructor = codebase.get_declaring_method(metadata.name.as_bytes(), b"__construct")?;
                Some((namespace_of(metadata.name).to_vec(), Some(metadata.name), constructor.parameters.as_slice()))
            })
            .collect()
    }
}

/// Spec section 32: a class value is a concrete class of the service, built through its main constructor.
fn check_class_value<A>(context: &mut Context<'_, '_, A>, service: &TUnion, class: Word, span: Span)
where
    A: Arena,
{
    let Some(metadata) = context.codebase.get_class_like(class.as_bytes()) else {
        report_undefined_type(context, class, span);
        return;
    };

    let class_name = display_class_like_name(context, metadata.original_name);
    let service_name = display_sharp_type(context, service);
    if !is_buildable(metadata) {
        let kind = if metadata.kind.is_interface() {
            "an interface"
        } else if metadata.kind.is_enum() {
            "an enum"
        } else {
            "an abstract class"
        };
        context.collector.report_with_code(
            IssueCode::InvalidBinding,
            Issue::error(format!("`{class_name}` is {kind}, so the container can't build it."))
                .with_annotation(Annotation::primary(span).with_message("Bound here."))
                .with_help(format!("Bind `{service_name}` to a class that the container can build.")),
        );
        return;
    }

    let value = get_named_object(metadata.name, None);
    let mut comparison = ComparisonResult::with_sharp_rules(true);
    if !union_comparator::is_contained_by(context.codebase, &value, service, false, false, false, &mut comparison) {
        context.collector.report_with_code(
            IssueCode::InvalidBinding,
            Issue::error(format!("`{class_name}` is not a `{service_name}`."))
                .with_annotation(Annotation::primary(span).with_message("Bound here."))
                .with_help(format!("Bind `{service_name}` to a class that implements it.")),
        );
    }
}

fn report_duplicate_binding<A>(
    context: &mut Context<'_, '_, A>,
    service: &TUnion,
    binding: &BindingMetadata,
    first: Span,
) where
    A: Arena,
{
    let service_name = display_sharp_type(context, service);
    let target =
        binding.target.map(|target| format!(" for `{}`", display_class_like_name(context, target))).unwrap_or_default();
    context.collector.report_with_code(
        IssueCode::InvalidBinding,
        Issue::error(format!("This module already binds `{service_name}`{target}: bind each service once."))
            .with_annotation(Annotation::primary(binding.span).with_message("Bound again here."))
            .with_annotation(Annotation::secondary(first).with_message("First bound here.")),
    );
}

/// Reports that `binding` needs itself through the classes of `cycle`, at the binding `at`.
fn report_cycle<A>(context: &mut Context<'_, '_, A>, binding: &BindingMetadata, cycle: &[Word], at: &BindingMetadata)
where
    A: Arena,
{
    let service_name = display_sharp_type(context, &binding.service.type_union);
    let through = cycle
        .iter()
        .map(|name| match context.codebase.get_class_like(name.as_bytes()) {
            Some(class) => format!("`{}`", display_class_like_name(context, class.original_name)),
            None => format!("`{name}`"),
        })
        .collect::<Vec<_>>()
        .join(", then ");
    let path = if through.is_empty() { String::new() } else { format!(" through {through}") };
    context.collector.report_with_code(
        IssueCode::ServiceCycle,
        Issue::error(format!("`{service_name}` needs itself{path}, so the container can never build it."))
            .with_annotation(Annotation::primary(at.span).with_message("Bound here."))
            .with_help("Break the cycle: one of these services must not take the next one."),
    );
}

/// Reports that the singleton `binding` holds the scoped `captive`, at the binding `at`.
fn report_captive<A>(
    context: &mut Context<'_, '_, A>,
    binding: &BindingMetadata,
    captive: &BindingMetadata,
    at: &BindingMetadata,
) where
    A: Arena,
{
    let service_name = display_sharp_type(context, &binding.service.type_union);
    let scoped_name = display_sharp_type(context, &captive.service.type_union);
    let mut issue = Issue::error(format!(
        "The singleton `{service_name}` holds the scoped `{scoped_name}`, which would outlive the scope it belongs to."
    ))
    .with_annotation(Annotation::primary(at.span).with_message("Bound here."))
    .with_help(format!("Make `{service_name}` scoped too, or make `{scoped_name}` a singleton."));
    for (shown, message) in [(binding, "A singleton."), (captive, "Scoped here.")] {
        if !std::ptr::eq(shown, at) {
            issue = issue.with_annotation(Annotation::secondary(shown.span).with_message(message));
        }
    }

    context.collector.report_with_code(IssueCode::CaptiveService, issue);
}

/// Whether `function_like` is the lambda a binding's expression runs as.
pub(crate) fn is_binding_value(codebase: &CodebaseMetadata, function_like: &FunctionLikeMetadata) -> bool {
    function_like.kind.is_closure()
        && codebase.modules.iter().flat_map(|module| &module.bindings).any(
            |binding| matches!(binding.value, BindingValueMetadata::Expression(lambda) if lambda == function_like.name),
        )
}

/// A populated type, or nothing when it names a class no file declares.
fn service_type(written: &TUnion) -> Option<&TUnion> {
    written
        .get_all_child_nodes()
        .iter()
        .all(|node| !matches!(node, TypeRef::Atomic(TAtomic::Reference(_))))
        .then_some(written)
}

/// The type a constructor or binding parameter needs.
fn parameter_service(parameter: &FunctionLikeParameterMetadata) -> Option<&TUnion> {
    service_type(&parameter.type_metadata.as_ref()?.type_union)
}

/// What names one service: its type, in lowercase, so `Mailer` and `List<PriceRule>` each name one.
fn service_key(service: &TUnion) -> Word {
    ascii_lowercase_word(service.get_id().as_bytes())
}

/// The element type of a `List` service.
fn list_element(service: &TUnion) -> Option<&TUnion> {
    match service.types.as_ref() {
        [TAtomic::Array(TArray::List(list))] => Some(&list.element_type),
        _ => None,
    }
}

/// Whether only a binding can build `service`: an interface or abstract class of the app, or a `List` of one.
fn needs_binding(codebase: &CodebaseMetadata, service: &TUnion) -> bool {
    if let Some(element) = list_element(service) {
        return needs_binding(codebase, element);
    }

    single_class(codebase, service).is_some_and(|class| !is_buildable(class) && !is_standard_library(class))
}

fn single_class<'ctx>(codebase: &'ctx CodebaseMetadata, service: &TUnion) -> Option<&'ctx ClassLikeMetadata> {
    match service.types.as_ref() {
        [TAtomic::Object(TObject::Named(object))] => codebase.get_class_like(object.name.as_bytes()),
        _ => None,
    }
}

/// Whether the container can build `class` through its main constructor: a class that is not abstract.
fn is_buildable(class: &ClassLikeMetadata) -> bool {
    class.kind.is_class() && !class.flags.is_abstract()
}

/// Whether the standard library declares `class`, whose services, such as `Environment`, the host gives.
fn is_standard_library(class: &ClassLikeMetadata) -> bool {
    class.name.as_bytes().get(..6).is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"sharp\\"))
}

/// The namespace of `class`, in lowercase, as modules name theirs.
fn namespace_of(class: Word) -> Vec<u8> {
    let name = class.as_bytes();
    let namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&[][..], |end| &name[..end]);

    namespace.to_ascii_lowercase()
}
