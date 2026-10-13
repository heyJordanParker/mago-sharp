use mago_span::HasSpan;
use mago_syntax::error::ParseError;
use mago_syntax::error::SyntaxError;

use crate::ir::error::Error;
use crate::ir::error::ErrorKind;

pub(crate) fn lower_parse_error(error: &ParseError) -> Error {
    let span = error.span();
    let kind = match error {
        ParseError::SyntaxError(SyntaxError::UnexpectedToken(..)) => ErrorKind::UnexpectedToken,
        ParseError::SyntaxError(SyntaxError::UnrecognizedToken(..)) => ErrorKind::UnrecognizedToken,
        ParseError::SyntaxError(SyntaxError::UnexpectedEndOfFile(..)) => ErrorKind::UnexpectedEndOfFile,
        ParseError::SyntaxError(SyntaxError::RecursionLimitExceeded(..)) => ErrorKind::RecursionLimitExceeded,
        ParseError::UnexpectedEndOfFile(..) => ErrorKind::UnexpectedEndOfFile,
        // PHP syntax in a PHP# file, such as a `fn` lambda, a PHP# parameter or field without its type, `in` in a PHP#
        // loop, PHP# syntax not supported yet, an escape a PHP# template refuses, an operator symbol a class cannot
        // declare, or effects after a function type's `>`, is a token the PHP# grammar does not expect there.
        ParseError::UnexpectedToken(..)
        | ParseError::PhpSyntaxInSharp(..)
        | ParseError::PhpLambdaInSharp(..)
        | ParseError::LambdaAfterOperatorInSharp(..)
        | ParseError::ConditionalInGuardInSharp(..)
        | ParseError::QualifiedNameInSharp(..)
        | ParseError::UntypedParameterInSharp(..)
        | ParseError::UntypedFieldInSharp(..)
        | ParseError::ForInInSharp(..)
        | ParseError::NotSupportedYetInSharp(..)
        | ParseError::InvalidTemplateEscapeInSharp(..)
        | ParseError::UndeclarableOperatorInSharp(..)
        | ParseError::UsesAfterFunctionTypeInSharp(..) => ErrorKind::UnexpectedToken,
        ParseError::UnclosedLiteralString(..) => ErrorKind::UnclosedLiteralString,
        ParseError::RecursionLimitExceeded(..) | ParseError::NestingTooDeepInSharp(..) => {
            ErrorKind::RecursionLimitExceeded
        }
    };

    Error { span, kind }
}
