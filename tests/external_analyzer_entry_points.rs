#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_database::Database;
use mago_database::file::File;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_syntax::settings::ParserSettings;
use mago_word::word;

use crate::common;

const SOURCE: &str = r"<?php

declare(strict_types=1);

#[Attribute(Attribute::TARGET_METHOD)]
final class FrameworkEntry {}

abstract class FrameworkTestCase
{
    protected function inheritedEntry(): void {}
}

trait FrameworkBehavior
{
    private function traitEntry(): void {}
}

final class ConcreteTest extends FrameworkTestCase
{
    use FrameworkBehavior;

    private function actionMatched(): void {}

    #[FrameworkEntry]
    private function attributedMatched(): void {}

    private function actuallyUnused(): void {}
}
";

#[test]
fn declarative_entry_points_reference_inherited_trait_and_attributed_methods_without_callbacks()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(mut service) = service(&[("src/entry-points.php", SOURCE)])? else {
        return Ok(());
    };

    let result = service.analyze()?;
    let unused_methods = result
        .issues
        .iter()
        .filter(|issue| issue.code.as_deref() == Some("unused-method"))
        .map(|issue| issue.message.as_str())
        .collect::<Vec<_>>();
    assert_eq!(unused_methods.len(), 1, "only the undeclared entry point should be unused: {unused_methods:#?}");
    assert!(unused_methods[0].contains("actuallyUnused"));

    let mut recorded_references = Vec::new();
    service.symbol_references().for_each_reference(|_, target, _| recorded_references.push(target));
    for symbol in [
        (word(b"concretetest"), word(b"actionmatched")),
        (word(b"concretetest"), word(b"attributedmatched")),
        (word(b"frameworktestcase"), word(b"inheritedentry")),
        (word(b"frameworkbehavior"), word(b"traitentry")),
    ] {
        assert!(
            service.symbol_references().count_referencing_symbols(&symbol, false) > 0,
            "declarative entry-point matching should reference {symbol:?}; recorded {recorded_references:#?}"
        );
    }

    Ok(())
}

/// An entry point of the PHP# `Demo\Box::total` reaches `total` and never its twin `Total`, a method of its own, spec
/// section 24.
#[test]
fn an_entry_point_of_a_sharp_method_reaches_it_and_not_its_twin() -> Result<(), Box<dyn std::error::Error>> {
    const BOX: &str =
        "namespace Demo;\n\npublic class Box\n{\n    public int total() => 1;\n\n    public int Total() => 2;\n}\n";
    let Some(mut service) = service(&[("src/Box.sharp", BOX)])? else {
        return Ok(());
    };

    service.analyze()?;
    let reached = |method: &[u8]| {
        service.symbol_references().count_referencing_symbols(&(word(b"Demo\\Box"), word(method)), false) > 0
    };

    assert_eq!((reached(b"total"), reached(b"Total")), (true, false));

    Ok(())
}

/// An entry point of the class prefix `demo\Bo*` finds the namespace in any case and the start of a PHP# class's name as
/// written, spec section 24, so it reaches `Demo\Box::run` and not `Demo\BOX::run`.
#[test]
fn an_entry_point_of_a_class_prefix_reaches_a_sharp_class_by_its_written_name() -> Result<(), Box<dyn std::error::Error>>
{
    const BOX: &str = "namespace Demo;\n\npublic class Box\n{\n    public int run() => 1;\n}\n";
    const TWIN: &str = "namespace Demo;\n\npublic class BOX\n{\n    public int run() => 2;\n}\n";
    let Some(mut service) = service(&[("src/Box.sharp", BOX), ("src/BOX.sharp", TWIN)])? else {
        return Ok(());
    };

    service.analyze()?;
    let reached =
        |class: &[u8]| service.symbol_references().count_referencing_symbols(&(word(class), word(b"run")), false) > 0;

    assert_eq!((reached(b"Demo\\Box"), reached(b"Demo\\BOX")), (true, false));

    Ok(())
}

/// A service that analyzes `files`, each a path and its source, with the entry-point fixture worker, or none when the
/// PHP SDK is not available.
fn service(
    files: &[(&'static str, &'static str)],
) -> Result<Option<IncrementalAnalysisService>, Box<dyn std::error::Error>> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external analyzer entry-point test") {
        return Ok(None);
    }

    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-entry-point-worker.php"))
        .with_current_directory(repository);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())?;
    let external = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)?;
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(external)));

    let configuration = common::database_configuration("/entry-point-proof", vec![Cow::Borrowed(b"src")]);
    let mut database = Database::new(configuration);
    for (path, source) in files {
        database.add(File::new(Cow::Borrowed(path.as_bytes()), FileType::Host, None, Cow::Borrowed(source.as_bytes())));
    }

    let mut settings = Settings::new(PHPVersion::PHP85);
    settings.find_unused_definitions = true;
    settings.find_unused_expressions = false;
    settings.find_unused_parameters = false;

    Ok(Some(IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        settings,
        ParserSettings::default(),
        Arc::new(registry),
    )))
}
