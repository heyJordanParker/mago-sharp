use mago_database::file::HasFileId;
use mago_span::HasSpan;
use mago_span::Span;

use crate::T;
use crate::cst::cst::FunctionHint;
use crate::cst::cst::GenericHint;
use crate::cst::cst::Hint;
use crate::cst::cst::Identifier;
use crate::cst::cst::IntersectionHint;
use crate::cst::cst::Keyword;
use crate::cst::cst::LocalIdentifier;
use crate::cst::cst::NullableHint;
use crate::cst::cst::ParenthesizedHint;
use crate::cst::cst::TypeArgumentList;
use crate::cst::cst::TypeParameter;
use crate::cst::cst::TypeParameterBound;
use crate::cst::cst::TypeParameterList;
use crate::cst::cst::UnionHint;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::parser::Parser;
use crate::token::Token;
use mago_allocator::prelude::*;
use mago_syntax_core::stack::ensure_sufficient_stack;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn is_at_type_hint(&mut self) -> Result<bool, ParseError> {
        if self.is_at_generic_hint()? || self.is_at_function_hint()? {
            return Ok(true);
        }

        Ok(matches!(
            self.stream.peek_kind(0)?,
            Some(T!["?"
                | "("
                | "array"
                | "callable"
                | "null"
                | "true"
                | "false"
                | "static"
                | "self"
                | "parent"
                | "enum"
                | "from"
                | Identifier
                | QualifiedIdentifier
                | FullyQualifiedIdentifier])
        ))
    }

    pub(crate) fn parse_optional_type_hint(&mut self) -> Result<Option<Hint<'arena>>, ParseError> {
        if self.is_at_type_hint()? { Ok(Some(self.parse_type_hint()?)) } else { Ok(None) }
    }

    /// A union or intersection nests its right side, so a long one recurses once for each of its types.
    pub(crate) fn parse_type_hint(&mut self) -> Result<Hint<'arena>, ParseError> {
        ensure_sufficient_stack(|| self.parse_type_hint_inner(false))
    }

    /// Parses the type of a pattern or of `as`, which can end an expression, so a `?` after it can start a `? :`.
    pub(crate) fn parse_type_hint_in_expression(&mut self) -> Result<Hint<'arena>, ParseError> {
        ensure_sufficient_stack(|| self.parse_type_hint_inner(true))
    }

    fn parse_type_hint_inner(&mut self, in_expression: bool) -> Result<Hint<'arena>, ParseError> {
        let token = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;

        let hint = match &token.kind {
            T!["?"] => {
                let nullable = self.parse_nullable_type_hint()?;
                if self.dialect.is_sharp() {
                    self.errors.push(ParseError::PhpSyntaxInSharp(T!["?"], nullable.question_mark));
                }

                Hint::Nullable(nullable)
            }
            T!["("] => Hint::Parenthesized(self.parse_parenthesized_type_hint()?),
            T!["array"] => Hint::Array(self.expect_any_keyword()?),
            T!["callable"] => Hint::Callable(self.expect_any_keyword()?),
            T!["null"] => Hint::Null(self.expect_any_keyword()?),
            T!["true"] => Hint::True(self.expect_any_keyword()?),
            T!["false"] => Hint::False(self.expect_any_keyword()?),
            T!["static"] => Hint::Static(self.expect_any_keyword()?),
            T!["self"] => Hint::Self_(self.expect_any_keyword()?),
            T!["parent"] => Hint::Parent(self.expect_any_keyword()?),
            T![Identifier | "list" | "class"] if self.is_at_generic_hint()? => {
                Hint::Generic(self.parse_generic_hint()?)
            }
            T!["function"] if self.is_at_function_hint()? => Hint::Function(self.parse_function_hint()?),
            T!["enum" | "from" | QualifiedIdentifier | FullyQualifiedIdentifier] => {
                Hint::Identifier(self.parse_identifier()?)
            }
            T![Identifier] => match token.value {
                val if val.eq_ignore_ascii_case(b"void") => Hint::Void(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"never") => Hint::Never(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"float") => Hint::Float(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"bool") => Hint::Bool(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"int") => Hint::Integer(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"string") => Hint::String(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"object") => Hint::Object(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"mixed") => Hint::Mixed(self.parse_local_identifier()?),
                // PHP# writes PHP's `mixed` as `Any`, as spec section 24 decides.
                b"Any" if self.dialect.is_sharp() => Hint::Mixed(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"iterable") => Hint::Iterable(self.parse_local_identifier()?),
                _ => Hint::Identifier(self.parse_identifier()?),
            },
            _ => {
                return Err(self.stream.unexpected(
                    Some(token),
                    T![
                        "?",
                        "(",
                        "array",
                        "callable",
                        "null",
                        "true",
                        "false",
                        "static",
                        "self",
                        "parent",
                        "enum",
                        "from",
                        Identifier,
                        QualifiedIdentifier,
                        FullyQualifiedIdentifier,
                    ],
                ));
            }
        };

        // A `>>` that closed this type's arguments also closed the enclosing list, so nothing after it belongs here.
        if self.state.closing_angle.is_some() {
            return Ok(hint);
        }

        // PHP# writes a nullable type with `?` after it, as in `int?`.
        let hint = if self.dialect.is_sharp()
            && self.stream.is_at(T!["?"])?
            && !(in_expression && self.is_at_conditional()?)
        {
            let question_mark = self.stream.eat_span(T!["?"])?;

            Hint::Nullable(NullableHint { question_mark, hint: self.arena.alloc(hint) })
        } else {
            hint
        };

        // Effects written after a function type's `>`, as in `Function<Charge(Cart)> uses Http charge`, are an error,
        // and the parse goes on after them.
        if matches!(hint, Hint::Function(_) | Hint::Nullable(NullableHint { hint: Hint::Function(_), .. }))
            && self.stream.lookahead(0)?.is_some_and(|token| token.kind == T![Identifier] && token.value == b"uses")
            && self.stream.peek_kind(1)? == Some(T![Identifier])
            && let Some(misplaced) = self.parse_optional_uses()?
        {
            self.errors.push(ParseError::UsesAfterFunctionTypeInSharp(misplaced.span()));
        }

        let next = self.stream.lookahead(0)?;
        Ok(match next.map(|t| t.kind) {
            Some(T!["|"]) => {
                let left = hint;
                let pipe = self.stream.eat_span(T!["|"])?;
                let right = ensure_sufficient_stack(|| self.parse_type_hint_inner(in_expression))?;

                Hint::Union(UnionHint { left: self.arena.alloc(left), pipe, right: self.arena.alloc(right) })
            }
            Some(T!["&"]) if !matches!(self.stream.peek_kind(1)?, Some(T!["$variable"] | T!["..."] | T!["&"])) => {
                let left = hint;
                let ampersand = self.stream.eat_span(T!["&"])?;
                let right = ensure_sufficient_stack(|| self.parse_type_hint_inner(in_expression))?;

                Hint::Intersection(IntersectionHint {
                    left: self.arena.alloc(left),
                    ampersand,
                    right: self.arena.alloc(right),
                })
            }
            _ => hint,
        })
    }

    /// Whether the `?` after a type that can end an expression starts a `? :` instead of making the type nullable, as
    /// Roslyn's `TryEatNullableQualifierIfApplicable` decides it for C#. The type stays nullable when the `?` is followed
    /// by the end of a pattern, or by a name and then the end of a pattern, as in `value is string? text)`. Otherwise a
    /// `?` followed by anything that starts an expression starts a `? :`, as in `value is string ? text : ""`. A PHP#
    /// pattern also ends at `=>`, `when`, `and` and `or`.
    fn is_at_conditional(&mut self) -> Result<bool, ParseError> {
        let ends_pattern = |token: Option<Token<'_>>| {
            token.is_none_or(|token| {
                matches!(token.kind, T![")" | "]" | "}" | "{" | "," | ";" | "=>" | "and" | "or"])
                    || (token.kind == T![Identifier] && token.value == b"when")
            })
        };

        Ok(match self.stream.lookahead(1)? {
            next if ends_pattern(next) => false,
            Some(name) if name.kind == T![Identifier] => !ends_pattern(self.stream.lookahead(2)?),
            next => next.is_some_and(|token| Self::is_at_start_of_expression(token.kind)),
        })
    }

    /// Whether a PHP# type with type arguments starts here: a name, which may be `List`, followed by `<`. The lexer
    /// reads `Class` as PHP's `class` keyword, which starts a type only here, as in `Class<Order>` of spec section 25.
    pub(crate) fn is_at_generic_hint(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.lookahead(0)?.is_some_and(|token| {
                matches!(token.kind, T![Identifier | "list"]) || (token.kind == T!["class"] && token.value == b"Class")
            })
            && self.stream.peek_kind(1)? == Some(T!["<"]))
    }

    /// Parses a PHP# type with type arguments, as in `Map<string, List<Line>>`.
    fn parse_generic_hint(&mut self) -> Result<GenericHint<'arena>, ParseError> {
        Ok(GenericHint { name: self.parse_local_identifier()?, type_arguments: self.parse_type_argument_list()? })
    }

    /// Parses PHP# type arguments, as a type, a `new` and a call write them: `<string, List<Line>>`.
    pub(crate) fn parse_type_argument_list(&mut self) -> Result<TypeArgumentList<'arena>, ParseError> {
        let (less_than, arguments, greater_than) = self.parse_angle_bracket_list(Self::parse_type_hint)?;

        Ok(TypeArgumentList { less_than, arguments, greater_than })
    }

    /// Parses a PHP# list in angle brackets, `<a, b>`, of the items `parse_item` reads, and returns its `<`, its items
    /// and its `>`.
    fn parse_angle_bracket_list<Item>(
        &mut self,
        parse_item: impl Fn(&mut Self) -> Result<Item, ParseError>,
    ) -> Result<(Span, TokenSeparatedSequence<'arena, Item>, Span), ParseError> {
        let less_than = self.stream.eat_span(T!["<"])?;
        let mut items = Vec::new_in(self.arena);
        let mut commas = Vec::new_in(self.arena);
        loop {
            items.push(parse_item(self)?);
            if self.state.closing_angle.is_some() || !self.stream.is_at(T![","])? {
                break;
            }

            commas.push(self.stream.consume()?);
        }

        Ok((less_than, TokenSeparatedSequence::new(items, commas), self.parse_closing_angle()?))
    }

    /// Parses the PHP# type arguments after the class of `new`, as in `new PaginatedList<Order>(rows)`.
    pub(crate) fn parse_optional_type_argument_list(&mut self) -> Result<Option<TypeArgumentList<'arena>>, ParseError> {
        if self.dialect.is_sharp() && self.stream.is_at(T!["<"])? {
            Ok(Some(self.parse_type_argument_list()?))
        } else {
            Ok(None)
        }
    }

    /// Parses the PHP# type arguments of a method call, as in `Json.decode<WebhookPayload>(body)`. As C# decides, a
    /// `<` starts them only when every token up to the `>` that closes it can be part of a type and `(` follows that
    /// `>`, so `this.count < limit` stays a comparison.
    pub(crate) fn parse_optional_call_type_argument_list(
        &mut self,
    ) -> Result<Option<TypeArgumentList<'arena>>, ParseError> {
        if self.dialect.is_sharp()
            && self.stream.is_at(T!["<"])?
            && self.stream.peek_kind_after_type_arguments()? == Some(T!["("])
        {
            Ok(Some(self.parse_type_argument_list()?))
        } else {
            Ok(None)
        }
    }

    /// Parses the PHP# type parameters after the name of a class, an interface or a method, as in
    /// `<out TItem : DatabaseEntity & Shareable, TKey>`.
    pub(crate) fn parse_optional_type_parameter_list(
        &mut self,
    ) -> Result<Option<TypeParameterList<'arena>>, ParseError> {
        if !self.dialect.is_sharp() || !self.stream.is_at(T!["<"])? {
            return Ok(None);
        }

        let (less_than, parameters, greater_than) = self.parse_angle_bracket_list(Self::parse_type_parameter)?;

        Ok(Some(TypeParameterList { less_than, parameters, greater_than }))
    }

    /// Parses one PHP# type parameter. `in` and `out` are names the lexer reads as identifiers, so one is the variance
    /// only when another name follows it, as `of` is read in a `for … of` loop.
    fn parse_type_parameter(&mut self) -> Result<TypeParameter<'arena>, ParseError> {
        let variance = match self.stream.lookahead(0)? {
            Some(token)
                if token.kind == T![Identifier]
                    && matches!(token.value, b"in" | b"out")
                    && self.stream.peek_kind(1)? == Some(T![Identifier]) =>
            {
                let token = self.stream.consume()?;

                Some(Keyword { span: token.span_for(self.stream.file_id()), value: token.value })
            }
            _ => None,
        };

        Ok(TypeParameter {
            variance,
            name: self.parse_local_identifier()?,
            bound: if self.stream.is_at(T![":"])? {
                Some(TypeParameterBound { colon: self.stream.eat_span(T![":"])?, hint: self.parse_type_hint()? })
            } else {
                None
            },
        })
    }

    /// Whether a PHP# function type starts here: `Function` followed by `<`. The lexer reads `Function` as PHP's
    /// `function` keyword, which PHP# writes only here.
    pub(crate) fn is_at_function_hint(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self
                .stream
                .lookahead(0)?
                .is_some_and(|token| token.kind == T!["function"] && token.value == b"Function")
            && self.stream.peek_kind(1)? == Some(T!["<"]))
    }

    /// Parses a PHP# function type, as in `Function<Money?(Line, string)>` or `Function<Charge(Cart) uses Http>`.
    fn parse_function_hint(&mut self) -> Result<FunctionHint<'arena>, ParseError> {
        let function = self.expect_any_keyword()?;
        let less_than = self.stream.eat_span(T!["<"])?;
        let return_type = self.parse_type_hint()?;
        let mut parameters = Vec::new_in(self.arena);
        let mut commas = Vec::new_in(self.arena);
        let (left_parenthesis, right_parenthesis) =
            if self.stream.lookahead(0)?.is_some_and(|token| token.kind.is_cast()) {
                // The lexer reads `(int)` as a cast, which here is the parentheses around the one parameter type.
                let cast = self.stream.consume()?;
                let span = cast.span_for(self.stream.file_id());
                let start =
                    cast.value.iter().skip(1).position(|byte| !byte.is_ascii_whitespace()).map_or(1, |index| index + 1);
                let end = start + cast.value[start..].iter().take_while(|byte| byte.is_ascii_alphabetic()).count();
                let name =
                    LocalIdentifier { span: span.subspan(start as u32, end as u32), value: &cast.value[start..end] };
                parameters.push(match name.value {
                    b"int" => Hint::Integer(name),
                    b"float" => Hint::Float(name),
                    b"bool" => Hint::Bool(name),
                    b"string" => Hint::String(name),
                    b"void" => Hint::Void(name),
                    b"object" => Hint::Object(name),
                    b"array" => Hint::Array(Keyword { span: name.span, value: name.value }),
                    _ => Hint::Identifier(Identifier::Local(name)),
                });

                (span.subspan(0, 1), span.subspan(span.length() - 1, span.length()))
            } else {
                let left_parenthesis = self.stream.eat_span(T!["("])?;
                while !self.stream.is_at(T![")"])? {
                    parameters.push(self.parse_type_hint()?);
                    if !self.stream.is_at(T![","])? {
                        break;
                    }

                    commas.push(self.stream.consume()?);
                }

                (left_parenthesis, self.stream.eat_span(T![")"])?)
            };

        Ok(FunctionHint {
            function,
            less_than,
            return_type: self.arena.alloc(return_type),
            left_parenthesis,
            parameters: TokenSeparatedSequence::new(parameters, commas),
            right_parenthesis,
            uses: self.parse_optional_uses()?,
            greater_than: self.parse_closing_angle()?,
        })
    }

    /// Consumes the `>` that closes a type argument list. A `>>` closes this list and the enclosing one.
    fn parse_closing_angle(&mut self) -> Result<Span, ParseError> {
        if let Some(span) = self.state.closing_angle.take() {
            return Ok(span);
        }

        if self.stream.is_at(T![">>"])? {
            let span = self.stream.consume_span()?;
            self.state.closing_angle = Some(span.subspan(1, 2));

            return Ok(span.subspan(0, 1));
        }

        self.stream.eat_span(T![">"])
    }

    pub(crate) fn parse_nullable_type_hint(&mut self) -> Result<NullableHint<'arena>, ParseError> {
        let question_mark = self.stream.eat_span(T!["?"])?;
        let hint = self.parse_type_hint()?;

        Ok(NullableHint { question_mark, hint: self.arena.alloc(hint) })
    }

    pub(crate) fn parse_parenthesized_type_hint(&mut self) -> Result<ParenthesizedHint<'arena>, ParseError> {
        let left_parenthesis = self.stream.eat_span(T!["("])?;
        let hint = self.parse_type_hint()?;
        let right_parenthesis = self.stream.eat_span(T![")"])?;

        Ok(ParenthesizedHint { left_parenthesis, hint: self.arena.alloc(hint), right_parenthesis })
    }
}
