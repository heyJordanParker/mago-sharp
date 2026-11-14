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
#[allow(clippy::unwrap_used)]
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

    /// `autoload`'s and `autoload-dev`'s PSR-4 roots keep the order `composer.json` writes them in, since the first root
    /// decides where `mago convert` writes `Externs.sharp`.
    #[test]
    fn the_psr_4_roots_keep_the_order_composer_json_writes() {
        let roots = ["Zoo\\", "App\\", "Mid\\", "Beta\\", "Yard\\", "Core\\", "Lib\\", "Kit\\", "Edge\\", "Dock\\"];
        let map = r#"{"Zoo\\": "a/", "App\\": "b/", "Mid\\": "c/", "Beta\\": "d/", "Yard\\": "e/", "Core\\": "f/", "Lib\\": "g/", "Kit\\": "h/", "Edge\\": "i/", "Dock\\": "j/"}"#;
        let json = format!(r#"{{"autoload": {{"psr-4": {map}}}, "autoload-dev": {{"psr-4": {map}}}}}"#);

        let package = ComposerPackage::from_str(&json).unwrap();
        let autoload: Vec<&str> = package.autoload.as_ref().unwrap().psr_4.keys().map(String::as_str).collect();
        let autoload_dev: Vec<&str> = package.autoload_dev.as_ref().unwrap().psr_4.keys().map(String::as_str).collect();

        assert_eq!((autoload, autoload_dev), (roots.to_vec(), roots.to_vec()));
    }
}
