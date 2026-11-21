use std::process::ExitCode;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::Parser;
use rand::RngCore;

use crate::error::Error;

/// Prints a new key for the key ring `SHARP_KEYS`, spec section 24, which a `Secret`'s stored form is encrypted with.
#[derive(Parser, Debug)]
#[command(
    name = "generate-key",
    about = "Print a new key for SHARP_KEYS, which encrypts each Secret's stored form",
    long_about = r#"
The `generate-key` command prints a new key for the key ring `SHARP_KEYS`: `<id>:<base64 of 32 random bytes>`, its id
8 hex characters. Put it first in `SHARP_KEYS`, as in `SHARP_KEYS=<new key>,<older key>`: a new `Secret` is encrypted
with the first key, and one encrypted with an older key still decrypts while the ring holds that key.
"#
)]
pub struct GenerateKeyCommand;

impl GenerateKeyCommand {
    pub fn execute(self) -> Result<ExitCode, Error> {
        let mut bytes = [0u8; 36];
        rand::rng().fill_bytes(&mut bytes);
        let (id, key) = bytes.split_at(4);
        let id: String = id.iter().map(|byte| format!("{byte:02x}")).collect();
        println!("{id}:{}", STANDARD.encode(key));

        Ok(ExitCode::SUCCESS)
    }
}
