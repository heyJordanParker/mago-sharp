#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use mago_linter::external::ExternalLinter;
use mago_php_version::PHPVersion;

use crate::common;

/// Each interpreter that served the host is one worker. Once mago stops the host, its reducers collect each one's
/// state, on the thread it runs on, and reduce all of it in one of them. The host runs in a pool of its own, so no other
/// test's engine takes one of its threads meanwhile.
#[test]
fn worker_reducers_merge_every_interpreter_in_one() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("MAGO_TRACE_PHP_SDK_TESTS").is_some() {
        let _subscriber = tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .with_test_writer()
            .try_init();
    }

    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external worker-reduction test") {
        return Ok(());
    }

    let temporary = tempfile::tempdir()?;
    let host = common::host(
        &repository.join("composer/tests/Sdk/Fixtures/worker-reducer.php"),
        Duration::from_secs(30),
        &[("MAGO_REDUCTION_AUDIT_DIRECTORY", temporary.path().to_str().expect("a UTF-8 path"))],
    );
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let started = pool.install(move || {
        let started = rayon::broadcast(|_| ExternalLinter::initialize([host.clone()], PHPVersion::PHP85).is_ok());
        drop(host);
        started
    });

    let alpha = read_audit(&temporary.path().join("alpha.txt"), "alpha")?;
    let beta = read_audit(&temporary.path().join("beta.txt"), "beta")?;
    assert!(started.iter().all(|started| *started));
    assert_eq!(alpha.worker_ids, beta.worker_ids, "logical extensions must observe the same worker order");
    assert_eq!(alpha.worker_ids.iter().collect::<HashSet<_>>().len(), 3);
    assert!(alpha.worker_ids.contains(&alpha.leader), "the reducing interpreter's own contribution must be included");
    assert!(beta.worker_ids.contains(&beta.leader), "the reducing interpreter's own contribution must be included");
    assert_eq!(alpha.leader, beta.leader, "all reducers must run in the same interpreter");

    Ok(())
}

struct Audit {
    leader: String,
    worker_ids: Vec<String>,
}

fn read_audit(path: &Path, identifier: &str) -> Result<Audit, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let lines = contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.first(), Some(&identifier));
    let worker_ids = lines[2..]
        .iter()
        .map(|payload| {
            payload
                .strip_prefix(identifier)
                .and_then(|payload| payload.strip_prefix(':'))
                .expect("payload must retain its extension identity")
                .to_string()
        })
        .collect();

    Ok(Audit { leader: lines[1].to_string(), worker_ids })
}
