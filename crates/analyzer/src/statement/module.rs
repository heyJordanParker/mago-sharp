//! Spec section 32: a module binds the services the container builds, and PHP# creates only the classes a module
//! binds. The checker proves the container can build every one, whether or not anything takes it, as Guice's
//! `requireExplicitBindings()` does. As in .NET's scope validation and Dagger's graph check, it refuses a service
//! nothing binds, a service that needs itself, and a `scoped` service a `singleton` would keep after its scope ends.
//!
//! A module also lists the namespaces its namespace imports from, as Java's `module-info.java` lists what a module
//! `requires`, and the checker refuses an import outside the list and two lists that name each other.

use std::cell::RefCell;
use std::rc::Rc;

use foldhash::HashMap;
use foldhash::HashSet;
use mago_allocator::Arena;
use mago_codex::identifier::method::MethodIdentifier;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::metadata::class_like::ClassLikeMetadata;
use mago_codex::metadata::function_like::FunctionLikeMetadata;
use mago_codex::metadata::module::BindingMetadata;
use mago_codex::metadata::module::BindingValueMetadata;
use mago_codex::metadata::module::Lifetime;
use mago_codex::metadata::module::ModuleKind;
use mago_codex::metadata::module::ModuleMetadata;
use mago_codex::metadata::module::STANDARD_LIBRARY_NAMESPACE;
use mago_codex::metadata::module::service_id;
use mago_codex::metadata::module::top_dispatcher_namespaces;
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
use mago_database::file::FolderNamespace;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::BindingValue;
use mago_syntax::cst::Call;
use mago_syntax::cst::FunctionLikeParameterList;
use mago_syntax::cst::Module;
use mago_syntax::cst::ModuleMember;
use mago_syntax::cst::TokenSeparatedSequence;
use mago_syntax::dialect::Dialect;
use mago_word::Word;
use mago_word::empty_word;
use mago_word::word;

use crate::artifacts::AnalysisArtifacts;
use crate::artifacts::CallTarget;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::effects;
use crate::effects::Body;
use crate::error::AnalysisError;
use crate::statement::function_like::FunctionLikeBody;
use crate::statement::function_like::analyze_function_like;
use crate::statement::function_like::report_undefined_type;
use crate::statement::function_like::report_undefined_types;
use crate::statement::use_statement::holds_function_or_constant;
use crate::statement::use_statement::report_non_existent_import;
use crate::utils::names::and_list;
use crate::utils::names::article;
use crate::utils::names::display_class_like_name;
use crate::utils::names::display_sharp_type;
use crate::utils::names::display_variable_name;
use crate::utils::template::explain_blocked_substitution;
use crate::visibility::display_namespace;
use crate::visibility::dotted;
use crate::visibility::list_refusing;

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
    // Only a test start turns a test module on, so one in a file that holds no test would never run.
    if metadata.kind == ModuleKind::Test && !holds_test(context, metadata) {
        context.collector.report_with_code(
            IssueCode::InvalidBinding,
            Issue::error(format!("`{}` holds no test, so its test module would never run.", metadata.file))
                .with_annotation(Annotation::primary(metadata.span).with_message("Declared here."))
                .with_note(
                    "Only a test of the file's class turns a test module on: a class that isn't abstract and extends `PHPUnit.Framework.TestCase`, in a folder `composer.json` lists under `autoload-dev`.",
                )
                .with_help("Move the module into the test file of the tests it serves."),
        );
    }

    let written = module.members.iter().filter_map(|member| match member {
        ModuleMember::Binding(binding) => Some(binding),
        ModuleMember::Imports(_) | ModuleMember::Law(_) => None,
    });
    let origin = ReferenceOrigin::File(word(&context.source_file.name));
    let services = Services::new(context.codebase, metadata);
    let mut bound: Vec<((Word, Option<Word>), Span)> = Vec::new();
    for (binding, binding_metadata) in written.zip(&metadata.bindings) {
        let Some(service) = service_type(&binding_metadata.service.type_union) else {
            report_undefined_types(context, &binding_metadata.service.type_union, binding.service.span());
            continue;
        };

        let key =
            (binding_metadata.service_id, binding_metadata.target.map(|target| context.codebase.class_key(target)));
        if let Some((_, first)) = bound.iter().find(|(earlier, _)| *earlier == key) {
            report_duplicate_binding(context, service, binding_metadata, *first);
        } else {
            bound.push((key, binding_metadata.span));
        }
        if binding_metadata.binds_dispatcher(context.codebase) {
            check_app_dispatcher(context, metadata, binding_metadata);
        }

        if let (Some(target), Some(written_target)) = (binding_metadata.target, &binding.target)
            && !context.codebase.get_class_like(target).is_some_and(|class| class.kind.is_class())
        {
            report_undefined_type(context, target, written_target.class.span());
        }

        match &binding_metadata.value {
            BindingValueMetadata::Class(class, span) => {
                if let Some(class) = check_class_value(context, service, *class, *span) {
                    check_class_services(
                        context,
                        &services,
                        class,
                        Builds::Bound(class.original_name, binding_metadata, *span),
                    );
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
                    if let Some(class) = check_class_value(context, element, *class, *span) {
                        let builds = Builds::Bound(class.original_name, binding_metadata, *span);
                        check_class_services(context, &services, class, builds);
                    }
                }
            }
            BindingValueMetadata::Expression(name) => {
                let BindingValue::Expression(expression) = binding.value else {
                    unreachable!("the scanner records an expression binding only for an expression value");
                };
                let Some(lambda) = context.codebase.get_closure(name) else {
                    unreachable!("the scanner registers the lambda of each expression binding");
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
                let body = FunctionLikeBody::Expression(expression);
                analyze_function_like(
                    context,
                    artifacts,
                    &mut block_context,
                    lambda,
                    parameter_list,
                    None,
                    body,
                    None,
                )?;
                effects::summary::record(
                    context,
                    artifacts,
                    Body::Binding(*name),
                    empty_word(),
                    parameter_list.parameters.iter().map(|parameter| parameter.variable.span).collect(),
                    None,
                    body,
                );
                effects::check::report_escaping_functions(context, artifacts, lambda, parameter_list, body);

                let namespace = metadata.namespace.as_bytes();
                for parameter in &lambda.parameters {
                    if let Some(missing) = services.missing(namespace, None, parameter) {
                        report_missing_service(context, Builds::Binding(service), missing, parameter);
                    }
                }
            }
        }
    }

    // An environment or test module also changes what the base modules' bindings reach, so each base binding its set
    // leaves in place is walked again over its services.
    let own = (metadata.kind != ModuleKind::Base).then_some(metadata);
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
                    .binding_for(module.namespace.as_bytes(), binding.service_id, binding.target)
                    .is_some_and(|(_, found)| std::ptr::eq(found, *binding))
        });
    let mut reported: Vec<(IssueCode, *const BindingMetadata)> = Vec::new();
    let (mut seen, mut classes) = (HashSet::default(), HashSet::default());
    for (module, binding) in starts {
        let mut walk = Walk::new(&services, own, binding, seen, classes);
        walk.dependencies_of((module, binding));
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
        if let Some((transient, class, at)) = walk.owner
            && first(IssueCode::UnownedDisposable, at)
        {
            report_undisposed_owner(context, transient, class, at);
        }
        (seen, classes) = (walk.seen, walk.classes);
    }
    for class in classes {
        artifacts.symbol_references.add_reference(origin, (class, empty_word()), true);
    }

    Ok(())
}

