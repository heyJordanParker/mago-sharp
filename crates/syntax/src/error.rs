use mago_database::file::FileId;
use mago_database::file::HasFileId;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;

use crate::T;
use crate::cst::LiteralStringKind;
use crate::parser::MAX_RECURSION_DEPTH;
use crate::token::TokenKind;

const SYNTAX_ERROR_CODE: &str = "syntax";
const PARSE_ERROR_CODE: &str = "parse";

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SyntaxError {
    UnexpectedToken(FileId, u8, Position),
    UnrecognizedToken(FileId, u8, Position),
    UnexpectedEndOfFile(FileId, Position),
    RecursionLimitExceeded(FileId, Position),
}

/// The token kinds a parser expected at the point an error was raised.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Expected {
    Exactly(TokenKind),
    OneOf(&'static [TokenKind]),
}

impl Expected {
    #[must_use]
    pub fn kinds(&self) -> &[TokenKind] {
        match self {
            Expected::Exactly(kind) => std::slice::from_ref(kind),
            Expected::OneOf(kinds) => kinds,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ParseError {
    SyntaxError(SyntaxError),
    UnexpectedEndOfFile(Expected, FileId, Position),
    UnexpectedToken(Expected, TokenKind, Span),
    UnclosedLiteralString(LiteralStringKind, Span),
    RecursionLimitExceeded(Span),
    /// PHP syntax written in a PHP# file, such as `->` where PHP# writes `.`.
    PhpSyntaxInSharp(TokenKind, Span),
    /// A `\` name written in a PHP# file, such as `\Lib\Calc`, with the dotted name its `import` line takes.
    QualifiedNameInSharp(Box<str>, Span),
    /// A PHP# parameter written without its type, such as `run(extra)`, at its name.
    UntypedParameterInSharp(Span),
    /// A PHP# field written without its type, such as `protected override table = "orders";`, at its name.
    UntypedFieldInSharp(Span),
    /// A PHP# statement, expression or type nested more than [`MAX_RECURSION_DEPTH`] levels deep, at the innermost one.
    NestingTooDeepInSharp(Span),
    /// TypeScript's `in` written in a PHP# `for … of` loop, at the `in`.
    ForInInSharp(Span),
    /// PHP# syntax the engine cannot run yet, such as `required` or a named constructor, where it starts.
    NotSupportedYetInSharp(&'static str, Span),
    /// An escape JavaScript refuses in a PHP# template, such as `\1` or `\x4`, at its backslash and next character.
    InvalidTemplateEscapeInSharp(Span),
    /// A PHP `fn` arrow function or `function` closure written in a PHP# file, at its keyword.
    PhpLambdaInSharp(Span),
}

impl HasFileId for SyntaxError {
    fn file_id(&self) -> FileId {
        match self {
            Self::UnexpectedToken(file_id, _, _) => *file_id,
            Self::UnrecognizedToken(file_id, _, _) => *file_id,
            Self::UnexpectedEndOfFile(file_id, _) => *file_id,
            Self::RecursionLimitExceeded(file_id, _) => *file_id,
        }
    }
}

impl HasFileId for ParseError {
    fn file_id(&self) -> FileId {
        match self {
            ParseError::SyntaxError(syntax_error) => syntax_error.file_id(),
            ParseError::UnexpectedEndOfFile(_, file_id, _) => *file_id,
            ParseError::UnexpectedToken(_, _, span) => span.file_id,
            ParseError::UnclosedLiteralString(_, span) => span.file_id,
            ParseError::RecursionLimitExceeded(span) => span.file_id,
            ParseError::PhpSyntaxInSharp(_, span)
            | ParseError::QualifiedNameInSharp(_, span)
            | ParseError::UntypedParameterInSharp(span)
            | ParseError::UntypedFieldInSharp(span)
            | ParseError::NestingTooDeepInSharp(span)
            | ParseError::ForInInSharp(span)
            | ParseError::NotSupportedYetInSharp(_, span)
            | ParseError::InvalidTemplateEscapeInSharp(span)
            | ParseError::PhpLambdaInSharp(span) => span.file_id,
        }
    }
}

impl HasSpan for SyntaxError {
    fn span(&self) -> Span {
        let (file_id, position) = match self {
            Self::UnexpectedToken(file_id, _, p) => (file_id, p),
            Self::UnrecognizedToken(file_id, _, p) => (file_id, p),
            Self::UnexpectedEndOfFile(file_id, p) => (file_id, p),
            Self::RecursionLimitExceeded(file_id, p) => (file_id, p),
        };

        Span::new(*file_id, *position, position.forward(1))
    }
}

impl HasSpan for ParseError {
    fn span(&self) -> Span {
        match &self {
            ParseError::SyntaxError(syntax_error) => syntax_error.span(),
            ParseError::UnexpectedEndOfFile(_, file_id, position) => Span::new(*file_id, *position, *position),
            ParseError::UnexpectedToken(_, _, span) => *span,
            ParseError::UnclosedLiteralString(_, span) => *span,
            ParseError::RecursionLimitExceeded(span) => *span,
            ParseError::PhpSyntaxInSharp(_, span)
            | ParseError::QualifiedNameInSharp(_, span)
            | ParseError::UntypedParameterInSharp(span)
            | ParseError::UntypedFieldInSharp(span)
            | ParseError::NestingTooDeepInSharp(span)
            | ParseError::ForInInSharp(span)
            | ParseError::NotSupportedYetInSharp(_, span)
            | ParseError::InvalidTemplateEscapeInSharp(span)
            | ParseError::PhpLambdaInSharp(span) => *span,
        }
    }
}

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::UnexpectedToken(_, token, _) => &format!("Unexpected token `{}` (0x{:02X})", *token as char, token),
            Self::UnrecognizedToken(_, token, _) => {
                &format!("Unrecognised token `{}` (0x{:02X})", *token as char, token)
            }
            Self::UnexpectedEndOfFile(_, _) => "Unexpected end of file",
            Self::RecursionLimitExceeded(_, _) => "Maximum recursion depth exceeded",
        };

        write!(f, "{message}")
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            ParseError::SyntaxError(e) => {
                return write!(f, "{e}");
            }
            ParseError::UnexpectedEndOfFile(expected, _, _) => {
                let expected = expected.kinds().iter().map(ToString::to_string).collect::<Vec<_>>().join("`, `");

                if expected.is_empty() {
                    "Unexpected end of file".to_string()
                } else if expected.len() == 1 {
                    format!("Expected `{expected}` before end of file")
                } else {
                    format!("Expected one of `{expected}` before end of file")
                }
            }
            ParseError::UnexpectedToken(expected, found, _) => {
                let expected = expected.kinds().iter().map(ToString::to_string).collect::<Vec<_>>().join("`, `");

                let found = found.to_string();

                if expected.is_empty() {
                    format!("Unexpected token `{found}`")
                } else if expected.len() == 1 {
                    format!("Expected `{expected}`, found `{found}`")
                } else {
                    format!("Expected one of `{expected}`, found `{found}`")
                }
            }
            ParseError::UnclosedLiteralString(kind, _) => match kind {
                LiteralStringKind::SingleQuoted => "Unclosed single-quoted string".to_string(),
                LiteralStringKind::DoubleQuoted => "Unclosed double-quoted string".to_string(),
            },
            ParseError::RecursionLimitExceeded(_) => "Maximum recursion depth exceeded".to_string(),
            ParseError::PhpSyntaxInSharp(kind, _) => match kind {
                T!["->"] => "`->` is PHP syntax: PHP# writes member access with `.`".to_string(),
                T!["?->"] => "`?->` is PHP syntax: PHP# writes null-safe member access with `?.`".to_string(),
                T!["?"] => "`?` before a type is PHP syntax: PHP# writes it after the type, as in `int?`".to_string(),
                T!["::"] => "`::` is PHP syntax: PHP# writes static access with `.`".to_string(),
                T![".="] => "`.=` is PHP syntax: in PHP# `.` is member access".to_string(),
                T!["function"] => "`function` is PHP syntax: a PHP# method starts with its return type".to_string(),
                T!["$variable"] => "A `$` variable is PHP syntax: PHP# names have no `$`".to_string(),
                T!["use"] => "`use` is PHP syntax: PHP# imports a class with `import`".to_string(),
                T!["foreach"] => "`foreach` is PHP syntax: PHP# loops over a collection with `for … of`".to_string(),
                T!["#["] => {
                    "`#[` is PHP syntax: PHP# writes attributes in square brackets, as in `[Searchable]`".to_string()
                }
                kind => format!("`{kind}` is PHP syntax that PHP# does not have"),
            },
            ParseError::QualifiedNameInSharp(name, _) => {
                let short_name = name.rsplit('.').next().unwrap_or(name);

                format!("A `\\` name is PHP syntax: add `import {name};` and write `{short_name}`")
            }
            ParseError::UntypedParameterInSharp(_) => "A PHP# parameter needs a type, as in `int extra`.".to_string(),
            ParseError::UntypedFieldInSharp(_) => {
                "A PHP# field needs a type, as in `private int count = 0;`.".to_string()
            }
            ParseError::NestingTooDeepInSharp(_) => {
                format!("PHP# nests statements, expressions and types at most {MAX_RECURSION_DEPTH} levels deep.")
            }
            ParseError::ForInInSharp(_) => {
                "PHP# loops over a collection with `of`, as in `for (const line of lines)`.".to_string()
            }
            ParseError::NotSupportedYetInSharp(construct, _) => format!("{construct} is not supported yet in PHP#."),
            ParseError::InvalidTemplateEscapeInSharp(_) => {
                "A template takes JavaScript's escapes, as in `\\n`, `\\x41` or `\\u{1F600}`.".to_string()
            }
            ParseError::PhpLambdaInSharp(_) => {
                "PHP# writes a lambda as a bare arrow without `fn` or `function`, as in `x => x.id` or `(a, b) => { … }`"
                    .to_string()
            }
        };

        write!(f, "{message}")
    }
}

