//! PHP# names as Lean writes them.

/// Names Lean declares for every structure and inductive, which `«»` does not free.
pub(crate) const RESERVED: &[&str] = &["rec", "casesOn", "recOn", "noConfusion", "noConfusionType", "below", "brecOn"];

/// One PHP# identifier as a Lean identifier: in `«»`, which keeps the name whatever it is, so no PHP# name is one of
/// Lean's keywords or the tokens of the notations a module imports.
pub(crate) fn identifier(name: &[u8]) -> String {
    format!("«{}»", String::from_utf8_lossy(name))
}

/// A fully qualified PHP name, `App\Shared\Money`, as its Lean name, `«App».«Shared».«Money»`.
pub(crate) fn full_name(name: &[u8]) -> String {
    let name = name.strip_prefix(b"\\").unwrap_or(name);

    name.split(|byte| *byte == b'\\').map(identifier).collect::<Vec<_>>().join(".")
}

/// The member `member` of the class whose Lean name is `class`.
pub(crate) fn member(class: &str, member: &[u8]) -> String {
    format!("{class}.{}", identifier(member))
}

/// The default of the parameter at `position` of the method whose Lean name is `method`, which a call that leaves its
/// argument out passes: `«App».«Shop».«Order».«ship».«$default1»`.
pub(crate) fn default(method: &str, position: usize) -> String {
    format!("{method}.«$default{position}»")
}

/// A Lean name as Lean prints it and PHP# reads it, without the `«»` it is written in: `App.Shared.Money.add` of
/// `«App».«Shared».«Money».«add»`.
pub(crate) fn unescaped(name: &str) -> String {
    name.replace(['«', '»'], "")
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
    fn every_name_is_escaped_whatever_it_is_and_reads_back_unescaped() {
        assert_eq!(identifier(b"until"), "«until»");
        assert_eq!(identifier(b"currency"), "«currency»");
        assert_eq!(full_name(br"\App\Shared\Money"), "«App».«Shared».«Money»");
        assert_eq!(member("«App».«Range»", b"end"), "«App».«Range».«end»");
        assert_eq!(unescaped(&member("«App».«Range»", b"end")), "App.Range.end");
    }

    #[test]
    fn a_string_is_its_bytes() {
        assert_eq!(string(b"EUR"), "([69, 85, 82] : string)");
        assert_eq!(string(b""), "([] : string)");
    }
}
