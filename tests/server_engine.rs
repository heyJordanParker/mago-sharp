#![allow(clippy::expect_used, clippy::missing_panics_doc)]

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;

use mago_analyzer::external::ExternalAnalyzer;
use mago_analyzer::external::ExternalAnalyzerHandle;
use mago_analyzer::plugin::PluginRegistry;
use mago_analyzer::settings::Settings as AnalyzerSettings;
use mago_database::Database;
use mago_database::DatabaseReader;
use mago_database::file::File;
use mago_database::file::FileId;
use mago_database::file::FileType;
use mago_extension::WorkerCommand;
use mago_extension::WorkerPool;
use mago_extension::WorkerPoolOptions;
use mago_orchestrator::service::incremental_analysis::compile::Compilation;
use mago_php_version::PHPVersion;
use mago_reporting::IssueCollection;
use mago_server::Server;
use mago_server::Settings;
use mago_sharp_bridge::unit::Input;
use mago_sharp_bridge::unit::header;
use mago_sharp_bridge::unit::source_hash;
use mago_syntax::settings::ParserSettings;

mod common;

const USER: &str = "<?php\nfunction use_box(Box $box): int { return $box->value(); }\n";

const BOX: &str = "<?php\nfinal class Box { public function value(): int { return 1; } }\n";

