//! Writes the prelude `mago`'s build script writes, to this package's `OUT_DIR`, so the orchestrator's tests decode
//! the prelude a user's `mago` decodes. The library itself embeds nothing.

use std::io;
use std::path::Path;

use mago_prelude::Prelude;

pub fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=../prelude/assets");
    println!("cargo:rerun-if-changed=build.rs");

    let out_dir =
        std::env::var("OUT_DIR").map_err(|_| io::Error::other("cargo set no OUT_DIR for the build script"))?;

    Prelude::write(&Path::new(&out_dir).join("prelude.bin"))
}
