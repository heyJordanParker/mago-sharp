use mago_allocator::Arena;
use mago_codex::metadata::constant::ConstantMetadata;
use mago_codex::scanner::inference::get_literal_constant_type;
use mago_codex::scanner::inference::get_platform_constant_type;
use mago_codex::ttype::expander;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::union::TUnion;
use mago_names::binding::Binding;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::Access;
use mago_syntax::cst::ClassLikeMemberSelector;
use mago_syntax::cst::ConstantAccess;
use mago_syntax::cst::DirectVariable;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Identifier;
use mago_syntax::cst::Keyword;
use mago_syntax::cst::LocalIdentifier;
use mago_syntax::cst::PropertyAccess;
use mago_syntax::cst::StaticPropertyAccess;
use mago_syntax::cst::Variable;
use mago_word::Word;
use mago_word::word;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::variable::read_variable;
use crate::utils::expression::get_bare_name_variable_id;
use crate::utils::names::namespace_member;
use mago_bytes::BytesDisplay;

impl<'arena> Analyzable<'_, 'arena> for ConstantAccess<'arena> {
    fn analyze<'ctx, A>(
        &self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        if let Some(variable_id) = get_bare_name_variable_id(&self.name, context.resolved_names) {
            let resulting_type = read_variable(context, block_context, artifacts, variable_id.as_bytes(), self.span());
            artifacts.set_rc_expression_type(self, resulting_type);

            return Ok(());
        }

        if context.resolved_names.binding(&self.name) == Some(Binding::Field) {
            match field_storage(self, context, block_context) {
                Some(storage) => storage.analyze(context, block_context, artifacts)?,
                None => artifacts.set_expression_type(self, get_mixed()),
            }

            return Ok(());
        }

        // The semantic checks report a bare member, and a class or a type parameter is only ever the object of a member
        // access.
        if let Some(Binding::Class | Binding::Member | Binding::TypeParameter { .. }) =
            context.resolved_names.binding(&self.name)
        {
            artifacts.set_expression_type(self, get_mixed());

            return Ok(());
        }

        let name_bytes = context.resolved_names.get(self);
        let unqualified_name = self.name.value();

        // Spec section 23: a bare name in a `.sharp` file reads the code's own constant, of its namespace, and PHP's
        // constants are read through their namespace, as `global.PHP_EOL`.
        let constant_metadata = if context.dialect.is_sharp() {
            context.codebase.get_constant(name_bytes)
        } else {
            context.codebase.get_constant_or_global(name_bytes, unqualified_name)
        };

        let Some(constant_metadata) = constant_metadata else {
            // The name as written, since a `.sharp` file resolves `TRUE` into its namespace, and the semantic checks
            // refuse it as `true` written in another case.
            if let Some(literal_type) = get_literal_constant_type(unqualified_name) {
                artifacts.set_expression_type(self, literal_type);
                return Ok(());
            }

            let is_known = block_context.known_constants.contains(&word(name_bytes));
            let written = BytesDisplay(unqualified_name);

            if is_known {
                artifacts.set_expression_type(self, get_mixed());
            } else if context.dialect.is_sharp()
                && let Some(global) = context.codebase.get_constant(unqualified_name)
            {
                context.collector.report_with_code(
                    IssueCode::NonExistentConstant,
                    Issue::error(format!(
                        "`{written}` is PHP's constant, and a bare name reads only this code's own: write `global.{written}`."
                    ))
                    .with_annotation(Annotation::primary(self.span()).with_message("Reads PHP's root constant.")),
                );
                artifacts.set_expression_type(self, constant_type(context, global, unqualified_name));
            } else if context.dialect.is_sharp() {
                context.collector.report_with_code(
                    IssueCode::NonExistentConstant,
                    Issue::error(format!("Undefined constant: `{written}`."))
                        .with_annotation(
                            Annotation::primary(self.span())
                                .with_message(format!("Constant `{written}` is not defined.")),
                        )
                        .with_note("The constant might be misspelled or not defined.")
                        .with_help(format!(
                            "Check `{written}` for typos. A bare name reads a constant of this file's namespace, declared in a `.php` file. A class constant is read through its class, as in `Limits.MAX`, and PHP's constant through its namespace, as in `global.PHP_EOL`."
                        )),
                );
            } else {
                let name = BytesDisplay(name_bytes);
                context.collector.report_with_code(
                    IssueCode::NonExistentConstant,
                    Issue::error(format!(
                        "Undefined constant: `{name}`."
                    ))
                    .with_annotation(
                        Annotation::primary(self.span())
                            .with_message(format!("Constant `{name}` is not defined."))
                    )
                    .with_note(
                        "The constant might be misspelled, not defined, or not imported."
                    )
                    .with_help(
                        format!(
                            "Define the constant `{name}` using `define()` or `const`, or check for typos and ensure it's available in this scope."
                        )
                    ),
                );
            }

            return Ok(());
        };

        let constant_type =
            read_constant(context, block_context, artifacts, constant_metadata, name_bytes, self.span());
        artifacts.set_expression_type(self, constant_type);

        Ok(())
    }
}

