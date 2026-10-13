//! Spec section 32: a module binds the services the container builds, and PHP# creates only the classes a module
//! binds. The checker proves the container can build every one, whether or not anything takes it, as Guice's
//! `requireExplicitBindings()` does. As in .NET's scope validation and Dagger's graph check, it refuses a service
//! nothing binds, a service that needs itself, and a `scoped` service a `singleton` would keep after its scope ends.

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
use mago_codex::metadata::module::service_id;
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
use crate::utils::names::article;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;
use crate::utils::names::display_variable_name;

/// Checks each binding of `module` where it is written, with every service its class or its expression takes, then
/// walks the services each one needs for a cycle and for a `scoped` service a `singleton` holds.
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
    let services = Services::new(context.codebase, metadata);
    let mut bound: Vec<((Word, Option<Word>), Span)> = Vec::new();
    for (binding, binding_metadata) in written.zip(&metadata.bindings) {
        let Some(service) = service_type(&binding_metadata.service.type_union) else {
            report_undefined_types(context, &binding_metadata.service.type_union, binding.service.span());
            continue;
        };

        let key = (service_id(service), binding_metadata.target.map(|target| ascii_lowercase_word(target.as_bytes())));
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
            BindingValueMetadata::Class(class, span) => {
                if check_class_value(context, service, *class, *span) {
                    check_constructor_services(context, &services, *class, *span);
                }
            }
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
                    if check_class_value(context, element, *class, *span) {
                        check_constructor_services(context, &services, *class, *span);
                    }
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

                let namespace = metadata.namespace.as_bytes();
                for parameter in &lambda.parameters {
                    if let Some(missing) = services.missing(namespace, None, parameter) {
                        report_missing_service(context, Builds::Binding(service), missing, parameter);
                    }
                }
            }
        }
    }

    // An environment or test module also changes what the base modules' bindings reach, so each base binding it
    // leaves in place is walked again over its services.
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
        walk.dependencies_of((module, binding));
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

/// Spec section 32: PHP# creates the classes a module binds, so the bound `class` is built through its main
/// constructor, and each parameter needs a binding in the module of the class's namespace or of a namespace above it.
/// A problem is reported at the binding's class value, `bound`, with a note at the parameter.
fn check_constructor_services<A>(context: &mut Context<'_, '_, A>, services: &Services<'_>, class: Word, bound: Span)
where
    A: Arena,
{
    if let Some(metadata) = context.codebase.get_class_like(class.as_bytes()) {
        check_class_services(context, services, metadata, Builds::Bound(metadata.original_name, bound));
    }
}

/// Spec sections 15 and 32: PHP# also creates the class of a listener, a new object for every event, so each
/// parameter of its main constructor needs a binding as a bound class's does, in the base modules. A problem is
/// reported at the parameter. `event` is the first event the class listens to.
pub(crate) fn check_listener_services<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata, event: Word)
where
    A: Arena,
{
    let services = Services { codebase: context.codebase, overrides: None };

    check_class_services(context, &services, class, Builds::Listener(class.original_name, event));
}

/// Reports each parameter of `class`'s main constructor that `services` can't fill, as `builds` names the class.
fn check_class_services<A>(
    context: &mut Context<'_, '_, A>,
    services: &Services<'_>,
    class: &ClassLikeMetadata,
    builds: Builds<'_>,
) where
    A: Arena,
{
    let Some(constructor) = context.codebase.get_declaring_method(class.name.as_bytes(), b"__construct") else {
        return;
    };

    let namespace = namespace_of(class.name);
    for parameter in &constructor.parameters {
        if let Some(missing) = services.missing(&namespace, Some(class.name), parameter) {
            report_missing_service(context, builds, missing, parameter);
        }
    }
}

/// What the container builds when it fills a parameter: a class a binding binds, with the span of the binding's
/// class value, the service of a binding's expression, or a listener's class, with the first event it listens to.
#[derive(Clone, Copy)]
enum Builds<'ctx> {
    Bound(Word, Span),
    Binding(&'ctx TUnion),
    Listener(Word, Word),
}

