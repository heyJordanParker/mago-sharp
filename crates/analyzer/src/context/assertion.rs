use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_span::Span;
use mago_word::Word;

use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_codex::ttype::atomic::TAtomic;
use mago_codex::ttype::atomic::array::TArray;
use mago_syntax::cst::ArrayAccess;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Node;
use mago_syntax::dialect::Dialect;
use mago_syntax::utils::pattern::PatternNames;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::utils::pattern::php_shape;

use crate::artifacts::AnalysisArtifacts;
use crate::utils::expression::get_expression_id;

#[derive(Debug)]
pub struct AssertionContext<'ctx, 'arena, A> {
    pub resolved_names: &'ctx ResolvedNames<'arena>,
    pub arena: &'arena A,
    pub codebase: &'ctx CodebaseMetadata,
    pub this_class_name: Option<Word>,
    pub trust_existence_checks: bool,
    /// How many hidden variables the PHP# pattern forms around the expression hold.
    pub temporaries: u32,
    /// The language of the file, which decides what `==` tests.
    pub dialect: Dialect,
}

impl<A> AssertionContext<'_, '_, A> {
    #[inline]
    pub fn get_expression_id(&self, expression: &Expression<'_>) -> Option<Word> {
        get_expression_id(expression, self.this_class_name, self.resolved_names, Some(self.codebase))
    }
}

impl<'arena, A> AssertionContext<'_, 'arena, A>
where
    A: Arena,
{
    /// The PHP a PHP# `is`, `as` or `match` runs as, which the analyzer analyzes and narrows on. `None` for any other
    /// node, and for a form the slice refuses. `artifacts` hold the type of the value an index it tests reads.
    pub fn php_shape<'shape>(&self, node: Node<'_, 'shape>, artifacts: &AnalysisArtifacts) -> Option<PhpShape<'shape>>
    where
        'arena: 'shape,
    {
        let names = CodebasePatternNames { names: self.resolved_names, codebase: self.codebase, artifacts };

        php_shape(self.arena, node, self.temporaries, &names)
    }
}

/// What the PHP of a PHP# pattern depends on, answered from a file's resolved names, the codebase and the types the
/// analysis gave, for the analyzer and the bridge alike.
pub struct CodebasePatternNames<'names, 'arena> {
    pub names: &'names ResolvedNames<'arena>,
    pub codebase: &'names CodebaseMetadata,
    pub artifacts: &'names AnalysisArtifacts,
}

impl PatternNames for CodebasePatternNames<'_, '_> {
    fn reads_quietly(&self, access: &ArrayAccess<'_>) -> bool {
        !self.artifacts.get_expression_type(access.array).is_some_and(|indexed| {
            indexed.types.iter().all(|atomic| matches!(atomic, TAtomic::Array(TArray::List(_)) | TAtomic::Null))
        })
    }

    fn is_local(&self, span: Span) -> bool {
        matches!(self.names.binding(&span), Some(Binding::Local(_)))
    }

    fn case_fields(&self, span: Span) -> Option<Vec<Vec<u8>>> {
        let class = self.codebase.get_class_like(self.names.resolve(&span)?)?;
        if !class.flags.is_enum_case_class() {
            return None;
        }
        let Some(constructor) = self.codebase.get_method(class.name.as_bytes(), b"__construct") else {
            unreachable!("a case's class has a constructor, which takes no field for a plain case")
        };

        Some(
            constructor
                .parameters
                .iter()
                .map(|parameter| parameter.get_name().0.as_bytes().strip_prefix(b"$").unwrap_or_default().to_vec())
                .collect(),
        )
    }
}

impl<A> Clone for AssertionContext<'_, '_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for AssertionContext<'_, '_, A> {}