/// Analyzes the PHP# read `read` of `namespace_name.member`, as `global.PHP_EOL`, or `Pdf.DEFAULT_DPI` after
/// `import Vendor.Pdf;`, as a read of the plain PHP constant `member` of `namespace`, which [`member_namespace`] found
/// (spec section 23). The read is refused when the namespace has a function of that name, which the code calls, or no
/// constant of that name.
///
/// [`member_namespace`]: crate::utils::names::member_namespace
pub(crate) fn analyze_namespace_constant<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    read: &PropertyAccess<'_>,
    namespace_name: &Identifier<'_>,
    member: &LocalIdentifier<'_>,
    namespace: Word,
) where
    A: Arena,
{
    // The name as the import and the read write it, which PHP finds as the codebase did: its namespace in any case.
    let full_name = namespace_member(namespace, member.value);
    if let Some(constant) = context.codebase.get_constant(full_name.as_bytes()) {
        let constant_type =
            read_constant(context, block_context, artifacts, constant, full_name.as_bytes(), read.span());
        artifacts.record_namespace_constant(read.span(), full_name);
        artifacts.set_expression_type(read, constant_type);

        return;
    }

    let written = format!("{}.{}", BytesDisplay(namespace_name.value()), BytesDisplay(member.value));
    let issue = if context.codebase.function_exists(full_name.as_bytes()) {
        Issue::error(format!("`{written}` is a plain PHP function, not a constant: call it, as in `{written}(…)`."))
            .with_annotation(Annotation::primary(read.span()).with_message("Reads the function without calling it."))
    } else {
        let owner = if namespace.is_empty() {
            "PHP's root namespace".to_owned()
        } else {
            format!("The namespace `{}`", String::from_utf8_lossy(namespace.as_bytes()).replace('\\', "."))
        };

        Issue::error(format!("Undefined constant: `{written}`."))
            .with_annotation(
                Annotation::primary(read.span()).with_message(format!("Constant `{written}` is not defined.")),
            )
            .with_note(format!("{owner} declares no constant `{}`.", BytesDisplay(member.value)))
            .with_help(format!("Check `{}` for typos.", BytesDisplay(member.value)))
    };
    context.collector.report_with_code(IssueCode::NonExistentConstant, issue);
    artifacts.set_expression_type(read, get_mixed());
}

/// The type of a read at `span` of the constant `metadata`, whose full name is `name`, once the read is checked as
/// every read of a constant is: for a deprecated or experimental constant, and for one the configured PHP version
/// lacks. The read references the constant, as a call references its function, so an edit to the file that declares
/// it reaches the reading file.
fn read_constant<A>(
    context: &mut Context<'_, '_, A>,
    block_context: &BlockContext<'_>,
    artifacts: &mut AnalysisArtifacts,
    metadata: &ConstantMetadata,
    name: &[u8],
    span: Span,
) -> TUnion
where
    A: Arena,
{
    artifacts.symbol_references.add_reference_to_symbol(&block_context.scope, metadata.name, false);

    let display = BytesDisplay(name);
    if metadata.flags.is_deprecated() {
        context.collector.report_with_code(
            IssueCode::DeprecatedConstant,
            Issue::warning(format!("Using deprecated constant: `{display}`."))
                .with_annotation(Annotation::primary(span).with_message("This constant is deprecated."))
                .with_note("Consider using an alternative constant or variable.")
                .with_help(format!("Check `{display}` documentation for alternatives or updates.")),
        );
    }

    crate::utils::experimental::check_experimental_constant(context, block_context, &metadata.flags, name, span);
    crate::utils::availability::check_constant_availability(context, metadata, &display, span);

    constant_type(context, metadata, name)
}

/// The type of the constant `metadata`, whose full name is `name`: the platform's type for a constant whose value
/// depends on the machine, else its declared type, else the type of its value.
fn constant_type<A>(context: &Context<'_, '_, A>, metadata: &ConstantMetadata, name: &[u8]) -> TUnion
where
    A: Arena,
{
    let mut constant_type = get_platform_constant_type(name).unwrap_or_else(|| match &metadata.type_metadata {
        Some(declared) => declared.type_union.clone(),
        None => metadata.inferred_type.clone().unwrap_or_else(get_mixed),
    });
    expander::expand_union(context.codebase, &mut constant_type, &TypeExpansionOptions::default());

    constant_type
}

/// The property access that `field` runs as in its accessor: `$this->name`, which PHP reads and writes as the storage
/// inside the property's own hook. It has the span of `field`, so its type is the type of `field`. Returns `None`
/// outside an accessor, where the semantic checks report `field`.
pub(crate) fn field_storage<'arena, A>(
    field: &ConstantAccess<'arena>,
    context: &Context<'_, 'arena, A>,
    block_context: &BlockContext<'_>,
) -> Option<&'arena Expression<'arena>>
where
    A: Arena,
{
    let (property, _) = block_context.scope.get_property_hook()?;
    let span = field.span();
    // A static property's storage is the class's own static, `self::$name`.
    if block_context
        .scope
        .get_class_like()
        .and_then(|class| class.properties.get(&property))
        .is_some_and(|metadata| metadata.flags.is_static())
    {
        let name = context.arena.alloc_slice_copy(property.as_bytes());
        let class = context.arena.alloc(Expression::Self_(Keyword { span, value: b"self" }));

        return Some(context.arena.alloc(Expression::Access(Access::StaticProperty(StaticPropertyAccess {
            class,
            double_colon: span,
            property: Variable::Direct(DirectVariable { span, name }),
        }))));
    }

    let name = context.arena.alloc_slice_copy(property.as_bytes().strip_prefix(b"$")?);
    let this = context.arena.alloc(Expression::Variable(Variable::Direct(DirectVariable { span, name: b"$this" })));

    Some(context.arena.alloc(Expression::Access(Access::Property(PropertyAccess {
        object: this,
        arrow: span,
        property: ClassLikeMemberSelector::Identifier(LocalIdentifier { span, value: name }),
    }))))
}