/// A parameter the container can't fill: a service no module binds, or a type that is no service, `None` when the
/// parameter has no type.
enum Missing<'ctx> {
    Unbound(&'ctx TUnion),
    NoService(Option<&'ctx TUnion>),
}

/// Reports that the container can't build what `builds` names because it can't fill `parameter`, at a binding's
/// class value or else at the parameter.
fn report_missing_service<A>(
    context: &mut Context<'_, '_, A>,
    builds: Builds<'_>,
    missing: Missing<'_>,
    parameter: &FunctionLikeParameterMetadata,
) where
    A: Arena,
{
    let (name, subject) = match builds {
        Builds::Bound(class, _) => {
            let name = display_class_like_name(context, class).to_string();
            (name.clone(), format!("`{name}`"))
        }
        Builds::Binding(service) => {
            let name = display_sharp_type(context, service);
            (name.clone(), format!("`{name}`"))
        }
        Builds::Listener(class, event) => {
            let name = display_class_like_name(context, class).to_string();
            let subject = format!("`{name}` listens to `{}` and", display_class_like_name(context, event));
            (name, subject)
        }
    };
    let issue = match missing {
        Missing::Unbound(service) => {
            let service_name = display_sharp_type(context, service);
            let example = match single_class(context.codebase, service) {
                Some(class) if is_buildable(class) => format!("transient {service_name} = {service_name};"),
                _ => format!("singleton {service_name} = …;"),
            };
            let place = match builds {
                Builds::Binding(_) => "this namespace".to_owned(),
                Builds::Bound(..) | Builds::Listener(..) => format!("`{name}`'s namespace"),
            };
            Issue::error(format!(
                "{subject} needs {} `{service_name}`, and no module binds one: the container can't build `{name}`.",
                article(&service_name)
            ))
            .with_help(format!("Bind `{service_name}` in a module of {place} or one above it, as in `{example}`."))
        }
        Missing::NoService(written) => {
            let taken = match written {
                Some(written) => {
                    let written = display_sharp_type(context, written);
                    format!("{} `{written}`", article(&written))
                }
                None => format!("`{}` with no type", display_variable_name(context, parameter.name.0)),
            };
            let help = match builds {
                Builds::Bound(..) => format!(
                    "Bind `{name}` to an expression that creates it, as in `singleton {name}(…) = new {name}(…);`."
                ),
                Builds::Binding(_) | Builds::Listener(..) => {
                    "Take a service that gives the value, such as `Environment`, and read the value from it.".to_owned()
                }
            };
            Issue::error(format!(
                "{subject} takes {taken}, which no binding can give: the container can't build `{name}`."
            ))
            .with_help(help)
        }
    };
    let issue = match builds {
        Builds::Bound(_, bound) => issue
            .with_annotation(Annotation::primary(bound).with_message("Bound here."))
            .with_annotation(Annotation::secondary(parameter.span).with_message("Needed here.")),
        Builds::Binding(_) | Builds::Listener(..) => {
            issue.with_annotation(Annotation::primary(parameter.span).with_message("Needed here."))
        }
    };

    context.collector.report_with_code(IssueCode::MissingService, issue);
}

/// The modules one set of services reads: every folder's base module, and the environment or test module whose
/// bindings win over them.
struct Services<'ctx> {
    codebase: &'ctx CodebaseMetadata,
    overrides: Option<&'ctx ModuleMetadata>,
}

