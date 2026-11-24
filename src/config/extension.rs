use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use mago_extension::HostFile;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Configuration for one extension host.
///
/// mago runs the host's file inside itself, in each interpreter of the PHP# engine it links, which every host shares.
/// The file registers the host's worker, which serves the linter rules and analyzer capabilities of its extensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExtensionHostConfiguration {
    /// Whether this extension host is active.
    pub enabled: bool,

    /// The PHP file that registers the host's worker. A relative path resolves from the effective configuration
    /// file's directory, which each interpreter also starts in.
    pub file: PathBuf,

    /// Environment variables the host's worker reads through `getenv()`, `$_ENV` and `$_SERVER`, over mago's own. All
    /// hosts share each interpreter, so two hosts that set one variable set it to the same value.
    pub environment: BTreeMap<String, String>,

    /// PHP settings the host's worker runs with, by name, as `php -d` sets them before the host's file runs:
    /// `memory_limit`, `error_log` for the file PHP logs to, or any other `php.ini` directive. All hosts share each
    /// interpreter, so two hosts that set one setting set it to the same value.
    pub ini: BTreeMap<String, String>,

    /// Longest one unit of a request may run, in milliseconds. It counts from when the worker starts a request, and
    /// starts again each time the worker starts one of the request's units: a hook, before-analysis, after-file,
    /// after-analysis or node, a file's hooks in an after-file batch, one plugin's initialization hooks, a codebase-scan
    /// hook, or an extension's worker reducer. A hook that runs past it fails the request, and the error names the host
    /// and the hook.
    pub request_timeout_ms: u64,

    #[serde(skip)]
    #[schemars(skip)]
    base_directory: PathBuf,
}

impl Default for ExtensionHostConfiguration {
    fn default() -> Self {
        Self {
            enabled: true,
            file: PathBuf::new(),
            environment: BTreeMap::new(),
            ini: BTreeMap::new(),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            base_directory: PathBuf::new(),
        }
    }
}

impl ExtensionHostConfiguration {
    pub(super) fn normalize(&mut self, name: &str, base_directory: &Path) -> Result<(), String> {
        self.base_directory = base_directory.to_path_buf();

        if !self.enabled {
            return Ok(());
        }

        if self.file.as_os_str().is_empty() {
            return Err(format!("extension host `{name}` must name the PHP file that registers its worker in `file`"));
        }

        if self.request_timeout_ms == 0 {
            return Err(format!("extension host `{name}` request-timeout-ms must be greater than zero"));
        }

        Ok(())
    }

    /// The host `name` as mago runs it: its file resolved from the configuration's directory, its deadline, its
    /// environment and its PHP settings.
    #[must_use]
    pub fn host_file(&self, name: &str) -> HostFile {
        HostFile {
            name: name.to_owned(),
            file: self.base_directory.join(&self.file),
            deadline: Duration::from_millis(self.request_timeout_ms),
            environment: self.environment.clone(),
            ini: self.ini.clone(),
        }
    }

    /// The effective configuration file's directory, which each interpreter starts in.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.base_directory
    }
}
