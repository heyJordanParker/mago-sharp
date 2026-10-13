//! PHP# names as Lean writes them.

/// Lean's keywords. A PHP# name that is one is written in `«»`, which keeps the name.
const KEYWORDS: &[&str] = &[
    "abbrev",
    "at",
    "attribute",
    "axiom",
    "break",
    "by",
    "calc",
    "catch",
    "class",
    "continue",
    "def",
    "deriving",
    "do",
    "else",
    "end",
    "example",
    "export",
    "finally",
    "for",
    "from",
    "fun",
    "have",
    "if",
    "import",
    "in",
    "inductive",
    "infix",
    "instance",
    "let",
    "local",
    "macro",
    "match",
    "mut",
    "namespace",
    "notation",
    "open",
    "partial",
    "private",
    "protected",
    "return",
    "section",
    "set_option",
    "show",
    "structure",
    "suffices",
    "syntax",
    "then",
    "theorem",
    "try",
    "unless",
    "universe",
    "unsafe",
    "variable",
    "where",
    "with",
];

/// Names Lean declares for every structure and inductive, which `«»` does not free.
pub(crate) const RESERVED: &[&str] = &["rec", "casesOn", "recOn", "noConfusion", "noConfusionType", "below", "brecOn"];

/// One PHP# identifier as a Lean identifier.
pub(crate) fn identifier(name: &[u8]) -> String {
    let name = String::from_utf8_lossy(name);
    if KEYWORDS.contains(&name.as_ref()) { format!("«{name}»") } else { name.into_owned() }
}

/// A fully qualified PHP name, `App\Shared\Money`, as its Lean name, `App.Shared.Money`.
pub(crate) fn full_name(name: &[u8]) -> String {
    let name = name.strip_prefix(b"\\").unwrap_or(name);

    name.split(|byte| *byte == b'\\').map(identifier).collect::<Vec<_>>().join(".")
}

/// The member `member` of the class whose Lean name is `class`.
pub(crate) fn member(class: &str, member: &[u8]) -> String {
    format!("{class}.{}", identifier(member))
}

/// The last segment of a fully qualified PHP name: `Money` of `App\Shared\Money`.
pub(crate) fn short_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name.rsplit(|byte| *byte == b'\\').next().unwrap_or(name)).into_owned()
}

/// A PHP# string as the bytes Lean's `string` holds.
pub(crate) fn string(bytes: &[u8]) -> String {
    let bytes: Vec<String> = bytes.iter().map(u8::to_string).collect();

    format!("([{}] : string)", bytes.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyword_is_escaped_and_any_other_name_is_kept() {
        assert_eq!(identifier(b"end"), "«end»");
        assert_eq!(identifier(b"currency"), "currency");
        assert_eq!(full_name(br"\App\Shared\Money"), "App.Shared.Money");
        assert_eq!(full_name(br"App\match\Score"), "App.«match».Score");
        assert_eq!(member("App.Range", b"end"), "App.Range.«end»");
    }

    #[test]
    fn a_string_is_its_bytes() {
        assert_eq!(string(b"EUR"), "([69, 85, 82] : string)");
        assert_eq!(string(b""), "([] : string)");
    }
}
