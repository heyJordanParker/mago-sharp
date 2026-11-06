#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings;
use mago_database::Database;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::IncrementalAnalysisService;
use mago_php_version::PHPVersion;
use mago_syntax::settings::ParserSettings;

mod common;

type AuditEntry = (u32, BTreeMap<String, Vec<String>>);

fn database(first: &str, ignored: usize) -> (Database<'static>, FileId, FileId) {
    let configuration = common::database_configuration("/scan-proof", vec![Cow::Borrowed(b".")]);
    let mut database = Database::new(configuration);
    let first = File::new(
        Cow::Borrowed(b"database/migrations/001.php"),
        FileType::Host,
        None,
        Cow::Owned(format!("<?php\n\n#[Marker(\"{first}\")]\nfunction first(): void {{}}\n").into_bytes()),
    );
    let first_id = first.id;
    database.add(first);
    database.add(File::new(
        Cow::Borrowed(b"database/migrations/nested/002.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\n\n#[Marker('two')]\nfunction second(): void {}\n"),
    ));
    let ignored = File::new(
        Cow::Borrowed(b"src/ignored.php"),
        FileType::Host,
        None,
        Cow::Owned(format!("<?php\n\nconst IGNORED = {ignored};\n").into_bytes()),
    );
    let ignored_id = ignored.id;
    database.add(ignored);
    database.add(File::new(
        Cow::Borrowed(b"database/migrations/003.php"),
        FileType::Vendored,
        None,
        Cow::Borrowed(b"<?php\n\n$value = 'vendor';\n"),
    ));

    (database, first_id, ignored_id)
}

fn audit(path: &Path) -> Vec<AuditEntry> {
    std::fs::read_to_string(path)
        .expect("scan audit should be readable")
        .lines()
        .map(|line| serde_json::from_str(line).expect("scan audit should contain JSON records"))
        .collect()
}

#[test]
fn selected_declarations_refresh_every_worker_incrementally() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external analyzer scan test") {
        return;
    }

    let temporary = tempfile::tempdir().expect("temporary scan directory should be created");
    let audit_log = temporary.path().join("audit.jsonl");
    std::fs::write(&audit_log, []).expect("scan audit should be initialized");
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-scan-worker.php"))
        .with_current_directory(repository)
        .with_environment("MAGO_SCAN_AUDIT_LOG", &audit_log);
    let pool = WorkerPool::spawn(command, NonZeroUsize::new(3).expect("three workers"), WorkerPoolOptions::default())
        .expect("scan worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("scan analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    let (initial, migration_id, ignored_id) = database("one\\n", 1);
    let mut settings = Settings::new(PHPVersion::PHP85);
    settings.find_unused_definitions = false;
    settings.find_unused_expressions = false;
    let mut service = IncrementalAnalysisService::new(
        initial.read_only(),
        Default::default,
        settings,
        ParserSettings::default(),
        Arc::new(registry),
    );

    service.analyze().expect("initial analysis should scan selected files");
    let initial_audit = audit(&audit_log);
    assert_eq!(initial_audit.len(), 3);
    for (_, files) in &initial_audit {
        assert_eq!(files.len(), 2);
        assert_eq!(files["database/migrations/001.php"], ["one\n"]);
        assert_eq!(files["database/migrations/nested/002.php"], ["two"]);
    }

    let (changed, _, _) = database("changed\\t", 1);
    service.update_database(changed.read_only());
    service.analyze_incremental(Some(&[migration_id])).expect("a selected source change should refresh every worker");
    let changed_audit = audit(&audit_log);
    assert_eq!(changed_audit.len(), 6);
    for (_, files) in &changed_audit[3..] {
        assert_eq!(files["database/migrations/001.php"], ["changed\t"]);
    }

    let (ignored, _, _) = database("changed\\t", 2);
    service.update_database(ignored.read_only());
    service.analyze_incremental(Some(&[ignored_id])).expect("an unrelated source change should remain analyzable");
    assert_eq!(audit(&audit_log).len(), 6, "unmatched changes must not cross the worker boundary");
}

/// An entity in plain PHP, whose attributes read classes, enums and a library's constants.
const PHP_ENTITY: &str = "<?php\n\nnamespace App\\Php;\n\nuse App\\Status;\nuse Lib\\Order;\nuse Lib\\Plan;\nuse Lib\\Type;\n\n#[Type(Type::String, plan: Plan::Starter, of: Order::class, keys: [1, 2], map: ['a' => 1], item: new Type(Order::class))]\n#[Read(\\Vendor\\Pdf\\DEFAULT_DPI, \\APP_LIMIT, \\Vendor\\Pdf\\render(...), \\App\\Tier\\Free::instance(), \\App\\Lookup\\Missing::instance(), [Status::Paid->value => 'Paid'])]\nfinal class Entity {}\n";

