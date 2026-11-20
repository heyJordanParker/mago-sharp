//! Writes the law checker's build for the platform it runs on to the archive its argument names, which the release
//! build embeds in Mago through `MAGO_CHECKER_ARCHIVE`:
//! `cargo run --release -p mago-sharp-lean --example checker -- checker.tar.zst`.

fn main() -> std::io::Result<()> {
    let Some(archive) = std::env::args_os().nth(1) else {
        return Err(std::io::Error::other("name the archive to write"));
    };

    mago_sharp_lean::write_checker(std::path::Path::new(&archive))
}
