use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::Weak;
use std::time::Instant;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerError;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_extension::Host;
use mago_extension::Hosts;
use mago_extension::WorkerError;
use mago_linter::external::ExternalLintError;
use mago_linter::external::ExternalLinter;
use mago_php_version::PHPVersion;

use crate::config::extension::ExtensionHostConfiguration;

/// The hosts this process runs for one configuration, while a linter or an analyzer holds them.
type Running = Option<(BTreeMap<String, ExtensionHostConfiguration>, Weak<Hosts>)>;

/// Each enabled host of `extension_hosts`, or none when no host is enabled. A linter and an analyzer of one
/// configuration share its hosts, since each thread runs one interpreter, so the hosts start once while either holds
/// them, and stop, running their worker reducers, once neither does.
fn hosts(extension_hosts: &BTreeMap<String, ExtensionHostConfiguration>) -> Result<Option<Vec<Host>>, WorkerError> {
    static RUNNING: Mutex<Running> = Mutex::new(None);

    let enabled: Vec<_> = extension_hosts.iter().filter(|(_, host)| host.enabled).collect();
    let Some((_, first)) = enabled.first() else {
        return Ok(None);
    };

    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((configuration, hosts)) = running.as_ref()
        && configuration == extension_hosts
        && let Some(hosts) = hosts.upgrade()
    {
        return Ok(Some(hosts.hosts()));
    }

    let cache = mago_sharp_lean::cache_root().map_err(WorkerError::Sdk)?.join("sdk");
    let files = enabled.iter().map(|(name, host)| host.host_file(name)).collect();
    let hosts = Hosts::new(files, first.directory().to_path_buf(), &cache)?;
    *running = Some((extension_hosts.clone(), Arc::downgrade(&hosts)));

    Ok(Some(hosts.hosts()))
}

/// Starts every enabled extension host and validates its linter registration.
pub(crate) fn initialize_external_linter(
    extension_hosts: &BTreeMap<String, ExtensionHostConfiguration>,
    php_version: PHPVersion,
) -> Result<Option<ExternalLinter>, ExternalLintError> {
    let trace_start = tracing::enabled!(tracing::Level::TRACE).then(Instant::now);
    let Some(hosts) = hosts(extension_hosts)? else {
        tracing::trace!("No external linter hosts are enabled.");
        return Ok(None);
    };

    let linter = ExternalLinter::initialize(hosts, php_version)?;
    if let Some(start) = trace_start {
        tracing::trace!(
            extensions = linter.extensions().len(),
            rules = linter.extensions().iter().map(|extension| extension.rules.len()).sum::<usize>(),
            elapsed = ?start.elapsed(),
            "External linter hosts initialized."
        );
    }

    Ok(Some(linter))
}

/// Starts every enabled extension host and validates its analyzer registration.
pub(crate) fn initialize_external_analyzer(
    extension_hosts: &BTreeMap<String, ExtensionHostConfiguration>,
    php_version: PHPVersion,
    enabled_plugins: &[String],
    disable_defaults: bool,
) -> Result<Option<ExternalAnalyzer>, ExternalAnalyzerError> {
    let trace_start = tracing::enabled!(tracing::Level::TRACE).then(Instant::now);
    let Some(hosts) = hosts(extension_hosts)? else {
        tracing::trace!("No external analyzer hosts are enabled.");
        return Ok(None);
    };

    let analyzer = ExternalAnalyzer::initialize(hosts, php_version, enabled_plugins, disable_defaults)?;
    if let Some(start) = trace_start {
        tracing::trace!(
            extensions = analyzer.extensions().len(),
            plugins = analyzer.extensions().iter().map(|extension| extension.plugins.len()).sum::<usize>(),
            elapsed = ?start.elapsed(),
            "External analyzer hosts initialized."
        );
    }

    Ok(Some(analyzer))
}

/// Starts external analyzer initialization without blocking the codebase pipeline.
pub(crate) fn start_external_analyzer(
    extension_hosts: &BTreeMap<String, ExtensionHostConfiguration>,
    php_version: PHPVersion,
    enabled_plugins: &[String],
    disable_defaults: bool,
) -> Option<Arc<ExternalAnalyzerHandle>> {
    if !extension_hosts.values().any(|host| host.enabled) {
        tracing::trace!("Skipping external analyzer initialization because no hosts are enabled.");
        return None;
    }

    let extension_hosts = extension_hosts.clone();
    let enabled_plugins = enabled_plugins.to_vec();
    tracing::trace!(
        enabled_hosts = extension_hosts.values().filter(|host| host.enabled).count(),
        "Starting concurrent external analyzer initialization."
    );

    let initializer = std::thread::Builder::new()
        .name("mago-external-analyzer-init".to_string())
        .spawn(move || {
            let start = tracing::enabled!(tracing::Level::TRACE).then(Instant::now);
            tracing::trace!("External analyzer initialization thread started.");
            let analyzer =
                initialize_external_analyzer(&extension_hosts, php_version, &enabled_plugins, disable_defaults)?
                    .ok_or_else(|| ExternalAnalyzerError::protocol("external analyzer has no enabled hosts"));
            if let Some(start) = start {
                tracing::trace!(elapsed = ?start.elapsed(), success = analyzer.is_ok(), "External analyzer initialization thread finished.");
            }

            analyzer
        })
        .expect("failed to spawn external analyzer initialization thread");

    Some(Arc::new(ExternalAnalyzerHandle::pending(initializer)))
}
