//! The words a `.sharp` message must not name, because only PHP has them. The tests that analyze `.sharp` files
//! include this module and check every issue those files get against it.

use std::sync::LazyLock;

use mago_reporting::Issue;
use regex::Regex;

/// What a `.sharp` message must not name, each with the pattern that finds it.
static BANNED: LazyLock<Vec<(&str, Regex)>> = LazyLock::new(|| {
    [
        ("`mixed`", r"\bmixed\b"),
        ("the word `unit`", r"(?i)\bunit\b"),
        ("`isset`", r"\bisset\b"),
        ("a collection stub class", r"\b(List|Map|Set)Methods\b"),
        ("a PHP class name with a backslash", r"\w\\\w"),
        ("PHP member access", r"(->|::)\$?[A-Za-z_]"),
        ("a spec section", r"(?i)\bsections?\s+\d|§"),
    ]
    .into_iter()
    .map(|(name, pattern)| (name, Regex::new(pattern).expect("a valid pattern")))
    .collect()
});

/// The PHP form a refusal quotes because the developer wrote it, as in "PHP# has no `mixed`".
static REFUSED_FORM: LazyLock<Regex> = LazyLock::new(|| Regex::new("PHP# has no `[^`]*`").expect("a valid pattern"));

/// The first text of `issue`, its message, help, notes or annotations, that names something only PHP has outside a
/// refused form it quotes, with what it names: what it is and the words found.
pub fn banned_word(issue: &Issue) -> Option<(&str, String)> {
    std::iter::once(issue.message.as_str())
        .chain(issue.help.as_deref())
        .chain(issue.notes.iter().map(String::as_str))
        .chain(issue.annotations.iter().filter_map(|annotation| annotation.message.as_deref()))
        .find_map(|text| {
            let own_words = REFUSED_FORM.replace_all(text, "");
            BANNED.iter().find_map(|(name, pattern)| {
                pattern.find(&own_words).map(|found| (text, format!("{name}: `{}`", found.as_str())))
            })
        })
}