/// The PHP# twin of [`PHP_ENTITY`], whose every argument is the PHP that means it, spec sections 20, 23 and 25.
const SHARP_ENTITY: &str = "namespace App.Sharp;\n\nimport App.Lookup;\nimport App.Status;\nimport App.Tier;\nimport Illuminate.Foundation;\nimport Lib.Order;\nimport Lib.Plan;\nimport Lib.Type;\nimport Vendor.Pdf;\n\n[Type(Type.String, plan: Plan.Starter, of: typeof(Order), keys: [1, 2], map: [\"a\": 1], item: new Type(typeof(Order)))]\n[Read(Pdf.DEFAULT_DPI, Foundation.APP_LIMIT, Pdf.render, Tier.Free, Lookup<int>.Missing, [Status.Paid: \"Paid\"])]\nclass Entity {}\n";

/// The twins and the code their attributes read, where `pdf` declares the library's namespaced constants: a library's
/// function and constant in a namespace, a constant in PHP's root namespace beside `Illuminate\Foundation`'s classes, a
/// backed enum, and two PHP# enums whose cases carry data. `Lib\Order`, `Lib\Plan` and `Lib\Type` are in no file.
fn twins(pdf: &str) -> Database<'static> {
    let mut database = Database::new(common::database_configuration("/scan-proof", vec![Cow::Borrowed(b".")]));
    let files = [
        (
            "lib/pdf.php",
            format!(
                "<?php\n\nnamespace Vendor\\Pdf;\n\n{pdf}function render(string $html): string\n{{\n    return $html;\n}}\n"
            ),
        ),
        ("lib/Foundation/helpers.php", "<?php\n\nconst APP_LIMIT = 5;\n".to_owned()),
        (
            "lib/Foundation/Application.php",
            "<?php\n\nnamespace Illuminate\\Foundation;\n\nclass Application\n{\n}\n".to_owned(),
        ),
        (
            "lib/Status.php",
            "<?php\n\nnamespace App;\n\nenum Status: string\n{\n    case Paid = 'paid';\n}\n".to_owned(),
        ),
        (
            "lib/Tier.sharp",
            "namespace App;\n\npublic enum Tier\n{\n    case Free;\n    case Paid(int seats);\n}\n".to_owned(),
        ),
        (
            "lib/Lookup.sharp",
            "namespace App;\n\npublic enum Lookup<T>\n{\n    case Found(T value);\n    case Missing;\n}\n".to_owned(),
        ),
        ("src/Php/Entity.php", PHP_ENTITY.to_owned()),
        ("src/Sharp/Entity.sharp", SHARP_ENTITY.to_owned()),
    ];
    for (name, code) in files {
        database.add(File::new(Cow::Borrowed(name.as_bytes()), FileType::Host, None, Cow::Owned(code.into_bytes())));
    }

    database
}

/// An analysis of `database` whose one worker records each class attribute argument of `src/` it receives in
/// `audit_log`, or None without PHP and the SDK.
fn attribute_scan(database: &Database<'static>, audit_log: &Path) -> Option<IncrementalAnalysisService> {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the external analyzer scan test") {
        return None;
    }

    std::fs::write(audit_log, []).expect("scan audit should be initialized");
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-attribute-scan-worker.php"))
        .with_current_directory(repository)
        .with_environment("MAGO_SCAN_AUDIT_LOG", audit_log);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
        .expect("scan worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("scan analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    Some(IncrementalAnalysisService::new(
        database.read_only(),
        Default::default,
        Settings::new(PHPVersion::PHP85),
        ParserSettings::default(),
        Arc::new(registry),
    ))
}

/// What the latest scan sent for each argument of `path`'s class: its name, its type, and its value's kind and parts.
fn arguments(audit_log: &Path, path: &str) -> Vec<serde_json::Value> {
    let scans = audit(audit_log);
    let (_, files) = scans.last().expect("the worker records a scan");

    files[path].iter().map(|argument| serde_json::from_str(argument).expect("an argument is JSON")).collect()
}

/// A value's kind and parts as the worker records them.
fn value(kind: &str, value: serde_json::Value, constant: Option<&str>) -> serde_json::Value {
    serde_json::json!({ "kind": kind, "value": value, "constant": constant, "items": [], "arguments": [] })
}

