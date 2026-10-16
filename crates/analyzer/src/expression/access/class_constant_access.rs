use mago_allocator::Arena;
use mago_codex::ttype::add_optional_union_type;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::object::TObject;
use mago_codex::ttype::atomic::object::named::TNamedObject;
use mago_codex::ttype::expander::StaticClassType;
use mago_codex::ttype::expander::TypeExpansionOptions;
use mago_codex::ttype::expander::expand_union;
use mago_codex::ttype::get_mixed;
use mago_codex::ttype::get_never;
use mago_codex::ttype::union::TUnion;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Span;
use mago_syntax::cst::ClassConstantAccess;
use mago_syntax::cst::ClassLikeConstantSelector;
use mago_syntax::cst::Expression;

use crate::analyzable::Analyzable;
use crate::artifacts::AnalysisArtifacts;
use crate::code::IssueCode;
use crate::context::Context;
use crate::context::block::BlockContext;
use crate::error::AnalysisError;
use crate::expression::ancestor_type_arguments;
use crate::resolver::class_constant::resolve_class_constants;
use crate::resolver::class_name::generic_class_type;
use crate::utils::names::display_type;

impl<'ast, 'arena> Analyzable<'ast, 'arena> for ClassConstantAccess<'arena> {
    fn analyze<'ctx, A>(
        &'ast self,
        context: &mut Context<'ctx, 'arena, A>,
        block_context: &mut BlockContext<'ctx>,
        artifacts: &mut AnalysisArtifacts,
    ) -> Result<(), AnalysisError>
    where
        A: Arena,
    {
        analyze_class_constant_access(context, block_context, artifacts, self.class, &self.constant, self.span(), false)
    }
}

/// Analyzes the class constant or enum case `class::constant`, which spans `span`: PHP's `Class::NAME`, or PHP#'s
/// `Class.name` when the class has a constant or enum case by that name. A PHP# read through a class value, `type.name`,
/// analyzed its class value to find it is one, so it is `class_is_analyzed`.
pub(crate) fn analyze_class_constant_access<'ctx, 'arena, A>(
    context: &mut Context<'ctx, 'arena, A>,
    block_context: &mut BlockContext<'ctx>,
    artifacts: &mut AnalysisArtifacts,
    class: &Expression<'arena>,
    constant: &ClassLikeConstantSelector<'arena>,
    span: Span,
    class_is_analyzed: bool,
) -> Result<(), AnalysisError>
where
    A: Arena,
{
    let resolution = resolve_class_constants(context, block_context, artifacts, class, constant, class_is_analyzed)?;

    let mut resulting_type = if resolution.has_ambiguous_path { Some(get_mixed()) } else { None };
    for resolved_constant in resolution.constants {
        resulting_type =
            Some(add_optional_union_type(resolved_constant.const_type, resulting_type.as_ref(), context.codebase));
    }

    if let Some(resulting_type) = &mut resulting_type {
        expand_union(
            context.codebase,
            resulting_type,
            &TypeExpansionOptions {
                self_class: block_context.scope.get_class_like_name(),
                static_class_type: if let Some(calling_class) = block_context.scope.get_class_like_name() {
                    StaticClassType::Name(calling_class)
                } else {
                    StaticClassType::None
                },
                ..Default::default()
            },
        );
    }

    if context.dialect.is_sharp()
        && let Some(resulting_type) = &mut resulting_type
    {
        give_case_type_arguments(context, artifacts, class, span, resulting_type);
    }

    artifacts.set_expression_type(
        &span,
        if resolution.has_invalid_path { get_never() } else { resulting_type.unwrap_or_else(get_mixed) },
    );

    Ok(())
}