/// Spec sections 15 and 32: PHP# also creates the class of a listener, a new object for every event, so each
/// parameter of its main constructor needs a binding as a bound class's does, in the base modules. A problem is
/// reported at the parameter. `event` is the first event the class listens to.
pub(crate) fn check_listener_services<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata, event: Word)
where
    A: Arena,
{
    let services = Services::reading(context.codebase, Vec::new());

    check_class_services(context, &services, class, Builds::Listener(class.original_name, event));
}

/// Spec sections 27 and 32: PHP# also creates the class of a program, the class that declares `main`, before it calls
/// `main`, so each parameter of its main constructor needs a binding as a bound class's does, in the base modules. A
/// problem is reported at the parameter.
pub(crate) fn check_program_services<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata)
where
    A: Arena,
{
    let services = Services::reading(context.codebase, Vec::new());

    check_class_services(context, &services, class, Builds::Program(class.original_name));
}

/// Spec section 32: PHPUnit creates a test class, one that extends its `TestCase`, for each test, and an extension, one
/// that implements its `Extension`, once, with `new`, so the container fills each parameter of their main constructor
/// as a bound class's: a test class's from its file's test module over the base modules, rebuilt for every test, and an
/// extension's from the base modules. A problem is reported at the parameter.
pub(crate) fn check_filled_services<A>(context: &mut Context<'_, '_, A>, class: &ClassLikeMetadata)
where
    A: Arena,
{
    let codebase = context.codebase;
    if !is_buildable(class) {
        return;
    }
    // A test class with no main constructor of its own below `TestCase` takes PHPUnit's, which PHPUnit fills.
    let constructor = MethodIdentifier::new(class.name, word("__construct"));
    let declaring = codebase.get_declaring_method_identifier(&constructor).get_class_name();
    if !codebase.class_extends(declaring, word(TEST_CASE)) && !codebase.class_implements(declaring, word(EXTENSION)) {
        return;
    }
    let overrides = if codebase.class_extends(class.name, word(TEST_CASE)) {
        codebase
            .modules
            .iter()
            .filter(|module| module.kind == ModuleKind::Test && module.span.file_id == class.span.file_id)
            .collect()
    } else if codebase.class_implements(class.name, word(EXTENSION)) {
        Vec::new()
    } else {
        return;
    };
    let services = Services::reading(codebase, overrides);

    check_class_services(context, &services, class, Builds::Filled(class.original_name));
}