impl<'ctx> Services<'ctx> {
    /// Why the container can't fill `parameter` of `requester`, or of a binding of `namespace` when it is `None`, or
    /// nothing when a binding gives it or its type names a class no file declares, which is reported where it is
    /// written.
    fn missing<'parameter>(
        &self,
        namespace: &[u8],
        requester: Option<Word>,
        parameter: &'parameter FunctionLikeParameterMetadata,
    ) -> Option<Missing<'parameter>> {
        let Some(written) = parameter.type_metadata.as_ref().map(|metadata| &metadata.type_union) else {
            return Some(Missing::NoService(None));
        };
        let service = service_type(written)?;
        if !is_service(service) {
            return Some(Missing::NoService(Some(service)));
        }

        self.binding_for(namespace, service, requester).is_none().then_some(Missing::Unbound(service))
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
        let key = service_id(service);
        let mut namespace = namespace;
        loop {
            let modules = self
                .overrides
                .into_iter()
                .chain(self.codebase.modules.iter().filter(|module| module.kind == ModuleKind::Base));
            for module in modules.filter(|module| module.namespace.as_bytes() == namespace) {
                let serves = |binding: &&BindingMetadata| service_id(&binding.service.type_union) == key;
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

/// A service the walk reaches: a binding and the module that holds it.
type Node<'ctx> = (&'ctx ModuleMetadata, &'ctx BindingMetadata);

/// A walk of the services one binding needs, which records every class it reads, the first cycle back to that
/// binding, and the first `scoped` service a `singleton` binding reaches without passing another `singleton`. Each
/// problem carries the binding it is reported at: the start for a base module, and the last binding of the
/// environment or test module on the way for an override, whose walks of base bindings report only what it changed.
struct Walk<'services, 'ctx> {
    services: &'services Services<'ctx>,
    start: &'ctx BindingMetadata,
    path: Vec<Word>,
    bindings: Vec<&'ctx BindingMetadata>,
    seen: HashSet<(*const BindingMetadata, bool, bool)>,
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
    /// services.
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

    /// Visits the binding that serves `service` to `requester`. A service no binding serves is reported at the
    /// binding of the class that takes it, so the walk stops there.
    fn visit(&mut self, namespace: &[u8], requester: Option<Word>, service: &TUnion, held: bool) {
        let Some((module, binding)) = self.services.binding_for(namespace, service, requester) else {
            return;
        };

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

        // A singleton past the start keeps its own services, and its own walk checks them.
        let held = held && binding.lifetime != Lifetime::Singleton;
        if self.seen.insert((std::ptr::from_ref(binding), held, self.reported_at(None).is_some())) {
            self.bindings.push(binding);
            self.visit_parameters_of((module, binding), held);
            self.bindings.pop();
        }
    }

    /// The parameters the container fills to build `node`, each list with the namespace its services resolve from
    /// and the class they serve: a bound class's main constructor, or a binding expression's own parameters, which
    /// resolve from the module that holds the binding.
    fn parameters_of(
        &mut self,
        node: Node<'ctx>,
    ) -> Vec<(Vec<u8>, Option<Word>, &'ctx [FunctionLikeParameterMetadata])> {
        let codebase = self.services.codebase;
        let classes: Vec<Word> = match node {
            (_, BindingMetadata { value: BindingValueMetadata::Class(class, _), .. }) => vec![*class],
            (_, BindingMetadata { value: BindingValueMetadata::Classes(classes), .. }) => {
                classes.iter().map(|(class, _)| *class).collect()
            }
            (module, BindingMetadata { value: BindingValueMetadata::Expression(lambda), .. }) => {
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

/// Spec section 32: a class value is a concrete class of the service, built through its main constructor. Whether it
/// is one.
fn check_class_value<A>(context: &mut Context<'_, '_, A>, service: &TUnion, class: Word, span: Span) -> bool
where
    A: Arena,
{
    let Some(metadata) = context.codebase.get_class_like(class.as_bytes()) else {
        report_undefined_type(context, class, span);
        return false;
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
        return false;
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
        return false;
    }

    true
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

/// The element type of a `List` service.
fn list_element(service: &TUnion) -> Option<&TUnion> {
    match service.types.as_ref() {
        [TAtomic::Array(TArray::List(list))] => Some(&list.element_type),
        _ => None,
    }
}

/// Whether a binding can give `service`, as the container reads a parameter's service: a class, or a collection of
/// objects, which the compiler names in the parameter's `Service` attribute.
fn is_service(service: &TUnion) -> bool {
    match service.types.as_ref() {
        [TAtomic::Object(TObject::Named(_))] => true,
        [TAtomic::Array(_)] => {
            service.get_all_child_nodes().iter().any(|node| matches!(node, TypeRef::Atomic(TAtomic::Object(_))))
        }
        _ => false,
    }
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

/// The namespace of `class`, in lowercase, as modules name theirs.
fn namespace_of(class: Word) -> Vec<u8> {
    let name = class.as_bytes();
    let namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&[][..], |end| &name[..end]);

    namespace.to_ascii_lowercase()
}