impl std::error::Error for SyntaxError {}

impl std::error::Error for ParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ParseError::SyntaxError(e) => Some(e),
            _ => None,
        }
    }
}
impl From<&SyntaxError> for Issue {
    fn from(error: &SyntaxError) -> Issue {
        let span = error.span();

        Issue::error("Syntax error encountered during lexing")
            .with_code(SYNTAX_ERROR_CODE)
            .with_annotation(Annotation::primary(span).with_message(error.to_string()))
            .with_note("This error indicates that the lexer encountered a syntax issue.")
            .with_help("Check the syntax of your code.")
    }
}

impl From<SyntaxError> for ParseError {
    fn from(error: SyntaxError) -> Self {
        ParseError::SyntaxError(error)
    }
}

impl From<&ParseError> for Issue {
    fn from(error: &ParseError) -> Self {
        match error {
            ParseError::SyntaxError(syntax_error) => syntax_error.into(),
            // A PHP# parse error names the rule it breaks, so its message is the title.
            ParseError::PhpSyntaxInSharp(..)
            | ParseError::QualifiedNameInSharp(..)
            | ParseError::UntypedParameterInSharp(..)
            | ParseError::UntypedFieldInSharp(..)
            | ParseError::ForInInSharp(..)
            | ParseError::InvalidTemplateEscapeInSharp(..)
            | ParseError::PhpLambdaInSharp(..) => Issue::error(error.to_string())
                .with_code(PARSE_ERROR_CODE)
                .with_annotation(Annotation::primary(error.span()).with_message("Written here.")),
            ParseError::NotSupportedYetInSharp(_, span) => Issue::error(error.to_string())
                .with_code(PARSE_ERROR_CODE)
                .with_annotation(Annotation::primary(*span).with_message("Not supported yet."))
                .with_note("The PHP# engine cannot run this spec syntax yet."),
            _ => Issue::error("Parse error encountered during parsing")
                .with_code(PARSE_ERROR_CODE)
                .with_annotation(Annotation::primary(error.span()).with_message(error.to_string()))
                .with_note("This error indicates that the parser encountered a parse issue.")
                .with_help("Check the syntax of your code."),
        }
    }
}