/// Spec section 32: PHP# creates the classes a module binds, the classes of listeners and the class of a program
/// through their main constructors, so each parameter needs a binding in the module of the class's namespace or of a
/// namespace above it.
/// Reports each parameter of `class`'s main constructor that `services` can't fill, as `builds` names the class: a
/// bound class's at the binding's class value, with a note at the parameter.
fn check_class_services<A>(
    context: &mut Context<'_, '_, A>,
    services: &Services<'_>,
    class: &ClassLikeMetadata,
    builds: Builds<'_>,
) where
    A: Arena,
{
    let Some(constructor) = context.codebase.get_declaring_method(class.name, word("__construct")) else {
        return;
    };

    let namespace = namespace_of(class.name);
    for parameter in &constructor.parameters {
        if let Some(missing) = services.missing(&namespace, Some(class.name), parameter) {
            report_missing_service(context, builds, missing, parameter);
        }
    }
}

/// What the container builds when it fills a parameter: a class a binding binds, with that binding and the span of
/// the class in its value, the service of a binding's expression, a listener's class, with the first event it
/// listens to, a program's class, or a test class or PHPUnit extension, whose services the container fills.
#[derive(Clone, Copy)]
enum Builds<'ctx> {
    Bound(Word, &'ctx BindingMetadata, Span),
    Binding(&'ctx TUnion),
    Listener(Word, Word),
    Program(Word),
    Filled(Word),
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
        Builds::Bound(class, ..) | Builds::Program(class) | Builds::Filled(class) => {
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
                Builds::Bound(..) | Builds::Listener(..) | Builds::Program(_) | Builds::Filled(_) => {
                    format!("`{name}`'s namespace")
                }
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
                Builds::Bound(_, binding, _) => {
                    let service = display_sharp_type(context, &binding.service.type_union);
                    format!(
                        "Bind `{service}` to an expression that creates it, as in `{} {service}(…) = {};`.",
                        binding.lifetime.as_str(),
                        binding_expression(context, binding)
                    )
                }
                Builds::Binding(_) | Builds::Listener(..) | Builds::Program(_) | Builds::Filled(_) => {
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
        Builds::Bound(_, _, bound) => issue
            .with_annotation(Annotation::primary(bound).with_message("Bound here."))
            .with_annotation(Annotation::secondary(parameter.span).with_message("Needed here.")),
        Builds::Binding(_) | Builds::Listener(..) | Builds::Program(_) | Builds::Filled(_) => {
            issue.with_annotation(Annotation::primary(parameter.span).with_message("Needed here."))
        }
    };

    context.collector.report_with_code(IssueCode::MissingService, issue);
}

/// The expression that creates what `binding`'s class value creates: `new` of its class, or a list of `new` of each
/// class of its list. A class whose main constructor takes parameters shows them as `(…)`.
fn binding_expression<A>(context: &Context<'_, '_, A>, binding: &BindingMetadata) -> String
where
    A: Arena,
{
    let creates = |class: Word| {
        let metadata = context.codebase.get_class_like(class);
        let takes =
            metadata.and_then(|metadata| context.codebase.get_declaring_method(metadata.name, word("__construct")));
        let arguments = if takes.is_some_and(|constructor| !constructor.parameters.is_empty()) { "…" } else { "" };
        let name = metadata.map_or(class, |metadata| metadata.original_name);
        format!("new {}({arguments})", display_class_like_name(context, name))
    };

    match &binding.value {
        BindingValueMetadata::Classes(classes) => {
            format!("[{}]", classes.iter().map(|(class, _)| creates(*class)).collect::<Vec<_>>().join(", "))
        }
        BindingValueMetadata::Class(class, _) => creates(*class),
        BindingValueMetadata::Expression(_) => unreachable!("a binding's expression builds no class value"),
    }
}

/// The modules of one namespace that bind one service, in the order the container reads them, each with its bindings
/// of that service as written.
type Serving<'ctx> = Vec<(&'ctx ModuleMetadata, Vec<&'ctx BindingMetadata>)>;

/// A class a binding builds, or its binding expression, with the binding that serves each parameter the container fills
/// to build it, in the order it takes them. A parameter no binding serves has none.
struct Built<'ctx> {
    /// The class, or `None` for a binding expression.
    class: Option<&'ctx ClassLikeMetadata>,
    served: Vec<Node<'ctx>>,
}

/// The modules one set of services reads: every folder's base module, and the modules whose bindings win over them,
/// every module of one environment or one test module.
struct Services<'ctx> {
    codebase: &'ctx CodebaseMetadata,
    /// The modules that serve each service id in each namespace.
    serving: HashMap<&'ctx [u8], HashMap<Word, Serving<'ctx>>>,
    /// What each binding the set has walked builds, by the binding.
    built: RefCell<HashMap<*const BindingMetadata, Rc<[Built<'ctx>]>>>,
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

        self.binding_for(namespace, service_id(service, self.codebase), requester)
            .is_none()
            .then_some(Missing::Unbound(service))
    }

    /// What `node` builds: a bound class's main constructor's parameters, which resolve from the class's namespace, or
    /// a binding expression's own, which resolve from the module that holds the binding. Resolved once for each
    /// binding the set walks.
    fn built_by(&self, (module, binding): Node<'ctx>) -> Rc<[Built<'ctx>]> {
        if let Some(built) = self.built.borrow().get(&std::ptr::from_ref(binding)) {
            return Rc::clone(built);
        }

        let codebase = self.codebase;
        let serve = |namespace: &[u8], requester: Option<Word>, parameters: &[FunctionLikeParameterMetadata]| {
            parameters
                .iter()
                .filter_map(parameter_service)
                .filter_map(|service| self.binding_for(namespace, service_id(service, codebase), requester))
                .collect::<Vec<Node<'ctx>>>()
        };
        let of_class = |class: Word| {
            let class = codebase.get_class_like(class)?;
            let constructor = codebase.get_declaring_method(class.name, word("__construct"));
            let served = constructor.map_or_else(Vec::new, |constructor| {
                serve(&namespace_of(class.name), Some(class.original_name), &constructor.parameters)
            });

            Some(Built { class: Some(class), served })
        };
        let built: Rc<[Built<'ctx>]> = match &binding.value {
            BindingValueMetadata::Class(class, _) => of_class(*class).into_iter().collect(),
            BindingValueMetadata::Classes(classes) => {
                classes.iter().filter_map(|(class, _)| of_class(*class)).collect()
            }
            BindingValueMetadata::Expression(lambda) => codebase
                .get_closure(lambda)
                .map(|lambda| Built {
                    class: None,
                    served: serve(module.namespace.as_bytes(), None, &lambda.parameters),
                })
                .into_iter()
                .collect(),
        };
        self.built.borrow_mut().insert(std::ptr::from_ref(binding), Rc::clone(&built));

        built
    }

    /// The set of services `module` belongs to: the base set for a base module, the base set under every module of
    /// its environment for an environment module, as the container reads each `Module.<SHARP_ENV>.sharp` of the class's
    /// namespace and each one above it, and the base set under its own bindings for a test module.
    fn new(codebase: &'ctx CodebaseMetadata, module: &'ctx ModuleMetadata) -> Self {
        let overrides = match module.kind {
            ModuleKind::Base => Vec::new(),
            ModuleKind::Environment(_) => codebase.modules.iter().filter(|other| other.kind == module.kind).collect(),
            ModuleKind::Test => vec![module],
        };

        Self::reading(codebase, overrides)
    }

    /// The set that reads `overrides` before every folder's base module.
    fn reading(codebase: &'ctx CodebaseMetadata, overrides: Vec<&'ctx ModuleMetadata>) -> Self {
        let mut serving: HashMap<&[u8], HashMap<Word, Serving<'ctx>>> = HashMap::default();
        let base = codebase.modules.iter().filter(|module| module.kind == ModuleKind::Base);
        for module in overrides.into_iter().chain(base) {
            let services = serving.entry(module.namespace.as_bytes()).or_default();
            for binding in &module.bindings {
                let modules = services.entry(binding.service_id).or_default();
                match modules.last_mut() {
                    Some((last, bindings)) if std::ptr::eq(*last, module) => bindings.push(binding),
                    _ => modules.push((module, vec![binding])),
                }
            }
        }

        Self { codebase, serving, built: RefCell::default() }
    }

    /// The binding that serves the service of the id `service` to `requester`, a class of `namespace`: the nearest
    /// namespace's module first, up to the root namespace's, then the standard library's module, as the container
    /// reads them, an override before its base module, and in one module a binding `for` the requester before one for
    /// any class.
    fn binding_for(
        &self,
        namespace: &[u8],
        service: Word,
        requester: Option<Word>,
    ) -> Option<(&'ctx ModuleMetadata, &'ctx BindingMetadata)> {
        let namespaces = std::iter::successors(Some(namespace), |namespace| {
            let above = namespace.iter().rposition(|byte| *byte == b'\\').unwrap_or(0);

            (!namespace.is_empty()).then(|| &namespace[..above])
        })
        .chain([STANDARD_LIBRARY_NAMESPACE.as_bytes()]);
        for namespace in namespaces {
            let modules = self.serving.get(namespace).and_then(|services| services.get(&service));
            for (module, bindings) in modules.into_iter().flatten() {
                let targeted = bindings.iter().find(|binding| {
                    binding.target.zip(requester).is_some_and(|(target, requester)| {
                        self.codebase.class_key(target) == self.codebase.class_key(requester)
                    })
                });
                if let Some(binding) = targeted.or_else(|| bindings.iter().find(|binding| binding.target.is_none())) {
                    return Some((*module, *binding));
                }
            }
        }

        None
    }
}

