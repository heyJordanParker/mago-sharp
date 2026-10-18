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
        // PHP syntax in a PHP# file, such as a `fn` lambda or a trait's `use` in a class, a PHP# parameter or field
        // without its type, `in` in a PHP# loop, PHP# syntax not supported yet, `required` on a method, an escape a PHP#
        // template refuses, an operator symbol a class cannot declare, effects after a function type's `>` or on an
        // import, a module member that is no law, a computed property's body after a parameter, anything but `super(…)`
        // or `this(…)` after a constructor's `:`, a body after an event, a type declared inside another, `partial`
        // before a type, type parameters on a named constructor, a keyword in another case, a class cast, a function
        // parameter a method names in `uses`, a name after a pattern without a type, an attribute target after the first
        // attribute, attributes on a type parameter or an attributed lambda without parentheses, is a token the PHP#
        // grammar does not expect there.
        ParseError::UnexpectedToken(..)
        | ParseError::PhpSyntaxInSharp(..)
        | ParseError::TraitUseInSharp(..)
        | ParseError::RequiredMethodInSharp(..)
        | ParseError::ClassNameConstantInSharp(..)
        | ParseError::PhpLambdaInSharp(..)
        | ParseError::LambdaAfterOperatorInSharp(..)
        | ParseError::ConditionalInGuardInSharp(..)
        | ParseError::QualifiedNameInSharp(..)
        | ParseError::UntypedParameterInSharp(..)
        | ParseError::UntypedFieldInSharp(..)
        | ParseError::UntypedPropertyInSharp(..)
        | ParseError::NamedReachInSharp(..)
        | ParseError::ForInInSharp(..)
        | ParseError::NotSupportedYetInSharp(..)
        | ParseError::InvalidTemplateEscapeInSharp(..)
        | ParseError::UndeclarableOperatorInSharp(..)
        | ParseError::UsesAfterFunctionTypeInSharp(..)
        | ParseError::UsesOnImportInSharp(..)
        | ParseError::ReadonlyNotLastInSharp(..)
        | ParseError::OtherMemberInSharpModule(..)
        | ParseError::ComputedParameterInSharp(..)
        | ParseError::DefaultBeforeAccessorsInSharp(..)
        | ParseError::ConstructorInitializerInSharp(..)
        | ParseError::EventBodyInSharp(..)
        | ParseError::StructHeaderTypeInSharp(..)
        | ParseError::NestedTypeInSharp(..)
        | ParseError::PartialTypeInSharp(..)
        | ParseError::NamedConstructorTypeParametersInSharp(..)
        | ParseError::MiscasedKeywordInSharp(..)
        | ParseError::ClassCastInSharp(..)
        | ParseError::FunctionParameterInUsesInSharp(..)
        | ParseError::LateAttributeTargetInSharp(..)
        | ParseError::TypeParameterAttributesInSharp(..)
        | ParseError::UnparenthesizedAttributedLambdaInSharp(..) => ErrorKind::UnexpectedToken,
        ParseError::UnclosedLiteralString(..) => ErrorKind::UnclosedLiteralString,
        ParseError::RecursionLimitExceeded(..) | ParseError::NestingTooDeepInSharp(..) => {
            ErrorKind::RecursionLimitExceeded
        }
    };

    Error { span, kind }
}
