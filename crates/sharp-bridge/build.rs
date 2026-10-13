#![allow(clippy::expect_used, clippy::big_endian_bytes)]

use std::env;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

use cbindgen::Builder;
use cbindgen::Config;
use cbindgen::DocumentationStyle;
use cbindgen::ExportConfig;
use cbindgen::Language;
use cbindgen::Style;

/// Writes the bridge's C header, `sharp_unit.h`, to `OUT_DIR`, and exports that folder to `ext/sharp` as
/// `DEP_SHARP_BRIDGE_INCLUDE`. The header holds the compiled file's layout and its constants.
fn main() {
    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    let abi = crate_dir.join("src/lib.rs");
    let unit = crate_dir.join("src/unit.rs");
    let kinds = crate_dir.join("src/kind.rs");
    let kinds_source = fs::read_to_string(&kinds).expect("the generated sharp_kind is readable");

    let config = Config {
        language: Language::C,
        style: Style::Type,
        usize_is_size_t: true,
        include_guard: Some("SHARP_UNIT_H".to_owned()),
        after_includes: Some(format!("{}{}", constants(&kinds_source), kinds_macro(&kinds_source))),
        documentation_style: DocumentationStyle::C99,
        export: ExportConfig {
            include: vec!["sharp_unit_header".to_owned(), "sharp_input".to_owned(), "sharp_node".to_owned()],
            ..ExportConfig::default()
        },
        ..Config::default()
    };

    Builder::new()
        .with_config(config)
        .with_src(&abi)
        .with_src(&unit)
        .generate()
        .expect("the bridge ABI generates a C header")
        .write_to_file(out_dir.join("sharp_unit.h"));

    for source in [&abi, &unit, &kinds] {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    println!("cargo:include={}", out_dir.display());
}

/// The compiled file is little-endian, and `ext/sharp` compares a file's magic and ABI with these bytes.
fn constants(kinds: &str) -> String {
    let abi = kinds
        .lines()
        .find_map(|line| {
            line.strip_prefix("pub const SHARP_UNIT_ABI: [u8; 16] = 0x")?.strip_suffix("u128.to_be_bytes();")
        })
        .expect("the generated kind.rs holds SHARP_UNIT_ABI");
    let abi = u128::from_str_radix(abi, 16).expect("SHARP_UNIT_ABI is hexadecimal");

    format!(
        "\n#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ != __ORDER_LITTLE_ENDIAN__\n\
         #error \"a .sharpc file is little-endian, and ext/sharp reads it by casting its bytes\"\n\
         #endif\n\n\
         #define SHARP_UNIT_MAGIC {}\n\
         #define SHARP_UNIT_ABI {}\n",
        c_bytes(b"SHARPC\0\0"),
        c_bytes(&abi.to_be_bytes()),
    )
}

/// A C string literal of `bytes`, one escape per byte.
fn c_bytes(bytes: &[u8]) -> String {
    let mut literal = String::from("\"");
    for byte in bytes {
        let _ = write!(literal, "\\x{byte:02x}");
    }
    literal.push('"');

    literal
}

/// `SHARP_KINDS(X)` calls `X(KIND)` once per `sharp_kind`, so `ext/sharp` asserts that each equals its `zend_ast_kind`.
fn kinds_macro(kinds: &str) -> String {
    let calls: Vec<String> = kinds
        .lines()
        .filter_map(|line| line.trim().strip_prefix("SHARP_AST_")?.split_once(" = "))
        .map(|(kind, _)| format!("  X({kind})"))
        .collect();

    format!("\n#define SHARP_KINDS(X) \\\n{}\n", calls.join(" \\\n"))
}
