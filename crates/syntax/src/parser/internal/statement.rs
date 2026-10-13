use mago_allocator::prelude::*;
use mago_database::file::HasFileId;
use mago_span::Span;
use mago_syntax_core::stack::ensure_sufficient_stack;

use crate::T;
use crate::cst::cst::AttributeList;
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
        self.read_php_statement()?;
        let token = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
        // Each path returns its parser's `Result` as it is, never unwrapped with `?` and wrapped again: an unoptimized
        // build gives every unwrapped value its own stack slot, and this frame sits between two `ensure_sufficient_stack`
        // calls, so it must stay inside their red zone.
        if !self.state.within_block && self.is_internal(token) {
            return self.parse_modified_class_like_with_attributes(Sequence::empty());
        }

        match token.kind {
            T![InlineText | InlineShebang] => self.parse_inline().map(Statement::Inline),
            T!["<?php"] | T!["<?"] => self.parse_opening_tag().map(Statement::OpeningTag),
            T!["<?="] => self.parse_echo_tag().map(Statement::EchoTag),
            T!["?>"] => self.parse_closing_tag().map(Statement::ClosingTag),
            T!["declare"] => self.parse_declare().map(Statement::Declare),
            T!["namespace"] => self.parse_namespace().map(Statement::Namespace),
            T!["use"] => self.parse_use().map(Statement::Use),
            T![Identifier] if self.dialect.is_sharp() && token.value == b"import" => {
                self.parse_import().map(Statement::Use)
            }
            T![Identifier]
                if self.dialect.is_sharp()
                    && token.value == b"extern"
                    && self.stream.peek_kind(1)?.is_some_and(|kind| kind.is_identifier_maybe_reserved()) =>
            {
                self.parse_extern().map(Statement::Extern)
            }
            T![Identifier]
                if self.dialect.is_sharp()
                    && token.value == b"module"
                    && matches!(self.stream.peek_kind(1)?, Some(T!["{" | ":"])) =>
            {
                self.parse_module().map(Statement::Module)
            }
            T![Identifier] if !self.state.within_block && self.is_at_event()? => self
                .parse_event_with_attributes_and_modifiers(Sequence::empty(), Sequence::empty())
                .map(Statement::Event),
            T![Identifier] if self.is_at_emit()? => self.parse_emit().map(Statement::Emit),
            T!["const" | Identifier | "("] if self.is_at_local_declaration()? => {
                self.parse_local_declaration().map(Statement::LocalDeclaration)
            }
            T!["return"] => self.parse_return().map(Statement::Return),
            T!["#["] | T!["["] if token.kind == T!["#["] || self.is_at_sharp_attribute_list() => {
                let attributes = self.parse_attribute_list_sequence()?;
                self.read_php_construct()?;
                let next = self.stream.lookahead(0)?.ok_or_else(|| self.stream.unexpected(None, &[]))?;
                let maybe_after = self.stream.peek_kind(1)?;

                match next.kind {
                    T!["interface"] => self.parse_interface_with_attributes(attributes).map(Statement::Interface),
                    T!["trait"] => self.parse_trait_with_attributes(attributes).map(Statement::Trait),
                    T!["enum"] => self.parse_enum_with_attributes(attributes).map(Statement::Enum),
                    T!["class"] => self.parse_class_with_attributes(attributes).map(Statement::Class),
                    T!["const"] => self.parse_constant_with_attributes(attributes).map(Statement::Constant),
                    T!["function"] => self.parse_closure_or_function(attributes),
                    T!["fn"] => Ok(Statement::Expression(ExpressionStatement {
                        expression: self.parse_arrow_function_with_attributes(attributes)?,
                        terminator: self.parse_terminator()?,
                    })),
                    T!["static"] if maybe_after == Some(T!["fn"]) => Ok(Statement::Expression(ExpressionStatement {
                        expression: self.parse_arrow_function_with_attributes(attributes)?,
                        terminator: self.parse_terminator()?,
                    })),
                    T!["static"] if maybe_after == Some(T!["function"]) => {
                        Ok(Statement::Expression(ExpressionStatement {
                            expression: self.parse_closure_with_attributes(attributes)?,
                            terminator: self.parse_terminator()?,
                        }))
                    }
                    kind if kind.is_modifier() || self.is_internal(next) => {
                        self.parse_modified_class_like_with_attributes(attributes)
                    }
                    T![Identifier] if self.is_at_event()? => self
                        .parse_event_with_attributes_and_modifiers(attributes, Sequence::empty())
                        .map(Statement::Event),
                    _ => Err(self.stream.unexpected(
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
                    )),
                }
            }
            T!["interface"] => self.parse_interface_with_attributes(Sequence::empty()).map(Statement::Interface),
            T!["trait"] => self.parse_trait_with_attributes(Sequence::empty()).map(Statement::Trait),
            T!["enum"] => self.parse_enum_with_attributes(Sequence::empty()).map(Statement::Enum),
            T!["class"] => self.parse_class_with_attributes(Sequence::empty()).map(Statement::Class),
            T!["function"] => self.parse_closure_or_function(Sequence::empty()),
            T!["global"] => self.parse_global().map(Statement::Global),
            T!["static"] if matches!(self.stream.peek_kind(1)?, Some(T!["$variable"])) => {
                self.parse_static().map(Statement::Static)
            }
            kind if kind.is_modifier()
                && !matches!(
                    self.stream.peek_kind(1)?,
                    Some(T!["::" | "(" | "->" | "?->" | "[" | "fn" | "function"])
                ) =>
            {
                self.parse_modified_class_like_with_attributes(Sequence::empty())
            }
            T!["__halt_compiler"] => self.parse_halt_compiler().map(Statement::HaltCompiler),
            T![";"] => self.stream.consume_span().map(Statement::Noop),
            T!["const"] => self.parse_constant_with_attributes(Sequence::empty()).map(Statement::Constant),
            T!["if"] => self.parse_if().map(Statement::If),
            T!["match"] if self.dialect.is_sharp() && self.stream.peek_kind(1)? == Some(T!["("]) => {
                self.parse_pattern_match().map(Statement::PatternMatch)
            }
            T!["switch"] => self.parse_switch().map(Statement::Switch),
            T!["foreach"] => self.parse_foreach().map(Statement::Foreach),
            T!["for"] => self.parse_for(),
            T!["while"] => self.parse_while().map(Statement::While),
            T!["do"] => self.parse_do_while().map(Statement::DoWhile),
            T!["continue"] => self.parse_continue().map(Statement::Continue),
            T!["break"] => self.parse_break().map(Statement::Break),
            T!["unset"] => self.parse_unset().map(Statement::Unset),
            T!["{"] => self.parse_block().map(Statement::Block),
            T!["try"] => self.parse_try().map(Statement::Try),
            T!["echo"] => self.parse_echo().map(Statement::Echo),
            T!["goto"] => self.parse_goto().map(Statement::Goto),
            kind if kind.is_identifier_maybe_reserved() && matches!(self.stream.peek_kind(1)?, Some(T![":"])) => {
                self.parse_label().map(Statement::Label)
            }
            _ => Ok(Statement::Expression(ExpressionStatement {
                expression: self.arena.alloc(self.parse_expression()?),
                terminator: self.parse_terminator()?,
            })),
        }
    }

    fn parse_closure_or_function(
        &mut self,
        attributes: Sequence<'arena, AttributeList<'arena>>,
    ) -> Result<Statement<'arena>, ParseError> {
        Ok(match (self.stream.peek_kind(1)?, self.stream.lookahead(2)?.map(|t| t.kind)) {
            // if the next token is `(` or `&` followed by `(`, then we know this is a closure
            (Some(T!["("]), _) | (Some(T!["&"]), Some(T!["("])) => Statement::Expression(ExpressionStatement {
                expression: self.parse_closure_with_attributes(attributes)?,
                terminator: self.parse_terminator()?,
            }),
            _ => {
                // otherwise, we know this is a function
                Statement::Function(self.parse_function_with_attributes(attributes)?)
            }
        })
    }
}
