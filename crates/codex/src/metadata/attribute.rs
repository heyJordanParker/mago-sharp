use ordered_float::OrderedFloat;

use mago_span::HasSpan;
use mago_span::Span;
use mago_word::Word;

use crate::ttype::union::TUnion;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AttributeArgumentMetadata {
    pub name: Option<Word>,
    pub span: Span,
    pub name_span: Option<Span>,
    pub value_span: Option<Span>,
    pub value_type: Option<TUnion>,
    pub value: Option<ConstantExpression>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ConstantExpression {
    Null,
    Bool(bool),
    Int(i64),
    Float(OrderedFloat<f64>),
    String(Word),
    Array(Vec<(Option<ConstantExpression>, ConstantExpression)>),
    ClassName(Word),
    ClassConstant(Word, Word),
    Constant(Word),
    New(Word, Vec<(Option<Word>, ConstantExpression)>),
    /// A PHP# `X.member` with the full name `X` resolves to, at its span: a class constant, an enum case, a case object
    /// or a namespace's constant, which only the whole codebase tells apart, spec section 23.
    MemberRead(Word, Word, Span),
    Unsupported(Span),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
pub struct AttributeMetadata {
    pub name: Word,
    pub span: Span,
    pub arguments: Vec<AttributeArgumentMetadata>,
}

impl AttributeMetadata {
    /// The literal string the attribute's argument at `position` gives, or the argument `name` names, as PHP binds a
    /// positional argument before named ones: `"return_url"` of both `Key("return_url")` and
    /// `Key(name: "return_url")`. None for an argument that is no literal string or that the attribute leaves out.
    #[must_use]
    pub fn text_argument(&self, position: usize, name: &[u8]) -> Option<&[u8]> {
        match self.argument(position, name)? {
            ConstantExpression::String(text) => Some(text.as_bytes()),
            _ => None,
        }
    }

    /// Whether the argument at `position`, or the one `name` names, is the literal `true`: `omitNull` of
    /// `Key(omitNull: true)`.
    #[must_use]
    pub fn flag_argument(&self, position: usize, name: &[u8]) -> bool {
        matches!(self.argument(position, name), Some(ConstantExpression::Bool(true)))
    }

    fn argument(&self, position: usize, name: &[u8]) -> Option<&ConstantExpression> {
        self.arguments
            .iter()
            .enumerate()
            .find(|(index, argument)| match argument.name {
                Some(named) => named.as_bytes() == name,
                None => *index == position,
            })?
            .1
            .value
            .as_ref()
    }
}

impl HasSpan for AttributeMetadata {
    fn span(&self) -> Span {
        self.span
    }
}
