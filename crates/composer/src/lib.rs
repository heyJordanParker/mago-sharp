#![allow(clippy::exhaustive_enums)]
#![allow(clippy::pub_use)]

use std::str::FromStr;

use serde_json::Error;
use serde_json::from_str;

pub use crate::schema::*;

pub mod schema;

impl FromStr for ComposerPackage {
    type Err = Error;

    #[inline]
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        from_str(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Composer's legacy `include-path` and `target-dir` keys, which the schema does not model, still parse, in the
    /// package and in an inline package of a repository.
    #[test]
    fn a_package_with_legacy_keys_parses() {
        let json = r#"{
            "name": "acme/legacy",
            "include-path": ["lib/"],
            "target-dir": "Acme/Legacy",
            "repositories": [
                {"type": "package", "package": {"name": "acme/old", "version": "1.0.0", "include-path": ["src/"], "target-dir": "Old"}}
            ]
        }"#;

        let name = ComposerPackage::from_str(json).map(|package| package.name.map(|name| name.0));

        assert_eq!(name.map_err(|error| error.to_string()), Ok(Some("acme/legacy".to_owned())));
    }
}