/// A service the walk reaches: a binding and the module that holds it.
type Node<'ctx> = (&'ctx ModuleMetadata, &'ctx BindingMetadata);

/// A walk of the services one binding needs, which records every class it reads, the first cycle back to that
/// binding, and the first `scoped` service a `singleton` binding reaches without passing another `singleton`. Each
/// problem carries the binding it is reported at: the start for a base module, and for an environment or test module,
/// `own`, the last of its bindings on the way, so its walks of base bindings report only what it changed.
struct Walk<'services, 'ctx> {
    services: &'services Services<'ctx>,
    own: Option<&'ctx ModuleMetadata>,
    start: &'ctx BindingMetadata,
    path: Vec<Word>,
    bindings: Vec<&'ctx BindingMetadata>,
    seen: HashSet<(*const BindingMetadata, bool, bool)>,
    /// Every class this walk and the walks before it read.
    classes: HashSet<Word>,
    cycle: Option<(Vec<Word>, &'ctx BindingMetadata)>,
    captive: Option<(&'ctx BindingMetadata, &'ctx BindingMetadata)>,
    /// A transient `Disposable` service, the class that takes it and so owns it but isn't `Disposable`, and where it
    /// is reported.
    owner: Option<(&'ctx BindingMetadata, &'ctx ClassLikeMetadata, &'ctx BindingMetadata)>,
}

impl<'services, 'ctx> Walk<'services, 'ctx> {
    /// A walk from `start` that adds to `classes`, the classes the walks before it read, and keeps `seen`'s room.
    fn new(
        services: &'services Services<'ctx>,
        own: Option<&'ctx ModuleMetadata>,
        start: &'ctx BindingMetadata,
        mut seen: HashSet<(*const BindingMetadata, bool, bool)>,
        classes: HashSet<Word>,
    ) -> Self {
        seen.clear();

        Self {
            services,
            own,
            start,
            path: Vec::new(),
            bindings: vec![start],
            seen,
            classes,
            cycle: None,
            captive: None,
            owner: None,
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
        let Some(own) = self.own else {
            return Some(self.start);
        };

        reached
            .into_iter()
            .chain(self.bindings.iter().rev().copied())
            .find(|binding| own.bindings.iter().any(|written| std::ptr::eq(written, *binding)))
    }

    /// Visits the binding that serves each service each class `node` builds takes, with that class on the path a cycle
    /// names. A service no binding serves is reported at the binding of the class that takes it, so the walk stops
    /// there.
    fn visit_parameters_of(&mut self, node: Node<'ctx>, held: bool) {
        for built in self.services.built_by(node).iter() {
            if let Some(class) = built.class {
                self.classes.insert(class.name);
                self.path.push(class.original_name);
            }
            for served in &built.served {
                if let Some(class) = built.class
                    && self.owner.is_none()
                    && self.owns_undisposed(class, served.1)
                    && let Some(at) = self.reported_at(Some(served.1))
                {
                    self.owner = Some((served.1, class, at));
                }
                self.visit(*served, held);
            }
            if built.class.is_some() {
                self.path.pop();
            }
        }
    }

    /// Spec sections 2 and 32: whether `class` takes the service of the transient `binding`, a `Disposable` one, and
    /// isn't `Disposable` itself. A transient service is the class's own, so it owns the value, and only a
    /// `Disposable` class can dispose a value it holds.
    fn owns_undisposed(&self, class: &ClassLikeMetadata, binding: &BindingMetadata) -> bool {
        let codebase = self.services.codebase;
        let disposable = word(b"Sharp\\Disposable");

        binding.lifetime == Lifetime::Transient
            && binding.service.type_union.types.iter().any(|atomic| match atomic {
                TAtomic::Object(TObject::Named(named)) => codebase.is_instance_of(named.name, disposable),
                _ => false,
            })
            && !codebase.is_instance_of(class.name, disposable)
    }

    /// Visits `binding`, which serves a service the walk reached.
    fn visit(&mut self, (module, binding): Node<'ctx>, held: bool) {
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
}

/// Spec section 32: a class value is a concrete class of the service, built through its main constructor. The class,
/// when it is one.
fn check_class_value<'ctx, A>(
    context: &mut Context<'ctx, '_, A>,
    service: &TUnion,
    class: Word,
    span: Span,
) -> Option<&'ctx ClassLikeMetadata>
where
    A: Arena,
{
    let codebase = context.codebase;
    let Some(metadata) = codebase.get_class_like(class) else {
        report_undefined_type(context, class, span);
        return None;
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
        return None;
    }

    let value = get_named_object(metadata.name, None);
    let mut comparison = ComparisonResult::for_dialect(Dialect::Sharp);
    if !union_comparator::is_contained_by(context.codebase, &value, service, false, false, false, &mut comparison) {
        // A class that implements the service's class with another type argument is refused as spec section 11.1 words
        // an invariant substitution.
        let issue = Issue::error(format!("`{class_name}` is not a `{service_name}`."))
            .with_annotation(Annotation::primary(span).with_message("Bound here."))
            .with_help(format!("Bind `{service_name}` to a class that implements it."));
        let (code, issue) = explain_blocked_substitution(
            context,
            &value,
            service,
            IssueCode::InvalidBinding,
            (IssueCode::InvalidBinding, issue),
        );
        context.collector.report_with_code(code, issue);
        return None;
    }

    Some(metadata)
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

/// Refuses the app's binding of the dispatcher in `module` unless its module is the app's one top module that binds it,
/// spec section 15: "An app replaces the dispatcher once at startup". A module below the top one is refused, and
/// two top modules of the same depth are each refused. A test module replaces it freely, and a package's or the
/// standard library's binding is never the app's.
fn check_app_dispatcher<A>(context: &mut Context<'_, '_, A>, module: &ModuleMetadata, binding: &BindingMetadata)
where
    A: Arena,
{
    if module.kind == ModuleKind::Test
        || context.source_file.is_standard_library
        || module.file.as_bytes().starts_with(b"vendor/")
    {
        return;
    }

    let codebase = context.codebase;
    let top = top_dispatcher_namespaces(&codebase.modules, codebase);
    if top.as_slice() == [module.namespace] {
        return;
    }

    let others: Vec<(Word, Span)> = codebase
        .modules
        .iter()
        .filter(|other| other.kind != ModuleKind::Test && other.namespace != module.namespace)
        .filter(|other| top.contains(&other.namespace) && !other.file.as_bytes().starts_with(b"vendor/"))
        .filter_map(|other| {
            other.bindings.iter().find(|bound| bound.binds_dispatcher(codebase)).map(|bound| (other.file, bound.span))
        })
        .collect();
    let files = others.iter().map(|(file, _)| format!("`{file}`")).collect::<Vec<_>>().join(" and ");
    let dispatcher = display_sharp_type(context, &binding.service.type_union.clone().without_readonly());
    let (message, help) = if top.contains(&module.namespace) {
        (
            format!("This module and {files} each bind `{dispatcher}`, and an app binds it once, in its top module."),
            format!("Bind `{dispatcher}` in one module, in the namespace above both."),
        )
    } else {
        (
            format!("Only {files} binds `{dispatcher}`: an app binds it once, in its top module."),
            "Remove this binding. The app's dispatcher serves every `emit`, and a test module replaces it in tests."
                .to_owned(),
        )
    };
    let mut issue =
        Issue::error(message).with_annotation(Annotation::primary(binding.span).with_message("Bound here."));
    for (_, span) in others {
        issue = issue.with_annotation(Annotation::secondary(span).with_message("The app's dispatcher is bound here."));
    }
    context.collector.report_with_code(IssueCode::InvalidBinding, issue.with_help(help));
}

/// Spec section 32: checks the `imports` line of `metadata`, a folder's module. Each namespace it lists is a namespace
/// of the project or its libraries that each list above it allows: one a class or function declares, or one an
/// `extern namespace` line gives a library's folder, spec section 23, as a class import from it resolves.
pub(crate) fn analyze_imports<A>(context: &mut Context<'_, '_, A>, metadata: &ModuleMetadata)
where
    A: Arena,
{
    if metadata.kind != ModuleKind::Base || metadata.imports.is_empty() {
        return;
    }

    let codebase = context.codebase;
    let given = |namespace: &[u8]| {
        codebase.extern_namespaces.iter().any(|declaration| {
            declaration.namespace.as_bytes().eq_ignore_ascii_case(namespace)
                && codebase.extern_namespace_folder(declaration).is_some()
        })
    };
    for (import, span) in &metadata.imports {
        let namespace = import.as_bytes();
        if !codebase.namespace_exists(*import) && !holds_function_or_constant(codebase, namespace) && !given(namespace)
        {
            report_non_existent_import(context, *span, namespace, "namespace");
        } else if let Some(parent) =
            list_refusing(context.codebase, context.source_file.id, metadata.namespace.as_bytes(), namespace)
        {
            let parent = display_namespace(context.codebase, parent);
            context.collector.report_with_code(
                IssueCode::UnlistedImport,
                Issue::error(format!(
                    "`{}` is outside `{parent}`'s list, so this entry allows nothing.",
                    dotted(namespace)
                ))
                .with_annotation(Annotation::primary(*span))
                .with_help(format!("Add `{}` to `{parent}`'s list, or remove this entry.", dotted(namespace))),
            );
        }
    }
}

/// Spec section 23: reports the import at `span` when its namespace leads back to the importing file's, directly or
/// through other namespaces, a parent and its child among them, as Go refuses every import cycle between packages. The
/// edges are the project's imports, and the namespace is the unit. The codebase's population finds each cycle once,
/// [`CodebaseMetadata::import_cycle`], and the error names the namespaces of the cycle, the other file's import that
/// closes it, and the two ways out.
pub(crate) fn check_import_cycle<A>(context: &mut Context<'_, '_, A>, span: Span)
where
    A: Arena,
{
    let Some(cycle) = context.codebase.import_cycle(span) else {
        return;
    };
    let Some(closing) = cycle.last() else {
        return;
    };
    let through: Vec<Word> = cycle.iter().map(|import| word(dotted(import.namespace.as_bytes()))).collect();

    let file = closing.file.as_bytes().rsplit(|byte| *byte == b'/').next().unwrap_or_default();
    let name = closing.name.as_bytes().rsplit(|byte| *byte == b'\\').next().unwrap_or_default();
    context.collector.report_with_code(
        IssueCode::ImportCycle,
        Issue::error(format!(
            "{} import each other ({} imports {}). Move the code, or declare an interface in one namespace that the other implements.",
            and_list(&through),
            String::from_utf8_lossy(file),
            String::from_utf8_lossy(name)
        ))
        .with_annotation(Annotation::primary(span).with_message("Imported here."))
        .with_annotation(Annotation::secondary(closing.span).with_message("Imported back here.")),
    );
}

/// Spec section 32: refuses `call`, a plain PHP call in PHP# code, at `span` when a value of a class a module binds
/// comes out of it: its result type names one, or `narrowed`, the classes PHP# code narrows its result to with `as`
/// or `is`, names one. A bound class reaches code only through injection, a constructor or a method parameter, as
/// Dagger and Angular hand a bound class only to what they build. The one place plain PHP gives one is the expression
/// of the binding that binds it, when it is a plain PHP class: there a framework's object enters the container, as
/// C#'s `services.AddScoped<T>(sp => …)` adapts one.
pub(crate) fn report_bound_from_plain_php<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &AnalysisArtifacts,
    call: &Call<'_>,
    narrowed: &[Word],
    span: Span,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let is_plain_php = artifacts.get_callees(call).any(|target| match target {
        CallTarget::FunctionLike { callee, .. } | CallTarget::MagicMethod { callee, .. } => {
            codebase.get_function_like(callee).is_some_and(|function| !function.flags.is_sharp())
        }
        CallTarget::Property { .. } | CallTarget::Element { .. } => false,
    });
    if !context.dialect.is_sharp() || !is_plain_php {
        return;
    }

    let returned = artifacts.get_expression_type(call).into_iter().flat_map(|result| {
        result.types.iter().filter_map(|atomic| match atomic {
            TAtomic::Object(TObject::Named(object)) => Some(object.name),
            _ => None,
        })
    });
    let binding = enclosing_binding(codebase, span);
    let adapted = |class: &Word| {
        binding.is_some_and(|binding| binds(binding, class.as_bytes()))
            && codebase.get_class_like(*class).is_some_and(|class| !class.flags.is_sharp())
    };
    let Some((class, module)) = returned
        .chain(narrowed.iter().copied())
        .filter(|class| !adapted(class))
        .find_map(|class| Some((class, bound_module(codebase, class.as_bytes())?)))
    else {
        return;
    };

    let class =
        display_class_like_name(context, codebase.get_class_like(class).map_or(class, |class| class.original_name));
    let namespace = display_namespace(codebase, module);
    context.collector.report_with_code(
        IssueCode::InvalidBinding,
        Issue::error(format!(
            "`{class}` is bound in `{namespace}`'s module, so it reaches code through a constructor or a parameter."
        ))
        .with_annotation(Annotation::primary(span).with_message("Reached through plain PHP here."))
        .with_annotation(Annotation::secondary(module.span).with_message("Bound here."))
        .with_help(format!("Take `{class}` as a constructor or method parameter, and the container passes it.")),
    );
}

/// The base or environment module that binds the class named `class`, spec section 32.
fn bound_module<'codebase>(codebase: &'codebase CodebaseMetadata, class: &[u8]) -> Option<&'codebase ModuleMetadata> {
    codebase
        .modules
        .iter()
        .filter(|module| module.kind != ModuleKind::Test)
        .find(|module| module.bindings.iter().any(|binding| binds(binding, class)))
}

/// Whether `binding`'s service is the class named `class`.
fn binds(binding: &BindingMetadata, class: &[u8]) -> bool {
    binding.service.type_union.clone().without_readonly().get_id().as_bytes().eq_ignore_ascii_case(class)
}

/// The binding whose expression holds `span`, a lambda in it included, spec section 32.
pub(crate) fn enclosing_binding(codebase: &CodebaseMetadata, span: Span) -> Option<&BindingMetadata> {
    codebase.modules.iter().flat_map(|module| &module.bindings).find(|binding| {
        matches!(binding.value, BindingValueMetadata::Expression(_))
            && binding.span.file_id == span.file_id
            && binding.span.contains(&span.start)
    })
}

/// Spec section 32: whether the PHP# code of the analyzed file imports `name`, the full name of a class-like, of a
/// namespace, or of a namespace's function or constant, past each list that covers its namespace. Reports at `span`,
/// and returns false, when a [list refuses](list_refusing) it. No list refuses an import of a test file.
pub(crate) fn check_listed_import<A>(context: &mut Context<'_, '_, A>, name: &[u8], span: Span) -> bool
where
    A: Arena,
{
    let namespace = context.scope.namespace_name().unwrap_or_default();
    let Some(module) = list_refusing(context.codebase, context.source_file.id, namespace, name) else {
        return true;
    };

    let listed: Vec<Word> = module.imports.iter().map(|(listed, _)| word(dotted(listed.as_bytes()))).collect();
    context.collector.report_with_code(
        IssueCode::UnlistedImport,
        Issue::error(format!("`{}` imports only {}.", display_namespace(context.codebase, module), and_list(&listed)))
            .with_annotation(Annotation::primary(span).with_message(format!("Imports `{}`.", dotted(name))))
            .with_note(format!("The list is the `imports` line of {}.", module.file)),
    );

    false
}

/// Reports that `binding` needs itself through the classes of `cycle`, at the binding `at`.
fn report_cycle<A>(context: &mut Context<'_, '_, A>, binding: &BindingMetadata, cycle: &[Word], at: &BindingMetadata)
where
    A: Arena,
{
    let service_name = display_sharp_type(context, &binding.service.type_union);
    let through = cycle
        .iter()
        .map(|name| format!("`{}`", display_class_like_name(context, *name)))
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

/// Reports that `class` takes the transient `Disposable` service of `transient`, and so owns it, without being
/// `Disposable`, at the binding `at`.
fn report_undisposed_owner<A>(
    context: &mut Context<'_, '_, A>,
    transient: &BindingMetadata,
    class: &ClassLikeMetadata,
    at: &BindingMetadata,
) where
    A: Arena,
{
    let service_name = display_sharp_type(context, &transient.service.type_union);
    let class_name = display_class_like_name(context, class.original_name);
    let mut issue = Issue::error(format!(
        "The transient `{service_name}` is `Disposable`, so `{class_name}`, which takes it, owns it, and only a `Disposable` class can dispose it."
    ))
    .with_annotation(Annotation::primary(at.span).with_message("Bound here."))
    .with_help(format!("Make `{class_name}` `Disposable` and dispose it there, or make `{service_name}` scoped."));
    if !std::ptr::eq(transient, at) {
        issue = issue.with_annotation(Annotation::secondary(transient.span).with_message("Transient here."));
    }

    context.collector.report_with_code(IssueCode::UnownedDisposable, issue);
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
        [TAtomic::Object(TObject::Named(object))] => codebase.get_class_like(object.name),
        _ => None,
    }
}

/// Whether the container can build `class` through its main constructor: a class that is not abstract.
fn is_buildable(class: &ClassLikeMetadata) -> bool {
    class.kind.is_class() && !class.flags.is_abstract()
}

/// The class every PHPUnit test extends.
const TEST_CASE: &str = "PHPUnit\\Framework\\TestCase";

/// The interface every PHPUnit extension implements.
const EXTENSION: &str = "PHPUnit\\Runner\\Extension\\Extension";

/// Whether the file that declares `module`, a test module, holds a test, which alone turns a test module on, spec
/// section 32: it is a test file, in a folder `composer.json` lists under `autoload-dev`, and its class isn't abstract
/// and extends PHPUnit's `TestCase`, directly or through a parent, so PHPUnit runs its tests.
fn holds_test<A>(context: &Context<'_, '_, A>, module: &ModuleMetadata) -> bool
where
    A: Arena,
{
    context.source_file.folder_namespace == Some(FolderNamespace::Test)
        && context.codebase.class_likes.values().any(|class| {
            class.span.file_id == module.span.file_id
                && is_buildable(class)
                && context.codebase.class_extends(class.name, word(TEST_CASE))
        })
}

/// The namespace of `class`, in lowercase, as modules name theirs.
fn namespace_of(class: Word) -> Vec<u8> {
    let name = class.as_bytes();
    let namespace = name.iter().rposition(|byte| *byte == b'\\').map_or(&[][..], |end| &name[..end]);

    namespace.to_ascii_lowercase()
}