fn database(files: &[(&'static str, &str)]) -> Database<'static> {
    let mut database = Database::new(common::database_configuration("/server-proof", vec![Cow::Borrowed(b"src")]));
    for (name, contents) in files {
        database.add(File::new(
            Cow::Borrowed(name.as_bytes()),
            FileType::Host,
            None,
            Cow::Owned(contents.as_bytes().to_vec()),
        ));
    }

    database
}

fn server(repository: &Path, database: Database<'static>) -> Server {
    let command = WorkerCommand::new("php")
        .with_argument(repository.join("composer/tests/Sdk/Fixtures/analyzer-server-worker.php"))
        .with_current_directory(repository);
    let pool = WorkerPool::spawn(command, NonZeroUsize::MIN, WorkerPoolOptions::default())
        .expect("server proof worker pool should start");
    let analyzer = ExternalAnalyzer::initialize([Arc::new(pool)], PHPVersion::PHP85, &[], false)
        .expect("server proof analyzer should initialize");
    let mut registry = PluginRegistry::with_library_providers();
    registry.set_external_analyzer(Arc::new(ExternalAnalyzerHandle::ready(analyzer)));

    let settings = Settings {
        parser: ParserSettings::default(),
        analyzer: AnalyzerSettings::new(PHPVersion::PHP85),
        plugin_registry: Arc::new(registry),
        use_progress_bars: false,
    };

    Server::new(database, Default::default, settings)
}

fn codes(issues: &IssueCollection) -> BTreeMap<String, Vec<String>> {
    let mut codes = BTreeMap::<String, Vec<String>>::new();
    for issue in issues.iter() {
        codes.entry(issue.code.clone().unwrap_or_default()).or_default().push(issue.message.clone());
    }
    codes
}

#[test]
fn a_scan_only_server_reports_only_the_before_analysis_issues() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the scan-only server test") {
        return;
    }

    let files = [
        ("src/marker.php", "<?php\nconst PROOF_BEFORE = 1;\n"),
        ("src/broken.php", "<?php\nfunction broken(): int { return 'text'; }\n"),
    ];

    let full = codes(&server(repository, database(&files)).analyze().expect("full analysis").issues);
    assert!(full.contains_key("invalid-return-statement"), "a full analysis analyzes files: {full:#?}");

    let scanned = codes(&server(repository, database(&files)).scan_only().analyze().expect("scan").issues);
    assert_eq!(scanned.keys().collect::<Vec<_>>(), ["server-proof/before"], "{scanned:#?}");
}

#[test]
fn a_failed_analysis_resets_the_server_so_the_next_answers_like_a_fresh_one() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the server reset test") {
        return;
    }

    let mut warm = server(
        repository,
        database(&[("src/user.php", USER), ("src/box.php", BOX), ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n")]),
    );
    warm.analyze().expect("initial analysis");

    let flag = FileId::new(b"src/flag.php");
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_FAIL = 1;\n"));
    assert!(warm.analyze_incremental(&[flag]).is_err(), "the after-analysis hook fails the pass");

    let user = FileId::new(b"src/user.php");
    let edited = "<?php\nfunction use_box(Box $box): int { return $box->value() + 1; }\n";
    warm.database_mut().update(flag, Cow::Borrowed(b"<?php\nconst PROOF_OK = 1;\n"));
    warm.database_mut().update(user, Cow::Borrowed(edited.as_bytes()));
    let recovered = codes(&warm.analyze_incremental(&[flag, user]).expect("recovered analysis").issues);

    let fresh = codes(
        &server(
            repository,
            database(&[
                ("src/user.php", edited),
                ("src/box.php", BOX),
                ("src/flag.php", "<?php\nconst PROOF_OK = 1;\n"),
            ]),
        )
        .analyze()
        .expect("fresh analysis")
        .issues,
    );
    assert_eq!(recovered, fresh);
}

/// The messages the reading hook reports under `code`.
fn reads(issues: &IssueCollection, code: &str) -> Vec<String> {
    let code = format!("server-reads/{code}");
    issues
        .iter()
        .filter(|issue| issue.code.as_deref() == Some(code.as_str()))
        .map(|issue| issue.message.clone())
        .collect()
}

#[test]
fn a_hook_that_read_a_class_runs_again_when_the_class_signature_changes_and_not_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the recorded reads test") {
        return;
    }

    let mut server =
        server(repository, database(&[("src/reader.php", "<?php\n// reads: Box::value\n"), ("src/box.php", BOX)]));
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(reads(&first, "read"), ["Run 1: `Box::value` is Public."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(reads(&body, "read"), ["Run 1: `Box::value` is Public."], "a body edit leaves the read alone");

    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { private function value(): int { return 2; } }\n"));
    let hidden = server.analyze_incremental(&[box_file]).expect("analysis after a visibility change").issues;
    assert_eq!(reads(&hidden, "read"), ["Run 2: `Box::value` is Private."]);
}

#[test]
fn a_hook_that_listed_classes_sees_a_new_class_on_the_next_run_and_does_not_run_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the listing reads test") {
        return;
    }

    let mut server =
        server(repository, database(&[("src/lister.php", "<?php\n// lists: classes\n"), ("src/box.php", BOX)]));
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(reads(&first, "listing"), ["Run 1: classes Box."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(reads(&body, "listing"), ["Run 1: classes Box."], "a body edit leaves the listing alone");

    let crate_file = FileId::new(b"src/crate.php");
    server.database_mut().add(File::new(
        Cow::Borrowed(b"src/crate.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\nfinal class Crate {}\n"),
    ));
    let added = server.analyze_incremental(&[crate_file]).expect("analysis after a new class").issues;
    assert_eq!(reads(&added, "listing"), ["Run 2: classes Box, Crate."]);

    server.database_mut().delete(crate_file);
    let removed = server.analyze_incremental(&[crate_file]).expect("analysis after a removed class").issues;
    assert_eq!(reads(&removed, "listing"), ["Run 3: classes Box."]);
}

/// The messages the reading hook reports under `comparison`, sorted, since files analyzed together report in any order.
fn comparisons(issues: &IssueCollection) -> Vec<String> {
    let mut messages = reads(issues, "comparison");
    messages.sort();
    messages
}

/// Two files whose hook compares `Box`: the worker answers the second file's comparison from its cache.
#[test]
fn hooks_that_compared_a_class_run_again_when_the_class_changes_parent_and_not_after_a_body_edit() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the comparison reads test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("src/billing.php", "<?php\n// compares: Box\n"),
            ("src/payroll.php", "<?php\n// compares: Box\n"),
            ("src/base.php", "<?php\nclass Base {}\n"),
            ("src/box.php", BOX),
        ]),
    );
    let first = server.analyze().expect("initial analysis").issues;
    assert_eq!(comparisons(&first), ["Run 1: `Box` is not a `Base`.", "Run 2: `Box` is not a `Base`."]);

    let box_file = FileId::new(b"src/box.php");
    server
        .database_mut()
        .update(box_file, Cow::Borrowed(b"<?php\nfinal class Box { public function value(): int { return 2; } }\n"));
    let body = server.analyze_incremental(&[box_file]).expect("analysis after a body edit").issues;
    assert_eq!(comparisons(&body), comparisons(&first), "a body edit leaves the comparisons alone");

    server.database_mut().update(
        box_file,
        Cow::Borrowed(b"<?php\nfinal class Box extends Base { public function value(): int { return 2; } }\n"),
    );
    let extended = server.analyze_incremental(&[box_file]).expect("analysis after a parent change").issues;
    assert_eq!(comparisons(&extended), ["Run 3: `Box` is a `Base`.", "Run 4: `Box` is a `Base`."]);
}

