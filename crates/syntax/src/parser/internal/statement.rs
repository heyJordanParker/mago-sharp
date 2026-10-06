use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::Span;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::T;
use crate::cst::cst::AttributeList;
use crate::cst::cst::Expression;
use crate::cst::cst::ExpressionStatement;
use crate::cst::cst::Statement;
use crate::cst::sequence::Sequence;
use crate::error::ParseError;
use crate::parser::MAX_RECURSION_DEPTH;
use crate::parser::Parser;

impl<'arena, A> Parser<'_, 'arena, A>
where
    A: Arena,
{
    pub(crate) fn parse_statement(&mut self) -> Result<Statement<'arena>, ParseError> {
        self.state.recursion_depth += 1;
        if self.state.recursion_depth > MAX_RECURSION_DEPTH {
            self.state.recursion_depth -= 1;
            let file_id = self.stream.file_id();

            let span =
                self.stream.lookahead(0)?.map(|t| t.span_for(file_id)).unwrap_or_else(|| {
                    Span::new(file_id, self.stream.current_position(), self.stream.current_position())
                });

            return Err(self.recursion_limit_exceeded(span));
        }

        let result = ensure_sufficient_stack(|| self.parse_statement_inner());
        self.state.recursion_depth -= 1;
        result
    }

    fn parse_statement_inner(&mut self) -> Result<Statement<'arena>, ParseError> {
        let token = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
        Ok(match token.kind {
            T![InlineText | InlineShebang] => Statement::Inline(self.parse_inline()?),
            T!["<?php"] | T!["<?"] => Statement::OpeningTag(self.parse_opening_tag()?),
            T!["<?="] => Statement::EchoTag(self.parse_echo_tag()?),
            T!["?>"] => Statement::ClosingTag(self.parse_closing_tag()?),
            T!["declare"] => Statement::Declare(self.parse_declare()?),
            T!["namespace"] => Statement::Namespace(self.parse_namespace()?),
            T!["use"] => Statement::Use(self.parse_use()?),
            T![Identifier] if self.dialect.is_sharp() && token.value == b"import" => {
                Statement::Use(self.parse_import()?)
            }
            T!["const" | Identifier | "list" | "function" | "("] if self.is_at_local_declaration()? => {
                Statement::LocalDeclaration(self.parse_local_declaration()?)
            }
            T!["return"] => Statement::Return(self.parse_return()?),
            T!["#["] | T!["["] if token.kind == T!["#["] || self.is_at_sharp_attribute_list() => {
                let attributes = self.parse_attribute_list_sequence()?;
                let next = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
                let maybe_after = self.stream.peek_kind(1)?;

                match next.kind {
                    T!["interface"] => Statement::Interface(self.parse_interface_with_attributes(attributes)?),
                    T!["trait"] => Statement::Trait(self.parse_trait_with_attributes(attributes)?),
                    T!["enum"] => Statement::Enum(self.parse_enum_with_attributes(attributes)?),
                    T!["class"] => Statement::Class(self.parse_class_with_attributes(attributes)?),
                    T!["const"] => Statement::Constant(self.parse_constant_with_attributes(attributes)?),
                    T!["function"] => self.parse_closure_or_function(attributes)?,
                    T!["fn"] => Statement::Expression(ExpressionStatement {
                        expression: {
                            self.arena.alloc(Expression::ArrowFunction(
                                self.parse_arrow_function_with_attributes(attributes)?,
                            ))
                        },
                        terminator: self.parse_terminator()?,
                    }),
                    T!["static"] if maybe_after == Some(T!["fn"]) => Statement::Expression(ExpressionStatement {
                        expression: {
                            self.arena.alloc(Expression::ArrowFunction(
                                self.parse_arrow_function_with_attributes(attributes)?,
                            ))
                        },
                        terminator: self.parse_terminator()?,
                    }),
                    T!["static"] if maybe_after == Some(T!["function"]) => Statement::Expression(ExpressionStatement {
                        expression: {
                            self.arena.alloc(Expression::Closure(self.parse_closure_with_attributes(attributes)?))
                        },
                        terminator: self.parse_terminator()?,
                    }),
                    kind if kind.is_modifier() => self.parse_modified_class_like_with_attributes(attributes)?,
                    _ => {
                        return Err(self.stream.unexpected(
                            Some(next),
                            T![
                                "interface",
                                "trait",
                                "enum",
                                "class",
                                "function",
                                "fn",
                                "readonly",
                                "abstract",
                                "final",
                                "static",
                            ],
                        ));
                    }
                }
            }
            T!["interface"] => Statement::Interface(self.parse_interface_with_attributes(Sequence::empty())?),
            T!["trait"] => Statement::Trait(self.parse_trait_with_attributes(Sequence::empty())?),
            T!["enum"] => Statement::Enum(self.parse_enum_with_attributes(Sequence::empty())?),
            T!["class"] => Statement::Class(self.parse_class_with_attributes(Sequence::empty())?),
            T!["function"] => self.parse_closure_or_function(Sequence::empty())?,
            T!["global"] => Statement::Global(self.parse_global()?),
            T!["static"] if matches!(self.stream.peek_kind(1)?, Some(T!["$variable"])) => {
                Statement::Static(self.parse_static()?)
            }
            kind if kind.is_modifier()
                && !matches!(
                    self.stream.peek_kind(1)?,
                    Some(T!["::" | "(" | "->" | "?->" | "[" | "fn" | "function"])
                ) =>
            {
                self.parse_modified_class_like_with_attributes(Sequence::empty())?
            }
            T!["__halt_compiler"] => Statement::HaltCompiler(self.parse_halt_compiler()?),
            T![";"] => Statement::Noop(self.stream.consume_span()?),
            T!["const"] => Statement::Constant(self.parse_constant_with_attributes(Sequence::empty())?),
            T!["if"] => Statement::If(self.parse_if()?),
            T!["match"] if self.dialect.is_sharp() && self.stream.peek_kind(1)? == Some(T!["("]) => {
                Statement::PatternMatch(self.parse_pattern_match()?)
            }
            T!["switch"] => Statement::Switch(self.parse_switch()?),
            T!["foreach"] => Statement::Foreach(self.parse_foreach()?),
            T!["for"] => self.parse_for()?,
            T!["while"] => Statement::While(self.parse_while()?),
            T!["do"] => Statement::DoWhile(self.parse_do_while()?),
            T!["continue"] => Statement::Continue(self.parse_continue()?),
            T!["break"] => Statement::Break(self.parse_break()?),
            T!["unset"] => Statement::Unset(self.parse_unset()?),
            T!["{"] => Statement::Block(self.parse_block()?),
            T!["try"] => Statement::Try(self.parse_try()?),
            T!["echo"] => Statement::Echo(self.parse_echo()?),
            T!["goto"] => Statement::Goto(self.parse_goto()?),
            kind if kind.is_identifier_maybe_reserved() && matches!(self.stream.peek_kind(1)?, Some(T![":"])) => {
                Statement::Label(self.parse_label()?)
            }
            _ => Statement::Expression(ExpressionStatement {
                expression: self.arena.alloc(self.parse_expression()?),
                terminator: self.parse_terminator()?,
            }),
        })
    }

    fn parse_closure_or_function(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Statement<'arena>, ParseError> {
        Ok(match (self.stream.peek_kind(1)?, self.stream.lookahead(2)?.map(|t| t.kind)) {
            // if the next token is `(` or `&` followed by `(`, then we know this is a closure
            (Some(T!["("]), _) | (Some(T!["&"]), Some(T!["("])) => Statement::Expression(ExpressionStatement {
                expression: { self.arena.alloc(Expression::Closure(self.parse_closure_with_attributes(attributes)?)) },
                terminator: self.parse_terminator()?,
            }),
            _ => {
                // otherwise, we know this is a function
                Statement::Function(self.parse_function_with_attributes(attributes)?)
            }
        })
    }
}
