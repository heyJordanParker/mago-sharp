//! The words a `.sharp` message must not name, because only PHP has them or only the checker's own workings do. The
//! tests that analyze `.sharp` files include this module and check every issue those files get against it.

use std::sync::LazyLock;

use mago_reporting::Issue;
use regex::Regex;

/// What a `.sharp` message must not name, each with the pattern that finds it. Lean stays where a message is about the
/// developer's own `.lean` file or the Lean install, and `vendor/bin/mago compile` stays as the command.
static BANNED: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    [
        ("`mixed`", r"\bmixed\b"),
        ("the word `unit`", r"(?i)\bunit\b"),
        ("`isset`", r"\bisset\b"),
        ("a collection stub class", r"\b(List|Map|Set)Methods\b"),
        ("a PHP class name with a backslash", r"\w\\\w"),
        ("PHP member access", r"(->|::)\$?[A-Za-z_]"),
        ("a spec section", r"(?i)\bsections?\s+\d|§"),
        ("the checker's own name", r"\bMago\b"),
        ("Lean as the checker's translation", r"\b(?:to|in) Lean\b|\bLean (?:models|declares|cannot (?:prove|see))\b"),
        ("a hidden variable of a pattern's PHP", r"\b(?:match|as)#\d"),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(pattern).expect("a valid pattern")))
    .collect()
});

/// The PHP form a refusal quotes because the developer wrote it, as in "PHP# has no `compact`".
static REFUSED_FORM: LazyLock<Regex> = LazyLock::new(|| Regex::new("PHP# has no `[^`]*`").expect("a valid pattern"));

/// The token a parse error quotes because the developer wrote it, as in the typo error "Unexpected `mixed`" or
/// "Expected `;`, found `mixed`".
static WRITTEN_TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?:Unexpected|found) `[^`]*`").expect("a valid pattern"));

/// The first text of `issue`, its message, help, notes or annotations, that names something only PHP has outside a
/// refused form it quotes, with what it names: what it is and the words found. Only a parse error quotes a token as
/// written.
pub fn banned_word(issue: &Issue) -> Option<(&str, String)> {
    let parse_error = issue.code.as_deref() == Some("parse");
    std::iter::once(issue.message.as_str())
        .chain(issue.help.as_deref())
        .chain(issue.notes.iter().map(String::as_str))
        .chain(issue.annotations.iter().filter_map(|annotation| annotation.message.as_deref()))
        .find_map(|text| {
            let own_words = REFUSED_FORM.replace_all(text, "");
            let own_words = if parse_error {
                WRITTEN_TOKEN.replace_all(&own_words, "").into_owned()
            } else {
                own_words.into_owned()
            };
            BANNED.iter().find_map(|(name, pattern)| {
                pattern.find(&own_words).map(|found| (text, format!("{name}: `{}`", found.as_str())))
            })
        })
}