/// The model whose `total` return type the fixture's provider gives `Query::total`.
fn order(returned: &str) -> String {
    format!(
        "<?php\n\nnamespace App\\Models;\n\nfinal class Order\n{{\n    public int $count = 0;\n\n    public function total(): {returned}\n    {{\n        return {};\n    }}\n}}\n",
        if returned == "int" { "1" } else { "'1'" }
    )
}

const QUERY: &str = "<?php\n\nnamespace App\\Models;\n\nfinal class Query\n{\n    public function total(): mixed\n    {\n        return null;\n    }\n\n    public function models(): mixed\n    {\n        return null;\n    }\n\n    public function billable(): mixed\n    {\n        return null;\n    }\n}\n";

/// A PHP# file whose `int` return holds only while the provider answers `Query::total` with `int`.
const REVENUE: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Revenue\n{\n    public int total(Query query)\n    {\n        return query.total();\n    }\n}\n";

fn shop(order: &str) -> Database<'static> {
    database(&[("app/Models/Order.php", order), ("app/Models/Query.php", QUERY), ("app/Revenue.sharp", REVENUE)])
}

#[test]
fn a_file_whose_type_a_provider_read_from_a_class_is_analyzed_again_when_the_class_signature_changes() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads test") {
        return;
    }

    let mut warm = server(repository, shop(&order("int")));
    let first = codes(&warm.analyze().expect("initial analysis").issues);
    assert!(!first.contains_key("invalid-return-statement"), "the provider answers int: {first:#?}");

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("string");
    warm.database_mut().update(order_file, Cow::Owned(edited.clone().into_bytes()));
    let after = codes(&warm.analyze_incremental(&[order_file]).expect("analysis after a signature change").issues);

    let fresh = codes(&server(repository, shop(&edited)).analyze().expect("fresh analysis").issues);
    assert!(fresh.contains_key("invalid-return-statement"), "the provider answers string: {fresh:#?}");
    assert_eq!(after, fresh);
}

/// Each input `server` names for its one compiled PHP# file, stamped from the database, and the file's bytes.
fn compiled(server: &mut Server) -> (BTreeMap<String, Option<Input>>, Vec<u8>) {
    let database = server.database().read_only();
    let mut inputs = BTreeMap::new();
    let compiled = server
        .compile(
            |path| {
                let input = database.get(&FileId::new(path)).ok().map(|file| Input {
                    path: path.to_vec(),
                    size: file.contents.len() as u64,
                    mtime_ns: 0,
                    hash: source_hash(&file.contents),
                });
                inputs.insert(String::from_utf8_lossy(path).into_owned(), input.clone());
                Ok(input)
            },
            |_| Ok(Default::default()),
        )
        .expect("the compile runs");
    let [(_, Compilation::Accepted(bytes))] = compiled.as_slice() else {
        panic!("the PHP# file is not accepted: {compiled:?}");
    };

    (inputs, bytes.clone())
}

#[test]
fn a_sharp_file_whose_type_a_provider_read_from_a_model_lists_the_model_among_its_inputs() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads compile test") {
        return;
    }

    let mut server = server(repository, shop(&order("int")));
    server.analyze().expect("initial analysis");
    let (before, before_bytes) = compiled(&mut server);
    assert!(before.contains_key("app/Models/Order.php"), "the model the provider read: {:?}", before.keys());

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("int").replace("return 1;", "return 2;");
    server.database_mut().update(order_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a body edit");
    let (after, after_bytes) = compiled(&mut server);

    assert_ne!(before["app/Models/Order.php"], after["app/Models/Order.php"]);
    assert_eq!(header(&before_bytes).expect("a header").key, header(&after_bytes).expect("a header").key);
    assert_ne!(before_bytes, after_bytes, "the compiled file holds the model's new stamp");
}

