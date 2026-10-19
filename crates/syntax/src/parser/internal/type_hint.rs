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
use crate::cst::cst::ReadonlyHint;
use crate::cst::cst::TupleElementHint;
use crate::cst::cst::TupleHint;
use crate::cst::cst::TypeArgumentList;
use crate::cst::cst::TypeParameter;
use crate::cst::cst::TypeParameterBound;
use crate::cst::cst::TypeParameterList;
use crate::cst::cst::UnionHint;
use crate::cst::sequence::TokenSeparatedSequence;
use crate::error::ParseError;
use crate::error::SyntaxError;
use crate::parser::Parser;
use crate::token::Token;
use crate::token::TokenKind;
use mago_allocator::prelude::*;
use mago_syntax_core::stack::ensure_sufficient_stack;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn is_at_type_hint(&mut self) -> Result<bool, ParseError> {
        if self.is_at_generic_hint()? || self.is_at_function_hint()? || self.is_at_readonly_hint()? {
            return Ok(true);
        }
        if self.is_at_tuple_hint()? {
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

    /// Whether `written` names the scalar type `name`. PHP reads it ignoring case, and PHP# writes it in lowercase
    /// only, spec section 24, so `Int`, `Float`, `Bool` and `String` in a `.sharp` file are classes, as `(Int)` is.
    fn is_scalar_name(&self, written: &[u8], name: &[u8]) -> bool {
        if self.dialect.is_sharp() { written == name } else { written.eq_ignore_ascii_case(name) }
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
            T!["readonly"] if self.is_at_readonly_hint()? => {
                let readonly = self.expect_any_keyword()?;
                let hint = ensure_sufficient_stack(|| self.parse_type_hint_inner(in_expression))?;

                // `readonly` covers the whole type after it, its `?` and its unions included.
                return Ok(Hint::Readonly(ReadonlyHint { readonly, hint: self.arena.alloc(hint) }));
            }
            T!["?"] => {
                let nullable = self.parse_nullable_type_hint()?;
                if self.dialect.is_sharp() {
                    self.report_unexpected_in_sharp(nullable.question_mark);
                }

                Hint::Nullable(nullable)
            }
            T!["("] => Hint::Parenthesized(self.parse_parenthesized_type_hint()?),
            T!["["] if self.dialect.is_sharp() => Hint::Tuple(self.parse_tuple_hint()?),
            _ if self.is_at_php_keyword(T!["array"])? => Hint::Array(self.expect_any_keyword()?),
            _ if self.is_at_php_keyword(T!["callable"])? => Hint::Callable(self.expect_any_keyword()?),
            T!["null"] => Hint::Null(self.expect_any_keyword()?),
            T!["true"] => Hint::True(self.expect_any_keyword()?),
            T!["false"] => Hint::False(self.expect_any_keyword()?),
            T!["static"] => Hint::Static(self.expect_any_keyword()?),
            T!["self"] => Hint::Self_(self.expect_any_keyword()?),
            T!["parent"] => Hint::Parent(self.expect_any_keyword()?),
            T![Identifier] if self.is_at_function_hint()? => Hint::Function(self.parse_function_hint()?),
            T![Identifier] if self.is_at_generic_hint()? => Hint::Generic(self.parse_generic_hint()?),
            T!["enum" | "from" | QualifiedIdentifier | FullyQualifiedIdentifier] => {
                Hint::Identifier(self.parse_identifier()?)
            }
            T![Identifier] => match token.value {
                val if val.eq_ignore_ascii_case(b"void") => Hint::Void(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"never") => Hint::Never(self.parse_local_identifier()?),
                val if self.is_scalar_name(val, b"float") => Hint::Float(self.parse_local_identifier()?),
                val if self.is_scalar_name(val, b"bool") => Hint::Bool(self.parse_local_identifier()?),
                val if self.is_scalar_name(val, b"int") => Hint::Integer(self.parse_local_identifier()?),
                val if self.is_scalar_name(val, b"string") => Hint::String(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"object") => Hint::Object(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"mixed") => Hint::Mixed(self.parse_local_identifier()?),
                // PHP# writes PHP's `mixed` as `Any`, as spec section 24 decides.
                b"Any" if self.dialect.is_sharp() => Hint::Mixed(self.parse_local_identifier()?),
                val if val.eq_ignore_ascii_case(b"iterable") => Hint::Iterable(self.parse_local_identifier()?),
                // Spec section 20: a case of an enum is a type of its own, written `PaymentResult.Paid`. A longer
                // dotted name is a full name, which the analyzer refuses with the import to add, spec section 23. A
                // name followed by `(` or `<` is a named constructor's, as in `public Money.of(int cents)`.
                _ if self.dialect.is_sharp()
                    && self.stream.peek_kind(1)? == Some(T!["."])
                    && !self.peek_dotted_name()?.1.is_some_and(|after| matches!(after.kind, T!["(" | "<"])) =>
                {
                    Hint::Identifier(self.parse_dotted_identifier()?)
                }
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

                match right {
                    // PHP#'s `&` binds tighter than `|`, as TypeScript's does, spec section 24, so `A & B | C` joins
                    // `A` with `B` only, and the union holds that intersection and `C`.
                    Hint::Union(union) if self.dialect.is_sharp() => Hint::Union(UnionHint {
                        left: self.arena.alloc(Hint::Intersection(IntersectionHint {
                            left: self.arena.alloc(left),
                            ampersand,
                            right: union.left,
                        })),
                        pipe: union.pipe,
                        right: union.right,
                    }),
                    right => Hint::Intersection(IntersectionHint {
                        left: self.arena.alloc(left),
                        ampersand,
                        right: self.arena.alloc(right),
                    }),
                }
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
                matches!(token.kind, T![")" | "]" | "}" | "{" | "," | ";" | "=>"])
                    || (token.kind == T![Identifier] && matches!(token.value, b"when" | b"and" | b"or"))
            })
        };

        Ok(match self.stream.lookahead(1)? {
            next if ends_pattern(next) => false,
            Some(name) if name.kind == T![Identifier] => !ends_pattern(self.stream.lookahead(2)?),
            next => next.is_some_and(|token| Self::is_at_start_of_expression(token.kind)),
        })
    }

    /// Whether a PHP# readonly type starts here, spec section 13: `readonly` followed by a type, as in
    /// `readonly Order order`. Before `class`, `struct`, another modifier or anything else, `readonly` is a modifier,
    /// which the checker refuses on a class and a field with the message that names `{ get; }`.
    pub(crate) fn is_at_readonly_hint(&mut self) -> Result<bool, ParseError> {
        if !self.dialect.is_sharp() || self.stream.peek_kind(0)? != Some(T!["readonly"]) {
            return Ok(false);
        }

        Ok(match self.stream.lookahead(1)? {
            Some(token) if token.kind == T![Identifier] => !matches!(
                token.value,
                b"struct" | b"virtual" | b"override" | b"required" | b"extern" | b"lazy" | b"internal" | b"foreign"
            ),
            Some(token) => {
                // `readonly readonly Order` reads as one readonly type inside another, which the checker refuses.
                matches!(
                    token.kind,
                    T!["("
                        | "self"
                        | "array"
                        | "callable"
                        | "readonly"
                        | QualifiedIdentifier
                        | FullyQualifiedIdentifier]
                )
            }
            None => false,
        })
    }

    /// Whether a PHP# tuple type starts here, which PHP# writes in the brackets that also open an attribute list and a
    /// list: a `[` whose closing `]`, and an optional `?` written right after it, are followed by a name and then by
    /// `(`, `,`, `)`, `=`, `;` or `{`, as in `[Money, int] totals()` or `[Money, int] pair = …`. After an attribute list,
    /// a type and a name follow, as in `[Searchable] string name`, or the class's own name and `(`, as in
    /// `[Deprecated] Report(…)`, its constructor. After a list an operator or `.` follows.
    pub(crate) fn is_at_tuple_hint(&self) -> Result<bool, ParseError> {
        let Some((name, after)) = self.peek_after_tuple_hint()? else {
            return Ok(false);
        };
        if after.kind == T!["("] && self.state.members_name.as_deref() == Some(name.value) {
            return Ok(false);
        }

        Ok(matches!(after.kind, T!["(" | "," | ")" | "=" | ";" | "{"]))
    }

    /// Whether a PHP# loop variable with a tuple type starts here, as in `[string, int] entry of stock`: the name after
    /// the type is followed by `of`, or TypeScript's `in`, which `parse_of_keyword` reports. In `[key, count] of stock`
    /// no name follows the brackets, so they unpack each element.
    pub(crate) fn is_at_tuple_loop_variable(&self) -> Result<bool, ParseError> {
        Ok(self
            .peek_after_tuple_hint()?
            .is_some_and(|(_, after)| after.kind == T![Identifier] && matches!(after.value, b"of" | b"in")))
    }

    /// The number of names joined by `.` that start here, and the token after them, as `2` and `(` for `Money.of(`, or
    /// `None` at the end of the file. A `.` that no name follows is the token after them.
    pub(crate) fn peek_dotted_name(&self) -> Result<(usize, Option<Token<'_>>), ParseError> {
        let mut tokens = self.stream.peek_tokens().skip(1);
        let mut parts = 1;
        loop {
            match tokens.next().transpose()? {
                Some(dot) if dot.kind == T!["."] => match tokens.next().transpose()? {
                    Some(name) if name.kind.is_identifier_maybe_reserved() => parts += 1,
                    _ => return Ok((parts, Some(dot))),
                },
                after => return Ok((parts, after)),
            }
        }
    }

    /// The name after the PHP# tuple type that may start here, and the token after that name: the type, as
    /// [`Self::peek_after_tuple_type`] reads it, is followed by a name.
    fn peek_after_tuple_hint(&self) -> Result<Option<(Token<'_>, Token<'_>)>, ParseError> {
        let Some(TupleTypeEnd { after: name, mut tokens, .. }) = self.peek_after_tuple_type()? else {
            return Ok(None);
        };
        if !name.kind.is_identifier_maybe_reserved() {
            return Ok(None);
        }

        Ok(tokens.next().transpose()?.map(|after| (name, after)))
    }

    /// Whether a PHP# type pattern that starts with a tuple type starts here: the type, as
    /// [`Self::peek_after_tuple_type`] reads it, is a union, as in `[int, string]|int`, or a name follows it, as in
    /// `[int, string] pair`. Any other `[` starts a list pattern.
    pub(crate) fn is_at_tuple_type_pattern(&self) -> Result<bool, ParseError> {
        let Some(TupleTypeEnd { after, union, mut tokens }) = self.peek_after_tuple_type()? else {
            return Ok(false);
        };
        if union {
            return Ok(true);
        }

        Ok(match after.value {
            b"when" | b"and" | b"or" => false,
            b"is" => matches!(tokens.next().transpose()?.map(|token| token.kind), Some(T![")" | "=>" | ";" | ","])),
            _ => after.kind.is_identifier_maybe_reserved(),
        })
    }

    /// The token after the PHP# type that may start here with a tuple type, which PHP# writes in the brackets that also
    /// open an attribute list, a list and a list pattern: a `[` and its closing `]`, an optional `?` written right after
    /// it, then each `|` of a union and the type after it, whose brackets and angle brackets it skips whole.
    fn peek_after_tuple_type(
        &self,
    ) -> Result<Option<TupleTypeEnd<'_, impl Iterator<Item = Result<Token<'_>, SyntaxError>>>>, ParseError> {
        if !self.dialect.is_sharp() {
            return Ok(None);
        }

        let mut tokens = self.stream.peek_tokens();
        if !matches!(tokens.next().transpose()?, Some(token) if token.kind == T!["["]) {
            return Ok(None);
        }
        let Some(mut last) = skip_to_close(&mut tokens, T!["["], T!["]"])? else {
            return Ok(None);
        };
        let mut union = false;
        let mut next = tokens.next().transpose()?;
        loop {
            if let Some(question_mark) = next
                && question_mark.kind == T!["?"]
                && question_mark.start.offset == last.start.offset + last.value.len() as u32
            {
                next = tokens.next().transpose()?;
            }
            if next.is_none_or(|token| token.kind != T!["|"]) {
                break;
            }

            union = true;
            let Some(member) = tokens.next().transpose()? else {
                return Ok(None);
            };
            if member.kind == T!["["] {
                let Some(close) = skip_to_close(&mut tokens, T!["["], T!["]"])? else {
                    return Ok(None);
                };
                last = close;
                next = tokens.next().transpose()?;
                continue;
            }
            if !member.kind.is_identifier_maybe_reserved() {
                return Ok(None);
            }

            last = member;
            next = tokens.next().transpose()?;
            while next.is_some_and(|token| token.kind == T!["."]) {
                let Some(part) = tokens.next().transpose()?.filter(|part| part.kind.is_identifier_maybe_reserved())
                else {
                    return Ok(None);
                };
                last = part;
                next = tokens.next().transpose()?;
            }
            if next.is_some_and(|token| token.kind == T!["<"]) {
                let Some(close) = skip_to_close(&mut tokens, T!["<"], T![">"])? else {
                    return Ok(None);
                };
                last = close;
                next = tokens.next().transpose()?;
            }
        }

        Ok(next.map(|after| TupleTypeEnd { after, union, tokens }))
    }

    /// Parses a PHP# tuple type, spec section 30: element types in brackets, each followed by its optional name, as in
    /// `[Money total, int count]`.
    fn parse_tuple_hint(&mut self) -> Result<TupleHint<'arena>, ParseError> {
        let left_bracket = self.stream.eat_span(T!["["])?;
        let mut elements = Vec::new_in(self.arena);
        let mut commas = Vec::new_in(self.arena);
        loop {
            let hint = self.parse_type_hint()?;
            let name = if self.stream.peek_kind(0)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()) {
                Some(self.parse_local_identifier()?)
            } else {
                None
            };
            elements.push(TupleElementHint { hint, name });
            if !self.stream.is_at(T![","])? {
                break;
            }

            commas.push(self.stream.consume()?);
        }

        Ok(TupleHint {
            left_bracket,
            elements: TokenSeparatedSequence::new(elements, commas),
            right_bracket: self.stream.eat_span(T!["]"])?,
        })
    }

    /// Whether a PHP# type with type arguments starts here: a name, such as `List` or `Class`, followed by `<`.
    pub(crate) fn is_at_generic_hint(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self.stream.peek_kind(0)? == Some(T![Identifier])
            && self.stream.peek_kind(1)? == Some(T!["<"]))
    }

    /// Parses a PHP# type with type arguments, as in `Map<string, List<Line>>`, and the case of a generic enum it may
    /// name after them, as in `Result<Order, PaymentError>.Ok`.
    fn parse_generic_hint(&mut self) -> Result<GenericHint<'arena>, ParseError> {
        let name = self.parse_local_identifier()?;
        let type_arguments = self.parse_type_argument_list()?;
        // The `.` follows this list's own `>`, which a `>>` that closes an outer list too does not end with.
        let case =
            if self.stream.lookahead(0)?.is_some_and(|token| {
                token.kind == T!["."] && token.start.offset == type_arguments.greater_than.end.offset
            }) && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved())
            {
                self.stream.consume()?;

                Some(self.parse_local_identifier()?)
            } else {
                None
            };

        Ok(GenericHint { name, type_arguments, case })
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
            && self.stream.peek_kind_after_type_arguments(0)? == Some(T!["("])
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
        // C# writes an attribute on a type parameter, as in `<[Marker] T>`, which is no PHP# syntax, spec section 1.
        let attribute_lists = self.parse_typeless_attribute_list_sequence()?;
        if let Some(first) = attribute_lists.first() {
            self.report_unexpected_in_sharp(first.span());
        }

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

    /// Whether a PHP# function type starts here: `Function` followed by `<`.
    pub(crate) fn is_at_function_hint(&mut self) -> Result<bool, ParseError> {
        Ok(self.dialect.is_sharp()
            && self
                .stream
                .lookahead(0)?
                .is_some_and(|token| token.kind == T![Identifier] && token.value == b"Function")
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

/// Where a PHP# type that starts with a tuple type ends: the token after it, whether the type is a union, and the
/// tokens after that one.
struct TupleTypeEnd<'token, I> {
    after: Token<'token>,
    union: bool,
    tokens: I,
}

/// Reads `tokens` up to the `close` that closes the `open` read just before them, and returns that `close`. A `>>`
/// closes two angle brackets. None when the file, a statement or a block ends first.
fn skip_to_close<'token>(
    tokens: &mut impl Iterator<Item = Result<Token<'token>, SyntaxError>>,
    open: TokenKind,
    close: TokenKind,
) -> Result<Option<Token<'token>>, ParseError> {
    let mut depth = 1usize;
    for token in tokens {
        let token = token?;
        let closes = match token.kind {
            kind if kind == open => {
                depth += 1;
                0
            }
            kind if kind == close => 1,
            T![">>"] if open == T!["<"] => 2,
            T![";" | "{" | "}"] => return Ok(None),
            _ => 0,
        };
        if closes >= depth {
            return Ok(Some(token));
        }
        depth -= closes;
    }

    Ok(None)
}
