#![allow(dead_code)]

use std::borrow::Cow;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use mago_database::DatabaseConfiguration;
use mago_database::GlobSettings;
use mago_extension::Host;
use mago_extension::HostFile;
use mago_extension::Hosts;

pub fn php_sdk_is_available(repository: &std::path::Path, test: &str) -> bool {
    let available = repository.join("vendor/autoload.php").is_file();
    assert!(
        available || std::env::var_os("MAGO_REQUIRE_PHP_SDK_TESTS").is_none(),
        "vendor dependencies are required for {test}"
    );
    available
}

/// The extension host `acme` whose worker `file` registers, as mago runs it from the repository: in each thread's
/// interpreter of the engine linked into the test binary, with `deadline` and `environment`.
pub fn host(file: &Path, deadline: Duration, environment: &[(&str, &str)]) -> Host {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let file = HostFile {
        name: "acme".to_owned(),
        file: file.to_path_buf(),
        deadline,
        environment: environment.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect(),
        ini: Default::default(),
    };
    let sdk = Path::new(env!("CARGO_TARGET_TMPDIR")).join("sdk");

    Hosts::new(vec![file], repository.to_path_buf(), &sdk).expect("the extension host starts").hosts().remove(0)
}

pub fn database_configuration(
    workspace: impl Into<PathBuf>,
    paths: Vec<Cow<'static, [u8]>>,
) -> DatabaseConfiguration<'static> {
    DatabaseConfiguration {
        workspace: Cow::Owned(workspace.into()),
        paths,
        includes: Vec::new(),
        patches: Vec::new(),
        excludes: Vec::new(),
        extensions: vec![Cow::Borrowed(b"php")],
        glob: GlobSettings::default(),
    }
}