#[test]
fn changing_a_property_type_of_the_model_a_provider_read_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads key test") {
        return;
    }

    let mut server = server(repository, shop(&order("int")));
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let order_file = FileId::new(b"app/Models/Order.php");
    let edited = order("int").replace("public int $count = 0;", "public string $count = '';");
    server.database_mut().update(order_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a property type change");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

#[test]
fn changing_a_property_type_of_the_parent_of_the_model_a_provider_read_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider reads parent key test") {
        return;
    }

    let parent = "<?php\n\nnamespace App\\Models;\n\nclass Model\n{\n    public int $count = 0;\n}\n";
    let order = order("int").replace("final class Order\n", "final class Order extends Model\n");
    let order = order.replace("    public int $count = 0;\n\n", "");
    let mut server = server(
        repository,
        database(&[
            ("app/Models/Model.php", parent),
            ("app/Models/Order.php", &order),
            ("app/Models/Query.php", QUERY),
            ("app/Revenue.sharp", REVENUE),
        ]),
    );
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let parent_file = FileId::new(b"app/Models/Model.php");
    let edited = parent.replace("public int $count = 0;", "public string $count = '';");
    server.database_mut().update(parent_file, Cow::Owned(edited.into_bytes()));
    server.analyze_incremental(&[parent_file]).expect("analysis after a parent's property type change");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

/// A PHP# file whose `int` return holds only while the provider, which lists every class to answer, answers
/// `Query::models` with `int`.
const CATALOG: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Catalog\n{\n    public int models(Query query)\n    {\n        return query.models();\n    }\n}\n";

#[test]
fn adding_a_class_to_the_classes_a_provider_listed_gives_the_sharp_file_a_new_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the provider listing key test") {
        return;
    }

    let mut server = server(repository, database(&[("app/Models/Query.php", QUERY), ("app/Catalog.sharp", CATALOG)]));
    server.analyze().expect("initial analysis");
    let (_, before) = compiled(&mut server);

    let invoice = server.database_mut().add(File::new(
        Cow::Borrowed(b"app/Models/Invoice.php"),
        FileType::Host,
        None,
        Cow::Borrowed(b"<?php\n\nnamespace App\\Models;\n\nfinal class Invoice {}\n"),
    ));
    server.analyze_incremental(&[invoice]).expect("analysis after a new class");
    let (_, after) = compiled(&mut server);

    assert_ne!(header(&before).expect("a header").key, header(&after).expect("a header").key);
}

/// The model the fixture's provider compares against `App\Models\Model`, declared with `parent` as its parent class.
fn order_extending(parent: &str) -> String {
    format!("<?php\n\nnamespace App\\Models;\n\nfinal class Order extends {parent} {{}}\n")
}

/// A PHP# file whose `int` return the provider answers by asking whether `Order` is a `Model`.
const BILLING: &str = "namespace App;\n\nimport App.Models.Query;\n\npublic class Billing\n{\n    public int billable(Query query)\n    {\n        return query.billable();\n    }\n}\n";

#[test]
fn changing_the_parent_of_a_model_a_provider_compared_changes_the_sharp_file_inputs_and_key() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    if !common::php_sdk_is_available(repository, "the comparator reads compile test") {
        return;
    }

    let mut server = server(
        repository,
        database(&[
            ("app/Models/Model.php", "<?php\n\nnamespace App\\Models;\n\nclass Model {}\n"),
            ("app/Models/Record.php", "<?php\n\nnamespace App\\Models;\n\nclass Record {}\n"),
            ("app/Models/Order.php", &order_extending("Model")),
            ("app/Models/Query.php", QUERY),
            ("app/Billing.sharp", BILLING),
        ]),
    );
    server.analyze().expect("initial analysis");
    let (before, before_bytes) = compiled(&mut server);
    assert!(before.contains_key("app/Models/Order.php"), "the model the provider compared: {:?}", before.keys());

    let order_file = FileId::new(b"app/Models/Order.php");
    server.database_mut().update(order_file, Cow::Owned(order_extending("Record").into_bytes()));
    server.analyze_incremental(&[order_file]).expect("analysis after a parent change");
    let (after, after_bytes) = compiled(&mut server);

    assert_ne!(before["app/Models/Order.php"], after["app/Models/Order.php"]);
    assert_ne!(header(&before_bytes).expect("a header").key, header(&after_bytes).expect("a header").key);
}