/// Spec section 31: a plain case of a generic enum is one object for each list of type arguments, which the read
/// `class.case` at `span` gives `case_type`: those it writes, as `Lookup<int>.Missing`, or else those of the declared
/// type it goes into, as `Lookup<int> lookup = Lookup.Missing;`. The bridge reads them where the analysis records them.
/// A read that has neither is refused, as a call none of whose type arguments is fixed is.
fn give_case_type_arguments<A>(
    context: &mut Context<'_, '_, A>,
    artifacts: &mut AnalysisArtifacts,
    class: &Expression<'_>,
    span: Span,
    case_type: &mut TUnion,
) where
    A: Arena,
{
    let codebase = context.codebase;
    let [TAtomic::Object(TObject::Named(case))] = case_type.types.as_ref() else {
        return;
    };
    let Some((case_class, r#enum)) = codebase
        .get_class_like(case.name.as_bytes())
        .filter(|class| class.flags.is_enum_case_class() && !class.template_types.is_empty())
        .and_then(|class| Some((class, codebase.get_class_like(class.direct_parent_class?.as_bytes())?)))
    else {
        return;
    };

    let arguments = match generic_class_type(artifacts, class) {
        Some(written) => written.get_type_parameters().map(<[_]>::to_vec),
        None => artifacts.get_expected_type(&span).and_then(|expected| {
            expected.types.iter().find_map(|atomic| ancestor_type_arguments(codebase, atomic, r#enum))
        }),
    };
    let Some(arguments) = arguments else {
        let case_name = display_type(context, case_type);
        let names: Vec<String> = case_class.template_types.keys().map(|name| format!("`{name}`")).collect();
        let noun = if names.len() == 1 { "argument" } else { "arguments" };
        context.collector.report_with_code(
            IssueCode::MissingTemplateParameter,
            Issue::error(format!("Nothing fixes the type {noun} {} of `{case_name}`.", names.join(", ")))
                .with_annotation(Annotation::primary(span).with_message("The case is read here"))
                .with_note(
                    "A plain case of a generic enum takes its type arguments from the ones it writes and from the declared type it goes into.",
                )
                .with_help(format!("Write the type {noun} on the enum, or give the case a declared type.")),
        );

        return;
    };

    *case_type = TUnion::from_atomic(TAtomic::Object(TObject::Named(TNamedObject::new_with_type_parameters(
        case.name,
        Some(arguments.clone()),
    ))));
    artifacts.inferred_type_arguments.insert((span.start.offset, span.end.offset), arguments);
}

#[cfg(test)]
mod tests {
    use indoc::indoc;

    use crate::code::IssueCode;
    use crate::test_analysis;

    test_analysis! {
        name = const_access_on_undefined_class,
        code = indoc! {"
            <?php

            $_ = NonExistentClass::SOME_CONST;
        "},
        issues = [
            IssueCode::NonExistentClassLike,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_self_outside_class,
        code = indoc! {"
            <?php

            $_ = self::SOME_CONST;
        "},
        issues = [
            IssueCode::SelfOutsideClassScope,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_static_outside_class,
        code = indoc! {"
            <?php

            $_ = static::SOME_CONST;
        "},
        issues = [
            IssueCode::StaticOutsideClassScope,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_parent_outside_class,
        code = indoc! {"
            <?php

            $_ = parent::SOME_CONST;
        "},
        issues = [
            IssueCode::ParentOutsideClassScope,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_dynamic_class_unknown_type,
        code = indoc! {"
            <?php

            $const = $unknownVar::{KNOWN_CONST};
        "},
        issues = [
            IssueCode::UndefinedVariable,
            IssueCode::NonExistentConstant,
            IssueCode::UnknownConstantSelectorType,
            IssueCode::MixedAssignment,
        ]
    }

    test_analysis! {
        name = const_access_dynamic_const_name_unknown_type,
        code = indoc! {"
            <?php

            class MyClass { const C = 1; }

            $const = MyClass::{$unknownConstName};
        "},
        issues = [
            IssueCode::UndefinedVariable,
            IssueCode::InvalidConstantSelector,
            IssueCode::MixedAssignment,
        ]
    }

    test_analysis! {
        name = const_access_dynamic_const_name_non_literal_string,
        code = indoc! {r#"
            <?php

            class MyClass { const GREETING = "hello"; }

            function getName(): string { return "GREETING"; }
            $constName = getName();

            $const = MyClass::{$constName};
        "#},
        issues = [
            IssueCode::StringConstantSelector,
            IssueCode::MixedAssignment,
        ]
    }

    test_analysis! {
        name = const_access_dynamic_const_name_invalid_type,
        code = indoc! {"
            <?php

            class MyClass { const C = 1; }
            $constName = 123;
            $_ = MyClass::{$constName};
        "},
        issues = [
            IssueCode::InvalidConstantSelector,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_class_on_string_variable,
        code = indoc! {r#"
            <?php
            $className = "MyClass";
            $_ = $className::class;
        "#},
        issues = [
            IssueCode::InvalidClassConstantOnString,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = class_magic_constant_on_undefined_class,
        code = indoc! {"
            <?php

            $_ = NonExistentClass::class;
        "},
        issues = [
            IssueCode::NonExistentClassLike,
        ]
    }

    test_analysis! {
        name = const_access_on_generic_object_type,
        code = indoc! {"
            <?php
            class stdClass {} // stub

            function get_some_object(): object { return new stdClass(); }

            $obj = get_some_object();
            $const = $obj::SOME_CONST;
        "},
        issues = [
            IssueCode::AmbiguousClassLikeConstantAccess,
            IssueCode::MixedAssignment,
        ]
    }

    test_analysis! {
        name = const_access_on_generic_class_string,
        code = indoc! {"
            <?php
            /** @param class-string $cs */
            function process_class_string(string $cs) {
                return $cs::SOME_CONST;
            }
        "},
        issues = [
            IssueCode::AmbiguousClassLikeConstantAccess,
        ]
    }

    test_analysis! {
        name = const_access_undefined_on_enum,
        code = indoc! {"
            <?php
            enum Suit { case Hearts; }
            $_ = Suit::Diamonds; // Accessing 'Diamonds' like a const/case
        "},
        issues = [
            IssueCode::NonExistentClassConstant,
            IssueCode::ImpossibleAssignment,
        ]
    }

    test_analysis! {
        name = const_access_interface_const_via_class,
        code = indoc! {r#"
            <?php

            interface MyInterface { const IFACE_CONST = "hello"; }
            class ImplementingClass implements MyInterface {}

            $_ = ImplementingClass::IFACE_CONST;
        "#},
    }

    test_analysis! {
        name = const_access_on_interface_directly,
        code = indoc! {"
            <?php
            interface ConstantsInterface { const MY_CONST = 42; }
            $_ = ConstantsInterface::MY_CONST;
        "},
    }

    test_analysis! {
        name = const_type_inference,
        code = indoc! {r#"
            <?php

            class A
            {
                public const FOO = <<<'XML'
                    <?xml version="1.0" encoding="UTF-8"?>
                    <foo>
                        <bar>baz</bar>
                    </foo>
                XML;

                /**
                 * @return non-empty-string
                 */
                public function getFoo(): string
                {
                    return self::FOO;
                }
            }
        "#},
    }

    test_analysis! {
        name = const_var_type,
        code = indoc! {"
            <?php

            class NumberFormatter
            {
                public const int CURRENCY = 1;
                public const int DECIMAL = 2;

                private string $locale;
                private int $style;

                public function __construct(string $locale, int $style)
                {
                    $this->locale = $locale;
                    $this->style = $style;
                }

                public function getLocale(): string { return $this->locale; }
                public function getStyle(): int { return $this->style; }

                public function formatCurrency(float $_value, string $_currency): string|false
                {
                    return false;
                }
            }

            enum PremiumBasisCode: string
            {
                case ADMISSIONS = 'admissions';
                case AREA = 'area';
                case COST = 'cost';
                case EACH = 'each';
                case FLAT = 'flat';
                case OTHER = 'other';
                case PAYROLL = 'payroll';
                case RECEIPTS = 'receipts';
                case SALES = 'sales';
                case UNITS = 'units';
            }

            class ExposureFormatter
            {
                /**
                 * @var array<string, int>
                 */
                private const array PREFIXES = [
                    PremiumBasisCode::ADMISSIONS->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::AREA->value => NumberFormatter::DECIMAL,
                    PremiumBasisCode::COST->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::EACH->value => NumberFormatter::DECIMAL,
                    PremiumBasisCode::FLAT->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::OTHER->value => NumberFormatter::DECIMAL,
                    PremiumBasisCode::PAYROLL->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::RECEIPTS->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::SALES->value => NumberFormatter::CURRENCY,
                    PremiumBasisCode::UNITS->value => NumberFormatter::DECIMAL,
                ];

                private string $locale;

                public function __construct(string $locale = 'en_US')
                {
                    $this->locale = $locale;
                }

                public function format(PremiumBasisCode $basisCode, float $value): null|string
                {
                    $formatter = new NumberFormatter($this->locale, self::PREFIXES[$basisCode->value]);
                    $result = $formatter->formatCurrency($value, 'USD');

                    if (false === $result) {
                        return null;
                    } else {
                        return $result;
                    }
                }
            }
        "},
    }
}