/// A host types an entity by its attributes, so each argument of a PHP# attribute reaches it as its PHP twin does, with
/// the same kind and parts. `Type.String` of a class no file declares is the class constant `Type::String` is, and
/// `Pdf.DEFAULT_DPI` after `import Vendor.Pdf;` the library's constant `\Vendor\Pdf\DEFAULT_DPI`, spec section 23. A
/// function read through its namespace, a case of an enum whose cases carry data, which runs as an object, and a backed
/// enum's case keying a map, which is its value, are no constant expression, in PHP# as in PHP.
#[test]
fn a_sharp_attribute_argument_reaches_a_host_as_its_php_twin() {
    let temporary = tempfile::tempdir().expect("temporary scan directory should be created");
    let audit_log = temporary.path().join("audit.jsonl");
    let database = twins("const DEFAULT_DPI = 300;\n\n");
    let Some(mut service) = attribute_scan(&database, &audit_log) else {
        return;
    };
    service.analyze().expect("the analysis should scan both twins");

    let order = || value("ClassName", "Lib\\Order".into(), None);
    let literal = |literal: serde_json::Value| value("Literal", literal, None);
    let unsupported = || value("Unsupported", serde_json::Value::Null, None);
    let array = |items: serde_json::Value| serde_json::json!({ "kind": "Array_", "value": null, "constant": null, "items": items, "arguments": [] });
    let values = [
        (None, value("ClassConstant", "Lib\\Type".into(), Some("String"))),
        (Some("plan"), value("ClassConstant", "Lib\\Plan".into(), Some("Starter"))),
        (Some("of"), order()),
        (Some("keys"), array(serde_json::json!([[null, literal(1.into())], [null, literal(2.into())]]))),
        (Some("map"), array(serde_json::json!([[literal("a".into()), literal(1.into())]]))),
        (
            Some("item"),
            serde_json::json!({ "kind": "New_", "value": "Lib\\Type", "constant": null, "items": [], "arguments": [[null, order()]] }),
        ),
        (None, value("Constant", "Vendor\\Pdf\\DEFAULT_DPI".into(), None)),
        (None, value("Constant", "APP_LIMIT".into(), None)),
        (None, unsupported()),
        (None, unsupported()),
        (None, unsupported()),
        (None, array(serde_json::json!([[unsupported(), literal("Paid".into())]]))),
    ];
    let without_types = |arguments: Vec<serde_json::Value>| {
        arguments
            .into_iter()
            .map(|argument| (argument[0].as_str().map(str::to_owned), argument[2].clone()))
            .collect::<Vec<_>>()
    };
    let expected = values.iter().map(|(name, value)| (name.map(str::to_owned), value.clone())).collect::<Vec<_>>();

    assert_eq!(without_types(arguments(&audit_log, "src/Php/Entity.php")), expected);
    assert_eq!(without_types(arguments(&audit_log, "src/Sharp/Entity.sharp")), expected);
    // A read of a namespace's constant or function, or of a case object, has no type in a scan, as a constant or a call
    // has none in PHP. A type goes as its file's code sees it, so a map is a `Sharp\Map` in PHP#.
    let types = |path| {
        serde_json::Value::from(
            arguments(&audit_log, path).into_iter().map(|argument| argument[1].clone()).collect::<Vec<_>>(),
        )
    };
    assert_eq!(
        types("src/Php/Entity.php"),
        serde_json::json!([
            "Lib\\Type",
            "Lib\\Plan",
            "class-string",
            "list",
            "array",
            null,
            null,
            null,
            null,
            null,
            null,
            "array"
        ])
    );
    assert_eq!(
        types("src/Sharp/Entity.sharp"),
        serde_json::json!([
            "Lib\\Type",
            "Lib\\Plan",
            "class-string",
            "list",
            "Sharp\\Map",
            null,
            null,
            null,
            null,
            null,
            null,
            "Sharp\\Map"
        ])
    );
}

/// A PHP# member read resolves against the whole scan, so a change to the file that declares what it reads reaches
/// the host in the payload of the unchanged file that reads it: once `Vendor\Pdf\DEFAULT_DPI` is gone, `Pdf.DEFAULT_DPI`
/// names nothing in the namespace, and is the class constant its PHP twin `Pdf::DEFAULT_DPI` would be.
#[test]
fn a_sharp_member_read_resolves_again_when_the_file_it_reads_changes() {
    let temporary = tempfile::tempdir().expect("temporary scan directory should be created");
    let audit_log = temporary.path().join("audit.jsonl");
    let database = twins("const DEFAULT_DPI = 300;\n\n");
    let Some(mut service) = attribute_scan(&database, &audit_log) else {
        return;
    };
    service.analyze().expect("the first analysis should scan the twins");
    let dpi = |audit_log: &Path| arguments(audit_log, "src/Sharp/Entity.sharp")[6][2].clone();
    assert_eq!(dpi(&audit_log), value("Constant", "Vendor\\Pdf\\DEFAULT_DPI".into(), None));

    service.update_database(twins("").read_only());
    service.analyze_incremental(Some(&[FileId::new(b"lib/pdf.php")])).expect("the library's change should rescan");

    assert_eq!(audit(&audit_log).len(), 2, "the change reaches the host");
    assert_eq!(dpi(&audit_log), value("ClassConstant", "Vendor\\Pdf".into(), Some("DEFAULT_DPI")));
}
