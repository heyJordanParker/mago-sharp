use mago_database::file::FileId;
use mago_database::file::HasFileId;
use mago_reporting::Annotation;
use mago_reporting::Issue;
use mago_span::HasSpan;
use mago_span::Position;
use mago_span::Span;
use mago_syntax_core::input::Input;

use crate::cst::LiteralStringKind;
use crate::cst::Operator;
use crate::lexer::Lexer;
use crate::parser::MAX_RECURSION_DEPTH;
use crate::settings::LexerSettings;
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
    /// A PHP# parameter written without its type, such as `run(extra)`, at its name.
    UntypedParameterInSharp(Span),
    /// A PHP# field written without its type, such as `protected override table = "orders";`, at its name.
    UntypedFieldInSharp(Span),
    /// A PHP# property written without its type, such as `protected override table { get; set; }`, at its name.
    UntypedPropertyInSharp(Span),
    /// A PHP# statement, expression or type nested more than [`MAX_RECURSION_DEPTH`] levels deep, at the innermost one.
    NestingTooDeepInSharp(Span),
    /// `required` written on a PHP# method or operator, at the `required`. Spec section 25 keeps it to the constructor,
    /// which `new Self(…)` calls, and section 6.1 to a property.
    RequiredMethodInSharp(Span),
    /// An escape JavaScript refuses in a PHP# template, such as `\1` or `\x4`, at its backslash and next character.
    InvalidTemplateEscapeInSharp(Span),
    /// A PHP# lambda written right after an operator, such as `handler ?? item => item.ready`, at its parameters and
    /// arrow. A lambda binds as loosely as assignment, as in C#, so it needs parentheses there.
    LambdaAfterOperatorInSharp(Span),
    /// A `? :` written at the top of a PHP# `when` condition, such as `when strict ? forced : ready =>`, at the whole
    /// conditional. A `when` condition binds as tightly as `??`, as in C#, so it needs parentheses there.
    ConditionalInGuardInSharp(Span),
    /// A PHP# operator declared with a symbol that is no binary operator, such as `operator !`, with the symbol, at the
    /// symbol. Semantics refuses a binary operator a class cannot declare.
    UndeclarableOperatorInSharp(Box<str>, Span),
    /// A `uses` clause written after a PHP# function type's `>`, as in `Function<Charge(Cart)> uses Http charge`, at
    /// the clause. A function type lists its effects inside its brackets.
    UsesAfterFunctionTypeInSharp(Span),
    /// A PHP# method's function parameter named in its `uses`, as `f` in `apply(Function<int()> f) uses f`, with the
    /// name, at the name. Spec section 29: a method takes on the effects of each function it is given, with nothing
    /// written.
    FunctionParameterInUsesInSharp(Box<str>, Span),
    /// A `uses` clause written on a PHP# `import`, as in `import Vendor.Pdf uses Files;`, at the clause. Spec section
    /// 23: a library's effect lives in its one `extern` declaration.
    UsesOnImportInSharp(Span),
    /// A PHP# method's `readonly` written before its `on` or `uses` clause, at the `readonly`. Spec section 13: it is
    /// always the last clause.
    ReadonlyNotLastInSharp(Span),
    /// A member other than a binding or a law written in a PHP# module, such as a method or a statement, at the
    /// member. Spec section 32: a module holds no statements, branches, loops or methods.
    OtherMemberInSharpModule(Span),
    /// A PHP# parameter written as a computed property, as in `public string slug => "x"`, at its `=>`. A computed
    /// property has no incoming value to receive.
    ComputedParameterInSharp(Span),
    /// A PHP# constructor parameter's default written before its accessors, as PHP orders a promoted property, as in
    /// `public string raw = "" { get; }`, at the default. Spec section 9: the default comes last, after the accessors.
    DefaultBeforeAccessorsInSharp(Span),
    /// Anything but `super(…)` or `this(…)` after a PHP# constructor's `:`, as in `: parent(row)`, `: super { }` or
    /// `: ?Type`, from the `:` up to the body.
    ConstructorInitializerInSharp(Span),
    /// A body written after a PHP# event's parameters, as in `public event Paid(int orderId) { }`, at the body. Spec
    /// section 15: an event's parameters are its whole declaration.
    EventBodyInSharp(Span),
    /// A type declared inside a PHP# class-like, as in `class Order { public event Paid(int id); }`, with the
    /// class-like as a message names it, `A class`, and the type's name, at the whole type. Every type, events and
    /// interfaces included, is declared in its own file.
    NestedTypeInSharp(&'static str, Box<str>, Span),
    /// A type other than an interface name in a PHP# struct's header, as in `struct Username : string`, at the type.
    /// Spec section 10: a struct implements interfaces, and keys a `Map` by its whole value.
    StructHeaderTypeInSharp(Span),
    /// Type parameters written on a PHP# named constructor, as in `Plain.of<T>(T value)`, with its class's name and the
    /// names of the type parameters its class declares, at the type parameters. A named constructor takes its class's
    /// type arguments, as Dart's does, and declares none.
    NamedConstructorTypeParametersInSharp(Box<str>, Option<Box<str>>, Span),
    /// A PHP# keyword written in another case, as in `Return 5;`, with the keyword as PHP# writes it, at the keyword.
    /// PHP#'s keywords are lowercase and case-sensitive, as C#'s are, except `Self`. The analyzer reports it too, where
    /// a name that spells a keyword names nothing, as `THIS` does.
    MiscasedKeywordInSharp(&'static str, Span),
    /// A PHP# lambda with attributes and a parameter without parentheses, as in `[Pure] n => n * 2`, from the first
    /// attribute list to the parameter. Spec section 1: a lambda with attributes puts its parameters in parentheses.
    UnparenthesizedAttributedLambdaInSharp(Span),
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
            ParseError::UntypedParameterInSharp(span)
            | ParseError::UntypedFieldInSharp(span)
            | ParseError::UntypedPropertyInSharp(span)
            | ParseError::NestingTooDeepInSharp(span)
            | ParseError::RequiredMethodInSharp(span)
            | ParseError::InvalidTemplateEscapeInSharp(span)
            | ParseError::LambdaAfterOperatorInSharp(span)
            | ParseError::ConditionalInGuardInSharp(span)
            | ParseError::UndeclarableOperatorInSharp(_, span)
            | ParseError::UsesAfterFunctionTypeInSharp(span)
            | ParseError::FunctionParameterInUsesInSharp(_, span)
            | ParseError::UsesOnImportInSharp(span)
            | ParseError::ReadonlyNotLastInSharp(span)
            | ParseError::OtherMemberInSharpModule(span)
            | ParseError::ComputedParameterInSharp(span)
            | ParseError::DefaultBeforeAccessorsInSharp(span)
            | ParseError::ConstructorInitializerInSharp(span)
            | ParseError::EventBodyInSharp(span)
            | ParseError::NestedTypeInSharp(_, _, span)
            | ParseError::StructHeaderTypeInSharp(span)
            | ParseError::NamedConstructorTypeParametersInSharp(_, _, span)
            | ParseError::MiscasedKeywordInSharp(_, span)
            | ParseError::UnparenthesizedAttributedLambdaInSharp(span) => span.file_id,
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
            ParseError::UntypedParameterInSharp(span)
            | ParseError::UntypedFieldInSharp(span)
            | ParseError::UntypedPropertyInSharp(span)
            | ParseError::NestingTooDeepInSharp(span)
            | ParseError::RequiredMethodInSharp(span)
            | ParseError::InvalidTemplateEscapeInSharp(span)
            | ParseError::LambdaAfterOperatorInSharp(span)
            | ParseError::ConditionalInGuardInSharp(span)
            | ParseError::UndeclarableOperatorInSharp(_, span)
            | ParseError::UsesAfterFunctionTypeInSharp(span)
            | ParseError::FunctionParameterInUsesInSharp(_, span)
            | ParseError::UsesOnImportInSharp(span)
            | ParseError::ReadonlyNotLastInSharp(span)
            | ParseError::OtherMemberInSharpModule(span)
            | ParseError::ComputedParameterInSharp(span)
            | ParseError::DefaultBeforeAccessorsInSharp(span)
            | ParseError::ConstructorInitializerInSharp(span)
            | ParseError::EventBodyInSharp(span)
            | ParseError::NestedTypeInSharp(_, _, span)
            | ParseError::StructHeaderTypeInSharp(span)
            | ParseError::NamedConstructorTypeParametersInSharp(_, _, span)
            | ParseError::MiscasedKeywordInSharp(_, span)
            | ParseError::UnparenthesizedAttributedLambdaInSharp(span) => *span,
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
            ParseError::UntypedParameterInSharp(_) => "A PHP# parameter needs a type, as in `int extra`.".to_string(),
            ParseError::UntypedFieldInSharp(_) => {
                "A PHP# field needs a type, as in `private int count = 0;`.".to_string()
            }
            ParseError::UntypedPropertyInSharp(_) => {
                "A PHP# property needs a type, as in `public int views { get; set; }`.".to_string()
            }
            ParseError::NestingTooDeepInSharp(_) => {
                format!("PHP# nests statements, expressions and types at most {MAX_RECURSION_DEPTH} levels deep.")
            }
            ParseError::RequiredMethodInSharp(_) => {
                "`required` goes on the constructor or a property, never on a method: remove it here.".to_string()
            }
            ParseError::InvalidTemplateEscapeInSharp(_) => {
                "A template takes JavaScript's escapes, as in `\\n`, `\\x41` or `\\u{1F600}`.".to_string()
            }
            ParseError::LambdaAfterOperatorInSharp(_) => {
                "A lambda after an operator needs parentheses, as in `handler ?? (item => item.ready)`.".to_string()
            }
            ParseError::ConditionalInGuardInSharp(_) => {
                "A `? :` in a `when` condition needs parentheses, as in `when (strict ? forced : ready) =>`.".to_string()
            }
            ParseError::UndeclarableOperatorInSharp(symbol, _) => {
                format!("`operator {symbol}` cannot be declared: only {} can.", Operator::DECLARABLE)
            }
            ParseError::UsesAfterFunctionTypeInSharp(_) => {
                "A function type lists its effects inside its brackets, as in `Function<Charge(Cart) uses Http>`."
                    .to_string()
            }
            ParseError::FunctionParameterInUsesInSharp(name, _) => format!(
                "`{name}` is a function parameter, and a method takes on the effects of each function it is given, so `uses` names no parameter: delete it."
            ),
            ParseError::UsesOnImportInSharp(_) => {
                "An import never carries `uses`: declare the library's effect once, with `extern`.".to_string()
            }
            ParseError::ReadonlyNotLastInSharp(_) => {
                "A method's `readonly` is its last clause, after `on` and `uses`, as in `Money quote(Cart cart) uses Http readonly;`."
                    .to_string()
            }
            ParseError::OtherMemberInSharpModule(_) => {
                "A module holds only its imports, service bindings and laws: give each binding one expression.".to_string()
            }
            ParseError::ComputedParameterInSharp(_) => {
                "A parameter cannot be a computed property: a computed property has no incoming value to receive.".to_string()
            }
            ParseError::DefaultBeforeAccessorsInSharp(_) => {
                "A parameter's default comes last: write it after the accessors, as in `public string raw { get; } = \"\"`."
                    .to_string()
            }
            ParseError::ConstructorInitializerInSharp(_) => {
                "A constructor passes control to its base class with `: super(…)`, and a named constructor to the main one with `: this(…)`.".to_string()
            }
            ParseError::EventBodyInSharp(_) => {
                "An event declares no body: its parameters are its properties, as in `public event OrderPaid(int orderId);`."
                    .to_string()
            }
            ParseError::NestedTypeInSharp(owner, name, _) => {
                format!("{owner} can't declare a type inside it: declare `{name}` in its own file.")
            }
            ParseError::StructHeaderTypeInSharp(_) => {
                "A struct's header lists the interfaces it implements: a struct keys a `Map` by its whole value."
                    .to_string()
            }
            ParseError::NamedConstructorTypeParametersInSharp(..) => {
                "A named constructor takes its class's type arguments and declares none of its own.".to_string()
            }
            ParseError::MiscasedKeywordInSharp("Self", _) => {
                "Write the keyword `Self` with a capital S and the rest in lowercase.".to_string()
            }
            ParseError::MiscasedKeywordInSharp(keyword, _) => format!("Write the keyword `{keyword}` in lowercase."),
            ParseError::UnparenthesizedAttributedLambdaInSharp(_) => {
                "A lambda with attributes puts its parameters in parentheses, as in `[Pure] (n) => n * 2`.".to_string()
            }
        };

        write!(f, "{message}")
    }
}

