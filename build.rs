use std::io;
use std::path::Path;

use mago_prelude::Prelude;

pub fn main() -> io::Result<()> {
    println!("cargo:rustc-env=TARGET={}", std::env::var("TARGET").unwrap());
    println!("cargo:rerun-if-changed=crates/prelude/assets");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR environment variable not set");

    Prelude::write(&Path::new(&out_dir).join("prelude.bin"))
}
