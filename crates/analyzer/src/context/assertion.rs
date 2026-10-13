use mago_names::ResolvedNames;
use mago_names::binding::Binding;
use mago_span::Span;
use mago_word::Word;

use mago_allocator::Arena;
use mago_codex::metadata::CodebaseMetadata;
use mago_syntax::cst::Expression;
use mago_syntax::cst::Node;
use mago_syntax::dialect::Dialect;
use mago_syntax::utils::pattern::PhpShape;
use mago_syntax::utils::pattern::php_shape;

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
    /// node, and for a form the slice refuses.
    pub fn php_shape<'shape>(&self, node: Node<'_, 'shape>) -> Option<PhpShape<'shape>>
    where
        'arena: 'shape,
    {
        let names = self.resolved_names;
        let is_local = |span: Span| matches!(names.binding(&span), Some(Binding::Local(_)));

        php_shape(self.arena, node, self.temporaries, &is_local)
    }
}

impl<A> Clone for AssertionContext<'_, '_, A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A> Copy for AssertionContext<'_, '_, A> {}