impl ParseError {
    /// The error a typo gives at the first token from `offset` of a PHP# file's `code`, as `asdf` does in
    /// `public asdf#($@`: that token is unexpected. Every form outside the PHP# grammar fails with it, whichever
    /// language it comes from.
    #[must_use]
    pub fn unexpected_in_sharp(file_id: FileId, code: &[u8], offset: u32) -> ParseError {
        let start = Position::new(offset);
        let mut lexer =
            Lexer::sharp(Input::anchored_at(file_id, &code[offset as usize..], start), LexerSettings::default());

        loop {
            match lexer.advance() {
                Some(Ok(token)) if token.kind.is_trivia() => {}
                Some(Ok(token)) => {
                    return ParseError::UnexpectedToken(Expected::OneOf(&[]), token.kind, token.span_for(file_id));
                }
                Some(Err(error)) => return ParseError::SyntaxError(error),
                None => {
                    return ParseError::UnexpectedEndOfFile(Expected::OneOf(&[]), file_id, lexer.current_position());
                }
            }
        }
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
            ParseError::RequiredMethodInSharp(..)
            | ParseError::UntypedParameterInSharp(..)
            | ParseError::UntypedFieldInSharp(..)
            | ParseError::UntypedPropertyInSharp(..)
            | ParseError::InvalidTemplateEscapeInSharp(..)
            | ParseError::LambdaAfterOperatorInSharp(..)
            | ParseError::ConditionalInGuardInSharp(..)
            | ParseError::UndeclarableOperatorInSharp(..)
            | ParseError::UsesAfterFunctionTypeInSharp(..)
            | ParseError::FunctionParameterInUsesInSharp(..)
            | ParseError::UsesOnImportInSharp(..)
            | ParseError::ReadonlyNotLastInSharp(..)
            | ParseError::OtherMemberInSharpModule(..)
            | ParseError::ComputedParameterInSharp(..)
            | ParseError::DefaultBeforeAccessorsInSharp(..)
            | ParseError::ConstructorInitializerInSharp(..)
            | ParseError::EventBodyInSharp(..)
            | ParseError::NestedTypeInSharp(..)
            | ParseError::StructHeaderTypeInSharp(..)
            | ParseError::MiscasedKeywordInSharp(..)
            | ParseError::UnparenthesizedAttributedLambdaInSharp(..) => Issue::error(error.to_string())
                .with_code(PARSE_ERROR_CODE)
                .with_annotation(Annotation::primary(error.span()).with_message("Written here.")),
            // A generic class's static method takes the class's type arguments, so its help declares none of its own.
            ParseError::NamedConstructorTypeParametersInSharp(class, type_parameters, span) => {
                Issue::error(error.to_string())
                    .with_code(PARSE_ERROR_CODE)
                    .with_annotation(Annotation::primary(*span).with_message("Written here."))
                    .with_help(match type_parameters {
                        Some(names) => {
                            let first = names.split(", ").next().unwrap_or(names);
                            format!(
                                "Declare a static method, as in `public static {class}<{names}> of({first} value)`: it takes the class's type arguments."
                            )
                        }
                        None => format!(
                            "Declare a generic static method, as in `public static {class} of<T>(T value)`, or make the class generic, as in `class {class}<T>`."
                        ),
                    })
            }
            _ => Issue::error("Parse error encountered during parsing")
                .with_code(PARSE_ERROR_CODE)
                .with_annotation(Annotation::primary(error.span()).with_message(error.to_string()))
                .with_note("This error indicates that the parser encountered a parse issue.")
                .with_help("Check the syntax of your code."),
        }
    }
}
